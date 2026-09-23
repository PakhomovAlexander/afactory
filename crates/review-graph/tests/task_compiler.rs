#[path = "task_compiler/node_bounds.rs"]
mod node_bounds;

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
        retains: BTreeMap::new(),
        worker_input_type: Some("af/Requirements@1".into()),
        worker_output_type: Some("af/CheckedDocument@1".into()),
        outcome_port: None,
        attempt: Some(review_graph::task::OperatorAttemptCost {
            tokens: 0,
            wall_ms: 1000,
        }),
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
        slot_workers: BTreeMap::new(),
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
fn installed_review_retains_its_optional_predecessor_barrier_and_task_branch_decision() {
    use review_graph::task::{CompiledOperator, CompiledTask, ReviewOperation};
    use review_graph::{ArtifactMap, Dispatch, Node, NodeOutcome, SuppressionReason};
    use std::sync::Mutex;

    struct Host<'a> {
        graph: &'a CompiledTask,
        upstream_fails: bool,
        selected: bool,
        invocations: Mutex<Vec<String>>,
    }
    impl Dispatch for Host<'_> {
        fn requires_successful_predecessors(&self, node: &Node) -> bool {
            self.graph.requires_successful_predecessors(&node.id)
        }
        fn task_node_selected(&self, node: &Node, _: &ArtifactMap) -> Result<bool, String> {
            Ok(node.id == "root.inputs" || self.selected)
        }
        fn record_invocation(&self, node: &Node, _: &ArtifactMap) -> Result<(), String> {
            self.invocations.lock().unwrap().push(node.id.clone());
            Ok(())
        }
        fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            if node.id == "root.inputs" {
                if self.upstream_fails {
                    return Err("upstream did not produce a receipt".into());
                }
                return Ok(BTreeMap::from([("requirements".into(), vec![])]));
            }
            assert!(
                inputs["input"].is_empty(),
                "a completed optional producer may have no value"
            );
            Ok(BTreeMap::from([(
                "output".into(),
                vec!["completed-output".into()],
            )]))
        }
    }
    let (task, pipelines, signatures) = fixture();
    let mut graph = compile_task(
        &task,
        "builtin/document",
        &CompileContext {
            slot_workers: BTreeMap::new(),
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
            pipelines: &pipelines,
            signatures: &signatures,
            max_nodes: 64,
            max_depth: 4,
        },
    )
    .unwrap();
    graph
        .nodes
        .get_mut("root.inputs")
        .unwrap()
        .contract
        .outputs
        .get_mut("requirements")
        .unwrap()
        .optional = true;
    graph
        .nodes
        .get_mut("root.nodes.write")
        .unwrap()
        .contract
        .inputs
        .get_mut("input")
        .unwrap()
        .optional = true;
    let ordinary = graph.nodes["root.nodes.write"].operator.clone();
    for (compatibility, upstream_fails, selected) in [
        (false, true, true),
        (true, true, true),
        (true, false, true),
        (true, false, false),
    ] {
        graph.nodes.get_mut("root.nodes.write").unwrap().operator = if compatibility {
            CompiledOperator::ReviewDomain {
                review_node: "gather".into(),
                operation: ReviewOperation::Gather,
            }
        } else {
            ordinary.clone()
        };
        let host = Host {
            graph: &graph,
            upstream_fails,
            selected,
            invocations: Mutex::new(vec![]),
        };
        let report = graph.run(&host).unwrap();
        let ran = host
            .invocations
            .lock()
            .unwrap()
            .contains(&"root.nodes.write".to_string());
        match (compatibility && upstream_fails, selected) {
            (true, _) => {
                assert!(!ran);
                assert!(matches!(
                    report.outcome("root.nodes.write"),
                    Some(NodeOutcome::Suppressed {
                        reason: SuppressionReason::UpstreamMissing
                    })
                ));
            }
            (false, false) => {
                assert!(!ran);
                assert!(matches!(
                    report.outcome("root.nodes.write"),
                    Some(NodeOutcome::Suppressed {
                        reason: SuppressionReason::BranchNotSelected
                    })
                ));
            }
            (false, true) => {
                assert!(ran);
                assert!(matches!(
                    report.outcome("root.nodes.write"),
                    Some(NodeOutcome::Completed { .. })
                ));
            }
        }
    }
}

