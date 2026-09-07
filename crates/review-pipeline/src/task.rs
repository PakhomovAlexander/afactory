//! The Task kernel: v2 sequential implementation, composed beside [`Kernel`](crate::Kernel).
//!
//! `af task start` captures the source Snapshot and resolves Task authority in the CLI, exactly
//! as `af review run` does before handing a Campaign to `Kernel`. Everything from the first
//! durable Task event to the terminal outcome is composed here: the implementer sandbox, the
//! sealed derived Snapshot and its ceiling, the acceptance Gates, the independent evaluator, and
//! every event the Task log records. The CLI keeps argument parsing, state resolution, and
//! presentation ([ADR-0046](../../../docs/adr/0046-record-tasks-in-a-distinct-typed-task-log.md)).
//!
//! Two bounds are deliberate and load-bearing. The evaluator never receives the complete
//! mutation list — it gets [`mutation_summary`] plus the artifact that holds the rest, because
//! the full list is already durable and the evaluator can inspect its read-only sandbox. And a
//! derived Snapshot is refused before any of its bytes are published when it exceeds the
//! [`DerivedSize`] ceilings, so build output cannot become permanent state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_check::{CheckDefinition, CheckResult, CheckRunner, CheckStatus};
use review_core::event::{TaskEventType, task_artifact};
use review_runner::{ContextManifest, ModelRunner, ResolvedReviewer, TokenUsage, extract_result};
use review_sandbox::{Mode, MutationSet, Sandbox, SandboxTemplate};
use review_source_git::Manifest;
use review_store::{Cas, CasError};
use serde::{Deserialize, Serialize};

use crate::mutations::mutation_summary;

/// Ceiling on what one implementer Attempt may add or modify relative to its source Snapshot,
/// counted over the sealed sandbox's entries. This is the bound that refuses build output: a
/// `cargo check` of this workspace leaves ~10k files and ~1 GiB under `target/`, while a
/// plausible source change touches at most hundreds of files and a few megabytes. The limit
/// leaves room for a generated fixture corpus without letting a compiler's cache through.
pub const MAX_DERIVED_MUTATION_ENTRIES_V1: u64 = 4_096;
/// Byte counterpart of [`MAX_DERIVED_MUTATION_ENTRIES_V1`]: the sizes of every added or modified
/// entry, summed.
pub const MAX_DERIVED_MUTATION_BYTES_V1: u64 = 256 * 1024 * 1024;
/// Ceiling on the whole derived tree, the same numbers a Cache Snapshot is held to
/// (`MAX_CACHE_ENTRIES_V1`, `MAX_CACHE_BYTES_V1`). A source Snapshot larger than this cannot
/// produce a derived Snapshot at all; the refusal names the limit.
pub const MAX_DERIVED_SNAPSHOT_ENTRIES_V1: u64 = 250_000;
/// Byte counterpart of [`MAX_DERIVED_SNAPSHOT_ENTRIES_V1`].
pub const MAX_DERIVED_SNAPSHOT_BYTES_V1: u64 = 4 * 1024 * 1024 * 1024;

/// Prompt-side records. They are Worker Input, not log contracts, so they carry a `schema`
/// marker for the context manifest but no event references them.
pub const WORKER_PACKAGE_V1: &str = "af/worker-package@1";
pub const IMPLEMENT_INPUT_V1: &str = "af/implement-input@1";
pub const EVALUATE_INPUT_V1: &str = "af/evaluate-input@1";

/// Where Task events land. The one implementation is the CLI's SQLite Task log. An
/// implementation must call [`admit_task_artifact`] before its row becomes durable, so the log
/// can never reference bytes the CAS does not hold — the same enforcement point the Campaign
/// log's `EventStore::append` is.
pub trait TaskLog {
    fn append(
        &mut self,
        cas: &Cas,
        task_id: &str,
        event_type: TaskEventType,
        artifact_id: &str,
    ) -> Result<(), String>;
}

/// Refuse a Task event whose artifact the CAS does not hold, and — where the event type
/// declares one — whose artifact does not carry the `schema` marker that type references.
///
/// This reuses the exact CAS operations `EventStore::append` performs for a Campaign event:
/// the object is re-verified and scheduled for the publication barrier, so a log row can only
/// follow bytes that are durable, never precede them.
pub fn admit_task_artifact(
    cas: &Cas,
    event_type: TaskEventType,
    artifact_id: &str,
) -> Result<(), String> {
    let dangling = |error: CasError| match error {
        CasError::NotFound { .. } | CasError::InvalidDigest(_) => {
            format!("{event_type} references an artifact that is not durable: {artifact_id}")
        }
        other => format!("{event_type} artifact {artifact_id} failed verification: {other}"),
    };
    let Some(schema) = event_type.artifact_schema() else {
        return cas.prepare_for_publication(artifact_id).map_err(dangling);
    };
    let value = cas
        .get_json_for_publication(artifact_id)
        .map_err(dangling)?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(schema) {
        return Err(format!(
            "{event_type} must reference an `{schema}` artifact: {artifact_id}"
        ));
    }
    Ok(())
}

/// A Task Snapshot receipt (`af/snapshot@1`), referenced by ID from Task records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotReceipt {
    pub schema: String,
    pub kind: String,
    pub content_digest: String,
    pub manifest_artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_snapshot_id: Option<String>,
    pub repository_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

