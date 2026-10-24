//! `af/TaskCheckReport@1` and `TaskGateRetried@1` (ADR-0199): a fully populated value of each
//! is valid against its schema and the Rust validator and round-trips; every shape either side
//! refuses the other refuses too. The check report is the receipt companion that records the
//! host's load and CPUs around a check, its test threads, its steps by stage and its flaky
//! tests.

use super::*;
use review_core::task::check_report::*;
use review_core::task::gate_retry::*;
use std::collections::BTreeMap;

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn report() -> TaskCheckReportV1 {
    TaskCheckReportV1 {
        receipt_id: digest('a'),
        checks: BTreeMap::from([
            (
                "kernel".to_string(),
                TaskCheckRunV1 {
                    host: TaskCheckHostV1 {
                        cpus_start: 14,
                        cpus_end: 12,
                        load_1m_start: Some("22.44".into()),
                        load_1m_end: Some("9.79".into()),
                    },
                    report: CheckReportStateV1::Admitted,
                    refusal: None,
                    steps: vec![
                        CheckStepV1 {
                            label: "performance-kernel-harness".into(),
                            stage: CheckStageV1::Preflight,
                            exit_code: 0,
                            command: vec!["python3".into(), "scripts/x.py".into(), String::new()],
                            fix: Some(
                                "python3 -I scripts/performance/cli.py harness-digest --write"
                                    .into(),
                            ),
                        },
                        CheckStepV1 {
                            label: "test".into(),
                            stage: CheckStageV1::Tests,
                            exit_code: 100,
                            command: vec!["python3".into()],
                            fix: None,
                        },
                    ],
                    tests: Some(CheckTestsV1 {
                        test_threads: 7,
                        failed_count: 1,
                        failed: vec!["af::it task_heavy::x".into()],
                        flaky: vec![FlakyTestV1 {
                            name: "af::it task_starters::heavy_y".into(),
                            parallel: TestOutcomeV1::Failed,
                            isolated: TestOutcomeV1::Passed,
                        }],
                        rerun: CheckRerunV1::Isolated,
                    }),
                },
            ),
            (
                "markdownlint".to_string(),
                TaskCheckRunV1 {
                    host: TaskCheckHostV1 {
                        cpus_start: 1,
                        cpus_end: 1,
                        load_1m_start: None,
                        load_1m_end: None,
                    },
                    report: CheckReportStateV1::Refused,
                    refusal: Some("line 1 is not an af.check-report/1 record".into()),
                    steps: Vec::new(),
                    tests: None,
                },
            ),
        ]),
    }
}

fn report_accepted(value: &Value) -> bool {
    serde_json::from_value::<TaskCheckReportV1>(value.clone())
        .is_ok_and(|report| report.validate().is_ok())
}

#[test]
fn a_check_report_records_host_load_cpus_threads_stages_and_flaky_tests() {
    let report = report();
    report.validate().unwrap();
    let value = serde_json::to_value(&report).unwrap();
    assert_valid("task-check-report-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskCheckReportV1>(value.clone()).unwrap(),
        report
    );
    let kernel = &value["checks"]["kernel"];
    assert_eq!(kernel["host"]["cpus_start"], 14);
    assert_eq!(kernel["host"]["cpus_end"], 12);
    assert_eq!(kernel["host"]["load_1m_start"], "22.44");
    assert_eq!(kernel["host"]["load_1m_end"], "9.79");
    assert_eq!(kernel["tests"]["test_threads"], 7);
    assert_eq!(kernel["steps"][0]["stage"], "preflight");
    assert_eq!(kernel["tests"]["flaky"][0]["parallel"], "failed");
    assert_eq!(kernel["tests"]["flaky"][0]["isolated"], "passed");
    assert_eq!(
        report.checks["kernel"].stopped_at().unwrap().label,
        "test",
        "the first step that did not exit 0"
    );
}

