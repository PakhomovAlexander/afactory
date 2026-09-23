use review_core::task::report::*;

use super::*;

#[test]
fn run_report_cannot_forge_completion_revision_order_or_history_sequence() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let state = f.state();
    let execution = state.execution.unwrap();
    let diagnostic_id = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            json!({"message":"context refused","truncated":false}),
        )
        .unwrap()
        .0;
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        phase_id: None,
        nodes: execution
            .graph
            .order
            .iter()
            .map(|node| TaskNodeReportV1 {
                node: node.clone(),
                outcome: execution.outputs.get(node).map_or_else(
                    || TaskNodeOutcomeV1::Failed {
                        diagnostic_id: diagnostic_id.clone(),
                        class: TaskFailureClassV1::Execution,
                    },
                    |(id, _)| TaskNodeOutcomeV1::Completed {
                        output_id: id.clone(),
                    },
                ),
            })
            .collect(),
    };
    let root_output = execution.outputs.values().next().unwrap().0.clone();
    let mut variants = Vec::new();
    let mut bad = report.clone();
    bad.task_revision_id = f.plan_id.clone();
    variants.push(bad);
    let mut bad = report.clone();
    bad.plan_id = f.revision_id.clone();
    variants.push(bad);
    let mut bad = report.clone();
    bad.through_sequence -= 1;
    variants.push(bad);
    let mut bad = report.clone();
    bad.nodes.pop();
    variants.push(bad);
    let mut bad = report.clone();
    bad.nodes.reverse();
    variants.push(bad);
    let mut bad = report.clone();
    bad.nodes
        .iter_mut()
        .find(|n| n.node == "root.nodes.write")
        .unwrap()
        .outcome = TaskNodeOutcomeV1::Completed {
        output_id: root_output,
    };
    variants.push(bad);
    let mut bad = report.clone();
    bad.nodes
        .iter_mut()
        .find(|n| n.node == "root.nodes.write")
        .unwrap()
        .outcome = TaskNodeOutcomeV1::Failed {
        diagnostic_id: f.revision_id.clone(),
        class: TaskFailureClassV1::Execution,
    };
    variants.push(bad);
    let save = |value: &TaskRunReportV1| {
        f.cas
            .put_artifact(
                TASK_RUN_REPORT_V2,
                producer(),
                vec![],
                None,
                serde_json::to_value(value).unwrap(),
            )
            .unwrap()
            .0
    };
    for bad in variants {
        let id = save(&bad);
        assert!(f.store.record_task_run_report(&f.cas, &lease, &id).is_err());
        assert_eq!(
            f.store
                .task_projection(&f.cas, &f.revision.task_id)
                .unwrap()
                .unwrap()
                .next_sequence,
            state.next_sequence
        );
    }
    let id = save(&report);
    f.store.record_task_run_report(&f.cas, &lease, &id).unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.run_reports, vec![id]);
    assert_eq!(state.phase, TaskPhaseV1::Running {});
    let execution = state.execution.unwrap();
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(execution.budget.committed_tokens(), 0);
}
