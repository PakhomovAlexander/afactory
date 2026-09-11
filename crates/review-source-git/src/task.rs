//! Task source identity. These additive records do not reinterpret the frozen Review Kernel
//! SourceSnapshot@1 Capture::Derived, whose meaning includes checked internal Integration.

use review_core::task::ArtifactInputV1;
use review_core::{PortCardinality, Producer, is_digest};
use review_store::Cas;
use serde::{Deserialize, Serialize};

use crate::Manifest;

pub const SOURCE_TREE_V1: &str = "af/SourceTree@1";
pub const CANDIDATE_TREE_V1: &str = "af/CandidateTree@1";

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
    mut refs: Vec<String>,
) -> Result<ArtifactInputV1, String> {
    read_snapshot(cas, snapshot_id)?;
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
