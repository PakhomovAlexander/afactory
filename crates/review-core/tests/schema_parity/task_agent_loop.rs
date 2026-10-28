//! The agent loop's documents and records (ADR-0211): `af/task-findings@2`,
//! `af/task-materialized@1`, `af/task-diff@1`, `af/task-status@2`, `af/task-detached@3`,
//! `af/task-cancelled@2`, the `runs/` records `af/task-run-process@1` and
//! `af/task-cancel-request@1`, and the `af/TaskDeliveryProof@1` a verification Task's delivery
//! references. A typed value the Rust validator accepts satisfies its schema, both reject the
//! same broken shapes, and another version fails closed.
use super::{assert_invalid, assert_valid};
use review_core::task::agent_loop::*;
use review_core::task::delivery::TaskDeliveryProofV1;
use review_core::task::state::*;
use serde_json::{Value, json};

fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn verified() -> TaskStateV1 {
    TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None)
}

/// One change to a document, and why the contract must refuse it.
type Edit = (Box<dyn Fn(&mut Value)>, &'static str);

/// Each value validates in Rust and against `schema`; each edit breaks both.
fn both<T: serde::Serialize + serde::de::DeserializeOwned>(
    schema: &str,
    value: &T,
    validate: impl Fn(&T) -> Result<(), String>,
    edits: Vec<Edit>,
) {
    validate(value).unwrap();
    let document = serde_json::to_value(value).unwrap();
    assert_valid(schema, &document);
    let typed: T = serde_json::from_value(document.clone()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), document);
    for (edit, why) in edits {
        let mut changed = document.clone();
        edit(&mut changed);
        assert_invalid(schema, &changed, why);
        let rust = serde_json::from_value::<T>(changed)
            .map_err(|e| e.to_string())
            .and_then(|typed| validate(&typed));
        assert!(rust.is_err(), "the Rust contract accepted {why}");
    }
}

fn run_process() -> TaskRunProcessV1 {
    TaskRunProcessV1 {
        schema: TASK_RUN_PROCESS_V1.into(),
        task_id: "pagination-cli".into(),
        run: 2,
        pid: 812,
        host: "builder".into(),
        writer: "cli-812".into(),
        started_unix_ms: 1_792_718_994_633,
        detached: true,
        log: Some("/state/runs/pagination-cli-2.log".into()),
    }
}

fn cancel_request() -> TaskCancelRequestV1 {
    TaskCancelRequestV1 {
        schema: TASK_CANCEL_REQUEST_V1.into(),
        task_id: "pagination-cli".into(),
        run: 2,
        epoch: 3,
        writer: "cli-812".into(),
        pid: 812,
        host: "builder".into(),
        requester: TaskCancelRequesterV1 {
            user: "dev".into(),
            host: "builder".into(),
            pid: 900,
        },
        requested_unix_ms: 1_792_718_999_000,
    }
}

#[test]
fn the_run_records_name_their_process_lease_and_requester() {
    both(
        "task-run-process-v1.json",
        &run_process(),
        TaskRunProcessV1::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-run-process@0")),
                "another version",
            ),
            (
                Box::new(|v: &mut Value| v["log"] = Value::Null),
                "a detached run without its log",
            ),
            (Box::new(|v: &mut Value| v["pid"] = json!(0)), "pid 0"),
            (
                Box::new(|v: &mut Value| v["extra"] = json!(1)),
                "an undeclared field",
            ),
        ],
    );
    let mut foreground = run_process();
    foreground.detached = false;
    foreground.log = None;
    foreground.validate().unwrap();
    assert_valid(
        "task-run-process-v1.json",
        &serde_json::to_value(&foreground).unwrap(),
    );
    both(
        "task-cancel-request-v1.json",
        &cancel_request(),
        TaskCancelRequestV1::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["epoch"] = json!(0)),
                "a lease epoch of 0",
            ),
            (
                Box::new(|v: &mut Value| v["requester"]["user"] = json!("")),
                "no requester",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-cancel-request@2")),
                "another version",
            ),
        ],
    );
}

