use super::{assert_invalid, assert_valid};
use review_core::task::usage::{DecimalU64, TaskTokenUsageV3};
use serde_json::{Value, json};

#[test]
fn exact_usage_schema_and_canonical_json_retain_the_full_native_range() {
    for tokens in [
        0,
        7,
        9_007_199_254_740_992,
        u128::from(i64::MAX as u64 + 1),
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
        assert_eq!(value["chargeable_tokens"], tokens.to_string());
        assert_valid("task-token-usage-v3.json", &value);
        review_core::json::admit(&value).unwrap();
        assert_eq!(
            serde_json::from_value::<TaskTokenUsageV3>(value).unwrap(),
            usage
        );
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
        json!("340282366920938463463374607431768211456"),
        json!("999999999999999999999999999999999999999"),
        json!("3402823669209384634633746074317682114550"),
        json!("١"),
    ] {
        for field in [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "chargeable_tokens",
        ] {
            let mut value = json!({"chargeable_tokens":"0"});
            value[field] = invalid.clone();
            assert_invalid("task-token-usage-v3.json", &value, "strict exact usage");
            assert!(serde_json::from_value::<TaskTokenUsageV3>(value).is_err());
        }
    }
    for value in [json!({}), json!({"chargeable_tokens":"0", "extra":0})] {
        assert_invalid("task-token-usage-v3.json", &value, "closed exact usage");
        assert!(serde_json::from_value::<TaskTokenUsageV3>(value).is_err());
    }
    // Counters that stay u64 elsewhere (attempt counts, byte ranges) keep their own domain.
    for invalid in ["18446744073709551616", "99999999999999999999", "01", ""] {
        assert!(serde_json::from_value::<DecimalU64>(json!(invalid)).is_err());
    }
    assert_eq!(
        serde_json::from_value::<DecimalU64>(json!(u64::MAX.to_string()))
            .unwrap()
            .get(),
        u64::MAX
    );
    // A decimal payload does not loosen the canonical numeric domain elsewhere.
    let number: Value = json!(u64::MAX);
    assert!(review_core::json::admit(&number).is_err());
}

#[test]
fn cumulative_execution_accounting_has_one_full_width_encoding() {
    use review_core::task::execution::TaskExecutionRecordV1;
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
            assert_valid("task-execution-record-v5.json", &value);
            review_core::json::admit(&value).unwrap();
            let typed: TaskExecutionRecordV1 = serde_json::from_value(value.clone()).unwrap();
            typed.validate().unwrap();
            assert_eq!(serde_json::to_value(&typed).unwrap(), value);
        }
    }
    let numeric = json!({"kind":"usage_observed", "attempt_id":"A".repeat(26),
        "charged_tokens":7, "usage_id":digest, "raw_artifact_ids":[]});
    assert_invalid("task-execution-record-v5.json", &numeric, "numeric charge");
    assert!(serde_json::from_value::<TaskExecutionRecordV1>(numeric).is_err());
}

#[test]
fn cumulative_records_reject_invalid_decimal_encodings() {
    use review_core::task::execution::TaskExecutionRecordV1;
    let digest = format!("sha256:{}", "1".repeat(64));
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
        let record = json!({"kind":"usage_observed", "attempt_id":"A".repeat(26),
            "charged_tokens":invalid, "usage_id":digest, "raw_artifact_ids":[]});
        assert_invalid(
            "task-execution-record-v5.json",
            &record,
            "canonical cumulative record",
        );
        assert!(serde_json::from_value::<TaskExecutionRecordV1>(record).is_err());
    }
}

#[test]
fn task_review_provenance_keeps_the_wide_charge_and_unknown_usage() {
    use review_core::task::campaign_review::TaskReviewAttemptProvenanceV2;
    let id = format!("sha256:{}", "a".repeat(64));
    let schema = "task-review-attempt-provenance-v2.json";
    for charge in [
        0,
        u128::from(u64::MAX),
        u128::from(u64::MAX) + 20,
        u128::MAX,
    ] {
        for known in [false, true] {
            let mut value = json!({"context_id":id, "task_invocation_id":id,
                "attempt_id":"b".repeat(26), "review_node":"reviewer", "result_artifact_id":id,
                "mutations_artifact_id":id, "raw_artifact_id":id,
                "charged_tokens":charge.to_string()});
            if known {
                value["usage_id"] = json!(id);
            }
            assert_valid(schema, &value);
            let typed: TaskReviewAttemptProvenanceV2 =
                serde_json::from_value(value.clone()).unwrap();
            typed.validate().unwrap();
            assert_eq!(typed.charged_tokens.get(), charge);
            assert_eq!(typed.usage_id.is_some(), known);
            for (field, bad) in [
                ("charged_tokens", json!(1)),
                ("charged_tokens", json!(null)),
                ("charged_tokens", json!("01")),
                (
                    "charged_tokens",
                    json!("340282366920938463463374607431768211456"),
                ),
                ("usage_id", json!(null)),
                ("result_artifact_id", json!("missing")),
                ("extra", json!(true)),
            ] {
                let mut bad_value = value.clone();
                bad_value[field] = bad;
                assert_invalid(schema, &bad_value, "closed exact provenance");
                assert!(
                    serde_json::from_value::<TaskReviewAttemptProvenanceV2>(bad_value)
                        .map_or(true, |v| v.validate().is_err())
                );
            }
        }
    }
}
