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
    let result = account(Some(&response()), OPUS, None);
    assert!(result.switched);
    let error = result.error.unwrap();
    assert!(
        error.starts_with(
            "Claude Code switched models without reporting a safety stop, an unexplained switch (model_switched)"
        ),
        "{error}"
    );
    assert!(
        error.contains("the Worker bound claude-opus-5, which wrote 9451 output tokens, but claude-haiku-4-5-20251001 (17 output tokens) answered"),
        "{error}"
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
    let result = account(Some(&value), OPUS, None);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 242_845);
    assert!(result.observation.unwrap().charge_complete);

    value["modelUsage"]
        .as_object_mut()
        .unwrap()
        .remove("claude-haiku-4-5-20251001");
    value["usage"]["input_tokens"] = json!(2);
    value["usage"]["output_tokens"] = json!(9451);
    let result = account(Some(&value), OPUS, None);
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
}

#[test]
fn only_explicit_native_identity_mapping_is_accepted() {
    let mut value = json!({"usage":{"input_tokens":3,"output_tokens":4},
        "modelUsage":{"claude-opus-5-20260101":model(json!(3),json!(4),json!(0))}});
    assert!(
        account(Some(&value), OPUS, None).error.is_some(),
        "no date stripping"
    );
    value["modelUsage"]["claude-opus-5-20260101"]["canonicalModel"] = json!(OPUS);
    assert!(account(Some(&value), OPUS, None).error.is_none());
    assert!(
        account(Some(&value), "opus", None).error.is_some(),
        "no host alias guessing"
    );
    value["modelUsage"] =
        json!({OPUS:{"inputTokens":3,"outputTokens":4,"canonicalModel":"claude-haiku-4-5"}});
    assert!(
        account(Some(&value), OPUS, None).error.is_some(),
        "contradictory key and canonical ID"
    );
    value["modelUsage"][OPUS]["canonicalModel"] = Value::Null;
    let result = account(Some(&value), OPUS, None);
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
        let result = account(Some(&value), OPUS, None);
        assert!(result.error.is_some());
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_158);
        assert!(!result.observation.unwrap().charge_complete);
    }
    let mut value = response();
    value["modelUsage"]["claude-haiku-4-5-20251001"]
        .as_object_mut()
        .unwrap()
        .remove("inputTokens");
    let result = account(Some(&value), OPUS, None);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_158);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn exact_widened_components_and_charge_survive_multiple_models() {
    let n = json!(u64::MAX);
    let value = json!({"usage":{"input_tokens":n,"output_tokens":n,"cache_creation_input_tokens":n},
        "modelUsage":{OPUS:model(n.clone(),n.clone(),n.clone()),"other":model(n.clone(),n.clone(),n)}});
    let result = account(Some(&value), OPUS, None);
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
        let result = account(Some(&value), OPUS, None);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
        assert!(!result.observation.unwrap().charge_complete);
    }
    let mut value = response();
    value["usage"]["input_tokens"] = json!(1_000_000);
    let result = account(Some(&value), OPUS, None);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 1_142_139);
    assert!(!result.observation.unwrap().charge_complete);
    value["usage"]["input_tokens"] = Value::Null;
    let result = account(Some(&value), OPUS, None);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 242_845);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn duplicate_canonical_models_are_ambiguous_not_additive() {
    let value = json!({"usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{
        OPUS:{"inputTokens":10,"outputTokens":3,"canonicalModel":OPUS},
        "dated-id":{"inputTokens":12,"outputTokens":2,"canonicalModel":OPUS}}});
    let result = account(Some(&value), OPUS, None);
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
    let result = account(Some(&value), OPUS, None);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 192);
    assert!(!result.observation.unwrap().charge_complete);
    assert!(result.error.is_some());
}

#[test]
fn invalid_or_conflicting_nonbilling_cache_reads_refuse_without_inventing_unknown_billing() {
    for read in [Value::Null, json!(999)] {
        let value = json!({"usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":2},
            "modelUsage":{OPUS:{"inputTokens":3,"outputTokens":4,"cacheReadInputTokens":read}}});
        let result = account(Some(&value), OPUS, None);
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
        let result = account(Some(&value), OPUS, None);
        assert_eq!(result.usage, original.0);
        assert_eq!(result.observation, original.1);
        assert!(result.error.is_none());
    }
}