/// One digest-pinned Worker package, published to the CAS before any Worker runs.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerAuthority {
    pub role: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub package_artifact_id: String,
}

/// What one Worker Attempt cost and exactly what it was shown (`af/worker-evidence@1`).
#[derive(Debug, Clone, Serialize)]
pub struct WorkerEvidence {
    pub schema: String,
    pub role: String,
    pub package_artifact_id: String,
    pub raw_artifact: String,
    pub usage: TokenUsage,
    pub context_manifest: ContextManifest,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateEvidence {
    pub name: String,
    pub status: CheckStatus,
    pub result_artifact: String,
}

/// Measured size of a derived Snapshot. `entries`/`bytes` describe the whole sealed tree;
/// `mutated_*` count what the implementer added or modified, which is what the ceiling bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedSize {
    pub entries: u64,
    pub bytes: u64,
    pub mutated_entries: u64,
    pub mutated_bytes: u64,
}

impl DerivedSize {
    /// Measure a sealed tree against its mutation set. Added entries carry their size from the
    /// seal scan even though their bytes were never hashed, so no file is read here.
    pub fn measure(final_manifest: &Manifest, mutations: &MutationSet) -> DerivedSize {
        let mutated = |path: &str| {
            mutations
                .added
                .binary_search_by(|added| added.as_str().cmp(path))
                .is_ok()
                || mutations
                    .modified
                    .binary_search_by(|modified| modified.as_str().cmp(path))
                    .is_ok()
        };
        let mut size = DerivedSize {
            entries: final_manifest.entries.len() as u64,
            bytes: 0,
            mutated_entries: 0,
            mutated_bytes: 0,
        };
        for entry in &final_manifest.entries {
            size.bytes = size.bytes.saturating_add(entry.size);
            if mutated(&entry.path) {
                size.mutated_entries += 1;
                size.mutated_bytes = size.mutated_bytes.saturating_add(entry.size);
            }
        }
        size
    }

    /// The ceiling, checked before a single byte of the derived tree is published. The message
    /// names the limit that refused it.
    pub fn check_ceiling(&self) -> Result<(), String> {
        let limits = [
            (
                self.mutated_entries,
                MAX_DERIVED_MUTATION_ENTRIES_V1,
                "MAX_DERIVED_MUTATION_ENTRIES_V1",
                "added or modified entries",
            ),
            (
                self.mutated_bytes,
                MAX_DERIVED_MUTATION_BYTES_V1,
                "MAX_DERIVED_MUTATION_BYTES_V1",
                "added or modified bytes",
            ),
            (
                self.entries,
                MAX_DERIVED_SNAPSHOT_ENTRIES_V1,
                "MAX_DERIVED_SNAPSHOT_ENTRIES_V1",
                "entries",
            ),
            (
                self.bytes,
                MAX_DERIVED_SNAPSHOT_BYTES_V1,
                "MAX_DERIVED_SNAPSHOT_BYTES_V1",
                "bytes",
            ),
        ];
        for (actual, limit, name, what) in limits {
            if actual > limit {
                return Err(format!(
                    "derived Snapshot exceeds the kernel ceiling {name} = {limit}: {actual} {what}; \
                     build output must not be left in the sandbox"
                ));
            }
        }
        Ok(())
    }
}

/// The implement pipeline as the kernel needs it: the CLI parses and validates the TOML and
/// hands over the resolved values.
#[derive(Debug, Clone)]
pub struct TaskPipelineSpec {
    pub worker_timeout: Duration,
    pub check_timeout: Duration,
    pub attempt_tokens: u64,
    pub run_tokens: u64,
    pub checks: Vec<CheckDefinition>,
}

/// Resolved Task authority: exact pipeline, lock, project, and both Worker packages, every one
/// already durable in the CAS.
#[derive(Debug, Clone)]
pub struct TaskAuthority {
    pub pipeline: TaskPipelineSpec,
    pub pipeline_artifact_id: String,
    pub lock_artifact_id: String,
    pub project_artifact_id: String,
    pub implementer: ResolvedReviewer,
    pub evaluator: ResolvedReviewer,
    pub implementer_authority: WorkerAuthority,
    pub evaluator_authority: WorkerAuthority,
}

/// The captured source Snapshot the Task starts from.
#[derive(Debug, Clone)]
pub struct TaskSource {
    pub snapshot_id: String,
    pub manifest: Manifest,
    pub repository_id: String,
}

/// The exact `af` that runs the Task, recorded in its outcome.
#[derive(Debug, Clone)]
pub struct TaskCandidate {
    pub version: String,
    pub executable: String,
    pub binary_sha256: String,
}

/// Everything a Task needs before its first event.
#[derive(Debug, Clone)]
pub struct TaskInputs {
    pub task_id: String,
    pub goal: String,
    pub goal_artifact_id: String,
    pub source: TaskSource,
    pub authority: TaskAuthority,
    pub candidate: TaskCandidate,
}

/// The terminal result of one Task run, after `TaskCompleted@1` is durable.
#[derive(Debug, Clone)]
pub struct TaskReport {
    pub task_id: String,
    pub verified: bool,
    pub derived_snapshot_id: Option<String>,
    pub chargeable_tokens: u64,
    /// The `af/task-outcome@1` record exactly as persisted.
    pub outcome: serde_json::Value,
}

#[derive(Debug, Clone)]
struct Derived {
    snapshot_id: String,
    size: DerivedSize,
}

