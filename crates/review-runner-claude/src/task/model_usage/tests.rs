use super::*;
use serde_json::json;

const OPUS: &str = "claude-opus-5";

fn model(input: Value, output: Value, write: Value) -> Value {
    json!({"inputTokens":input,"outputTokens":output,"cacheCreationInputTokens":write,"cacheReadInputTokens":0})
}

fn response() -> Value {
    json!({"usage":{"input_tokens":2,"output_tokens":9451,"cache_creation_input_tokens":132688,"cache_read_input_tokens":3830},
        "modelUsage":{
            "claude-haiku-4-5-20251001":{"inputTokens":100687,"outputTokens":17,"cacheCreationInputTokens":0,"cacheReadInputTokens":0,"canonicalModel":"claude-haiku-4-5"},
            "claude-opus-5":{"inputTokens":2,"outputTokens":9451,"cacheCreationInputTokens":132688,"cacheReadInputTokens":3830,"canonicalModel":"claude-opus-5"}}})
}

#[test]
fn auxiliary_usage_is_charged_once_even_when_its_model_refuses_output() {
    let result = account(Some(&response()), OPUS);
    assert_eq!(
        result.error,
        Some("Claude model usage reports an unexpected model identity")
    );
    let usage = result.usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 242_845);
    assert_eq!(usage.input_tokens.unwrap().get(), 100_689);
    assert_eq!(usage.output_tokens.unwrap().get(), 9_468);
    assert_eq!(usage.cache_write_tokens.unwrap().get(), 132_688);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 3_830);
    assert!(result.observation.unwrap().charge_complete);
}

#[test]
fn selected_entry_or_whole_map_can_explain_top_level_without_double_counting() {
    let mut value = response();
    value["usage"]["input_tokens"] = json!(100689);
    value["usage"]["output_tokens"] = json!(9468);
    let result = account(Some(&value), OPUS);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 242_845);
    assert!(result.observation.unwrap().charge_complete);

    value["modelUsage"]
        .as_object_mut()
        .unwrap()
        .remove("claude-haiku-4-5-20251001");
    value["usage"]["input_tokens"] = json!(2);
    value["usage"]["output_tokens"] = json!(9451);
    let result = account(Some(&value), OPUS);
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
}

#[test]
fn only_explicit_native_identity_mapping_is_accepted() {
    let mut value = json!({"usage":{"input_tokens":3,"output_tokens":4},
        "modelUsage":{"claude-opus-5-20260101":model(json!(3),json!(4),json!(0))}});
    assert!(
        account(Some(&value), OPUS).error.is_some(),
        "no date stripping"
    );
    value["modelUsage"]["claude-opus-5-20260101"]["canonicalModel"] = json!(OPUS);
    assert!(account(Some(&value), OPUS).error.is_none());
    assert!(
        account(Some(&value), "opus").error.is_some(),
        "no host alias guessing"
    );
    value["modelUsage"] =
        json!({OPUS:{"inputTokens":3,"outputTokens":4,"canonicalModel":"claude-haiku-4-5"}});
    assert!(
        account(Some(&value), OPUS).error.is_some(),
        "contradictory key and canonical ID"
    );
    value["modelUsage"][OPUS]["canonicalModel"] = Value::Null;
    let result = account(Some(&value), OPUS);
    assert!(result.error.is_some());
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn malformed_components_preserve_all_known_model_contributions_and_native_u64_overflow_is_not_zero()
{
    for bad in [
        Value::Null,
        json!("100687"),
        json!(-1),
        json!(0.5),
        serde_json::from_str("18446744073709551616").unwrap(),
    ] {
        let mut value = response();
        value["modelUsage"]["claude-haiku-4-5-20251001"]["inputTokens"] = bad;
        let result = account(Some(&value), OPUS);
        assert!(result.error.is_some());
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_158);
        assert!(!result.observation.unwrap().charge_complete);
    }
    let mut value = response();
    value["modelUsage"]["claude-haiku-4-5-20251001"]
        .as_object_mut()
        .unwrap()
        .remove("inputTokens");
    let result = account(Some(&value), OPUS);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_158);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn exact_widened_components_and_charge_survive_multiple_models() {
    let n = json!(u64::MAX);
    let value = json!({"usage":{"input_tokens":n,"output_tokens":n,"cache_creation_input_tokens":n},
        "modelUsage":{OPUS:model(n.clone(),n.clone(),n.clone()),"other":model(n.clone(),n.clone(),n)}});
    let result = account(Some(&value), OPUS);
    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens.unwrap().get(), 2 * u128::from(u64::MAX));
    assert_eq!(usage.output_tokens.unwrap().get(), 2 * u128::from(u64::MAX));
    assert_eq!(
        usage.cache_write_tokens.unwrap().get(),
        2 * u128::from(u64::MAX)
    );
    assert_eq!(usage.chargeable_tokens.get(), 6 * u128::from(u64::MAX));
    assert!(result.observation.unwrap().charge_complete);
}

