use super::{assert_invalid, assert_valid};
use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
use review_core::task::review_handoff::*;
use serde_json::json;
fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
#[test]
fn review_handoff_receipt_has_exact_roots_and_closed_evidence_variants() {
    for evidence in [
        TaskReviewHandoffEvidenceV1::ClosedRound {
            report_event_id: "a".repeat(26),
        },
        TaskReviewHandoffEvidenceV1::SupersededInput {
            superseded_event_id: "b".repeat(26),
        },
    ] {
        let receipt = TaskReviewHandoffV1 {
            task_id: "review".into(),
            predecessor_revision_id: id('1'),
            predecessor_plan_id: id('2'),
            successor_revision_id: id('3'),
            successor_plan_id: id('4'),
            predecessor_round_id: id('5'),
            successor_round_id: id('6'),
            evidence,
        };
        receipt.validate().unwrap();
        let value = serde_json::to_value(&receipt).unwrap();
        assert_valid("task-review-handoff-v2.json", &value);
        assert_eq!(
            receipt.artifact_refs(),
            [id('1'), id('2'), id('3'), id('4'), id('5'), id('6')]
        );
        for pointer in [
            "/predecessor_revision_id",
            "/successor_plan_id",
            "/predecessor_round_id",
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("foreign");
            assert_invalid("task-review-handoff-v2.json", &bad, pointer);
            assert!(
                serde_json::from_value::<TaskReviewHandoffV1>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for field in ["limits", "approved", "spent_tokens"] {
            let mut bad = value.clone();
            bad[field] = json!(true);
            assert_invalid(
                "task-review-handoff-v2.json",
                &bad,
                "receipt cannot grant authority or credit",
            );
            assert!(serde_json::from_value::<TaskReviewHandoffV1>(bad).is_err());
        }
        let mut bad = receipt;
        bad.successor_round_id = bad.predecessor_round_id.clone();
        assert!(bad.validate().is_err());
    }
}
#[test]
fn review_continuation_is_an_ordinary_exact_transition_change() {
    let transition = TaskTransitionV1 {
        writer: "writer".into(),
        epoch: 1,
        now_unix_ms: 123,
        change: TaskChangeV1::ReviewContinued {
            handoff_id: id('a'),
        },
    };
    transition.validate().unwrap();
    let value = serde_json::to_value(&transition).unwrap();
    assert_valid("task-transition-v5.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskTransitionV1>(value.clone()).unwrap(),
        transition
    );
    review_core::event::validate_event_payload(review_core::EventType::TaskTransitionV5, &value)
        .unwrap();
    for (pointer, replacement) in [
        ("/epoch", json!(0)),
        ("/change/handoff_id", json!("missing")),
        ("/writer", json!("")),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-transition-v5.json", &bad, pointer);
        assert!(
            review_core::event::validate_event_payload(
                review_core::EventType::TaskTransitionV5,
                &bad
            )
            .is_err()
        );
    }
    let mut extra = value;
    extra["change"]["phase_id"] = json!(id('b'));
    assert_invalid(
        "task-transition-v5.json",
        &extra,
        "a continuation carries no other evidence",
    );
    assert!(serde_json::from_value::<TaskTransitionV1>(extra).is_err());
}