#[derive(Default)]
struct Progress {
    derived: Option<Derived>,
    workers: Vec<WorkerEvidence>,
    gates: Vec<GateEvidence>,
}

pub struct TaskKernel<'a> {
    cas: &'a Cas,
    log: &'a mut dyn TaskLog,
    inputs: TaskInputs,
    worker_timeout: Option<Duration>,
    observer: &'a (dyn Fn(&str) + 'a),
}

fn silent(_: &str) {}

impl<'a> TaskKernel<'a> {
    pub fn new(cas: &'a Cas, log: &'a mut dyn TaskLog, inputs: TaskInputs) -> TaskKernel<'a> {
        TaskKernel {
            cas,
            log,
            inputs,
            worker_timeout: None,
            observer: &silent,
        }
    }

    /// Override the pipeline's Worker timeout for both Workers.
    pub fn with_worker_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.worker_timeout = timeout;
        self
    }

    /// Receive one progress line per durable milestone (derived Snapshot, each Gate).
    pub fn with_observer(mut self, observer: &'a (dyn Fn(&str) + 'a)) -> Self {
        self.observer = observer;
        self
    }

    fn worker_timeout(&self) -> Duration {
        self.worker_timeout
            .unwrap_or(self.inputs.authority.pipeline.worker_timeout)
    }

    fn append(&mut self, event_type: TaskEventType, artifact_id: &str) -> Result<(), String> {
        self.log
            .append(self.cas, &self.inputs.task_id, event_type, artifact_id)
    }

    /// Run the Task to its terminal outcome. `Ok` means `TaskCompleted@1` is durable, verified
    /// or not; `Err` is an infrastructure failure that left no terminal record.
    pub fn run(mut self) -> Result<TaskReport, String> {
        let cas = self.cas;
        let opened = cas
            .put_json(&serde_json::json!({
                "schema": task_artifact::TASK_OPENED_V1,
                "task_id": self.inputs.task_id,
                "kind": "implement",
                "goal_artifact_id": self.inputs.goal_artifact_id,
                "source_snapshot_id": self.inputs.source.snapshot_id,
                "pipeline_artifact_id": self.inputs.authority.pipeline_artifact_id,
                "lock_artifact_id": self.inputs.authority.lock_artifact_id,
                "project_artifact_id": self.inputs.authority.project_artifact_id,
                "workers": [
                    &self.inputs.authority.implementer_authority,
                    &self.inputs.authority.evaluator_authority,
                ],
            }))
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::TaskOpenedV1, &opened)?;
        let mut progress = Progress::default();

        let implement_sandbox = {
            let template = SandboxTemplate::materialize(&self.inputs.source.manifest, cas)
                .map_err(|error| error.to_string())?;
            Sandbox::from_template(&template, Mode::EphemeralWrite)
                .map_err(|error| error.to_string())?
        };
        let implement_input = serde_json::json!({
            "schema": IMPLEMENT_INPUT_V1,
            "task_id": self.inputs.task_id,
            "goal": self.inputs.goal,
            "source_snapshot_id": self.inputs.source.snapshot_id,
            "constraints": [
                "Edit only the provided sandbox.",
                "Leave the sandbox in the complete state that should be evaluated.",
                "Do not publish, push, create a branch, or write back to the source checkout."
            ],
            "budget": {"reserved_tokens": self.inputs.authority.pipeline.attempt_tokens},
        });
        let implement_evidence = match invoke_worker(
            "implementer",
            &self.inputs.authority.implementer,
            &self.inputs.authority.implementer_authority,
            implement_sandbox.root(),
            cas,
            &implement_input,
            self.worker_timeout(),
        ) {
            Ok(evidence) => evidence,
            Err(reason) => return self.unverified(progress, "implementer", &reason),
        };
        let implement_event = cas
            .put_json(
                &serde_json::to_value(&implement_evidence).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::WorkerCompletedV1, &implement_event)?;
        let implement_tokens = implement_evidence.usage.chargeable_tokens;
        progress.workers.push(implement_evidence);
        let pipeline = &self.inputs.authority.pipeline;
        if implement_tokens > pipeline.attempt_tokens || implement_tokens > pipeline.run_tokens {
            return self.unverified(
                progress,
                "budget",
                "implementer exceeded the admitted token budget",
            );
        }

        let sealed = implement_sandbox
            .seal()
            .map_err(|error| format!("sealing implementer sandbox: {error}"))?;
        // The ceiling is checked on the seal scan's metadata, before capture publishes a byte.
        let size = DerivedSize::measure(&sealed.final_manifest, &sealed.mutations);
        if let Err(reason) = size.check_ceiling() {
            return self.unverified(progress, "snapshot", &reason);
        }
        let derived_manifest = sealed
            .capture_snapshot(cas)
            .map_err(|error| format!("capturing derived Snapshot: {error}"))?;
        let derived_manifest_id = put_manifest(cas, &derived_manifest)?;
        let derived_snapshot_id = cas
            .put_json(
                &serde_json::to_value(SnapshotReceipt {
                    schema: task_artifact::SNAPSHOT_V1.into(),
                    kind: "derived".into(),
                    content_digest: derived_manifest.content_digest(),
                    manifest_artifact_id: derived_manifest_id,
                    parent_snapshot_id: Some(self.inputs.source.snapshot_id.clone()),
                    repository_id: self.inputs.source.repository_id.clone(),
                    source_revision: None,
                })
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        verify_snapshot(cas, &derived_manifest, &derived_snapshot_id)?;
        // The complete mutation lists live here, once. Everything downstream carries the
        // bounded summary that names this artifact.
        let snapshot_event = cas
            .put_json(&serde_json::json!({
                "schema": task_artifact::DERIVED_SNAPSHOT_V1,
                "snapshot_id": derived_snapshot_id,
                "mutations": {
                    "added": sealed.mutations.added,
                    "modified": sealed.mutations.modified,
                    "deleted": sealed.mutations.deleted,
                },
                "size": size,
            }))
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::SnapshotDerivedV1, &snapshot_event)?;
        let mutations = mutation_summary(&sealed.mutations, &snapshot_event);
        drop(sealed);
        progress.derived = Some(Derived {
            snapshot_id: derived_snapshot_id.clone(),
            size,
        });
        (self.observer)(&format!("derived  {derived_snapshot_id}"));

        // Materialized once; every Gate and the evaluator receive a copy-on-write clone.
        let derived_template = SandboxTemplate::materialize(&derived_manifest, cas)
            .map_err(|error| error.to_string())?;
        let checks = self.inputs.authority.pipeline.checks.clone();
        let check_timeout = self.inputs.authority.pipeline.check_timeout;
        for definition in &checks {
            let gate_sandbox = Sandbox::from_template(&derived_template, Mode::ReadOnly)
                .map_err(|error| error.to_string())?;
            // Build systems may write caches, but acceptance gates may not mutate the Snapshot.
            // Give them an external disposable target directory instead of weakening read-only
            // mode.
            let gate_scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
            let mut result = CheckRunner::new(cas, gate_sandbox.root())
                .with_env(
                    "CARGO_TARGET_DIR",
                    gate_scratch
                        .path()
                        .join("cargo-target")
                        .display()
                        .to_string(),
                )
                .with_timeout(check_timeout)
                .run(definition);
            let gate_sealed = gate_sandbox.seal().map_err(|error| error.to_string())?;
            if !gate_sealed.unchanged() {
                result = mutated_gate_result(result, gate_sealed.mutations.paths());
            }
            let result_artifact = cas
                .put_json(&serde_json::to_value(&result).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            progress.gates.push(GateEvidence {
                name: result.name.clone(),
                status: result.status,
                result_artifact: result_artifact.clone(),
            });
            self.append(TaskEventType::GateCompletedV1, &result_artifact)?;
            (self.observer)(&format!("gate     {} -> {:?}", result.name, result.status));
        }
        if progress
            .gates
            .iter()
            .any(|gate| gate.status != CheckStatus::Passed)
        {
            return self.unverified(
                progress,
                "gates",
                "one or more required acceptance gates did not pass",
            );
        }
        let pipeline = &self.inputs.authority.pipeline;
        if pipeline.run_tokens.saturating_sub(implement_tokens) < pipeline.attempt_tokens {
            return self.unverified(
                progress,
                "budget",
                "remaining run budget cannot reserve the evaluator attempt",
            );
        }

        let evaluator_sandbox = Sandbox::from_template(&derived_template, Mode::ReadOnly)
            .map_err(|error| error.to_string())?;
        drop(derived_template);
        let evaluation_input = EvaluationInput {
            task_id: &self.inputs.task_id,
            goal: &self.inputs.goal,
            source_snapshot_id: &self.inputs.source.snapshot_id,
            derived_snapshot_id: &derived_snapshot_id,
            mutations: &mutations,
            gates: &progress.gates,
            reserved_tokens: pipeline.attempt_tokens,
        }
        .render();
        let evaluator_evidence = match invoke_worker(
            "evaluator",
            &self.inputs.authority.evaluator,
            &self.inputs.authority.evaluator_authority,
            evaluator_sandbox.root(),
            cas,
            &evaluation_input,
            self.worker_timeout(),
        ) {
            Ok(evidence) => evidence,
            Err(reason) => return self.unverified(progress, "evaluator", &reason),
        };
        let evaluator_sealed = evaluator_sandbox
            .seal()
            .map_err(|error| error.to_string())?;
        if !evaluator_sealed.unchanged() {
            progress.workers.push(evaluator_evidence);
            return self.unverified(
                progress,
                "evaluator",
                "evaluator mutated its read-only Snapshot",
            );
        }
        let evaluator_event = cas
            .put_json(
                &serde_json::to_value(&evaluator_evidence).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::WorkerCompletedV1, &evaluator_event)?;
        let evaluator_tokens = evaluator_evidence.usage.chargeable_tokens;
        let raw_artifact = evaluator_evidence.raw_artifact.clone();
        progress.workers.push(evaluator_evidence);
        let pipeline = &self.inputs.authority.pipeline;
        if evaluator_tokens > pipeline.attempt_tokens
            || implement_tokens.saturating_add(evaluator_tokens) > pipeline.run_tokens
        {
            return self.unverified(
                progress,
                "budget",
                "evaluator exceeded the admitted token budget",
            );
        }
        let raw = cas.get(&raw_artifact).map_err(|error| error.to_string())?;
        let answer = match worker_answer(&self.inputs.authority.evaluator.runner.program, &raw) {
            Ok(answer) => answer,
            Err(reason) => return self.unverified(progress, "evaluation", &reason),
        };
        let evaluation: Evaluation = match serde_json::from_str(extract_result(&answer)) {
            Ok(evaluation) => evaluation,
            Err(error) => {
                return self.unverified(
                    progress,
                    "evaluation",
                    &format!("evaluator returned malformed verdict: {error}"),
                );
            }
        };
        let evaluation_artifact = cas
            .put_json(&serde_json::json!({
                "schema": task_artifact::TASK_EVALUATION_V1,
                "verdict": evaluation.verdict,
                "summary": evaluation.summary,
            }))
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::EvaluationCompletedV1, &evaluation_artifact)?;
        if evaluation.verdict == EvaluationVerdict::Reject {
            return self.unverified(
                progress,
                "evaluation",
                "independent evaluator rejected the derived Snapshot",
            );
        }
        self.conclude(
            progress,
            serde_json::json!({"kind": "verified", "snapshot_id": derived_snapshot_id}),
        )
    }

    fn unverified(
        &mut self,
        progress: Progress,
        stage: &str,
        reason: &str,
    ) -> Result<TaskReport, String> {
        self.conclude(
            progress,
            serde_json::json!({"kind": "unverified", "stage": stage, "reason": reason}),
        )
    }

    fn conclude(
        &mut self,
        progress: Progress,
        outcome: serde_json::Value,
    ) -> Result<TaskReport, String> {
        let workers = &progress.workers;
        let chargeable_tokens = workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(worker.usage.chargeable_tokens)
        });
        let rendered_bytes = workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(worker.context_manifest.rendered_bytes)
        });
        let estimated_context_tokens = workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(worker.context_manifest.estimated_tokens)
        });
        let derived_snapshot_id = progress
            .derived
            .as_ref()
            .map(|derived| derived.snapshot_id.clone());
        let result = serde_json::json!({
            "schema": task_artifact::TASK_OUTCOME_V1,
            "candidate": {
                "version": self.inputs.candidate.version,
                "executable": self.inputs.candidate.executable,
                "binary_sha256": self.inputs.candidate.binary_sha256,
            },
            "task_id": self.inputs.task_id,
            "kind": "implement",
            "goal_artifact_id": self.inputs.goal_artifact_id,
            "source_snapshot_id": self.inputs.source.snapshot_id,
            "derived_snapshot_id": derived_snapshot_id,
            "derived_snapshot_size": progress.derived.as_ref().map(|derived| derived.size),
            "authority": {
                "pipeline_artifact_id": self.inputs.authority.pipeline_artifact_id,
                "lock_artifact_id": self.inputs.authority.lock_artifact_id,
                "project_artifact_id": self.inputs.authority.project_artifact_id,
                "workers": [
                    &self.inputs.authority.implementer_authority,
                    &self.inputs.authority.evaluator_authority,
                ],
            },
            "workers": workers,
            "gates": progress.gates,
            "totals": {
                "context": {
                    "rendered_bytes": rendered_bytes,
                    "estimated_tokens": estimated_context_tokens,
                },
                "usage": aggregate_usage(workers),
            },
            "delivery": {"kind": "none", "reason": "v2 ends at an internal Snapshot"},
            "outcome": outcome,
        });
        let result_artifact = self
            .cas
            .put_json(&result)
            .map_err(|error| error.to_string())?;
        self.append(TaskEventType::TaskCompletedV1, &result_artifact)?;
        Ok(TaskReport {
            task_id: self.inputs.task_id.clone(),
            verified: result["outcome"]["kind"] == "verified",
            derived_snapshot_id,
            chargeable_tokens,
            outcome: result,
        })
    }
}

