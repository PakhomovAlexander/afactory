//! Source materialization and durable candidates for Task Workers. S0 and S1 stay separate
//! inputs; only the installed seal operation establishes the derived Snapshot lineage.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::PortAffinityV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use review_core::{PortCardinality, Producer};
use review_graph::task::OperatorSignature;
use review_runner::task::WorkerAccess;
use review_sandbox::{Mode, Policy, Sandbox, SealedSandbox};
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

/// The sandbox access a Worker's captured signature derives. The source environment's mode and
/// the model adapter's tools both come from this one function, so they cannot disagree, and no
/// package, runner argument or `.af/` policy can name a tool outside it. Only a review Worker
/// turns `execute-checks` into a shell; any other Worker keeps its read-only source.
pub fn worker_access(signature: &OperatorSignature) -> WorkerAccess {
    if signature.effects.contains("write-source") {
        WorkerAccess::WriteSource
    } else if signature.effects.contains("execute-checks") && signature.roles.contains("review") {
        WorkerAccess::ExecuteChecks
    } else {
        WorkerAccess::ReadOnly
    }
}

impl SnapshotTaskEnvironment {
    /// Materialize a source snapshot in an ephemeral writable clone for AF-owned preparation.
    /// The caller must remove its private material before `finish`, which still requires the
    /// declared source to seal unchanged.
    pub(crate) fn materialize_preparation(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
    ) -> Result<Sandbox, String> {
        if signature.effects.contains("write-source")
            || signature
                .effects
                .iter()
                .any(|effect| !matches!(effect.as_str(), "read-source" | "execute-checks"))
        {
            return Err("AF-owned preparation requires a non-source-writing Worker".into());
        }
        self.materialize_mode(cas, invocation, Mode::EphemeralWrite, false)
    }

    fn materialize_mode(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        mode: Mode,
        captures_candidate: bool,
    ) -> Result<Sandbox, String> {
        let (id, _, mut manifest) = source_snapshot(
            cas,
            invocation
                .inputs
                .get("source")
                .ok_or("Source Worker has no source port")?,
        )?;
        add_review_inputs(cas, invocation, captures_candidate, &id, &mut manifest)?;
        let sandbox = Sandbox::materialize(&manifest, cas, mode).map_err(|e| e.to_string())?;
        review_sandbox::admit(self.policy, &sandbox).map_err(|e| e.to_string())?;
        Ok(sandbox)
    }
}

/// Every path by which a Worker without a candidate port changed its declared source, sorted.
/// A read-only Worker may change nothing at all. An execute-checks reviewer may add anything:
/// build output, its own harness, and the dotfiles the tools it runs write into `HOME`, which
/// is the sandbox root (`.claude.json`, say). The clone is discarded, so an added byte never
/// reaches a candidate, a Proposal or a delivered tree; what the declared source guarantees is
/// that every materialized entry is still there and byte-identical, and that is what is checked.
fn source_edits(access: WorkerAccess, sealed: &SealedSandbox) -> Vec<String> {
    let added = if access == WorkerAccess::ExecuteChecks {
        &[][..]
    } else {
        &sealed.mutations.added[..]
    };
    let mut changed: Vec<String> = sealed
        .mutations
        .modified
        .iter()
        .chain(&sealed.mutations.deleted)
        .chain(added)
        .cloned()
        .collect();
    changed.sort();
    changed
}

