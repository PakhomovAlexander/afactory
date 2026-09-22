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

mod build_cache;
pub mod closeout;
mod review_domain;
mod reviewer_inputs;
mod reviewer_output;
pub mod scatter;
pub mod session;
pub mod task;
mod warm;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use review_check::{CheckDefinition, CheckRunner, CheckStatus, GateDecision, check_event};
use review_core::{
    CampaignManifestV1, CampaignOpenedPayloadV1, Capture as SnapshotCapture, EventType,
    IntegrationCandidateV1, IntegrationCheckV1, IntegrationChecksCompletedPayloadV1,
    IntegrationChecksV1, IntegrationCommittedPayloadV1, IntegrationConflictPayloadV1,
    IntegrationPlanV1, LegacyStageOutput, MissingNodeV2, NodeOutputReceiptPayloadV1,
    PortArtifactsV1, Producer, ProposalAcceptedPayloadV1, ProposalCandidateV1,
    RecordedSetPayloadV1, ReviewerResultContract, ReviewerResultRejection, RoundStartedPayloadV1,
    RunCacheFailureReasonV5, RunCacheFailureV5, RunCacheKindV5, RunCacheMaterializationV5,
    RunCacheSnapshotV5, RunExecutionBindingV4, RunExecutionProviderV4, RunFailureReasonV3,
    RunIsolationV4, RunNodeOutcomeV2, RunNodeReportV2, RunSandboxModeV4, RunSuppressionReasonV2,
    RunVerdictV3, ShardOutcomeV1, ShardSetV1, SliceSetAcceptedPayloadV1, SliceSetV1,
    SnapshotAffinity, SourceSnapshot, SubjectV1, run_report_closes_round,
};
use review_graph::{
    ArtifactMap, Node, NodeFailureClass, NodeKind, NodeOutcome, PortContract, RunReport,
};
use review_runner::ContextManifest;
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, CacheMaterialization, CacheSource, ContainerProvider,
    Isolation, Mode, Policy, Sandbox, admit, materialize_cache, remove_materialized_caches,
};
use review_source_git::{Manifest, manifest_diff};
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ingest, Ledger, LedgerProjection, NewEvent,
    Verdict,
};

type CacheSourceResolver = dyn Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync;

fn is_generation_prior_findings_output(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "findings"
}

fn is_generation_finding_set_output(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::FINDING_SET_V1
}

fn is_demand_set_port(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::DEMAND_SET_V1
}

fn is_reviewer_prior_findings_input(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "prior_findings"
}

fn is_reviewer_finding_set_input(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::FINDING_SET_V1
}

fn is_reviewer_prior_set_input(port: &PortContract, pipeline_version: u32) -> bool {
    is_reviewer_prior_findings_input(port, pipeline_version) || is_reviewer_finding_set_input(port)
}

fn reviewer_result_contract(node: &Node) -> Result<ReviewerResultContract, String> {
    let [port] = node.outputs.as_slice() else {
        return Err(format!(
            "reviewer `{}` must declare exactly one result output",
            node.id
        ));
    };
    ReviewerResultContract::parse_artifact_type(&port.artifact_type)
        .or_else(|| {
            (port.artifact_type == review_core::contract::OPAQUE_V1)
                .then_some(ReviewerResultContract::V1)
        })
        .ok_or_else(|| {
            format!(
                "reviewer `{}` output `{}` has unsupported result type `{}`",
                node.id, port.name, port.artifact_type
            )
        })
}

fn is_change_set_port(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::CHANGE_SET_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "change_set"
}

fn run_isolation(isolation: Isolation) -> RunIsolationV4 {
    match isolation {
        Isolation::None => RunIsolationV4::None,
        Isolation::Container => RunIsolationV4::Container,
    }
}

fn run_cache_kind(kind: CacheKind) -> RunCacheKindV5 {
    match kind {
        CacheKind::Cargo => RunCacheKindV5::Cargo,
    }
}

