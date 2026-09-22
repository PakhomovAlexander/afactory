use super::review_handoff::HandoffAuthority;
use super::*;
use crate::store::task::review_round_publication::{
    TaskReviewRoundPublication, TaskReviewRoundSuccessor,
};
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};

fn fixture() -> (Fixture, TaskLease) {
    fixture_with_tokens(None)
}
fn fixture_with_tokens(tokens: Option<u64>) -> (Fixture, TaskLease) {
    let (mut f, _) = review::round::round_fixture_with_source(true);
    if let Some(tokens) = tokens {
        f.revision.limits.tokens = tokens;
        f.revision.limits.verification.tokens = tokens;
        f.revision_id = f
            .cas
            .put_artifact(
                task::TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&f.revision).unwrap(),
            )
            .unwrap()
            .0;
        f.plan.task_revision_id = f.revision_id.clone();
        f.plan.limits = f.revision.limits.clone();
        f.plan_id = f
            .cas
            .put_artifact(
                task::EXECUTION_PLAN_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&f.plan).unwrap(),
            )
            .unwrap()
            .0;
    }
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    (f, lease)
}

fn replacement(f: &Fixture, permit: &TaskReviewRoundPublication) -> Vec<NewEvent> {
    let old = f
        .store
        .latest_round_started(permit.campaign_id())
        .unwrap()
        .unwrap();
    let mut started: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(old.payload).unwrap();
    started.epoch = permit.next_epoch();
    let manifest: review_core::CampaignManifestV1 =
        serde_json::from_value(f.cas.get_json(&started.campaign_manifest_id).unwrap()).unwrap();
    let source = f
        .cas
        .get_json(&permit.predecessor().head_snapshot_id)
        .unwrap();
    let source_manifest = source["artifact_manifest"].as_str().unwrap();
    let opened = f
        .store
        .replay(permit.campaign_id())
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::CampaignOpenedV1)
        .unwrap();
    let refs = vec![
        manifest.authority_snapshot_id.clone(),
        started.campaign_manifest_id.clone(),
        permit.predecessor().head_snapshot_id.clone(),
        started.subject_id.clone(),
        started.prior_finding_set_id.clone(),
        started.prior_demand_set_id.clone(),
    ];
    vec![
        NewEvent::new(EventType::SourceCapturedV1, source.clone())
            .caused_by(opened.event_id)
            .correlating(&permit.predecessor().head_snapshot_id)
            .referencing(vec![
                manifest.authority_snapshot_id,
                started.campaign_manifest_id.clone(),
                permit.predecessor().head_snapshot_id.clone(),
                source_manifest.to_string(),
            ]),
        NewEvent::new(
            EventType::RoundInputSupersededV1,
            serde_json::to_value(review_core::RoundInputSupersededPayloadV1 {
                round: started.round,
                old_epoch: permit.predecessor().epoch,
                new_epoch: started.epoch,
                campaign_manifest_id: started.campaign_manifest_id.clone(),
                old_subject_id: started.subject_id.clone(),
                replacement_subject_id: started.subject_id.clone(),
            })
            .unwrap(),
        )
        .caused_by(permit.predecessor_round_event_id())
        .referencing(refs.clone()),
        NewEvent::new(
            EventType::RoundStartedV1,
            serde_json::to_value(started).unwrap(),
        )
        .caused_by(permit.predecessor_round_event_id())
        .referencing(refs),
    ]
}

