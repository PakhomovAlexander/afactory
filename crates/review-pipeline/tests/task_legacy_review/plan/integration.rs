use super::*;
use review_pipeline::task::legacy_review::plan::{REVIEW_TASK_POLICY_V4, ReviewPlanSettingsV2};

fn definition(order: &[&str]) -> String {
    include_str!("../../../../review-config/tests/fixtures/dynamic-v5.toml").replace(
        "[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n",
        "",
    ) + &format!(
        "\n[integration]\npost_apply_checks={}\n[[checks]]\nname=\"first\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit 0\"}}]\n[[checks]]\nname=\"second\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit 0\"}}]\n",
        serde_json::to_string(order).unwrap()
    )
}

fn settings_for(mode: &str) -> ReviewPlanSettingsV2 {
    let mut review = settings();
    review.mode = mode.into();
    review.executions = BTreeMap::from([
        ("scatter".into(), WorkerExecutionV1::Command {}),
        ("closeout".into(), WorkerExecutionV1::Command {}),
    ]);
    ReviewPlanSettingsV2 {
        review,
        provider_probes: BTreeMap::new(),
    }
}

#[test]
fn integration_is_captured_only_in_v4_with_its_original_dormant_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let cas_root = dir.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let round = capture::captured_fixture::open_round_authority_with_convergence(
        &cas,
        &mut store,
        &definition(&["first", "second"]),
        None,
        review_core::CampaignConvergenceV1 {
            clean_rounds: 2,
            max_rounds: 3,
            gate: "major".into(),
        },
    );
    let engine = cas.put(b"Integration compiler fixture").unwrap();
    for (generation, mode) in [(3, "heavy"), (4, "heavy")] {
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let compiler = if generation == 3 {
            LegacyReviewPlanCompiler::capture_v3(&cas, captured, engine.clone(), settings_for(mode))
        } else {
            LegacyReviewPlanCompiler::capture_v4(&cas, captured, engine.clone(), settings_for(mode))
        }
        .unwrap();
        let mut limits = capture::limits();
        limits.max_attempts = 6;
        let task = compiler
            .prepare_revision(&cas, &format!("integration-{generation}-{mode}"), limits)
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let (plan, captured) = compiler.compile(&cas, &revision).unwrap();
        let graph = &captured.compilation.graph;
        if generation == 4 && mode == "heavy" {
            assert_eq!(
                cas.get_artifact(compiler.policy_id())
                    .unwrap()
                    .artifact_type,
                REVIEW_TASK_POLICY_V4
            );
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
fn v4_refuses_ambiguous_check_order_without_changing_legacy_compilation() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let round = capture::captured_fixture::open_round_authority_with_convergence(
        &cas,
        &mut store,
        &definition(&["second", "first"]),
        None,
        review_core::CampaignConvergenceV1 {
            clean_rounds: 2,
            max_rounds: 3,
            gate: "major".into(),
        },
    );
    let engine = cas.put(b"Integration compiler fixture").unwrap();
    for generation in [3, 4] {
        let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
        let compiler = if generation == 3 {
            LegacyReviewPlanCompiler::capture_v3(
                &cas,
                captured,
                engine.clone(),
                settings_for("heavy"),
            )
        } else {
            LegacyReviewPlanCompiler::capture_v4(
                &cas,
                captured,
                engine.clone(),
                settings_for("heavy"),
            )
        }
        .unwrap();
        let mut limits = capture::limits();
        limits.max_attempts = 6;
        let task = compiler
            .prepare_revision(&cas, &format!("ordered-{generation}"), limits)
            .unwrap();
        let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
        let result = compiler.compile(&cas, &revision);
        if generation == 3 {
            assert!(result.is_ok());
        } else {
            assert!(result.err().unwrap().contains("declaration order"));
        }
    }
}

#[test]
fn v4_bounds_check_evidence_without_changing_legacy_compilation() {
    for count in [63, 64] {
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
            review_core::CampaignConvergenceV1 {
                clean_rounds: 2,
                max_rounds: 3,
                gate: "major".into(),
            },
        );
        let engine = cas
            .put(b"Integration compiler evidence-bound fixture")
            .unwrap();
        for generation in [3, 4] {
            let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap();
            let compiler = if generation == 3 {
                LegacyReviewPlanCompiler::capture_v3(
                    &cas,
                    captured,
                    engine.clone(),
                    settings_for("heavy"),
                )
            } else {
                LegacyReviewPlanCompiler::capture_v4(
                    &cas,
                    captured,
                    engine.clone(),
                    settings_for("heavy"),
                )
            }
            .unwrap();
            let mut limits = capture::limits();
            limits.max_attempts = 6;
            let task = compiler
                .prepare_revision(&cas, &format!("bounded-{count}-{generation}"), limits)
                .unwrap();
            let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
            let result = compiler.compile(&cas, &revision);
            if generation == 4 && count == 64 {
                let error = result.err().unwrap();
                assert!(error.contains("at most 63 post-apply checks"), "{error}");
                assert!(error.contains("64-artifact settlement bound"), "{error}");
            } else {
                let (plan, compiled) = result.unwrap();
                if generation == 4 {
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
        }
        assert!(store.attempt_wall("review").unwrap().is_empty());
    }
}