fn run_cache_materialization(method: CacheMaterialization) -> RunCacheMaterializationV5 {
    match method {
        CacheMaterialization::Reflink => RunCacheMaterializationV5::Reflink,
        CacheMaterialization::Copy => RunCacheMaterializationV5::Copy,
    }
}

fn cache_failure_reason(kind: CacheErrorKind) -> RunCacheFailureReasonV5 {
    match kind {
        CacheErrorKind::PolicyUnavailable => RunCacheFailureReasonV5::PolicyUnavailable,
        CacheErrorKind::SourceUnavailable => RunCacheFailureReasonV5::SourceUnavailable,
        CacheErrorKind::UnsafeContent => RunCacheFailureReasonV5::UnsafeContent,
        CacheErrorKind::LimitExceeded => RunCacheFailureReasonV5::LimitExceeded,
        CacheErrorKind::CopyLimitExceeded => RunCacheFailureReasonV5::CopyLimitExceeded,
        CacheErrorKind::ConcurrentChange => RunCacheFailureReasonV5::ConcurrentChange,
        CacheErrorKind::MaterializationFailed => RunCacheFailureReasonV5::MaterializationFailed,
    }
}

fn gate_provider_admitted(
    provider: review_config::SandboxProviderSpec,
    provided: Isolation,
    required: Isolation,
) -> bool {
    let usable = match provider {
        review_config::SandboxProviderSpec::TrustedLocal => true,
        review_config::SandboxProviderSpec::Container => provided == Isolation::Container,
    };
    usable && provided >= required
}

