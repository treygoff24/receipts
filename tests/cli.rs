mod common;

use std::path::PathBuf;

use assert_cmd::Command;
use receipts::envelope::SuccessEnvelope;
use receipts::pipeline::{Outcome, ResearchData, Verdict};
use serde::Deserialize;

use common::MockServer;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskData {
    #[serde(flatten)]
    research: ResearchData,
    brief: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DryRunData {
    dry_run: bool,
    planned_fanout: PlannedFanout,
    projected_cost: f64,
    projected_worst_case_cost: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlannedFanout {
    workers: usize,
    refinement_pass: bool,
    note: String,
}

#[derive(Deserialize)]
struct TextData {
    text: String,
}

#[derive(Deserialize)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

#[derive(Deserialize)]
struct DoctorCheck {
    id: String,
    ok: bool,
    detail: String,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    schema: String,
    ok: bool,
    command: String,
    error: ErrorDetail,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorDetail {
    code: String,
    retryable: bool,
    provider: Option<String>,
    message: String,
    suggested_fix: Option<String>,
}

fn stdout<T: for<'de> Deserialize<'de>>(output: &std::process::Output) -> SuccessEnvelope<T> {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &std::process::Output) -> ErrorEnvelope {
    serde_json::from_slice(&output.stderr).unwrap()
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "receipts-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn receipts_cmd(home: &PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("receipts").unwrap();
    cmd.env("HOME", home)
        .env_remove("CEREBRAS_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("RECEIPTS_MODEL")
        .env_remove("RECEIPTS_API_BASE")
        .env_remove("RECEIPTS_EXA_BASE")
        .env_remove("RECEIPTS_EXA_SEARCH_TYPE")
        .env_remove("RECEIPTS_MAX_CONCURRENCY");
    cmd
}

fn mock_server_cmd(home: &PathBuf, server: &MockServer) -> Command {
    let mut cmd = receipts_cmd(home);
    cmd.env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url());
    cmd
}

#[test]
fn quick_ask_runs_against_mock_server_and_reports_metered_cost() {
    let server = MockServer::start();
    let home = temp_home("quick");
    let output = mock_server_cmd(&home, &server)
        .arg("--json")
        .arg("--depth")
        .arg("quick")
        .arg("ask")
        .arg("is the mock fact supported?")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<AskData> = stdout(&output);

    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.command, "ask");
    assert!(!stdout.request_id.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"cost_dollars\""));
    assert_eq!(stdout.data.research.outcome, Outcome::Answered);
    assert_eq!(stdout.data.research.claims[0].verdict, Verdict::Supported);
    assert_eq!(
        stdout.data.research.claims[0].source_url.as_deref(),
        Some("https://example.com/source")
    );
    assert_eq!(
        stdout.data.research.claims[0].quote.as_deref(),
        Some("Mock fact is supported in this source text.")
    );
    assert!(stdout.data.research.search_trail.len() >= 2);

    // Dedup leaves one claim, so two workers produce eight billed chat calls:
    // four worker rounds, two extractions, one relevance gate, and one verifier.
    let expected_model = 8.0 * 0.00485;
    let expected_search = 2.0 * 0.01;
    let expected_total = expected_model + expected_search;
    assert!((stdout.cost_dollars.model - expected_model).abs() < 1e-12);
    assert!((stdout.cost_dollars.search - expected_search).abs() < 1e-12);
    assert!((stdout.cost_dollars.total - expected_total).abs() < 1e-12);
    assert!(!stdout.cost_dollars.estimated);
    assert!(server.request_count() > 0);
}

#[test]
fn unknown_flag_exits_usage_with_suggestion_on_stderr() {
    let home = temp_home("unknown-flag");
    let output = receipts_cmd(&home)
        .arg("--jsno")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = stderr(&output);
    assert_eq!(stderr.schema, "receipts.cli.error.v1");
    assert!(!stderr.ok);
    assert_eq!(stderr.error.code, "usage");
    assert!(stderr.error.message.contains("--jsno"));
    assert_eq!(stderr.error.message.matches("usage error:").count(), 1);
    assert!(stderr.error.suggested_fix.unwrap().contains("--json"));
}

#[test]
fn json_parse_error_keeps_json_and_attributes_flag_value_error_to_ask() {
    let home = temp_home("json-parse-error");
    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("--depth")
        .arg("impossible")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = stderr(&output);
    assert_eq!(stderr.command, "ask");
    assert_eq!(stderr.error.code, "usage");
}

#[cfg(target_os = "macos")]
#[test]
fn json_parse_error_stays_machine_readable_on_a_tty() {
    let output = std::process::Command::new("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            env!("CARGO_BIN_EXE_receipts"),
            "--json",
            "--depth",
            "impossible",
            "ask",
            "what?",
        ])
        .output()
        .unwrap();

    let rendered = String::from_utf8(output.stdout).unwrap();
    let json = rendered
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .find_map(|line| line.find('{').map(|start| &line[start..]))
        .expect("JSON error envelope on PTY");
    let envelope: ErrorEnvelope = serde_json::from_str(json).unwrap();
    assert_eq!(envelope.command, "ask");
    assert_eq!(envelope.error.code, "usage");
}

