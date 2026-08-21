//! The composition layer: the graph driving the real nodes.
//!
//! Everything below this crate was built to be provable in isolation — capture without a
//! scheduler, scheduling without models, checks without a graph. This is where they meet, and
//! the only thing it adds is wiring. That is deliberate: if composing them required new rules,
//! the boundaries underneath would be wrong.
//!
//! One review therefore looks like this, end to end:
//!
//! ```text
//!   capture ── snapshot ──┐
//!                         v
//!            gate (checks in a sandbox) ──decision──┐
//!                                                   v
//!                    architecture ┐  performance ┐  tdd ┐   (each sandboxed, gated)
//!                                 └──────────────┴──────┴──> gather
//!                                                              │
//!                                                              v
//!                                                           ledger ──> convergence
//! ```
//!
//! A blocked gate makes every node after it unreachable, so a review that could not build
//! produces no reviewer artifacts at all — not reviewer artifacts nobody reads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use review_attempt::{
    AttemptId, AttemptLedger, Budget, BudgetLedger, Receipt, Reservation, Scope, Selection,
};
use review_check::{CheckDefinition, CheckRunner, Command, GateDecision, check_event};
use review_core::event::{
    AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1, AttemptFailedPayloadV1,
    AttemptFencedPayloadV1, AttemptReleasedPayloadV1,
};
use review_core::{
    CampaignOpenedPayloadV1, ChangeSetV1, EventType, LegacyStageOutput, MissingNodeV2,
    NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, PortArtifactsV1, RoundStartedPayloadV1,
    RunFailureReasonV2, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV2,
    RunSuppressionReasonV2, RunVerdictV2, SnapshotAffinity, SourceSnapshot, SubjectV1,
    run_report_closes_round,
};
use review_graph::{ArtifactMap, Dispatch, Node, NodeKind, NodeOutcome, PortContract, RunReport};
use review_runner::{
    MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES, ReviewerAdapter, ReviewerInputArtifact,
    ReviewerInputs, RunnerError,
};
use review_sandbox::{Mode, Sandbox};
use review_source_git::Manifest;
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ingest, Ledger, NewEvent, Verdict,
};

/// The input port a reviewer receives the campaign's prior findings on.
const PRIOR_FINDINGS_PORT: &str = "prior_findings";

/// The artifact ids a node's resolved inputs carry, dropping the port labels — for reducers
/// (gather, ledger) that consume artifacts regardless of which port delivered them.
fn artifact_ids(inputs: &ArtifactMap) -> Vec<String> {
    inputs.values().flatten().cloned().collect()
}

fn bind_single_output(node: &Node, artifacts: Vec<String>) -> Result<ArtifactMap, String> {
    let [port] = node.outputs.as_slice() else {
        return Err(format!(
            "node {} has {} output ports, but its built-in dispatcher produces one port",
            node.id,
            node.outputs.len()
        ));
    };
    Ok(BTreeMap::from([(port.name.clone(), artifacts)]))
}

fn port_artifacts(
    contracts: &[PortContract],
    artifacts: &ArtifactMap,
    subject_snapshot_id: &str,
) -> Vec<PortArtifactsV1> {
    contracts
        .iter()
        .map(|port| PortArtifactsV1 {
            port: port.name.clone(),
            artifact_type: port.artifact_type.clone(),
            cardinality: port.cardinality,
            optional: port.optional,
            snapshot_affinity: port.snapshot_affinity,
            artifact_ids: artifacts.get(&port.name).cloned().unwrap_or_default(),
            subject_snapshot_id: (port.snapshot_affinity == SnapshotAffinity::SameSubject)
                .then(|| subject_snapshot_id.to_string()),
        })
        .collect()
}

/// The run's own budget accounts, alongside the caps that opened them.
struct Budgets {
    attempt_cap: u64,
    ledger: Mutex<BudgetLedger>,
}

struct PreparedReviewerAttempt {
    attempt: AttemptId,
    reservation: Option<Reservation>,
}

/// What one whole run amounts to.
///
/// `Incomplete` exists because a partial review must never pass on the strength of the part
/// that ran: a run where any node failed or was suppressed — a blocked gate, a refused budget
/// reservation or a crashed reviewer reports *which* nodes never contributed and cannot pass
/// the gate, whatever the ledger's findings would have said. This is the owner's exhaustion
/// policy (2026-08-18): finish in-flight work, dispatch nothing new, and fail closed at the
/// verdict rather than by discarding paid work. Budget exhaustion is the terminal
/// `Fail(Exhausted)` exception: it closes the Round while naming the work that could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    Pass,
    Fail(Verdict),
    Incomplete { missing: Vec<(String, String)> },
}

/// The immutable publication boundary every event emitted by one Round execution inherits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundAuthority {
    run_id: String,
    round_event_id: String,
    round: u32,
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_id: String,
    head_snapshot_id: String,
    head_content_digest: String,
    prior_finding_set_id: String,
    subject_kind: review_core::SubjectKind,
    change_set_id: Option<String>,
}

