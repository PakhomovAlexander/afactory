//! Real command Workers exercise shared Task summaries through fresh read-only CLI processes.
use review_store::{Cas, EventStore};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common_review_summaries/recovery.rs"]
mod recovery;

fn checked(output: Output, expected: i32) -> Output {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git(repo: &Path, home: &Path, args: &[&str]) {
    checked(
        Command::new("git")
            .current_dir(repo)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap(),
        0,
    );
}

fn invoke(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .env("HOME", home)
        .args(["review"])
        .args(args)
        .output()
        .unwrap()
}

fn fixture(directory: &Path, delayed: bool) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let repo = directory.join("repo");
    let home = directory.join("home");
    let root = directory.join("campaigns");
    let state = root.join("timing");
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo.join("source.txt"), "reviewed implementation\n").unwrap();
    let pipeline = r#"version = 2
[subject]
kind = "whole-tree"
[[checks]]
name = "noop"
program = "/bin/sh"
args = [{ value = "-c" }, { value = "true" }]
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/sh", args = [{ value = "-c" }, { value = '''cat >/dev/null; printf '%s' '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}' ''' }] }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["reviewer"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }
]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "reviewer", port = "gate" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "gather", port = "reviewer" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
[convergence]
clean_rounds = 2
max_rounds = 3
gate = "major"
"#;
    let pipeline = if delayed {
        pipeline.replace(
            "cat >/dev/null; printf",
            "cat >/dev/null; /bin/sleep 2; printf",
        )
    } else {
        pipeline.to_string()
    };
    std::fs::write(repo.join(".af/pipelines/review.toml"), &pipeline).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version=1\n[project]\nname='timing'\nmin_af='0.6'\n[defaults]\npipeline='review'\n",
    )
    .unwrap();
    let mut lock = review_config::lock::Lockfile::empty();
    lock.pipelines.insert(
        "review".into(),
        review_config::lock::Pin {
            version: "1.0.0".into(),
            digest: review_store::canonical::blob_content_id(pipeline.as_bytes()),
        },
    );
    std::fs::write(repo.join(".af/af.lock"), lock.to_toml()).unwrap();
    git(&repo, &home, &["init", "-q"]);
    git(
        &repo,
        &home,
        &["config", "user.email", "fixture@example.test"],
    );
    git(&repo, &home, &["config", "user.name", "Fixture"]);
    git(&repo, &home, &["add", "."]);
    git(&repo, &home, &["commit", "-qm", "captured authority"]);
    (repo, home, root, state)
}

