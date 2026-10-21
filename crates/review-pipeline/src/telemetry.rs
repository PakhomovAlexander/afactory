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
use review_telemetry::record::Node;
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

/// One open run of this process.
struct OpenRun {
    span: tracing::Span,
    /// The plans whose identity and nodes this execution recorded.
    plans: BTreeSet<String>,
}

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
        plan_id: &str,
        plan: &ExecutionPlanV1,
        graph: &CompiledTask,
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
