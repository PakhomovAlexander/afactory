use super::*;
use review_core::task::provider::{
    TASK_PROVIDER_PROBE_POLICY_V1, TaskProviderProbePolicyV1, TaskProviderProbeProtocolV1,
};
use review_graph::task::CompiledOperator;
use review_pipeline::task::legacy_review::plan::{
    REVIEW_TASK_POLICY_V1, REVIEW_TASK_POLICY_V2, ReviewPlanSettingsV2,
    ReviewProviderProbeSettingsV1,
};

fn definition() -> String {
    PIPELINE
        .replace("version = 2", "version = 4\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"")
        .replace("runner = { program = \"/bin/true\" }", r#"package="fixture"
gated_by="gate"
execution={credential_mode="brokered",operations=[{name="inference",destination="fixture.test",method="respond",max_request_bytes=4096,max_response_bytes=4096,max_calls=1,max_usage=10}]}"#)
        + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/true\"\n"
}

fn selected() -> ReviewPlanSettingsV2 {
    let mut review = settings();
    review.resources.uncapped_attempt_tokens = 2048;
    review.provider_admission.tokens = 7;
    review.executions.insert(
        "reviewer".into(),
        WorkerExecutionV1::Model {
            provider: "claude-personal".into(),
            provider_kind: "claude".into(),
            principal_id: "fixture-account".into(),
            model: "claude-fixture".into(),
            effort: "high".into(),
        },
    );
    ReviewPlanSettingsV2 {
        review,
        provider_probes: BTreeMap::from([(
            "reviewer".into(),
            ReviewProviderProbeSettingsV1 {
                probe_protocol: TaskProviderProbeProtocolV1::OkV1,
                operations: vec![review_core::BrokerOperationPolicyV1 {
                    name: "capability".into(),
                    destination: "probe.test".into(),
                    method: "respond".into(),
                    max_request_bytes: 256,
                    max_response_bytes: 128,
                    max_calls: 1,
                    max_usage: 7,
                }],
            },
        )]),
    }
}

#[test]
fn brokered_provider_compiler_captures_independent_policy_and_reopens_read_only() {
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round = capture::open_round_with_package(&cas, &mut store, &definition());
    let engine = cas.put(b"probe compiler fixture engine").unwrap();
    let compiler = LegacyReviewPlanCompiler::capture_v2(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        engine.clone(),
        selected(),
    )
    .unwrap();
    assert_eq!(
        cas.get_artifact(compiler.policy_id())
            .unwrap()
            .artifact_type,
        REVIEW_TASK_POLICY_V2
    );
    let mut limits = capture::limits();
    limits.tokens = 100_000;
    let task = compiler
        .prepare_revision(&cas, "probe-plan", limits)
        .unwrap();
    let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, compilation) = compiler.compile(&cas, &revision).unwrap();
    assert_plan_schemas(&cas, &compiler, &plan);
    let graph = &compilation.compilation.graph;
    let admissions: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|(name, node)| match &node.operator {
            CompiledOperator::ProviderAdmissionBrokered {
                bindings,
                probe_policy_id,
            } => Some((name, bindings, probe_policy_id)),
            _ => None,
        })
        .collect();
    assert_eq!(admissions.len(), 1);
    let (node, bindings, probe_id) = admissions[0];
    assert_eq!(bindings, &plan.bindings.keys().cloned().collect());
    let allowance = &graph.allowances[node];
    assert_eq!(
        (
            allowance.tokens_per_attempt,
            allowance.wall_ms_per_attempt,
            allowance.max_attempts
        ),
        (7, 1000, 1)
    );
    let probe = cas.get_artifact(probe_id).unwrap();
    assert_eq!(probe.artifact_type, TASK_PROVIDER_PROBE_POLICY_V1);
    assert_eq!(probe.input_artifacts, [compiler.policy_id()]);
    let policy: TaskProviderProbePolicyV1 = serde_json::from_value(probe.payload.clone()).unwrap();
    policy.validate().unwrap();
    assert_eq!(
        policy.execution,
        plan.bindings.values().next().unwrap().execution
    );
    assert_eq!(
        policy.operations,
        selected().provider_probes["reviewer"].operations
    );
    assert_eq!(policy.operations[0].name, "capability");
    assert_eq!(
        plan.dependencies["af/provider-probe-0"].artifact_id,
        *probe_id
    );
    assert!(
        cas.get_artifact(&plan.compiled_graph_id)
            .unwrap()
            .input_artifacts
            .contains(probe_id)
    );
    let original_bytes = serde_json::to_vec(&plan).unwrap();
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
        original_bytes
    );
    let probe_path = path(&cas_root, probe_id);
    let bytes = std::fs::read(&probe_path).unwrap();
    std::fs::remove_file(&probe_path).unwrap();
    assert!(reopened.validate_plan(&cas, &task, &plan).is_err());
    assert!(
        !probe_path.exists(),
        "validation recreated missing probe authority"
    );
    std::fs::write(probe_path, bytes).unwrap();
    let mut changed = plan.clone();
    changed.dependencies.remove("af/provider-probe-0");
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    let mut forged = probe.clone();
    forged.payload["operations"][0]["max_usage"] = serde_json::json!(8);
    let (_, forged) = cas
        .put_artifact(
            forged.artifact_type,
            forged.producer,
            forged.input_artifacts,
            forged.subject_snapshot_id,
            forged.payload,
        )
        .unwrap();
    let mut changed = plan.clone();
    let dependency = changed.dependencies.get_mut("af/provider-probe-0").unwrap();
    dependency.artifact_id = forged.artifact_id;
    dependency.content_digest = forged.content_id;
    assert!(reopened.validate_plan(&cas, &task, &changed).is_err());
    reopened.validate_plan(&cas, &task, &plan).unwrap();
    assert!(store.attempt_wall("review").unwrap().is_empty());

    // Existing authority is never silently upgraded to V2 or given probe operations.
    let old = LegacyReviewPlanCompiler::capture(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        engine,
        selected().review,
    )
    .unwrap();
    assert_eq!(
        cas.get_artifact(old.policy_id()).unwrap().artifact_type,
        REVIEW_TASK_POLICY_V1
    );
    assert_ne!(old.policy_id(), compiler.policy_id());
    let old_task = old
        .prepare_revision(&cas, "old-probe-plan", task.limits.clone())
        .unwrap();
    let old_revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &old_task);
    let (old_plan, old_compiled) = old.compile(&cas, &old_revision).unwrap();
    assert_eq!(old_plan.dependencies.len(), 2);
    assert!(
        old_compiled
            .compilation
            .graph
            .nodes
            .values()
            .any(|node| matches!(node.operator, CompiledOperator::ProviderAdmission { .. }))
    );
    assert!(
        !old_compiled
            .compilation
            .graph
            .nodes
            .values()
            .any(|node| matches!(
                node.operator,
                CompiledOperator::ProviderAdmissionBrokered { .. }
            ))
    );
    old.validate_plan(&cas, &old_task, &old_plan).unwrap();
}

