//! The Store commit mirror (design §7.5, ADR-0153): the one post-commit observer on
//! `EventStore`, told of every transaction the kernel commits on any path — `append`,
//! `append_batch`, the ordinary Task transitions, Round publication, atomic integration,
//! collection — with its events in committed order, and never of one that rolled back.
//!
//! What it records, each only after the commit it describes:
//!
//! - at `spans` and above, one event `af.store.event` per committed event on the current span,
//!   with `af.store.event.type`, `.sequence`, `.id`, `.causation_id`, `artifact_refs`, and the
//!   payload's digest and size (`af.store.event.payload_digest`, `.payload_bytes`); at `content`
//!   the payload itself is a blob the event names, redacted as a record of the kernel's own (its
//!   identity members kept) and counted against the process's content allowance;
//! - at `spans` and above, for a native Task Review log, a `finding` record (`af/TelemetryFinding@2`)
//!   for every Report admitted into the Ledger and whenever a summarized field of a Finding
//!   changes, with the event `af.finding.disposition` keyed by the Finding key when its status
//!   does, and a `demand` record (`af/TelemetryDemand@2`) — the Demand's own Subject and the
//!   Snapshot bound to it — with the events `af.demand.opened`, `af.demand.evidence_satisfied`
//!   (with the Evidence id; an admitted reuse that satisfies a stale Demand again included),
//!   `af.demand.stale` and `af.demand.waived`, keyed by the Demand id and never by a Finding
//!   key (ADR-0007). Every summary carries the log and committed sequence of its cause. They come from the canonical Ledger recomputed from committed native Round receipts
//!   and their original captured plans, so a reader
//!   needs no Store. At `content` an admitted Finding's body is a blob named by
//!   `af.finding.body`, keyed by the Finding key, whenever it differs from the body last
//!   recorded;
//! - at `metrics` and above, after every committed Task event that changes an Attempt's
//!   accounting (`UsageObserved`, `Settled` — recovery's included — and `Released`), the event
//!   `af.usage.state`: the Attempt's charge and counters as the Task projection holds them after
//!   that commit, with `af.usage.source` and `af.usage.sequence`, the committed event's own
//!   sequence (the charge series);
//! - at `metrics` and above, after every committed sidecar write, the event `af.usage.wall`
//!   with the merged row, `af.usage.source = sidecar` and `af.usage.wall.sequence`, the
//!   record's own `seq`, which orders one execution's sidecar writes and nothing else (the wall
//!   series, never merged with the charge).
//!
//! It reads only what the commit hands it and the Store's read API, never Telemetry, and it
//! changes nothing in the Store. A mirror failure is one `observer` problem, never a Store
//! error, and marks every run of that Store `af.ledger.coverage = incomplete`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use af_contracts::task::event::TaskChangeV1;
use af_contracts::task::execution::TaskExecutionRecordV1;
use af_contracts::task::usage::{
    TASK_USAGE_OBSERVATION_V1, TaskTokenUsageV3, TaskUsageObservationV1,
};
use af_contracts::{EventType, RunEvent};
use af_store::store::observer::{Commit, CommitObserver, WallCommit};
use af_store::store::task::execution::read_execution_record;
use af_store::store::task::read_task_transition;
use af_store::{Cas, CasError, EventStore, Ledger};
use af_telemetry::blob::{Document, Media};
use af_telemetry::content::{Scope, event::FINDING_BODY};
use af_telemetry::level::Level;
use af_telemetry::record::{Attrs, Demand, Finding, ProblemWhat};
use af_telemetry::segment::writer::Writer;
use af_telemetry::subscriber::{self, RecordedSpan};

/// The event mirroring one committed Store event.
pub const STORE_EVENT: &str = "af.store.event";
/// The charge series: an Attempt's accounting after a committed accounting transition.
pub const USAGE_STATE: &str = "af.usage.state";
/// The wall series: one committed sidecar write.
pub const USAGE_WALL: &str = "af.usage.wall";
/// A Finding's disposition changed.
pub const FINDING_DISPOSITION: &str = "af.finding.disposition";
pub const DEMAND_OPENED: &str = "af.demand.opened";
pub const DEMAND_EVIDENCE_SATISFIED: &str = "af.demand.evidence_satisfied";
pub const DEMAND_STALE: &str = "af.demand.stale";
pub const DEMAND_WAIVED: &str = "af.demand.waived";

/// The debug-only setting that makes every mirror write fail, as a fixture's injected failure;
/// a release binary never reads it.
pub const FAIL_SETTING: &str = "AF_TEST_TELEMETRY_MIRROR_FAIL";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What one native Task log's summaries were last recorded as.
struct LedgerWatch {
    /// The projection after the last event folded in; `None` once it failed: the log's
    /// summaries stop, and the failure was reported once.
    projection: Option<Ledger>,
    through_sequence: Option<u64>,
    failed: bool,
    /// Each Finding's last recorded summary, its record position fields cleared: a summary is
    /// written again whenever any summarized field differs.
    findings: BTreeMap<String, Finding>,
    /// Each Finding's node, as its admitting event named it.
    nodes: BTreeMap<String, String>,
    /// Each Finding's admitting Worker's bound model, when the kernel recorded the binding.
    models: BTreeMap<String, String>,
    /// The digest of each Finding's body as last recorded at `content`.
    bodies: BTreeMap<String, String>,
    /// Each Demand's last recorded summary, its record position fields cleared.
    demands: BTreeMap<String, Demand>,
    satisfactions: std::collections::BTreeSet<String>,
    baseline_findings: BTreeMap<String, String>,
    baseline_demands: BTreeMap<String, String>,
    /// The Snapshot bound to each Subject the log made active, by Subject id.
    subjects: BTreeMap<String, String>,
    /// The current Round's epoch.
    epoch: Option<u64>,
}

