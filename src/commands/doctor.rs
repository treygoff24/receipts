use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::to_value;

use crate::cli::{DoctorArgs, GlobalArgs};
use crate::commands::CommandSuccess;
use crate::commands::ask::{cost_from_spend, exa_base_url, require_key, retries_from_spend};
use crate::config::Config;
use crate::envelope::{Budget, Diagnostics, SuccessEnvelope};
use crate::error::{Provider, ReconError};
use crate::providers::cerebras::{CerebrasClient, ChatOpts, Message};
use crate::providers::exa::{ExaClient, SearchProvider};
use crate::providers::new_spend;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    schema_version: &'static str,
    status: &'static str,
    summary: DoctorSummary,
    checks: Vec<DoctorCheck>,
    run_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorSummary {
    total: usize,
    ok: usize,
    warn: usize,
    error: usize,
    fixable: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    id: &'static str,
    category: &'static str,
    severity: &'static str,
    ok: bool,
    detail: String,
    location: Option<String>,
    fix_available: bool,
    remediation: Option<Remediation>,
}

#[derive(Debug, Serialize)]
struct Remediation {
    summary: String,
    command: String,
    reversible: bool,
}

pub fn run(global: &GlobalArgs, args: &DoctorArgs) -> Result<CommandSuccess, ReconError> {
    let started = Instant::now();
    let cfg = Config::load()?;
    let spend = new_spend();
    let mut checks = offline_checks(&cfg, global);

    if args.online && keys_present(&cfg) {
        let model = global.model.clone().unwrap_or_else(|| cfg.model.clone());
        let chat = CerebrasClient::new(
            require_key(
                cfg.cerebras_api_key.as_deref(),
                Provider::Cerebras,
                "CEREBRAS_API_KEY",
            )?,
            cfg.api_base.clone(),
            model,
        )
        .with_spend(Arc::clone(&spend));
        checks.push(probe_cerebras(&chat));

        let search = ExaClient::new(
            require_key(cfg.exa_api_key.as_deref(), Provider::Exa, "EXA_API_KEY")?,
            exa_base_url(),
        )
        .with_spend(Arc::clone(&spend));
        checks.push(probe_exa(&search));
    }

    let exit_code = if checks
        .iter()
        .any(|check| !check.ok && check.category == "auth")
    {
        2
    } else {
        0
    };
    let report = report(checks);
    let retries = retries_from_spend(&spend)?;
    let envelope = SuccessEnvelope::new(
        "doctor",
        to_value(report).map_err(|err| {
            ReconError::upstream(format!("failed to serialize doctor report: {err}"))
        })?,
        cost_from_spend(&spend, false)?,
        Budget { hit: None },
        Diagnostics {
            duration_ms: started.elapsed().as_millis() as u64,
            retries,
        },
        None,
    );

    Ok(CommandSuccess {
        envelope,
        exit_code,
        hint: Some("run `recon doctor --online --json` to probe provider credentials"),
    })
}

fn offline_checks(cfg: &Config, global: &GlobalArgs) -> Vec<DoctorCheck> {
    vec![
        ok_check(
            "config.parse",
            "config",
            "config parsed; defaults and environment overrides resolved",
        ),
        key_check(
            "auth.cerebras_key",
            Provider::Cerebras,
            "CEREBRAS_API_KEY",
            cfg.cerebras_api_key.as_deref(),
        ),
        key_check(
            "auth.exa_key",
            Provider::Exa,
            "EXA_API_KEY",
            cfg.exa_api_key.as_deref(),
        ),
        ok_check(
            "config.resolved",
            "config",
            &format!(
                "model={}, apiBase={}, exaBase={}, maxConcurrency={}, depth={:?}, verify={:?}",
                global.model.as_deref().unwrap_or(&cfg.model),
                cfg.api_base,
                exa_base_url(),
                cfg.max_concurrency,
                global.depth,
                global.verify
            ),
        ),
    ]
}

fn probe_cerebras(client: &CerebrasClient) -> DoctorCheck {
    match client.chat(
        &[Message::user("Reply with ok.".to_string())],
        ChatOpts {
            max_completion_tokens: Some(1),
            ..ChatOpts::default()
        },
    ) {
        Ok(_) => ok_check(
            "online.cerebras",
            "auth",
            "Cerebras chat-completions probe succeeded",
        ),
        Err(err) => auth_probe_error(
            "online.cerebras",
            Provider::Cerebras,
            "CEREBRAS_API_KEY",
            err,
        ),
    }
}

fn probe_exa(client: &ExaClient) -> DoctorCheck {
    match client.search("recon doctor probe") {
        Ok(_) => ok_check("online.exa", "auth", "Exa search probe succeeded"),
        Err(err) => auth_probe_error("online.exa", Provider::Exa, "EXA_API_KEY", err),
    }
}

fn auth_probe_error(
    id: &'static str,
    provider: Provider,
    env_var: &'static str,
    err: ReconError,
) -> DoctorCheck {
    DoctorCheck {
        id,
        category: "auth",
        severity: "error",
        ok: false,
        detail: format!("{provider} probe failed; check {env_var}: {err}"),
        location: Some(env_var.to_string()),
        fix_available: false,
        remediation: Some(Remediation {
            summary: format!("set a valid {provider} API key"),
            command: format!("export {env_var}=..."),
            reversible: false,
        }),
    }
}

fn key_check(
    id: &'static str,
    provider: Provider,
    env_var: &'static str,
    key: Option<&str>,
) -> DoctorCheck {
    if key.is_some_and(|value| !value.trim().is_empty()) {
        ok_check(id, "auth", &format!("{provider} key present via {env_var}"))
    } else {
        DoctorCheck {
            id,
            category: "auth",
            severity: "error",
            ok: false,
            detail: format!("missing {provider} API key; set {env_var}"),
            location: Some(env_var.to_string()),
            fix_available: false,
            remediation: Some(Remediation {
                summary: format!("set {env_var}"),
                command: format!("export {env_var}=..."),
                reversible: false,
            }),
        }
    }
}

fn ok_check(id: &'static str, category: &'static str, detail: &str) -> DoctorCheck {
    DoctorCheck {
        id,
        category,
        severity: "info",
        ok: true,
        detail: detail.to_string(),
        location: None,
        fix_available: false,
        remediation: None,
    }
}

fn report(checks: Vec<DoctorCheck>) -> DoctorReport {
    let summary = DoctorSummary {
        total: checks.len(),
        ok: checks.iter().filter(|check| check.ok).count(),
        warn: checks
            .iter()
            .filter(|check| !check.ok && check.severity == "warn")
            .count(),
        error: checks
            .iter()
            .filter(|check| !check.ok && check.severity == "error")
            .count(),
        fixable: checks.iter().filter(|check| check.fix_available).count(),
    };
    let status = if summary.error > 0 {
        "broken"
    } else if summary.warn > 0 {
        "degraded"
    } else {
        "healthy"
    };
    DoctorReport {
        schema_version: "1.0",
        status,
        summary,
        checks,
        run_id: None,
    }
}

fn keys_present(cfg: &Config) -> bool {
    cfg.cerebras_api_key
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && cfg
            .exa_api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}
