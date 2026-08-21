use std::collections::BTreeMap;

use review_core::ChangeSetV1;
use review_runner::{ReviewerInputArtifact, ReviewerInputs};

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
    let rendered = ReviewerInputs {
        artifacts: BTreeMap::from([(
            "change_set".into(),
            vec![ReviewerInputArtifact {
                artifact_id: format!("sha256:{}", "c".repeat(64)),
                value: serde_json::to_value(change_set).unwrap(),
            }],
        )]),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains(&encoded));
    assert!(!rendered.contains('\u{fffd}'));
}
