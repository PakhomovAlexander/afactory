//! Resolving a Task file's optional `inputs` table into ordinary root ports.
//!
//! Resolution happens once, at plan time, and reads only the Store named by `--state`
//! (ADR-0117). From the resolved `ArtifactInputV1` onwards a binding is indistinguishable from
//! a capture: the compiled plan carries exact artifact IDs, so `af task run`, resume, retry and
//! replay never read the referencing Task file again.
//!
//! Every refusal here is an ordinary Task-file input error — `af: …` on stderr, exit 1, the
//! `af/error@1` document under `--json` — and every one of them runs before a Worker is
//! dispatched or a Provider admitted, because `start_captured` resolves bindings while it is
//! still building the Task revision.

use super::*;
use review_core::task::document::DOCUMENT_SOURCES_V1;
use review_core::task::input_bindings::{
    ReferencedTaskV1, TASK_INPUT_BINDINGS_V1, TaskInputBindingV1, TaskInputBindingsV1,
};
use review_core::task::optimization::OPTIMIZATION_HISTORY_V1;
use review_source_git::task::{
    SourceTree, TaskSnapshot, TaskSourceBoundFromV1, TaskSourceOriginV2, read_origin, read_snapshot,
};

/// One root input reference in the Task file: exactly one of the two closed forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskOutputRefV1 {
    pub(super) task: String,
    pub(super) port: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExactArtifactRefV1 {
    pub(super) artifact: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum TaskInputRefV1 {
    Task(TaskOutputRefV1),
    Artifact(ExactArtifactRefV1),
}

/// What resolution produced: the root ports to install, the identity of the typed record to
/// reference from the revision's provenance, and — when `source` was bound — the Manifest of
/// the Snapshot the Task will read, so plan-time advisories describe that tree, not the
/// checkout.
#[derive(Debug)]
pub(super) struct BoundInputs {
    pub(super) ports: BTreeMap<String, ArtifactInputV1>,
    pub(super) record_id: String,
    pub(super) source_manifest: Option<Manifest>,
}

/// The Store lookup resolution needs. Everything else a binding reads — the result, its output
/// ports and their artifacts — comes from the CAS, so this is the whole surface a recorded Task
/// presents here.
pub(super) trait RecordedTasks {
    fn phase(&self, cas: &Cas, task_id: &str) -> Result<Option<TaskPhaseV1>, String>;
}

impl RecordedTasks for EventStore {
    fn phase(&self, cas: &Cas, task_id: &str) -> Result<Option<TaskPhaseV1>, String> {
        let found = self.task_projection(cas, task_id);
        let projection = found.map_err(|error| error.to_string())?;
        Ok(projection.map(|state| state.phase))
    }
}

/// The ports whose construction the Task-file adapter owns, and which a binding replaces.
const BINDABLE: [&str; 3] = ["source", "history", "sources"];
/// Root ports the adapter constructs from something else. Named so the refusal can say why.
const NOT_BINDABLE: [&str; 3] = ["requirements", "base", "continuation"];

fn cardinality_name(value: PortCardinality) -> &'static str {
    match value {
        PortCardinality::One => "one",
        PortCardinality::Many => "many",
    }
}

fn phase_name(phase: &TaskPhaseV1) -> &'static str {
    match phase {
        TaskPhaseV1::Submitted {} => "submitted",
        TaskPhaseV1::Ready {} => "ready",
        TaskPhaseV1::Running {} => "running",
        TaskPhaseV1::Waiting { .. } => "waiting",
        TaskPhaseV1::Finished { .. } => "finished",
    }
}

/// Every label a refusal echoes comes from the Task file or from a recorded document, so it is
/// untrusted display data wherever it is printed. The preview's sanitizer maps each non-ASCII or
/// control character to `?`, and the bound keeps one refusal to one line; the artifact bound
/// leaves an exact `sha256:` digest whole.
fn shown(value: &str, limit: usize) -> String {
    super::preview::short(value, limit)
}
const NAME_SHOWN: usize = 48;
const DIGEST_SHOWN: usize = 80;

/// The untrusted half of every refusal: the destination port, and the referenced Task and its
/// output port, or the exact artifact ID.
fn label(port: &str, reference: &TaskInputRefV1) -> String {
    let port = shown(port, NAME_SHOWN);
    match reference {
        TaskInputRefV1::Task(task) => {
            let named = shown(&task.task, NAME_SHOWN);
            let output = shown(&task.port, NAME_SHOWN);
            format!("{port} <- task {named}/{output}")
        }
        TaskInputRefV1::Artifact(exact) => {
            let artifact = shown(&exact.artifact, DIGEST_SHOWN);
            format!("{port} <- artifact {artifact}")
        }
    }
}

