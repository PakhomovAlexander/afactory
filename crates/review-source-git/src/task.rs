//! Task source identity. These additive records do not reinterpret the frozen Review Kernel
//! SourceSnapshot@1 Capture::Derived, whose meaning includes checked internal Integration.

use review_core::task::ArtifactInputV1;
use review_core::{PortCardinality, Producer, is_digest};
use review_store::Cas;
use serde::{Deserialize, Serialize};

use crate::Manifest;

pub const SOURCE_TREE_V1: &str = "af/SourceTree@1";
pub const CANDIDATE_TREE_V1: &str = "af/CandidateTree@1";

/// Where a bound `source` came from: the artifact and Snapshot the Task file referenced, and
/// the Task that produced them when one was named. Display and provenance only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceBoundFromV1 {
    pub artifact_id: String,
    pub snapshot_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub task: Option<review_core::task::input_bindings::ReferencedTaskV1>,
}

/// Generation two of a Task source origin: the origin of a Snapshot that was re-rooted from a
/// recorded Task output rather than captured from a checkout (ADR-0117). It deliberately has
/// no `source_revision` — a derived tree has no commit, and inventing one would let delivery
/// compare the target repository against a different tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceOriginV2 {
    pub schema: String,
    pub repository_id: String,
    pub content_digest: String,
    pub bound_from: TaskSourceBoundFromV1,
}

/// Generation one, written unchanged for every ordinary capture. `source_revision` is absent
/// or null for a dirty capture, which is exactly what the writer has always produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceOriginV1 {
    pub schema: String,
    pub repository_id: String,
    #[serde(default)]
    pub source_revision: Option<String>,
    pub content_digest: String,
}

/// Either recorded generation. Readers branch on `schema` and never guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSourceOrigin {
    Captured(TaskSourceOriginV1),
    Bound(TaskSourceOriginV2),
}

impl TaskSourceOrigin {
    pub fn repository_id(&self) -> &str {
        match self {
            Self::Captured(origin) => &origin.repository_id,
            Self::Bound(origin) => &origin.repository_id,
        }
    }
    pub fn content_digest(&self) -> &str {
        match self {
            Self::Captured(origin) => &origin.content_digest,
            Self::Bound(origin) => &origin.content_digest,
        }
    }
    /// The committed revision this tree corresponds to, when one exists. A generation-two
    /// origin never has one, and neither does a dirty generation-one capture.
    pub fn source_revision(&self) -> Option<&str> {
        match self {
            Self::Captured(origin) => origin.source_revision.as_deref(),
            Self::Bound(_) => None,
        }
    }
    pub fn bound_from(&self) -> Option<&TaskSourceBoundFromV1> {
        match self {
            Self::Captured(_) => None,
            Self::Bound(origin) => Some(&origin.bound_from),
        }
    }
}

