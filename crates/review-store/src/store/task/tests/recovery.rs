//! Abandoned Attempts (ADR-0200): a lost writer's Attempt and a host-cancelled one leave the
//! Attempt limits, are charged only what they were observed to use, and are bounded by the
//! node's recovery allowance.
use super::*;
use crate::store::task::execution::{PreparedTaskAttempt, TaskAbandonCause};
use review_core::task::execution::*;

const NODE: &str = "root.nodes.write";

fn start(f: &mut Fixture, lease: &TaskLease, label: &str) -> PreparedTaskAttempt {
    let context = f.cas.put_json(&json!({ "context": label })).unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, lease, NODE, &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, lease, &attempt, &f.authority)
        .unwrap();
    attempt
}

/// The lease expires and `writer` takes it, as `af task run` does after its predecessor died.
/// Real time passes, so the successor can dispatch again under the plan's deadline.
fn take_over(f: &mut Fixture, previous: &TaskLease, writer: &str) -> TaskLease {
    let expiry = f.state().lease_until;
    while now().unwrap() <= expiry {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
    next
}

#[test]
fn a_lost_writers_attempt_is_charged_its_observed_usage_and_dispatched_again_once() {
    let mut f = Fixture::new(false).with_execution_graph();
    // A lease short enough to expire within the test, long enough to plan under any load.
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
        .unwrap();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let lost = start(&mut f, &lease, "first");
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
    let second = take_over(&mut f, &lease, "writer-2");

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
    let cancelled = start(&mut f, &second, "second");
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
}

/// Below its reservation, abandoned work states how its usage is known: a report, or the
/// unknown-usage marker.
#[test]
fn abandoned_work_below_its_reservation_needs_a_usage_report_or_the_marker() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let attempt = start(&mut f, &lease, "unreported");
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
}
