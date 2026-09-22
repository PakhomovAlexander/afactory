//! Worker warm layers, package P1: `WorkerNotes@1`, `HeadDelta@1` and `WarmSet@1`.
//!
//! Warmth is an artifact, never ambient state. Every layer a Worker starts from is a CAS
//! artifact selected before its Attempt is dispatched and listed in the Attempt's context
//! manifest. Notes are an inspection map left by one admitted Attempt for the next Attempt of
//! the same node; a Head Delta is the kernel's own relation between two consecutive heads and
//! carries no Subject identity and no Report Scope; a Warm Set is the exact per-node selection
//! recorded for one Round.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::change_set::PathRenameV1;
use crate::workspace::{WorkspaceBasisV1, is_workspace_id};

/// Hard upper bound for one encoded `WorkerNotes@1` payload. Policy may lower it, never raise it.
pub const MAX_WORKER_NOTES_BYTES: usize = 64 * 1024;

/// Default `notes_max_bytes` when a reviewer node enables Notes without naming a bound.
pub const DEFAULT_WORKER_NOTES_BYTES: usize = 16 * 1024;

/// Upper bound for one encoded `HeadDelta@1` rendered beside the Change Set.
pub const MAX_HEAD_DELTA_BYTES: usize = 256 * 1024;

fn is_false(value: &bool) -> bool {
    !*value
}

/// One path the Worker inspected, with the head tree entry digest the kernel resolved for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectedPathV1 {
    pub path: String,
    /// Content digest of the head tree entry at the time the notes were written. Absent when
    /// the Worker named a path the head tree does not contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_entry_digest: Option<String>,
}

/// One per-path hint. Data for the next Attempt, never a disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathHintV1 {
    pub path: String,
    pub note: String,
}

/// The payload of a `review.kernel/WorkerNotes@1` or `af/WorkerNotes@1` artifact.
///
/// An inspection map, not a verdict: verdicts live in the Ledger as Findings, and every prior
/// Finding still needs its explicit Report, Dispute or Drop whatever these notes say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerNotesV1 {
    pub node: String,
    pub attempt_id: String,
    pub head_snapshot_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inspected: Vec<InspectedPathV1>,
    pub model_of_change: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<PathHintV1>,
}

impl WorkerNotesV1 {
    /// Bind a Worker-supplied `af/WorkerNotes@1` payload to kernel authority. The Worker
    /// supplies the inspection map; the kernel supplies `node`, `attempt_id` and
    /// `head_snapshot_id`, overwriting whatever the Worker wrote there. Unknown fields and
    /// invalid paths are refused, so a reply cannot smuggle a verdict under the Notes type.
    pub fn bind(
        value: serde_json::Value,
        node: &str,
        attempt_id: &str,
        head_snapshot_id: &str,
    ) -> Result<Self, String> {
        let serde_json::Value::Object(mut fields) = value else {
            return Err("Worker Notes must be one JSON object".into());
        };
        fields.insert("node".into(), serde_json::Value::String(node.into()));
        fields.insert(
            "attempt_id".into(),
            serde_json::Value::String(attempt_id.into()),
        );
        fields.insert(
            "head_snapshot_id".into(),
            serde_json::Value::String(head_snapshot_id.into()),
        );
        let notes: Self = serde_json::from_value(serde_json::Value::Object(fields))
            .map_err(|error| format!("Worker Notes do not match WorkerNotes@1: {error}"))?;
        notes.validate()?;
        Ok(notes)
    }