/// The evaluator's exact Task input. `mutations` is the bounded [`mutation_summary`] of the
/// sealed implementer sandbox — never the complete path lists, which are durable once in the
/// `af/derived-snapshot@1` artifact the summary names.
pub struct EvaluationInput<'a> {
    pub task_id: &'a str,
    pub goal: &'a str,
    pub source_snapshot_id: &'a str,
    pub derived_snapshot_id: &'a str,
    pub mutations: &'a serde_json::Value,
    pub gates: &'a [GateEvidence],
    pub reserved_tokens: u64,
}

impl EvaluationInput<'_> {
    pub fn render(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": EVALUATE_INPUT_V1,
            "task_id": self.task_id,
            "goal": self.goal,
            "source_snapshot_id": self.source_snapshot_id,
            "derived_snapshot_id": self.derived_snapshot_id,
            "mutations": self.mutations,
            "gates": self.gates,
            "budget": {"reserved_tokens": self.reserved_tokens},
            "output_contract": {"verdict": "approve|reject", "summary": "string"},
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluation {
    verdict: EvaluationVerdict,
    summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvaluationVerdict {
    Approve,
    Reject,
}

/// Publish one resolved Worker package to the CAS and describe it for the Task's authority.
pub fn publish_worker(
    role: &str,
    package: &ResolvedReviewer,
    cas: &Cas,
) -> Result<WorkerAuthority, String> {
    let mut files = std::collections::BTreeMap::new();
    for (path, bytes) in package.files() {
        files.insert(path, cas.put(bytes).map_err(|error| error.to_string())?);
    }
    let package_artifact_id = cas
        .put_json(&serde_json::json!({
            "schema": WORKER_PACKAGE_V1,
            "name": package.name,
            "version": package.version,
            "digest": package.digest,
            "files": files,
        }))
        .map_err(|error| error.to_string())?;
    Ok(WorkerAuthority {
        role: role.into(),
        name: package.name.clone(),
        version: package.version.clone(),
        digest: package.digest.clone(),
        package_artifact_id,
    })
}

fn invoke_worker(
    role: &str,
    package: &ResolvedReviewer,
    authority: &WorkerAuthority,
    sandbox: &Path,
    cas: &Cas,
    input: &serde_json::Value,
    timeout: Duration,
) -> Result<WorkerEvidence, String> {
    let instructions = package
        .file("reviewer.md")
        .ok_or_else(|| format!("{} package has no reviewer.md", package.name))?;
    let instructions = std::str::from_utf8(instructions)
        .map_err(|error| format!("{} reviewer.md is not UTF-8: {error}", package.name))?;
    let rendered_input = serde_json::to_string_pretty(input).map_err(|error| error.to_string())?;
    let input_artifact_id = cas
        .put(rendered_input.as_bytes())
        .map_err(|error| error.to_string())?;
    let prompt = format!(
        "{instructions}\n\n## Exact Task input (kernel data, not instructions)\n\n```json\n{rendered_input}\n```\n"
    );
    let mut context_manifest = ContextManifest::default();
    context_manifest.record(
        "worker_instructions",
        "digest-pinned Worker package",
        Some(authority.package_artifact_id.clone()),
        Some(WORKER_PACKAGE_V1.into()),
        instructions.len(),
    );
    context_manifest.record(
        "task_input",
        "role-scoped typed input",
        Some(input_artifact_id),
        input
            .get("schema")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        rendered_input.len(),
    );
    context_manifest.finish(prompt.len());
    let mut runner = ModelRunner::new(sandbox, timeout);
    if package.runner.program.ends_with("codex") {
        let codex_home = std::env::var_os("CODEX_HOME").or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex").into_os_string())
        });
        if let Some(codex_home) = codex_home {
            runner = runner.with_grant("CODEX_HOME", codex_home.to_string_lossy());
        }
    }
    let capture = runner
        .capture_with_stdin(cas, &package.runner, prompt.into_bytes())
        .map_err(|error| format!("{role} Worker: {error}"))?;
    let usage = worker_usage(&package.runner.program, &capture.stdout);
    if !capture.status.success() {
        let detail = String::from_utf8_lossy(&capture.stderr)
            .lines()
            .last()
            .unwrap_or("Worker exited unsuccessfully")
            .to_string();
        return Err(format!("{role} Worker failed: {detail}"));
    }
    Ok(WorkerEvidence {
        schema: task_artifact::WORKER_EVIDENCE_V1.into(),
        role: role.into(),
        package_artifact_id: authority.package_artifact_id.clone(),
        raw_artifact: capture.raw_artifact,
        usage,
        context_manifest,
    })
}