#[derive(Default)]
struct State {
    /// The Store directory's `af.store_id`, by database path; `None` when it cannot be named.
    stores: BTreeMap<PathBuf, Option<String>>,
    /// Native Task logs, by Store and run id.
    ledgers: BTreeMap<(String, String), LedgerWatch>,
}

/// The Store commit mirror.
pub struct StoreMirror {
    /// The writer it records through: a fixture's, or `None` for the current dispatcher's.
    writer: Option<Writer>,
    /// Every write fails: a debug build's injected failure ([`FAIL_SETTING`]).
    failing: bool,
    state: Mutex<State>,
}

/// Install the mirror as the process's commit observer: every Store this process opens tells
/// it of its commits. Called once a recording command installed its subscriber; at `off`
/// nothing is installed and a commit costs one atomic load.
pub fn install_store_mirror() {
    let failing = cfg!(debug_assertions) && std::env::var_os(FAIL_SETTING).is_some();
    af_store::store::observer::install_commit_observer(Arc::new(StoreMirror {
        writer: None,
        failing,
        state: Mutex::new(State::default()),
    }));
}

impl StoreMirror {
    /// A mirror recording through `writer` on every thread (fixtures).
    pub fn with_writer(writer: Writer) -> Self {
        Self {
            writer: Some(writer),
            failing: false,
            state: Mutex::new(State::default()),
        }
    }

    fn writer(&self) -> Option<Writer> {
        match &self.writer {
            Some(writer) => Some(writer.clone()).filter(|writer| !writer.failed()),
            None => subscriber::current_writer(),
        }
    }

    fn store_id(&self, store: &EventStore) -> Option<String> {
        let path = store.database_path()?;
        let mut state = lock(&self.state);
        state
            .stores
            .entry(path.clone())
            .or_insert_with(|| {
                path.parent()
                    .and_then(|directory| af_telemetry::trace::store_id(directory).ok())
            })
            .clone()
    }

    /// A telemetry-only failure: one problem, and every run of the Store is incomplete.
    fn failed(&self, writer: &Writer, store_id: Option<&str>, reason: &str) {
        writer.problem(ProblemWhat::Observer, reason);
        super::mark_ledger_incomplete(store_id);
    }

    /// Write one event on `span`; a record the writer did not keep leaves the run incomplete.
    fn event(
        &self,
        writer: &Writer,
        store_id: &str,
        span: Option<&RecordedSpan>,
        name: &str,
        attrs: Attrs,
    ) -> bool {
        self.content_event(writer, store_id, span, name, attrs, None)
    }

    /// [`Self::event`], naming `content` as a blob at `content` (a payload, a Finding body),
    /// counted against the process's content allowance.
    fn content_event(
        &self,
        writer: &Writer,
        store_id: &str,
        span: Option<&RecordedSpan>,
        name: &str,
        attrs: Attrs,
        content: Option<(&[u8], Media)>,
    ) -> bool {
        if self.failing {
            self.failed(
                writer,
                Some(store_id),
                "the Store mirror's write failed (injected)",
            );
            return false;
        }
        let Some(span) = span else {
            self.failed(
                writer,
                Some(store_id),
                "a committed Store event had no span to be recorded on",
            );
            return false;
        };
        // Inside a Store commit: the payload's blob is published detached — appended and
        // signalled, never waiting for the consumer's flush — so a consumer busy with a released
        // harness stream delays no Store append and no lease renewal (design §6, §7.5).
        let written = match content {
            Some((bytes, media)) if writer.level().keeps_content() => writer
                .content_event_detached(&span.span_id, name, attrs, bytes, media, &Scope::Process),
            _ => writer.event(&span.span_id, name, attrs),
        };
        if written.is_none() {
            super::mark_ledger_incomplete(Some(store_id));
            return false;
        }
        true
    }

    /// The span a mirrored event goes on: the current span when it belongs to a run of this
    /// Store (or to some run, when this process opened none of this Store's), else the open run
    /// span of this Store, else the current span, else the command's.
    fn mirror_span(store_id: &str) -> Option<RecordedSpan> {
        let current = subscriber::current_recorded();
        let runs = super::store_runs(store_id);
        if let Some(current) = &current
            && let Some(trace) = &current.trace_id
            && (runs.is_empty() || runs.iter().any(|run| run.trace_id.as_ref() == Some(trace)))
        {
            return Some(current.clone());
        }
        runs.into_iter()
            .next()
            .or(current)
            .or_else(|| subscriber::command_span().and_then(|span| subscriber::recorded(&span)))
    }

    /// The span a summary or a disposition goes on: the open run span of this Store, else the
    /// mirror's.
    fn summary_span(store_id: &str) -> Option<RecordedSpan> {
        super::run_span(store_id, subscriber::current_recorded().as_ref())
            .or_else(|| Self::mirror_span(store_id))
    }

    /// The span an Attempt's accounting goes on: the Attempt's own span when this process
    /// opened it, else the run's.
    fn attempt_span(store_id: &str, attempt_id: &str) -> Option<RecordedSpan> {
        super::attempt_span(attempt_id).or_else(|| Self::summary_span(store_id))
    }

