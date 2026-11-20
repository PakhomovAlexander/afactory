use super::{assert_invalid, assert_valid};
use af_contracts::backlog::BacklogRecordV1;
use af_contracts::event::{EventType, validate_event_payload};
use serde_json::{Value, json};

#[test]
fn backlog_records_and_event_payloads_are_closed_and_versioned() {
    let project = format!("sha256:{}", "a".repeat(64));
    let pin = format!("sha256:{}", "b".repeat(64));
    let records = [
        json!({
            "schema": "af/BacklogReport@1", "project_id": project, "report_id": "producer-1",
            "workspace_id": "w-0123456789abcdef", "captured_unix_ms": "1",
            "title": "Private report", "observed": "A fact", "repro": "Reproduce the fact",
            "task_id": null, "run_id": null, "source_revision": "a".repeat(40),
            "privacy": "private", "evidence": [{"media_type":"text/plain", "text":"Trace", "content_id":pin}]
        }),
        json!({
            "schema": "af/BacklogItemRevision@1", "project_id": project, "item_id": pin,
            "report_ids": ["producer-1"], "claim": "A material claim", "evidence_ids": [pin],
            "source_revision": "a".repeat(40), "proposed": {"title": "Proposed", "body": "Body"}
        }),
        json!({
            "schema": "af/BacklogBatch@1", "project_id": project, "authority_id": pin,
            "first_sequence": "1", "last_sequence": "3", "report_ids": ["producer-1"],
            "scenario_id": pin, "policy_id": pin
        }),
        json!({
            "schema": "af/BacklogDecision@1", "project_id": project, "batch_id": pin,
            "revision_id": pin, "task_results": [pin], "independent_verification_id": pin,
            "disposition": "verified", "reason": "Supported", "evidence_ids": [pin]
        }),
        json!({
            "schema": "af/BacklogPublication@1", "project_id": project, "item_id": pin,
            "revision_id": pin, "decision_id": pin, "destination_id": pin, "intent_id": pin,
            "attempt_ids": [pin], "remote_id": "42", "provider_state_id": pin, "receipt_id": pin
        }),
    ];
    for record in records {
        let schema = "backlog-record-v1.json";
        let event_type = EventType::BacklogRecordStoredV1;
        assert_valid(schema, &record);
        assert!(validate_event_payload(event_type, &record).is_ok());
        let typed: BacklogRecordV1 = serde_json::from_value(record.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), record);

        let mut extra = record.clone();
        extra["unexpected"] = json!(true);
        assert_invalid(schema, &extra, "unknown field");
        assert!(validate_event_payload(event_type, &extra).is_err());

        let mut wrong_version = record;
        wrong_version["schema"] = Value::String(
            wrong_version["schema"]
                .as_str()
                .unwrap()
                .replace("@1", "@2"),
        );
        assert_invalid(schema, &wrong_version, "unknown version");
        assert!(validate_event_payload(event_type, &wrong_version).is_err());
    }
}
