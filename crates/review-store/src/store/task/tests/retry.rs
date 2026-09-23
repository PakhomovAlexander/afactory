use super::*;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};

fn running() -> (Fixture, TaskLease, String, String) {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation = f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"purpose":"retry eligibility fixture"}))
        .unwrap();
    (f, lease, invocation, context)
}

fn refuse_both(f: &mut Fixture, lease: &TaskLease, context: &str, message: &str) {
    let sequence = f.state().next_sequence;
    let error = f
        .store
        .reserve_task_attempt(&f.cas, lease, "root.nodes.write", &f.authority)
        .unwrap_err();
    assert!(error.to_string().contains(message), "{error}");
    let error = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, lease, "root.nodes.write", context, &f.authority)
        .unwrap_err();
    assert!(error.to_string().contains(message), "{error}");
    assert_eq!(f.state().next_sequence, sequence);
}

#[test]
fn pending_invocation_cannot_reserve_another_attempt_through_either_api() {
    let (mut f, lease, _, context) = running();
    let reserved = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    refuse_both(&mut f, &lease, &context, "pending Attempt");
    assert_eq!(f.state().execution.unwrap().budget.reserved_tokens(), 10);
    f.store
        .release_reserved_task_attempt(&f.cas, &lease, &reserved, "never dispatched")
        .unwrap();
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .is_ok()
    );
}

#[test]
fn captured_retry_policy_checks_durable_failures_before_any_new_reservation() {
    let (mut f, lease, _, context) = running();
    f.authority.retry_allowed = false;
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic_id = f
        .cas
        .put_json(&json!({"failure":"nonretryable fixture transport"}))
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
    refuse_both(&mut f, &lease, &context, "policy refuses retry");
    let budget = f.state().execution.unwrap().budget;
    assert_eq!(budget.committed_tokens(), 7);
    assert_eq!(budget.reserved_tokens(), 0);
    assert_eq!(budget.begun_attempts(), 1);
}

#[test]
fn selected_work_recovers_publication_instead_of_reserving_another_paid_attempt() {
    let (mut f, lease, invocation, context) = running();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let output_id = f.execution_output(&invocation, attempt.id());
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: output_id.clone(),
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    refuse_both(&mut f, &lease, &context, "recover its publication");
    f.store
        .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
        .unwrap();
    let budget = f.state().execution.unwrap().budget;
    assert_eq!(budget.committed_tokens(), 7);
    assert_eq!(budget.begun_attempts(), 1);
}
