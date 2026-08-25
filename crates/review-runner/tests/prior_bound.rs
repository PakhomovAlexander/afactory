use std::collections::BTreeMap;
use std::sync::Arc;

use review_core::ChangeSetV1;
use review_runner::{ReviewerInputArtifact, ReviewerInputs};

fn inputs_with_change_set(change_set: serde_json::Value) -> Result<ReviewerInputs, String> {
    let encoded = review_store::canonical::canonicalize(&change_set).unwrap();
    let artifact_id = review_store::canonical::blob_content_id(&encoded);
    Ok(ReviewerInputs {
        artifacts: BTreeMap::from([(
            "change_set".into(),
            vec![ReviewerInputArtifact::change_set_from_encoded(
                artifact_id,
                &encoded,
            )?],
        )]),
        ..ReviewerInputs::default()
    })
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
        .unwrap()
        .render()
        .unwrap();

    assert!(rendered.contains(&encoded));
    assert!(!rendered.contains('\u{fffd}'));
}

#[test]
fn encoded_change_sets_are_fully_validated_before_prompt_rendering() {
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

    let error = inputs_with_change_set(value).unwrap_err();
    assert!(error.contains("sorted, unique, and relative"), "{error}");
}

#[test]
fn prevalidated_change_sets_must_match_their_id_and_encoded_length() {
    let change_set = Arc::new(
        ChangeSetV1::new(
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
            vec!["src/a.rs".into()],
            vec![],
            b"patch",
            "git version test",
            "review.kernel/git-tree-diff@test",
        )
        .unwrap(),
    );
    let value = serde_json::to_value(change_set.as_ref()).unwrap();
    let encoded = review_store::canonical::canonicalize(&value).unwrap();
    let artifact_id = review_store::canonical::blob_content_id(&encoded);

    assert!(
        ReviewerInputArtifact::pre_validated_change_set(
            artifact_id.clone(),
            Arc::clone(&change_set),
            encoded.len(),
        )
        .is_ok()
    );
    assert!(
        ReviewerInputArtifact::pre_validated_change_set(
            format!("sha256:{}", "c".repeat(64)),
            Arc::clone(&change_set),
            encoded.len(),
        )
        .is_err()
    );
    assert!(
        ReviewerInputArtifact::pre_validated_change_set(
            artifact_id,
            change_set,
            encoded.len() + 1,
        )
        .is_err()
    );
}
