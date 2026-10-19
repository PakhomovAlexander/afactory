//! The Project and its Workspaces (ADR-0151): the three Project Store events, each against its
//! own schema and inside `run-event-v1.json`, and the two `af project` documents.
//!
//! What must stay unrepresentable is a host path in a record: a Workspace is named by an opaque
//! ID and a label, and its paths exist only in the machine-local `workspaces.toml` and in the
//! `--json` documents printed on this machine.

use super::{assert_invalid, assert_valid};
use review_core::event::validate_event_payload;
use review_core::project::{
    ProjectRegisteredPayloadV1, WorkspaceOriginV1, WorkspaceRegisteredPayloadV1,
    WorkspaceRemovedPayloadV1,
};
use review_core::{EventType, RunEvent};
use serde_json::{Value, json};

const ROOT: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA256_ROOT: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn project_id() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn event(event_type: EventType, payload: Value) -> Value {
    serde_json::to_value(RunEvent {
        event_id: "0".repeat(26),
        run_id: "project".into(),
        sequence: 0,
        event_type,
        occurred_at: "2026-10-19T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: Vec::new(),
        payload,
    })
    .unwrap()
}

/// One payload: its own schema, the event envelope's, and the Rust validator agree.
fn accepted(schema: &str, event_type: EventType, payload: &Value) {
    assert_valid(schema, payload);
    assert_valid("run-event-v1.json", &event(event_type, payload.clone()));
    validate_event_payload(event_type, payload).unwrap();
}

/// One payload all three refuse.
fn refused(schema: &str, event_type: EventType, payload: &Value, why: &str) {
    assert_invalid(schema, payload, why);
    assert_invalid(
        "run-event-v1.json",
        &event(event_type, payload.clone()),
        why,
    );
    assert!(
        validate_event_payload(event_type, payload).is_err(),
        "the Rust validator accepted it ({why})"
    );
}

fn with(base: &Value, key: &str, value: Value) -> Value {
    let mut changed = base.clone();
    changed[key] = value;
    changed
}

fn without(base: &Value, key: &str) -> Value {
    let mut changed = base.clone();
    changed.as_object_mut().unwrap().remove(key);
    changed
}

#[test]
fn project_registered_carries_identity_and_declaration_never_a_path() {
    const SCHEMA: &str = "project-registered-v1.json";
    let full = ProjectRegisteredPayloadV1 {
        project_id: project_id(),
        repository_id: format!("{ROOT},{SHA256_ROOT}"),
        name: Some("afactory".into()),
        min_af: Some("0.12.0-rc.4".into()),
        registered_unix_ms: 1_792_412_402_102.into(),
    };
    full.validate().unwrap();
    let value = serde_json::to_value(&full).unwrap();
    assert_eq!(value["registered_unix_ms"], "1792412402102");
    accepted(SCHEMA, EventType::ProjectRegisteredV1, &value);
    assert_eq!(
        serde_json::from_value::<ProjectRegisteredPayloadV1>(value.clone()).unwrap(),
        full
    );
    let bare = without(&without(&value, "name"), "min_af");
    accepted(SCHEMA, EventType::ProjectRegisteredV1, &bare);

    for (broken, why) in [
        (
            with(&value, "project_id", json!("sha256:short")),
            "a Project ID is a full digest",
        ),
        (
            with(&value, "repository_id", json!("/home/me/afactory")),
            "a repository_id is root commits, never a path",
        ),
        (
            with(
                &value,
                "repository_id",
                json!("https://github.com/PakhomovAlexander/afactory"),
            ),
            "a repository_id is never a remote URL",
        ),
        (
            with(&value, "name", json!(" ")),
            "a declared name is not blank",
        ),
        (
            with(&value, "min_af", json!("0.12 /tmp")),
            "min_af is version text",
        ),
        (
            with(&value, "registered_unix_ms", json!(1_792_412_402_102_u64)),
            "times are decimal text",
        ),
        (
            with(&value, "path", json!("/home/me/afactory")),
            "no field holds a checkout path",
        ),
        (without(&value, "repository_id"), "the identity is required"),
    ] {
        refused(SCHEMA, EventType::ProjectRegisteredV1, &broken, why);
    }
}

