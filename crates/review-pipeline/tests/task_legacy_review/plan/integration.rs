use super::*;
use review_pipeline::task::legacy_review::plan::REVIEW_TASK_POLICY_V4;

fn definition(order: &[&str]) -> String {
    include_str!("../../../../review-config/tests/fixtures/dynamic-v5.toml").replace(
        "[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n",
        "",
    ) + &format!(
        "\n[integration]\npost_apply_checks={}\n[[checks]]\nname=\"first\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit 0\"}}]\n[[checks]]\nname=\"second\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit 0\"}}]\n",
        serde_json::to_string(order).unwrap()
    )
}

fn settings_for(mode: &str) -> ReviewPlanSettings {
    let mut review = settings();
    review.mode = mode.into();
    review.executions = BTreeMap::from([
        ("scatter".into(), WorkerExecutionV1::Command {}),
        ("closeout".into(), WorkerExecutionV1::Command {}),
    ]);
    review
}

/// A light Campaign closes after its one Round; a heavy one may continue to a third.
fn convergence(mode: &str) -> review_core::CampaignConvergenceV1 {
    let (clean_rounds, max_rounds) = if mode == "heavy" { (2, 3) } else { (1, 1) };
    review_core::CampaignConvergenceV1 {
        clean_rounds,
        max_rounds,
        gate: "major".into(),
    }
}

#[test]
fn integration_is_captured_only_for_heavy_rounds_with_its_original_dormant_allowance() {
    for mode in ["light", "heavy"] {
        let dir = tempfile::tempdir().unwrap();
        let cas_root = dir.path().join("cas");
        let cas = Cas::open(&cas_root).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let round = capture::captured_fixture::open_round_authority_with_convergence(
            &cas,
            &mut store,
            &definition(&["first", "second"]),
            None,
            convergence(mode),
        );
        let engine = cas.put(b"Integration compiler fixture").unwrap();
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let compiler =
            LegacyReviewPlanCompiler::capture(&cas, captured, engine, settings_for(mode)).unwrap();
        let mut limits = capture::limits();
        limits.max_attempts = 6;
        let task = compiler
            .prepare_revision(&cas, &format!("integration-{mode}"), limits)
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let (plan, captured) = compiler.compile(&cas, &revision).unwrap();
        assert_cached_authority_is_fresh(&cas, &cas_root, &compiler, &task, &plan);
        let graph = &captured.compilation.graph;
        assert_eq!(
            cas.get_artifact(compiler.policy_id())
                .unwrap()
                .artifact_type,
            REVIEW_TASK_POLICY_V4
        );
        if mode == "heavy" {
            let dormant = graph.review_integration.as_ref().unwrap();
            assert!(!graph.nodes.contains_key(&dormant.node));
            assert!(!graph.order.contains(&dormant.node));
            assert!(!graph.allowances.contains_key(&dormant.node));
            assert_eq!(
                graph.execution_allowances().unwrap()[&dormant.node],
                dormant.allowance
            );
            assert_eq!(dormant.allowance.tokens_per_attempt, 0);
            assert_eq!(dormant.allowance.max_attempts, 1);
            assert_eq!(
                dormant.allowance.wall_ms_per_attempt,
                captured.loaded.check_timeout_seconds() * 2 * 1000
            );
            let sequence = cas.get_artifact(&dormant.sequence_policy_id).unwrap();
            assert_eq!(
                sequence.payload["ordered_check_names"],
                serde_json::json!(["first", "second"])
            );
            assert_eq!(
                sequence.payload["pipeline_policy_id"],
                sequence.payload["gate_execution_policy_id"]
            );
            assert!(
                cas.get_artifact(&plan.compiled_graph_id)
                    .unwrap()
                    .input_artifacts
                    .contains(&dormant.sequence_policy_id)
            );
            assert_eq!(
                plan.dependencies["af/review-integration-checks"].artifact_id,
                dormant.sequence_policy_id
            );
            let sequence_path = path(&cas_root, &dormant.sequence_policy_id);
            let bytes = std::fs::read(&sequence_path).unwrap();
            std::fs::remove_file(&sequence_path).unwrap();
            assert!(compiler.recompile(&cas, &task, &plan).is_err());
            assert!(
                !sequence_path.exists(),
                "recompilation must not recreate authority"
            );
            std::fs::write(sequence_path, bytes).unwrap();
        } else {
            // The captured pipeline still permits three Rounds, so Round 1 is not final:
            // only the mode gate keeps Integration out of this light Round.
            assert_eq!(captured.loaded.convergence().max_rounds, 3);
            assert!(graph.review_integration.is_none());
            assert!(
                !plan
                    .dependencies
                    .contains_key("af/review-integration-checks")
            );
            assert!(
                cas.get_artifact(&plan.compiled_graph_id)
                    .unwrap()
                    .payload
                    .get("review_integration")
                    .is_none()
            );
        }
        assert_eq!(
            compiler
                .recompile(&cas, &task, &plan)
                .unwrap()
                .compilation
                .graph,
            *graph
        );
        assert!(store.attempt_wall("review").unwrap().is_empty());
    }
}

