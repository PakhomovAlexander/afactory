use std::collections::{BTreeMap, BTreeSet};

use review_core::task::{TaskRevisionV1, pipeline::*};
use review_graph::task::{CompileContext, OperatorSignature, compile_task};

fn fixture() -> (
    TaskRevisionV1,
    BTreeMap<String, PipelineDefinitionV1>,
    BTreeMap<String, OperatorSignature>,
) {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("fixtures/task-contracts/v1");
    let task: TaskRevisionV1 =
        serde_json::from_slice(&std::fs::read(root.join("task-revision.json")).unwrap()).unwrap();
    let definition: PipelineDefinitionV1 =
        serde_json::from_slice(&std::fs::read(root.join("pipeline-definition.json")).unwrap())
            .unwrap();
    let mut output = definition.contract.outputs["document"].clone();
    output.covers.clear();
    let signature = OperatorSignature {
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([(
                "input".into(),
                definition.contract.inputs["requirements"].clone(),
            )]),
            outputs: BTreeMap::from([("output".into(), output)]),
        },
        effects: BTreeSet::from(["read-source".into()]),
        evidence: BTreeMap::from([(
            "output".into(),
            BTreeSet::from([task.acceptance["checked"].verifier_policy.clone()]),
        )]),
        roles: BTreeSet::from(["author".into()]),
        worker_input_type: Some("af/Requirements@1".into()),
        worker_output_type: Some("af/CheckedDocument@1".into()),
        outcome_port: None,
    };
    (
        task,
        BTreeMap::from([(definition.name.clone(), definition)]),
        BTreeMap::from([("worker/builtin/document-author".into(), signature)]),
    )
}

#[test]
fn compilation_is_deterministic_and_root_business_inputs_are_explicit_nodes() {
    let (task, pipelines, signatures) = fixture();
    let context = CompileContext {
        acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
        pipelines: &pipelines,
        signatures: &signatures,
        max_nodes: 64,
        max_depth: 4,
    };
    let a = compile_task(&task, "builtin/document", &context).unwrap();
    let b = compile_task(&task, "builtin/document", &context).unwrap();
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
    assert_eq!(a.order, ["root.inputs", "root.nodes.write"]);
    assert_eq!(
        a.nodes["root.nodes.write"].inputs["input"].qualified(),
        "root.inputs.requirements"
    );
    assert_eq!(
        a.inputs["requirements"].artifact_ids,
        task.inputs["requirements"].artifact_ids
    );
    assert_eq!(a.coverage["checked"], a.outputs["document"]);
}

#[test]
fn declarations_cannot_invent_worker_contracts_evidence_or_effect_authority() {
    let (mut task, pipelines, signatures) = fixture();
    let mut wrong = signatures.clone();
    wrong
        .get_mut("worker/builtin/document-author")
        .unwrap()
        .evidence
        .clear();
    assert!(
        compile_task(
            &task,
            "builtin/document",
            &CompileContext {
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                pipelines: &pipelines,
                signatures: &wrong,
                max_nodes: 64,
                max_depth: 4
            }
        )
        .unwrap_err()
        .contains("trusted producer")
    );
    wrong = signatures.clone();
    wrong
        .get_mut("worker/builtin/document-author")
        .unwrap()
        .worker_input_type = Some("af/Other@1".into());
    assert!(
        compile_task(
            &task,
            "builtin/document",
            &CompileContext {
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                pipelines: &pipelines,
                signatures: &wrong,
                max_nodes: 64,
                max_depth: 4
            }
        )
        .unwrap_err()
        .contains("does not implement slot")
    );
    task.authority.allowed_effects.clear();
    assert!(
        compile_task(
            &task,
            "builtin/document",
            &CompileContext {
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                pipelines: &pipelines,
                signatures: &signatures,
                max_nodes: 64,
                max_depth: 4
            }
        )
        .unwrap_err()
        .contains("authority")
    );
}

