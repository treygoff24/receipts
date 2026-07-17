use std::env;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::budget::Budget as RunBudget;
use crate::cli::{AskArgs, DepthArg, GlobalArgs, VerifyArg};
use crate::commands::CommandSuccess;
use crate::config::Config;
use crate::envelope::{Budget, CostDollars, Diagnostics, SuccessEnvelope};
use crate::error::{Provider, ReceiptsError};
use crate::pipeline::{self, Outcome, ResearchData, RunParams};
use crate::providers::cerebras::CerebrasClient;
use crate::providers::exa::{DEFAULT_BASE_URL as EXA_DEFAULT_BASE_URL, ExaClient};
use crate::providers::{SharedSpend, new_spend};
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, DECOMPOSE_WORST_CASE_COST, EXPECTED_CLAIMS_PER_WORKER,
    EXTRACT_WORST_CASE_COST, MAX_CLAIMS_PER_WORKER, RELEVANCE_WORST_CASE_COST,
    VERIFICATION_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
};

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AskData {
    Research(ResearchResponse),
    DryRun(DryRunData),
}

#[derive(Debug, Serialize)]
pub struct ResearchResponse {
    #[serde(flatten)]
    data: ResearchData,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunData {
    question: String,
    outcome: Outcome,
    dry_run: bool,
    planned_fanout: PlannedFanout,
    projected_cost: f64,
    projected_worst_case_cost: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlannedFanout {
    tier: &'static str,
    workers: usize,
    decompose_calls: usize,
    max_worker_rounds: usize,
    verify: &'static str,
    refinement_pass: bool,
    note: &'static str,
}

pub fn run(global: &GlobalArgs, args: &AskArgs) -> Result<CommandSuccess<AskData>, ReceiptsError> {
    let question = args.question.join(" ");
    let question = question.trim();
    if question.is_empty() {
        return Err(ReceiptsError::no_input(
            "ask requires <QUESTION>; run `receipts ask \"what do you need to know?\"`",
        ));
    }

    if global.dry_run {
        return dry_run(global, question);
    }

    let started = Instant::now();
    let cfg = Config::load()?;
    let cerebras_key = require_key(
        cfg.cerebras_api_key.as_deref(),
        Provider::Cerebras,
        "CEREBRAS_API_KEY",
    )?;
    let exa_key = require_key(cfg.exa_api_key.as_deref(), Provider::Exa, "EXA_API_KEY")?;

    let spend = new_spend();
    let model = global.model.clone().unwrap_or(cfg.model);
    let chat =
        CerebrasClient::new(cerebras_key, cfg.api_base, model).with_spend(Arc::clone(&spend));
    let search = ExaClient::new(exa_key, exa_base_url())
        .with_search_type(cfg.exa_search_type.clone())
        .with_spend(Arc::clone(&spend));
    let budget = RunBudget::new(global.max_dollars, global.max_seconds);
    let params = RunParams::new(
        today_string(),
        cfg.max_concurrency as usize,
        Arc::clone(&spend),
    );

    let mut data = pipeline::run(
        question,
        global.depth,
        global.verify,
        &budget,
        &chat,
        &search,
        params,
    )?;
    let mut brief = None;
    if global.brief {
        let brief_params = RunParams::new(
            today_string(),
            cfg.max_concurrency as usize,
            Arc::clone(&spend),
        );
        match pipeline::synthesize_brief(&data, &chat, &search, &budget, &brief_params) {
            Ok(Some(text)) => {
                brief = Some(text);
            }
            Ok(None) => {
                data.uncertainties
                    .push("brief omitted: budget gate refused synthesis call".to_string());
            }
            Err(err) => {
                data.uncertainties.push(format!("brief failed: {err}"));
            }
        }
    }
    let cost = cost_from_spend(&spend, false);
    let retries = retries_from_spend(&spend);
    let exit_code = if budget.hit().is_some() { 10 } else { 0 };
    let envelope = SuccessEnvelope::new(
        "ask",
        AskData::Research(ResearchResponse { data, brief }),
        cost,
        Budget {
            hit: budget.hit().map(str::to_string),
        },
        Diagnostics {
            duration_ms: started.elapsed().as_millis() as u64,
            retries,
        },
        None,
    );

    Ok(CommandSuccess {
        envelope,
        exit_code,
        hint: Some("rerun with --json for the raw envelope, or --depth deep for more coverage"),
    })
}

fn dry_run(global: &GlobalArgs, question: &str) -> Result<CommandSuccess<AskData>, ReceiptsError> {
    let depth = global.depth;
    let worker_count = depth.worker_count();
    let decompose_calls = usize::from(depth.needs_decompose());
    let verification_multiplier = match global.verify {
        VerifyArg::Adaptive => 1.0,
        VerifyArg::Paranoid => 3.0,
        VerifyArg::Off => 0.0,
    };
    let relevance_multiplier = if global.verify == VerifyArg::Off {
        0.0
    } else {
        1.0
    };
    let max_rounds = crate::pipeline::worker::MAX_ROUNDS as f64;
    // Workers typically resolve in a single search round (measured on the
    // validated prototype); MAX_ROUNDS only burns when sources keep failing.
    let expected_rounds = 1.0;

    // Both gates charge per extracted claim, not per worker. Use the prompt cap
    // for worst case and the empirical assumption for expected cost.
    let max_claims_per_worker = MAX_CLAIMS_PER_WORKER as f64;
    let expected_claims_per_worker = EXPECTED_CLAIMS_PER_WORKER as f64;

    let model_cost = |decompose_calls: usize, rounds: f64, claims_per_worker: f64| {
        decompose_calls as f64 * DECOMPOSE_WORST_CASE_COST
            + worker_count as f64 * rounds * WORKER_ROUND_WORST_CASE_COST
            + worker_count as f64 * EXTRACT_WORST_CASE_COST
            + worker_count as f64
                * claims_per_worker
                * relevance_multiplier
                * RELEVANCE_WORST_CASE_COST
            + worker_count as f64
                * claims_per_worker
                * verification_multiplier
                * VERIFICATION_WORST_CASE_COST
    };
    let model_projected = model_cost(decompose_calls, max_rounds, max_claims_per_worker);
    let model_expected = model_cost(decompose_calls, expected_rounds, expected_claims_per_worker);

    // Deep's worst case retries every dead subquestion through the full pipeline.
    let refinement_note = if global.depth == DepthArg::Deep {
        "worst case incl. refinement"
    } else {
        ""
    };
    let refinement_projected = if global.depth == DepthArg::Deep {
        model_cost(0, max_rounds, max_claims_per_worker)
    } else {
        0.0
    };

    let search_cost = |rounds: f64| {
        worker_count as f64
            * (rounds * crate::tiers::SEARCH_CALL_WORST_CASE_COST + CONTENTS_WORST_CASE_COST)
    };
    let search_projected = search_cost(max_rounds);
    let search_expected = search_cost(expected_rounds);

    let total_projected = model_projected + refinement_projected + search_projected;
    let total_expected = model_expected + search_expected;

    let data = DryRunData {
        question: question.to_string(),
        outcome: Outcome::Answered,
        dry_run: true,
        planned_fanout: PlannedFanout {
            tier: global.depth.as_str(),
            workers: worker_count,
            decompose_calls,
            max_worker_rounds: crate::pipeline::worker::MAX_ROUNDS,
            verify: global.verify.as_str(),
            refinement_pass: global.depth == DepthArg::Deep,
            note: refinement_note,
        },
        projected_cost: total_expected,
        projected_worst_case_cost: total_projected,
    };
    let envelope = SuccessEnvelope::new(
        "ask",
        AskData::DryRun(data),
        CostDollars {
            model: model_expected,
            search: search_expected,
            total: total_expected,
            estimated: true,
        },
        Budget { hit: None },
        Diagnostics {
            duration_ms: 0,
            retries: 0,
        },
        None,
    );
    Ok(CommandSuccess {
        envelope,
        exit_code: 0,
        hint: Some("remove --dry-run to spend against the configured providers"),
    })
}

pub(crate) fn cost_from_spend(spend: &SharedSpend, estimated: bool) -> CostDollars {
    let spend = spend.lock().expect("spend meter lock poisoned");
    CostDollars {
        model: spend.dollars,
        search: spend.search_dollars,
        total: spend.total_dollars(),
        estimated,
    }
}

pub(crate) fn retries_from_spend(spend: &SharedSpend) -> u32 {
    let spend = spend.lock().expect("spend meter lock poisoned");
    u32::try_from(spend.retries).expect("retry count exceeds u32")
}

pub(crate) fn require_key(
    key: Option<&str>,
    provider: Provider,
    env_var: &'static str,
) -> Result<String, ReceiptsError> {
    key.filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            let suggested_fix = format!(
                "set {env_var} or add {} to ~/.config/receipts/config.toml; verify with: receipts doctor",
                env_var.to_lowercase()
            );
            ReceiptsError::auth(format!("missing {provider} API key; set {env_var}"))
                .with_provider(provider)
                .with_suggested_fix(suggested_fix)
        })
}

pub(crate) fn exa_base_url() -> String {
    env::var("RECEIPTS_EXA_BASE").unwrap_or_else(|_| EXA_DEFAULT_BASE_URL.to_string())
}

fn today_string() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs()
        / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}
