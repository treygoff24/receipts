use std::env;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, to_value};

use crate::budget::Budget as RunBudget;
use crate::cli::{AskArgs, DepthArg, GlobalArgs, VerifyArg};
use crate::commands::CommandSuccess;
use crate::config::Config;
use crate::envelope::{Budget, CostDollars, Diagnostics, SuccessEnvelope};
use crate::error::{Provider, ReconError};
use crate::pipeline::{self, RunParams};
use crate::providers::cerebras::CerebrasClient;
use crate::providers::exa::{DEFAULT_BASE_URL as EXA_DEFAULT_BASE_URL, ExaClient};
use crate::providers::{SharedSpend, new_spend};
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, DECOMPOSE_WORST_CASE_COST, EXTRACT_WORST_CASE_COST,
    VERIFICATION_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
};

pub fn run(global: &GlobalArgs, args: &AskArgs) -> Result<CommandSuccess, ReconError> {
    let question = args.question.join(" ");
    let question = question.trim();
    if question.is_empty() {
        return Err(ReconError::no_input(
            "ask requires <QUESTION>; run `recon ask \"what do you need to know?\"`",
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
    let search = ExaClient::new(exa_key, exa_base_url()).with_spend(Arc::clone(&spend));
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
    let mut data_value = to_value(&data)
        .map_err(|err| ReconError::upstream(format!("failed to serialize research data: {err}")))?;
    if global.brief {
        data_value["brief"] = json!(brief_from_data(&data));
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

fn brief_from_data(data: &pipeline::ResearchData) -> String {
    let claims: Vec<_> = data
        .claims
        .iter()
        .filter(|claim| claim.verdict.is_supported_or_partial())
        .take(5)
        .map(|claim| format!("{} ({})", claim.claim, claim.source_url))
        .collect();
    if claims.is_empty() {
        return "No supported or partial claims found.".to_string();
    }
    format!("Supported or partial claims: {}", claims.join("; "))
}

fn dry_run(global: &GlobalArgs, question: &str) -> Result<CommandSuccess, ReconError> {
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
    let projected = decompose_calls as f64 * DECOMPOSE_WORST_CASE_COST
        + worker_count as f64
            * crate::pipeline::worker::MAX_ROUNDS as f64
            * WORKER_ROUND_WORST_CASE_COST
        + worker_count as f64 * EXTRACT_WORST_CASE_COST
        + worker_count as f64 * verification_multiplier * VERIFICATION_WORST_CASE_COST
        + worker_count as f64 * CONTENTS_WORST_CASE_COST;

    let data = json!({
        "question": question,
        "outcome": "answered",
        "dryRun": true,
        "plannedFanout": {
            "tier": depth_name(global.depth),
            "workers": worker_count,
            "decomposeCalls": decompose_calls,
            "maxWorkerRounds": crate::pipeline::worker::MAX_ROUNDS,
            "verify": verify_name(global.verify)
        },
        "projectedWorstCaseCost": projected
    });
    let envelope = SuccessEnvelope::new(
        "ask",
        data,
        CostDollars {
            model: projected,
            search: 0.0,
            total: projected,
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
) -> Result<CostDollars, ReconError> {
    let spend = spend
        .lock()
        .map_err(|_| ReconError::upstream("spend meter lock poisoned"))?;
    Ok(CostDollars {
        model: spend.dollars,
        search: spend.search_dollars,
        total: spend.total_dollars(),
        estimated,
    })
}

pub(crate) fn retries_from_spend(spend: &SharedSpend) -> Result<u32, ReconError> {
    let spend = spend
        .lock()
        .map_err(|_| ReconError::upstream("spend meter lock poisoned"))?;
    Ok(spend.retries.min(u32::MAX as u64) as u32)
}

pub(crate) fn require_key(
    key: Option<&str>,
    provider: Provider,
    env_var: &'static str,
) -> Result<String, ReconError> {
    key.filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            ReconError::auth(format!("missing {provider} API key; set {env_var}"))
                .with_provider(provider)
        })
}

pub(crate) fn exa_base_url() -> String {
    env::var("RECON_EXA_BASE")
        .or_else(|_| env::var("EXA_API_BASE"))
        .unwrap_or_else(|_| EXA_DEFAULT_BASE_URL.to_string())
}

fn depth_name(depth: DepthArg) -> &'static str {
    match depth {
        DepthArg::Quick => "quick",
        DepthArg::Standard => "standard",
        DepthArg::Deep => "deep",
    }
}

fn verify_name(verify: VerifyArg) -> &'static str {
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