impl RoundAuthority {
    pub fn load(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<Self, String> {
        let events = store.replay(run_id).map_err(|error| error.to_string())?;
        let opened = events
            .iter()
            .find(|event| event.event_type == EventType::CampaignOpenedV1)
            .ok_or("Round authority has no CampaignOpened@1")?;
        let opened: CampaignOpenedPayloadV1 =
            serde_json::from_value(opened.payload.clone()).map_err(|error| error.to_string())?;
        let round = events
            .iter()
            .rev()
            .find(|event| event.event_type == EventType::RoundStartedV1)
            .ok_or("Round authority has no RoundStarted@1")?;
        if round.event_id != round_event_id {
            return Err("requested Round is not the active Round epoch".into());
        }
        let payload: RoundStartedPayloadV1 =
            serde_json::from_value(round.payload.clone()).map_err(|error| error.to_string())?;
        if payload.campaign_manifest_id != opened.campaign_manifest_id {
            return Err("RoundStarted@1 does not reference the opened CampaignManifest".into());
        }
        let subject: SubjectV1 = serde_json::from_value(
            cas.get_json(&payload.subject_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        subject.validate()?;
        let change_set_id = subject.change_set_id.clone();
        if let Some(change_set_id) = &change_set_id {
            let change_set: ChangeSetV1 = serde_json::from_value(
                cas.get_json(change_set_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            change_set.validate()?;
            if change_set.base_snapshot_id
                != subject.base_snapshot_id.as_deref().unwrap_or_default()
                || change_set.head_snapshot_id != subject.head_snapshot_id
            {
                return Err("Round Change Set does not match its Subject".into());
            }
        }
        let source: SourceSnapshot = serde_json::from_value(
            cas.get_json(&subject.head_snapshot_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let source_manifest_id = source
            .artifact_manifest
            .as_deref()
            .ok_or("Round Subject SourceSnapshot has no artifact manifest")?;
        let source_manifest: Manifest = serde_json::from_value(
            cas.get_json(source_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source_manifest.content_digest() != source.content_digest {
            return Err("Round Subject manifest contradicts its SourceSnapshot".into());
        }
        let mut required = vec![
            opened.authority_snapshot_id.clone(),
            opened.campaign_manifest_id.clone(),
            payload.subject_id.clone(),
            subject.head_snapshot_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        required.extend(subject.base_snapshot_id.clone());
        required.extend(change_set_id.clone());
        for artifact in required {
            cas.get(&artifact).map_err(|error| error.to_string())?;
            if !round.artifact_refs.contains(&artifact) {
                return Err(format!(
                    "RoundStarted@1 does not publish required authority artifact `{artifact}`"
                ));
            }
        }
        Ok(Self {
            run_id: run_id.to_string(),
            round_event_id: round.event_id.clone(),
            round: payload.round,
            authority_snapshot_id: opened.authority_snapshot_id,
            campaign_manifest_id: payload.campaign_manifest_id,
            subject_id: payload.subject_id,
            head_snapshot_id: subject.head_snapshot_id,
            head_content_digest: source.content_digest,
            prior_finding_set_id: payload.prior_finding_set_id,
            subject_kind: subject.kind,
            change_set_id,
        })
    }

    fn artifact_refs(&self) -> Vec<String> {
        let refs = vec![
            self.authority_snapshot_id.clone(),
            self.campaign_manifest_id.clone(),
            self.subject_id.clone(),
            self.head_snapshot_id.clone(),
        ];
        refs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableReceipt {
    payload: NodeOutputReceiptPayloadV1,
    attempt_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedReviewer {
    attempt_id: String,
    result_artifact: String,
}

#[derive(Default)]
struct ReplayedExecution {
    invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    outputs: BTreeMap<String, DurableReceipt>,
    selected_reviewers: BTreeMap<String, SelectedReviewer>,
    gates: BTreeMap<String, GateDecision>,
    attempt_counts: BTreeMap<String, u64>,
    outstanding_attempts: Vec<(String, String, u64)>,
    committed_tokens: u64,
}

fn replay_execution(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    authority: &RoundAuthority,
) -> Result<ReplayedExecution, String> {
    let mut replayed = ReplayedExecution::default();
    let mut reservations = BTreeMap::new();
    let mut terminal_attempts = BTreeSet::new();
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    let mut round_lineage = BTreeSet::new();
    for event in &events {
        if event.event_type != EventType::RoundStartedV1 {
            continue;
        }
        let payload: RoundStartedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        if payload.round == authority.round
            && payload.campaign_manifest_id == authority.campaign_manifest_id
        {
            round_lineage.insert(event.event_id.clone());
        }
    }
    if !round_lineage.contains(&authority.round_event_id) {
        return Err("active Round is absent from its budget lineage".into());
    }

    for event in events {
        let Some(causation) = event.causation_id.as_deref() else {
            continue;
        };
        if !round_lineage.contains(causation) {
            continue;
        }
        let active_epoch = causation == authority.round_event_id;
        match event.event_type {
            EventType::NodeInvocationV1 if active_epoch => {
                let invocation: NodeInvocationPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                if event.node_id.as_deref() != Some(invocation.node.as_str()) {
                    return Err(
                        "durable node invocation metadata disagrees with its payload".into(),
                    );
                }
                if replayed
                    .invocations
                    .insert(invocation.node.clone(), invocation.clone())
                    .is_some_and(|prior| prior != invocation)
                {
                    return Err("one Round node has conflicting durable invocations".into());
                }
            }
            EventType::NodeOutputReceiptV1 if active_epoch => {
                let receipt: NodeOutputReceiptPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                if event.node_id.as_deref() != Some(receipt.node.as_str())
                    || !replayed.invocations.contains_key(&receipt.node)
                {
                    return Err("durable output receipt has no matching node invocation".into());
                }
                for port in &receipt.outputs {
                    if port.snapshot_affinity == SnapshotAffinity::SameSubject
                        && port.subject_snapshot_id.as_deref() != Some(&authority.head_snapshot_id)
                    {
                        return Err(format!(
                            "durable output `{}` has stale Subject affinity",
                            port.port
                        ));
                    }
                    for artifact in &port.artifact_ids {
                        cas.get(artifact).map_err(|error| error.to_string())?;
                    }
                }
                if replayed
                    .outputs
                    .insert(
                        receipt.node.clone(),
                        DurableReceipt {
                            payload: receipt,
                            attempt_id: event.attempt_id,
                        },
                    )
                    .is_some()
                {
                    return Err("one Round node has duplicate durable output receipts".into());
                }
            }
            EventType::GateDecisionV1 if active_epoch => {
                let node = event.node_id.ok_or("GateDecision@1 has no node ID")?;
                let decision: GateDecision =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                replayed.gates.insert(node, decision);
            }
            EventType::AttemptDispatchedV1 => {
                let payload: AttemptDispatchedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let node = event.node_id.ok_or("AttemptDispatched@1 has no node ID")?;
                if active_epoch {
                    *replayed.attempt_counts.entry(node.clone()).or_default() += 1;
                }
                let attempt = event
                    .attempt_id
                    .ok_or("AttemptDispatched@1 has no attempt ID")?;
                if reservations
                    .insert(attempt, (node, payload.reserved.unwrap_or(0), active_epoch))
                    .is_some()
                {
                    return Err("attempt has duplicate durable dispatch events".into());
                }
            }
            EventType::AttemptAdmittedV1 => {
                let payload: AttemptAdmittedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let node = event.node_id.ok_or("AttemptAdmitted@1 has no node ID")?;
                let attempt = event
                    .attempt_id
                    .ok_or("AttemptAdmitted@1 has no attempt ID")?;
                if !terminal_attempts.insert(attempt.clone()) {
                    if payload.selection == "quarantined" {
                        continue;
                    }
                    return Err("attempt has duplicate selected terminal lifecycle events".into());
                }
                reservations.remove(&attempt);
                replayed.committed_tokens = replayed
                    .committed_tokens
                    .checked_add(payload.cost_tokens)
                    .ok_or("replayed token charge overflow")?;
                if active_epoch && payload.selection == "selected" {
                    let result_artifact = payload
                        .result_artifact
                        .ok_or("selected attempt has no result artifact")?;
                    let provenance_artifact = payload
                        .provenance_artifact
                        .ok_or("selected attempt has no provenance artifact")?;
                    cas.get(&result_artifact)
                        .map_err(|error| error.to_string())?;
                    cas.get(&provenance_artifact)
                        .map_err(|error| error.to_string())?;
                    if replayed
                        .selected_reviewers
                        .insert(
                            node,
                            SelectedReviewer {
                                attempt_id: attempt,
                                result_artifact,
                            },
                        )
                        .is_some()
                    {
                        return Err("reviewer has multiple selected attempts".into());
                    }
                }
            }
            EventType::AttemptFailedV1 => {
                let payload: AttemptFailedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let attempt = event
                    .attempt_id
                    .ok_or("terminal attempt event has no attempt ID")?;
                if !terminal_attempts.insert(attempt.clone()) {
                    return Err("attempt has duplicate terminal lifecycle events".into());
                }
                reservations.remove(&attempt);
                replayed.committed_tokens = replayed
                    .committed_tokens
                    .checked_add(payload.charged.unwrap_or(0))
                    .ok_or("replayed token charge overflow")?;
            }
            EventType::AttemptFencedV1 => {
                let payload: AttemptFencedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let attempt = event
                    .attempt_id
                    .ok_or("terminal attempt event has no attempt ID")?;
                if !terminal_attempts.insert(attempt.clone()) {
                    return Err("attempt has duplicate terminal lifecycle events".into());
                }
                reservations.remove(&attempt);
                replayed.committed_tokens = replayed
                    .committed_tokens
                    .checked_add(payload.charged.unwrap_or(0))
                    .ok_or("replayed token charge overflow")?;
            }
            EventType::AttemptReleasedV1 => {
                let _: AttemptReleasedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let attempt = event
                    .attempt_id
                    .ok_or("AttemptReleased@1 has no attempt ID")?;
                if !terminal_attempts.insert(attempt.clone()) {
                    return Err("attempt has duplicate terminal lifecycle events".into());
                }
                reservations.remove(&attempt);
            }
            _ => {}
        }
    }
    for (attempt, (node, reserved, active_epoch)) in reservations {
        replayed.committed_tokens = replayed
            .committed_tokens
            .checked_add(reserved)
            .ok_or("replayed token charge overflow")?;
        if active_epoch {
            replayed
                .outstanding_attempts
                .push((node, attempt, reserved));
        }
    }
    for (node, receipt) in &replayed.outputs {
        let Some(attempt) = &receipt.attempt_id else {
            continue;
        };
        let selected = replayed
            .selected_reviewers
            .get(node)
            .ok_or("reviewer receipt has no selected admitted attempt")?;
        let output_artifacts: Vec<&String> = receipt
            .payload
            .outputs
            .iter()
            .flat_map(|port| &port.artifact_ids)
            .collect();
        if attempt != &selected.attempt_id
            || output_artifacts.len() != 1
            || output_artifacts[0] != &selected.result_artifact
        {
            return Err("reviewer receipt contradicts its selected admitted result".into());
        }
    }
    Ok(replayed)
}

impl RunVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, RunVerdict::Pass)
    }
}

/// Combine what ran with what converged. Completeness is checked first: convergence is a
/// statement about the findings that exist, and says nothing about the reviewers that never
/// produced any.
pub fn run_verdict(report: &RunReport, convergence: &Convergence) -> RunVerdict {
    let missing: Vec<(String, String)> = report
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| match outcome {
            NodeOutcome::Completed { .. } => None,
            NodeOutcome::Failed { error } => Some((id.clone(), error.clone())),
            NodeOutcome::Suppressed { reason } => Some((id.clone(), format!("{reason:?}"))),
        })
        .collect();
    if missing
        .iter()
        .any(|(_, reason)| reason.contains("run budget exhausted"))
    {
        return RunVerdict::Fail(Verdict::Exhausted);
    }
    if !missing.is_empty() {
        return RunVerdict::Incomplete { missing };
    }
    // A gate can be intentionally observational and gate no downstream node. Its blocked
    // decision still prevents a pass even though there is then no suppressed outcome to make
    // the run incomplete.
    if !report.blocked_gates.is_empty() {
        return RunVerdict::Fail(Verdict::NotConverged);
    }
    match convergence.verdict {
        Verdict::Converged => RunVerdict::Pass,
        other => RunVerdict::Fail(other),
    }
}

/// What a pipeline needs to run one generation.
pub struct Kernel<'a> {
    cas: &'a Cas,
    store: Mutex<&'a mut EventStore>,
    run_id: String,
    /// The immutable subject. Every node is materialized from this, so they all inspect the
    /// same content by construction rather than by discipline.
    snapshot: Manifest,
    subject: review_core::SubjectKind,
    authority: RoundAuthority,
    checks: Vec<CheckDefinition>,
    reviewers: BTreeMap<String, Box<dyn ReviewerAdapter>>,
    attempts: Mutex<AttemptLedger>,
    budgets: Option<Budgets>,
    /// Retries per node, spent only on timeouts. A retry is a new attempt: it fences its
    /// predecessor and reserves its own budget.
    timeout_retries: u32,
    /// Gate decisions by gate node. Keyed, so two gates in one pipeline never share a verdict.
    gates: Mutex<BTreeMap<String, GateDecision>>,
    /// The campaign's prior findings, as a CAS artifact every reviewer attempt receives —
    /// labelled data resolved by the kernel, which is what makes round N+1 a re-examination
    /// of round N's claims instead of a fresh look that happens to share a repository.
    prior_findings: Option<String>,
    /// Reviewer-thread events, held until a barrier flushes them in canonical order. Reviewers
    /// run concurrently, and appending from a worker thread would assign sequences — and the
    /// event IDs derived from them — in whatever order the threads happened to reach the log,
    /// so two identical runs would produce different, incomparable logs. Each `(node, seq)`
    /// keeps a node's own events in the order it emitted them; the flush sorts by that key.
    reviewer_events: Mutex<Vec<((String, u64), NewEvent)>>,
    reviewer_event_seq: Mutex<u64>,
    /// First attempts are reserved, assigned, and durably dispatched by the scheduler thread in
    /// plan order before any external model call starts. The worker removes its prepared entry.
    prepared_attempts: Mutex<BTreeMap<String, PreparedReviewerAttempt>>,
    /// The snapshot materialized once, cloned per sandbox. Built lazily on the first sandbox
    /// request — the gate's — so a run that never reaches a sandbox never pays for it.
    template: Mutex<Option<std::sync::Arc<review_sandbox::SandboxTemplate>>>,
    /// One kernel generation has exactly one durable conclusion.
    report_published: Mutex<bool>,
    replayed_invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    replayed_outputs: BTreeMap<String, DurableReceipt>,
    reviewer_selections: Mutex<BTreeMap<String, SelectedReviewer>>,
    replayed_spent: u64,
}

impl<'a> Kernel<'a> {
    fn new(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        let run_id = run_id.into();
        if authority.run_id != run_id {
            return Err("Round authority belongs to a different Campaign run".into());
        }
        if snapshot.content_digest() != authority.head_content_digest {
            return Err("executed manifest does not match the Round Subject Snapshot".into());
        }
        if subject != authority.subject_kind {
            return Err("pipeline Subject kind disagrees with Round authority".into());
        }
        let replayed = replay_execution(store, cas, &run_id, &authority)?;
        if !replayed.outstanding_attempts.is_empty() {
            let events: Vec<NewEvent> = replayed
                .outstanding_attempts
                .iter()
                .map(|(node, attempt, charged)| {
                    let mut event = NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::to_value(AttemptFencedPayloadV1 {
                            reason: "process ended before attempt publication".into(),
                            charged: Some(*charged),
                        })
                        .expect("typed attempt fence"),
                    )
                    .node(node)
                    .attempt(attempt)
                    .caused_by(authority.round_event_id.clone())
                    .correlating(authority.subject_id.clone());
                    event.artifact_refs.extend(authority.artifact_refs());
                    event
                })
                .collect();
            store
                .append_batch(&run_id, cas, &events)
                .map_err(|error| error.to_string())?;
        }
        let attempts =
            AttemptLedger::scoped(&authority.round_event_id, replayed.attempt_counts.clone());
        let prior_findings = Some(authority.prior_finding_set_id.clone());
        Ok(Kernel {
            cas,
            store: Mutex::new(store),
            run_id,
            snapshot,
            subject,
            authority,
            checks: Vec::new(),
            reviewers: BTreeMap::new(),
            attempts: Mutex::new(attempts),
            budgets: None,
            timeout_retries: 1,
            gates: Mutex::new(replayed.gates),
            prior_findings,
            reviewer_events: Mutex::new(Vec::new()),
            reviewer_event_seq: Mutex::new(0),
            prepared_attempts: Mutex::new(BTreeMap::new()),
            template: Mutex::new(None),
            report_published: Mutex::new(false),
            replayed_invocations: replayed.invocations,
            replayed_outputs: replayed.outputs,
            reviewer_selections: Mutex::new(replayed.selected_reviewers),
            replayed_spent: replayed.committed_tokens,
        })
    }

    /// Construct a kernel for the declared Subject kind. The legacy constructor above is
    /// explicitly whole-tree; callers carrying a pipeline definition use this entry point so
    /// an unsupported diff cannot silently execute with whole-tree semantics.
    fn for_subject(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        Kernel::new(cas, store, run_id, snapshot, subject, authority)
    }

    /// Compose execution from the exact validated pipeline definition.
    pub fn from_loaded(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        loaded: &review_config::Loaded,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        Kernel::for_subject(
            cas,
            store,
            run_id,
            snapshot,
            loaded.subject_kind(),
            authority,
        )
    }

    pub fn with_checks(mut self, checks: Vec<CheckDefinition>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_reviewer(mut self, node_id: impl Into<String>, command: Command) -> Self {
        self.reviewers.insert(node_id.into(), Box::new(command));
        self
    }

    /// Bind a model-backed (or any other) adapter to a node, behind the same contract the
    /// `command` reviewers use.
    pub fn with_adapter(
        mut self,
        node_id: impl Into<String>,
        adapter: Box<dyn ReviewerAdapter>,
    ) -> Self {
        self.reviewers.insert(node_id.into(), adapter);
        self
    }

    /// Cap the run. Reservation before every dispatch; a dispatch that cannot reserve does not
    /// happen, and the refusal names the scope that said no.
    pub fn with_budgets(mut self, attempt_cap: u64, run_cap: u64) -> Self {
        self.budgets = Some(Budgets {
            attempt_cap,
            ledger: Mutex::new(
                BudgetLedger::default()
                    .with_limit(Scope::Run, Budget::of(run_cap))
                    .with_committed(Scope::Run, self.replayed_spent),
            ),
        });
        self
    }

    /// Tokens committed so far, across every attempt including fenced ones. `None` when the
    /// run is uncapped.
    pub fn spent(&self) -> Option<u64> {
        self.budgets.as_ref().map(|b| {
            b.ledger
                .lock()
                .expect("budget ledger")
                .committed(&Scope::Run)
        })
    }

    /// Every attempt this run made, quarantines included — the operator's view.
    pub fn attempts(&self) -> AttemptLedger {
        self.attempts.lock().expect("attempt ledger").clone()
    }

    /// The decision a gate node reached, if it ran.
    pub fn gate_decision(&self, node_id: &str) -> Option<GateDecision> {
        self.gates.lock().expect("gates").get(node_id).cloned()
    }

    /// The ledger as it stands, rebuilt from the log rather than accumulated in memory.
    pub fn ledger(&self) -> Ledger {
        Ledger::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .expect("replay")
    }

    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        self.ledger().convergence(policy)
    }

    /// Append one event to the run's log. Everything the kernel decides goes through here:
    /// the log is the authority a run is rebuilt from, so a decision it never saw is a
    /// decision that, on replay, never happened.
    fn bind_authority(&self, mut event: NewEvent) -> NewEvent {
        if event.causation_id.is_none() {
            event.causation_id = Some(self.authority.round_event_id.clone());
        }
        if event.correlation_id.is_none() {
            event.correlation_id = Some(self.authority.subject_id.clone());
        }
        for artifact in self.authority.artifact_refs() {
            if !event.artifact_refs.contains(&artifact) {
                event.artifact_refs.push(artifact);
            }
        }
        event
    }

    fn append(&self, event: NewEvent) -> Result<(), String> {
        let event = self.bind_authority(event);
        self.store
            .lock()
            .expect("event store")
            .append(&self.run_id, self.cas, event)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn append_batch(&self, events: &[NewEvent]) -> Result<(), String> {
        let events: Vec<NewEvent> = events
            .iter()
            .cloned()
            .map(|event| self.bind_authority(event))
            .collect();
        self.store
            .lock()
            .expect("event store")
            .append_batch(&self.run_id, self.cas, &events)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn prepare_reviewer_attempt(
        &self,
        node_id: &str,
        prior_findings_artifact: Option<&String>,
        timeouts: &[String],
    ) -> Result<PreparedReviewerAttempt, String> {
        let reservation = match &self.budgets {
            Some(budgets) => Some(
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .reserve(
                        &[Scope::Node(node_id.to_string()), Scope::Run],
                        budgets.attempt_cap,
                    )
                    .map_err(|error| {
                        if timeouts.is_empty() {
                            format!("never dispatched: {error}")
                        } else {
                            format!("{}; retry refused: {error}", timeouts.join("; "))
                        }
                    })?,
            ),
            None => None,
        };
        let attempt = self
            .attempts
            .lock()
            .expect("attempt ledger")
            .dispatch(node_id);
        let event = NewEvent::new(
            EventType::AttemptDispatchedV1,
            serde_json::json!({
                "reserved": reservation.as_ref().map(|reservation| reservation.amount),
                "prior_findings": prior_findings_artifact,
            }),
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(prior_findings_artifact.cloned().into_iter().collect());
        if let Err(error) = self.append(event) {
            if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .release(reservation);
            }
            self.attempts.lock().expect("attempt ledger").fence(node_id);
            return Err(error);
        }
        Ok(PreparedReviewerAttempt {
            attempt,
            reservation,
        })
    }

    fn release_prepared_attempt(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        reservation: Option<&Reservation>,
        error: &str,
    ) -> Result<(), String> {
        if let (Some(budgets), Some(reservation)) = (&self.budgets, reservation) {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .release(reservation);
        }
        self.attempts.lock().expect("attempt ledger").fence(node_id);
        self.append(
            NewEvent::new(
                EventType::AttemptReleasedV1,
                serde_json::json!({
                    "error": error,
                    "released": reservation.map(|reservation| reservation.amount),
                }),
            )
            .node(node_id)
            .attempt(attempt.to_string()),
        )
    }

    fn fail_started_attempt(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        reservation: Option<&Reservation>,
        error: &str,
        charged: u64,
    ) -> Result<(), String> {
        if let (Some(budgets), Some(reservation)) = (&self.budgets, reservation) {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .charge(reservation, charged);
        }
        self.attempts
            .lock()
            .expect("attempt ledger")
            .charge(attempt, charged);
        self.append(
            NewEvent::new(
                EventType::AttemptFailedV1,
                serde_json::json!({ "error": error, "charged": charged }),
            )
            .node(node_id)
            .attempt(attempt.to_string()),
        )
    }

    /// Hold a reviewer-thread event for the canonical-order flush. See `reviewer_events`.
    fn buffer_reviewer_event(&self, node_id: &str, event: NewEvent) {
        let mut seq = self.reviewer_event_seq.lock().expect("reviewer event seq");
        let key = (node_id.to_string(), *seq);
        *seq += 1;
        self.reviewer_events
            .lock()
            .expect("reviewer events")
            .push((key, event));
    }

    /// Append every buffered reviewer event, sorted by `(node, emission order)`, then clear the
    /// buffer. Called at the gather barrier — every reviewer has finished by then — so the log
    /// is a function of the pipeline, not of thread timing. Idempotent: a second call on an
    /// already-drained buffer is a no-op, which is why a gather-less pipeline can still flush
    /// from the run driver.
    pub fn flush_reviewer_events(&self) -> Result<(), String> {
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        pending.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<NewEvent> = pending.iter().map(|(_, event)| event.clone()).collect();
        self.append_batch(&events)?;
        pending.clear();
        Ok(())
    }

    /// Durably record what became of every node, and the verdict derived from that. Without
    /// this the log holds the attempts but not the run's conclusion — an operator resuming
    /// from the log alone could not say what the review decided.
    pub fn publish_report(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
    ) -> Result<RunVerdict, String> {
        let mut published = self.report_published.lock().expect("report published");
        if *published {
            return Err("this kernel generation already published its conclusion".to_string());
        }
        let prior_conclusion = {
            let store = self.store.lock().expect("event store");
            let mut conclusion = false;
            for event in store
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
            {
                match event.event_type {
                    EventType::GenerationAdvancedV1 => conclusion = false,
                    EventType::RunReportV1 | EventType::RunReportV2 => {
                        conclusion = run_report_closes_round(&event)
                            .map_err(|error| error.to_string())?
                            .unwrap_or(false);
                    }
                    _ => {}
                }
            }
            conclusion
        };
        if prior_conclusion {
            return Err("this campaign generation already has a durable conclusion".to_string());
        }
        // The guaranteed flush point. `run_gather` flushes when it runs — the ordinary case,
        // and the one that keeps attempt events ahead of the findings — but a gather that was
        // suppressed (a failed reviewer upstream) or a pipeline with no gather node never
        // reaches it, and the buffered attempts, charges included, would be lost. Every run
        // ends with a report, so flushing here records the paid work no matter the graph.
        self.flush_reviewer_events()?;
        let verdict = run_verdict(report, &self.convergence(policy));
        let outcomes: Vec<RunNodeReportV2> = report
            .outcomes
            .iter()
            .map(|(id, outcome)| {
                let outcome = match outcome {
                    NodeOutcome::Completed { outputs } => RunNodeOutcomeV2::Completed {
                        output_artifacts: artifact_ids(outputs),
                    },
                    NodeOutcome::Failed { error } => RunNodeOutcomeV2::Failed {
                        error: error.clone(),
                    },
                    NodeOutcome::Suppressed { reason } => RunNodeOutcomeV2::Suppressed {
                        reason: match reason {
                            review_graph::SuppressionReason::GateBlocked => {
                                RunSuppressionReasonV2::GateBlocked
                            }
                            review_graph::SuppressionReason::UpstreamMissing => {
                                RunSuppressionReasonV2::UpstreamMissing
                            }
                        },
                    },
                };
                RunNodeReportV2 {
                    node: id.clone(),
                    outcome,
                }
            })
            .collect();
        let persisted_verdict = match &verdict {
            RunVerdict::Pass => RunVerdictV2::Pass,
            RunVerdict::Fail(Verdict::NotConverged) => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::NotConverged,
            },
            RunVerdict::Fail(Verdict::Exhausted) => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::Exhausted,
            },
            RunVerdict::Fail(Verdict::Converged) => {
                return Err("invalid run verdict: converged cannot be a failure".to_string());
            }
            RunVerdict::Incomplete { missing } => RunVerdictV2::Incomplete {
                missing_nodes: missing
                    .iter()
                    .map(|(node, reason)| MissingNodeV2 {
                        node: node.clone(),
                        reason: reason.clone(),
                    })
                    .collect(),
            },
        };
        let payload = RunReportPayloadV2 {
            outcomes,
            blocked_gates: report.blocked_gates.iter().cloned().collect(),
            verdict: persisted_verdict,
            spent_tokens: self.spent(),
        };
        self.append(NewEvent::new(
            EventType::RunReportV2,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        ))?;
        *published = true;
        Ok(verdict)
    }

    /// A sandbox in the requested mode, as a copy-on-write clone of the run's single
    /// materialized template. The template is built once, under the lock, on the first call
    /// (the gate's); every later sandbox — the reviewers' — clones it instead of walking the
    /// manifest and re-reading the whole tree from the CAS.
    fn sandbox(&self, mode: Mode) -> Result<Sandbox, String> {
        let template = {
            let mut guard = self.template.lock().expect("template");
            match guard.as_ref() {
                Some(template) => template.clone(),
                None => {
                    let template = std::sync::Arc::new(
                        review_sandbox::SandboxTemplate::materialize(&self.snapshot, self.cas)
                            .map_err(|e| e.to_string())?,
                    );
                    *guard = Some(template.clone());
                    template
                }
            }
        };
        Sandbox::from_template(&template, mode).map_err(|e| e.to_string())
    }

    /// Emit the run's generation state — the campaign's prior findings — as the artifact a
    /// reviewer receives on its `prior_findings` input edge. In the first round there is no
    /// prior state, so an empty finding set is emitted; the edge is satisfied either way, and
    /// nothing about delivery depends on ambient kernel state.
    fn run_generation(&self, node: &Node) -> Result<ArtifactMap, String> {
        let artifact = self
            .prior_findings
            .clone()
            .ok_or("campaign execution has no exact prior Finding Set from RoundStarted@1")?;
        let mut outputs = ArtifactMap::new();
        for port in &node.outputs {
            let value = match port.name.as_str() {
                "findings" => artifact.clone(),
                "change_set" => self
                    .authority
                    .change_set_id
                    .clone()
                    .ok_or("generation declares `change_set` for a whole-tree Subject")?,
                other => {
                    return Err(format!(
                        "generation has unsupported built-in output port `{other}`"
                    ));
                }
            };
            outputs.insert(port.name.clone(), vec![value]);
        }
        Ok(outputs)
    }

    fn run_gate(&self, node_id: &str) -> Result<Vec<String>, String> {
        // The gate's checks run in a read-only sandbox: a check that mutates the tree would
        // change what every reviewer after it inspects, which is the same torn-input problem
        // capture solves one layer down.
        let sandbox = self.sandbox(Mode::ReadOnly)?;
        // Run the checks holding no lock: each is a build or a test, and the store lock is
        // shared with every other node, so holding it across a check would stall the whole
        // pipeline for the build's duration. The lock is taken only to append each result.
        let runner = CheckRunner::new(self.cas, sandbox.root());
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let result = runner.run(check);
            self.buffer_reviewer_event(node_id, check_event(&result, node_id));
            results.push(result);
        }

        let decision = GateDecision::evaluate(&results);
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if !sealed.unchanged() {
            // Not fatal, but never silent: a read-only gate that mutated its tree has broken an
            // assumption every downstream node is relying on. Bounded — a check that ran a
            // build could have touched thousands of paths, and this is an error string.
            let paths = sealed.mutations.paths();
            return Err(format!(
                "gate mutated its read-only sandbox: {} paths, e.g. {:?}",
                paths.len(),
                &paths[..paths.len().min(20)]
            ));
        }
        let artifact = self
            .cas
            .put_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        self.buffer_reviewer_event(
            node_id,
            NewEvent::new(
                EventType::GateDecisionV1,
                serde_json::to_value(&decision).map_err(|e| e.to_string())?,
            )
            .node(node_id)
            .referencing(vec![artifact.clone()]),
        );
        self.gates
            .lock()
            .expect("gates")
            .insert(node_id.to_string(), decision);
        Ok(vec![artifact])
    }

    fn run_reviewer(
        &self,
        node_id: &str,
        node_inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        let adapter = self
            .reviewers
            .get(node_id)
            .ok_or_else(|| format!("no reviewer bound to node {node_id}"))?;
        let mut prepared = self
            .prepared_attempts
            .lock()
            .expect("prepared attempts")
            .remove(node_id);

        // Prior findings arrive through the wired `prior_findings` input port — a data artifact
        // the pipeline routed from the generation node — not from ambient kernel state. A
        // reviewer that declares no such input receives none; the plan is the delivery.
        let prior_findings_artifact = node_inputs
            .get(PRIOR_FINDINGS_PORT)
            .and_then(|artifacts| artifacts.first())
            .cloned();
        let mut inputs = ReviewerInputs::default();
        for (port, artifacts) in node_inputs {
            if port == PRIOR_FINDINGS_PORT {
                continue;
            }
            let mut resolved = Vec::with_capacity(artifacts.len());
            for artifact in artifacts {
                let encoded = self.cas.get(artifact).map_err(|error| error.to_string())?;
                let limit = if port == "change_set" {
                    MAX_CHANGE_SET_BYTES
                } else {
                    MAX_PRIOR_FINDINGS_BYTES
                };
                if encoded.len() > limit {
                    return Err(format!(
                        "reviewer input port '{port}' artifact {artifact} exceeds {limit} bytes"
                    ));
                }
                let value = serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
                resolved.push(ReviewerInputArtifact {
                    artifact_id: artifact.clone(),
                    value,
                });
            }
            inputs.artifacts.insert(port.clone(), resolved);
        }
        if let Some(artifact) = &prior_findings_artifact {
            let encoded = match self.cas.get(artifact) {
                Ok(encoded) => encoded,
                Err(error) => {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error.to_string(),
                        )?;
                    }
                    return Err(error.to_string());
                }
            };
            if encoded.len() > MAX_PRIOR_FINDINGS_BYTES {
                let error = format!(
                    "exact prior Finding Set is {} bytes; maximum is {} bytes and partitioning is required",
                    encoded.len(),
                    MAX_PRIOR_FINDINGS_BYTES
                );
                if let Some(prepared) = prepared.take() {
                    self.release_prepared_attempt(
                        node_id,
                        &prepared.attempt,
                        prepared.reservation.as_ref(),
                        &error,
                    )?;
                }
                return Err(error);
            }
            let value: serde_json::Value = match serde_json::from_slice(&encoded) {
                Ok(value) => value,
                Err(error) => {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error.to_string(),
                        )?;
                    }
                    return Err(error.to_string());
                }
            };
            // The generation node emits an empty document in the first round; only a non-empty
            // finding set is worth rendering into the prompt.
            let has_findings = value
                .get("prior_findings")
                .and_then(|f| f.as_array())
                .is_some_and(|f| !f.is_empty());
            if has_findings {
                inputs.prior_findings = Some(value);
            }
        }

        let mut timeouts: Vec<String> = Vec::new();
        for _ in 0..=self.timeout_retries {
            // The scheduler prepares the first attempt in plan order before spawning this
            // worker. Timeout retries are prepared here only after the predecessor is fenced.
            let PreparedReviewerAttempt {
                attempt,
                reservation,
            } = match prepared.take() {
                Some(prepared) => prepared,
                None => self.prepare_reviewer_attempt(
                    node_id,
                    prior_findings_artifact.as_ref(),
                    &timeouts,
                )?,
            };

            // Each attempt gets its own fresh sandbox. Reviewers may edit freely — a TDD
            // reviewer must — and nothing they do can reach a sibling, the source, the
            // snapshot, or a retry of themselves.
            let sandbox = match self.sandbox(Mode::EphemeralWrite) {
                Ok(sandbox) => sandbox,
                Err(error) => {
                    self.release_prepared_attempt(node_id, &attempt, reservation.as_ref(), &error)?;
                    return Err(error);
                }
            };

            let invoked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                adapter.invoke(self.cas, sandbox.root(), &inputs)
            }));
            let invoked = match invoked {
                Ok(invoked) => invoked,
                Err(_) => {
                    let error = format!("reviewer adapter panicked for node {node_id}");
                    let charged = reservation
                        .as_ref()
                        .map_or(0, |reservation| reservation.amount);
                    self.fail_started_attempt(
                        node_id,
                        &attempt,
                        reservation.as_ref(),
                        &error,
                        charged,
                    )?;
                    return Err(error);
                }
            };

            match invoked {
                Ok(returned) => {
                    let artifacts = (|| -> Result<(String, String), String> {
                        let sealed = sandbox.seal().map_err(|error| error.to_string())?;
                        let result_artifact = self
                            .cas
                            .put_json(&reviewer_result_value(&returned.output)?)
                            .map_err(|error| error.to_string())?;
                        // The mutation set can be enormous — a reviewer that built to verify a
                        // claim leaves a whole target/ behind. The full list lives once in the
                        // CAS; provenance carries only a bounded summary.
                        let mutations_artifact = self
                            .cas
                            .put_json(&serde_json::json!({
                                "added": sealed.mutations.added,
                                "modified": sealed.mutations.modified,
                                "deleted": sealed.mutations.deleted,
                            }))
                            .map_err(|error| error.to_string())?;
                        let mutation_summary =
                            mutation_summary(&sealed.mutations, &mutations_artifact);
                        let provenance_artifact = self
                            .cas
                            .put_json(&serde_json::json!({
                                "node": node_id,
                                "attempt": attempt.to_string(),
                                "result_artifact": result_artifact,
                                "cost_tokens": returned.cost_tokens,
                                "raw": returned.raw_artifact,
                                "sandbox_mutations": mutation_summary,
                            }))
                            .map_err(|error| error.to_string())?;
                        Ok((result_artifact, provenance_artifact))
                    })();
                    let (result_artifact, provenance_artifact) = match artifacts {
                        Ok(artifacts) => artifacts,
                        Err(error) => {
                            self.fail_started_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &error,
                                returned.cost_tokens,
                            )?;
                            return Err(error);
                        }
                    };

                    // Selection is recorded only after the complete receipted output exists.
                    let selection = self
                        .attempts
                        .lock()
                        .expect("attempt ledger")
                        .admit(&Receipt {
                            attempt: attempt.clone(),
                            output: returned.raw_artifact.clone(),
                            cost: returned.cost_tokens,
                        });
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, returned.cost_tokens);
                    }
                    let admitted = NewEvent::new(
                        EventType::AttemptAdmittedV1,
                        serde_json::to_value(AttemptAdmittedPayloadV1 {
                            selection: match selection {
                                Selection::Selected => "selected",
                                Selection::Quarantined => "quarantined",
                            }
                            .to_string(),
                            cost_tokens: returned.cost_tokens,
                            result_artifact: Some(result_artifact.clone()),
                            provenance_artifact: Some(provenance_artifact.clone()),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .attempt(attempt.to_string())
                    .referencing(vec![
                        result_artifact.clone(),
                        provenance_artifact,
                        returned.raw_artifact.clone(),
                    ]);
                    if selection == Selection::Quarantined {
                        self.append(admitted)?;
                        return Err(format!(
                            "attempt {attempt} was fenced; its late result is quarantined"
                        ));
                    }
                    if self
                        .reviewer_selections
                        .lock()
                        .expect("reviewer selections")
                        .insert(
                            node_id.to_string(),
                            SelectedReviewer {
                                attempt_id: attempt.to_string(),
                                result_artifact: result_artifact.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("reviewer {node_id} selected more than one attempt"));
                    }
                    self.buffer_reviewer_event(node_id, admitted);
                    return Ok(vec![result_artifact]);
                }
                Err(RunnerError::TimedOut { after_ms }) => {
                    // Fence, charge, retry. The killed process's true spend is unreportable,
                    // so the full reservation is charged — the conservative reading of "a
                    // fenced attempt charges", and the one that keeps a hang from being a
                    // free retry.
                    self.attempts.lock().expect("attempt ledger").fence(node_id);
                    if let Some(reservation) = &reservation {
                        self.attempts
                            .lock()
                            .expect("attempt ledger")
                            .charge(&attempt, reservation.amount);
                    }
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, reservation.amount);
                    }
                    self.append(
                        NewEvent::new(
                            EventType::AttemptFencedV1,
                            serde_json::json!({
                                "reason": format!("timed out after {after_ms}ms"),
                                "charged": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    )?;
                    timeouts.push(format!("attempt {attempt} timed out after {after_ms}ms"));
                }
                Err(error @ (RunnerError::Refused(_) | RunnerError::Unavailable(_))) => {
                    // Nothing executed, so nothing was spent: the reservation is released,
                    // not charged.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .release(reservation);
                    }
                    self.append(
                        NewEvent::new(
                            EventType::AttemptReleasedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "released": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    )?;
                    return Err(error.to_string());
                }
                Err(error) => {
                    // Failed or malformed: the reviewer did execute, its spend is unreported,
                    // and forgiving it would make crashing cheaper than answering. Full
                    // reservation, same rule as a timeout.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, reservation.amount);
                    }
                    if let Some(reservation) = &reservation {
                        self.attempts
                            .lock()
                            .expect("attempt ledger")
                            .charge(&attempt, reservation.amount);
                    }
                    self.append(
                        NewEvent::new(
                            EventType::AttemptFailedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "charged": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    )?;
                    return Err(error.to_string());
                }
            }
        }
        Err(format!("every attempt timed out: {}", timeouts.join("; ")))
    }

