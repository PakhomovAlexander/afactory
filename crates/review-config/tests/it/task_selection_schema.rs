use review_config::task::selection::*;
use review_core::task::{PipelineChoiceV1, PipelineFallbackV1};
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn selection_schema_and_rust_preserve_all_decisions_and_close_every_variant() {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("schemas/pipeline-selection-v1.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let states = [
        CandidateState::Fit {},
        CandidateState::Compatible {},
        CandidateState::NoFit {
            reasons: vec!["Wrong Task kind".into()],
        },
        CandidateState::UnknownFacts {
            facts: BTreeSet::from(["small".into()]),
        },
        CandidateState::Unavailable {
            reason: "No Provider".into(),
        },
        CandidateState::Infeasible {
            reason: "Protected verification".into(),
        },
        CandidateState::Invalid {
            reason: "Broken dependency contract".into(),
        },
    ];
    for decision in [
        SelectionDecision::Selected {
            pipeline: "project/small".into(),
        },
        SelectionDecision::Ambiguous {
            pipelines: BTreeSet::from(["project/a".into(), "project/b".into()]),
        },
        SelectionDecision::NeedsFacts {
            facts: BTreeSet::from(["small".into()]),
        },
        SelectionDecision::Unavailable {},
        SelectionDecision::Infeasible {},
        SelectionDecision::Invalid {},
        SelectionDecision::Refused {},
        SelectionDecision::NeedsGeneration {},
    ] {
        for requested in [
            None,
            Some(PipelineChoiceV1 {
                name: "project/small".into(),
                fallback: PipelineFallbackV1::Generate,
            }),
        ] {
            let value = serde_json::to_value(PipelineSelection {
                schema: "af.pipeline-selection/1".into(),
                requested,
                no_match: NoMatchPolicy::Refuse,
                candidates: states
                    .iter()
                    .enumerate()
                    .map(|(i, state)| CandidateAssessment {
                        pipeline: format!("project/c{i}"),
                        preferred: i == 0,
                        priority: u32::MAX,
                        state: state.clone(),
                    })
                    .collect(),
                decision: decision.clone(),
            })
            .unwrap();
            assert!(validator.is_valid(&value), "{value}");
            assert_eq!(
                serde_json::to_value(
                    serde_json::from_value::<PipelineSelection>(value.clone()).unwrap()
                )
                .unwrap(),
                value
            );
            for pointer in ["", "/candidates/0", "/candidates/0/state", "/decision"] {
                let mut invalid = value.clone();
                invalid.pointer_mut(pointer).unwrap()["extra"] = json!(true);
                assert!(!validator.is_valid(&invalid));
                assert!(serde_json::from_value::<PipelineSelection>(invalid).is_err());
            }
            let mut invalid = value.clone();
            invalid["decision"]["kind"] = json!("guessed_fit");
            assert!(!validator.is_valid(&invalid));
            assert!(serde_json::from_value::<PipelineSelection>(invalid).is_err());
            let mut invalid = value;
            invalid["candidates"][0]["priority"] = json!(-1);
            assert!(!validator.is_valid(&invalid));
            assert!(serde_json::from_value::<PipelineSelection>(invalid).is_err());
        }
    }
}