#[test]
fn common_two_round_timing_and_attempt_summaries_reopen_without_synthetic_legacy_rows() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, root, state) = fixture(directory.path(), false);
    let state_flag = state.to_str().unwrap();
    let mut checkpoints = Vec::new();
    for code in [3, 0] {
        checked(
            invoke(
                &repo,
                &home,
                &[
                    "run",
                    "--campaign",
                    "timing",
                    "--state",
                    state_flag,
                    "--pipeline",
                    ".af/pipelines/review.toml",
                    "--policy-rev",
                    "HEAD",
                    "--heavy",
                ],
            ),
            code,
        );
        let cas = Cas::open_existing(state.join("cas")).unwrap();
        let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let tasks = store.task_ids(&cas).unwrap();
        assert_eq!(tasks.len(), 1);
        checkpoints.push(store.task_projection(&cas, &tasks[0]).unwrap().unwrap());
    }

    let first = &checkpoints[0];
    let second = &checkpoints[1];
    assert_eq!(first.task_id, second.task_id);
    assert_eq!(first.revision.revision, 1);
    assert_eq!(second.revision.revision, 2);
    assert_eq!(
        second.revision.previous_revision_id.as_deref(),
        Some(first.revision_id.as_str())
    );
    assert_eq!(first.revision.limits, second.revision.limits);
    assert_eq!(first.revision.authority, second.revision.authority);
    assert_eq!(first.revision.provenance, second.revision.provenance);
    assert_eq!(second.review_handoffs.len(), 1);
    let handoff = &second.review_handoffs[0].1;
    assert_eq!(handoff.predecessor_revision_id, first.revision_id);
    assert_eq!(Some(&handoff.predecessor_plan_id), first.plan_id.as_ref());
    assert_eq!(handoff.successor_revision_id, second.revision_id);
    assert_eq!(Some(&handoff.successor_plan_id), second.plan_id.as_ref());
    let first_execution = first.execution.as_ref().unwrap();
    let second_execution = second.execution.as_ref().unwrap();
    assert_eq!(first_execution.budget.begun_attempts(), 2);
    assert_eq!(second_execution.budget.begun_attempts(), 4);
    for attempt in first_execution.attempt_accounting() {
        let current = second_execution
            .attempt_accounting()
            .into_iter()
            .find(|current| current.attempt_id == attempt.attempt_id)
            .unwrap();
        assert_eq!(current.invocation_id, attempt.invocation_id);
        assert_eq!(current.plan_id, attempt.plan_id);
        assert_eq!(current.reservation, attempt.reservation);
        assert_eq!(current.result, attempt.result);
        assert_eq!(current.charged_tokens, attempt.charged_tokens);
    }

    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let tasks = store.task_ids(&cas).unwrap();
    assert_eq!(tasks.len(), 1);
    let task_run = review_store::store::task::task_run_id(&tasks[0]).unwrap();
    let task = store.task_projection(&cas, &tasks[0]).unwrap().unwrap();
    let attempts = task.execution.as_ref().unwrap().attempt_accounting();
    assert_eq!(attempts.iter().filter(|a| a.started).count(), 4);
    let before_task = store.replay(&task_run).unwrap();
    let before_review = store.replay("campaign-timing").unwrap();
    assert!(store.attempt_wall("campaign-timing").unwrap().is_empty());
    assert!(
        !before_review
            .iter()
            .any(|event| event.event_type.as_str().starts_with("Attempt"))
    );
    // Use each Attempt's captured plan Round, not the current Round or cumulative snapshots.
    let rows = store.task_attempt_wall(&task_run).unwrap();
    let mut spans = BTreeMap::<(u32, u32), (u64, u64)>::new();
    for row in &rows {
        let attempt = attempts
            .iter()
            .find(|a| a.attempt_id == row.attempt_id)
            .unwrap();
        let plan: review_core::task::plan::ExecutionPlanV1 =
            serde_json::from_value(cas.get_artifact(&attempt.plan_id).unwrap().payload).unwrap();
        let round_id = &plan.inputs["round"].artifact_ids[0];
        let round: review_core::task::review_compat::LegacyReviewRoundV1 =
            serde_json::from_value(cas.get_artifact(round_id).unwrap().payload).unwrap();
        let end = row.started_unix_ms.checked_add(row.elapsed_ms).unwrap();
        spans
            .entry((round.round, round.epoch))
            .and_modify(|range| {
                range.0 = range.0.min(row.started_unix_ms);
                range.1 = range.1.max(end);
            })
            .or_insert((row.started_unix_ms, end));
    }
    assert_eq!(spans.len(), 2);
    let expected_wall: u64 = spans.values().map(|(start, end)| end - start).sum();
    let before_db = std::fs::read(state.join("events.sqlite")).unwrap();
    let report: Value = serde_json::from_slice(
        &checked(
            invoke(
                &repo,
                &home,
                &[
                    "report",
                    "--campaign",
                    "timing",
                    "--state",
                    state_flag,
                    "--format",
                    "json",
                ],
            ),
            0,
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(report["schema"], "af/review-report@3");
    assert_eq!(report["wall_ms"], expected_wall);
    assert_eq!(report["task_accounting"][0]["attempts_started"], "4");
    assert_eq!(
        report["task_accounting"][0]["business_attempts_started"],
        "2"
    );
    assert_eq!(
        report["task_accounting"][0]["provider_attempts_started"],
        "0"
    );
    assert_eq!(report["task_accounting"][0]["other_attempts_started"], "2");
    assert_eq!(report["rounds"].as_array().unwrap().len(), 2);
    assert!(report.get("spend").is_none());
    let campaigns: Value = serde_json::from_slice(
        &checked(
            invoke(
                &repo,
                &home,
                &[
                    "campaigns",
                    "--state-root",
                    root.to_str().unwrap(),
                    "--format",
                    "json",
                ],
            ),
            0,
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(campaigns["schema"], "af/review-campaigns@1");
    assert_eq!(campaigns["problems"], serde_json::json!([]));
    assert_eq!(campaigns["campaigns"][0]["wall_ms"], expected_wall);
    assert!(
        campaigns["campaigns"][0].get("task_accounting").is_none(),
        "historical campaigns JSON shape stays unchanged"
    );
    let summary = "4 started Attempts (Provider 0, business 2, other 2)";
    let listed = checked(
        invoke(
            &repo,
            &home,
            &["campaigns", "--state-root", root.to_str().unwrap()],
        ),
        0,
    );
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains(summary), "{listed}");
    assert!(listed.contains("  wall: "), "{listed}");
    let ledger = checked(
        invoke(
            &repo,
            &home,
            &["ledger", "--campaign", "timing", "--state", state_flag],
        ),
        0,
    );
    let ledger = String::from_utf8(ledger.stderr).unwrap();
    assert!(ledger.contains(summary), "{ledger}");
    assert!(ledger.contains("; wall "), "{ledger}");
    for format in ["text", "md"] {
        let output = checked(
            invoke(
                &repo,
                &home,
                &[
                    "report",
                    "--campaign",
                    "timing",
                    "--state",
                    state_flag,
                    "--format",
                    format,
                ],
            ),
            0,
        );
        let text = String::from_utf8(output.stdout).unwrap();
        for round in [1, 2] {
            assert!(
                text.contains(&format!("Round {round} epoch 1, reviewer: Attempt")),
                "{text}"
            );
        }
    }
    assert_eq!(store.replay(&task_run).unwrap(), before_task);
    assert_eq!(store.replay("campaign-timing").unwrap(), before_review);
    assert_eq!(
        std::fs::read(state.join("events.sqlite")).unwrap(),
        before_db
    );
}

#[test]
fn missing_common_task_refuses_without_reopening_legacy_execution() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, _, state) = fixture(directory.path(), false);
    let args = [
        "run",
        "--campaign",
        "timing",
        "--state",
        state.to_str().unwrap(),
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
        "--json",
    ];
    checked(invoke(&repo, &home, &args), 0);
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let tasks = store.task_ids(&cas).unwrap();
    assert_eq!(tasks.len(), 1);
    let task_run = review_store::store::task::task_run_id(&tasks[0]).unwrap();
    let review = store.replay("campaign-timing").unwrap();
    assert!(
        review.iter().any(|event| {
            event.event_type == review_core::EventType::TaskReviewResultSelectedV1
        })
    );
    drop(store);

    // Deliberately corrupt only this disposable fixture: retain canonical paid Review
    // evidence while losing its Task log, as after an incomplete state restoration.
    let connection = rusqlite::Connection::open(state.join("events.sqlite")).unwrap();
    let removed = connection
        .execute("DELETE FROM events WHERE run_id = ?1", [&task_run])
        .unwrap();
    assert!(removed > 0);
    drop(connection);

    let output = checked(invoke(&repo, &home, &args), 1);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "af/error@1");
    assert_eq!(document["exit_code"], 1);
    assert!(document.get("outcome").is_none());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("common Task evidence but its original Task is unavailable")
    );
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    assert!(store.task_ids(&cas).unwrap().is_empty());
    assert_eq!(store.replay("campaign-timing").unwrap(), review);
    assert!(store.replay(&task_run).unwrap().is_empty());
}
