//! Worker warm layers, package P1: Warm Set selection, Head Delta computation and Notes
//! capture.
//!
//! Warmth is an artifact, never ambient state. A node's Warm Set is selected from the durable
//! Campaign log, stored in the CAS, recorded as `WarmSetSelected@1` before the node's first
//! Attempt of the Round is dispatched, and delivered as declared inputs that the Attempt's
//! context manifest lists with bytes and estimated tokens. Only the previous closed Round's
//! admitted Attempt of the same node can be a source; fenced, quarantined, malformed and
//! released Attempts contribute nothing, and a retry inherits the Round's Warm Set rather than
//! its failed sibling's state.

use std::collections::BTreeSet;

use review_core::event::AttemptAdmittedPayloadV1;
use review_core::{
    ArtifactEnvelope, BuildCacheKindV1, EventType, HeadDeltaInputs, HeadDeltaV1, InspectedPathV1,
    PathHintV1, PathRenameV1, Producer, RoundStartedPayloadV1, RunEvent, SourceSnapshot, TreeView,
    WarmSetSelectedPayloadV1, WarmSetV1, WorkerNotesDropReasonV1, WorkerNotesRecordedPayloadV1,
    WorkerNotesV1, compute_head_delta_marks, run_report_closes_round,
};
use review_runner::{NotesRequest, ReviewerInputs, ReviewerNotesDeclaration};
use review_source_git::git::TREE_DIFF_POLICY_VERSION;
use review_source_git::{Manifest, TreeChangeKind, manifest_diff};
use review_store::{Cas, NewEvent};

use super::RoundAuthority;
use super::review_domain::ReviewDomainState;

/// The Warm Set one node's Attempts start from in the active Round, as durably recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WarmSetRecord {
    pub set: WarmSetV1,
    /// The `review.kernel/WarmSet@1` artifact every Attempt's manifest names.
    pub artifact_id: String,
}

impl ReviewDomainState<'_> {
    /// The pinned warm policy of a node, resolved through its binding node for dynamic shards.
    pub(crate) fn warm_policy(&self, node_id: &str) -> Option<review_config::WarmSpec> {
        let base = self.reviewer_binding_node(node_id);
        self.warm_policies
            .get(node_id)
            .or_else(|| self.warm_policies.get(&base))
            .copied()
    }

    /// The Notes bound a node's warm policy asks for, or `None` for a cold node.
    pub(crate) fn notes_max_bytes(&self, node_id: &str) -> Option<u64> {
        self.warm_policy(node_id)
            .filter(|policy| policy.notes)
            .map(|policy| policy.notes_max_bytes)
    }

    /// Select and durably record the node's Warm Set for this Round, once. A resumed Round
    /// finds its recorded selection and reuses it; nothing is recomputed or re-appended. Cold
    /// nodes have no Warm Set, and Notes are carried only from Round two on; a node that
    /// declares a build cache kind selects one in every Round, because that layer travels from
    /// this Round's Gate rather than from the previous Round.
    pub(crate) fn select_warm_set(&self, node_id: &str) -> Result<Option<WarmSetRecord>, String> {
        let notes = self.notes_max_bytes(node_id).is_some() && self.authority.round > 1;
        let build_cache = match self.node_build_cache_kind(node_id) {
            Some(kind) => {
                // Refused before the node's first Attempt of the Round is reserved or
                // dispatched; the cache is read only from the Gate this node waits on.
                self.build_cache_policy_admits()?;
                Some((kind, self.build_cache_gate(node_id)?))
            }
            None => None,
        };
        if !notes && build_cache.is_none() {
            return Ok(None);
        }
        if let Some(record) = self.warm_sets.lock().expect("warm sets").get(node_id) {
            return Ok(Some(record.clone()));
        }
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let recorded =
            recorded_warm_set(self.cas, &events, &self.authority.round_event_id, node_id)?;
        let record = match recorded {
            Some(record) => record,
            None => {
                let set = select(
                    self.cas,
                    &events,
                    &self.authority,
                    &self.snapshot,
                    node_id,
                    notes,
                    build_cache,
                )?;
                self.record_warm_set(set)?
            }
        };
        let mut warm_sets = self.warm_sets.lock().expect("warm sets");
        warm_sets.insert(node_id.to_string(), record.clone());
        Ok(Some(record))
    }

    fn record_warm_set(&self, set: WarmSetV1) -> Result<WarmSetRecord, String> {
        set.validate()?;
        let layer_ids: Vec<String> = set
            .notes_artifact_id
            .iter()
            .chain(set.head_delta_artifact_id.iter())
            .chain(set.build_cache_artifact_id.iter())
            .cloned()
            .collect();
        let producer = Producer::KernelOperation {
            run_id: self.run_id.clone(),
            node_id: Some(set.node.clone()),
            operation_id: format!(
                "warm-set:{}:{}:{}",
                set.node, self.authority.round, self.authority.epoch
            ),
        };
        let cas = self.cas;
        let (artifact_id, _) = cas
            .put_artifact(
                review_core::contract::WARM_SET_V1,
                producer,
                layer_ids.clone(),
                Some(self.authority.head_snapshot_id.clone()),
                encode(&set)?,
            )
            .map_err(|error| error.to_string())?;
        let selected = WarmSetSelectedPayloadV1 {
            warm_set_artifact_id: artifact_id.clone(),
            source_attempt_id: set.source_attempt_id.clone(),
            layers: set.layers(),
        };
        selected.validate()?;
        let mut refs = vec![artifact_id.clone()];
        refs.extend(layer_ids);
        self.append(
            NewEvent::new(EventType::WarmSetSelectedV1, encode(&selected)?)
                .node(&set.node)
                .referencing(refs),
        )?;
        Ok(WarmSetRecord { set, artifact_id })
    }
}

