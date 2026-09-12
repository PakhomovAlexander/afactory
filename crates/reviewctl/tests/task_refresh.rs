//! Source refresh keeps original authority, revokes stale approval and never grants new spend.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
fn call(repo: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .arg("--state")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap()
}
fn task(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let out = call(repo, state, args);
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn issue() -> Value {
    json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42","revision":"v1",
    "summary":"Implement offset and limit pagination","description":"Preserve the input values.",
    "acceptance":{"bounds":"Reject negative or noninteger bounds."}})
}
fn setup(
    root: &Path,
    key: Option<&minisign::KeyPair>,
    template: &str,
    max_attempts: u32,
) -> (PathBuf, PathBuf) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_af"));
    cmd.current_dir(root).args([
        "catalog",
        "init",
        "--profile",
        "all",
        "--destination",
        "project",
        "--json",
    ]);
    if let Some(key) = key {
        let pubkey = root.join("owner.pub");
        std::fs::write(&pubkey, key.pk.to_box().unwrap().into_string()).unwrap();
        cmd.arg("--developer-public-key").arg(pubkey);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repo = root.join("project");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    write(&repo.join("issue.json"), &issue());
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(repo.join(format!("{template}.json"))).unwrap())
            .unwrap();
    file["task_id"] = json!("issue-refresh");
    file["issue"] = json!({"kind":"local","path":"issue.json"});
    file["limits"]["max_attempts"] = json!(max_attempts);
    write(&repo.join("ticket.json"), &file);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "captured issue"]);
    (repo, root.join("state"))
}
fn revision(cas: &review_store::Cas, report: &Value) -> Value {
    cas.get_json(report["revision_id"].as_str().unwrap())
        .unwrap()["payload"]
        .clone()
}
fn approve(
    repo: &Path,
    state: &Path,
    root: &Path,
    key: &minisign::KeyPair,
    name: &str,
) -> (PathBuf, PathBuf) {
    let payload = root.join(format!("{name}.payload"));
    let signature = root.join(format!("{name}.minisig"));
    task(
        repo,
        state,
        &[
            "task",
            "decision-payload",
            "issue-refresh",
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "Reviewed exact inputs and plan",
            "--output",
            payload.to_str().unwrap(),
        ],
        0,
    );
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            std::fs::read(&payload).unwrap().as_slice(),
            Some("exact issue plan"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    task(
        repo,
        state,
        &[
            "task",
            "approve",
            "issue-refresh",
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
        0,
    );
    (payload, signature)
}
fn deliver(repo: &Path, state: &Path, target: &Path, branch: &str) -> Value {
    task(
        repo,
        state,
        &[
            "task",
            "deliver",
            "issue-refresh",
            "--branch",
            branch,
            "--worktree",
            target.to_str().unwrap(),
            "--confirm",
            "issue-refresh",
        ],
        0,
    )
}

fn interrupted_delivery_blocks_source_refresh(
    repo: &Path,
    state: &Path,
    cas: &review_store::Cas,
    report: &Value,
) {
    use review_core::task::delivery::*;
    let revision = revision(cas, report);
    let source_id = revision["inputs"]["source"]["snapshot_id"]
        .as_str()
        .unwrap();
    let derived_id = report["result"]["outputs"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let (source, _) = review_source_git::task::read_snapshot(cas, source_id).unwrap();
    let origin = cas.get_json(&source.origin_id).unwrap();
    let target = json!({"repository":std::fs::canonicalize(repo).unwrap(),"repository_id":origin["repository_id"],
        "branch":"af/interrupted-source-delivery","worktree":state.parent().unwrap().join("interrupted-delivery")});
    let target_id = cas.put_json(&target).unwrap();
    let result_id = report["phase"]["result_id"].as_str().unwrap();
    let mut receipt = json!({"schema":"af/task-delivery-prepared@1","delivery_id":"delivery-interrupted-test","task_id":"issue-refresh",
        "result_id":result_id,"source_snapshot_id":source_id,"derived_snapshot_id":derived_id,
        "source_revision":origin["source_revision"],"target":target});
    let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    for status in [TaskDeliveryStatusV1::Prepared, TaskDeliveryStatusV1::Failed] {
        if status == TaskDeliveryStatusV1::Failed {
            receipt["schema"] = json!("af/task-delivery@1");
            receipt.as_object_mut().unwrap().remove("source_revision");
            receipt["outcome"] =
                json!({"kind":"failed","reason":"Stopped before creating refs or a worktree"});
            receipt["ignored_paths"] = json!([]);
            receipt["remote_actions"] = json!([]);
        }
        let record = TaskDeliveryRecordV1 {
            task_id: "issue-refresh".into(),
            result_id: result_id.into(),
            source_snapshot_id: source_id.into(),
            derived_snapshot_id: derived_id.into(),
            target_id: target_id.clone(),
            receipt_id: cas.put_json(&receipt).unwrap(),
            status,
        };
        let record_id = cas
            .put_artifact(
                TASK_DELIVERY_RECORD_V1,
                review_core::Producer::KernelOperation {
                    run_id: review_store::store::task::task_run_id("issue-refresh").unwrap(),
                    node_id: None,
                    operation_id: "local-delivery@1".into(),
                },
                record.references().into_iter().map(str::to_owned).collect(),
                None,
                serde_json::to_value(record).unwrap(),
            )
            .unwrap()
            .0;
        let lease = store
            .take_task_lease(cas, "issue-refresh", "delivery-fixture", 15000)
            .unwrap();
        store.record_task_delivery(cas, &lease, &record_id).unwrap();
        store.release_task_lease(cas, &lease).unwrap();
        if status == TaskDeliveryStatusV1::Prepared {
            // A crashed preparation with no active lease must still be reconciled first.
            let refused = call(
                repo,
                state,
                &[
                    "task",
                    "refresh",
                    "issue-refresh",
                    "--source-file",
                    "absent-issue.json",
                ],
            );
            assert!(!refused.status.success());
            assert!(String::from_utf8_lossy(&refused.stderr).contains("pending local delivery"));
            assert_eq!(
                task(repo, state, &["task", "show", "issue-refresh"], 0)["revision_id"],
                report["revision_id"]
            );
        }
    }
}

#[test]
fn generated_issue_refresh_reuses_definition_but_requires_a_new_exact_signature() {
    let root = tempfile::tempdir().unwrap();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let (repo, state) = setup(root.path(), Some(&key), "planning", 7);
    let waiting = task(
        &repo,
        &state,
        &["task", "start", "--file", "ticket.json"],
        0,
    );
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    assert_eq!(waiting["attempts"], 1);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let original = revision(&cas, &waiting);
    let (old_payload, old_signature) = approve(&repo, &state, root.path(), &key, "old");
    let approved = task(&repo, &state, &["task", "explain", "issue-refresh"], 0);
    // A byte-only formatting change neither invalidates approval nor adds a transition.
    std::fs::write(
        repo.join("issue.json"),
        serde_json::to_vec(&issue()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        task(&repo, &state, &["task", "refresh", "issue-refresh"], 0),
        approved
    );
    let mut changed = issue();
    changed["description"] = json!("Preserve every original input value, including an empty list.");
    // Upstream revision labels are insufficient: selected field bytes changed under the same label.
    write(&repo.join("issue.json"), &changed);
    let refreshed = task(&repo, &state, &["task", "refresh", "issue-refresh"], 0);
    assert_eq!(refreshed["phase"]["reason"], "needs_plan_review");
    assert_ne!(refreshed["plan_id"], waiting["plan_id"]);
    assert_eq!(refreshed["attempts"], 1);
    assert_eq!(refreshed["chargeable_tokens"], "0");
    assert_eq!(refreshed["planning"], waiting["planning"]);
    let next = revision(&cas, &refreshed);
    assert_eq!(next["previous_revision_id"], waiting["revision_id"]);
    assert_eq!(next["limits"], original["limits"]);
    assert_eq!(next["authority"], original["authority"]);
    assert_eq!(next["inputs"]["source"], original["inputs"]["source"]);
    for args in [
        vec!["task", "run", "issue-refresh"],
        vec![
            "task",
            "approve",
            "issue-refresh",
            "--payload",
            old_payload.to_str().unwrap(),
            "--signature",
            old_signature.to_str().unwrap(),
        ],
    ] {
        let refused = call(&repo, &state, &args);
        assert!(!refused.status.success());
    }
    assert_eq!(
        task(&repo, &state, &["task", "show", "issue-refresh"], 0)["attempts"],
        1
    );
    approve(&repo, &state, root.path(), &key, "fresh");
    let done = task(&repo, &state, &["task", "run", "issue-refresh"], 0);
    assert_eq!(done["attempts"], 6);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(
        task(&repo, &state, &["task", "run", "issue-refresh"], 0),
        done
    );
}
#[test]
fn completed_task_refresh_retains_snapshot_and_spend_then_waits_when_capacity_is_used() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path(), None, "implementation-reviewed", 10);
    let first = task(
        &repo,
        &state,
        &["task", "start", "--file", "ticket.json"],
        0,
    );
    assert_eq!(first["attempts"], 5);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let original = revision(&cas, &first);
    interrupted_delivery_blocks_source_refresh(&repo, &state, &cas, &first);
    let first_worktree = root.path().join("delivered-v1");
    let first_delivery = deliver(&repo, &state, &first_worktree, "af/issue-v1");
    let first_bytes = std::fs::read(first_worktree.join("pagination.py")).unwrap();
    assert_eq!(first_delivery["result_id"], first["phase"]["result_id"]);
    let mut changed = issue();
    changed["revision"] = json!("v2");
    changed["acceptance"]["empty"] = json!("An empty input returns an empty list.");
    let external = root.path().join("updated.toml");
    let data: review_core::task::source::IssueInputV1 =
        serde_json::from_value(changed.clone()).unwrap();
    std::fs::write(&external, toml::to_string(&data).unwrap()).unwrap();
    // Live project edits are not recaptured as the execution Snapshot or trusted Task policy.
    std::fs::write(repo.join("pagination.py"), "unrelated live edit\n").unwrap();
    std::fs::write(repo.join("ticket.json"), "invalid live Task definition").unwrap();
    let refreshed = task(
        &repo,
        &state,
        &[
            "task",
            "refresh",
            "issue-refresh",
            "--source-file",
            external.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(refreshed["phase"]["kind"], "ready");
    assert_eq!(refreshed["attempts"], 5);
    let next = revision(&cas, &refreshed);
    assert_eq!(next["inputs"]["source"], original["inputs"]["source"]);
    assert_eq!(next["limits"], original["limits"]);
    assert_eq!(next["authority"], original["authority"]);
    assert!(refreshed["result"].is_null());
    let second = task(&repo, &state, &["task", "run", "issue-refresh"], 0);
    assert_eq!(second["attempts"], 10);
    assert_eq!(second["result"]["acceptance"], "satisfied");
    assert_ne!(
        second["result"]["task_revision_id"],
        first["result"]["task_revision_id"]
    );
    git(&repo, &["restore", "pagination.py", "ticket.json"]);
    let second_worktree = root.path().join("delivered-v2");
    let second_delivery = deliver(&repo, &state, &second_worktree, "af/issue-v2");
    assert_eq!(second_delivery["result_id"], second["phase"]["result_id"]);
    assert_ne!(second_delivery["result_id"], first_delivery["result_id"]);
    assert_eq!(
        deliver(&repo, &state, &second_worktree, "af/issue-v2"),
        second_delivery
    );
    assert_eq!(
        std::fs::read(first_worktree.join("pagination.py")).unwrap(),
        first_bytes
    );
    std::fs::write(repo.join("pagination.py"), "unrelated live edit\n").unwrap();
    changed["revision"] = json!("v3");
    write(&repo.join("issue.json"), &changed);
    // Refuse an active writer before reading even malformed source bytes.
    let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    let lease = store
        .take_task_lease(&cas, "issue-refresh", "fixture-writer", 15000)
        .unwrap();
    let refused = call(&repo, &state, &["task", "refresh", "issue-refresh"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("active writer"));
    store.release_task_lease(&cas, &lease).unwrap();
    drop(store);
    let waiting = task(&repo, &state, &["task", "refresh", "issue-refresh"], 0);
    assert_eq!(waiting["phase"]["reason"], "needs_resources");
    assert!(waiting["plan_id"].is_null());
    assert!(
        waiting["delivery"].is_null(),
        "Earlier delivery is history, not this revision's result"
    );
    assert_eq!(waiting["attempts"], 10);
    assert_eq!(waiting["chargeable_tokens"], "0");
    let last = revision(&cas, &waiting);
    assert_eq!(last["revision"], 3);
    assert_eq!(last["limits"], original["limits"]);
    let run = task(&repo, &state, &["task", "run", "issue-refresh"], 4);
    assert_eq!(run["attempts"], 10);
    assert_eq!(
        task(&repo, &state, &["task", "refresh", "issue-refresh"], 0),
        waiting
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("pagination.py")).unwrap(),
        "unrelated live edit\n"
    );
}
