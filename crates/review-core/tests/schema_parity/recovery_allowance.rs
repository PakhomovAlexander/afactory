//! A slot's `recovery_attempts` (ADR-0200) is `af.pipeline/3`'s alone: each version's schema
//! and Rust admission accept the pipelines of its own shape and refuse every other.

use review_core::task::pipeline::PipelineDefinitionV1;
use serde_json::{Value, json};

use super::{assert_invalid, assert_valid};

const PIPELINE_V1: &str = "pipeline-definition-v1.json";
const PIPELINE_V2: &str = "pipeline-definition-v2.json";
const PIPELINE_V3: &str = "pipeline-definition-v3.json";

fn fixture(version: &str) -> Value {
    let path = super::workspace_root()
        .join("fixtures/task-contracts")
        .join(version)
        .join("pipeline-definition.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Rust admission of a pipeline: the typed round trip and its validation.
fn admitted(value: &Value) -> Result<(), String> {
    let definition: PipelineDefinitionV1 =
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
fn a_recovery_allowance_is_af_pipeline_3_and_no_other_version_carries_it() {
    let v3 = fixture("v3");
    assert_eq!(v3["slots"]["author"]["recovery_attempts"], 2);
    assert_valid(PIPELINE_V3, &v3);
    admitted(&v3).unwrap();
    assert_invalid(PIPELINE_V1, &v3, "af.pipeline/1 is its own label");

    // The old versions carrying the new key: refused by every schema and by Rust, naming the
    // version the pipeline needs.
    for old in ["af.pipeline/1", "af.pipeline/2"] {
        let relabelled = labelled(&v3, old);
        assert_invalid(
            PIPELINE_V1,
            &relabelled,
            "no recovery allowance in version 1",
        );
        assert_invalid(
            PIPELINE_V2,
            &relabelled,
            "no recovery allowance in version 2",
        );
        assert_invalid(PIPELINE_V3, &relabelled, "af.pipeline/3 is its own label");
        let refused = admitted(&relabelled).unwrap_err();
        assert!(
            refused.contains(&format!(
                "declares recovery_attempts on slot author under {old}"
            )) && refused.contains("af.pipeline/3"),
            "{refused}"
        );
    }

    // Without the key the pipeline is version 1, never a second spelling of it.
    let mut plain = v3.clone();
    plain["slots"]["author"]
        .as_object_mut()
        .unwrap()
        .remove("recovery_attempts");
    assert_invalid(
        PIPELINE_V3,
        &plain,
        "af.pipeline/3 declares a recovery allowance",
    );
    let refused = admitted(&plain).unwrap_err();
    assert!(
        refused.contains("is af.pipeline/3 but no slot declares recovery_attempts"),
        "{refused}"
    );
    let plain = labelled(&plain, "af.pipeline/1");
    assert_valid(PIPELINE_V1, &plain);
    admitted(&plain).unwrap();
    assert_eq!(
        plain,
        fixture("v1"),
        "the same pipeline as the version-1 fixture"
    );

    // The allowance is bounded.
    let mut many = v3.clone();
    many["slots"]["author"]["recovery_attempts"] = json!(9);
    assert_invalid(PIPELINE_V3, &many, "at most 8 recoveries");
    assert!(admitted(&many).unwrap_err().contains("at most 8"));
    many["slots"]["author"]["recovery_attempts"] = json!(0);
    assert_valid(PIPELINE_V3, &many);
    admitted(&many).unwrap();
}

/// A pipeline with both a Project Brief port and a recovery allowance is `af.pipeline/3`.
#[test]
fn a_briefed_pipeline_with_a_recovery_allowance_is_af_pipeline_3() {
    let mut both = fixture("v2");
    let slot = both["slots"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    slot["recovery_attempts"] = json!(1);
    let refused = admitted(&both).unwrap_err();
    assert!(refused.contains("under af.pipeline/2"), "{refused}");
    assert_invalid(PIPELINE_V2, &both, "no recovery allowance in version 2");
    let both = labelled(&both, "af.pipeline/3");
    assert_valid(PIPELINE_V3, &both);
    admitted(&both).unwrap();
}
