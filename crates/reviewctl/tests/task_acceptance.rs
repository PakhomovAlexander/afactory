//! Review and exact requirements are independent obligations on the final implementation.
use serde_json::{Value, json};
use std::{path::Path, process::Command};
#[path = "support/task_cli.rs"]
mod task_cli;

fn run(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .arg("--state")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(expected),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn commit(repo: &Path) {
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
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "acceptance fixture"].as_slice(),
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
fn passing_review_cannot_mask_failed_or_missing_requirements_acceptance() {
    for missing in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(root.path(), "embedded-review");
        let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[{"outcome":"failed","reason":"The exact ticket acceptance criterion is unmet."}]}});
        let script = if missing {
            "raise Exception('independent evaluator unavailable')\n".into()
        } else {
            format!(
                "import json,sys\njson.load(sys.stdin)\nprint({:?})\n",
                reply.to_string()
            )
        };
        std::fs::write(
            repo.join(".af/task-packages/fixture/evaluator/worker.py"),
            script,
        )
        .unwrap();
        commit(&repo);
        let exit = if missing { 4 } else { 3 };
        let result = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            exit,
        );
        assert_eq!(result["attempts"], 5);
        assert_eq!(result["review_rounds"][0]["outcome"], "passed");
        assert_eq!(
            result["result"]["acceptance"],
            if missing {
                "inconclusive"
            } else {
                "unsatisfied"
            }
        );
        assert_eq!(result["result"]["missing_obligations"], json!(["goal"]));
        assert_eq!(
            run(
                &repo,
                &state,
                &["task", "run", "--execute", "pagination-cli"],
                exit
            ),
            result
        );
        let target = root.path().join("must-not-deliver");
        run(
            &repo,
            &state,
            &[
                "task",
                "deliver",
                "pagination-cli",
                "--confirm",
                "pagination-cli",
                "--branch",
                "unverified",
                "--worktree",
                target.to_str().unwrap(),
            ],
            1,
        );
        assert!(!target.exists());
    }
}
#[test]
fn requirements_retention_and_final_snapshot_are_proved_before_attempts() {
    for case in ["retention", "review_requirements", "stale"] {
        let stale = case == "stale";
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(
            root.path(),
            if stale {
                "bounded-repair"
            } else {
                "embedded-review"
            },
        );
        let packages = repo.join(".af/task-packages/fixture");
        if stale {
            let path = packages.join("implementation/pipeline.toml");
            let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let evaluation = pipeline
                .nodes
                .iter_mut()
                .find(|n| n.id == "evaluate_goal")
                .unwrap();
            evaluation.inputs.insert(
                "source".into(),
                serde_json::from_value(json!({"kind":"node","node":"seal","port":"snapshot"}))
                    .unwrap(),
            );
            evaluation.inputs.insert(
                "checks".into(),
                serde_json::from_value(json!({"kind":"node","node":"review","port":"checks"}))
                    .unwrap(),
            );
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
        } else if case == "review_requirements" {
            let path = packages.join("review/pipeline.toml");
            let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            pipeline
                .nodes
                .iter_mut()
                .find(|n| {
                    n.operator
                        == review_core::task::pipeline::TaskOperatorV1::Verify {
                            slot: "correctness".into(),
                        }
                })
                .unwrap()
                .inputs
                .remove("requirements");
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
        } else {
            let path = packages.join("evaluator/worker.toml");
            let mut worker: review_config::task::catalog::TaskWorkerManifest =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            worker
                .signature
                .retains
                .get_mut("result")
                .unwrap()
                .remove("requirements");
            std::fs::write(path, toml::to_string(&worker).unwrap()).unwrap();
        }
        commit(&repo);
        let result = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            1,
        );
        let text = result.to_string();
        assert!(
            if stale {
                text.contains("Snapshot") || text.contains("affinity") || text.contains("lineage")
            } else {
                text.contains("exact Task Requirements")
            },
            "{result:#}"
        );
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        for id in store.task_ids(&cas).unwrap() {
            let task = store.task_projection(&cas, &id).unwrap().unwrap();
            assert!(
                task.execution
                    .is_none_or(|e| e.budget.begun_attempts() == 0)
            );
        }
    }
}