#[test]
fn explicitly_unused_foreign_models_are_metadata_but_cache_read_activity_is_not() {
    let mut value = response();
    value["modelUsage"]["claude-haiku-4-5-20251001"] = model(json!(0), json!(0), json!(0));
    let result = account(Some(&value), OPUS, None);
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
    value["modelUsage"]["claude-haiku-4-5-20251001"]["cacheReadInputTokens"] = json!(1);
    let result = account(Some(&value), OPUS, None);
    assert!(result.error.is_some());
    assert!(result.observation.unwrap().charge_complete);
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 142_141);
    value["modelUsage"]["claude-haiku-4-5-20251001"]
        .as_object_mut()
        .unwrap()
        .remove("cacheReadInputTokens");
    let result = account(Some(&value), OPUS, None);
    assert!(!result.switched, "no counter above zero is no switch");
    assert_eq!(
        result.error.as_deref(),
        Some("Claude model usage leaves another model's activity unreported"),
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
    let result = account(Some(&uncounted_request()), "claude-opus-5-5", None);
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
    let result = account(Some(&value), OPUS, None);
    assert!(result.switched);
    assert!(result.error.unwrap().contains("af refused the reply"));
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
        let result = account(Some(&value), "claude-opus-5-5", None);
        assert_eq!(
            result.error.as_deref(),
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
    let result = account(Some(&value), "claude-opus-5-5", None);
    assert!(result.error.is_some());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 227_032);
    assert!(!result.observation.unwrap().charge_complete);
}

#[test]
fn top_level_cache_reads_above_a_covering_breakdown_refuse_without_unknown_billing() {
    let mut value = uncounted_request();
    value["usage"]["cache_read_input_tokens"] = json!(5_524_886);
    let result = account(Some(&value), "claude-opus-5-5", None);
    assert_eq!(
        result.error.as_deref(),
        Some("Claude top-level and per-model cache-read metadata disagree")
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 227_029);
    assert!(result.observation.unwrap().charge_complete);

    // An absent top-level cache-read counter has nothing to disagree with.
    value["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_read_input_tokens");
    let result = account(Some(&value), "claude-opus-5-5", None);
    assert!(result.error.is_none());
    assert!(result.observation.is_none());
}

/// The result Claude Code returned to three Provider-admission Attempts (2026-10-13 23:47,
/// 2026-10-14 00:04, 2026-10-15 01:11) asked for `claude-fable-5-1` at effort `xhigh`: the bound
/// model's safety classifier flagged the first request and `claude-opus-5` answered.
pub(crate) fn recorded_fallback() -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"result":"OK","num_turns":2,
        "safety_stops":2,"stop_reason":"end_turn","fallback_credit":null,
        "usage":{"input_tokens":4,"output_tokens":4,"cache_creation_input_tokens":9488,"cache_read_input_tokens":3001},
        "modelUsage":{
            "claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"},
            "claude-opus-5":{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0,"canonicalModel":"claude-opus-5"}}})
}

#[test]
fn the_recorded_fallback_is_refused_by_name_and_charged_what_it_reports() {
    let result = account(
        Some(&recorded_fallback()),
        "claude-fable-5-1",
        Some("xhigh"),
    );
    assert!(result.switched);
    let error = result.error.unwrap();
    for part in [
        "the safety classifier of claude-fable-5-1 flagged the request and Claude Code switched models, a safety-classifier fallback (model_refused)",
        "the Worker bound claude-fable-5-1 (effort xhigh), which wrote 0 output tokens",
        "claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)",
        "af refused the reply, because af never runs or accepts a model the Worker did not bind",
        "this is neither a credential nor a model-spelling problem",
        "next: ",
    ] {
        assert!(error.contains(part), "{part}: {error}");
    }
    // ADR-0125 is unchanged: the breakdown matches the top-level summary, the bill is complete
    // and it is the sum of both models.
    let usage = result.usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 9496);
    assert_eq!(usage.output_tokens.unwrap().get(), 4);
    assert_eq!(usage.cache_write_tokens.unwrap().get(), 9488);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 3001);
    assert!(result.observation.unwrap().charge_complete);
}

