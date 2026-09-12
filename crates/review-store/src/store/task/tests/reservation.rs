use super::*;
use review_core::task::execution::TaskExecutionRecordV1;

#[test]
fn reservation_requires_exact_context_before_start_and_refuses_rebinding() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation_id = f.record_execution_inputs(&lease);
    let reserved = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap();
    assert_eq!(reserved.invocation_id(), invocation_id);
    let before = f.state().next_sequence;
    let record_id = f
        .cas
        .put_artifact(
            review_core::task::execution::TASK_EXECUTION_RECORD_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(TaskExecutionRecordV1::Started {
                attempt_id: reserved.id().into(),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    assert!(
        f.store
            .append_task_transition(
                &f.cas,
                lease.task_id(),
                TaskTransitionV1 {
                    writer: lease.writer.clone(),
                    epoch: lease.epoch,
                    now_unix_ms: now().unwrap(),
                    change: TaskChangeV1::ExecutionRecorded { record_id },
                }
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    assert_eq!(f.state().execution.unwrap().budget.begun_attempts(), 0);
    let context = f
        .cas
        .put_json(&json!({"attempt_id":reserved.id(),
        "reservation_id": reserved.reservation().id, "invocation_id":invocation_id}))
        .unwrap();
    let prepared = f
        .store
        .bind_task_attempt_context(&f.cas, &lease, &reserved, &context, &f.authority)
        .unwrap();
    assert_eq!(prepared.id(), reserved.id());
    f.store = EventStore::open(&f.path).unwrap();
    let changed = f.cas.put_json(&json!({"attempt_id":"invented"})).unwrap();
    let before = f.state().next_sequence;
    for id in [&context, &changed] {
        assert!(
            f.store
                .bind_task_attempt_context(&f.cas, &lease, &reserved, id, &f.authority)
                .is_err()
        );
    }
    assert_eq!(f.state().next_sequence, before);
    f.store
        .start_task_attempt(&f.cas, &lease, &prepared, &f.authority)
        .unwrap();
    assert!(
        f.store
            .release_reserved_task_attempt(&f.cas, &lease, &reserved, "already started")
            .is_err()
    );
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.reserved_tokens(), 10);
}

#[test]
fn unbound_reservation_recovery_releases_credit_and_fences_old_context() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let reserved = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap();
    let time = f.state().lease_until + 1;
    f.store
        .append_task_transition(
            &f.cas,
            lease.task_id(),
            TaskTransitionV1 {
                writer: "writer-2".into(),
                epoch: 2,
                now_unix_ms: time,
                change: TaskChangeV1::LeaseTaken {
                    lease_until_unix_ms: time + 10000,
                },
            },
        )
        .unwrap();
    let next = TaskLease {
        task_id: lease.task_id.clone(),
        writer: "writer-2".into(),
        epoch: 2,
    };
    f.store
        .recover_task_attempts_at(&f.cas, &next, time)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert!(execution.pending_attempts().is_empty());
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(execution.budget.committed_tokens(), 0);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    let context = f
        .cas
        .put_json(&json!({"attempt_id":reserved.id()}))
        .unwrap();
    for stale in [&lease, &next] {
        assert!(
            f.store
                .bind_task_attempt_context(&f.cas, stale, &reserved, &context, &f.authority)
                .is_err()
        );
        assert!(
            f.store
                .release_reserved_task_attempt(&f.cas, stale, &reserved, "stale")
                .is_err()
        );
    }
}

#[test]
fn effect_currentness_requires_started_work_and_rechecks_revocation_and_settlement() {
    use review_core::task::execution::TaskAttemptResultV1;
    let mut f = Fixture::new(true).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.decide(&lease, PlanDecisionKindV1::Approved);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let reservation = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap();
    let context = f
        .cas
        .put_json(&json!({"attempt":reservation.id()}))
        .unwrap();
    let attempt = f
        .store
        .bind_task_attempt_context(&f.cas, &lease, &reservation, &context, &f.authority)
        .unwrap();
    assert!(
        f.store
            .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    f.store
        .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    f.authority.current = false;
    assert!(
        f.store
            .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
    f.authority.current = true;
    f.store
        .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic_id = f
        .cas
        .put_json(&json!({"error":"observed execution failure"}))
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    assert!(
        f.store
            .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 7);
}
