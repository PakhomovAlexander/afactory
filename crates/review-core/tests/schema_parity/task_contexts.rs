//! Payload schemas supplement exact Store/installed-host context reconstruction; they do
//! not grant the input port, contract, policy or Provider identity named by the payload.
use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

fn id() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn invocation() -> Value {
    json!({"plan_id":id(),"node":"root.worker","inputs":{}})
}
fn worker() -> Value {
    json!({"invocation":invocation(),"feedback_ids":[],"rendered_id":id(),"contract_id":id(),
        "manifest":{"entries":[
            {"name":"instructions","required_by":"captured Worker package","rendered_bytes":4,"estimated_tokens":1},
            {"name":"output_schemas","required_by":"captured Worker result contract","artifact_id":id(),"rendered_bytes":8,"estimated_tokens":2},
            {"name":"reply_format","required_by":"installed Worker protocol","rendered_bytes":12,"estimated_tokens":3}
        ],"rendered_bytes":40,"estimated_tokens":10}})
}

#[test]
fn worker_context_has_closed_fields_exact_identities_and_safe_counters() {
    let value = worker();
    assert_valid("task-context-v1.json", &value);
    for (path, replacement) in [
        ("/invocation/plan_id", json!("mutable-plan")),
        ("/invocation/node", json!("root..worker")),
        ("/rendered_id", json!(null)),
        ("/contract_id", json!("ambient")),
        ("/feedback_ids", json!([id(), id()])),
        ("/manifest/entries", json!([])),
        ("/manifest/entries/0/name", json!("")),
        ("/manifest/entries/1/artifact_id", json!("alias")),
        ("/manifest/entries/0/rendered_bytes", json!(-1)),
        (
            "/manifest/entries/0/estimated_tokens",
            json!(9_007_199_254_740_992_u64),
        ),
        ("/manifest/rendered_bytes", json!(1_048_577)),
        ("/manifest/estimated_tokens", json!(262_145)),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("task-context-v1.json", &invalid, path);
    }
    for (path, key) in [
        ("", "authority"),
        ("/invocation", "effects"),
        ("/manifest", "credential"),
        ("/manifest/entries/0", "hidden_input"),
    ] {
        let mut invalid = value.clone();
        invalid.pointer_mut(path).unwrap()[key] = json!("invented");
        assert_invalid("task-context-v1.json", &invalid, key);
    }
    let mut boundary = value;
    boundary["feedback_ids"] = json!(
        (0..16)
            .map(|n| format!("sha256:{n:064x}"))
            .collect::<Vec<_>>()
    );
    assert_valid("task-context-v1.json", &boundary);
    boundary["feedback_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!(format!("sha256:{:064x}", 16)));
    assert_invalid("task-context-v1.json", &boundary, "feedback bound");
}

#[test]
fn builtin_and_document_contexts_keep_their_distinct_existing_shapes() {
    for (schema, value) in [
        (
            "task-builtin-context-v1.json",
            json!({"invocation":invocation(),"feedback_ids":[],"policy_id":id()}),
        ),
        (
            "document-context-v1.json",
            json!({"invocation":invocation(),"feedback_ids":[]}),
        ),
    ] {
        assert_valid(schema, &value);
        for (path, replacement) in [
            ("/invocation/plan_id", json!("policy")),
            ("/invocation/node", json!("root.")),
            ("/feedback_ids", json!(["unrecorded"])),
            ("/feedback_ids", json!([id(), id()])),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(path).unwrap() = replacement;
            assert_invalid(schema, &invalid, path);
        }
        for key in ["rendered_id", "manifest", "authority", "credentials"] {
            let mut invalid = value.clone();
            invalid[key] = json!({});
            assert_invalid(schema, &invalid, key);
        }
        let mut invalid = value.clone();
        invalid["invocation"]["inputs"]["invalid port"] =
            json!({"artifact_type":"af/Source@1","artifact_ids":[id()],"cardinality":"one"});
        assert_invalid(schema, &invalid, "invalid port name");
    }
    assert_invalid(
        "document-context-v1.json",
        &json!({"invocation":invocation(),"feedback_ids":[],"policy_id":id()}),
        "DocumentContext has no policy field",
    );
    assert_invalid(
        "task-builtin-context-v1.json",
        &json!({"invocation":invocation(),"feedback_ids":[]}),
        "BuiltinContext requires its captured policy",
    );
}

#[test]
fn provider_context_pins_readiness_and_keeps_business_authority_out() {
    let schema = "task-provider-context-v1.json";
    let value = json!({"invocation":invocation(),"capability":{"plan_id":id(),"bindings":["root.writer"],"execution":{"kind":"model","provider":"alias","provider_kind":"fixture","principal_id":"account","model":"resolved","effort":"high"},"invocation_policy_id":id(),"outcome":"passed"},"rendered_id":id(),"manifest":{"entries":[{"name":"capability_probe","required_by":"installed Provider admission","artifact_id":id(),"rendered_bytes":23,"estimated_tokens":6}],"rendered_bytes":23,"estimated_tokens":6}});
    assert_valid(schema, &value);
    for (path, replacement) in [
        (
            "/invocation/inputs",
            json!({"undeclared":{"artifact_type":"af/Source@1","artifact_ids":[id()],"cardinality":"one"}}),
        ),
        ("/capability/execution", json!({"kind":"command"})),
        ("/capability/bindings", json!([])),
        ("/rendered_id", json!("ambient-request")),
        ("/manifest/rendered_bytes", json!(u64::MAX)),
        ("/manifest/entries/0/name", json!("business-input")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid(schema, &invalid, path);
    }
    for key in ["instructions", "feedback_ids", "credentials"] {
        let mut invalid = value.clone();
        invalid[key] = json!("hidden");
        assert_invalid(schema, &invalid, key);
    }
    let mut invalid = value.clone();
    invalid["capability"]["policy"] = json!(id());
    assert_invalid(schema, &invalid, "undeclared capability field");
}
