//! Real CLI previews: no dispatch before confirmation; displayed plans use captured bytes.
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};
#[path = "support/task_cli.rs"]
mod task_cli;

fn cli(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}
fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn without_time(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("TIME "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn start_previews_then_exact_confirmation_runs_and_finished_replay_spends_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let initial = cli(&repo, &state, &["task", "start", "--file", "ticket.json"]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let display = String::from_utf8(initial.stdout).unwrap();
    assert!(display.contains("TASK  pagination-cli"));
    assert!(display.contains("PIPE  fixture/implementation@1.0.0"));
    assert!(display.contains("confirmation required"));
    assert!(display.contains("--confirm-plan sha256:"));
    assert!(display.is_ascii() && display.lines().all(|line| line.len() <= 96));
    let before = value(cli(
        &repo,
        &state,
        &["task", "explain", "pagination-cli", "--json"],
    ));
    assert_eq!(before["attempts"], 0);
    assert_eq!(before["chargeable_tokens"], "0");
    let id = before["plan_id"].as_str().unwrap();
    let refused = cli(&repo, &state, &["task", "run", "pagination-cli", "--json"]);
    assert_eq!(refused.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&refused.stdout).expect("Exactly one JSON error");
    assert_eq!(error["schema"], "af/error@1");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--confirm-plan"));
    let stale = format!("sha256:{}", "0".repeat(64));
    let refused = cli(
        &repo,
        &state,
        &["task", "run", "pagination-cli", "--confirm-plan", &stale],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("differs"));
    let unchanged = value(cli(
        &repo,
        &state,
        &["task", "explain", "pagination-cli", "--json"],
    ));
    assert_eq!(
        before, unchanged,
        "Refusing confirmation must not dispatch or change Task history"
    );
    let done = value(cli(
        &repo,
        &state,
        &[
            "task",
            "run",
            "pagination-cli",
            "--confirm-plan",
            id,
            "--json",
        ],
    ));
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["attempts"], 3);
    let replay = value(cli(
        &repo,
        &state,
        &["task", "run", "pagination-cli", "--json"],
    ));
    assert_eq!(replay, done);
    let finished = cli(
        &repo,
        &state,
        &["task", "explain", "pagination-cli", "--plan", id, "--tree"],
    );
    assert!(finished.status.success());
    let finished = String::from_utf8(finished.stdout).unwrap();
    assert!(finished.contains("STATE finished"));
    assert!(!finished.contains("Approve and run"));
}

#[test]
fn tree_preserves_embedded_calls_conditions_and_captured_authority() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "embedded-review");
    let plan = value(cli(
        &repo,
        &state,
        &["task", "start", "--file", "ticket.json", "--json"],
    ));
    assert_eq!(plan["attempts"], 0, "JSON does not opt into execution");
    let args = ["task", "explain", "pagination-cli", "--tree"];
    let output = cli(&repo, &state, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    assert!(tree.contains("review: call fixture/review@1.0.0"), "{tree}");
    assert!(tree.contains("correctness  command Worker"), "{tree}");
    assert!(tree.contains("bugs  command Worker"), "{tree}");
    assert!(tree.contains("[if ") && tree.contains("=passed]"), "{tree}");
    assert!(
        tree.contains("OUT   evaluation, snapshot, verification"),
        "{tree}"
    );
    assert!(tree.lines().all(|line| line.len() <= 96));
    std::fs::remove_file(repo.join(".af/task-catalog.toml")).unwrap();
    let after = cli(&repo, &state, &args);
    assert!(after.status.success());
    assert_eq!(
        without_time(&tree),
        without_time(&String::from_utf8(after.stdout).unwrap())
    );
    let historical = cli(
        &repo,
        &state,
        &[
            "task",
            "explain",
            "pagination-cli",
            "--tree",
            "--plan",
            plan["plan_id"].as_str().unwrap(),
        ],
    );
    assert!(historical.status.success());
    let inspected = value(cli(
        &repo,
        &state,
        &["task", "explain", "pagination-cli", "--json"],
    ));
    assert_eq!(plan, inspected);
}
