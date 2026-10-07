use super::*;
use crate::store::task::execution::{PreparedTaskAttempt, TaskAttemptAccounting};
use review_attempt::task_budget::TaskTokenScope;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use review_graph::task::CompiledTask;

#[test]
fn compiled_scope_survives_store_reopen_and_blocks_retry_without_another_event() {
    let mut f = Fixture::new(false).with_execution_graph();
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.token_scopes.insert(
        "review.round1.author".into(),
        TaskTokenScope {
            tokens: 15,
            members: BTreeSet::from(["root.nodes.write".into()]),
        },
    );
    let mut impossible = graph.clone();
    impossible
        .token_scopes
        .get_mut("review.round1.author")
        .unwrap()
        .tokens = 9;
    assert!(
        impossible
            .budget(f.revision.limits.clone())
            .err()
            .unwrap()
            .contains("mandatory work")
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
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"context":"captured scope fixture"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic_id = f
        .cas
        .put_json(&json!({"failure":"recorded transport failure"}))
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
                unknown_usage: None,
            },
            &f.authority,
        )
        .unwrap();
    let usage_id = f
        .cas
        .put_json(&json!({"provider":"late observed usage"}))
        .unwrap();
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
    f.store = EventStore::open(&f.path).unwrap();
    let before = f.state();
    let budget = &before.execution.as_ref().unwrap().budget;
    assert_eq!(budget.committed_tokens(), 9);
    assert_eq!(
        budget.scope_committed_tokens("review.round1.author"),
        Some(9)
    );
    assert_eq!(
        budget.scope_reserved_tokens("review.round1.author"),
        Some(0)
    );
    assert_eq!(budget.begun_attempts(), 1);
    let error = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap_err();
    assert!(
        error.to_string().contains("review.round1.author"),
        "{error}"
    );
    assert_eq!(f.state().next_sequence, before.next_sequence);
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 9);
}

