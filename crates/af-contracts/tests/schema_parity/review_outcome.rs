//! CLI serializer parity lives with presentation.rs; these controls independently enforce
//! public shape, wide bounds, authority closure and the distinction between Round and Task.
use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

fn id() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn continuing() -> Value {
    json!({
        "schema":"af/review-outcome@3", "campaign_mode":"heavy",
        "candidate":{"version":"test", "executable":"/tools/af", "binary_sha256":id()},
        "run_id":"campaign",
        "authority":{"authority_snapshot_id":id(), "campaign_manifest_id":id(), "subject_id":id(),
            "head_snapshot_id":id(), "round_event_id":"1".repeat(26), "round":1, "epoch":1},
        "task":{"task_id":"review-task", "revision_id":id(), "plan_id":id(), "phase":{"kind":"running"},
            "result":null, "limits":{"tokens":100, "max_attempts":8, "deadline_unix_ms":1000,
                "verification":{"tokens":0,"attempts":0,"wall_ms":0}},
            "committed_tokens":"7", "begun_attempts":"1", "budget_breached":false},
        "node_outcomes":[], "blocked_gates":[], "attempts":[],
        "totals":{"selected_attempts":{"count":"0", "cost_tokens":"0",
                "context":{"rendered_bytes":"0", "estimated_tokens":"0"}, "usage":{"chargeable_tokens":"0"}},
            "open_required_demands":"0", "open_or_stale_demand_ids":[]},
        "findings":[], "ledger_production":"produced_clean", "available_node_results":[],
        "round_outcome":{"kind":"clean"},
        "outcome":{"kind":"incomplete","missing_nodes":[{"node":"task", "reason":"derived head requires full Review"}]},
        "continuation_required":true,
        "next_action":{"kind":"continue_campaign", "start_another_campaign":false}
    })
}

#[test]
fn review_outcome_wide_counters_and_authority_are_closed() {
    let mut value = continuing();
    assert_valid("review-outcome-v3.json", &value);
    value["task"]["committed_tokens"] = json!(u128::MAX.to_string());
    value["task"]["begun_attempts"] = json!(u64::MAX.to_string());
    value["totals"]["selected_attempts"]["cost_tokens"] = json!(u128::MAX.to_string());
    value["totals"]["selected_attempts"]["usage"]["chargeable_tokens"] =
        json!(u128::MAX.to_string());
    value["attempts"] = json!([{"node":"reviewer", "attempt_id":"b".repeat(26),
        "cost_tokens":u128::MAX.to_string(), "usage":{"input_tokens":u128::MAX.to_string(),
            "chargeable_tokens":u128::MAX.to_string()},
        "context_manifest":{"entries":[], "rendered_bytes":"0", "estimated_tokens":"0"},
        "raw_artifact":id(), "result_artifact":id()}]);
    assert_valid("review-outcome-v3.json", &value);
    for (path, replacement) in [
        (
            "/task/committed_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        ("/task/committed_tokens", json!(1)),
        ("/task/committed_tokens", json!("01")),
        ("/task/committed_tokens", json!("-1")),
        ("/task/begun_attempts", json!("18446744073709551616")),
        (
            "/totals/selected_attempts/usage/chargeable_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        (
            "/attempts/0/cost_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        (
            "/attempts/0/usage/input_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        (
            "/attempts/0/context_manifest/rendered_bytes",
            json!("18446744073709551616"),
        ),
        ("/authority/subject_id", json!("working-tree")),
        ("/task/plan_id", json!("current")),
        ("/task/limits/tokens", json!(9_007_199_254_740_992_u64)),
        ("/next_action/start_another_campaign", json!(true)),
        ("/schema", json!("af/review-outcome@1")),
        ("/schema", json!("af/review-outcome@2")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("review-outcome-v3.json", &invalid, path);
    }
    // `decimalU64` lost its dedicated schema when task-token-usage-v1.json went, so the whole
    // non-canonical matrix is kept on two fields that still carry the pattern.
    for noncanonical in [
        json!(0),
        json!(null),
        json!(""),
        json!("00"),
        json!("01"),
        json!("-1"),
        json!("1e2"),
        json!("\u{661}"),
        json!("18446744073709551616"),
    ] {
        for path in [
            "/totals/open_required_demands",
            "/totals/selected_attempts/count",
            "/attempts/0/context_manifest/rendered_bytes",
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(path).unwrap() = noncanonical.clone();
            assert_invalid("review-outcome-v3.json", &invalid, path);
        }
    }
    for (path, key) in [
        ("", "ambient_authority"),
        ("/task", "replacement_budget"),
        ("/authority", "mutable_subject"),
        ("/totals/selected_attempts/usage", "unobserved_spend"),
    ] {
        let mut invalid = value.clone();
        invalid.pointer_mut(path).unwrap()[key] = json!(true);
        assert_invalid("review-outcome-v3.json", &invalid, key);
    }
}

#[test]
fn review_outcome_never_calls_unfinished_or_inconclusive_pass_done() {
    let mut value = continuing();
    value["outcome"] = json!({"kind":"clean"});
    value["next_action"]["kind"] = json!("done");
    assert_invalid("review-outcome-v3.json", &value, "unverified continuation");
    value["continuation_required"] = json!(false);
    assert_invalid("review-outcome-v3.json", &value, "no finished Task result");
    value["task"]["phase"] = json!({"kind":"finished", "result_id":id()});
    value["task"]["result"] = json!({"task_revision_id":id(), "execution":"completed", "acceptance":"satisfied",
        "domain_conclusion":"canonical accepted Review", "outputs":{"findings":{
            "artifact_ids":[id()], "artifact_type":"review.kernel/FindingSet@1", "cardinality":"one"}},
        "evidence":[id()], "missing_obligations":[]});
    assert_valid("review-outcome-v3.json", &value);
    for (path, replacement) in [
        ("/task/result/acceptance", json!("inconclusive")),
        ("/task/budget_breached", json!(true)),
        ("/task/phase", json!({"kind":"running"})),
        (
            "/round_outcome",
            json!({"kind":"fail", "reason":"not_converged"}),
        ),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("review-outcome-v3.json", &invalid, path);
    }
    let mut terminal_failure = continuing();
    terminal_failure["round_outcome"] = json!({"kind":"fail", "reason":"not_converged"});
    terminal_failure["outcome"] = terminal_failure["round_outcome"].clone();
    assert_valid("review-outcome-v3.json", &terminal_failure);
}
