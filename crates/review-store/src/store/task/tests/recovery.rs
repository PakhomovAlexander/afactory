//! Abandoned Attempts (ADR-0209): a lost writer's Attempt and a host-cancelled one leave the
//! Attempt limits, are charged only what they were observed to use, and are bounded by the
//! node's recovery allowance.
use super::*;
use crate::store::task::execution::{PreparedTaskAttempt, TaskAbandonCause};
use review_core::task::execution::*;

const NODE: &str = "root.nodes.write";

fn start(
    f: &mut Fixture,
    lease: &TaskLease,
    label: &str,
    clock: &std::cell::Cell<u64>,
) -> PreparedTaskAttempt {
    start_at_offset(f, lease, label, clock, -1).unwrap()
}

fn start_at_offset(
    f: &mut Fixture,
    lease: &TaskLease,
    label: &str,
    clock: &std::cell::Cell<u64>,
    offset: i64,
) -> Option<PreparedTaskAttempt> {
    let context = f.cas.put_json(&json!({ "context": label })).unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, lease, NODE, &context, &f.authority)
        .unwrap();
    let deadline = attempt.reservation().deadline_unix_ms;
    assert_eq!(deadline, clock.get() + 1_000);
    assert_eq!(attempt.reservation().tokens, 10);
    f.store = EventStore::open(&f.path).unwrap();
    let run = task_run_id(lease.task_id()).unwrap();
    let prefix = f.store.replay(&run).unwrap();
    let state = f.state();
    let budget = state.execution.as_ref().unwrap().budget.clone();
    clock.set(deadline.checked_add_signed(offset).unwrap());
    assert!(clock.get() < state.lease_until);
    assert!(clock.get() < state.revision.limits.deadline_unix_ms);
    let started = f
        .store
        .start_task_attempt(&f.cas, lease, &attempt, &f.authority);
    if offset >= 0 {
        assert!(matches!(started, Err(StoreError::Conflict(message))
        if message == format!(
            "Task reservation for node {NODE} is not dispatchable: \
             its Attempt deadline passed: now {} ms >= deadline {deadline} ms",
            clock.get()
        )));
        f.store = EventStore::open(&f.path).unwrap();
        assert_eq!(f.store.replay(&run).unwrap(), prefix);
        let execution = f.state().execution.unwrap();
        assert_eq!(
            execution.budget.committed_tokens(),
            budget.committed_tokens()
        );
        assert_eq!(execution.budget.reserved_tokens(), budget.reserved_tokens());
        assert_eq!(execution.budget.begun_attempts(), budget.begun_attempts());
        f.store
            .release_task_attempt(&f.cas, lease, &attempt, "expired fixture")
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let execution = f.state().execution.unwrap();
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.committed_tokens(), 0);
        assert_eq!(execution.budget.begun_attempts(), 0);
        return None;
    }
    started.unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    let row = execution
        .attempt_accounting()
        .into_iter()
        .find(|row| row.attempt_id == attempt.id())
        .unwrap();
    assert_eq!(row.started_unix_ms, Some(clock.get()));
    assert_eq!(row.reservation, *attempt.reservation());
    assert_eq!(row.context_id.as_deref(), Some(attempt.context_id()));
    Some(attempt)
}