#[test]
fn check_report_shapes_either_side_refuses_the_other_refuses() {
    let base = serde_json::to_value(report()).unwrap();
    let mutate = |edit: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        edit(&mut value);
        value
    };
    for (why, value) in [
        ("unknown field", mutate(&|v| v["verdict"] = json!("passed"))),
        (
            "no receipt",
            mutate(&|v| v["receipt_id"] = json!("sha256:short")),
        ),
        ("no checks", mutate(&|v| v["checks"] = json!({}))),
        (
            "an invalid check name",
            mutate(&|v| v["checks"]["-kernel"] = v["checks"]["kernel"].clone()),
        ),
        (
            "a load without two decimals",
            mutate(&|v| v["checks"]["kernel"]["host"]["load_1m_start"] = json!("22.4")),
        ),
        (
            "a negative load",
            mutate(&|v| v["checks"]["kernel"]["host"]["load_1m_end"] = json!("-1.00")),
        ),
        (
            "no CPUs at the start",
            mutate(&|v| v["checks"]["kernel"]["host"]["cpus_start"] = json!(0)),
        ),
        (
            "no CPU count at the end",
            mutate(&|v| {
                v["checks"]["kernel"]["host"]
                    .as_object_mut()
                    .unwrap()
                    .remove("cpus_end");
            }),
        ),
        (
            "an unknown stage",
            mutate(&|v| v["checks"]["kernel"]["steps"][0]["stage"] = json!("deploy")),
        ),
        (
            "a fix a terminal would act on",
            mutate(&|v| v["checks"]["kernel"]["steps"][0]["fix"] = json!("run \u{1b}[2J")),
        ),
        (
            "an empty program",
            mutate(&|v| v["checks"]["kernel"]["steps"][1]["command"] = json!([""])),
        ),
        (
            "no test threads",
            mutate(&|v| v["checks"]["kernel"]["tests"]["test_threads"] = json!(0)),
        ),
        (
            "a flaky test that passed in parallel",
            mutate(&|v| v["checks"]["kernel"]["tests"]["flaky"][0]["parallel"] = json!("passed")),
        ),
        (
            "more than 25 flaky tests",
            mutate(&|v| {
                let one = v["checks"]["kernel"]["tests"]["flaky"][0].clone();
                v["checks"]["kernel"]["tests"]["flaky"] = json!(vec![one; 26]);
            }),
        ),
        (
            "an unknown rerun",
            mutate(&|v| v["checks"]["kernel"]["tests"]["rerun"] = json!("retried")),
        ),
        (
            "a refusal without its reason",
            mutate(&|v| {
                v["checks"]["markdownlint"]
                    .as_object_mut()
                    .unwrap()
                    .remove("refusal");
            }),
        ),
        (
            "a reason beside an admitted report",
            mutate(&|v| v["checks"]["kernel"]["refusal"] = json!("why")),
        ),
        (
            "steps of a refused report",
            mutate(&|v| {
                v["checks"]["markdownlint"]["steps"] = v["checks"]["kernel"]["steps"].clone();
            }),
        ),
        (
            "an absent check that reports tests",
            mutate(&|v| {
                v["checks"]["markdownlint"]["report"] = json!("absent");
                v["checks"]["markdownlint"]
                    .as_object_mut()
                    .unwrap()
                    .remove("refusal");
                v["checks"]["markdownlint"]["tests"] = v["checks"]["kernel"]["tests"].clone();
            }),
        ),
    ] {
        assert_invalid("task-check-report-v1.json", &value, why);
        assert!(!report_accepted(&value), "Rust accepted {why}");
    }
}

fn retry() -> TaskGateRetriedV1 {
    TaskGateRetriedV1 {
        schema: TASK_GATE_RETRIED_V1.into(),
        writer: "cli-4242".into(),
        epoch: 3,
        now_unix_ms: 1_760_000_000_000,
        result_id: digest('b'),
        gates: vec![RetriedGateV1 {
            node: "root.nodes.gate".into(),
            output_id: digest('c'),
        }],
    }
}

