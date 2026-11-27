//! Wire conformance for the executable offline navigation evidence reader.
use serde_json::{Value, json};

fn schema() -> Value {
    serde_json::from_str(include_str!(
        "../../../schemas/agent-bench-evidence-v2.json"
    ))
    .unwrap()
}

fn evidence() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/agent-bench/evidence.json")).unwrap()
}

fn typed_schema(kind: &str) -> Value {
    let mut typed = schema();
    typed["$ref"] = json!(format!("#/$defs/{kind}"));
    for key in ["type", "required", "properties", "additionalProperties"] {
        typed.as_object_mut().unwrap().remove(key);
    }
    typed
}

#[test]
fn agent_bench_wire_fixture_and_typed_objects_conform() {
    let schema = schema();
    let bundle = evidence();
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&bundle)
    );
    for (port, kind) in [
        ("schedule", "schedule"),
        ("receipts", "receipt"),
        ("grades", "grade"),
    ] {
        let references = if port == "schedule" {
            vec![&bundle[port]]
        } else {
            bundle[port].as_array().unwrap().iter().collect()
        };
        let typed = typed_schema(kind);
        let validator = jsonschema::validator_for(&typed).unwrap();
        for reference in references {
            let value = &bundle["objects"][reference.as_str().unwrap()];
            assert!(validator.is_valid(value), "{kind}: {value}");
            let mut unknown = value.clone();
            unknown["unexpected"] = json!(true);
            assert!(!validator.is_valid(&unknown));
            let mut old = value.clone();
            old["schema"] = json!("retired/0");
            assert!(!validator.is_valid(&old));
        }
    }
}

#[test]
fn agent_bench_slot_requires_case_and_positive_repetition() {
    let bundle = evidence();
    let schedule = &bundle["objects"][bundle["schedule"].as_str().unwrap()];
    let slot = &schedule["slots"][0];
    let validator = jsonschema::validator_for(&typed_schema("slot")).unwrap();
    assert!(validator.is_valid(slot));
    for key in ["case", "repetition"] {
        let mut missing = slot.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(!validator.is_valid(&missing));
    }
    for repetition in [json!(0), json!(-1), json!(true), json!(null), json!(1.5)] {
        let mut invalid = slot.clone();
        invalid["repetition"] = repetition;
        assert!(!validator.is_valid(&invalid));
    }
    let cases = &bundle["objects"][schedule["cases"].as_str().unwrap()];
    let validator = jsonschema::validator_for(&typed_schema("cases")).unwrap();
    assert!(validator.is_valid(cases));
    let mut empty = cases.clone();
    empty["cases"] = json!([]);
    assert!(!validator.is_valid(&empty));
    let mut unknown = cases.clone();
    unknown["cases"][0]["oracle"] = json!("must not enter subject cases");
    assert!(!validator.is_valid(&unknown));
}

#[test]
fn agent_bench_retired_evidence_and_schedule_versions_fail_closed() {
    let bundle = evidence();
    let validator = jsonschema::validator_for(&schema()).unwrap();
    let mut old = bundle.clone();
    old["schema"] = json!("af.agent-bench-evidence/1");
    assert!(!validator.is_valid(&old));
    let mut schedule = bundle["objects"][bundle["schedule"].as_str().unwrap()].clone();
    schedule["schema"] = json!("af.agent-bench-schedule/1");
    let validator = jsonschema::validator_for(&typed_schema("schedule")).unwrap();
    assert!(!validator.is_valid(&schedule));
}

#[test]
fn agent_bench_wire_refuses_unknown_version_and_fields() {
    let validator = jsonschema::validator_for(&schema()).unwrap();
    let mut value = evidence();
    value["schema"] = json!("af.agent-bench-evidence/0");
    assert!(!validator.is_valid(&value));
    let mut value = evidence();
    value["extra"] = json!(null);
    assert!(!validator.is_valid(&value));
}

#[test]
fn agent_bench_analysis_golden_conforms_without_claiming_improvement() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/agent-bench-analysis-v2.json"
    ))
    .unwrap();
    let mut report: Value =
        serde_json::from_str(include_str!("../../../fixtures/agent-bench/analysis.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&report));
    let mut old = report.clone();
    old["schema"] = json!("af.agent-bench-analysis/1");
    assert!(!validator.is_valid(&old));
    let mut invalid = report.clone();
    invalid["resources"]["native_tokens"]["relative_difference"]["denominator"] = json!(0);
    assert!(!validator.is_valid(&invalid));
    report["conclusion"] = json!("improved");
    assert!(!validator.is_valid(&report));
}