    fn mirror(&self, writer: &Writer, store_id: &str, events: &[RunEvent]) {
        let span = Self::mirror_span(store_id);
        for event in events {
            let payload = serde_json::to_string(&event.payload).unwrap_or_default();
            let mut attrs = Attrs::new()
                .id("af.store_id", store_id)
                .id("af.run_id", event.run_id.clone())
                .enumerated("af.store.event.type", event.event_type.as_str())
                .int(
                    "af.store.event.sequence",
                    i64::try_from(event.sequence).unwrap_or(i64::MAX),
                )
                .id("af.store.event.id", event.event_id.clone());
            if let Some(causation) = &event.causation_id {
                attrs = attrs.id("af.store.event.causation_id", causation.clone());
            }
            attrs = attrs
                .ids("artifact_refs", event.artifact_refs.clone())
                .id(
                    "af.store.event.payload_digest",
                    af_telemetry::trace::sha256_digest(payload.as_bytes()),
                )
                .int(
                    "af.store.event.payload_bytes",
                    i64::try_from(payload.len()).unwrap_or(i64::MAX),
                );
            let content = Some((payload.as_bytes(), Media::Kernel(Document::Record)));
            if !self.content_event(writer, store_id, span.as_ref(), STORE_EVENT, attrs, content) {
                return;
            }
        }
    }

    // --------------------------------------------------------------------------------------
    // the charge series

