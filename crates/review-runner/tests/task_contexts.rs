//! Actual context capture must satisfy public payload schemas. No adapter is invoked.
use review_core::task::execution::TaskInvocationV1;
use review_core::task::provider::TaskProviderAdmissionV2;
use review_core::task::{ArtifactInputV1, plan::WorkerExecutionV1};
use review_core::{PortCardinality, Producer};
use review_runner::ContextManifest;
use review_runner::task::provider::{PROBE_INPUT, TaskProviderContextV2};
use review_runner::task::{TASK_CONTEXT_V1, TaskContext, WorkerContract};
use review_store::Cas;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn validator(name: &str) -> jsonschema::Validator {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    let read = |name: &str| {
        serde_json::from_slice::<Value>(&std::fs::read(root.join(name)).unwrap()).unwrap()
    };
    let mut registry = jsonschema::Registry::new();
    for name in [
        "task-contracts-v1.json",
        "task-invocation-v1.json",
        "task-provider-admission-v2.json",
    ] {
        let schema = read(name);
        let id = schema["$id"].as_str().unwrap().to_owned();
        registry = registry
            .add(id, jsonschema::Resource::from_contents(schema))
            .unwrap();
    }
    {
        let registry = registry.prepare().unwrap();
        jsonschema::options()
            .with_registry(&registry)
            .build(&read(name))
            .unwrap()
    }
}

fn persist(cas: &Cas, original: &str, payload: Value) -> String {
    let frame = cas.get_artifact(original).unwrap();
    cas.put_artifact(
        TASK_CONTEXT_V1,
        frame.producer,
        frame.input_artifacts,
        None,
        payload,
    )
    .unwrap()
    .0
}

#[test]
fn captured_worker_context_matches_schema_and_rejects_structural_drift_before_read() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path()).unwrap();
    let contract = WorkerContract::capture(&cas,
        json!({"type":"object","additionalProperties":false,"required":["source"],"properties":{"source":{"type":"array","minItems":1,"maxItems":1}}}),
        BTreeMap::from([("answer".into(),json!({"type":"object"}))])).unwrap();
    let source = cas
        .put_artifact(
            "af/Source@1",
            Producer::KernelOperation {
                run_id: "context-fixture".into(),
                node_id: None,
                operation_id: "capture@1".into(),
            },
            vec![],
            None,
            json!({"text":"Only declared data"}),
        )
        .unwrap()
        .0;
    let input = ArtifactInputV1 {
        artifact_type: "af/Source@1".into(),
        artifact_ids: vec![source],
        cardinality: PortCardinality::One,
        snapshot_id: None,
    };
    let invocation = TaskInvocationV1 {
        plan_id: cas.put(b"captured plan").unwrap(),
        node: "root.worker".into(),
        inputs: BTreeMap::from([("source".into(), input.clone())]),
    };
    let id = contract
        .prepare(&cas, &invocation, &[], "Captured instructions")
        .unwrap();
    let original = cas.get_artifact(&id).unwrap().payload;
    let schema = validator("task-context-v1.json");
    schema.validate(&original).unwrap();
    let (context, rendered) = contract.read_context(&cas, &id).unwrap();
    assert_eq!(serde_json::to_value(&context).unwrap(), original);
    assert_eq!(context.manifest.rendered_bytes, rendered.len() as u64);
    assert_eq!(
        contract
            .prepare(&cas, &invocation, &[], "Captured instructions")
            .unwrap(),
        id
    );
    for (path, replacement) in [
        ("/invocation/plan_id", json!("ambient-plan")),
        ("/invocation/node", json!("root..worker")),
        ("/rendered_id", json!("mutable")),
        ("/feedback_ids", json!([id, id])),
        ("/manifest/entries", json!([])),
        (
            "/manifest/entries/0/estimated_tokens",
            json!(9_007_199_254_740_992_u64),
        ),
        ("/manifest/rendered_bytes", json!(1_048_577)),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert!(!schema.is_valid(&invalid), "{path}");
        assert!(
            serde_json::from_value::<TaskContext>(invalid.clone())
                .map_err(|e| e.to_string())
                .and_then(|c| c.validate())
                .is_err(),
            "{path}"
        );
        // Safe JSON values can still be stored as unreferenced data; the context reader refuses.
        if path != "/manifest/entries/0/estimated_tokens" {
            assert!(
                contract
                    .read_context(&cas, &persist(&cas, &id, invalid))
                    .is_err(),
                "{path}"
            );
        }
    }
    let mut extra = original.clone();
    extra["authority"] = json!({"effects":["execute"]});
    assert!(!schema.is_valid(&extra));
    assert!(serde_json::from_value::<TaskContext>(extra).is_err());
    // A different valid digest is structurally valid, but never acquires captured authority.
    let mut substituted = original.clone();
    substituted["contract_id"] = json!(cas.put(b"another contract").unwrap());
    assert!(schema.is_valid(&substituted));
    assert!(
        contract
            .read_context(&cas, &persist(&cas, &id, substituted))
            .unwrap_err()
            .contains("another contract")
    );
    let mut undeclared = invocation.clone();
    undeclared.inputs.insert("undeclared".into(), input);
    assert!(
        contract
            .prepare(&cas, &undeclared, &[], "Captured instructions")
            .unwrap_err()
            .contains("input schema")
    );
    // JSON Schema cannot express the estimator relation; the same Rust reader checks it.
    let mut estimate = original;
    estimate["manifest"]["estimated_tokens"] = json!(0);
    assert!(schema.is_valid(&estimate));
    assert!(
        contract
            .read_context(&cas, &persist(&cas, &id, estimate))
            .is_err()
    );
}

#[test]
fn actual_fixed_provider_context_v2_matches_schema_and_rechecks_equal_identities() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path()).unwrap();
    let rendered_id = cas.put(PROBE_INPUT).unwrap();
    let plan_id = cas.put(b"plan").unwrap();
    let mut manifest = ContextManifest::default();
    manifest.record(
        "capability_probe",
        "installed Provider admission",
        Some(rendered_id.clone()),
        None,
        PROBE_INPUT.len(),
    );
    manifest.finish(PROBE_INPUT.len());
    let mut context = TaskProviderContextV2 {
        invocation: TaskInvocationV1 {
            plan_id: plan_id.clone(),
            node: "root.provider".into(),
            inputs: BTreeMap::new(),
        },
        capability: TaskProviderAdmissionV2 {
            plan_id,
            bindings: std::collections::BTreeSet::from(["root.worker".into()]),
            execution: WorkerExecutionV1::Model {
                provider: "alias".into(),
                provider_kind: "fixture".into(),
                principal_id: "account".into(),
                model: "model".into(),
                effort: "high".into(),
            },
            probe_policy_id: cas.put(b"probe policy").unwrap(),
            outcome: review_core::task::pipeline::ReceiptOutcomeV1::Passed,
        },
        rendered_id,
        manifest,
    };
    context.validate().unwrap();
    let schema = validator("task-provider-context-v2.json");
    schema
        .validate(&serde_json::to_value(&context).unwrap())
        .unwrap();
    context.capability.plan_id = cas.put(b"different plan").unwrap();
    assert!(schema.is_valid(&serde_json::to_value(&context).unwrap()));
    assert!(context.validate().is_err());
}
