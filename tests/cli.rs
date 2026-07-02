mod common;

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

use common::MockServer;

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "recon-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn recon_cmd(home: &PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("recon").unwrap();
    cmd.env("HOME", home)
        .env_remove("CEREBRAS_API_KEY")
        .env_remove("EXA_API_KEY")
        .env_remove("RECON_MODEL")
        .env_remove("RECON_API_BASE")
        .env_remove("RECON_EXA_BASE")
        .env_remove("EXA_API_BASE")
        .env_remove("RECON_MAX_CONCURRENCY");
    cmd
}

#[test]
fn quick_ask_runs_against_mock_server_and_reports_metered_cost() {
    let server = MockServer::start();
    let home = temp_home("quick");
    let output = recon_cmd(&home)
        .env("CEREBRAS_API_KEY", "fake-cerebras")
        .env("EXA_API_KEY", "fake-exa")
        .env("RECON_API_BASE", server.base_url())
        .env("RECON_EXA_BASE", server.base_url())
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

    assert_eq!(stdout["schema"], "recon.cli.response.v1");
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
    assert!(stdout["data"]["searchTrail"].as_array().unwrap().len() >= 2);

    let expected_model = 7.0 * 0.00485;
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
    let output = recon_cmd(&home)
        .arg("--jsno")
        .arg("ask")
        .arg("what?")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["schema"], "recon.cli.error.v1");
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
    assert!(
        stderr["error"]["details"]["suggestedFix"]
            .as_str()
            .unwrap()
            .contains("--json")
    );
}

#[test]
fn missing_keys_exit_auth_before_any_request() {
    let server = MockServer::start();
    let home = temp_home("missing-keys");
    let output = recon_cmd(&home)
        .env("RECON_API_BASE", server.base_url())
        .env("RECON_EXA_BASE", server.base_url())
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
}

#[test]
fn missing_question_exits_no_input() {
    let home = temp_home("missing-question");
    let output = recon_cmd(&home).arg("--json").arg("ask").output().unwrap();

    assert_eq!(output.status.code(), Some(11));
    assert!(output.stdout.is_empty());
    let stderr: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(stderr["error"]["code"], "no_input");
}

#[test]
fn dry_run_outputs_estimated_plan_and_makes_zero_requests() {
    let server = MockServer::start();
    let home = temp_home("dry-run");
    let output = recon_cmd(&home)
        .env("RECON_API_BASE", server.base_url())
        .env("RECON_EXA_BASE", server.base_url())
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