    fn accounting(&self, writer: &Writer, store_id: &str, commit: &Commit<'_>) {
        let Some(execution) = commit.task.and_then(|task| task.execution.as_ref()) else {
            return;
        };
        for event in commit.events {
            if event.event_type != EventType::TaskTransitionV6 {
                continue;
            }
            let Ok(transition) = read_task_transition(event) else {
                continue;
            };
            let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                continue;
            };
            let record = match read_execution_record(commit.cas, &record_id) {
                Ok(decoded) => decoded.record,
                Err(error) => {
                    self.failed(
                        writer,
                        Some(store_id),
                        &format!("a committed execution record cannot be read: {error}"),
                    );
                    continue;
                }
            };
            let (attempt_id, source, usage_id, raw) = match record {
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id,
                    usage_id,
                    raw_artifact_ids,
                    ..
                } => (
                    attempt_id,
                    "usage_observed",
                    Some(usage_id),
                    raw_artifact_ids,
                ),
                TaskExecutionRecordV1::Settled {
                    attempt_id,
                    usage_id,
                    raw_artifact_ids,
                    ..
                } => (attempt_id, "settled", usage_id, raw_artifact_ids),
                TaskExecutionRecordV1::Released { attempt_id, .. } => {
                    (attempt_id, "released", None, Vec::new())
                }
                _ => continue,
            };
            let Some(row) = execution
                .attempt_accounting()
                .into_iter()
                .find(|row| row.attempt_id == attempt_id)
            else {
                self.failed(
                    writer,
                    Some(store_id),
                    "a committed accounting transition names an Attempt the projection lacks",
                );
                continue;
            };
            let usage = usage_id
                .as_deref()
                .map(|id| token_usage(commit.cas, id))
                .transpose();
            let read = native_observation(commit.cas, &raw);
            // A raw artifact the CAS cannot read is the observer's failure: the snapshot keeps
            // the projection's figures without the observation it may have held.
            for (artifact, why) in &read.unreadable {
                self.failed(
                    writer,
                    Some(store_id),
                    &format!(
                        "the raw artifact {artifact} of the committed {source} record at Task \
                         log sequence {} cannot be read ({why}); its native usage observation \
                         is not recorded",
                        event.sequence
                    ),
                );
            }
            let (usage, native) = match (usage, read.observation) {
                (Ok(usage), Ok(native)) => (usage, native),
                (Err(reason), _) | (_, Err(reason)) => {
                    self.failed(writer, Some(store_id), &reason);
                    continue;
                }
            };
            let mut attrs = Attrs::new()
                .id("af.store_id", store_id)
                .id("af.run_id", event.run_id.clone())
                .id("af.attempt_id", attempt_id.clone())
                .id("af.node_id", row.reservation.node.clone())
                .enumerated("af.usage.source", source)
                .int(
                    "af.usage.sequence",
                    i64::try_from(event.sequence).unwrap_or(i64::MAX),
                )
                .decimal("af.usage.chargeable_tokens", row.charged_tokens.to_string());
            attrs = counters(attrs, "af.usage", usage.as_ref());
            attrs = match row.unknown_usage {
                Some(cause) => attrs
                    .enumerated("af.usage.observation", "unknown")
                    .enumerated("af.usage.unknown_cause", cause.as_str()),
                None if native
                    .as_ref()
                    .is_some_and(|native| !native.charge_complete) =>
                {
                    attrs.enumerated("af.usage.observation", "incomplete")
                }
                None => attrs.enumerated("af.usage.observation", "complete"),
            };
            attrs = native_attrs(attrs, native.as_ref());
            let span = Self::attempt_span(store_id, &attempt_id);
            self.event(writer, store_id, span.as_ref(), USAGE_STATE, attrs);
        }
    }

    // --------------------------------------------------------------------------------------
    // Finding and Demand summaries

    /// Current complete native Round outputs in one committed Task event.
    fn native_rounds(cas: &Cas, event: &RunEvent) -> Result<(u64, Vec<String>), String> {
        use af_contracts::task::execution::{TASK_OUTPUT_V1, TaskOutputV1};
        use af_contracts::task::review::{
            ReviewConclusionV1, TASK_REVIEW_ROUND_V1, TaskReviewRoundV1,
        };
        if event.event_type != EventType::TaskTransitionV6 {
            return Ok((0, vec![]));
        }
        let transition = read_task_transition(event).map_err(|e| e.to_string())?;
        let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
            return Ok((transition.epoch, vec![]));
        };
        let record = read_execution_record(cas, &record_id).map_err(|e| e.to_string())?;
        let TaskExecutionRecordV1::Published { output_id, .. } = record.record else {
            return Ok((transition.epoch, vec![]));
        };
        let output = cas.get_artifact(&output_id).map_err(|e| e.to_string())?;
        if output.artifact_type != TASK_OUTPUT_V1 {
            return Err("Published output has another type".into());
        }
        let output: TaskOutputV1 =
            serde_json::from_value(output.payload).map_err(|e| e.to_string())?;
        output.validate()?;
        let mut ids = Vec::new();
        for port in output
            .outputs
            .values()
            .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
        {
            for id in &port.artifact_ids {
                let record = cas.get_artifact(id).map_err(|e| e.to_string())?;
                if record.artifact_type != TASK_REVIEW_ROUND_V1 {
                    return Err("Native Round output has another type".into());
                }
                let round: TaskReviewRoundV1 =
                    serde_json::from_value(record.payload).map_err(|e| e.to_string())?;
                round.validate()?;
                if round.conclusion != ReviewConclusionV1::Incomplete {
                    ids.push(id.clone());
                }
            }
        }
        Ok((transition.epoch, ids))
    }

    fn summaries(&self, writer: &Writer, store_id: &str, commit: &Commit<'_>) {
        for trigger in commit.events {
            match Self::native_rounds(commit.cas, trigger) {
                Ok((_, ids)) if ids.is_empty() => continue,
                Ok(_) => {}
                Err(reason) => {
                    self.failed(writer, Some(store_id), &reason);
                    continue;
                }
            }
            let mut state = lock(&self.state);
            let watch = state
                .ledgers
                .entry((store_id.into(), trigger.run_id.clone()))
                .or_insert_with(|| LedgerWatch {
                    projection: None,
                    through_sequence: None,
                    failed: false,
                    findings: BTreeMap::new(),
                    nodes: BTreeMap::new(),
                    models: BTreeMap::new(),
                    bodies: BTreeMap::new(),
                    demands: BTreeMap::new(),
                    satisfactions: Default::default(),
                    baseline_findings: Default::default(),
                    baseline_demands: Default::default(),
                    subjects: BTreeMap::new(),
                    epoch: None,
                });
            if watch.failed
                || watch
                    .through_sequence
                    .is_some_and(|s| s >= trigger.sequence)
            {
                continue;
            }
            // A post-commit callback can arrive late. Catch up the exact committed prefix in
            // sequence order before comparing summaries; never roll the watch back to a late
            // callback or miss a subsequent A -> B -> A disposition.
            let mut apply = || -> Result<(), String> {
                let first = watch.through_sequence.map_or(0, |s| s + 1);
                let events = commit
                    .store
                    .replay_from(&trigger.run_id, first)
                    .map_err(|e| e.to_string())?;
                let span = Self::summary_span(store_id);
                for event in events.iter().take_while(|e| e.sequence <= trigger.sequence) {
                    let (epoch, ids) = Self::native_rounds(commit.cas, event)?;
                    for id in ids {
                        let ledger = crate::task::review::ReviewTaskDomain::recorded_ledger(
                            commit.cas, &id,
                        )?;
                        let models = native_models(commit.cas, &id)?;
                        if watch.projection.is_none() {
                            let round: af_contracts::task::review::TaskReviewRoundV1 =
                                serde_json::from_value(
                                    commit
                                        .cas
                                        .get_artifact(&id)
                                        .map_err(|e| e.to_string())?
                                        .payload,
                                )
                                .map_err(|e| e.to_string())?;
                            if let Some(port) =
                                round.invocation.inputs.get("decisions").filter(|port| {
                                    port.artifact_type
                                        == af_contracts::task::review_decisions::REVIEW_DECISIONS_V1
                                })
                            {
                                let batch: af_contracts::task::review_decisions::ReviewDecisionsV1 =
                                    serde_json::from_value(
                                        commit
                                            .cas
                                            .get_artifact(&port.artifact_ids[0])
                                            .map_err(|e| e.to_string())?
                                            .payload,
                                    )
                                    .map_err(|e| e.to_string())?;
                                let prior = crate::task::review::ReviewTaskDomain::recorded_ledger(
                                    commit.cas,
                                    &batch.review_id,
                                )?;
                                watch.baseline_findings = prior
                                    .findings()
                                    .into_iter()
                                    .map(|f| (f.key.clone(), f.status.as_str().into()))
                                    .collect();
                                watch.baseline_demands = prior
                                    .demand_views()
                                    .into_iter()
                                    .map(|d| (d.demand_id, enum_text(&d.status)))
                                    .collect();
                                let (_, satisfaction_ids, _) =
                                    prior.demand_reduction_artifact_ids();
                                watch.satisfactions.extend(satisfaction_ids);
                            } else if let Some(port) = round.invocation.inputs.get("history") {
                                // recorded_ledger above recomputed this Round and its exact
                                // prior history. A new discovery Task must not re-emit any
                                // inherited satisfaction/reuse, even when it remains current
                                // or a waiver wins the final view. Read the checked prior Set's
                                // identities without rebuilding that Ledger a second time.
                                let history: af_contracts::task::review::ReviewHistoryV1 =
                                    serde_json::from_value(
                                        commit
                                            .cas
                                            .get_artifact(&port.artifact_ids[0])
                                            .map_err(|e| e.to_string())?
                                            .payload,
                                    )
                                    .map_err(|e| e.to_string())?;
                                if let af_contracts::task::review::ReviewHistoryV1::Recorded {
                                    demand_set_id,
                                    ..
                                } = history
                                {
                                    let record = commit
                                        .cas
                                        .get_artifact(&demand_set_id)
                                        .map_err(|e| e.to_string())?;
                                    if record.artifact_type != af_contracts::contract::DEMAND_SET_V1
                                    {
                                        return Err(
                                            "Review history lost its current Demand Set".into()
                                        );
                                    }
                                    let prior: af_contracts::DemandSetV1 =
                                        serde_json::from_value(record.payload)
                                            .map_err(|e| e.to_string())?;
                                    watch.satisfactions.extend(prior.satisfaction_artifact_ids);
                                }
                            }
                        }
                        for finding in ledger.findings() {
                            if let Some(report) = finding.reports.last()
                                && let Ok(record) = commit.cas.get_artifact(&report.report_id)
                                && let af_contracts::Producer::Attempt {
                                    node_id,
                                    attempt_id,
                                    ..
                                } = record.producer
                            {
                                watch.nodes.insert(finding.key.clone(), node_id.clone());
                                let model = match models.get(&(node_id.clone(), attempt_id.clone()))
                                {
                                    Some(model) => model.clone(),
                                    None => super::bound_model(Some(&attempt_id), &node_id),
                                };
                                match model {
                                    Some(model) => {
                                        watch.models.insert(finding.key.clone(), model);
                                    }
                                    None => {
                                        watch.models.remove(&finding.key);
                                    }
                                }
                            }
                        }
                        watch.epoch = Some(epoch);
                        watch.projection = Some(ledger);
                        self.record_changes(
                            writer,
                            store_id,
                            span.as_ref(),
                            &event.run_id,
                            event,
                            commit.cas,
                            watch,
                        );
                    }
                    watch.through_sequence = Some(event.sequence);
                }
                Ok(())
            };
            if let Err(reason) = apply() {
                watch.failed = true;
                watch.projection = None;
                self.failed(writer, Some(store_id), &reason);
            }
        }
    }

    /// Record what the committed native Round changed: a `finding` summary for every Finding
    /// whose canonical summarized fields changed, with a disposition when its status did;
    /// the Demand lifecycle
    /// events it caused; and a `demand` summary for every Demand whose summarized fields
    /// changed. Every summary and lifecycle record carries the event's log and committed
    /// sequence, by which a reader orders them.
    #[allow(clippy::too_many_arguments)]
    fn record_changes(
        &self,
        writer: &Writer,
        store_id: &str,
        span: Option<&RecordedSpan>,
        log: &str,
        event: &RunEvent,
        cas: &Cas,
        watch: &mut LedgerWatch,
    ) {
        let Some(projection) = &watch.projection else {
            return;
        };
        let ledger = projection;
        let baseline_findings = std::mem::take(&mut watch.baseline_findings);
        let baseline_demands = std::mem::take(&mut watch.baseline_demands);
        let sequence = i64::try_from(event.sequence).unwrap_or(i64::MAX);
        let base = || {
            Attrs::new()
                .id("af.store_id", store_id)
                .id("af.run_id", log)
                .int("af.store.event.sequence", sequence)
        };
        // The Snapshot bound to the Subject this log made active, kept for every Demand of it.
        if let (Some(subject), Some(snapshot)) =
            (ledger.active_subject_id(), ledger.active_head_snapshot_id())
        {
            watch
                .subjects
                .insert(subject.to_string(), snapshot.to_string());
        }
        let span_id = span.map(|span| span.span_id.clone()).unwrap_or_default();
        let mut findings = Vec::new();
        let keeps_content = writer.level().keeps_content();
        for finding in ledger.findings() {
            let report = finding.reports.last();
            let summary = Finding {
                seq: 0,
                schema: String::new(),
                span_id: String::new(),
                log: String::new(),
                sequence: 0,
                key: finding.key.clone(),
                severity: enum_text(&finding.severity),
                effective_severity: finding
                    .convergence_severity
                    .map_or_else(|| "out".to_string(), |severity| enum_text(&severity)),
                status: finding.status.as_str().to_string(),
                scope: finding.convergence_scope_label().to_string(),
                file: Some(finding.file.clone()).filter(|file| !file.is_empty()),
                line: finding.line.and_then(|line| u64::try_from(line).ok()),
                title: finding.title.clone(),
                report_artifact_id: report
                    .map(|report| {
                        report
                            .artifact_id
                            .clone()
                            .unwrap_or_else(|| report.report_id.clone())
                    })
                    .unwrap_or_default(),
                node_id: watch.nodes.get(&finding.key).cloned().unwrap_or_default(),
                worker: report
                    .map(|report| report.source.clone())
                    .unwrap_or_default(),
                model: watch.models.get(&finding.key).cloned(),
                round: report.map(|report| u64::from(report.round)),
                epoch: watch.epoch,
            };
            let previous = watch.findings.get(&finding.key);
            if previous == Some(&summary) {
                continue;
            }
            let previous_status = previous
                .map(|previous| previous.status.clone())
                .or_else(|| baseline_findings.get(&finding.key).cloned());
            watch.findings.insert(finding.key.clone(), summary.clone());
            // At `content` the body goes beside the summary whenever it is new.
            let body = (keeps_content && !finding.body.is_empty())
                .then(|| {
                    let digest = af_telemetry::trace::sha256_digest(finding.body.as_bytes());
                    (watch.bodies.get(&finding.key) != Some(&digest)).then(|| {
                        watch.bodies.insert(finding.key.clone(), digest);
                        finding.body.clone()
                    })
                })
                .flatten();
            findings.push((summary, previous_status, body));
        }
        for (summary, previous, body) in findings {
            let (key, status) = (summary.key.clone(), summary.status.clone());
            let record = Finding {
                span_id: span_id.clone(),
                log: log.to_string(),
                sequence: event.sequence,
                ..summary
            };
            if !self.summary(writer, store_id, span, |writer| writer.finding(record)) {
                return;
            }
            if let Some(body) = body {
                let attrs = base().id("af.finding.id", key.clone());
                let content = Some((body.as_bytes(), Media::Text));
                if !self.content_event(writer, store_id, span, FINDING_BODY, attrs, content) {
                    return;
                }
            }
            if let Some(previous) = previous.filter(|previous| *previous != status) {
                let attrs = base()
                    .id("af.finding.id", key)
                    .text("af.finding.status", status)
                    .text("af.finding.previous_status", previous);
                if !self.event(writer, store_id, span, FINDING_DISPOSITION, attrs) {
                    return;
                }
            }
        }
        // Native publication carries a complete checked Ledger, rather than legacy
        // EvidenceSatisfied events. Compare exact canonical satisfaction/admission identities
        // after committed-prefix catch-up; a delayed callback cannot emit them twice.
        for view in ledger.demand_views() {
            let Some(demand) = ledger.demand(&view.demand_id) else {
                continue;
            };
            // Demand status is a final projection: a later waiver in this same committed
            // batch must not erase its satisfaction. The view selects exact current Subject
            // or explicitly admitted reuse identities, excluding stale satisfactions. The
            // watch is seeded with every prior-history identity, including reuse admissions.
            for satisfaction in &demand.satisfactions {
                if view.satisfaction_ids.contains(&satisfaction.artifact_id)
                    && watch.satisfactions.insert(satisfaction.artifact_id.clone())
                {
                    let attrs = base().id("af.demand.id", view.demand_id.clone()).id(
                        "af.evidence.id",
                        satisfaction.satisfaction.evidence_id.clone(),
                    );
                    if !self.event(writer, store_id, span, DEMAND_EVIDENCE_SATISFIED, attrs) {
                        return;
                    }
                }
            }
            for admission in &demand.reuse_admissions {
                if !view.satisfaction_ids.contains(&admission.artifact_id)
                    || !watch.satisfactions.insert(admission.artifact_id.clone())
                {
                    continue;
                }
                let Some(satisfaction) = demand
                    .satisfactions
                    .iter()
                    .find(|s| s.artifact_id == admission.admission.satisfaction_id)
                else {
                    continue;
                };
                let attrs = base()
                    .id("af.demand.id", view.demand_id.clone())
                    .id(
                        "af.evidence.id",
                        satisfaction.satisfaction.evidence_id.clone(),
                    )
                    .id(
                        "af.evidence.reuse_admission_id",
                        admission.artifact_id.clone(),
                    );
                if !self.event(writer, store_id, span, DEMAND_EVIDENCE_SATISFIED, attrs) {
                    return;
                }
            }
        }
        for view in ledger.demand_views() {
            let status = enum_text(&view.status);
            // The Demand's own Subject, never the active one of a later head, with the Snapshot
            // bound to it.
            let subject_id = ledger.demand(&view.demand_id).map_or_else(
                || view.subject_id.clone(),
                |demand| demand.demand.subject_id.clone(),
            );
            let snapshot_id = subject_snapshot(cas, &mut watch.subjects, &subject_id);
            let summary = Demand {
                seq: 0,
                schema: String::new(),
                span_id: String::new(),
                log: String::new(),
                sequence: 0,
                demand_id: view.demand_id.clone(),
                kind: enum_text(&view.requirement),
                required: view.requirement == af_contracts::DemandRequirement::Required,
                subject_id,
                snapshot_id,
                status: status.clone(),
            };
            let previous = watch
                .demands
                .get(&view.demand_id)
                .map(|previous| previous.status.clone())
                .or_else(|| baseline_demands.get(&view.demand_id).cloned());
            if watch.demands.get(&view.demand_id) == Some(&summary) {
                continue;
            }
            watch
                .demands
                .insert(view.demand_id.clone(), summary.clone());
            let record = Demand {
                span_id: span_id.clone(),
                log: log.to_string(),
                sequence: event.sequence,
                ..summary
            };
            if !self.summary(writer, store_id, span, |writer| writer.demand(record)) {
                return;
            }
            if previous.is_none() {
                let attrs = base()
                    .id("af.demand.id", view.demand_id.clone())
                    .text("af.demand.status", status.clone());
                if !self.event(writer, store_id, span, DEMAND_OPENED, attrs) {
                    return;
                }
            }
            let lifecycle = match (previous.as_deref(), status.as_str()) {
                (previous, "stale") if previous != Some("stale") => Some(DEMAND_STALE),
                (previous, "waived") if previous != Some("waived") => Some(DEMAND_WAIVED),
                _ => None,
            };
            if let Some(name) = lifecycle {
                let attrs = base()
                    .id("af.demand.id", view.demand_id.clone())
                    .text("af.demand.status", status);
                if !self.event(writer, store_id, span, name, attrs) {
                    return;
                }
            }
        }
    }

    /// Write one summary record; a record the writer did not keep leaves the run incomplete.
    fn summary(
        &self,
        writer: &Writer,
        store_id: &str,
        span: Option<&RecordedSpan>,
        write: impl FnOnce(&Writer) -> Option<u64>,
    ) -> bool {
        if self.failing {
            self.failed(
                writer,
                Some(store_id),
                "the Store mirror's write failed (injected)",
            );
            return false;
        }
        if span.is_none() {
            self.failed(
                writer,
                Some(store_id),
                "a Finding or Demand summary had no run span to be recorded on",
            );
            return false;
        }
        if write(writer).is_none() {
            super::mark_ledger_incomplete(Some(store_id));
            return false;
        }
        true
    }
}

