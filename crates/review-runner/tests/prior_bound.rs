use std::collections::BTreeMap;

use review_core::ChangeSetV1;
use review_runner::{ReviewerInputArtifact, ReviewerInputs};

fn inputs_with_change_set(change_set: serde_json::Value) -> ReviewerInputs {
    ReviewerInputs {
        artifacts: BTreeMap::from([(
            "change_set".into(),
            vec![ReviewerInputArtifact {
                artifact_id: format!("sha256:{}", "c".repeat(64)),
                value: change_set,
            }],
        )]),
        ..ReviewerInputs::default()
    }
}

#[test]
fn oversized_prior_findings_fail_closed_without_silent_truncation() {
    let prior = serde_json::json!({
        "subject_id": format!("sha256:{}", "a".repeat(64)),
        "round": 2,
        "prior_findings": [{
            "key": "large",
            "severity": "blocker",
            "body": "x".repeat(256 * 1024),
        }],
    });
    let rendered = ReviewerInputs {
        prior_findings: Some(prior),
        ..ReviewerInputs::default()
    }
    .render();

    let error = rendered.expect_err("an inexact prompt must never reach a reviewer");
    assert!(error.contains("partitioning is required"), "{error}");
}

#[test]
fn non_utf8_change_set_patches_remain_byte_exact_in_the_prompt() {
    let change_set = ChangeSetV1::new(
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
        vec!["src/raw.txt".into()],
        vec![],
        b"diff --git a/src/raw.txt b/src/raw.txt\n-\xff\n+\xfe\n",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let encoded = change_set.canonical_patch_base64.clone();
    let rendered = inputs_with_change_set(serde_json::to_value(change_set).unwrap())
        .render()
        .unwrap();

    assert!(rendered.contains(&encoded));
    assert!(!rendered.contains('\u{fffd}'));
}

#[test]
fn change_sets_are_fully_revalidated_at_prompt_rendering() {
    let change_set = ChangeSetV1::new(
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
        vec!["src/a.rs".into(), "src/b.rs".into()],
        vec![],
        b"patch",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let mut value = serde_json::to_value(change_set).unwrap();
    value["changed_paths"] = serde_json::json!(["src/b.rs", "src/a.rs"]);

    let error = inputs_with_change_set(value).render().unwrap_err();
    assert!(error.contains("sorted, unique, and relative"), "{error}");
}
