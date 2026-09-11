//! TOML convenience normalization occurs before artifact capture. Recorded JSON is always
//! loaded through the strict versioned contracts; this parser never normalizes stored bytes.

use review_core::task::pipeline::PipelineDefinitionV1;
use serde_json::{Value, json};

use crate::ConfigError;

pub mod catalog;
pub mod kind;
pub mod shared;

pub fn parse_task_pipeline(text: &str) -> Result<PipelineDefinitionV1, ConfigError> {
    let toml: toml::Value = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let mut value = serde_json::to_value(toml).map_err(|e| ConfigError::Parse(e.to_string()))?;
    if let Some(contract) = value.get_mut("contract") {
        for side in ["inputs", "outputs"] {
            if let Some(ports) = contract.get_mut(side).and_then(Value::as_object_mut) {
                for port in ports.values_mut().filter_map(Value::as_object_mut) {
                    port.entry("covers").or_insert_with(|| json!([]));
                    port.entry("optional").or_insert_with(|| json!(false));
                    port.entry("affinity")
                        .or_insert_with(|| json!({"kind":"unbound"}));
                }
            }
        }
    }
    fn normalize(value: &mut Value) -> Result<(), ConfigError> {
        match value {
            Value::Object(object) => {
                for (name, value) in object {
                    if matches!(
                        name.as_str(),
                        "covers" | "kinds" | "checks" | "independent_from"
                    ) {
                        if let Some(items) = value.as_array_mut() {
                            let mut strings = items
                                .iter()
                                .map(|item| {
                                    item.as_str().map(str::to_owned).ok_or_else(|| {
                                        ConfigError::Parse(format!("{name} requires names"))
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            strings.sort();
                            if strings.windows(2).any(|w| w[0] == w[1]) {
                                return Err(ConfigError::Parse(format!("Duplicate {name} entry")));
                            }
                            *items = strings.into_iter().map(Value::String).collect();
                        }
                    } else {
                        normalize(value)?;
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    normalize(item)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    normalize(&mut value)?;
    let definition: PipelineDefinitionV1 =
        serde_json::from_value(value).map_err(|e| ConfigError::Parse(e.to_string()))?;
    definition.validate().map_err(ConfigError::Binding)?;
    Ok(definition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> PipelineDefinitionV1 {
        let path = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1/pipeline-definition.json");
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn canonical_toml_round_trips_and_unknown_fields_or_versions_are_refused() {
        let definition = definition();
        let text = toml::to_string(&definition).unwrap();
        assert_eq!(parse_task_pipeline(&text).unwrap(), definition);
        assert!(parse_task_pipeline(&format!("unknown = true\n{text}")).is_err());
        assert!(parse_task_pipeline(&text.replace("af.pipeline/1", "af.pipeline/2")).is_err());
    }

    #[test]
    fn human_set_order_is_normalized_but_duplicates_are_not_silently_erased() {
        let text = toml::to_string(&definition()).unwrap();
        let unordered = text.replace(
            "kinds = [\"document\"]",
            "kinds = [\"review\", \"document\"]",
        );
        let parsed = parse_task_pipeline(&unordered).unwrap();
        assert_eq!(
            parsed.accepts.kinds.into_iter().collect::<Vec<_>>(),
            ["document", "review"]
        );
        assert!(
            parse_task_pipeline(&text.replace(
                "kinds = [\"document\"]",
                "kinds = [\"document\", \"document\"]"
            ))
            .is_err()
        );
    }
}
