use super::*;
use review_pipeline::task::legacy_review::plan::REVIEW_TASK_POLICY_V4;

#[test]
fn owned_scatter_children_are_captured_in_the_plan_and_reopen_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let definition = include_str!("../../../../review-config/tests/fixtures/dynamic-v5.toml")
        .replace(
            "[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n",
            "",
        );
    let round = capture::open_round_with_pipeline(&cas, &mut store, &definition);
    let engine = cas.put(b"owned Review compiler test engine").unwrap();
    let mut settings = settings();
    settings.executions = BTreeMap::from([
        ("scatter".into(), WorkerExecutionV1::Command {}),
        ("closeout".into(), WorkerExecutionV1::Command {}),
    ]);
    let compiler = LegacyReviewPlanCompiler::capture(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        engine.clone(),
        settings,
    )
    .unwrap();
    assert_eq!(
        cas.get_artifact(compiler.policy_id())
            .unwrap()
            .artifact_type,
        REVIEW_TASK_POLICY_V4
    );
    let task = compiler
        .prepare_revision(&cas, "owned-review", capture::limits())
        .unwrap();
    let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, captured) = compiler.compile(&cas, &revision).unwrap();
    assert_plan_schemas(&cas, &compiler, &plan);
    let graph = &captured.compilation.graph;
    let owner = &captured.compilation.nodes["scatter"].task_node;
    assert_eq!(graph.owned_children.len(), 1);
    assert!(!graph.allowances.contains_key(owner));
    let template = &graph.owned_children[owner];
    assert_eq!(template.max_children, 2);
    assert_eq!(template.allowance.tokens_per_attempt, 19);
    assert_eq!(
        template.contract.outputs["o0"].artifact_type,
        review_core::contract::REVIEWER_RESULT_V2
    );
    for pointer in [
        "/owned_children/OWNER/max_children",
        "/owned_children/OWNER/allowance/max_attempts",
    ] {
        let envelope = cas.get_artifact(&plan.compiled_graph_id).unwrap();
        let mut payload = envelope.payload.clone();
        *payload
            .pointer_mut(&pointer.replace("OWNER", owner))
            .unwrap() = serde_json::json!(99);
        let (id, _) = cas
            .put_artifact(
                envelope.artifact_type,
                envelope.producer,
                envelope.input_artifacts,
                envelope.subject_snapshot_id,
                payload,
            )
            .unwrap();
        let mut forged = plan.clone();
        forged.compiled_graph_id = id;
        assert!(compiler.validate_plan(&cas, &task, &forged).is_err());
    }
    let bytes = serde_json::to_vec(&plan).unwrap();
    let reopened = LegacyReviewPlanCompiler::reopen(
        &cas,
        CapturedLegacyReviewRound::load_recorded(&cas, &store, "review", &round).unwrap(),
        &engine,
        compiler.policy_id(),
    )
    .unwrap();
    reopened.validate_plan(&cas, &task, &plan).unwrap();
    assert_eq!(
        serde_json::to_vec(&reopened.compile(&cas, &revision).unwrap().0).unwrap(),
        bytes
    );
    let graph_path = path(&cas_root, &plan.compiled_graph_id);
    std::fs::remove_file(&graph_path).unwrap();
    assert!(reopened.validate_plan(&cas, &task, &plan).is_err());
    assert!(
        !graph_path.exists(),
        "read-only admission recreated a missing compiled graph"
    );
    assert!(store.task_attempt_wall("review").unwrap().is_empty());
}
