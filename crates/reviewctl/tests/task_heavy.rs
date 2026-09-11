//! Full S2 discovery consumes independently verified fixes without rewriting closed S1 history.
use review_core::task::pipeline::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Command};
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
    repair.coverage.clear();
    write(&path, &repair);
    let path = packages.join("implementation/pipeline.toml");
    let mut root: PipelineDefinitionV1 = read(&path);
    root.max_attempts = 10;
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
    value["limits"]["max_attempts"] = json!(10);
    value["limits"]["verification"]["attempts"] = json!(8);
    value["limits"]["verification"]["wall_ms"] = json!(40000);
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
    assert_eq!(result["attempts"], 10);
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
        assert_eq!(result["attempts"], 10, "{case}: {result:#}");
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
