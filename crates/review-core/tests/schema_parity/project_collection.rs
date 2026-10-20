//! Survey collection (ADR-0152 clause 13): `SurveyCollected@1`, the tombstone a collected
//! Survey leaves in its Project Store's log and that binds every identity field of its record,
//! against its own schema, inside `run-event-v1.json` and through the Rust validator alike; and
//! `af/TaskStorageSweep@1`, whose kind set Survey collection leaves unchanged, so a Survey's
//! removal is counted and never listed.

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
fn a_survey_tombstone_binds_the_records_identity_keeps_its_totals_and_never_a_path() {
    let tombstone = SurveyCollectedPayloadV1 {
        survey_id: format!("sha256:{}", "a".repeat(64)),
        head: HEAD.into(),
        cutoff_unix_s: 1_800_000_000.into(),
        completeness: SurveyCompletenessV1::Complete,
        refs_digest: format!("sha256:{}", "b".repeat(64)),
        policy_digest: format!("sha256:{}", "c".repeat(64)),
        surveyor_version: 5.into(),
        previous_survey_id: Some(format!("sha256:{}", "d".repeat(64))),
        tree: Some(SurveyCollectedTreeV1 {
            files: 16.into(),
            bytes: 1496.into(),
            lines: 70.into(),
        }),
        history: Some(SurveyCollectedHistoryV1 {
            commits: 8.into(),
            merges: 1.into(),
        }),
        unreadable: None,
        collected_unix_ms: 1_800_000_000_123.into(),
    };
    tombstone.validate().unwrap();
    let value = serde_json::to_value(&tombstone).unwrap();
    let bare = without(&without(&value, "tree"), "history");
    let first = without(&value, "previous_survey_id");
    let unreadable = with(
        &bare,
        "unreadable",
        json!("its bytes are not a Survey record: syntax error at line 1, column 1"),
    );
    for accepted in [&value, &bare, &unreadable, &first] {
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
            without(&value, "refs_digest"),
            "a tombstone binds the record's refs",
        ),
        (
            without(&value, "policy_digest"),
            "a tombstone binds the record's policy",
        ),
        (
            without(&value, "surveyor_version"),
            "a tombstone binds the record's Surveyor version",
        ),
        (
            with(&value, "refs_digest", json!("refs")),
            "the refs are a digest",
        ),
        (
            with(&value, "policy_digest", json!("policy")),
            "the policy is a digest",
        ),
        (
            with(&value, "previous_survey_id", json!("survey-0")),
            "the previous Survey is named by its digest",
        ),
        (
            with(&value, "surveyor_version", json!(5)),
            "the Surveyor version is decimal text",
        ),
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
        (
            with(&value, "unreadable", json!("corrupt")),
            "an unreadable record keeps no totals",
        ),
        (
            with(&bare, "unreadable", json!("x".repeat(257))),
            "the reason is at most 256 bytes",
        ),
        (
            with(&bare, "unreadable", json!("")),
            "the reason is not empty",
        ),
        (
            with(&bare, "unreadable", json!("two\nlines")),
            "the reason holds no control character",
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

/// ADR-0145 clause 2 for the sweep record: Survey collection adds no kind to
/// `af/TaskStorageSweep@1`. A sweep that took a Survey counts it in `omitted_removals` and still
/// validates; a removal of kind `survey` is a shape the record does not have, refused by its
/// schema, inside `TaskTransition@5` and by the typed contract alike.
#[test]
fn the_sweep_record_counts_a_survey_and_has_no_survey_kind() {
    use review_core::task::storage_sweep::{
        StorageSweepStopV1, TASK_STORAGE_SWEEP_V1, TaskStorageSweepV1,
    };
    let counted = TaskStorageSweepV1 {
        schema: TASK_STORAGE_SWEEP_V1.into(),
        removals: Vec::new(),
        omitted_removals: 1,
        failures: Vec::new(),
        omitted_failures: 0,
        stop: StorageSweepStopV1::Fits,
        max_bytes: 1 << 30,
        total_before: 1 << 30,
        total_after: (1 << 30) - 4096,
    };
    counted.validate().unwrap();
    let value = serde_json::to_value(&counted).unwrap();
    assert_valid("task-storage-sweep-v1.json", &value);
    let mut listed = value.clone();
    listed["removals"] = json!([{
        "kind": "survey",
        "path": "/home/me/.local/state/af/project/0123/cas/objects/ab/cdef",
        "bytes": 4096,
        "rule": "collection",
    }]);
    assert_invalid(
        "task-storage-sweep-v1.json",
        &listed,
        "a Survey is no kind of the sweep record",
    );
    assert!(serde_json::from_value::<TaskStorageSweepV1>(listed.clone()).is_err());
    let transition = json!({
        "writer": "af-storage-sweep-1",
        "epoch": 3,
        "now_unix_ms": 100,
        "change": {"kind": "storage_sweep", "sweep": listed},
    });
    assert_invalid(
        "task-transition-v5.json",
        &transition,
        "a Survey is no kind of the sweep record",
    );
    assert!(validate_event_payload(EventType::TaskTransitionV5, &transition).is_err());
}
