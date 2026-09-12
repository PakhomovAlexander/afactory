use super::*;
use review_pipeline::task::legacy_review::plan::{REVIEW_TASK_POLICY_V3, ReviewPlanSettingsV2};

#[test]
fn owned_scatter_is_an_explicit_capture_generation_and_reopens_exactly() {
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
    let mut historical_graph = None;
    for generation in 1..=3 {
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let v2 = ReviewPlanSettingsV2 {
            review: settings.clone(),
            provider_probes: BTreeMap::new(),
        };
        let compiler = match generation {
            1 => {
                LegacyReviewPlanCompiler::capture(&cas, captured, engine.clone(), settings.clone())
            }
            2 => LegacyReviewPlanCompiler::capture_v2(&cas, captured, engine.clone(), v2),
            _ => LegacyReviewPlanCompiler::capture_v3(&cas, captured, engine.clone(), v2),
        }
        .unwrap();
        let task = compiler
            .prepare_revision(
                &cas,
                &format!("owned-generation-{generation}"),
                capture::limits(),
            )
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let (plan, captured) = compiler.compile(&cas, &revision).unwrap();
        assert_plan_schemas(&cas, &compiler, &plan);
        let graph = &captured.compilation.graph;
        let owner = &captured.compilation.nodes["scatter"].task_node;
        if generation < 3 {
            assert!(graph.owned_children.is_empty());
            assert!(graph.allowances.contains_key(owner));
            assert!(
                cas.get_artifact(&plan.compiled_graph_id)
                    .unwrap()
                    .payload
                    .get("owned_children")
                    .is_none()
            );
            if let Some(previous) = &historical_graph {
                assert_eq!(graph, previous);
            } else {
                historical_graph = Some(graph.clone());
            }
        } else {
            assert_eq!(
                cas.get_artifact(compiler.policy_id())
                    .unwrap()
                    .artifact_type,
                REVIEW_TASK_POLICY_V3
            );
            assert_eq!(graph.owned_children.len(), 1);
            assert!(!graph.allowances.contains_key(owner));
            let template = &graph.owned_children[owner];
            assert_eq!(template.max_children, 2);
            assert_eq!(template.allowance.tokens_per_attempt, 19);
            assert_eq!(graph.nodes, historical_graph.as_ref().unwrap().nodes);
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
        let graph_bytes = std::fs::read(&graph_path).unwrap();
        std::fs::remove_file(&graph_path).unwrap();
        assert!(reopened.validate_plan(&cas, &task, &plan).is_err());
        assert!(
            !graph_path.exists(),
            "read-only admission recreated a missing compiled graph"
        );
        std::fs::write(graph_path, graph_bytes).unwrap();
    }
    assert!(store.attempt_wall("review").unwrap().is_empty());
}
