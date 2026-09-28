//! `af.code-task-policy/1` and its schema must not drift (ADR-0124). Every policy the Rust
//! validator accepts is valid against the schema, and every `[warm]` shape either one refuses
//! the other refuses too, including `[warm]` together with `require_container = true`.

use review_pipeline::task::code::CodeTaskPolicy;
use serde_json::{Value, json};

fn validator() -> jsonschema::Validator {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let schema: Value = serde_json::from_slice(
        &std::fs::read(root.join("schemas/code-task-policy-v1.json")).unwrap(),
    )
    .unwrap();
    jsonschema::options().build(&schema).unwrap()
}

fn policy(warm: Option<Value>, require_container: bool) -> Value {
    let mut value = json!({
        "schema": "af.code-task-policy/1",
        "checks": {"kernel": {"name": "kernel", "required": true, "command": {
            "program": "bash",
            "args": [{"value": "scripts/verify.sh", "provenance": "literal"}]
        }}},
        "check_wall_ms": 3_600_000,
        "require_container": require_container
    });
    if let Some(warm) = warm {
        value["warm"] = warm;
    }
    value
}

fn rust_accepts(value: &Value) -> bool {
    serde_json::from_value::<CodeTaskPolicy>(value.clone())
        .is_ok_and(|policy| policy.validate().is_ok())
}

#[test]
fn accepted_policies_round_trip_through_the_schema() {
    let schema = validator();
    for warm in [
        None,
        Some(json!({"build_cache": ["cargo_target"]})),
        Some(json!({"caches": ["cargo"]})),
        Some(json!({"build_cache": ["cargo_target"], "caches": ["cargo"], "max_bytes": 1})),
        Some(json!({"build_cache": ["cargo_target"], "max_bytes": 34_359_738_368_u64})),
        Some(json!({"build_cache": ["cargo_target"], "max_bytes": 4096, "hard_max_bytes": 4096})),
        Some(json!({"build_cache": ["cargo_target"], "hard_max_bytes": 34_359_738_368_u64})),
        Some(json!({"build_cache": ["cargo_home"]})),
        Some(json!({"build_cache": ["cargo_target", "cargo_home"]})),
        Some(json!({"build_cache": ["cargo_home", "cargo_target"], "caches": ["cargo"]})),
    ] {
        let value = policy(warm.clone(), false);
        let parsed: CodeTaskPolicy = serde_json::from_value(value.clone()).unwrap();
        parsed.validate().unwrap();
        let written = serde_json::to_value(&parsed).unwrap();
        assert_eq!(written, value, "the captured bytes are the declared ones");
        assert!(schema.is_valid(&written), "{written}");
        if warm.is_none() {
            assert!(written.get("warm").is_none(), "no table, no field");
        }
    }
    assert!(schema.is_valid(&policy(None, true)));
    assert!(rust_accepts(&policy(None, true)));
}

#[test]
fn refused_warm_shapes_are_refused_by_both() {
    let schema = validator();
    let container = policy(Some(json!({"build_cache": ["cargo_target"]})), true);
    let error = serde_json::from_value::<CodeTaskPolicy>(container.clone())
        .unwrap()
        .validate()
        .unwrap_err();
    assert!(
        error.contains("[warm]") && error.contains("require_container = true"),
        "{error}"
    );
    assert!(!schema.is_valid(&container));
    for warm in [
        json!({}),
        json!({"build_cache": []}),
        json!({"build_cache": ["cargo_target", "cargo_target"]}),
        json!({"build_cache": ["cargo_home", "cargo_home"]}),
        json!({"build_cache": ["cargo_home", "rustup_home"]}),
        json!({"caches": ["cargo", "cargo"]}),
        json!({"build_cache": ["target"]}),
        json!({"caches": ["npm"]}),
        json!({"build_cache": ["cargo_target"], "max_bytes": 0}),
        json!({"build_cache": ["cargo_target"], "max_bytes": 34_359_738_369_u64}),
        json!({"build_cache": ["cargo_target"], "max_bytes": null}),
        json!({"build_cache": ["cargo_target"], "hard_max_bytes": 0}),
        json!({"build_cache": ["cargo_target"], "hard_max_bytes": 34_359_738_369_u64}),
        json!({"build_cache": ["cargo_target"], "hard_max_bytes": null}),
        json!({"build_cache": ["cargo_target"], "path": "/tmp/target"}),
    ] {
        let value = policy(Some(warm.clone()), false);
        assert!(!rust_accepts(&value), "Rust accepted {warm}");
        assert!(!schema.is_valid(&value), "the schema accepted {warm}");
    }
    // A hard bound below the eviction bound is refused by the typed policy; the schema cannot
    // compare two fields, so it only bounds each one.
    let below = policy(
        Some(json!({"build_cache": ["cargo_target"], "max_bytes": 8192, "hard_max_bytes": 4096})),
        false,
    );
    assert!(!rust_accepts(&below));
    assert!(schema.is_valid(&below));
    let null = json!({"schema": "af.code-task-policy/1", "checks": {}, "check_wall_ms": 1,
        "require_container": false, "warm": null});
    assert!(!rust_accepts(&null));
    assert!(!schema.is_valid(&null));
}