#[test]
fn invalid_config_has_actionable_suggested_fix() {
    let home = temp_home("invalid-config");
    let output = receipts_cmd(&home)
        .env("RECEIPTS_EXA_SEARCH_TYPE", "impossible")
        .arg("--json")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = stderr(&output);
    assert_eq!(stderr.error.code, "config");
    let fix = stderr.error.suggested_fix.expect("suggestedFix");
    assert!(fix.contains("RECEIPTS_EXA_SEARCH_TYPE"));
    assert!(fix.contains("fast, instant, auto"));
}

fn fault_cmd(home: &PathBuf, base_url: &str) -> Command {
    let mut cmd = receipts_cmd(home);
    cmd.env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", base_url)
        .env("RECEIPTS_EXA_BASE", base_url)
        .env("RECEIPTS_TEST_NO_RETRY_SLEEP", "1");
    cmd
}

fn assert_provider_failure(output: &std::process::Output, exit: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    let stderr = stderr(output);
    assert_eq!(stderr.command, "ask");
    assert_eq!(stderr.error.code, code);
    assert!(stderr.error.retryable);
    assert_eq!(stderr.error.provider.as_deref(), Some("cerebras"));
    assert!(stderr.error.suggested_fix.is_some());
}

#[test]
fn decompose_and_workers_http_500_exit_upstream() {
    let server = MockServer::returning(500);
    let home = temp_home("all-workers-500");
    let output = fault_cmd(&home, server.base_url())
        .args(["--json", "--depth", "standard", "ask", "what?"])
        .output()
        .unwrap();

    assert_provider_failure(&output, 5, "upstream");
}

#[test]
fn all_workers_dead_port_exit_network() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let home = temp_home("all-workers-dead-port");
    let output = fault_cmd(&home, &base_url)
        .args(["--json", "--depth", "quick", "ask", "what?"])
        .output()
        .unwrap();

    assert_provider_failure(&output, 4, "network");
}

#[test]
fn all_workers_http_429_exit_rate_limited() {
    let server = MockServer::returning(429);
    let home = temp_home("all-workers-429");
    let output = fault_cmd(&home, server.base_url())
        .args(["--json", "--depth", "quick", "ask", "what?"])
        .output()
        .unwrap();

    assert_provider_failure(&output, 6, "rate_limited");
}

#[test]
fn one_worker_failure_with_surviving_claim_exits_zero_with_uncertainty() {
    let server = MockServer::failing_one_worker();
    let home = temp_home("one-worker-fails");
    let output = fault_cmd(&home, server.base_url())
        .args(["--json", "--depth", "quick", "ask", "what?"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<AskData> = stdout(&output);
    assert!(!stdout.data.research.claims.is_empty());
    assert!(
        stdout
            .data
            .research
            .uncertainties
            .iter()
            .any(|item| item.contains("worker failed"))
    );
}

#[test]
fn missing_keys_exit_auth_before_any_request() {
    let server = MockServer::start();
    let home = temp_home("missing-keys");
    let output = receipts_cmd(&home)
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
        .arg("--json")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = stderr(&output);
    assert_eq!(stderr.error.code, "auth");
    assert_eq!(stderr.error.provider.as_deref(), Some("cerebras"));
    assert!(stderr.error.message.contains("CEREBRAS_API_KEY"));
    assert_eq!(server.request_count(), 0);
    assert!(
        stderr
            .error
            .suggested_fix
            .is_some_and(|s| s.contains("CEREBRAS_API_KEY")),
        "suggestedFix must mention CEREBRAS_API_KEY"
    );
}

#[test]
fn missing_question_exits_no_input() {
    let home = temp_home("missing-question");
    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("ask")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(11));
    assert!(output.stdout.is_empty());
    let stderr = stderr(&output);
    assert_eq!(stderr.error.code, "no_input");
}

#[test]
fn dry_run_outputs_estimated_plan_and_makes_zero_requests() {
    let server = MockServer::start();
    let home = temp_home("dry-run");
    let output = receipts_cmd(&home)
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
        .arg("--json")
        .arg("--dry-run")
        .arg("--depth")
        .arg("quick")
        .arg("ask")
        .arg("what would run?")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<DryRunData> = stdout(&output);
    assert!(stdout.ok);
    assert!(stdout.data.dry_run);
    assert_eq!(stdout.data.planned_fanout.workers, 2);
    assert!(stdout.cost_dollars.estimated);
    assert_eq!(server.request_count(), 0);
}

#[test]
fn help_with_json_emits_success_envelope() {
    let home = temp_home("help-json");
    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("--help")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout: SuccessEnvelope<TextData> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.command, "help");
    assert!(!stdout.data.text.is_empty(), "data.text must be non-empty");
}

