use super::*;

fn compile_with_limit(
    task: &TaskRevisionV1,
    root: &str,
    pipelines: &BTreeMap<String, PipelineDefinitionV1>,
    signatures: &BTreeMap<String, OperatorSignature>,
    max_nodes: usize,
) -> Result<review_graph::task::CompiledTask, String> {
    compile_task(
        task,
        root,
        &CompileContext {
            slot_workers: BTreeMap::new(),
            pipelines,
            signatures,
            max_nodes,
            max_depth: 4,
            acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
        },
    )
}

#[test]
fn primitive_root_and_select_share_the_exact_physical_node_cap() {
    let (task, mut pipelines, signatures) = fixture();
    assert!(
        compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 1)
            .unwrap_err()
            .contains("node limit")
    );
    assert_eq!(
        compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 2)
            .unwrap()
            .nodes
            .len(),
        2
    );
    let definition = pipelines.get_mut("builtin/document").unwrap();
    let mut extra = definition.nodes[0].clone();
    extra.id = "extra".into();
    definition.nodes.push(extra);
    assert!(
        compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 2)
            .unwrap_err()
            .contains("node limit")
    );

    let (task, pipelines, signatures) = branching_fixture();
    let graph = compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 6).unwrap();
    assert_eq!(graph.nodes.len(), 6);
    assert!(matches!(
        graph.nodes["root.nodes.choose"].operator,
        review_graph::task::CompiledOperator::Select
    ));
    assert!(
        compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 5)
            .unwrap_err()
            .contains("node limit")
    );
}

#[test]
fn nested_expansion_counts_its_physical_children_in_the_same_cap() {
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
    parent.outputs.insert(
        "document".into(),
        ValueRefV1::Node {
            node: "write".into(),
            port: "document".into(),
        },
    );
    parent
        .coverage
        .insert("checked".into(), parent.outputs["document"].clone());
    pipelines.insert(parent.name.clone(), parent);
    let graph = compile_with_limit(&task, "team/document", &pipelines, &signatures, 2).unwrap();
    assert_eq!(graph.nodes.len(), 2);
    assert!(graph.nodes.contains_key("root.nodes.write.nodes.write"));
    assert!(
        compile_with_limit(&task, "team/document", &pipelines, &signatures, 1)
            .unwrap_err()
            .contains("node limit")
    );
}

#[test]
fn physical_64_nodes_still_refuse_an_installed_provider_node() {
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    let (mut task, mut pipelines, signatures) = fixture();
    task.limits.max_attempts = 63;
    task.limits.verification.attempts = 63;
    task.limits.verification.wall_ms = 63000;
    let definition = pipelines.get_mut("builtin/document").unwrap();
    definition.max_attempts = 63;
    for index in 1..63 {
        let mut node = definition.nodes[0].clone();
        node.id = format!("extra{index}");
        definition.nodes.push(node);
    }
    let mut graph =
        compile_with_limit(&task, "builtin/document", &pipelines, &signatures, 64).unwrap();
    assert_eq!(graph.nodes.len(), 64);
    let before = graph.clone();
    let digest = task.authority.policy_id.clone();
    let bindings = BTreeMap::from([(
        graph.slots.keys().next().unwrap().clone(),
        EffectiveWorkerBindingV1 {
            package_digest: digest.clone(),
            package_artifact_id: digest.clone(),
            invocation_policy_id: digest,
            execution: WorkerExecutionV1::Model {
                provider: "fixture".into(),
                provider_kind: "codex".into(),
                principal_id: "synthetic-principal".into(),
                model: "synthetic".into(),
                effort: "high".into(),
            },
        },
    )]);
    assert!(
        graph
            .require_provider_admission(
                &bindings,
                &review_graph::task::OperatorAttemptCost {
                    tokens: 1,
                    wall_ms: 1
                },
                &task.limits
            )
            .unwrap_err()
            .contains("installed graph bound")
    );
    assert_eq!(graph, before);
}
