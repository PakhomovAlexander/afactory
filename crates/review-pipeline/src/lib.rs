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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use review_attempt::{
    AttemptId, AttemptLedger, Budget, BudgetLedger, Receipt, Reservation, Scope, Selection,
};
use review_check::{CheckDefinition, CheckRunner, Command, GateDecision, check_event};
use review_core::event::{
    AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1, AttemptFailedPayloadV1,
    AttemptFeedbackPayloadV1, AttemptFencedPayloadV1, AttemptInputPayloadV1,
    AttemptReleasedPayloadV1,
};
use review_core::{
    CampaignManifestV1, CampaignOpenedPayloadV1, EventType, LegacyStageOutput,
    MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES, MissingNodeV2, NodeInvocationPayloadV1,
    NodeOutputReceiptPayloadV1, PortArtifactsV1, ReviewerResultRejection, RoundStartedPayloadV1,
    RunFailureReasonV3, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV3,
    RunSuppressionReasonV2, RunVerdictV3, SnapshotAffinity, SourceSnapshot,
    run_report_closes_round,
};
use review_graph::{
    ArtifactMap, Dispatch, Node, NodeFailureClass, NodeKind, NodeOutcome, PortContract, RunReport,
};
use review_runner::{
    ContextManifest, ReviewerAdapter, ReviewerAttemptContext, ReviewerInputArtifact,
    ReviewerInputs, RunnerError, TokenUsage,
};
use review_sandbox::{Mode, Sandbox};
use review_source_git::Manifest;
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ingest, Ledger, LedgerProjection, NewEvent,
    Verdict,
};

fn is_generation_prior_findings_output(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "findings"
}

fn is_reviewer_prior_findings_input(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "prior_findings"
}

fn is_change_set_port(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::CHANGE_SET_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "change_set"
}

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
    refusal_history_id: Option<String>,
}

#[derive(Default)]
struct AttemptFailureEvidence<'a> {
    raw_artifact: Option<&'a str>,
    refusal_history: Option<&'a [String]>,
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

/// Selected Attempt accounting reconstructed from its durable provenance artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptEvidence {
    pub node: String,
    pub attempt_id: String,
    pub cost_tokens: u64,
    pub usage: TokenUsage,
    pub context_manifest: ContextManifest,
    pub raw_artifact: String,
}

/// The immutable publication boundary every event emitted by one Round execution inherits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundAuthority {
    run_id: String,
    round_event_id: String,
    round: u32,
    epoch: u32,
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_id: String,
    head_snapshot_id: String,
    head_content_digest: String,
    prior_finding_set_id: String,
    prior_reduction_finding_set_id: String,
    finding_identity_policy: String,
    subject_kind: review_core::SubjectKind,
    change_set_id: Option<String>,
    change_set: Option<Arc<review_store::ResolvedChangeSet>>,
    reviewer_packages: BTreeMap<String, (String, String)>,
    policy_ids: Vec<String>,
}

impl RoundAuthority {
    pub fn authority_snapshot_id(&self) -> &str {
        &self.authority_snapshot_id
    }

    pub fn campaign_manifest_id(&self) -> &str {
        &self.campaign_manifest_id
    }

    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    pub fn head_snapshot_id(&self) -> &str {
        &self.head_snapshot_id
    }