    /// A real gather: one artifact holding exactly the report artifacts the edges delivered.
    /// A reviewer whose result port feeds no edge is absent here, and therefore absent from
    /// everything downstream — the plan is the data flow, not a suggestion about it.
    ///
    /// This is also the run's canonical barrier: every reviewer has finished, so the buffered
    /// reviewer events are flushed here in node order, giving the log a shape that is a
    /// function of the pipeline rather than of thread timing.
    fn run_gather(&self, inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        self.flush_reviewer_events()?;
        let artifact = self
            .cas
            .put_json(&serde_json::json!(inputs))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }

    fn run_ledger(&self, inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        // The ledger reduces what its edges delivered — never a global map of whatever happened
        // to run. Each input is one reviewer's result, or a gather manifest of result ids.
        let mut results: Vec<(String, LegacyStageOutput)> = Vec::new();
        let mut load = |node: &str, id: &str, value: serde_json::Value| -> Result<(), String> {
            let output =
                reviewer_stage_output(value).map_err(|error| format!("artifact {id}: {error}"))?;
            results.push((node.to_string(), output));
            Ok(())
        };
        for (input_port, artifacts) in inputs {
            for input in artifacts {
                let value = self.cas.get_json(input).map_err(|e| e.to_string())?;
                if value.get("verdict").is_some() && value.get("reports").is_some() {
                    load(input_port, input, value)?;
                    continue;
                }
                match value {
                    serde_json::Value::Object(manifest) => {
                        for (node, ids) in manifest {
                            let ids = ids.as_array().ok_or_else(|| {
                                format!("gather manifest {input} has a non-array port")
                            })?;
                            for id in ids {
                                let id = id.as_str().ok_or_else(|| {
                                    format!("gather manifest {input} holds a non-id")
                                })?;
                                let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                                load(&node, id, value)?;
                            }
                        }
                    }
                    // Compatibility for gather manifests emitted before source-labelled maps.
                    serde_json::Value::Array(ids) => {
                        for id in &ids {
                            let id = id
                                .as_str()
                                .ok_or_else(|| format!("gather manifest {input} holds a non-id"))?;
                            let selected: Vec<String> = self
                                .reviewer_selections
                                .lock()
                                .expect("reviewer selections")
                                .iter()
                                .filter(|(_, selection)| selection.result_artifact == id)
                                .map(|(node, _)| node.clone())
                                .collect();
                            if selected.len() != 1 {
                                return Err(format!(
                                    "legacy gather manifest {input} cannot uniquely identify artifact {id}"
                                ));
                            }
                            let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                            load(&selected[0], id, value)?;
                        }
                    }
                    _ => {
                        return Err(format!(
                            "artifact {input} is neither ReviewerResult@1 nor a gather manifest"
                        ));
                    }
                }
            }
        }
        // Canonical gather order: node id — not completion order, not artifact digest order.
        results.sort_by(|a, b| a.0.cmp(&b.0));

        let (round, finding_count) = {
            let mut store = self.store.lock().expect("event store");
            let mut ingest = Ingest::new(*store, self.cas, self.run_id.clone())
                .map_err(|e| e.to_string())?
                .under_round(&self.authority.round_event_id);
            let stages: Vec<(&str, &LegacyStageOutput)> = results
                .iter()
                .map(|(node, stage)| (node.as_str(), stage))
                .collect();
            ingest
                .add_live_stage_outputs(&stages)
                .map_err(|e| e.to_string())?;
            (ingest.ledger().round, ingest.ledger().len())
        };
        // The `findings` port must carry a real artifact, not a label: the scheduler delivers
        // exactly this string to whatever consumes the port, and a downstream event referencing
        // a non-CAS string would be rejected as a dangling artifact far from its cause.
        let artifact = self
            .cas
            .put_json(&serde_json::json!({
                "round": round,
                "sources": results.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "findings": finding_count,
            }))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }
}

