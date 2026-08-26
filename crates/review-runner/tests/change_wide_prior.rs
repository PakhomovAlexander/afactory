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
}
