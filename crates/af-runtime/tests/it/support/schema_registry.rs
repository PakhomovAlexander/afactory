//! Local closed schema registry for actual compiler and runtime writer parity.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn validator(file: &str) -> jsonschema::Validator {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let directory = root.join("schemas");
    // nextest runs each case in its own process. Reuse the local registry across the
    // Pipeline, graph and enclosing-contract assertions of that case.
    static REGISTRY: std::sync::OnceLock<jsonschema::Registry<'static>> =
        std::sync::OnceLock::new();
    let registry = REGISTRY.get_or_init(|| {
        let mut registry = jsonschema::Registry::new();
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                let id = schema["$id"].as_str().map(str::to_owned);
                if let Some(id) = id {
                    registry = registry
                        .add(&id, jsonschema::Resource::from_contents(schema))
                        .unwrap();
                }
            }
        }
        registry.prepare().unwrap()
    });
    let schema: Value = if file.starts_with("urn:") {
        serde_json::json!({"$ref":file})
    } else {
        serde_json::from_slice(&std::fs::read(directory.join(file)).unwrap()).unwrap()
    };
    jsonschema::options()
        .with_registry(registry)
        .build(&schema)
        .unwrap()
}