#[test]
fn heavy_review_refuses_ambiguous_check_order_and_light_review_ignores_it() {
    for mode in ["light", "heavy"] {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let round = capture::captured_fixture::open_round_authority_with_convergence(
            &cas,
            &mut store,
            &definition(&["second", "first"]),
            None,
            convergence(mode),
        );
        let engine = cas.put(b"Integration compiler fixture").unwrap();
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let compiler =
            LegacyReviewPlanCompiler::capture(&cas, captured, engine, settings_for(mode)).unwrap();
        let mut limits = capture::limits();
        limits.max_attempts = 6;
        let task = compiler
            .prepare_revision(&cas, &format!("ordered-{mode}"), limits)
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let result = compiler.compile(&cas, &revision);
        if mode == "light" {
            assert!(
                result
                    .unwrap()
                    .1
                    .compilation
                    .graph
                    .review_integration
                    .is_none()
            );
        } else {
            assert!(result.err().unwrap().contains("declaration order"));
        }
    }
}

#[test]
fn heavy_review_bounds_check_evidence_and_light_review_ignores_it() {
    for (count, mode) in [(63, "light"), (63, "heavy"), (64, "light"), (64, "heavy")] {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let names: Vec<_> = (0..count).map(|index| format!("check{index}")).collect();
        let mut definition =
            include_str!("../../../../review-config/tests/fixtures/dynamic-v5.toml").replace(
                "[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n",
                "",
            );
        definition.push_str(&format!(
            "\n[integration]\npost_apply_checks={}\n",
            serde_json::to_string(&names).unwrap()
        ));
        for name in &names {
            definition.push_str(&format!(
                "[[checks]]\nname=\"{name}\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit 0\"}}]\n"
            ));
        }
        let round = capture::captured_fixture::open_round_authority_with_convergence(
            &cas,
            &mut store,
            &definition,
            None,
            convergence(mode),
        );
        let engine = cas
            .put(b"Integration compiler evidence-bound fixture")
            .unwrap();
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let compiler =
            LegacyReviewPlanCompiler::capture(&cas, captured, engine, settings_for(mode)).unwrap();
        let mut limits = capture::limits();
        limits.max_attempts = 6;
        let task = compiler
            .prepare_revision(&cas, &format!("bounded-{count}-{mode}"), limits)
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let result = compiler.compile(&cas, &revision);
        if mode == "heavy" && count == 64 {
            let error = result.err().unwrap();
            assert!(error.contains("at most 63 post-apply checks"), "{error}");
            assert!(error.contains("64-artifact settlement bound"), "{error}");
        } else {
            let (plan, compiled) = result.unwrap();
            if mode == "heavy" {
                let dormant = compiled
                    .compilation
                    .graph
                    .review_integration
                    .as_ref()
                    .unwrap();
                let sequence = cas.get_artifact(&dormant.sequence_policy_id).unwrap();
                assert_eq!(
                    sequence.payload["ordered_check_names"],
                    serde_json::json!(names)
                );
                assert_eq!(
                    compiler
                        .recompile(&cas, &task, &plan)
                        .unwrap()
                        .compilation
                        .graph,
                    compiled.compilation.graph
                );
            } else {
                assert!(compiled.compilation.graph.review_integration.is_none());
            }
        }
        assert!(store.attempt_wall("review").unwrap().is_empty());
    }
}