#[test]
fn installed_review_worker_is_guarded_by_the_same_compiled_provider_admission() {
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    use review_graph::task::{CompiledOperator, OperatorAttemptCost, ReviewOperation};
    let (task, pipelines, signatures) = fixture();
    let mut graph = compile_task(
        &task,
        "builtin/document",
        &CompileContext {
            slot_workers: BTreeMap::new(),
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
            pipelines: &pipelines,
            signatures: &signatures,
            max_nodes: 64,
            max_depth: 4,
        },
    )
    .unwrap();
    let CompiledOperator::Primitive {
        operator: TaskOperatorV1::Worker { slot },
        ..
    } = &graph.nodes["root.nodes.write"].operator
    else {
        panic!("fixture Worker")
    };
    let slot = slot.clone();
    graph.nodes.get_mut("root.nodes.write").unwrap().operator = CompiledOperator::ReviewDomain {
        review_node: "correctness".into(),
        operation: ReviewOperation::Reviewer { slot: slot.clone() },
    };
    let bindings = BTreeMap::from([(
        slot.clone(),
        EffectiveWorkerBindingV1 {
            package_digest: "a".repeat(64),
            package_artifact_id: "b".repeat(64),
            invocation_policy_id: "c".repeat(64),
            execution: WorkerExecutionV1::Model {
                provider: "personal".into(),
                provider_kind: "fixture".into(),
                principal_id: "fixture-principal".into(),
                model: "fixture-model".into(),
                effort: "high".into(),
            },
        },
    )]);
    graph
        .install_provider_admission(
            &bindings,
            &OperatorAttemptCost {
                tokens: 11,
                wall_ms: 12,
            },
        )
        .unwrap();
    let admissions: Vec<_> = graph
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.operator, CompiledOperator::ProviderAdmission { .. }))
        .collect();
    assert_eq!(admissions.len(), 1);
    let (id, admission) = admissions[0];
    assert!(
        matches!(&admission.operator, CompiledOperator::ProviderAdmission { bindings } if bindings == &BTreeSet::from([slot]))
    );
    assert_eq!(graph.allowances[id].tokens_per_attempt, 11);
    assert_eq!(
        graph.nodes["root.nodes.write"]
            .conditions
            .last()
            .unwrap()
            .source
            .node,
        *id
    );
    assert_eq!(
        graph.nodes["root.nodes.write"]
            .conditions
            .last()
            .unwrap()
            .outcome,
        ReceiptOutcomeV1::Passed
    );
    assert!(
        graph
            .install_provider_admission(
                &bindings,
                &OperatorAttemptCost {
                    tokens: 11,
                    wall_ms: 12
                }
            )
            .is_err()
    );
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
                slot_workers: BTreeMap::new(),
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
                slot_workers: BTreeMap::new(),
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
                slot_workers: BTreeMap::new(),
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
fn provider_admission_groups_exact_executions_and_preserves_reservations() {
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    use review_graph::task::{CompiledOperator, OperatorAttemptCost};
    let (task, pipelines, signatures) = fixture();
    let mut base = compile_task(
        &task,
        "builtin/document",
        &CompileContext {
            slot_workers: BTreeMap::new(),
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
            pipelines: &pipelines,
            signatures: &signatures,
            max_nodes: 64,
            max_depth: 4,
        },
    )
    .unwrap();
    let CompiledOperator::Primitive {
        operator: TaskOperatorV1::Worker { slot },
        ..
    } = &base.nodes["root.nodes.write"].operator
    else {
        panic!("Worker")
    };
    let first = slot.clone();
    let second = "root.second".to_string();
    let mut node = base.nodes["root.nodes.write"].clone();
    let CompiledOperator::Primitive {
        operator: TaskOperatorV1::Worker { slot },
        ..
    } = &mut node.operator
    else {
        unreachable!()
    };
    *slot = second.clone();
    base.nodes.insert("root.nodes.second".into(), node);
    base.slots
        .insert(second.clone(), base.slots[&first].clone());
    base.allowances.insert(
        "root.nodes.second".into(),
        base.allowances["root.nodes.write"].clone(),
    );
    let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let binding = EffectiveWorkerBindingV1 {
        package_digest: digest('a'),
        package_artifact_id: digest('b'),
        invocation_policy_id: digest('c'),
        execution: WorkerExecutionV1::Model {
            provider: "personal".into(),
            provider_kind: "fixture".into(),
            principal_id: "account".into(),
            model: "resolved".into(),
            effort: "high".into(),
        },
    };
    let bindings = BTreeMap::from([(first.clone(), binding.clone()), (second.clone(), binding)]);
    let cost = OperatorAttemptCost {
        tokens: 7,
        wall_ms: 19,
    };
    for split in 0..3 {
        let mut bindings = bindings.clone();
        match split {
            1 => bindings.get_mut(&second).unwrap().invocation_policy_id = digest('f'),
            2 => {
                let WorkerExecutionV1::Model { provider, .. } =
                    &mut bindings.get_mut(&second).unwrap().execution
                else {
                    unreachable!()
                };
                *provider = "another-alias".into();
            }
            _ => {}
        }
        let mut graph = base.clone();
        graph.install_provider_admission(&bindings, &cost).unwrap();
        let admissions: Vec<_> = graph
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.operator, CompiledOperator::ProviderAdmission { .. }))
            .collect();
        let expected: Vec<String> = (0..if split == 0 { 1 } else { 2 })
            .map(|index| format!("root.providers.admit{index}"))
            .collect();
        assert_eq!(
            admissions
                .iter()
                .map(|(name, _)| (*name).clone())
                .collect::<Vec<_>>(),
            expected
        );
        for (name, node) in admissions {
            assert_eq!(
                node.contract.outputs["result"].artifact_type,
                "af/TaskProviderAdmission@1"
            );
            let allowance = &graph.allowances[name];
            assert_eq!(
                (
                    allowance.tokens_per_attempt,
                    allowance.wall_ms_per_attempt,
                    allowance.max_attempts
                ),
                (7, 19, 1)
            );
        }
        for node in ["root.nodes.write", "root.nodes.second"] {
            assert_eq!(
                graph.nodes[node].conditions.last().unwrap().outcome,
                ReceiptOutcomeV1::Passed
            );
        }
    }
    for cost in [
        OperatorAttemptCost {
            tokens: 0,
            wall_ms: 19,
        },
        OperatorAttemptCost {
            tokens: 7,
            wall_ms: 0,
        },
    ] {
        assert!(
            base.clone()
                .install_provider_admission(&bindings, &cost)
                .is_err()
        );
    }
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
        slot_workers: BTreeMap::new(),
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
                    slot_workers: BTreeMap::new(),
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
    let (mut task, mut pipelines, mut signatures) = fixture();
    task.limits.max_attempts = 10;
    task.limits.verification.attempts = 4;
    task.limits.verification.wall_ms = 4000;
    signatures
        .get_mut("worker/builtin/document-author")
        .unwrap()
        .outcome_port = Some("output".into());
    let pipeline = pipelines.get_mut("builtin/document").unwrap();
    pipeline.max_attempts = 10;
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
            slot_workers: BTreeMap::new(),
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
        let report = Scheduler::new(&planned, 4).run(&dispatch);
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
                    slot_workers: BTreeMap::new(),
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