    /// Check a durably stored `af/WorkerNotes@1` payload against the authority that bound it.
    pub fn check_bound(
        &self,
        node: &str,
        attempt_id: &str,
        head_snapshot_id: &str,
    ) -> Result<(), String> {
        self.validate()?;
        if self.node != node
            || self.attempt_id != attempt_id
            || self.head_snapshot_id != head_snapshot_id
        {
            return Err("Worker Notes name another node, Attempt or head Snapshot".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("WorkerNotes@1 has an empty node".into());
        }
        if !is_monotonic_id(&self.attempt_id) {
            return Err("WorkerNotes@1 has an invalid Attempt ID".into());
        }
        if !crate::is_digest(&self.head_snapshot_id) {
            return Err("WorkerNotes@1 has an invalid head Snapshot ID".into());
        }
        let mut inspected = BTreeSet::new();
        for entry in &self.inspected {
            if !crate::is_valid_repo_path(&entry.path) || !inspected.insert(entry.path.as_str()) {
                return Err("WorkerNotes@1 inspected paths must be canonical and unique".into());
            }
            if !entry
                .tree_entry_digest
                .as_deref()
                .is_none_or(crate::is_digest)
            {
                return Err("WorkerNotes@1 has an invalid tree entry digest".into());
            }
        }
        for hint in &self.hints {
            if !crate::is_valid_repo_path(&hint.path) || hint.note.trim().is_empty() {
                return Err("WorkerNotes@1 hints need a canonical path and a note".into());
            }
        }
        if self
            .open_questions
            .iter()
            .any(|question| question.trim().is_empty())
        {
            return Err("WorkerNotes@1 has an empty open question".into());
        }
        Ok(())
    }

    /// Every path these notes mention. Head Delta marks are computed over this union so a
    /// path the notes reference always receives a mark, even when the current Change Set no
    /// longer contains it.
    pub fn referenced_paths(&self) -> BTreeSet<String> {
        self.inspected
            .iter()
            .map(|entry| entry.path.clone())
            .chain(self.hints.iter().map(|hint| hint.path.clone()))
            .collect()
    }
}

/// Why an Attempt's notes did not become a `WorkerNotes@1` artifact. The Attempt is admitted
/// regardless; only the layer is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerNotesDropReasonV1 {
    /// The reviewer returned no `notes` field.
    Absent,
    /// The `notes` field did not have the declared shape.
    Malformed,
    /// A referenced path is not a canonical repository-relative path.
    InvalidPath,
    /// The encoded artifact exceeds the policy bound `notes_max_bytes`.
    OverBound,
}

/// Payload of `WorkerNotesRecorded@1`: what became of one Attempt's notes declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerNotesRecordedPayloadV1 {
    pub result_artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<WorkerNotesDropReasonV1>,
    /// Encoded payload size, so an over-bound drop records how far over it was.
    pub bytes: u64,
}

impl WorkerNotesRecordedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.result_artifact_id) {
            return Err("WorkerNotesRecorded@1 has an invalid result artifact ID".into());
        }
        match (&self.notes_artifact_id, self.dropped) {
            (Some(id), None) if crate::is_digest(id) => {}
            (None, Some(_)) => {}
            _ => {
                return Err(
                    "WorkerNotesRecorded@1 must name exactly one of a notes artifact or a drop reason"
                        .into(),
                );
            }
        }
        if self.bytes > crate::json::SAFE_INTEGER_MAX as u64 {
            return Err("WorkerNotesRecorded@1 bytes exceed the JSON safe-integer bound".into());
        }
        Ok(())
    }
}

/// One mark per path over the union of Notes paths, both Subject views and the head-to-head
/// path set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadDeltaMarkV1 {
    /// Present at both heads with different content.
    Changed,
    /// Present at both heads with identical content.
    Unchanged,
    /// Absent at the previous head, present now.
    New,
    /// Its current state equals the Base again: modified at the previous head and restored,
    /// or added at the previous head and gone now.
    Reverted,
    /// Present at the previous head (or only named by the Notes) and absent now, while the
    /// Base never had it or still has it.
    Removed,
    /// The current path of a rename Git detected between the two heads.
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadDeltaEntryV1 {
    pub path: String,
    pub mark: HeadDeltaMarkV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

/// The payload of a `review.kernel/HeadDelta@1` artifact.
///
/// Not a Change Set: it names two consecutive heads of one node, never a Base, carries no
/// Subject identity and no Report Scope, and exists for whole-tree Subjects too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadDeltaV1 {
    pub node: String,
    pub from_snapshot_id: String,
    pub to_snapshot_id: String,
    pub diff_policy_version: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rename_detection_truncated: bool,
    /// The complete add/delete/modify path set between the two heads, sorted and unique.
    pub changed_paths: Vec<String>,
    /// Sorted by path, unique. Covers `changed_paths` plus every extra path the caller asked
    /// to mark.
    pub marks: Vec<HeadDeltaEntryV1>,
}

