//! The approval command authenticates a detached signature, never an actor string or model reply.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

fn run(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn commit(repo: &Path) {
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "developer public authority"].as_slice(),
    ] {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
fn setup(root: &Path) -> (PathBuf, PathBuf, minisign::KeyPair) {
    let (repo, state) = task_cli::fixture_named(root, "pagination");
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog.as_table_mut().unwrap().insert("developers".into(),toml::Value::try_from(json!({"schema":"af.task-developers/1","keys":{"owner":key.pk.to_box().unwrap().into_string()}})).unwrap());
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    commit(&repo);
    (repo, state, key)
}
fn sign(key: &minisign::KeyPair, bytes: &[u8], path: &Path) {
    std::fs::write(
        path,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes,
            Some("reviewed exact Task plan"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
}
fn bytes(value: &Value) -> Vec<u8> {
    let mut bytes = b"af/task-plan-authorization/1\n".to_vec();
    bytes.extend(review_store::canonicalize(value).unwrap());
    bytes
}
fn decisions(value: &Value) -> usize {
    value["plan_decisions"].as_array().map_or(0, Vec::len)
}

#[test]
fn signed_approval_is_exact_idempotent_and_survives_process_and_catalog_changes() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path());
    let planned = run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    let payload = root.path().join("approval.payload");
    let signature = root.path().join("approval.minisig");
    let request = run(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            "pagination-cli",
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "Reviewed all stages and verifier coverage",
            "--output",
            payload.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(request["payload"]["plan_id"], planned["plan_id"]);
    assert_eq!(
        request["payload"]["task_revision_id"],
        planned["revision_id"]
    );
    let message = std::fs::read(&payload).unwrap();
    assert_eq!(message, bytes(&request["payload"]));
    sign(&key, &message, &signature);
    let args = [
        "task",
        "approve",
        "pagination-cli",
        "--payload",
        payload.to_str().unwrap(),
        "--signature",
        signature.to_str().unwrap(),
    ];
    let approved = run(&repo, &state, &args, 0);
    assert_eq!(approved["attempts"], 0);
    assert_eq!(decisions(&approved), 1);
    assert_eq!(
        approved["plan_decisions"][0]["decision"]["decision"],
        "approved"
    );
    let duplicate = run(&repo, &state, &args, 0);
    assert_eq!(decisions(&duplicate), 1);
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        "untrusted edited developer keys",
    )
    .unwrap();
    let finished = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(finished["attempts"], 3);
    assert_eq!(finished["result"]["acceptance"], "satisfied");
    assert_eq!(finished["plan_decisions"], approved["plan_decisions"]);
    assert_eq!(
        run(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
            0
        ),
        finished
    );
}

#[test]
fn forged_changed_wrong_task_and_expired_authorizations_never_record_a_decision() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path());
    run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    let original = root.path().join("original.payload");
    let request = run(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            "pagination-cli",
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "Reviewed exact plan",
            "--output",
            original.to_str().unwrap(),
        ],
        0,
    );
    let original_bytes = std::fs::read(original).unwrap();
    for case in [
        "forged",
        "other_key",
        "changed_reason",
        "changed_plan",
        "expired",
        "wrong_decision",
        "another_task",
    ] {
        let payload = root.path().join(format!("{case}.payload"));
        let signature = root.path().join(format!("{case}.minisig"));
        let mut value = request["payload"].clone();
        match case {
            "changed_reason" => value["reason"] = json!("Worker changed authorization"),
            "changed_plan" => value["plan_id"] = json!(format!("sha256:{}", "0".repeat(64))),
            "expired" => value["valid_until_unix_ms"] = json!(1),
            "wrong_decision" => value["decision"] = json!("rejected"),
            "another_task" => {
                value["task_revision_id"] = json!(format!("sha256:{}", "0".repeat(64)))
            }
            _ => (),
        }
        let message = bytes(&value);
        std::fs::write(&payload, &message).unwrap();
        match case {
            "forged" => {
                std::fs::write(&signature, "{\"developer\":\"owner\",\"approved\":true}").unwrap()
            }
            "other_key" => sign(
                &minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
                &message,
                &signature,
            ),
            "changed_reason" => sign(&key, &original_bytes, &signature),
            _ => sign(&key, &message, &signature),
        }
        run(
            &repo,
            &state,
            &[
                "task",
                "approve",
                "pagination-cli",
                "--payload",
                payload.to_str().unwrap(),
                "--signature",
                signature.to_str().unwrap(),
            ],
            1,
        );
        let untouched = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
        assert_eq!(untouched["attempts"], 0, "{case}");
        assert_eq!(decisions(&untouched), 0, "{case}");
    }
}

#[test]
fn signed_rejection_prevents_dispatch_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path());
    run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    let payload = root.path().join("rejected.payload");
    let signature = root.path().join("rejected.minisig");
    run(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            "pagination-cli",
            "--developer",
            "owner",
            "--decision",
            "rejected",
            "--reason",
            "Verification is insufficient",
            "--output",
            payload.to_str().unwrap(),
        ],
        0,
    );
    sign(&key, &std::fs::read(&payload).unwrap(), &signature);
    let rejected = run(
        &repo,
        &state,
        &[
            "task",
            "reject",
            "pagination-cli",
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(
        rejected["plan_decisions"][0]["decision"]["decision"],
        "rejected"
    );
    run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        1,
    );
    let inspected = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
    assert_eq!(inspected["attempts"], 0);
    assert_eq!(decisions(&inspected), 1);
}