fn refuse(port: &str, reference: &TaskInputRefV1, reason: impl std::fmt::Display) -> String {
    let named = label(port, reference);
    format!("Task input binding {named}: {reason}")
}

/// The type and cardinality this port carries for this Task's profile. A `many` recorded output
/// never binds a `one` port, so the comparison is exact equality, never containment.
fn expected_port(
    profile: TaskKindProfile,
    port: &str,
) -> Result<(&'static str, PortCardinality), String> {
    let optimization = matches!(
        profile,
        TaskKindProfile::OptimizationAnalysis | TaskKindProfile::OptimizationCandidate
    );
    let history = if optimization {
        OPTIMIZATION_HISTORY_V1
    } else {
        REVIEW_HISTORY_V1
    };
    let artifact_type = match port {
        "source" => SOURCE_TREE_V1,
        "history" => history,
        "sources" => DOCUMENT_SOURCES_V1,
        other if NOT_BINDABLE.contains(&other) => {
            let bindable = BINDABLE.join(", ");
            let named = shown(other, NAME_SHOWN);
            let reason = format!("the root input {named} is not bindable");
            return Err(format!("{reason}; only {bindable} are"));
        }
        other => {
            let bindable = BINDABLE.join(", ");
            let named = shown(other, NAME_SHOWN);
            let reason = format!("{named} is not a bindable root input port");
            return Err(format!("{reason}; only {bindable} are"));
        }
    };
    Ok((artifact_type, PortCardinality::One))
}

/// The recorded output of a Task in this Store, with the provenance the binding record keeps.
fn recorded_output(
    cas: &Cas,
    store: &dyn RecordedTasks,
    reference: &TaskOutputRefV1,
) -> Result<(ArtifactInputV1, ReferencedTaskV1), String> {
    if !is_name(&reference.task) {
        return Err("the referenced Task ID is not a valid identity".into());
    }
    if !is_name(&reference.port) {
        return Err("the referenced output port is not a valid name".into());
    }
    let phase = store.phase(cas, &reference.task)?;
    let phase = phase.ok_or("no such Task is recorded in this Store")?;
    let TaskPhaseV1::Finished { result_id } = &phase else {
        let state = phase_name(&phase);
        return Err(format!("the referenced Task is {state}, not finished"));
    };
    let result: TaskResultV1 = artifact(cas, result_id, TASK_RESULT_V1)?;
    result.validate()?;
    let Some(recorded) = result.outputs.get(&reference.port) else {
        let carried = result
            .outputs
            .keys()
            .map(|name| shown(name, NAME_SHOWN))
            .collect::<Vec<_>>();
        let names = if carried.is_empty() {
            "none".to_owned()
        } else {
            carried.join(", ")
        };
        let port = shown(&reference.port, NAME_SHOWN);
        return Err(format!(
            "the recorded result has no output port {port}; it carries {names}"
        ));
    };
    for id in &recorded.artifact_ids {
        cas.verify(id)
            .map_err(|error| format!("the recorded output is unreadable: {error}"))?;
    }
    let task = ReferencedTaskV1 {
        task_id: reference.task.clone(),
        task_revision_id: result.task_revision_id.clone(),
        result_id: result_id.clone(),
        port: reference.port.clone(),
        acceptance: result.acceptance,
        domain_conclusion: result.domain_conclusion.clone(),
    };
    task.validate()?;
    Ok((recorded.clone(), task))
}

/// One artifact in this Store by exact ID: cardinality `one` by definition, and the type is
/// the envelope's own.
fn exact_artifact(cas: &Cas, exact: &ExactArtifactRefV1) -> Result<ArtifactInputV1, String> {
    if !review_core::is_digest(&exact.artifact) {
        return Err("an exact reference must be a sha256: artifact ID".into());
    }
    let found = cas.get_artifact(&exact.artifact);
    let envelope = found.map_err(|e| format!("not in this Store: {e}"))?;
    validate_envelope(&envelope)?;
    Ok(ArtifactInputV1 {
        artifact_ids: vec![exact.artifact.clone()],
        artifact_type: envelope.artifact_type,
        cardinality: PortCardinality::One,
        snapshot_id: envelope.subject_snapshot_id,
    })
}

type BoundSource = (ArtifactInputV1, TaskInputBindingV1, Manifest);
type AdmittedSource = (String, String, TaskSnapshot, Manifest);