#[test]
fn workspace_events_name_a_workspace_by_id_and_label_only() {
    const REGISTERED: &str = "project-workspace-registered-v1.json";
    const REMOVED: &str = "project-workspace-removed-v1.json";
    for origin in [
        WorkspaceOriginV1::Adopted,
        WorkspaceOriginV1::Created,
        WorkspaceOriginV1::Delivered,
    ] {
        let registered = WorkspaceRegisteredPayloadV1 {
            workspace_id: "w-0123456789abcdef".into(),
            label: "research".into(),
            origin,
            registered_unix_ms: 7.into(),
        };
        registered.validate().unwrap();
        let value = serde_json::to_value(&registered).unwrap();
        assert_eq!(value["origin"], origin.as_str());
        accepted(REGISTERED, EventType::WorkspaceRegisteredV1, &value);
        assert_eq!(
            serde_json::from_value::<WorkspaceRegisteredPayloadV1>(value).unwrap(),
            registered
        );
    }
    let value = json!({
        "workspace_id": "w-0123456789abcdef", "label": "research", "origin": "adopted",
        "registered_unix_ms": "7"
    });
    for (broken, why) in [
        (
            with(
                &value,
                "workspace_id",
                json!("/home/me/afactory-wt-research"),
            ),
            "a Workspace ID is opaque, never a path",
        ),
        (
            with(
                &value,
                "workspace_id",
                json!("0123456789abcdef0123456789abcdef"),
            ),
            "a Warm Workspace identity is not a Workspace ID",
        ),
        (
            with(&value, "label", json!("../research")),
            "a label is a safe name",
        ),
        (
            with(&value, "origin", json!("forked")),
            "the origins are closed",
        ),
        (
            with(
                &value,
                "state",
                json!("/home/me/.local/state/af/task/local/0123"),
            ),
            "no Store path in a record",
        ),
    ] {
        refused(REGISTERED, EventType::WorkspaceRegisteredV1, &broken, why);
    }

    let removed = WorkspaceRemovedPayloadV1 {
        workspace_id: "w-0123456789abcdef".into(),
        removed_unix_ms: 9.into(),
    };
    let value = serde_json::to_value(&removed).unwrap();
    accepted(REMOVED, EventType::WorkspaceRemovedV1, &value);
    for (broken, why) in [
        (
            with(&value, "workspace_id", json!("research")),
            "a removal names the Workspace ID",
        ),
        (
            with(&value, "removed_unix_ms", json!(-1)),
            "times are decimal text",
        ),
        (
            with(&value, "path", json!("/home/me/afactory-wt-research")),
            "no path in a record",
        ),
    ] {
        refused(REMOVED, EventType::WorkspaceRemovedV1, &broken, why);
    }
}

fn workspace(label: &str, origin: &str) -> Value {
    json!({
        "workspace_id": "w-0123456789abcdef",
        "label": label,
        "origin": origin,
        "registered_unix_ms": "7",
        "path": format!("/src/afactory-wt-{label}"),
        "state": format!("/home/me/.local/state/af/project/{}/workspaces/{label}/state", "a".repeat(64)),
        "bindings": "/home/me/.local/state/af/project/x/workspaces/research/bindings.toml",
        "tasks": "/home/me/.local/state/af/project/x/workspaces/research/tasks",
    })
}

