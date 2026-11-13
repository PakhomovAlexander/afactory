use super::*;
use af_contracts::task::wake::*;

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn observation() -> Value {
    json!({"schema":WAKE_OBSERVATION_V1,"task":{"store_incarnation":digest('1'),"task_id":"wake-task","task_run_id":format!("task:{}",digest('2')),"opening_event_id":"0123456789abcdef0123456789"},
    "observation_id":digest('3'),"causal":{"sequence":4,"event_id":"0123456789abcdef0123456789"},"checked_prefix":{"sequence":4,"event_id":"0123456789abcdef0123456789"},"revision_id":digest('4'),"episode":1,"kind":"completed","state":{"phase":"finished","outcome":"verified","cause":null,"waiting_reason":null},"evidence":{"kind":"history"},"observed_unix_ms":100,"result_id":digest('5')})
}
fn rust(name: &str, value: Value) -> Result<(), String> {
    macro_rules! read {
        ($t:ty) => {
            serde_json::from_value::<$t>(value)
                .map_err(|e| e.to_string())?
                .validate()
        };
    }
    match name {
        "observation" => read!(WakeObservationV1),
        "checkpoint" => read!(WakeCheckpointV1),
        "subscription" => read!(WakeSubscriptionV1),
        "envelope" => read!(WakeEnvelopeV1),
        "receipt" => read!(WakeReceiptV1),
        _ => unreachable!(),
    }
}

