use super::{assert_invalid, assert_valid};
use review_core::task::plan::WorkerExecutionV1;
use serde_json::{Value, json};

fn id() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn value() -> Value {
    json!({"schema":"af/provider-doctor@2", "ready":true, "run_id":"campaign",
        "authority":{"authority_snapshot_id":id(),"campaign_manifest_id":id(),"subject_id":id(),
            "head_snapshot_id":id(),"round_event_id":"1".repeat(26),"round":1,"epoch":1},
        "task":{"task_id":"review-task","revision_id":id(),"plan_id":id(),"phase":{"kind":"running"},
            "limits":{"tokens":100,"max_attempts":8,"deadline_unix_ms":1000,"verification":{"tokens":0,"attempts":0,"wall_ms":0}},
            "committed_tokens":"3","begun_attempts":"1","budget_breached":false,"deadline_expired":false,"resources_exhausted":false},
        "provider_admissions":[{"node":"root.af_provider_0","bindings":["root.reviewer"],
            "execution":WorkerExecutionV1::Model{provider:"personal".into(),provider_kind:"claude".into(),principal_id:"fixture-account".into(),model:"fixture".into(),effort:"high".into()},
            "outcome":{"kind":"completed","output_id":id(),"admission_ids":[id()]}}],
        "admitted_nodes":["reviewer"],"gates_run":false,"workers_dispatched":false})
}

#[test]
fn provider_doctor_has_exact_identity_and_wide_common_task_accounting() {
    let value = value();
    assert_valid("provider-doctor-v2.json", &value);
    let mut overrun = value.clone();
    overrun["ready"] = json!(false);
    overrun["task"]["committed_tokens"] = json!(u128::MAX.to_string());
    overrun["task"]["begun_attempts"] = json!(u64::MAX.to_string());
    overrun["task"]["budget_breached"] = json!(true);
    overrun["task"]["resources_exhausted"] = json!(true);
    assert_valid("provider-doctor-v2.json", &overrun);
    for (path, replacement) in [
        (
            "/task/committed_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        ("/task/committed_tokens", json!(7)),
        ("/task/committed_tokens", json!("07")),
        ("/task/begun_attempts", json!("18446744073709551616")),
        ("/authority/subject_id", json!("mutable-head")),
        ("/task/plan_id", json!("latest")),
        (
            "/provider_admissions/0/bindings",
            json!(["root.reviewer", "root.reviewer"]),
        ),
        ("/provider_admissions/0/node", json!("root..provider")),
        (
            "/provider_admissions/0/execution",
            json!({"kind":"command"}),
        ),
        ("/provider_admissions/0/execution/principal_id", json!("")),
        ("/provider_admissions/0/outcome/admission_ids", json!([])),
        ("/gates_run", json!(true)),
        ("/workers_dispatched", json!(true)),
        ("/schema", json!("af/provider-doctor@1")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("provider-doctor-v2.json", &invalid, path);
    }
    for (path, key) in [
        ("", "result"),
        ("/task", "replacement_budget"),
        ("/provider_admissions/0/execution", "credential"),
        ("/authority", "ambient_head"),
    ] {
        let mut invalid = value.clone();
        invalid.pointer_mut(path).unwrap()[key] = json!(true);
        assert_invalid("provider-doctor-v2.json", &invalid, key);
    }
}

#[test]
fn provider_doctor_refusals_and_exhausted_task_never_claim_ready() {
    let value = value();
    for (path, replacement) in [
        ("/task/budget_breached", json!(true)),
        ("/task/deadline_expired", json!(true)),
        (
            "/provider_admissions/0/outcome",
            json!({"kind":"failed","error":"paid Provider refused capability"}),
        ),
    ] {
        let mut refused = value.clone();
        *refused.pointer_mut(path).unwrap() = replacement;
        if path.starts_with("/task/") {
            refused["task"]["resources_exhausted"] = json!(true);
        }
        assert_invalid("provider-doctor-v2.json", &refused, "false ready");
        refused["ready"] = json!(false);
        assert_valid("provider-doctor-v2.json", &refused);
    }
    let mut empty = value;
    empty["provider_admissions"] = json!([]);
    empty["admitted_nodes"] = json!([]);
    empty["task"]["committed_tokens"] = json!("0");
    empty["task"]["begun_attempts"] = json!("0");
    assert_valid("provider-doctor-v2.json", &empty); // Command-only plan has no paid probe.
}