/// Ask a warm node's Attempt to leave Notes. Cold nodes get no request and no prompt change.
pub(crate) fn request_notes(inputs: &mut ReviewerInputs, notes_max_bytes: Option<u64>) {
    inputs.notes_request = notes_max_bytes.map(|max_bytes| NotesRequest { max_bytes });
}

/// Deliver the recorded Warm Set as declared inputs. Every layer is read back from the CAS by
/// its recorded identity, so replay renders the same bytes.
pub(crate) fn apply_warm_set(
    cas: &Cas,
    inputs: &mut ReviewerInputs,
    record: &WarmSetRecord,
) -> Result<(), String> {
    if let Some(id) = &record.set.notes_artifact_id {
        let notes = read_notes(cas, id)?;
        inputs.notes = Some(encode(&notes)?);
        inputs.notes_artifact_id = Some(id.clone());
    }
    if let Some(id) = &record.set.head_delta_artifact_id {
        let delta = read_head_delta(cas, id)?;
        inputs.head_delta = Some(encode(&delta)?);
        inputs.head_delta_artifact_id = Some(id.clone());
    }
    // The build cache is never rendered; it reaches the sandbox as bytes when the Attempt's
    // sandbox is prepared. The manifest still names it through this identity.
    inputs.build_cache_artifact_id = record.set.build_cache_artifact_id.clone();
    inputs.warm_set_artifact_id = Some(record.artifact_id.clone());
    Ok(())
}

fn read_json(cas: &Cas, id: &str) -> Result<serde_json::Value, String> {
    cas.get_json(id).map_err(|error| error.to_string())
}

