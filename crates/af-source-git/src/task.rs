//! Task source identity. These additive records do not reinterpret the frozen Review Kernel
//! SourceSnapshot@1 Capture::Derived, whose meaning includes checked internal Integration.

use af_contracts::task::ArtifactInputV1;
use af_contracts::{PortCardinality, Producer, is_digest};
use af_store::Cas;
use serde::{Deserialize, Serialize};

use crate::Manifest;

pub const SOURCE_TREE_V1: &str = "af/SourceTree@1";
pub const CANDIDATE_TREE_V1: &str = "af/CandidateTree@1";
pub const TASK_SOURCE_ORIGIN_SCHEMA: &str = "af.task-source-origin/3";

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
        deserialize_with = "af_contracts::task::present_option"
    )]
    pub task: Option<af_contracts::task::input_bindings::ReferencedTaskV1>,
}

/// Current tagged provenance. A bound output cannot claim a Git commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskSourceOrigin {
    Captured {
        schema: String,
        repository_id: String,
        content_digest: String,
        source_revision: Option<String>,
    },
    Bound {
        schema: String,
        repository_id: String,
        content_digest: String,
        bound_from: TaskSourceBoundFromV1,
    },
}
impl TaskSourceOrigin {
    pub fn validate(&self) -> Result<(), String> {
        let schema = match self {
            Self::Captured { schema, .. } | Self::Bound { schema, .. } => schema,
        };
        if schema != TASK_SOURCE_ORIGIN_SCHEMA
            || self.repository_id().is_empty()
            || self.repository_id().len() > 512
            || !is_digest(self.content_digest())
        {
            return Err("Unsupported or invalid Task source origin".into());
        }
        if let Some(revision) = self.source_revision()
            && (revision.is_empty()
                || revision.len() > 64
                || !revision.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("Captured Task source revision must be a Git object name".into());
        }
        if let Some(from) = self.bound_from() {
            if !is_digest(&from.artifact_id) || !is_digest(&from.snapshot_id) {
                return Err(
                    "Bound Task source needs exact artifact and Snapshot provenance".into(),
                );
            }
            if let Some(task) = &from.task {
                task.validate()?;
            }
        }
        Ok(())
    }
    pub fn repository_id(&self) -> &str {
        match self {
            Self::Captured { repository_id, .. } | Self::Bound { repository_id, .. } => {
                repository_id
            }
        }
    }
    pub fn content_digest(&self) -> &str {
        match self {
            Self::Captured { content_digest, .. } | Self::Bound { content_digest, .. } => {
                content_digest
            }
        }
    }
    pub fn source_revision(&self) -> Option<&str> {
        match self {
            Self::Captured {
                source_revision, ..
            } => source_revision.as_deref(),
            Self::Bound { .. } => None,
        }
    }
    pub fn bound_from(&self) -> Option<&TaskSourceBoundFromV1> {
        match self {
            Self::Captured { .. } => None,
            Self::Bound { bound_from, .. } => Some(bound_from),
        }
    }
}

