use super::*;
use review_core::task::VerificationReserveV1;
use review_store::{Cas, content_id};
use serde_json::json;

const PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
[[nodes]]
id = "first/reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "second"
kind = "reviewer"
inputs = []
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reports", type = "review.kernel/Opaque@1", cardinality = "many", optional = false, snapshot_affinity = "any" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "first/reviewer", port = "prior_findings" }
[[edges]]
from = { node = "first/reviewer", port = "result" }
to = { node = "gather", port = "reports" }
[[edges]]
from = { node = "second", port = "result" }
to = { node = "gather", port = "reports" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

fn context(loaded: &Loaded) -> ReviewCompileContext {
    let id = content_id(&json!({"fixture": true})).unwrap();
    let input = |ty: &str| ArtifactInputV1 {
        artifact_ids: vec![id.clone()],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(id.clone()),
    };
    let workers = loaded
        .planned()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter))
        .map(|(name, _)| {
            (
                name.clone(),
                ReviewWorker {
                    package: "fixture/reviewer".into(),
                    allowance: NodeAllowance {
                        tokens_per_attempt: 100,
                        wall_ms_per_attempt: 1000,
                        max_attempts: 2,
                        verification_attempts: 1,
                    },
                },
            )
        })
        .collect();
    ReviewCompileContext {
        inputs: BTreeMap::from([
            ("head".into(), input(contract::SOURCE_SNAPSHOT_V1)),
            ("round".into(), input(REVIEW_ROUND_V1)),
        ]),
        head_input: "head".into(),
        round_input: "round".into(),
        workers,
        outputs: BTreeMap::from([(
            "findings".into(),
            Address {
                node: "ledger".into(),
                port: "findings".into(),
            },
        )]),
        limits: TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 8,
            deadline_unix_ms: 9999999999999,
            verification: VerificationReserveV1 {
                tokens: 400,
                attempts: 4,
                wall_ms: 4000,
            },
        },
        max_parallel: 2,
        gate_wall_ms: 2000,
    }
}

#[test]
fn static_review_has_public_contract_typed_lanes_transitive_gates_and_one_budget() {
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    let graph = &compilation.graph;
    assert_eq!(graph.nodes.len(), 7);
    assert_eq!(graph.allowances.len(), 3);
    assert_eq!(graph.calls.len(), 1);
    assert_eq!(graph.slots.len(), 2);
    assert_eq!(compilation.contract.inputs.len(), 2);
    assert_eq!(compilation.contract.outputs.len(), 1);
    compilation.contract.validate().unwrap();
    assert_eq!(
        compilation,
        compile_legacy_review(&loaded, context(&loaded)).unwrap()
    );
    let gate = &compilation.nodes["gate"];
    for name in ["first/reviewer", "second", "gather", "ledger"] {
        let mapping = &compilation.nodes[name];
        assert!(mapping.task_node.split('.').all(review_core::task::is_name));
        let node = &graph.nodes[&mapping.task_node];
        assert!(
            node.conditions
                .iter()
                .any(|condition| condition.source.node == gate.task_node
                    && condition.source.port == "outcome"
                    && condition.outcome == ReceiptOutcomeV1::Passed)
        );
        assert!(graph.requires_successful_predecessors(&mapping.task_node));
    }
    for name in ["first/reviewer", "second"] {
        let node = &graph.nodes[&compilation.nodes[name].task_node];
        assert_eq!(node.contract.outputs.len(), 2);
        assert_eq!(
            node.contract.outputs["o0"].artifact_type,
            contract::REVIEWER_RESULT_V1
        );
        assert_eq!(
            node.contract.outputs["metadata"].artifact_type,
            TASK_REVIEW_RESULT_METADATA_V1
        );
    }
    let gather = &compilation.nodes["gather"];
    assert_eq!(gather.inputs.len(), 2);
    assert!(
        gather
            .inputs
            .values()
            .all(|lane| lane.review_port == "reports")
    );
    let task_gather = &graph.nodes[&gather.task_node];
    assert_eq!(
        task_gather.contract.inputs["i0"].cardinality,
        PortCardinality::One
    );
    assert_eq!(
        task_gather.contract.inputs["i1"].artifact_type,
        contract::REVIEWER_RESULT_V1
    );
    assert_eq!(graph.scheduler_plan().unwrap().order, graph.order);
    let wire = serde_json::to_value(&compilation).unwrap();
    assert_eq!(
        serde_json::from_value::<LegacyReviewCompilation>(wire).unwrap(),
        compilation
    );
}

