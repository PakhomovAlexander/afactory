use super::*;
use serde_json::{Value, json};

fn parsed(usage: Option<Value>, prior: bool) -> TaskEvents {
    let mut event = json!({"type":"turn.completed"});
    if let Some(usage) = usage {
        event["usage"] = usage;
    }
    let prior = if prior {
        format!(
            "{}\n",
            json!({"type":"turn.completed","usage":{"input_tokens":u64::MAX,"output_tokens":u64::MAX}})
        )
    } else {
        String::new()
    };
    TaskEvents::parse(format!("{prior}{event}\n").as_bytes())
}

#[test]
fn malformed_required_counters_retain_prior_wide_floor_and_valid_contribution() {
    for prior in [false, true] {
        let base = if prior { 2 * u128::from(u64::MAX) } else { 0 };
        for bad in [
            None,
            Some(Value::Null),
            Some(json!("3")),
            Some(json!(-1)),
            Some(json!(1.5)),
            Some(serde_json::from_str::<Value>("18446744073709551616").unwrap()),
        ] {
            for key in ["input_tokens", "output_tokens"] {
                let mut value = json!({"input_tokens":11,"output_tokens":7});
                value.as_object_mut().unwrap().remove(key);
                if let Some(bad) = &bad {
                    value[key] = bad.clone();
                }
                let events = parsed(Some(value.clone()), prior);
                assert!(events.error.is_some(), "{value}");
                let observation = events.observation().unwrap();
                assert!(!observation.charge_complete, "{value}");
                assert_eq!(
                    observation.reported_usage.unwrap().chargeable_tokens.get(),
                    base + if key == "input_tokens" { 7 } else { 11 },
                    "{value}"
                );
            }
        }
        for bad in [
            None,
            Some(Value::Null),
            Some(json!("usage")),
            Some(json!([])),
        ] {
            let events = parsed(bad, prior);
            assert!(!events.observation().unwrap().charge_complete);
            assert_eq!(
                events
                    .reported_usage()
                    .map_or(0, |u| u.chargeable_tokens.get()),
                base
            );
        }
    }
}

#[test]
fn billing_discount_and_nonbilling_metadata_have_different_completeness() {
    for key in [
        "cached_input_tokens",
        "reasoning_output_tokens",
        "cache_write_input_tokens",
    ] {
        for bad in [
            Value::Null,
            json!("3"),
            json!(-1),
            json!(0.5),
            serde_json::from_str::<Value>("18446744073709551616").unwrap(),
        ] {
            let mut usage = json!({"input_tokens":11,"output_tokens":7});
            usage[key] = bad;
            let events = parsed(Some(usage.clone()), false);
            assert!(events.error.is_some());
            let observation = events.observation().unwrap();
            assert_eq!(
                observation.charge_complete,
                key != "cached_input_tokens",
                "{usage}"
            );
            assert_eq!(
                observation.reported_usage.unwrap().chargeable_tokens.get(),
                if key == "cached_input_tokens" { 7 } else { 18 }
            );
        }
    }
    let events = parsed(
        Some(json!({"input_tokens":1,"cached_input_tokens":2,"output_tokens":7})),
        false,
    );
    assert!(!events.observation().unwrap().charge_complete);
    assert_eq!(events.usage.chargeable_tokens.get(), 7);
    for usage in [
        json!({"input_tokens":0,"output_tokens":0}),
        json!({"input_tokens":0,"output_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"reasoning_output_tokens":0}),
    ] {
        let events = parsed(Some(usage), false);
        assert!(events.error.is_none());
        assert!(events.observation().is_none());
        let usage = events.reported_usage().unwrap();
        assert_eq!(usage.chargeable_tokens.get(), 0);
        assert_eq!(usage.cache_read_tokens.unwrap().get(), 0);
        assert_eq!(usage.reasoning_tokens.unwrap().get(), 0);
    }
}

#[test]
fn absent_protocol_usage_keeps_the_historical_unknown_convention() {
    let events = TaskEvents::parse(
        br#"{"type":"item.completed","item":{"type":"agent_message","text":"OK"}}"#,
    );
    assert!(events.error.is_none());
    assert!(events.reported_usage().is_none());
    assert!(
        events.observation().is_none(),
        "no synthetic known-zero bill"
    );
}
