//! The renderer, the policy, the constructor and binding admission, unit by unit;
//! `tests/it/project_brief.rs` runs them end to end through `af`.

use super::*;
use review_core::project::ProjectSurveyV2;
use review_core::task::project_brief::{
    MAX_BRIEF_BYTES, MIN_BRIEF_BYTES, PROJECT_BRIEF_CAPTURE_V1, ProjectBriefCaptureV1,
};

/// A complete Survey the Surveyor recorded for the PJ2 fixture repository.
fn survey() -> ProjectSurveyV2 {
    let text = include_str!("../../../tests/fixtures/project-survey/expected.json");
    let survey: ProjectSurveyV2 = serde_json::from_str(text).unwrap();
    survey.validate().unwrap();
    survey
}

fn pin() -> ProjectSurveyPinV1 {
    ProjectSurveyPinV1::of(&format!("sha256:{}", "5".repeat(64)), &survey()).unwrap()
}

fn variant(sections: &[&str], max_bytes: u64) -> ProjectBriefVariantV1 {
    ProjectBriefVariantV1 {
        sections: sections.iter().map(|s| s.to_string()).collect(),
        max_bytes: max_bytes.into(),
        decisions: 12.into(),
        hot_paths: 20.into(),
    }
}

fn policy(text: &str) -> Result<Variants, String> {
    let table: toml::Value = toml::from_str(text).unwrap();
    policy_of(table.get("brief"))
}

#[test]
fn a_brief_is_its_exact_canonical_bytes_and_carries_no_link_beyond_its_pin() {
    let pin = pin();
    let (brief, bytes) = render(&pin, ORIENT, &ProjectBriefVariantV1::orient()).unwrap();
    assert_eq!(brief.rendered_bytes.get(), bytes.len() as u64);
    let reparsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        crate::projects::survey::canonical_bytes(&reparsed).unwrap(),
        bytes
    );
    assert!(brief.dropped.is_empty(), "{:?}", brief.dropped);
    for section in ORIENT_SECTIONS_RENDERED {
        assert!(brief.sections.has(section), "{section}");
    }
    assert!(!brief.sections.has("contributors") && !brief.sections.has("identity"));
    assert!(rendered_from(&pin, &brief));
    // The pin names no other record: its only digests are the Survey's own identities.
    let text = String::from_utf8(serde_json::to_vec(&pin).unwrap()).unwrap();
    assert!(!text.contains("previous_survey_id"));
}

const ORIENT_SECTIONS_RENDERED: [&str; 5] =
    ["tree", "vocabulary", "decisions", "checks", "hot_paths"];

#[test]
fn whole_sections_drop_last_declared_first_until_the_exact_bytes_fit() {
    let pin = pin();
    let sections = ["tree", "decisions", "vocabulary", "checks"];
    let (full, bytes) = render(&pin, "slim", &variant(&sections, MAX_BRIEF_BYTES)).unwrap();
    assert!(full.dropped.is_empty());
    // Exactly at the boundary: nothing is dropped.
    let at = bytes.len() as u64;
    let (kept, kept_bytes) = render(&pin, "slim", &variant(&sections, at)).unwrap();
    assert_eq!(kept_bytes.len() as u64, at);
    assert!(kept.dropped.is_empty());
    // One byte under: the last-declared section goes, whole, with its reason.
    let (cut, cut_bytes) = render(&pin, "slim", &variant(&sections, at - 1)).unwrap();
    assert_eq!(
        cut.dropped,
        [ProjectBriefDropV1 {
            section: "checks".into(),
            reason: ProjectBriefDropReasonV1::MaxBytes,
        }]
    );
    assert!((cut_bytes.len() as u64) < at);
    assert_eq!(cut.rendered_bytes.get(), cut_bytes.len() as u64);
    assert!(cut.sections.tree.is_some() && cut.sections.checks.is_none());
    assert!(rendered_from(&pin, &cut));
}

