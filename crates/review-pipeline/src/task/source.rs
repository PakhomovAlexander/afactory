//! Source materialization and durable candidates for Task Workers. S0 and S1 stay separate
//! inputs; only the installed seal operation establishes the derived Snapshot lineage.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::diff_subject::{
    DIFF_SUBJECT_BASE, DIFF_SUBJECT_OPERATION, DIFF_SUBJECT_PORT, DIFF_SUBJECT_SCHEMA,
    DIFF_SUBJECT_V1, DiffSubjectChangeV1, DiffSubjectV1, MAX_DIFF_PATHS,
};
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
/// package, runner argument or `.af/` policy can name a tool outside it. A review Worker turns
/// `execute-checks` into a shell (ADR-0118), and so does a report author that reads source and
/// never writes it (ADR-0133); any other non-writing Worker keeps its read-only source.
pub fn worker_access(signature: &OperatorSignature) -> WorkerAccess {
    if signature.effects.contains("write-source") {
        if signature.effects.contains("execute-checks") {
            WorkerAccess::WriteSourceWithShell
        } else {
            WorkerAccess::WriteSource
        }
    } else if signature.effects.contains("execute-checks")
        && (signature.roles.contains("review")
            || (signature.roles.contains("author") && signature.effects.contains("read-source")))
    {
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
        self.materialize_mode(cas, invocation, Mode::EphemeralWrite, false, false)
    }

    fn materialize_mode(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        mode: Mode,
        captures_candidate: bool,
        with_base: bool,
    ) -> Result<Sandbox, String> {
        let (id, snapshot, mut manifest) = source_snapshot(
            cas,
            invocation
                .inputs
                .get("source")
                .ok_or("Source Worker has no source port")?,
        )?;
        add_review_inputs(cas, invocation, captures_candidate, &id, &mut manifest)?;
        let sandbox = Sandbox::materialize(&manifest, cas, mode).map_err(|e| e.to_string())?;
        review_sandbox::admit(self.policy, &sandbox).map_err(|e| e.to_string())?;
        let base = if with_base {
            subject_base(cas, &invocation.plan_id, &snapshot)?
        } else {
            None
        };
        if let Some(base_id) = base {
            let (_, base) = read_snapshot(cas, &base_id)?;
            // The change's paths reach a Worker that declares them as its typed `diff_subject`
            // input (ADR-0254); nothing is written beside the tree but the base itself.
            sandbox
                .attach_base(&base, cas, &[])
                .map_err(|e| format!("The base tree beside the sandbox: {e}"))?;
        }
        Ok(sandbox)
    }
}

