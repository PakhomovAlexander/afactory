use super::{assert_invalid, assert_valid};
use review_core::task::usage::{DecimalU64, TaskTokenUsageV1};
use serde_json::{Value, json};

#[test]
fn exact_usage_schema_and_canonical_json_retain_the_full_native_range() {
    for tokens in [0, 7, 9_007_199_254_740_992, i64::MAX as u64 + 1, u64::MAX] {
        let usage = TaskTokenUsageV1 {
            input_tokens: Some(tokens.into()),
            chargeable_tokens: tokens.into(),
            ..Default::default()
        };
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(value["chargeable_tokens"], tokens.to_string());
        assert_valid("task-token-usage-v1.json", &value);
        assert_eq!(
            serde_json::from_value::<TaskTokenUsageV1>(value).unwrap(),
            usage
        );
        assert_eq!(usage.input_tokens.map(DecimalU64::get), Some(tokens));
    }
}

#[test]
fn exact_usage_rejects_noncanonical_numbers_and_never_coerces_optional_nulls() {
    for invalid in [
        json!(0),
        json!(null),
        json!(true),
        json!(""),
        json!("00"),
        json!("01"),
        json!("-1"),
        json!("+1"),
        json!(" 1"),
        json!("1 "),
        json!("1\n"),
        json!("1.0"),
        json!("1e2"),
        json!("18446744073709551616"),
        json!("99999999999999999999"),
        json!("184467440737095516150"),
        json!("١"),
    ] {
        for field in ["chargeable_tokens", "input_tokens"] {
            let mut value = json!({"chargeable_tokens":"0"});
            value[field] = invalid.clone();
            assert_invalid("task-token-usage-v1.json", &value, "strict exact usage");
            assert!(serde_json::from_value::<TaskTokenUsageV1>(value).is_err());
        }
    }
    for value in [json!({}), json!({"chargeable_tokens":"0", "extra":0})] {
        assert_invalid("task-token-usage-v1.json", &value, "closed exact usage");
        assert!(serde_json::from_value::<TaskTokenUsageV1>(value).is_err());
    }
    // A decimal payload does not loosen the canonical numeric domain elsewhere.
    let number: Value = json!(u64::MAX);
    assert!(review_core::json::admit(&number).is_err());
}

#[test]
fn decimal_accounting_upgrades_only_its_declared_wire_version() {
    use review_core::task::execution::{
        TaskAttemptResultV1, TaskExecutionRecordV1, TaskExecutionRecordV2,
    };
    let record = TaskExecutionRecordV1::Settled {
        attempt_id: "A".repeat(26),
        charged_tokens: u128::from(u64::MAX),
        result: TaskAttemptResultV1::Failed {
            diagnostic_id: format!("sha256:{}", "1".repeat(64)),
            feedback_id: None,
        },
        raw_artifact_ids: vec![],
        usage_id: None,
    };
    assert!(
        record.validate().is_err(),
        "the released v1 bound remains frozen"
    );
    let upgraded = TaskExecutionRecordV2::from_accounting(&record).unwrap();
    upgraded.validate().unwrap();
    let value = serde_json::to_value(&upgraded).unwrap();
    assert_valid("task-execution-record-v2.json", &value);
    assert_invalid(
        "task-execution-record-v1.json",
        &value,
        "versioned text counter",
    );
    assert_eq!(upgraded.into_record(), record);
    for invalid in [json!(7), json!("01"), json!("18446744073709551616")] {
        let mut refused = value.clone();
        refused["charged_tokens"] = invalid;
        assert_invalid(
            "task-execution-record-v2.json",
            &refused,
            "strict charge encoding",
        );
        assert!(serde_json::from_value::<TaskExecutionRecordV2>(refused).is_err());
    }
    let started = json!({"kind":"started", "attempt_id":"A".repeat(26)});
    assert_valid("task-execution-record-v1.json", &started);
    assert_invalid(
        "task-execution-record-v2.json",
        &started,
        "only accounting has version two",
    );
    assert!(serde_json::from_value::<TaskExecutionRecordV2>(started).is_err());
}

