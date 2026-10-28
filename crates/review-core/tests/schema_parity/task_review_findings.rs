//! `af/TaskReviewFindings@1` and `af/TaskReadFirst@2` (ADR-0212, ADR-0254): the Rust types and the schemas
//! accept and refuse the same documents.

use super::{assert_invalid, assert_valid};
use review_core::finding::Severity;
use review_core::task::findings::*;
use serde_json::json;

fn id(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn findings() -> TaskReviewFindingsV1 {
    TaskReviewFindingsV1 {
        schema: TASK_REVIEW_FINDINGS_SCHEMA.into(),
        continues: TaskContinuesV1 {
            task_id: "p11-round-1".into(),
            task_revision_id: id('1'),
            result_id: id('2'),
            round: 1,
            round_id: id('3'),
            snapshot_id: id('4'),
        },
        findings: vec![
            TaskReviewFindingV1 {
                id: "bugs-1".into(),
                origin: TaskReviewFindingOriginV1::Reviewer,
                by: "bugs".into(),
                artifact_id: id('5'),
                severity: Some(Severity::Major),
                file: Some("src/lib.rs".into()),
                line: Some(7),
                title: "Off by one".into(),
                body: "The last item is dropped.".into(),
                fix: Some("Use an inclusive bound.".into()),
            },
            TaskReviewFindingV1 {
                id: "evaluator".into(),
                origin: TaskReviewFindingOriginV1::Evaluator,
                by: "evaluator".into(),
                artifact_id: id('6'),
                severity: None,
                file: None,
                line: None,
                title: "Evaluator verdict: failed".into(),
                body: "Pagination ignores the offset.".into(),
                fix: None,
            },
        ],
        missing_reviewers: ["uix".to_owned()].into(),
    }
}

#[test]
fn task_review_findings_schema_and_type_agree() {
    let record = findings();
    record.validate().unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_valid("task-review-findings-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskReviewFindingsV1>(value.clone()).unwrap(),
        record
    );
    let refused = |mutate: &dyn Fn(&mut serde_json::Value), why: &str| {
        let mut invalid = value.clone();
        mutate(&mut invalid);
        assert_invalid("task-review-findings-v1.json", &invalid, why);
        let decoded = serde_json::from_value::<TaskReviewFindingsV1>(invalid);
        assert!(decoded.map_or(true, |r| r.validate().is_err()), "{why}");
    };
    refused(
        &|v| v["findings"][0]["approved"] = json!(true),
        "a finding grants nothing",
    );
    refused(
        &|v| {
            v["findings"][0].as_object_mut().unwrap().remove("severity");
        },
        "a reviewer's finding has a severity",
    );
    refused(
        &|v| v["findings"][1]["file"] = json!("src/lib.rs"),
        "the evaluator's verdict names no place",
    );
    refused(
        &|v| v["continues"]["round"] = json!(0),
        "rounds count from one",
    );
    refused(
        &|v| v["schema"] = json!("af.task-findings/2"),
        "another version is another type",
    );
    refused(
        &|v| v["findings"][0]["title"] = json!("two\nlines"),
        "a title is one line",
    );
    refused(
        &|v| v["missing_reviewers"] = json!([]),
        "an empty set has one spelling: absent",
    );
}

#[test]
fn task_read_first_schema_and_type_agree() {
    let entry = |reference: &str, path: &str| TaskReadFirstEntryV2 {
        reference: reference.into(),
        path: path.into(),
        digest: id('a'),
    };
    let record = TaskReadFirstV2 {
        schema: TASK_READ_FIRST_SCHEMA.into(),
        snapshot_id: id('b'),
        rules: Some(entry(".af/rules.md", ".af/rules.md")),
        context: vec![entry("docs/design.md#items", "docs/design.md")],
    };
    record.validate().unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_valid("task-read-first-v2.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskReadFirstV2>(value.clone()).unwrap(),
        record
    );
    let only_context = json!({"schema":"af.task-read-first/2","snapshot_id":id('b'),
        "context":[value["context"][0]]});
    assert_valid("task-read-first-v2.json", &only_context);
    let empty = json!({"schema":"af.task-read-first/2","snapshot_id":id('b')});
    assert_invalid("task-read-first-v2.json", &empty, "never written empty");
    let decoded: TaskReadFirstV2 = serde_json::from_value(empty).unwrap();
    assert!(decoded.validate().is_err());
    let mut digestless = value.clone();
    digestless["rules"]["digest"] = json!("rules.md");
    assert_invalid("task-read-first-v2.json", &digestless, "bound by digest");
    assert!(
        serde_json::from_value::<TaskReadFirstV2>(digestless)
            .unwrap()
            .validate()
            .is_err()
    );
    // The tree is named: a record without it is the @1 shape, which neither side accepts.
    let mut treeless = value.clone();
    treeless.as_object_mut().unwrap().remove("snapshot_id");
    assert_invalid("task-read-first-v2.json", &treeless, "names its tree");
    assert!(serde_json::from_value::<TaskReadFirstV2>(treeless).is_err());
    let mut first = value;
    first.as_object_mut().unwrap().remove("snapshot_id");
    first["schema"] = json!("af.task-read-first/1");
    assert_invalid("task-read-first-v2.json", &first, "@1 is refused");
    assert!(serde_json::from_value::<TaskReadFirstV2>(first).is_err());
}

#[test]
fn diff_subject_schema_and_type_agree() {
    use review_core::task::diff_subject::*;
    let subject = DiffSubjectV1 {
        schema: DIFF_SUBJECT_SCHEMA.into(),
        base_snapshot_id: id('1'),
        snapshot_id: id('2'),
        base: DIFF_SUBJECT_BASE.into(),
        changes: vec![
            DiffSubjectChangeV1 {
                path: "NOTES.md".into(),
                before: None,
                after: Some(id('3')),
            },
            DiffSubjectChangeV1 {
                path: "pagination.py".into(),
                before: Some(id('4')),
                after: Some(id('5')),
            },
        ],
    };
    subject.validate().unwrap();
    let value = serde_json::to_value(&subject).unwrap();
    assert_valid("diff-subject-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<DiffSubjectV1>(value.clone()).unwrap(),
        subject
    );
    let mut sideless = value.clone();
    sideless["changes"][0]
        .as_object_mut()
        .unwrap()
        .remove("after");
    assert_invalid("diff-subject-v1.json", &sideless, "a change has a side");
    assert!(
        serde_json::from_value::<DiffSubjectV1>(sideless)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut elsewhere = value.clone();
    elsewhere["base"] = json!("/tmp/base");
    assert_invalid("diff-subject-v1.json", &elsewhere, "the base is ../base");
    assert!(
        serde_json::from_value::<DiffSubjectV1>(elsewhere)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut unknown = value;
    unknown["approved"] = json!(true);
    assert_invalid("diff-subject-v1.json", &unknown, "closed");
    assert!(serde_json::from_value::<DiffSubjectV1>(unknown).is_err());
}

#[test]
fn task_base_schema_and_type_agree() {
    let base = TaskBaseV1 {
        schema: TASK_BASE_SCHEMA.into(),
        commit: "a".repeat(40),
        snapshot_id: id('1'),
    };
    base.validate().unwrap();
    let value = serde_json::to_value(&base).unwrap();
    assert_valid("task-base-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskBaseV1>(value.clone()).unwrap(),
        base
    );
    for (commit, why) in [("HEAD", "a resolved commit"), ("ABCDEF", "a full ID")] {
        let mut wrong = value.clone();
        wrong["commit"] = json!(commit);
        assert_invalid("task-base-v1.json", &wrong, why);
        assert!(
            serde_json::from_value::<TaskBaseV1>(wrong)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