impl CommitObserver for StoreMirror {
    fn committed(&self, commit: &Commit<'_>) {
        let Some(writer) = self.writer() else {
            return;
        };
        let level = writer.level();
        if !level.records() {
            return;
        }
        let Some(store_id) = self.store_id(commit.store) else {
            self.failed(
                &writer,
                None,
                "the Store's directory cannot be named; its commits are not mirrored",
            );
            return;
        };
        if level >= Level::Spans {
            self.mirror(&writer, &store_id, commit.events);
            self.summaries(&writer, &store_id, commit);
        }
        self.accounting(&writer, &store_id, commit);
    }

    fn wall_recorded(&self, commit: &WallCommit<'_>) {
        let Some(writer) = self.writer() else {
            return;
        };
        if !writer.level().records() {
            return;
        }
        let Some(store_id) = self.store_id(commit.store) else {
            self.failed(
                &writer,
                None,
                "the Store's directory cannot be named; its sidecar is not mirrored",
            );
            return;
        };
        let wall = commit.wall;
        let mut attrs = Attrs::new()
            .id("af.store_id", store_id.clone())
            .id("af.run_id", wall.run_id.clone())
            .id("af.attempt_id", wall.attempt_id.clone())
            .id("af.node_id", wall.node_id.clone())
            .int("af.round", i64::from(wall.round))
            .int("af.epoch", i64::from(wall.epoch))
            .enumerated("af.usage.source", "sidecar")
            .int(
                "af.usage.wall.started_unix_ms",
                i64::try_from(wall.started_unix_ms).unwrap_or(i64::MAX),
            )
            .int(
                "af.usage.wall.elapsed_ms",
                i64::try_from(wall.elapsed_ms).unwrap_or(i64::MAX),
            );
        if let Some(usage) = &wall.usage {
            attrs = attrs.decimal(
                "af.usage.chargeable_tokens",
                usage.chargeable_tokens.get().to_string(),
            );
        }
        attrs = counters(attrs, "af.usage", wall.usage.as_ref());
        attrs = native_attrs(attrs, commit.observation);
        if self.failing {
            self.failed(
                &writer,
                Some(&store_id),
                "the Store mirror's write failed (injected)",
            );
            return;
        }
        let Some(span) = Self::attempt_span(&store_id, &wall.attempt_id) else {
            self.failed(
                &writer,
                Some(&store_id),
                "a sidecar write had no span to be recorded on",
            );
            return;
        };
        if writer
            .event_with_sequence(&span.span_id, USAGE_WALL, attrs, "af.usage.wall.sequence")
            .is_none()
        {
            super::mark_ledger_incomplete(Some(&store_id));
        }
    }
}

