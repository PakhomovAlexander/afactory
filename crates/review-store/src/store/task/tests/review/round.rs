use super::*;
use crate::store::task::review_round::ReviewRoundFence;

pub(in crate::store::task::tests) fn round_fixture() -> (Fixture, LegacyReviewRoundV1) {
    let mut f = Fixture::new(false).with_execution_graph();
    let context = canonical_context(
        &mut f,
        &format!("sha256:{}", "a".repeat(64)),
        &"a".repeat(26),
    );
    let subject: review_core::SubjectV1 =
        serde_json::from_value(f.cas.get_json(&context.subject_id).unwrap()).unwrap();
    let round = LegacyReviewRoundV1 {
        campaign_id: context.campaign_id,
        round_event_id: context.round_event_id,
        campaign_manifest_id: context.campaign_manifest_id,
        subject_id: context.subject_id,
        head_snapshot_id: subject.head_snapshot_id,
        round: 1,
        epoch: 1,
    };
    let id = f
        .cas
        .put_artifact(
            LEGACY_REVIEW_ROUND_V1,
            producer(),
            round
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(round.head_snapshot_id.clone()),
            serde_json::to_value(&round).unwrap(),
        )
        .unwrap()
        .0;
    let input = task::ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: LEGACY_REVIEW_ROUND_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(round.head_snapshot_id.clone()),
    };
    f.revision.inputs.insert("round".into(), input.clone());
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
    // This Store fixture supplies a trusted compiler double. Extend its actual root port so
    // all normal graph/input publication checks still apply to the captured Round.
    let mut graph: review_graph::task::CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs.insert("round".into(), input);
    graph
        .nodes
        .get_mut("root.inputs")
        .unwrap()
        .contract
        .outputs
        .insert(
            "round".into(),
            task::pipeline::PipelinePortV1 {
                artifact_type: LEGACY_REVIEW_ROUND_V1.into(),
                cardinality: PortCardinality::One,
                optional: false,
                affinity: task::pipeline::PortAffinityV1::Unbound {},
                root_default: None,
                covers: BTreeSet::new(),
            },
        );
    f.plan.compiled_graph_id = f
        .cas
        .put_artifact(
            "af/CompiledTask@1",
            producer(),
            vec![],
            None,
            serde_json::to_value(graph).unwrap(),
        )
        .unwrap()
        .0;
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.inputs = f.revision.inputs.clone();
    f.plan_id = f
        .cas
        .put_artifact(
            task::EXECUTION_PLAN_V1,
            producer(),
            vec![f.revision_id.clone()],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;
    (f, round)
}

pub(in crate::store::task::tests) fn supersede(f: &Fixture, round: &LegacyReviewRoundV1) {
    let mut other = EventStore::open(&f.path).unwrap();
    let old = other
        .latest_round_started(&round.campaign_id)
        .unwrap()
        .unwrap();
    let mut replacement: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(old.payload).unwrap();
    replacement.epoch += 1;
    other
        .append_batch(
            &round.campaign_id,
            &f.cas,
            &[
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(review_core::RoundInputSupersededPayloadV1 {
                        round: round.round,
                        old_epoch: round.epoch,
                        new_epoch: round.epoch + 1,
                        campaign_manifest_id: round.campaign_manifest_id.clone(),
                        old_subject_id: round.subject_id.clone(),
                        replacement_subject_id: round.subject_id.clone(),
                    })
                    .unwrap(),
                )
                .caused_by(&round.round_event_id),
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(replacement).unwrap(),
                )
                .caused_by(&round.round_event_id)
                .referencing(old.artifact_refs),
            ],
        )
        .unwrap();
}

