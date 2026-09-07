//! Replay of a resumed Round: the durable execution state — invocations, receipts, selected
//! reviewers, gate decisions, bindings, and spend — a new Kernel generation inherits from the log.

use std::collections::{BTreeMap, BTreeSet};

use review_check::GateDecision;
use review_core::event::{
    AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1, AttemptFailedPayloadV1,
    AttemptFeedbackPayloadV1, AttemptFencedPayloadV1, AttemptInputPayloadV1,
    AttemptReleasedPayloadV1,
};
use review_core::{
    BrokerOperationReceiptV1, EventType, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1,
    ProposalCandidateV1, ProposalPreparedPayloadV1, ReviewerExecutionBindingV1,
    RoundStartedPayloadV1, RunCacheKindV5, RunCacheSnapshotV5, RunExecutionBindingV4,
    SnapshotAffinity,
};
use review_store::{Cas, EventStore};

use crate::authority::RoundAuthority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableReceipt {
    pub(crate) payload: NodeOutputReceiptPayloadV1,
    pub(crate) attempt_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedReviewer {
    pub(crate) attempt_id: String,
    pub(crate) result_artifact: String,
    pub(crate) proposal_candidate: Option<String>,
}

#[derive(Default)]
pub(crate) struct ReplayedExecution {
    pub(crate) invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    pub(crate) outputs: BTreeMap<String, DurableReceipt>,
    pub(crate) selected_reviewers: BTreeMap<String, SelectedReviewer>,
    pub(crate) gates: BTreeMap<String, GateDecision>,
    pub(crate) execution_bindings: BTreeMap<String, RunExecutionBindingV4>,
    pub(crate) cache_snapshots: BTreeMap<(String, RunCacheKindV5), RunCacheSnapshotV5>,
    pub(crate) attempt_counts: BTreeMap<String, u64>,
    pub(crate) outstanding_attempts: Vec<(String, String, u64)>,
    pub(crate) refusal_histories: BTreeMap<String, Vec<String>>,
    pub(crate) committed_tokens: u64,
    pub(crate) fan_out_committed: BTreeMap<String, u64>,
    /// Charged tokens per Attempt node, so a resumed Round keeps counting against node caps.
    pub(crate) node_committed: BTreeMap<String, u64>,
}

pub(crate) fn replay_execution(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    authority: &RoundAuthority,
) -> Result<ReplayedExecution, String> {
    let mut replayed = ReplayedExecution::default();
    let mut reservations = BTreeMap::new();
    let mut terminal_attempts = BTreeSet::new();
    let mut terminal_charges = BTreeMap::new();
    let mut provider_operations = BTreeMap::new();
    let mut broker_authorized_usage = BTreeMap::new();
    let mut broker_observed_usage = BTreeMap::new();
    let mut attempt_nodes = BTreeMap::new();
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
            EventType::GateExecutionBoundV1 if active_epoch => {
                let node = event.node_id.ok_or("GateExecutionBound@1 has no node ID")?;
                let binding: RunExecutionBindingV4 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                if binding.node != node {
                    return Err("GateExecutionBound@1 metadata disagrees with its payload".into());
                }
                // An incomplete Gate may resolve again in this epoch after provider state
                // changes. Replay uses the latest observation; the append-only log retains all
                // earlier failed admissions for forensics.
                replayed.execution_bindings.insert(node, binding);
            }
            EventType::CacheSnapshotMaterializedV1 if active_epoch => {
                let node = event
                    .node_id
                    .ok_or("CacheSnapshotMaterialized@1 has no node ID")?;
                let snapshot: RunCacheSnapshotV5 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                if snapshot.node != node || !event.artifact_refs.contains(&snapshot.source_digest) {
                    return Err(
                        "CacheSnapshotMaterialized@1 metadata or manifest reference disagrees with its payload"
                            .into(),
                    );
                }
                let manifest: review_core::CacheManifestV1 = serde_json::from_value(
                    cas.get_json(&snapshot.source_digest)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                manifest.validate()?;
                if manifest.kind != snapshot.kind
                    || u64::try_from(manifest.entries.len()).ok() != Some(snapshot.files)
                    || manifest.bytes() != snapshot.bytes
                {
                    return Err("Cache Snapshot receipt contradicts CacheManifest@1".into());
                }
                replayed
                    .cache_snapshots
                    .insert((node, snapshot.kind), snapshot);
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
                attempt_nodes.insert(attempt.clone(), node.clone());
                if reservations
                    .insert(attempt, (node, payload.reserved.unwrap_or(0), active_epoch))
                    .is_some()
                {
                    return Err("attempt has duplicate durable dispatch events".into());
                }
            }
            EventType::ReviewerExecutionBoundV1 => {
                let binding: ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let authorized = review_core::broker_authority_usage(&binding.operations)?;
                if broker_authorized_usage
                    .insert(binding.attempt_id, authorized)
                    .is_some()
                {
                    return Err("attempt has duplicate reviewer Execution Bindings".into());
                }
            }
            EventType::BrokerOperationCompletedV1 => {
                let receipt: BrokerOperationReceiptV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let observed = broker_observed_usage
                    .entry(receipt.attempt_id)
                    .or_insert(0_u64);
                *observed = observed
                    .checked_add(receipt.charged_usage)
                    .ok_or("broker receipt usage overflow")?;
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
                terminal_charges.insert(attempt.clone(), payload.cost_tokens);
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
                                proposal_candidate: None,
                            },
                        )
                        .is_some()
                    {
                        return Err("reviewer has multiple selected attempts".into());
                    }
                }
            }
            EventType::ProposalPreparedV1 if active_epoch => {
                let payload: ProposalPreparedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                let node = event.node_id.ok_or("ProposalPrepared@1 has no node ID")?;
                let attempt = event
                    .attempt_id
                    .ok_or("ProposalPrepared@1 has no Attempt ID")?;
                let candidate: ProposalCandidateV1 = serde_json::from_value(
                    cas.get_json(&payload.candidate_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                candidate.validate().map_err(str::to_string)?;
                let selected = replayed
                    .selected_reviewers
                    .get_mut(&node)
                    .ok_or("prepared Proposal has no selected reviewer Attempt")?;
                if selected.attempt_id != attempt
                    || selected.result_artifact != payload.result_artifact_id
                    || candidate.result_artifact_id != payload.result_artifact_id
                    || selected.proposal_candidate.is_some()
                {
                    return Err("prepared Proposal contradicts its selected Attempt".into());
                }
                selected.proposal_candidate = Some(payload.candidate_artifact_id);
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
                terminal_charges.insert(attempt, payload.charged.unwrap_or(0));
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
                terminal_charges.insert(attempt, payload.charged.unwrap_or(0));
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
                terminal_charges.insert(attempt, 0);
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
    for (attempt, settled) in terminal_charges {
        let charged = settled.max(broker_observed_usage.get(&attempt).copied().unwrap_or(0));
        replayed.committed_tokens = replayed
            .committed_tokens
            .checked_add(charged)
            .ok_or("replayed token charge overflow")?;
        if let Some(node) = attempt_nodes.get(&attempt) {
            let committed = replayed.node_committed.entry(node.clone()).or_default();
            *committed = committed
                .checked_add(charged)
                .ok_or("replayed node token charge overflow")?;
        }
        if let Some(scatter) = attempt_nodes
            .get(&attempt)
            .and_then(|node| node.split_once("#slice:").map(|(scatter, _)| scatter))
        {
            let committed = replayed
                .fan_out_committed
                .entry(scatter.to_string())
                .or_default();
            *committed = committed
                .checked_add(charged)
                .ok_or("replayed fan-out token charge overflow")?;
        }
    }
    for (attempt, (node, reserved, active_epoch)) in reservations {
        // A fenced attempt charges conservatively. The dispatch reservation covers ordinary
        // reviewers; a Broker binding is also a durable reservation because an in-flight
        // connector can finish after another process fences the attempt. Observed receipts are
        // included explicitly so recovery remains correct even for older or partial bindings.
        let charged = reserved
            .max(broker_authorized_usage.get(&attempt).copied().unwrap_or(0))
            .max(broker_observed_usage.get(&attempt).copied().unwrap_or(0));
        replayed.committed_tokens = replayed
            .committed_tokens
            .checked_add(charged)
            .ok_or("replayed token charge overflow")?;
        {
            let committed = replayed.node_committed.entry(node.clone()).or_default();
            *committed = committed
                .checked_add(charged)
                .ok_or("replayed node token charge overflow")?;
        }
        if let Some(scatter) = node.split_once("#slice:").map(|(scatter, _)| scatter) {
            let committed = replayed
                .fan_out_committed
                .entry(scatter.to_string())
                .or_default();
            *committed = committed
                .checked_add(charged)
                .ok_or("replayed fan-out token charge overflow")?;
        }
        if active_epoch {
            replayed.outstanding_attempts.push((node, attempt, charged));
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
