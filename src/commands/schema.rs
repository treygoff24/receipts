use serde_json::{Value, json};

use crate::cli::{GlobalArgs, SchemaArgs, SchemaTarget};
use crate::commands::CommandSuccess;
use crate::envelope::{Budget, CostDollars, Diagnostics, SuccessEnvelope};
use crate::error::ReconError;

pub fn run(_global: &GlobalArgs, args: &SchemaArgs) -> Result<CommandSuccess, ReconError> {
    let data = match args.target {
        SchemaTarget::Response => response_schema(),
        SchemaTarget::Error => error_schema(),
        SchemaTarget::All => json!({
            "response": response_schema(),
            "error": error_schema()
        }),
    };
    Ok(CommandSuccess {
        envelope: SuccessEnvelope::new(
            "schema",
            data,
            CostDollars {
                model: 0.0,
                search: 0.0,
                total: 0.0,
                estimated: false,
            },
            Budget { hit: None },
            Diagnostics {
                duration_ms: 0,
                retries: 0,
            },
            None,
        ),
        exit_code: 0,
        hint: None,
    })
}

fn response_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "recon.cli.response.v1",
        "type": "object",
        "additionalProperties": false,
        "required": ["schema", "ok", "command", "requestId", "data", "costDollars", "budget", "diagnostics"],
        "properties": {
            "schema": {"const": "recon.cli.response.v1"},
            "ok": {"const": true},
            "command": {"type": "string"},
            "requestId": {"type": "string"},
            "data": {
                "type": "object",
                "required": ["question", "outcome", "claims", "searchTrail", "uncertainties"],
                "properties": {
                    "question": {"type": "string"},
                    "outcome": {"enum": ["answered", "partial", "unanswered"]},
                    "claims": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["claim", "sourceUrl", "quote", "verdict", "note", "published"],
                            "properties": {
                                "claim": {"type": "string"},
                                "sourceUrl": {"type": "string"},
                                "quote": {"type": ["string", "null"]},
                                "verdict": {"enum": ["supported", "partial", "unsupported", "no_source"]},
                                "note": {"type": "string"},
                                "published": {"type": ["string", "null"]}
                            }
                        }
                    },
                    "searchTrail": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["query", "results"],
                            "properties": {"query": {"type": "string"}, "results": {"type": "integer"}}
                        }
                    },
                    "uncertainties": {"type": "array", "items": {"type": "string"}}
                }
            },
            "costDollars": {
                "type": "object",
                "required": ["model", "search", "total", "estimated"],
                "properties": {
                    "model": {"type": "number"},
                    "search": {"type": "number"},
                    "total": {"type": "number"},
                    "estimated": {"type": "boolean"}
                }
            },
            "budget": {"type": "object", "required": ["hit"], "properties": {"hit": {"enum": ["dollars", "seconds", null]}}},
            "diagnostics": {"type": "object", "required": ["durationMs", "retries"], "properties": {"durationMs": {"type": "integer"}, "retries": {"type": "integer"}}}
        }
    })
}

fn error_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "recon.cli.error.v1",
        "type": "object",
        "required": ["schema", "ok", "command", "requestId", "error"],
        "properties": {
            "schema": {"const": "recon.cli.error.v1"},
            "ok": {"const": false},
            "command": {"type": "string"},
            "requestId": {"type": "string"},
            "error": {
                "type": "object",
                "required": ["code", "category", "retryable", "provider", "message", "partial"],
                "properties": {
                    "code": {"enum": ["usage", "auth", "config", "network", "upstream", "rate_limited", "partial", "no_input"]},
                    "category": {"type": "string"},
                    "retryable": {"type": "boolean"},
                    "provider": {"enum": ["cerebras", "exa", null]},
                    "message": {"type": "string"},
                    "partial": {"type": ["object", "array", "string", "number", "boolean", "null"]},
                    "suggestedFix": {"type": "string"},
                    "details": {"type": "object"}
                }
            }
        }
    })
}
