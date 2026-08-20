use review_runner::ReviewerInputs;

#[test]
fn prior_findings_rendering_is_deterministically_bounded() {
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
    }
    .render();

    assert!(
        rendered.len() < 70 * 1024,
        "rendered {} bytes",
        rendered.len()
    );
    assert!(rendered.contains("\"truncated\": true"));
    assert!(rendered.contains("\"total_findings\": 1"));
}