#[test]
fn a_brief_that_cannot_fit_without_its_sections_fails_naming_the_variant_and_the_bytes() {
    let pin = pin();
    // Below the policy floor, which no loaded policy reaches: the renderer itself still refuses
    // an oversized or silently empty Brief.
    let error = render(&pin, "tiny", &variant(&["tree", "checks"], 200)).unwrap_err();
    assert!(error.contains("variant tiny"), "{error}");
    assert!(error.contains("over its max_bytes of 200"), "{error}");
    let needed: u64 = error
        .split("still ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|bytes| bytes.parse().ok())
        .unwrap();
    assert!(needed > 200, "{error}");
}

#[test]
fn activity_and_sections_the_survey_lacks_are_recorded_as_dropped() {
    let (brief, _) = render(&pin(), "team", &variant(&["tree", "activity"], 65_536)).unwrap();
    assert_eq!(
        brief.dropped,
        [ProjectBriefDropV1 {
            section: "activity".into(),
            reason: ProjectBriefDropReasonV1::NotInSurvey,
        }]
    );
}

#[test]
fn the_policy_merges_over_orient_and_refuses_every_field_outside_its_closed_schema() {
    assert_eq!(policy("").unwrap(), BTreeMap::new());
    let merged = policy("[brief.orient]\nmax_bytes = 4096\n").unwrap();
    let mut expected = ProjectBriefVariantV1::orient();
    expected.max_bytes = 4096.into();
    assert_eq!(merged[ORIENT], expected);
    let slim = policy("[brief.slim]\nsections = [\"tree\"]\n").unwrap();
    assert_eq!(slim["slim"], variant(&["tree"], 8192));
    for (text, needle) in [
        (
            "[brief.slim]\nsections = [\"tree\"]\nlimit = 3\n",
            "[brief.slim]: unknown field `limit`",
        ),
        (
            "[brief.slim]\nsections = [\"owners\"]\n",
            "[brief.slim] sections: `owners` is no section",
        ),
        (
            "[brief.slim]\nsections = [\"tree\", \"tree\"]\n",
            "[brief.slim] sections: `tree` is listed twice",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\ndecisions = 1001\n",
            "[brief.slim] decisions",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\nhot_paths = -1\n",
            "[brief.slim] hot_paths",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\nmax_bytes = 1023\n",
            "[brief.slim] max_bytes: 1023",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\nmax_bytes = 262145\n",
            "[brief.slim] max_bytes: 262145",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\nmax_bytes = \"8k\"\n",
            "[brief.slim] max_bytes",
        ),
        ("[brief.slim]\nmax_bytes = 4096\n", "[brief.slim] sections"),
        ("[brief.none]\nsections = [\"tree\"]\n", "[brief.none] name"),
        (
            "[brief.orient]\nsections = [\"tree\", \"contributors\"]\n",
            "[brief.orient] sections",
        ),
        (
            "[brief.orient]\nsections = [\"activity\"]\n",
            "[brief.orient] sections",
        ),
    ] {
        let error = policy(text).unwrap_err();
        assert!(
            error.starts_with("the [brief] policy does not load"),
            "{error}"
        );
        assert!(error.contains(needle), "`{needle}` missing: {error}");
    }
    assert_eq!(
        policy(&format!("[brief.wide]\nsections = [\"contributors\", \"activity\"]\nmax_bytes = {MAX_BRIEF_BYTES}\n"))
            .unwrap()["wide"]
            .max_bytes
            .get(),
        MAX_BRIEF_BYTES
    );
    assert_eq!(
        policy(&format!(
            "[brief.small]\nsections = [\"tree\"]\nmax_bytes = {MIN_BRIEF_BYTES}\n"
        ))
        .unwrap()["small"]
            .max_bytes
            .get(),
        MIN_BRIEF_BYTES
    );
}

/// A revision whose provenance holds `capture`.
fn revision_with(cas: &Cas, capture: &ProjectBriefCaptureV1) -> TaskRevisionV1 {
    let mut revision: TaskRevisionV1 = serde_json::from_str(include_str!(
        "../../../../../fixtures/task-contracts/v1/task-revision.json"
    ))
    .unwrap();
    let (id, _) = cas
        .put_artifact(
            PROJECT_BRIEF_CAPTURE_V1,
            producer("capture-project-brief@1"),
            capture.pin_id.iter().cloned().collect(),
            None,
            serde_json::to_value(capture).unwrap(),
        )
        .unwrap();
    revision.provenance.input_artifact_ids.push(id);
    revision
}

#[test]
fn the_constructor_renders_only_from_the_pinned_capture() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path()).unwrap();
    let pin = pin();
    let (pin_id, _) = cas
        .put_artifact(
            PROJECT_SURVEY_PIN_V1,
            pin_producer(),
            vec![],
            None,
            value_of(&pin).unwrap(),
        )
        .unwrap();
    let mut capture = ProjectBriefCaptureV1 {
        schema: PROJECT_BRIEF_CAPTURE_V1.into(),
        pin_id: Some(pin_id.clone()),
        ..Default::default()
    };
    let nodes = ["implement".to_string()];
    let revision = revision_with(&cas, &capture);
    let built = Resolver
        .resolve(&cas, &revision, "project", ORIENT, &nodes)
        .unwrap()
        .unwrap();
    // The ID `af project brief` names is the one the constructor publishes.
    let (brief, _) = render(&pin, ORIENT, &ProjectBriefVariantV1::orient()).unwrap();
    assert_eq!(
        identities(&pin, &brief).unwrap(),
        (pin_id.clone(), built.artifact_ids[0].clone())
    );
    assert_eq!(admit_bound(&cas, &built), Ok(()));
    // Rendering again is the same artifact: a pure function of the pin.
    let again = Resolver
        .resolve(&cas, &revision, "project", ORIENT, &nodes)
        .unwrap();
    assert_eq!(again, Some(built));
    assert_eq!(
        Resolver.resolve(&cas, &revision, "project", "none", &nodes),
        Ok(None)
    );
    let unknown = Resolver
        .resolve(&cas, &revision, "project", "slim", &nodes)
        .unwrap_err();
    assert!(
        unknown.contains("node implement") && unknown.contains("variant slim"),
        "{unknown}"
    );
    // Without a pin the port stays empty, and a project variant is the captured one.
    capture.pin_id = None;
    capture
        .variants
        .insert("slim".into(), variant(&["tree"], 4096));
    let revision = revision_with(&cas, &capture);
    assert_eq!(
        Resolver.resolve(&cas, &revision, "project", "slim", &nodes),
        Ok(None)
    );
}

