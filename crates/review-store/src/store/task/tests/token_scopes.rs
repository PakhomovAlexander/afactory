use super::*;
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
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
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
