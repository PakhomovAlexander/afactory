//! Actual captured Integration and successor Review, shared with fresh CLI inspection.
use super::captured_fixture;
use review_core::EventType;
use review_core::task::TaskRevisionV1;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper, TaskDomain};
use review_pipeline::task::legacy_review::plan::{
    LegacyReviewPlanCompiler, ReviewPlanSettings, ReviewPlanSettingsV2,
};
use review_pipeline::task::legacy_review::{CapturedLegacyReviewRound, host::LegacyReviewTaskHost};
use review_pipeline::task::{TaskOperatorHost, TaskRuntime, TaskWorkOutput};
use review_source_git::{Entry, EntryKind, Manifest, manifest_diff};
use review_store::{Cas, EventStore, NewEvent, SharedEventStore};
use std::collections::BTreeMap;
fn limits() -> review_core::task::TaskLimitsV1 {
    review_core::task::TaskLimitsV1 {
        tokens: 100,
        max_attempts: 8,
        deadline_unix_ms: 9999999999999,
        verification: review_core::task::VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        },
    }
}
fn settings() -> ReviewPlanSettings {
    ReviewPlanSettings {
        mode: "heavy".into(),
        resources: review_config::task::legacy_review::resources::ReviewResourcePolicy {
            uncapped_attempt_tokens: 1,
        },
        outputs: BTreeMap::from([(
            "findings".into(),
            review_graph::task::Address {
                node: "ledger".into(),
                port: "findings".into(),
            },
        )]),
        executions: BTreeMap::new(),
        provider_admission: review_graph::task::OperatorAttemptCost {
            tokens: 1,
            wall_ms: 1000,
        },
        allowed_effects: Default::default(),
    }
}
fn artifact(cas: &Cas, ty: &str, value: impl serde::Serialize) -> String {
    cas.put_artifact(
        ty,
        review_core::Producer::KernelOperation {
            run_id: "integration-test".into(),
            node_id: None,
            operation_id: "capture".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}
struct PlanOnly;
impl TaskOperatorHost for PlanOnly {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        panic!("admission must not render context")
    }
    fn execute(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: Option<&review_store::store::task::execution::PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("admission must not execute")
    }
}
impl TaskDomain for PlanOnly {
    fn validate_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Err("not executing".into())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &review_core::task::plan::ExecutionPlanV1,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        Err("not executing".into())
    }
    fn validate_result(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &review_core::task::TaskResultV1,
    ) -> Result<(), String> {
        Err("not accepting".into())
    }
}
pub(super) fn definition(
    cas: &Cas,
    proposal: bool,
    failed: bool,
    calls: &std::path::Path,
) -> String {
    let mut reply = serde_json::json!({"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[]});
    let command = if proposal {
        let manifest = |bytes: &[u8]| {
            Manifest::new(vec![Entry {
                path: "value.txt".into(),
                kind: EntryKind::File,
                content: cas.put(bytes).unwrap(),
                size: bytes.len() as u64,
            }])
            .unwrap()
        };
        let patch = String::from_utf8(
            manifest_diff(&manifest(b"before\n"), &manifest(b"after\n"), cas)
                .unwrap()
                .patch()
                .into(),
        )
        .unwrap();
        reply["verdict"] = serde_json::json!("request-changes");
        reply["findings"] = serde_json::json!([{"severity":"minor","file":"value.txt","line":1,"title":"Improve fixture value","body":"The value can be improved","fix":"Use after","confidence":1.0}]);
        reply["proposal"] = serde_json::json!({"patch":patch,"report_indexes":[0],"paths":["value.txt"],"description":"Improve fixture value","auto_apply_nominated":true});
        format!(
            "cat >/dev/null; printf 'after\\n' > value.txt; printf '%s' '{}'",
            reply.to_string().replace('\'', "'\\''")
        )
    } else {
        format!("cat >/dev/null; printf '%s' '{reply}'")
    };
    let first = format!(
        "if test \"$(cat value.txt)\" = after; then printf x >> '{}'; printf ordered > sequence-marker; fi",
        calls.display()
    );
    let second = format!(
        "if test \"$(cat value.txt)\" = after; then test \"$(cat sequence-marker)\" = ordered || exit 31; exit {}; fi",
        if failed { 7 } else { 0 }
    );
    include_str!("../../../review-config/tests/fixtures/dynamic-v5.toml")
        .replace("[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n", "")
        .replace("max_paths_per_slice = 1", "max_paths_per_slice = 99")
        .replacen("runner = { program = \"/bin/true\" }", &format!("runner={{program=\"/bin/sh\",args=[{{value=\"-c\"}},{{value={}}}]}}",serde_json::to_string(&command).unwrap()),1)
        .replacen("execution = { credential_mode = \"credential_free\" }", "execution={credential_mode=\"credential_free\",auto_apply=true}",1)
        .replace("runner = { program = \"/bin/true\" }", &format!("runner={{program=\"/bin/sh\",args=[{{value=\"-c\"}},{{value={}}}]}}",serde_json::to_string("cat >/dev/null; printf '%s' '{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"dispositions\":[]}'").unwrap()))
        + &format!("\n[integration]\npost_apply_checks=[\"first\",\"second\"]\n[[checks]]\nname=\"first\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value={}}}]\n[[checks]]\nname=\"second\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value={}}}]\n",serde_json::to_string(&first).unwrap(),serde_json::to_string(&second).unwrap())
}

pub(super) fn admit_integration(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    let round = captured_fixture::open_round_authority_with_source(
        cas,
        store,
        definition,
        None,
        review_core::CampaignConvergenceV1 {
            clean_rounds: 1,
            max_rounds: 3,
            gate: "major".into(),
        },
        BTreeMap::from([("value.txt".into(), b"before\n".to_vec())]),
    );
    let mut settings = settings();
    settings.mode = "heavy".into();
    settings.executions = BTreeMap::from([
        (
            "scatter".into(),
            review_core::task::plan::WorkerExecutionV1::Command {},
        ),
        (
            "closeout".into(),
            review_core::task::plan::WorkerExecutionV1::Command {},
        ),
    ]);
    let compiler = LegacyReviewPlanCompiler::capture_v4(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"Integration host fixture").unwrap(),
        ReviewPlanSettingsV2 {
            review: settings,
            provider_probes: BTreeMap::new(),
        },
    )
    .unwrap();
    let mut limits = limits();
    limits.max_attempts = 8;
    let task = compiler
        .prepare_revision(cas, "integration-review", limits)
        .unwrap();
    let revision = artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, _) = compiler.compile(cas, &revision).unwrap();
    let plan_id = artifact(cas, review_core::task::EXECUTION_PLAN_V1, &plan);
    let authority =
        CapturedTaskAuthority::for_legacy_review(&compiler, &PlanOnly, &NoTaskDeveloper);
    let lease = store.open_task(cas, &revision, "developer", 60000).unwrap();
    store
        .propose_task_plan(cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, &authority).unwrap();
    (compiler, lease)
}