fn measured(measures: Value, objectives: Option<Value>, warm: Option<Value>) -> Value {
    let mut value = policy(warm, false);
    value["measures"] = measures;
    if let Some(objectives) = objectives {
        value["objectives"] = objectives;
    }
    value
}

fn write(repetitions: u64, warm: bool, metrics: Value) -> Value {
    json!({
        "command": {"program": "python3", "args": [{"value": "measure.py", "provenance": "literal"}]},
        "repetitions": repetitions,
        "warm": warm,
        "wall_ms": 60_000,
        "metrics": metrics
    })
}

/// `[measures]` and `[objectives]` (ADR-0125): every accepted shape is valid against the schema
/// and captured as declared, a number threshold is captured as its canonical decimal text, and
/// every shape either side refuses the other refuses too.
#[test]
fn measures_and_objectives_round_trip_and_refuse_alike() {
    let schema = validator();
    let bytes = json!([{"key": "bytes_written", "unit": "bytes"}]);
    let objective = |extra: Value| {
        let mut value = json!({"measure": "write", "metric": "bytes_written", "direction": "lower",
            "min_improvement_ratio": "0.1", "min_repetitions": 3});
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        json!({"smaller": value})
    };
    for (value, captured_ratio) in [
        (
            measured(json!({"write": write(3, false, bytes.clone())}), None, None),
            None,
        ),
        (
            measured(
                json!({"write": write(3, false, bytes.clone())}),
                Some(objective(json!({}))),
                None,
            ),
            Some("0.1"),
        ),
        (
            measured(
                json!({"write": write(16, true, json!([]))}),
                Some(objective(
                    json!({"metric": "elapsed_ms", "min_improvement_ratio": "0.1"}),
                )),
                Some(json!({"build_cache": ["cargo_target"]})),
            ),
            Some("0.1"),
        ),
        (
            measured(
                json!({"write": write(1, false, bytes.clone())}),
                Some(objective(
                    json!({"min_improvement_ratio": 1, "direction": "higher"}),
                )),
                None,
            ),
            Some("1"),
        ),
    ] {
        assert!(schema.is_valid(&value), "{value}");
        let parsed: CodeTaskPolicy = serde_json::from_value(value.clone()).unwrap();
        parsed.validate().unwrap();
        let written = serde_json::to_value(&parsed).unwrap();
        assert!(schema.is_valid(&written), "{written}");
        match captured_ratio {
            Some(ratio) => assert_eq!(
                written["objectives"]["smaller"]["min_improvement_ratio"],
                ratio
            ),
            None => assert!(written.get("objectives").is_none()),
        }
        // An empty metric list is captured as absent, like every other empty table.
        let mut declared = value["measures"].clone();
        if declared["write"]["metrics"] == json!([]) {
            declared["write"].as_object_mut().unwrap().remove("metrics");
        }
        assert_eq!(written["measures"], declared);
    }
    let without = policy(None, false);
    let parsed: CodeTaskPolicy = serde_json::from_value(without.clone()).unwrap();
    let written = serde_json::to_value(&parsed).unwrap();
    assert!(written.get("measures").is_none() && written.get("objectives").is_none());

    // Shapes both refuse.
    for value in [
        measured(json!({"write": write(0, false, bytes.clone())}), None, None),
        measured(
            json!({"write": write(17, false, bytes.clone())}),
            None,
            None,
        ),
        measured(
            json!({"write": {"command": {"program": "python3", "args": []},
            "repetitions": 1, "warm": false, "wall_ms": 3_600_001}}),
            None,
            None,
        ),
        measured(
            json!({"write": write(3, false, json!([{"key": "elapsed_ms", "unit": "ms"}]))}),
            None,
            None,
        ),
        measured(
            json!({"write": write(3, false, json!([{"key": "b", "unit": "kilobytes"}]))}),
            None,
            None,
        ),
        measured(
            json!({"write": write(3, false, json!([{"key": "b", "unit": "bytes", "note": 1}]))}),
            None,
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"min_improvement_ratio": 1.5}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"min_improvement_ratio": "0.10"}))),
            None,
        ),
        // A float is rounded by the parser before the kernel sees it: refused, not captured.
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"min_improvement_ratio": 0.10}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"min_repetitions": 0}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"min_repetitions": 17}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"direction": "down"}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"extra": true}))),
            None,
        ),
        measured(
            json!({"bad name": write(3, false, bytes.clone())}),
            None,
            None,
        ),
    ] {
        assert!(!rust_accepts(&value), "Rust accepted {value}");
        assert!(!schema.is_valid(&value), "the schema accepted {value}");
    }
    // Cross-table rules only the Rust validator can state: an objective's measure and metric
    // must be declared, and a warm measure needs the Warm Check Cache's cargo_target.
    for value in [
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"measure": "other"}))),
            None,
        ),
        measured(
            json!({"write": write(3, false, bytes.clone())}),
            Some(objective(json!({"metric": "other"}))),
            None,
        ),
        measured(json!({"write": write(3, true, bytes.clone())}), None, None),
        measured(
            json!({"write": write(3, true, bytes.clone())}),
            None,
            Some(json!({"build_cache": ["cargo_home"]})),
        ),
    ] {
        assert!(!rust_accepts(&value), "Rust accepted {value}");
    }
}
