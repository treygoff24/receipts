mod common;

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

use common::MockServer;

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

#[test]
fn quick_ask_runs_against_mock_server_and_reports_metered_cost() {
    let server = MockServer::start();
    let home = temp_home("quick");
    let output = receipts_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "ask");
    assert!(stdout.get("requestId").is_some());
    assert!(stdout.get("costDollars").is_some());
    assert!(stdout.get("cost_dollars").is_none());
    assert_eq!(stdout["data"]["outcome"], "answered");
    assert_eq!(stdout["data"]["claims"][0]["verdict"], "supported");
    assert_eq!(
        stdout["data"]["claims"][0]["sourceUrl"],
        "https://example.com/source"
    );
    assert_eq!(
        stdout["data"]["claims"][0]["quote"],
        "Mock fact is supported in this source text."
    );
    assert!(stdout["data"]["searchTrail"].as_array().unwrap().len() >= 2);

    // Dedup leaves one claim, so two workers produce eight billed chat calls:
    // four worker rounds, two extractions, one relevance gate, and one verifier.
    let expected_model = 8.0 * 0.00485;
    let expected_search = 2.0 * 0.01;
    let expected_total = expected_model + expected_search;
    assert!((stdout["costDollars"]["model"].as_f64().unwrap() - expected_model).abs() < 1e-12);
    assert!((stdout["costDollars"]["search"].as_f64().unwrap() - expected_search).abs() < 1e-12);
    assert!((stdout["costDollars"]["total"].as_f64().unwrap() - expected_total).abs() < 1e-12);
    assert_eq!(stdout["costDollars"]["estimated"], false);
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
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["schema"], "receipts.cli.error.v1");
    assert_eq!(stderr["ok"], false);
    assert_eq!(stderr["error"]["code"], "usage");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--jsno")
    );
    assert!(
        stderr["error"]["suggestedFix"]
            .as_str()
            .unwrap()
            .contains("--json")
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
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "auth");
    assert_eq!(stderr["error"]["provider"], "cerebras");
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap()
            .contains("CEREBRAS_API_KEY")
    );
    assert_eq!(server.request_count(), 0);
    assert!(
        stderr["error"]["suggestedFix"]
            .as_str()
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
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "no_input");
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["dryRun"], true);
    assert_eq!(stdout["data"]["plannedFanout"]["workers"], 2);
    assert_eq!(stdout["costDollars"]["estimated"], true);
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "help");
    assert!(
        stdout["data"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "data.text must be non-empty"
    );
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "version");
    assert!(
        stdout["data"]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "data.text must be non-empty"
    );
}

#[test]
fn exit_10_partial_on_budget_hit_with_zero_claims() {
    let server = MockServer::start();
    let home = temp_home("exit-10");
    let output = receipts_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["budget"]["hit"], "dollars");
    assert_eq!(stdout["data"]["outcome"], "partial");
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

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    let projected = stdout["data"]["projectedWorstCaseCost"].as_f64().unwrap();
    assert!(
        (projected - total).abs() < 1e-12,
        "projected {projected} != expected {total}"
    );
    let expected = stdout["data"]["projectedCost"].as_f64().unwrap();
    assert!(
        (expected - total_expected).abs() < 1e-12,
        "projectedCost {expected} != expected {total_expected}"
    );
    assert!((stdout["costDollars"]["model"].as_f64().unwrap() - model_expected).abs() < 1e-12);
    assert!((stdout["costDollars"]["search"].as_f64().unwrap() - search_expected).abs() < 1e-12);
    assert!((stdout["costDollars"]["total"].as_f64().unwrap() - total_expected).abs() < 1e-12);
    assert!(stdout["costDollars"]["search"].as_f64().unwrap() > 0.0);
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

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["ok"], true);
    let projected = stdout["data"]["projectedWorstCaseCost"].as_f64().unwrap();
    assert!(
        (projected - total).abs() < 1e-12,
        "projected {projected} != expected {total}"
    );
    assert_eq!(stdout["data"]["plannedFanout"]["refinementPass"], true);
    assert_eq!(
        stdout["data"]["plannedFanout"]["note"],
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
    let expected = stdout["data"]["projectedCost"].as_f64().unwrap();
    assert!(
        (expected - total_expected).abs() < 1e-12,
        "projectedCost {expected} != expected {total_expected}"
    );
    assert!((stdout["costDollars"]["model"].as_f64().unwrap() - model_expected).abs() < 1e-12);
    assert!((stdout["costDollars"]["search"].as_f64().unwrap() - search_expected).abs() < 1e-12);
    assert!((stdout["costDollars"]["total"].as_f64().unwrap() - total_expected).abs() < 1e-12);
    assert!(
        expected < projected,
        "expected case must undercut worst case"
    );
}

#[test]
fn doctor_online_happy_path_against_mock() {
    let server = MockServer::start();
    let home = temp_home("doctor-online-ok");
    let output = receipts_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
        .arg("--json")
        .arg("doctor")
        .arg("--online")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "doctor");

    let checks = stdout["data"]["checks"].as_array().unwrap();
    let cerebras_check = checks
        .iter()
        .find(|c| c["id"] == "online.cerebras")
        .expect("online.cerebras check exists");
    assert_eq!(cerebras_check["ok"], true);
    let exa_check = checks
        .iter()
        .find(|c| c["id"] == "online.exa")
        .expect("online.exa check exists");
    assert_eq!(exa_check["ok"], true);
}

#[test]
fn doctor_online_bad_exa_key_exits_2() {
    let server = MockServer::start();
    let home = temp_home("doctor-online-bad-exa");
    let output = receipts_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "bad-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
        .arg("--json")
        .arg("doctor")
        .arg("--online")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["command"], "doctor");

    let checks = stdout["data"]["checks"].as_array().unwrap();
    let exa_check = checks
        .iter()
        .find(|c| c["id"] == "online.exa")
        .expect("online.exa check exists");
    assert_eq!(exa_check["ok"], false);
    assert!(
        exa_check["detail"].as_str().unwrap().contains("exa"),
        "exa check should name the provider"
    );
}

#[test]
fn brief_wired_to_pipeline_synthesis_against_mock() {
    let server = MockServer::start();
    let home = temp_home("brief");
    let output = receipts_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECEIPTS_API_BASE", server.base_url())
        .env("RECEIPTS_EXA_BASE", server.base_url())
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
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "receipts.cli.response.v1");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["data"]["outcome"], "answered");
    assert!(
        stdout["data"]["brief"].as_str().is_some(),
        "brief field must be present"
    );
    assert!(
        !stdout["data"]["brief"].as_str().unwrap().is_empty(),
        "brief must be non-empty"
    );
}