/// The Snapshot bound to `subject_id`: the one the log made active with it, else its Subject
/// artifact's head, read once from the CAS and kept; `unknown` when neither says.
fn subject_snapshot(
    cas: &Cas,
    subjects: &mut BTreeMap<String, String>,
    subject_id: &str,
) -> String {
    if let Some(snapshot) = subjects.get(subject_id) {
        return snapshot.clone();
    }
    let read = cas
        .get_json(subject_id)
        .ok()
        .and_then(|value| serde_json::from_value::<af_contracts::SubjectV1>(value).ok())
        .map(|subject| subject.head_snapshot_id)
        .filter(|snapshot| !snapshot.is_empty());
    match read {
        Some(snapshot) => {
            subjects.insert(subject_id.to_string(), snapshot.clone());
            snapshot
        }
        None => "unknown".into(),
    }
}

/// A closed enumeration's serde spelling.
fn enum_text(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

/// The `TaskTokenUsage@3` a committed record names; a record the Store committed names a
/// readable one, so a failure is the mirror's, never an absent counter.
fn token_usage(cas: &Cas, id: &str) -> Result<TaskTokenUsageV3, String> {
    let envelope = cas
        .get_artifact(id)
        .map_err(|error| format!("a committed usage record cannot be read: {error}"))?;
    serde_json::from_value(envelope.payload)
        .map_err(|error| format!("a committed usage record cannot be read: {error}"))
}

/// What a committed record's raw artifacts hold: its native usage observation, when there is
/// one and every raw artifact was read, and each raw artifact the CAS could not read, named
/// for a problem, with why.
struct NativeRead {
    observation: Result<Option<TaskUsageObservationV1>, String>,
    unreadable: Vec<(String, String)>,
}

/// Every raw artifact a committed record names, read before any answer: the first native
/// usage observation among them, and the raw artifacts the CAS holds but cannot read. When any
/// read failed the observation is withheld, since the unread artifact may have held it.
fn native_observation(cas: &Cas, raw: &[String]) -> NativeRead {
    let mut observation = Ok(None);
    let mut unreadable = Vec::new();
    for id in raw {
        let envelope = match cas.get_optional_artifact(id) {
            Ok(Some(envelope)) => envelope,
            // A raw blob, not a typed envelope.
            Ok(None) => continue,
            // The Store read every raw artifact before it committed the record, so one it
            // cannot read now is corrupt or unreadable, never a blob to pass over.
            Err(error) => {
                unreadable.push((artifact_name(id), cas_failure(&error)));
                continue;
            }
        };
        if envelope.artifact_type == TASK_USAGE_OBSERVATION_V1 && matches!(observation, Ok(None)) {
            observation = serde_json::from_value(envelope.payload)
                .map(Some)
                .map_err(|error| format!("a committed usage observation cannot be read: {error}"));
        }
    }
    if !unreadable.is_empty() && observation.is_ok() {
        observation = Ok(None);
    }
    NativeRead {
        observation,
        unreadable,
    }
}

/// A CAS id as a problem can carry it: a digest's first 16 hex digits, since the whole digest
/// is a hex run the credential rule withholds, and with it the problem's whole reason.
fn artifact_name(id: &str) -> String {
    match id.strip_prefix("sha256:") {
        Some(hex) => format!("sha256:{}…", hex.chars().take(16).collect::<String>()),
        None => "named by no digest".into(),
    }
}

/// Why the CAS could not read an object, without the digest its errors spell out.
fn cas_failure(error: &CasError) -> String {
    match error {
        CasError::Corrupt { .. } => "its stored bytes do not match its digest".into(),
        CasError::NotFound { .. } => "it is not in the CAS".into(),
        CasError::InvalidDigest(_) => "its id is no CAS digest".into(),
        CasError::Canonical(_) => "its stored bytes are not canonical".into(),
        CasError::Io(error) => format!("it cannot be read: {}", error.kind()),
    }
}

/// The exact token counters of `usage` under `prefix`, each a canonical decimal string; a
/// counter the source did not deliver is absent, never zero (ADR-0143).
fn counters(mut attrs: Attrs, prefix: &str, usage: Option<&TaskTokenUsageV3>) -> Attrs {
    let Some(usage) = usage else {
        return attrs;
    };
    for (name, value) in [
        ("input_tokens", usage.input_tokens),
        ("output_tokens", usage.output_tokens),
        ("cache_read_tokens", usage.cache_read_tokens),
        ("cache_write_tokens", usage.cache_write_tokens),
        ("reasoning_tokens", usage.reasoning_tokens),
    ] {
        if let Some(value) = value {
            attrs = attrs.decimal(&format!("{prefix}.{name}"), value.get().to_string());
        }
    }
    attrs
}

/// The separately identified native observation: its counters under `af.usage.native.*` and
/// whether it completes the charge.
// Called only after complete native reconstruction. Source attribution follows the exact
// selected Attempt and its own captured plan, including across a later human Task/process.
// No node-name fallback supplies a model for another Attempt.
fn native_models(
    cas: &Cas,
    round_id: &str,
) -> Result<BTreeMap<(String, String), Option<String>>, String> {
    use af_contracts::task::pipeline::TaskOperatorV1;
    use af_contracts::task::{
        EXECUTION_PLAN_V4, REVIEW_HISTORY_V1, TASK_REVISION_V1, TaskRevisionV1, plan::*, review::*,
    };
    use af_graph::task::{CompiledOperator, CompiledTask};
    let mut id = round_id.to_owned();
    let mut models = BTreeMap::new();
    for _ in 0..16 {
        let record = cas.get_artifact(&id).map_err(|e| e.to_string())?;
        if record.artifact_type != TASK_REVIEW_ROUND_V1 {
            return Err("Attribution lost its current native Round".into());
        }
        let round: TaskReviewRoundV1 =
            serde_json::from_value(record.payload).map_err(|e| e.to_string())?;
        let record = cas
            .get_artifact(&round.invocation.plan_id)
            .map_err(|e| e.to_string())?;
        if record.artifact_type != EXECUTION_PLAN_V4 {
            return Err("Attribution lost its original plan".into());
        }
        let plan: ExecutionPlan =
            serde_json::from_value(record.payload).map_err(|e| e.to_string())?;
        let revision = cas
            .get_artifact(&plan.task_revision_id)
            .map_err(|e| e.to_string())?;
        if revision.artifact_type != TASK_REVISION_V1 {
            return Err("Attribution lost its original Task".into());
        }
        let revision: TaskRevisionV1 =
            serde_json::from_value(revision.payload).map_err(|e| e.to_string())?;
        let run =
            af_store::store::task::task_run_id(&revision.task_id).map_err(|e| e.to_string())?;
        let graph: CompiledTask = serde_json::from_value(
            cas.get_artifact(&plan.compiled_graph_id)
                .map_err(|e| e.to_string())?
                .payload,
        )
        .map_err(|e| e.to_string())?;
        for result in round.selected_results.values() {
            let record = cas.get_artifact(result).map_err(|e| e.to_string())?;
            let af_contracts::Producer::Attempt {
                run_id,
                node_id,
                attempt_id,
            } = record.producer
            else {
                continue;
            };
            if run_id != run {
                continue;
            } // Human receipts retain another Task's originals.
            let node = graph
                .nodes
                .get(&node_id)
                .or_else(|| {
                    node_id
                        .rsplit_once(".slice")
                        .and_then(|(parent, _)| graph.nodes.get(parent))
                })
                .ok_or("Original reviewer has no captured node")?;
            let CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::Verify { slot } | TaskOperatorV1::ReviewScatter { slot, .. },
                ..
            } = &node.operator
            else {
                return Err("Original reviewer has no reserved captured slot".into());
            };
            let binding = plan
                .bindings
                .get(slot)
                .ok_or("Original reviewer lost its captured binding")?;
            let model = match &binding.execution {
                WorkerExecutionV3::Model { model, .. } => Some(model.clone()),
                WorkerExecutionV3::Command {} => None,
            };
            models.insert((node_id, attempt_id), model);
        }
        let history_id = round
            .invocation
            .inputs
            .get("history")
            .and_then(|p| p.artifact_ids.first())
            .ok_or("Attribution lost native history")?;
        let history = cas.get_artifact(history_id).map_err(|e| e.to_string())?;
        if history.artifact_type != REVIEW_HISTORY_V1 {
            return Err("Attribution changed native history type".into());
        }
        match serde_json::from_value::<ReviewHistoryV1>(history.payload)
            .map_err(|e| e.to_string())?
        {
            ReviewHistoryV1::Empty {} => return Ok(models),
            ReviewHistoryV1::Recorded {
                round_report_id, ..
            } => id = round_report_id,
        }
    }
    Err("Attribution exceeds bounded native history".into())
}

fn native_attrs(attrs: Attrs, native: Option<&TaskUsageObservationV1>) -> Attrs {
    let Some(native) = native else {
        return attrs;
    };
    let mut attrs = attrs.enumerated(
        "af.usage.native.observation",
        if native.charge_complete {
            "complete"
        } else {
            "incomplete"
        },
    );
    if let Some(reported) = &native.reported_usage {
        attrs = attrs.decimal(
            "af.usage.native.chargeable_tokens",
            reported.chargeable_tokens.get().to_string(),
        );
    }
    counters(attrs, "af.usage.native", native.reported_usage.as_ref())
}