#[test]
fn composite_public_results_must_retain_the_exact_verifier_receipt() {
    let (task, mut pipelines, mut signatures) = fixture();
    let pipeline = pipelines.get_mut("builtin/document").unwrap();
    let mut input = pipeline.contract.outputs["document"].clone();
    input.covers.clear();
    let mut output = input.clone();
    output.affinity = PortAffinityV1::SameAs {
        input: "document".into(),
    };
    let mut signature = signatures["worker/builtin/document-author"].clone();
    signature.contract = PipelineContractV1 {
        inputs: BTreeMap::from([("document".into(), input)]),
        outputs: BTreeMap::from([("output".into(), output)]),
    };
    signature.evidence.clear();
    signature.attempt = None;
    signature
        .retains
        .insert("output".into(), BTreeSet::from(["document".into()]));
    signatures.insert("operator/attest-fixes".into(), signature);
    pipeline.nodes.push(TaskNodeV1 {
        id: "wrap".into(),
        operator: TaskOperatorV1::AttestFixes {},
        inputs: BTreeMap::from([(
            "document".into(),
            ValueRefV1::Node {
                node: "write".into(),
                port: "output".into(),
            },
        )]),
        when: None,
    });
    pipeline.outputs.insert(
        "document".into(),
        ValueRefV1::Node {
            node: "wrap".into(),
            port: "output".into(),
        },
    );
    let compile = |signatures: &BTreeMap<String, OperatorSignature>| {
        compile_task(
            &task,
            "builtin/document",
            &CompileContext {
                slot_workers: BTreeMap::new(),
                pipelines: &pipelines,
                signatures,
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                max_nodes: 64,
                max_depth: 4,
            },
        )
    };
    let graph = compile(&signatures).unwrap();
    assert_eq!(graph.outputs["document"].node, "root.nodes.wrap");
    assert_eq!(graph.coverage["checked"].node, "root.nodes.write");
    signatures
        .get_mut("operator/attest-fixes")
        .unwrap()
        .retains
        .clear();
    assert!(
        compile(&signatures)
            .unwrap_err()
            .contains("does not retain")
    );
}

