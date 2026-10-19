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