/// A bounded diagnostic naming changed paths: a build in the wrong place can touch thousands.
fn name_paths(paths: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut named = paths
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        named.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    named
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
        // An execute-checks reviewer gets the same ephemeral clone as AF-owned preparation:
        // writable so it can build and run the candidate, sealed back never.
        let mode = match worker_access(signature) {
            WorkerAccess::ReadOnly => Mode::ReadOnly,
            WorkerAccess::ExecuteChecks | WorkerAccess::WriteSource => Mode::EphemeralWrite,
        };
        self.materialize_mode(cas, invocation, mode, candidate_port(signature))
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
            let access = worker_access(signature);
            let changed = source_edits(access, &sealed);
            if !changed.is_empty() {
                let what = if access == WorkerAccess::ExecuteChecks {
                    "Execute-checks reviewer changed its declared source"
                } else {
                    "Read-only Worker mutated its source Snapshot"
                };
                return Err(format!("{what}: {}", name_paths(&changed)));
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

/// Host input files belong only to the disposable baseline. They are not source Snapshot
/// entries and can never flow through the candidate-tree capture path; an execute-checks clone
/// seals them back unchanged or fails like any other declared source.
pub(super) fn add_review_inputs(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    captures_candidate: bool,
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
    if captures_candidate || port.cardinality != PortCardinality::One {
        return Err("Readable Review inputs require a Worker whose tree is never captured".into());
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
    let path = review_source_git::encode_path(scope.patch.path.as_bytes());
    // Reserve this exact host-input directory: a source file, directory or symlink there
    // must not be overwritten or mistaken for host-provided context.
    let directory = review_source_git::encode_path(b".af-review-inputs");
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
    *manifest = review_source_git::Manifest::new(entries).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::pipeline::PipelineContractV1;
    use review_source_git::{Entry, EntryKind, Manifest};

    fn signature(effects: &str, roles: &str) -> OperatorSignature {
        OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::new(),
                outputs: BTreeMap::new(),
            },
            effects: effects.split_whitespace().map(str::to_owned).collect(),
            evidence: BTreeMap::new(),
            retains: BTreeMap::new(),
            roles: roles.split_whitespace().map(str::to_owned).collect(),
            worker_input_type: None,
            worker_output_type: None,
            outcome_port: None,
            attempt: None,
        }
    }

    #[test]
    fn only_a_review_worker_turns_execute_checks_into_a_shell() {
        use WorkerAccess::{ExecuteChecks, ReadOnly, WriteSource};
        for (effects, roles, access) in [
            ("read-source", "review", ReadOnly),
            ("", "review", ReadOnly),
            ("read-source execute-checks", "review", ExecuteChecks),
            ("execute-checks", "author review", ExecuteChecks),
            ("read-source execute-checks", "author", ReadOnly),
            ("read-source execute-checks", "", ReadOnly),
            ("write-source", "author", WriteSource),
            ("write-source execute-checks", "review", WriteSource),
        ] {
            assert_eq!(
                worker_access(&signature(effects, roles)),
                access,
                "effects `{effects}`, roles `{roles}`"
            );
        }
    }

    /// Seal an ephemeral-write clone of a two-file source after `edit` ran in it.
    fn sealed_after(cas: &Cas, edit: impl FnOnce(&std::path::Path)) -> SealedSandbox {
        let entries: Vec<Entry> = [
            ("lib.rs", "pub fn a() {}\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]
        .into_iter()
        .map(|(path, text)| Entry {
            path: path.into(),
            kind: EntryKind::File,
            content: cas.put(text.as_bytes()).unwrap(),
            size: text.len() as u64,
        })
        .collect();
        let manifest = Manifest::new(entries).unwrap();
        let sandbox = Sandbox::materialize(&manifest, cas, Mode::EphemeralWrite).unwrap();
        edit(sandbox.root());
        sandbox.seal().unwrap()
    }

    #[test]
    fn execute_checks_reviewer_may_add_but_never_change_or_remove_declared_source() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let build = sealed_after(&cas, |root| {
            std::fs::create_dir_all(root.join("target/uix-harness")).unwrap();
            std::fs::write(root.join("target/uix-harness/drive.py"), "import pty\n").unwrap();
        });
        assert!(source_edits(WorkerAccess::ExecuteChecks, &build).is_empty());
        assert_eq!(
            source_edits(WorkerAccess::ReadOnly, &build),
            ["target/uix-harness/drive.py"]
        );
        let edited = sealed_after(&cas, |root| {
            std::fs::create_dir_all(root.join("target")).unwrap();
            std::fs::write(root.join("target/out"), "build output\n").unwrap();
            std::fs::write(root.join("lib.rs"), "pub fn b() {}\n").unwrap();
            std::fs::remove_file(root.join("src/main.rs")).unwrap();
            std::fs::write(root.join("src/extra.rs"), "pub fn c() {}\n").unwrap();
            std::fs::write(root.join("NOTES.md"), "root file\n").unwrap();
        });
        assert_eq!(
            source_edits(WorkerAccess::ExecuteChecks, &edited),
            ["lib.rs", "src/main.rs"]
        );
        let dotfile = sealed_after(&cas, |root| {
            std::fs::write(root.join(".claude.json"), "{}\n").unwrap();
        });
        assert!(source_edits(WorkerAccess::ExecuteChecks, &dotfile).is_empty());
        assert_eq!(
            source_edits(WorkerAccess::ReadOnly, &dotfile),
            [".claude.json"]
        );
        let paths: Vec<String> = (0..25).map(|n| format!("p{n:02}")).collect();
        assert_eq!(
            name_paths(&paths),
            format!("{} and 5 more", paths[..20].join(", "))
        );
    }
}