impl HeadDeltaV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("HeadDelta@1 has an empty node".into());
        }
        if !crate::is_digest(&self.from_snapshot_id) || !crate::is_digest(&self.to_snapshot_id) {
            return Err("HeadDelta@1 has an invalid Snapshot ID".into());
        }
        if self.diff_policy_version.trim().is_empty() {
            return Err("HeadDelta@1 requires a diff policy identity".into());
        }
        if self.changed_paths.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .changed_paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path))
        {
            return Err("HeadDelta@1 changed paths must be sorted, unique, and relative".into());
        }
        if self
            .marks
            .windows(2)
            .any(|pair| pair[0].path >= pair[1].path)
        {
            return Err("HeadDelta@1 marks must be sorted by path and unique".into());
        }
        let marked: BTreeSet<&str> = self.marks.iter().map(|entry| entry.path.as_str()).collect();
        for entry in &self.marks {
            if !crate::is_valid_repo_path(&entry.path) {
                return Err("HeadDelta@1 mark path is not a canonical repository path".into());
            }
            match (entry.mark, &entry.renamed_from) {
                (HeadDeltaMarkV1::Renamed, Some(from))
                    if crate::is_valid_repo_path(from) && from != &entry.path => {}
                (HeadDeltaMarkV1::Renamed, _) => {
                    return Err("HeadDelta@1 renamed mark needs a distinct renamed_from".into());
                }
                (_, Some(_)) => {
                    return Err("HeadDelta@1 renamed_from is only valid on a renamed mark".into());
                }
                (_, None) => {}
            }
        }
        if self
            .changed_paths
            .iter()
            .any(|path| !marked.contains(path.as_str()))
        {
            return Err("HeadDelta@1 must mark every changed path".into());
        }
        Ok(())
    }
}

/// Content identity of one tree entry as a Head Delta compares it: the entry kind and its
/// byte digest. Two entries with equal identity are `unchanged`.
pub type TreeView = BTreeMap<String, String>;

/// Everything the mark computation needs, supplied by the caller that owns the manifests.
pub struct HeadDeltaInputs<'a> {
    /// The previous head, path to content identity.
    pub from: &'a TreeView,
    /// The current head.
    pub to: &'a TreeView,
    /// The pinned Base of a diff Subject; `None` for a whole-tree Subject, where `reverted`
    /// cannot occur.
    pub base: Option<&'a TreeView>,
    /// Renames Git detected from the previous head to the current one.
    pub renames: &'a [PathRenameV1],
    /// Paths that must receive a mark even when neither head changed them: the Notes paths and
    /// both diff Subjects' Change Set paths. A whole-tree Subject view is never enumerated: a
    /// path present in both heads and absent from the marks is `unchanged` by construction.
    pub extra_paths: &'a BTreeSet<String>,
}

