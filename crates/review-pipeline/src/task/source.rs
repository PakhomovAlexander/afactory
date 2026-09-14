//! Source materialization and durable candidates for Task Workers. S0 and S1 stay separate
//! inputs; only the installed seal operation establishes the derived Snapshot lineage.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::PortAffinityV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use review_core::{PortCardinality, Producer};
use review_graph::task::OperatorSignature;
use review_sandbox::{Mode, Policy, Sandbox};
use review_source_git::task::{
    CANDIDATE_TREE_V1, CandidateTree, SOURCE_TREE_V1, SourceTree, TaskSnapshot, derive_source_tree,
    read_manifest, read_snapshot,
};
use review_store::Cas;
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::store::task::task_run_id;

use super::envelope;
use super::host::TaskEnvironment;

pub struct SnapshotTaskEnvironment {
    pub policy: Policy,
}

pub fn invocation_producer(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    attempt: Option<&PreparedTaskAttempt>,
) -> Result<Producer, String> {
    let plan: ExecutionPlanV1 = serde_json::from_value(envelope(cas, &invocation.plan_id)?.payload)
        .map_err(|e| e.to_string())?;
    let task: TaskRevisionV1 =
        serde_json::from_value(envelope(cas, &plan.task_revision_id)?.payload)
            .map_err(|e| e.to_string())?;
    let run_id = task_run_id(&task.task_id).map_err(|e| e.to_string())?;
    Ok(match attempt {
        Some(attempt) => Producer::Attempt {
            run_id,
            node_id: invocation.node.clone(),
            attempt_id: attempt.id().into(),
        },
        None => Producer::KernelOperation {
            run_id,
            node_id: Some(invocation.node.clone()),
            operation_id: "task-builtin@1".into(),
        },
    })
}

pub fn source_input(cas: &Cas, input: &ArtifactInputV1) -> Result<String, String> {
    source_snapshot(cas, input).map(|(id, _, _)| id)
}

/// Read exact source authority and its validated tree once for this operation.
pub fn source_snapshot(
    cas: &Cas,
    input: &ArtifactInputV1,
) -> Result<(String, TaskSnapshot, review_source_git::Manifest), String> {
    input.validate()?;
    if input.artifact_type != SOURCE_TREE_V1 || input.cardinality != PortCardinality::One {
        return Err("Source environment requires one typed SourceTree".into());
    }
    let artifact = envelope(cas, &input.artifact_ids[0])?;
    let source: SourceTree = serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
    if artifact.artifact_type != SOURCE_TREE_V1
        || artifact.subject_snapshot_id.as_ref() != Some(&source.snapshot_id)
        || input.snapshot_id.as_ref() != Some(&source.snapshot_id)
    {
        return Err("SourceTree disagrees with its admitted Snapshot".into());
    }
    let (snapshot, manifest) = read_snapshot(cas, &source.snapshot_id)?;
    Ok((source.snapshot_id, snapshot, manifest))
}

fn candidate_port(signature: &OperatorSignature) -> bool {
    signature.effects.contains("write-source")
        && signature
            .contract
            .outputs
            .get("candidate")
            .is_some_and(|port| {
                port.artifact_type == CANDIDATE_TREE_V1
                    && port.cardinality == PortCardinality::One
                    && !port.optional
                    && port.affinity
                        == (PortAffinityV1::SameAs {
                            input: "source".into(),
                        })
            })
}

impl TaskEnvironment for SnapshotTaskEnvironment {
    fn kernel_outputs(&self, signature: &OperatorSignature) -> BTreeSet<String> {
        if candidate_port(signature) {
            BTreeSet::from(["candidate".into()])
        } else {
            BTreeSet::new()
        }
    }

