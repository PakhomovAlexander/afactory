use super::{assert_invalid, assert_valid};
use review_core::task::provider::{TaskProviderAdmissionV2, TaskProviderProbePolicyV1};
use review_core::{TaskBrokerBindingV1, TaskBrokerTargetV1};
use serde_json::{Value, json};

fn policy() -> Value {
    json!({
        "authority_policy_id":format!("sha256:{}", "a".repeat(64)),
        "execution":{"kind":"model","provider":"alias","provider_kind":"fixture","principal_id":"account","model":"resolved-model","effort":"high"},
        "credential_mode":"brokered", "probe_protocol":"af.provider-ok/1",
        "operations":[{"name":"probe","destination":"fixture","method":"generate","max_request_bytes":128,"max_response_bytes":128,"max_calls":1,"max_usage":7}]
    })
}

#[test]
fn provider_probe_policy_has_exact_independent_identity_and_closed_protocol() {
    let value = policy();
    assert_valid("task-provider-probe-policy-v1.json", &value);
    let typed: TaskProviderProbePolicyV1 = serde_json::from_value(value.clone()).unwrap();
    typed.validate().unwrap();
    assert_eq!(
        typed.artifact_refs(),
        vec![typed.authority_policy_id.as_str()]
    );
    assert_eq!(serde_json::to_value(typed).unwrap(), value);
    for (path, replacement) in [
        ("/authority_policy_id", json!("alias")),
        ("/execution", json!({"kind":"command"})),
        ("/execution/principal_id", json!("")),
        ("/execution/model", json!("")),
        ("/credential_mode", json!("trusted_unsafe")),
        ("/probe_protocol", json!("invented")),
        ("/operations", json!([])),
        ("/operations/0/max_usage", json!(0)),
        ("/operations/0/max_calls", json!(4_294_967_296_u64)),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("task-provider-probe-policy-v1.json", &invalid, path);
        assert!(
            serde_json::from_value::<TaskProviderProbePolicyV1>(invalid)
                .map_err(|e| e.to_string())
                .and_then(|p| p.validate())
                .is_err(),
            "{path}"
        );
    }
    for key in ["slot", "invocation_policy_id", "tokens", "credential"] {
        let mut invalid = value.clone();
        invalid[key] = json!("invented");
        assert_invalid("task-provider-probe-policy-v1.json", &invalid, key);
        assert!(serde_json::from_value::<TaskProviderProbePolicyV1>(invalid).is_err());
    }
    // Semantic checks beyond JSON Schema: distinct policies may not reuse an operation name.
    let mut duplicate: TaskProviderProbePolicyV1 = serde_json::from_value(value).unwrap();
    let mut other = duplicate.operations[0].clone();
    other.destination = "another".into();
    duplicate.operations.push(other);
    assert!(duplicate.validate().is_err());
    duplicate.operations[1].name = "second".into();
    duplicate.operations[0].max_usage = 9_007_199_254_740_990;
    assert!(duplicate.validate().is_err());
}

#[test]
fn provider_admission_v2_references_probe_policy_and_does_not_relabel_v1() {
    let probe = policy();
    let value = json!({"plan_id":probe["authority_policy_id"],"bindings":["root.writer"],"execution":probe["execution"],"probe_policy_id":probe["authority_policy_id"],"outcome":"passed"});
    assert_valid("task-provider-admission-v2.json", &value);
    serde_json::from_value::<TaskProviderAdmissionV2>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    assert_invalid(
        "task-provider-admission-v1.json",
        &value,
        "V2 needs its own artifact type",
    );
    for (key, replacement) in [
        ("probe_policy_id", json!("mutable")),
        ("bindings", json!([])),
        ("bindings", json!(["root.writer", "root.writer"])),
        ("bindings", json!(["root..writer"])),
        ("outcome", json!("failed")),
        ("execution", json!({"kind":"command"})),
        ("invocation_policy_id", probe["authority_policy_id"].clone()),
    ] {
        let mut invalid = value.clone();
        invalid[key] = replacement;
        assert_invalid("task-provider-admission-v2.json", &invalid, key);
        assert!(
            serde_json::from_value::<TaskProviderAdmissionV2>(invalid)
                .map_err(|e| e.to_string())
                .and_then(|p| p.validate())
                .is_err(),
            "{key}"
        );
    }
}

#[test]
fn task_broker_target_is_disjoint_and_provider_has_no_worker_slot() {
    let target = TaskBrokerTargetV1::ProviderAdmission {
        probe_policy_id: format!("sha256:{}", "a".repeat(64)),
    };
    target.validate().unwrap();
    let mut value = serde_json::to_value(super::task_broker::binding()).unwrap();
    value["target"] = serde_json::to_value(&target).unwrap();
    assert_valid("task-broker-binding-v1.json", &value);
    let binding: TaskBrokerBindingV1 = serde_json::from_value(value.clone()).unwrap();
    binding.validate().unwrap();
    assert_eq!(
        binding.artifact_refs().last().copied(),
        Some(target.policy_id())
    );
    for replacement in [
        json!({"kind":"provider_admission","probe_policy_id":"invalid"}),
        json!({"kind":"provider_admission","probe_policy_id":target.policy_id(),"slot":"root.writer"}),
        json!({"kind":"worker","probe_policy_id":target.policy_id()}),
        json!({"kind":"worker","slot":"root..writer","invocation_policy_id":target.policy_id()}),
        json!({"kind":"worker","slot":"root.writer","invocation_policy_id":target.policy_id(),"probe_policy_id":target.policy_id()}),
    ] {
        let mut invalid = value.clone();
        invalid["target"] = replacement;
        assert_invalid(
            "task-broker-binding-v1.json",
            &invalid,
            "mixed or malformed target",
        );
        assert!(
            serde_json::from_value::<TaskBrokerBindingV1>(invalid)
                .map_err(|e| e.to_string())
                .and_then(|b| b.validate())
                .is_err()
        );
    }
}

#[test]
fn provider_context_v2_has_only_fixed_probe_inputs_and_exact_manifest() {
    let id = format!("sha256:{}", "a".repeat(64));
    let probe = policy();
    let value = json!({
        "invocation":{"plan_id":id,"node":"root.providers.admit0","inputs":{}},
        "capability":{"plan_id":id,"bindings":["root.writer"],"execution":probe["execution"],"probe_policy_id":id,"outcome":"passed"},
        "rendered_id":id,
        "manifest":{"entries":[{"name":"capability_probe","required_by":"installed Provider admission","artifact_id":id,"rendered_bytes":23,"estimated_tokens":6}],"rendered_bytes":23,"estimated_tokens":6}
    });
    assert_valid("task-provider-context-v2.json", &value);
    for (path, replacement) in [
        (
            "/invocation/inputs",
            json!({"source":{"artifact_ids":[id],"artifact_type":"af/Source@1","cardinality":"one"}}),
        ),
        ("/manifest/rendered_bytes", json!(21)),
        ("/manifest/estimated_tokens", json!(0)),
        ("/manifest/entries/0/name", json!("business_prompt")),
        ("/manifest/entries/0/artifact_id", json!("mutable")),
        ("/manifest/entries/0/rendered_bytes", json!(24)),
        ("/capability/probe_policy_id", json!(null)),
        ("/manifest/entries", json!([])),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("task-provider-context-v2.json", &invalid, path);
    }
    for key in [
        "credential",
        "feedback_ids",
        "instructions",
        "invocation_policy_id",
    ] {
        let mut invalid = value.clone();
        invalid[key] = json!("undeclared");
        assert_invalid("task-provider-context-v2.json", &invalid, key);
    }
}
