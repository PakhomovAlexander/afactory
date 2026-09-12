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
        charged_tokens: u64::MAX,
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
