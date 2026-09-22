use review_core::{
    BrokerFailureReasonV1, BrokerOperationOutcomeV1, BrokerOperationReceiptV1,
    BrokerOperationReceiptV2,
};
use serde_json::json;

#[test]
fn exact_broker_receipt_has_a_distinct_bounded_wire_contract() {
    let legacy = BrokerOperationReceiptV1 {
        handle_id: "a".repeat(26),
        node: "reviewer".into(),
        attempt_id: "b".repeat(26),
        lease_epoch: 1,
        operation: "ask".into(),
        destination: "provider.personal".into(),
        method: "inference".into(),
        ordinal: 2,
        outcome: BrokerOperationOutcomeV1::Failed,
        failure_reason: Some(BrokerFailureReasonV1::UsageOverrun),
        request_digest: format!("sha256:{}", "a".repeat(64)),
        response_digest: None,
        request_bytes: 3,
        response_bytes: 0,
        reserved_usage: 10,
        charged_usage: u64::MAX,
    };
    assert!(legacy.validate().is_err());
    let exact = BrokerOperationReceiptV2::from(legacy);
    exact.validate().unwrap();
    let value = serde_json::to_value(&exact).unwrap();
    let schema = serde_json::from_str(include_str!(
        "../../../schemas/broker-operation-receipt-v2.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&value));
    assert_eq!(value["charged_usage"], u64::MAX.to_string());
    assert!(exact.try_into_legacy().is_err());
    for bad in [
        json!(u64::MAX),
        json!("18446744073709551616"),
        json!("00"),
        json!("-1"),
        json!("1e3"),
        json!("1\n"),
        json!(null),
    ] {
        let mut changed = value.clone();
        changed["charged_usage"] = bad;
        assert!(!validator.is_valid(&changed));
        assert!(serde_json::from_value::<BrokerOperationReceiptV2>(changed).is_err());
    }
    for field in ["failure_reason", "response_digest"] {
        let mut changed = value.clone();
        changed[field] = json!(null);
        assert!(!validator.is_valid(&changed));
        assert!(serde_json::from_value::<BrokerOperationReceiptV2>(changed).is_err());
    }
    let mut bad = value;
    bad["extra"] = json!(false);
    assert!(!validator.is_valid(&bad));
    assert!(serde_json::from_value::<BrokerOperationReceiptV2>(bad).is_err());
}

/// The exact receipt keeps the historical receipt's outcome-shape rules: only its charge bound
/// widens. A usage overrun needs a charge above the reservation, and a redacted credential
/// exposure may never charge more than was reserved.
#[test]
fn exact_broker_receipt_keeps_the_outcome_shape_rules() {
    let receipt = BrokerOperationReceiptV1 {
        handle_id: "b".repeat(26),
        node: "correctness".into(),
        attempt_id: "a".repeat(26),
        lease_epoch: 1,
        operation: "model_inference".into(),
        destination: "provider.test".into(),
        method: "responses.create".into(),
        ordinal: 1,
        outcome: BrokerOperationOutcomeV1::Succeeded,
        failure_reason: None,
        request_digest: format!("sha256:{}", "c".repeat(64)),
        response_digest: Some(format!("sha256:{}", "d".repeat(64))),
        request_bytes: 128,
        response_bytes: 256,
        reserved_usage: 1000,
        charged_usage: 900,
    };
    let contradictory = BrokerOperationReceiptV1 {
        outcome: BrokerOperationOutcomeV1::Failed,
        failure_reason: Some(BrokerFailureReasonV1::UsageOverrun),
        ..receipt.clone()
    };
    let redacted_overrun = BrokerOperationReceiptV1 {
        outcome: BrokerOperationOutcomeV1::Failed,
        failure_reason: Some(BrokerFailureReasonV1::UsageOverrun),
        response_digest: None,
        response_bytes: 0,
        charged_usage: 1_001,
        ..receipt.clone()
    };
    let overcharged_exposure = BrokerOperationReceiptV1 {
        failure_reason: Some(BrokerFailureReasonV1::CredentialExposure),
        charged_usage: 1_001,
        ..redacted_overrun.clone()
    };
    for (receipt, valid) in [
        (receipt, true),
        (contradictory, false),
        (redacted_overrun, true),
        (overcharged_exposure, false),
    ] {
        assert_eq!(receipt.validate().is_ok(), valid, "{receipt:?}");
        let exact = BrokerOperationReceiptV2::from(receipt);
        assert_eq!(exact.validate().is_ok(), valid, "{exact:?}");
    }
}
