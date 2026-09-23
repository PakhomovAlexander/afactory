use review_core::task::report::*;

use super::*;

#[test]
fn task_run_reports_and_pre_attempt_diagnostics_keep_closed_bounded_contracts() {
    let id = format!("sha256:{}", "a".repeat(64));
    let value = json!({"task_revision_id":id,"plan_id":id,"through_sequence":9,"nodes":[
        {"node":"root.inputs","outcome":{"kind":"completed","output_id":id}},
        {"node":"root.nodes.worker","outcome":{"kind":"failed","diagnostic_id":id,"class":"execution"}},
        {"node":"root.nodes.verifier","outcome":{"kind":"suppressed","reason":"upstream_missing"}}
    ]});
    assert_valid("task-run-report-v2.json", &value);
    let typed: TaskRunReportV1 = serde_json::from_value(value.clone()).unwrap();
    typed.validate().unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), value);
    for (pointer, invalid) in [
        ("/through_sequence", json!(0)),
        ("/through_sequence", json!(9_007_199_254_740_992_u64)),
        ("/plan_id", json!("main")),
        ("/nodes/0/node", json!("root..inputs")),
        ("/nodes/0/outcome/output_id", json!(null)),
        ("/nodes/1/outcome/class", json!("pass")),
        ("/nodes/2/outcome/reason", json!("clean")),
        ("/nodes", json!([])),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = invalid;
        assert_invalid("task-run-report-v2.json", &changed, pointer);
        assert!(
            serde_json::from_value::<TaskRunReportV1>(changed)
                .map(|v| v.validate().is_err())
                .unwrap_or(true)
        );
    }
    let mut duplicate: TaskRunReportV1 = serde_json::from_value(value).unwrap();
    duplicate.nodes.push(duplicate.nodes[0].clone());
    assert!(duplicate.validate().is_err());
    for length in [65_536, 65_537] {
        let diagnostic = TaskDiagnosticV1::capture(&"é".repeat(length));
        diagnostic.validate().unwrap();
        assert_eq!(diagnostic.truncated, length > 65_536);
        assert_valid(
            "task-diagnostic-v1.json",
            &serde_json::to_value(diagnostic).unwrap(),
        );
    }
    let oversized = json!({"message":"x".repeat(65_537),"truncated":false});
    assert_invalid(
        "task-diagnostic-v1.json",
        &oversized,
        "unbounded diagnostic",
    );
    assert!(
        serde_json::from_value::<TaskDiagnosticV1>(oversized)
            .unwrap()
            .validate()
            .is_err()
    );
    let transition = json!({"writer":"host","epoch":1,"now_unix_ms":1,"change":{"kind":"run_reported","report_id":id}});
    assert_valid("task-transition-v5.json", &transition);
    serde_json::from_value::<review_core::task::event::TaskTransitionV1>(transition)
        .unwrap()
        .validate()
        .unwrap();
}
