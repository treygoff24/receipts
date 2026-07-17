use serde_json::{Value, json};

use crate::cli::{GlobalArgs, SchemaArgs, SchemaTarget};
use crate::commands::CommandSuccess;
use crate::error::ReceiptsError;

pub fn run(_global: &GlobalArgs, args: &SchemaArgs) -> Result<CommandSuccess, ReceiptsError> {
    let data = match args.target {
        SchemaTarget::Response => response_schema(),
        SchemaTarget::Error => error_schema(),
        SchemaTarget::All => json!({
            "response": response_schema(),
            "error": error_schema()
        }),
    };
    Ok(CommandSuccess::free("schema", data))
}

fn response_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "receipts.cli.response.v1",
        "type": "object",
        "required": ["schema", "ok", "command", "requestId", "data", "costDollars", "budget", "diagnostics"],
        "properties": {
            "schema": {"const": "receipts.cli.response.v1"},
            "ok": {"const": true},
            "command": {"enum": ["ask", "doctor", "capabilities", "schema", "help", "version"]},
            "requestId": {"type": "string"},
            "data": {"oneOf": [
                {
                    "type": "object",
                    "description": "ask success payload",
                    "required": ["question", "outcome", "claims", "searchTrail", "uncertainties"],
                    "properties": {
                        "question": {"type": "string"},
                        "outcome": {"enum": ["answered", "partial", "unanswered"]},
                        "claims": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["claim", "sourceUrl", "quote", "verdict", "relevance", "note", "published"],
                                "properties": {
                                    "claim": {"type": "string"},
                                    "sourceUrl": {"type": ["string", "null"], "description": "http(s) URL, or null when the source had no usable URL (see note)"},
                                    "quote": {"type": ["string", "null"], "description": "exact substring of the fetched source text; only present on supported/partial verdicts"},
                                    "verdict": {"enum": ["supported", "partial", "unsupported", "no_source", "off_topic"]},
                                    "relevance": {"enum": ["direct", "related", "off_topic"], "description": "relevance-gate result for this claim against the original question: direct answers it, related is useful context but incomplete, off_topic never reached verification"},
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
                        "uncertainties": {"type": "array", "items": {"type": "string"}},
                        "brief": {"type": ["string", "null"]}
                    }
                },
                {
                    "type": "object",
                    "description": "ask dry-run payload",
                    "required": ["question", "outcome", "dryRun", "plannedFanout", "projectedCost", "projectedWorstCaseCost"],
                    "properties": {
                        "question": {"type": "string"},
                        "outcome": {"enum": ["answered", "partial", "unanswered"]},
                        "dryRun": {"const": true},
                        "plannedFanout": {
                            "type": "object",
                            "properties": {
                                "tier": {"enum": ["quick", "standard", "deep"]},
                                "workers": {"type": "integer"},
                                "decomposeCalls": {"type": "integer"},
                                "maxWorkerRounds": {"type": "integer"},
                                "verify": {"enum": ["adaptive", "paranoid", "off"]},
                                "refinementPass": {"type": "boolean"},
                                "note": {"type": "string"}
                            }
                        },
                        "projectedCost": {"type": "number", "description": "expected-case estimate: one search round per worker, no refinement"},
                        "projectedWorstCaseCost": {"type": "number"}
                    }
                },
                {
                    "type": "object",
                    "description": "doctor report payload",
                    "required": ["schemaVersion", "status", "summary", "checks"],
                    "properties": {
                        "schemaVersion": {"type": "string"},
                        "status": {"enum": ["healthy", "degraded", "broken"]},
                        "summary": {
                            "type": "object",
                            "properties": {
                                "total": {"type": "integer"},
                                "ok": {"type": "integer"},
                                "warn": {"type": "integer"},
                                "error": {"type": "integer"},
                                "fixable": {"type": "integer"}
                            }
                        },
                        "checks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string"},
                                    "category": {"type": "string"},
                                    "severity": {"type": "string"},
                                    "ok": {"type": "boolean"},
                                    "detail": {"type": "string"},
                                    "location": {"type": ["string", "null"]},
                                    "fixAvailable": {"type": "boolean"},
                                    "remediation": {"type": ["object", "null"]}
                                }
                            }
                        },
                        "runId": {"type": ["string", "null"]}
                    }
                },
                {
                    "type": "object",
                    "description": "capabilities payload (free-form object)",
                    "properties": {
                        "name": {"type": "string"},
                        "version": {"type": "string"},
                        "schema": {"type": "string"},
                        "commands": {"type": "array"},
                        "globalFlags": {"type": "array"},
                        "exitCodes": {"type": "object"},
                        "envVars": {"type": "array"},
                        "tiers": {"type": "array"},
                        "budgetUnitCosts": {"type": "object"},
                        "schemas": {"type": "object"}
                    }
                },
                {
                    "type": "object",
                    "description": "schema payload",
                    "properties": {
                        "response": {"type": "object"},
                        "error": {"type": "object"}
                    }
                },
                {
                    "type": "object",
                    "description": "help/version payload",
                    "required": ["text"],
                    "properties": {
                        "text": {"type": "string"}
                    }
                }
            ]},
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
        "$id": "receipts.cli.error.v1",
        "type": "object",
        "required": ["schema", "ok", "command", "requestId", "error"],
        "properties": {
            "schema": {"const": "receipts.cli.error.v1"},
            "ok": {"const": false},
            "command": {"type": "string"},
            "requestId": {"type": "string"},
            "error": {
                "type": "object",
                "required": ["code", "category", "retryable", "provider", "message", "partial", "suggestedFix"],
                "properties": {
                    "code": {"enum": ["usage", "auth", "config", "network", "upstream", "rate_limited", "partial", "no_input"]},
                    "category": {"type": "string"},
                    "retryable": {"type": "boolean"},
                    "provider": {"enum": ["cerebras", "exa", null]},
                    "message": {"type": "string"},
                    "partial": {"type": ["object", "array", "string", "number", "boolean", "null"]},
                    "suggestedFix": {"type": ["string", "null"]}
                }
            }
        }
    })
}