/// The lease expires and `writer` takes it, as `af task run` does after its predecessor died.
/// Store time advances explicitly beyond expiry; no real-time wait runs on a frozen clock.
fn take_over(
    f: &mut Fixture,
    previous: &TaskLease,
    writer: &str,
    clock: &std::cell::Cell<u64>,
) -> TaskLease {
    let expiry = f.state().lease_until;
    let run = task_run_id(previous.task_id()).unwrap();
    let prefix = f.store.replay(&run).unwrap();
    clock.set(expiry - 1);
    let refused = f
        .store
        .take_task_lease(&f.cas, previous.task_id(), writer, 1_000_000)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("takeover is premature"), "{refused}");
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(f.store.replay(&run).unwrap(), prefix);
    clock.set(expiry + 1);
    let time = now().unwrap();
    let epoch = previous.epoch + 1;
    f.store
        .append_task_transition(
            &f.cas,
            &previous.task_id,
            TaskTransitionV1 {
                writer: writer.into(),
                epoch,
                now_unix_ms: time,
                change: TaskChangeV1::LeaseTaken {
                    lease_until_unix_ms: time + 1_000_000,
                },
            },
        )
        .unwrap();
    let next = TaskLease {
        task_id: previous.task_id.clone(),
        writer: writer.into(),
        epoch,
    };
    f.store
        .recover_task_attempts_at(&f.cas, &next, time)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let prefix = f.store.replay(&run).unwrap();
    assert!(f.store.renew_task_lease(&f.cas, previous, 3_000).is_err());
    assert!(
        f.store
            .recover_task_attempts_at(&f.cas, previous, time)
            .is_err()
    );
    f.store
        .recover_task_attempts_at(&f.cas, &next, time)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(f.store.replay(&run).unwrap(), prefix);
    next
}

#[test]
fn a_lost_writers_attempt_is_charged_its_observed_usage_and_dispatched_again_once() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        // Retain the original three-second lease; setup runs at controlled Store time.
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
            .unwrap();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let lost = start(&mut f, &lease, "first", clock);
        let reserved = u128::from(lost.reservation().tokens);
        let usage_id = f.cas.put_json(&json!({"provider":"partial"})).unwrap();
        f.store
            .observe_task_usage(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: lost.id().into(),
                    charged_tokens: 3,
                    usage_id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
        let last_seen = f.state().last_recorded_unix_ms();
        let expiry = f.state().lease_until;
        let second = take_over(&mut f, &lease, "writer-2", clock);

        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.as_ref().unwrap();
        let row = execution
            .attempt_accounting()
            .into_iter()
            .find(|row| row.attempt_id == lost.id())
            .unwrap();
        assert!(matches!(
            row.result,
            Some(TaskAttemptResultV1::Abandoned { .. })
        ));
        assert_eq!(row.abandoned, Some(TaskAbandonCause::WriterLost));
        // Charged what the Provider reported before the loss, never the reservation: the rest of
        // the reservation is back in the Task's remaining tokens.
        assert!(reserved > 3);
        assert_eq!(row.charged_tokens, 3);
        assert_eq!(execution.budget.committed_tokens(), 3);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(
            execution.budget.remaining_limits().tokens,
            state.revision.limits.tokens - 3
        );
        // It began, but counts against no Attempt limit.
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert_eq!(execution.budget.chargeable_attempts(), 0);
        let recovery = execution.recoveries();
        assert_eq!(recovery.len(), 1);
        assert_eq!(
            (
                recovery[0].node.as_str(),
                recovery[0].chargeable,
                recovery[0].writer_lost,
                recovery[0].host_cancelled,
                recovery[0].allowance,
                recovery[0].exhausted(),
                recovery[0].last_abandoned_unix_ms,
            ),
            (NODE, 0, 1, 0, 1, false, Some(last_seen))
        );
        // The diagnostic records the loss: the lost writer's last record, its lease's expiry and
        // when the successor observed it.
        let Some(TaskAttemptResultV1::Abandoned { diagnostic_id }) = &row.result else {
            unreachable!()
        };
        let diagnostic = f.cas.get_json(diagnostic_id).unwrap();
        assert_eq!(diagnostic["cause"], "writer_lost");
        assert_eq!(diagnostic["last_recorded_unix_ms"], last_seen);
        assert_eq!(diagnostic["lease_expiry_unix_ms"], expiry);
        assert!(diagnostic["observed_unix_ms"].as_u64().unwrap() > expiry);

        // The node is dispatched again under the successor. Its host cancels that Attempt, which it
        // settles as abandoned itself: the recovery allowance of one is then spent.
        let cancelled = start(&mut f, &second, "second", clock);
        let diagnostic_id = f
            .cas
            .put_json(&json!({"schema":"af.task-diagnostic/1","cause":"host_cancelled"}))
            .unwrap();
        f.store
            .settle_task_attempt(
                &f.cas,
                &second,
                TaskExecutionRecordV1::Settled {
                    attempt_id: cancelled.id().into(),
                    charged_tokens: 0,
                    result: TaskAttemptResultV1::Abandoned { diagnostic_id },
                    raw_artifact_ids: vec![],
                    usage_id: None,
                    unknown_usage: Some(task::usage::TaskUnknownUsageV1 {
                        cause: task::usage::TaskUnknownUsageCauseV1::Interrupted,
                    }),
                },
                &f.authority,
            )
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.as_ref().unwrap();
        let recovery = &execution.recoveries()[0];
        assert_eq!(
            (
                recovery.writer_lost,
                recovery.host_cancelled,
                recovery.chargeable
            ),
            (1, 1, 0)
        );
        assert!(recovery.exhausted());
        let context = f.cas.put_json(&json!({"context":"third"})).unwrap();
        let refused = f
            .store
            .reserve_and_bind_task_attempt(&f.cas, &second, NODE, &context, &f.authority)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&format!(
                "Task node {NODE} exhausted its recovery allowance: 2 abandoned Attempts, recovery \
             allowance 1"
            )),
            "{refused}"
        );
    });
}

