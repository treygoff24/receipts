use std::env;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, to_value};

use crate::budget::Budget as RunBudget;
use crate::cli::{AskArgs, DepthArg, GlobalArgs, VerifyArg};
use crate::commands::CommandSuccess;
use crate::config::Config;
use crate::envelope::{Budget, CostDollars, Diagnostics, SuccessEnvelope};
use crate::error::{Provider, ReceiptsError};
use crate::pipeline::{self, RunParams};
use crate::providers::cerebras::CerebrasClient;
use crate::providers::exa::{DEFAULT_BASE_URL as EXA_DEFAULT_BASE_URL, ExaClient};
use crate::providers::{SharedSpend, new_spend};
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, DECOMPOSE_WORST_CASE_COST, EXPECTED_CLAIMS_PER_WORKER,
    EXTRACT_WORST_CASE_COST, MAX_CLAIMS_PER_WORKER, RELEVANCE_WORST_CASE_COST,
    VERIFICATION_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
};

pub fn run(global: &GlobalArgs, args: &AskArgs) -> Result<CommandSuccess, ReceiptsError> {
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

    let data = pipeline::run(
        question,
        global.depth.into(),
        global.verify.into(),
        &budget,
        &chat,
        &search,
        params,
    )?;
    let mut data_value = to_value(&data).map_err(|err| {
        ReceiptsError::upstream(format!("failed to serialize research data: {err}"))
    })?;
    if global.brief {
        let brief_params = RunParams::new(
            today_string(),
            cfg.max_concurrency as usize,
            Arc::clone(&spend),
        );
        match pipeline::synthesize_brief(&data, &chat, &search, &budget, &brief_params) {
            Ok(Some(text)) => {
                data_value["brief"] = json!(text);
            }
            Ok(None) => {
                // Budget gate refused the synthesis call — omit the field with
                // an uncertainty note, matching brief.rs behavior.
                if let Some(uncertainties) = data_value
                    .get_mut("uncertainties")
                    .and_then(|u| u.as_array_mut())
                {
                    uncertainties.push(json!("brief omitted: budget gate refused synthesis call"));
                }
            }
            Err(err) => {
                if let Some(uncertainties) = data_value
                    .get_mut("uncertainties")
                    .and_then(|u| u.as_array_mut())
                {
                    uncertainties.push(json!(format!("brief failed: {err}")));
                }
            }
        }
    }
    let cost = cost_from_spend(&spend, false)?;
    let retries = retries_from_spend(&spend)?;
    let exit_code = if budget.hit().is_some() { 10 } else { 0 };
    let envelope = SuccessEnvelope::new(
        "ask",
        data_value,
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

fn dry_run(global: &GlobalArgs, question: &str) -> Result<CommandSuccess, ReceiptsError> {
    let depth: crate::tiers::Depth = global.depth.into();
    let worker_count = match global.depth {
        DepthArg::Quick => 2,
        DepthArg::Standard => 4,
        DepthArg::Deep => 8,
    };
    let decompose_calls = usize::from(depth.needs_decompose());
    let verification_multiplier = match global.verify {
        VerifyArg::Adaptive => 1.0,
        VerifyArg::Paranoid => 3.0,
        VerifyArg::Off => 0.0,
    };
    // The relevance gate runs once per claim candidate whenever verification
    // itself is enabled (it's skipped, same as verify, under `--verify off`).
    let relevance_multiplier = if global.verify == VerifyArg::Off {
        0.0
    } else {
        1.0
    };
    let max_rounds = crate::pipeline::worker::MAX_ROUNDS as f64;
    // Workers typically resolve in a single search round (measured on the
    // validated prototype); MAX_ROUNDS only burns when sources keep failing.
    let expected_rounds = 1.0;

    // The relevance gate and verifier each run once PER EXTRACTED CLAIM, not
    // once per worker — a worker's answer can extract up to
    // MAX_CLAIMS_PER_WORKER claims (see extract::extract_claims). Worst case
    // projects the full cap; the expected case uses a documented, smaller
    // assumption (EXPECTED_CLAIMS_PER_WORKER) since most answers cite only a
    // handful of atomic claims.
    let max_claims_per_worker = MAX_CLAIMS_PER_WORKER as f64;
    let expected_claims_per_worker = EXPECTED_CLAIMS_PER_WORKER as f64;

    // Model component: decompose + worker rounds + extract + relevance + verify.
    let model_projected = decompose_calls as f64 * DECOMPOSE_WORST_CASE_COST
        + worker_count as f64 * max_rounds * WORKER_ROUND_WORST_CASE_COST
        + worker_count as f64 * EXTRACT_WORST_CASE_COST
        + worker_count as f64
            * max_claims_per_worker
            * relevance_multiplier
            * RELEVANCE_WORST_CASE_COST
        + worker_count as f64
            * max_claims_per_worker
            * verification_multiplier
            * VERIFICATION_WORST_CASE_COST;
    let model_expected = decompose_calls as f64 * DECOMPOSE_WORST_CASE_COST
        + worker_count as f64 * expected_rounds * WORKER_ROUND_WORST_CASE_COST
        + worker_count as f64 * EXTRACT_WORST_CASE_COST
        + worker_count as f64
            * expected_claims_per_worker
            * relevance_multiplier
            * RELEVANCE_WORST_CASE_COST
        + worker_count as f64
            * expected_claims_per_worker
            * verification_multiplier
            * VERIFICATION_WORST_CASE_COST;

    // Refinement pass (Deep only): worst case all subquestions are dead and
    // get a second worker round, PLUS a second extract/relevance/verify pass
    // over the refined claims — up to `worker_count` additional units of
    // each.
    let refinement_note = if global.depth == DepthArg::Deep {
        "worst case incl. refinement"
    } else {
        ""
    };
    let refinement_projected = if global.depth == DepthArg::Deep {
        worker_count as f64 * max_rounds * WORKER_ROUND_WORST_CASE_COST
            + worker_count as f64 * EXTRACT_WORST_CASE_COST
            + worker_count as f64
                * max_claims_per_worker
                * relevance_multiplier
                * RELEVANCE_WORST_CASE_COST
            + worker_count as f64
                * max_claims_per_worker
                * verification_multiplier
                * VERIFICATION_WORST_CASE_COST
    } else {
        0.0
    };

    // Search component: per-search-call costs (worker_count * max_rounds
    // search calls) + per-worker contents fetch costs. Uses the same
    // unit-cost semantics as live metering.
    let search_projected =
        worker_count as f64 * max_rounds * crate::tiers::SEARCH_CALL_WORST_CASE_COST
            + worker_count as f64 * CONTENTS_WORST_CASE_COST;
    let search_expected =
        worker_count as f64 * expected_rounds * crate::tiers::SEARCH_CALL_WORST_CASE_COST
            + worker_count as f64 * CONTENTS_WORST_CASE_COST;

    let total_projected = model_projected + refinement_projected + search_projected;
    // Expected case: single round per worker, no refinement pass fires.
    let total_expected = model_expected + search_expected;

    let data = json!({
        "question": question,
        "outcome": "answered",
        "dryRun": true,
        "plannedFanout": {
            "tier": depth_name(global.depth),
            "workers": worker_count,
            "decomposeCalls": decompose_calls,
            "maxWorkerRounds": crate::pipeline::worker::MAX_ROUNDS,
            "verify": verify_name(global.verify),
            "refinementPass": global.depth == DepthArg::Deep,
            "note": refinement_note
        },
        "projectedCost": total_expected,
        "projectedWorstCaseCost": total_projected
    });
    let envelope = SuccessEnvelope::new(
        "ask",
        data,
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

pub(crate) fn cost_from_spend(
    spend: &SharedSpend,
    estimated: bool,
) -> Result<CostDollars, ReceiptsError> {
    let spend = spend
        .lock()
        .map_err(|_| ReceiptsError::upstream("spend meter lock poisoned"))?;
    Ok(CostDollars {
        model: spend.dollars,
        search: spend.search_dollars,
        total: spend.total_dollars(),
        estimated,
    })
}

pub(crate) fn retries_from_spend(spend: &SharedSpend) -> Result<u32, ReceiptsError> {
    let spend = spend
        .lock()
        .map_err(|_| ReceiptsError::upstream("spend meter lock poisoned"))?;
    Ok(spend.retries.min(u32::MAX as u64) as u32)
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
    env::var("RECEIPTS_EXA_BASE")
        .or_else(|_| env::var("EXA_API_BASE"))
        .unwrap_or_else(|_| EXA_DEFAULT_BASE_URL.to_string())
}

pub(crate) fn depth_name(depth: DepthArg) -> &'static str {
    match depth {
        DepthArg::Quick => "quick",
        DepthArg::Standard => "standard",
        DepthArg::Deep => "deep",
    }
}

pub(crate) fn verify_name(verify: VerifyArg) -> &'static str {
    match verify {
        VerifyArg::Adaptive => "adaptive",
        VerifyArg::Paranoid => "paranoid",
        VerifyArg::Off => "off",
    }
}

fn today_string() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
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
