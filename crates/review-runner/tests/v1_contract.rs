//! The live wire format reaches the v1 contract, and the bridge is tested.
//!
//! The adapters parse the legacy stage-output shape — that is what `RESULT_CONTRACT` asks a
//! model for, tolerantly. The v1 contract is where those answers are headed, and
//! `LegacyStageOutput::into_reports` is the ingestion bridge. This test walks one answer across
//! the wire contract and the bridge separately: model text → `ReviewerResult@1` flat reports,
//! then live reports → durable FindingReport@1 artifacts.

use std::path::PathBuf;

use review_runner::parse_stage_output;
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn validator(name: &str) -> jsonschema::Validator {
    let schema_root = workspace_root().join("schemas");
    let path = schema_root.join(name);
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let finding_report: Value = serde_json::from_str(
        &std::fs::read_to_string(schema_root.join("finding-report-v1.json")).unwrap(),
    )
    .unwrap();
    jsonschema::options()
        .with_resource(
            "urn:review-kernel:schema:finding-report:1",
            jsonschema::Resource::from_contents(finding_report).unwrap(),
        )
        .build(&schema)
        .unwrap()
}

fn assert_valid(name: &str, instance: &Value) {
    let v = validator(name);
    if !v.is_valid(instance) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();
        panic!("{name} rejected the bridged value: {}", errors.join("; "));
    }
}

#[test]
fn a_contract_shaped_answer_bridges_to_the_v1_result() {
    // Exactly what RESULT_CONTRACT asks for — including a dispute in the v1 `claim_id` form.
    let answer = r#"{"verdict":"request-changes","summary":null,"findings":[
        {"severity":"major","file":"src/main.rs","line":3,"title":"Unbounded loop",
         "body":"spins forever","fix":"bound it","confidence":0.9}
    ],"benchmark_demands":[{"claim":"put is O(1)","why":"unmeasured",
        "suggested_method":"bench 8k puts"}],
      "disputes":[{"claim_id":"ab12cd34ef56","position":"refute","reason":"not reproducible"}]}"#;

    let stage = parse_stage_output(answer).expect("the contract's own shape parses");
    assert_eq!(
        stage.disputes[0].fp, "ab12cd34ef56",
        "claim_id lands in the fp slot"
    );

    let reports: Vec<Value> = stage
        .clone()
        .into_reports()
        .expect("a contract-complete finding imports")
        .iter()
        .map(|report| serde_json::to_value(report).unwrap())
        .collect();
    for report in &reports {
        assert_valid("finding-report-v1.json", report);
    }

    let result = json!({
        "verdict": serde_json::to_value(stage.verdict).unwrap(),
        "summary": stage.summary,
        "reports": serde_json::to_value(&stage.findings).unwrap(),
        "benchmark_demands": serde_json::to_value(&stage.benchmark_demands).unwrap(),
        "disputes": stage.disputes.iter().map(|d| json!({
            "claim_id": d.fp, "position": d.position, "reason": d.reason,
        })).collect::<Vec<_>>(),
    });
    assert_valid("reviewer-result-v1.json", &result);
}

#[test]
fn a_finding_missing_its_fix_parses_but_does_not_bridge() {
    // Tolerance ends at the bridge: the tolerant parse admits a null fix so an expensive
    // answer is not refused outright, and the v1 import is where the rule binds.
    let answer = r#"{"verdict":"block","summary":null,"findings":[
        {"severity":"major","file":"src/main.rs","line":3,"title":"T","body":"B",
         "fix":null,"confidence":0.9}
    ],"benchmark_demands":[],"disputes":[]}"#;
    let stage = parse_stage_output(answer).expect("the tolerant parse admits it");
    assert!(stage.into_reports().is_err(), "the v1 import refuses it");
}
