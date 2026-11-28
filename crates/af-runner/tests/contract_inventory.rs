//! The fast contract gate exercises actual shipped whole Worker requests, not leaf guesses.
use af_runner::task::{WorkerContract, WorkerRequest, reply_schema};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

fn root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn read(path: &str) -> Value {
    let (file, fragment) = path.split_once('#').unwrap_or((path, ""));
    let value: Value = serde_json::from_slice(&std::fs::read(root().join(file)).unwrap()).unwrap();
    value.pointer(fragment).unwrap().clone()
}

#[test]
fn inventory_schemas_compile_and_shipped_whole_requests_have_reviewed_projection_modes() {
    let inventory = read("scripts/contract-inventory.json");
    let paths: Vec<_> = inventory["families"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["schemas"].as_array().unwrap())
        .map(|p| p.as_str().unwrap())
        .collect();
    let mut registry = jsonschema::Registry::new();
    for path in paths.iter().filter(|p| p.starts_with("schemas/")) {
        let schema = read(path);
        if let Some(id) = schema["$id"].as_str() {
            registry = registry
                .add(id, jsonschema::Resource::from_contents(schema.clone()))
                .unwrap();
        }
    }
    let registry = registry.prepare().unwrap();
    for path in &paths {
        jsonschema::options()
            .with_registry(&registry)
            .build(&read(path))
            .unwrap_or_else(|error| panic!("{path}: {error}"));
    }
    for set in inventory["requests"].as_array().unwrap() {
        let outputs: BTreeMap<String, Value> = set["outputs"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(port, path)| (port.clone(), read(path.as_str().unwrap())))
            .collect();
        if outputs.is_empty() {
            assert_eq!(set["strict"], false);
            continue;
        }
        let projected = reply_schema::strict_outputs(&outputs)
            .unwrap_or_else(|e| panic!("{}: {e}", set["worker"]));
        assert_eq!(
            projected.is_some(),
            set["strict"].as_bool().unwrap(),
            "{}: unexplained projection mode drift",
            set["worker"]
        );
        assert!(!set["reason"].as_str().unwrap().is_empty());
        if let Some(schema) = projected {
            reply_schema::validate_strict(&schema).unwrap();
        }
    }
}

#[test]
fn triage_contract_kernel_validation_and_roundtrip_boundaries() {
    let path = "fixtures/custom-tasks/triage/worker/outputs/triage.schema.json";
    // The custom payload shipped to the provider is the exact schema from #1106.
    let schema = read(path);
    let temp = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(temp.path().join("cas")).unwrap();
    let outputs = BTreeMap::from([("triage".into(), schema)]);
    let contract =
        WorkerContract::capture(&cas, json!({"type":"object"}), outputs.clone()).unwrap();
    let request = WorkerRequest::new("Triage.", BTreeMap::new(), vec![], outputs)
        .rendered()
        .unwrap();
    let projected = reply_schema::strict(&request).unwrap().unwrap();
    let leaf = &projected["properties"]["outputs"]["properties"]["triage"]["items"];
    assert_eq!(leaf["properties"]["schema"]["type"], "string");
    assert!(
        leaf["properties"]["class"]["type"]
            .as_array()
            .unwrap()
            .contains(&json!("null"))
    );
    let id = format!("sha256:{}", "a".repeat(64));
    let valid = json!({"schema":"custom.issue-triage/2", "source_artifact_ids":[id], "class":null,"size":null,"pipeline":null,"confidence":0.2,"rationale":"Missing evidence", "needs_info":["Supply repro"],"disposition":"needs_info","adr":null,"source_identity":{"revision":"captured","captured_at":"unknown","body_sha256":id}});
    let reply = |payload: Value| {
        serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"triage":[payload]}}))
            .unwrap()
    };
    let original = reply(valid.clone());
    let normalized = reply_schema::normalize_strict_reply(&request, &original);
    let parsed = contract.validate_reply(&normalized).unwrap();
    assert_eq!(parsed.outputs["triage"][0], valid);
    for (field, value) in [
        ("schema", json!("custom.issue-triage/3")),
        ("confidence", json!(1.1)),
        ("confidence", json!("0.2")),
        ("source_artifact_ids", json!(["sha256:bad"])),
        ("class", json!("bug")),
        ("size", json!("S")),
        (
            "source_identity",
            json!({"revision":"x","captured_at":"y","body_sha256":id,"unknown":1}),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert!(contract.validate_reply(&reply(invalid)).is_err(), "{field}");
    }
    for field in ["schema", "class", "source_identity"] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert!(
            contract.validate_reply(&reply(invalid)).is_err(),
            "missing differs from null: {field}"
        );
    }
    // Kernel envelopes still refuse unknown ports, unknown fields and bad versions.
    for bad in [
        json!({"schema":"af.worker-reply/2","outputs":{}}),
        json!({"schema":"af.worker-reply/1","outputs":{"unknown":[]}}),
        json!({"schema":"af.worker-reply/1","outputs":{},"extra":true}),
        json!({"schema":"af.worker-reply/1","outputs":{"triage":[{}]}}),
    ] {
        assert!(
            contract
                .validate_reply(&serde_json::to_vec(&bad).unwrap())
                .is_err()
        );
    }
}

#[test]
fn task_gate_schema_rejects_empty_labels_and_accepts_checked_in_config() {
    let validator = jsonschema::options()
        .build(&read("schemas/task-gate-v1.json"))
        .unwrap();
    let checked_in = read(".config/task-gate.json");
    assert!(
        validator.is_valid(&checked_in),
        "checked-in .config/task-gate.json must validate against schemas/task-gate-v1.json"
    );
    for invalid in [
        json!({"schema": "af.task-gate/1", "labels": [""], "exclude_release_steps": []}),
        json!({"schema": "af.task-gate/1", "labels": ["noe2e"], "exclude_release_steps": []}),
        json!({"schema": "af.task-gate/1", "labels": ["no-e2ee"], "exclude_release_steps": []}),
        json!({"schema": "af.task-gate/1", "labels": [], "exclude_release_steps": []}),
        json!({"schema": "af.task-gate/1", "labels": ["release:fast"], "exclude_release_steps": [""]}),
    ] {
        assert!(
            !validator.is_valid(&invalid),
            "expected schema rejection for {invalid}"
        );
    }
}