#[test]
fn findings_name_each_reviewers_findings_and_the_evaluators_verdict() {
    let finding = |severity, title: &str| TaskFindingV1 {
        finding_id: Some(id('1')),
        severity,
        status: Some("open".into()),
        confidence: Some(0.99),
        title: title.into(),
        file: Some("pagination.py".into()),
        line: Some(1),
        evidence: "A negative offset slices from the end.".into(),
        fix: Some("Raise ValueError for a negative offset.".into()),
    };
    let document = TaskFindingsV2 {
        schema: TASK_FINDINGS_V2.into(),
        task_id: "chain-base".into(),
        state: verified(),
        rounds: 1,
        round: Some(TaskFindingsRoundV1 {
            index: 1,
            round: 1,
            snapshot_id: id('2'),
            conclusion: review_core::task::review::ReviewConclusionV1::Pass,
            complete: true,
            missing_reviewers: vec![],
            counts: TaskFindingCountsV1 {
                blocker: 0,
                major: 1,
                minor: 1,
            },
            reviewers: vec![TaskReviewerFindingsV1 {
                reviewer: "bugs".into(),
                result_id: Some(id('3')),
                findings: vec![
                    finding(review_core::Severity::Major, "Reject negative offset"),
                    finding(review_core::Severity::Minor, "Name the default limit"),
                ],
            }],
        }),
        evaluation: Some(TaskFindingsEvaluationV1 {
            node: "evaluate".into(),
            artifact_id: id('4'),
            outcome: review_core::task::pipeline::ReceiptOutcomeV1::Passed,
            reason: "Verified on the sealed source".into(),
        }),
    };
    both(
        "task-findings-v2.json",
        &document,
        TaskFindingsV2::validate,
        vec![
            (
                Box::new(|v: &mut Value| {
                    v["round"]["reviewers"][0]["findings"][0]["severity"] = json!("critical")
                }),
                "a severity outside the three",
            ),
            (
                Box::new(|v: &mut Value| {
                    v["round"]["reviewers"][0]["findings"][0]["confidence"] = json!(1.5)
                }),
                "a confidence above 1",
            ),
            (
                Box::new(|v: &mut Value| {
                    v["round"]["reviewers"][0]["findings"][0]["evidence"] = json!("")
                }),
                "a finding without evidence",
            ),
            (
                Box::new(|v: &mut Value| {
                    v["round"]["reviewers"][0]["findings"][0]["line"] = json!(0)
                }),
                "line 0",
            ),
            (
                Box::new(|v: &mut Value| {
                    v.as_object_mut().unwrap().remove("evaluation");
                }),
                "a missing field instead of null",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-findings@1")),
                "the previous version",
            ),
        ],
    );
    let none = TaskFindingsV2 {
        rounds: 0,
        round: None,
        evaluation: None,
        ..document
    };
    none.validate().unwrap();
    assert_valid(
        "task-findings-v2.json",
        &serde_json::to_value(&none).unwrap(),
    );
}

#[test]
fn materialized_diff_and_status_documents_are_closed() {
    let materialized = TaskMaterializedV1 {
        schema: TASK_MATERIALIZED_V1.into(),
        task_id: "chain-base".into(),
        snapshot_id: id('1'),
        verified: false,
        into: "/work/review-wt".into(),
        written: vec!["added.txt".into(), "pagination.py".into()],
        deleted: vec!["README.md".into()],
        mode: vec!["tool.sh".into()],
        next: vec![
            "git -C ../review-wt status".into(),
            "af task report chain-base".into(),
        ],
    };
    both(
        "task-materialized-v1.json",
        &materialized,
        TaskMaterializedV1::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["written"] = json!(["a", "a"])),
                "a path written twice",
            ),
            (
                Box::new(|v: &mut Value| v["snapshot_id"] = json!("HEAD")),
                "a Snapshot that is not an exact ID",
            ),
        ],
    );
    let diff = TaskDiffV1 {
        schema: TASK_DIFF_V1.into(),
        task_id: "chain-base".into(),
        base_snapshot_id: id('1'),
        snapshot_id: id('2'),
        files: vec![
            TaskDiffFileV1 {
                path: "b.txt".into(),
                status: TaskDiffStatusV1::Renamed,
                old_path: Some("a.txt".into()),
                added: Some(1),
                deleted: Some(0),
            },
            TaskDiffFileV1 {
                path: "logo.png".into(),
                status: TaskDiffStatusV1::Added,
                old_path: None,
                added: None,
                deleted: None,
            },
        ],
        patch: "diff --git a/a.txt b/b.txt\n".into(),
    };
    both(
        "task-diff-v1.json",
        &diff,
        TaskDiffV1::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["files"][0]["old_path"] = Value::Null),
                "a rename without its old path",
            ),
            (
                Box::new(|v: &mut Value| v["files"][1]["status"] = json!("copied")),
                "a status outside the five",
            ),
        ],
    );
    let status = TaskStatusV2 {
        schema: TASK_STATUS_V2.into(),
        task_id: "pagination-cli".into(),
        state: TaskStateV1::ended(
            TaskStatePhaseV1::Abandoned,
            TaskOutcomeV1::Cancelled,
            Some(TaskCauseV1::HostCancelled),
        )
        .with_next("af task run pagination-cli"),
        node: None,
        elapsed_ms: Some(12_000),
        charged_tokens: 412_000u128.into(),
        reserved_tokens: 2_000u128.into(),
        remaining_tokens: 586_000u128.into(),
        unknown_usage_attempts: 1,
        last_gate: Some("check test: 51m 00s".into()),
        run: Some(run_process()),
        cancel: Some(cancel_request()),
    };
    both(
        "task-status-v2.json",
        &status,
        TaskStatusV2::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-status@1")),
                "an older status document",
            ),
            (
                Box::new(|v: &mut Value| v["charged_tokens"] = json!(412000)),
                "a token count that is not decimal text",
            ),
            (
                Box::new(|v: &mut Value| v["reserved_tokens"] = json!("02")),
                "a noncanonical reservation",
            ),
            (
                Box::new(|v: &mut Value| v["run"]["schema"] = json!("af/task-run-process@2")),
                "a run record of another version",
            ),
            (
                Box::new(|v: &mut Value| v["state"]["cause"] = Value::Null),
                "an abandoned state without its cause",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-status@1")),
                "the previous version",
            ),
        ],
    );
}

