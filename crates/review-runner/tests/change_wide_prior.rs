use review_core::ReviewerResultContract;
use review_runner::{RESULT_CONTRACT, ReviewerInputs};

#[test]
fn prompt_states_the_change_wide_encoding_used_by_prior_rows() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!([{
            "key": "claim",
            "file": null,
            "title": "change-wide claim"
        }])),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(RESULT_CONTRACT.contains("An empty `file` means the claim is change-wide"));
    assert!(rendered.contains("row's `file` is null and `location_unrecorded` is absent or false"));
    assert!(rendered.contains("When `location_unrecorded` is true"));
    assert!(rendered.contains("re-locate a surviving claim"));
    assert!(rendered.contains("use an empty `file` to report it change-wide"));
    assert!(rendered.contains("re-report it with the same title"));
    assert!(!rendered.contains("confirm it in `disputes`"));
}

#[test]
fn canonical_prior_claims_use_explicit_confirmation() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!([{
            "key": "sha256:claim",
            "file": "src/lib.rs",
            "title": "claim"
        }])),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("confirm it in `disputes`"));
    assert!(!rendered.contains("re-report it with the same title"));
}

#[test]
fn v2_prior_findings_require_dispositions_without_duplicate_reports() {
    let rendered = ReviewerInputs {
        result_contract: ReviewerResultContract::V2,
        prior_findings: Some(serde_json::json!({
            "findings": [{
                "finding_id": "sha256:claim",
                "location_unrecorded": true,
                "title": "claim"
            }]
        })),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("do not emit a second flat report"));
    assert!(rendered.contains("use its `corroborate` disposition"));
    assert!(!rendered.contains("confirming it only in `disputes`"));
}
