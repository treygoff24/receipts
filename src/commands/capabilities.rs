use serde_json::json;

use crate::cli::GlobalArgs;
use crate::commands::CommandSuccess;
use crate::config::{
    DEFAULT_API_BASE, DEFAULT_EXA_SEARCH_TYPE, DEFAULT_MAX_CONCURRENCY, DEFAULT_MODEL,
    EXA_SEARCH_TYPES,
};
use crate::envelope::{Budget, CostDollars, Diagnostics, SuccessEnvelope};
use crate::error::ReceiptsError;
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, DECOMPOSE_WORST_CASE_COST, EXTRACT_WORST_CASE_COST,
    RELEVANCE_WORST_CASE_COST, VERIFICATION_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
};

pub fn run(_global: &GlobalArgs) -> Result<CommandSuccess, ReceiptsError> {
    let data = json!({
        "name": "receipts",
        "version": env!("CARGO_PKG_VERSION"),
        "schema": "receipts.cli.capabilities.v1",
        "commands": [
            {
                "name": "ask",
                "usage": "receipts ask <QUESTION>",
                "defaultSubcommand": true,
                "readOnly": true,
                "destructive": false,
                "spendsMoney": true,
                "stdout": "receipts.cli.response.v1",
                "stderrOnError": "receipts.cli.error.v1"
            },
            {
                "name": "doctor",
                "usage": "receipts doctor [--online]",
                "readOnly": true,
                "destructive": false,
                "spendsMoney": true,
                "spendsMoneyNote": "only with --online provider probes"
            },
            {
                "name": "capabilities",
                "usage": "receipts capabilities",
                "readOnly": true,
                "destructive": false,
                "spendsMoney": false
            },
            {
                "name": "schema",
                "usage": "receipts schema [response|error|all]",
                "readOnly": true,
                "destructive": false,
                "spendsMoney": false
            }
        ],
        "globalFlags": [
            {"name": "--json", "type": "bool", "default": false, "description": "force JSON envelope"},
            {"name": "--model", "type": "string", "default": DEFAULT_MODEL},
            {"name": "--depth", "type": "enum", "values": ["quick", "standard", "deep"], "default": "standard"},
            {"name": "--max-seconds", "type": "integer", "minimum": 1, "default": null},
            {"name": "--max-dollars", "type": "number", "minimum": 0, "default": null},
            {"name": "--verify", "type": "enum", "values": ["adaptive", "paranoid", "off"], "default": "adaptive"},
            {"name": "--brief", "type": "bool", "default": false},
            {"name": "--dry-run", "type": "bool", "default": false}
        ],
        "exitCodes": {
            "0": "ok",
            "1": "usage",
            "2": "auth; doctor emits its structured report on stdout even at exit 2",
            "3": "config",
            "4": "network",
            "5": "upstream",
            "6": "rate-limit",
            "10": "partial; budget/partial-driven regardless of claim count; stdout carries ok:true success envelope with data.outcome=partial and budget.hit set; a zero-claim partial means the budget closed before work completed",
            "11": "no-input"
        },
        "envVars": [
            {"name": "CEREBRAS_API_KEY", "requiredFor": ["ask", "doctor --online"], "secret": true},
            {"name": "EXA_API_KEY", "requiredFor": ["ask", "doctor --online"], "secret": true},
            {"name": "RECEIPTS_MODEL", "default": DEFAULT_MODEL},
            {"name": "RECEIPTS_API_BASE", "default": DEFAULT_API_BASE},
            {"name": "RECEIPTS_EXA_BASE", "default": "https://api.exa.ai"},
            {"name": "RECEIPTS_EXA_SEARCH_TYPE", "default": DEFAULT_EXA_SEARCH_TYPE, "allowed": EXA_SEARCH_TYPES},
            {"name": "RECEIPTS_MAX_CONCURRENCY", "default": DEFAULT_MAX_CONCURRENCY}
        ],
        "tiers": [
            {"name": "quick", "workers": 2, "latencyExpectation": "~10s target", "costExpectation": "~$0.05-$0.10 typical", "notes": "two same-question workers with complementary search angles"},
            {"name": "standard", "workers": 4, "latencyExpectation": "~9-15s measured", "costExpectation": "~$0.15 measured", "notes": "default; four decomposed subquestions"},
            {"name": "deep", "workers": 8, "latencyExpectation": "~9s measured", "costExpectation": "~$0.31 measured", "notes": "adaptive verification and refinement pass"}
        ],
        "budgetUnitCosts": {
            "decompose": DECOMPOSE_WORST_CASE_COST,
            "workerRound": WORKER_ROUND_WORST_CASE_COST,
            "extract": EXTRACT_WORST_CASE_COST,
            "relevance": RELEVANCE_WORST_CASE_COST,
            "verify": VERIFICATION_WORST_CASE_COST,
            "contents": CONTENTS_WORST_CASE_COST
        },
        "schemas": {
            "response": "receipts schema response",
            "error": "receipts schema error",
            "all": "receipts schema all"
        }
    });

    Ok(CommandSuccess {
        envelope: SuccessEnvelope::new(
            "capabilities",
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
