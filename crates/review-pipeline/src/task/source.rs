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
        let (_, _, manifest) = source_snapshot(
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
