//! One JSON-Schema registry over `schemas/`, shared by every suite that validates published
//! output.
//!
//! Each published schema is registered under its own `$id`, so a validator built here resolves
//! every `$ref` the shipped closure makes; a dangling one panics at build time rather than
//! silently passing a document.

use serde_json::Value;

/// A validator for one published schema file, resolved against the whole shipped closure.
pub fn validator(name: &str) -> jsonschema::Validator {
    let directory = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("schemas");
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Some(id) = value["$id"].as_str().map(str::to_owned) else {
            continue;
        };
        registry = registry
            .add(id, jsonschema::Resource::from_contents(value))
            .unwrap();
    }
    let root: Value =
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap();
    let registry = registry.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .build(&root)
        .unwrap()
}

/// Assert `value` is valid, reporting every error with its instance path.
pub fn valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