#[test]
fn version_with_json_emits_success_envelope() {
    let home = temp_home("version-json");
    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("--version")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout: SuccessEnvelope<TextData> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.command, "version");
    assert!(!stdout.data.text.is_empty(), "data.text must be non-empty");
}

#[test]
fn exit_10_partial_on_budget_hit_with_zero_claims() {
    let server = MockServer::start();
    let home = temp_home("exit-10");
    let output = mock_server_cmd(&home, &server)
        .arg("--json")
        .arg("--depth")
        .arg("quick")
        .arg("--max-dollars")
        .arg("0.001")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(10));
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<AskData> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.budget.hit.as_deref(), Some("dollars"));
    assert_eq!(stdout.data.research.outcome, Outcome::Partial);
}

#[test]
fn dry_run_quick_projection_matches_closed_form_sum() {
    let home = temp_home("dry-run-quick-projection");

    let decompose_cost = 0.001_f64;
    let worker_round_cost = 0.03_f64;
    let extract_cost = 0.01_f64;
    let relevance_cost = 0.001_f64;
    let verify_cost = 0.002_f64;
    let contents_cost = 0.005_f64;
    let search_call_cost = 0.01_f64;
    let max_claims = 15_f64;
    let expected_claims = 3_f64;

    let workers = 2_f64;
    let max_rounds = 5_f64;
    let decompose_calls = 0_f64;
    let verify_mult = 1.0_f64;
    let relevance_mult = 1.0_f64;

    // Relevance and verify scale off claims-per-worker, not worker count: a
    // worker's extracted answer can produce up to MAX_CLAIMS_PER_WORKER
    // claims, each of which pays its own relevance + verify call.
    let model = decompose_calls * decompose_cost
        + workers * max_rounds * worker_round_cost
        + workers * extract_cost
        + workers * max_claims * relevance_mult * relevance_cost
        + workers * max_claims * verify_mult * verify_cost;
    let search = workers * max_rounds * search_call_cost + workers * contents_cost;
    let total = model + search;

    // costDollars carries the expected case: one search round per worker,
    // and a smaller documented claims-per-worker assumption.
    let expected_rounds = 1_f64;
    let model_expected = decompose_calls * decompose_cost
        + workers * expected_rounds * worker_round_cost
        + workers * extract_cost
        + workers * expected_claims * relevance_mult * relevance_cost
        + workers * expected_claims * verify_mult * verify_cost;
    let search_expected = workers * expected_rounds * search_call_cost + workers * contents_cost;
    let total_expected = model_expected + search_expected;

    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("--dry-run")
        .arg("--depth")
        .arg("quick")
        .arg("ask")
        .arg("what would run?")
        .output()
        .unwrap();

    let stdout: SuccessEnvelope<DryRunData> = stdout(&output);
    assert!(stdout.ok);
    let projected = stdout.data.projected_worst_case_cost;
    assert!(
        (projected - total).abs() < 1e-12,
        "projected {projected} != expected {total}"
    );
    let expected = stdout.data.projected_cost;
    assert!(
        (expected - total_expected).abs() < 1e-12,
        "projectedCost {expected} != expected {total_expected}"
    );
    assert!((stdout.cost_dollars.model - model_expected).abs() < 1e-12);
    assert!((stdout.cost_dollars.search - search_expected).abs() < 1e-12);
    assert!((stdout.cost_dollars.total - total_expected).abs() < 1e-12);
    assert!(stdout.cost_dollars.search > 0.0);
    assert!(expected < total, "expected case must undercut worst case");
}

