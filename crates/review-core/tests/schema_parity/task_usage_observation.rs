use super::*;
use review_core::task::usage::*;

#[test]
fn native_observation_separates_charge_completeness_from_exact_floor() {
    for value in [
        TaskUsageObservationV1 {
            reported_usage: None,
            charge_complete: false,
        },
        TaskUsageObservationV1 {
            reported_usage: Some(TaskTokenUsageV3::charge_only(0)),
            charge_complete: true,
        },
        TaskUsageObservationV1 {
            reported_usage: Some(TaskTokenUsageV3::charge_only(u128::MAX)),
            charge_complete: false,
        },
    ] {
        value.validate().unwrap();
        let json = serde_json::to_value(&value).unwrap();
        assert_valid("task-usage-observation-v1.json", &json);
        for (field, bad_value) in [
            ("charge_complete", json!(null)),
            ("reported_usage", json!(null)),
            ("reservation", json!(100)),
        ] {
            let mut bad = json.clone();
            bad[field] = bad_value;
            assert_invalid("task-usage-observation-v1.json", &bad, field);
            assert!(serde_json::from_value::<TaskUsageObservationV1>(bad).is_err());
        }
    }
    let bad = TaskUsageObservationV1 {
        reported_usage: None,
        charge_complete: true,
    };
    assert!(bad.validate().is_err());
    assert_invalid(
        "task-usage-observation-v1.json",
        &serde_json::to_value(bad).unwrap(),
        "complete is not unavailable",
    );
    let mut bad = json!({"charge_complete":false,"reported_usage":{"chargeable_tokens":"340282366920938463463374607431768211456"}});
    assert_invalid("task-usage-observation-v1.json", &bad, "wide overflow");
    bad["reported_usage"]["chargeable_tokens"] = json!("00");
    assert_invalid("task-usage-observation-v1.json", &bad, "noncanonical floor");
}

#[test]
fn observations_never_erase_prior_incompleteness_or_sum_cumulative_spend() {
    let previous = TaskUsageObservationV1 {
        reported_usage: Some(TaskTokenUsageV3 {
            input_tokens: Some(u128::MAX.into()),
            ..TaskTokenUsageV3::charge_only(90)
        }),
        charge_complete: false,
    };
    let mut current = TaskUsageObservationV1 {
        reported_usage: Some(TaskTokenUsageV3::charge_only(7)),
        charge_complete: true,
    };
    current.merge_previous(&previous);
    assert!(!current.charge_complete);
    assert_eq!(
        current
            .reported_usage
            .as_ref()
            .unwrap()
            .chargeable_tokens
            .get(),
        90
    );
    assert_eq!(
        current.reported_usage.unwrap().input_tokens.unwrap().get(),
        u128::MAX
    );
}
