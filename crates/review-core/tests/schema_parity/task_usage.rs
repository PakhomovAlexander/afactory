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
fn cumulative_execution_accounting_has_one_full_width_encoding() {
    use review_core::task::execution::{TaskExecutionRecordV1, TaskExecutionRecordV3};
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
            assert!(
                normalized.validate().is_err(),
                "accounting is never v1 wire"
            );
            assert_invalid(
                "task-execution-record-v1.json",
                &value,
                "accounting is never v1 wire",
            );
        }
    }
    let numeric = json!({"kind":"usage_observed", "attempt_id":"A".repeat(26),
        "charged_tokens":7, "usage_id":digest, "raw_artifact_ids":[]});
    assert_invalid("task-execution-record-v1.json", &numeric, "numeric charge");
    assert!(serde_json::from_value::<TaskExecutionRecordV1>(numeric).is_err());
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

#[test]
fn native_turn_components_have_an_additive_exact_generation() {
    use review_core::task::usage::{TaskTokenUsageV2, TaskTokenUsageV3};
    for tokens in [
        0,
        u128::from(u64::MAX),
        u128::from(u64::MAX) + 20,
        u128::MAX,
    ] {
        let usage = TaskTokenUsageV3 {
            input_tokens: Some(tokens.into()),
            output_tokens: Some(tokens.into()),
            cache_read_tokens: Some(tokens.into()),
            cache_write_tokens: Some(tokens.into()),
            reasoning_tokens: Some(tokens.into()),
            chargeable_tokens: tokens.into(),
        };
        let value = serde_json::to_value(&usage).unwrap();
        assert_valid("task-token-usage-v3.json", &value);
        review_core::json::admit(&value).unwrap();
        assert_eq!(
            serde_json::from_value::<TaskTokenUsageV3>(value.clone()).unwrap(),
            usage
        );
        assert_eq!(
            TaskTokenUsageV2::try_from(&usage).is_ok(),
            tokens <= u128::from(u64::MAX)
        );
        assert_eq!(
            serde_json::from_value::<TaskTokenUsageV2>(value.clone()).is_ok(),
            tokens <= u128::from(u64::MAX)
        );
        assert_eq!(
            serde_json::from_value::<TaskTokenUsageV1>(value).is_ok(),
            tokens <= u128::from(u64::MAX)
        );
    }
    for field in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "reasoning_tokens",
        "chargeable_tokens",
    ] {
        for bad in [
            json!(0),
            json!(null),
            json!("01"),
            json!("-1"),
            json!("340282366920938463463374607431768211456"),
        ] {
            let mut value = json!({"chargeable_tokens":"0"});
            value[field] = bad;
            assert_invalid("task-token-usage-v3.json", &value, "exact component range");
            assert!(serde_json::from_value::<TaskTokenUsageV3>(value).is_err());
        }
    }
}

#[test]
fn task_review_provenance_upgrades_only_the_charge_generation() {
    use review_core::task::review_compat::{
        TaskReviewAttemptProvenanceV1, TaskReviewAttemptProvenanceV2,
    };
    let id = format!("sha256:{}", "a".repeat(64));
    let mut value = json!({"context_id":id,"task_invocation_id":id,"attempt_id":"a".repeat(26),
        "review_node":"reviewer","result_artifact_id":id,"mutations_artifact_id":id,
        "raw_artifact_id":id,"usage_id":id,"charged_tokens":(u128::from(u64::MAX)+20).to_string()});
    assert_valid("task-review-attempt-provenance-v2.json", &value);
    serde_json::from_value::<TaskReviewAttemptProvenanceV2>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    assert_invalid(
        "task-review-attempt-provenance-v1.json",
        &value,
        "frozen provenance range",
    );
    assert!(serde_json::from_value::<TaskReviewAttemptProvenanceV1>(value.clone()).is_err());
    for bad in [
        json!(null),
        json!("01"),
        json!("340282366920938463463374607431768211456"),
    ] {
        value["charged_tokens"] = bad;
        assert_invalid(
            "task-review-attempt-provenance-v2.json",
            &value,
            "strict provenance range",
        );
        assert!(serde_json::from_value::<TaskReviewAttemptProvenanceV2>(value.clone()).is_err());
    }
}