fn worker_usage(program: &str, stdout: &[u8]) -> TokenUsage {
    if program.ends_with("codex") {
        let mut usage = TokenUsage::default();
        for line in stdout.split(|byte| *byte == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) != Some("turn.completed") {
                continue;
            }
            let Some(receipt) = value.get("usage") else {
                continue;
            };
            let count = |name: &str| {
                receipt
                    .get(name)
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            };
            let input = count("input_tokens");
            let output = count("output_tokens");
            let cache = count("cached_input_tokens");
            add_usage(&mut usage.input_tokens, input);
            add_usage(&mut usage.output_tokens, output);
            add_usage(&mut usage.cache_read_tokens, cache);
            add_usage(
                &mut usage.cache_write_tokens,
                count("cache_write_input_tokens"),
            );
            add_usage(
                &mut usage.reasoning_tokens,
                count("reasoning_output_tokens"),
            );
            usage.chargeable_tokens = usage
                .chargeable_tokens
                .saturating_add(input.saturating_sub(cache).saturating_add(output));
        }
        return usage;
    }
    if program.ends_with("claude")
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(stdout)
        && let Some(receipt) = value.get("usage")
    {
        let count = |name: &str| {
            receipt
                .get(name)
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        };
        let input = count("input_tokens");
        let output = count("output_tokens");
        let cache_write = count("cache_creation_input_tokens");
        return TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_tokens: Some(count("cache_read_input_tokens")),
            cache_write_tokens: Some(cache_write),
            reasoning_tokens: None,
            chargeable_tokens: input.saturating_add(output).saturating_add(cache_write),
        };
    }
    TokenUsage::default()
}