/// Compute the sorted, unique mark list and the head-to-head changed path set.
pub fn compute_head_delta_marks(
    inputs: HeadDeltaInputs<'_>,
) -> (Vec<String>, Vec<HeadDeltaEntryV1>) {
    let mut changed_paths = BTreeSet::new();
    for (path, identity) in inputs.from {
        if inputs.to.get(path) != Some(identity) {
            changed_paths.insert(path.clone());
        }
    }
    for path in inputs.to.keys() {
        if !inputs.from.contains_key(path) {
            changed_paths.insert(path.clone());
        }
    }
    let mut renamed_to: BTreeMap<&str, &str> = BTreeMap::new();
    let mut renamed_from: BTreeSet<&str> = BTreeSet::new();
    for rename in inputs.renames {
        renamed_to.insert(rename.new_path.as_str(), rename.old_path.as_str());
        renamed_from.insert(rename.old_path.as_str());
        changed_paths.insert(rename.old_path.clone());
        changed_paths.insert(rename.new_path.clone());
    }
    let mut union: BTreeSet<String> = changed_paths.clone();
    union.extend(inputs.extra_paths.iter().cloned());
    let marks = union
        .into_iter()
        .map(|path| {
            let from = inputs.from.get(&path);
            let to = inputs.to.get(&path);
            let base = inputs.base.and_then(|base| base.get(&path));
            let (mark, origin) = if let Some(old_path) = renamed_to.get(path.as_str()) {
                (HeadDeltaMarkV1::Renamed, Some((*old_path).to_string()))
            } else if renamed_from.contains(path.as_str()) {
                (HeadDeltaMarkV1::Removed, None)
            } else {
                let mark = match (from, to) {
                    (Some(before), Some(after)) if before == after => HeadDeltaMarkV1::Unchanged,
                    (Some(_), Some(after)) if base == Some(after) => HeadDeltaMarkV1::Reverted,
                    (Some(_), Some(_)) => HeadDeltaMarkV1::Changed,
                    (None, Some(after)) if base == Some(after) => HeadDeltaMarkV1::Reverted,
                    (None, Some(_)) => HeadDeltaMarkV1::New,
                    (Some(_), None) if inputs.base.is_some() && base.is_none() => {
                        HeadDeltaMarkV1::Reverted
                    }
                    (Some(_), None) | (None, None) => HeadDeltaMarkV1::Removed,
                };
                (mark, None)
            };
            HeadDeltaEntryV1 {
                path,
                mark,
                renamed_from: origin,
            }
        })
        .collect();
    (changed_paths.into_iter().collect(), marks)
}

/// One carried layer, as the Warm Set and the run report name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmLayerV1 {
    Notes,
    HeadDelta,
    /// Package P2: the Gate's candidate-built output cloned into the Attempt's sandbox.
    BuildCache,
    /// Package P3: the node's stable template re-based to the head, or reused unchanged,
    /// instead of materialized from scratch.
    Workspace,
    /// Package P4: the previous admitted Attempt's harness transcript, re-materialized and
    /// resumed forked so only the delta prompt is sent. Claude adapters only.
    Session,
}

impl WarmLayerV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::HeadDelta => "head_delta",
            Self::BuildCache => "build_cache",
            Self::Workspace => "workspace",
            Self::Session => "session",
        }
    }
}

/// Why a node that declared a build cache kind starts this Round without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheDropReasonV1 {
    /// No Gate of this Round recorded a capture of the declared kind.
    NotCaptured,
    /// The Gate recorded a refusal for the declared kind; the reason is on its event.
    Refused,
}

/// Why a Round's Head Delta was computed but not carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadDeltaDropReasonV1 {
    /// The canonical Head Delta exceeded `MAX_HEAD_DELTA_BYTES`; the Attempt runs on Notes
    /// alone and the manifest lists no delta.
    OverBound,
}

/// The payload of a `review.kernel/WarmSet@1` artifact: the exact carried layers one node's
/// Attempts start from in one Round. Recorded before the first Attempt is dispatched; a retry
/// inherits it, never its failed sibling's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmSetV1 {
    pub node: String,
    pub round: u32,
    /// The previous closed Round's admitted Attempt of this node, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_delta_artifact_id: Option<String>,
    /// Recorded when a Head Delta was computed but could not be carried; never set beside a
    /// carried delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_delta_dropped: Option<HeadDeltaDropReasonV1>,
    /// Package P2: the `review.kernel/BuildCache@1` this Round's Gate captured for the kind
    /// the node declares, carried Gate to Worker within the same Round. Explicitly unsafe and
    /// admitted only under the trusted-local policy; never a Cache Snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cache_artifact_id: Option<String>,
    /// Recorded when the node declared a build cache kind and none was carried; never set
    /// beside a carried build cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cache_dropped: Option<BuildCacheDropReasonV1>,
    /// Package P3: how the node's stable workspace template came to hold this Round's head.
    /// Present exactly with `workspace_id`; absent for a node that templates fresh per Round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBasisV1>,
    /// The opaque identity of the node's stable workspace root within this Campaign. Never a
    /// host path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Package P4: the `review.kernel/SessionSnapshot@1` of the previous closed Round's
    /// admitted Attempt of this node, re-materialized and resumed forked. Selected only when
    /// the adapter supports resume, the cleanup of its capture completed, its age is under
    /// `warm.session.max_age` and its estimated tokens fit the reservation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_artifact_id: Option<String>,
    /// Recorded when the node's policy asked for the session layer and none was carried; never
    /// set beside a carried session. The Attempt runs on Notes alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_dropped: Option<crate::session::SessionDropReasonV1>,
}

