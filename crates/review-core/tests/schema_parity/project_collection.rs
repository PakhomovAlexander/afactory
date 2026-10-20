//! Survey collection (ADR-0152 clause 13): `SurveyCollected@1`, the tombstone a collected
//! Survey leaves in its Project Store's log, against its own schema, inside `run-event-v1.json`
//! and through the Rust validator alike.

use super::{assert_invalid, assert_valid};
use review_core::EventType;
use review_core::event::validate_event_payload;
use review_core::project::{
    SurveyCollectedHistoryV1, SurveyCollectedPayloadV1, SurveyCollectedTreeV1, SurveyCompletenessV1,
};
use serde_json::{Value, json};

const SCHEMA: &str = "project-survey-collected-v1.json";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn event(payload: Value) -> Value {
    json!({
        "event_id": "0".repeat(26),
        "run_id": "survey-collection",
        "sequence": 0,
        "type": "SurveyCollected@1",
        "occurred_at": "2026-10-20T00:00:00Z",
        "artifact_refs": [],
        "payload": payload,
    })
}

fn with(base: &Value, key: &str, value: Value) -> Value {
    let mut changed = base.clone();
    changed[key] = value;
    changed
}

fn without(base: &Value, key: &str) -> Value {
    let mut changed = base.clone();
    changed.as_object_mut().unwrap().remove(key);
    changed
}

#[test]
fn a_survey_tombstone_keeps_its_totals_and_never_a_path() {
    let tombstone = SurveyCollectedPayloadV1 {
        survey_id: format!("sha256:{}", "a".repeat(64)),
        head: HEAD.into(),
        cutoff_unix_s: 1_800_000_000.into(),
        completeness: SurveyCompletenessV1::Complete,
        tree: Some(SurveyCollectedTreeV1 {
            files: 16.into(),
            bytes: 1496.into(),
            lines: 70.into(),
        }),
        history: Some(SurveyCollectedHistoryV1 {
            commits: 8.into(),
            merges: 1.into(),
        }),
        collected_unix_ms: 1_800_000_000_123.into(),
    };
    tombstone.validate().unwrap();
    let value = serde_json::to_value(&tombstone).unwrap();
    let bare = without(&without(&value, "tree"), "history");
    for accepted in [&value, &bare] {
        assert_valid(SCHEMA, accepted);
        assert_valid("run-event-v1.json", &event(accepted.clone()));
        validate_event_payload(EventType::SurveyCollectedV1, accepted).unwrap();
    }
    assert_eq!(
        serde_json::from_value::<SurveyCollectedPayloadV1>(value.clone()).unwrap(),
        tombstone
    );
    for (broken, why) in [
        (
            with(&value, "survey_id", json!("survey-1")),
            "a Survey is named by its digest",
        ),
        (with(&value, "head", json!("main")), "the head is a commit"),
        (
            with(&value, "cutoff_unix_s", json!(1_800_000_000_u64)),
            "times are decimal text",
        ),
        (
            with(&value, "completeness", json!("collected")),
            "completeness is the record's",
        ),
        (
            with(&value, "path", json!("/home/me/.local/state")),
            "no field holds a path",
        ),
        (
            with(&value, "tree", json!({"files": "1", "bytes": "2"})),
            "the tree keeps files, bytes and lines",
        ),
        (
            with(
                &value,
                "history",
                json!({"commits": "1", "merges": "1", "windows": {}}),
            ),
            "the history keeps commits and merges only",
        ),
        (
            without(&value, "collected_unix_ms"),
            "a tombstone says when",
        ),
    ] {
        assert_invalid(SCHEMA, &broken, why);
        assert_invalid("run-event-v1.json", &event(broken.clone()), why);
        assert!(
            validate_event_payload(EventType::SurveyCollectedV1, &broken).is_err(),
            "the Rust validator accepted it ({why})"
        );
    }
}