/// Below its reservation, abandoned work states how its usage is known: a report, or the
/// unknown-usage marker.
#[test]
fn abandoned_work_below_its_reservation_needs_a_usage_report_or_the_marker() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let attempt = start(&mut f, &lease, "unreported", clock);
        let diagnostic_id = f.cas.put_json(&json!({"failure":"fixture"})).unwrap();
        let settle = |unknown_usage| TaskExecutionRecordV1::Settled {
            attempt_id: attempt.id().into(),
            charged_tokens: 0,
            result: TaskAttemptResultV1::Abandoned {
                diagnostic_id: diagnostic_id.clone(),
            },
            raw_artifact_ids: vec![],
            usage_id: None,
            unknown_usage,
        };
        let refused = f
            .store
            .settle_task_attempt(&f.cas, &lease, settle(None), &f.authority)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("needs a usage report"), "{refused}");
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                settle(Some(task::usage::TaskUnknownUsageV1 {
                    cause: task::usage::TaskUnknownUsageCauseV1::Interrupted,
                })),
                &f.authority,
            )
            .unwrap();
        let row = f.state().execution.unwrap().attempt_accounting().remove(0);
        assert_eq!(row.abandoned, Some(TaskAbandonCause::HostCancelled));
        assert_eq!(row.charged_tokens, 0);
    });
}

/// A native observation of 3 tokens with `charge_complete: false`, against the Attempt's larger
/// reservation, as the runtime retains it before settlement: the wall record keeps the observed
/// usage beside the observation.
fn observe_incomplete(f: &mut Fixture, lease: &TaskLease, attempt: &PreparedTaskAttempt) -> String {
    let observation = task::usage::TaskUsageObservationV1 {
        reported_usage: Some(task::usage::TaskTokenUsageV3::charge_only(3)),
        charge_complete: false,
    };
    let run_id = task_run_id(lease.task_id()).unwrap();
    f.store
        .record_task_attempt_wall_with_observation(
            &crate::TaskAttemptWall {
                run_id: run_id.clone(),
                attempt_id: attempt.id().into(),
                node_id: NODE.into(),
                round: 0,
                epoch: u32::try_from(lease.epoch).unwrap(),
                started_unix_ms: now().unwrap(),
                elapsed_ms: 1,
                usage: Some(task::usage::TaskTokenUsageV3::charge_only(3)),
            },
            &observation,
        )
        .unwrap();
    execution::usage_observation::capture_task_usage_observation(
        &f.cas,
        review_core::Producer::Attempt {
            run_id,
            node_id: NODE.into(),
            attempt_id: attempt.id().into(),
        },
        attempt.context_id(),
        &observation,
    )
    .unwrap()
}

