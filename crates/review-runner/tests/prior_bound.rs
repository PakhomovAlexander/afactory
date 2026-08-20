use review_runner::ReviewerInputs;

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