#[test]
fn the_project_documents_join_the_log_with_this_machines_paths() {
    let project = json!({
        "schema": "af/project@1",
        "project_id": project_id(),
        "repository_id": ROOT,
        "name": "afactory",
        "min_af": "0.12",
        "store": format!("/home/me/.local/state/af/project/{}", "a".repeat(64)),
        "registered_unix_ms": "5",
        "workspaces": [workspace("research", "created"), workspace("pj1", "delivered")],
    });
    assert_valid("project-v1.json", &project);
    // A Workspace whose paths this machine does not know is listed without them.
    let unknown = json!({
        "workspace_id": "w-0123456789abcdef", "label": "elsewhere", "origin": "adopted",
        "registered_unix_ms": "7"
    });
    assert_valid(
        "project-v1.json",
        &with(&project, "workspaces", json!([unknown])),
    );
    assert_valid(
        "project-v1.json",
        &without(&without(&project, "name"), "min_af"),
    );
    for (broken, why) in [
        (
            with(&project, "schema", json!("af/project@2")),
            "the document names its version",
        ),
        (
            with(&project, "store", json!("state/af/project")),
            "paths are absolute",
        ),
        (
            with(
                &project,
                "workspaces",
                json!([with(&workspace("a", "adopted"), "origin", json!("cloned"))]),
            ),
            "the origins are closed",
        ),
        (
            with(
                &project,
                "workspaces",
                json!([with(&workspace("a", "adopted"), "extra", json!(true))]),
            ),
            "a Workspace is closed",
        ),
        (without(&project, "workspaces"), "the Workspaces are listed"),
    ] {
        assert_invalid("project-v1.json", &broken, why);
    }

    let listed = json!({
        "schema": "af/project-workspaces@1",
        "project_id": project_id(),
        "workspaces": [workspace("research", "created")],
    });
    assert_valid("project-workspaces-v1.json", &listed);
    assert_valid(
        "project-workspaces-v1.json",
        &with(&listed, "workspaces", json!([])),
    );
    assert_invalid(
        "project-workspaces-v1.json",
        &with(&listed, "project_id", json!("afactory")),
        "the Project is named by its ID",
    );
    assert_invalid(
        "project-workspaces-v1.json",
        &with(&listed, "store", json!("/x")),
        "the list document is closed",
    );
}

// ------------------------------------------------------------------------------------------
// Surveys (ADR-0152): the record, its two events and the history document.