#[test]
fn brokered_provider_settings_refuse_unknown_native_and_over_budget_probes() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round = capture::open_round_with_package(&cas, &mut store, &definition());
    let engine = cas.put(b"probe compiler fixture engine").unwrap();
    for mutate in [
        |s: &mut ReviewPlanSettingsV2| {
            s.provider_probes.get_mut("reviewer").unwrap().operations[0].max_usage = 8
        },
        |s: &mut ReviewPlanSettingsV2| {
            let probe = s.provider_probes.remove("reviewer").unwrap();
            s.provider_probes.insert("unknown".into(), probe);
        },
        |s: &mut ReviewPlanSettingsV2| {
            s.review
                .executions
                .insert("reviewer".into(), WorkerExecutionV1::Command {});
        },
        |s: &mut ReviewPlanSettingsV2| {
            s.provider_probes
                .get_mut("reviewer")
                .unwrap()
                .operations
                .clear()
        },
    ] {
        let mut settings = selected();
        mutate(&mut settings);
        assert!(
            LegacyReviewPlanCompiler::capture_v2(
                &cas,
                CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
                engine.clone(),
                settings
            )
            .is_err()
        );
    }
    let definition = definition().replace("credential_mode=\"brokered\",operations=[{name=\"inference\",destination=\"fixture.test\",method=\"respond\",max_request_bytes=4096,max_response_bytes=4096,max_calls=1,max_usage=10}]", "credential_mode=\"trusted_unsafe\"");
    let other = tempfile::tempdir().unwrap();
    let cas = Cas::open(other.path().join("cas")).unwrap();
    let mut store = EventStore::open(other.path().join("events.sqlite")).unwrap();
    let round = capture::open_round_with_package(&cas, &mut store, &definition);
    assert!(
        LegacyReviewPlanCompiler::capture_v2(
            &cas,
            CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
            cas.put(b"engine").unwrap(),
            selected()
        )
        .is_err()
    );
}