#[test]
fn embedding_checks_the_child_interface_without_applying_its_root_kind_selector() {
    let (task, mut pipelines, signatures) = fixture();
    let mut parent = pipelines["builtin/document"].clone();
    parent.name = "team/document".into();
    parent.slots.clear();
    parent.nodes[0].operator = TaskOperatorV1::Call {
        pipeline: "builtin/document".into(),
        bindings: BTreeMap::new(),
    };
    parent.nodes[0].inputs = BTreeMap::from([(
        "requirements".into(),
        ValueRefV1::Input {
            port: "requirements".into(),
        },
    )]);
    parent
        .outputs
        .get_mut("document")
        .unwrap()
        .clone_from(&ValueRefV1::Node {
            node: "write".into(),
            port: "document".into(),
        });
    parent
        .coverage
        .get_mut("checked")
        .unwrap()
        .clone_from(&parent.outputs["document"]);
    pipelines.get_mut("builtin/document").unwrap().accepts.kinds =
        BTreeSet::from(["review".into()]);
    pipelines.insert(parent.name.clone(), parent);
    let context = CompileContext {
        acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
        pipelines: &pipelines,
        signatures: &signatures,
        max_nodes: 64,
        max_depth: 4,
    };
    let graph = compile_task(&task, "team/document", &context).unwrap();
    assert_eq!(
        graph.outputs["document"].qualified(),
        "root.nodes.write.nodes.write.output"
    );
    assert_eq!(graph.calls.len(), 2);
    assert_eq!(
        graph.nodes.len(),
        2,
        "embedding expands into the parent's graph"
    );
    assert!(
        compile_task(&task, "builtin/document", &context)
            .unwrap_err()
            .contains("kind")
    );
}

#[test]
fn missing_inputs_cycles_unknown_ports_and_recursive_calls_fail_before_dispatch() {
    let (task, pipelines, signatures) = fixture();
    for defect in ["input", "port", "cycle", "recursive"] {
        let mut broken = pipelines.clone();
        let pipeline = broken.get_mut("builtin/document").unwrap();
        match defect {
            "input" => pipeline.nodes[0].inputs.clear(),
            "port" => pipeline
                .outputs
                .insert(
                    "document".into(),
                    ValueRefV1::Node {
                        node: "write".into(),
                        port: "missing".into(),
                    },
                )
                .map(|_| ())
                .unwrap(),
            "cycle" => {
                pipeline.nodes[0].inputs.insert(
                    "input".into(),
                    ValueRefV1::Node {
                        node: "write".into(),
                        port: "output".into(),
                    },
                );
            }
            "recursive" => {
                pipeline.nodes[0].operator = TaskOperatorV1::Call {
                    pipeline: "builtin/document".into(),
                    bindings: BTreeMap::new(),
                };
                pipeline.nodes[0].inputs = BTreeMap::from([(
                    "requirements".into(),
                    ValueRefV1::Input {
                        port: "requirements".into(),
                    },
                )]);
            }
            _ => unreachable!(),
        }
        assert!(
            compile_task(
                &task,
                "builtin/document",
                &CompileContext {
                    acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                    pipelines: &broken,
                    signatures: &signatures,
                    max_nodes: 64,
                    max_depth: 4
                }
            )
            .is_err(),
            "{defect}"
        );
    }
}

fn branching_fixture() -> (
    TaskRevisionV1,
    BTreeMap<String, PipelineDefinitionV1>,
    BTreeMap<String, OperatorSignature>,
) {
    let (task, mut pipelines, mut signatures) = fixture();
    signatures
        .get_mut("worker/builtin/document-author")
        .unwrap()
        .outcome_port = Some("output".into());
    let pipeline = pipelines.get_mut("builtin/document").unwrap();
    let writer = pipeline.nodes[0].clone();
    for (id, outcome) in [
        ("passed", ReceiptOutcomeV1::Passed),
        ("failed", ReceiptOutcomeV1::Failed),
        ("inconclusive", ReceiptOutcomeV1::Inconclusive),
    ] {
        let mut branch = writer.clone();
        branch.id = id.into();
        branch.when = Some(NodeConditionV1 {
            node: "write".into(),
            outcome,
        });
        pipeline.nodes.push(branch);
    }
    let reference = |node: &str| ValueRefV1::Node {
        node: node.into(),
        port: "output".into(),
    };
    pipeline.nodes.push(TaskNodeV1 {
        id: "choose".into(),
        operator: TaskOperatorV1::Select {},
        when: None,
        inputs: BTreeMap::from([
            ("condition".into(), reference("write")),
            ("passed".into(), reference("passed")),
            ("failed".into(), reference("failed")),
            ("inconclusive".into(), reference("inconclusive")),
        ]),
    });
    pipeline
        .outputs
        .insert("document".into(), reference("choose"));
    pipeline
        .coverage
        .insert("checked".into(), reference("choose"));
    (task, pipelines, signatures)
}

