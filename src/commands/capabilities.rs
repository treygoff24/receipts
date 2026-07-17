use serde::Serialize;

use crate::cli::GlobalArgs;
use crate::commands::CommandSuccess;
use crate::config::{
    DEFAULT_API_BASE, DEFAULT_EXA_SEARCH_TYPE, DEFAULT_MAX_CONCURRENCY, DEFAULT_MODEL,
    EXA_SEARCH_TYPES,
};
use crate::error::ReceiptsError;
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, DECOMPOSE_WORST_CASE_COST, EXTRACT_WORST_CASE_COST,
    RELEVANCE_WORST_CASE_COST, VERIFICATION_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    name: &'static str,
    version: &'static str,
    schema: &'static str,
    commands: [CommandCapability; 4],
    global_flags: [GlobalFlag; 8],
    exit_codes: ExitCodes,
    env_vars: [EnvVar; 7],
    tiers: [Tier; 3],
    budget_unit_costs: BudgetUnitCosts,
    schemas: Schemas,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandCapability {
    name: &'static str,
    usage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_subcommand: Option<bool>,
    read_only: bool,
    destructive: bool,
    spends_money: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    spends_money_note: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_on_error: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum CapabilityValue {
    Bool(bool),
    String(&'static str),
    Unsigned(u32),
}

#[derive(Serialize)]
struct GlobalFlag {
    name: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum: Option<u8>,
    default: Option<CapabilityValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'static str>,
}

#[derive(Serialize)]
struct ExitCodes {
    #[serde(rename = "0")]
    ok: &'static str,
    #[serde(rename = "1")]
    usage: &'static str,
    #[serde(rename = "2")]
    auth: &'static str,
    #[serde(rename = "3")]
    config: &'static str,
    #[serde(rename = "4")]
    network: &'static str,
    #[serde(rename = "5")]
    upstream: &'static str,
    #[serde(rename = "6")]
    rate_limit: &'static str,
    #[serde(rename = "10")]
    partial: &'static str,
    #[serde(rename = "11")]
    no_input: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvVar {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_for: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<CapabilityValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tier {
    name: &'static str,
    workers: usize,
    latency_expectation: &'static str,
    cost_expectation: &'static str,
    notes: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BudgetUnitCosts {
    decompose: f64,
    worker_round: f64,
    extract: f64,
    relevance: f64,
    verify: f64,
    contents: f64,
}

#[derive(Serialize)]
struct Schemas {
    response: &'static str,
    error: &'static str,
    all: &'static str,
}

pub fn run(_global: &GlobalArgs) -> Result<CommandSuccess<Capabilities>, ReceiptsError> {
    let command = |name, usage, default_subcommand, spends_money| CommandCapability {
        name,
        usage,
        default_subcommand,
        read_only: true,
        destructive: false,
        spends_money,
        spends_money_note: None,
        stdout: None,
        stderr_on_error: None,
    };
    let flag = |name, kind, default| GlobalFlag {
        name,
        kind,
        values: None,
        minimum: None,
        default,
        description: None,
    };
    let env_var = |name, default| EnvVar {
        name,
        required_for: None,
        secret: None,
        default,
        allowed: None,
    };

    let mut ask = command("ask", "receipts ask <QUESTION>", Some(true), true);
    ask.stdout = Some("receipts.cli.response.v1");
    ask.stderr_on_error = Some("receipts.cli.error.v1");
    let mut doctor = command("doctor", "receipts doctor [--online]", None, true);
    doctor.spends_money_note = Some("only with --online provider probes");

    let mut json_flag = flag("--json", "bool", Some(CapabilityValue::Bool(false)));
    json_flag.description = Some("force JSON envelope");
    let mut depth = flag("--depth", "enum", Some(CapabilityValue::String("standard")));
    depth.values = Some(&["quick", "standard", "deep"]);
    let mut max_seconds = flag("--max-seconds", "integer", None);
    max_seconds.minimum = Some(1);
    let mut max_dollars = flag("--max-dollars", "number", None);
    max_dollars.minimum = Some(0);
    let mut verify = flag(
        "--verify",
        "enum",
        Some(CapabilityValue::String("adaptive")),
    );
    verify.values = Some(&["adaptive", "paranoid", "off"]);

    let required_for = &["ask", "doctor --online"];
    let mut cerebras_key = env_var("CEREBRAS_API_KEY", None);
    cerebras_key.required_for = Some(required_for);
    cerebras_key.secret = Some(true);
    let mut exa_key = env_var("EXA_API_KEY", None);
    exa_key.required_for = Some(required_for);
    exa_key.secret = Some(true);
    let mut exa_search_type = env_var(
        "RECEIPTS_EXA_SEARCH_TYPE",
        Some(CapabilityValue::String(DEFAULT_EXA_SEARCH_TYPE)),
    );
    exa_search_type.allowed = Some(EXA_SEARCH_TYPES);

    let data = Capabilities {
        name: "receipts",
        version: env!("CARGO_PKG_VERSION"),
        schema: "receipts.cli.capabilities.v1",
        commands: [
            ask,
            doctor,
            command("capabilities", "receipts capabilities", None, false),
            command(
                "schema",
                "receipts schema [response|error|all]",
                None,
                false,
            ),
        ],
        global_flags: [
            json_flag,
            flag(
                "--model",
                "string",
                Some(CapabilityValue::String(DEFAULT_MODEL)),
            ),
            depth,
            max_seconds,
            max_dollars,
            verify,
            flag("--brief", "bool", Some(CapabilityValue::Bool(false))),
            flag("--dry-run", "bool", Some(CapabilityValue::Bool(false))),
        ],
        exit_codes: ExitCodes {
            ok: "ok",
            usage: "usage",
            auth: "auth; doctor emits its structured report on stdout even at exit 2",
            config: "config",
            network: "network",
            upstream: "upstream",
            rate_limit: "rate-limit",
            partial: "partial; budget/partial-driven regardless of claim count; stdout carries ok:true success envelope with data.outcome=partial and budget.hit set; a zero-claim partial means the budget closed before work completed",
            no_input: "no-input",
        },
        env_vars: [
            cerebras_key,
            exa_key,
            env_var(
                "RECEIPTS_MODEL",
                Some(CapabilityValue::String(DEFAULT_MODEL)),
            ),
            env_var(
                "RECEIPTS_API_BASE",
                Some(CapabilityValue::String(DEFAULT_API_BASE)),
            ),
            env_var(
                "RECEIPTS_EXA_BASE",
                Some(CapabilityValue::String("https://api.exa.ai")),
            ),
            exa_search_type,
            env_var(
                "RECEIPTS_MAX_CONCURRENCY",
                Some(CapabilityValue::Unsigned(DEFAULT_MAX_CONCURRENCY)),
            ),
        ],
        tiers: [
            Tier {
                name: "quick",
                workers: 2,
                latency_expectation: "~10s target",
                cost_expectation: "~$0.05-$0.10 typical",
                notes: "two same-question workers with complementary search angles",
            },
            Tier {
                name: "standard",
                workers: 4,
                latency_expectation: "~9-15s measured",
                cost_expectation: "~$0.15 measured",
                notes: "default; four decomposed subquestions",
            },
            Tier {
                name: "deep",
                workers: 8,
                latency_expectation: "~9s measured",
                cost_expectation: "~$0.31 measured",
                notes: "adaptive verification and refinement pass",
            },
        ],
        budget_unit_costs: BudgetUnitCosts {
            decompose: DECOMPOSE_WORST_CASE_COST,
            worker_round: WORKER_ROUND_WORST_CASE_COST,
            extract: EXTRACT_WORST_CASE_COST,
            relevance: RELEVANCE_WORST_CASE_COST,
            verify: VERIFICATION_WORST_CASE_COST,
            contents: CONTENTS_WORST_CASE_COST,
        },
        schemas: Schemas {
            response: "receipts schema response",
            error: "receipts schema error",
            all: "receipts schema all",
        },
    };

    Ok(CommandSuccess::free("capabilities", data))
}
