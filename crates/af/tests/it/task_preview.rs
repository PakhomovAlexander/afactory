//! Real CLI previews: no dispatch before confirmation; displayed plans use captured bytes.
use crate::task_cli;
use serde_json::Value;
use std::path::Path;
use std::process::Output;

fn cli(repo: &Path, state: &Path, args: &[&str]) -> Output {
    crate::common::af()
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

/// The shell tokens of the copied command that starts on the row holding `start`, joining `\`
/// continuations the way a shell joins pasted lines.
fn copied(display: &str, start: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for row in display.lines().skip_while(|row| !row.contains(start)) {
        let row = &row[row.find("af task").unwrap_or(0)..];
        let continued = row.ends_with(" \\");
        let row = row.strip_suffix(" \\").unwrap_or(row);
        tokens.extend(row.split_whitespace().map(str::to_owned));
        if !continued {
            break;
        }
    }
    tokens
}

/// Issue #157: a Task ID of the longest admitted length stays one shell token in the suggested
/// run action and the Details hint, and the copied commands address the same Task and plan.
#[test]
fn preview_commands_stay_copyable_for_short_and_maximum_length_task_ids() {
    let longest = "a".repeat(128);
    for id in ["pagination-cli", longest.as_str()] {
        let temp = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
        // The Task file stays outside the checkout, as an operator's would.
        let mut ticket: Value =
            serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
        ticket["task_id"] = id.into();
        let file = temp.path().join("ticket.json");
        std::fs::write(&file, serde_json::to_vec(&ticket).unwrap()).unwrap();
        let file = file.to_str().unwrap();
        let preview = cli(&repo, &state, &["task", "start", "--file", file]);
        assert!(
            preview.status.success(),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        let display = String::from_utf8(preview.stdout).unwrap();
        assert!(display.is_ascii(), "{display}");
        assert!(display.contains("Use the same --repo and --state selectors as this preview."));
        // Only the command rows may exceed the layout width; prose still wraps.
        let prose = display.lines().filter(|row| !row.contains("af task "));
        assert!(prose.into_iter().all(|row| row.len() <= 96), "{display}");
        let continued: Vec<_> = display.lines().filter(|r| r.ends_with('\\')).collect();
        assert_eq!(continued, [format!("  af task run {id} \\")], "{display}");

        let planned = value(cli(&repo, &state, &["task", "explain", id, "--json"]));
        assert_eq!(planned["attempts"], 0, "Previewing dispatches nothing");
        let plan = planned["plan_id"].as_str().unwrap();
        let run = copied(&display, "af task run");
        assert_eq!(run, ["af", "task", "run", id, "--confirm-plan", plan]);
        let details = copied(&display, "Details: af task explain");
        assert_eq!(details[..5], ["af", "task", "explain", id, "--tree"]);

        // The copied Details command, with the same state selector, reads the same Task.
        let args: Vec<&str> = details[1..5].iter().map(String::as_str).collect();
        let explained = cli(&repo, &state, &args);
        assert!(explained.status.success());
        let explained = String::from_utf8(explained.stdout).unwrap();
        assert!(explained.contains(&format!("PLAN  {plan}")), "{explained}");
        assert_eq!(copied(&explained, "af task run"), run);

        // The copied run action confirms the exact plan; the fixture's Workers are commands.
        if id == longest {
            let mut args: Vec<&str> = run[1..].iter().map(String::as_str).collect();
            args.push("--json");
            let done = value(cli(&repo, &state, &args));
            assert_eq!(done["result"]["acceptance"], "satisfied");
            assert_eq!(done["plan_id"], plan);
        }
    }
}
