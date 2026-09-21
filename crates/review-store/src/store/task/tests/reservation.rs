use super::*;
use review_core::task::execution::TaskExecutionRecordV1;

#[test]
fn reservation_requires_exact_context_before_start_and_refuses_rebinding() {
    let mut f = Fixture::new(false).with_execution_graph();
    // The reservation must still be dispatchable after the store is reopened below; a loaded
    // runner can spend more than the fixture's one-second attempt allowance on that.
    f.revision.limits.verification.wall_ms = 60_000;
    f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.limits = f.revision.limits.clone();
    let mut graph: review_graph::task::CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph
        .allowances
        .get_mut("root.nodes.write")
        .unwrap()
        .wall_ms_per_attempt = 60_000;
    f.plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@1", &graph);
    f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
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

#[test]
fn inflight_usage_survives_reopen_revocation_lower_settlement_and_writer_loss() {
    use review_core::task::execution::TaskAttemptResultV1;
    for recover in [false, true] {
        let mut f = Fixture::new(true).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context = f
            .cas
            .put_json(&json!({"purpose":"bounded review"}))
            .unwrap();
        let attempt = f
            .store
            .reserve_and_bind_task_attempt(
                &f.cas,
                &lease,
                "root.nodes.write",
                &context,
                &f.authority,
            )
            .unwrap();
        let usage_id = f
            .cas
            .put_json(&json!({"provider":"receipt proof"}))
            .unwrap();
        let observe = |amount| TaskExecutionRecordV1::UsageObserved {
            attempt_id: attempt.id().into(),
            charged_tokens: amount,
            usage_id: usage_id.clone(),
            raw_artifact_ids: vec![],
        };
        let sequence = f.state().next_sequence;
        assert!(
            f.store
                .observe_task_usage(&f.cas, &lease, observe(4))
                .is_err()
        );
        assert_eq!(f.state().next_sequence, sequence);
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        for amount in [4, 4, 2] {
            f.store
                .observe_task_usage(&f.cas, &lease, observe(amount))
                .unwrap();
        }
        f.store = EventStore::open(&f.path).unwrap();
        let execution = f.state().execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), 4);
        assert_eq!(execution.budget.reserved_tokens(), 6);
        assert_eq!(execution.pending_attempts(), vec![attempt.id().to_string()]);
        f.store
            .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        f.store
            .observe_task_usage(&f.cas, &lease, observe(17))
            .unwrap();
        assert!(
            f.store
                .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
                .is_err()
        );
        // Revocation cannot erase receipt evidence for work that already started.
        f.authority.current = false;
        f.store
            .observe_task_usage(&f.cas, &lease, observe(20))
            .unwrap();
        let execution = f.state().execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), 20);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert!(execution.budget.breached());
        if recover {
            let time = f.state().lease_until + 1;
            f.store
                .append_task_transition(
                    &f.cas,
                    lease.task_id(),
                    TaskTransitionV1 {
                        writer: "recovery".into(),
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
                writer: "recovery".into(),
                epoch: 2,
            };
            assert!(
                f.store
                    .observe_task_usage(&f.cas, &lease, observe(100))
                    .is_err()
            );
            f.store
                .recover_task_attempts_at(&f.cas, &next, time)
                .unwrap();
            f.store
                .recover_task_attempts_at(&f.cas, &next, time)
                .unwrap();
        } else {
            let diagnostic_id = f
                .cas
                .put_json(&json!({"failure":"transport ended"}))
                .unwrap();
            let settlement = TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            };
            f.store
                .settle_task_attempt(&f.cas, &lease, settlement.clone(), &f.authority)
                .unwrap();
            let sequence = f.state().next_sequence;
            f.store
                .settle_task_attempt(&f.cas, &lease, settlement.clone(), &f.authority)
                .unwrap();
            assert_eq!(f.state().next_sequence, sequence);
            let mut changed = settlement;
            if let TaskExecutionRecordV1::Settled { charged_tokens, .. } = &mut changed {
                *charged_tokens = 21;
            }
            assert!(
                f.store
                    .settle_task_attempt(&f.cas, &lease, changed, &f.authority)
                    .is_err()
            );
            assert_eq!(f.state().next_sequence, sequence);
        }
        f.store = EventStore::open(&f.path).unwrap();
        let execution = f.state().execution.unwrap();
        assert!(execution.pending_attempts().is_empty());
        assert!(!execution.outputs.contains_key("root.nodes.write"));
        assert_eq!(execution.budget.committed_tokens(), 20);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert!(execution.budget.breached());
        let settlements: Vec<_> = f
            .store
            .replay(&task_run_id(lease.task_id()).unwrap())
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                let transition: TaskTransitionV1 = serde_json::from_value(event.payload).unwrap();
                let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                    return None;
                };
                let record = execution::read_execution_record(&f.cas, &record_id)
                    .unwrap()
                    .record;
                matches!(record, TaskExecutionRecordV1::Settled { .. }).then_some(record)
            })
            .collect();
        assert!(matches!(&settlements[..], [TaskExecutionRecordV1::Settled {
            attempt_id, charged_tokens, ..
        }] if attempt_id == attempt.id() && *charged_tokens == if recover { 20 } else { 7 }));
    }
}

fn put<T: serde::Serialize>(cas: &Cas, kind: &str, value: &T) -> String {
    cas.put_artifact(
        kind,
        producer(),
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}