fn reviewer_result_value(stage: &LegacyStageOutput) -> Result<serde_json::Value, String> {
    let mut object = serde_json::to_value(stage)
        .map_err(|error| error.to_string())?
        .as_object()
        .cloned()
        .ok_or("reviewer result did not serialize as an object")?;
    let reports = object
        .remove("findings")
        .ok_or("reviewer result has no findings field")?;
    object.insert("reports".into(), reports);
    if let Some(disputes) = object
        .get_mut("disputes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for dispute in disputes {
            let dispute = dispute
                .as_object_mut()
                .ok_or("reviewer result has a non-object dispute")?;
            let claim_id = dispute
                .remove("fp")
                .ok_or("reviewer dispute has no claim ID")?;
            dispute.insert("claim_id".into(), claim_id);
            if !matches!(
                dispute.get("position").and_then(serde_json::Value::as_str),
                Some("confirm" | "refute")
            ) {
                return Err("reviewer dispute has an invalid position".into());
            }
        }
    }
    Ok(serde_json::Value::Object(object))
}

fn reviewer_stage_output(value: serde_json::Value) -> Result<LegacyStageOutput, String> {
    let mut object = value
        .as_object()
        .cloned()
        .ok_or("ReviewerResult@1 is not an object")?;
    let reports = object
        .remove("reports")
        .ok_or("ReviewerResult@1 has no reports field")?;
    object.insert("findings".into(), reports);
    serde_json::from_value(serde_json::Value::Object(object)).map_err(|error| error.to_string())
}