impl WarmSetV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() || self.round == 0 {
            return Err("WarmSet@1 needs a node and a positive Round".into());
        }
        if !self
            .source_attempt_id
            .as_deref()
            .is_none_or(is_monotonic_id)
        {
            return Err("WarmSet@1 has an invalid source Attempt ID".into());
        }
        let valid_layer = |id: &Option<String>| id.as_deref().is_none_or(crate::is_digest);
        if !valid_layer(&self.notes_artifact_id)
            || !valid_layer(&self.head_delta_artifact_id)
            || !valid_layer(&self.build_cache_artifact_id)
            || !valid_layer(&self.session_artifact_id)
        {
            return Err("WarmSet@1 has an invalid layer artifact ID".into());
        }
        if self.notes_artifact_id.is_some() && self.source_attempt_id.is_none() {
            return Err("WarmSet@1 carries Notes without a source Attempt".into());
        }
        if self.session_artifact_id.is_some() && self.source_attempt_id.is_none() {
            return Err("WarmSet@1 carries a Session Snapshot without a source Attempt".into());
        }
        if self.session_artifact_id.is_some() && self.session_dropped.is_some() {
            return Err("WarmSet@1 both carries and drops its Session Snapshot".into());
        }
        if self.head_delta_artifact_id.is_some() && self.head_delta_dropped.is_some() {
            return Err("WarmSet@1 both carries and drops its Head Delta".into());
        }
        if self.build_cache_artifact_id.is_some() && self.build_cache_dropped.is_some() {
            return Err("WarmSet@1 both carries and drops its Build Cache".into());
        }
        match (&self.workspace, &self.workspace_id) {
            (None, None) => {}
            (Some(_), Some(id)) if is_workspace_id(id) => {}
            _ => {
                return Err(
                    "WarmSet@1 names a workspace basis and identity together or not at all".into(),
                );
            }
        }
        Ok(())
    }

    pub fn layers(&self) -> Vec<WarmLayerV1> {
        let mut layers = Vec::new();
        if self.notes_artifact_id.is_some() {
            layers.push(WarmLayerV1::Notes);
        }
        if self.head_delta_artifact_id.is_some() {
            layers.push(WarmLayerV1::HeadDelta);
        }
        if self.build_cache_artifact_id.is_some() {
            layers.push(WarmLayerV1::BuildCache);
        }
        if self.workspace.is_some_and(WorkspaceBasisV1::carried) {
            layers.push(WarmLayerV1::Workspace);
        }
        if self.session_artifact_id.is_some() {
            layers.push(WarmLayerV1::Session);
        }
        layers
    }
}

/// Payload of `WarmSetSelected@1`, appended before the node's first Attempt of the Round is
/// reserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmSetSelectedPayloadV1 {
    pub warm_set_artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_attempt_id: Option<String>,
    pub layers: Vec<WarmLayerV1>,
}

impl WarmSetSelectedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.warm_set_artifact_id) {
            return Err("WarmSetSelected@1 has an invalid Warm Set artifact ID".into());
        }
        if !self
            .source_attempt_id
            .as_deref()
            .is_none_or(is_monotonic_id)
        {
            return Err("WarmSetSelected@1 has an invalid source Attempt ID".into());
        }
        if self.layers.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("WarmSetSelected@1 layers must be sorted and unique".into());
        }
        Ok(())
    }
}

