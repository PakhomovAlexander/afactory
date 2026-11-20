//! Current Pipeline capabilities preserve recovery bounds and refuse retired identities.

use af_contracts::task::pipeline::PipelineDefinition;
use serde_json::{Value, json};

use super::{assert_invalid, assert_valid};

const PIPELINE: &str = "pipeline-definition-v6.json";

fn fixture(version: &str) -> Value {
    let path = super::workspace_root()
        .join("fixtures/task-contracts")
        .join(version)
        .join("pipeline-definition.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Rust admission of a pipeline: the typed round trip and its validation.
fn admitted(value: &Value) -> Result<(), String> {
    let definition: PipelineDefinition =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    definition.validate()?;
    assert_eq!(&serde_json::to_value(&definition).unwrap(), value);
    Ok(())
}

fn labelled(value: &Value, schema: &str) -> Value {
    let mut changed = value.clone();
    changed["schema"] = json!(schema);
    changed
}

#[test]
fn recovery_is_optional_bounded_and_retired_pipeline_formats_are_refused() {
    let recovering = fixture("v3");
    assert_eq!(recovering["slots"]["author"]["recovery_attempts"], 2);
    assert_valid(PIPELINE, &recovering);
    admitted(&recovering).unwrap();
    for old in [
        "af.pipeline/1",
        "af.pipeline/2",
        "af.pipeline/3",
        "af.pipeline/4",
    ] {
        let retired = labelled(&recovering, old);
        assert_invalid(PIPELINE, &retired, "retired Pipeline format");
        assert!(admitted(&retired).is_err());
    }
    let mut plain = recovering.clone();
    plain["slots"]["author"]
        .as_object_mut()
        .unwrap()
        .remove("recovery_attempts");
    assert_valid(PIPELINE, &plain);
    admitted(&plain).unwrap();
    assert_eq!(plain, fixture("v1"));
    for allowance in [0, 1, 8] {
        let mut bounded = recovering.clone();
        bounded["slots"]["author"]["recovery_attempts"] = json!(allowance);
        assert_valid(PIPELINE, &bounded);
        admitted(&bounded).unwrap();
    }
    for invalid in [json!(9), json!(-1), json!(null)] {
        let mut many = recovering.clone();
        many["slots"]["author"]["recovery_attempts"] = invalid;
        assert_invalid(PIPELINE, &many, "bounded non-null recovery allowance");
        assert!(admitted(&many).is_err());
    }
}

#[test]
fn brief_and_recovery_combine_in_the_same_current_format() {
    let mut both = fixture("v2");
    let slot = both["slots"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    slot["recovery_attempts"] = json!(1);
    assert_valid(PIPELINE, &both);
    admitted(&both).unwrap();
}