#[test]
fn detached_cancelled_and_the_delivery_proof_are_closed() {
    let running = TaskStateV1::new(TaskStatePhaseV1::Running);
    let detached = TaskDetachedV3 {
        schema: TASK_DETACHED_V3.into(),
        task_id: "pagination-cli".into(),
        run: 2,
        pid: 812,
        host: "builder".into(),
        log: "/state/runs/pagination-cli-2.log".into(),
        admitted: true,
        state: running,
        next_action: Some("af task wait pagination-cli".into()),
    };
    both(
        "task-detached-v3.json",
        &detached,
        TaskDetachedV3::validate,
        vec![
            (Box::new(|v: &mut Value| v["run"] = json!(0)), "run 0"),
            (Box::new(|v: &mut Value| v["log"] = json!("")), "no log"),
            (
                Box::new(|v: &mut Value| v["next_action"] = Value::Null),
                "an admitted run without the command that waits for it",
            ),
            (
                Box::new(|v: &mut Value| {
                    v.as_object_mut().unwrap().remove("admitted");
                }),
                "a run that does not say whether it was admitted",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-detached@2")),
                "the earlier version",
            ),
        ],
    );
    // Still starting when the command gave up waiting: not admitted, and the next command
    // looks at it rather than claiming it runs.
    let starting = TaskDetachedV3 {
        admitted: false,
        state: TaskStateV1::new(TaskStatePhaseV1::Planned).with_next("af task run pagination-cli"),
        next_action: Some("af task status pagination-cli".into()),
        ..detached.clone()
    };
    both(
        "task-detached-v3.json",
        &starting,
        TaskDetachedV3::validate,
        vec![],
    );
    let ended = TaskDetachedV3 {
        admitted: false,
        state: verified(),
        next_action: None,
        ..detached
    };
    both(
        "task-detached-v3.json",
        &ended,
        TaskDetachedV3::validate,
        vec![],
    );
    let cancelled = TaskCancelledV2 {
        schema: TASK_CANCELLED_V2.into(),
        task_id: "pagination-cli".into(),
        request: cancel_request(),
        state: TaskStateV1::ended(
            TaskStatePhaseV1::Abandoned,
            TaskOutcomeV1::Cancelled,
            Some(TaskCauseV1::HostCancelled),
        )
        .with_next("af task run pagination-cli"),
    };
    both(
        "task-cancelled-v2.json",
        &cancelled,
        TaskCancelledV2::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["request"]["pid"] = json!(0)),
                "a request of no process",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/task-cancelled@1")),
                "the previous version",
            ),
        ],
    );
    let proof = TaskDeliveryProofV1 {
        verification_task_id: "chain-verify".into(),
        verification_result_id: id('1'),
        candidate_task_id: "chain-base".into(),
        candidate_result_id: id('2'),
        candidate_snapshot_id: id('3'),
        candidate_source_snapshot_id: id('4'),
    };
    both(
        "task-delivery-proof-v1.json",
        &proof,
        TaskDeliveryProofV1::validate,
        vec![
            (
                Box::new(|v: &mut Value| v["candidate_snapshot_id"] = json!("sha256:x")),
                "a Snapshot that is not an exact ID",
            ),
            (
                Box::new(|v: &mut Value| v["schema"] = json!("af/TaskDeliveryProof@1")),
                "an undeclared field",
            ),
        ],
    );
    let mut same = proof;
    same.candidate_task_id = same.verification_task_id.clone();
    assert!(same.validate().is_err(), "a Task cannot prove itself");
}
