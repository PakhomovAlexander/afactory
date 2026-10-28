//! `af/TaskCheckReceipt@2` and `af/TaskAcceptance@1` (ADR-0255): the Rust types and the schemas
//! accept and refuse the same documents, and the receipt version before them is refused by
//! name.

use super::{assert_invalid, assert_valid};
use review_core::task::acceptance::*;
use review_core::task::pipeline::ReceiptOutcomeV1;
use review_core::task::verification::*;
use serde_json::json;
use std::collections::BTreeMap;

fn id(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn items() -> Vec<TaskAcceptanceItemV1> {
    vec![
        TaskAcceptanceItemV1 {
            id: "bounds".into(),
            text: "Negative offsets and limits are refused".into(),
        },
        TaskAcceptanceItemV1 {
            id: "past-end".into(),
            text: "An offset past the end returns no items\nand no error".into(),
        },
    ]
}

fn receipt() -> TaskCheckReceiptV2 {
    TaskCheckReceiptV2 {
        plan_id: id('1'),
        snapshot_id: id('2'),
        policy_id: id('3'),
        outcome: ReceiptOutcomeV1::Inconclusive,
        checks: BTreeMap::from([("kernel".into(), id('4')), ("lint".into(), id('5'))]),
        stopped: BTreeMap::from([(
            "kernel".into(),
            TaskCheckStopV2 {
                cause: TaskCheckStopCauseV2::Wall,
                wall_ms: 5_400_000,
                elapsed_ms: 5_400_012,
            },
        )]),
        acceptance: Some(TaskReceiptAcceptanceV2 {
            record_id: id('6'),
            items: items(),
        }),
    }
}

#[test]
fn task_check_receipt_v2_schema_and_type_agree() {
    let record = receipt();
    record.validate().unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["stopped"]["kernel"]["cause"], "wall");
    assert_valid("task-check-receipt-v2.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskCheckReceiptV2>(value.clone()).unwrap(),
        record
    );
    // Without a stopped check or acceptance, neither field is written.
    let plain = TaskCheckReceiptV2 {
        stopped: BTreeMap::new(),
        acceptance: None,
        ..record
    };
    let plain_value = serde_json::to_value(&plain).unwrap();
    assert!(plain_value.get("stopped").is_none() && plain_value.get("acceptance").is_none());
    assert_valid("task-check-receipt-v2.json", &plain_value);

    let refused = |mutate: &dyn Fn(&mut serde_json::Value), why: &str| {
        let mut invalid = value.clone();
        mutate(&mut invalid);
        assert_invalid("task-check-receipt-v2.json", &invalid, why);
        let decoded = serde_json::from_value::<TaskCheckReceiptV2>(invalid);
        assert!(decoded.map_or(true, |r| r.validate().is_err()), "{why}");
    };
    refused(
        &|v| v["stopped"]["kernel"]["cause"] = json!("deadline"),
        "a cause outside the closed vocabulary",
    );
    refused(
        &|v| v["stopped"]["kernel"]["wall_ms"] = json!(0),
        "a wall is positive",
    );
    refused(
        &|v| {
            v["stopped"]["kernel"]
                .as_object_mut()
                .unwrap()
                .remove("elapsed_ms");
        },
        "a stopped check states how long it ran",
    );
    refused(
        &|v| v["stopped"] = json!({}),
        "an empty set has one spelling: absent",
    );
    refused(&|v| v["acceptance"] = json!(null), "absent, never null");
    refused(
        &|v| v["acceptance"]["items"] = json!([]),
        "acceptance states at least one item",
    );
    refused(
        &|v| v["acceptance"]["items"][0]["id"] = json!("-x"),
        "an item ID is a name",
    );
    refused(
        &|v| v["acceptance"]["items"][0]["approved"] = json!(true),
        "an item grants nothing",
    );
    refused(&|v| v["acceptance"]["record_id"] = json!("x"), "a digest");
    // Rust alone checks what the schema cannot say: a stopped check has a result, and an item
    // ID is stated once.
    let mut orphan = receipt();
    orphan.stopped.insert(
        "absent".into(),
        TaskCheckStopV2 {
            cause: TaskCheckStopCauseV2::Wall,
            wall_ms: 1,
            elapsed_ms: 1,
        },
    );
    assert!(orphan.validate().is_err());
    let mut twice = receipt();
    let acceptance = twice.acceptance.as_mut().unwrap();
    acceptance.items[1].id = "bounds".into();
    assert!(twice.validate().is_err());
}

#[test]
fn the_receipt_before_adr_0255_is_refused_by_name() {
    assert_eq!(RETIRED_TASK_CHECK_RECEIPT_V1, "af/TaskCheckReceipt@1");
    let refusal = refuse_retired_receipt(RETIRED_TASK_CHECK_RECEIPT_V1).unwrap_err();
    assert!(
        refusal.starts_with("af/TaskCheckReceipt@1 is the check receipt before ADR-0255")
            && refusal.contains("declare af/TaskCheckReceipt@2"),
        "{refusal}"
    );
    refuse_retired_receipt(TASK_CHECK_RECEIPT_V2).unwrap();
    // The old schema is gone with its writer. An @1 payload reads like an @2 receipt without
    // `stopped` or `acceptance`, so a reader decides by the artifact type, refused by name above,
    // never by the payload.
    assert!(
        !super::workspace_root()
            .join("schemas/task-check-receipt-v1.json")
            .exists()
    );
}

#[test]
fn task_acceptance_schema_and_type_agree() {
    let record = TaskAcceptanceV1::new(items()).unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_valid("task-acceptance-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskAcceptanceV1>(value.clone()).unwrap(),
        record
    );
    let refused = |mutate: &dyn Fn(&mut serde_json::Value), why: &str| {
        let mut invalid = value.clone();
        mutate(&mut invalid);
        assert_invalid("task-acceptance-v1.json", &invalid, why);
        let decoded = serde_json::from_value::<TaskAcceptanceV1>(invalid);
        assert!(decoded.map_or(true, |r| r.validate().is_err()), "{why}");
    };
    refused(
        &|v| v["schema"] = json!("af.task-acceptance/2"),
        "another version is another type",
    );
    refused(&|v| v["items"] = json!([]), "never written empty");
    refused(
        &|v| v["items"][0]["text"] = json!("   "),
        "an item says something",
    );
    refused(
        &|v| v["items"][0]["text"] = json!("bell \u{7}"),
        "no control characters but newline and tab",
    );
    refused(
        &|v| v["items"][0]["text"] = json!("x".repeat(4097)),
        "at most 4096 characters",
    );
    refused(
        &|v| {
            v["items"] = json!(
                (0..33)
                    .map(|n| json!({"id": format!("i{n}"), "text": "x"}))
                    .collect::<Vec<_>>()
            )
        },
        "at most 32 items",
    );
    refused(
        &|v| {
            v["items"][0].as_object_mut().unwrap().remove("id");
        },
        "every item has its ID",
    );
}
