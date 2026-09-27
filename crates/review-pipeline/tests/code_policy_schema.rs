//! `af.code-task-policy/1` and its schema must not drift (ADR-0123). Every policy the Rust
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
        json!({"caches": ["cargo", "cargo"]}),
        json!({"build_cache": ["target"]}),
        json!({"caches": ["npm"]}),
        json!({"build_cache": ["cargo_target"], "max_bytes": 0}),
        json!({"build_cache": ["cargo_target"], "max_bytes": 34_359_738_369_u64}),
        json!({"build_cache": ["cargo_target"], "max_bytes": null}),
        json!({"build_cache": ["cargo_target"], "path": "/tmp/target"}),
    ] {
        let value = policy(Some(warm.clone()), false);
        assert!(!rust_accepts(&value), "Rust accepted {warm}");
        assert!(!schema.is_valid(&value), "the schema accepted {warm}");
    }
    let null = json!({"schema": "af.code-task-policy/1", "checks": {}, "check_wall_ms": 1,
        "require_container": false, "warm": null});
    assert!(!rust_accepts(&null));
    assert!(!schema.is_valid(&null));
}
