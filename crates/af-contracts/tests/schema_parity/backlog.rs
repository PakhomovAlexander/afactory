use super::{assert_invalid, assert_valid};
use af_contracts::backlog::BacklogRecordV2;
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
            "schema": "af/BacklogBatch@2", "settlement_id": null, "project_id": project, "authority_id": pin,
            "first_sequence": "1", "last_sequence": "3", "report_ids": ["producer-1"],
            "scenario_id": pin, "policy_id": pin
        }),
        json!({
            "schema": "af/BacklogDecision@2", "project_id": project, "batch_id": pin,
            "revision_id": pin, "task_results": [pin], "independent_verification_id": pin,
            "disposition": "verified", "reason": "Supported", "evidence_ids": [pin], "local_match": null
        }),
        json!({
            "schema": "af/BacklogPublication@1", "project_id": project, "item_id": pin,
            "revision_id": pin, "decision_id": pin, "destination_id": pin, "intent_id": pin,
            "attempt_ids": [pin], "remote_id": "42", "provider_state_id": pin, "receipt_id": pin
        }),
    ];
    for record in records {
        let schema = "backlog-record-v2.json";
        let event_type = EventType::BacklogRecordStoredV2;
        assert_valid(schema, &record);
        assert!(validate_event_payload(event_type, &record).is_ok());
        let typed: BacklogRecordV2 = serde_json::from_value(record.clone()).unwrap();
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
                .split_once('@')
                .map(|(name, version)| {
                    format!("{name}@{}", if version == "2" { "1" } else { "99" })
                })
                .unwrap(),
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

#[test]
fn backlog_batch_journal_and_policy_are_closed_and_versioned() {
    use af_contracts::RunEvent;
    use af_contracts::backlog::batch::{Journal, Policy};

    let id = format!("sha256:{}", "a".repeat(64));
    let policy = json!({
        "schema": "af/backlog-batch-policy@2",
        "project_id": id, "authority_id": id, "scenario_id": id, "destination_id": id,
        "timezone": "UTC", "hour": 2, "minute": 30, "start_unix_ms": 1, "lateness_ms": 180000,
        "run_tokens": 1, "run_wall_ms": 10000, "run_dispatches": 2, "rolling_tokens": 3,
        "rolling_window_ms": 86400000, "publication_limit": 1,
        "evidence_access": "metadata_only", "hold_policy": "hold_all"
    });
    let policy_schema = "backlog-batch-policy-v2.json";
    assert_valid(policy_schema, &policy);
    let parsed: Policy = serde_json::from_value(policy.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), policy);
    let mut unsupported = policy.clone();
    unsupported["schema"] = json!("af/backlog-batch-policy@1");
    assert_invalid(policy_schema, &unsupported, "unsupported policy version");
    assert!(serde_json::from_value::<Policy>(unsupported).is_err());

    let variants = [
        json!({"kind":"pinned","policy":policy,"zone":{
            "name":"UTC","initial_offset":0,"transitions":[],"valid_until_unix_seconds":null
        }}),
        json!({"kind":"ticked","at_unix_ms":1,"paused":false,"recurring":false,"source":null,"high_water":"1","members":["report-1",id]}),
        json!({"kind":"ticked","at_unix_ms":1,"paused":false,"recurring":true,"high_water":"1",
            "members":["report-1"],"source":{"commit":"a".repeat(40),"enrollment_revision":id}}),
        json!({"kind":"reconsidered","request":{
            "item_id":id,"report_id":"report-1","evidence_report_id":null,"eligible_unix_ms":2
        }}),
        json!({"kind":"settled","batch_id":id,"member_id":"report-1","settlement":{
            "kind":"reserve_publication","item_id":id
        }}),
        json!({"kind":"closed","batch_id":id,"at_unix_ms":2,
            "pending_deliveries":[{"revision_id":id,"decision_id":id}],
            "resolved_deliveries":[{"revision_id":id,"outcome":"requires_reverification"}],
        }),
    ];
    let event_type = EventType::BacklogBatchStoredV6;
    let record_schema = "backlog-batch-record-v6.json";
    for payload in variants {
        assert_valid(record_schema, &payload);
        validate_event_payload(event_type, &payload).unwrap();
        let parsed: Journal = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), payload);
        let event = json!({
            "schema":"review.kernel/RunEvent@5",
            "event_id":"01jd8m4qz9k7v3n2p6r8t0w1xy","run_id":"backlog-daily-batch",
            "sequence":0,"type":event_type,"occurred_at":"2026-03-06T07:30:00Z","payload":payload
        });
        assert_valid("run-event-v5.json", &event);
        let decoded: RunEvent = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(decoded.event_type, event_type);
        assert_eq!(serde_json::to_value(decoded).unwrap(), event);
        let mut retired = event;
        retired["schema"] = json!("review.kernel/RunEvent@3");
        assert_invalid(
            "run-event-v5.json",
            &retired,
            "retired backlog event envelope",
        );
        assert!(serde_json::from_value::<RunEvent>(retired).is_err());

        let mut extra = payload;
        extra["unexpected"] = json!(true);
        assert_invalid(record_schema, &extra, "unknown record field");
        assert!(validate_event_payload(event_type, &extra).is_err());
    }
    assert!("BacklogBatchStored@5".parse::<EventType>().is_err());
    let old_tick = json!({"kind":"ticked","at_unix_ms":1,"paused":false,"recurring":true,"high_water":"0","members":[]});
    assert_invalid(
        record_schema,
        &old_tick,
        "tick lacks the recorded source field",
    );
    assert!(validate_event_payload(event_type, &old_tick).is_err());
    let oversized = json!({
        "kind":"ticked","at_unix_ms":1,"paused":false,"recurring":false,"source":null,"high_water":"65",
        "members": (1..=65).map(|index| format!("report-{index}")).collect::<Vec<_>>(),
    });
    assert_invalid(
        record_schema,
        &oversized,
        "membership exceeds the citation budget",
    );
    assert!(validate_event_payload(event_type, &oversized).is_err());
    let duplicate = json!({"kind":"ticked","at_unix_ms":1,"paused":false,"recurring":false,"source":null,"high_water":"1","members":["r","r"]});
    assert_invalid(record_schema, &duplicate, "duplicate frozen member");
    assert!(validate_event_payload(event_type, &duplicate).is_err());
}

#[test]
fn portable_batch_reference_round_trips_without_caller_membership_or_policy() {
    use af_contracts::backlog::batch::BatchReference;
    let digest = format!("sha256:{}", "a".repeat(64));
    let value = json!({
        "schema":"af/backlog-batch-reference@1",
        "project_id":digest, "batch_id":digest, "policy_id":digest, "high_water":"15"
    });
    let schema = "backlog-batch-reference-v1.json";
    assert_valid(schema, &value);
    let reference: BatchReference = serde_json::from_value(value.clone()).unwrap();
    reference.validate().unwrap();
    assert_eq!(serde_json::to_value(reference).unwrap(), value);
    let mut extra = value.clone();
    extra["members"] = json!(["caller-nominated"]);
    assert_invalid(schema, &extra, "caller membership");
    assert!(serde_json::from_value::<BatchReference>(extra).is_err());
    let mut zero = value;
    zero["high_water"] = json!("0");
    assert_invalid(schema, &zero, "zero cutoff");
    assert!(
        serde_json::from_value::<BatchReference>(zero)
            .unwrap()
            .validate()
            .is_err()
    );
}