    fn materialize(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
    ) -> Result<Sandbox, String> {
        if signature.effects.iter().any(|effect| {
            !matches!(
                effect.as_str(),
                "read-source" | "write-source" | "execute-checks"
            )
        }) {
            return Err("Source environment cannot provide the declared effects".into());
        }
        if signature.effects.contains("write-source") && !candidate_port(signature) {
            return Err(
                "Source-writing Worker must expose a kernel-captured candidate port".into(),
            );
        }
        let (id, _, mut manifest) = source_snapshot(
            cas,
            invocation
                .inputs
                .get("source")
                .ok_or("Source Worker has no source port")?,
        )?;
        let mode = if candidate_port(signature) {
            Mode::EphemeralWrite
        } else {
            Mode::ReadOnly
        };
        add_review_inputs(cas, invocation, mode, &id, &mut manifest)?;
        let sandbox = Sandbox::materialize(&manifest, cas, mode).map_err(|e| e.to_string())?;
        review_sandbox::admit(self.policy, &sandbox).map_err(|e| e.to_string())?;
        Ok(sandbox)
    }

    fn finish(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
        attempt: &PreparedTaskAttempt,
        sandbox: Sandbox,
        mut outputs: BTreeMap<String, ArtifactInputV1>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if !candidate_port(signature) {
            if !sealed.unchanged() {
                return Err("Read-only Worker mutated its source Snapshot".into());
            }
            return Ok(outputs);
        }
        if outputs.contains_key("candidate") {
            return Err("Worker cannot supply the kernel candidate port".into());
        }
        let source = &invocation.inputs["source"];
        let parent_snapshot_id = source_input(cas, source)?;
        let manifest = sealed.capture_snapshot(cas).map_err(|e| e.to_string())?;
        let manifest_id = cas
            .put_json(&serde_json::to_value(&manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        read_manifest(cas, &manifest_id)?;
        let candidate = CandidateTree {
            parent_snapshot_id: parent_snapshot_id.clone(),
            manifest_id: manifest_id.clone(),
        };
        let mut refs = source.artifact_ids.clone();
        refs.extend([parent_snapshot_id.clone(), manifest_id]);
        let id = cas
            .put_artifact(
                CANDIDATE_TREE_V1,
                invocation_producer(cas, invocation, Some(attempt))?,
                refs,
                Some(parent_snapshot_id.clone()),
                serde_json::to_value(candidate).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        outputs.insert(
            "candidate".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: CANDIDATE_TREE_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: Some(parent_snapshot_id),
            },
        );
        Ok(outputs)
    }

    fn validate_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        signature: &OperatorSignature,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if candidate_port(signature) {
            let port = output
                .outputs
                .get("candidate")
                .ok_or("Implementation result lost its captured candidate")?;
            let source = source_input(cas, input.inputs.get("source").ok_or("Missing source")?)?;
            let candidate: CandidateTree =
                serde_json::from_value(envelope(cas, &port.artifact_ids[0])?.payload)
                    .map_err(|e| e.to_string())?;
            if candidate.parent_snapshot_id != source || port.snapshot_id.as_ref() != Some(&source)
            {
                return Err("Captured candidate changed its source Snapshot".into());
            }
            read_manifest(cas, &candidate.manifest_id)?;
        }
        Ok(())
    }
}

pub fn seal_candidate(cas: &Cas, invocation: &TaskInvocationV1) -> Result<ArtifactInputV1, String> {
    let input = invocation
        .inputs
        .get("candidate")
        .ok_or("Seal has no candidate input")?;
    if input.artifact_type != CANDIDATE_TREE_V1 || input.cardinality != PortCardinality::One {
        return Err("Seal requires one typed candidate".into());
    }
    let artifact = envelope(cas, &input.artifact_ids[0])?;
    let candidate: CandidateTree =
        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
    if input.snapshot_id.as_ref() != Some(&candidate.parent_snapshot_id)
        || !matches!(artifact.producer, Producer::Attempt { .. })
    {
        return Err("Seal candidate has no admitted Attempt or parent affinity".into());
    }
    derive_source_tree(
        cas,
        invocation_producer(cas, invocation, None)?,
        &candidate.manifest_id,
        &candidate.parent_snapshot_id,
        input.artifact_ids.clone(),
    )
}

pub fn validate_seal(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    output: &TaskOutputV1,
) -> Result<(), String> {
    let input = invocation
        .inputs
        .get("candidate")
        .ok_or("Seal has no candidate")?;
    let candidate: CandidateTree =
        serde_json::from_value(envelope(cas, &input.artifact_ids[0])?.payload)
            .map_err(|e| e.to_string())?;
    let (_, snapshot, _) = source_snapshot(
        cas,
        output
            .outputs
            .get("snapshot")
            .ok_or("Seal has no Snapshot")?,
    )?;
    if snapshot.parent_snapshot_id.as_ref() != Some(&candidate.parent_snapshot_id)
        || snapshot.manifest_id != candidate.manifest_id
    {
        return Err("Seal output does not identify its exact admitted candidate tree".into());
    }
    Ok(())
}

/// Host input files belong only to the disposable read-only baseline. They are not source
/// Snapshot entries and can never flow through the candidate-tree capture path.
pub(super) fn add_review_inputs(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    mode: Mode,
    source: &str,
    manifest: &mut review_source_git::Manifest,
) -> Result<(), String> {
    use review_core::task::review::{TASK_REVIEW_SUBJECT_V2, TaskReviewSubjectV2};
    let Some(port) = invocation
        .inputs
        .get("subject")
        .filter(|p| p.artifact_type == TASK_REVIEW_SUBJECT_V2)
    else {
        return Ok(());
    };
    port.validate()?;
    if mode != Mode::ReadOnly || port.cardinality != PortCardinality::One {
        return Err("Readable Review inputs require a read-only source Worker".into());
    }
    let artifact = envelope(cas, &port.artifact_ids[0])?;
    let subject: TaskReviewSubjectV2 =
        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
    subject.validate()?;
    if subject.snapshot_id != source
        || artifact.artifact_type != TASK_REVIEW_SUBJECT_V2
        || artifact.subject_snapshot_id.as_deref() != Some(source)
        || port.snapshot_id.as_deref() != Some(source)
    {
        return Err("Readable Review input changed its exact source Snapshot".into());
    }
    let Some(scope) = subject.change_scope.as_ref() else {
        return Ok(());
    };
    let change_id = subject
        .subject
        .change_set_id
        .as_ref()
        .ok_or("Readable Diff lacks ChangeSet")?;
    let bytes = cas
        .get_bounded(change_id, review_core::MAX_CHANGE_SET_BYTES as u64)
        .map_err(|e| e.to_string())?;
    let changes: review_core::ChangeSetV1 =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    changes.validate()?;
    let patch = cas
        .get_bounded(
            &scope.patch.content_id,
            review_core::MAX_CHANGE_SET_BYTES as u64,
        )
        .map_err(|e| e.to_string())?;
    if patch.len() as u64 != scope.patch.bytes
        || patch != changes.canonical_patch()?
        || changes.head_snapshot_id != source
        || Some(&changes.base_snapshot_id) != subject.subject.base_snapshot_id.as_ref()
        || changes.changed_paths != scope.changed_paths
        || changes.renames != scope.renames
        || changes.rename_detection_truncated != scope.rename_detection_truncated
        || changes.git_version != scope.git_version
        || changes.diff_policy_version != scope.diff_policy_version
        || !artifact.input_artifacts.contains(change_id)
        || !artifact.input_artifacts.contains(&scope.patch.content_id)
    {
        return Err("Readable Review file changed its declared content, bytes or authority".into());
    }
    let path = manifest.encode_key(scope.patch.path.as_bytes());
    // Reserve this exact host-input directory: a source file, directory or symlink there
    // must not be overwritten or mistaken for host-provided context.
    let directory = manifest.encode_key(b".af-review-inputs");
    if manifest
        .entries
        .iter()
        .any(|entry| entry.path == directory || entry.path.starts_with(&format!("{directory}/")))
    {
        return Err("Source collides with the declared Review input directory".into());
    }
    let mut entries = manifest.entries.clone();
    entries.push(review_source_git::Entry {
        path,
        kind: review_source_git::EntryKind::File,
        content: scope.patch.content_id.clone(),
        size: scope.patch.bytes,
    });
    *manifest = review_source_git::Manifest::new_with_encoding(entries, manifest.path_encoding)
        .map_err(|e| e.to_string())?;
    Ok(())
}