fn add_usage(total: &mut Option<u64>, amount: u64) {
    *total = Some(total.unwrap_or(0).saturating_add(amount));
}

fn worker_answer(program: &str, stdout: &[u8]) -> Result<String, String> {
    if program.ends_with("codex") {
        let mut answer = None;
        for line in stdout.split(|byte| *byte == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) == Some("item.completed")
                && value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(|value| value.as_str())
                    == Some("agent_message")
            {
                answer = value
                    .get("item")
                    .and_then(|item| item.get("text"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
        }
        return answer.ok_or("evaluator produced no final Codex message".into());
    }
    if program.ends_with("claude") {
        let value: serde_json::Value =
            serde_json::from_slice(stdout).map_err(|error| error.to_string())?;
        return value
            .get("result")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or("evaluator produced no Claude result".into());
    }
    String::from_utf8(stdout.to_vec()).map_err(|error| error.to_string())
}

fn mutated_gate_result(result: CheckResult, paths: Vec<String>) -> CheckResult {
    CheckResult {
        status: CheckStatus::NotRun,
        exit_code: None,
        reason: Some(format!(
            "read-only gate mutated its Snapshot: {} paths",
            paths.len()
        )),
        ..result
    }
}

fn aggregate_usage(workers: &[WorkerEvidence]) -> TokenUsage {
    fn sum(workers: &[WorkerEvidence], select: impl Fn(&TokenUsage) -> Option<u64>) -> Option<u64> {
        workers
            .iter()
            .filter_map(|worker| select(&worker.usage))
            .reduce(u64::saturating_add)
    }
    TokenUsage {
        input_tokens: sum(workers, |usage| usage.input_tokens),
        output_tokens: sum(workers, |usage| usage.output_tokens),
        cache_read_tokens: sum(workers, |usage| usage.cache_read_tokens),
        cache_write_tokens: sum(workers, |usage| usage.cache_write_tokens),
        reasoning_tokens: sum(workers, |usage| usage.reasoning_tokens),
        chargeable_tokens: workers.iter().fold(0_u64, |total, worker| {
            total.saturating_add(worker.usage.chargeable_tokens)
        }),
    }
}

/// Publish a Manifest as one JSON artifact.
pub fn put_manifest(cas: &Cas, manifest: &Manifest) -> Result<String, String> {
    cas.put_json(&serde_json::to_value(manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

/// Read a Task Snapshot receipt by ID.
pub fn load_snapshot(cas: &Cas, snapshot_id: &str) -> Result<SnapshotReceipt, String> {
    serde_json::from_value(
        cas.get_json(snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("reading Snapshot {snapshot_id}: {error}"))
}

/// Read and validate the Manifest a Snapshot receipt names.
pub fn load_manifest(cas: &Cas, snapshot: &SnapshotReceipt) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(&snapshot.manifest_artifact_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("reading Snapshot Manifest: {error}"))?;
    manifest.validate().map_err(|error| error.to_string())?;
    Ok(manifest)
}

/// The distinct content digests a Manifest references, sorted. Symlink entries name their
/// target bytes exactly as regular files do, so they verify the same way.
pub fn distinct_contents(manifest: &Manifest) -> Vec<&str> {
    manifest
        .entries
        .iter()
        .map(|entry| entry.content.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Re-verify a Snapshot receipt and every distinct object its Manifest names — once per digest,
/// on the shared bounded executor. The pass guards objects inherited from an earlier process;
/// it never has to prove the same bytes twice.
pub fn verify_snapshot(cas: &Cas, manifest: &Manifest, snapshot_id: &str) -> Result<(), String> {
    cas.verify(snapshot_id).map_err(|error| error.to_string())?;
    let distinct = distinct_contents(manifest);
    review_parallel::try_for_each(&distinct, |digest| {
        cas.verify(digest)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_source_git::{Entry, EntryKind};

    fn entry(path: &str, content: &str, size: u64) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            content: content.into(),
            size,
        }
    }

    #[test]
    fn evaluator_input_stays_bounded_for_thousands_of_mutations() {
        let mutations = MutationSet {
            added: (0..10_000)
                .map(|index| format!("target/debug/deps/libreview_{index:05}.rmeta"))
                .collect(),
            modified: vec!["crates/review-core/src/lib.rs".into()],
            deleted: Vec::new(),
        };
        let summary = mutation_summary(&mutations, "sha256:derived-record");
        let gates = vec![GateEvidence {
            name: "kernel".into(),
            status: CheckStatus::Passed,
            result_artifact: "sha256:gate".into(),
        }];
        let input = EvaluationInput {
            task_id: "task-0123456789abcdef0123",
            goal: "make the thing",
            source_snapshot_id: "sha256:source",
            derived_snapshot_id: "sha256:derived",
            mutations: &summary,
            gates: &gates,
            reserved_tokens: 300_000,
        }
        .render();
        let rendered = serde_json::to_string_pretty(&input).unwrap();
        assert!(
            rendered.len() < 4 * 1024,
            "evaluator input grew with the mutation set: {} bytes",
            rendered.len()
        );
        assert_eq!(input["mutations"]["count"], 10_001);
        assert_eq!(input["mutations"]["added"], 10_000);
        assert_eq!(
            input["mutations"]["sample"].as_array().unwrap().len(),
            crate::mutations::MUTATION_SAMPLE
        );
        assert_eq!(input["mutations"]["truncated"], true);
        assert_eq!(input["mutations"]["artifact"], "sha256:derived-record");
        assert!(input["mutations"].get("deleted").is_some());
        assert!(input.get("implementer_output").is_none());
    }

    #[test]
    fn derived_size_measures_only_what_the_implementer_touched() {
        let manifest = Manifest::new(vec![
            entry("README.md", "sha256:readme", 10),
            entry("src/lib.rs", "sha256:lib", 200),
            entry("src/new.rs", "sha256:new", 300),
            entry("target/debug/a.o", "sha256:a", 1_000),
        ])
        .unwrap();
        let mutations = MutationSet {
            added: vec!["src/new.rs".into(), "target/debug/a.o".into()],
            modified: vec!["src/lib.rs".into()],
            deleted: vec!["src/old.rs".into()],
        };
        let size = DerivedSize::measure(&manifest, &mutations);
        assert_eq!(
            size,
            DerivedSize {
                entries: 4,
                bytes: 1_510,
                mutated_entries: 3,
                mutated_bytes: 1_500,
            }
        );
        assert!(size.check_ceiling().is_ok());
    }

    #[test]
    fn ceiling_refusal_names_the_limit_it_applied() {
        let too_many = DerivedSize {
            entries: 5_000,
            bytes: 10,
            mutated_entries: MAX_DERIVED_MUTATION_ENTRIES_V1 + 1,
            mutated_bytes: 10,
        };
        let reason = too_many.check_ceiling().unwrap_err();
        assert!(
            reason.contains("MAX_DERIVED_MUTATION_ENTRIES_V1 = 4096"),
            "{reason}"
        );
        assert!(
            reason.contains("4097 added or modified entries"),
            "{reason}"
        );

        let too_big = DerivedSize {
            entries: 10,
            bytes: MAX_DERIVED_MUTATION_BYTES_V1 + 1,
            mutated_entries: 1,
            mutated_bytes: MAX_DERIVED_MUTATION_BYTES_V1 + 1,
        };
        let reason = too_big.check_ceiling().unwrap_err();
        assert!(reason.contains("MAX_DERIVED_MUTATION_BYTES_V1"), "{reason}");

        let huge_tree = DerivedSize {
            entries: MAX_DERIVED_SNAPSHOT_ENTRIES_V1 + 1,
            bytes: 10,
            mutated_entries: 1,
            mutated_bytes: 1,
        };
        assert!(
            huge_tree
                .check_ceiling()
                .unwrap_err()
                .contains("MAX_DERIVED_SNAPSHOT_ENTRIES_V1")
        );
        // A cargo check target tree on this workspace must not fit; an ordinary change must.
        let cargo_check_target = DerivedSize {
            entries: 10_408,
            bytes: 1024 * 1024 * 1024 + 4_211_917,
            mutated_entries: 10_000,
            mutated_bytes: 1024 * 1024 * 1024,
        };
        assert!(cargo_check_target.check_ceiling().is_err());
        let plausible_change = DerivedSize {
            entries: 908,
            bytes: 4_211_917 + 8 * 1024 * 1024,
            mutated_entries: 500,
            mutated_bytes: 8 * 1024 * 1024,
        };
        assert!(plausible_change.check_ceiling().is_ok());
    }

    #[test]
    fn verify_snapshot_checks_each_distinct_object_once_and_detects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let shared = cas.put(b"shared bytes").unwrap();
        let unique = cas.put(b"unique bytes").unwrap();
        let entries: Vec<Entry> = (0..50)
            .map(|index| entry(&format!("dup/{index:02}.txt"), &shared, 12))
            .chain(std::iter::once(entry("unique.txt", &unique, 12)))
            .collect();
        let manifest = Manifest::new(entries).unwrap();
        let mut expected = vec![shared.as_str(), unique.as_str()];
        expected.sort_unstable();
        assert_eq!(distinct_contents(&manifest), expected);
        let snapshot_id = put_manifest(&cas, &manifest).unwrap();
        verify_snapshot(&cas, &manifest, &snapshot_id).unwrap();

        let missing = Manifest::new(vec![entry(
            "gone.txt",
            &format!("sha256:{}", "0".repeat(64)),
            1,
        )])
        .unwrap();
        assert!(verify_snapshot(&cas, &missing, &snapshot_id).is_err());
        assert!(verify_snapshot(&cas, &manifest, &format!("sha256:{}", "1".repeat(64))).is_err());
    }

    struct MemoryLog(Vec<(String, TaskEventType, String)>);

    impl TaskLog for MemoryLog {
        fn append(
            &mut self,
            cas: &Cas,
            task_id: &str,
            event_type: TaskEventType,
            artifact_id: &str,
        ) -> Result<(), String> {
            admit_task_artifact(cas, event_type, artifact_id)?;
            self.0
                .push((task_id.into(), event_type, artifact_id.into()));
            Ok(())
        }
    }

    #[test]
    fn task_log_admission_refuses_dangling_and_mistyped_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut log = MemoryLog(Vec::new());
        let absent = format!("sha256:{}", "a".repeat(64));
        let refused = log
            .append(&cas, "task-x", TaskEventType::GateCompletedV1, &absent)
            .unwrap_err();
        assert!(refused.contains("not durable"), "{refused}");
        assert!(
            log.append(
                &cas,
                "task-x",
                TaskEventType::TaskCompletedV1,
                "not-a-digest"
            )
            .unwrap_err()
            .contains("not durable")
        );

        let evaluation = cas
            .put_json(&serde_json::json!({
                "schema": task_artifact::TASK_EVALUATION_V1,
                "verdict": "approve",
                "summary": "fine",
            }))
            .unwrap();
        let mistyped = log
            .append(&cas, "task-x", TaskEventType::TaskCompletedV1, &evaluation)
            .unwrap_err();
        assert!(
            mistyped.contains("must reference an `af/task-outcome@1`"),
            "{mistyped}"
        );
        log.append(
            &cas,
            "task-x",
            TaskEventType::EvaluationCompletedV1,
            &evaluation,
        )
        .unwrap();
        let check = cas.put_json(&serde_json::json!({"name": "gate"})).unwrap();
        log.append(&cas, "task-x", TaskEventType::GateCompletedV1, &check)
            .unwrap();
        assert_eq!(log.0.len(), 2);
    }
}