#[test]
fn partial_or_conflicting_summaries_keep_a_floor_without_asserting_a_complete_bill() {
    for bad in [Value::Null, json!([]), json!("bad"), json!({})] {
        let mut value = response();
        value["modelUsage"] = bad;
        let result = account(Some(&value), OPUS);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
        assert!(!result.observation.unwrap().charge_complete);
    }
    let mut value = response();
    value["usage"]["input_tokens"] = json!(1_000_000);
    let result = account(Some(&value), OPUS);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 1_142_139);
    assert!(!result.observation.unwrap().charge_complete);
    value["usage"]["input_tokens"] = Value::Null;
    let result = account(Some(&value), OPUS);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 242_845);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn duplicate_canonical_models_are_ambiguous_not_additive() {
    let value = json!({"usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{
        OPUS:{"inputTokens":10,"outputTokens":3,"canonicalModel":OPUS},
        "dated-id":{"inputTokens":12,"outputTokens":2,"canonicalModel":OPUS}}});
    let result = account(Some(&value), OPUS);
    assert!(result.error.is_some());
    let usage = result.usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 14);
    assert!(usage.input_tokens.is_none() && usage.output_tokens.is_none());
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn oversized_maps_retain_bounded_known_contributions_and_never_appear_complete() {
    let mut models = serde_json::Map::new();
    for i in 0..=MAX_MODELS {
        models.insert(format!("model-{i}"), model(json!(1), json!(2), json!(3)));
    }
    let value = json!({"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":models});
    let result = account(Some(&value), OPUS);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 192);
    assert!(!result.observation.unwrap().charge_complete);
    assert!(result.error.is_some());
}

#[test]
fn invalid_or_conflicting_nonbilling_cache_reads_refuse_without_inventing_unknown_billing() {
    for read in [Value::Null, json!(999)] {
        let value = json!({"usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":2},
            "modelUsage":{OPUS:{"inputTokens":3,"outputTokens":4,"cacheReadInputTokens":read}}});
        let result = account(Some(&value), OPUS);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 7);
        assert!(result.error.is_some());
        assert!(result.observation.unwrap().charge_complete);
    }
}

#[test]
fn absent_map_preserves_exact_top_level_contract_including_its_old_observation() {
    for value in [
        json!({"usage":{"input_tokens":11,"output_tokens":7}}),
        json!({"usage":{"input_tokens":11,"output_tokens":7,"cache_read_input_tokens":null}}),
        json!({"usage":{"input_tokens":null,"output_tokens":7}}),
        json!({}),
    ] {
        let original = super::super::parse_usage(Some(&value));
        let result = account(Some(&value), OPUS);
        assert_eq!(result.usage, original.0);
        assert_eq!(result.observation, original.1);
        assert!(result.error.is_none());
    }
}

#[test]
fn explicitly_unused_foreign_models_are_metadata_but_cache_read_activity_is_not() {
    let mut value = response();
    value["modelUsage"]["claude-haiku-4-5-20251001"] = model(json!(0), json!(0), json!(0));
    let result = account(Some(&value), OPUS);
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
    value["modelUsage"]["claude-haiku-4-5-20251001"]["cacheReadInputTokens"] = json!(1);
    let result = account(Some(&value), OPUS);
    assert!(result.error.is_some());
    assert!(result.observation.unwrap().charge_complete);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
    value["modelUsage"]["claude-haiku-4-5-20251001"]
        .as_object_mut()
        .unwrap()
        .remove("cacheReadInputTokens");
    assert!(
        account(Some(&value), OPUS).error.is_some(),
        "unknown activity is not explicitly zero"
    );
}

/// The final result of a 57-turn `claude-opus-5-5` session on native 2.1.285: its breakdown
/// counts one more request than its top-level summary.
fn uncounted_request() -> Value {
    json!({"usage":{"input_tokens":88,"output_tokens":60087,"cache_creation_input_tokens":165142,"cache_read_input_tokens":5375264},
        "modelUsage":{"claude-opus-5-5":{"inputTokens":90,"outputTokens":60095,"cacheCreationInputTokens":166844,"cacheReadInputTokens":5524885,"costUSD":3.641989}}})
}

#[test]
fn a_breakdown_no_top_level_component_exceeds_is_the_whole_bill() {
    let result = account(Some(&uncounted_request()), "claude-opus-5-5");
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
    let usage = result.usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 227_029);
    assert_eq!(usage.input_tokens.unwrap().get(), 90);
    assert_eq!(usage.output_tokens.unwrap().get(), 60_095);
    assert_eq!(usage.cache_write_tokens.unwrap().get(), 166_844);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 5_524_885);

    // The larger bill is charged in full while another model's activity refuses the reply.
    let mut value = response();
    value["usage"]["output_tokens"] = json!(9000);
    let result = account(Some(&value), OPUS);
    assert_eq!(
        result.error,
        Some("Claude model usage reports an unexpected model identity")
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 242_845);
    assert!(result.observation.unwrap().charge_complete);
}

#[test]
fn a_top_level_component_above_the_breakdown_is_never_a_complete_bill() {
    for (key, above) in [
        ("input_tokens", 91),
        ("output_tokens", 60_096),
        ("cache_creation_input_tokens", 166_845),
    ] {
        let mut value = uncounted_request();
        value["usage"][key] = json!(above);
        let result = account(Some(&value), "claude-opus-5-5");
        assert_eq!(
            result.error,
            Some("Claude top-level and per-model usage cannot be reconciled"),
            "{key}"
        );
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 227_029);
        assert!(!result.observation.unwrap().charge_complete, "{key}");
    }
    // The whole top-level summary above the breakdown is spend no reported model explains.
    let mut value = uncounted_request();
    value["usage"] =
        json!({"input_tokens":91,"output_tokens":60096,"cache_creation_input_tokens":166845});
    let result = account(Some(&value), "claude-opus-5-5");
    assert!(result.error.is_some());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 227_032);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn top_level_cache_reads_above_a_covering_breakdown_refuse_without_unknown_billing() {
    let mut value = uncounted_request();
    value["usage"]["cache_read_input_tokens"] = json!(5_524_886);
    let result = account(Some(&value), "claude-opus-5-5");
    assert_eq!(
        result.error,
        Some("Claude top-level and per-model cache-read metadata disagree")
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 227_029);
    assert!(result.observation.unwrap().charge_complete);

    // An absent top-level cache-read counter has nothing to disagree with.
    value["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_read_input_tokens");
    let result = account(Some(&value), "claude-opus-5-5");
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
}
