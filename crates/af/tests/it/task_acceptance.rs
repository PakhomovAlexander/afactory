//! Review and exact requirements are independent obligations on the final implementation.
use crate::task_cli;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn run(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let out = crate::common::af()
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
            af_pipeline_config::lock::package_digest(
                name,
                &repo.join(pin["path"].as_str().unwrap()),
            )
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
        // A missing evaluator reply is a failed Worker (6); a rejection asked for changes (3).
        let exit = if missing { 6 } else { 3 };
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
            let mut pipeline: af_contracts::task::pipeline::PipelineDefinitionV1 =
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
            let mut pipeline: af_contracts::task::pipeline::PipelineDefinitionV1 =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            pipeline
                .nodes
                .iter_mut()
                .find(|n| {
                    n.operator
                        == af_contracts::task::pipeline::TaskOperatorV1::Verify {
                            slot: "correctness".into(),
                        }
                })
                .unwrap()
                .inputs
                .remove("requirements");
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
        } else {
            let path = packages.join("evaluator/worker.toml");
            let mut worker: af_pipeline_config::task::catalog::TaskWorkerManifest =
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
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        for id in store.task_ids(&cas).unwrap() {
            let task = store.task_projection(&cas, &id).unwrap().unwrap();
            assert!(
                task.execution
                    .is_none_or(|e| e.budget.begun_attempts() == 0)
            );
        }
    }
}