fn read_envelope(cas: &Cas, id: &str) -> Result<ArtifactEnvelope, String> {
    cas.get_artifact(id).map_err(|error| error.to_string())
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn recorded_warm_set(
    cas: &Cas,
    events: &[RunEvent],
    round_event_id: &str,
    node_id: &str,
) -> Result<Option<WarmSetRecord>, String> {
    let Some(event) = events.iter().find(|event| {
        event.event_type == EventType::WarmSetSelectedV1
            && event.causation_id.as_deref() == Some(round_event_id)
            && event.node_id.as_deref() == Some(node_id)
    }) else {
        return Ok(None);
    };
    let payload: WarmSetSelectedPayloadV1 = decode(event.payload.clone())?;
    let envelope = read_envelope(cas, &payload.warm_set_artifact_id)?;
    if envelope.artifact_type != review_core::contract::WARM_SET_V1 {
        return Err("recorded Warm Set artifact has another type".into());
    }
    let set: WarmSetV1 = decode(envelope.payload)?;
    set.validate()?;
    if set.node != node_id || set.layers() != payload.layers {
        return Err("recorded Warm Set contradicts its selection event".into());
    }
    Ok(Some(WarmSetRecord {
        set,
        artifact_id: payload.warm_set_artifact_id,
    }))
}

struct PreviousRound {
    event_id: String,
    payload: RoundStartedPayloadV1,
}

/// The latest closed Round before the active one, in this Campaign.
fn previous_closed_round(
    events: &[RunEvent],
    authority: &RoundAuthority,
) -> Result<Option<PreviousRound>, String> {
    let active_sequence = events
        .iter()
        .find(|event| event.event_id == authority.round_event_id)
        .map(|event| event.sequence)
        .ok_or("active Round is absent from its Campaign log")?;
    for event in events.iter().rev() {
        if event.event_type != EventType::RoundStartedV1 || event.sequence >= active_sequence {
            continue;
        }
        let payload: RoundStartedPayloadV1 = decode(event.payload.clone())?;
        if payload.campaign_manifest_id != authority.campaign_manifest_id
            || payload.round >= authority.round
        {
            continue;
        }
        let closed = events.iter().try_fold(false, |closed, candidate| {
            if candidate.causation_id.as_deref() != Some(event.event_id.as_str())
                || !candidate.event_type.is_run_report()
            {
                return Ok(closed);
            }
            run_report_closes_round(candidate)
                .map(|value| closed || value.unwrap_or(false))
                .map_err(|error| error.to_string())
        })?;
        if closed {
            return Ok(Some(PreviousRound {
                event_id: event.event_id.clone(),
                payload,
            }));
        }
    }
    Ok(None)
}

/// The one admitted Attempt of `node_id` in a Round, whichever execution frontend selected
/// it. Quarantined, fenced, failed and released Attempts are not admitted.
fn admitted_attempt(
    events: &[RunEvent],
    round_event_id: &str,
    node_id: &str,
) -> Result<Option<String>, String> {
    let mut selected: Option<String> = None;
    for event in events.iter().filter(|event| {
        event.causation_id.as_deref() == Some(round_event_id)
            && event.node_id.as_deref() == Some(node_id)
    }) {
        let admitted = match event.event_type {
            EventType::AttemptAdmittedV1 => {
                let payload: AttemptAdmittedPayloadV1 = decode(event.payload.clone())?;
                payload.selection == "selected"
            }
            EventType::TaskReviewResultSelectedV1 => true,
            _ => false,
        };
        if !admitted {
            continue;
        }
        let attempt = event
            .attempt_id
            .clone()
            .ok_or("admitted Attempt has no Attempt ID")?;
        match &selected {
            Some(previous) if previous != &attempt => {
                return Err(format!(
                    "node `{node_id}` has more than one admitted Attempt in its previous Round"
                ));
            }
            Some(_) => {}
            None => selected = Some(attempt),
        }
    }
    Ok(selected)
}

fn recorded_notes(
    events: &[RunEvent],
    round_event_id: &str,
    attempt_id: &str,
) -> Result<Option<String>, String> {
    let Some(event) = events.iter().find(|event| {
        event.event_type == EventType::WorkerNotesRecordedV1
            && event.causation_id.as_deref() == Some(round_event_id)
            && event.attempt_id.as_deref() == Some(attempt_id)
    }) else {
        return Ok(None);
    };
    let payload: WorkerNotesRecordedPayloadV1 = decode(event.payload.clone())?;
    payload.validate()?;
    Ok(payload.notes_artifact_id)
}

fn select(
    cas: &Cas,
    events: &[RunEvent],
    authority: &RoundAuthority,
    head: &Manifest,
    node_id: &str,
    notes: bool,
    build_cache: Option<(BuildCacheKindV1, String)>,
) -> Result<WarmSetV1, String> {
    let mut set = WarmSetV1 {
        node: node_id.to_string(),
        round: authority.round,
        source_attempt_id: None,
        notes_artifact_id: None,
        head_delta_artifact_id: None,
        head_delta_dropped: None,
        build_cache_artifact_id: None,
        build_cache_dropped: None,
    };
    if let Some((kind, gate)) = build_cache {
        let (artifact_id, dropped) =
            crate::build_cache::select_build_cache(cas, events, authority, &gate, kind)?;
        set.build_cache_artifact_id = artifact_id;
        set.build_cache_dropped = dropped;
    }
    if !notes {
        return Ok(set);
    }
    let Some(previous) = previous_closed_round(events, authority)? else {
        return Ok(set);
    };
    let source = admitted_attempt(events, &previous.event_id, node_id)?;
    if let Some(attempt_id) = &source {
        set.notes_artifact_id = recorded_notes(events, &previous.event_id, attempt_id)?;
    }
    set.source_attempt_id = source;
    let mut extra_paths = match &set.notes_artifact_id {
        Some(id) => read_notes(cas, id)?.referenced_paths(),
        None => BTreeSet::new(),
    };
    let previous_subject = review_store::resolve_subject(cas, &previous.payload.subject_id)
        .map_err(|error| error.to_string())?;
    if let Some(change_set) = &previous_subject.change_set {
        extra_paths.extend(change_set.change_set().changed_paths.iter().cloned());
    }
    if let Some(change_set) = authority.change_set() {
        extra_paths.extend(change_set.change_set().changed_paths.iter().cloned());
    }
    let from_snapshot_id = previous_subject.subject.head_snapshot_id.as_str();
    let delta = compute_head_delta(cas, authority, head, node_id, from_snapshot_id, extra_paths)?;
    let delta_bytes = review_store::canonical::canonicalize(&encode(&delta)?)
        .map_err(|error| error.to_string())?
        .len();
    if delta_bytes > review_core::MAX_HEAD_DELTA_BYTES {
        // Selection is the only place a layer may be refused: an Attempt runs on Notes alone
        // and the Warm Set records why, instead of a renderer stranding the Round later.
        set.head_delta_dropped = Some(review_core::HeadDeltaDropReasonV1::OverBound);
        return Ok(set);
    }
    let producer = Producer::KernelOperation {
        run_id: authority.run_id.clone(),
        node_id: Some(node_id.to_string()),
        operation_id: format!(
            "head-delta:{node_id}:{}:{}",
            delta.from_snapshot_id, delta.to_snapshot_id
        ),
    };
    let heads = vec![delta.from_snapshot_id.clone(), delta.to_snapshot_id.clone()];
    let (artifact_id, _) = cas
        .put_artifact(
            review_core::contract::HEAD_DELTA_V1,
            producer,
            heads,
            Some(authority.head_snapshot_id.clone()),
            encode(&delta)?,
        )
        .map_err(|error| error.to_string())?;
    set.head_delta_artifact_id = Some(artifact_id);
    Ok(set)
}

/// Marks over the union of the Notes paths, both diff Subjects' Change Set paths and the
/// head-to-head path set. The path set and rename linkage come from the same typed Git diff a
/// Change Set uses; the marks compare tree entries directly, so a whole-tree Subject is marked
/// too. A whole-tree Subject view is never enumerated: every path present in both heads and
/// absent from the marks is `unchanged` by construction, which keeps the delta bounded by what
/// moved instead of by the size of the tree.
fn compute_head_delta(
    cas: &Cas,
    authority: &RoundAuthority,
    head: &Manifest,
    node_id: &str,
    from_snapshot_id: &str,
    extra_paths: BTreeSet<String>,
) -> Result<HeadDeltaV1, String> {
    let from_manifest = if from_snapshot_id == authority.head_snapshot_id {
        head.clone()
    } else {
        snapshot_manifest(cas, from_snapshot_id)?
    };
    let base_manifest = match authority.change_set() {
        Some(resolved) => Some(snapshot_manifest(
            cas,
            &resolved.change_set().base_snapshot_id,
        )?),
        None => None,
    };
    let (renames, diff_policy_version, rename_detection_truncated) =
        head_to_head(cas, &from_manifest, head)?;
    let from = tree_view(&from_manifest);
    let to = tree_view(head);
    let base = base_manifest.as_ref().map(tree_view);
    let (changed_paths, marks) = compute_head_delta_marks(HeadDeltaInputs {
        from: &from,
        to: &to,
        base: base.as_ref(),
        renames: &renames,
        extra_paths: &extra_paths,
    });
    let delta = HeadDeltaV1 {
        node: node_id.to_string(),
        from_snapshot_id: from_snapshot_id.to_string(),
        to_snapshot_id: authority.head_snapshot_id.clone(),
        diff_policy_version,
        rename_detection_truncated,
        changed_paths,
        marks,
    };
    delta.validate()?;
    Ok(delta)
}

/// Rename linkage, diff policy identity and truncation between two heads. An unchanged head
/// needs no Git invocation and still records the same policy identity.
fn head_to_head(
    cas: &Cas,
    from: &Manifest,
    head: &Manifest,
) -> Result<(Vec<PathRenameV1>, String, bool), String> {
    if from.content_digest() == head.content_digest() {
        return Ok((Vec::new(), TREE_DIFF_POLICY_VERSION.to_string(), false));
    }
    let diff = manifest_diff(from, head, cas);
    let diff = diff.map_err(|error| error.to_string())?;
    let mut renames = Vec::new();
    for change in &diff.changes {
        let TreeChangeKind::Renamed { similarity } = &change.kind else {
            continue;
        };
        let (Some(old_path), Some(new_path)) = (&change.old_path, &change.new_path) else {
            continue;
        };
        renames.push(PathRenameV1 {
            old_path: review_core::encode_path(old_path),
            new_path: review_core::encode_path(new_path),
            similarity: *similarity,
        });
    }
    Ok((renames, diff.diff_policy, diff.rename_detection_truncated))
}

fn tree_view(manifest: &Manifest) -> TreeView {
    manifest
        .entries
        .iter()
        .map(|entry| {
            let identity = format!("{}:{}", entry.kind.mode(), entry.content);
            (entry.path.clone(), identity)
        })
        .collect()
}

fn snapshot_manifest(cas: &Cas, snapshot_id: &str) -> Result<Manifest, String> {
    let source: SourceSnapshot = decode(read_json(cas, snapshot_id)?)?;
    let manifest_id = source
        .artifact_manifest
        .as_deref()
        .ok_or("previous head SourceSnapshot has no artifact manifest")?;
    let manifest: Manifest = decode(read_json(cas, manifest_id)?)?;
    if manifest.content_digest() != source.content_digest {
        return Err("previous head manifest contradicts its SourceSnapshot".into());
    }
    Ok(manifest)
}

fn read_notes(cas: &Cas, artifact_id: &str) -> Result<WorkerNotesV1, String> {
    let envelope = read_envelope(cas, artifact_id)?;
    if envelope.artifact_type != review_core::contract::WORKER_NOTES_V1 {
        return Err(format!("artifact {artifact_id} is not WorkerNotes@1"));
    }
    let notes: WorkerNotesV1 = decode(envelope.payload)?;
    notes.validate()?;
    Ok(notes)
}

fn read_head_delta(cas: &Cas, artifact_id: &str) -> Result<HeadDeltaV1, String> {
    let envelope = read_envelope(cas, artifact_id)?;
    if envelope.artifact_type != review_core::contract::HEAD_DELTA_V1 {
        return Err(format!("artifact {artifact_id} is not HeadDelta@1"));
    }
    let delta: HeadDeltaV1 = decode(envelope.payload)?;
    delta.validate()?;
    Ok(delta)
}

/// What one admitted answer declared as Notes, with the bound and head tree the kernel binds
/// it to.
pub(crate) struct NotesCapture<'a> {
    pub declaration: Result<Option<ReviewerNotesDeclaration>, String>,
    pub max_bytes: u64,
    pub head_manifest: &'a Manifest,
}