/// The Task revision an invocation's plan was made for, or `None` when it names no readable
/// plan: the dispatch's own records, read again where the dispatch reads them.
fn plan_revision(cas: &Cas, plan_id: &str) -> Result<Option<TaskRevisionV1>, String> {
    let Ok(plan) = envelope(cas, plan_id) else {
        return Ok(None);
    };
    let Ok(plan) = serde_json::from_value::<ExecutionPlanV1>(plan.payload) else {
        return Ok(None);
    };
    serde_json::from_value(envelope(cas, &plan.task_revision_id)?.payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The tree a judging Worker of the plan `plan_id` reads at `../base` when its source is
/// `snapshot`: the base its Task file declared, else the base the Store recorded for the source.
/// `None` when there is neither.
pub fn subject_base(
    cas: &Cas,
    plan_id: &str,
    snapshot: &TaskSnapshot,
) -> Result<Option<String>, String> {
    match declared_base(cas, plan_revision(cas, plan_id)?.as_ref())? {
        Some(declared) => Ok(Some(declared)),
        None => recorded_base(cas, snapshot),
    }
}

/// The Diff Subject the kernel supplies on a judging Worker's `diff_subject` port (ADR-0254):
/// the change between the tree the Worker reads at `../base` and its `source`, recorded as one
/// `af/DiffSubject@1` artifact of the kernel's `diff-subject@1` operation for `node`, with the
/// source's Snapshot as its subject. The same records give the same artifact, so a replayed
/// invocation names the identity it recorded. `None` when the source has no base.
pub fn kernel_diff_subject(
    cas: &Cas,
    plan_id: &str,
    node: &str,
    source: &ArtifactInputV1,
) -> Result<Option<ArtifactInputV1>, String> {
    let (id, snapshot, manifest) = source_snapshot(cas, source)?;
    let Some(base_id) = subject_base(cas, plan_id, &snapshot)? else {
        return Ok(None);
    };
    let (_, base) = read_snapshot(cas, &base_id)?;
    let subject = diff_subject(&base_id, &base, &id, &manifest)?;
    let task = plan_revision(cas, plan_id)?.ok_or("A Diff Subject needs its Task's plan")?;
    let producer = Producer::KernelOperation {
        run_id: task_run_id(&task.task_id).map_err(|e| e.to_string())?,
        node_id: Some(node.to_owned()),
        operation_id: DIFF_SUBJECT_OPERATION.into(),
    };
    let mut refs: Vec<String> = source
        .artifact_ids
        .iter()
        .cloned()
        .chain([base_id, id.clone()])
        .collect();
    refs.sort();
    refs.dedup();
    let (artifact_id, _) = cas
        .put_artifact(
            DIFF_SUBJECT_V1,
            producer,
            refs,
            Some(id.clone()),
            serde_json::to_value(&subject).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    Ok(Some(ArtifactInputV1 {
        artifact_ids: vec![artifact_id],
        artifact_type: DIFF_SUBJECT_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(id),
    }))
}

/// Before a judging Worker is dispatched, its `diff_subject` port holds exactly what the kernel
/// would build for it (ADR-0254): absent when its contract does not declare the port or its
/// source has no base, else one `af/DiffSubject@1` of the kernel's own operation for this node
/// whose document is the change between that base and the source. Anything else is refused,
/// naming the node and what to do, so no Worker judges a change it was not given.
pub fn check_diff_subject(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    signature: &OperatorSignature,
) -> Result<Option<DiffSubjectV1>, String> {
    let node = &invocation.node;
    let declared = signature
        .contract
        .inputs
        .get(DIFF_SUBJECT_PORT)
        .is_some_and(|port| port.artifact_type == DIFF_SUBJECT_V1);
    let carried = invocation.inputs.get(DIFF_SUBJECT_PORT);
    if !declared {
        if carried.is_some() {
            return Err(format!(
                "Node {node} carries a `{DIFF_SUBJECT_PORT}` input its Worker's contract does not \
                 declare; plan the Task again"
            ));
        }
        return Ok(None);
    }
    let source = invocation
        .inputs
        .get("source")
        .ok_or_else(|| format!("Node {node} declares the Diff Subject but has no source"))?;
    let (id, snapshot, manifest) = source_snapshot(cas, source)?;
    let base = subject_base(cas, &invocation.plan_id, &snapshot)?;
    let (base_id, carried) = match (base, carried) {
        (None, None) => return Ok(None),
        (Some(base_id), Some(carried)) => (base_id, carried),
        (Some(base_id), None) => {
            return Err(format!(
                "The Diff Subject of node {node} is absent: its source {id} changed {base_id}, so \
                 its Worker would judge a change it was not given; plan the Task again"
            ));
        }
        (None, Some(_)) => {
            return Err(format!(
                "Node {node} carries a Diff Subject, but its source {id} has no base; plan the \
                 Task again"
            ));
        }
    };
    let (_, base) = read_snapshot(cas, &base_id)?;
    let expected = diff_subject(&base_id, &base, &id, &manifest)?;
    carried.validate()?;
    let artifact = envelope(cas, &carried.artifact_ids[0])?;
    let built = matches!(
        &artifact.producer,
        Producer::KernelOperation { node_id, operation_id, .. }
            if node_id.as_deref() == Some(node.as_str()) && operation_id == DIFF_SUBJECT_OPERATION
    );
    let document = serde_json::from_value::<DiffSubjectV1>(artifact.payload)
        .map_err(|e| format!("The Diff Subject of node {node}: {e}"))?;
    if carried.artifact_type != DIFF_SUBJECT_V1
        || carried.cardinality != PortCardinality::One
        || carried.artifact_ids.len() != 1
        || carried.snapshot_id.as_deref() != Some(id.as_str())
        || artifact.artifact_type != DIFF_SUBJECT_V1
        || artifact.subject_snapshot_id.as_deref() != Some(id.as_str())
        || !built
        || document != expected
    {
        return Err(format!(
            "The Diff Subject of node {node} is not the change between {base_id} and its source \
             {id} the kernel builds; plan the Task again"
        ));
    }
    Ok(Some(document))
}

/// The base a Task file declared with `"base": {"commit": …}`, read from the `af/TaskBase@1`
/// record in the revision's provenance (ADR-0212). A revision that names none, or no revision,
/// declares no base.
fn declared_base(cas: &Cas, task: Option<&TaskRevisionV1>) -> Result<Option<String>, String> {
    use review_core::task::findings::{TASK_BASE_V1, TaskBaseV1};
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

/// The acceptance items a Task file stated, read from the `af/TaskAcceptance@1` record in the
/// provenance of the revision `plan_id` executes (ADR-0255), as a check receipt carries them;
/// `None` for a Task that stated none, or a plan that cannot be read.
pub fn task_acceptance(
    cas: &Cas,
    plan_id: &str,
) -> Result<Option<review_core::task::verification::TaskReceiptAcceptanceV2>, String> {
    use review_core::task::acceptance::{TASK_ACCEPTANCE_V1, TaskAcceptanceV1};
    // As for a declared base: the plan and revision are the dispatch's own records, and an
    // invocation that names no readable plan states no acceptance.
    let Ok(plan) = envelope(cas, plan_id) else {
        return Ok(None);
    };
    let Ok(plan) = serde_json::from_value::<ExecutionPlanV1>(plan.payload) else {
        return Ok(None);
    };
    let task: TaskRevisionV1 =
        serde_json::from_value(envelope(cas, &plan.task_revision_id)?.payload)
            .map_err(|e| e.to_string())?;
    for id in &task.provenance.input_artifact_ids {
        let Some(artifact) = cas.get_optional_artifact(id).map_err(|e| e.to_string())? else {
            continue;
        };
        if artifact.artifact_type != TASK_ACCEPTANCE_V1 {
            continue;
        }
        let record: TaskAcceptanceV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        record.validate()?;
        return Ok(Some(
            review_core::task::verification::TaskReceiptAcceptanceV2 {
                record_id: id.clone(),
                items: record.items,
            },
        ));
    }
    Ok(None)
}

/// The tree a Snapshot changed, as the kernel recorded it (ADR-0212): a sealed candidate's
/// parent, or, for a tree a Task bound and re-rooted, the parent of the tree it was bound from.
/// `None` for a root capture, which changed nothing the Store knows of.
pub fn recorded_base(cas: &Cas, snapshot: &TaskSnapshot) -> Result<Option<String>, String> {
    if let Some(parent) = &snapshot.parent_snapshot_id {
        return Ok(Some(parent.clone()));
    }
    // Only a bound origin names a tree this one came from; any other origin, typed or not,
    // names none.
    let Ok(origin) = review_source_git::task::read_origin(cas, &snapshot.origin_id) else {
        return Ok(None);
    };
    let Some(bound) = origin.bound_from() else {
        return Ok(None);
    };
    let (from, _) = read_snapshot(cas, &bound.snapshot_id)?;
    Ok(from.parent_snapshot_id)
}

/// `af/diff-subject@1` between two recorded trees: every path whose kind or content differs,
/// sorted, with the content digest on each side that holds it.
pub fn diff_subject(
    base_id: &str,
    base: &review_source_git::Manifest,
    snapshot_id: &str,
    tree: &review_source_git::Manifest,
) -> Result<DiffSubjectV1, String> {
    let before: BTreeMap<&str, (&str, review_source_git::EntryKind)> = base
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), (entry.content.as_str(), entry.kind)))
        .collect();
    let after: BTreeMap<&str, (&str, review_source_git::EntryKind)> = tree
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), (entry.content.as_str(), entry.kind)))
        .collect();
    let paths: BTreeSet<&str> = before.keys().chain(after.keys()).copied().collect();
    let changes = paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(|path| DiffSubjectChangeV1 {
            path: path.to_owned(),
            before: before.get(path).map(|(content, _)| (*content).to_owned()),
            after: after.get(path).map(|(content, _)| (*content).to_owned()),
        })
        .collect::<Vec<_>>();
    if changes.len() > MAX_DIFF_PATHS {
        return Err(format!(
            "The change holds {} paths, more than a diff subject lists",
            changes.len()
        ));
    }
    let subject = DiffSubjectV1 {
        schema: DIFF_SUBJECT_SCHEMA.into(),
        base_snapshot_id: base_id.to_owned(),
        snapshot_id: snapshot_id.to_owned(),
        base: DIFF_SUBJECT_BASE.into(),
        changes,
    };
    subject.validate()?;
    Ok(subject)
}