#[test]
fn a_gate_retry_event_names_its_lease_result_and_failed_gates() {
    let retry = retry();
    retry.validate().unwrap();
    let value = serde_json::to_value(&retry).unwrap();
    assert_valid("task-gate-retried-v1.json", &value);
    review_core::event::validate_event_payload(review_core::EventType::TaskGateRetriedV1, &value)
        .unwrap();
    let event = json!({
        "event_id": "01jd8m4qz9k7v3n2p6r8t0w202",
        "run_id": "task:retry",
        "sequence": 12,
        "type": "TaskGateRetried@1",
        "occurred_at": "2026-10-23T00:00:00Z",
        "artifact_refs": retry.artifact_refs(),
        "payload": value,
    });
    assert_valid("run-event-v1.json", &event);
    let mutate = |edit: &dyn Fn(&mut Value)| {
        let mut changed = serde_json::to_value(&retry).unwrap();
        edit(&mut changed);
        changed
    };
    for (why, value) in [
        (
            "another schema",
            mutate(&|v| v["schema"] = json!("af/TaskGateRetried@2")),
        ),
        ("no epoch", mutate(&|v| v["epoch"] = json!(0))),
        ("no gates", mutate(&|v| v["gates"] = json!([]))),
        ("a plan", mutate(&|v| v["plan_id"] = json!(digest('d')))),
        (
            "an output that is no digest",
            mutate(&|v| v["gates"][0]["output_id"] = json!("output")),
        ),
        (
            "an empty node",
            mutate(&|v| v["gates"][0]["node"] = json!("")),
        ),
    ] {
        assert_invalid("task-gate-retried-v1.json", &value, why);
        assert!(
            serde_json::from_value::<TaskGateRetriedV1>(value.clone())
                .map_or(true, |retry| retry.validate().is_err()),
            "Rust accepted {why}"
        );
    }
}

/// `af task show --json` (`af/task-inspection@13`) carries a gate's evidence under `gate`
/// beside its summary: each check node's latest check report, as recorded, and every gate
/// retry of the Task's log (here a retry whose writer was lost before its check finished).
/// Both are optional, never empty, and hold only those two shapes.
#[test]
fn an_inspection_carries_its_gate_s_check_reports_and_retries() {
    let report = serde_json::to_value(report()).unwrap();
    let retry = serde_json::to_value(retry()).unwrap();
    let value = json!({
        "schema":"af/task-inspection@13", "task_id":"pagination-cli", "revision_id":digest('1'),
        "phase":{"kind":"running"},
        "state":{"phase":"abandoned","outcome":"cancelled","cause":"writer_lost",
            "next_action":"af task run pagination-cli"},
        "clock":{"wall_ms":60000,"started_unix_ms":5,"deadline_unix_ms":60005},
        "plan_id":digest('3'), "chargeable_tokens":"0", "attempts":3,
        "gate":{"status":"failed","failed_tests":["af::it task_heavy::x"],
            "flaky":["af::it task_starters::heavy_y"],
            "check_reports":[{"node":"root.nodes.gate","artifact_id":digest('4'),
                "record":report}],
            "retries":[retry]},
        "history":[], "execution_records":[], "run_reports":[]
    });
    assert_valid("task-inspection-v13.json", &value);
    let mut summary_only = value.clone();
    let gate = summary_only["gate"].as_object_mut().unwrap();
    gate.remove("check_reports");
    gate.remove("retries");
    assert_valid("task-inspection-v13.json", &summary_only);
    for (pointer, replacement, why) in [
        ("/gate/check_reports", json!([]), "an empty report list"),
        ("/gate/retries", json!([]), "an empty retry list"),
        (
            "/gate/check_reports/0/artifact_id",
            json!("report"),
            "a report that is no digest",
        ),
        (
            "/gate/check_reports/0/record/checks/kernel/host/load_1m_start",
            json!("22.4"),
            "a load without two decimals",
        ),
        (
            "/gate/retries/0/schema",
            json!("af/TaskGateRetried@2"),
            "a retry of another schema",
        ),
        (
            "/gate/retries/0/gates",
            json!([]),
            "a retry that names no gate",
        ),
        (
            "/gate/status",
            json!("flaky"),
            "a gate status outside the three",
        ),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-inspection-v13.json", &changed, why);
    }
    for field in ["node", "record"] {
        let mut changed = value.clone();
        changed["gate"]["check_reports"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_invalid("task-inspection-v13.json", &changed, field);
    }
    let mut extra = value;
    extra["gate"]["receipts"] = json!([]);
    assert_invalid(
        "task-inspection-v13.json",
        &extra,
        "a gate field the inspection does not declare",
    );
}
