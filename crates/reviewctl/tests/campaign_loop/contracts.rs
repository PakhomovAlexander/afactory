//! Validate actual CLI outputs with the shipped schema closure.
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn valid(name: &str, value: &Value) {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let directory = root.join("schemas");
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if let Some(id) = schema["$id"].as_str().map(str::to_owned) {
            registry = registry
                .add(id, jsonschema::Resource::from_contents(schema))
                .unwrap();
        }
    }
    let schema: Value =
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap();
    let validator = {
        let registry = registry.prepare().unwrap();
        jsonschema::options()
            .with_registry(&registry)
            .build(&schema)
            .unwrap()
    };
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}");
}
