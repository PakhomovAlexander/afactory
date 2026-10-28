//! `af/TaskAttemptWallEnd@1` and `af/TaskPartialCandidate@1` (ADR-0257): the Rust types and the
//! schemas accept and refuse the same documents.

use super::{assert_invalid, assert_valid};
use review_core::task::attempt_wall::*;
use serde_json::{Value, json};

fn id(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

/// `value` with each field of `edit` set, or removed where `edit` holds null.
fn edited(value: &Value, edit: &Value) -> Value {
    let mut changed = value.clone();
    for (key, field) in edit.as_object().unwrap() {
        if field.is_null() {
            changed.as_object_mut().unwrap().remove(key);
        } else {
            changed[key] = field.clone();
        }
    }
    changed
}

#[test]
fn the_attempt_wall_end_and_its_schema_agree() {
    let end = TaskAttemptWallEndV1 {
        schema: TASK_ATTEMPT_WALL_END_SCHEMA.into(),
        task_id: "fix-1".into(),
        node: "root.nodes.implement".into(),
        attempt_id: "01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
        wall_ms: 1_800_000,
        elapsed_ms: 1_800_412,
        partial_candidate_id: Some(id('e')),
    };
    for record in [
        end.clone(),
        TaskAttemptWallEndV1 {
            partial_candidate_id: None,
            ..end.clone()
        },
    ] {
        record.validate().unwrap();
        let value = serde_json::to_value(&record).unwrap();
        assert_valid("task-attempt-wall-end-v1.json", &value);
        assert_eq!(
            serde_json::from_value::<TaskAttemptWallEndV1>(value).unwrap(),
            record
        );
    }
    let value = serde_json::to_value(&end).unwrap();
    let mut null_partial = value.clone();
    null_partial["partial_candidate_id"] = Value::Null;
    for (why, changed) in [
        ("a zero wall", edited(&value, &json!({"wall_ms": 0}))),
        (
            "another version",
            edited(&value, &json!({"schema": "af.task-attempt-wall-end/2"})),
        ),
        (
            "an Attempt that is not one",
            edited(&value, &json!({"attempt_id": "a"})),
        ),
        (
            "an unqualified node",
            edited(&value, &json!({"node": "root..implement"})),
        ),
        (
            "a partial candidate by name",
            edited(&value, &json!({"partial_candidate_id": "tree"})),
        ),
        ("a null partial candidate", null_partial),
        ("an unknown field", edited(&value, &json!({"seconds": 1}))),
        (
            "no elapsed time",
            edited(&value, &json!({"elapsed_ms": null})),
        ),
    ] {
        assert_invalid("task-attempt-wall-end-v1.json", &changed, why);
        assert!(
            serde_json::from_value::<TaskAttemptWallEndV1>(changed)
                .map_or(true, |record| record.validate().is_err()),
            "Rust accepted {why}"
        );
    }
}

#[test]
fn the_partial_candidate_and_its_schema_agree() {
    let partial = TaskPartialCandidateV1 {
        schema: TASK_PARTIAL_CANDIDATE_SCHEMA.into(),
        parent_snapshot_id: id('a'),
        snapshot_id: id('b'),
        source_tree_id: id('c'),
        changed_paths: 2,
    };
    partial.validate().unwrap();
    let value = serde_json::to_value(&partial).unwrap();
    assert_valid("task-partial-candidate-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskPartialCandidateV1>(value.clone()).unwrap(),
        partial
    );
    for (why, edit) in [
        (
            "another version",
            json!({"schema": "af.task-partial-candidate/2"}),
        ),
        ("a Snapshot by name", json!({"snapshot_id": "tree"})),
        ("no SourceTree", json!({"source_tree_id": null})),
        ("a negative count", json!({"changed_paths": -1})),
        ("an unknown field", json!({"manifest": id('d')})),
    ] {
        let changed = edited(&value, &edit);
        assert_invalid("task-partial-candidate-v1.json", &changed, why);
        assert!(
            serde_json::from_value::<TaskPartialCandidateV1>(changed)
                .map_or(true, |record| record.validate().is_err()),
            "Rust accepted {why}"
        );
    }
}