fn prepare_handoff(
    f: &Fixture,
    lease: &TaskLease,
    permit: &TaskReviewRoundPublication,
    events: &[NewEvent],
) -> String {
    let preview = f
        .store
        .preview_task_review_round(&f.cas, lease, permit, events, &f.authority)
        .unwrap();
    let put = |ty: &str, value: serde_json::Value| {
        f.cas
            .put_artifact(ty, producer(), vec![], None, value)
            .unwrap()
            .0
    };
    let mut round = permit.predecessor().clone();
    round.round_event_id = preview.round_event().event_id.clone();
    round.round = permit.next_round();
    round.epoch = permit.next_epoch();
    let id = f
        .cas
        .put_artifact(
            review_core::task::campaign_review::CAMPAIGN_REVIEW_ROUND_V1,
            producer(),
            round
                .artifact_refs()
                .into_iter()
                .map(str::to_string)
                .collect(),
            Some(round.head_snapshot_id.clone()),
            serde_json::to_value(round).unwrap(),
        )
        .unwrap()
        .0;
    let mut next = f.revision.clone();
    next.revision += 1;
    next.previous_revision_id = Some(f.revision_id.clone());
    next.inputs.get_mut("round").unwrap().artifact_ids = vec![id];
    let revision_id = put(task::TASK_REVISION_V1, serde_json::to_value(&next).unwrap());
    let mut graph: review_graph::task::CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs = next.inputs.clone();
    let mut plan = f.plan.clone();
    plan.task_revision_id = revision_id;
    plan.inputs = next.inputs;
    plan.compiled_graph_id = put("af/CompiledTask@1", serde_json::to_value(graph).unwrap());
    let plan_id = put(task::EXECUTION_PLAN_V1, serde_json::to_value(plan).unwrap());
    let handoff = preview.prepare_handoff(&f.cas, &plan_id).unwrap();
    crate::store::task::review_handoff::capture_task_review_handoff(&f.cas, &handoff).unwrap()
}
fn publish(
    f: &mut Fixture,
    lease: &TaskLease,
    permit: &TaskReviewRoundPublication,
    events: &[NewEvent],
    handoff: &str,
) -> Result<Vec<RunEvent>, StoreError> {
    let successor = HandoffAuthority(&f.authority);
    f.store.publish_task_review_round(
        &f.cas,
        lease,
        permit,
        events,
        &f.authority,
        &TaskReviewRoundSuccessor {
            handoff_id: handoff,
            authority: &successor,
        },
    )
}

