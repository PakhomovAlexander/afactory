use review_runner::{RESULT_CONTRACT_V2, ReviewerInputs};

#[test]
fn prompt_states_the_change_wide_encoding_used_by_prior_rows() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!({
            "findings": [{
                "finding_id": "sha256:claim",
                "file": null,
                "title": "change-wide claim"
            }]
        })),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(RESULT_CONTRACT_V2.contains("An empty `file` means the claim is change-wide"));
    assert!(rendered.contains("row's `file` is null and `location_unrecorded` is absent or false"));
    assert!(rendered.contains("When `location_unrecorded` is true"));
    assert!(rendered.contains("use an empty `file` to report it change-wide"));
}

#[test]
fn prior_findings_require_dispositions_without_duplicate_reports() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!({
            "findings": [{
                "finding_id": "sha256:claim",
                "location_unrecorded": true,
                "title": "claim"
            }]
        })),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("do not emit a second flat report"));
    assert!(rendered.contains("use its `corroborate` disposition"));
    assert!(!rendered.contains("`disputes`"));
}