#[test]
fn bounded_branches_execute_only_the_selected_arm_and_keep_evidence_coverage() {
    use review_graph::{ArtifactMap, Dispatch, Node, NodeOutcome, Scheduler, SuppressionReason};
    use std::sync::Mutex;
    let (task, pipelines, signatures) = branching_fixture();
    let graph = compile_task(
        &task,
        "builtin/document",
        &CompileContext {
            pipelines: &pipelines,
            signatures: &signatures,
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
            max_nodes: 64,
            max_depth: 4,
        },
    )
    .unwrap();
    let planned = graph.scheduler_plan().unwrap();
    struct Branches<'a> {
        graph: &'a review_graph::task::CompiledTask,
        outcome: ReceiptOutcomeV1,
        invocations: Mutex<Vec<String>>,
    }
    impl Dispatch for Branches<'_> {
        fn task_node_selected(&self, node: &Node, inputs: &ArtifactMap) -> Result<bool, String> {
            self.graph
                .node_selected(&node.id, inputs, |_| Ok(self.outcome))
        }
        fn record_invocation(&self, node: &Node, _: &ArtifactMap) -> Result<(), String> {
            self.invocations.lock().unwrap().push(node.id.clone());
            Ok(())
        }
        fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            match node.id.as_str() {
                "root.inputs" => Ok(self
                    .graph
                    .inputs
                    .iter()
                    .map(|(p, i)| (p.clone(), i.artifact_ids.clone()))
                    .collect()),
                "root.nodes.choose" => self
                    .graph
                    .select_output(&node.id, inputs, |_| Ok(self.outcome)),
                _ => Ok(BTreeMap::from([("output".into(), vec![node.id.clone()])])),
            }
        }
    }
    for (selected, outcome) in [
        ("passed", ReceiptOutcomeV1::Passed),
        ("failed", ReceiptOutcomeV1::Failed),
        ("inconclusive", ReceiptOutcomeV1::Inconclusive),
    ] {
        let dispatch = Branches {
            graph: &graph,
            outcome,
            invocations: Mutex::new(Vec::new()),
        };
        let report = Scheduler::new(&planned).run(&dispatch);
        let NodeOutcome::Completed { outputs } = report.outcome("root.nodes.choose").unwrap()
        else {
            panic!("{report:?}")
        };
        assert_eq!(outputs["output"], [format!("root.nodes.{selected}")]);
        let invoked = dispatch.invocations.lock().unwrap();
        assert_eq!(invoked.len(), 4, "root, receipt, selected arm, Select");
        for inactive in ["passed", "failed", "inconclusive"]
            .into_iter()
            .filter(|arm| *arm != selected)
        {
            assert!(!invoked.contains(&format!("root.nodes.{inactive}")));
            assert_eq!(
                report.outcome(&format!("root.nodes.{inactive}")),
                Some(&NodeOutcome::Suppressed {
                    reason: SuppressionReason::BranchNotSelected
                })
            );
        }
    }
}

#[test]
fn branch_contracts_reject_missing_paths_wrong_arms_and_untyped_conditions() {
    let (task, pipelines, signatures) = branching_fixture();
    for defect in [
        "missing-path",
        "wrong-arm",
        "not-a-receipt",
        "conditional-condition",
        "condition-cycle",
    ] {
        let mut pipes = pipelines.clone();
        let mut sigs = signatures.clone();
        let pipeline = pipes.get_mut("builtin/document").unwrap();
        match defect {
            "missing-path" => {
                pipeline.outputs.insert(
                    "document".into(),
                    ValueRefV1::Node {
                        node: "passed".into(),
                        port: "output".into(),
                    },
                );
            }
            "wrong-arm" => {
                pipeline.nodes.last_mut().unwrap().inputs.insert(
                    "failed".into(),
                    ValueRefV1::Node {
                        node: "passed".into(),
                        port: "output".into(),
                    },
                );
            }
            "not-a-receipt" => {
                sigs.get_mut("worker/builtin/document-author")
                    .unwrap()
                    .outcome_port = None
            }
            "conditional-condition" => {
                pipeline.nodes[2].when.as_mut().unwrap().node = "passed".into()
            }
            "condition-cycle" => {
                pipeline.nodes[0].when = Some(NodeConditionV1 {
                    node: "passed".into(),
                    outcome: ReceiptOutcomeV1::Passed,
                })
            }
            _ => unreachable!(),
        }
        assert!(
            compile_task(
                &task,
                "builtin/document",
                &CompileContext {
                    pipelines: &pipes,
                    signatures: &sigs,
                    acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                    max_nodes: 64,
                    max_depth: 4,
                }
            )
            .is_err(),
            "{defect}"
        );
    }
}
