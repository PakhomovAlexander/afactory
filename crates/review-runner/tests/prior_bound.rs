use std::collections::BTreeMap;

use review_core::{ChangeSetV1, SubjectV1};
use review_runner::{MAX_CHANGE_SET_BYTES, ReviewerInputArtifact, ReviewerInputs};

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
    assert!(error.contains("reject, group, or fix Findings"), "{error}");
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
fn resolved_change_sets_bind_verified_bytes_without_requiring_reserialization_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(directory.path()).unwrap();
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let change_set = ChangeSetV1::new(
        base.clone(),
        head.clone(),
        vec!["src/a.rs".into()],
        vec![],
        b"patch",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let mut value = serde_json::to_value(change_set).unwrap();
    // This is schema-valid but the Rust serializer deliberately omits the false default. The
    // verified stored bytes, not a later re-serialization, remain artifact identity authority.
    value["rename_detection_truncated"] = serde_json::Value::Bool(false);
    let encoded = review_store::canonical::canonicalize(&value).unwrap();
    let artifact_id = cas.put(&encoded).unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(head, base, artifact_id.clone())).unwrap())
        .unwrap();
    let resolved = review_store::resolve_subject(&cas, &subject_id).unwrap();
    let resolved_change_set = resolved.change_set.unwrap();
    assert_eq!(resolved_change_set.artifact_id(), artifact_id);
    assert_eq!(resolved_change_set.encoded_bytes(), encoded.len());

    let input = ReviewerInputArtifact::from_resolved_change_set(resolved_change_set).unwrap();
    assert_eq!(input.artifact_id(), artifact_id);
    let inputs = ReviewerInputs {
        artifacts: BTreeMap::from([("renamed_diff".into(), vec![input])]),
        ..ReviewerInputs::default()
    };
    let command_document = serde_json::to_value(&inputs).unwrap();
    let delivered = &command_document["artifacts"]["renamed_diff"][0];
    assert_eq!(
        delivered["artifact_type"],
        review_core::contract::CHANGE_SET_V1
    );
    assert_eq!(delivered["value"]["changed_paths"][0], "src/a.rs");
    assert!(delivered["value"]["canonical_patch_base64"].is_string());
}

#[test]
fn oversized_change_sets_are_refused_while_resolving_subject_authority() {
    let directory = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(directory.path()).unwrap();
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let patch = vec![b'x'; MAX_CHANGE_SET_BYTES * 3 / 4 + 1024];
    let change_set = ChangeSetV1::new(
        base.clone(),
        head.clone(),
        vec!["src/a.rs".into()],
        vec![],
        &patch,
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let encoded = serde_json::to_vec(&change_set).unwrap();
    assert!(encoded.len() > MAX_CHANGE_SET_BYTES);
    let artifact_id = cas.put(&encoded).unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(head, base, artifact_id)).unwrap())
        .unwrap();
    let error = review_store::resolve_subject(&cas, &subject_id).unwrap_err();
    assert!(error.to_string().contains("limit is 4194304"), "{error}");
}