#[test]
fn only_the_bound_model_is_no_switch() {
    let mut value = recorded_fallback();
    value["modelUsage"]
        .as_object_mut()
        .unwrap()
        .remove("claude-opus-5");
    value["usage"] = json!({"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001});
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(!result.switched);
    assert!(result.error.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 3212);
}

/// A reviewer case: the recorded fallback with its top-level `output_tokens` made malformed. The
/// switch still leads, the accounting reasons follow it, and the charge stays what the
/// accounting rules decide: the breakdown, 9496, now incomplete.
#[test]
fn a_malformed_top_level_usage_never_hides_the_recorded_switch() {
    for bad in [Value::Null, json!("4"), json!(-1)] {
        let mut value = recorded_fallback();
        value["usage"]["output_tokens"] = bad;
        let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
        assert!(result.switched);
        let error = result.error.unwrap();
        let hint = crate::hint::compose(
            BoundModel {
                model: "claude-fable-5-1",
                effort: Some("xhigh"),
                provider: None,
            },
            &ModelOutcome::Switched {
                bound_output_tokens: Some(0),
                others: &[OtherModel {
                    model: "claude-opus-5".into(),
                    output_tokens: Some(4),
                }],
                safety_stops: Some(2),
                num_turns: Some(2),
            },
        );
        assert_eq!(
            error,
            format!(
                "{hint}; Claude top-level usage is malformed; Claude top-level and per-model \
                 usage cannot be reconciled"
            )
        );
        let diagnostic = format!("{}{error}", crate::hint::DIAGNOSTIC_PREFIX);
        assert_eq!(crate::hint::find(&diagnostic), Some(hint.as_str()));
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 9496);
        assert!(!result.observation.unwrap().charge_complete);
    }
}

/// A reviewer case: malformed top-level usage, `safety_stops` 0 and one valid active foreign
/// entry, the only one. An unexplained switch, its reasons after it.
#[test]
fn a_lone_active_foreign_entry_beside_malformed_usage_is_an_unexplained_switch() {
    let value = json!({"safety_stops":0,"num_turns":1,
        "usage":{"input_tokens":"2","output_tokens":4,"cache_creation_input_tokens":6278},
        "modelUsage":{OPUS:{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0}}});
    let result = account(Some(&value), "claude-fable-5-1", None);
    assert!(result.switched);
    let error = result.error.unwrap();
    assert!(
        error.starts_with(
            "Claude Code switched models without reporting a safety stop, an unexplained switch \
             (model_switched): the Worker bound claude-fable-5-1, which reported no usage, but \
             claude-opus-5 (4 output tokens) answered (safety_stops 0, num_turns 1)"
        ),
        "{error}"
    );
    assert!(
        error.ends_with("change the input or bind another model; Claude top-level usage is malformed; Claude top-level and per-model usage cannot be reconciled"),
        "{error}"
    );
    assert!(
        crate::hint::find(&format!("{}{error}", crate::hint::DIAGNOSTIC_PREFIX)).is_some(),
        "{error}"
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 6284);
    assert!(!result.observation.unwrap().charge_complete);
}

/// A reason found after the switch follows it too, and a reason found twice is said once.
#[test]
fn reasons_found_after_the_switch_follow_its_hint_once_each() {
    let value = json!({"usage":{"input_tokens":10,"output_tokens":3},"modelUsage":{
        "a-dated-id":{"inputTokens":12,"outputTokens":2,"canonicalModel":OPUS},
        "b-other":{"inputTokens":1,"outputTokens":1},
        "c-dated-id":{"inputTokens":10,"outputTokens":null,"canonicalModel":OPUS},
        "d-dated-id":{"inputTokens":null,"outputTokens":3,"canonicalModel":OPUS}}});
    let result = account(Some(&value), OPUS, None);
    assert!(result.switched);
    let error = result.error.unwrap();
    assert!(error.starts_with("Claude Code switched models"), "{error}");
    let tail = error.split("bind another model; ").nth(1).unwrap();
    assert_eq!(
        tail,
        "Claude per-model usage is malformed; Claude model usage contains ambiguous duplicate \
         identities; Claude top-level and per-model usage cannot be reconciled"
    );
    assert!(!result.observation.unwrap().charge_complete);
}

/// A well-formed foreign key outside grammar G is counted as ADR-0102 says and named
/// `[not printable]`; a Bedrock-style ID is named verbatim.
#[test]
fn a_foreign_key_outside_g_is_counted_but_never_echoed() {
    let mut value = recorded_fallback();
    let opus = value["modelUsage"]["claude-opus-5"].take();
    let map = value["modelUsage"].as_object_mut().unwrap();
    map.remove("claude-opus-5");
    let mut foreign = opus.clone();
    foreign["canonicalModel"] = json!("/Users/alice/keys-1");
    map.insert("/Users/alice/keys-1".into(), foreign);
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(result.switched);
    let error = result.error.unwrap();
    assert!(
        error.contains("but [not printable] (19 bytes) (4 output tokens) answered"),
        "{error}"
    );
    assert!(
        !error.contains("Users") && !error.contains("alice"),
        "{error}"
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 9496);
    assert!(result.observation.unwrap().charge_complete);

    let mut value = recorded_fallback();
    let map = value["modelUsage"].as_object_mut().unwrap();
    map.remove("claude-opus-5");
    let mut bedrock = opus;
    bedrock["canonicalModel"] = json!("us.anthropic.claude-opus-5-5-v1:0");
    map.insert("us.anthropic.claude-opus-5-5-v1:0".into(), bedrock);
    let error = account(Some(&value), "claude-fable-5-1", Some("xhigh"))
        .error
        .unwrap();
    assert!(
        error.contains("but us.anthropic.claude-opus-5-5-v1:0 (4 output tokens) answered"),
        "{error}"
    );
}

/// Round 3, (1): the switch decision reads every entry, however many. 32 explicitly unused
/// entries and one active foreign entry after them: the hint leads and names it, and the
/// map-size reason follows; the charge still reads only the first 32 entries (ADR-0102).
#[test]
fn a_foreign_activity_after_32_unused_entries_is_a_switch() {
    let mut models = serde_json::Map::new();
    for i in 0..MAX_MODELS {
        models.insert(
            format!("claude-unused-{i:02}"),
            model(json!(0), json!(0), json!(0)),
        );
    }
    models.insert(OPUS.into(), model(json!(2), json!(4), json!(6278)));
    let value = json!({"safety_stops":0,"num_turns":1,
        "usage":{"input_tokens":2,"output_tokens":4,"cache_creation_input_tokens":6278},
        "modelUsage":models});
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(result.switched);
    let hint = compose(
        BoundModel {
            model: "claude-fable-5-1",
            effort: Some("xhigh"),
            provider: None,
        },
        &ModelOutcome::Switched {
            bound_output_tokens: None,
            others: &[OtherModel {
                model: OPUS.into(),
                output_tokens: Some(4),
            }],
            safety_stops: Some(0),
            num_turns: Some(1),
        },
    );
    assert_eq!(
        result.error.unwrap(),
        format!(
            "{hint}; Claude model usage exceeds its finite nonempty model bound; Claude top-level \
             and per-model usage cannot be reconciled"
        )
    );
    assert!(
        hint.contains("but claude-opus-5 (4 output tokens) answered"),
        "{hint}"
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 6284);
    assert!(!result.observation.unwrap().charge_complete);
}

/// Every foreign activity is named in map order, wherever it stands, and an entry past the 32nd
/// is named but never charged.
#[test]
fn every_foreign_activity_is_named_in_map_order() {
    let mut value = recorded_fallback();
    let map = value["modelUsage"].as_object_mut().unwrap();
    for i in 0..MAX_MODELS {
        map.insert(
            format!("claude-unused-{i:02}"),
            model(json!(0), json!(0), json!(0)),
        );
    }
    map.insert(
        "claude-haiku-4-5".into(),
        model(json!(1), json!(3), json!(0)),
    );
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(result.switched);
    let error = result.error.unwrap();
    assert!(
        error.contains(
            "which wrote 0 output tokens, but claude-opus-5 (4 output tokens), claude-haiku-4-5 \
             (3 output tokens) answered (safety_stops 2, num_turns 2)"
        ),
        "{error}"
    );
    assert!(
        error.ends_with("; Claude model usage exceeds its finite nonempty model bound"),
        "{error}"
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 9496);
}

/// Round 3, (2): an empty foreign entry beside the bound model reported no activity. It is the
/// malformed-usage refusal only, never a switch.
#[test]
fn an_empty_foreign_entry_is_malformed_usage_not_a_switch() {
    let value = json!({"safety_stops":0,"num_turns":1,
        "usage":{"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001},
        "modelUsage":{
            "claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"},
            OPUS:{}}});
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(!result.switched);
    assert_eq!(
        result.error.as_deref(),
        Some("Claude per-model usage is malformed")
    );
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 3212);
    assert!(!result.observation.unwrap().charge_complete);

    // Malformed counters are never activity, even beside a counter above zero.
    let mut value = value;
    value["modelUsage"][OPUS] = json!({"inputTokens":5,"outputTokens":"4"});
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(!result.switched);
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .starts_with("Claude per-model usage is malformed"),
        "{:?}",
        result.error
    );
}

/// A foreign entry with all four counters at zero is explicitly unused: no switch, no reason.
#[test]
fn an_all_zero_foreign_entry_is_explicitly_unused() {
    let mut value = recorded_fallback();
    value["modelUsage"][OPUS] = model(json!(0), json!(0), json!(0));
    value["usage"] = json!({"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001});
    let result = account(Some(&value), "claude-fable-5-1", Some("xhigh"));
    assert!(!result.switched);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.observation.is_none());
    assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 3212);
}