pub fn read_origin(cas: &Cas, id: &str) -> Result<TaskSourceOrigin, String> {
    if !is_digest(id) {
        return Err("Task source origin identity is invalid".into());
    }
    let value = cas.get_json(id).map_err(|e| e.to_string())?;
    let declared = value.get("schema").and_then(serde_json::Value::as_str);
    let schema = declared.unwrap_or_default().to_owned();
    match schema.as_str() {
        "af.task-source-origin/1" => {
            let read: Result<TaskSourceOriginV1, _> = serde_json::from_value(value);
            let origin = read.map_err(|e| e.to_string())?;
            Ok(TaskSourceOrigin::Captured(origin))
        }
        "af.task-source-origin/2" => {
            let read: Result<TaskSourceOriginV2, _> = serde_json::from_value(value);
            let origin = read.map_err(|e| e.to_string())?;
            Ok(TaskSourceOrigin::Bound(origin))
        }
        _ => Err("Unsupported Task source origin".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSnapshot {
    pub schema: String,
    pub manifest_id: String,
    pub content_digest: String,
    pub origin_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub parent_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTree {
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateTree {
    pub parent_snapshot_id: String,
    pub manifest_id: String,
}

pub fn read_manifest(cas: &Cas, id: &str) -> Result<Manifest, String> {
    if !is_digest(id) {
        return Err("Task Manifest identity is invalid".into());
    }
    let manifest: Manifest = serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    manifest.validate().map_err(|e| e.to_string())?;
    for entry in &manifest.entries {
        if crate::decode_path(&entry.path)
            .split(|byte| *byte == b'/')
            .any(|part| part.eq_ignore_ascii_case(b".git"))
        {
            return Err("Task tree contains Git administration paths".into());
        }
        if cas.verify(&entry.content).map_err(|e| e.to_string())? != entry.size {
            return Err("Task Manifest entry length differs from captured bytes".into());
        }
    }
    Ok(manifest)
}

pub fn read_snapshot(cas: &Cas, id: &str) -> Result<(TaskSnapshot, Manifest), String> {
    if !is_digest(id) {
        return Err("Task Snapshot identity is invalid".into());
    }
    let snapshot: TaskSnapshot =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if snapshot.schema != "af.task-snapshot/1"
        || !is_digest(&snapshot.origin_id)
        || snapshot
            .parent_snapshot_id
            .as_deref()
            .is_some_and(|parent| !is_digest(parent) || parent == id)
    {
        return Err("Unsupported or invalid Task Snapshot".into());
    }
    cas.verify(&snapshot.origin_id).map_err(|e| e.to_string())?;
    let manifest = read_manifest(cas, &snapshot.manifest_id)?;
    if manifest.content_digest() != snapshot.content_digest {
        return Err("Task Snapshot content differs from its Manifest".into());
    }
    Ok((snapshot, manifest))
}

pub fn capture_snapshot(
    cas: &Cas,
    manifest: &Manifest,
    origin_id: &str,
    parent: Option<&str>,
) -> Result<String, String> {
    cas.verify(origin_id).map_err(|e| e.to_string())?;
    if let Some(parent) = parent {
        let (previous, _) = read_snapshot(cas, parent)?;
        if previous.origin_id != origin_id {
            return Err("Derived Task Snapshot changed source origin".into());
        }
    }
    let manifest_id = cas
        .put_json(&serde_json::to_value(manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    read_manifest(cas, &manifest_id)?;
    publish_snapshot(cas, manifest, manifest_id, origin_id, parent)
}

// Only called immediately after this operation has validated the complete source bytes.
fn publish_snapshot(
    cas: &Cas,
    manifest: &Manifest,
    manifest_id: String,
    origin_id: &str,
    parent: Option<&str>,
) -> Result<String, String> {
    let record = TaskSnapshot {
        schema: "af.task-snapshot/1".into(),
        manifest_id,
        content_digest: manifest.content_digest(),
        origin_id: origin_id.into(),
        parent_snapshot_id: parent.map(str::to_owned),
    };
    cas.put_json(&serde_json::to_value(record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

pub fn source_tree(
    cas: &Cas,
    producer: Producer,
    snapshot_id: &str,
    refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    read_snapshot(cas, snapshot_id)?;
    publish_source_tree(cas, producer, snapshot_id, refs)
}

fn publish_source_tree(
    cas: &Cas,
    producer: Producer,
    snapshot_id: &str,
    mut refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    refs.push(snapshot_id.into());
    refs.sort();
    refs.dedup();
    let id = cas
        .put_artifact(
            SOURCE_TREE_V1,
            producer,
            refs,
            Some(snapshot_id.into()),
            serde_json::to_value(SourceTree {
                snapshot_id: snapshot_id.into(),
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    Ok(ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: SOURCE_TREE_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(snapshot_id.into()),
    })
}

/// Follow only captured same-origin Snapshot ancestry; never recapture a live worktree.
pub fn descends_from(cas: &Cas, child: &str, ancestor: &str) -> Result<bool, String> {
    let (root, _) = read_snapshot(cas, ancestor)?;
    let mut current = child.to_owned();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..64 {
        if !seen.insert(current.clone()) {
            return Err("Cyclic Task Snapshot ancestry".into());
        }
        let (snapshot, _) = read_snapshot(cas, &current)?;
        if snapshot.origin_id != root.origin_id {
            return Err("Task Snapshot ancestry changed origin".into());
        }
        match snapshot.parent_snapshot_id {
            Some(parent) if parent == ancestor => return Ok(true),
            Some(parent) => current = parent,
            None => return Ok(false),
        }
    }
    Err("Task Snapshot ancestry exceeds graph bound".into())
}

/// Seal one candidate against its captured parent in a single trusted operation. Both trees
/// are freshly verified; publishing their derived metadata does not reread those same trees.
/// No verified state escapes this call or authorizes a later operation without fresh reads.
pub fn derive_source_tree(
    cas: &Cas,
    producer: Producer,
    manifest_id: &str,
    parent_snapshot_id: &str,
    refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    let (parent, _) = read_snapshot(cas, parent_snapshot_id)?;
    let manifest = read_manifest(cas, manifest_id)?;
    let canonical = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if canonical != manifest_id {
        return Err("Candidate Manifest is not canonical".into());
    }
    let snapshot_id = publish_snapshot(
        cas,
        &manifest,
        canonical,
        &parent.origin_id,
        Some(parent_snapshot_id),
    )?;
    publish_source_tree(cas, producer, &snapshot_id, refs)
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    use serde_json::json;

    fn cas() -> (tempfile::TempDir, Cas) {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        (directory, cas)
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn bound_from() -> serde_json::Value {
        json!({"artifact_id":digest('b'),"snapshot_id":digest('c')})
    }

    fn referenced_task() -> serde_json::Value {
        json!({"task_id":"layout-l3b","task_revision_id":digest('1'),
            "result_id":digest('2'),"port":"snapshot","acceptance":"unsatisfied",
            "domain_conclusion":"changes_requested"})
    }

    #[test]
    fn both_origin_generations_read_and_only_generation_one_carries_a_revision() {
        let (_directory, cas) = cas();
        let value = json!({"schema":"af.task-source-origin/1",
            "repository_id":"example/hub","source_revision":"bba24cb",
            "content_digest":digest('a')});
        let committed = cas.put_json(&value).unwrap();
        let origin = read_origin(&cas, &committed).unwrap();
        assert_eq!(origin.repository_id(), "example/hub");
        assert_eq!(origin.content_digest(), digest('a'));
        assert_eq!(origin.source_revision(), Some("bba24cb"));
        assert!(origin.bound_from().is_none());

        // A dirty capture has always written an explicit null here; it stays readable.
        let value = json!({"schema":"af.task-source-origin/1",
            "repository_id":"example/hub","source_revision":null,
            "content_digest":digest('a')});
        let dirty = cas.put_json(&value).unwrap();
        let origin = read_origin(&cas, &dirty).unwrap();
        assert_eq!(origin.source_revision(), None);

        let mut from = bound_from();
        from["task"] = referenced_task();
        let value = json!({"schema":"af.task-source-origin/2",
            "repository_id":"example/hub","content_digest":digest('a'),
            "bound_from":from});
        let bound = cas.put_json(&value).unwrap();
        let origin = read_origin(&cas, &bound).unwrap();
        assert_eq!(origin.repository_id(), "example/hub");
        let revision = origin.source_revision();
        assert_eq!(revision, None, "a re-rooted tree has no commit");
        let task = origin.bound_from().unwrap().task.as_ref().unwrap();
        assert_eq!(task.task_id, "layout-l3b");
        assert_eq!(task.port, "snapshot");
        task.validate().unwrap();
    }

    #[test]
    fn an_unknown_field_or_generation_is_refused_in_either_origin() {
        let (_directory, cas) = cas();
        let mut extra = bound_from();
        extra["why"] = json!("x");
        let invalid = [
            json!({"schema":"af.task-source-origin/1","repository_id":"r",
                "content_digest":digest('a'),"source_revision":"c",
                "bound_from":bound_from()}),
            json!({"schema":"af.task-source-origin/2","repository_id":"r",
                "content_digest":digest('a'),"source_revision":"c",
                "bound_from":bound_from()}),
            json!({"schema":"af.task-source-origin/2","repository_id":"r",
                "content_digest":digest('a'),"bound_from":extra}),
            json!({"schema":"af.task-source-origin/3","repository_id":"r",
                "content_digest":digest('a')}),
        ];
        for value in invalid {
            let id = cas.put_json(&value).unwrap();
            assert!(read_origin(&cas, &id).is_err(), "{value}");
        }

        let value = json!({"schema":"af.task-source-origin/2","repository_id":"r",
            "content_digest":digest('a'),"bound_from":bound_from()});
        let parentless = cas.put_json(&value).unwrap();
        let origin = read_origin(&cas, &parentless).unwrap();
        let named = origin.bound_from().unwrap().task.is_none();
        assert!(named, "an exact-artifact binding names no Task");
    }
}