#[test]
fn closed_review_round_refuses_prepared_start_and_keeps_credit_release_available() {
    let (mut f, round) = round_fixture();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"exact":"context"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    let fence = ReviewRoundFence::capture(&f.cas, &f.revision)
        .unwrap()
        .unwrap();
    let mut other = EventStore::open(&f.path).unwrap();
    other
        .append(
            &round.campaign_id,
            &f.cas,
            NewEvent::new(
                EventType::RunReportV2,
                serde_json::to_value(review_core::RunReportPayloadV2 {
                    outcomes: vec![review_core::RunNodeReportV2 {
                        node: "reviewer".into(),
                        outcome: review_core::RunNodeOutcomeV2::Failed {
                            error: "worker unavailable".into(),
                        },
                    }],
                    blocked_gates: vec![],
                    verdict: review_core::RunVerdictV2::Incomplete {
                        missing_nodes: vec![review_core::MissingNodeV2 {
                            node: "reviewer".into(),
                            reason: "worker unavailable".into(),
                        }],
                    },
                    spent_tokens: Some(0),
                })
                .unwrap(),
            )
            .caused_by(&round.round_event_id),
        )
        .unwrap();
    // An Incomplete report leaves this exact Round open for recovery.
    fence.validate(&f.store.conn).unwrap();
    f.store
        .check_task_dispatch(&f.cas, &lease, &f.authority)
        .unwrap();
    other
        .append(
            &round.campaign_id,
            &f.cas,
            NewEvent::new(
                EventType::RunReportV2,
                serde_json::to_value(review_core::RunReportPayloadV2 {
                    outcomes: vec![review_core::RunNodeReportV2 {
                        node: "reviewer".into(),
                        outcome: review_core::RunNodeOutcomeV2::Failed {
                            error: "budget exhausted".into(),
                        },
                    }],
                    blocked_gates: vec![],
                    verdict: review_core::RunVerdictV2::Fail {
                        reason: review_core::RunFailureReasonV2::Exhausted,
                    },
                    spent_tokens: Some(0),
                })
                .unwrap(),
            )
            .caused_by(&round.round_event_id),
        )
        .unwrap();
    let before = f.state().next_sequence;
    assert!(
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    {
        let tx = f
            .store
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        assert!(fence.validate(&tx).is_err());
    }
    f.store
        .release_task_attempt(&f.cas, &lease, &attempt, "Round closed before dispatch")
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(execution.budget.reserved_tokens(), 0);
}

#[test]
fn superseded_review_round_blocks_dispatch_but_retains_started_usage_and_settlement() {
    let (mut f, round) = round_fixture();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"exact":"context"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    f.store
        .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    supersede(&f, &round);
    let first = f.state().next_sequence;
    let error = f
        .store
        .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
        .unwrap_err();
    assert!(
        error.to_string().contains("superseded or closed"),
        "{error}"
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, first);
    let usage = f.cas.put_json(&json!({"paid":7})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                usage_id: usage.clone(),
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 3,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: usage,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err()
    );
}

#[test]
fn review_round_write_fence_compares_other_campaign_changes_inside_transaction() {
    let (mut f, round) = round_fixture();
    let lease = f.open();
    let state = f.state();
    let transition = TaskTransitionV1 {
        writer: lease.writer.clone(),
        epoch: lease.epoch,
        now_unix_ms: now().unwrap(),
        change: TaskChangeV1::PlanProposed {
            plan_id: f.plan_id.clone(),
        },
    };
    let value = serde_json::to_value(&transition).unwrap();
    let event = NewEvent::new(EventType::TaskTransitionV1, value.clone())
        .referencing(references(&f.cas, &transition.change, Some(&state)).unwrap());
    let run = task_run_id(lease.task_id()).unwrap();
    let permit = WritePermit {
        run_id: run.clone(),
        first: state.next_sequence,
        payloads: vec![value],
        event_type: EventType::TaskTransitionV1,
        valid_until: None,
        review_round: ReviewRoundFence::capture(&f.cas, &state.revision).unwrap(),
    };
    permit
        .validate(
            &f.store.conn,
            &run,
            state.next_sequence as i64,
            &[event.clone()],
        )
        .unwrap();
    supersede(&f, &round);
    // No Task event changed; a Task-only sequence comparison would accept this stale write.
    assert_eq!(f.state().next_sequence, state.next_sequence);
    let error = f
        .store
        .append_batch_inner(&run, &f.cas, &[event], Some(&permit), None)
        .unwrap_err();
    assert!(
        error.to_string().contains("superseded or closed"),
        "{error}"
    );
    assert_eq!(f.state().next_sequence, state.next_sequence);
}

#[test]
fn captured_review_round_rejects_forged_subject_and_missing_round_before_task_open() {
    for mutation in ["head", "round", "epoch", "manifest"] {
        let (mut f, mut round) = round_fixture();
        match mutation {
            "head" => round.head_snapshot_id = f.revision.authority.policy_id.clone(),
            "round" => round.round_event_id = "z".repeat(26),
            "epoch" => round.epoch = 2,
            "manifest" => round.campaign_manifest_id = f.revision.authority.policy_id.clone(),
            _ => unreachable!(),
        }
        let id = f
            .cas
            .put_artifact(
                LEGACY_REVIEW_ROUND_V1,
                producer(),
                round
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                Some(round.head_snapshot_id.clone()),
                serde_json::to_value(&round).unwrap(),
            )
            .unwrap()
            .0;
        let input = f.revision.inputs.get_mut("round").unwrap();
        input.artifact_ids = vec![id];
        input.snapshot_id = Some(round.head_snapshot_id);
        let revision = f
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
        assert!(
            f.store
                .open_task(&f.cas, &revision, "owner", 100_000)
                .is_err(),
            "{mutation}"
        );
        assert_eq!(f.store.len(&task_run_id("task-1").unwrap()).unwrap(), 0);
    }
}
