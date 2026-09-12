//! Real S1 finding -> one bounded repair -> independent current-S2 verification.
use serde_json::Value;
use std::path::Path;
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

fn run(repo: &Path, state: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| serde_json::json!({"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)}));
    (output.status.code().unwrap(), value)
}

#[test]
fn bounded_repair_preserves_one_round_and_delivers_verified_s2() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
    let (code, plan) = run(&repo, &state, &["task", "plan", "--file", "ticket.json"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["attempts"], 0);
    let (code, result) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(result["attempts"], 8);
    assert_eq!(result["review_rounds"].as_array().unwrap().len(), 1);
    assert_eq!(result["review_rounds"][0]["round"], 1);
    assert_eq!(
        result["review_rounds"][0]["conclusion"],
        "changes_requested"
    );
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let verification = result["result"]["outputs"]["verification"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let accepted = cas.get_json(verification).unwrap();
    assert_eq!(accepted["type"], "af/RepairAllowedImplementation@1");
    assert_eq!(accepted["payload"]["scope"], "targeted_fixes");
    assert_ne!(
        accepted["payload"]["snapshot_id"],
        result["review_rounds"][0]["snapshot_id"]
    );
    let repair_id = accepted["payload"]["invocation"]["inputs"]["repair"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let context = cas.get_json(repair_id).unwrap();
    assert_eq!(
        context["payload"]["continuation"]["previous_subject_id"],
        result["review_rounds"][0]["subject_id"]
    );
    assert_eq!(
        context["payload"]["previous_snapshot_id"],
        result["review_rounds"][0]["snapshot_id"]
    );
    let history_id = context["payload"]["continuation"]["prior_history_id"]
        .as_str()
        .unwrap();
    let history = cas.get_json(history_id).unwrap();
    let original_round = cas
        .get_json(history["payload"]["round_report_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(original_round["payload"], result["review_rounds"][0]);
    let (code, resumed) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0, "{resumed:#}");
    assert_eq!(resumed, result);
    let worktree = directory.path().join("delivered");
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "deliver",
            "repair-cli",
            "--branch",
            "repair-output",
            "--worktree",
        ])
        .arg(&worktree)
        .args(["--confirm", "repair-cli", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::read_to_string(worktree.join("pagination.py"))
            .unwrap()
            .contains("raise ValueError")
    );
}

fn commit_fixture(repo: &Path) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "repair fixture variation"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn repair_rejects_missing_stale_and_negative_receipts_and_current_check_failures() {
    for case in [
        "negative",
        "missing",
        "stale_view",
        "stale_subject",
        "missing_claim",
        "failed_checks",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
        let path = repo.join(".af/task-packages/fixture/fix-verifier/worker.py");
        let original = std::fs::read_to_string(&path).unwrap();
        let replacement = match case {
            "negative" => original.replace("'positive' if fixed else 'negative'", "'negative'"),
            "missing" => "raise Exception('fix verifier unavailable')\n".into(),
            "stale_view" => original.replace("v['current_view_id']", "c['previous_snapshot_id']"),
            "stale_subject" => original.replace(
                "c['continuation']['current_subject_id']",
                "c['continuation']['previous_subject_id']",
            ),
            "missing_claim" => original.replace("print(json.dumps", "claims={}\nprint(json.dumps"),
            "failed_checks" => {
                let repair = repo.join(".af/task-packages/fixture/repairer/worker.py");
                std::fs::write(
                    &repair,
                    std::fs::read_to_string(&repair)
                        .unwrap()
                        .replace("items[offset:offset+limit]", "items[:1]"),
                )
                .unwrap();
                original
            }
            _ => unreachable!(),
        };
        std::fs::write(path, replacement).unwrap();
        commit_fixture(&repo);
        let (code, value) = run(&repo, &state, &["task", "start", "--file", "ticket.json"]);
        let expected = if matches!(case, "negative" | "failed_checks") {
            3
        } else {
            4
        };
        assert_eq!(code, expected, "{case}: {value:#}");
        assert_ne!(value["result"]["acceptance"], "satisfied", "{case}");
        assert_eq!(
            value["attempts"],
            if case == "failed_checks" { 6 } else { 8 },
            "{case}"
        );
        assert_eq!(
            value["review_rounds"].as_array().unwrap().len(),
            1,
            "{case}"
        );
        let (replay_code, replayed) = run(&repo, &state, &["task", "run", "repair-cli"]);
        assert_eq!(replay_code, code, "{case}");
        assert_eq!(
            replayed, value,
            "{case}: spent repair allowance cannot be reopened"
        );
    }
}

#[test]
fn repair_cannot_claim_full_review_omit_verifiers_or_bypass_reserved_budget() {
    for case in [
        "full_review",
        "policy_refuses",
        "omitted_verifier",
        "reserve",
        "repair_bound",
        "self_verify",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
        match case {
            "full_review" | "reserve" => {
                let path = repo.join("ticket.json");
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if case == "full_review" {
                    value["verification"] = "review".into();
                } else {
                    value["limits"]["verification"]["attempts"] = 4.into();
                }
                std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
            "policy_refuses" => {
                let path = repo.join(".af/task-catalog.toml");
                let mut value: toml::Value =
                    toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                value["review"]["allow_targeted_repairs"] = false.into();
                std::fs::write(path, toml::to_string(&value).unwrap()).unwrap();
            }
            "omitted_verifier" | "repair_bound" => {
                let path = repo.join(".af/task-packages/fixture/repair/pipeline.toml");
                let mut value: review_core::task::pipeline::PipelineDefinitionV1 =
                    review_config::task::parse_task_pipeline(
                        &std::fs::read_to_string(&path).unwrap(),
                    )
                    .unwrap();
                if case == "repair_bound" {
                    value.max_attempts = 2;
                } else {
                    value
                        .nodes
                        .iter_mut()
                        .find(|n| n.id == "accept_repair")
                        .unwrap()
                        .inputs
                        .remove("verification");
                }
                std::fs::write(path, toml::to_string(&value).unwrap()).unwrap();
            }
            "self_verify" => {
                let path = repo.join(".af/task-packages/fixture/fix-verifier/worker.toml");
                let mut value: review_config::task::catalog::TaskWorkerManifest =
                    toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                value.signature.effects.insert("write-source".into());
                std::fs::write(path, toml::to_string(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        commit_fixture(&repo);
        let (code, value) = run(&repo, &state, &["task", "plan", "--file", "ticket.json"]);
        assert_eq!(code, 1, "{case}: {value:#}");
    }
}

#[test]
fn clean_review_selects_s1_without_spending_the_repair_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
    let package = repo.join(".af/task-packages/fixture");
    std::fs::copy(
        package.join("correctness/worker.py"),
        package.join("bugs/worker.py"),
    )
    .unwrap();
    commit_fixture(&repo);
    let (code, result) = run(&repo, &state, &["task", "start", "--file", "ticket.json"]);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(result["attempts"], 5);
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(result["review_rounds"][0]["conclusion"], "pass");
    assert!(result.get("repair_assessments").is_none());
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let id = result["result"]["outputs"]["verification"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    assert_eq!(
        cas.get_json(id).unwrap()["payload"]["scope"],
        "complete_review"
    );
}

#[test]
fn interrupted_fix_verification_resumes_same_continuation_and_charges_the_lost_attempt() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
    let package = repo.join(".af/task-packages/fixture");
    let worker = package.join("fix-verifier/worker.py");
    let source = std::fs::read_to_string(&worker).unwrap();
    std::fs::write(&worker, format!("import time\ntime.sleep(2)\n{source}")).unwrap();
    for name in ["implementation", "repair"] {
        let path = package.join(name).join("pipeline.toml");
        let mut value: review_core::task::pipeline::PipelineDefinitionV1 =
            review_config::task::parse_task_pipeline(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        value.max_attempts += 1;
        value.slots.get_mut("fix-verifier").unwrap().max_attempts = 2;
        std::fs::write(path, toml::to_string(&value).unwrap()).unwrap();
    }
    let ticket = repo.join("ticket.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&ticket).unwrap()).unwrap();
    value["limits"]["max_attempts"] = 9.into();
    std::fs::write(ticket, serde_json::to_vec(&value).unwrap()).unwrap();
    commit_fixture(&repo);
    let (code, plan) = run(&repo, &state, &["task", "plan", "--file", "ticket.json"]);
    assert_eq!(code, 0, "{plan:#}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args(["task", "run", "repair-cli", "--json", "--state"])
        .arg(&state)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    let revision = cas.get_json(plan["revision_id"].as_str().unwrap()).unwrap();
    let deadline = revision["payload"]["limits"]["deadline_unix_ms"]
        .as_u64()
        .unwrap();
    let run_id = review_store::store::task::task_run_id("repair-cli").unwrap();
    let mut next_sequence = 0;
    let mut target_attempts = std::collections::BTreeSet::new();
    // Observe only newly appended records. Full projection replay validates the entire
    // artifact closure and can itself delay the Worker enough to miss a short crash window.
    let interrupted_attempt = 'observe: loop {
        for event in store.replay_from(&run_id, next_sequence).unwrap() {
            next_sequence = event.sequence + 1;
            let transition: review_core::task::event::TaskTransitionV1 =
                serde_json::from_value(event.payload).unwrap();
            if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            {
                let record = cas.get_json(&record_id).unwrap();
                if matches!(
                    record["payload"]["kind"].as_str(),
                    Some("prepared" | "reserved")
                ) {
                    let invocation = cas
                        .get_json(record["payload"]["invocation_id"].as_str().unwrap())
                        .unwrap();
                    if invocation["payload"]["node"] == "root.nodes.repair.nodes.verify_fixes" {
                        target_attempts
                            .insert(record["payload"]["attempt_id"].as_str().unwrap().to_owned());
                    }
                } else if record["payload"]["kind"] == "started"
                    && let Some(attempt) = record["payload"]["attempt_id"].as_str()
                    && target_attempts.contains(attempt)
                {
                    // Fence the process before doing expensive projection validation.
                    child.kill().unwrap();
                    child.wait().unwrap();
                    break 'observe attempt.to_owned();
                }
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        if now >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            let projection = store.task_projection(&cas, "repair-cli").unwrap().unwrap();
            panic!(
                "No started fix-verifier Attempt before the admitted Task deadline: phase={:?}, attempts={}",
                projection.phase,
                projection
                    .execution
                    .as_ref()
                    .map_or(0, |e| e.budget.begun_attempts())
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let frozen = store.task_projection(&cas, "repair-cli").unwrap().unwrap();
    let interrupted = frozen.execution.as_ref().unwrap();
    assert_eq!(interrupted.budget.begun_attempts(), 7);
    assert!(
        interrupted
            .outputs
            .contains_key("root.nodes.repair.nodes.attest")
    );
    assert!(
        interrupted
            .pending_attempts()
            .contains(&interrupted_attempt)
    );
    let final_lease = store
        .task_projection(&cas, "repair-cli")
        .unwrap()
        .unwrap()
        .lease_until_unix_ms();
    let execution = frozen.execution.as_ref().unwrap();
    let context = execution.outputs["root.nodes.repair.nodes.attest"].clone();
    let original = execution.outputs["root.nodes.review.nodes.reduce"].clone();
    // Recovery uses the real fenced lease boundary. The already-running command has only
    // read authority and exits after its bounded two-second fixture delay.
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        if now > final_lease {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, result) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(
        result["attempts"], 9,
        "The abandoned verifier still consumes its parent and child allowance"
    );
    let resumed = store.task_projection(&cas, "repair-cli").unwrap().unwrap();
    let outputs = &resumed.execution.as_ref().unwrap().outputs;
    assert_eq!(outputs["root.nodes.repair.nodes.attest"], context);
    assert_eq!(outputs["root.nodes.review.nodes.reduce"], original);
    assert_eq!(result["review_rounds"].as_array().unwrap().len(), 1);
    assert_eq!(result["repair_assessments"].as_array().unwrap().len(), 1);
    let (code, again) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0);
    assert_eq!(again, result);
}