/// The top-level names the materialized source holds.
fn owned_top_levels(sealed: &SealedSandbox) -> BTreeSet<String> {
    sealed
        .baseline
        .entries
        .iter()
        .map(|entry| top_level(&entry.path).to_owned())
        .collect()
}

/// The top-level name a Manifest path lives under: its first component.
fn top_level(path: &str) -> &str {
    path.split_once('/').map_or(path, |(first, _)| first)
}

/// Scratch a shell-enabled writer leaves in its clone: a path under a top-level name the
/// source did not hold (`target/`, `.cache/`), or a new top-level dotfile (`.claude.json`).
/// Everything else it added, changed or removed is its candidate.
fn shell_scratch(access: WorkerAccess, owned: &BTreeSet<String>, path: &str) -> bool {
    if access != WorkerAccess::WriteSourceWithShell {
        return false;
    }
    let top = top_level(path);
    if owned.contains(top) {
        return false;
    }
    path.contains('/') || top.starts_with('.')
}

/// Every path by which a Worker without a candidate port changed its declared source, sorted.
/// A read-only Worker may change nothing at all. An execute-checks Worker may add beside the
/// source: build output, its own harness, and the dotfiles the tools it runs write into `HOME`,
/// which is the sandbox root (`.claude.json`, say) — anything under a top-level name the source
/// did not hold. An addition under a name the source holds (`src/extra.rs`) is an edit of the
/// declared source, whatever the clone's fate: the source tree is the same set of entries, not
/// only the same bytes. The clone is discarded, so an added byte never reaches a candidate, a
/// Proposal or a delivered tree.
fn source_edits(access: WorkerAccess, sealed: &SealedSandbox) -> Vec<String> {
    let owned = owned_top_levels(sealed);
    let added: Vec<&String> = sealed
        .mutations
        .added
        .iter()
        .filter(|path| access != WorkerAccess::ExecuteChecks || owned.contains(top_level(path)))
        .collect();
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

/// What sealing a source-writing Worker's tree captured: the Snapshot it started from, the
/// Manifest of what it left, the record of the generated paths left out, and how many paths
/// the Manifest adds, changes or removes.
struct CapturedCandidate {
    parent_snapshot_id: String,
    manifest_id: String,
    ignored_id: Option<String>,
    changed_paths: u64,
}

/// The one capture of a writer's sealed tree, for its candidate and for the partial tree an
/// Attempt wall leaves (ADR-0253) alike. A Worker with a shell leaves scratch behind (build
/// output, tool caches); it is not part of what the Worker wrote, so it never reaches the
/// tree. What the project's ignore rules exclude among what the Worker added — generated files
/// of its own test runs and builds — is left out and listed (ADR-0207).
fn capture_candidate(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    signature: &OperatorSignature,
    sealed: &SealedSandbox,
) -> Result<CapturedCandidate, String> {
    let source = &invocation.inputs["source"];
    let parent_snapshot_id = source_input(cas, source)?;
    let access = worker_access(signature);
    let owned = owned_top_levels(sealed);
    let rules = sealed.ignore_rules().map_err(|e| e.to_string())?;
    let ignored: BTreeSet<String> = sealed
        .ignored_by(&rules)
        .into_iter()
        .filter(|path| !shell_scratch(access, &owned, path))
        .collect();
    let kept = |path: &str| !shell_scratch(access, &owned, path) && !ignored.contains(path);
    let manifest = sealed
        .capture_snapshot_where(cas, kept)
        .map_err(|e| e.to_string())?;
    let manifest_id = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    read_manifest(cas, &manifest_id)?;
    let changed_paths = sealed
        .mutations
        .paths()
        .iter()
        .filter(|path| kept(path))
        .count() as u64;
    let mut ignored_id = None;
    if !ignored.is_empty() {
        let record = review_core::task::seal::SealIgnoredV1::new(
            manifest_id.clone(),
            ignored.iter().cloned().collect(),
        );
        record.validate()?;
        let id = cas
            .put_json(&serde_json::to_value(&record).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        eprintln!(
            "seal: left out {} generated path{} the ignore rules exclude ({}{}); listed in {id}",
            record.ignored_count,
            if record.ignored_count == 1 { "" } else { "s" },
            record.ignored[0],
            if record.ignored_count > 1 {
                ", …"
            } else {
                ""
            }
        );
        ignored_id = Some(id);
    }
    Ok(CapturedCandidate {
        parent_snapshot_id,
        manifest_id,
        ignored_id,
        changed_paths,
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
            WorkerAccess::ExecuteChecks
            | WorkerAccess::WriteSource
            | WorkerAccess::WriteSourceWithShell => Mode::EphemeralWrite,
        };
        // A Worker that judges the source without writing it gets the tree before the change
        // beside its own, read-only, with the change's paths (ADR-0212); a writer changes the
        // source itself and gets none.
        let judges = !signature.effects.contains("write-source");
        if judges {
            check_diff_subject(cas, invocation, signature)?;
        }
        self.materialize_mode(cas, invocation, mode, candidate_port(signature), judges)
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
        let captured = capture_candidate(cas, invocation, signature, &sealed)?;
        let parent_snapshot_id = captured.parent_snapshot_id.clone();
        let candidate = CandidateTree {
            parent_snapshot_id: parent_snapshot_id.clone(),
            manifest_id: captured.manifest_id.clone(),
        };
        let mut refs = source.artifact_ids.clone();
        refs.extend([parent_snapshot_id.clone(), captured.manifest_id.clone()]);
        refs.extend(captured.ignored_id);
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

    /// The tree a source-writing Worker left when its Attempt wall ended it, sealed exactly as
    /// `finish` seals a candidate and kept as `af/TaskPartialCandidate@1` (ADR-0253): the same
    /// scratch and ignore exclusions, a derived Snapshot of the Worker's source and the
    /// `af/SourceTree@1` that names it. A Worker without a candidate port, or one that changed
    /// nothing, leaves nothing to keep.
    fn retain_partial(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
        attempt: &PreparedTaskAttempt,
        sandbox: Sandbox,
    ) -> Result<Option<String>, String> {
        if !candidate_port(signature) {
            return Ok(None);
        }
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        let captured = capture_candidate(cas, invocation, signature, &sealed)?;
        if captured.changed_paths == 0 {
            return Ok(None);
        }
        let source = &invocation.inputs["source"];
        let producer = invocation_producer(cas, invocation, Some(attempt))?;
        let mut refs = source.artifact_ids.clone();
        refs.push(captured.manifest_id.clone());
        refs.extend(captured.ignored_id);
        let tree = review_source_git::task::derive_source_tree(
            cas,
            producer.clone(),
            &captured.manifest_id,
            &captured.parent_snapshot_id,
            refs,
        )?;
        let source_tree_id = tree.artifact_ids[0].clone();
        let partial = review_core::task::attempt_wall::TaskPartialCandidateV1 {
            schema: review_core::task::attempt_wall::TASK_PARTIAL_CANDIDATE_SCHEMA.into(),
            parent_snapshot_id: captured.parent_snapshot_id.clone(),
            snapshot_id: tree
                .snapshot_id
                .clone()
                .ok_or("The partial SourceTree names no Snapshot")?,
            source_tree_id: source_tree_id.clone(),
            changed_paths: captured.changed_paths,
        };
        partial.validate()?;
        let (id, _) = cas
            .put_artifact(
                review_core::task::attempt_wall::TASK_PARTIAL_CANDIDATE_V1,
                producer,
                vec![source_tree_id, captured.parent_snapshot_id],
                partial.snapshot_id.clone().into(),
                serde_json::to_value(&partial).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        Ok(Some(id))
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

    #[test]
    fn a_diff_subject_lists_added_changed_removed_and_mode_changed_paths_only() {
        let entry = |path: &str, bytes: &[u8], kind: EntryKind| Entry {
            path: path.into(),
            kind,
            content: review_source_git::digest_bytes(bytes),
            size: bytes.len() as u64,
        };
        let base = Manifest::new(vec![
            entry("same.rs", b"same", EntryKind::File),
            entry("changed.rs", b"before", EntryKind::File),
            entry("gone.rs", b"gone", EntryKind::File),
            entry("run.sh", b"run", EntryKind::File),
        ])
        .unwrap();
        let tree = Manifest::new(vec![
            entry("same.rs", b"same", EntryKind::File),
            entry("changed.rs", b"after", EntryKind::File),
            entry("new.rs", b"new", EntryKind::File),
            entry("run.sh", b"run", EntryKind::Executable),
        ])
        .unwrap();
        let id = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let subject = diff_subject(&id('1'), &base, &id('2'), &tree).unwrap();
        let paths: Vec<&str> = subject.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["changed.rs", "gone.rs", "new.rs", "run.sh"]);
        assert!(subject.changes[1].after.is_none() && subject.changes[2].before.is_none());
        assert_eq!(subject.changes[3].before, subject.changes[3].after);
        assert_eq!(subject.base, "../base");
    }

    /// #331 item 3 (ADR-0254): before a judging Worker starts, its `diff_subject` port holds
    /// exactly the kernel's Diff Subject, or the Attempt is refused.
    #[test]
    fn a_judging_workers_diff_subject_is_checked_before_dispatch() {
        use review_core::task::pipeline::PipelinePortV1;
        use review_source_git::task::{capture_snapshot, source_tree};
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let file = |path: &str, text: &str| Entry {
            path: path.into(),
            kind: EntryKind::File,
            content: cas.put(text.as_bytes()).unwrap(),
            size: text.len() as u64,
        };
        let base = Manifest::new(vec![file("lib.rs", "a")]).unwrap();
        let tree = Manifest::new(vec![file("lib.rs", "b"), file("new.rs", "c")]).unwrap();
        let origin = cas
            .put_json(&serde_json::json!({"schema":"af.task-source-origin/1"}))
            .unwrap();
        let base_id = capture_snapshot(&cas, &base, &origin, None).unwrap();
        let tree_id = capture_snapshot(&cas, &tree, &origin, Some(&base_id)).unwrap();
        let kernel = |node: &str, operation: &str| Producer::KernelOperation {
            run_id: "task:fixture".into(),
            node_id: Some(node.into()),
            operation_id: operation.into(),
        };
        let source = source_tree(
            &cas,
            kernel("root.inputs", "task-builtin@1"),
            &tree_id,
            vec![],
        )
        .unwrap();
        let node = "root.nodes.review.nodes.bugs";
        let mut judge = signature("read-source", "review");
        let declared = |artifact_type: &str, affinity: PortAffinityV1| -> PipelinePortV1 {
            serde_json::from_value(serde_json::json!({
                "artifact_type": artifact_type, "cardinality": "one", "optional": true,
                "affinity": affinity, "covers": []
            }))
            .unwrap()
        };
        judge.contract.inputs.insert(
            "source".into(),
            declared(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
        );
        judge.contract.inputs.insert(
            DIFF_SUBJECT_PORT.into(),
            declared(
                DIFF_SUBJECT_V1,
                PortAffinityV1::SameAs {
                    input: "source".into(),
                },
            ),
        );
        let invocation = |subject: Option<ArtifactInputV1>| TaskInvocationV1 {
            plan_id: format!("sha256:{}", "9".repeat(64)),
            node: node.into(),
            inputs: std::iter::once(("source".to_owned(), source.clone()))
                .chain(subject.map(|subject| (DIFF_SUBJECT_PORT.to_owned(), subject)))
                .collect(),
        };
        let carried = |producer: Producer, document: &DiffSubjectV1| {
            let (id, _) = cas
                .put_artifact(
                    DIFF_SUBJECT_V1,
                    producer,
                    vec![base_id.clone(), tree_id.clone()],
                    Some(tree_id.clone()),
                    serde_json::to_value(document).unwrap(),
                )
                .unwrap();
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: DIFF_SUBJECT_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: Some(tree_id.clone()),
            }
        };
        let expected = diff_subject(&base_id, &base, &tree_id, &tree).unwrap();
        // The kernel's own document for this node is admitted, and is the change.
        let admitted = check_diff_subject(
            &cas,
            &invocation(Some(carried(
                kernel(node, DIFF_SUBJECT_OPERATION),
                &expected,
            ))),
            &judge,
        )
        .unwrap()
        .unwrap();
        assert_eq!(admitted, expected);
        assert_eq!(admitted.changes.len(), 2);
        // Absent where the source has a base: refused, naming the node.
        let absent = check_diff_subject(&cas, &invocation(None), &judge).unwrap_err();
        assert!(
            absent.contains("is absent") && absent.contains(node),
            "{absent}"
        );
        // Another change, another node's, or one a Worker produced: refused.
        let mut other = expected.clone();
        other.changes.pop();
        for subject in [
            carried(kernel(node, DIFF_SUBJECT_OPERATION), &other),
            carried(
                kernel("root.nodes.other", DIFF_SUBJECT_OPERATION),
                &expected,
            ),
            carried(kernel(node, "task-builtin@1"), &expected),
        ] {
            let refused = check_diff_subject(&cas, &invocation(Some(subject)), &judge).unwrap_err();
            assert!(refused.contains("is not the change"), "{refused}");
        }
        // A Worker whose contract does not declare the port gets none.
        let undeclared = signature("read-source", "review");
        assert!(
            check_diff_subject(&cas, &invocation(None), &undeclared)
                .unwrap()
                .is_none()
        );
        let carried_anyway = check_diff_subject(
            &cas,
            &invocation(Some(carried(
                kernel(node, DIFF_SUBJECT_OPERATION),
                &expected,
            ))),
            &undeclared,
        )
        .unwrap_err();
        assert!(
            carried_anyway.contains("does not declare"),
            "{carried_anyway}"
        );
    }

    fn signature(effects: &str, roles: &str) -> OperatorSignature {
        OperatorSignature {
            unbound_allowed: Default::default(),
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
    fn only_a_review_worker_or_a_reading_author_turns_execute_checks_into_a_shell() {
        use WorkerAccess::{ExecuteChecks, ReadOnly, WriteSource, WriteSourceWithShell};
        for (effects, roles, access) in [
            ("read-source", "review", ReadOnly),
            ("", "review", ReadOnly),
            ("read-source execute-checks", "review", ExecuteChecks),
            ("execute-checks", "author review", ExecuteChecks),
            // A report author that reads source gets the review Worker's clone (ADR-0133).
            ("read-source execute-checks", "author", ExecuteChecks),
            ("execute-checks", "author", ReadOnly),
            ("read-source", "author", ReadOnly),
            ("read-source execute-checks", "implement", ReadOnly),
            ("read-source execute-checks", "", ReadOnly),
            ("write-source", "author", WriteSource),
            (
                "write-source execute-checks",
                "review",
                WriteSourceWithShell,
            ),
            (
                "write-source execute-checks",
                "implement",
                WriteSourceWithShell,
            ),
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
    fn a_shell_writer_candidate_keeps_its_edits_and_drops_its_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let sealed = sealed_after(&cas, |root| {
            // What it wrote: an edit, a new file beside the source, a new root file.
            std::fs::write(root.join("lib.rs"), "pub fn b() {}\n").unwrap();
            std::fs::write(root.join("src/extra.rs"), "pub fn c() {}\n").unwrap();
            std::fs::write(root.join("NOTES.md"), "root file\n").unwrap();
            // What its shell left: build output, a tool cache, a HOME dotfile.
            std::fs::create_dir_all(root.join("target/debug")).unwrap();
            std::fs::write(root.join("target/debug/out"), "binary\n").unwrap();
            std::fs::create_dir_all(root.join(".cache/tool")).unwrap();
            std::fs::write(root.join(".cache/tool/x"), "cache\n").unwrap();
            std::fs::write(root.join(".claude.json"), "{}\n").unwrap();
        });
        let owned = owned_top_levels(&sealed);
        let paths = |access: WorkerAccess| -> Vec<String> {
            sealed
                .capture_snapshot_where(&cas, |path| !shell_scratch(access, &owned, path))
                .unwrap()
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect()
        };
        assert_eq!(
            paths(WorkerAccess::WriteSourceWithShell),
            ["NOTES.md", "lib.rs", "src/extra.rs", "src/main.rs"]
        );
        // A writer without a shell keeps its old contract: the whole sealed tree.
        assert_eq!(paths(WorkerAccess::WriteSource).len(), 7);
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
        // An addition under a name the source holds is an edit of the declared source; one
        // beside the source (`NOTES.md`, `target/`) is scratch the clone takes with it.
        assert_eq!(
            source_edits(WorkerAccess::ExecuteChecks, &edited),
            ["lib.rs", "src/extra.rs", "src/main.rs"]
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