/// ADR-0143: an Attempt recovered after its writer's lease expired, with no usage reported,
/// settles at zero with its usage unknown. Its reservation leaves the reserve whole, so a
/// reserve with room for one reservation still admits the next Attempt; under the reservation
/// charge it would have been exhausted. The recovered Attempt still counts as begun.
#[test]
fn a_recovered_attempt_without_usage_leaves_its_reserve_room_for_the_next_attempt() {
    let mut f = Fixture::new(false).with_execution_graph();
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.token_scopes.insert(
        "review.round1.verify".into(),
        TaskTokenScope {
            tokens: 15,
            members: BTreeSet::from(["root.nodes.write".into()]),
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
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"context":"captured reserve fixture"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let reserved = u128::from(attempt.reservation().tokens);
    assert!(
        2 * reserved > 15,
        "the reserve holds one reservation, not two"
    );
    // The machine sleeps: the writer's lease expires and a successor takes it.
    let time = f.state().lease_until + 1;
    f.store
        .append_task_transition(
            &f.cas,
            &lease.task_id,
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
    let state = f.state();
    let execution = state.execution.as_ref().unwrap();
    let budget = &execution.budget;
    assert_eq!(budget.committed_tokens(), 0);
    assert_eq!(
        budget.scope_committed_tokens("review.round1.verify"),
        Some(0)
    );
    assert_eq!(
        budget.scope_reserved_tokens("review.round1.verify"),
        Some(0)
    );
    assert_eq!(budget.begun_attempts(), 1);
    let recovered = execution
        .attempt_accounting()
        .into_iter()
        .find(|row| row.attempt_id == attempt.id())
        .unwrap();
    assert_eq!(recovered.charged_tokens, 0);
    assert_eq!(
        recovered.unknown_usage,
        Some(review_core::task::usage::TaskUnknownUsageCauseV1::LeaseExpired)
    );
    assert!(matches!(
        recovered.result,
        Some(TaskAttemptResultV1::Abandoned { .. })
    ));
    // The recorded settlement carries the marker, at zero and with no usage report.
    let settled = execution.attempt_accounting();
    assert!(settled.iter().all(|row| row.usage_id.is_none()));
    // The next Attempt fits the reserve; had the recovery charged the reservation, the
    // reserve's remaining tokens could not hold it.
    let mut after = budget.clone();
    let reservation = after
        .prepare("root.nodes.write", recovered.started_unix_ms.unwrap())
        .unwrap();
    assert_eq!(u128::from(reservation.tokens), reserved);
    assert_eq!(
        after.scope_reserved_tokens("review.round1.verify"),
        Some(reserved)
    );
}

/// A settlement may not claim unknown usage for an Attempt whose usage was observed, nor charge
/// an abandoned Attempt below its reservation without the marker.
#[test]
fn unknown_usage_is_refused_once_usage_was_observed() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"context":"observed usage fixture"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic_id = f.cas.put_json(&json!({"failure":"fixture"})).unwrap();
    let abandoned_below_reservation = TaskExecutionRecordV1::Settled {
        attempt_id: attempt.id().into(),
        charged_tokens: 0,
        result: TaskAttemptResultV1::Abandoned {
            diagnostic_id: diagnostic_id.clone(),
        },
        raw_artifact_ids: vec![],
        usage_id: None,
        unknown_usage: None,
    };
    assert!(
        f.store
            .settle_task_attempt(&f.cas, &lease, abandoned_below_reservation, &f.authority)
            .is_err()
    );
    let usage_id = f.cas.put_json(&json!({"provider":"observed"})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: 3,
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    let unknown = TaskExecutionRecordV1::Settled {
        attempt_id: attempt.id().into(),
        charged_tokens: 0,
        result: TaskAttemptResultV1::Failed {
            diagnostic_id,
            feedback_id: None,
        },
        raw_artifact_ids: vec![],
        usage_id: None,
        unknown_usage: Some(review_core::task::usage::TaskUnknownUsageV1 {
            cause: review_core::task::usage::TaskUnknownUsageCauseV1::Capacity,
        }),
    };
    let error = f
        .store
        .settle_task_attempt(&f.cas, &lease, unknown, &f.authority)
        .unwrap_err();
    assert!(error.to_string().contains("observed usage"), "{error}");
}

/// A started Attempt of the execution fixture, with the lease that owns it.
fn started_attempt(f: &mut Fixture) -> (TaskLease, PreparedTaskAttempt) {
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"context":"zero usage fixture"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    (lease, attempt)
}

fn observe_zero(f: &mut Fixture, lease: &TaskLease, attempt: &PreparedTaskAttempt) {
    let usage_id = f.cas.put_json(&json!({"provider":"zero"})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: 0,
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
}

fn settled_unknown(f: &Fixture, attempt: &PreparedTaskAttempt) -> TaskExecutionRecordV1 {
    TaskExecutionRecordV1::Settled {
        attempt_id: attempt.id().into(),
        charged_tokens: 0,
        result: TaskAttemptResultV1::Failed {
            diagnostic_id: f.cas.put_json(&json!({"failure":"capacity"})).unwrap(),
            feedback_id: None,
        },
        raw_artifact_ids: vec![],
        usage_id: None,
        unknown_usage: Some(review_core::task::usage::TaskUnknownUsageV1 {
            cause: review_core::task::usage::TaskUnknownUsageCauseV1::Capacity,
        }),
    }
}

fn accounting(f: &Fixture, attempt: &PreparedTaskAttempt) -> TaskAttemptAccounting {
    f.state()
        .execution
        .as_ref()
        .unwrap()
        .attempt_accounting()
        .into_iter()
        .find(|row| row.attempt_id == attempt.id())
        .unwrap()
}

/// A usage observation that reported 0 tokens is still a usage report: the Attempt's usage is
/// known, so it cannot then settle as unknown (ADR-0143).
#[test]
fn a_zero_token_observation_refuses_a_later_unknown_settlement() {
    let mut f = Fixture::new(false).with_execution_graph();
    let (lease, attempt) = started_attempt(&mut f);
    observe_zero(&mut f, &lease, &attempt);
    let unknown = settled_unknown(&f, &attempt);
    let error = f
        .store
        .settle_task_attempt(&f.cas, &lease, unknown, &f.authority)
        .unwrap_err();
    assert!(error.to_string().contains("observed usage"), "{error}");
}

/// An observation recorded after an unknown settlement makes the usage known, even when it
/// reported 0 tokens: the Attempt is known at charge 0, never shown as unknown, and replay
/// after reopening agrees.
#[test]
fn a_zero_token_observation_after_an_unknown_settlement_makes_usage_known() {
    let mut f = Fixture::new(false).with_execution_graph();
    let (lease, attempt) = started_attempt(&mut f);
    let unknown = settled_unknown(&f, &attempt);
    f.store
        .settle_task_attempt(&f.cas, &lease, unknown, &f.authority)
        .unwrap();
    assert_eq!(
        accounting(&f, &attempt).unknown_usage,
        Some(review_core::task::usage::TaskUnknownUsageCauseV1::Capacity)
    );
    observe_zero(&mut f, &lease, &attempt);
    f.store = EventStore::open(&f.path).unwrap();
    let row = accounting(&f, &attempt);
    assert_eq!(row.charged_tokens, 0);
    assert_eq!(row.unknown_usage, None);
}

/// Recovery of an Attempt whose lost writer recorded a 0-token observation settles it as
/// reported usage, not as unknown, which replay would refuse.
#[test]
fn recovery_after_a_zero_token_observation_settles_reported_usage() {
    let mut f = Fixture::new(false).with_execution_graph();
    let (lease, attempt) = started_attempt(&mut f);
    observe_zero(&mut f, &lease, &attempt);
    let time = f.state().lease_until + 1;
    f.store
        .append_task_transition(
            &f.cas,
            &lease.task_id,
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
    let row = accounting(&f, &attempt);
    assert_eq!(row.unknown_usage, None);
    assert_eq!(
        row.charged_tokens,
        u128::from(attempt.reservation().tokens),
        "reported usage of abandoned work keeps its reservation floor"
    );
}
