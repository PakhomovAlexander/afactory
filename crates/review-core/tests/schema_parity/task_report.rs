//! `af/task-report@1` (ADR-0142): the Rust document and the published schema agree, the
//! checked-in fixture a test Store printed is valid, and the closed shape refuses a Provider
//! label, a principal, a null for an unknown figure and Attempts on a collected Task.
use super::{assert_invalid, assert_valid, workspace_root};
use review_core::task::task_report::*;
use review_core::task::usage::DecimalU128;
use serde_json::{Value, json};

fn report() -> TaskReportV1 {
    let node = |name: &str, worker, failed| TaskReportNodeV1 {
        node: name.into(),
        role: "implement".into(),
        worker,
        attempts: 2,
        failed_attempts: failed,
        tokens: DecimalU128::from(u128::MAX / 4),
        elapsed_ms: Some(9_000),
        checks: vec![
            TaskReportCheckV1 {
                name: "pagination".into(),
                status: TaskReportCheckStatusV1::Passed,
                elapsed_ms: Some(40),
            },
            TaskReportCheckV1 {
                name: "lint".into(),
                status: TaskReportCheckStatusV1::NotRun,
                elapsed_ms: None,
            },
        ],
    };
    let task = TaskReportEntryV1 {
        task_id: "implement-x".into(),
        kind: "implement".into(),
        pipeline: Some("builtin/implement@1.2.0".into()),
        outcome: "changes_requested".into(),
        collected: false,
        review_rounds: Some(2),
        runs: 3,
        attempts: Some(TaskReportAttemptsV1 {
            total: 4,
            failed: 2,
            failed_tokens: DecimalU128::from(30),
            failures: vec![
                TaskReportFailureV1 {
                    class: TaskReportFailureClassV1::ProviderFailure,
                    attempts: 1,
                    tokens: DecimalU128::from(10),
                },
                TaskReportFailureV1 {
                    class: TaskReportFailureClassV1::Abandoned,
                    attempts: 1,
                    tokens: DecimalU128::from(20),
                },
            ],
        }),
        chargeable_tokens: DecimalU128::from(u128::MAX / 2),
        wall_ms: 60_000,
        active_ms: 20_000,
        nodes: Some(vec![
            node(
                "root.nodes.implement",
                Some(TaskReportWorkerV1::Model {
                    provider_kind: "codex".into(),
                    model: "gpt-6-sol".into(),
                    effort: "high".into(),
                }),
                2,
            ),
            node(
                "root.nodes.evaluate",
                Some(TaskReportWorkerV1::Command {}),
                0,
            ),
            node("root.nodes.check", None, 0),
        ]),
    };
    let collected = TaskReportEntryV1 {
        task_id: "verify-x".into(),
        kind: "review".into(),
        pipeline: None,
        outcome: "pass".into(),
        collected: true,
        review_rounds: None,
        runs: 1,
        attempts: None,
        chargeable_tokens: DecimalU128::from(5),
        wall_ms: 10,
        active_ms: 10,
        nodes: None,
    };
    TaskReportV1::new(vec![task, collected]).unwrap()
}

#[test]
fn task_report_document_and_schema_agree_in_both_directions() {
    let report = report();
    let value = serde_json::to_value(&report).unwrap();
    assert_valid("task-report-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskReportV1>(value.clone()).unwrap(),
        report
    );
    assert!(
        value["totals"].get("attempts").is_none(),
        "a sum over an unknown part is absent"
    );

    let invalid = |edit: &dyn Fn(&mut Value), why: &str| {
        let mut changed = value.clone();
        edit(&mut changed);
        assert_invalid("task-report-v1.json", &changed, why);
        assert!(
            serde_json::from_value::<TaskReportV1>(changed.clone())
                .map_err(|e| e.to_string())
                .and_then(|r| r.validate())
                .is_err(),
            "the Rust document accepted what the schema refuses ({why}): {changed}"
        );
    };
    let worker = "/tasks/0/nodes/0/worker";
    invalid(
        &|v| v.pointer_mut(worker).unwrap()["provider"] = json!("codex-personal"),
        "a Provider registry label",
    );
    invalid(
        &|v| v.pointer_mut(worker).unwrap()["principal_id"] = json!("account"),
        "a Provider principal",
    );
    invalid(&|v| v["tasks"][0]["auth_dir"] = json!("/home/x"), "a path");
    invalid(
        &|v| v["tasks"][0]["review_rounds"] = Value::Null,
        "null for unknown",
    );
    invalid(
        &|v| v["tasks"][1]["attempts"] = v["tasks"][0]["attempts"].clone(),
        "Attempts on a collected Task",
    );
    invalid(
        &|v| v["tasks"][0]["attempts"]["failures"][0]["class"] = json!("timeout"),
        "an unknown failure class",
    );
    invalid(
        &|v| v["tasks"][0]["chargeable_tokens"] = json!(5),
        "tokens as a bounded number",
    );
    invalid(
        &|v| v["tasks"][0]["pipeline"] = json!("builtin/implement"),
        "a Pipeline without its version",
    );
    invalid(&|v| v["tasks"] = json!([]), "no Task");
    invalid(
        &|v| v["schema"] = json!("af/task-report@2"),
        "another schema",
    );
}

#[test]
fn the_checked_in_fixture_is_a_valid_document() {
    let path = workspace_root().join("fixtures/task-report/report.json");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_valid("task-report-v1.json", &value);
    serde_json::from_value::<TaskReportV1>(value)
        .unwrap()
        .validate()
        .unwrap();
}
