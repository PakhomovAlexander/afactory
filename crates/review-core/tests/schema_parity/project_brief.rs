//! The Project Brief's records (ADR-0153) and the pipeline port that asks for one: each schema
//! accepts what the Rust types write and refuses what they refuse.

use review_core::project::ProjectSurveyV1;
use review_core::task::project_brief::*;
use serde_json::{Value, json};

use super::{assert_invalid, assert_valid};

const BRIEF: &str = "project-brief-v1.json";
const PIN: &str = "project-survey-pin-v1.json";
const CAPTURE: &str = "project-brief-capture-v1.json";
const RECEIPT: &str = "project-brief-receipt-v1.json";
const PREVIEW: &str = "project-brief-preview-v1.json";

fn digest(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

/// A complete Survey the Surveyor recorded for the PJ2 fixture repository.
fn survey() -> ProjectSurveyV1 {
    let text = include_str!("../../../af/tests/fixtures/project-survey/expected.json");
    let survey: ProjectSurveyV1 = serde_json::from_str(text).unwrap();
    survey.validate().unwrap();
    survey
}

fn pin() -> ProjectSurveyPinV1 {
    ProjectSurveyPinV1::of(&digest('5'), &survey()).unwrap()
}

/// A Brief of every section but `contributors`, which it dropped, and `activity`, which the
/// Survey lacks.
fn brief() -> ProjectBriefV1 {
    let pin = pin();
    let identity = pin.identity().unwrap().clone();
    let mut sections = ProjectBriefSectionsV1::default();
    for section in [
        "identity",
        "tree",
        "history",
        "decisions",
        "vocabulary",
        "checks",
        "hot_paths",
    ] {
        sections.render(&pin, section, 1, 2);
    }
    let brief = ProjectBriefV1 {
        schema: PROJECT_BRIEF_V1.into(),
        survey_id: pin.survey_id.clone(),
        project_id: identity.project_id,
        variant: "wide".into(),
        head: ProjectBriefHeadV1 {
            commit: identity.head,
            tree_digest: identity.tree_digest,
        },
        sections,
        dropped: vec![
            ProjectBriefDropV1 {
                section: "activity".into(),
                reason: ProjectBriefDropReasonV1::NotInSurvey,
            },
            ProjectBriefDropV1 {
                section: "contributors".into(),
                reason: ProjectBriefDropReasonV1::MaxBytes,
            },
        ],
        rendered_bytes: 4321.into(),
    };
    brief.validate().unwrap();
    brief
}

fn changed(base: &Value, pointer: &str, key: &str, value: Option<Value>) -> Value {
    let mut changed = base.clone();
    let parent = changed
        .pointer_mut(pointer)
        .unwrap()
        .as_object_mut()
        .unwrap();
    match value {
        Some(value) => parent.insert(key.into(), value),
        None => parent.remove(key),
    };
    changed
}

#[test]
fn a_brief_holds_survey_sections_verbatim_and_names_what_it_dropped() {
    let brief = brief();
    let value = serde_json::to_value(&brief).unwrap();
    assert_valid(BRIEF, &value);
    assert_eq!(
        value["sections"]["hot_paths"].as_array().unwrap().len(),
        2.min(pin().history.body.unwrap().hot_paths.len())
    );
    assert!(
        value["sections"]["decisions"]["records"]
            .as_array()
            .unwrap()
            .len()
            <= 1
    );
    assert_eq!(
        serde_json::from_value::<ProjectBriefV1>(value.clone()).unwrap(),
        brief
    );
    for (pointer, key, replacement, why) in [
        ("", "variant", Some(json!("none")), "never the variant none"),
        ("", "rendered_bytes", Some(json!(4321)), "decimal text"),
        ("", "rendered_bytes", None, "rendered_bytes is required"),
        (
            "",
            "previous_survey_id",
            Some(json!(digest('6'))),
            "no link",
        ),
        (
            "/sections",
            "activity",
            Some(json!({})),
            "activity is never rendered",
        ),
        ("/sections", "owners", Some(json!({})), "no unknown section"),
        ("/head", "commit", Some(json!("HEAD")), "an exact commit"),
        (
            "/dropped/0",
            "reason",
            Some(json!("too_big")),
            "closed drop reasons",
        ),
    ] {
        let invalid = changed(&value, pointer, key, replacement);
        assert_invalid(BRIEF, &invalid, why);
        let refused = serde_json::from_value::<ProjectBriefV1>(invalid)
            .map_or(true, |brief| brief.validate().is_err());
        assert!(refused, "{why}");
    }
}

#[test]
fn a_pin_holds_one_complete_survey_and_no_link_to_another() {
    let pin = pin();
    let value = serde_json::to_value(&pin).unwrap();
    assert_valid(PIN, &value);
    assert_eq!(
        serde_json::from_value::<ProjectSurveyPinV1>(value.clone()).unwrap(),
        pin
    );
    let linked = changed(&value, "", "previous_survey_id", Some(json!(digest('6'))));
    assert_invalid(PIN, &linked, "no previous Survey");
    assert!(serde_json::from_value::<ProjectSurveyPinV1>(linked).is_err());
    let partial = changed(&value, "/tree", "completeness", Some(json!("partial")));
    let partial = changed(&partial, "/tree", "reason", Some(json!("max_wall_ms")));
    assert_invalid(PIN, &partial, "only a complete Survey is pinned");
    let typed: ProjectSurveyPinV1 = serde_json::from_value(partial).unwrap();
    assert!(typed.validate().is_err());
}

#[test]
fn a_capture_records_the_pin_the_variants_and_each_heads_ancestry() {
    let mut capture = ProjectBriefCaptureV1 {
        schema: PROJECT_BRIEF_CAPTURE_V1.into(),
        pin_id: Some(digest('7')),
        source_commit: Some("b".repeat(40)),
        ..Default::default()
    };
    capture
        .variants
        .insert(ORIENT.into(), ProjectBriefVariantV1::orient());
    capture.ancestry.insert(
        "a".repeat(40),
        ProjectBriefAncestryV1::Behind {
            behind_commits: 3.into(),
        },
    );
    capture.ancestry.insert(
        "c".repeat(40),
        ProjectBriefAncestryV1::Unknown {
            reason: ProjectBriefUnknownV1::NotAncestor,
        },
    );
    capture.validate().unwrap();
    let value = serde_json::to_value(&capture).unwrap();
    assert_valid(CAPTURE, &value);
    assert_eq!(
        serde_json::from_value::<ProjectBriefCaptureV1>(value.clone()).unwrap(),
        capture
    );
    let empty = json!({"schema": PROJECT_BRIEF_CAPTURE_V1});
    assert_valid(CAPTURE, &empty);
    for (max_bytes, valid) in [
        ("1023", false),
        ("1024", true),
        ("262144", true),
        ("262145", false),
    ] {
        let candidate = changed(
            &value,
            "/variants/orient",
            "max_bytes",
            Some(json!(max_bytes)),
        );
        if valid {
            assert_valid(CAPTURE, &candidate);
        } else {
            assert_invalid(CAPTURE, &candidate, "max_bytes is 1024..=262144");
        }
        let typed: ProjectBriefCaptureV1 = serde_json::from_value(candidate).unwrap();
        assert_eq!(typed.validate().is_ok(), valid, "{max_bytes}");
    }
    for (pointer, key, replacement, why) in [
        (
            "/variants",
            "none",
            Some(value["variants"]["orient"].clone()),
            "none is built in",
        ),
        (
            "/variants/orient",
            "sections",
            Some(json!(["tree", "tree"])),
            "no repeated section",
        ),
        (
            "/variants/orient",
            "sections",
            Some(json!(["tree", "owners"])),
            "no unknown section",
        ),
        (
            "/variants/orient",
            "decisions",
            Some(json!("1001")),
            "at most max_entries",
        ),
        (
            "/variants/orient",
            "colour",
            Some(json!("red")),
            "closed variant schema",
        ),
        (
            &format!("/ancestry/{}", "c".repeat(40)),
            "reason",
            Some(json!("tired")),
            "closed unknown reasons",
        ),
    ] {
        let invalid = changed(&value, pointer, key, replacement);
        assert_invalid(CAPTURE, &invalid, why);
        let refused = serde_json::from_value::<ProjectBriefCaptureV1>(invalid)
            .map_or(true, |capture| capture.validate().is_err());
        assert!(refused, "{why}");
    }
}

#[test]
fn a_receipt_holds_a_brief_and_its_staleness_or_why_it_holds_none() {
    let constructed = ProjectBriefReceiptV1 {
        schema: PROJECT_BRIEF_RECEIPT_V1.into(),
        port: "project".into(),
        outcome: ProjectBriefOutcomeV1::Constructed,
        variant: ORIENT.into(),
        reason: None,
        brief_id: Some(digest('8')),
        source_commit: Some("d".repeat(40)),
        staleness: Some(ProjectBriefAncestryV1::Behind {
            behind_commits: 0.into(),
        }),
    };
    let none = ProjectBriefReceiptV1 {
        outcome: ProjectBriefOutcomeV1::None,
        reason: Some(ProjectBriefNoneReasonV1::NoSurvey),
        brief_id: None,
        staleness: None,
        ..constructed.clone()
    };
    for receipt in [&constructed, &none] {
        receipt.validate().unwrap();
        let value = serde_json::to_value(receipt).unwrap();
        assert_valid(RECEIPT, &value);
        assert_eq!(
            &serde_json::from_value::<ProjectBriefReceiptV1>(value).unwrap(),
            receipt
        );
    }
    let value = serde_json::to_value(&constructed).unwrap();
    let none_value = serde_json::to_value(&none).unwrap();
    for (invalid, why) in [
        (
            changed(&value, "", "staleness", None),
            "a Brief has its staleness",
        ),
        (
            changed(&value, "", "reason", Some(json!("no_survey"))),
            "a Brief needs no reason",
        ),
        (
            changed(&none_value, "", "brief_id", Some(json!(digest('9')))),
            "none holds no Brief",
        ),
        (changed(&none_value, "", "reason", None), "none says why"),
        (
            changed(&value, "", "outcome", Some(json!("guessed"))),
            "closed outcomes",
        ),
    ] {
        assert_invalid(RECEIPT, &invalid, why);
        let refused = serde_json::from_value::<ProjectBriefReceiptV1>(invalid)
            .map_or(true, |receipt| receipt.validate().is_err());
        assert!(refused, "{why}");
    }
}

#[test]
fn a_preview_carries_the_brief_unchanged_or_the_none_outcome() {
    let brief = brief();
    let id = digest('a');
    let preview = ProjectBriefPreviewV1 {
        schema: PROJECT_BRIEF_PREVIEW_V1.into(),
        variant: "wide".into(),
        outcome: ProjectBriefPreviewOutcomeV1::Brief,
        reason: None,
        brief_id: Some(id.clone()),
        brief: Some(brief),
        manifest: Some(ProjectBriefManifestEntryV1 {
            name: "project".into(),
            required_by: "port:project".into(),
            artifact_id: id,
            artifact_type: PROJECT_BRIEF_V1.into(),
            rendered_bytes: 4500.into(),
            estimated_tokens: 1125.into(),
        }),
    };
    let none = ProjectBriefPreviewV1 {
        outcome: ProjectBriefPreviewOutcomeV1::None,
        reason: Some(ProjectBriefNoneReasonV1::VariantNone),
        brief_id: None,
        brief: None,
        manifest: None,
        variant: NONE.into(),
        ..preview.clone()
    };
    for document in [&preview, &none] {
        document.validate().unwrap();
        let value = serde_json::to_value(document).unwrap();
        assert_valid(PREVIEW, &value);
    }
    let value = serde_json::to_value(&preview).unwrap();
    let none_value = serde_json::to_value(&none).unwrap();
    for (invalid, why) in [
        (
            changed(&value, "", "manifest", None),
            "a Brief has its manifest entry",
        ),
        (
            changed(&none_value, "", "brief_id", Some(json!(digest('b')))),
            "none has no Brief",
        ),
        (
            changed(
                &value,
                "/manifest",
                "required_by",
                Some(json!("declared input port")),
            ),
            "the port asks",
        ),
    ] {
        assert_invalid(PREVIEW, &invalid, why);
        let refused = serde_json::from_value::<ProjectBriefPreviewV1>(invalid)
            .map_or(true, |document| document.validate().is_err());
        assert!(refused, "{why}");
    }
}

#[test]
fn a_project_brief_port_is_one_optional_unbound_brief_naming_its_variant() {
    let definition = |port: Value| {
        json!({
            "schema": "af.pipeline/1", "name": "fixture/oriented", "version": "1.0.0",
            "contract": {
                "inputs": {"project": port},
                "outputs": {"out": {"artifact_type": "af/Document@1", "cardinality": "one",
                    "optional": false, "affinity": {"kind": "unbound"}, "covers": []}}
            },
            "accepts": {"kinds": ["report"], "required_facts": {}},
            "slots": {},
            "nodes": [{"id": "n", "operator": {"op": "report_seal"}, "inputs": {}}],
            "outputs": {"out": {"kind": "node", "node": "n", "port": "document"}},
            "coverage": {}, "max_attempts": 1, "max_parallel": 1
        })
    };
    let port = json!({"artifact_type": "af/ProjectBrief@1", "cardinality": "one", "optional": true,
        "affinity": {"kind": "unbound"}, "covers": [], "root_default": "project_brief", "brief": "orient"});
    for (port, valid) in [
        (port.clone(), true),
        (changed(&port, "", "brief", None), false),
        (changed(&port, "", "optional", Some(json!(false))), false),
        (
            changed(&port, "", "cardinality", Some(json!("many"))),
            false,
        ),
        (
            changed(
                &port,
                "",
                "artifact_type",
                Some(json!("af/ReviewHistory@1")),
            ),
            false,
        ),
        (changed(&port, "", "root_default", None), false),
        (
            changed(&port, "", "brief", Some(json!("not a name"))),
            false,
        ),
        (
            changed(
                &port,
                "",
                "affinity",
                Some(json!({"kind": "same_as", "input": "source"})),
            ),
            false,
        ),
    ] {
        let value = definition(port.clone());
        if valid {
            assert_valid("pipeline-definition-v1.json", &value);
        } else {
            assert_invalid(
                "pipeline-definition-v1.json",
                &value,
                "project_brief port shape",
            );
        }
        let typed = serde_json::from_value::<review_core::task::pipeline::PipelinePortV1>(port)
            .is_ok_and(|port| port.validate().is_ok());
        assert_eq!(typed, valid, "{value}");
    }
}
