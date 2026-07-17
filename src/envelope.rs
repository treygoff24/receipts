//! `receipts.cli.response.v1` (stdout, success) and `receipts.cli.error.v1`
//! (stderr, failure) envelopes. On failure stdout stays empty — the whole
//! result travels in the error envelope on stderr.

use std::io::{self, IsTerminal};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{PartialData, ReceiptsError};

fn new_request_id(request_id: Option<String>) -> String {
    request_id.unwrap_or_else(|| Uuid::new_v4().to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostDollars {
    pub model: f64,
    pub search: f64,
    pub total: f64,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// "dollars" | "seconds" | null
    pub hit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub duration_ms: u64,
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessEnvelope<T> {
    pub schema: String,
    pub ok: bool,
    pub command: String,
    pub request_id: String,
    pub data: T,
    pub cost_dollars: CostDollars,
    pub budget: Budget,
    pub diagnostics: Diagnostics,
}

impl<T> SuccessEnvelope<T> {
    pub fn new(
        command: impl Into<String>,
        data: T,
        cost_dollars: CostDollars,
        budget: Budget,
        diagnostics: Diagnostics,
        request_id: Option<String>,
    ) -> Self {
        Self {
            schema: "receipts.cli.response.v1".to_string(),
            ok: true,
            command: command.into(),
            request_id: new_request_id(request_id),
            data,
            cost_dollars,
            budget,
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: String,
    pub category: String,
    pub retryable: bool,
    pub provider: Option<String>,
    pub message: String,
    pub partial: Option<PartialData>,
    pub suggested_fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    pub schema: String,
    pub ok: bool,
    pub command: String,
    pub request_id: String,
    pub error: ErrorDetail,
    /// Kept out of JSON; `emit_error` returns it as the process status.
    #[serde(skip)]
    pub exit_code: i32,
}

impl ErrorEnvelope {
    pub fn from_error(
        command: impl Into<String>,
        err: &ReceiptsError,
        request_id: Option<String>,
    ) -> Self {
        Self {
            schema: "receipts.cli.error.v1".to_string(),
            ok: false,
            command: command.into(),
            request_id: new_request_id(request_id),
            error: ErrorDetail {
                code: err.code().to_string(),
                category: err.category().to_string(),
                retryable: err.is_retryable(),
                provider: err.provider().map(|p| p.as_str().to_string()),
                message: err.to_string(),
                partial: err.partial_data().cloned(),
                suggested_fix: err.suggested_fix().map(ToString::to_string),
            },
            exit_code: err.exit_code(),
        }
    }
}

pub fn emit_success<T: Serialize>(env: &SuccessEnvelope<T>, force_json: bool) {
    let stdout = io::stdout();
    if !force_json && stdout.is_terminal() {
        render_success_human(env);
    } else {
        println!(
            "{}",
            serde_json::to_string(env).expect("envelope serializes")
        );
    }
}

pub fn emit_error(env: &ErrorEnvelope, force_json: bool) -> i32 {
    let stderr = io::stderr();
    if !force_json && stderr.is_terminal() {
        render_error_human(env);
    } else {
        eprintln!(
            "{}",
            serde_json::to_string(env).expect("envelope serializes")
        );
    }
    env.exit_code
}

fn render_success_human<T: Serialize>(env: &SuccessEnvelope<T>) {
    println!(
        "receipts {} — ok (requestId {})",
        env.command, env.request_id
    );
    println!(
        "cost: ${:.4} (model ${:.4} + search ${:.4}){}",
        env.cost_dollars.total,
        env.cost_dollars.model,
        env.cost_dollars.search,
        if env.cost_dollars.estimated {
            " [estimated]"
        } else {
            ""
        }
    );
    if let Some(hit) = &env.budget.hit {
        println!("budget hit: {hit}");
    }
    println!(
        "duration: {}ms, retries: {}",
        env.diagnostics.duration_ms, env.diagnostics.retries
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&env.data).expect("JSON value serializes")
    );
}

fn render_error_human(env: &ErrorEnvelope) {
    eprintln!(
        "receipts {} — error [{}] (requestId {})",
        env.command, env.error.code, env.request_id
    );
    eprintln!("{}", env.error.message);
    if let Some(provider) = &env.error.provider {
        eprintln!("provider: {provider}");
    }
    eprintln!("retryable: {}", env.error.retryable);
    if let Some(partial) = &env.error.partial {
        eprintln!(
            "partial: {}",
            serde_json::to_string_pretty(partial).expect("JSON value serializes")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Provider;

    const FIXED_REQUEST_ID: &str = "00000000-0000-0000-0000-000000000000";

    #[derive(Serialize)]
    struct TestAskData {
        question: &'static str,
        outcome: &'static str,
    }

    #[test]
    fn golden_success_envelope_has_exact_camel_case_fields() {
        let env = SuccessEnvelope::new(
            "ask",
            TestAskData {
                question: "what is rust",
                outcome: "answered",
            },
            CostDollars {
                model: 0.09,
                search: 0.04,
                total: 0.13,
                estimated: false,
            },
            Budget { hit: None },
            Diagnostics {
                duration_ms: 12100,
                retries: 0,
            },
            Some(FIXED_REQUEST_ID.to_string()),
        );

        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            format!(
                r#"{{"schema":"receipts.cli.response.v1","ok":true,"command":"ask","requestId":"{FIXED_REQUEST_ID}","data":{{"question":"what is rust","outcome":"answered"}},"costDollars":{{"model":0.09,"search":0.04,"total":0.13,"estimated":false}},"budget":{{"hit":null}},"diagnostics":{{"durationMs":12100,"retries":0}}}}"#
            )
        );
    }

    #[test]
    fn golden_error_envelope_has_exact_camel_case_fields() {
        let err = ReceiptsError::rate_limit("Cerebras returned 429")
            .with_provider(Provider::Cerebras)
            .with_retryable(true)
            .with_partial(PartialData::default())
            .with_suggested_fix("wait and retry; Cerebras rate windows are minute-scale");

        let env = ErrorEnvelope::from_error("ask", &err, Some(FIXED_REQUEST_ID.to_string()));
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            format!(
                r#"{{"schema":"receipts.cli.error.v1","ok":false,"command":"ask","requestId":"{FIXED_REQUEST_ID}","error":{{"code":"rate_limited","category":"rate_limited","retryable":true,"provider":"cerebras","message":"rate limited: Cerebras returned 429","partial":{{"claims":[]}},"suggestedFix":"wait and retry; Cerebras rate windows are minute-scale"}}}}"#
            )
        );

        assert_eq!(env.exit_code, 6);
    }

    #[test]
    fn error_envelope_without_provider_or_partial_omits_neither_field() {
        let err = ReceiptsError::usage("unknown flag --frobnicate");
        let env = ErrorEnvelope::from_error("ask", &err, Some(FIXED_REQUEST_ID.to_string()));
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            format!(
                r#"{{"schema":"receipts.cli.error.v1","ok":false,"command":"ask","requestId":"{FIXED_REQUEST_ID}","error":{{"code":"usage","category":"usage","retryable":false,"provider":null,"message":"usage error: unknown flag --frobnicate","partial":null,"suggestedFix":null}}}}"#
            )
        );
        assert_eq!(env.exit_code, 1);
    }

    #[test]
    fn request_id_defaults_to_a_fresh_uuid_when_none_given() {
        let env = SuccessEnvelope::new(
            "capabilities",
            (),
            CostDollars {
                model: 0.0,
                search: 0.0,
                total: 0.0,
                estimated: false,
            },
            Budget { hit: None },
            Diagnostics {
                duration_ms: 1,
                retries: 0,
            },
            None,
        );
        assert!(Uuid::parse_str(&env.request_id).is_ok());
    }
}