/// One Task named integration-review, Campaign review, stored in cas/ and events.sqlite.
/// A real Proposal is checked, promoted, handed off and resolved by the next complete Review.
pub fn run_integration_handoff() -> tempfile::TempDir {
    use review_core::task::review_handoff::*;
    use review_store::store::task::review_handoff::capture_task_review_handoff;
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let definition = definition(&cas, true, false, &dir.path().join("calls"));
    let mut definition: toml::Value = toml::from_str(&definition).unwrap();
    for node in definition["nodes"].as_array_mut().unwrap() {
        if !matches!(node["id"].as_str(), Some("scatter" | "closeout")) {
            continue;
        }
        let command = node["runner"]["args"][1]["value"].as_str().unwrap();
        let reviewed = r#"input=$(cat); if test "$(cat value.txt)" = after; then finding=$(printf '%s' "$input" | sed -n 's/.*"finding_id":"\(sha256:[0-9a-f]*\)".*/\1/p'); test -n "$finding" || exit 47; printf '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[{"finding_id":"%s","position":"not_reproduced","reason":"The complete derived head contains after"}]}' "$finding"; else printf '%s' "$input" | "#;
        node["runner"]["args"][1]["value"] =
            toml::Value::String(format!("{reviewed}{command}; fi"));
    }
    let (compiler, lease) =
        admit_integration(&cas, &mut store, &toml::to_string(&definition).unwrap());
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
    assert_eq!(conclusion.verdict, review_pipeline::RunVerdict::Pass);
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    let phase_runtime = TaskRuntime::with_review_integration(
        shared.clone(),
        &cas,
        lease.clone(),
        &authority,
        &host,
        &phase,
    )
    .unwrap();
    let (report_id, report) = phase_runtime.execute_review_integration(&phase).unwrap();
    assert!(report.complete(), "{report:?}");
    let phase = host
        .finish_recorded_integration(&cas, &phase, &report_id)
        .unwrap();
    let committed_id = phase.integration_committed_event_id().unwrap().to_owned();
    assert!(
        host.assemble_recorded_result(&cas)
            .unwrap_err()
            .contains("full Review")
    );
    let before = runtime.projection().unwrap();
    let attempts = before.execution.as_ref().unwrap().attempt_accounting();
    let mut locked = shared.lock().unwrap();
    let events = locked.replay("review").unwrap();
    let commit: review_core::IntegrationCommittedPayloadV1 = serde_json::from_value(
        events
            .iter()
            .find(|e| e.event_id == committed_id)
            .unwrap()
            .payload
            .clone(),
    )
    .unwrap();
    let projection = review_store::LedgerProjection::from_events("review", &events, &cas).unwrap();
    let findings = projection.ledger().finding_views();
    assert_eq!(findings.len(), 1);
    assert_eq!(
        serde_json::to_value(findings[0].status).unwrap(),
        serde_json::json!("pending_verification")
    );
    let old = locked.latest_round_started("review").unwrap().unwrap();
    let mut next: review_core::RoundStartedPayloadV1 = serde_json::from_value(old.payload).unwrap();
    next.round += 1;
    next.epoch = 1;
    next.subject_id = commit.derived_subject_id.clone();
    next.prior_demand_set_id = commit.expected_demand_set_id.clone();
    let prior:Vec<_>=findings.iter().map(|f|serde_json::json!({"key":f.key,"severity":f.severity,"status":f.status,"file":f.file,"line":f.line,"title":f.title,"body":f.body,"source":f.source,"last_seen_round":f.last_seen_round})).collect();
    next.prior_finding_set_id=cas.put_json(&serde_json::json!({"subject_id":next.subject_id,"round":next.round,"prior_findings":prior})).unwrap();
    let mut refs = old.artifact_refs;
    refs.extend([
        commit.derived_subject_id.clone(),
        commit.derived_snapshot_id.clone(),
        next.prior_finding_set_id.clone(),
        next.prior_demand_set_id.clone(),
    ]);
    refs.sort();
    refs.dedup();
    let opened = locked.campaign_opened("review").unwrap().unwrap();
    let round = locked
        .append(
            "review",
            &cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&next).unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(refs),
        )
        .unwrap();
    let projection = review_store::LedgerProjection::from_events(
        "review",
        &locked.replay("review").unwrap(),
        &cas,
    )
    .unwrap();
    let mut ingest = review_store::Ingest::from_projection(&mut locked, &cas, "review", projection)
        .unwrap()
        .under_round(&round.event_id);
    while ingest.ledger().round < next.round {
        ingest.advance().unwrap();
    }
    drop(ingest);
    let successor = LegacyReviewPlanCompiler::reopen(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &locked, "review", &round.event_id).unwrap(),
        &before.revision.provenance.adapter_id,
        compiler.policy_id(),
    )
    .unwrap();
    drop(locked);
    let revision = successor
        .prepare_continuation_revision(&cas, &before.revision_id, &before.revision)
        .unwrap();
    let revision_id = artifact(&cas, review_core::task::TASK_REVISION_V1, &revision);
    let (plan, _) = successor.compile(&cas, &revision_id).unwrap();
    let plan_id = artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &plan);
    let handoff = TaskReviewHandoffV1 {
        task_id: lease.task_id().into(),
        predecessor_revision_id: before.revision_id.clone(),
        predecessor_plan_id: before.plan_id.clone().unwrap(),
        successor_revision_id: revision_id,
        successor_plan_id: plan_id,
        predecessor_round_id: before.revision.inputs["round"].artifact_ids[0].clone(),
        successor_round_id: revision.inputs["round"].artifact_ids[0].clone(),
        evidence: TaskReviewHandoffEvidenceV1::IntegratedRound {
            report_event_id: conclusion.canonical_report_event_id,
            phase_id: phase.phase_id().into(),
            integration_committed_event_id: committed_id,
        },
    };
    let handoff_id = capture_task_review_handoff(&cas, &handoff).unwrap();
    assert_eq!(
        cas.get_artifact(&handoff_id).unwrap().artifact_type,
        TASK_REVIEW_HANDOFF_V2
    );
    let next_authority =
        CapturedTaskAuthority::for_legacy_review(&successor, &host, &NoTaskDeveloper);
    shared
        .lock()
        .unwrap()
        .continue_task_review(&cas, &lease, &handoff_id, &next_authority)
        .unwrap();
    shared
        .lock()
        .unwrap()
        .admit_task_plan(&cas, &lease, &next_authority)
        .unwrap();
    let next_host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &successor,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let next_authority =
        CapturedTaskAuthority::for_legacy_review(&successor, &next_host, &NoTaskDeveloper);
    let runtime = TaskRuntime::with_store(
        shared.clone(),
        &cas,
        lease.clone(),
        &next_authority,
        &next_host,
    )
    .unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let conclusion = next_host.publish_recorded_round_conclusion(&cas).unwrap();
    assert_eq!(conclusion.verdict, review_pipeline::RunVerdict::Pass);
    let phase = next_host
        .select_recorded_integration(&cas)
        .unwrap()
        .unwrap();
    assert!(phase.finished() && !phase.requires_checks());
    let after = runtime.projection().unwrap();
    assert_eq!(after.revision.limits, before.revision.limits);
    let after_attempts = after.execution.as_ref().unwrap().attempt_accounting();
    assert_eq!(after_attempts.len(), attempts.len() + 3);
    assert!(attempts.iter().all(|prior| {
        after_attempts
            .iter()
            .any(|a| a.attempt_id == prior.attempt_id && a.charged_tokens == prior.charged_tokens)
    }));
    let projection = review_store::LedgerProjection::from_events(
        "review",
        &shared.lock().unwrap().replay("review").unwrap(),
        &cas,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(projection.ledger().finding_views()[0].status).unwrap(),
        serde_json::json!("pending_verification")
    );
    let finding = projection.ledger().finding_views()[0].key.clone();
    let results: std::collections::BTreeSet<_> = after
        .execution
        .as_ref()
        .unwrap()
        .outputs
        .values()
        .flat_map(|(_, output)| output.outputs.values())
        .filter(|port| port.artifact_type == review_core::contract::REVIEWER_RESULT_V2)
        .flat_map(|port| port.artifact_ids.iter().cloned())
        .collect();
    assert_eq!(results.len(), 2);
    for id in results {
        let result = cas.get_artifact(&id).unwrap();
        assert_eq!(
            result.subject_snapshot_id.as_deref(),
            Some(commit.derived_snapshot_id.as_str())
        );
        assert_eq!(
            result.payload["dispositions"],
            serde_json::json!([{"finding_id":finding,"position":"not_reproduced","reason":"The complete derived head contains after"}])
        );
    }
    let result = next_host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(
        result.acceptance,
        review_core::task::TaskAcceptanceV1::Satisfied
    );
    let result_id = artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&result_id).unwrap();
    assert_eq!(runtime.projection().unwrap().review_handoffs.len(), 1);
    dir
}