    pub fn load(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<Self, String> {
        let opened = store
            .campaign_opened(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Round authority has no CampaignOpened@1")?;
        let opened: CampaignOpenedPayloadV1 =
            serde_json::from_value(opened.payload).map_err(|error| error.to_string())?;
        let round = store
            .latest_round_started(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Round authority has no RoundStarted@1")?;
        if round.event_id != round_event_id {
            return Err("requested Round is not the active Round epoch".into());
        }
        let payload: RoundStartedPayloadV1 =
            serde_json::from_value(round.payload.clone()).map_err(|error| error.to_string())?;
        if payload.campaign_manifest_id != opened.campaign_manifest_id {
            return Err("RoundStarted@1 does not reference the opened CampaignManifest".into());
        }
        let campaign_manifest: CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&opened.campaign_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        campaign_manifest.validate()?;
        let prior_reduction_finding_set_id = if campaign_manifest.finding_identity_policy
            == review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            canonical_prior_finding_set_id(
                store,
                cas,
                run_id,
                payload.round,
                &opened.campaign_manifest_id,
                &campaign_manifest,
            )?
        } else {
            payload.prior_finding_set_id.clone()
        };
        let reviewer_packages = campaign_manifest
            .reviewers
            .iter()
            .map(|reviewer| {
                (
                    reviewer.node.clone(),
                    (
                        reviewer.package_artifact_id.clone(),
                        reviewer.digest.clone(),
                    ),
                )
            })
            .collect();
        let mut policy_ids = campaign_manifest.execution_policy_ids.clone();
        policy_ids.extend(campaign_manifest.project_policy_ids.clone());
        policy_ids.sort();
        policy_ids.dedup();
        let resolved = review_store::resolve_subject(cas, &payload.subject_id)
            .map_err(|error| error.to_string())?;
        let subject = resolved.subject;
        let change_set_id = subject.change_set_id.clone();
        let change_set = match (&change_set_id, resolved.change_set) {
            (Some(artifact_id), Some(change_set)) if change_set.artifact_id() == artifact_id => {
                Some(change_set)
            }
            (None, None) => None,
            _ => return Err("resolved Subject has incomplete Change Set authority".into()),
        };
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
            cas.verify(&artifact).map_err(|error| error.to_string())?;
            if !round.artifact_refs.contains(&artifact) {
                return Err(format!(
                    "RoundStarted@1 does not publish required authority artifact `{artifact}`"
                ));
            }
        }
        Ok(Self {
            run_id: run_id.to_string(),
            round_event_id: round.event_id,
            round: payload.round,
            epoch: payload.epoch,
            authority_snapshot_id: opened.authority_snapshot_id,
            campaign_manifest_id: payload.campaign_manifest_id,
            subject_id: payload.subject_id,
            head_snapshot_id: subject.head_snapshot_id,
            head_content_digest: source.content_digest,
            prior_finding_set_id: payload.prior_finding_set_id,
            prior_reduction_finding_set_id,
            finding_identity_policy: campaign_manifest.finding_identity_policy,
            subject_kind: subject.kind,
            change_set_id,
            change_set,
            reviewer_packages,
            policy_ids,
        })
    }

    pub fn round_event_id(&self) -> &str {
        &self.round_event_id
    }

    pub fn round(&self) -> u32 {
        self.round
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
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

fn canonical_prior_finding_set_id(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    canonical_prior_finding_set_id_from_events(cas, &events, round, campaign_manifest_id, campaign)
}

fn canonical_prior_finding_set_id_from_events(
    cas: &Cas,
    events: &[review_core::RunEvent],
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    if round == 1 {
        cas.verify(&campaign.finding_genesis_id)
            .map_err(|error| error.to_string())?;
        return Ok(campaign.finding_genesis_id.clone());
    }
    let prior_rounds: Vec<_> = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type != EventType::RoundStartedV1 {
                return None;
            }
            let payload: RoundStartedPayloadV1 =
                serde_json::from_value(event.payload.clone()).ok()?;
            (payload.round < round && payload.campaign_manifest_id == campaign_manifest_id)
                .then_some((event.event_id.as_str(), payload.round))
        })
        .collect();
    for (prior_round_event_id, prior_round) in prior_rounds {
        let closed = events.iter().try_fold(false, |closed, event| {
            if event.causation_id.as_deref() != Some(prior_round_event_id)
                || !event.event_type.is_run_report()
            {
                return Ok(closed);
            }
            run_report_closes_round(event)
                .map(|value| closed || value.unwrap_or(false))
                .map_err(|error| error.to_string())
        })?;
        if !closed {
            continue;
        }
        let mut ids = Vec::new();
        for event in events {
            if event.event_type != EventType::NodeOutputReceiptV1
                || event.causation_id.as_deref() != Some(prior_round_event_id)
            {
                continue;
            }
            let receipt: NodeOutputReceiptPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            for port in receipt.outputs {
                if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                    ids.extend(port.artifact_ids);
                    continue;
                }
                if port.port != "findings" {
                    continue;
                }
                for id in port.artifact_ids {
                    let Ok(value) = cas.get_json(&id) else {
                        continue;
                    };
                    let Ok(envelope) =
                        serde_json::from_value::<review_core::ArtifactEnvelope>(value)
                    else {
                        continue;
                    };
                    if envelope.artifact_type == review_core::contract::FINDING_SET_V1 {
                        ids.push(id);
                    }
                }
            }
        }
        if ids.is_empty() {
            continue;
        }
        if ids.len() != 1 {
            return Err(format!(
                "canonical Finding Set lineage round {prior_round} has {} ledger outputs",
                ids.len()
            ));
        }
        let id = ids.pop().expect("exactly one canonical Finding Set output");
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&id).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("prior Finding Set {id} is not an artifact envelope: {error}"))?;
        review_store::validate_envelope(&envelope).map_err(|error| error.to_string())?;
        if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
            return Err(format!("prior ledger output {id} is not FindingSet@1"));
        }
        let set: review_core::FindingSetV1 = serde_json::from_value(envelope.payload)
            .map_err(|error| format!("prior Finding Set {id} is invalid: {error}"))?;
        set.validate()?;
        if set.round != prior_round
            || set.round >= round
            || set.identity_policy != review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            return Err(format!(
                "prior Finding Set {id} disagrees with canonical round {prior_round}"
            ));
        }
        return Ok(id);
    }
    cas.verify(&campaign.finding_genesis_id)
        .map_err(|error| error.to_string())?;
    Ok(campaign.finding_genesis_id.clone())
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
    refusal_histories: BTreeMap<String, Vec<String>>,
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
    let mut provider_operations = BTreeMap::new();
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
                        cas.verify(artifact).map_err(|error| error.to_string())?;
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
            EventType::AttemptInputV1 if active_epoch => {
                let payload: AttemptInputPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let node = event.node_id.ok_or("AttemptInput@1 has no node ID")?;
                let failures: Vec<String> = serde_json::from_value(
                    cas.get_json(&payload.refusal_history_id)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                if failures.is_empty() {
                    return Err("AttemptInput@1 refusal history is empty".into());
                }
                replayed.refusal_histories.insert(node, failures);
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
                    cas.verify(&result_artifact)
                        .map_err(|error| error.to_string())?;
                    cas.verify(&provenance_artifact)
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
                event
                    .node_id
                    .ok_or("terminal attempt event has no node ID")?;
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
                event
                    .node_id
                    .ok_or("terminal attempt event has no node ID")?;
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
            EventType::AttemptFeedbackV1 if active_epoch => {
                let payload: AttemptFeedbackPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let node = event.node_id.ok_or("AttemptFeedback@1 has no node ID")?;
                let failures: Vec<String> = serde_json::from_value(
                    cas.get_json(&payload.refusal_history_id)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                if failures.is_empty() {
                    return Err("AttemptFeedback@1 refusal history is empty".into());
                }
                replayed.refusal_histories.insert(node, failures);
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
            EventType::ProviderOperationTransitionV1 => {
                let payload: review_core::ProviderOperationTransitionPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                replayed.committed_tokens = replayed
                    .committed_tokens
                    .checked_add(payload.charged_tokens)
                    .ok_or("replayed provider token charge overflow")?;
                provider_operations.insert(payload.operation_id.clone(), payload);
            }
            _ => {}
        }
    }
    for provider in provider_operations.values() {
        if provider.state == review_core::ProviderOperationStateV1::Running
            && provider.failure_class.is_none()
        {
            replayed.committed_tokens = replayed
                .committed_tokens
                .checked_add(provider.reserved_tokens)
                .ok_or("replayed provider reservation overflow")?;
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

fn failed_retry_context(
    attempt: &str,
    failure_class: &str,
    rejection_code: Option<&str>,
) -> String {
    match rejection_code {
        Some(code) => {
            format!("attempt {attempt} returned an invalid result: {failure_class}:{code}")
        }
        None => format!("attempt {attempt} returned an invalid result: {failure_class}"),
    }
}

fn fenced_retry_context(attempt: &str, reason: &str) -> String {
    format!("attempt {attempt} {reason}")
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
            NodeOutcome::Failed { error, .. } => Some((id.clone(), error.clone())),
            NodeOutcome::Suppressed { reason } => Some((id.clone(), format!("{reason:?}"))),
        })
        .collect();
    if report.outcomes.iter().any(|(_, outcome)| {
        matches!(
            outcome,
            NodeOutcome::Failed {
                class: Some(NodeFailureClass::RunBudgetExhausted),
                ..
            }
        )
    }) {
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
    pipeline_version: u32,
    authority: RoundAuthority,
    checks: Vec<CheckDefinition>,
    check_timeout: Duration,
    reviewers: BTreeMap<String, Box<dyn ReviewerAdapter>>,
    attempts: Mutex<AttemptLedger>,
    budgets: Option<Budgets>,
    /// Retries per node, spent on timeouts or an inadmissible returned result. A retry is a new
    /// attempt: it fences its predecessor and reserves its own budget.
    timeout_retries: u32,
    /// Gate decisions by gate node. Keyed, so two gates in one pipeline never share a verdict.
    gates: Mutex<BTreeMap<String, GateDecision>>,
    /// The campaign's prior findings, as a CAS artifact every reviewer attempt receives —
    /// labelled data resolved by the kernel, which is what makes round N+1 a re-examination
    /// of round N's claims instead of a fresh look that happens to share a repository.
    prior_findings: Option<String>,
    /// Reviewer result and gate events held until their node receipt can publish them as one
    /// batch. Each `(node, seq)` preserves emission order inside that node. Dispatch and terminal
    /// failure events are deliberately not buffered: dispatch must be durable before external
    /// execution, and a failed attempt must be durable before its retry dispatch. Their ordering
    /// across concurrently executing nodes therefore records real completion order rather than
    /// claiming whole-log determinism that the scheduler cannot provide.
    reviewer_events: Mutex<Vec<((String, u64), NewEvent)>>,
    reviewer_event_seq: Mutex<u64>,
    /// First attempts are reserved, assigned, and durably dispatched by the scheduler thread in
    /// plan order before any external model call starts. The worker removes its prepared entry.
    prepared_attempts: Mutex<BTreeMap<String, PreparedReviewerAttempt>>,
    failure_classes: Mutex<BTreeMap<String, NodeFailureClass>>,
    /// The snapshot materialized once, cloned per sandbox. Built lazily on the first sandbox
    /// request — the gate's — so a run that never reaches a sandbox never pays for it.
    template: Mutex<Option<std::sync::Arc<review_sandbox::SandboxTemplate>>>,
    /// One kernel generation has exactly one durable conclusion.
    report_published: Mutex<bool>,
    /// Latest projection of this generation's durable log. Every append advances its watermark,
    /// including events that leave the visible Ledger unchanged; an out-of-order concurrent
    /// observation drops the cache so the next reader rebuilds. Gather installs its live ingest.
    ledger_cache: Mutex<Option<LedgerProjection>>,
    replayed_invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    replayed_outputs: BTreeMap<String, DurableReceipt>,
    replayed_refusal_histories: BTreeMap<String, Vec<String>>,
    reviewer_selections: Mutex<BTreeMap<String, SelectedReviewer>>,
    reviewer_input_artifacts: Mutex<BTreeMap<String, Vec<String>>>,
    replayed_spent: u64,
}

fn persisted_verdict(
    verdict: &RunVerdict,
    convergence: &Convergence,
    has_blocked_gates: bool,
) -> Result<RunVerdictV3, String> {
    Ok(match verdict {
        RunVerdict::Pass => RunVerdictV3::Pass,
        RunVerdict::Fail(Verdict::NotConverged) => RunVerdictV3::Fail {
            reason: if convergence.authority_failures_recent > 0
                && convergence.open_blocking == 0
                && convergence.new_recent == 0
                && !has_blocked_gates
            {
                RunFailureReasonV3::AuthorityUnavailable
            } else {
                RunFailureReasonV3::NotConverged
            },
        },
        RunVerdict::Fail(Verdict::Exhausted) => RunVerdictV3::Fail {
            reason: RunFailureReasonV3::Exhausted,
        },
        RunVerdict::Fail(Verdict::Converged) => {
            return Err("invalid run verdict: converged cannot be a failure".to_string());
        }
        RunVerdict::Incomplete { missing } => RunVerdictV3::Incomplete {
            missing_nodes: missing
                .iter()
                .map(|(node, reason)| MissingNodeV2 {
                    node: node.clone(),
                    reason: reason.clone(),
                })
                .collect(),
        },
    })
}

impl<'a> Kernel<'a> {
    fn new(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        pipeline_version: u32,
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
        let reviewer_input_artifacts = replayed
            .invocations
            .iter()
            .map(|(node, invocation)| {
                (
                    node.clone(),
                    invocation
                        .inputs
                        .iter()
                        .flat_map(|port| port.artifact_ids.iter().cloned())
                        .collect(),
                )
            })
            .collect();
        Ok(Kernel {
            cas,
            store: Mutex::new(store),
            run_id,
            snapshot,
            subject,
            pipeline_version,
            authority,
            checks: Vec::new(),
            check_timeout: Duration::from_secs(3600),
            reviewers: BTreeMap::new(),
            attempts: Mutex::new(attempts),
            budgets: None,
            timeout_retries: 1,
            gates: Mutex::new(replayed.gates),
            prior_findings,
            reviewer_events: Mutex::new(Vec::new()),
            reviewer_event_seq: Mutex::new(0),
            prepared_attempts: Mutex::new(BTreeMap::new()),
            failure_classes: Mutex::new(BTreeMap::new()),
            template: Mutex::new(None),
            report_published: Mutex::new(false),
            ledger_cache: Mutex::new(None),
            replayed_invocations: replayed.invocations,
            replayed_outputs: replayed.outputs,
            replayed_refusal_histories: replayed.refusal_histories,
            reviewer_selections: Mutex::new(replayed.selected_reviewers),
            reviewer_input_artifacts: Mutex::new(reviewer_input_artifacts),
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
        pipeline_version: u32,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        Kernel::new(
            cas,
            store,
            run_id,
            snapshot,
            subject,
            pipeline_version,
            authority,
        )
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
            loaded.version(),
            authority,
        )
    }

    pub fn with_checks(mut self, checks: Vec<CheckDefinition>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_check_timeout(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
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

    /// Seed the generation-local projection with the Ledger rebuilt while its Round input was
    /// prepared. Any intervening durable suffix is folded before installation, and subsequent
    /// appends advance the watermarked cache in sequence.
    pub fn with_ledger_projection(self, mut projection: LedgerProjection) -> Result<Self, String> {
        if !projection.belongs_to(&self.run_id) {
            return Err("Ledger projection belongs to a different Campaign run".into());
        }
        {
            let store = self.store.lock().expect("event store");
            projection
                .fast_forward(*store, self.cas)
                .map_err(|error| error.to_string())?;
        }
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        Ok(self)
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

    /// Selected Attempt evidence for this exact Round epoch, loaded from the durable provenance
    /// artifacts referenced by `AttemptAdmitted@1`.
    pub fn selected_attempt_evidence(&self) -> Result<Vec<AttemptEvidence>, String> {
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let mut evidence = Vec::new();
        for event in events.into_iter().filter(|event| {
            event.event_type == EventType::AttemptAdmittedV1
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        }) {
            let payload: AttemptAdmittedPayloadV1 =
                serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
            if payload.selection != "selected" {
                continue;
            }
            let node = event.node_id.ok_or("selected Attempt has no node ID")?;
            let attempt_id = event
                .attempt_id
                .ok_or("selected Attempt has no Attempt ID")?;
            let provenance_id = payload
                .provenance_artifact
                .ok_or("selected Attempt has no provenance artifact")?;
            let provenance = self
                .cas
                .get_json(&provenance_id)
                .map_err(|error| error.to_string())?;
            if provenance["node"].as_str() != Some(node.as_str())
                || provenance["attempt"].as_str() != Some(attempt_id.as_str())
                || provenance["cost_tokens"].as_u64() != Some(payload.cost_tokens)
            {
                return Err("selected Attempt provenance contradicts its admission event".into());
            }
            evidence.push(AttemptEvidence {
                node,
                attempt_id,
                cost_tokens: payload.cost_tokens,
                usage: serde_json::from_value(provenance["usage"].clone())
                    .map_err(|error| error.to_string())?,
                context_manifest: serde_json::from_value(provenance["context_manifest"].clone())
                    .map_err(|error| error.to_string())?,
                raw_artifact: provenance["raw"]
                    .as_str()
                    .ok_or("selected Attempt provenance has no raw artifact")?
                    .to_string(),
            });
        }
        evidence.sort_by(|left, right| {
            (&left.node, &left.attempt_id).cmp(&(&right.node, &right.attempt_id))
        });
        Ok(evidence)
    }

    /// The decision a gate node reached, if it ran.
    pub fn gate_decision(&self, node_id: &str) -> Option<GateDecision> {
        self.gates.lock().expect("gates").get(node_id).cloned()
    }

    /// The ledger as it stands, derived from the log and cached only through a run-bound
    /// projection capability.
    pub fn ledger(&self) -> Ledger {
        self.with_ledger(Ledger::clone)
    }

    fn rebuild_ledger_projection(&self) -> LedgerProjection {
        LedgerProjection::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .expect("replay")
    }

    fn with_ledger<R>(&self, inspect: impl FnOnce(&Ledger) -> R) -> R {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        if let Some(projection) = cached.as_ref() {
            return inspect(projection.ledger());
        }
        // Keep the cache lock across replay. Appends release the store lock before invalidating
        // this cache, so there is no nested inverse lock order; an invalidating append can only
        // clear the rebuilt value after it becomes visible, never race an older value back in.
        let rebuilt = self.rebuild_ledger_projection();
        let result = inspect(rebuilt.ledger());
        *cached = Some(rebuilt);
        result
    }

    fn take_ledger_projection(&self) -> LedgerProjection {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        if let Some(projection) = cached.take() {
            return projection;
        }
        // See `with_ledger`: keeping this lock closes the same stale-repopulation window.
        self.rebuild_ledger_projection()
    }

    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        self.with_ledger(|ledger| ledger.convergence(policy))
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
        let appended = {
            self.store
                .lock()
                .expect("event store")
                .append(&self.run_id, self.cas, event)
                .map_err(|e| e.to_string())?
        };
        self.fold_appended_into_ledger_cache(std::slice::from_ref(&appended));
        Ok(())
    }

    fn append_batch(&self, events: &[NewEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let events: Vec<NewEvent> = events
            .iter()
            .cloned()
            .map(|event| self.bind_authority(event))
            .collect();
        let appended = {
            self.store
                .lock()
                .expect("event store")
                .append_batch(&self.run_id, self.cas, &events)
                .map_err(|e| e.to_string())?
        };
        self.fold_appended_into_ledger_cache(&appended);
        Ok(())
    }

    fn fold_appended_into_ledger_cache(&self, events: &[review_core::RunEvent]) {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        let Some(projection) = cached.as_mut() else {
            return;
        };
        for event in events {
            if event.sequence < projection.event_count() {
                // A concurrent reader rebuilt through this append before we acquired the cache.
                continue;
            }
            if event.sequence > projection.event_count()
                || projection.apply_event(event, self.cas).is_err()
            {
                // Concurrent appends may reach this lock out of sequence. Dropping the cache is
                // safe; the next reader replays the exact durable log under the cache lock.
                *cached = None;
                return;
            }
        }
    }

    fn prepare_reviewer_attempt(
        &self,
        node_id: &str,
        prior_findings_artifact: Option<&String>,
        prior_failures: &[String],
    ) -> Result<PreparedReviewerAttempt, String> {
        let refusal_history_id = (!prior_failures.is_empty())
            .then(|| {
                let value =
                    serde_json::to_value(prior_failures).map_err(|error| error.to_string())?;
                self.cas.put_json(&value).map_err(|error| error.to_string())
            })
            .transpose()?;
        let reservation = match &self.budgets {
            Some(budgets) => {
                let result = budgets.ledger.lock().expect("budget ledger").reserve(
                    &[Scope::Node(node_id.to_string()), Scope::Run],
                    budgets.attempt_cap,
                );
                Some(result.map_err(|error| {
                    if error.scope == Scope::Run {
                        self.failure_classes
                            .lock()
                            .expect("failure classes")
                            .insert(node_id.to_string(), NodeFailureClass::RunBudgetExhausted);
                    }
                    if prior_failures.is_empty() {
                        format!("never dispatched: {error}")
                    } else {
                        format!("{}; retry refused: {error}", prior_failures.join("; "))
                    }
                })?)
            }
            None => None,
        };
        let attempt = self
            .attempts
            .lock()
            .expect("attempt ledger")
            .dispatch(node_id);
        let dispatch = NewEvent::new(
            EventType::AttemptDispatchedV1,
            serde_json::json!({
                "reserved": reservation.as_ref().map(|reservation| reservation.amount),
                "prior_findings": prior_findings_artifact,
            }),
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(prior_findings_artifact.cloned().into_iter().collect());
        let mut events = Vec::with_capacity(2);
        if let Some(refusal_history_id) = &refusal_history_id {
            events.push(
                NewEvent::new(
                    EventType::AttemptInputV1,
                    serde_json::to_value(AttemptInputPayloadV1 {
                        refusal_history_id: refusal_history_id.clone(),
                    })
                    .map_err(|error| error.to_string())?,
                )
                .node(node_id)
                .attempt(attempt.to_string())
                .referencing(vec![refusal_history_id.clone()]),
            );
        }
        events.push(dispatch);
        if let Err(error) = self.append_batch(&events) {
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
            refusal_history_id,
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
        evidence: AttemptFailureEvidence<'_>,
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
        let mut event = NewEvent::new(
            EventType::AttemptFailedV1,
            serde_json::json!({ "error": error, "charged": charged }),
        )
        .node(node_id)
        .attempt(attempt.to_string());
        if let Some(raw_artifact) = evidence.raw_artifact {
            event = event.referencing(vec![raw_artifact.to_string()]);
        }
        let mut events = vec![event];
        if let Some(refusal_history) = evidence.refusal_history {
            events.push(self.feedback_event(node_id, attempt, refusal_history)?);
        }
        self.append_batch(&events)
    }

    fn feedback_event(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        refusal_history: &[String],
    ) -> Result<NewEvent, String> {
        let refusal_history_id = self
            .cas
            .put_json(&serde_json::to_value(refusal_history).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        Ok(NewEvent::new(
            EventType::AttemptFeedbackV1,
            serde_json::to_value(AttemptFeedbackPayloadV1 {
                refusal_history_id: refusal_history_id.clone(),
            })
            .map_err(|error| error.to_string())?,
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(vec![refusal_history_id]))
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

    /// Append every still-buffered node event, sorted by `(node, emission order)`, then clear the
    /// buffer. Ordinary successful nodes flush their own events with their output receipt;
    /// gather and final publication drain leftovers from failed or suppressed paths. This makes
    /// each published node batch internally canonical. It does not reorder already-durable
    /// dispatch/failure events or successful node receipts across concurrent nodes. Idempotent:
    /// a second call on an already-drained buffer is a no-op.
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
                    event_type if event_type.is_run_report() => {
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
        let convergence = self.convergence(policy);
        let verdict = run_verdict(report, &convergence);
        let outcomes: Vec<RunNodeReportV2> = report
            .outcomes
            .iter()
            .map(|(id, outcome)| {
                let outcome = match outcome {
                    NodeOutcome::Completed { outputs } => RunNodeOutcomeV2::Completed {
                        output_artifacts: artifact_ids(outputs),
                    },
                    NodeOutcome::Failed { error, .. } => RunNodeOutcomeV2::Failed {
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
        let persisted_verdict =
            persisted_verdict(&verdict, &convergence, !report.blocked_gates.is_empty())?;
        let payload = RunReportPayloadV3 {
            outcomes,
            blocked_gates: report.blocked_gates.iter().cloned().collect(),
            verdict: persisted_verdict,
            spent_tokens: self.spent(),
        };
        self.append(NewEvent::new(
            EventType::RunReportV3,
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
        let mut outputs = ArtifactMap::new();
        for port in &node.outputs {
            let value = if is_generation_prior_findings_output(port, self.pipeline_version) {
                self.prior_findings.clone().ok_or(
                    "campaign execution has no exact prior Finding Set from RoundStarted@1",
                )?
            } else if is_change_set_port(port, self.pipeline_version) {
                self.authority
                    .change_set_id
                    .clone()
                    .ok_or("generation declares ChangeSet@1 for a whole-tree Subject")?
            } else {
                return Err(format!(
                    "generation output `{}` has unsupported artifact type `{}`",
                    port.name, port.artifact_type
                ));
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
        let runner = CheckRunner::new(self.cas, sandbox.root()).with_timeout(self.check_timeout);
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

    fn run_reviewer(&self, node: &Node, node_inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        let node_id = node.id.as_str();
        let mut prepared = self
            .prepared_attempts
            .lock()
            .expect("prepared attempts")
            .remove(node_id);
        let adapter = match self.reviewers.get(node_id) {
            Some(adapter) => adapter,
            None => {
                let error = format!("no reviewer bound to node {node_id}");
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
        };

        // Prior findings arrive through the wired `prior_findings` input port — a data artifact
        // the pipeline routed from the generation node — not from ambient kernel state. A
        // reviewer that declares no such input receives none; the plan is the delivery.
        let prior_findings_port = node
            .inputs
            .iter()
            .find(|port| is_reviewer_prior_findings_input(port, self.pipeline_version))
            .map(|port| port.name.as_str());
        let prior_findings_artifact = prior_findings_port
            .and_then(|port| node_inputs.get(port))
            .and_then(|artifacts| artifacts.first())
            .cloned();
        let mut inputs = ReviewerInputs {
            finding_identity_policy: Some(self.authority.finding_identity_policy.clone()),
            ..ReviewerInputs::default()
        };
        let resolved_inputs = (|| -> Result<(), String> {
            for (port, artifacts) in node_inputs {
                let contract = node
                    .inputs
                    .iter()
                    .find(|contract| contract.name == *port)
                    .ok_or_else(|| {
                        format!("reviewer input port '{port}' has no declared contract")
                    })?;
                if is_reviewer_prior_findings_input(contract, self.pipeline_version) {
                    continue;
                }
                let is_change_set = is_change_set_port(contract, self.pipeline_version);
                let mut resolved = Vec::with_capacity(artifacts.len());
                for artifact in artifacts {
                    if is_change_set
                        && self.authority.change_set_id.as_deref() == Some(artifact.as_str())
                    {
                        resolved.push(ReviewerInputArtifact::from_resolved_change_set(
                            self.authority
                                .change_set
                                .as_ref()
                                .ok_or("Round authority has no validated Change Set input")?
                                .clone(),
                        )?);
                        continue;
                    }
                    let limit = if is_change_set {
                        MAX_CHANGE_SET_BYTES
                    } else {
                        MAX_PRIOR_FINDINGS_BYTES
                    };
                    let encoded = self
                        .cas
                        .get_bounded(artifact, limit as u64)
                        .map_err(|error| error.to_string())?;
                    if is_change_set {
                        resolved.push(ReviewerInputArtifact::change_set_from_encoded(
                            artifact.clone(),
                            &encoded,
                        )?);
                    } else {
                        let value =
                            serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
                        resolved.push(ReviewerInputArtifact::from_json(
                            artifact.clone(),
                            contract.artifact_type.clone(),
                            value,
                            encoded.len(),
                        ));
                    }
                }
                inputs.artifacts.insert(port.clone(), resolved);
            }
            Ok(())
        })();
        if let Err(error) = resolved_inputs {
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
        if let Some(artifact) = &prior_findings_artifact {
            let encoded = match self
                .cas
                .get_bounded(artifact, MAX_PRIOR_FINDINGS_BYTES as u64)
            {
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
        inputs.prior_findings_artifact_id = prior_findings_artifact.clone();

        let mut retry_failures: Vec<String> = Vec::new();
        for _ in 0..=self.timeout_retries {
            // The scheduler prepares the first attempt in plan order before spawning this
            // worker. Retries are prepared here only after the predecessor is terminal.
            let PreparedReviewerAttempt {
                attempt,
                reservation,
                refusal_history_id,
            } = match prepared.take() {
                Some(prepared) => prepared,
                None => self.prepare_reviewer_attempt(
                    node_id,
                    prior_findings_artifact.as_ref(),
                    &retry_failures,
                )?,
            };

            inputs.refusal_history_artifact_id = refusal_history_id.clone();
            inputs.refused_attempts = match refusal_history_id.as_ref() {
                Some(refusal_history_id) => {
                    let decoded = self
                        .cas
                        .get_json(refusal_history_id)
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            serde_json::from_value(value).map_err(|error| error.to_string())
                        });
                    match decoded {
                        Ok(history) => history,
                        Err(error) => {
                            self.release_prepared_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &error,
                            )?;
                            return Err(error);
                        }
                    }
                }
                None => Vec::new(),
            };
            retry_failures.clone_from(&inputs.refused_attempts);
            inputs.attempt_context = Some(ReviewerAttemptContext {
                attempt_id: attempt.to_string(),
                round: self.authority.round,
                epoch: self.authority.epoch,
                subject_id: self.authority.subject_id.clone(),
                head_snapshot_id: self.authority.head_snapshot_id.clone(),
                campaign_manifest_id: self.authority.campaign_manifest_id.clone(),
                reviewer_package_artifact_id: self
                    .authority
                    .reviewer_packages
                    .get(node_id)
                    .map(|(artifact_id, _)| artifact_id.clone()),
                reviewer_package_digest: self
                    .authority
                    .reviewer_packages
                    .get(node_id)
                    .map(|(_, digest)| digest.clone()),
                policy_ids: self.authority.policy_ids.clone(),
                reserved_tokens: reservation.as_ref().map(|reservation| reservation.amount),
            });

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
                adapter.invoke_receipted(self.cas, sandbox.root(), &inputs)
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
                        AttemptFailureEvidence::default(),
                    )?;
                    return Err(error);
                }
            };

            match invoked {
                Ok(receipted) => {
                    let returned = receipted.returned;
                    let result_value = match reviewer_result_value(&returned.output) {
                        Ok(value) => value,
                        Err(error) => {
                            retry_failures.push(failed_retry_context(
                                &attempt.to_string(),
                                "contract_error",
                                Some(error.code()),
                            ));
                            let detail = error.to_string();
                            self.fail_started_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &detail,
                                returned.cost_tokens,
                                AttemptFailureEvidence {
                                    raw_artifact: Some(&returned.raw_artifact),
                                    refusal_history: Some(&retry_failures),
                                },
                            )?;
                            continue;
                        }
                    };
                    let artifacts = (|| -> Result<(String, String), String> {
                        let sealed = sandbox.seal().map_err(|error| error.to_string())?;
                        let result_artifact = self
                            .cas
                            .put_json(&result_value)
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
                                "usage": receipted.usage,
                                "context_manifest": receipted.context_manifest,
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
                                AttemptFailureEvidence {
                                    raw_artifact: Some(&returned.raw_artifact),
                                    refusal_history: None,
                                },
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
                Err(RunnerError::MalformedOutput { raw_artifact, why }) => {
                    let error = format!("reviewer output is not a ReviewerResult@1: {why}");
                    retry_failures.push(failed_retry_context(
                        &attempt.to_string(),
                        "parse_error",
                        None,
                    ));
                    let charged = reservation
                        .as_ref()
                        .map_or(0, |reservation| reservation.amount);
                    self.fail_started_attempt(
                        node_id,
                        &attempt,
                        reservation.as_ref(),
                        &error,
                        charged,
                        AttemptFailureEvidence {
                            raw_artifact: Some(&raw_artifact),
                            refusal_history: Some(&retry_failures),
                        },
                    )?;
                    continue;
                }
                Err(RunnerError::TimedOut {
                    after_ms,
                    raw_artifact,
                }) => {
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
                    let reason = format!("timed out after {after_ms}ms");
                    retry_failures.push(fenced_retry_context(&attempt.to_string(), &reason));
                    let fenced = NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::json!({
                            "reason": reason,
                            "charged": reservation.as_ref().map(|r| r.amount),
                        }),
                    )
                    .node(node_id)
                    .attempt(attempt.to_string())
                    .referencing(raw_artifact.into_iter().collect());
                    let feedback = self.feedback_event(node_id, &attempt, &retry_failures)?;
                    self.append_batch(&[fenced, feedback])?;
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
                    // Failed: the reviewer did execute, its spend is unreported, and forgiving
                    // it would make crashing cheaper than answering. Full reservation, same
                    // rule as a timeout. Malformed answers took the durable correction loop above.
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
        Err(format!(
            "every reviewer attempt failed: {}",
            retry_failures.join("; ")
        ))
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
        let canonical = self.authority.finding_identity_policy
            == review_core::CANONICAL_FINDING_IDENTITY_POLICY;
        let mut results: Vec<(String, String, LegacyStageOutput)> = Vec::new();
        let mut load = |node: &str, id: &str, value: serde_json::Value| -> Result<(), String> {
            let output =
                reviewer_stage_output(value).map_err(|error| format!("artifact {id}: {error}"))?;
            results.push((node.to_string(), id.to_string(), output));
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
                            if canonical {
                                let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                                load(input_port, id, value)?;
                                continue;
                            }
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
        if canonical {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let mut result_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, (_, result_id, _)) in results.iter().enumerate() {
                result_indices
                    .entry(result_id.clone())
                    .or_default()
                    .push(index);
            }
            for (result_id, mut indices) in result_indices {
                let mut selected: Vec<_> = selections
                    .iter()
                    .filter(|(_, selection)| selection.result_artifact == result_id)
                    .map(|(node, _)| node.clone())
                    .collect();
                if selected.len() != indices.len() {
                    return Err(format!(
                        "selected reviewer result {result_id} has {} delivered copies and {} matching Attempts",
                        indices.len(),
                        selected.len(),
                    ));
                }
                selected.sort();
                indices.sort_by(|left, right| {
                    (&results[*left].0, *left).cmp(&(&results[*right].0, *right))
                });
                for (index, node) in indices.into_iter().zip(selected) {
                    results[index].0 = node;
                }
            }
        }
        // Canonical gather order: reviewer node id — not completion order, input-port label, or
        // artifact digest order. Legacy campaigns retain their frozen port-labelled projection.
        results.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

        let canonical_metadata = if canonical {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let reviewer_inputs = self
                .reviewer_input_artifacts
                .lock()
                .expect("reviewer inputs");
            Some(
                results
                    .iter()
                    .map(|(node, result_id, _)| {
                        let selection = selections.get(node).ok_or_else(|| {
                            format!("selected reviewer result for `{node}` has no Attempt")
                        })?;
                        if &selection.result_artifact != result_id {
                            return Err(format!(
                                "selected reviewer result for `{node}` disagrees with its Attempt"
                            ));
                        }
                        Ok((
                            selection.attempt_id.clone(),
                            reviewer_inputs.get(node).cloned().ok_or_else(|| {
                                format!("selected reviewer `{node}` has no exact invocation inputs")
                            })?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            )
        } else {
            None
        };

        let projection = self.take_ledger_projection();
        let (round, finding_count, finding_entries, reduction, projection) = {
            let mut store = self.store.lock().expect("event store");
            let mut ingest =
                Ingest::from_projection(*store, self.cas, self.run_id.clone(), projection)
                    .map_err(|e| e.to_string())?
                    .under_round(&self.authority.round_event_id);
            let reduction = match &canonical_metadata {
                Some(metadata) => {
                    let stages: Vec<_> = results
                        .iter()
                        .zip(metadata)
                        .map(
                            |((node, result_id, stage), (attempt_id, input_artifacts))| {
                                review_store::CanonicalStage {
                                    source: node,
                                    stage,
                                    attempt_id,
                                    result_artifact_id: result_id,
                                    input_artifacts,
                                    subject_snapshot_id: &self.authority.head_snapshot_id,
                                }
                            },
                        )
                        .collect();
                    Some(
                        ingest
                            .add_canonical_stage_outputs(&stages)
                            .map_err(|error| error.to_string())?,
                    )
                }
                None => {
                    let stages: Vec<_> = results
                        .iter()
                        .map(|(node, _, stage)| (node.as_str(), stage))
                        .collect();
                    ingest
                        .add_live_stage_outputs(&stages)
                        .map_err(|error| error.to_string())?;
                    None
                }
            };
            (
                ingest.ledger().round,
                ingest.ledger().len(),
                canonical.then(|| finding_set_entries(ingest.ledger())),
                reduction,
                ingest.into_projection(),
            )
        };
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        if let (Some(entries), Some(reduction)) = (finding_entries, reduction) {
            let payload = review_core::FindingSetV1 {
                subject_id: self.authority.subject_id.clone(),
                round,
                prior_finding_set_id: self.authority.prior_reduction_finding_set_id.clone(),
                reducer_version: review_core::FINDING_REDUCER_VERSION.to_string(),
                identity_policy: self.authority.finding_identity_policy.clone(),
                selected_report_ids: reduction.selected_report_ids,
                relation_ids: reduction.relation_ids,
                resolution_ids: Vec::new(),
                findings: entries,
            };
            payload.validate()?;
            let mut reduction_inputs = vec![self.authority.prior_reduction_finding_set_id.clone()];
            reduction_inputs.extend(reduction.input_artifact_ids);
            let operation_digest = review_store::content_id(&serde_json::json!({
                "reducer_version": review_core::FINDING_REDUCER_VERSION,
                "identity_policy": self.authority.finding_identity_policy,
                "inputs": reduction_inputs,
            }))
            .map_err(|error| error.to_string())?;
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::FINDING_SET_V1,
                    review_core::Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some("ledger".into()),
                        operation_id: format!(
                            "{}:{}:{}",
                            review_core::FINDING_REDUCER_VERSION,
                            self.authority.finding_identity_policy,
                            operation_digest
                        ),
                    },
                    reduction_inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            return Ok(vec![record_id]);
        }
        // The `findings` port must carry a real artifact, not a label: the scheduler delivers
        // exactly this string to whatever consumes the port, and a downstream event referencing
        // a non-CAS string would be rejected as a dangling artifact far from its cause.
        let artifact = self
            .cas
            .put_json(&serde_json::json!({
                "round": round,
                "sources": results.iter().map(|(node, _, _)| node).collect::<Vec<_>>(),
                "findings": finding_count,
            }))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }
}

fn reviewer_result_value(
    stage: &LegacyStageOutput,
) -> Result<serde_json::Value, ReviewerResultRejection> {
    let mut object =
        match serde_json::to_value(stage).map_err(|_| ReviewerResultRejection::ReportPayload)? {
            serde_json::Value::Object(object) => object,
            _ => return Err(ReviewerResultRejection::NotObject),
        };
    let reports = object
        .remove("findings")
        .ok_or(ReviewerResultRejection::UnexpectedFields)?;
    object.insert("reports".into(), reports);
    if let Some(disputes) = object
        .get_mut("disputes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for dispute in disputes {
            let dispute = dispute
                .as_object_mut()
                .ok_or(ReviewerResultRejection::MalformedDispute)?;
            let claim_id = dispute
                .remove("fp")
                .ok_or(ReviewerResultRejection::InvalidDispute)?;
            dispute.insert("claim_id".into(), claim_id);
            if !matches!(
                dispute.get("position").and_then(serde_json::Value::as_str),
                Some("confirm" | "refute")
            ) {
                return Err(ReviewerResultRejection::InvalidDispute);
            }
        }
    }
    let value = serde_json::Value::Object(object);
    review_core::validate_reviewer_result_classified(&value)?;
    Ok(value)
}

fn reviewer_stage_output(value: serde_json::Value) -> Result<LegacyStageOutput, String> {
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        _ => return Err("ReviewerResult@1 is not an object".into()),
    };
    let reports = object
        .remove("reports")
        .ok_or("ReviewerResult@1 has no reports array")?;
    object.insert("findings".into(), reports);
    serde_json::from_value(serde_json::Value::Object(object)).map_err(|error| error.to_string())
}

fn finding_set_entries(ledger: &review_store::Ledger) -> Vec<review_core::FindingSetEntryV1> {
    ledger
        .findings()
        .iter()
        .map(|finding| {
            let (file, line, location_unrecorded) =
                if finding.identity_file == review_core::legacy::CHANGE_WIDE_SENTINEL {
                    (None, None, false)
                } else if review_core::is_valid_repo_path(&finding.identity_file) {
                    (
                        Some(finding.identity_file.clone()),
                        finding.identity_line,
                        false,
                    )
                } else {
                    (None, None, true)
                };
            review_core::FindingSetEntryV1 {
                finding_id: finding.key.clone(),
                status: finding.status.as_str().to_string(),
                severity: finding.severity,
                effective_severity: finding.convergence_severity,
                scope: finding.convergence_scope_label().to_string(),
                file,
                line,
                location_unrecorded,
                title: finding.title.clone(),
                body: finding.body.clone(),
                fix: finding.fix.clone(),
                confidence: finding.confidence,
                source: finding.source.clone(),
                last_seen_round: finding.last_seen_round,
                report_ids: finding
                    .reports
                    .iter()
                    .map(|report| {
                        report
                            .artifact_id
                            .clone()
                            .unwrap_or_else(|| report.report_id.clone())
                    })
                    .collect(),
            }
        })
        .collect()
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
    pipeline_version: u32,
) -> Result<(), String> {
    if node.kind != NodeKind::Generation {
        return Ok(());
    }
    for port in &node.outputs {
        let expected = if is_generation_prior_findings_output(port, pipeline_version) {
            Some(&authority.prior_finding_set_id)
        } else if is_change_set_port(port, pipeline_version) {
            authority.change_set_id.as_ref()
        } else {
            return Err(format!(
                "generation receipt port `{}` has unsupported artifact type `{}`",
                port.name, port.artifact_type
            ));
        };
        if outputs.get(&port.name).and_then(|ids| ids.first()) != expected
            || outputs.get(&port.name).is_some_and(|ids| ids.len() != 1)
        {
            return Err(format!(
                "generation receipt port `{}` contradicts Round {} authority",
                port.name, authority.round
            ));
        }
    }
    Ok(())
}

impl Dispatch for Kernel<'_> {
    fn failure_class(&self, node_id: &str) -> Option<NodeFailureClass> {
        self.failure_classes
            .lock()
            .expect("failure classes")
            .get(node_id)
            .copied()
    }

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
        if node.kind == NodeKind::Reviewer {
            self.reviewer_input_artifacts
                .lock()
                .expect("reviewer inputs")
                .insert(node.id.clone(), artifact_ids(inputs));
        }
        if node.kind == NodeKind::Reviewer && !self.replayed_outputs.contains_key(&node.id) {
            if !self.reviewers.contains_key(&node.id) {
                return Err(format!("no reviewer bound to node {}", node.id));
            }
            let prior_findings = node
                .inputs
                .iter()
                .find(|port| is_reviewer_prior_findings_input(port, self.pipeline_version))
                .and_then(|port| inputs.get(&port.name))
                .and_then(|artifacts| artifacts.first());
            let replayed_failures = self
                .replayed_refusal_histories
                .get(&node.id)
                .cloned()
                .unwrap_or_default();
            let prepared =
                self.prepare_reviewer_attempt(&node.id, prior_findings, &replayed_failures)?;
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
            validate_generation_outputs(&self.authority, node, &outputs, self.pipeline_version)?;
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
            NodeKind::Reviewer => self.run_reviewer(node, inputs),
        }?;
        bind_single_output(node, artifacts)
    }

    fn record_outputs(&self, node: &Node, outputs: &ArtifactMap) -> Result<(), String> {
        validate_generation_outputs(&self.authority, node, outputs, self.pipeline_version)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_lineage_falls_back_to_genesis_when_a_closed_round_emitted_no_set() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let digest = cas.put(b"genesis").unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: digest.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: digest.clone(),
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: digest.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![digest.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: digest.clone(),
            demand_genesis_id: digest,
        };
        let round = review_core::RunEvent {
            event_id: "round-1".into(),
            run_id: "run".into(),
            sequence: 0,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RoundStartedPayloadV1 {
                round: 1,
                epoch: 1,
                campaign_manifest_id: campaign_manifest_id.clone(),
                subject_id: format!("sha256:{}", "b".repeat(64)),
                prior_finding_set_id: format!("sha256:{}", "c".repeat(64)),
                prior_demand_set_id: format!("sha256:{}", "d".repeat(64)),
            })
            .unwrap(),
        };
        let terminal = review_core::RunEvent {
            event_id: "report-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::RunReportV3,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RunReportPayloadV3 {
                outcomes: vec![RunNodeReportV2 {
                    node: "reviewer".into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: "run budget exhausted".into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::Exhausted,
                },
                spent_tokens: Some(1_000_000),
            })
            .unwrap(),
        };

        assert_eq!(
            canonical_prior_finding_set_id_from_events(
                &cas,
                &[round, terminal],
                2,
                &campaign_manifest_id,
                &campaign,
            )
            .unwrap(),
            campaign.finding_genesis_id
        );
    }

    #[test]
    fn unreadable_report_locations_are_omitted_from_finding_sets() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let report_id = cas.put(b"not a report").unwrap();
        let mut ledger = Ledger::default();
        ledger
            .apply_event(
                &review_core::RunEvent {
                    event_id: "finding".into(),
                    run_id: "run".into(),
                    sequence: 0,
                    event_type: EventType::FindingReportedV1,
                    occurred_at: "2026-08-26T00:00:00Z".into(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: Some("claim".into()),
                    artifact_refs: vec![report_id.clone()],
                    payload: serde_json::json!({
                        "key": "claim",
                        "round": 1,
                        "source": "correctness",
                        "report_id": report_id,
                    }),
                },
                &cas,
            )
            .unwrap();

        let entries = finding_set_entries(&ledger);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file, None);
        assert!(entries[0].location_unrecorded);
    }

    #[test]
    fn unavailable_authority_has_a_distinct_durable_failure_reason() {
        let mut convergence = Convergence {
            round: 2,
            open_blocking: 1,
            new_recent: 1,
            authority_failures_recent: 1,
            verdict: Verdict::NotConverged,
        };
        assert_eq!(
            persisted_verdict(
                &RunVerdict::Fail(Verdict::NotConverged),
                &convergence,
                false,
            )
            .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::NotConverged
            },
            "real finding blockers remain the immediate durable cause"
        );

        convergence.open_blocking = 0;
        convergence.new_recent = 0;
        assert_eq!(
            persisted_verdict(
                &RunVerdict::Fail(Verdict::NotConverged),
                &convergence,
                false,
            )
            .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::AuthorityUnavailable
            }
        );

        assert_eq!(
            persisted_verdict(&RunVerdict::Fail(Verdict::NotConverged), &convergence, true,)
                .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::NotConverged
            },
            "an explicit blocked gate is the immediate durable cause"
        );
    }

    #[test]
    fn flat_reviewer_reports_reach_the_legacy_reducer() {
        let output = reviewer_stage_output(serde_json::json!({
            "verdict": "request-changes",
            "summary": null,
            "reports": [{
                "severity": "major",
                "file": "src/a.rs",
                "line": 1,
                "title": "flat claim",
                "body": "body",
                "fix": "fix",
                "confidence": 0.9
            }],
            "benchmark_demands": [],
            "disputes": [{
                "claim_id": "prior",
                "position": "refute",
                "reason": "not reproduced"
            }]
        }))
        .unwrap();
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].file, "src/a.rs");
        assert_eq!(output.disputes[0].fp, "prior");
    }
}
