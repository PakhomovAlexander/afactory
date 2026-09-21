//! Store boundaries use the existing admitted Task compiler fixture. Successful Review
//! selection/commit uses real captured Review fixtures in review-pipeline integration tests.
use super::*;
use review_core::task::event::TaskTransitionV3;
use review_core::task::execution::*;
use review_core::task::report::*;
use review_core::task::review_integration::*;
use review_graph::task::{CompiledReviewIntegrationV1, CompiledTask};

fn fixture() -> Fixture {
    let mut f = Fixture::new(false).with_execution_graph();
    let sequence = TaskReviewCheckSequencePolicyV1 {
        authority_policy_id: f.plan.authority.policy_id.clone(),
        pipeline_policy_id: f.plan.pipeline_id.clone(),
        gate_execution_policy_id: f.plan.pipeline_id.clone(),
        ordered_check_names: vec!["test".into()],
        check_timeout_ms: 1000,
    };
    let (id, frame) = f
        .cas
        .put_artifact(
            TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1,
            producer(),
            sequence.artifact_refs(),
            None,
            serde_json::to_value(&sequence).unwrap(),
        )
        .unwrap();
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.review_integration = Some(CompiledReviewIntegrationV1 {
        node: "root.integration_checks".into(),
        sequence_policy_id: id.clone(),
        allowance: review_attempt::task_budget::NodeAllowance {
            tokens_per_attempt: 0,
            wall_ms_per_attempt: 1000,
            max_attempts: 1,
            verification_attempts: 0,
        },
    });
    f.plan.dependencies.insert(
        "af/review-integration-checks".into(),
        PlanDependencyV1 {
            name: "af/review-integration-checks".into(),
            artifact_id: id,
            content_digest: frame.content_id,
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
    f
}
#[test]
fn dormant_sequence_has_original_allowance_but_no_store_dispatch_authority() {
    let mut f = fixture();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let before = f.state();
    let execution = before.execution.as_ref().unwrap();
    assert!(
        !execution
            .graph
            .nodes
            .contains_key("root.integration_checks")
    );
    assert!(
        !execution
            .graph
            .order
            .iter()
            .any(|n| n == "root.integration_checks")
    );
    // The account already exists inside the original shared budget. This clone is never
    // installed or dispatched; activation cannot add a fresh account or enlarge the Task.
    let mut budget = execution.budget.clone();
    assert!(
        budget
            .prepare("root.integration_checks", now().unwrap())
            .is_ok()
    );
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(
        execution.budget.remaining_limits().deadline_unix_ms,
        f.revision.limits.deadline_unix_ms
    );
    let invocation = f
        .cas
        .put_artifact(
            TASK_INVOCATION_V1,
            producer(),
            vec![f.plan_id.clone()],
            None,
            serde_json::to_value(TaskInvocationV1 {
                plan_id: f.plan_id.clone(),
                node: "root.integration_checks".into(),
                inputs: BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    assert!(
        f.store
            .record_task_invocation(&f.cas, &lease, &invocation, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("Dormant Integration")
    );
    let context = f.cas.put_json(&json!({"fixture":"context"})).unwrap();
    assert!(
        f.store
            .reserve_and_bind_task_attempt(
                &f.cas,
                &lease,
                "root.integration_checks",
                &context,
                &f.authority
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before.next_sequence);
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(
        f.state().execution.unwrap().budget.remaining_limits(),
        execution.budget.remaining_limits()
    );
}
#[test]
fn ordinary_report_keeps_round_membership_and_phase_report_requires_activation() {
    let mut f = fixture();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let state = f.state();
    let execution = state.execution.as_ref().unwrap();
    let diagnostic = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(TaskDiagnosticV1::capture("fixture did not dispatch")).unwrap(),
        )
        .unwrap()
        .0;
    let outcome = TaskNodeOutcomeV1::Failed {
        diagnostic_id: diagnostic,
        class: TaskFailureClassV1::Execution,
    };
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        nodes: execution
            .graph
            .order
            .iter()
            .map(|n| TaskNodeReportV1 {
                node: n.clone(),
                outcome: execution.outputs.get(n).map_or_else(
                    || outcome.clone(),
                    |(id, _)| TaskNodeOutcomeV1::Completed {
                        output_id: id.clone(),
                    },
                ),
            })
            .collect(),
    };
    let phase_report = TaskRunReportV2 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        phase_id: f.plan.compiled_graph_id.clone(),
        nodes: vec![TaskNodeReportV1 {
            node: "root.integration_checks".into(),
            outcome: outcome.clone(),
        }],
    };
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V2,
            producer(),
            vec![],
            None,
            serde_json::to_value(phase_report).unwrap(),
        )
        .unwrap()
        .0;
    assert!(f.store.record_task_run_report(&f.cas, &lease, &id).is_err());
    let mut inflated = report.clone();
    inflated.nodes.push(TaskNodeReportV1 {
        node: "root.integration_checks".into(),
        outcome,
    });
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(inflated).unwrap(),
        )
        .unwrap()
        .0;
    assert!(f.store.record_task_run_report(&f.cas, &lease, &id).is_err());
    assert_eq!(f.state().next_sequence, state.next_sequence);
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store.record_task_run_report(&f.cas, &lease, &id).unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(f.state().run_reports, vec![id]);
}
#[test]
fn ordinary_append_cannot_forge_phase_activation_or_completion() {
    let mut f = fixture();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let before = f.state().next_sequence;
    for change in [
        TaskChangeV1::ReviewIntegrationSelected {
            phase_id: f.plan_id.clone(),
        },
        TaskChangeV1::ReviewIntegrationFinished {
            phase_id: f.plan_id.clone(),
            report_id: f.revision_id.clone(),
            integration_committed_event_id: None,
        },
    ] {
        let transition = TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now().unwrap(),
            change,
        };
        let payload =
            serde_json::to_value(TaskTransitionV3::from_integration(&transition).unwrap()).unwrap();
        let error = f
            .store
            .append(
                &task_run_id(&lease.task_id).unwrap(),
                &f.cas,
                NewEvent::new(EventType::TaskTransitionV3, payload),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("trusted Task entry point"),
            "{error}"
        );
        assert_eq!(f.state().next_sequence, before);
    }
}