#[test]
fn cumulative_execution_accounting_has_an_additive_full_width_version() {
    use review_core::task::execution::{
        TaskExecutionRecordV1, TaskExecutionRecordV2, TaskExecutionRecordV3,
    };
    let digest = format!("sha256:{}", "1".repeat(64));
    for tokens in [
        0,
        7,
        u128::from(u64::MAX),
        u128::from(u64::MAX) + 7,
        u128::MAX,
    ] {
        for kind in ["settled", "usage_observed"] {
            let mut value = json!({
                "kind": kind,
                "attempt_id": "A".repeat(26),
                "charged_tokens": tokens.to_string(),
                "usage_id": digest,
                "raw_artifact_ids": [digest],
            });
            if kind == "settled" {
                value["result"] = json!({"kind":"abandoned", "diagnostic_id":digest});
            }
            assert_valid("task-execution-record-v3.json", &value);
            review_core::json::admit(&value).unwrap();
            let typed: TaskExecutionRecordV3 = serde_json::from_value(value.clone()).unwrap();
            typed.validate().unwrap();
            assert_eq!(serde_json::to_value(&typed).unwrap(), value);
            let normalized = typed.into_record();
            assert_eq!(
                TaskExecutionRecordV3::from_accounting(&normalized)
                    .unwrap()
                    .into_record(),
                normalized
            );
            assert_eq!(
                normalized.validate().is_ok(),
                tokens <= review_core::json::SAFE_INTEGER_MAX as u128
            );
            assert_eq!(
                TaskExecutionRecordV2::from_accounting(&normalized).is_some(),
                tokens <= u128::from(u64::MAX)
            );
        }
    }
    let legacy = json!({"kind":"usage_observed", "attempt_id":"A".repeat(26),
        "charged_tokens":7, "usage_id":digest, "raw_artifact_ids":[]});
    let record: TaskExecutionRecordV1 = serde_json::from_value(legacy.clone()).unwrap();
    record.validate().unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), legacy);
    assert!(
        TaskExecutionRecordV3::from_accounting(&TaskExecutionRecordV1::Started {
            attempt_id: "A".repeat(26)
        })
        .is_none()
    );
}

#[test]
fn cumulative_usage_keeps_native_components_and_rejects_invalid_decimal_encodings() {
    use review_core::task::{execution::TaskExecutionRecordV3, usage::TaskTokenUsageV2};
    let digest = format!("sha256:{}", "1".repeat(64));
    let usage = TaskTokenUsageV2 {
        input_tokens: Some(u64::MAX.into()),
        chargeable_tokens: u128::MAX.into(),
        ..Default::default()
    };
    let value = serde_json::to_value(&usage).unwrap();
    assert_valid("task-token-usage-v2.json", &value);
    review_core::json::admit(&value).unwrap();
    assert_eq!(
        serde_json::from_value::<TaskTokenUsageV2>(value.clone()).unwrap(),
        usage
    );
    assert_invalid("task-token-usage-v1.json", &value, "v1 does not widen");
    for invalid in [
        json!(7),
        json!(null),
        json!(true),
        json!(""),
        json!("00"),
        json!("01"),
        json!("-1"),
        json!("+1"),
        json!("1\n"),
        json!(" 1"),
        json!("1 "),
        json!("1.0"),
        json!("1e2"),
        json!("١"),
        json!("340282366920938463463374607431768211456"),
        json!("999999999999999999999999999999999999999"),
    ] {
        let mut refused = value.clone();
        refused["chargeable_tokens"] = invalid.clone();
        assert_invalid(
            "task-token-usage-v2.json",
            &refused,
            "canonical cumulative usage",
        );
        assert!(serde_json::from_value::<TaskTokenUsageV2>(refused).is_err());
        let record = json!({"kind":"usage_observed", "attempt_id":"A".repeat(26),
            "charged_tokens":invalid, "usage_id":digest, "raw_artifact_ids":[]});
        assert_invalid(
            "task-execution-record-v3.json",
            &record,
            "canonical cumulative record",
        );
        assert!(serde_json::from_value::<TaskExecutionRecordV3>(record).is_err());
    }
    for invalid in [json!(null), json!("18446744073709551616")] {
        let mut refused = value.clone();
        refused["input_tokens"] = invalid;
        assert_invalid(
            "task-token-usage-v2.json",
            &refused,
            "native components retain their domain",
        );
        assert!(serde_json::from_value::<TaskTokenUsageV2>(refused).is_err());
    }
    for value in [json!({"chargeable_tokens":"0", "extra":0}), json!({})] {
        assert_invalid(
            "task-token-usage-v2.json",
            &value,
            "closed cumulative usage",
        );
        assert!(serde_json::from_value::<TaskTokenUsageV2>(value).is_err());
    }
    let started = json!({"kind":"started", "attempt_id":"A".repeat(26)});
    assert_invalid("task-execution-record-v3.json", &started, "accounting only");
    assert!(serde_json::from_value::<TaskExecutionRecordV3>(started).is_err());
}