/// The artifact ids a node's resolved inputs carry, dropping the port labels — for reducers
/// (gather, ledger) that consume artifacts regardless of which port delivered them.
fn artifact_ids(inputs: &ArtifactMap) -> Vec<String> {
    inputs.values().flatten().cloned().collect()
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

/// Selected Attempt accounting reconstructed from its durable Task provenance artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptEvidence {
    pub node: String,
    pub attempt_id: String,
    pub cost_tokens: u128,
    pub usage: review_core::task::usage::TaskTokenUsageV3,
    pub context_manifest: ContextManifest,
    pub raw_artifact: String,
    pub result_artifact: String,
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
    pipeline_policy_id: String,
    max_rounds: u32,
    subject_id: String,
    head_snapshot_id: String,
    head_content_digest: String,
    prior_finding_set_id: String,
    prior_reduction_finding_set_id: String,
    prior_demand_set_id: String,
    finding_genesis_id: String,
    demand_genesis_id: String,
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

    /// The validated Change Set this Round reviews, when the Subject is a Diff.
    pub fn change_set(&self) -> Option<&Arc<review_store::ResolvedChangeSet>> {
        self.change_set.as_ref()
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
        let round = store
            .latest_round_started(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Round authority has no RoundStarted@1")?;
        if round.event_id != round_event_id {
            return Err("requested Round is not the active Round epoch".into());
        }
        Self::from_recorded(store, cas, run_id, round)
    }

    /// Reconstruct an exact historical Round for read-only plan validation and inspection.
    /// This does not grant dispatch authority: active effects still require the Store's
    /// current Round fence, and `load` retains its latest-epoch check.
    pub fn load_recorded(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<Self, String> {
        let round = store
            .replay(run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|event| {
                event.event_id == round_event_id && event.event_type == EventType::RoundStartedV1
            })
            .ok_or("Recorded Review Round does not exist in this Campaign")?;
        Self::from_recorded(store, cas, run_id, round)
    }

    fn from_recorded(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
        round: review_core::RunEvent,
    ) -> Result<Self, String> {
        let events = store.replay(run_id).map_err(|error| error.to_string())?;
        Self::from_history(cas, run_id, round, &events)
    }

    // Shared pure reconstruction. Prospective callers must hold Store's opaque validated
    // history preview; this function never grants live Round currentness.
    pub(crate) fn from_history(
        cas: &Cas,
        run_id: &str,
        round: review_core::RunEvent,
        events: &[review_core::RunEvent],
    ) -> Result<Self, String> {
        let opened = events
            .iter()
            .find(|e| e.event_type == EventType::CampaignOpenedV1)
            .ok_or("Round authority has no CampaignOpened@1")?;
        let opened: CampaignOpenedPayloadV1 =
            serde_json::from_value(opened.payload.clone()).map_err(|error| error.to_string())?;
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
        let prior_reduction_finding_set_id = canonical_prior_finding_set_id_from_events(
            cas,
            events,
            round.sequence,
            payload.round,
            &opened.campaign_manifest_id,
            &campaign_manifest,
        )?;
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
            pipeline_policy_id: campaign_manifest.pipeline.artifact_id,
            max_rounds: campaign_manifest.convergence.max_rounds,
            subject_id: payload.subject_id,
            head_snapshot_id: subject.head_snapshot_id,
            head_content_digest: source.content_digest,
            prior_finding_set_id: payload.prior_finding_set_id,
            prior_reduction_finding_set_id,
            prior_demand_set_id: payload.prior_demand_set_id,
            finding_genesis_id: campaign_manifest.finding_genesis_id,
            demand_genesis_id: campaign_manifest.demand_genesis_id,
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

fn canonical_prior_finding_set_id_from_events(
    cas: &Cas,
    events: &[review_core::RunEvent],
    active_sequence: u64,
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    let pipeline = cas
        .get(&campaign.pipeline.artifact_id)
        .map_err(|error| format!("canonical pipeline authority is unreadable: {error}"))?;
    let pipeline = std::str::from_utf8(&pipeline)
        .map_err(|error| format!("canonical pipeline authority is not UTF-8: {error}"))?;
    let definition = review_config::Definition::from_toml(pipeline)
        .map_err(|error| format!("canonical pipeline authority is invalid: {error}"))?;
    let ledger_nodes: BTreeSet<_> = definition
        .nodes
        .iter()
        .filter(|node| node.kind == review_config::NodeKindSpec::Ledger)
        .map(|node| node.id.as_str())
        .collect();
    if ledger_nodes.is_empty() {
        return Err("canonical pipeline authority has no ledger node".into());
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
            (payload.campaign_manifest_id == campaign_manifest_id
                && (payload.round < round
                    || (payload.round == round && event.sequence < active_sequence)))
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
        if prior_round < round && !closed {
            continue;
        }
        let mut ids = Vec::new();
        let mut saw_ledger_receipt = false;
        for event in events {
            if event.event_type != EventType::NodeOutputReceiptV1
                || event.causation_id.as_deref() != Some(prior_round_event_id)
            {
                continue;
            }
            let receipt: NodeOutputReceiptPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            if !ledger_nodes.contains(receipt.node.as_str()) {
                continue;
            }
            saw_ledger_receipt = true;
            for port in receipt.outputs {
                if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                    ids.extend(port.artifact_ids);
                    continue;
                }
                for id in port.artifact_ids {
                    let value = cas.get_json(&id).map_err(|error| {
                        format!("prior ledger output {id} is unreadable: {error}")
                    })?;
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
            if saw_ledger_receipt {
                return Err(format!(
                    "canonical Finding Set lineage round {prior_round} has a ledger receipt but no FindingSet@1 output"
                ));
            }
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
            || set.round > round
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
    proposal_candidate: Option<String>,
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

fn reviewer_result_value(
    stage: &LegacyStageOutput,
    contract: ReviewerResultContract,
    assigned_finding_ids: &[String],
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
    let entries = object
        .get_mut("disputes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(ReviewerResultRejection::MalformedDispute)?;
    for entry in entries.iter_mut() {
        let entry = entry.as_object_mut().ok_or(match contract {
            ReviewerResultContract::V1 => ReviewerResultRejection::MalformedDispute,
            ReviewerResultContract::V2 => ReviewerResultRejection::MalformedDisposition,
        })?;
        let finding_id = entry.remove("fp").ok_or(match contract {
            ReviewerResultContract::V1 => ReviewerResultRejection::InvalidDispute,
            ReviewerResultContract::V2 => ReviewerResultRejection::InvalidDisposition,
        })?;
        let key = match contract {
            ReviewerResultContract::V1 => "claim_id",
            ReviewerResultContract::V2 => "finding_id",
        };
        entry.insert(key.into(), finding_id);
        let valid = match contract {
            ReviewerResultContract::V1 => matches!(
                entry.get("position").and_then(serde_json::Value::as_str),
                Some("confirm" | "refute")
            ),
            ReviewerResultContract::V2 => matches!(
                entry.get("position").and_then(serde_json::Value::as_str),
                Some("corroborate" | "not_reproduced" | "dispute")
            ),
        };
        if !valid {
            return Err(match contract {
                ReviewerResultContract::V1 => ReviewerResultRejection::InvalidDispute,
                ReviewerResultContract::V2 => ReviewerResultRejection::InvalidDisposition,
            });
        }
    }
    if contract == ReviewerResultContract::V2 {
        let dispositions = object
            .remove("disputes")
            .ok_or(ReviewerResultRejection::MalformedDisposition)?;
        object.insert("dispositions".into(), dispositions);
    }
    let value = serde_json::Value::Object(object);
    match contract {
        ReviewerResultContract::V1 => {
            review_core::validate_reviewer_result_classified(&value)?;
        }
        ReviewerResultContract::V2 => {
            review_core::validate_reviewer_result_v2_classified(&value)?;
            let expected: BTreeSet<_> = assigned_finding_ids.iter().map(String::as_str).collect();
            let dispositions = value["dispositions"]
                .as_array()
                .expect("ReviewerResult@2 validator checked dispositions");
            let mut actual = BTreeSet::new();
            for disposition in dispositions {
                let finding_id = disposition["finding_id"]
                    .as_str()
                    .expect("ReviewerResult@2 validator checked finding_id");
                if !actual.insert(finding_id) {
                    return Err(ReviewerResultRejection::DuplicateDisposition);
                }
                if !expected.contains(finding_id) {
                    return Err(ReviewerResultRejection::UnassignedDisposition);
                }
            }
            if actual != expected {
                return Err(ReviewerResultRejection::MissingDispositionCoverage);
            }
        }
    }
    Ok(value)
}

fn reviewer_stage_output(
    value: serde_json::Value,
) -> Result<(ReviewerResultContract, LegacyStageOutput), String> {
    let contract = match (
        value.get("disputes").is_some(),
        value.get("dispositions").is_some(),
    ) {
        (true, false) => ReviewerResultContract::V1,
        (false, true) => ReviewerResultContract::V2,
        _ => return Err("reviewer result has ambiguous versioned disposition fields".into()),
    };
    match contract {
        ReviewerResultContract::V1 => review_core::validate_reviewer_result(&value)?,
        ReviewerResultContract::V2 => review_core::validate_reviewer_result_v2(&value)?,
    }
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        _ => return Err("ReviewerResult is not an object".into()),
    };
    let reports = object
        .remove("reports")
        .ok_or("ReviewerResult has no reports array")?;
    object.insert("findings".into(), reports);
    if contract == ReviewerResultContract::V2 {
        let mut dispositions = object
            .remove("dispositions")
            .ok_or("ReviewerResult@2 has no dispositions array")?;
        for disposition in dispositions
            .as_array_mut()
            .ok_or("ReviewerResult@2 dispositions is not an array")?
        {
            let disposition = disposition
                .as_object_mut()
                .ok_or("ReviewerResult@2 disposition is not an object")?;
            let finding_id = disposition
                .remove("finding_id")
                .ok_or("ReviewerResult@2 disposition has no finding_id")?;
            disposition.insert("fp".into(), finding_id);
        }
        object.insert("disputes".into(), dispositions);
    }
    serde_json::from_value(serde_json::Value::Object(object))
        .map(|stage| (contract, stage))
        .map_err(|error| error.to_string())
}

fn canonical_reduction_round(ledger_round: u32, authority_round: u32) -> Result<u32, String> {
    if ledger_round != authority_round {
        return Err(format!(
            "canonical ledger is at Round {ledger_round} but active Round authority is {authority_round}"
        ));
    }
    Ok(authority_round)
}

fn finding_set_entries(ledger: &review_store::Ledger) -> Vec<review_core::FindingSetEntryV1> {
    ledger
        .finding_views()
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
                fix: Some(finding.fix.clone()),
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

fn retain_round_assignment(
    set: &mut review_core::FindingSetV1,
    round_assignment: &serde_json::Value,
) -> Result<(), String> {
    let rows = round_assignment
        .get("prior_findings")
        .and_then(serde_json::Value::as_array)
        .ok_or("Round prior Finding assignment does not contain a prior_findings array")?;
    let mut assigned = BTreeSet::new();
    for row in rows {
        let key = row
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or("Round prior Finding assignment contains a row without a key")?;
        if !assigned.insert(key) {
            return Err("Round prior Finding assignment contains a duplicate key".into());
        }
    }
    let available: BTreeSet<_> = set
        .findings
        .iter()
        .map(|finding| finding.finding_id.as_str())
        .collect();
    if !assigned.is_subset(&available) {
        return Err(
            "Round prior Finding assignment is not a subset of its exact FindingSet@1".into(),
        );
    }
    set.findings
        .retain(|finding| assigned.contains(finding.finding_id.as_str()));
    Ok(())
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
            vec![authority.prior_finding_set_id.clone()]
        } else if is_generation_finding_set_output(port) {
            if authority.prior_reduction_finding_set_id == authority.finding_genesis_id {
                Vec::new()
            } else {
                vec![authority.prior_reduction_finding_set_id.clone()]
            }
        } else if is_change_set_port(port, pipeline_version) {
            authority.change_set_id.iter().cloned().collect()
        } else {
            return Err(format!(
                "generation receipt port `{}` has unsupported artifact type `{}`",
                port.name, port.artifact_type
            ));
        };
        if outputs.get(&port.name) != Some(&expected) {
            return Err(format!(
                "generation receipt port `{}` contradicts Round {} authority",
                port.name, authority.round
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A RunReport@6 that closes `round_event_id` with an exhausted verdict.
    fn exhausted_report(
        sequence: u64,
        round_event_id: &str,
        node: &str,
        error: &str,
    ) -> review_core::RunEvent {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        review_core::RunEvent {
            event_id: "report-1".into(),
            run_id: "run".into(),
            sequence,
            event_type: EventType::RunReportV6,
            occurred_at: "2026-08-26T00:00:02Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: Some(round_event_id.into()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(review_core::RunReportPayloadV6 {
                outcomes: vec![RunNodeReportV2 {
                    node: node.into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: error.into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::Exhausted,
                },
                spent_tokens: 1_u128.into(),
                task_accounting: review_core::TaskReviewAccountingV1 {
                    task_id: "review".into(),
                    task_revision_id: digest('1'),
                    plan_id: digest('2'),
                    task_report_id: digest('3'),
                    through_sequence: sequence,
                },
                execution: review_core::RunReportExecutionV6::Unbound {},
            })
            .unwrap(),
        }
    }

    #[test]
    fn an_unusable_container_is_not_admitted_even_when_none_was_required() {
        assert!(!gate_provider_admitted(
            review_config::SandboxProviderSpec::Container,
            Isolation::None,
            Isolation::None,
        ));
        assert!(gate_provider_admitted(
            review_config::SandboxProviderSpec::Container,
            Isolation::Container,
            Isolation::None,
        ));
    }

    #[test]
    fn canonical_reduction_refuses_a_stale_ledger_round() {
        assert_eq!(canonical_reduction_round(2, 2).unwrap(), 2);
        assert_eq!(
            canonical_reduction_round(1, 2).unwrap_err(),
            "canonical ledger is at Round 1 but active Round authority is 2"
        );
    }

    #[test]
    fn canonical_lineage_falls_back_to_genesis_when_a_closed_round_emitted_no_set() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let digest = cas.put(b"genesis").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = ["findings"]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: digest.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
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
            check_timeout_seconds: 3600,
            git_timeout_seconds: 300,
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
        let terminal = exhausted_report(1, &round.event_id, "reviewer", "run budget exhausted");

        assert_eq!(
            canonical_prior_finding_set_id_from_events(
                &cas,
                &[round, terminal],
                2,
                2,
                &campaign_manifest_id,
                &campaign,
            )
            .unwrap(),
            campaign.finding_genesis_id
        );
    }

    #[test]
    fn canonical_lineage_anchors_a_superseded_epoch_of_the_active_round() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let genesis = cas.put(b"genesis").unwrap();
        let subject = cas.put(b"subject").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = [{ name = "set", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: genesis.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: genesis.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![genesis.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: 3600,
            git_timeout_seconds: 300,
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: genesis.clone(),
            demand_genesis_id: genesis.clone(),
        };
        let set = review_core::FindingSetV1 {
            subject_id: subject.clone(),
            round: 1,
            prior_finding_set_id: genesis.clone(),
            reducer_version: review_core::FINDING_REDUCER_VERSION.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: Vec::new(),
        };
        let (set_id, _) = cas
            .put_artifact(
                review_core::contract::FINDING_SET_V1,
                review_core::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: Some("ledger".into()),
                    operation_id: "superseded-epoch-test".into(),
                },
                vec![genesis.clone()],
                Some(subject.clone()),
                serde_json::to_value(set).unwrap(),
            )
            .unwrap();
        let round_payload = |epoch| RoundStartedPayloadV1 {
            round: 1,
            epoch,
            campaign_manifest_id: campaign_manifest_id.clone(),
            subject_id: subject.clone(),
            prior_finding_set_id: genesis.clone(),
            prior_demand_set_id: genesis.clone(),
        };
        let superseded_round = review_core::RunEvent {
            event_id: "round-1-epoch-1".into(),
            run_id: "run".into(),
            sequence: 0,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(round_payload(1)).unwrap(),
        };
        let receipt = review_core::RunEvent {
            event_id: "receipt-1-epoch-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::NodeOutputReceiptV1,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: Some("ledger".into()),
            attempt_id: None,
            causation_id: Some(superseded_round.event_id.clone()),
            correlation_id: None,
            artifact_refs: vec![set_id.clone()],
            payload: serde_json::to_value(NodeOutputReceiptPayloadV1 {
                node: "ledger".into(),
                outputs: vec![PortArtifactsV1 {
                    port: "set".into(),
                    artifact_type: review_core::contract::FINDING_SET_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    optional: false,
                    snapshot_affinity: SnapshotAffinity::Any,
                    artifact_ids: vec![set_id.clone()],
                    subject_snapshot_id: None,
                }],
            })
            .unwrap(),
        };
        let active_round = review_core::RunEvent {
            event_id: "round-1-epoch-2".into(),
            run_id: "run".into(),
            sequence: 2,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:02Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(round_payload(2)).unwrap(),
        };

        assert_eq!(
            canonical_prior_finding_set_id_from_events(
                &cas,
                &[superseded_round, receipt, active_round],
                2,
                1,
                &campaign_manifest_id,
                &campaign,
            )
            .unwrap(),
            set_id
        );
    }

    #[test]
    fn canonical_lineage_refuses_an_unreadable_untyped_ledger_output() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let genesis = cas.put(b"genesis").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = ["findings"]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: genesis.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: genesis.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![genesis.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: 3600,
            git_timeout_seconds: 300,
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: genesis.clone(),
            demand_genesis_id: genesis,
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
        let unreadable_set_id = format!("sha256:{}", "f".repeat(64));
        let hex = unreadable_set_id.strip_prefix("sha256:").unwrap();
        let object = directory
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(object, b"corrupt").unwrap();
        let receipt = review_core::RunEvent {
            event_id: "receipt-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::NodeOutputReceiptV1,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: Some("ledger".into()),
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: vec![unreadable_set_id.clone()],
            payload: serde_json::to_value(NodeOutputReceiptPayloadV1 {
                node: "ledger".into(),
                outputs: vec![PortArtifactsV1 {
                    port: "set".into(),
                    artifact_type: review_core::contract::OPAQUE_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    optional: false,
                    snapshot_affinity: SnapshotAffinity::Any,
                    artifact_ids: vec![unreadable_set_id],
                    subject_snapshot_id: None,
                }],
            })
            .unwrap(),
        };
        let terminal = exhausted_report(2, &round.event_id, "ledger", "campaign exhausted");

        let error = canonical_prior_finding_set_id_from_events(
            &cas,
            &[round, receipt, terminal],
            3,
            2,
            &campaign_manifest_id,
            &campaign,
        )
        .unwrap_err();
        assert!(error.contains("prior ledger output"), "{error}");
        assert!(error.contains("unreadable"), "{error}");
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
    fn exact_finding_set_is_filtered_by_the_pinned_round_assignment() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let entry = |finding_id: String| review_core::FindingSetEntryV1 {
            finding_id,
            status: "open".into(),
            severity: review_core::Severity::Major,
            effective_severity: Some(review_core::Severity::Major),
            scope: "in".into(),
            file: Some("src/lib.rs".into()),
            line: Some(1),
            location_unrecorded: false,
            title: "claim".into(),
            body: "body".into(),
            fix: Some("fix".into()),
            confidence: Some(0.9),
            source: "correctness".into(),
            last_seen_round: 1,
            report_ids: vec![digest('d')],
        };
        let keep = digest('a');
        let declined = digest('b');
        let diagnostic = digest('c');
        let mut set = review_core::FindingSetV1 {
            subject_id: digest('d'),
            round: 1,
            prior_finding_set_id: digest('e'),
            reducer_version: review_core::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: vec![entry(keep.clone()), entry(declined), entry(diagnostic)],
        };

        retain_round_assignment(
            &mut set,
            &serde_json::json!({
                "subject_id": digest('f'),
                "round": 2,
                "prior_findings": [{"key": keep}]
            }),
        )
        .unwrap();

        assert_eq!(set.findings.len(), 1);
        assert_eq!(set.findings[0].finding_id, keep);
    }

    #[test]
    fn unavailable_authority_has_a_distinct_durable_failure_reason() {
        let mut convergence = Convergence {
            round: 2,
            open_blocking: 1,
            open_required_demands: 0,
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
        let (contract, output) = reviewer_stage_output(serde_json::json!({
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
        assert_eq!(contract, ReviewerResultContract::V1);
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].file, "src/a.rs");
        assert_eq!(output.disputes[0].fp, "prior");
    }

    #[test]
    fn reviewer_result_v2_requires_exact_disposition_coverage() {
        let stage = |ids: &[&str]| LegacyStageOutput {
            verdict: review_core::legacy::LegacyVerdict::Approve,
            summary: None,
            findings: Vec::new(),
            benchmark_demands: Vec::new(),
            disputes: ids
                .iter()
                .map(|id| review_core::legacy::LegacyDispute {
                    fp: (*id).into(),
                    position: "not_reproduced".into(),
                    reason: "the current Subject no longer reaches the failing branch".into(),
                })
                .collect(),
        };
        let assigned = vec!["finding:a".to_string(), "finding:b".to_string()];

        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::MissingDispositionCoverage
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::DuplicateDisposition
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:outside"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::UnassignedDisposition
        );

        let value = reviewer_result_value(
            &stage(&["finding:b", "finding:a"]),
            ReviewerResultContract::V2,
            &assigned,
        )
        .unwrap();
        assert!(value.get("disputes").is_none());
        assert_eq!(value["dispositions"].as_array().unwrap().len(), 2);
    }
}