/// The referenced `af/SourceTree@1`, admitted before anything is decided about it.
///
/// One Snapshot identity is named in three places — the envelope's payload, its
/// `subject_snapshot_id` and the recorded port — and all three must agree, because an envelope
/// that named one Snapshot in its subject and another in its payload would otherwise resolve to
/// whichever the branch below happened to read. The Snapshot's origin must read and belong to
/// the tree it describes. Only an admitted source is carried verbatim or re-rooted.
fn admitted_source(cas: &Cas, recorded: &ArtifactInputV1) -> Result<AdmittedSource, String> {
    let [referenced] = recorded.artifact_ids.as_slice() else {
        return Err("a bound source names exactly one af/SourceTree@1 artifact".into());
    };
    let found = cas.get_artifact(referenced);
    let envelope = found.map_err(|e| format!("the source reference is unreadable: {e}"))?;
    validate_envelope(&envelope)?;
    if envelope.artifact_id != *referenced || envelope.artifact_type != SOURCE_TREE_V1 {
        return Err("the reference is not an exact af/SourceTree@1 artifact".into());
    }
    let subject = envelope.subject_snapshot_id.clone();
    let decoded = serde_json::from_value(envelope.payload);
    let tree: SourceTree = decoded.map_err(|error| error.to_string())?;
    // Three identities, one Snapshot: the payload's, the envelope subject's and the recorded
    // port's. An absent one cannot agree, so it is refused like a different one.
    let port = recorded.snapshot_id.as_deref();
    if subject.as_deref() != Some(tree.snapshot_id.as_str())
        || port != Some(tree.snapshot_id.as_str())
    {
        return Err("the referenced SourceTree disagrees with the Snapshot it names".into());
    }
    let (snapshot, manifest) = read_snapshot(cas, &tree.snapshot_id)?;
    let origin = read_origin(cas, &snapshot.origin_id)?;
    if origin.repository_id().trim().is_empty() {
        return Err("the referenced Snapshot's origin names no repository".into());
    }
    // A root capture's origin describes that tree itself, which is exactly what delivery
    // compares; a derived tree's origin describes the root it descends from, and the re-rooted
    // origin below states the bound tree's own content digest.
    let root = snapshot.parent_snapshot_id.is_none();
    if root && origin.content_digest() != snapshot.content_digest {
        return Err("the referenced root Snapshot's origin describes another tree".into());
    }
    Ok((referenced.clone(), tree.snapshot_id, snapshot, manifest))
}

/// A bound `source` is decided by one property of the admitted Snapshot and nothing else: a
/// root capture is carried verbatim, a derived tree is re-rooted over the identical Manifest.
fn bind_source(
    cas: &Cas,
    recorded: ArtifactInputV1,
    task: Option<&ReferencedTaskV1>,
) -> Result<BoundSource, String> {
    let (referenced, snapshot_id, snapshot, manifest) = admitted_source(cas, &recorded)?;
    if snapshot.parent_snapshot_id.is_none() {
        // A root capture — including a parentless Snapshot whose origin is already generation
        // two. Nothing is re-rooted twice, and a committed one delivers exactly as a Task
        // planned from the checkout would.
        let binding = TaskInputBindingV1 {
            artifact_id: referenced,
            resolved_artifact_id: None,
            snapshot_id: Some(snapshot_id.clone()),
            rerooted_snapshot_id: None,
            task: task.cloned(),
        };
        let port = ArtifactInputV1 {
            snapshot_id: Some(snapshot_id),
            ..recorded
        };
        return Ok((port, binding, manifest));
    }
    let origin = read_origin(cas, &snapshot.origin_id)?;
    let rerooted_origin = TaskSourceOriginV2 {
        schema: "af.task-source-origin/2".into(),
        repository_id: origin.repository_id().to_owned(),
        content_digest: snapshot.content_digest.clone(),
        bound_from: TaskSourceBoundFromV1 {
            artifact_id: referenced.clone(),
            snapshot_id: snapshot_id.clone(),
            task: task.cloned(),
        },
    };
    let encoded = serde_json::to_value(&rerooted_origin);
    let encoded = encoded.map_err(|error| error.to_string())?;
    let put = cas.put_json(&encoded);
    let origin_id = put.map_err(|error| error.to_string())?;
    let rerooted = capture_snapshot(cas, &manifest, &origin_id, None)?;
    // The referenced envelope still names the derived Snapshot, and `source_snapshot`
    // admission refuses a SourceTree that disagrees with its Snapshot, so the re-rooted
    // Snapshot gets its own envelope and the port carries that new artifact ID.
    let refs = vec![origin_id];
    let port = source_tree(cas, producer(), &rerooted, refs)?;
    let first = port.artifact_ids.first().cloned();
    let resolved = first.ok_or("re-rooting published no SourceTree")?;
    let binding = TaskInputBindingV1 {
        artifact_id: referenced,
        resolved_artifact_id: Some(resolved),
        snapshot_id: Some(snapshot_id),
        rerooted_snapshot_id: Some(rerooted),
        task: task.cloned(),
    };
    Ok((port, binding, manifest))
}