#[test]
fn review_round_publication_refuses_pending_and_stale_task_prefix_then_reopens_exactly() {
    let (mut f, lease) = fixture();
    let permit = f
        .store
        .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
        .unwrap();
    let events = replacement(&f, &permit);
    let handoff = prepare_handoff(&f, &lease, &permit, &events);
    let before = f.store.replay(permit.campaign_id()).unwrap();
    let context = f
        .cas
        .put_json(&json!({"context":"started original worker"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    assert!(
        f.store
            .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("pending Attempts")
    );
    assert!(publish(&mut f, &lease, &permit, &events, &handoff).is_err());
    assert_eq!(f.store.replay(permit.campaign_id()).unwrap(), before);
    let diagnostic = f.cas.put_json(&json!({"failed":"worker stopped"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: diagnostic,
                    feedback_id: None,
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    assert!(
        publish(&mut f, &lease, &permit, &events, &handoff)
            .unwrap_err()
            .to_string()
            .contains("prefix")
    );
    let permit = f
        .store
        .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
        .unwrap();
    let events = replacement(&f, &permit);
    let handoff = prepare_handoff(&f, &lease, &permit, &events);
    let task_before = f
        .store
        .replay(&task_run_id(&f.revision.task_id).unwrap())
        .unwrap();
    let appended = publish(&mut f, &lease, &permit, &events, &handoff).unwrap();
    assert_eq!(appended.len(), 3);
    assert_eq!(appended[2].event_id, permit.event_id_at(2).unwrap());
    assert_eq!(
        f.store
            .replay(&task_run_id(&f.revision.task_id).unwrap())
            .unwrap(),
        task_before,
        "Round publication cannot rewrite Task budget or lifecycle"
    );
    let reopened = EventStore::open(&f.path).unwrap();
    assert_eq!(
        reopened.replay(permit.campaign_id()).unwrap(),
        f.store.replay(permit.campaign_id()).unwrap()
    );
    assert_eq!(
        reopened
            .replay(&task_run_id(&f.revision.task_id).unwrap())
            .unwrap(),
        task_before
    );
    assert!(
        reopened
            .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("exact current Review Round")
    );
    // No dispatch authority remains on the old Round, but actual late paid work is retained.
    let usage_id = f
        .cas
        .put_artifact(
            review_core::task::usage::TASK_TOKEN_USAGE_V3,
            producer(),
            vec![],
            None,
            serde_json::to_value(review_core::task::usage::TaskTokenUsageV3::charge_only(9))
                .unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: 9,
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    assert_eq!(
        f.state().execution.unwrap().attempt_accounting()[0].charged_tokens,
        9
    );
}

#[test]
fn review_round_publication_refuses_foreign_writer_review_prefix_and_source_identity() {
    let (mut f, lease) = fixture();
    let permit = f
        .store
        .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
        .unwrap();
    let events = replacement(&f, &permit);
    let handoff = prepare_handoff(&f, &lease, &permit, &events);
    let before = f.store.replay(permit.campaign_id()).unwrap();
    let mut foreign = lease.clone();
    foreign.writer = "foreign-writer".into();
    assert!(publish(&mut f, &foreign, &permit, &events, &handoff).is_err());
    let mut forged = events.clone();
    forged[0].payload["repository_id"] = json!("foreign");
    assert!(
        publish(&mut f, &lease, &permit, &forged, &handoff)
            .unwrap_err()
            .to_string()
            .contains("exact successor Source")
    );
    let mut forged = events.clone();
    forged[2].payload["epoch"] = json!(3);
    assert!(publish(&mut f, &lease, &permit, &forged, &handoff).is_err());
    let mut forged = events.clone();
    forged[2].payload["prior_finding_set_id"] = json!(f.cas.put_json(&json!({
        "subject_id":permit.predecessor().subject_id,"round":permit.next_round(),"prior_findings":[]
    })).unwrap());
    assert!(
        f.store
            .preview_task_review_round(&f.cas, &lease, &permit, &forged, &f.authority)
            .is_err()
    );
    assert_eq!(f.store.replay(permit.campaign_id()).unwrap(), before);
    f.store
        .append(
            permit.campaign_id(),
            &f.cas,
            NewEvent::new(EventType::SourceCapturedV1, events[0].payload.clone()),
        )
        .unwrap();
    let after = f.store.replay(permit.campaign_id()).unwrap();
    assert!(
        publish(&mut f, &lease, &permit, &events, &handoff)
            .unwrap_err()
            .to_string()
            .contains("prefix")
    );
    assert_eq!(f.store.replay(permit.campaign_id()).unwrap(), after);
}

#[test]
fn review_round_publication_rolls_back_source_and_supersession_when_final_insert_fails() {
    let (mut f, lease) = fixture();
    let permit = f
        .store
        .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
        .unwrap();
    let events = replacement(&f, &permit);
    let handoff = prepare_handoff(&f, &lease, &permit, &events);
    let before = f.store.replay(permit.campaign_id()).unwrap();
    let task_before = f
        .store
        .replay(&task_run_id(&f.revision.task_id).unwrap())
        .unwrap();
    f.store.conn.execute_batch("CREATE TRIGGER refuse_replacement BEFORE INSERT ON events WHEN NEW.type='RoundStarted@1' BEGIN SELECT RAISE(ABORT,'injected replacement failure'); END;").unwrap();
    let error = publish(&mut f, &lease, &permit, &events, &handoff).unwrap_err();
    assert!(
        error.to_string().contains("injected replacement failure"),
        "{error}"
    );
    assert_eq!(f.store.replay(permit.campaign_id()).unwrap(), before);
    assert_eq!(
        f.store
            .replay(&task_run_id(&f.revision.task_id).unwrap())
            .unwrap(),
        task_before
    );
    f.store
        .conn
        .execute_batch("DROP TRIGGER refuse_replacement")
        .unwrap();
    publish(&mut f, &lease, &permit, &events, &handoff).unwrap();
}

#[test]
fn prospective_successor_cannot_publish_when_original_remaining_tokens_cannot_fund_it() {
    let (mut f, lease) = fixture_with_tokens(Some(10));
    let context = f
        .cas
        .put_json(&json!({"context":"original reservation"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic = f.cas.put_json(&json!({"failed":"worker stopped"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: diagnostic,
                    feedback_id: None,
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    assert!(!f.state().execution.as_ref().unwrap().budget.breached());
    let permit = f
        .store
        .prepare_task_review_round_publication(&f.cas, &lease, &f.authority)
        .unwrap();
    let events = replacement(&f, &permit);
    let handoff = prepare_handoff(&f, &lease, &permit, &events);
    let before_task = f
        .store
        .replay(&task_run_id(lease.task_id()).unwrap())
        .unwrap();
    let before_review = f.store.replay(permit.campaign_id()).unwrap();
    let error = publish(&mut f, &lease, &permit, &events, &handoff).unwrap_err();
    assert!(
        error.to_string().contains("reserve") || error.to_string().contains("budget"),
        "{error}"
    );
    assert_eq!(f.store.replay(permit.campaign_id()).unwrap(), before_review);
    assert_eq!(
        f.store
            .replay(&task_run_id(lease.task_id()).unwrap())
            .unwrap(),
        before_task
    );
    assert_eq!(f.state().revision.limits.tokens, 10);
    assert_eq!(
        f.state().execution.unwrap().attempt_accounting()[0].charged_tokens,
        7
    );
}
