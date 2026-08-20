use review_core::{EventType, event::validate_event_payload};

#[test]
fn malformed_legacy_incomplete_verdict_is_rejected() {
    let payload = serde_json::json!({
        "outcomes": [{
            "node": "review",
            "status": "failed",
            "detail": "boom",
        }],
        "blocked_gates": [],
        "verdict": "Incomplete { missing: [not-a-Debug-value] }",
        "spent_tokens": null,
    });

    assert!(validate_event_payload(EventType::RunReportV1, &payload).is_err());
}