#[test]
fn wake_owner_routes_and_receiver_contracts_are_closed_and_versioned() {
    use af_contracts::task::wake::delivery::*;
    let installation = json!({"executable":"/owner/node","executable_digest":digest('a'),
        "extension":"/owner/extension.ts","extension_digest":digest('b'),"socket":null,"socket_device":null,"socket_inode":null});
    let route = json!({"schema":"af/task-wake-route@1","route_id":digest('1'),"installation_id":digest('2'),
        "capability_digest":digest('3'),"task":observation()["task"],"host":"pi","session":"original_session",
        "mode":"follow_up","expires_unix_ms":1000,"installation":installation});
    let grant =
        json!({"schema":"af/task-wake-grant@1","route_id":digest('1'),"capability":digest('4')});
    let envelope = json!({"schema":WAKE_ENVELOPE_V1,"subscription_id":digest('6'),"delivery_id":digest('a'),"payload_digest":digest('b'),"observation":observation(),"expires_unix_ms":1000});
    let delivery = json!({"schema":"af/task-wake-delivery@1","envelope":envelope,"route_id":digest('1'),
        "attempts":1,"epoch":1,"claim_until_unix_ms":100,"next_attempt_unix_ms":0,"state":"pending","reason":null,"receipts":[]});
    for (schema, value) in [
        ("route", route.clone()),
        ("grant", grant),
        ("delivery", delivery),
    ] {
        let validate = |v: Value| -> Result<(), String> {
            match schema {
                "route" => serde_json::from_value::<WakeRouteV1>(v)
                    .map_err(|e| e.to_string())?
                    .validate(),
                "grant" => serde_json::from_value::<WakeGrantV1>(v)
                    .map_err(|e| e.to_string())?
                    .validate(),
                _ => serde_json::from_value::<WakeDeliveryV1>(v)
                    .map_err(|e| e.to_string())?
                    .validate(),
            }
        };
        let filename = format!("task-wake-{schema}-v1.json");
        assert_valid(&filename, &value);
        validate(value.clone()).unwrap();
        for field in value.as_object().unwrap().keys() {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert_invalid(&filename, &bad, field);
            assert!(validate(bad).is_err());
        }
        for (field, extra) in [
            ("command", json!("arbitrary")),
            ("prompt", json!("injection")),
            ("schema", json!("old")),
        ] {
            let mut bad = value.clone();
            bad[field] = extra;
            assert_invalid(&filename, &bad, field);
            assert!(validate(bad).is_err());
        }
    }
    for (pointer, value) in [
        ("/host", json!("claude")),
        ("/mode", json!("steer")),
        ("/session", json!("wrong\nsession")),
        ("/installation/socket", json!("https://foreign")),
        ("/installation/extension", Value::Null),
    ] {
        let mut bad = route.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert_invalid("task-wake-route-v1.json", &bad, pointer);
        assert!(
            serde_json::from_value::<WakeRouteV1>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
#[test]
fn closed_wake_contracts_match_schemas_and_refuse_authority_fields() {
    let obs = observation();
    let sub = json!({"schema":WAKE_SUBSCRIPTION_V1,"subscription_id":digest('6'),"registration_key":digest('7'),"configuration_digest":digest('8'),"task":obs["task"],"recipient_ref":digest('9'),"registered_unix_ms":1,"expires_unix_ms":1000,"max_observations":16});
    let envelope = json!({"schema":WAKE_ENVELOPE_V1,"subscription_id":digest('6'),"delivery_id":digest('a'),"payload_digest":digest('b'),"observation":obs,"expires_unix_ms":1000});
    let receipt = json!({"schema":WAKE_RECEIPT_V1,"receipt_id":digest('c'),"delivery_id":digest('a'),"payload_digest":digest('b'),"attempt_id":digest('d'),"observed_unix_ms":200,"stage":{"kind":"admission_unknown","intent_id":digest('e')}});
    let checkpoint = json!({"schema":"af/task-wake-checkpoint@1","configuration_digest":digest('1'),
        "run_facts_digest":digest('2'),"through":obs["checked_prefix"],"prior_semantic":[1,digest('3'),obs["state"]],
        "lease_boundary":obs["causal"],"observations":1,"observed_unix_ms":200,
        "prefix_digest":digest('4'),"clock_observed":true});
    for (name, value) in [
        ("observation", obs),
        ("checkpoint", checkpoint),
        ("subscription", sub),
        ("envelope", envelope),
        ("receipt", receipt),
    ] {
        let schema = format!("task-wake-{name}-v1.json");
        assert_valid(&schema, &value);
        rust(name, value.clone()).unwrap();
        for field in [
            "command",
            "next_action",
            "prompt",
            "credentials",
            "url",
            "environment",
        ] {
            let mut bad = value.clone();
            bad[field] = json!("untrusted");
            assert_invalid(&schema, &bad, field);
            assert!(rust(name, bad).is_err());
        }
        for field in value.as_object().unwrap().keys() {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert_invalid(&schema, &bad, field);
            assert!(rust(name, bad).is_err(), "{name}.{field}");
        }
        let mut bad = value.clone();
        bad["schema"] = json!("af/TaskWake@0");
        assert_invalid(&schema, &bad, "unknown version");
        assert!(rust(name, bad).is_err());
    }
    for (pointer, value) in [
        ("/task/task_id", json!("task\n")),
        (
            "/task/opening_event_id",
            json!("0123456789abcdef0123456789\n"),
        ),
        ("/revision_id", json!(format!("{}\n", digest('4')))),
        ("/task/store_incarnation", json!("/tmp/store")),
        ("/task/task_run_id", json!("wake-task")),
        ("/episode", json!(0)),
        ("/state/phase", json!("abandoned")),
        ("/result_id", Value::Null),
        ("/evidence/kind", json!("process_exited")),
    ] {
        let mut bad = observation();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert_invalid("task-wake-observation-v1.json", &bad, pointer);
        assert!(rust("observation", bad).is_err());
    }
}

#[test]
fn wake_states_and_receipt_stages_are_independently_typed() {
    use af_contracts::task::state::*;
    let wrapper = json!({"$ref":"urn:af:schema:task-wake-observation:1#/$defs/state"});
    // The same registry as the public observation validator.
    let mut registry = jsonschema::Registry::new();
    let schema = super::schema("task-wake-observation-v1.json");
    registry = registry
        .add(
            "urn:af:schema:task-wake-observation:1",
            jsonschema::Resource::from_contents(schema),
        )
        .unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry.prepare().unwrap())
        .build(&wrapper)
        .unwrap();
    for phase in TaskStatePhaseV1::ALL {
        for outcome in std::iter::once(None).chain(TaskOutcomeV1::ALL.map(Some)) {
            for cause in std::iter::once(None).chain(TaskCauseV1::ALL.map(Some)) {
                let state = WakeStateV1 {
                    phase,
                    outcome,
                    cause,
                    waiting_reason: None,
                };
                assert_eq!(
                    validator.is_valid(&serde_json::to_value(&state).unwrap()),
                    state.validate().is_ok(),
                    "{state:?}"
                );
            }
        }
    }
    for stage in [
        json!({"kind":"outbox_extracted"}),
        json!({"kind":"inbox_accepted","inbox_ref":digest('1')}),
        json!({"kind":"native_admission_intent","intent_id":digest('2')}),
        json!({"kind":"native_admitted","intent_id":digest('2'),"native_ref":digest('3')}),
        json!({"kind":"admission_unknown","intent_id":digest('2')}),
        json!({"kind":"turn_started","native_ref":digest('3'),"turn_ref":digest('4')}),
        json!({"kind":"report_observed","turn_ref":digest('4'),"report_ref":digest('5')}),
    ] {
        let value = json!({"schema":WAKE_RECEIPT_V1,"receipt_id":digest('1'),"delivery_id":digest('2'),"payload_digest":digest('3'),"attempt_id":digest('4'),"observed_unix_ms":200,"stage":stage});
        assert_valid("task-wake-receipt-v1.json", &value);
        rust("receipt", value).unwrap();
    }
    assert!(decode_bounded::<WakeReceiptV1>(&vec![b' '; MAX_WAKE_BYTES + 1]).is_err());
}
