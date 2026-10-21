//! The Task runtime's telemetry spans (design §7.1, ADR-0153): `af.run`, `af.authority.load`,
//! `af.plan` with one `af/TelemetryNode@1` record per plan node, `af.node` and `af.attempt`.
//!
//! Telemetry only records here; nothing in this module is read back, and no decision of the
//! runtime depends on it. At `off` no subscriber is installed: every span site below is a
//! disabled span, and nothing about the run is computed for it.
//!
//! **One execution per process.** A run's `af.run` span covers the time this process spends on
//! it: it is opened by the first runtime of the run the process builds and ended by
//! [`end_runs`], which `af` calls before its `af.command` span ends, so a review run whose Round
//! and Integration phases each build a runtime still records one execution. The execution's
//! plan identity and node graph are recorded once per plan.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

mod mirror;

pub use mirror::{
    DEMAND_EVIDENCE_SATISFIED, DEMAND_OPENED, DEMAND_STALE, DEMAND_WAIVED, FAIL_SETTING,
    FINDING_DISPOSITION, STORE_EVENT, StoreMirror, USAGE_STATE, USAGE_WALL, install_store_mirror,
};

use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_graph::task::{CompiledNode, CompiledTask};
use review_store::store::task::{TaskLease, TaskProjection, task_run_id};
use review_store::{Cas, EventStore};
use review_telemetry::record::{ConfigRow, Node};
use review_telemetry::subscriber::{self, Typed};
use serde_json::Value;
use tracing::field::Empty;

/// What kind of run the process works on (`af.run.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Task,
    Review,
    Optimize,
    Measure,
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Review => "review",
            Self::Optimize => "optimize",
            Self::Measure => "measure",
        }
    }
}

static KIND: Mutex<RunKind> = Mutex::new(RunKind::Task);

/// The kind of every run this process opens from now on: `af review run` sets `review`, the
/// self-optimizer `optimize`; a Task run is the default.
pub fn set_run_kind(kind: RunKind) {
    *lock(&KIND) = kind;
}

static CAMPAIGN: Mutex<Option<(String, u32)>> = Mutex::new(None);