#[test]
fn evidence_from_an_earlier_output_cannot_validate_a_later_final_output() {
    let (mut task, mut pipelines, signatures) = fixture();
    task.limits.verification.attempts = 2;
    task.limits.verification.wall_ms = 2000;
    let pipeline = pipelines.get_mut("builtin/document").unwrap();
    let mut revised = pipeline.nodes[0].clone();
    revised.id = "revised".into();
    pipeline.nodes.push(revised);
    let evidence = pipeline.contract.outputs["document"].clone();
    pipeline
        .contract
        .outputs
        .get_mut("document")
        .unwrap()
        .covers
        .clear();
    pipeline
        .contract
        .outputs
        .insert("evidence".into(), evidence);
    pipeline.outputs.insert(
        "document".into(),
        ValueRefV1::Node {
            node: "revised".into(),
            port: "output".into(),
        },
    );
    pipeline.outputs.insert(
        "evidence".into(),
        ValueRefV1::Node {
            node: "write".into(),
            port: "output".into(),
        },
    );
    let result = compile_task(
        &task,
        "builtin/document",
        &CompileContext {
            slot_workers: BTreeMap::new(),
            pipelines: &pipelines,
            signatures: &signatures,
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
            max_nodes: 64,
            max_depth: 4,
        },
    );
    assert!(
        result
            .unwrap_err()
            .contains("does not judge the required final output")
    );
}

