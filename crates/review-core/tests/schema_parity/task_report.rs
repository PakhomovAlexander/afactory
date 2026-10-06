//! `af/task-report@1` (ADR-0142): the Rust document and the published schema agree, the
//! checked-in fixture a test Store printed is valid, and the closed shape refuses a Provider
//! label, a principal, a null for an unknown figure and Attempts on a collected Task.
use super::{assert_invalid, assert_valid, validator, workspace_root};
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
        unknown_usage: 0,
    };
    let task = TaskReportEntryV1 {
        round: 0,
        task_id: "implement-x".into(),
        kind: "implement".into(),
        pipeline: Some("builtin/implement@1.2.0".into()),
        outcome: "changes_requested".into(),
        collected: false,
        review_rounds: Some(2),
        findings: Some(TaskReportFindingsV1 {
            blocker: Some(1),
            major: Some(6),
            minor: Some(1),
            review_ran: true,
            gate_failed: false,
            failed_reviewers: 1,
        }),
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
            unknown_usage: 0,
            unknown_usage_causes: Vec::new(),
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
        round: 0,
        task_id: "verify-x".into(),
        kind: "review".into(),
        pipeline: None,
        outcome: "pass".into(),
        collected: true,
        review_rounds: None,
        findings: None,
        runs: 1,
        attempts: None,
        chargeable_tokens: DecimalU128::from(5),
        wall_ms: 10,
        active_ms: 10,
        nodes: None,
    };
    let step = |stage, role: &str, nodes: &[&str], worker, checks: &[&str]| TaskReportStepV1 {
        stage,
        role: role.into(),
        nodes: nodes.iter().map(|n| n.to_string()).collect(),
        worker,
        checks: checks.iter().map(|c| c.to_string()).collect(),
    };
    let pipeline = TaskReportPipelineV1 {
        name: "builtin/implement".into(),
        version: "1.2.0".into(),
        steps: vec![
            step(
                1,
                "implement",
                &["root.nodes.implement"],
                Some(TaskReportWorkerV1::Model {
                    provider_kind: "codex".into(),
                    model: "gpt-6-sol".into(),
                    effort: "high".into(),
                }),
                &[],
            ),
            step(
                2,
                "gate",
                &["root.nodes.check"],
                None,
                &["lint", "pagination"],
            ),
            step(
                3,
                "review",
                &["root.nodes.bugs", "root.nodes.correctness"],
                Some(TaskReportWorkerV1::Model {
                    provider_kind: "claude".into(),
                    model: "claude-opus-5-5".into(),
                    effort: "high".into(),
                }),
                &[],
            ),
            step(
                4,
                "evaluate",
                &["root.nodes.evaluate"],
                Some(TaskReportWorkerV1::Command {}),
                &[],
            ),
        ],
    };
    TaskReportV1::new(vec![pipeline], vec![task, collected]).unwrap()
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
    invalid(
        &|v| v.pointer_mut(worker).unwrap()["model"] = json!("/Users/fixture/.codex/auth.json"),
        "a path as the model",
    );
    invalid(&|v| v["tasks"] = json!([]), "no Task");
    invalid(
        &|v| {
            v.as_object_mut().unwrap().remove("pipelines");
        },
        "no pipelines",
    );
    invalid(
        &|v| {
            v["tasks"][0].as_object_mut().unwrap().remove("round");
        },
        "a Task without its round",
    );
    invalid(&|v| v["tasks"][0]["round"] = json!(0), "round 0");
    let review = "/pipelines/0/steps/2";
    invalid(
        &|v| v.pointer_mut(review).unwrap()["nodes"] = json!([]),
        "a step without nodes",
    );
    invalid(
        &|v| v.pointer_mut(review).unwrap()["provider"] = json!("codex-personal"),
        "a Provider label on a step",
    );
    invalid(
        &|v| v.pointer_mut(review).unwrap()["worker"]["model"] = json!("C:/secrets/key"),
        "a path as a step's model",
    );
    invalid(
        &|v| v.pointer_mut(review).unwrap()["checks"] = json!(["fmt"]),
        "checks on a Worker step",
    );
    invalid(
        &|v| v.pointer_mut("/pipelines/0/steps/1").unwrap()["checks"] = json!([]),
        "a gate without checks",
    );
    invalid(
        &|v| v.pointer_mut("/pipelines/0/steps/1").unwrap()["worker"] = json!({"kind": "command"}),
        "a gate with a Worker",
    );
    invalid(
        &|v| v["pipelines"][0]["steps"][0]["stage"] = json!(0),
        "stage 0",
    );
    invalid(
        &|v| v["tasks"][0]["findings"]["review_ran"] = json!(false),
        "findings without a review that ran",
    );
    invalid(
        &|v| v["tasks"][0]["findings"]["gate_failed"] = json!(true),
        "a failed gate beside a review that ran",
    );
    invalid(
        &|v| v["tasks"][0]["findings"]["critical"] = json!(1),
        "a severity the reduce step does not record",
    );
    invalid(
        &|v| v["tasks"][1]["findings"] = v["tasks"][0]["findings"].clone(),
        "findings on a collected Task",
    );
    invalid(
        &|v| v["tasks"][0]["findings"] = Value::Null,
        "null findings",
    );
    invalid(
        &|v| v["tasks"][0]["findings"]["minor"] = Value::Null,
        "null for an unknown count",
    );
    invalid(
        &|v| {
            v["tasks"][0]["findings"]
                .as_object_mut()
                .unwrap()
                .remove("minor");
        },
        "one count unknown beside known ones",
    );
    let unknown_counts = |v: &mut Value| {
        let findings = v["tasks"][0]["findings"].as_object_mut().unwrap();
        for severity in ["blocker", "major", "minor"] {
            findings.remove(severity);
        }
    };
    invalid(
        &|v| {
            unknown_counts(v);
            v["tasks"][0]["findings"]["review_ran"] = json!(false);
            v["tasks"][0]["findings"]["failed_reviewers"] = json!(0);
        },
        "unknown counts without a review that ran",
    );
    // An incomplete round: the review ran, its counts are unknown.
    let mut incomplete = value.clone();
    unknown_counts(&mut incomplete);
    assert_valid("task-report-v1.json", &incomplete);
    let parsed: TaskReportV1 = serde_json::from_value(incomplete.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.tasks[0].findings.as_ref().unwrap().major, None);
    assert_eq!(serde_json::to_value(&parsed).unwrap(), incomplete);
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