/// The durable outcome of one Notes declaration: a `WorkerNotes@1` artifact or a recorded
/// drop reason, either way carried by one `WorkerNotesRecorded@1` event.
pub(crate) struct PreparedNotes {
    pub event: NewEvent,
}

/// Bind, bound and store one Attempt's Notes. A drop never fails the Attempt: the event says
/// why the layer is missing and the Reviewer Result is admitted as it would be without notes.
pub(crate) fn prepare_notes(
    cas: &Cas,
    authority: &RoundAuthority,
    node_id: &str,
    attempt_id: &str,
    result_artifact: &str,
    capture: NotesCapture<'_>,
) -> Result<PreparedNotes, String> {
    let dropped = |reason: WorkerNotesDropReasonV1, bytes: u64| {
        notes_record(
            node_id,
            attempt_id,
            result_artifact,
            None,
            Some(reason),
            bytes,
        )
    };
    let declaration = match capture.declaration {
        Ok(Some(declaration)) => declaration,
        Ok(None) => return dropped(WorkerNotesDropReasonV1::Absent, 0),
        Err(_) => return dropped(WorkerNotesDropReasonV1::Malformed, 0),
    };
    let paths_valid = declaration
        .inspected
        .iter()
        .chain(declaration.hints.iter().map(|hint| &hint.path))
        .all(|path| review_core::is_valid_repo_path(path));
    if !paths_valid {
        return dropped(WorkerNotesDropReasonV1::InvalidPath, 0);
    }
    let mut inspected: Vec<InspectedPathV1> = declaration
        .inspected
        .iter()
        .map(|path| InspectedPathV1 {
            path: path.clone(),
            tree_entry_digest: tree_entry_digest(capture.head_manifest, path),
        })
        .collect();
    inspected.sort_by(|left, right| left.path.cmp(&right.path));
    inspected.dedup_by(|left, right| left.path == right.path);
    let notes = WorkerNotesV1 {
        node: node_id.to_string(),
        attempt_id: attempt_id.to_string(),
        head_snapshot_id: authority.head_snapshot_id.clone(),
        inspected,
        model_of_change: declaration.model_of_change,
        open_questions: declaration.open_questions,
        hints: declaration
            .hints
            .into_iter()
            .map(|hint| PathHintV1 {
                path: hint.path,
                note: hint.note,
            })
            .collect(),
    };
    if notes.validate().is_err() {
        return dropped(WorkerNotesDropReasonV1::Malformed, 0);
    }
    let payload = encode(&notes)?;
    let encoded =
        review_store::canonical::canonicalize(&payload).map_err(|error| error.to_string())?;
    let bytes = encoded.len() as u64;
    if bytes > capture.max_bytes {
        return dropped(WorkerNotesDropReasonV1::OverBound, bytes);
    }
    let producer = Producer::Attempt {
        run_id: authority.run_id.clone(),
        node_id: node_id.to_string(),
        attempt_id: attempt_id.to_string(),
    };
    let (artifact_id, _) = cas
        .put_artifact(
            review_core::contract::WORKER_NOTES_V1,
            producer,
            vec![result_artifact.to_string()],
            Some(authority.head_snapshot_id.clone()),
            payload,
        )
        .map_err(|error| error.to_string())?;
    notes_record(
        node_id,
        attempt_id,
        result_artifact,
        Some(artifact_id),
        None,
        bytes,
    )
}

fn notes_record(
    node_id: &str,
    attempt_id: &str,
    result_artifact: &str,
    notes_artifact_id: Option<String>,
    dropped: Option<WorkerNotesDropReasonV1>,
    bytes: u64,
) -> Result<PreparedNotes, String> {
    let payload = WorkerNotesRecordedPayloadV1 {
        result_artifact_id: result_artifact.to_string(),
        notes_artifact_id: notes_artifact_id.clone(),
        dropped,
        bytes,
    };
    payload.validate()?;
    let mut refs = vec![result_artifact.to_string()];
    refs.extend(notes_artifact_id);
    Ok(PreparedNotes {
        event: NewEvent::new(EventType::WorkerNotesRecordedV1, encode(&payload)?)
            .node(node_id)
            .attempt(attempt_id.to_string())
            .referencing(refs),
    })
}

fn tree_entry_digest(manifest: &Manifest, path: &str) -> Option<String> {
    manifest
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(path))
        .ok()
        .map(|index| manifest.entries[index].content.clone())
}