const HEAD: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn digest(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

/// A Survey with every section measured, as the Surveyor writes one.
fn full_survey() -> review_core::project::ProjectSurveyV1 {
    use review_core::project::*;
    use std::collections::BTreeMap;
    let identity = SurveyIdentityV1 {
        project_id: project_id(),
        repository_id: ROOT.into(),
        name: Some("fixture".into()),
        min_af: Some("0.12".into()),
        af_pin: Some(SurveyAfPinV1 {
            version: "0.12.0".into(),
            digests: BTreeMap::from([("x86_64-unknown-linux-musl".into(), digest('d'))]),
        }),
        head: HEAD.into(),
        refs_digest: digest('b'),
        tree_digest: Some(digest('e')),
        tree_digest_reason: None,
        cutoff_unix_s: 1_792_000_000.into(),
        policy_digest: digest('f'),
        surveyor_version: 2.into(),
        git_version: "git version 2.50.1".into(),
    };
    let tree = SurveyTreeV1 {
        files: 3.into(),
        bytes: 30.into(),
        lines: 4.into(),
        files_without_lines: 1.into(),
        directories: BTreeMap::from([
            (
                ".".into(),
                SurveyDirectoryV1 {
                    files: 1.into(),
                    lines: 1.into(),
                    bytes: 10.into(),
                },
            ),
            (
                "src".into(),
                SurveyDirectoryV1 {
                    files: 2.into(),
                    lines: 3.into(),
                    bytes: 20.into(),
                },
            ),
        ]),
        languages: BTreeMap::from([
            (
                "markdown".into(),
                SurveyLanguageV1 {
                    files: 1.into(),
                    lines: 1.into(),
                },
            ),
            (
                "rust".into(),
                SurveyLanguageV1 {
                    files: 2.into(),
                    lines: 3.into(),
                },
            ),
        ]),
        tests: SurveyTestsV1 {
            files: 1.into(),
            lines: 2.into(),
            other_files: 2.into(),
            other_lines: 2.into(),
        },
        seen: BTreeMap::new(),
    };
    let window = |commits: u64| SurveyWindowV1 {
        commits: commits.into(),
        merges: 0.into(),
    };
    let history = SurveyHistoryV1 {
        reachable_commits: 5.into(),
        commits: 4.into(),
        merges: 1.into(),
        windows: SurveyWindowsV1 {
            days_30: window(1),
            days_90: window(2),
            days_365: window(3),
        },
        first_commit_unix_s: 1_700_000_000.into(),
        last_commit_unix_s: 1_791_000_000.into(),
        oldest_reached: Some(HEAD.into()),
        churn_90d: BTreeMap::from([(
            "src".into(),
            SurveyChurnV1 {
                commits: 2.into(),
                added: 5.into(),
                removed: 1.into(),
            },
        )]),
        hot_paths: vec![
            SurveyHotPathV1 {
                path: "src/lib.rs".into(),
                commits: 2.into(),
                removed: None,
            },
            SurveyHotPathV1 {
                path: "src/gone.rs".into(),
                commits: 1.into(),
                removed: Some(true),
            },
        ],
        tags: Some(SurveyTagsV1 {
            count: 2.into(),
            release: vec![SurveyNameV1 {
                name: "v1.0.0".into(),
                truncated: false,
            }],
        }),
        local_branches: Some(1.into()),
        seen: BTreeMap::new(),
    };
    let contributors = SurveyContributorsV1 {
        identity: SurveyIdentityPolicyV1::Name,
        identities: 2.into(),
        active_90d: 1.into(),
        human_commits: 3.into(),
        agent_commits: 1.into(),
        human_share_bp: 7500.into(),
        agent_share_bp: 2500.into(),
        top_share_bp: 7500.into(),
        by_identity: BTreeMap::from([
            (
                "Ada".into(),
                SurveyContributorV1 {
                    commits: 3.into(),
                    added: 5.into(),
                    removed: 1.into(),
                    first_unix_s: 1_700_000_000.into(),
                    last_unix_s: 1_791_000_000.into(),
                    active_90d: true,
                    truncated: false,
                },
            ),
            (
                "dependabot[bot]".into(),
                SurveyContributorV1 {
                    commits: 1.into(),
                    added: 0.into(),
                    removed: 0.into(),
                    first_unix_s: 1_700_000_000.into(),
                    last_unix_s: 1_700_000_000.into(),
                    active_90d: false,
                    truncated: false,
                },
            ),
        ]),
        seen: BTreeMap::new(),
    };
    let decisions = SurveyDecisionsV1 {
        count: 2.into(),
        by_status: BTreeMap::from([("accepted".into(), 1.into()), ("unknown".into(), 1.into())]),
        records: vec![
            SurveyDecisionV1 {
                number: "0001".into(),
                path: "docs/adr/0001-a.md".into(),
                title: None,
                truncated: false,
                status: None,
                date: None,
            },
            SurveyDecisionV1 {
                number: "0002".into(),
                path: "docs/adr/0002-b.md".into(),
                title: Some("Contact ada@example.com".into()),
                truncated: true,
                status: Some("accepted".into()),
                date: Some("2026-10-19".into()),
            },
        ],
        seen: BTreeMap::new(),
    };
    let term = |name: &str| SurveyNameV1 {
        name: name.into(),
        truncated: false,
    };
    let vocabulary = SurveyVocabularyV1 {
        file: "CONTEXT.md".into(),
        present: true,
        count: Some(2.into()),
        terms: Some(vec![term("Project"), term("Survey")]),
        seen: BTreeMap::new(),
    };
    let checks = SurveyChecksV1 {
        declared: vec![SurveyCheckV1 {
            source: ".af/code-policy.toml".into(),
            name: "check".into(),
            program: "make".into(),
            truncated: false,
        }],
        toolchain: vec!["Cargo.toml".into(), "rust-toolchain.toml".into()],
        ci_workflows: 1.into(),
        unreadable: Vec::new(),
        seen: BTreeMap::new(),
    };
    ProjectSurveyV1 {
        schema: PROJECT_SURVEY_V1.into(),
        completeness: SurveyCompletenessV1::Partial,
        previous_survey_id: Some(digest('c')),
        identity: SurveySectionV1::complete(identity),
        tree: SurveySectionV1::complete(tree),
        history: SurveySectionV1::partial(SurveyReasonV1::MaxCommits, history),
        contributors: SurveySectionV1::partial(SurveyReasonV1::MaxCommits, contributors),
        decisions: SurveySectionV1::complete(decisions),
        vocabulary: SurveySectionV1::complete(vocabulary),
        checks: SurveySectionV1::complete(checks),
    }
}

/// The record, its schema and the Rust validator agree on what a Survey is.
#[test]
fn a_survey_is_decimal_text_with_closed_sections_and_never_an_email() {
    use review_core::project::*;
    const SCHEMA: &str = "project-survey-v1.json";
    let survey = full_survey();
    survey.validate().unwrap();
    let value = serde_json::to_value(&survey).unwrap();
    assert_valid(SCHEMA, &value);
    assert_eq!(
        serde_json::from_value::<ProjectSurveyV1>(value.clone()).unwrap(),
        survey
    );
    assert_eq!(value["tree"]["files"], "3");
    assert_eq!(value["history"]["reason"], "max_commits");
    assert_eq!(value["history"]["windows"]["30d"]["commits"], "1");

    // An unavailable section is its completeness and its reason, nothing else.
    let mut cut = survey.clone();
    cut.decisions = SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs);
    cut.vocabulary = SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs);
    cut.checks = SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs);
    cut.validate().unwrap();
    let cut_value = serde_json::to_value(&cut).unwrap();
    assert_eq!(
        cut_value["checks"],
        json!({"completeness": "unavailable", "reason": "max_wall_ms"})
    );
    assert_valid(SCHEMA, &cut_value);

    // An early deadline leaves the Tree Digest absent with its reason, never a digest of a part;
    // a cut list says how many it saw; an unread declaration is listed; a name that is an
    // address is hashed.
    let mut cut_short = survey.clone();
    cut_short.completeness = SurveyCompletenessV1::Partial;
    if let Some(identity) = cut_short.identity.body.as_mut() {
        identity.tree_digest = None;
        identity.tree_digest_reason = Some(SurveyReasonV1::MaxWallMs);
    }
    cut_short.identity.completeness = SurveyCompletenessV1::Partial;
    cut_short.identity.reason = Some(SurveyReasonV1::MaxWallMs);
    if let Some(tree) = cut_short.tree.body.as_mut() {
        tree.seen = std::collections::BTreeMap::from([("paths".into(), 200_001.into())]);
    }
    cut_short.tree.completeness = SurveyCompletenessV1::Partial;
    cut_short.tree.reason = Some(SurveyReasonV1::MaxPaths);
    if let Some(checks) = cut_short.checks.body.as_mut() {
        checks.unreadable = vec![".af/pipelines/huge.toml".into()];
    }
    cut_short.checks.completeness = SurveyCompletenessV1::Partial;
    cut_short.checks.reason = Some(SurveyReasonV1::MaxFileBytes);
    if let Some(vocabulary) = cut_short.vocabulary.body.as_mut() {
        vocabulary.count = None;
        vocabulary.terms = None;
    }
    cut_short.vocabulary.completeness = SurveyCompletenessV1::Partial;
    cut_short.vocabulary.reason = Some(SurveyReasonV1::MaxFileBytes);
    if let Some(contributors) = cut_short.contributors.body.as_mut() {
        let entry = contributors.by_identity.remove("Ada").unwrap();
        contributors
            .by_identity
            .insert("h:0123456789abcdef".into(), entry);
    }
    cut_short.validate().unwrap();
    let cut_short_value = serde_json::to_value(&cut_short).unwrap();
    assert!(cut_short_value["identity"].get("tree_digest").is_none());
    assert_eq!(
        cut_short_value["identity"]["tree_digest_reason"],
        "max_wall_ms"
    );
    assert_eq!(cut_short_value["tree"]["seen"]["paths"], "200001");
    assert!(cut_short_value["vocabulary"].get("terms").is_none());
    assert_valid(SCHEMA, &cut_short_value);
    for (broken, why) in [
        {
            let mut both = cut_short.clone();
            if let Some(identity) = both.identity.body.as_mut() {
                identity.tree_digest = Some(digest('e'));
            }
            (both, "a Tree Digest and its absence's reason at once")
        },
        {
            let mut complete = cut_short.clone();
            complete.tree.completeness = SurveyCompletenessV1::Complete;
            complete.tree.reason = None;
            (complete, "a section that cut a list is never complete")
        },
        {
            let mut unread = cut_short.clone();
            unread.vocabulary.completeness = SurveyCompletenessV1::Complete;
            unread.vocabulary.reason = None;
            (
                unread,
                "an unread vocabulary is never complete with zero terms",
            )
        },
    ] {
        assert!(broken.validate().is_err(), "{why}");
    }

    let mut hashed = survey.clone();
    if let Some(contributors) = hashed.contributors.body.as_mut() {
        contributors.identity = SurveyIdentityPolicyV1::EmailHash;
        contributors.by_identity = contributors
            .by_identity
            .values()
            .cloned()
            .zip(["0123456789abcdef", "fedcba9876543210"])
            .map(|(entry, key)| (key.to_string(), entry))
            .collect();
    }
    hashed.validate().unwrap();
    assert_valid(SCHEMA, &serde_json::to_value(&hashed).unwrap());

    let pointer = |path: &str, replacement: Value| {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        changed
    };
    for (broken, why) in [
        (
            pointer("/tree/files", json!(3)),
            "every number is decimal text",
        ),
        (
            pointer("/tree/bytes", json!(30.5)),
            "a float is never a Survey number",
        ),
        (
            pointer(
                "/contributors/by_identity",
                json!({"ada@example.com": {
                "commits": "1", "added": "0", "removed": "0", "first_unix_s": "1",
                "last_unix_s": "1", "active_90d": true}}),
            ),
            "an email is never an identity",
        ),
        (
            pointer("/identity/git_version", json!("")),
            "the git version is git's line",
        ),
        (
            {
                let mut changed = value.clone();
                changed["identity"]
                    .as_object_mut()
                    .unwrap()
                    .remove("refs_digest");
                changed
            },
            "the identity names the refs of the survey key",
        ),
        (
            pointer("/decisions/records/0/number", json!("00001")),
            "a decision number is four digits",
        ),
        (
            pointer("/decisions/records/1/truncated", json!(false)),
            "only a cut string is marked",
        ),
        (
            {
                let mut changed = value.clone();
                changed["tree"]["seen"] = json!({"lines": "9"});
                changed
            },
            "seen names the section's lists",
        ),
        (
            pointer("/vocabulary/terms/0/name", json!("x".repeat(257))),
            "a display string is at most 256 bytes",
        ),
        (
            pointer("/checks", json!({"completeness": "unavailable"})),
            "an unavailable section names its reason",
        ),
        (
            pointer(
                "/checks",
                json!({"completeness": "unavailable", "reason": "max_wall_ms", "ci_workflows": "1"}),
            ),
            "an unavailable section measured nothing",
        ),
        (
            pointer("/tree/completeness", json!("unknown")),
            "completeness is closed",
        ),
        (
            pointer("/history/reason", json!("slow")),
            "the reasons are the bounds",
        ),
        (
            pointer("/identity/head", json!("HEAD")),
            "the head is a commit ID",
        ),
        (
            pointer("/checks/declared/0/program", json!("/home/me/bin/make")),
            "a program is never a host path",
        ),
        (
            pointer("/schema", json!("af/ProjectSurvey@2")),
            "the record names its version",
        ),
        (
            with(&value, "activity", json!({"completeness": "complete"})),
            "activity is not a section of this version",
        ),
    ] {
        assert_invalid(SCHEMA, &broken, why);
        let typed = serde_json::from_value::<ProjectSurveyV1>(broken.clone());
        assert!(
            typed.is_err() || typed.unwrap().validate().is_err(),
            "the Rust type accepted it ({why})"
        );
    }
    // What only the Rust validator can see: sums and the fold.
    let mut unfolded = survey.clone();
    unfolded.completeness = SurveyCompletenessV1::Complete;
    assert!(
        unfolded.validate().is_err(),
        "the record folds its sections"
    );
    let mut unsummed = survey;
    if let Some(tree) = unsummed.tree.body.as_mut() {
        tree.files = 4.into();
    }
    assert!(
        unsummed.validate().is_err(),
        "directories add up to the total"
    );
}