#[test]
fn dry_run_deep_includes_refinement_pass() {
    let home = temp_home("dry-run-deep");

    let decompose_cost = 0.001_f64;
    let worker_round_cost = 0.03_f64;
    let extract_cost = 0.01_f64;
    let relevance_cost = 0.001_f64;
    let verify_cost = 0.002_f64;
    let contents_cost = 0.005_f64;
    let search_call_cost = 0.01_f64;
    let max_claims = 15_f64;
    let expected_claims = 3_f64;

    let workers = 8_f64;
    let max_rounds = 5_f64;
    let decompose_calls = 1_f64;
    let verify_mult = 1.0_f64;
    let relevance_mult = 1.0_f64;

    let model = decompose_calls * decompose_cost
        + workers * max_rounds * worker_round_cost
        + workers * extract_cost
        + workers * max_claims * relevance_mult * relevance_cost
        + workers * max_claims * verify_mult * verify_cost;
    // Refinement worst case re-runs worker rounds AND a second
    // extract/relevance/verify pass over the refined claims.
    let refinement = workers * max_rounds * worker_round_cost
        + workers * extract_cost
        + workers * max_claims * relevance_mult * relevance_cost
        + workers * max_claims * verify_mult * verify_cost;
    let search = workers * max_rounds * search_call_cost + workers * contents_cost;
    let total = model + refinement + search;

    let output = receipts_cmd(&home)
        .arg("--json")
        .arg("--dry-run")
        .arg("--depth")
        .arg("deep")
        .arg("ask")
        .arg("what would run?")
        .output()
        .unwrap();

    let stdout: SuccessEnvelope<DryRunData> = stdout(&output);
    assert!(stdout.ok);
    let projected = stdout.data.projected_worst_case_cost;
    assert!(
        (projected - total).abs() < 1e-12,
        "projected {projected} != expected {total}"
    );
    assert!(stdout.data.planned_fanout.refinement_pass);
    assert_eq!(
        stdout.data.planned_fanout.note,
        "worst case incl. refinement"
    );

    let expected_rounds = 1_f64;
    let model_expected = decompose_calls * decompose_cost
        + workers * expected_rounds * worker_round_cost
        + workers * extract_cost
        + workers * expected_claims * relevance_mult * relevance_cost
        + workers * expected_claims * verify_mult * verify_cost;
    let search_expected = workers * expected_rounds * search_call_cost + workers * contents_cost;
    let total_expected = model_expected + search_expected;
    let expected = stdout.data.projected_cost;
    assert!(
        (expected - total_expected).abs() < 1e-12,
        "projectedCost {expected} != expected {total_expected}"
    );
    assert!((stdout.cost_dollars.model - model_expected).abs() < 1e-12);
    assert!((stdout.cost_dollars.search - search_expected).abs() < 1e-12);
    assert!((stdout.cost_dollars.total - total_expected).abs() < 1e-12);
    assert!(
        expected < projected,
        "expected case must undercut worst case"
    );
}

#[test]
fn doctor_online_happy_path_against_mock() {
    let server = MockServer::start();
    let home = temp_home("doctor-online-ok");
    let output = mock_server_cmd(&home, &server)
        .arg("--json")
        .arg("doctor")
        .arg("--online")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<DoctorReport> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.command, "doctor");

    let cerebras_check = stdout
        .data
        .checks
        .iter()
        .find(|check| check.id == "online.cerebras")
        .expect("online.cerebras check exists");
    assert!(cerebras_check.ok);
    let exa_check = stdout
        .data
        .checks
        .iter()
        .find(|check| check.id == "online.exa")
        .expect("online.exa check exists");
    assert!(exa_check.ok);
}

#[test]
fn doctor_online_bad_exa_key_exits_2() {
    let server = MockServer::start();
    let home = temp_home("doctor-online-bad-exa");
    let output = mock_server_cmd(&home, &server)
        .env("EXA_API_KEY", "bad-exa")
        .arg("--json")
        .arg("doctor")
        .arg("--online")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<DoctorReport> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.command, "doctor");

    let exa_check = stdout
        .data
        .checks
        .iter()
        .find(|check| check.id == "online.exa")
        .expect("online.exa check exists");
    assert!(!exa_check.ok);
    assert!(
        exa_check.detail.contains("exa"),
        "exa check should name the provider"
    );
}

#[test]
fn brief_wired_to_pipeline_synthesis_against_mock() {
    let server = MockServer::start();
    let home = temp_home("brief");
    let output = mock_server_cmd(&home, &server)
        .arg("--json")
        .arg("--depth")
        .arg("quick")
        .arg("--brief")
        .arg("ask")
        .arg("is the mock fact supported?")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout: SuccessEnvelope<AskData> = stdout(&output);
    assert_eq!(stdout.schema, "receipts.cli.response.v1");
    assert!(stdout.ok);
    assert_eq!(stdout.data.research.outcome, Outcome::Answered);
    assert!(
        stdout.data.brief.is_some_and(|brief| !brief.is_empty()),
        "brief must be present and non-empty"
    );
}