pub(crate) fn is_monotonic_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(entries: &[(&str, &str)]) -> TreeView {
        entries
            .iter()
            .map(|(path, identity)| ((*path).to_string(), (*identity).to_string()))
            .collect()
    }

    fn mark_of(marks: &[HeadDeltaEntryV1], path: &str) -> HeadDeltaMarkV1 {
        marks
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("{path} received no mark"))
            .mark
    }

    #[test]
    fn marks_cover_reverted_removed_new_changed_and_unchanged_paths() {
        let base = view(&[("a.rs", "a0"), ("b.rs", "b0"), ("gone.rs", "g0")]);
        let from = view(&[
            ("a.rs", "a1"),
            ("b.rs", "b1"),
            ("c.rs", "c1"),
            ("gone.rs", "g0"),
        ]);
        let to = view(&[("a.rs", "a1"), ("b.rs", "b0"), ("d.rs", "d1")]);
        let extra = BTreeSet::from(["notes-only.rs".to_string(), "a.rs".to_string()]);
        let (changed, marks) = compute_head_delta_marks(HeadDeltaInputs {
            from: &from,
            to: &to,
            base: Some(&base),
            renames: &[],
            extra_paths: &extra,
        });
        assert_eq!(changed, vec!["b.rs", "c.rs", "d.rs", "gone.rs"]);
        assert_eq!(mark_of(&marks, "a.rs"), HeadDeltaMarkV1::Unchanged);
        assert_eq!(mark_of(&marks, "b.rs"), HeadDeltaMarkV1::Reverted);
        assert_eq!(
            mark_of(&marks, "c.rs"),
            HeadDeltaMarkV1::Reverted,
            "added at the previous head and gone now equals the Base again"
        );
        assert_eq!(mark_of(&marks, "d.rs"), HeadDeltaMarkV1::New);
        assert_eq!(mark_of(&marks, "gone.rs"), HeadDeltaMarkV1::Removed);
        assert_eq!(mark_of(&marks, "notes-only.rs"), HeadDeltaMarkV1::Removed);
        assert!(marks.windows(2).all(|pair| pair[0].path < pair[1].path));
        let delta = HeadDeltaV1 {
            node: "correctness".into(),
            from_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            to_snapshot_id: format!("sha256:{}", "2".repeat(64)),
            diff_policy_version: "test".into(),
            rename_detection_truncated: false,
            changed_paths: changed,
            marks,
        };
        delta.validate().unwrap();
    }

    #[test]
    fn restoring_a_base_path_after_a_deletion_is_reverted_not_new() {
        let base = view(&[("a.rs", "a0")]);
        let from = view(&[]);
        let to = view(&[("a.rs", "a0"), ("fresh.rs", "f1")]);
        let (changed, marks) = compute_head_delta_marks(HeadDeltaInputs {
            from: &from,
            to: &to,
            base: Some(&base),
            renames: &[],
            extra_paths: &BTreeSet::new(),
        });
        assert_eq!(changed, vec!["a.rs", "fresh.rs"]);
        assert_eq!(mark_of(&marks, "a.rs"), HeadDeltaMarkV1::Reverted);
        assert_eq!(mark_of(&marks, "fresh.rs"), HeadDeltaMarkV1::New);
    }

    #[test]
    fn a_warm_set_cannot_both_carry_and_drop_its_head_delta() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let set = WarmSetV1 {
            node: "correctness".into(),
            round: 2,
            source_attempt_id: Some("a".repeat(26)),
            notes_artifact_id: None,
            head_delta_artifact_id: Some(digest),
            head_delta_dropped: Some(HeadDeltaDropReasonV1::OverBound),
            build_cache_artifact_id: None,
            build_cache_dropped: None,
            workspace: None,
            workspace_id: None,
            session_artifact_id: None,
            session_dropped: None,
        };
        assert!(set.validate().is_err());
        let dropped = WarmSetV1 {
            head_delta_artifact_id: None,
            ..set
        };
        dropped.validate().unwrap();
        assert!(dropped.layers().is_empty());
    }

    #[test]
    fn a_warm_set_names_its_workspace_basis_and_identity_together() {
        let set = WarmSetV1 {
            node: "correctness".into(),
            round: 2,
            source_attempt_id: None,
            notes_artifact_id: None,
            head_delta_artifact_id: None,
            head_delta_dropped: None,
            build_cache_artifact_id: None,
            build_cache_dropped: None,
            workspace: Some(WorkspaceBasisV1::Rebased),
            workspace_id: Some("b".repeat(32)),
            session_artifact_id: None,
            session_dropped: None,
        };
        set.validate().unwrap();
        assert_eq!(set.layers(), vec![WarmLayerV1::Workspace]);
        let full = WarmSetV1 {
            workspace: Some(WorkspaceBasisV1::Full),
            ..set.clone()
        };
        full.validate().unwrap();
        assert!(
            full.layers().is_empty(),
            "a fully materialized template carries nothing"
        );
        let reused = WarmSetV1 {
            workspace: Some(WorkspaceBasisV1::Reused),
            ..set.clone()
        };
        assert_eq!(reused.layers(), vec![WarmLayerV1::Workspace]);
        let nameless = WarmSetV1 {
            workspace_id: None,
            ..set.clone()
        };
        assert!(nameless.validate().is_err());
        let baseless = WarmSetV1 {
            workspace: None,
            ..set.clone()
        };
        assert!(baseless.validate().is_err());
        let host_path = WarmSetV1 {
            workspace_id: Some("/Users/operator/.cache/af/workspaces/x".into()),
            ..set
        };
        assert!(host_path.validate().is_err());
    }

    #[test]
    fn a_warm_set_cannot_both_carry_and_drop_its_build_cache() {
        let digest = format!("sha256:{}", "b".repeat(64));
        let set = WarmSetV1 {
            node: "tdd".into(),
            round: 1,
            source_attempt_id: None,
            notes_artifact_id: None,
            head_delta_artifact_id: None,
            head_delta_dropped: None,
            build_cache_artifact_id: Some(digest),
            build_cache_dropped: Some(BuildCacheDropReasonV1::Refused),
            workspace: None,
            workspace_id: None,
            session_artifact_id: None,
            session_dropped: None,
        };
        assert!(set.validate().is_err());
        let carried = WarmSetV1 {
            build_cache_dropped: None,
            ..set.clone()
        };
        carried.validate().unwrap();
        assert_eq!(
            carried.layers(),
            vec![WarmLayerV1::BuildCache],
            "a Round-one Warm Set may carry a build cache and nothing else"
        );
        let dropped = WarmSetV1 {
            build_cache_artifact_id: None,
            ..set
        };
        dropped.validate().unwrap();
        assert!(dropped.layers().is_empty());
    }

    #[test]
    fn task_worker_notes_are_bound_to_kernel_authority() {
        let head = format!("sha256:{}", "1".repeat(64));
        let supplied = serde_json::json!({
            "node": "someone-else",
            "attempt_id": "z".repeat(26),
            "head_snapshot_id": format!("sha256:{}", "9".repeat(64)),
            "inspected": [{"path": "src/lib.rs"}],
            "model_of_change": "one cap",
            "open_questions": [],
            "hints": [],
        });
        let bound = WorkerNotesV1::bind(supplied, "implement", &"a".repeat(26), &head).unwrap();
        assert_eq!(bound.node, "implement");
        assert_eq!(bound.attempt_id, "a".repeat(26));
        assert_eq!(bound.head_snapshot_id, head);
        bound
            .check_bound("implement", &"a".repeat(26), &head)
            .unwrap();
        assert!(
            bound
                .check_bound("evaluate", &"a".repeat(26), &head)
                .is_err()
        );
        let smuggled = serde_json::json!({
            "inspected": [], "model_of_change": "x", "open_questions": [], "hints": [],
            "verdict": "approve",
        });
        assert!(WorkerNotesV1::bind(smuggled, "implement", &"a".repeat(26), &head).is_err());
        let bad_path = serde_json::json!({
            "inspected": [{"path": "../etc/passwd"}], "model_of_change": "x",
            "open_questions": [], "hints": [],
        });
        assert!(WorkerNotesV1::bind(bad_path, "implement", &"a".repeat(26), &head).is_err());
    }

    #[test]
    fn a_whole_tree_subject_never_reports_reverted() {
        let from = view(&[("a.rs", "a1"), ("c.rs", "c1")]);
        let to = view(&[("a.rs", "a2")]);
        let (changed, marks) = compute_head_delta_marks(HeadDeltaInputs {
            from: &from,
            to: &to,
            base: None,
            renames: &[],
            extra_paths: &BTreeSet::new(),
        });
        assert_eq!(changed, vec!["a.rs", "c.rs"]);
        assert_eq!(mark_of(&marks, "a.rs"), HeadDeltaMarkV1::Changed);
        assert_eq!(mark_of(&marks, "c.rs"), HeadDeltaMarkV1::Removed);
    }

    #[test]
    fn renames_mark_the_new_path_and_remove_the_old_one() {
        let from = view(&[("old.rs", "x")]);
        let to = view(&[("new.rs", "x")]);
        let renames = [PathRenameV1 {
            old_path: "old.rs".into(),
            new_path: "new.rs".into(),
            similarity: 100,
        }];
        let (_, marks) = compute_head_delta_marks(HeadDeltaInputs {
            from: &from,
            to: &to,
            base: None,
            renames: &renames,
            extra_paths: &BTreeSet::new(),
        });
        assert_eq!(mark_of(&marks, "new.rs"), HeadDeltaMarkV1::Renamed);
        assert_eq!(
            marks
                .iter()
                .find(|entry| entry.path == "new.rs")
                .unwrap()
                .renamed_from
                .as_deref(),
            Some("old.rs")
        );
        assert_eq!(mark_of(&marks, "old.rs"), HeadDeltaMarkV1::Removed);
    }

    #[test]
    fn notes_recorded_payload_names_exactly_one_disposition() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let recorded = WorkerNotesRecordedPayloadV1 {
            result_artifact_id: digest.clone(),
            notes_artifact_id: Some(digest.clone()),
            dropped: None,
            bytes: 12,
        };
        recorded.validate().unwrap();
        let both = WorkerNotesRecordedPayloadV1 {
            dropped: Some(WorkerNotesDropReasonV1::OverBound),
            ..recorded.clone()
        };
        assert!(both.validate().is_err());
        let neither = WorkerNotesRecordedPayloadV1 {
            notes_artifact_id: None,
            ..recorded
        };
        assert!(neither.validate().is_err());
    }

    #[test]
    fn a_warm_set_with_notes_needs_its_source_attempt() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let set = WarmSetV1 {
            node: "correctness".into(),
            round: 2,
            source_attempt_id: None,
            notes_artifact_id: Some(digest),
            head_delta_artifact_id: None,
            head_delta_dropped: None,
            build_cache_artifact_id: None,
            build_cache_dropped: None,
            workspace: None,
            workspace_id: None,
            session_artifact_id: None,
            session_dropped: None,
        };
        assert!(set.validate().is_err());
        let set = WarmSetV1 {
            source_attempt_id: Some("a".repeat(26)),
            ..set
        };
        set.validate().unwrap();
        assert_eq!(set.layers(), vec![WarmLayerV1::Notes]);
    }

    #[test]
    fn a_warm_set_carries_its_session_only_beside_the_attempt_that_left_it() {
        use crate::session::SessionDropReasonV1;
        let digest = format!("sha256:{}", "e".repeat(64));
        let set = WarmSetV1 {
            node: "correctness".into(),
            round: 3,
            source_attempt_id: Some("a".repeat(26)),
            notes_artifact_id: None,
            head_delta_artifact_id: None,
            head_delta_dropped: None,
            build_cache_artifact_id: None,
            build_cache_dropped: None,
            workspace: None,
            workspace_id: None,
            session_artifact_id: Some(digest),
            session_dropped: None,
        };
        set.validate().unwrap();
        assert_eq!(set.layers(), vec![WarmLayerV1::Session]);
        let orphan = WarmSetV1 {
            source_attempt_id: None,
            ..set.clone()
        };
        assert!(
            orphan.validate().is_err(),
            "a transcript with no source Attempt is not a warm layer"
        );
        let both = WarmSetV1 {
            session_dropped: Some(SessionDropReasonV1::TooOld),
            ..set.clone()
        };
        assert!(both.validate().is_err());
        let dropped = WarmSetV1 {
            session_artifact_id: None,
            session_dropped: Some(SessionDropReasonV1::ProviderUnsupported),
            ..set
        };
        dropped.validate().unwrap();
        assert!(
            dropped.layers().is_empty(),
            "a dropped session layer falls back to Notes alone"
        );
    }
}
