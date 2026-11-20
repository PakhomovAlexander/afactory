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

#[test]
fn backlog_intake_exchange_and_durable_records_have_closed_schemas() {
    use af_contracts::backlog::intake::{IntakeRecord, Policy, Receipt, Signed, Submission};

    let digest = format!("sha256:{}", "a".repeat(64));
    let report = json!({
        "project_id": digest, "report_id": "r-offline", "workspace_id": "w-0123456789abcdef",
        "captured_unix_ms": "1", "title": "Report", "observed": "Fact", "repro": null,
        "task_id": null, "run_id": null, "source_revision": null, "privacy": "private",
        "evidence": []
    });
    let policy = json!({
        "schema": "af/backlog-intake-policy@1", "project_id": digest,
        "authority_key": "A".repeat(56), "producer_key": null, "enrolled_keys": []
    });
    let submission = json!({
        "schema": "af/backlog-signed-submission@1",
        "body": {
            "schema": "af/backlog-submission@1", "authority_id": digest,
            "producer_id": digest, "report": report,
        },
        "signature": "test signature"
    });
    let receipt = json!({
        "schema": "af/backlog-signed-receipt@1",
        "body": {
            "schema": "af/backlog-authority-receipt@1", "project_id": digest,
            "authority_id": digest, "producer_id": digest, "report_id": "r-offline",
            "report_digest": digest, "sequence": "1"
        },
        "signature": "test signature"
    });
    fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(schema: &str, value: &Value) {
        assert_valid(schema, value);
        let parsed: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(&serde_json::to_value(parsed).unwrap(), value);
        let mut extra = value.clone();
        extra["unexpected"] = json!(true);
        assert_invalid(schema, &extra, "unknown field");
        assert!(serde_json::from_value::<T>(extra).is_err());
    }
    round_trip::<Policy>("backlog-intake-policy-v1.json", &policy);
    round_trip::<Signed<Submission>>("backlog-signed-submission-v1.json", &submission);
    round_trip::<Signed<Receipt>>("backlog-signed-receipt-v1.json", &receipt);

    for record in [
        json!({"schema": "af/BacklogAccepted@1", "report": report, "receipt": receipt}),
        json!({"schema": "af/BacklogAcknowledged@1", "receipt": receipt}),
    ] {
        let event_type = EventType::BacklogIntakeStoredV1;
        round_trip::<IntakeRecord>("backlog-intake-record-v1.json", &record);
        assert!(validate_event_payload(event_type, &record).is_ok());

        let mut wrong = record.clone();
        wrong["receipt"]["body"]["sequence"] = json!("0");
        assert_invalid(
            "backlog-intake-record-v1.json",
            &wrong,
            "sequence starts at one",
        );
        assert!(validate_event_payload(event_type, &wrong).is_err());

        let mut unsupported = record;
        unsupported["schema"] = json!("af/BacklogAccepted@2");
        assert_invalid(
            "backlog-intake-record-v1.json",
            &unsupported,
            "unknown version",
        );
        assert!(validate_event_payload(event_type, &unsupported).is_err());
    }
}