/// The Campaign and Round the review run this process opens next works on (`af.campaign`,
/// `af.round`).
pub fn set_campaign(campaign: &str, round: u32) {
    *lock(&CAMPAIGN) = Some((campaign.to_string(), round));
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The sections of an execution's configuration record (design §7.4) that the command knows
/// and the runtime does not: written in the record's fixed order around the runtime's own
/// policy, pipeline and binding rows.
#[derive(Debug, Clone, Default)]
pub struct MachineConfig {
    /// `af --version --json`, one row per member.
    pub version: Vec<ConfigRow>,
    /// The effective configuration, one row per key with its layer, file and line; a value
    /// whose origin is the environment is `<env>`.
    pub configuration: Vec<ConfigRow>,
    /// The digests of `.af/af.toml` and `.af/af.lock`.
    pub policy_files: Vec<ConfigRow>,
    /// The Provider registry entries by name and kind, grant paths as placeholders.
    pub providers: Vec<ConfigRow>,
    /// The harness versions the preflight probed, and the Claude Code managed-settings digests.
    pub harnesses: Vec<ConfigRow>,
    /// The host facts.
    pub host: Vec<ConfigRow>,
    /// The names, never the values, of the `AF_*` and `OTEL_*` variables present.
    pub variables: Vec<ConfigRow>,
    /// The texts a `content` level names by the event `af.config.text`: kind, digest, bytes.
    pub texts: Vec<(&'static str, String, u64)>,
}

/// What the command hands every run it opens: computed when the record is written, so the
/// preflight's probes before it are in it.
type MachineConfigSource = Box<dyn Fn() -> MachineConfig + Send + Sync>;

static MACHINE: Mutex<Option<MachineConfigSource>> = Mutex::new(None);

/// The command's sections of every configuration record this process writes. `af` sets it when
/// it starts recording at a level that keeps the record; nothing reads it otherwise.
pub fn set_machine_config(source: MachineConfigSource) {
    *lock(&MACHINE) = Some(source);
}

/// One row of a configuration record.
pub fn row(layer: &str, key: &str, value: &str, origin: Option<String>) -> ConfigRow {
    ConfigRow {
        key: key.to_string(),
        value: value.to_string(),
        layer: layer.to_string(),
        origin,
    }
}

/// One open run of this process.
struct OpenRun {
    span: tracing::Span,
    /// The plans whose identity and nodes this execution recorded.
    plans: BTreeSet<String>,
}

/// The executions whose configuration record this process wrote.
static CONFIGURED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Every run this process opened, by `store_id:run_id`.
static RUNS: Mutex<BTreeMap<String, OpenRun>> = Mutex::new(BTreeMap::new());

/// The Stores (by `af.store_id`, `*` for one that could not be named) a mirror failure left
/// incomplete: every run of theirs records `af.ledger.coverage = incomplete`.
static INCOMPLETE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The `af.attempt` span of every Attempt this process opened, by Attempt id: its accounting
/// snapshots and sidecar writes are events on it.
static ATTEMPTS: Mutex<BTreeMap<String, subscriber::RecordedSpan>> = Mutex::new(BTreeMap::new());

/// A mirror failure: every open run of the Store records `af.ledger.coverage = incomplete`,
/// and so does every run of it this process opens later.
pub(crate) fn mark_ledger_incomplete(store_id: Option<&str>) {
    let store = store_id.unwrap_or("*");
    lock(&INCOMPLETE).insert(store.to_string());
    for (key, open) in lock(&RUNS).iter() {
        if store == "*" || key.starts_with(&format!("{store}:")) {
            open.span.record("af.ledger.coverage", "incomplete");
        }
    }
}

fn ledger_incomplete(store_id: &str) -> bool {
    let incomplete = lock(&INCOMPLETE);
    incomplete.contains("*") || incomplete.contains(store_id)
}

/// The open `af.run` spans of this process's runs of the Store `store_id`, by run id.
pub(crate) fn store_runs(store_id: &str) -> Vec<subscriber::RecordedSpan> {
    lock(&RUNS)
        .iter()
        .filter(|(key, _)| key.starts_with(&format!("{store_id}:")))
        .filter_map(|(_, open)| subscriber::recorded(&open.span))
        .collect()
}

/// The open `af.run` span of a run of the Store `store_id`: the one whose trace `current` is
/// in, else the only one, else the first by run id.
pub(crate) fn run_span(
    store_id: &str,
    current: Option<&subscriber::RecordedSpan>,
) -> Option<subscriber::RecordedSpan> {
    let recorded = store_runs(store_id);
    let trace = current.and_then(|current| current.trace_id.as_ref());
    recorded
        .iter()
        .find(|run| trace.is_some() && run.trace_id.as_ref() == trace)
        .or_else(|| recorded.first())
        .cloned()
}

/// The `af.attempt` span this process opened for `attempt_id`.
pub(crate) fn attempt_span(attempt_id: &str) -> Option<subscriber::RecordedSpan> {
    lock(&ATTEMPTS).get(attempt_id).cloned()
}

/// End every `af.run` span this process opened, each with its outcome already recorded. `af`
/// calls it before its `af.command` span ends; a run span still held elsewhere ends when the
/// last holder drops it.
pub fn end_runs() {
    let runs = std::mem::take(&mut *lock(&RUNS));
    drop(runs);
}

fn identity(value: &str) -> Option<Typed> {
    Typed::identity(value)
}

fn field(typed: &Option<Typed>) -> Option<&(dyn std::error::Error + 'static)> {
    typed.as_ref().map(Typed::field)
}

/// A Worker slot's package and binding, for `af.attempt`.
#[derive(Debug, Clone, Default)]
struct Binding {
    worker: Option<String>,
    provider: Option<String>,
    provider_kind: Option<String>,
}

/// What every span of a run names (design §5): its Store, its run, its kind and the execution,
/// with the Task it works on.
#[derive(Debug, Clone)]
struct Ids {
    store: Option<Typed>,
    run: Option<Typed>,
    kind: &'static str,
    execution: Option<Typed>,
    task: String,
}

/// The telemetry of one runtime: its run's span and what its node and Attempt spans name.
pub(crate) struct RunTelemetry {
    run: tracing::Span,
    key: Option<String>,
    epoch: u64,
    bindings: BTreeMap<String, Binding>,
    ids: Option<Ids>,
}

impl RunTelemetry {
    /// The run of `lease` in the Store `store`: this process's open `af.run` span for it, or a
    /// new one — a disabled span when nothing records.
    pub(crate) fn open(store: &EventStore, cas: &Cas, lease: &TaskLease) -> Self {
        let disabled = Self {
            run: tracing::Span::none(),
            key: None,
            epoch: lease.epoch(),
            bindings: BTreeMap::new(),
            ids: None,
        };
        let Some(writer) = subscriber::current_writer() else {
            return disabled;
        };
        let Ok(run_id) = task_run_id(lease.task_id()) else {
            return disabled;
        };
        let store_id = store
            .database_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .and_then(|directory| review_telemetry::trace::store_id(&directory).ok());
        let Some(store_id) = store_id else {
            writer.problem(
                review_telemetry::record::ProblemWhat::Observer,
                "the Store's directory cannot be named; its run is recorded without a trace",
            );
            return disabled;
        };
        let key = format!("{store_id}:{run_id}");
        let kind = *lock(&KIND);
        let ids = Ids {
            store: identity(&store_id),
            run: identity(&run_id),
            kind: kind.as_str(),
            execution: writer.segment_id().and_then(|id| identity(&id)),
            task: lease.task_id().to_string(),
        };
        let mut runs = lock(&RUNS);
        if let Some(open) = runs.get(&key) {
            return Self {
                run: open.span.clone(),
                key: Some(key),
                ids: Some(ids),
                ..disabled
            };
        }
        let sequence = store.latest_run_sequence(&run_id).ok().flatten();
        // The execution's number among the run's: one more than the execution markers
        // (`TaskExecutionStarted@1`) of the leases before this one, which the runtime committed before it opened
        // this span. The lease epoch counts every lease — a plan-only start, a sweep's
        // housekeeping lease — so it stays only in `af.epoch`.
        let ordinal = match store.task_run_executions_before(cas, lease.task_id(), lease.epoch()) {
            Ok(before) => before.checked_add(1),
            Err(_) => {
                writer.problem(
                    review_telemetry::record::ProblemWhat::Observer,
                    "the Task's committed leases cannot be read; the execution carries no ordinal",
                );
                None
            }
        };
        let campaign = lock(&CAMPAIGN).clone();
        let parent = subscriber::command_span_id();
        let span = tracing::info_span!(
            parent: parent,
            "af.run",
            af.store_id = field(&ids.store),
            af.run_id = field(&ids.run),
            af.run.kind = Typed::enumerated(ids.kind).field(),
            af.execution_id = field(&ids.execution),
            af.execution.ordinal = ordinal,
            af.store.sequence_at_start = sequence,
            af.task_id = ids.task.as_str(),
            af.campaign = campaign.as_ref().map(|(campaign, _)| campaign.as_str()),
            af.round = campaign.as_ref().map(|(_, round)| u64::from(*round)),
            af.epoch = lease.epoch(),
            af.policy_rev = Empty,
            af.pipeline = Empty,
            af.pipeline.digest = Empty,
            af.outcome = Empty,
            af.usage.chargeable_tokens = Empty,
            af.failure.class = Empty,
            af.failure.reason = Empty,
            error.type = Empty,
            af.ledger.coverage = Empty,
            af.status = Empty,
        );
        // Whether every committed event and summary of this execution reached Telemetry: the
        // mirror records from `spans` on, and a failure since makes it incomplete.
        if writer.level() >= review_telemetry::level::Level::Spans {
            span.record(
                "af.ledger.coverage",
                if ledger_incomplete(&store_id) {
                    "incomplete"
                } else {
                    "complete"
                },
            );
        }
        // The command names the first run it works on, once known.
        if let Some(command) = subscriber::command_span() {
            command.record("af.store_id", field(&ids.store));
            command.record("af.run_id", field(&ids.run));
        }
        runs.insert(
            key.clone(),
            OpenRun {
                span: span.clone(),
                plans: BTreeSet::new(),
            },
        );
        Self {
            run: span,
            key: Some(key),
            ids: Some(ids),
            ..disabled
        }
    }

    /// The run's `af.run` span (a disabled one when nothing records).
    pub(crate) fn span(&self) -> &tracing::Span {
        &self.run
    }

    /// The registry Provider a Worker slot is bound to, for `af.admission`.
    pub(crate) fn provider(&self, slot: Option<&str>) -> Option<String> {
        slot.and_then(|slot| self.bindings.get(slot))
            .and_then(|binding| binding.provider.clone())
    }

    /// The `af.authority.load` span, entered by the caller around the authority check.
    pub(crate) fn authority_load(&self) -> tracing::Span {
        let Some(ids) = &self.ids else {
            return tracing::Span::none();
        };
        tracing::info_span!(
            parent: &self.run,
            "af.authority.load",
            af.store_id = field(&ids.store),
            af.run_id = field(&ids.run),
            af.run.kind = Typed::enumerated(ids.kind).field(),
            af.execution_id = field(&ids.execution),
            af.policy_rev = Empty,
            af.pipeline = Empty,
            af.pipeline.digest = Empty,
            af.status = Empty,
        )
    }

    /// Record what the authority load resolved on its span, and the plan this execution runs:
    /// the `af.plan` span with one `node` record per plan node, once per plan.
    pub(crate) fn plan(
        &mut self,
        load: &tracing::Span,
        cas: &Cas,
        plan_id: &str,
        plan: &ExecutionPlanV1,
        graph: &CompiledTask,
        task_kind: &str,
    ) {
        for (slot, definition) in &graph.slots {
            let binding = plan.bindings.get(slot);
            let (provider, provider_kind) = match binding.map(|binding| &binding.execution) {
                Some(WorkerExecutionV1::Model {
                    provider,
                    provider_kind,
                    ..
                }) => (Some(provider.clone()), Some(provider_kind.clone())),
                _ => (None, None),
            };
            self.bindings.insert(
                slot.clone(),
                Binding {
                    worker: Some(definition.worker.clone()),
                    provider,
                    provider_kind,
                },
            );
        }
        if self.run.is_disabled() {
            return;
        }
        let pipeline = graph.calls.get("root").map(|call| call.pipeline.clone());
        let policy = identity(&plan.authority.policy_id);
        let pipeline_digest = identity(&plan.pipeline_id);
        // On the load that resolved them, and on the run they govern (design §5).
        for span in [load, &self.run] {
            span.record("af.policy_rev", field(&policy));
            if let Some(pipeline) = &pipeline {
                span.record("af.pipeline", pipeline.as_str());
            }
            span.record("af.pipeline.digest", field(&pipeline_digest));
        }
        load.record("af.status", "ok");
        let Some(key) = &self.key else {
            return;
        };
        if !lock(&RUNS)
            .get_mut(key)
            .is_some_and(|open| open.plans.insert(plan_id.to_string()))
        {
            return;
        }
        let digest = identity(plan_id);
        let Some(ids) = &self.ids else {
            return;
        };
        self.record_config(cas, plan, graph, pipeline.as_deref(), task_kind);
        let span = tracing::info_span!(
            parent: &self.run,
            "af.plan",
            af.store_id = field(&ids.store),
            af.run_id = field(&ids.run),
            af.run.kind = Typed::enumerated(ids.kind).field(),
            af.execution_id = field(&ids.execution),
            af.plan_digest = field(&digest),
            af.pipeline = pipeline.as_deref(),
            af.pipeline.digest = field(&pipeline_digest),
            af.node_count = graph.nodes.len() as u64,
            af.bindings = plan.bindings.len() as u64,
            af.status = "ok",
        );
        let Some(writer) = subscriber::current_writer() else {
            return;
        };
        if !writer.level().records() || writer.level() == review_telemetry::level::Level::Metrics {
            return;
        }
        let _entered = span.enter();
        record_nodes(&writer, plan_id, graph);
    }

    /// The execution's configuration record (`af/TelemetryConfig@1`, design §7.4), once per
    /// execution, at `spans` and above: its rows in the fixed order — version, configuration,
    /// policy and pipeline, bindings, Providers, harness versions, host, variable names — as
    /// many `config` parts as the record bound needs. At `content` the texts it names are the
    /// events `af.config.text` with their digest and size (their bytes are blobs only once the
    /// content level stores configuration texts).
    fn record_config(
        &self,
        cas: &Cas,
        plan: &ExecutionPlanV1,
        graph: &CompiledTask,
        pipeline: Option<&str>,
        task_kind: &str,
    ) {
        let Some(writer) = subscriber::current_writer() else {
            return;
        };
        if writer.level() == review_telemetry::level::Level::Metrics {
            return;
        }
        let Some(execution) = writer.segment_id() else {
            return;
        };
        let Some(ids) = &self.ids else {
            return;
        };
        if !lock(&CONFIGURED).insert(execution.clone()) {
            return;
        }
        let machine = lock(&MACHINE)
            .as_ref()
            .map(|source| source())
            .unwrap_or_default();
        let mut rows = machine.version;
        rows.extend(machine.configuration);
        rows.push(row(
            "policy",
            "af.policy_rev",
            &plan.authority.policy_id,
            None,
        ));
        rows.push(row(
            "policy",
            "af.pipeline",
            pipeline.unwrap_or("unknown"),
            None,
        ));
        rows.push(row("policy", "af.pipeline.digest", &plan.pipeline_id, None));
        for (node_id, node) in &graph.nodes {
            let (kind, _) = operator_kind(node);
            rows.push(row(
                "policy",
                "af.pipeline.node",
                &format!("{node_id} {kind}"),
                None,
            ));
        }
        rows.extend(machine.policy_files);
        rows.push(row("policy", "af.task_id", &ids.task, None));
        rows.push(row("policy", "af.task.kind", task_kind, None));
        rows.push(row(
            "policy",
            "af.task.pipeline",
            pipeline.unwrap_or("unknown"),
            None,
        ));
        for (node_id, node) in &graph.nodes {
            let (_, slot) = operator_kind(node);
            let Some(slot) = slot else {
                continue;
            };
            let worker = graph.slots.get(&slot).map(|slot| slot.worker.as_str());
            let execution = plan.bindings.get(&slot).map(|binding| &binding.execution);
            let value = match execution {
                Some(WorkerExecutionV1::Model {
                    provider,
                    model,
                    effort,
                    ..
                }) => format!(
                    "worker={} provider={provider} model={model} effort={effort}",
                    worker.unwrap_or("unknown")
                ),
                Some(WorkerExecutionV1::Command {}) => {
                    format!("worker={} command", worker.unwrap_or("unknown"))
                }
                None => format!("worker={} unbound", worker.unwrap_or("unknown")),
            };
            rows.push(row("binding", node_id, &value, None));
        }
        rows.extend(machine.providers);
        rows.extend(machine.harnesses);
        rows.extend(machine.host);
        rows.extend(machine.variables);
        writer.config(&execution, rows);
        if writer.level().keeps_content() {
            let size = |id: &str| cas.get(id).map(|bytes| bytes.len() as u64).ok();
            let texts = [
                (
                    "pipeline",
                    plan.pipeline_id.clone(),
                    size(&plan.pipeline_id),
                ),
                (
                    "task_file",
                    plan.task_revision_id.clone(),
                    size(&plan.task_revision_id),
                ),
            ]
            .into_iter()
            .chain(
                machine
                    .texts
                    .into_iter()
                    .map(|(kind, digest, bytes)| (kind, digest, Some(bytes))),
            );
            for (kind, digest, bytes) in texts {
                let digest = identity(&digest);
                tracing::info!(
                    parent: &self.run,
                    af.event = "af.config.text",
                    af.config.text.kind = Typed::enumerated(kind).field(),
                    af.config.text.digest = field(&digest),
                    af.config.text.bytes = bytes,
                );
            }
        }
    }

    /// The `af.node` span of `node_id`, on any thread: its parent is the run, named explicitly.
    pub(crate) fn node(&self, node_id: &str, node: Option<&CompiledNode>) -> tracing::Span {
        if self.run.is_disabled() {
            return tracing::Span::none();
        }
        let Some(ids) = &self.ids else {
            return tracing::Span::none();
        };
        let (kind, _) = node.map(operator_kind).unwrap_or(("unknown".into(), None));
        tracing::info_span!(
            parent: &self.run,
            "af.node",
            af.store_id = field(&ids.store),
            af.run_id = field(&ids.run),
            af.run.kind = Typed::enumerated(ids.kind).field(),
            af.execution_id = field(&ids.execution),
            af.task_id = ids.task.as_str(),
            af.node_id = node_id,
            af.node.kind = kind.as_str(),
            af.outcome = Empty,
            af.failure.class = Empty,
            af.failure.reason = Empty,
            error.type = Empty,
            af.status = Empty,
        )
    }

    /// The `af.attempt` span of one Attempt of `node_id`, under its node's span.
    pub(crate) fn attempt(
        &self,
        node_span: &tracing::Span,
        node_id: &str,
        attempt_id: &str,
        slot: Option<&str>,
    ) -> tracing::Span {
        if node_span.is_disabled() {
            return tracing::Span::none();
        }
        let binding = slot
            .and_then(|slot| self.bindings.get(slot))
            .cloned()
            .unwrap_or_default();
        let Some(ids) = &self.ids else {
            return tracing::Span::none();
        };
        let attempt = identity(attempt_id);
        let span = tracing::info_span!(
            parent: node_span,
            "af.attempt",
            af.store_id = field(&ids.store),
            af.run_id = field(&ids.run),
            af.run.kind = Typed::enumerated(ids.kind).field(),
            af.execution_id = field(&ids.execution),
            af.task_id = ids.task.as_str(),
            af.attempt_id = field(&attempt),
            af.node_id = node_id,
            af.epoch = self.epoch,
            af.worker = binding.worker.as_deref(),
            af.provider = binding.provider.as_deref(),
            af.provider.kind = binding.provider_kind.as_deref(),
            af.outcome = Empty,
            af.failure.class = Empty,
            af.failure.reason = Empty,
            error.type = Empty,
            af.usage.input_tokens = Empty,
            af.usage.output_tokens = Empty,
            af.usage.cache_read_tokens = Empty,
            af.usage.cache_write_tokens = Empty,
            af.usage.reasoning_tokens = Empty,
            af.usage.chargeable_tokens = Empty,
            af.usage.observation = Empty,
            af.usage.unknown_cause = Empty,
            af.status = Empty,
        );
        if let Some(recorded) = subscriber::recorded(&span) {
            lock(&ATTEMPTS).insert(attempt_id.to_string(), recorded);
        }
        span
    }

    /// Record how the run stands as this runtime finishes: the Task's phase and its charge as
    /// the Task projection holds them (the kernel's accounting, never a sum of its own).
    pub(crate) fn finish(&self, projection: &TaskProjection, error: Option<&str>) {
        if self.run.is_disabled() {
            return;
        }
        let phase = serde_json::to_value(&projection.phase)
            .ok()
            .and_then(|value| {
                value
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".into());
        self.run.record("af.outcome", phase.as_str());
        if let Some(execution) = &projection.execution {
            self.run.record(
                "af.usage.chargeable_tokens",
                execution.budget.committed_tokens(),
            );
        }
        match error {
            Some(reason) => {
                self.run.record("af.status", "error");
                self.run.record("af.failure.class", "failed");
                self.run.record("error.type", "failed");
                self.run.record("af.failure.reason", reason);
            }
            None => {
                self.run.record("af.status", "ok");
            }
        }
    }
}

/// The run of `lease` finished: its `af.run` span says so (the Task's result is recorded).
pub fn finished(store: &EventStore, lease: &TaskLease) {
    let Ok(run_id) = task_run_id(lease.task_id()) else {
        return;
    };
    let Some(store_id) = store
        .database_path()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .and_then(|directory| review_telemetry::trace::store_id(&directory).ok())
    else {
        return;
    };
    if let Some(open) = lock(&RUNS).get(&format!("{store_id}:{run_id}")) {
        open.span.record("af.outcome", "finished");
    }
}

// ------------------------------------------------------------------------------------------
// The phases below a node (design §7.1). Each opens only inside a span of a run, so the same
// phase outside one records nothing; each takes the run's identity from its parent span.

/// `af.gate` over a check node's checks.
pub(crate) fn gate() -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.gate",
        af.gate.checks = tracing::field::Empty,
        af.gate.decision = tracing::field::Empty,
        af.check.warm = tracing::field::Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End `af.gate`: how many checks it ran, its decision (`passed`, `failed`, `inconclusive`)
/// and whether any check ran warm. A gate that did not decide is an error with its reason.
pub(crate) fn gate_end(
    span: &tracing::Span,
    checks: usize,
    decision: Result<&'static str, &str>,
    warm: &'static str,
) {
    if span.is_disabled() {
        return;
    }
    span.record("af.gate.checks", checks as u64);
    span.record("af.check.warm", Typed::enumerated(warm).field());
    match decision {
        Ok(decision) => {
            span.record("af.gate.decision", Typed::enumerated(decision).field());
            review_telemetry::process::outcome(span, None);
        }
        Err(reason) => review_telemetry::process::outcome(span, Some(("failed", reason))),
    }
}

/// `af.warm.restore` or `af.warm.save` of one check's Warm Check Cache layer.
pub(crate) fn warm(name: &'static str) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    match name {
        "af.warm.restore" => tracing::info_span!(
            "af.warm.restore",
            af.warm.key = Empty,
            af.warm.result = Empty,
            af.warm.kinds = Empty,
            af.warm.bytes = Empty,
            af.outcome = Empty,
            af.failure.class = Empty,
            af.failure.reason = Empty,
            error.type = Empty,
            af.status = Empty,
        ),
        _ => tracing::info_span!(
            "af.warm.save",
            af.warm.key = Empty,
            af.warm.result = Empty,
            af.warm.kinds = Empty,
            af.warm.bytes = Empty,
            af.outcome = Empty,
            af.failure.class = Empty,
            af.failure.reason = Empty,
            error.type = Empty,
            af.status = Empty,
        ),
    }
}

/// End a warm span: the toolchain key, `hit` or `miss`, the kinds and the bytes (available
/// before the check for a restore, evicted after it for a save).
pub(crate) fn warm_end(
    span: &tracing::Span,
    key: Option<&str>,
    hit: bool,
    kinds: usize,
    bytes: u64,
    failure: Option<&str>,
) {
    if span.is_disabled() {
        return;
    }
    if let Some(key) = key.and_then(Typed::identity) {
        span.record("af.warm.key", key.field());
    }
    span.record(
        "af.warm.result",
        Typed::enumerated(if hit { "hit" } else { "miss" }).field(),
    );
    span.record("af.warm.kinds", kinds as u64);
    span.record("af.warm.bytes", bytes);
    review_telemetry::process::outcome(span, failure.map(|reason| ("failed", reason)));
}

/// `af.context.compose`: the composed Worker input an Attempt is bound to — its manifest
/// digest, rendered bytes, estimated tokens and layer list — never its bytes.
pub(crate) fn context_compose(
    manifest_id: &str,
    layers: Vec<String>,
    bytes: u64,
    estimated_tokens: u64,
) {
    if !review_telemetry::subscriber::in_run() {
        return;
    }
    let manifest = identity(manifest_id);
    let span = tracing::info_span!(
        "af.context.compose",
        af.context.manifest = field(&manifest),
        af.context.bytes = bytes,
        af.context.estimated_tokens = estimated_tokens,
        // The layer names, in manifest order, comma-separated: a `tracing` field carries no
        // array.
        af.context.layers = layers.join(",").as_str(),
        af.status = "ok",
    );
    drop(span);
}

/// `af.admission`: the Attempt's admission before dispatch — the free-disk floor and the
/// durable start of its reservation.
pub(crate) fn admission(reserved_tokens: u64, provider: Option<&str>) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.admission",
        af.admission.reserved_tokens = reserved_tokens,
        af.provider = provider,
        af.admission.wait_ms = Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End `af.admission` with its wait and its outcome.
pub(crate) fn admission_end(
    span: &tracing::Span,
    wait: std::time::Duration,
    refusal: Option<&str>,
) {
    if span.is_disabled() {
        return;
    }
    span.record(
        "af.admission.wait_ms",
        u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
    );
    review_telemetry::process::outcome(span, refusal.map(|reason| ("refused", reason)));
}

/// `af.output.parse`: the admission of an Attempt's outputs against its node contract.
pub(crate) fn output_parse(contract: Vec<String>) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.output.parse",
        af.output.contract = contract.join(",").as_str(),
        af.output.valid = Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End `af.output.parse`: valid, or the refusal reason (`invalid_output_contract`).
pub(crate) fn output_parse_end(span: &tracing::Span, refusal: Option<&str>) {
    if span.is_disabled() {
        return;
    }
    span.record("af.output.valid", refusal.is_none());
    review_telemetry::process::outcome(
        span,
        refusal.map(|reason| ("invalid_output_contract", reason)),
    );
}

/// `af.usage.account`: the kernel's accounting of one Attempt's usage.
pub(crate) fn usage_account() -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.usage.account",
        af.usage.observation = Empty,
        af.usage.unknown_cause = Empty,
        af.usage.chargeable_tokens = Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End `af.usage.account` with the observation state (`complete`, `incomplete`, `unknown`),
/// the unknown cause, and the charge as the kernel accounted it.
pub(crate) fn usage_account_end(
    span: &tracing::Span,
    observation: &'static str,
    unknown: Option<&review_core::task::usage::TaskUnknownUsageV1>,
    charged: u128,
) {
    if span.is_disabled() {
        return;
    }
    span.record(
        "af.usage.observation",
        Typed::enumerated(observation).field(),
    );
    if let Some(unknown) = unknown {
        let cause = serde_json::to_value(unknown.cause)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unreported".into());
        span.record("af.usage.unknown_cause", cause.as_str());
    }
    span.record("af.usage.chargeable_tokens", charged);
    review_telemetry::process::outcome(span, None);
}

/// `af.remote_check`: the gate pull request phase of the remote checks (ADR-0140).
pub(crate) fn remote_check(branch: &str, checks: usize) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.remote_check",
        af.remote_check.branch = branch,
        af.remote_check.checks = checks as u64,
        af.remote_check.pull_request = Empty,
        af.remote_check.polls = Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// `af.process` of a supervised process no other span names (a toolchain probe, `gh`).
pub(crate) fn process(program: &str, kind: &str) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    let program = std::path::Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    review_telemetry::process_span!(
        "af.process",
        process.command = program.as_deref(),
        af.process.kind = kind,
    )
}

/// The `af.run` of one kernel measurement (ADR-0132, design §7.7): `af.run.kind = measure`,
/// its own run id — derived from the Task's run, the measure Attempt and the measure's name, so
/// a measurement is a run of its own — and at its end `af.measurement.id`, the
/// `af/Measurement@1` it recorded. Its repetitions are its child spans.
pub(crate) fn measurement(task_id: &str, attempt_id: &str, measure: &str) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    let Ok(task_run) = task_run_id(task_id) else {
        return tracing::Span::none();
    };
    let run = review_telemetry::trace::sha256_digest(
        format!("af/measurement-run@1:{task_run}:{attempt_id}:{measure}").as_bytes(),
    );
    tracing::info_span!(
        "af.run",
        af.run_id = field(&identity(&run)),
        af.run.kind = Typed::enumerated(RunKind::Measure.as_str()).field(),
        af.task_id = task_id,
        af.attempt_id = field(&identity(attempt_id)),
        af.measure = measure,
        af.measurement.id = Empty,
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End a measurement's `af.run`: the Measurement it recorded, passed or failed.
pub(crate) fn measurement_end(span: &tracing::Span, measurement_id: &str, failed: bool) {
    if span.is_disabled() {
        return;
    }
    if let Some(id) = identity(measurement_id) {
        span.record("af.measurement.id", id.field());
    }
    review_telemetry::process::outcome(
        span,
        failed.then_some(("failed", "a repetition failed the measurement")),
    );
}

/// One repetition of a measurement, under its `af.run`.
pub(crate) fn repetition(repetition: u32) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    tracing::info_span!(
        "af.measurement.repetition",
        af.measurement.repetition = u64::from(repetition),
        af.outcome = Empty,
        af.failure.class = Empty,
        af.failure.reason = Empty,
        error.type = Empty,
        af.status = Empty,
    )
}

/// End a repetition: completed, or failed with its detail.
pub(crate) fn repetition_end(span: &tracing::Span, failure: Option<&str>) {
    review_telemetry::process::outcome(span, failure.map(|detail| ("failed", detail)));
}

/// `af.git` of a git process this crate supervises itself (the remote check's private
/// repository), named by its subcommand.
pub(crate) fn git(op: Option<&str>) -> tracing::Span {
    if !review_telemetry::subscriber::in_run() {
        return tracing::Span::none();
    }
    review_telemetry::process_span!("af.git", process.command = "git", af.git.op = op)
}

fn resource_use(rusage: review_process::Rusage) -> review_telemetry::process::ResourceUse {
    review_telemetry::process::ResourceUse {
        user_ms: rusage.user_ms,
        system_ms: rusage.system_ms,
        max_rss_bytes: rusage.max_rss_bytes,
    }
}

/// End a process span from a supervised capture: `ok` for a clean exit, else the class.
pub(crate) fn process_capture(span: &tracing::Span, captured: &review_process::SupervisedCapture) {
    if span.is_disabled() {
        return;
    }
    let mut facts = review_telemetry::process::ProcessFacts {
        pid: captured.pid,
        rusage: captured.rusage.map(resource_use),
        stdout_bytes: Some(captured.stdout.len() as u64),
        stderr_bytes: Some(captured.stderr.len() as u64),
        stderr_held: captured.stderr_held,
        ..Default::default()
    };
    if let Ok(status) = &captured.status {
        facts = facts.with_status(status);
    }
    process_end(span, &facts, captured.status.as_ref().map(|status| *status));
}

/// End a process span from a supervised run's result.
pub(crate) fn process_output(
    span: &tracing::Span,
    output: &Result<review_process::SupervisedOutput, review_process::SupervisedError>,
) {
    if span.is_disabled() {
        return;
    }
    let facts = match output {
        Ok(output) => review_telemetry::process::ProcessFacts {
            pid: output.pid,
            rusage: output.rusage.map(resource_use),
            stdout_bytes: Some(output.stdout.len() as u64),
            stderr_bytes: Some(output.stderr.len() as u64),
            stderr_held: output.stderr_held,
            ..Default::default()
        }
        .with_status(&output.status),
        Err(review_process::SupervisedError::TimedOut {
            stdout,
            stderr,
            pid,
            rusage,
        }) => review_telemetry::process::ProcessFacts {
            pid: *pid,
            rusage: rusage.map(resource_use),
            stdout_bytes: Some(stdout.len() as u64),
            stderr_bytes: Some(stderr.len() as u64),
            ..Default::default()
        },
        Err(_) => review_telemetry::process::ProcessFacts::default(),
    };
    process_end(span, &facts, output.as_ref().map(|output| output.status));
}

fn process_end(
    span: &tracing::Span,
    facts: &review_telemetry::process::ProcessFacts,
    status: Result<std::process::ExitStatus, &review_process::SupervisedError>,
) {
    use review_process::SupervisedError;
    let failure = match status {
        Ok(status) if status.success() => None,
        Ok(_) => Some(("failed", "the process exited unsuccessfully".to_string())),
        Err(SupervisedError::TimedOut { .. }) => {
            Some(("timed_out", "the process exceeded its deadline".to_string()))
        }
        Err(SupervisedError::Cancelled) => Some(("refused", "cancelled".to_string())),
        Err(SupervisedError::Spawn(_)) => Some((
            "unavailable",
            "the process could not be started".to_string(),
        )),
        Err(error) => Some(("failed", error.to_string())),
    };
    review_telemetry::process::finish(
        span,
        facts,
        failure
            .as_ref()
            .map(|(class, reason)| (*class, reason.as_str())),
    );
}

/// One `af/TelemetryNode@1` record per node of the plan's compiled graph: its kind, its Worker
/// package, every node it depends on (its inputs and its conditions, up to 64) and how many
/// nodes the plan has, so a reader that finds fewer says so.
fn record_nodes(
    writer: &review_telemetry::segment::writer::Writer,
    plan_id: &str,
    graph: &CompiledTask,
) {
    let execution = writer.segment_id().unwrap_or_default();
    let expected = u32::try_from(graph.nodes.len()).unwrap_or(u32::MAX);
    for (node_id, node) in &graph.nodes {
        let (kind, slot) = operator_kind(node);
        let mut depends_on: Vec<String> = node
            .inputs
            .values()
            .map(|address| address.node.clone())
            .chain(
                node.conditions
                    .iter()
                    .map(|condition| condition.source.node.clone()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        depends_on.truncate(64);
        writer.node(Node {
            seq: 0,
            schema: String::new(),
            execution_id: execution.clone(),
            plan_digest: plan_id.to_string(),
            node_id: node_id.clone(),
            kind,
            package: slot
                .and_then(|slot| graph.slots.get(&slot))
                .map(|slot| slot.worker.clone()),
            depends_on,
            expected_nodes: expected,
        });
    }
}

/// A node's kind (its operator: `worker`, `check`, `seal`, `root_inputs`, …) and the Worker
/// slot it names, when it names one.
pub(crate) fn operator_kind(node: &CompiledNode) -> (String, Option<String>) {
    let value = serde_json::to_value(&node.operator).unwrap_or(Value::Null);
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let inner = value.get("operator");
    let op = inner
        .and_then(|operator| operator.get("op"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let slot = inner
        .and_then(|operator| operator.get("slot"))
        .or_else(|| {
            value
                .get("operation")
                .and_then(|operation| operation.get("slot"))
        })
        .and_then(Value::as_str)
        .map(str::to_string);
    (op.unwrap_or(kind), slot)
}

/// Record an Attempt's or node's end on its span: the outcome, and on failure the kernel's
/// class and reason (`error.type` equal to the class).
pub(crate) fn outcome(span: &tracing::Span, failure: Option<(&str, &str)>) {
    if span.is_disabled() {
        return;
    }
    match failure {
        None => {
            span.record("af.outcome", "succeeded");
            span.record("af.status", "ok");
        }
        Some((class, reason)) => {
            span.record("af.outcome", "failed");
            span.record("af.status", "error");
            span.record("af.failure.class", class);
            span.record("error.type", class);
            span.record("af.failure.reason", reason);
        }
    }
}

/// Record an Attempt's end: the kernel's accounting as the Attempt settled — its charge, the
/// exact counters it reported and whether its usage is known (ADR-0078, ADR-0143) — and its
/// outcome.
pub(crate) fn attempt_end(
    span: &tracing::Span,
    charged: u128,
    usage: Option<&review_core::task::usage::TaskTokenUsageV3>,
    unknown: Option<&review_core::task::usage::TaskUnknownUsageV1>,
    failure: Option<(&str, &str)>,
) {
    if span.is_disabled() {
        return;
    }
    span.record("af.usage.chargeable_tokens", charged);
    if let Some(usage) = usage {
        for (key, value) in [
            ("af.usage.input_tokens", usage.input_tokens),
            ("af.usage.output_tokens", usage.output_tokens),
            ("af.usage.cache_read_tokens", usage.cache_read_tokens),
            ("af.usage.cache_write_tokens", usage.cache_write_tokens),
            ("af.usage.reasoning_tokens", usage.reasoning_tokens),
        ] {
            if let Some(value) = value {
                span.record(key, value.get());
            }
        }
    }
    match unknown {
        Some(unknown) => {
            span.record("af.usage.observation", "unknown");
            let cause = serde_json::to_value(unknown.cause)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unreported".into());
            span.record("af.usage.unknown_cause", cause.as_str());
        }
        None => {
            span.record("af.usage.observation", "complete");
        }
    }
    outcome(span, failure);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design §7.1: a Pipeline holds up to 64 static nodes, so the graph is records, not an
    /// attribute: a 64-node plan whose last node depends on every other one writes 64 node
    /// records, each within the record bound, each with its full `depends_on` and
    /// `expected_nodes`.
    #[test]
    fn telemetry_plan_nodes_at_64() {
        let mut nodes = serde_json::Map::new();
        for index in 0..64 {
            let inputs: serde_json::Map<String, Value> = (0..index)
                .map(|other| {
                    (
                        format!("in{other}"),
                        serde_json::json!({"node": format!("root.nodes.n{other:02}"), "port": "out"}),
                    )
                })
                .collect();
            nodes.insert(
                format!("root.nodes.n{index:02}"),
                serde_json::json!({
                    "operator": {"kind": "primitive", "operator": {"op": "seal"}, "signature": "operator/seal"},
                    "contract": {"inputs": {}, "outputs": {}},
                    "inputs": inputs,
                    "conditions": [],
                }),
            );
        }
        let graph: CompiledTask = serde_json::from_value(serde_json::json!({
            "schema": "af.compiled-task/1",
            "nodes": nodes,
            "order": [],
            "inputs": {},
            "outputs": {},
            "coverage": {},
            "calls": {},
            "slots": {},
            "max_parallel": 1,
            "allowances": {},
        }))
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let writer = review_telemetry::segment::writer::Writer::new(
            review_telemetry::segment::writer::WriterOptions::new(
                temp.path(),
                review_telemetry::level::Level::Spans,
            ),
        )
        .unwrap();
        // The process's `af.command` opened the segment, as in a run.
        writer.span_start(
            None,
            None,
            "af.command",
            review_telemetry::record::SpanKind::Internal,
            review_telemetry::record::Attrs::new(),
        );
        let plan = format!("sha256:{}", "a".repeat(64));
        record_nodes(&writer, &plan, &graph);
        writer.flush();
        let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|line| {
                assert!(line.len() <= review_telemetry::record::RECORD_MAX_BYTES);
                serde_json::from_str(line).unwrap()
            })
            .filter(|record: &Value| record["t"] == "node")
            .collect();
        assert_eq!(records.len(), 64);
        let execution = writer.segment_id().unwrap();
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["schema"], "af/TelemetryNode@1");
            assert_eq!(record["expected_nodes"], 64);
            assert_eq!(record["plan_digest"], plan.as_str());
            assert_eq!(record["execution_id"], execution.as_str());
            assert_eq!(record["kind"], "seal");
            assert_eq!(record["node_id"], format!("root.nodes.n{index:02}"));
            let depends_on = record["depends_on"].as_array().unwrap();
            assert_eq!(depends_on.len(), index);
            for (other, id) in depends_on.iter().enumerate() {
                assert_eq!(id, &Value::from(format!("root.nodes.n{other:02}")));
            }
        }
        writer
            .finish(review_telemetry::record::SealExit::Ok)
            .unwrap();
    }
}
