//! Full S2 discovery consumes independently verified fixes without rewriting closed S1 history.
use review_core::task::pipeline::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Command};
#[path = "support/review_memo.rs"]
mod review_memo;
#[path = "support/task_cli.rs"]
mod task_cli;

fn run(repo: &Path, state: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--state"])
        .arg(state)
        .arg("--json")
        .output()
        .unwrap();
    (out.status.code().unwrap(), serde_json::from_slice(&out.stdout).unwrap_or_else(|_| json!({"stderr":String::from_utf8_lossy(&out.stderr),"stdout":String::from_utf8_lossy(&out.stdout)})))
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &impl serde::Serialize) {
    std::fs::write(path, toml::to_string(value).unwrap()).unwrap();
}
fn reference(node: &str, port: &str) -> ValueRefV1 {
    ValueRefV1::Node {
        node: node.into(),
        port: port.into(),
    }
}
fn configure(repo: &Path, case: &str) {
    let packages = repo.join(".af/task-packages/fixture");
    let path = packages.join("review/pipeline.toml");
    let mut review: PipelineDefinitionV1 = read(&path);
    let mut continuation = review.contract.inputs["source"].clone();
    continuation.artifact_type = "af/TaskReviewContinuation@1".into();
    continuation.affinity = PortAffinityV1::SameAs {
        input: "source".into(),
    };
    continuation.optional = true;
    review
        .contract
        .inputs
        .insert("continuation".into(), continuation.clone());
    review
        .nodes
        .iter_mut()
        .find(|n| matches!(n.operator, TaskOperatorV1::ReviewBind {}))
        .unwrap()
        .inputs
        .insert(
            "continuation".into(),
            ValueRefV1::Input {
                port: "continuation".into(),
            },
        );
    write(&path, &review);
    let path = packages.join("repair/pipeline.toml");
    let mut repair: PipelineDefinitionV1 = read(&path);
    repair
        .nodes
        .iter_mut()
        .find(|n| matches!(n.operator, TaskOperatorV1::RepairAccept {}))
        .unwrap()
        .operator = TaskOperatorV1::ReviewContinue {};
    repair.contract.outputs.remove("result");
    continuation.optional = false;
    continuation.affinity = PortAffinityV1::DerivedFrom {
        input: "source".into(),
    };
    repair
        .contract
        .outputs
        .insert("continuation".into(), continuation);
    repair.outputs = BTreeMap::from([
        ("snapshot".into(), reference("seal_repair", "snapshot")),
        (
            "continuation".into(),
            reference("accept_repair", "continuation"),
        ),
    ]);
    repair
        .outputs
        .insert("checks".into(), reference("repair_checks", "result"));
    repair.coverage.clear();
    write(&path, &repair);
    let path = packages.join("implementation/pipeline.toml");
    let mut root: PipelineDefinitionV1 = read(&path);
    root.max_attempts = 11;
    root.contract
        .outputs
        .get_mut("verification")
        .unwrap()
        .artifact_type = "af/ReviewedImplementation@1".into();
    let mut second = root
        .nodes
        .iter()
        .find(|n| n.id == "review")
        .unwrap()
        .clone();
    second.id = "second_review".into();
    second.when = Some(NodeConditionV1 {
        node: "accept".into(),
        outcome: ReceiptOutcomeV1::Failed,
    });
    second
        .inputs
        .insert("source".into(), reference("repair", "snapshot"));
    second
        .inputs
        .insert("history".into(), reference("review", "history"));
    second
        .inputs
        .insert("continuation".into(), reference("repair", "continuation"));
    root.nodes.push(second);
    let mut accepted = root
        .nodes
        .iter()
        .find(|n| n.id == "accept")
        .unwrap()
        .clone();
    accepted.id = "second_accept".into();
    accepted.when = Some(NodeConditionV1 {
        node: "accept".into(),
        outcome: ReceiptOutcomeV1::Failed,
    });
    accepted.inputs = BTreeMap::from([
        ("source".into(), reference("repair", "snapshot")),
        ("review".into(), reference("second_review", "review")),
        ("checks".into(), reference("second_review", "checks")),
    ]);
    root.nodes.push(accepted);
    for node in &mut root.nodes {
        if node.id == "final_snapshot" {
            node.inputs
                .insert("failed".into(), reference("second_accept", "snapshot"));
        }
        if node.id == "final_checks" {
            node.inputs
                .insert("failed".into(), reference("second_review", "checks"));
        }
        if node.id == "final_verification" {
            node.inputs
                .insert("passed".into(), reference("accept", "result"));
            node.inputs
                .insert("inconclusive".into(), reference("accept", "result"));
            node.inputs
                .insert("failed".into(), reference("second_accept", "result"));
        }
    }
    write(&path, &root);
    let policy_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value = read(&policy_path);
    // A complete heavy review can use the repair bridge without enabling targeted acceptance.
    catalog["review"]["max_rounds"] = toml::Value::Integer(2);
    catalog["review"]["allow_targeted_repairs"] = toml::Value::Boolean(false);
    let bug = packages.join("bugs/worker.py");
    let original = std::fs::read_to_string(&bug).unwrap();
    let first = original.lines().last().unwrap();
    let second = if case == "rediscovered" {
        first.into()
    } else {
        "print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'verdict':'approve','summary':'Current S2 checked','reports':[],'benchmark_demands':[],'disputes':[]}]}}))".to_string()
    };
    std::fs::write(&bug,format!("import json,sys,runpy\nr=json.load(sys.stdin)\nif r['inputs']['subject'][0]['payload']['round']==1:\n    {first}\nelse:\n    f=runpy.run_path('pagination.py')['paginate']\n    try:\n        f([1,2],-1,1)\n        raise AssertionError('negative offset accepted')\n    except ValueError: pass\n    {second}\n")).unwrap();
    if matches!(case, "negative" | "missing" | "stale") {
        let path = packages.join("fix-verifier/worker.py");
        let script = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            match case {
                "negative" => script.replace("'positive' if fixed else 'negative'", "'negative'"),
                "stale" => script.replace("v['current_view_id']", "c['previous_snapshot_id']"),
                _ => "raise Exception('unavailable verifier')\n".into(),
            },
        )
        .unwrap();
    }
    upgrade_review_generation(&packages, &mut catalog);
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    write(&policy_path, &catalog);
    let ticket = repo.join("ticket.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&ticket).unwrap()).unwrap();
    value["verification"] = json!("review");
    value["strategy"] = json!("heavy");
    value["limits"]["max_attempts"] = json!(11);
    value["limits"]["verification"]["attempts"] = json!(9);
    value["limits"]["verification"]["wall_ms"] = json!(45000);
    std::fs::write(ticket, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "configure heavy review"].as_slice(),
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
fn full_s2_review_retains_original_round_and_consumes_independent_fix_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(dir.path(), "bounded-repair");
    configure(&repo, "positive");
    let (code, plan) = run(&repo, &state, &["task", "plan", "--file", "ticket.json"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["attempts"], 0);
    let (code, result) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(result["attempts"], 11);
    assert_eq!(result["result"]["acceptance"], "satisfied");
    let rounds = result["review_rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0]["round"], 1);
    assert_eq!(rounds[0]["conclusion"], "changes_requested");
    assert_eq!(rounds[1]["round"], 2);
    assert_eq!(rounds[1]["conclusion"], "pass");
    assert_ne!(rounds[0]["snapshot_id"], rounds[1]["snapshot_id"]);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let before = cas
        .get_json(rounds[0]["finding_set_id"].as_str().unwrap())
        .unwrap();
    let after = cas
        .get_json(rounds[1]["finding_set_id"].as_str().unwrap())
        .unwrap();
    let first = &before["payload"]["findings"][0];
    let second = &after["payload"]["findings"][0];
    assert_eq!(first["status"], "open", "{before:#}");
    assert_eq!(second["status"], "fixed", "{after:#}");
    assert_eq!(first["finding_id"], second["finding_id"]);
    let verification = cas
        .get_json(
            result["result"]["outputs"]["verification"]["artifact_ids"][0]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(verification["type"], "af/ReviewedImplementation@1");
    assert_eq!(verification["payload"]["scope"], "complete_review");
    let (code, replay) = run(&repo, &state, &["task", "run", "repair-cli"]);
    assert_eq!(code, 0);
    assert_eq!(replay, result);
    review_memo::refuses_changed_history(&state, &rounds[1]["invocation"], &rounds[0]);
}

#[test]
fn heavy_review_cannot_erase_negative_missing_stale_or_rediscovered_claims() {
    for case in ["negative", "missing", "stale", "rediscovered"] {
        let dir = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(dir.path(), "bounded-repair");
        configure(&repo, case);
        let (code, result) = run(&repo, &state, &["task", "start", "--file", "ticket.json"]);
        assert_eq!(code, 3, "{case}: {result:#}");
        assert_eq!(
            result["result"]["acceptance"], "unsatisfied",
            "{case}: {result:#}"
        );
        assert_eq!(result["attempts"], 11, "{case}: {result:#}");
        assert_eq!(result["review_rounds"][0]["round"], 1);
        assert_eq!(
            result["review_rounds"][1]["conclusion"],
            "convergence_exhausted"
        );
        let (code, replay) = run(&repo, &state, &["task", "run", "repair-cli"]);
        assert_eq!(code, 3);
        assert_eq!(replay, result);
    }
}

/// Upgrade only this newly configured disposable fixture. Frozen bounded-repair bytes stay V1.
fn upgrade_review_generation(packages: &Path, catalog: &mut toml::Value) {
    catalog["review"]["generation"] = toml::Value::Integer(2);
    let mut output_schema: Value =
        serde_json::from_slice(include_bytes!("../../../schemas/reviewer-result-v2.json")).unwrap();
    let original_schema: Value =
        serde_json::from_slice(include_bytes!("../../../schemas/reviewer-result-v1.json")).unwrap();
    output_schema["properties"]["reports"]["items"] =
        original_schema["$defs"]["legacyReport"].clone();
    output_schema.as_object_mut().unwrap().remove("$id");
    output_schema.as_object_mut().unwrap().remove("$schema");
    for name in ["bugs", "correctness"] {
        let path = packages.join(name).join("worker.toml");
        let mut worker: review_config::task::catalog::TaskWorkerManifest = read(&path);
        worker.signature.worker_output_type = Some("review.kernel/ReviewerResult@2".into());
        worker
            .signature
            .contract
            .outputs
            .get_mut("result")
            .unwrap()
            .artifact_type = "review.kernel/ReviewerResult@2".into();
        worker
            .signature
            .contract
            .inputs
            .get_mut("subject")
            .unwrap()
            .artifact_type = "af/TaskReviewSubject@2".into();
        let mut assignment = worker.signature.contract.inputs["subject"].clone();
        assignment.artifact_type = "af/TaskReviewAssignment@1".into();
        worker
            .signature
            .contract
            .inputs
            .insert("assignment".into(), assignment);
        worker
            .signature
            .retains
            .get_mut("result")
            .unwrap()
            .insert("assignment".into());
        write(&path, &worker);
        let path = packages.join(name).join("input.schema.json");
        let mut schema: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        schema["required"]
            .as_array_mut()
            .unwrap()
            .push(json!("assignment"));
        std::fs::write(&path, serde_json::to_vec(&schema).unwrap()).unwrap();
        std::fs::write(
            packages.join(name).join("outputs/result.schema.json"),
            serde_json::to_vec(&output_schema).unwrap(),
        )
        .unwrap();
        // Preserve the original checks and emitted discovery result; add the exact disposition
        // for the now-declared prior assignment using that same current-Snapshot observation.
        let path = packages.join(name).join("worker.py");
        let original = std::fs::read_to_string(&path).unwrap();
        let encoded = serde_json::to_string(&original).unwrap();
        std::fs::write(&path, format!("import json,sys,io,contextlib\nrequest=json.load(sys.stdin)\nassignment=request['inputs']['assignment'][0]['payload']\nassert assignment['reviewer']=={name:?}\nassert all(f['source']=={name:?} for f in assignment['findings'])\nsys.stdin=io.StringIO(json.dumps(request))\ncaptured=io.StringIO()\nwith contextlib.redirect_stdout(captured):\n    exec({encoded},{{}})\nreply=json.loads(captured.getvalue())\nfor result in reply['outputs']['result']:\n    assert result.pop('disputes')==[]\n    result['dispositions']=[{{'finding_id':f['finding_id'],'position':'corroborate' if result['reports'] else 'not_reproduced','reason':'Repeated the original current-Snapshot check.'}} for f in assignment['findings']]\nprint(json.dumps(reply))\n")).unwrap();
    }
    for pin in catalog["packages"].as_table().unwrap().values() {
        let path = packages
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(pin["path"].as_str().unwrap())
            .join("pipeline.toml");
        if !path.is_file() {
            continue;
        }
        let mut pipeline: PipelineDefinitionV1 = read(&path);
        for slot in pipeline.slots.values_mut() {
            if matches!(slot.worker.as_str(), "fixture/bugs" | "fixture/correctness") {
                slot.output_type = "review.kernel/ReviewerResult@2".into();
            }
        }
        if pipeline.name == "fixture/review" {
            for node in &mut pipeline.nodes {
                if let TaskOperatorV1::Verify { slot } = &node.operator {
                    node.inputs
                        .insert("assignment".into(), reference("bind", slot));
                }
            }
        }
        write(&path, &pipeline);
    }
}