/// A compact record of a sandbox's mutations: the counts, a bounded sample of paths, and the
/// CAS digest of the full set. Bounded on purpose — the full list is thousands of entries when
/// a reviewer built, and it must not be inlined into every event payload.
fn mutation_summary(
    mutations: &review_sandbox::MutationSet,
    full_artifact: &str,
) -> serde_json::Value {
    const SAMPLE: usize = 20;
    let groups = [&mutations.added, &mutations.modified, &mutations.deleted];
    let mut positions = [0_usize; 3];
    let mut sample = Vec::new();
    while sample.len() < SAMPLE {
        let next = (0..groups.len())
            .filter(|index| positions[*index] < groups[*index].len())
            .min_by_key(|index| groups[*index][positions[*index]].as_str());
        let Some(index) = next else { break };
        sample.push(&groups[index][positions[index]]);
        positions[index] += 1;
    }
    let count = groups.iter().map(|group| group.len()).sum::<usize>();
    serde_json::json!({
        "count": count,
        "added": mutations.added.len(),
        "modified": mutations.modified.len(),
        "deleted": mutations.deleted.len(),
        "sample": sample,
        "truncated": count > SAMPLE,
        "artifact": full_artifact,
    })
}

fn validate_generation_outputs(
    authority: &RoundAuthority,
    node: &Node,
    outputs: &ArtifactMap,
) -> Result<(), String> {
    if node.kind != NodeKind::Generation {
        return Ok(());
    }
    let expected_findings = vec![authority.prior_finding_set_id.clone()];
    let expected_change_set = authority.change_set_id.as_ref().map(|id| vec![id.clone()]);
    if outputs.get("findings") != Some(&expected_findings)
        || outputs.get("change_set") != expected_change_set.as_ref()
    {
        return Err(format!(
            "generation receipt contradicts Round {}'s pinned inputs or Change Set",
            authority.round
        ));
    }
    Ok(())
}