/// ADR-0209 item 4 with a native observation: an Attempt whose Provider reported 3 tokens
/// without completing its counters is charged exactly 3 when its host cancels it and when its
/// writer is lost, and the rest of its reservation returns to the Task; settled by its own
/// failure instead, the same counters still keep the reservation floor.
#[test]
fn abandoned_attempts_with_incomplete_native_usage_are_charged_what_was_observed() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
            .unwrap();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let limit = f.state().revision.limits.tokens;

        // Writer loss: the successor settles the Attempt from the retained wall and observation.
        let lost = start(&mut f, &lease, "lost", clock);
        assert!(u128::from(lost.reservation().tokens) > 3);
        observe_incomplete(&mut f, &lease, &lost);
        let second = take_over(&mut f, &lease, "writer-2", clock);
        let state = f.state();
        let execution = state.execution.as_ref().unwrap();
        let row = execution
            .attempt_accounting()
            .into_iter()
            .find(|row| row.attempt_id == lost.id())
            .unwrap();
        assert_eq!(row.abandoned, Some(TaskAbandonCause::WriterLost));
        assert_eq!(row.charged_tokens, 3);
        assert_eq!(execution.budget.committed_tokens(), 3);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.remaining_limits().tokens, limit - 3);

        // Host cancellation: the Attempt's own writer settles it abandoned at the observed 3.
        let cancelled = start(&mut f, &second, "cancelled", clock);
        let observation_id = observe_incomplete(&mut f, &second, &cancelled);
        let diagnostic_id = f
            .cas
            .put_json(&json!({"schema":"af.task-diagnostic/1","cause":"host_cancelled"}))
            .unwrap();
        let usage_id = f.cas.put_json(&json!({"provider":"partial"})).unwrap();
        f.store
            .settle_task_attempt(
                &f.cas,
                &second,
                TaskExecutionRecordV1::Settled {
                    attempt_id: cancelled.id().into(),
                    charged_tokens: 3,
                    result: TaskAttemptResultV1::Abandoned { diagnostic_id },
                    raw_artifact_ids: vec![observation_id.clone()],
                    usage_id: Some(usage_id.clone()),
                    unknown_usage: None,
                },
                &f.authority,
            )
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.as_ref().unwrap();
        let row = execution
            .attempt_accounting()
            .into_iter()
            .find(|row| row.attempt_id == cancelled.id())
            .unwrap();
        assert_eq!(row.abandoned, Some(TaskAbandonCause::HostCancelled));
        assert_eq!(row.charged_tokens, 3);
        assert_eq!(execution.budget.committed_tokens(), 6);
        assert_eq!(execution.budget.remaining_limits().tokens, limit - 6);
    });
}

/// The reservation floor of incomplete counters still binds an Attempt its own failure ended:
/// settled below its reservation it is refused, at its reservation it is admitted.
#[test]
fn a_failed_attempt_with_incomplete_native_usage_keeps_the_reservation_floor() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let attempt = start(&mut f, &lease, "failed", clock);
        let reserved = u128::from(attempt.reservation().tokens);
        let observation_id = observe_incomplete(&mut f, &lease, &attempt);
        let diagnostic_id = f.cas.put_json(&json!({"failure":"fixture"})).unwrap();
        let settle = |charged_tokens| TaskExecutionRecordV1::Settled {
            attempt_id: attempt.id().into(),
            charged_tokens,
            result: TaskAttemptResultV1::Failed {
                diagnostic_id: diagnostic_id.clone(),
                feedback_id: None,
            },
            raw_artifact_ids: vec![observation_id.clone()],
            usage_id: None,
            unknown_usage: None,
        };
        let refused = f
            .store
            .settle_task_attempt(&f.cas, &lease, settle(3), &f.authority)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("incomplete native observation floor"),
            "{refused}"
        );
        f.store
            .settle_task_attempt(&f.cas, &lease, settle(reserved), &f.authority)
            .unwrap();
        let row = f.state().execution.unwrap().attempt_accounting().remove(0);
        assert_eq!(row.charged_tokens, reserved);
    });
}