#[test]
fn survey_events_name_the_survey_by_digest_and_never_a_path_or_an_email() {
    use review_core::project::*;
    const RECORDED: &str = "project-survey-recorded-v1.json";
    const FAILED: &str = "project-survey-failed-v1.json";
    let recorded = SurveyRecordedPayloadV1 {
        survey_id: digest('a'),
        head: HEAD.into(),
        cutoff_unix_s: 1_792_000_000.into(),
        completeness: SurveyCompletenessV1::Complete,
        policy_digest: digest('b'),
        surveyor_version: 2.into(),
        refs_digest: digest('d'),
        previous_survey_id: Some(digest('c')),
        recorded_unix_ms: 1_792_000_000_123.into(),
    };
    recorded.validate().unwrap();
    let value = serde_json::to_value(&recorded).unwrap();
    accepted(RECORDED, EventType::SurveyRecordedV1, &value);
    accepted(
        RECORDED,
        EventType::SurveyRecordedV1,
        &without(&value, "previous_survey_id"),
    );
    assert_eq!(
        serde_json::from_value::<SurveyRecordedPayloadV1>(value.clone()).unwrap(),
        recorded
    );
    for (broken, why) in [
        (
            with(&value, "survey_id", json!("survey-1")),
            "a Survey is named by its digest",
        ),
        (
            with(&value, "head", json!("main")),
            "the head is a commit ID",
        ),
        (
            with(&value, "cutoff_unix_s", json!(1_792_000_000_u64)),
            "times are decimal text",
        ),
        (
            with(&value, "completeness", json!("done")),
            "completeness is closed",
        ),
        (
            with(&value, "path", json!("/home/me/afactory")),
            "no field holds a path",
        ),
        (
            without(&value, "policy_digest"),
            "the survey key needs the policy digest",
        ),
        (
            without(&value, "refs_digest"),
            "the survey key needs the refs digest",
        ),
    ] {
        refused(RECORDED, EventType::SurveyRecordedV1, &broken, why);
    }

    let failed = SurveyFailedPayloadV1 {
        head: HEAD.into(),
        reason: "the [survey] policy does not load: unknown field `refresh`".into(),
        recorded_unix_ms: 9.into(),
    };
    failed.validate().unwrap();
    let value = serde_json::to_value(&failed).unwrap();
    accepted(FAILED, EventType::SurveyFailedV1, &value);
    for (broken, why) in [
        (
            with(&value, "reason", json!("")),
            "a failure names its reason",
        ),
        (
            with(
                &value,
                "reason",
                json!("git log failed for ada@example.com"),
            ),
            "a reason never holds an email",
        ),
        (
            with(&value, "reason", json!("line\nbreak")),
            "a reason is one line",
        ),
        (
            with(&value, "head", json!("HEAD")),
            "the head is a commit ID",
        ),
    ] {
        refused(FAILED, EventType::SurveyFailedV1, &broken, why);
    }
}

#[test]
fn the_history_document_lists_the_chain_newest_first() {
    let entry = json!({
        "survey_id": digest('a'), "recorded_unix_ms": "2", "head": HEAD,
        "cutoff_unix_s": "1792000000", "completeness": "partial"
    });
    let document = json!({
        "schema": "af/project-history@1",
        "project_id": project_id(),
        "surveys": [entry.clone(), with(&entry, "survey_id", json!(digest('b')))],
    });
    assert_valid("project-history-v1.json", &document);
    assert_valid(
        "project-history-v1.json",
        &with(&document, "surveys", json!([])),
    );
    for (broken, why) in [
        (
            with(&document, "schema", json!("af/project-history@2")),
            "the document names its version",
        ),
        (
            with(
                &document,
                "surveys",
                json!([with(&entry, "recorded_unix_ms", json!(2))]),
            ),
            "times are decimal text",
        ),
        (
            with(
                &document,
                "surveys",
                json!([with(&entry, "store", json!("/home/me/.local/state"))]),
            ),
            "an entry is closed",
        ),
        (without(&document, "surveys"), "the chain is listed"),
    ] {
        assert_invalid("project-history-v1.json", &broken, why);
    }
}