#[test]
fn restored_fan_in_sorts_original_ids_and_refuses_missing_or_extra_lanes() {
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    let mapping = &compilation.nodes["gather"];
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let mut expected = Vec::new();
    let mut lanes = review_graph::ArtifactMap::new();
    for (index, (name, lane)) in mapping.inputs.iter().enumerate() {
        let raw = cas.put_json(&json!({"summary": index})).unwrap();
        let id = lane
            .codec
            .capture(
                &cas,
                &raw,
                review_core::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: None,
                    operation_id: format!("capture-{index}"),
                },
                None,
            )
            .unwrap();
        lanes.insert(name.clone(), vec![id]);
        expected.push(raw);
    }
    expected.sort();
    assert_eq!(
        mapping.restore_inputs(&cas, &lanes).unwrap(),
        BTreeMap::from([("reports".into(), expected)])
    );
    let removed = lanes.remove("i0").unwrap();
    assert!(mapping.restore_inputs(&cas, &lanes).is_err());
    lanes.insert("i0".into(), removed);
    lanes.insert("ambient".into(), Vec::new());
    assert!(mapping.restore_inputs(&cas, &lanes).is_err());
}

#[test]
fn v1_opaque_generation_is_explicitly_adapted_but_v2_stays_strict() {
    let v1 = PIPELINE.replace("version = 2\n[subject]\nkind = \"whole-tree\"", "version = 1")
        .replace("outputs = [{ name = \"findings\", type = \"review.kernel/PriorFindings@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]", "outputs = [\"findings\"]")
        .replace("inputs = [{ name = \"prior_findings\", type = \"review.kernel/PriorFindings@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]", "inputs = [\"prior_findings\"]");
    let loaded = crate::Definition::from_toml(&v1).unwrap().load().unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    assert_eq!(
        compilation.nodes["generation"].outputs["o0"].codec,
        ReviewArtifactCodec::Flat {
            artifact_type: contract::PRIOR_FINDINGS_V1.into()
        }
    );
    let v2 = v1.replace(
        "version = 1",
        "version = 2\n[subject]\nkind = \"whole-tree\"",
    );
    assert!(crate::Definition::from_toml(&v2).unwrap().load().is_err());
    let mut bad = context(&loaded);
    bad.workers.remove("second");
    assert!(compile_legacy_review(&loaded, bad).is_err());
    let mut bad = context(&loaded);
    bad.inputs.get_mut("round").unwrap().artifact_type = contract::OPAQUE_V1.into();
    assert!(compile_legacy_review(&loaded, bad).is_err());
    let mut bad = context(&loaded);
    bad.outputs.get_mut("findings").unwrap().port = "undeclared".into();
    assert!(compile_legacy_review(&loaded, bad).is_err());
}

#[test]
fn dynamic_review_retains_enveloped_history_and_shared_provider_guards() {
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    use review_graph::task::OperatorAttemptCost;
    let loaded =
        crate::Definition::from_toml(include_str!("../../../tests/fixtures/dynamic-v5.toml"))
            .unwrap()
            .load()
            .unwrap();
    let config = context(&loaded);
    let limits = config.limits.clone();
    let mut compilation = compile_legacy_review(&loaded, config).unwrap();
    let generation = &compilation.nodes["generation"];
    assert_eq!(
        generation.outputs["o0"].codec,
        ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into()
        }
    );
    let scatter = &compilation.nodes["scatter"];
    let history = scatter
        .inputs
        .iter()
        .find(|(_, lane)| lane.review_port == "prior_findings")
        .unwrap();
    let history_port = &compilation.graph.nodes[&scatter.task_node].contract.inputs[history.0];
    assert!(history_port.optional);
    assert_eq!(history_port.affinity, PortAffinityV1::Unbound {});
    assert_eq!(
        compilation.contract.outputs["findings"].affinity,
        PortAffinityV1::SameAs {
            input: "head".into()
        }
    );
    let id = content_id(&json!({"binding": "captured"})).unwrap();
    let bindings = compilation
        .graph
        .slots
        .keys()
        .map(|slot| {
            (
                slot.clone(),
                EffectiveWorkerBindingV1 {
                    package_digest: id.clone(),
                    package_artifact_id: id.clone(),
                    invocation_policy_id: id.clone(),
                    execution: WorkerExecutionV1::Model {
                        provider: "personal".into(),
                        provider_kind: "fixture".into(),
                        principal_id: "same-person".into(),
                        model: "fixture-model".into(),
                        effort: "high".into(),
                    },
                },
            )
        })
        .collect();
    compilation
        .graph
        .require_provider_admission(
            &bindings,
            &OperatorAttemptCost {
                tokens: 10,
                wall_ms: 1000,
            },
            &limits,
        )
        .unwrap();
    let admissions: Vec<_> = compilation
        .graph
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.operator, CompiledOperator::ProviderAdmission { .. }))
        .collect();
    assert_eq!(admissions.len(), 1);
    for name in ["scatter", "closeout"] {
        let node = &compilation.graph.nodes[&compilation.nodes[name].task_node];
        assert!(
            node.conditions
                .iter()
                .any(|condition| condition.source.node == *admissions[0].0)
        );
    }
}