/// Publish only a validated current provenance record, using the same type the reader admits.
pub fn write_origin(cas: &Cas, origin: &TaskSourceOrigin) -> Result<String, String> {
    origin.validate()?;
    cas.put_json(&serde_json::to_value(origin).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

pub fn read_origin(cas: &Cas, id: &str) -> Result<TaskSourceOrigin, String> {
    if !is_digest(id) {
        return Err("Task source origin identity is invalid".into());
    }
    let origin: TaskSourceOrigin =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    origin.validate()?;
    Ok(origin)
}

pub use af_contracts::task::source::TaskSnapshot;

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
        if crate::git_refuses_path(&crate::decode_path(&entry.path), entry.kind) {
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
    snapshot.validate(id)?;
    read_origin(cas, &snapshot.origin_id)?;
    let manifest = read_manifest(cas, &snapshot.manifest_id)?;
    if manifest.content_digest() != snapshot.content_digest {
        return Err("Task Snapshot content differs from its Manifest".into());
    }
    Ok((snapshot, manifest))
}

/// Resolve the Task's exact declared base, as execution does (ADR-0212).
pub fn declared_base(
    cas: &Cas,
    task: Option<&af_contracts::task::TaskRevisionV1>,
) -> Result<Option<String>, String> {
    use af_contracts::task::findings::{TASK_BASE_V1, TaskBaseV1};
    let Some(task) = task else {
        return Ok(None);
    };
    for id in &task.provenance.input_artifact_ids {
        let Some(artifact) = cas.get_optional_artifact(id).map_err(|e| e.to_string())? else {
            continue;
        };
        if artifact.artifact_type != TASK_BASE_V1 {
            continue;
        }
        let base: TaskBaseV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        base.validate()?;
        return Ok(Some(base.snapshot_id));
    }
    Ok(None)
}

/// Resolve a source's recorded base, including a re-rooted bound candidate (ADR-0212).
pub fn recorded_base(cas: &Cas, snapshot: &TaskSnapshot) -> Result<Option<String>, String> {
    if let Some(parent) = &snapshot.parent_snapshot_id {
        return Ok(Some(parent.clone()));
    }
    // Only a bound origin names a tree this one came from; any other origin, typed or not,
    // names none.
    let Ok(origin) = read_origin(cas, &snapshot.origin_id) else {
        return Ok(None);
    };
    let Some(bound) = origin.bound_from() else {
        return Ok(None);
    };
    let (from, _) = read_snapshot(cas, &bound.snapshot_id)?;
    Ok(from.parent_snapshot_id)
}

pub fn capture_snapshot(
    cas: &Cas,
    manifest: &Manifest,
    origin_id: &str,
    parent: Option<&str>,
) -> Result<String, String> {
    read_origin(cas, origin_id)?;
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
    af_contracts::task::source::snapshot_descends_from(child, ancestor, |id| {
        read_snapshot(cas, id).map(|(snapshot, _)| snapshot)
    })
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

    fn origin_schema() -> jsonschema::Validator {
        record_schema("task-source-origin-v3.json")
    }

    fn record_schema(selected: &str) -> jsonschema::Validator {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let mut builder = jsonschema::Registry::new();
        let mut origin = serde_json::Value::Null;
        for name in [
            "task-contracts-v4.json",
            "task-input-bindings-v1.json",
            selected,
        ] {
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("schemas").join(name)).unwrap())
                    .unwrap();
            let id = value["$id"].as_str().unwrap().to_owned();
            if name == selected {
                origin = value.clone();
            }
            builder = builder
                .add(id, jsonschema::Resource::from_contents(value))
                .unwrap();
        }
        let registry = builder.prepare().unwrap();
        jsonschema::options()
            .with_registry(&registry)
            .build(&origin)
            .unwrap()
    }

    fn checked_write(
        cas: &Cas,
        schema: &jsonschema::Validator,
        value: &serde_json::Value,
    ) -> String {
        assert!(schema.is_valid(value), "{value}");
        let origin: TaskSourceOrigin = serde_json::from_value(value.clone()).unwrap();
        let id = write_origin(cas, &origin).unwrap();
        assert_eq!(read_origin(cas, &id).unwrap(), origin);
        assert_eq!(cas.get_json(&id).unwrap(), *value);
        id
    }

    #[test]
    fn captured_snapshot_ancestry_preserves_siblings_origins_integrity_and_the_64_link_bound() {
        let (_directory, cas) = cas();
        let manifest = Manifest::new(vec![]).unwrap();
        let origin = write_origin(
            &cas,
            &TaskSourceOrigin::Captured {
                schema: TASK_SOURCE_ORIGIN_SCHEMA.into(),
                repository_id: "ancestry".into(),
                content_digest: manifest.content_digest(),
                source_revision: None,
            },
        )
        .unwrap();
        let root = capture_snapshot(&cas, &manifest, &origin, None).unwrap();
        let mut chain = vec![root.clone()];
        for _ in 0..65 {
            chain.push(
                capture_snapshot(&cas, &manifest, &origin, chain.last().map(String::as_str))
                    .unwrap(),
            );
        }
        let schema = record_schema("task-snapshot-v1.json");
        for id in [&root, &chain[64]] {
            let actual = cas.get_json(id).unwrap();
            assert!(schema.is_valid(&actual));
            assert_eq!(
                serde_json::to_value(read_snapshot(&cas, id).unwrap().0).unwrap(),
                actual
            );
            let mut open = actual.clone();
            open["unadmitted_parent"] = json!(root);
            assert!(!schema.is_valid(&open));
            assert!(serde_json::from_value::<TaskSnapshot>(open).is_err());
        }
        assert!(descends_from(&cas, &chain[64], &root).unwrap());
        assert!(
            descends_from(&cas, &chain[65], &root)
                .unwrap_err()
                .contains("graph bound")
        );
        assert!(!descends_from(&cas, &root, &chain[1]).unwrap());
        let sibling_manifest = Manifest::new(vec![crate::Entry {
            path: "sibling".into(),
            kind: crate::EntryKind::File,
            content: cas.put(b"sibling").unwrap(),
            size: 7,
        }])
        .unwrap();
        let sibling = capture_snapshot(&cas, &sibling_manifest, &origin, Some(&root)).unwrap();
        assert!(!descends_from(&cas, &sibling, &chain[1]).unwrap());
        let other_origin = write_origin(
            &cas,
            &TaskSourceOrigin::Captured {
                schema: TASK_SOURCE_ORIGIN_SCHEMA.into(),
                repository_id: "unrelated".into(),
                content_digest: manifest.content_digest(),
                source_revision: None,
            },
        )
        .unwrap();
        let other = capture_snapshot(&cas, &manifest, &other_origin, None).unwrap();
        assert!(
            descends_from(&cas, &other, &root)
                .unwrap_err()
                .contains("changed origin")
        );
        let mut forged = read_snapshot(&cas, &chain[1]).unwrap().0;
        forged.origin_id = other_origin;
        let forged = cas
            .put_json(&serde_json::to_value(forged).unwrap())
            .unwrap();
        assert!(
            descends_from(&cas, &forged, &root)
                .unwrap_err()
                .contains("changed origin")
        );
        let mut corrupt = read_snapshot(&cas, &chain[1]).unwrap().0;
        corrupt.manifest_id = cas.put(b"not a Manifest").unwrap();
        let corrupt = cas
            .put_json(&serde_json::to_value(corrupt).unwrap())
            .unwrap();
        assert!(descends_from(&cas, &corrupt, &root).is_err());
    }

    #[test]
    fn current_origin_preserves_captured_commit_dirty_capture_and_bound_task_provenance() {
        let (_directory, cas) = cas();
        let schema = origin_schema();
        let value = json!({"schema":"af.task-source-origin/3","kind":"captured",
            "repository_id":"example/hub","source_revision":"bba24cb",
            "content_digest":digest('a')});
        let committed = checked_write(&cas, &schema, &value);
        let origin = read_origin(&cas, &committed).unwrap();
        assert_eq!(origin.repository_id(), "example/hub");
        assert_eq!(origin.content_digest(), digest('a'));
        assert_eq!(origin.source_revision(), Some("bba24cb"));
        assert!(origin.bound_from().is_none());

        // A current dirty capture carries no committed revision.
        let value = json!({"schema":"af.task-source-origin/3","kind":"captured",
            "repository_id":"example/hub","source_revision":null,
            "content_digest":digest('a')});
        let dirty = checked_write(&cas, &schema, &value);
        let origin = read_origin(&cas, &dirty).unwrap();
        assert_eq!(origin.source_revision(), None);

        let mut from = bound_from();
        from["task"] = referenced_task();
        let value = json!({"schema":"af.task-source-origin/3","kind":"bound",
            "repository_id":"example/hub","content_digest":digest('a'),
            "bound_from":from});
        let bound = checked_write(&cas, &schema, &value);
        let origin = read_origin(&cas, &bound).unwrap();
        assert_eq!(origin.repository_id(), "example/hub");
        let revision = origin.source_revision();
        assert_eq!(revision, None, "a re-rooted tree has no commit");
        let task = origin.bound_from().unwrap().task.as_ref().unwrap();
        assert_eq!(task.task_id, "layout-l3b");
        assert_eq!(task.port, "snapshot");
        task.validate().unwrap();
    }

    /// Task trees refuse Git administration through the predicate materialization uses.
    #[test]
    fn a_task_manifest_naming_git_administration_is_refused_in_every_spelling() {
        let (_directory, cas) = cas();
        let content = cas.put(b"x").unwrap();
        let manifest = |path: &str, kind: crate::EntryKind| {
            let entry = crate::Entry {
                path: path.into(),
                kind,
                content: content.clone(),
                size: 1,
            };
            let manifest = Manifest::new(vec![entry]).unwrap();
            cas.put_json(&serde_json::to_value(manifest).unwrap())
                .unwrap()
        };
        let refused = [
            (".git/config", crate::EntryKind::File),
            (".GIT/HEAD", crate::EntryKind::File),
            (".git./x", crate::EntryKind::File),
            ("git~1/x", crate::EntryKind::File),
            (".gitmodules", crate::EntryKind::Symlink),
        ];
        for (path, kind) in refused {
            let error = read_manifest(&cas, &manifest(path, kind)).unwrap_err();
            assert_eq!(
                error, "Task tree contains Git administration paths",
                "{path}"
            );
        }
        for (path, kind) in [
            (".gitmodules", crate::EntryKind::File),
            (".github/ci.yml", crate::EntryKind::File),
        ] {
            read_manifest(&cas, &manifest(path, kind)).unwrap();
        }
    }

    #[test]
    fn retired_origins_unknown_fields_and_bound_commits_fail_closed() {
        let (_directory, cas) = cas();
        let schema = origin_schema();
        let mut extra = bound_from();
        extra["why"] = json!("x");
        let mut invalid = vec![
            json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"r",
                "content_digest":digest('a'),"source_revision":"c",
                "bound_from":bound_from()}),
            json!({"schema":"af.task-source-origin/3","kind":"bound","repository_id":"r",
                "content_digest":digest('a'),"source_revision":"c",
                "bound_from":bound_from()}),
            json!({"schema":"af.task-source-origin/3","kind":"bound","repository_id":"r",
                "content_digest":digest('a'),"bound_from":extra}),
            json!({"schema":"af.task-source-origin/3","repository_id":"r",
                "content_digest":digest('a')}),
        ];
        for (field, bad) in [
            ("repository_id", json!("")),
            ("content_digest", json!("invented")),
            ("source_revision", json!("not-a-commit")),
        ] {
            let mut value = json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"r",
                "content_digest":digest('a'),"source_revision":"bba24cb"});
            value[field] = bad;
            invalid.push(value);
        }
        let manifest = Manifest::new(vec![]).unwrap();
        let manifest_id = cas
            .put_json(&serde_json::to_value(&manifest).unwrap())
            .unwrap();
        for version in [1, 2] {
            let mut value = json!({"schema":format!("af.task-source-origin/{version}"),
                "repository_id":"r","content_digest":manifest.content_digest()});
            if version == 1 {
                value["source_revision"] = serde_json::Value::Null;
            } else {
                value["bound_from"] = bound_from();
            }
            for tagged in [false, true] {
                if tagged {
                    value["kind"] = json!(if version == 1 { "captured" } else { "bound" });
                }
                assert!(!schema.is_valid(&value), "retired {value}");
                let id = cas.put_json(&value).unwrap();
                assert!(read_origin(&cas, &id).is_err(), "retired {value}");
                assert!(capture_snapshot(&cas, &manifest, &id, None).is_err());
                // An unchanged Snapshot wrapper cannot smuggle a retired origin into a current SourceTree.
                let snapshot = TaskSnapshot {
                    schema: "af.task-snapshot/1".into(),
                    manifest_id: manifest_id.clone(),
                    content_digest: manifest.content_digest(),
                    origin_id: id,
                    parent_snapshot_id: None,
                };
                let snapshot_id = cas
                    .put_json(&serde_json::to_value(snapshot).unwrap())
                    .unwrap();
                assert!(read_snapshot(&cas, &snapshot_id).is_err());
                assert!(
                    source_tree(
                        &cas,
                        Producer::KernelOperation {
                            run_id: "fixture".into(),
                            node_id: None,
                            operation_id: "source@1".into()
                        },
                        &snapshot_id,
                        vec![]
                    )
                    .is_err()
                );
            }
        }
        for value in invalid {
            assert!(!schema.is_valid(&value), "{value}");
            let id = cas.put_json(&value).unwrap();
            assert!(read_origin(&cas, &id).is_err(), "{value}");
        }

        let value = json!({"schema":"af.task-source-origin/3","kind":"bound","repository_id":"r",
            "content_digest":digest('a'),"bound_from":bound_from()});
        let parentless = cas.put_json(&value).unwrap();
        let origin = read_origin(&cas, &parentless).unwrap();
        let named = origin.bound_from().unwrap().task.is_none();
        assert!(named, "an exact-artifact binding names no Task");
    }
}
