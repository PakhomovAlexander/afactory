use super::*;
use crate::capture::captured_fixture;
use review_store::NewEvent;
#[path = "../../support/captured_review_continuation.rs"]
mod numeric_fixture;
use numeric_fixture::{admit_heavy_definition, observe_charge};

#[test]
fn heavy_round_conclusion_is_durable_without_finishing_and_successor_retains_task_authority() {
    for declared_demands in [true, false] {
        numeric_fixture::run_numeric_rounds(declared_demands, true);
    }
}

#[test]
fn input_epoch_handoff_retains_failed_attempts_and_the_same_round_scope() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let path = temp.path().join("events.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let script = r#"input=$(cat); case "$input" in *'"epoch":1'*) exit 1;; *'"epoch":2'*) printf '%s' '{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[]}' ;; *) exit 33;; esac"#;
    let definition = PIPELINE.replace(
        "runner = { program = \"/bin/true\" }",
        &format!(
            "runner = {{program=\"/bin/sh\",args=[{{value=\"-c\"}},{{value={}}}]}}",
            serde_json::to_string(script).unwrap(),
        ),
    );
    let (compiler, lease) = admit_heavy_definition(&cas, &mut store, &definition);
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
    assert!(!runtime.execute().unwrap().complete());
    let old_state = runtime.projection().unwrap();
    let old_execution = old_state.execution.as_ref().unwrap();
    let attempts = old_execution.attempt_accounting();
    assert_eq!(attempts.len(), 2);
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.started && attempt.result.is_some())
    );
    observe_charge(
        &cas,
        &mut shared.lock().unwrap(),
        &lease,
        &attempts[0].attempt_id,
        3,
    );
    let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
    assert!(matches!(
        conclusion.verdict,
        review_pipeline::RunVerdict::Incomplete { .. }
    ));
    assert!(!conclusion.can_continue);
    assert!(
        conclusion.resources_failed,
        "the failed node consumed its captured retry allowance"
    );
    assert!(
        !runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .budget
            .breached(),
        "the existing Task still has capacity for an explicitly superseded epoch"
    );
    let old_round = shared
        .lock()
        .unwrap()
        .latest_round_started("review")
        .unwrap()
        .unwrap();
    let mut replacement: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(old_round.payload.clone()).unwrap();
    replacement.epoch += 1;
    let superseded = review_core::RoundInputSupersededPayloadV1 {
        round: replacement.round,
        old_epoch: replacement.epoch - 1,
        new_epoch: replacement.epoch,
        campaign_manifest_id: replacement.campaign_manifest_id.clone(),
        old_subject_id: replacement.subject_id.clone(),
        replacement_subject_id: replacement.subject_id.clone(),
    };
    let events = shared
        .lock()
        .unwrap()
        .append_batch(
            "review",
            &cas,
            &[
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(superseded).unwrap(),
                )
                .caused_by(old_round.event_id.clone())
                .referencing(old_round.artifact_refs.clone()),
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(replacement).unwrap(),
                )
                .caused_by(old_round.event_id.clone())
                .referencing(old_round.artifact_refs),
            ],
        )
        .unwrap();
    let successor = LegacyReviewPlanCompiler::reopen(
        &cas,
        CapturedLegacyReviewRound::load(
            &cas,
            &shared.lock().unwrap(),
            "review",
            &events[1].event_id,
        )
        .unwrap(),
        &old_state.revision.provenance.adapter_id,
        compiler.policy_id(),
    )
    .unwrap();
    let next = successor
        .prepare_continuation_revision(&cas, &old_state.revision_id, &old_state.revision)
        .unwrap();
    let next_id = plan::artifact(&cas, review_core::task::TASK_REVISION_V1, &next);
    let (next_plan, compiled) = successor.compile(&cas, &next_id).unwrap();
    assert_eq!(
        compiled.compilation.graph.token_scopes,
        old_execution.graph.token_scopes
    );
    let next_plan_id = plan::artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &next_plan);
    let handoff = review_core::task::review_handoff::TaskReviewHandoffV1 {
        task_id: lease.task_id().into(),
        predecessor_revision_id: old_state.revision_id.clone(),
        predecessor_plan_id: old_state.plan_id.clone().unwrap(),
        successor_revision_id: next_id,
        successor_plan_id: next_plan_id,
        predecessor_round_id: old_state.revision.inputs["round"].artifact_ids[0].clone(),
        successor_round_id: next.inputs["round"].artifact_ids[0].clone(),
        evidence: review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::SupersededInput {
            superseded_event_id: events[0].event_id.clone(),
        },
    };
    let id = review_store::store::task::review_handoff::capture_task_review_handoff(&cas, &handoff)
        .unwrap();
    let next_authority =
        CapturedTaskAuthority::for_legacy_review(&successor, &host, &NoTaskDeveloper);
    shared
        .lock()
        .unwrap()
        .continue_task_review(&cas, &lease, &id, &next_authority)
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
    let next_runtime = TaskRuntime::with_store(
        shared.clone(),
        &cas,
        lease.clone(),
        &next_authority,
        &next_host,
    )
    .unwrap();
    let report = next_runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    observe_charge(
        &cas,
        &mut shared.lock().unwrap(),
        &lease,
        &attempts[0].attempt_id,
        5,
    );
    let after = next_host.publish_recorded_round_conclusion(&cas).unwrap();
    assert!(after.can_continue);
    let state = next_runtime.projection().unwrap();
    let execution = state.execution.as_ref().unwrap();
    assert_eq!(execution.budget.committed_tokens(), 5);
    assert_eq!(
        execution.budget.scope_committed_tokens("review.round1"),
        Some(5)
    );
    assert_eq!(
        execution.budget.scope_committed_tokens("review.round2"),
        None
    );
    assert_eq!(execution.budget.begun_attempts(), 3);
    for original in attempts {
        let actual = execution
            .attempt_accounting()
            .into_iter()
            .find(|attempt| attempt.attempt_id == original.attempt_id)
            .unwrap();
        assert_eq!(actual.plan_id, original.plan_id);
        assert_eq!(actual.result, original.result);
    }
    let reopened = EventStore::open_read_only(path).unwrap();
    let replayed = reopened
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert_eq!(replayed.review_handoffs, [(id, handoff)]);
    assert_eq!(replayed.execution.unwrap().budget.committed_tokens(), 5);
    let first_report = reopened
        .replay("review")
        .unwrap()
        .into_iter()
        .find(|event| event.event_id == conclusion.canonical_report_event_id)
        .unwrap();
    assert_eq!(first_report.payload["spent_tokens"], "3");
}