/// The recovery setup must obey the original Attempt deadline independently of the lease.
#[test]
fn recovery_fixture_dispatch_refuses_deadline_and_later_after_cold_reopen() {
    for label in ["first", "lost"] {
        for offset in [0, 1] {
            test_clock::with_time(1_000_000, |clock| {
                let mut f = Fixture::new(false).with_execution_graph();
                let lease = f
                    .store
                    .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
                    .unwrap();
                f.propose(&lease);
                f.store
                    .admit_task_plan(&f.cas, &lease, &f.authority)
                    .unwrap();
                f.record_execution_inputs(&lease);
                assert!(start_at_offset(&mut f, &lease, label, clock, offset).is_none());
            });
        }
    }
}

/// The wall-end evidence of one Attempt: its partial candidate and its wall end, produced by
/// `attempt` at node `by`, kept in the sidecar under `attempt` (ADR-0253). Returns their IDs.
fn keep_wall_end(f: &mut Fixture, lease: &TaskLease, attempt: &str, by: &str) -> (String, String) {
    use review_core::task::attempt_wall::*;
    let run = task_run_id(lease.task_id()).unwrap();
    let producer = review_core::Producer::Attempt {
        run_id: run.clone(),
        node_id: by.into(),
        attempt_id: attempt.into(),
    };
    let digest = |n: char| format!("sha256:{}", n.to_string().repeat(64));
    let partial = TaskPartialCandidateV1 {
        schema: TASK_PARTIAL_CANDIDATE_SCHEMA.into(),
        parent_snapshot_id: digest('a'),
        snapshot_id: digest('b'),
        source_tree_id: digest('c'),
        changed_paths: 2,
    };
    let (partial_id, _) = f
        .cas
        .put_artifact(
            TASK_PARTIAL_CANDIDATE_V1,
            producer.clone(),
            vec![],
            None,
            serde_json::to_value(&partial).unwrap(),
        )
        .unwrap();
    let end = TaskAttemptWallEndV1 {
        schema: TASK_ATTEMPT_WALL_END_SCHEMA.into(),
        task_id: lease.task_id().into(),
        node: NODE.into(),
        attempt_id: attempt.into(),
        wall_ms: 1_000,
        elapsed_ms: 1_001,
        partial_candidate_id: Some(partial_id.clone()),
    };
    let (wall_end_id, _) = f
        .cas
        .put_artifact(
            TASK_ATTEMPT_WALL_END_V1,
            producer,
            vec![partial_id.clone()],
            None,
            serde_json::to_value(&end).unwrap(),
        )
        .unwrap();
    f.cas.flush().unwrap();
    f.store
        .record_task_attempt_wall_end(&run, attempt, &wall_end_id)
        .unwrap();
    // Recorded again it is the same record; another one is refused.
    f.store
        .record_task_attempt_wall_end(&run, attempt, &wall_end_id)
        .unwrap();
    assert!(
        f.store
            .record_task_attempt_wall_end(&run, attempt, &partial_id)
            .is_err(),
        "one Attempt keeps one wall end"
    );
    (partial_id, wall_end_id)
}