pub(super) fn resolve(
    cas: &Cas,
    store: &dyn RecordedTasks,
    profile: TaskKindProfile,
    references: &BTreeMap<String, TaskInputRefV1>,
) -> Result<BoundInputs, String> {
    let mut ports = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut source_manifest = None;
    for (port, reference) in references {
        let wanted = expected_port(profile, port);
        let (want_type, want_one) = wanted.map_err(|e| refuse(port, reference, e))?;
        let (recorded, task) = match reference {
            TaskInputRefV1::Task(named) => {
                let found = recorded_output(cas, store, named);
                let (input, task) = found.map_err(|e| refuse(port, reference, e))?;
                (input, Some(task))
            }
            TaskInputRefV1::Artifact(exact) => {
                let found = exact_artifact(cas, exact);
                (found.map_err(|e| refuse(port, reference, e))?, None)
            }
        };
        if recorded.artifact_type != want_type || recorded.cardinality != want_one {
            let reason = format!(
                "the reference is {} {} and this port takes {} {}",
                shown(&recorded.artifact_type, NAME_SHOWN),
                cardinality_name(recorded.cardinality),
                want_type,
                cardinality_name(want_one)
            );
            return Err(refuse(port, reference, reason));
        }
        let (resolved, binding) = if port.as_str() == "source" {
            let bound = bind_source(cas, recorded, task.as_ref());
            let (input, binding, manifest) = bound.map_err(|e| refuse(port, reference, e))?;
            source_manifest = Some(manifest);
            (input, binding)
        } else {
            // `history` and `sources` hold no Snapshot semantics that sealing, ancestry or
            // delivery depend on, so the referenced artifact ID is carried verbatim.
            let first = recorded.artifact_ids.first().cloned();
            let reason = "the reference holds no artifact";
            let artifact_id = first.ok_or_else(|| refuse(port, reference, reason))?;
            let binding = TaskInputBindingV1 {
                artifact_id,
                resolved_artifact_id: None,
                snapshot_id: None,
                rerooted_snapshot_id: None,
                task: task.clone(),
            };
            let input = ArtifactInputV1 {
                snapshot_id: None,
                ..recorded
            };
            (input, binding)
        };
        let valid = resolved.validate();
        valid.map_err(|e| refuse(port, reference, e))?;
        ports.insert(port.clone(), resolved);
        bindings.insert(port.clone(), binding);
    }
    let record = TaskInputBindingsV1 {
        schema: "af.task-input-bindings/1".into(),
        bindings,
    };
    record.validate()?;
    let mut refs = Vec::new();
    for binding in record.bindings.values() {
        refs.push(binding.artifact_id.clone());
        refs.extend(binding.resolved_artifact_id.clone());
    }
    refs.sort();
    refs.dedup();
    let payload = serde_json::to_value(&record).map_err(|e| e.to_string())?;
    let put = cas.put_artifact(TASK_INPUT_BINDINGS_V1, producer(), refs, None, payload);
    let record_id = put.map_err(|error| error.to_string())?.0;
    Ok(BoundInputs {
        ports,
        record_id,
        source_manifest,
    })
}

/// The bindings a recorded Task revision kept, read back from its provenance. `None` for every
/// Task whose file carried no `inputs` table, which is the reason such a Task's revision, plan
/// and `--json` documents are exactly what they were.
pub(super) fn recorded(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<TaskInputBindingsV1>, String> {
    for id in &revision.provenance.input_artifact_ids {
        // A raw blob in provenance is not a typed record and never claims to be one.
        let found = cas.get_optional_artifact(id);
        let Some(envelope) = found.map_err(|e| e.to_string())? else {
            continue;
        };
        if envelope.artifact_type != TASK_INPUT_BINDINGS_V1 {
            continue;
        }
        let decoded = serde_json::from_value(envelope.payload);
        let record: TaskInputBindingsV1 = decoded.map_err(|e| e.to_string())?;
        record.validate()?;
        return Ok(Some(record));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