/// The schema's model pattern and `is_model_identity` draw the same line on the one table
/// both are held to, `TASK_REPORT_MODEL_CASES`, and at the length bound.
#[test]
fn the_model_pattern_and_the_rust_rule_agree() {
    let value = serde_json::to_value(report()).unwrap();
    let long = "m".repeat(TASK_REPORT_MODEL_MAX);
    let longer = "m".repeat(TASK_REPORT_MODEL_MAX + 1);
    let lengths = [(long.as_str(), true), (longer.as_str(), false)];
    for (model, identity) in TASK_REPORT_MODEL_CASES.into_iter().chain(lengths) {
        assert_eq!(is_model_identity(model), identity, "the table on {model:?}");
        let mut changed = value.clone();
        changed.pointer_mut("/tasks/0/nodes/0/worker").unwrap()["model"] = json!(model);
        assert_eq!(
            validator("task-report-v1.json").is_valid(&changed),
            is_model_identity(model),
            "the schema and the Rust rule disagree on {model:?}"
        );
        assert_eq!(
            serde_json::from_value::<TaskReportV1>(changed)
                .unwrap()
                .validate()
                .is_ok(),
            is_model_identity(model),
            "{model:?}"
        );
    }
}

/// ADR-0143: Attempts whose usage is unknown are counted on the Task's attempts, by cause, on
/// each node and in the totals; the counts must add up, and the schema knows only the closed
/// causes. The collected Task records no Attempts, so the totals count the other Task's.
#[test]
fn unknown_usage_counts_add_up_and_carry_closed_causes() {
    use review_core::task::usage::TaskUnknownUsageCauseV1 as Cause;
    let mut tasks = report().tasks;
    for task in &mut tasks {
        task.round = 0;
    }
    let attempts = tasks[0].attempts.as_mut().unwrap();
    attempts.unknown_usage = 2;
    attempts.unknown_usage_causes = vec![
        TaskReportUnknownUsageV1 {
            cause: Cause::Capacity,
            attempts: 1,
        },
        TaskReportUnknownUsageV1 {
            cause: Cause::LeaseExpired,
            attempts: 1,
        },
    ];
    tasks[0].nodes.as_mut().unwrap()[0].unknown_usage = 2;
    let pipelines = report().pipelines;
    let unknown = TaskReportV1::new(pipelines, tasks).unwrap();
    assert_eq!(unknown.totals.unknown_usage, 2);
    let value = serde_json::to_value(&unknown).unwrap();
    assert_valid("task-report-v1.json", &value);
    assert_eq!(
        value["tasks"][0]["attempts"]["unknown_usage_causes"],
        json!([{"cause":"capacity","attempts":1},{"cause":"lease_expired","attempts":1}])
    );
    assert_eq!(
        serde_json::from_value::<TaskReportV1>(value.clone()).unwrap(),
        unknown
    );
    // With every usage known the count is zero and the causes are absent.
    let known = serde_json::to_value(report()).unwrap();
    assert_eq!(known["tasks"][0]["attempts"]["unknown_usage"], 0);
    assert!(
        known["tasks"][0]["attempts"]
            .get("unknown_usage_causes")
            .is_none()
    );

    let refused = |edit: &dyn Fn(&mut Value), why: &str| {
        let mut changed = value.clone();
        edit(&mut changed);
        assert!(
            serde_json::from_value::<TaskReportV1>(changed)
                .map_err(|e| e.to_string())
                .and_then(|report| report.validate())
                .is_err(),
            "{why}"
        );
    };
    refused(
        &|v| v["tasks"][0]["attempts"]["unknown_usage_causes"][1]["attempts"] = json!(2),
        "causes that do not add up",
    );
    refused(
        &|v| v["tasks"][0]["nodes"][0]["unknown_usage"] = json!(1),
        "nodes that do not add up",
    );
    refused(
        &|v| v["totals"]["unknown_usage"] = json!(0),
        "totals that are not the sum",
    );
    refused(
        &|v| {
            let causes = v["tasks"][0]["attempts"]["unknown_usage_causes"].clone();
            v["tasks"][0]["attempts"]["unknown_usage_causes"] = json!([causes[1], causes[0]]);
        },
        "causes out of order",
    );
    for (edit, why) in [
        (
            json!({"cause":"estimate","attempts":1}),
            "a cause outside the closed set",
        ),
        (json!({"cause":"capacity","attempts":0}), "an empty cause"),
    ] {
        let mut changed = value.clone();
        changed["tasks"][0]["attempts"]["unknown_usage_causes"][0] = edit;
        assert_invalid("task-report-v1.json", &changed, why);
    }
    let mut missing = value.clone();
    missing["totals"]
        .as_object_mut()
        .unwrap()
        .remove("unknown_usage");
    assert_invalid(
        "task-report-v1.json",
        &missing,
        "the totals always carry the count",
    );
}