#[test]
fn a_brief_no_kernel_constructor_rendered_is_refused_at_binding() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path()).unwrap();
    let pin = pin();
    let (pin_id, _) = cas
        .put_artifact(
            PROJECT_SURVEY_PIN_V1,
            pin_producer(),
            vec![],
            None,
            value_of(&pin).unwrap(),
        )
        .unwrap();
    let (brief, _) = render(&pin, ORIENT, &ProjectBriefVariantV1::orient()).unwrap();
    let input = |id: String| ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: PROJECT_BRIEF_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    };
    // Schema-valid and naming the pin, but published by a Worker's Attempt.
    let worker = Producer::Attempt {
        run_id: "task:x".into(),
        node_id: "implement".into(),
        attempt_id: "0".repeat(26),
    };
    let (authored, _) = cas
        .put_artifact(
            PROJECT_BRIEF_V1,
            worker,
            vec![pin_id.clone()],
            None,
            value_of(&brief).unwrap(),
        )
        .unwrap();
    let error = admit_bound(&cas, &input(authored)).unwrap_err();
    assert!(error.starts_with(NOT_KERNEL_CONSTRUCTED), "{error}");
    // The kernel's producer on a Brief that is not the pin's rendering is refused too.
    let mut forged = brief.clone();
    forged.variant = "slim".into();
    forged.sections.tree = None;
    sized(&mut forged).unwrap();
    let mut tampered = forged.clone();
    if let Some(vocabulary) = tampered
        .sections
        .vocabulary
        .as_mut()
        .and_then(|v| v.body.as_mut())
    {
        vocabulary.file = "OTHER.md".into();
    }
    sized(&mut tampered).unwrap();
    let (tampered, _) = cas
        .put_artifact(
            PROJECT_BRIEF_V1,
            brief_producer(),
            vec![pin_id.clone()],
            None,
            value_of(&tampered).unwrap(),
        )
        .unwrap();
    let error = admit_bound(&cas, &input(tampered)).unwrap_err();
    assert!(
        error.contains("is not a rendering of the Survey"),
        "{error}"
    );
    // A smaller variant of the same pin is the kernel's rendering, and binds.
    let (forwarded, _) = cas
        .put_artifact(
            PROJECT_BRIEF_V1,
            brief_producer(),
            vec![pin_id],
            None,
            value_of(&forged).unwrap(),
        )
        .unwrap();
    assert_eq!(admit_bound(&cas, &input(forwarded)), Ok(()));
}