impl Dispatch for Kernel<'_> {
    fn record_invocation(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        let payload = NodeInvocationPayloadV1 {
            node: node.id.clone(),
            inputs: port_artifacts(&node.inputs, inputs, &self.authority.head_snapshot_id),
        };
        if let Some(recorded) = self.replayed_invocations.get(&node.id) {
            if recorded != &payload {
                return Err(format!(
                    "node `{}` no longer resolves to its durable invocation",
                    node.id
                ));
            }
        } else {
            self.append(
                NewEvent::new(
                    EventType::NodeInvocationV1,
                    serde_json::to_value(payload).map_err(|e| e.to_string())?,
                )
                .node(&node.id)
                .referencing(artifact_ids(inputs)),
            )?;
        }
        if node.kind == NodeKind::Reviewer && !self.replayed_outputs.contains_key(&node.id) {
            if !self.reviewers.contains_key(&node.id) {
                return Err(format!("no reviewer bound to node {}", node.id));
            }
            let prior_findings = inputs
                .get(PRIOR_FINDINGS_PORT)
                .and_then(|artifacts| artifacts.first());
            let prepared = self.prepare_reviewer_attempt(&node.id, prior_findings, &[])?;
            self.prepared_attempts
                .lock()
                .expect("prepared attempts")
                .insert(node.id.clone(), prepared);
        }
        Ok(())
    }

    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        if let Some(receipt) = self.replayed_outputs.get(&node.id) {
            if node.kind == NodeKind::Reviewer {
                let selections = self
                    .reviewer_selections
                    .lock()
                    .expect("reviewer selections");
                let selected = selections.get(&node.id).ok_or_else(|| {
                    format!(
                        "reviewer '{}': receipt has no selected admitted attempt",
                        node.id
                    )
                })?;
                let output_artifacts: Vec<&String> = receipt
                    .payload
                    .outputs
                    .iter()
                    .flat_map(|port| &port.artifact_ids)
                    .collect();
                if receipt.attempt_id.as_deref() != Some(selected.attempt_id.as_str())
                    || output_artifacts.len() != 1
                    || output_artifacts[0] != &selected.result_artifact
                {
                    return Err(format!(
                        "reviewer '{}': receipt contradicts its selected admitted result",
                        node.id
                    ));
                }
            }
            let receipt = &receipt.payload;
            let outputs: ArtifactMap = receipt
                .outputs
                .iter()
                .map(|port| (port.port.clone(), port.artifact_ids.clone()))
                .collect();
            validate_generation_outputs(&self.authority, node, &outputs)?;
            let expected =
                port_artifacts(&node.outputs, &outputs, &self.authority.head_snapshot_id);
            if receipt.node != node.id || receipt.outputs != expected {
                return Err(format!(
                    "node '{}': durable receipt violates its output contracts",
                    node.id
                ));
            }
            return Ok(outputs);
        }
        // Routing is on the validated kind, never the id: an id is a name someone chose, and a
        // reviewer named `gather` must still be a reviewer that runs.
        if node.kind == NodeKind::Generation {
            return self.run_generation(node);
        }
        let artifacts = match node.kind {
            NodeKind::Generation => unreachable!("generation returned above"),
            NodeKind::Gate => self.run_gate(&node.id),
            // Gather and ledger reduce whatever artifacts their edges delivered; the port
            // labels are the reviewer's concern, not theirs.
            NodeKind::Gather => self.run_gather(inputs),
            NodeKind::Ledger => self.run_ledger(inputs),
            NodeKind::Reviewer => self.run_reviewer(&node.id, inputs),
        }?;
        bind_single_output(node, artifacts)
    }

    fn record_outputs(&self, node: &Node, outputs: &ArtifactMap) -> Result<(), String> {
        validate_generation_outputs(&self.authority, node, outputs)?;
        if let Some(recorded) = self.replayed_outputs.get(&node.id) {
            let expected = NodeOutputReceiptPayloadV1 {
                node: node.id.clone(),
                outputs: port_artifacts(&node.outputs, outputs, &self.authority.head_snapshot_id),
            };
            return if recorded.payload == expected {
                Ok(())
            } else {
                Err(format!(
                    "node `{}` replayed outputs disagree with its durable receipt",
                    node.id
                ))
            };
        }
        let payload = NodeOutputReceiptPayloadV1 {
            node: node.id.clone(),
            outputs: port_artifacts(&node.outputs, outputs, &self.authority.head_snapshot_id),
        };
        let mut event = NewEvent::new(
            EventType::NodeOutputReceiptV1,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        )
        .node(&node.id)
        .referencing(artifact_ids(outputs));
        if node.kind == NodeKind::Reviewer {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let selected = selections.get(&node.id).ok_or_else(|| {
                format!(
                    "reviewer '{}': output has no selected admitted attempt",
                    node.id
                )
            })?;
            event = event.attempt(&selected.attempt_id);
        }
        // The scheduler publishes outputs as soon as this returns. Commit any node lifecycle
        // facts and its receipt together before that publication point. Reviewers contribute
        // attempt admission; gates contribute check results and the gate decision.
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        let mut events: Vec<NewEvent> = pending
            .iter()
            .filter(|((id, _), _)| id == &node.id)
            .map(|(_, event)| event.clone())
            .collect();
        events.push(event);
        self.append_batch(&events)?;
        pending.retain(|((id, _), _)| id != &node.id);
        Ok(())
    }

    fn gate_passed(&self, node_id: &str, _outputs: &ArtifactMap) -> bool {
        self.gates
            .lock()
            .expect("gates")
            .get(node_id)
            .map(GateDecision::passed)
            .unwrap_or(false)
    }
}

impl review_config::SubjectDispatch for Kernel<'_> {
    fn subject_kind(&self) -> review_core::SubjectKind {
        self.subject
    }
}
