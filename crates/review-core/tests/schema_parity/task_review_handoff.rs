use super::{assert_invalid, assert_valid};
use review_core::task::event::{TaskChangeV1, TaskTransitionV1, TaskTransitionV2};
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
        assert_valid("task-review-handoff-v1.json", &value);
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
            assert_invalid("task-review-handoff-v1.json", &bad, pointer);
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
                "task-review-handoff-v1.json",
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
fn review_handoff_requires_new_transition_generation() {
    let normalized = TaskTransitionV1 {
        writer: "writer".into(),
        epoch: 1,
        now_unix_ms: 123,
        change: TaskChangeV1::ReviewContinued {
            handoff_id: id('a'),
        },
    };
    assert!(normalized.validate().is_err());
    assert!(serde_json::to_value(&normalized).is_err());
    let wire = TaskTransitionV2::from_continuation(&normalized).unwrap();
    wire.validate().unwrap();
    let value = serde_json::to_value(&wire).unwrap();
    assert_valid("task-transition-v2.json", &value);
    assert_invalid(
        "task-transition-v1.json",
        &value,
        "old lifecycle cannot admit new handoff",
    );
    assert!(serde_json::from_value::<TaskTransitionV1>(value.clone()).is_err());
    assert_eq!(wire.into_transition(), normalized);
    review_core::event::validate_event_payload(review_core::EventType::TaskTransitionV2, &value)
        .unwrap();
    for (pointer, replacement) in [
        ("/epoch", json!(0)),
        ("/change/handoff_id", json!("missing")),
        ("/writer", json!("")),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-transition-v2.json", &bad, pointer);
        assert!(
            review_core::event::validate_event_payload(
                review_core::EventType::TaskTransitionV2,
                &bad
            )
            .is_err()
        );
    }
    let mut old = value;
    old["change"] = json!({"kind":"resumed"});
    assert_invalid(
        "task-transition-v2.json",
        &old,
        "new payload does not reinterpret old lifecycle",
    );
}
