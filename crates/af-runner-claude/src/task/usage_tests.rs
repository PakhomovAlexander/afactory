use super::*;
use serde_json::{Value, json};

#[test]
fn malformed_billing_components_preserve_every_unambiguous_contribution() {
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
    ] {
        for bad in [
            None,
            Some(Value::Null),
            Some(json!("3")),
            Some(json!(-1)),
            Some(json!(0.5)),
            Some(serde_json::from_str::<Value>("18446744073709551616").unwrap()),
        ] {
            let mut usage =
                json!({"input_tokens":u64::MAX,"output_tokens":7,"cache_creation_input_tokens":11});
            usage.as_object_mut().unwrap().remove(key);
            if let Some(bad) = &bad {
                usage[key] = bad.clone();
            }
            let (reported, observation) = parse_usage(Some(&json!({"usage":usage})));
            let amount = u128::from(u64::MAX) + 18
                - match key {
                    "input_tokens" => u128::from(u64::MAX),
                    "output_tokens" => 7,
                    _ => 11,
                };
            assert_eq!(reported.unwrap().chargeable_tokens.get(), amount);
            if bad.is_none() && key == "cache_creation_input_tokens" {
                assert!(observation.is_none());
            } else {
                assert!(!observation.unwrap().charge_complete);
            }
        }
    }
    for value in [
        json!({}),
        json!({"usage":null}),
        json!({"usage":"wrong"}),
        json!({"usage":[]}),
    ] {
        let (reported, observation) = parse_usage(Some(&value));
        assert!(reported.is_none());
        assert!(!observation.unwrap().charge_complete);
    }
}

#[test]
fn invalid_cache_read_metadata_refuses_protocol_without_unknown_billing() {
    for bad in [
        Value::Null,
        json!("3"),
        json!(-1),
        json!(0.5),
        serde_json::from_str::<Value>("18446744073709551616").unwrap(),
    ] {
        let (usage, observation) = parse_usage(Some(
            &json!({"usage":{"input_tokens":11,"output_tokens":7,"cache_read_input_tokens":bad}}),
        ));
        assert!(observation.unwrap().charge_complete);
        let usage = usage.unwrap();
        assert_eq!(usage.chargeable_tokens.get(), 18);
        assert!(usage.cache_read_tokens.is_none());
    }
    for value in [
        json!({"usage":{"input_tokens":0,"output_tokens":0}}),
        json!({"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}),
    ] {
        let (usage, observation) = parse_usage(Some(&value));
        assert!(observation.is_none());
        assert_eq!(usage.unwrap().chargeable_tokens.get(), 0);
    }
}

#[test]
fn unparseable_protocol_keeps_the_historical_unknown_convention() {
    let (usage, observation) = parse_usage(None);
    assert!(usage.is_none());
    assert!(observation.is_none(), "no synthetic known-zero bill");
}