/// A writer lost after its Attempt wall ended an Attempt, and after it kept the wall end, leaves
/// its successor that wall end (ADR-0253): the Attempt settles failed at its wall with the wall
/// end and the partial tree it names, counts as a failed Attempt and spends no recovery
/// allowance, where without it the Attempt reads abandoned by a lost writer.
#[test]
fn a_lost_writers_wall_ended_attempt_settles_failed_from_its_kept_wall_end() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
            .unwrap();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let walled = start(&mut f, &lease, "walled", clock);
        let (partial_id, wall_end_id) = keep_wall_end(&mut f, &lease, walled.id(), NODE);
        let _second = take_over(&mut f, &lease, "writer-2", clock);

        let state = f.state();
        let execution = state.execution.as_ref().unwrap();
        let row = execution
            .attempt_accounting()
            .into_iter()
            .find(|row| row.attempt_id == walled.id())
            .unwrap();
        let Some(TaskAttemptResultV1::Failed {
            diagnostic_id,
            feedback_id: None,
        }) = &row.result
        else {
            panic!("a wall-ended Attempt settles failed: {:?}", row.result)
        };
        assert_eq!(
            f.cas.get_json(diagnostic_id).unwrap()["cause"],
            "attempt_wall"
        );
        assert_eq!(row.abandoned, None);
        assert!(row.raw_artifact_ids.contains(&wall_end_id));
        assert!(row.raw_artifact_ids.contains(&partial_id));
        // Nothing was reported: charged zero, its usage unknown because its writer was lost.
        assert_eq!(row.charged_tokens, 0);
        assert_eq!(
            row.unknown_usage,
            Some(task::usage::TaskUnknownUsageCauseV1::LeaseExpired)
        );
        // A failed Attempt, counted as one; no recovery allowance is spent.
        assert_eq!(execution.budget.chargeable_attempts(), 1);
        assert!(execution.recoveries().is_empty());
    });
}

/// A kept wall end roots its evidence against collection (ADR-0253): a writer lost after it kept
/// the wall end, whose lease then expired, leaves a Task that `af task gc --apply` may sweep
/// before anyone recovers it, and the sweep keeps the wall end and the partial tree it names,
/// so the successor still settles the Attempt failed at its wall.
#[test]
fn a_kept_wall_end_and_its_partial_tree_survive_collection_before_recovery() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
            .unwrap();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let walled = start(&mut f, &lease, "walled", clock);
        let (partial_id, wall_end_id) = keep_wall_end(&mut f, &lease, walled.id(), NODE);
        // The writer is lost: its lease expires and collection runs before any successor.
        clock.set(f.state().lease_until + 1);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let outcome = f.store.apply_task_collection(&f.cas, 0, 0, false).unwrap();
        assert!(outcome.swept);
        assert!(outcome.tombstoned.is_empty(), "an unfinished Task is kept");
        assert!(f.cas.is_filed(&wall_end_id), "the wall end was swept");
        assert!(f.cas.is_filed(&partial_id), "the partial tree was swept");

        let _second = take_over(&mut f, &lease, "writer-2", clock);
        let state = f.state();
        let row = state
            .execution
            .as_ref()
            .unwrap()
            .attempt_accounting()
            .into_iter()
            .find(|row| row.attempt_id == walled.id())
            .unwrap();
        assert!(
            matches!(row.result, Some(TaskAttemptResultV1::Failed { .. })),
            "{:?}",
            row.result
        );
        assert!(row.raw_artifact_ids.contains(&wall_end_id));
    });
}

/// A wall end kept under an Attempt that another node produced is not its evidence: the
/// successor refuses to settle from it rather than reading it as this Attempt's wall.
#[test]
fn a_wall_end_another_attempt_produced_is_refused_at_recovery() {
    test_clock::with_time(1_000_000, |clock| {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
            .unwrap();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let walled = start(&mut f, &lease, "walled", clock);
        keep_wall_end(&mut f, &lease, walled.id(), "root.nodes.other");
        clock.set(f.state().lease_until + 1);
        let time = now().unwrap();
        let epoch = lease.epoch + 1;
        f.store
            .append_task_transition(
                &f.cas,
                &lease.task_id,
                TaskTransitionV1 {
                    writer: "writer-2".into(),
                    epoch,
                    now_unix_ms: time,
                    change: TaskChangeV1::LeaseTaken {
                        lease_until_unix_ms: time + 1_000_000,
                    },
                },
            )
            .unwrap();
        let next = TaskLease {
            task_id: lease.task_id.clone(),
            writer: "writer-2".into(),
            epoch,
        };
        let refused = f
            .store
            .recover_task_attempts_at(&f.cas, &next, time)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("not that Attempt's own record"),
            "{refused}"
        );
    });
}