#[test]
fn planning_compilation_has_a_proposal_contract_without_business_coverage_or_verifier_credit() {
    use review_core::task::planning::PIPELINE_PROPOSAL_V1;
    let (task, mut pipelines, mut signatures) = fixture();
    let mut definition = pipelines.remove("builtin/document").unwrap();
    definition.name = "af-internal/planning".into();
    definition.coverage.clear();
    let mut port = definition.contract.outputs.remove("document").unwrap();
    port.artifact_type = PIPELINE_PROPOSAL_V1.into();
    port.covers.clear();
    definition
        .contract
        .outputs
        .insert("proposal".into(), port.clone());
    let output = definition.outputs.remove("document").unwrap();
    definition.outputs.insert("proposal".into(), output);
    for slot in definition.slots.values_mut() {
        slot.role = "plan".into();
        slot.output_type = PIPELINE_PROPOSAL_V1.into();
    }
    let signature = signatures
        .get_mut("worker/builtin/document-author")
        .unwrap();
    signature.contract.outputs.insert("output".into(), port);
    signature.evidence.clear();
    signature.roles = BTreeSet::from(["plan".into()]);
    signature.worker_output_type = Some(PIPELINE_PROPOSAL_V1.into());
    pipelines.insert(definition.name.clone(), definition);
    let context = CompileContext {
        pipelines: &pipelines,
        signatures: &signatures,
        slot_workers: BTreeMap::new(),
        acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
        max_nodes: 64,
        max_depth: 4,
    };
    let (graph, resources) =
        review_graph::task::compile_task_preparation(&task, "af-internal/planning", &context)
            .unwrap();
    assert!(resources.is_empty());
    assert!(graph.coverage.is_empty());
    assert_eq!(graph.inputs, task.inputs);
    assert_eq!(graph.outputs.len(), 1);
    assert!(graph.outputs.contains_key("proposal"));
    assert!(
        graph
            .allowances
            .values()
            .all(|a| a.verification_attempts == 0)
    );
    assert!(
        compile_task(&task, "af-internal/planning", &context).is_err(),
        "Proposal pretended to satisfy business acceptance"
    );
    let mut unavailable = signatures.clone();
    unavailable
        .get_mut("worker/builtin/document-author")
        .unwrap()
        .effects
        .insert("undeclared-effect".into());
    let context = CompileContext {
        signatures: &unavailable,
        ..context
    };
    assert!(
        review_graph::task::compile_task_preparation(&task, "af-internal/planning", &context)
            .is_err(),
        "Preparation widened effect authority"
    );
}

#[test]
fn captured_integration_allowance_is_dormant_and_cannot_collide_or_add_paid_work() {
    use review_graph::task::CompiledReviewIntegrationV1;
    let (task, pipelines, signatures) = fixture();
    let context = CompileContext {
        slot_workers: BTreeMap::new(),
        acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
        pipelines: &pipelines,
        signatures: &signatures,
        max_nodes: 64,
        max_depth: 4,
    };
    let ordinary = compile_task(&task, "builtin/document", &context).unwrap();
    let old_bytes = serde_json::to_value(&ordinary).unwrap();
    assert!(old_bytes.get("review_integration").is_none());
    let mut graph = ordinary.clone();
    graph.review_integration = Some(CompiledReviewIntegrationV1 {
        node: "root.integration_checks".into(),
        sequence_policy_id: format!("sha256:{}", "a".repeat(64)),
        allowance: review_attempt::task_budget::NodeAllowance {
            tokens_per_attempt: 0,
            wall_ms_per_attempt: 1000,
            max_attempts: 1,
            verification_attempts: 0,
        },
    });
    let allowances = graph.execution_allowances().unwrap();
    assert_eq!(allowances.len(), ordinary.allowances.len() + 1);
    assert_eq!(graph.nodes, ordinary.nodes);
    assert_eq!(graph.order, ordinary.order);
    assert_eq!(graph.outputs, ordinary.outputs);
    assert_eq!(graph.coverage, ordinary.coverage);
    let mut budget = graph.budget(task.limits.clone()).unwrap();
    let reservation = budget.prepare("root.integration_checks", 1).unwrap();
    assert_eq!(reservation.tokens, 0);
    for mutation in 0..4 {
        let mut bad = graph.clone();
        let phase = bad.review_integration.as_mut().unwrap();
        match mutation {
            0 => phase.node = "root.nodes.write".into(),
            1 => phase.allowance.tokens_per_attempt = 1,
            2 => phase.allowance.max_attempts = 2,
            3 => phase.allowance.verification_attempts = 1,
            _ => unreachable!(),
        }
        assert!(bad.execution_allowances().is_err());
    }
    let mut null = old_bytes;
    null["review_integration"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<review_graph::task::CompiledTask>(null).is_err());
}