/// The shipped reviewed-verification topology publishes evaluator acceptance, while the
/// canonical Review receipt is retained by its public source. Both judgments must count,
/// including when a base-backed source delivers the evaluator's typed Diff Subject.
#[test]
fn verification_evaluator_pass_cannot_override_required_open_review_findings() {
    for (case, severity) in [
        ("clean", None),
        ("blocker", Some("blocker")),
        ("major", Some("major")),
        ("missing-review", None),
    ] {
        let incomplete = case == "missing-review";
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(root.path(), "embedded-review");
        let base = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(base.status.success());
        let base = String::from_utf8(base.stdout).unwrap();
        let packages = repo.join(".af/task-packages/fixture");
        let pipeline = include_str!(
            "../../../../.af/task-packages/kernel/verification-reviewed/pipeline.toml"
        )
        .replace("kernel/verification-reviewed", "fixture/implementation")
        .replace("kernel/review-code", "fixture/review")
        .replace("\"kernel/", "\"fixture/")
        .replace("version = \"1.2.0\"", "version = \"1.0.0\"")
        .replace("max_attempts = 2\n", "max_attempts = 1\n")
        .replace(
            "allow_local_replacement = true",
            "allow_local_replacement = false",
        )
        .replace("standard = true", "");
        std::fs::write(packages.join("implementation/pipeline.toml"), pipeline).unwrap();
        // This test installs the CURRENT shipped verification topology. Its evaluator
        // consumes EvaluationInput@2, including the optional exact diff Subject.
        let path = packages.join("evaluator/worker.toml");
        let mut evaluator: af_pipeline_config::task::catalog::TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let current: af_pipeline_config::task::catalog::TaskWorkerManifest = toml::from_str(
            include_str!("../../../../.af/task-packages/kernel/evaluator/worker.toml"),
        )
        .unwrap();
        evaluator.signature.worker_input_type = current.signature.worker_input_type;
        evaluator.signature.contract.inputs.insert(
            "diff_subject".into(),
            current.signature.contract.inputs["diff_subject"].clone(),
        );
        std::fs::write(path, toml::to_string(&evaluator).unwrap()).unwrap();
        let path = packages.join("evaluator/input.schema.json");
        let mut schema: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let current_schema: Value = serde_json::from_str(include_str!(
            "../../../../.af/task-packages/kernel/evaluator/input.schema.json"
        ))
        .unwrap();
        schema["properties"]["diff_subject"] = current_schema["properties"]["diff_subject"].clone();
        std::fs::write(path, serde_json::to_vec(&schema).unwrap()).unwrap();
        let path = packages.join("evaluator/worker.py");
        let script = std::fs::read_to_string(&path).unwrap();
        let load = "request=json.load(sys.stdin)\n";
        assert!(script.contains(load));
        std::fs::write(
            path,
            script.replace(
                load,
                r#"request=json.load(sys.stdin)
import hashlib,pathlib
diff,=request['inputs']['diff_subject']
source,=request['inputs']['source']
assert diff['artifact_type']=='af/DiffSubject@1'
assert diff['snapshot_id']==source['snapshot_id']
subject=diff['payload']
assert subject['schema']=='af/diff-subject@1'
assert subject['snapshot_id']==source['snapshot_id']
assert subject['base_snapshot_id']!=subject['snapshot_id']
assert subject['base']=='../base'
change=next(c for c in subject['changes'] if c['path']=='pagination.py')
for side,path in [('before','../base/pagination.py'),('after','pagination.py')]:
    content=b'review.kernel/content-id/v1\0'+pathlib.Path(path).read_bytes()
    assert change[side]=='sha256:'+hashlib.sha256(content).hexdigest()
assert change['before']!=change['after']
"#,
            ),
        )
        .unwrap();
        let kind = packages.join("verification-kind");
        std::fs::create_dir(&kind).unwrap();
        std::fs::write(kind.join("kind.toml"),
            "schema = \"af.task-kind/1\"\nname = \"fixture/verification-kind\"\nversion = \"1.0.0\"\nkind = \"verification\"\nprofile = \"implementation\"\n").unwrap();
        let path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        catalog["packages"].as_table_mut().unwrap().insert("fixture/verification-kind".into(), toml::from_str::<toml::Value>(
            "version = \"1.0.0\"\npath = \".af/task-packages/fixture/verification-kind\"\ndigest = \"\"\n"
        ).unwrap());
        catalog.as_table_mut().unwrap().insert(
            "kinds".into(),
            toml::from_str::<toml::Value>("verification = \"fixture/verification-kind\"\n")
                .unwrap(),
        );
        std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
        std::fs::write(
            repo.join("pagination.py"),
            "def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n",
        )
        .unwrap();
        if let Some(severity) = severity {
            let path = packages.join("bugs/worker.py");
            let script = std::fs::read_to_string(&path).unwrap();
            let report = json!({"severity":severity,"file":"pagination.py","line":1,
                "title":"Reject negative offset","body":"A negative offset slices from the end.",
                "fix":"Raise ValueError for a negative offset.","confidence":0.99});
            std::fs::write(
                path,
                script.replace("'reports':[]", &format!("'reports':[{report}]")),
            )
            .unwrap();
        }
        if incomplete {
            std::fs::write(
                packages.join("bugs/worker.py"),
                "raise Exception('required reviewer unavailable')\n",
            )
            .unwrap();
        }
        let path = repo.join("ticket.json");
        let mut ticket: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        ticket["kind"] = json!("verification");
        ticket["base"] = json!({"commit": base.trim()});
        ticket.as_object_mut().unwrap().remove("verification");
        std::fs::write(path, serde_json::to_vec(&ticket).unwrap()).unwrap();
        commit(&repo);
        let exit = if incomplete {
            6
        } else if severity.is_some() {
            3
        } else {
            0
        };
        let result = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            exit,
        );
        assert_eq!(result["attempts"], 4, "{result:#}");
        assert_eq!(
            result["review_rounds"][0]["conclusion"],
            if incomplete {
                "incomplete"
            } else if severity.is_some() {
                "changes_requested"
            } else {
                "pass"
            },
            "{result:#}"
        );
        assert_eq!(
            result["result"]["domain_conclusion"],
            if incomplete {
                "incomplete"
            } else if severity.is_some() {
                "changes_requested"
            } else {
                "verified"
            }
        );
        assert_eq!(
            result["result"]["acceptance"],
            if incomplete {
                "inconclusive"
            } else if severity.is_some() {
                "unsatisfied"
            } else {
                "satisfied"
            }
        );
        let findings = run(&repo, &state, &["task", "findings", "pagination-cli"], 0);
        assert_eq!(findings["evaluation"]["outcome"], "passed", "{findings:#}");
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let evaluation_receipt = cas
            .get_artifact(
                result["result"]["outputs"]["verification"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(evaluation_receipt.payload["outcome"], "passed");
        assert_eq!(
            evaluation_receipt.payload["evaluation_id"],
            findings["evaluation"]["artifact_id"]
        );
        assert_eq!(
            findings["evaluation"]["reason"],
            "Verified offset and limit on the sealed source"
        );
        let bugs = &findings["round"]["reviewers"][0]["findings"];
        assert_eq!(
            bugs.as_array().unwrap().len(),
            usize::from(severity.is_some())
        );
        if let Some(severity) = severity {
            assert_eq!(bugs[0]["severity"], severity);
            assert_eq!(bugs[0]["status"], "open");
            assert_eq!(
                bugs[0]["evidence"],
                "A negative offset slices from the end."
            );
            assert_eq!(bugs[0]["fix"], "Raise ValueError for a negative offset.");
        }
        if incomplete {
            assert_eq!(findings["round"]["complete"], false);
            assert_eq!(findings["round"]["missing_reviewers"], json!(["bugs"]));
        }
        if severity.is_some() || incomplete {
            assert_eq!(result["result"]["missing_obligations"], json!(["verified"]));
            assert!(
                !result["state"]["next_action"]
                    .as_str()
                    .unwrap()
                    .contains("deliver")
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
            verification_replay_refuses_hidden_or_overridden_review(&state, &result);
        }
        assert_eq!(
            run(
                &repo,
                &state,
                &["task", "run", "--execute", "pagination-cli"],
                exit
            ),
            result
        );
        let shown = run(&repo, &state, &["task", "show", "pagination-cli"], 0);
        assert_eq!(shown["result"], result["result"]);
    }
}

/// Revalidate through the domain used by Store finish/replay, without dispatching any Worker.
fn verification_replay_refuses_hidden_or_overridden_review(state: &Path, inspection: &Value) {
    use af_contracts::task::{TaskAcceptanceV1, TaskResultV1};
    use af_runtime::task::{host::TaskDomain, review::ReviewTaskDomain};
    let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let projection = store
        .task_projection(&cas, "pagination-cli")
        .unwrap()
        .unwrap();
    let plan: af_contracts::task::plan::ExecutionPlanV3 = serde_json::from_value(
        cas.get_artifact(projection.plan_id.as_ref().unwrap())
            .unwrap()
            .payload,
    )
    .unwrap();
    let graph =
        serde_json::from_value(cas.get_artifact(&plan.compiled_graph_id).unwrap().payload).unwrap();
    let domain = ReviewTaskDomain::captured(
        &cas,
        inspection["review_rounds"][0]["policy_id"]
            .as_str()
            .unwrap(),
        graph,
    )
    .unwrap()
    .with_review_task(false);
    let result: TaskResultV1 = serde_json::from_value(inspection["result"].clone()).unwrap();
    domain
        .validate_result(&cas, &projection.revision, &result)
        .unwrap();
    let mut promoted = result.clone();
    promoted.acceptance = TaskAcceptanceV1::Satisfied;
    promoted.domain_conclusion = "verified".into();
    promoted.missing_obligations.clear();
    assert!(
        domain
            .validate_result(&cas, &projection.revision, &promoted)
            .is_err()
    );

    // The same Snapshot under a different source wrapper must not hide Review acceptance.
    let mut hidden = promoted.clone();
    hidden.outputs.insert(
        "snapshot".into(),
        projection.revision.inputs["source"].clone(),
    );
    assert!(
        domain
            .validate_result(&cas, &projection.revision, &hidden)
            .is_err()
    );
    let original = cas
        .get_artifact(&result.outputs["snapshot"].artifact_ids[0])
        .unwrap();
    for override_outcome in [false, true] {
        let mut source = original.clone();
        let mut found = false;
        for id in &mut source.input_artifacts {
            if Some(&*id) == source.subject_snapshot_id.as_ref() {
                continue;
            }
            let mut retained = cas.get_artifact(id).unwrap();
            if retained.artifact_type != "af/ReviewedImplementation@1" {
                continue;
            }
            found = true;
            if override_outcome {
                retained.payload["outcome"] = json!("passed");
                *id = cas
                    .put_artifact(
                        &retained.artifact_type,
                        retained.producer,
                        retained.input_artifacts,
                        retained.subject_snapshot_id,
                        retained.payload,
                    )
                    .unwrap()
                    .0;
            } else {
                *id = source.subject_snapshot_id.clone().unwrap();
            }
        }
        assert!(found);
        source.input_artifacts.sort();
        source.input_artifacts.dedup();
        let id = cas
            .put_artifact(
                &source.artifact_type,
                source.producer,
                source.input_artifacts,
                source.subject_snapshot_id,
                source.payload,
            )
            .unwrap()
            .0;
        let mut forged = promoted.clone();
        forged.outputs.get_mut("snapshot").unwrap().artifact_ids = vec![id];
        assert!(
            domain
                .validate_result(&cas, &projection.revision, &forged)
                .is_err(),
            "hidden or overridden canonical Review was accepted"
        );
    }
}
