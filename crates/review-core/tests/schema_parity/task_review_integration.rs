use super::{assert_invalid, assert_valid};
use review_core::task::event::{
    TaskChangeV1, TaskTransitionV1, TaskTransitionV2, TaskTransitionV3,
};
use review_core::task::report::*;
use review_core::task::review_handoff::*;
use review_core::task::review_integration::*;
use serde_json::json;
fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn phase() -> TaskReviewIntegrationPhaseV1 {
    TaskReviewIntegrationPhaseV1 {
        task_id: "review".into(),
        task_revision_id: id('1'),
        plan_id: id('2'),
        round_id: id('3'),
        closing_report_event_id: "a".repeat(26),
        selection: TaskReviewIntegrationSelectionV1::Prepared {
            integration_plan_id: id('4'),
            derived_snapshot_id: id('5'),
        },
    }
}
#[test]
fn integration_sequence_pins_existing_gate_policy_and_complete_order() {
    let policy = TaskReviewCheckSequencePolicyV1 {
        authority_policy_id: id('1'),
        pipeline_policy_id: id('2'),
        gate_execution_policy_id: id('2'),
        ordered_check_names: vec!["format".into(), "test".into()],
        check_timeout_ms: 1000,
    };
    policy.validate().unwrap();
    let value = serde_json::to_value(&policy).unwrap();
    assert_valid("task-review-check-sequence-policy-v1.json", &value);
    assert_eq!(policy.artifact_refs(), [id('1'), id('2')]);
    for (field, bad_value) in [
        ("ordered_check_names", json!([])),
        ("ordered_check_names", json!(["test", "test"])),
        ("ordered_check_names", json!(["   "])),
        ("check_timeout_ms", json!(0)),
        ("gate_execution_policy_id", json!("missing")),
    ] {
        let mut bad = value.clone();
        bad[field] = bad_value;
        assert_invalid("task-review-check-sequence-policy-v1.json", &bad, field);
        assert!(
            serde_json::from_value::<TaskReviewCheckSequencePolicyV1>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut boundary = policy.clone();
    boundary.ordered_check_names = (0..63).map(|n| format!("check{n}")).collect();
    boundary.validate().unwrap();
    assert_valid(
        "task-review-check-sequence-policy-v1.json",
        &serde_json::to_value(&boundary).unwrap(),
    );
    boundary.ordered_check_names.push("check63".into());
    assert!(
        boundary.validate().is_err(),
        "summary needs the final existing settlement artifact slot"
    );
    assert_invalid(
        "task-review-check-sequence-policy-v1.json",
        &serde_json::to_value(boundary).unwrap(),
        "64 checks overflow the fixed common settlement envelope",
    );
    let mut foreign = policy;
    foreign.gate_execution_policy_id = id('3');
    assert!(
        foreign.validate().is_err(),
        "a receipt cannot invent a distinct Gate policy"
    );
}
#[test]
fn integration_selection_has_no_embedded_allowance_or_admission() {
    for selection in [
        TaskReviewIntegrationSelectionV1::Empty {},
        TaskReviewIntegrationSelectionV1::Conflict {
            conflict: review_core::IntegrationConflictPayloadV1 {
                base_snapshot_id: id('3'),
                proposal_ids: vec![id('4')],
                paths: vec!["src/lib.rs".into()],
                reason: "protected path".into(),
            },
        },
        phase().selection,
    ] {
        let value = TaskReviewIntegrationPhaseV1 {
            selection,
            ..phase()
        };
        value.validate().unwrap();
        let json = serde_json::to_value(value).unwrap();
        assert_valid("task-review-integration-phase-v1.json", &json);
        for field in ["allowance", "approved", "writer"] {
            let mut bad = json.clone();
            bad[field] = json!(true);
            assert_invalid("task-review-integration-phase-v1.json", &bad, field);
            assert!(serde_json::from_value::<TaskReviewIntegrationPhaseV1>(bad).is_err());
        }
        let mut bad = json;
        bad["closing_report_event_id"] = json!(id('a'));
        assert_invalid(
            "task-review-integration-phase-v1.json",
            &bad,
            "requires event identity",
        );
        assert!(
            serde_json::from_value::<TaskReviewIntegrationPhaseV1>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
#[test]
fn integration_requires_new_transition_generation() {
    for change in [
        TaskChangeV1::ReviewIntegrationSelected { phase_id: id('a') },
        TaskChangeV1::ReviewIntegrationFinished {
            phase_id: id('a'),
            report_id: id('b'),
            integration_committed_event_id: None,
        },
        TaskChangeV1::ReviewIntegrationFinished {
            phase_id: id('a'),
            report_id: id('b'),
            integration_committed_event_id: Some("c".repeat(26)),
        },
    ] {
        let normalized = TaskTransitionV1 {
            writer: "writer".into(),
            epoch: 1,
            now_unix_ms: 123,
            change,
        };
        assert!(normalized.validate().is_err());
        assert!(serde_json::to_value(&normalized).is_err());
        assert!(TaskTransitionV2::from_continuation(&normalized).is_none());
        let wire = TaskTransitionV3::from_integration(&normalized).unwrap();
        wire.validate().unwrap();
        let value = serde_json::to_value(&wire).unwrap();
        assert_valid("task-transition-v3.json", &value);
        assert_invalid("task-transition-v1.json", &value, "old lifecycle unchanged");
        assert_invalid(
            "task-transition-v2.json",
            &value,
            "handoff generation unchanged",
        );
        assert!(serde_json::from_value::<TaskTransitionV1>(value.clone()).is_err());
        assert_eq!(wire.into_transition(), normalized);
        review_core::event::validate_event_payload(
            review_core::EventType::TaskTransitionV3,
            &value,
        )
        .unwrap();
        let mut bad = value.clone();
        bad["change"]["phase_id"] = json!("foreign");
        assert_invalid("task-transition-v3.json", &bad, "invalid identity");
        assert!(
            review_core::event::validate_event_payload(
                review_core::EventType::TaskTransitionV3,
                &bad
            )
            .is_err()
        );
        if value["change"]["kind"] == "review_integration_finished" {
            let mut bad = value;
            bad["change"]["integration_committed_event_id"] = json!(null);
            assert_invalid("task-transition-v3.json", &bad, "omitted is not null");
            assert!(serde_json::from_value::<TaskTransitionV3>(bad).is_err());
        }
    }
}
#[test]
fn integration_report_is_exactly_one_executed_or_failed_phase_node() {
    let report = TaskRunReportV2 {
        task_revision_id: id('1'),
        plan_id: id('2'),
        through_sequence: 4,
        phase_id: id('3'),
        nodes: vec![TaskNodeReportV1 {
            node: "root.integration_checks".into(),
            outcome: TaskNodeOutcomeV1::Failed {
                diagnostic_id: id('4'),
                class: TaskFailureClassV1::Resources,
            },
        }],
    };
    report.validate().unwrap();
    let value = serde_json::to_value(&report).unwrap();
    assert_valid("task-run-report-v2.json", &value);
    assert_invalid(
        "task-run-report-v1.json",
        &value,
        "ordinary report stays phase-free",
    );
    let mut two = report.clone();
    two.nodes.push(TaskNodeReportV1 {
        node: "root.other".into(),
        outcome: two.nodes[0].outcome.clone(),
    });
    assert!(two.validate().is_err());
    assert_invalid(
        "task-run-report-v2.json",
        &serde_json::to_value(two).unwrap(),
        "no second operation",
    );
    let mut suppressed = report;
    suppressed.nodes[0].outcome = TaskNodeOutcomeV1::Suppressed {
        reason: TaskSuppressionV1::BranchNotSelected,
    };
    assert!(suppressed.validate().is_err());
    assert_invalid(
        "task-run-report-v2.json",
        &serde_json::to_value(suppressed).unwrap(),
        "activated sequence is factual",
    );
}
#[test]
fn integrated_handoff_cannot_be_serialized_as_frozen_handoff() {
    let receipt = TaskReviewHandoffV1 {
        task_id: "review".into(),
        predecessor_revision_id: id('1'),
        predecessor_plan_id: id('2'),
        successor_revision_id: id('3'),
        successor_plan_id: id('4'),
        predecessor_round_id: id('5'),
        successor_round_id: id('6'),
        evidence: TaskReviewHandoffEvidenceV1::IntegratedRound {
            report_event_id: "a".repeat(26),
            phase_id: id('7'),
            integration_committed_event_id: "b".repeat(26),
        },
    };
    assert!(receipt.validate().is_err());
    assert!(serde_json::to_value(&receipt).is_err());
    let wire = TaskReviewHandoffV2::from_integrated(&receipt).unwrap();
    wire.validate().unwrap();
    let value = serde_json::to_value(&wire).unwrap();
    assert_valid("task-review-handoff-v2.json", &value);
    assert_invalid(
        "task-review-handoff-v1.json",
        &value,
        "closed-round generation unchanged",
    );
    assert_eq!(wire.into_handoff(), receipt);
    let mut bad = value;
    bad["evidence"]["integration_committed_event_id"] = json!(id('b'));
    assert_invalid(
        "task-review-handoff-v2.json",
        &bad,
        "commit is an exact event",
    );
    assert!(
        serde_json::from_value::<TaskReviewHandoffV2>(bad)
            .unwrap()
            .validate()
            .is_err()
    );
}
