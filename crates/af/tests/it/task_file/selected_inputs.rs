//! Planning must validate the object rendered after kernel input selection.
use super::*;

#[test]
fn review_knowledge_presence_is_proved_before_checks() {
    use serde_json::json;
    for constraint in [
        "required",
        "valid_query",
        "maxProperties",
        "forbidden",
        "unrelated_bound_query",
        "unrelated_unbound_query",
        "unrelated_output_query",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "review");
        let gate_marker = directory.path().join("gate-ran");
        let policy_path = repo.join(".af/code-policy.toml");
        let mut policy: toml::Value =
            toml::from_str(&std::fs::read_to_string(&policy_path).unwrap()).unwrap();
        policy["checks"]["syntax"]["command"] = toml::Value::try_from(json!({
            "program":"/usr/bin/python3",
            "args":[
                {"value":"-c", "provenance":"literal"},
                {"value":format!("from pathlib import Path; Path({:?}).touch()", gate_marker),
                 "provenance":"literal"}
            ]
        }))
        .unwrap();
        std::fs::write(policy_path, toml::to_string(&policy).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        for name in ["bugs", "correctness"] {
            let package = format!("fixture/{name}");
            let path = repo.join(format!(".af/task-packages/{package}"));
            let worker_path = path.join("worker.toml");
            let mut worker: af_pipeline_config::task::catalog::TaskWorkerManifest =
                toml::from_str(&std::fs::read_to_string(&worker_path).unwrap()).unwrap();
            worker.signature.contract.inputs.insert(
                "knowledge".into(),
                serde_json::from_value(json!({
                    "artifact_type":"af/KnowledgeContext@1", "cardinality":"one",
                    "optional":true, "affinity":{"kind":"unbound"}, "covers":[]
                }))
                .unwrap(),
            );
            if constraint.contains("query") {
                let query = serde_json::from_value(json!({
                    "artifact_type": if constraint == "valid_query" {
                        "af/KnowledgeQuery@1"
                    } else {
                        "af/Requirements@1"
                    },
                    "cardinality":"one", "optional":true,
                    "affinity":{"kind":"unbound"}, "covers":[]
                }))
                .unwrap();
                let ports = if constraint == "unrelated_output_query" {
                    &mut worker.signature.contract.outputs
                } else {
                    &mut worker.signature.contract.inputs
                };
                ports.insert("knowledge_query".into(), query);
            }
            std::fs::write(worker_path, toml::to_string(&worker).unwrap()).unwrap();
            // A missing source excerpt still selects a manifest and an omission context.
            std::fs::write(
                path.join("knowledge.toml"),
                include_str!("../../../../../fixtures/worker-knowledge/knowledge.toml"),
            )
            .unwrap();
            let schema_path = path.join("input.schema.json");
            let original: Value =
                serde_json::from_slice(&std::fs::read(&schema_path).unwrap()).unwrap();
            let predicate = match constraint {
                "required" => json!({"required":["knowledge"]}),
                case if case.contains("query") => {
                    json!({"required":["knowledge"],"properties":{"knowledge_query":false}})
                }
                // The ordinary five ports plus exact explicit Requirements fit this bound;
                // selected knowledge must make it fail during planning.
                "maxProperties" => json!({"maxProperties":6}),
                _ => json!({"properties":{"knowledge":false}}),
            };
            std::fs::write(
                schema_path,
                serde_json::to_vec(&json!({"allOf":[original,predicate]})).unwrap(),
            )
            .unwrap();
            catalog["packages"][&package]["digest"] = toml::Value::String(
                af_pipeline_config::lock::package_digest(&package, &path).unwrap(),
            );
        }
        if constraint == "unrelated_bound_query" {
            let package = "fixture/review";
            let path = repo.join(format!(".af/task-packages/{package}"));
            let pipeline_path = path.join("pipeline.toml");
            let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
                toml::from_str(&std::fs::read_to_string(&pipeline_path).unwrap()).unwrap();
            for node in &mut pipeline.nodes {
                if matches!(node.id.as_str(), "bugs" | "correctness") {
                    node.inputs.insert(
                        "knowledge_query".into(),
                        serde_json::from_value(json!({"kind":"input","port":"requirements"}))
                            .unwrap(),
                    );
                }
            }
            std::fs::write(pipeline_path, toml::to_string(&pipeline).unwrap()).unwrap();
            catalog["packages"][package]["digest"] = toml::Value::String(
                af_pipeline_config::lock::package_digest(package, &path).unwrap(),
            );
        }
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let file_path = repo.join("review.json");
        let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
        file["requirements"] = json!({"acceptance":["Keep exact Requirements"]});
        std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-qm", "selected knowledge contract"],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let result = crate::common::af()
            .current_dir(&repo)
            .args([
                "review",
                "run",
                "--file",
                "review.json",
                "--json",
                "--state",
            ])
            .arg(&state)
            .output()
            .unwrap();
        let diagnostic = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if matches!(constraint, "required" | "valid_query") {
            assert_eq!(result.status.code(), Some(3), "{diagnostic}");
            let result: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(result["result"]["acceptance"], "satisfied");
            assert_eq!(result["result"]["execution"], "completed");
            assert_eq!(result["attempts"], 3);
            assert!(gate_marker.exists());
        } else {
            assert_eq!(result.status.code(), Some(1), "{diagnostic}");
            assert!(
                diagnostic.contains(if constraint.starts_with("unrelated_") {
                    "The knowledge_query port must have type af/KnowledgeQuery@1"
                } else {
                    "Worker input presence cannot be proved"
                }),
                "{diagnostic}"
            );
            let listed = crate::common::af()
                .current_dir(&repo)
                .args(["task", "list", "--json", "--state"])
                .arg(&state)
                .output()
                .unwrap();
            assert!(listed.status.success());
            assert!(!String::from_utf8_lossy(&listed.stdout).contains("review-cli"));
            assert!(!gate_marker.exists());
        }
        assert!(!diagnostic.contains("malformed_reply"), "{diagnostic}");
    }
}
