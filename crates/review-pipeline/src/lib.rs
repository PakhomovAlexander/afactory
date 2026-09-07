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

pub mod scatter;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use review_attempt::{
    AttemptId, AttemptLedger, Budget, BudgetLedger, BudgetScope, Receipt, Reservation, Selection,
};
use review_broker::{
    AuthorityError, Broker, BrokerClient, BrokerHandle, Connector, Credential, LeaseAuthority,
    ReceiptError, ReceiptSink,
};
use review_check::{CheckDefinition, CheckRunner, CheckStatus, Command, GateDecision, check_event};
use review_core::event::{
    AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1, AttemptFailedPayloadV1,
    AttemptFeedbackPayloadV1, AttemptFencedPayloadV1, AttemptInputPayloadV1,
    AttemptReleasedPayloadV1,
};
use review_core::{
    BrokerCredentialModeV1, BrokerLeaseV1, BrokerOperationReceiptV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, Capture as SnapshotCapture, EventType, IntegrationCandidateV1,
    IntegrationCheckV1, IntegrationChecksCompletedPayloadV1, IntegrationChecksV1,
    IntegrationCommittedPayloadV1, IntegrationConflictPayloadV1, IntegrationPlanV1,
    IntegrationPreparedPayloadV1, LegacyStageOutput, MAX_CHANGE_SET_BYTES,
    MAX_PRIOR_FINDINGS_BYTES, MissingNodeV2, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1,
    PortArtifactsV1, Producer, ProposalAcceptedPayloadV1, ProposalCandidateV1,
    ProposalPreparedPayloadV1, ProposalRefusalReasonV1, ProposalRefusedPayloadV1,
    RecordedSetPayloadV1, ReviewerExecutionBindingV1, ReviewerResultContract,
    ReviewerResultRejection, RoundStartedPayloadV1, RunCacheFailureReasonV5, RunCacheFailureV5,
    RunCacheKindV5, RunCacheMaterializationV5, RunCacheSnapshotV5, RunExecutionBindingV4,
    RunExecutionProviderV4, RunFailureReasonV3, RunIsolationV4, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportPayloadV3, RunReportPayloadV4, RunReportPayloadV5, RunSandboxModeV4,
    RunSuppressionReasonV2, RunVerdictV3, ShardOutcomeV1, ShardReceiptV1, ShardSetV1,
    SliceSetAcceptedPayloadV1, SliceSetV1, SnapshotAffinity, SourceSnapshot, SubjectV1,
    run_report_closes_round,
};
use review_graph::{
    ArtifactMap, Dispatch, Node, NodeFailureClass, NodeKind, NodeOutcome, PortContract, RunReport,
};
use review_runner::{
    ContextManifest, ReviewerAdapter, ReviewerAttemptContext, ReviewerInputArtifact,
    ReviewerInputs, ReviewerProposalDeclaration, RunnerError, TokenUsage,
};
use review_sandbox::{
    CacheError, CacheErrorKind, CacheKind, CacheMaterialization, CacheSource, ContainerProvider,
    Isolation, Mode, Policy, Sandbox, admit, materialize_cache, remove_materialized_caches,
};
use review_source_git::{Manifest, manifest_diff};
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ingest, Ledger, LedgerProjection, NewEvent,
    StoreError, Verdict,
};

type CacheSourceResolver = dyn Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync;

/// Machine-local capability material for one brokered reviewer. Neither field is captured in
/// project authority or durable evidence; only bounded symbolic operation policy is.
pub struct BrokerProvider {
    credential: Vec<u8>,
    connector: Arc<dyn Connector>,
}

impl BrokerProvider {
    pub fn new(
        credential: impl Into<Vec<u8>>,
        connector: Arc<dyn Connector>,
    ) -> Result<Self, String> {
        let credential = credential.into();
        Credential::new(credential.clone()).map_err(|error| error.to_string())?;
        Ok(Self {
            credential,
            connector,
        })
    }
}

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
        Isolation::Process => RunIsolationV4::Process,
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

    pub fn finding_identity_policy(&self) -> &str {
        &self.finding_identity_policy
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
                round.sequence,
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

fn canonical_prior_finding_set_id(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    round_sequence: u64,
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    canonical_prior_finding_set_id_from_events(
        cas,
        &events,
        round_sequence,
        round,
        campaign_manifest_id,
        campaign,
    )
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

enum PreparedProposal {
    None,
    Prepared {
        candidate_artifact: String,
        event: NewEvent,
    },
    Refused(NewEvent),
}

#[derive(Default)]
struct ReplayedExecution {
    invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    outputs: BTreeMap<String, DurableReceipt>,
    selected_reviewers: BTreeMap<String, SelectedReviewer>,
    gates: BTreeMap<String, GateDecision>,
    execution_bindings: BTreeMap<String, RunExecutionBindingV4>,
    cache_snapshots: BTreeMap<(String, RunCacheKindV5), RunCacheSnapshotV5>,
    attempt_counts: BTreeMap<String, u64>,
    outstanding_attempts: Vec<(String, String, u64)>,
    refusal_histories: BTreeMap<String, Vec<String>>,
    committed_tokens: u64,
    fan_out_committed: BTreeMap<String, u64>,
    /// Charged tokens per Attempt node, so a resumed Round keeps counting against node caps.
    node_committed: BTreeMap<String, u64>,
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

/// The persisted classification of a scheduler suppression. `RunSuppressionReasonV2` is the
/// schema-pinned vocabulary every RunReport since `@2` carries in `outcomes[].reason`; the
/// graph's own enum never reaches a payload through `Debug`.
pub fn run_suppression_reason(reason: review_graph::SuppressionReason) -> RunSuppressionReasonV2 {
    match reason {
        review_graph::SuppressionReason::GateBlocked => RunSuppressionReasonV2::GateBlocked,
        review_graph::SuppressionReason::UpstreamMissing => RunSuppressionReasonV2::UpstreamMissing,
    }
}

/// The string a suppressed node's `missing_nodes[].reason` carries: the same serde name as its
/// `outcomes[].reason`, so one payload cannot spell one suppression two ways.
fn suppression_reason_label(reason: review_graph::SuppressionReason) -> String {
    match serde_json::to_value(run_suppression_reason(reason))
        .expect("RunSuppressionReasonV2 serializes")
    {
        serde_json::Value::String(name) => name,
        other => other.to_string(),
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
            NodeOutcome::Suppressed { reason } => {
                Some((id.clone(), suppression_reason_label(*reason)))
            }
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
    /// Absent only for frozen v1/v2 pipeline semantics. V3 resolves this exact Gate binding from
    /// captured authority before any candidate check executes.
    gate_execution: Option<review_config::GateExecutionSpec>,
    integration: Option<review_config::IntegrationSpec>,
    /// Optional machine-resolved provider. The CLI normally lets the kernel probe locally;
    /// embedding callers and deterministic boundary tests may bind an already-probed provider.
    container_provider: Option<ContainerProvider>,
    /// Machine-local sources resolved by the CLI. Host paths never enter captured pipeline
    /// authority or durable events.
    cache_sources: BTreeMap<CacheKind, CacheSource>,
    cache_source_resolver: Option<Arc<CacheSourceResolver>>,
    execution_bindings: Mutex<BTreeMap<String, RunExecutionBindingV4>>,
    cache_snapshots: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheSnapshotV5>>,
    cache_failures: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheFailureV5>>,
    reviewers: BTreeMap<String, Box<dyn ReviewerAdapter>>,
    reviewer_execution: BTreeMap<String, review_config::ReviewerExecutionSpec>,
    broker_providers: BTreeMap<String, BrokerProvider>,
    demand_requirements: BTreeMap<String, review_core::DemandRequirement>,
    slicing: BTreeMap<String, scatter::StaticSlicePolicy>,
    closeouts: BTreeMap<String, String>,
    static_node_ids: BTreeSet<String>,
    /// Dynamic reviewer ID -> owning static Scatter. The binding is installed only after the
    /// complete SliceSet is durable and lets existing reviewer authority remain keyed by the
    /// captured static graph.
    dynamic_reviewer_bases: Mutex<BTreeMap<String, String>>,
    fan_out_cap: Option<u64>,
    /// Worker nodes with their own Attempt cap (`[[nodes]] budget.attempt`); every other node
    /// reserves the pipeline-wide attempt cap.
    node_attempt_caps: BTreeMap<String, u64>,
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
    /// Validated graph provenance: downstream node -> input port -> exact upstream node, output
    /// port, and kind. The kind distinguishes reviewer selection from deterministic node output.
    input_bindings: review_config::InputBindings,
    /// Outputs made durable in this scheduler run, keyed by their producing node. Downstream
    /// canonical reducers consult only this map plus the validated graph when binding artifacts.
    node_outputs: Mutex<BTreeMap<String, ArtifactMap>>,
    replayed_spent: u64,
    replayed_fan_out_spent: BTreeMap<String, u64>,
    replayed_node_spent: BTreeMap<String, u64>,
}

struct KernelBrokerBoundary<'kernel, 'store> {
    kernel: &'kernel Kernel<'store>,
}

impl LeaseAuthority for KernelBrokerBoundary<'_, '_> {
    fn ensure_current(
        &self,
        lease: &BrokerLeaseV1,
        handle: &BrokerHandle,
    ) -> Result<(), AuthorityError> {
        if lease.campaign_id != self.kernel.run_id
            || lease.round_event_id != self.kernel.authority.round_event_id
            || lease.node_id.trim().is_empty()
        {
            return Err(AuthorityError);
        }
        let events = self
            .kernel
            .store
            .lock()
            .expect("event store")
            .replay(&self.kernel.run_id)
            .map_err(|_| AuthorityError)?;
        let latest_round = events
            .iter()
            .rev()
            .find(|event| event.event_type == EventType::RoundStartedV1)
            .map(|event| event.event_id.as_str());
        if latest_round != Some(lease.round_event_id.as_str()) {
            return Err(AuthorityError);
        }
        let mut latest_attempt = None;
        let mut bound = false;
        let mut terminal = false;
        for event in events.iter().filter(|event| {
            event.causation_id.as_deref() == Some(lease.round_event_id.as_str())
                && event.node_id.as_deref() == Some(lease.node_id.as_str())
        }) {
            if event.event_type == EventType::AttemptDispatchedV1 {
                latest_attempt = event.attempt_id.as_deref();
            }
            if event.attempt_id.as_deref() != Some(lease.attempt_id.as_str()) {
                continue;
            }
            if event.event_type == EventType::ReviewerExecutionBoundV1 {
                let binding: ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|_| AuthorityError)?;
                bound = binding.admitted
                    && binding.lease_epoch == lease.lease_epoch
                    && binding.broker_handle.as_deref() == Some(handle.as_str());
            }
            if matches!(
                event.event_type,
                EventType::AttemptAdmittedV1
                    | EventType::AttemptFailedV1
                    | EventType::AttemptFencedV1
                    | EventType::AttemptReleasedV1
            ) {
                terminal = true;
            }
        }
        (latest_attempt == Some(lease.attempt_id.as_str()) && bound && !terminal)
            .then_some(())
            .ok_or(AuthorityError)
    }
}

impl ReceiptSink for KernelBrokerBoundary<'_, '_> {
    fn record(&self, receipt: &BrokerOperationReceiptV1) -> Result<(), ReceiptError> {
        let event = NewEvent::new(
            EventType::BrokerOperationCompletedV1,
            serde_json::to_value(receipt).map_err(|_| ReceiptError::Unavailable)?,
        )
        .node(&receipt.node)
        .attempt(&receipt.attempt_id)
        .correlating(&receipt.handle_id);
        let event = self.kernel.bind_authority(event);
        let appended = self.kernel.store.lock().expect("event store").append(
            &self.kernel.run_id,
            self.kernel.cas,
            event,
        );
        match appended {
            Ok(event) => {
                self.kernel
                    .fold_appended_into_ledger_cache(std::slice::from_ref(&event));
                Ok(())
            }
            Err(StoreError::AttemptNotCurrent) => Err(ReceiptError::AuthorityRevoked),
            Err(_) => Err(ReceiptError::Unavailable),
        }
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
            gate_execution: None,
            integration: None,
            container_provider: None,
            cache_sources: BTreeMap::new(),
            cache_source_resolver: None,
            execution_bindings: Mutex::new(replayed.execution_bindings),
            cache_snapshots: Mutex::new(replayed.cache_snapshots),
            cache_failures: Mutex::new(BTreeMap::new()),
            reviewers: BTreeMap::new(),
            reviewer_execution: BTreeMap::new(),
            broker_providers: BTreeMap::new(),
            demand_requirements: BTreeMap::new(),
            slicing: BTreeMap::new(),
            closeouts: BTreeMap::new(),
            static_node_ids: BTreeSet::new(),
            dynamic_reviewer_bases: Mutex::new(BTreeMap::new()),
            fan_out_cap: None,
            node_attempt_caps: BTreeMap::new(),
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
            input_bindings: BTreeMap::new(),
            node_outputs: Mutex::new(BTreeMap::new()),
            replayed_spent: replayed.committed_tokens,
            replayed_fan_out_spent: replayed.fan_out_committed,
            replayed_node_spent: replayed.node_committed,
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
        let mut kernel = Kernel::for_subject(
            cas,
            store,
            run_id,
            snapshot,
            loaded.subject_kind(),
            loaded.version(),
            authority,
        )?;
        kernel.input_bindings = loaded.input_bindings();
        kernel.demand_requirements = loaded.demand_requirements().clone();
        kernel.gate_execution = loaded.gate_execution().cloned();
        kernel.integration = loaded.integration().cloned();
        kernel.reviewer_execution = loaded.reviewer_execution().clone();
        kernel.slicing = loaded
            .slicing()
            .iter()
            .map(|(node, policy)| {
                Ok((
                    node.clone(),
                    scatter::StaticSlicePolicy {
                        scatter_node: policy.scatter.clone(),
                        max_paths_per_slice: policy.max_paths_per_slice,
                        max_fanout: policy.max_fanout,
                        coverage: policy.coverage,
                        all_shards_required: policy.all_shards_required,
                        closeout: policy
                            .closeout_policy()
                            .map_err(|error| error.to_string())?,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        kernel.closeouts = loaded.closeouts().clone();
        kernel.static_node_ids = loaded.plan_order().iter().cloned().collect();
        kernel.fan_out_cap = loaded.budgets().and_then(|budgets| budgets.fan_out);
        kernel.node_attempt_caps = loaded.node_attempt_caps().clone();
        Ok(kernel)
    }

    pub fn with_checks(mut self, checks: Vec<CheckDefinition>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_check_timeout(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
        self
    }

    pub fn with_cache_sources(mut self, sources: BTreeMap<CacheKind, CacheSource>) -> Self {
        self.cache_sources = sources;
        self
    }

    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync + 'static,
    {
        self.cache_source_resolver = Some(Arc::new(resolver));
        self
    }

    pub fn with_container_provider(mut self, provider: ContainerProvider) -> Self {
        self.container_provider = Some(provider);
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

    /// Bind machine-local connector and credential material for one v4 brokered reviewer.
    /// Project authority names only the operation policy; these bytes remain outside snapshots,
    /// events, reviewer context, and release artifacts.
    pub fn with_broker_provider(
        mut self,
        node_id: impl Into<String>,
        provider: BrokerProvider,
    ) -> Self {
        self.broker_providers.insert(node_id.into(), provider);
        self
    }

    /// Cap the run. Reservation before every dispatch; a dispatch that cannot reserve does not
    /// happen, and the refusal names the scope that said no.
    pub fn with_budgets(mut self, attempt_cap: u64, run_cap: u64) -> Self {
        let mut ledger = BudgetLedger::default()
            .with_limit(BudgetScope::Run, Budget::of(run_cap))
            .with_committed(BudgetScope::Run, self.replayed_spent);
        let fan_out_cap = self.fan_out_cap.unwrap_or(run_cap);
        for policy in self.slicing.values() {
            let scope = BudgetScope::FanOut(policy.scatter_node.clone());
            ledger = ledger.with_limit(scope.clone(), Budget::of(fan_out_cap));
            ledger = ledger.with_committed(
                scope,
                self.replayed_fan_out_spent
                    .get(&policy.scatter_node)
                    .copied()
                    .unwrap_or(0),
            );
        }
        // A node that declared its own cap is limited at its own scope too, so a retry storm on
        // one cheap Worker cannot spend what the pipeline reserved for the deep one.
        for (node, cap) in &self.node_attempt_caps {
            ledger = ledger
                .with_limit(BudgetScope::Node(node.clone()), Budget::of(*cap))
                .with_committed(
                    BudgetScope::Node(node.clone()),
                    self.replayed_node_spent.get(node).copied().unwrap_or(0),
                );
        }
        self.budgets = Some(Budgets {
            attempt_cap,
            ledger: Mutex::new(ledger),
        });
        self
    }

    /// What one Attempt of `node_id` reserves: the node's own cap, a dynamic shard's owning
    /// Scatter cap, else the pipeline-wide attempt cap.
    fn attempt_reservation(&self, node_id: &str, budgets: &Budgets) -> u64 {
        let base = self.reviewer_binding_node(node_id);
        self.node_attempt_caps
            .get(node_id)
            .or_else(|| self.node_attempt_caps.get(&base))
            .copied()
            .unwrap_or(budgets.attempt_cap)
    }

    fn reviewer_binding_node(&self, node_id: &str) -> String {
        self.dynamic_reviewer_bases
            .lock()
            .expect("dynamic reviewer bases")
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| node_id.to_string())
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
                .committed(&BudgetScope::Run)
        })
    }

    /// Charge accumulated by the dynamic reviewers owned by one captured Scatter.
    pub fn fan_out_spent(&self, scatter: &str) -> Option<u64> {
        self.budgets.as_ref().map(|budgets| {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .committed(&BudgetScope::FanOut(scatter.to_string()))
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
                result_artifact: payload
                    .result_artifact
                    .ok_or("selected Attempt has no result artifact")?,
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

    /// Records how long one reviewer Attempt took and what its Provider reported, in the store's
    /// sidecar. A failed sidecar write must never change an Attempt's fate, so it is not an
    /// error here: the report then shows that Attempt as "not recorded".
    fn record_attempt_wall(
        &self,
        node_id: &str,
        attempt: &impl std::fmt::Display,
        started: SystemTime,
        elapsed: Duration,
        usage: Option<&review_runner::TokenUsage>,
    ) {
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let wall = review_store::AttemptWall {
            run_id: self.run_id.clone(),
            attempt_id: attempt.to_string(),
            node_id: node_id.to_string(),
            round: self.authority.round,
            epoch: self.authority.epoch,
            started_unix_ms: started.duration_since(UNIX_EPOCH).map(millis).unwrap_or(0),
            elapsed_ms: millis(elapsed),
            usage: usage.map(|usage| review_store::AttemptUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                chargeable_tokens: usage.chargeable_tokens,
            }),
        };
        let _ = self
            .store
            .lock()
            .expect("event store")
            .record_attempt_wall(&wall);
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

    /// M9 transitions intentionally occur after the Round has closed, so they cannot carry the
    /// Round causation installed by `append`. Keep that exception closed over the exact event
    /// vocabulary instead of exposing a general authority-bypass primitive.
    fn append_integration_transition(&self, event: NewEvent) -> Result<(), String> {
        if !matches!(
            event.event_type,
            EventType::IntegrationPreparedV1
                | EventType::IntegrationConflictV1
                | EventType::IntegrationChecksCompletedV1
        ) || event.causation_id.is_some()
        {
            return Err("invalid non-transactional Integration transition".into());
        }
        let appended = self
            .store
            .lock()
            .expect("event store")
            .append(&self.run_id, self.cas, event)
            .map_err(|error| error.to_string())?;
        self.fold_appended_into_ledger_cache(std::slice::from_ref(&appended));
        Ok(())
    }

    /// The sole M9 visibility boundary: zero or more Change Attestations immediately followed
    /// by one Integration commit. `EventStore::append_batch` supplies the SQLite transaction.
    fn commit_integration(&self, events: &[NewEvent]) -> Result<(), String> {
        let Some(last) = events.last() else {
            return Err("empty Integration commit batch".into());
        };
        if last.event_type != EventType::IntegrationCommittedV1
            || last.causation_id.is_some()
            || events[..events.len() - 1].iter().any(|event| {
                event.event_type != EventType::ChangeAttestedV1 || event.causation_id.is_some()
            })
        {
            return Err("Integration commit batch contains an unauthorized transition".into());
        }
        let appended = self
            .store
            .lock()
            .expect("event store")
            .append_batch(&self.run_id, self.cas, events)
            .map_err(|error| error.to_string())?;
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
                let base = self.reviewer_binding_node(node_id);
                let mut scopes = vec![BudgetScope::Node(node_id.to_string())];
                if base != node_id {
                    scopes.push(BudgetScope::FanOut(base));
                }
                scopes.push(BudgetScope::Run);
                let amount = self.attempt_reservation(node_id, budgets);
                let result = budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .reserve(&scopes, amount);
                Some(result.map_err(|error| {
                    if error.scope == BudgetScope::Run {
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
                        reason: run_suppression_reason(*reason),
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
        let blocked_gates = report.blocked_gates.iter().cloned().collect();
        let spent_tokens = self.spent();
        if self.gate_execution.is_some() {
            let execution_bindings: Vec<_> = self
                .execution_bindings
                .lock()
                .expect("execution bindings")
                .values()
                .cloned()
                .collect();
            let cache_snapshots: Vec<_> = self
                .cache_snapshots
                .lock()
                .expect("cache snapshots")
                .values()
                .cloned()
                .collect();
            let cache_failures: Vec<_> = self
                .cache_failures
                .lock()
                .expect("cache failures")
                .values()
                .cloned()
                .collect();
            let cache_requested = self
                .gate_execution
                .as_ref()
                .is_some_and(|binding| !binding.caches.is_empty());
            if !cache_requested {
                let payload = RunReportPayloadV4 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                };
                self.append(NewEvent::new(
                    EventType::RunReportV4,
                    serde_json::to_value(payload).map_err(|e| e.to_string())?,
                ))?;
            } else {
                let manifest_refs = cache_snapshots
                    .iter()
                    .map(|snapshot| snapshot.source_digest.clone())
                    .collect();
                let payload = RunReportPayloadV5 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                    cache_snapshots,
                    cache_failures,
                };
                self.append(
                    NewEvent::new(
                        EventType::RunReportV5,
                        serde_json::to_value(payload).map_err(|e| e.to_string())?,
                    )
                    .referencing(manifest_refs),
                )?;
            }
        } else {
            let payload = RunReportPayloadV3 {
                outcomes,
                blocked_gates,
                verdict: persisted_verdict,
                spent_tokens,
            };
            self.append(NewEvent::new(
                EventType::RunReportV3,
                serde_json::to_value(payload).map_err(|e| e.to_string())?,
            ))?;
        }
        *published = true;
        if verdict == RunVerdict::Pass
            && self.integration.is_some()
            && self.authority.round < self.authority.max_rounds
        {
            self.integrate_selected_proposals()?;
        }
        Ok(verdict)
    }

    /// Compose every eligible, disjoint Proposal into one checked internal Snapshot. This is
    /// deliberately callable for deterministic boundary tests; normal execution reaches it only
    /// after a passing terminal RunReport.
    pub fn integrate_selected_proposals(&self) -> Result<Option<String>, String> {
        let Some(policy) = self.integration.as_ref() else {
            return Ok(None);
        };
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let terminal = events.iter().rev().find(|event| {
            event.event_type.is_run_report()
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        });
        if terminal.and_then(|event| event.payload.pointer("/verdict/kind"))
            != Some(&serde_json::Value::String("pass".into()))
        {
            return Err(
                "automatic Integration requires the current Round's passing conclusion".into(),
            );
        }
        if events.iter().any(|event| {
            event.event_type == EventType::IntegrationCommittedV1
                && event
                    .payload
                    .get("prior_subject_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.authority.subject_id.as_str())
        }) {
            return Ok(None);
        }

        let ledger = self.ledger();
        let mut report_to_finding = BTreeMap::new();
        for finding in ledger.finding_views() {
            for report in &finding.reports {
                report_to_finding.insert(report.report_id.clone(), finding.key.clone());
                if let Some(artifact_id) = &report.artifact_id {
                    report_to_finding.insert(artifact_id.clone(), finding.key.clone());
                }
            }
        }
        let priorities: BTreeMap<&str, u32> = policy
            .reviewer_priority
            .iter()
            .enumerate()
            .map(|(index, node)| (node.as_str(), index as u32))
            .collect();
        let default_priority = u32::try_from(priorities.len()).unwrap_or(u32::MAX);
        let mut selected = Vec::new();
        let mut seen_patches = BTreeSet::new();
        for event in events.iter().filter(|event| {
            event.event_type == EventType::ProposalAcceptedV1
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        }) {
            let accepted: ProposalAcceptedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            let node = event
                .node_id
                .as_deref()
                .ok_or("accepted Proposal has no producing node")?;
            let binding_node = self.reviewer_binding_node(node);
            if !self
                .reviewer_execution
                .get(&binding_node)
                .is_some_and(|binding| binding.auto_apply)
            {
                continue;
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                self.cas
                    .get_json(&accepted.proposal_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            review_store::validate_envelope(&envelope)?;
            let proposal: review_core::PatchProposal =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            proposal.check_shape().map_err(str::to_string)?;
            if !proposal.auto_apply_nominated
                || proposal.base_snapshot_id != self.authority.head_snapshot_id
                || !seen_patches.insert(proposal.patch_artifact_id.clone())
            {
                continue;
            }
            if proposal.paths.iter().any(|path| {
                policy.protected_paths.iter().any(|protected| {
                    path == protected
                        || path
                            .strip_prefix(protected)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
            }) {
                self.record_integration_conflict(
                    &[accepted.proposal_id],
                    &proposal.paths,
                    "Proposal changes a protected path",
                )?;
                return Ok(None);
            }
            let candidate: ProposalCandidateV1 = serde_json::from_value(
                self.cas
                    .get_json(&accepted.candidate_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            candidate.validate().map_err(str::to_string)?;
            if candidate.base_snapshot_id != proposal.base_snapshot_id
                || candidate.patch_artifact_id != proposal.patch_artifact_id
                || candidate.paths != proposal.paths
            {
                return Err("accepted Proposal contradicts its sealed candidate".into());
            }
            let mut finding_ids = BTreeSet::new();
            for claim in &proposal.finding_refs {
                let finding = match claim.kind {
                    review_core::ClaimRefKind::Finding => {
                        ledger.finding_view(&claim.id).map(|finding| finding.key)
                    }
                    review_core::ClaimRefKind::Report => report_to_finding.get(&claim.id).cloned(),
                };
                let Some(finding) = finding else {
                    self.record_integration_conflict(
                        std::slice::from_ref(&accepted.proposal_id),
                        &proposal.paths,
                        "Proposal claim no longer resolves in the exact Finding view",
                    )?;
                    return Ok(None);
                };
                finding_ids.insert(finding);
            }
            for evidence in &proposal.evidence_ids {
                self.cas.verify(evidence).map_err(|error| {
                    format!("Proposal evidence `{evidence}` is not durable: {error}")
                })?;
            }
            selected.push((
                IntegrationCandidateV1 {
                    proposal_id: accepted.proposal_id,
                    candidate_artifact_id: accepted.candidate_artifact_id,
                    node_id: node.to_string(),
                    priority: priorities
                        .get(binding_node.as_str())
                        .copied()
                        .unwrap_or(default_priority),
                    patch_artifact_id: proposal.patch_artifact_id,
                    derived_manifest_artifact_id: candidate.derived_manifest_artifact_id,
                    paths: proposal.paths,
                    finding_ids: finding_ids.into_iter().collect(),
                    evidence_ids: proposal.evidence_ids,
                },
                accepted.proposal_artifact_id,
            ));
        }
        if selected.is_empty() {
            return Ok(None);
        }
        selected.sort_by(|left, right| {
            (&left.0.priority, &left.0.node_id, &left.0.proposal_id).cmp(&(
                &right.0.priority,
                &right.0.node_id,
                &right.0.proposal_id,
            ))
        });
        for left in 0..selected.len() {
            for right in left + 1..selected.len() {
                let overlap: Vec<String> = selected[left]
                    .0
                    .paths
                    .iter()
                    .filter(|a| selected[right].0.paths.iter().any(|b| paths_overlap(a, b)))
                    .cloned()
                    .collect();
                if !overlap.is_empty() {
                    self.record_integration_conflict(
                        &[
                            selected[left].0.proposal_id.clone(),
                            selected[right].0.proposal_id.clone(),
                        ],
                        &overlap,
                        "selected Proposals overlap; semantic merging is forbidden",
                    )?;
                    return Ok(None);
                }
            }
        }

        let mut entries: BTreeMap<String, review_source_git::Entry> = self
            .snapshot
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        for (candidate, _) in &selected {
            let derived: Manifest = serde_json::from_value(
                self.cas
                    .get_json(&candidate.derived_manifest_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            derived.validate().map_err(|error| error.to_string())?;
            for path in &candidate.paths {
                match derived.get(path).cloned() {
                    Some(entry) => {
                        entries.insert(path.clone(), entry);
                    }
                    None => {
                        entries.remove(path);
                    }
                }
            }
        }
        let derived_manifest = Manifest::new_with_encoding(
            entries.into_values().collect(),
            self.snapshot.path_encoding,
        )
        .map_err(|error| error.to_string())?;
        let derived_manifest_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_manifest).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let plan = IntegrationPlanV1 {
            subject_id: self.authority.subject_id.clone(),
            base_snapshot_id: self.authority.head_snapshot_id.clone(),
            policy_id: self.authority.pipeline_policy_id.clone(),
            protected_paths: policy.protected_paths.clone(),
            candidates: selected.iter().map(|selected| selected.0.clone()).collect(),
            derived_manifest_artifact_id: derived_manifest_artifact_id.clone(),
        };
        plan.validate()?;
        let plan_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&plan).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let batch_id = format!("integration-{}", &plan_artifact_id[7..23]);
        let prior_snapshot: SourceSnapshot = serde_json::from_value(
            self.cas
                .get_json(&self.authority.head_snapshot_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let derived_snapshot = SourceSnapshot {
            repository_id: prior_snapshot.repository_id,
            vcs: prior_snapshot.vcs,
            capture: SnapshotCapture::Derived {
                tree_id: derived_manifest.content_digest(),
                parent_snapshot_id: self.authority.head_snapshot_id.clone(),
                integration_batch_id: batch_id.clone(),
            },
            content_digest: derived_manifest.content_digest(),
            parent_snapshot_id: Some(self.authority.head_snapshot_id.clone()),
            source_revision: prior_snapshot.source_revision,
            artifact_manifest: Some(derived_manifest_artifact_id.clone()),
            submodules: prior_snapshot.submodules,
        };
        let derived_snapshot_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_snapshot).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationPreparedV1,
                serde_json::to_value(IntegrationPreparedPayloadV1 {
                    batch_id: batch_id.clone(),
                    plan_artifact_id: plan_artifact_id.clone(),
                    derived_snapshot_id: derived_snapshot_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone())
            .referencing(vec![
                plan_artifact_id.clone(),
                derived_snapshot_id.clone(),
                derived_manifest_artifact_id,
            ]),
        )?;

        let checks =
            self.run_integration_checks(policy, &derived_manifest, &derived_snapshot_id)?;
        let passed = checks.passed();
        let result_ids: Vec<String> = checks
            .checks
            .iter()
            .map(|check| check.result_artifact_id.clone())
            .collect();
        let checks_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&checks).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let mut check_refs = vec![checks_artifact_id.clone(), derived_snapshot_id.clone()];
        check_refs.extend(result_ids);
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationChecksCompletedV1,
                serde_json::to_value(IntegrationChecksCompletedPayloadV1 {
                    batch_id: batch_id.clone(),
                    checks_artifact_id: checks_artifact_id.clone(),
                    passed,
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(derived_snapshot_id.clone())
            .referencing(check_refs),
        )?;
        if !passed {
            return Ok(None);
        }

        let (finding_set_id, demand_set_id, semantic_closure_id) =
            current_integration_authority(&events, &self.authority.round_event_id)?;
        let current_subject: SubjectV1 = serde_json::from_value(
            self.cas
                .get_json(&self.authority.subject_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let derived_subject = match self.authority.subject_kind {
            review_core::SubjectKind::WholeTree => SubjectV1::whole_tree(&derived_snapshot_id),
            review_core::SubjectKind::Diff => {
                let base_snapshot_id = current_subject
                    .base_snapshot_id
                    .as_deref()
                    .ok_or("diff Integration has no Campaign Base")?;
                let base_snapshot: SourceSnapshot = serde_json::from_value(
                    self.cas
                        .get_json(base_snapshot_id)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let base_manifest: Manifest = serde_json::from_value(
                    self.cas
                        .get_json(
                            base_snapshot
                                .artifact_manifest
                                .as_deref()
                                .ok_or("Campaign Base has no Manifest")?,
                        )
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let change_set = manifest_diff(&base_manifest, &derived_manifest, self.cas)
                    .map_err(|error| error.to_string())?
                    .change_set(base_snapshot_id, &derived_snapshot_id)?;
                let change_set_id = self
                    .cas
                    .put_json(&serde_json::to_value(change_set).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                SubjectV1::diff(&derived_snapshot_id, base_snapshot_id, change_set_id)
            }
        };
        derived_subject.validate()?;
        let derived_subject_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_subject).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

        let mut by_finding: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> =
            BTreeMap::new();
        for (candidate, _) in &selected {
            for finding in &candidate.finding_ids {
                let grouped = by_finding.entry(finding.clone()).or_default();
                grouped.0.extend(candidate.paths.iter().cloned());
                grouped.1.extend(candidate.evidence_ids.iter().cloned());
            }
        }
        let mut commit_events = Vec::new();
        let mut attestation_ids = Vec::new();
        for (finding_id, (paths, evidence_ids)) in by_finding {
            let attestation = review_core::ChangeAttestationV1 {
                finding_id: finding_id.clone(),
                expected_finding_view_id: ledger
                    .finding_view_id(&finding_id)
                    .ok_or("Integration Finding view disappeared before commit")?,
                subject_id: self.authority.subject_id.clone(),
                change_set_id: self.authority.change_set_id.clone(),
                changed_regions: paths
                    .into_iter()
                    .map(|path| review_core::ChangedRegionV1 {
                        path,
                        start_line: None,
                        end_line: None,
                    })
                    .collect(),
                actor: "review.kernel/automatic-integration@1".into(),
                reason: format!("checked Integration batch {batch_id}"),
                evidence_ids: evidence_ids.into_iter().collect(),
            };
            attestation.validate()?;
            let mut inputs = selected
                .iter()
                .filter(|selected| selected.0.finding_ids.contains(&finding_id))
                .map(|selected| selected.1.clone())
                .collect::<Vec<_>>();
            inputs.extend(attestation.evidence_ids.iter().cloned());
            inputs.sort();
            inputs.dedup();
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::CHANGE_ATTESTATION_V1,
                    Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: None,
                        operation_id: format!("automatic-integration:{batch_id}:{finding_id}"),
                    },
                    inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(attestation).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            attestation_ids.push(record_id.clone());
            commit_events.push(
                NewEvent::new(
                    EventType::ChangeAttestedV1,
                    serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                        artifact_id: record_id.clone(),
                    })
                    .map_err(|error| error.to_string())?,
                )
                .correlating(finding_id)
                .referencing(vec![record_id]),
            );
        }
        let proposal_ids = selected
            .iter()
            .map(|selected| selected.0.proposal_id.clone())
            .collect::<Vec<_>>();
        let committed = IntegrationCommittedPayloadV1 {
            batch_id,
            prior_subject_id: self.authority.subject_id.clone(),
            derived_subject_id: derived_subject_id.clone(),
            prior_snapshot_id: self.authority.head_snapshot_id.clone(),
            derived_snapshot_id: derived_snapshot_id.clone(),
            proposal_ids,
            attestation_ids: attestation_ids.clone(),
            expected_finding_set_id: finding_set_id.clone(),
            expected_demand_set_id: demand_set_id.clone(),
            policy_id: self.authority.pipeline_policy_id.clone(),
            semantic_closure_id: semantic_closure_id.clone(),
        };
        committed.validate()?;
        let mut commit_refs = vec![
            plan_artifact_id,
            checks_artifact_id,
            derived_subject_id.clone(),
            derived_snapshot_id,
            finding_set_id,
            demand_set_id,
            semantic_closure_id,
        ];
        commit_refs.extend(attestation_ids);
        commit_events.push(
            NewEvent::new(
                EventType::IntegrationCommittedV1,
                serde_json::to_value(committed).map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone())
            .referencing(commit_refs),
        );
        self.commit_integration(&commit_events)?;
        Ok(Some(derived_subject_id))
    }

    fn record_integration_conflict(
        &self,
        proposal_ids: &[String],
        paths: &[String],
        reason: &str,
    ) -> Result<(), String> {
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationConflictV1,
                serde_json::to_value(IntegrationConflictPayloadV1 {
                    base_snapshot_id: self.authority.head_snapshot_id.clone(),
                    proposal_ids: proposal_ids.to_vec(),
                    paths: paths.to_vec(),
                    reason: reason.into(),
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone()),
        )
    }

    fn run_integration_checks(
        &self,
        policy: &review_config::IntegrationSpec,
        manifest: &Manifest,
        derived_snapshot_id: &str,
    ) -> Result<IntegrationChecksV1, String> {
        let template = review_sandbox::SandboxTemplate::materialize(manifest, self.cas)
            .map_err(|error| error.to_string())?;
        let binding = self
            .gate_execution
            .as_ref()
            .ok_or("Integration requires the captured Gate Execution Binding")?;
        let container = match binding.provider {
            review_config::SandboxProviderSpec::TrustedLocal => None,
            review_config::SandboxProviderSpec::Container => Some(
                self.container_provider
                    .clone()
                    .unwrap_or_else(ContainerProvider::detect)
                    .with_image(
                        binding
                            .image
                            .as_deref()
                            .ok_or("container Integration binding has no pinned image")?,
                    ),
            ),
        };
        if let Some(provider) = container.as_ref()
            && !provider.availability().usable()
        {
            return Err(format!(
                "container provider unavailable: {}",
                provider.availability().reason()
            ));
        }
        let sandbox = match container.as_ref() {
            Some(provider) => provider
                .sandbox_from_template(&template, Mode::EphemeralWrite)
                .map_err(|error| error.to_string())?,
            None => Sandbox::from_template(&template, Mode::EphemeralWrite)
                .map_err(|error| error.to_string())?,
        };
        let runner = CheckRunner::new(self.cas, sandbox.root()).with_timeout(self.check_timeout);
        let selected: BTreeSet<_> = policy
            .post_apply_checks
            .iter()
            .map(String::as_str)
            .collect();
        let mut checks = Vec::new();
        for definition in self
            .checks
            .iter()
            .filter(|definition| selected.contains(definition.name.as_str()))
        {
            let result = match container.as_ref() {
                Some(provider) => runner.run_with(definition, |program, args, env, timeout| {
                    provider
                        .exec_evidenced(sandbox.root(), program, args, env, timeout)
                        .map(|execution| (execution.output, execution.stderr_held))
                        .map_err(|error| error.to_string())
                }),
                None => runner.run(definition),
            };
            let result_artifact_id = self
                .cas
                .put_json(&serde_json::to_value(&result).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            checks.push(IntegrationCheckV1 {
                name: result.name,
                passed: result.status == CheckStatus::Passed,
                result_artifact_id,
            });
        }
        let checks = IntegrationChecksV1 {
            derived_snapshot_id: derived_snapshot_id.into(),
            checks,
        };
        checks.validate()?;
        Ok(checks)
    }
    /// A sandbox in the requested mode, as a copy-on-write clone of the run's single
    /// materialized template. The template is built once, under the lock, on the first call
    /// (the gate's); every later sandbox — the reviewers' — clones it instead of walking the
    /// manifest and re-reading the whole tree from the CAS.
    fn sandbox_template(&self) -> Result<Arc<review_sandbox::SandboxTemplate>, String> {
        Ok({
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
        })
    }

    fn sandbox(&self, mode: Mode) -> Result<Sandbox, String> {
        let template = self.sandbox_template()?;
        Sandbox::from_template(&template, mode).map_err(|e| e.to_string())
    }

    /// Emit the run's generation state — the campaign's prior findings — as the artifact a
    /// reviewer receives on its `prior_findings` input edge. In the first round there is no
    /// prior state, so an empty finding set is emitted; the edge is satisfied either way, and
    /// nothing about delivery depends on ambient kernel state.
    fn run_generation(&self, node: &Node) -> Result<ArtifactMap, String> {
        let mut outputs = ArtifactMap::new();
        for port in &node.outputs {
            let artifacts = if is_generation_prior_findings_output(port, self.pipeline_version) {
                vec![self.prior_findings.clone().ok_or(
                    "campaign execution has no exact prior Finding Set from RoundStarted@1",
                )?]
            } else if is_generation_finding_set_output(port) {
                if self.authority.prior_reduction_finding_set_id
                    == self.authority.finding_genesis_id
                {
                    Vec::new()
                } else {
                    vec![self.authority.prior_reduction_finding_set_id.clone()]
                }
            } else if is_change_set_port(port, self.pipeline_version) {
                vec![
                    self.authority
                        .change_set_id
                        .clone()
                        .ok_or("generation declares ChangeSet@1 for a whole-tree Subject")?,
                ]
            } else {
                return Err(format!(
                    "generation output `{}` has unsupported artifact type `{}`",
                    port.name, port.artifact_type
                ));
            };
            outputs.insert(port.name.clone(), artifacts);
        }
        Ok(outputs)
    }

    fn record_cache_failure(
        &self,
        node_id: &str,
        kind: CacheKind,
        reason: RunCacheFailureReasonV5,
    ) {
        let identity = (node_id.to_string(), run_cache_kind(kind));
        if self
            .cache_snapshots
            .lock()
            .expect("cache snapshots")
            .contains_key(&identity)
        {
            return;
        }
        let failure = RunCacheFailureV5 {
            node: node_id.to_string(),
            kind: run_cache_kind(kind),
            reason,
        };
        self.cache_failures
            .lock()
            .expect("cache failures")
            .entry(identity)
            .or_insert(failure);
    }

    fn record_unmaterialized_cache_failures(&self, node_id: &str, reason: RunCacheFailureReasonV5) {
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                self.record_cache_failure(node_id, kind, reason);
            }
        }
    }

    fn run_gate(&self, node_id: &str) -> Result<Vec<String>, String> {
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let identity = (node_id.to_string(), run_cache_kind(kind));
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .remove(&identity);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&identity);
            }
        }
        let (sandbox, container, require_unchanged) = match self.gate_execution.as_ref() {
            None => {
                // Frozen pipeline v1/v2 semantics. Those Campaigns captured no Gate Execution
                // Binding, so replay retains the old local/read-only behavior exactly.
                (self.sandbox(Mode::ReadOnly)?, None, true)
            }
            Some(binding) => {
                let mode = match binding.mode {
                    review_config::GateModeSpec::EphemeralWrite => Mode::EphemeralWrite,
                };
                let required = match binding.required_isolation {
                    review_config::IsolationSpec::None => Isolation::None,
                    review_config::IsolationSpec::Container => Isolation::Container,
                };
                let policy = Policy { require: required };
                let container = match binding.provider {
                    review_config::SandboxProviderSpec::TrustedLocal => None,
                    review_config::SandboxProviderSpec::Container => Some(
                        self.container_provider
                            .clone()
                            .unwrap_or_else(ContainerProvider::detect)
                            .with_image(
                                binding
                                    .image
                                    .as_deref()
                                    .expect("validated container Gate image"),
                            ),
                    ),
                };
                let provided = container
                    .as_ref()
                    .map_or(Isolation::None, ContainerProvider::isolation);
                let admitted = gate_provider_admitted(binding.provider, provided, required);
                // Record the resolved provider claim before materialization. A broken CAS or
                // failed clone must still leave RunReport@4 able to explain which binding was
                // selected and whether its isolation was sufficient.
                let report = RunExecutionBindingV4 {
                    node: node_id.to_string(),
                    provider: match binding.provider {
                        review_config::SandboxProviderSpec::TrustedLocal => {
                            RunExecutionProviderV4::TrustedLocal
                        }
                        review_config::SandboxProviderSpec::Container => {
                            RunExecutionProviderV4::Container
                        }
                    },
                    image: binding.image.clone(),
                    required_isolation: run_isolation(required),
                    provided_isolation: run_isolation(provided),
                    mode: RunSandboxModeV4::EphemeralWrite,
                    admitted,
                };
                self.execution_bindings
                    .lock()
                    .expect("execution bindings")
                    .insert(node_id.to_string(), report.clone());
                self.buffer_reviewer_event(
                    node_id,
                    NewEvent::new(
                        EventType::GateExecutionBoundV1,
                        serde_json::to_value(report).map_err(|error| error.to_string())?,
                    )
                    .node(node_id),
                );
                if let Some(provider) = container.as_ref() {
                    if !provider.availability().usable() {
                        return Err(format!(
                            "container provider unavailable: {}",
                            provider.availability().reason()
                        ));
                    }
                }
                let template = self.sandbox_template()?;
                let sandbox = match container.as_ref() {
                    Some(provider) => provider
                        .sandbox_from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                    None => Sandbox::from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                };
                admit(policy, &sandbox).map_err(|error| error.to_string())?;
                (sandbox, container, false)
            }
        };
        let mut cache_receipt_artifacts = Vec::new();
        let mut cache_environments = Vec::new();
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let resolved = if let Some(source) = self.cache_sources.get(&kind).cloned() {
                    Ok(source)
                } else if let Some(resolver) = self.cache_source_resolver.as_ref() {
                    resolver(kind)
                } else {
                    Err(CacheError::new(
                        CacheErrorKind::PolicyUnavailable,
                        format!(
                            "Gate requested `{}` cache without an available machine-local mapping",
                            kind.name()
                        ),
                    ))
                };
                let source = match resolved {
                    Ok(source) if source.kind == kind => source,
                    Ok(source) => {
                        let error = CacheError::new(
                            CacheErrorKind::PolicyUnavailable,
                            format!(
                                "machine-local cache mapping for `{}` resolved as `{}`",
                                kind.name(),
                                source.kind.name()
                            ),
                        );
                        self.record_cache_failure(
                            node_id,
                            kind,
                            RunCacheFailureReasonV5::PolicyUnavailable,
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let snapshot = match materialize_cache(&source, &sandbox, self.cas) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let receipt = RunCacheSnapshotV5 {
                    node: node_id.to_string(),
                    kind: run_cache_kind(snapshot.kind),
                    source_digest: snapshot.source_digest,
                    bytes: snapshot.bytes,
                    files: snapshot.files,
                    materialization: run_cache_materialization(snapshot.materialization),
                };
                let receipt_artifact = self
                    .cas
                    .put_json(&serde_json::to_value(&receipt).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::CacheSnapshotMaterializedV1,
                        serde_json::to_value(&receipt).map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .referencing(vec![
                        receipt.source_digest.clone(),
                        receipt_artifact.clone(),
                    ]),
                )?;
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .insert((node_id.to_string(), receipt.kind), receipt);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&(node_id.to_string(), run_cache_kind(kind)));
                cache_receipt_artifacts.push(receipt_artifact);
                cache_environments.push(kind.environment(sandbox.root()));
            }
        }
        // Run the checks holding no lock: each is a build or a test, and the store lock is
        // shared with every other node, so holding it across a check would stall the whole
        // pipeline for the build's duration. The lock is taken only to append each result.
        let mut runner =
            CheckRunner::new(self.cas, sandbox.root()).with_timeout(self.check_timeout);
        for environment in cache_environments {
            for ((local_key, local_value), (container_key, container_value)) in
                environment.local.into_iter().zip(environment.container)
            {
                if local_key != container_key {
                    return Err("cache environment keys disagree across providers".into());
                }
                runner = runner.with_split_env(local_key, local_value, container_value);
            }
        }
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let mut cleanup_failure = None;
            let result = match container.as_ref() {
                Some(provider) => runner.run_with(check, |program, args, env, timeout| {
                    match provider.exec_evidenced(sandbox.root(), program, args, env, timeout) {
                        Ok(execution) => Ok((execution.output, execution.stderr_held)),
                        Err(error) => {
                            if !error.cleanup_confirmed() {
                                cleanup_failure = Some(error.to_string());
                            }
                            Err(error.to_string())
                        }
                    }
                }),
                None => runner.run(check),
            };
            self.buffer_reviewer_event(node_id, check_event(&result, node_id));
            results.push(result);
            if let Some(error) = cleanup_failure {
                // A container may still own the writable bind. Do not scan a concurrently
                // changing tree or delete it from under that process. Preserving this temporary
                // sandbox is the fail-closed forensic residue for an operator to recover.
                let preserved = sandbox.root().to_path_buf();
                std::mem::forget(sandbox);
                return Err(format!(
                    "container cleanup was not confirmed; Gate sandbox preserved at {}: {error}",
                    preserved.display()
                ));
            }
        }

        let decision = GateDecision::evaluate(&results);
        if !cache_receipt_artifacts.is_empty() {
            remove_materialized_caches(&sandbox)?;
        }
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if require_unchanged && !sealed.unchanged() {
            // Frozen v1/v2 behavior: those Gates promised read-only execution. V3 deliberately
            // permits writes in this one disposable clone; reviewer clones still start from the
            // pristine template, so Gate mutations cannot become Subject content.
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
        let mut event_artifacts = vec![artifact.clone()];
        event_artifacts.extend(cache_receipt_artifacts);
        if !require_unchanged {
            // V3 Gates may write only inside their disposable clone. Preserve what they wrote
            // before that clone disappears: the complete mutation set lives once in the CAS,
            // while the Gate event references the same bounded summary shape as Worker
            // provenance. The decision artifact remains the Gate's sole graph output.
            let mutations_artifact = self
                .cas
                .put_json(&serde_json::json!({
                    "added": sealed.mutations.added,
                    "modified": sealed.mutations.modified,
                    "deleted": sealed.mutations.deleted,
                }))
                .map_err(|error| error.to_string())?;
            let mutation_summary = mutation_summary(&sealed.mutations, &mutations_artifact);
            let mutation_summary_artifact = self
                .cas
                .put_json(&mutation_summary)
                .map_err(|error| error.to_string())?;
            event_artifacts.push(mutation_summary_artifact);
        }
        self.buffer_reviewer_event(
            node_id,
            NewEvent::new(
                EventType::GateDecisionV1,
                serde_json::to_value(&decision).map_err(|e| e.to_string())?,
            )
            .node(node_id)
            .referencing(event_artifacts),
        );
        self.gates
            .lock()
            .expect("gates")
            .insert(node_id.to_string(), decision);
        Ok(vec![artifact])
    }

    #[allow(clippy::too_many_arguments)] // one exact Attempt boundary; grouping would obscure authority inputs
    fn prepare_proposal(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        result_artifact: &str,
        declaration: Result<Option<ReviewerProposalDeclaration>, String>,
        assigned_finding_ids: &[String],
        report_count: usize,
        sealed: &review_sandbox::SealedSandbox,
    ) -> Result<PreparedProposal, String> {
        let refused = |reason| {
            PreparedProposal::Refused(
                NewEvent::new(
                    EventType::ProposalRefusedV1,
                    serde_json::to_value(ProposalRefusedPayloadV1 {
                        reason,
                        result_artifact_id: result_artifact.to_string(),
                    })
                    .expect("typed Proposal refusal serializes"),
                )
                .node(node_id)
                .attempt(attempt.to_string())
                .referencing(vec![result_artifact.to_string()]),
            )
        };
        let declaration = match declaration {
            Ok(Some(declaration)) => declaration,
            Ok(None) => return Ok(PreparedProposal::None),
            Err(_) => return Ok(refused(ProposalRefusalReasonV1::MalformedDeclaration)),
        };
        if self.authority.finding_identity_policy != review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
        }
        if declaration.patch.is_empty() || declaration.patch.len() > MAX_CHANGE_SET_BYTES {
            return Ok(refused(ProposalRefusalReasonV1::MalformedDeclaration));
        }
        let mut report_indexes = declaration.report_indexes;
        report_indexes.sort_unstable();
        if report_indexes.windows(2).any(|pair| pair[0] == pair[1])
            || report_indexes
                .iter()
                .any(|index| usize::try_from(*index).map_or(true, |index| index >= report_count))
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
        }
        let mut finding_ids = declaration.finding_ids;
        finding_ids.sort();
        if finding_ids.windows(2).any(|pair| pair[0] == pair[1])
            || finding_ids
                .iter()
                .any(|id| !assigned_finding_ids.contains(id))
            || report_indexes.is_empty() && finding_ids.is_empty()
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
        }
        let mut evidence_ids = declaration.evidence_ids;
        evidence_ids.sort();
        if evidence_ids.windows(2).any(|pair| pair[0] == pair[1])
            || evidence_ids.iter().any(|id| self.cas.verify(id).is_err())
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidEvidence));
        }
        let mut paths = declaration.paths;
        paths.sort();
        if paths.is_empty()
            || paths.windows(2).any(|pair| pair[0] == pair[1])
            || paths
                .iter()
                .any(|path| !review_core::is_valid_repo_path(path))
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidPath));
        }
        if sealed.mutations.is_empty() {
            return Ok(refused(ProposalRefusalReasonV1::EmptyMutation));
        }
        if paths != sealed.mutations.paths() {
            return Ok(refused(ProposalRefusalReasonV1::PathMismatch));
        }
        let final_manifest = sealed
            .capture_snapshot(self.cas)
            .map_err(|error| error.to_string())?;
        let diff = manifest_diff(sealed.baseline.as_ref(), &final_manifest, self.cas)
            .map_err(|error| error.to_string())?;
        if declaration.patch.as_bytes() != diff.patch() {
            return Ok(refused(ProposalRefusalReasonV1::PatchMismatch));
        }
        let patch_artifact_id = self
            .cas
            .put(diff.patch())
            .map_err(|error| error.to_string())?;
        let derived_manifest_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&final_manifest).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let candidate = ProposalCandidateV1 {
            base_snapshot_id: self.authority.head_snapshot_id.clone(),
            patch_artifact_id: patch_artifact_id.clone(),
            derived_manifest_artifact_id: derived_manifest_artifact_id.clone(),
            result_artifact_id: result_artifact.to_string(),
            report_indexes,
            finding_ids,
            evidence_ids: evidence_ids.clone(),
            paths,
            description: declaration.description,
            auto_apply_nominated: declaration.auto_apply_nominated,
        };
        candidate.validate().map_err(str::to_string)?;
        let candidate_artifact = self
            .cas
            .put_json(&serde_json::to_value(candidate).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let mut artifacts = vec![
            candidate_artifact.clone(),
            result_artifact.to_string(),
            patch_artifact_id,
            derived_manifest_artifact_id,
        ];
        artifacts.extend(evidence_ids);
        Ok(PreparedProposal::Prepared {
            candidate_artifact: candidate_artifact.clone(),
            event: NewEvent::new(
                EventType::ProposalPreparedV1,
                serde_json::to_value(ProposalPreparedPayloadV1 {
                    candidate_artifact_id: candidate_artifact,
                    result_artifact_id: result_artifact.to_string(),
                })
                .map_err(|error| error.to_string())?,
            )
            .node(node_id)
            .attempt(attempt.to_string())
            .referencing(artifacts),
        })
    }

    fn run_reviewer(&self, node: &Node, node_inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        let node_id = node.id.as_str();
        let binding_node = self.reviewer_binding_node(node_id);
        let mut prepared = self
            .prepared_attempts
            .lock()
            .expect("prepared attempts")
            .remove(node_id);
        let adapter = match self.reviewers.get(&binding_node) {
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
        let result_contract = match reviewer_result_contract(node) {
            Ok(contract) => contract,
            Err(error) => {
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
        let prior_findings_contract = node
            .inputs
            .iter()
            .find(|port| is_reviewer_prior_set_input(port, self.pipeline_version));
        let exact_finding_set = prior_findings_contract.is_some_and(is_reviewer_finding_set_input);
        if (result_contract == ReviewerResultContract::V2) != exact_finding_set {
            let error = format!(
                "reviewer `{node_id}` must pair ReviewerResult@2 with an exact FindingSet@1 input"
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
        let prior_findings_port = prior_findings_contract.map(|port| port.name.as_str());
        let prior_findings_artifact = prior_findings_port
            .and_then(|port| node_inputs.get(port))
            .and_then(|artifacts| artifacts.first())
            .cloned();
        let mut inputs = ReviewerInputs {
            result_contract,
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
                if is_reviewer_prior_set_input(contract, self.pipeline_version) {
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
            let value = if exact_finding_set {
                let envelope: review_core::ArtifactEnvelope = match serde_json::from_value(value) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        let error = format!(
                            "exact prior FindingSet@1 `{artifact}` is not an envelope: {error}"
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
                };
                if let Err(error) = review_store::validate_envelope(&envelope) {
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
                if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                    let error = format!("exact prior artifact `{artifact}` is not FindingSet@1");
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
                let mut set: review_core::FindingSetV1 =
                    match serde_json::from_value(envelope.payload) {
                        Ok(set) => set,
                        Err(error) => {
                            let error = format!("exact prior FindingSet@1 is invalid: {error}");
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
                if let Err(error) = set.validate() {
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
                let round_assignment = match self.cas.get_json(&self.authority.prior_finding_set_id)
                {
                    Ok(assignment) => assignment,
                    Err(error) => {
                        let error =
                            format!("exact Round finding assignment is unreadable: {error}");
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
                let assignment_node = self.reviewer_binding_node(node_id);
                if let Err(error) =
                    retain_round_assignment(&mut set, &round_assignment, &assignment_node)
                {
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
                serde_json::to_value(set).expect("validated FindingSet@1 serializes")
            } else {
                value
            };
            // An empty assignment needs no prompt section and requires an empty disposition list.
            let findings_field = if exact_finding_set {
                "findings"
            } else {
                "prior_findings"
            };
            let has_findings = value
                .get(findings_field)
                .and_then(|findings| findings.as_array())
                .is_some_and(|findings| !findings.is_empty());
            if has_findings {
                inputs.prior_findings = Some(value);
            }
        }
        inputs.prior_findings_artifact_id = prior_findings_artifact.clone();

        let mut retry_failures: Vec<String> = Vec::new();
        let broker_fence_authority = self
            .reviewer_execution
            .get(&binding_node)
            .filter(|execution| {
                execution.credential_mode == review_core::BrokerCredentialModeV1::Brokered
            })
            .map(|execution| review_core::broker_authority_usage(&execution.operations))
            .transpose()?
            .unwrap_or(0);
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
                    .get(&binding_node)
                    .map(|(artifact_id, _)| artifact_id.clone()),
                reviewer_package_digest: self
                    .authority
                    .reviewer_packages
                    .get(&binding_node)
                    .map(|(_, digest)| digest.clone()),
                policy_ids: self.authority.policy_ids.clone(),
                reserved_tokens: reservation.as_ref().map(|reservation| reservation.amount),
            });

            let boundary = KernelBrokerBoundary { kernel: self };
            let brokered = (|| -> Result<Option<Broker<'_>>, String> {
                let Some(execution) = self.reviewer_execution.get(&binding_node) else {
                    return Ok(None);
                };
                let lease_epoch = self
                    .attempts
                    .lock()
                    .expect("attempt ledger")
                    .attempt(&attempt)
                    .and_then(|attempt| attempt.epoch.checked_add(1))
                    .ok_or_else(|| "reviewer Attempt has no broker lease epoch".to_string())?;
                let mut broker = None;
                let broker_handle = if execution.credential_mode == BrokerCredentialModeV1::Brokered
                {
                    let provider = self.broker_providers.get(&binding_node).ok_or_else(|| {
                        format!("brokered reviewer `{node_id}` has no machine-local provider")
                    })?;
                    let issued = Broker::issue(
                        BrokerLeaseV1 {
                            campaign_id: self.run_id.clone(),
                            round_event_id: self.authority.round_event_id.clone(),
                            node_id: node_id.to_string(),
                            attempt_id: attempt.to_string(),
                            lease_epoch,
                        },
                        execution.operations.clone(),
                        Credential::new(provider.credential.clone())
                            .map_err(|error| error.to_string())?,
                        &boundary,
                        provider.connector.as_ref(),
                        &boundary,
                    )
                    .map_err(|error| error.to_string())?;
                    let handle = issued.handle().as_str().to_string();
                    broker = Some(issued);
                    Some(handle)
                } else {
                    None
                };
                let binding = ReviewerExecutionBindingV1 {
                    node: node_id.to_string(),
                    attempt_id: attempt.to_string(),
                    lease_epoch,
                    credential_mode: execution.credential_mode,
                    auto_apply: execution.auto_apply,
                    broker_handle,
                    operations: execution.operations.clone(),
                    admitted: true,
                };
                self.append(
                    NewEvent::new(
                        EventType::ReviewerExecutionBoundV1,
                        serde_json::to_value(binding).map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .attempt(attempt.to_string()),
                )?;
                Ok(broker)
            })();
            let broker = match brokered {
                Ok(broker) => broker,
                Err(error) => {
                    self.release_prepared_attempt(node_id, &attempt, reservation.as_ref(), &error)?;
                    return Err(error);
                }
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

            let wall_started = SystemTime::now();
            let wall_clock = Instant::now();
            let invoked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                adapter.invoke_with_broker(
                    self.cas,
                    sandbox.root(),
                    &inputs,
                    broker.as_ref().map(|broker| broker as &dyn BrokerClient),
                )
            }));
            let broker_charged = broker.as_ref().map_or(0, Broker::charged_usage);
            let invoked = match invoked {
                Ok(invoked) => invoked,
                Err(_) => {
                    let error = format!("reviewer adapter panicked for node {node_id}");
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
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

            // Wall-clock and provider usage live beside the event stream, never in it: identity,
            // replay, the Ledger, and convergence ignore them; people read them through
            // `af review report` and `af review campaigns`.
            self.record_attempt_wall(
                node_id,
                &attempt,
                wall_started,
                wall_clock.elapsed(),
                invoked.as_ref().ok().map(|receipted| &receipted.usage),
            );

            match invoked {
                Ok(receipted) => {
                    let reported_charge = receipted
                        .returned
                        .cost_tokens
                        .max(receipted.usage.chargeable_tokens);
                    if broker.is_some()
                        && (receipted.returned.cost_tokens != broker_charged
                            || receipted.usage.chargeable_tokens != broker_charged)
                    {
                        let error = format!(
                            "brokered reviewer usage mismatch: Broker charged {broker_charged}, adapter reported cost_tokens={} and chargeable_tokens={}",
                            receipted.returned.cost_tokens, receipted.usage.chargeable_tokens
                        );
                        self.fail_started_attempt(
                            node_id,
                            &attempt,
                            reservation.as_ref(),
                            &error,
                            broker_charged.max(reported_charge),
                            AttemptFailureEvidence {
                                raw_artifact: Some(&receipted.returned.raw_artifact),
                                refusal_history: None,
                            },
                        )?;
                        return Err(error);
                    }
                    let returned = receipted.returned;
                    let proposal_declaration = returned.proposal;
                    let assigned_finding_ids = inputs
                        .prior_findings
                        .as_ref()
                        .and_then(|value| value.get("findings"))
                        .and_then(serde_json::Value::as_array)
                        .map(|findings| {
                            findings
                                .iter()
                                .filter_map(|finding| finding.get("finding_id"))
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let result_value = match reviewer_result_value(
                        &returned.output,
                        result_contract,
                        &assigned_finding_ids,
                    ) {
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
                                returned.cost_tokens.max(broker_charged),
                                AttemptFailureEvidence {
                                    raw_artifact: Some(&returned.raw_artifact),
                                    refusal_history: Some(&retry_failures),
                                },
                            )?;
                            continue;
                        }
                    };
                    let artifacts = (|| -> Result<(String, String, PreparedProposal), String> {
                        let sealed = sandbox.seal().map_err(|error| error.to_string())?;
                        let result_artifact = self
                            .cas
                            .put_json(&result_value)
                            .map_err(|error| error.to_string())?;
                        let proposal = self.prepare_proposal(
                            node_id,
                            &attempt,
                            &result_artifact,
                            proposal_declaration,
                            &assigned_finding_ids,
                            returned.output.findings.len(),
                            &sealed,
                        )?;
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
                        Ok((result_artifact, provenance_artifact, proposal))
                    })();
                    let (result_artifact, provenance_artifact, proposal) = match artifacts {
                        Ok(artifacts) => artifacts,
                        Err(error) => {
                            self.fail_started_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &error,
                                returned.cost_tokens.max(broker_charged),
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
                                proposal_candidate: match &proposal {
                                    PreparedProposal::Prepared {
                                        candidate_artifact, ..
                                    } => Some(candidate_artifact.clone()),
                                    PreparedProposal::None | PreparedProposal::Refused(_) => None,
                                },
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("reviewer {node_id} selected more than one attempt"));
                    }
                    self.buffer_reviewer_event(node_id, admitted);
                    match proposal {
                        PreparedProposal::None => {}
                        PreparedProposal::Prepared { event, .. }
                        | PreparedProposal::Refused(event) => {
                            self.buffer_reviewer_event(node_id, event)
                        }
                    }
                    return Ok(vec![result_artifact]);
                }
                Err(RunnerError::MalformedOutput { raw_artifact, why }) => {
                    let error = format!(
                        "reviewer output is not a {}: {why}",
                        result_contract.artifact_type()
                    );
                    retry_failures.push(failed_retry_context(
                        &attempt.to_string(),
                        "parse_error",
                        None,
                    ));
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
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
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged)
                        .max(broker_fence_authority);
                    self.attempts.lock().expect("attempt ledger").fence(node_id);
                    self.attempts
                        .lock()
                        .expect("attempt ledger")
                        .charge(&attempt, charged);
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, charged);
                    }
                    let reason = format!("timed out after {after_ms}ms");
                    retry_failures.push(fenced_retry_context(&attempt.to_string(), &reason));
                    let fenced = NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::json!({
                            "reason": reason,
                            "charged": reservation
                                .as_ref()
                                .map(|_| charged)
                                .or((broker_charged > 0).then_some(charged)),
                        }),
                    )
                    .node(node_id)
                    .attempt(attempt.to_string())
                    .referencing(raw_artifact.into_iter().collect());
                    let feedback = self.feedback_event(node_id, &attempt, &retry_failures)?;
                    self.append_batch(&[fenced, feedback])?;
                }
                Err(error @ (RunnerError::Refused(_) | RunnerError::Unavailable(_))) => {
                    if broker_charged > 0 {
                        let detail = error.to_string();
                        self.fail_started_attempt(
                            node_id,
                            &attempt,
                            reservation.as_ref(),
                            &detail,
                            broker_charged,
                            AttemptFailureEvidence::default(),
                        )?;
                        return Err(detail);
                    }
                    // No Broker operation or model execution spent anything, so release rather
                    // than turning a structural refusal into a charge.
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
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, charged);
                    }
                    self.attempts
                        .lock()
                        .expect("attempt ledger")
                        .charge(&attempt, charged);
                    self.append(
                        NewEvent::new(
                            EventType::AttemptFailedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "charged": reservation
                                    .as_ref()
                                    .map(|_| charged)
                                    .or((broker_charged > 0).then_some(charged)),
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

    fn run_slicer(&self, node: &Node) -> Result<Vec<String>, String> {
        let policy = self
            .slicing
            .get(&node.id)
            .ok_or_else(|| format!("Slicer `{}` has no captured slicing policy", node.id))?;
        let subject_paths = match &self.authority.change_set {
            Some(change_set) => change_set.change_set().changed_paths.clone(),
            None => self
                .snapshot
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        let slice_set = policy.plan(
            &self.authority.subject_id,
            &subject_paths,
            &self.static_node_ids,
        )?;
        let operation_id = review_store::content_id(&serde_json::json!({
            "operation": "slice-set@1",
            "node": node.id,
            "subject": self.authority.subject_id,
            "policy": {
                "scatter": policy.scatter_node,
                "max_paths_per_slice": policy.max_paths_per_slice,
                "max_fanout": policy.max_fanout,
                "coverage": policy.coverage,
                "all_shards_required": policy.all_shards_required,
                "closeout": policy.closeout,
            }
        }))
        .map_err(|error| error.to_string())?;
        let mut inputs = vec![self.authority.subject_id.clone()];
        inputs.extend(self.authority.change_set_id.clone());
        let (record_id, envelope) = self
            .cas
            .put_artifact(
                review_core::contract::SLICE_SET_V1,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some(node.id.clone()),
                    operation_id,
                },
                inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(slice_set).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        // This event is deliberately appended inside the Slicer, before its graph receipt can
        // make the artifact visible to Scatter.
        self.append(
            NewEvent::new(
                EventType::SliceSetAcceptedV1,
                serde_json::to_value(SliceSetAcceptedPayloadV1 {
                    slice_set_id: envelope.artifact_id,
                    slice_set_artifact_id: record_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .node(&node.id)
            .referencing(vec![record_id.clone()]),
        )?;
        Ok(vec![record_id])
    }

    fn run_scatter(&self, node: &Node, inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        let slice_port = node
            .inputs
            .iter()
            .find(|port| port.artifact_type == review_core::contract::SLICE_SET_V1)
            .ok_or_else(|| format!("Scatter `{}` has no SliceSet@1 input", node.id))?;
        let [slice_set_record] = inputs
            .get(&slice_port.name)
            .map(Vec::as_slice)
            .unwrap_or_default()
        else {
            return Err(format!(
                "Scatter `{}` did not receive exactly one SliceSet@1",
                node.id
            ));
        };
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            self.cas
                .get_json(slice_set_record)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("SliceSet@1 envelope is malformed: {error}"))?;
        review_store::validate_envelope(&envelope)?;
        if envelope.artifact_type != review_core::contract::SLICE_SET_V1
            || envelope.subject_snapshot_id.as_deref()
                != Some(self.authority.head_snapshot_id.as_str())
        {
            return Err("Scatter received a SliceSet outside current Subject authority".into());
        }
        let slice_set: SliceSetV1 = serde_json::from_value(envelope.payload)
            .map_err(|error| format!("SliceSet@1 payload is malformed: {error}"))?;
        let subject_paths = match &self.authority.change_set {
            Some(change_set) => change_set.change_set().changed_paths.clone(),
            None => self
                .snapshot
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        slice_set.validate_coverage(&subject_paths)?;
        if !self
            .slicing
            .values()
            .any(|policy| policy.scatter_node == node.id)
        {
            return Err(format!(
                "Scatter `{}` has no accepted Slicer owner",
                node.id
            ));
        }

        let mut inherited_contracts = node
            .inputs
            .iter()
            .filter(|contract| contract.artifact_type != review_core::contract::SLICE_SET_V1)
            .cloned()
            .collect::<Vec<_>>();
        if inherited_contracts
            .iter()
            .any(|contract| contract.name == "slice")
        {
            return Err(format!(
                "Scatter `{}` reserves dynamic input port `slice`",
                node.id
            ));
        }
        inherited_contracts.push(PortContract::new(
            "slice",
            review_core::contract::REVIEW_SLICE_V1,
        ));
        let result_contract = if inherited_contracts
            .iter()
            .any(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
        {
            review_core::contract::REVIEWER_RESULT_V2
        } else {
            review_core::contract::REVIEWER_RESULT_V1
        };
        let inherited_inputs = inputs
            .iter()
            .filter(|(port, _)| *port != &slice_port.name)
            .map(|(port, artifacts)| (port.clone(), artifacts.clone()))
            .collect::<ArtifactMap>();

        let mut runnable = Vec::new();
        let mut outcomes: BTreeMap<String, ShardOutcomeV1> = BTreeMap::new();
        for slice in &slice_set.slices {
            self.dynamic_reviewer_bases
                .lock()
                .expect("dynamic reviewer bases")
                .insert(slice.runtime_node_id.clone(), node.id.clone());
            let (slice_record, _) = self
                .cas
                .put_artifact(
                    review_core::contract::REVIEW_SLICE_V1,
                    Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some(node.id.clone()),
                        operation_id: format!("slice:{}", slice.slice_id),
                    },
                    vec![slice_set_record.clone()],
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(slice).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            let dynamic_node = Node::new(&slice.runtime_node_id, NodeKind::Reviewer)
                .accepting_contracts(inherited_contracts.clone())
                .emitting_contracts(vec![PortContract::new("out", result_contract)]);
            let mut dynamic_inputs = inherited_inputs.clone();
            dynamic_inputs.insert("slice".into(), vec![slice_record]);
            // Invocation and reservation both up front, in canonical Slice order: a shard
            // the fan-out cap refuses is a durable Missing outcome before any shard runs.
            match self
                .record_invocation(&dynamic_node, &dynamic_inputs)
                .and_then(|()| self.prepare_dispatch(&dynamic_node, &dynamic_inputs))
            {
                Ok(()) => runnable.push((slice.clone(), dynamic_node, dynamic_inputs)),
                Err(error) => {
                    outcomes.insert(
                        slice.slice_id.clone(),
                        ShardOutcomeV1::Missing { reason: error },
                    );
                }
            }
        }

        // Dispatches above are durable in canonical Slice order. Shards are model calls that
        // block for minutes, so they run on their own bounded thread set — never on the shared
        // filesystem executor, whose width is the host's CPU count and whose workers the shards'
        // own sandbox clone, seal, and CAS phases need. The in-flight bound is the Slice
        // policy's `max_fanout`: the persisted SliceSet@1 carries it and validates
        // `slices.len() <= max_fanout`, so the fan-out the plan declares is the fan-out that
        // runs. Receipts are committed below in the same canonical order.
        let executed = dispatch_shards(
            runnable,
            slice_set.max_fanout,
            |(slice, dynamic_node, inputs)| {
                (
                    slice,
                    dynamic_node.clone(),
                    self.run(&dynamic_node, &inputs),
                )
            },
        );
        for (slice, dynamic_node, result) in executed {
            let outcome = match result {
                Ok(outputs) => match self.record_outputs(&dynamic_node, &outputs) {
                    Ok(()) => ShardOutcomeV1::Completed {
                        result_artifact_ids: artifact_ids(&outputs),
                    },
                    Err(error) => ShardOutcomeV1::Failed { reason: error },
                },
                Err(error) => ShardOutcomeV1::Failed { reason: error },
            };
            outcomes.insert(slice.slice_id, outcome);
        }
        let shard_set = ShardSetV1 {
            subject_id: slice_set.subject_id.clone(),
            slice_set_id: slice_set_record.clone(),
            all_shards_required: slice_set.all_shards_required,
            shards: slice_set
                .slices
                .iter()
                .map(|slice| ShardReceiptV1 {
                    slice_id: slice.slice_id.clone(),
                    runtime_node_id: slice.runtime_node_id.clone(),
                    outcome: outcomes.remove(&slice.slice_id).unwrap_or_else(|| {
                        ShardOutcomeV1::Missing {
                            reason: "dynamic shard produced no terminal outcome".into(),
                        }
                    }),
                })
                .collect(),
        };
        shard_set.validate_against(&slice_set)?;
        let mut artifact_inputs = vec![slice_set_record.clone()];
        artifact_inputs.extend(
            shard_set
                .shards
                .iter()
                .flat_map(|shard| match &shard.outcome {
                    ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } => result_artifact_ids.clone(),
                    ShardOutcomeV1::Failed { .. } | ShardOutcomeV1::Missing { .. } => vec![],
                }),
        );
        let operation_id = review_store::content_id(
            &serde_json::to_value(&shard_set).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let (record_id, _) = self
            .cas
            .put_artifact(
                review_core::contract::SHARD_SET_V1,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some(node.id.clone()),
                    operation_id,
                },
                artifact_inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(shard_set).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        self.append(
            NewEvent::new(
                EventType::ShardSetRecordedV1,
                serde_json::to_value(RecordedSetPayloadV1 {
                    artifact_id: record_id.clone(),
                    record_id: record_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .node(&node.id)
            .referencing(vec![record_id.clone(), slice_set_record.clone()]),
        )?;
        Ok(vec![record_id])
    }

    /// A real gather: one artifact holding exactly the report artifacts the edges delivered.
    /// A reviewer whose result port feeds no edge is absent here, and therefore absent from
    /// everything downstream — the plan is the data flow, not a suggestion about it.
    ///
    /// This is also the run's canonical barrier: every reviewer has finished, so the buffered
    /// reviewer events are flushed here in node order, giving the log a shape that is a
    /// function of the pipeline rather than of thread timing.
    fn run_gather(&self, node: &Node, inputs: &ArtifactMap) -> Result<Vec<String>, String> {
        self.flush_reviewer_events()?;
        if self.authority.finding_identity_policy == review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            let sources = self.input_bindings.get(&node.id).ok_or_else(|| {
                format!("canonical gather `{}` has no pinned input graph", node.id)
            })?;
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let node_outputs = self.node_outputs.lock().expect("node outputs");
            let mut manifest: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (port, artifacts) in inputs {
                let upstream = sources.get(port).map(Vec::as_slice).unwrap_or(&[]);
                let mut remaining = artifacts.clone();
                for (source, source_port, kind) in upstream {
                    let produced = node_outputs
                        .get(source)
                        .and_then(|outputs| outputs.get(source_port))
                        .ok_or_else(|| {
                            format!(
                                "canonical gather `{}.{port}` has no durable output for pinned source `{source}.{source_port}`",
                                node.id
                            )
                        })?;
                    if *kind == NodeKind::Reviewer {
                        let selected = selections.get(source).ok_or_else(|| {
                            format!(
                                "canonical gather `{}.{port}` received from `{source}` without a selected reviewer Attempt",
                                node.id
                            )
                        })?;
                        if produced.as_slice() != [selected.result_artifact.as_str()] {
                            return Err(format!(
                                "canonical gather `{}.{port}` input from `{source}` disagrees with its selected reviewer Attempt",
                                node.id
                            ));
                        }
                    }
                    for artifact in produced {
                        let Some(index) =
                            remaining.iter().position(|delivered| delivered == artifact)
                        else {
                            return Err(format!(
                                "canonical gather `{}.{port}` input from `{source}.{source_port}` disagrees with its durable output",
                                node.id
                            ));
                        };
                        manifest
                            .entry(source.clone())
                            .or_default()
                            .push(remaining.remove(index));
                    }
                }
                if !remaining.is_empty() {
                    return Err(format!(
                        "canonical gather `{}.{port}` has {} artifacts without pinned upstream provenance",
                        node.id,
                        remaining.len()
                    ));
                }
            }
            let artifact = self
                .cas
                .put_json(&serde_json::to_value(manifest).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            return Ok(vec![artifact]);
        }
        let artifact = self
            .cas
            .put_json(&serde_json::json!(inputs))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }

    fn finalize_proposals(
        &self,
        reduction: &review_store::CanonicalReduction,
    ) -> Result<Vec<String>, String> {
        let existing = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                event.event_type == EventType::ProposalAcceptedV1
                    && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
            })
            .map(|event| {
                let payload: ProposalAcceptedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                Ok((payload.candidate_artifact_id.clone(), payload))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let selections = self
            .reviewer_selections
            .lock()
            .expect("reviewer selections")
            .clone();
        let mut proposal_ids = Vec::new();
        let mut events = Vec::new();
        for (node, selection) in selections {
            let Some(candidate_artifact) = selection.proposal_candidate else {
                continue;
            };
            if let Some(accepted) = existing.get(&candidate_artifact) {
                self.cas
                    .verify(&accepted.proposal_artifact_id)
                    .map_err(|error| error.to_string())?;
                proposal_ids.push(accepted.proposal_id.clone());
                continue;
            }
            let candidate: ProposalCandidateV1 = serde_json::from_value(
                self.cas
                    .get_json(&candidate_artifact)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            candidate.validate().map_err(str::to_string)?;
            if candidate.result_artifact_id != selection.result_artifact
                || candidate.base_snapshot_id != self.authority.head_snapshot_id
            {
                return Err(format!(
                    "prepared Proposal for `{node}` contradicts selected Round authority"
                ));
            }
            let source_reports = reduction
                .report_ids_by_source
                .get(&node)
                .ok_or_else(|| format!("prepared Proposal source `{node}` reached no Reports"))?;
            let mut finding_refs = candidate
                .finding_ids
                .iter()
                .map(|id| review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Finding,
                    id: id.clone(),
                })
                .collect::<Vec<_>>();
            for index in &candidate.report_indexes {
                let report_id = source_reports
                    .get(*index as usize)
                    .ok_or_else(|| {
                        format!("prepared Proposal for `{node}` names absent Report {index}")
                    })?
                    .clone();
                finding_refs.push(review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Report,
                    id: report_id,
                });
            }
            let proposal = review_core::PatchProposal {
                base_snapshot_id: candidate.base_snapshot_id.clone(),
                patch_artifact_id: candidate.patch_artifact_id.clone(),
                finding_refs,
                evidence_ids: candidate.evidence_ids.clone(),
                paths: candidate.paths.clone(),
                description: candidate.description.clone(),
                auto_apply_nominated: candidate.auto_apply_nominated,
            };
            proposal.check_shape().map_err(str::to_string)?;
            let mut inputs = vec![
                candidate_artifact.clone(),
                candidate.result_artifact_id.clone(),
                candidate.patch_artifact_id.clone(),
                candidate.derived_manifest_artifact_id.clone(),
            ];
            inputs.extend(candidate.evidence_ids.iter().cloned());
            inputs.sort();
            inputs.dedup();
            let (proposal_artifact_id, envelope) = self
                .cas
                .put_artifact(
                    review_core::contract::PATCH_PROPOSAL_V1,
                    Producer::Attempt {
                        run_id: self.run_id.clone(),
                        node_id: node.clone(),
                        attempt_id: selection.attempt_id.clone(),
                    },
                    inputs,
                    Some(candidate.base_snapshot_id.clone()),
                    serde_json::to_value(proposal).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            let payload = ProposalAcceptedPayloadV1 {
                proposal_id: envelope.artifact_id.clone(),
                proposal_artifact_id: proposal_artifact_id.clone(),
                candidate_artifact_id: candidate_artifact.clone(),
            };
            proposal_ids.push(envelope.artifact_id);
            events.push(
                NewEvent::new(
                    EventType::ProposalAcceptedV1,
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .node(node)
                .attempt(selection.attempt_id)
                .referencing(vec![proposal_artifact_id, candidate_artifact]),
            );
        }
        self.append_batch(&events)?;
        proposal_ids.sort();
        proposal_ids.dedup();
        Ok(proposal_ids)
    }

    fn run_ledger(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        // The ledger reduces what its edges delivered — never a global map of whatever happened
        // to run. Each input is one reviewer's result, or a gather manifest of result ids.
        let canonical = self.authority.finding_identity_policy
            == review_core::CANONICAL_FINDING_IDENTITY_POLICY;
        let mut results: Vec<(String, String, ReviewerResultContract, LegacyStageOutput)> =
            Vec::new();
        let mut dynamic_sets: Vec<(String, String, SliceSetV1, ShardSetV1)> = Vec::new();
        let mut direct_sources_used = BTreeSet::new();
        let mut load = |node: &str, id: &str, value: serde_json::Value| -> Result<(), String> {
            let (contract, output) =
                reviewer_stage_output(value).map_err(|error| format!("artifact {id}: {error}"))?;
            results.push((node.to_string(), id.to_string(), contract, output));
            Ok(())
        };
        for (input_port, artifacts) in inputs {
            for input in artifacts {
                let value = self.cas.get_json(input).map_err(|e| e.to_string())?;
                if value.get("type").and_then(serde_json::Value::as_str)
                    == Some(review_core::contract::SHARD_SET_V1)
                {
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value)
                        .map_err(|error| format!("ShardSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&envelope)?;
                    if envelope.subject_snapshot_id.as_deref()
                        != Some(self.authority.head_snapshot_id.as_str())
                    {
                        return Err(
                            "Ledger received a ShardSet outside current Subject authority".into(),
                        );
                    }
                    let scatter_node = match &envelope.producer {
                        Producer::KernelOperation {
                            node_id: Some(node),
                            ..
                        } => node.clone(),
                        _ => return Err("ShardSet@1 was not produced by its static Scatter".into()),
                    };
                    let shard_set: ShardSetV1 = serde_json::from_value(envelope.payload)
                        .map_err(|error| format!("ShardSet@1 payload is malformed: {error}"))?;
                    let slice_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(&shard_set.slice_set_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| format!("SliceSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&slice_envelope)?;
                    if slice_envelope.artifact_type != review_core::contract::SLICE_SET_V1 {
                        return Err("ShardSet@1 references a non-SliceSet artifact".into());
                    }
                    let slice_set: SliceSetV1 = serde_json::from_value(slice_envelope.payload)
                        .map_err(|error| format!("SliceSet@1 payload is malformed: {error}"))?;
                    shard_set.validate_against(&slice_set)?;
                    if slice_set.all_shards_required && !shard_set.complete() {
                        let failures = shard_set
                            .shards
                            .iter()
                            .filter_map(|shard| match &shard.outcome {
                                ShardOutcomeV1::Failed { reason }
                                | ShardOutcomeV1::Missing { reason } => {
                                    Some(format!("{}: {reason}", shard.runtime_node_id))
                                }
                                ShardOutcomeV1::Completed { .. } => None,
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        return Err(format!(
                            "Scatter `{scatter_node}` has a failed or missing required shard: {failures}"
                        ));
                    }
                    for shard in &shard_set.shards {
                        if let ShardOutcomeV1::Completed {
                            result_artifact_ids,
                        } = &shard.outcome
                        {
                            for result_id in result_artifact_ids {
                                let result = self
                                    .cas
                                    .get_json(result_id)
                                    .map_err(|error| error.to_string())?;
                                load(&shard.runtime_node_id, result_id, result)?;
                            }
                        }
                    }
                    dynamic_sets.push((scatter_node, input.clone(), slice_set, shard_set));
                    continue;
                }
                if value.get("verdict").is_some() && value.get("reports").is_some() {
                    let source = if canonical {
                        let upstream = self
                            .input_bindings
                            .get(&node.id)
                            .and_then(|ports| ports.get(input_port))
                            .ok_or_else(|| {
                                format!(
                                    "canonical ledger `{}.{input_port}` has no pinned input graph",
                                    node.id
                                )
                            })?;
                        let selections = self
                            .reviewer_selections
                            .lock()
                            .expect("reviewer selections");
                        let source = upstream.iter().find(|(source, _, kind)| {
                            *kind == NodeKind::Reviewer
                                && !direct_sources_used.contains(source)
                                && selections
                                    .get(source)
                                    .is_some_and(|selection| selection.result_artifact == *input)
                        });
                        source.map(|(source, _, _)| source.clone()).ok_or_else(|| {
                            format!(
                                "canonical ledger `{}.{input_port}` cannot bind delivered result {input} to its pinned upstream reviewers",
                                node.id
                            )
                        })?
                    } else {
                        input_port.clone()
                    };
                    direct_sources_used.insert(source.clone());
                    load(&source, input, value)?;
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
                            "artifact {input} is neither a supported ReviewerResult nor a gather manifest"
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
            for (index, (_, result_id, _, _)) in results.iter().enumerate() {
                result_indices
                    .entry(result_id.clone())
                    .or_default()
                    .push(index);
            }
            for (result_id, indices) in result_indices {
                let mut assigned = BTreeSet::new();
                let mut unmatched = Vec::new();
                for index in indices {
                    let node = &results[index].0;
                    if selections
                        .get(node)
                        .is_some_and(|selection| selection.result_artifact == result_id)
                    {
                        if !assigned.insert(node.clone()) {
                            return Err(format!(
                                "selected reviewer result for `{node}` was delivered more than once"
                            ));
                        }
                    } else {
                        unmatched.push(index);
                    }
                }
                let mut selected: Vec<_> = selections
                    .iter()
                    .filter(|(node, selection)| {
                        selection.result_artifact == result_id && !assigned.contains(*node)
                    })
                    .map(|(node, _)| node.clone())
                    .collect();
                if selected.len() < unmatched.len() {
                    return Err(format!(
                        "selected reviewer result {result_id} has {} unlabelled delivered copies and {} unused matching Attempts",
                        unmatched.len(),
                        selected.len(),
                    ));
                }
                selected.sort();
                unmatched.sort_by(|left, right| {
                    (&results[*left].0, *left).cmp(&(&results[*right].0, *right))
                });
                for (index, node) in unmatched.into_iter().zip(selected) {
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
                    .map(|(node, result_id, _, _)| {
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
        let (
            round,
            finding_count,
            finding_entries,
            grouping_relation_ids,
            grouping_input_artifact_ids,
            resolution_ids,
            resolution_input_artifact_ids,
            demand_entries,
            demand_artifact_ids,
            demand_input_artifact_ids,
            reduction,
            mut projection,
        ) = {
            let mut store = self.store.lock().expect("event store");
            let mut ingest =
                Ingest::from_projection(*store, self.cas, self.run_id.clone(), projection)
                    .map_err(|e| e.to_string())?
                    .under_round(&self.authority.round_event_id);
            let reduction_round =
                canonical_reduction_round(ingest.ledger().round, self.authority.round)?;
            let reduction = match &canonical_metadata {
                Some(metadata) => {
                    let stages: Vec<_> = results
                        .iter()
                        .zip(metadata)
                        .map(
                            |(
                                (node, result_id, result_contract, stage),
                                (attempt_id, input_artifacts),
                            )| {
                                let binding_node = self.reviewer_binding_node(node);
                                review_store::CanonicalStage {
                                    source: node,
                                    demand_requirement: self
                                        .demand_requirements
                                        .get(&binding_node)
                                        .copied()
                                        .unwrap_or(review_core::DemandRequirement::Required),
                                    stage,
                                    attempt_id,
                                    result_artifact_id: result_id,
                                    input_artifacts,
                                    subject_snapshot_id: &self.authority.head_snapshot_id,
                                    subject_id: &self.authority.subject_id,
                                    result_contract: *result_contract,
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
                    if results
                        .iter()
                        .any(|(_, _, contract, _)| *contract == ReviewerResultContract::V2)
                    {
                        return Err(
                            "ReviewerResult@2 requires canonical Finding identity authority".into(),
                        );
                    }
                    let stages: Vec<_> = results
                        .iter()
                        .map(|(node, _, _, stage)| (node.as_str(), stage))
                        .collect();
                    ingest
                        .add_live_stage_outputs(&stages)
                        .map_err(|error| error.to_string())?;
                    None
                }
            };
            (
                reduction_round,
                ingest.ledger().finding_views().len(),
                canonical.then(|| finding_set_entries(ingest.ledger())),
                ingest.ledger().grouping_relation_ids(),
                ingest.ledger().grouping_input_artifact_ids(),
                ingest.ledger().resolution_artifact_ids(),
                ingest.ledger().resolution_input_artifact_ids(),
                canonical.then(|| ingest.ledger().demand_views()),
                ingest.ledger().demand_reduction_artifact_ids(),
                ingest.ledger().demand_reduction_input_ids(),
                reduction,
                ingest.into_projection(),
            )
        };
        let proposal_ids = if let Some(reduction) = &reduction {
            let proposals = self.finalize_proposals(reduction)?;
            projection
                .fast_forward(*self.store.lock().expect("event store"), self.cas)
                .map_err(|error| error.to_string())?;
            proposals
        } else {
            Vec::new()
        };
        let semantic_reduction_outputs = reduction.as_ref().map(|reduction| {
            (
                reduction.selected_report_ids.clone(),
                reduction.relation_ids.clone(),
                reduction.selected_demand_artifact_ids.clone(),
            )
        });
        let semantic_grouping_ids = grouping_relation_ids.clone();
        let semantic_resolution_ids = resolution_ids.clone();
        let semantic_demand_lifecycle_ids = demand_artifact_ids.clone();
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        let findings_artifact = if let (Some(entries), Some(reduction)) =
            (finding_entries, reduction)
        {
            let reducer_version = reduction.reducer_version;
            let payload = review_core::FindingSetV1 {
                subject_id: self.authority.subject_id.clone(),
                round,
                prior_finding_set_id: self.authority.prior_reduction_finding_set_id.clone(),
                reducer_version: reducer_version.to_string(),
                identity_policy: self.authority.finding_identity_policy.clone(),
                selected_report_ids: reduction.selected_report_ids,
                relation_ids: reduction
                    .relation_ids
                    .into_iter()
                    .chain(grouping_relation_ids)
                    .collect(),
                resolution_ids,
                findings: entries,
            };
            payload.validate()?;
            let mut reduction_inputs = vec![self.authority.prior_reduction_finding_set_id.clone()];
            reduction_inputs.extend(reduction.input_artifact_ids);
            reduction_inputs.extend(grouping_input_artifact_ids);
            reduction_inputs.extend(resolution_input_artifact_ids);
            let operation_digest = review_store::content_id(&serde_json::json!({
                "reducer_version": reducer_version,
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
                            reducer_version,
                            self.authority.finding_identity_policy,
                            operation_digest
                        ),
                    },
                    reduction_inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            record_id
        } else {
            // The `findings` port must carry a real artifact, not a label: the scheduler delivers
            // exactly this string to whatever consumes the port, and a downstream event referencing
            // a non-CAS string would be rejected as a dangling artifact far from its cause.
            self.cas
                .put_json(&serde_json::json!({
                    "round": round,
                    "sources": results.iter().map(|(node, _, _, _)| node).collect::<Vec<_>>(),
                    "findings": finding_count,
                }))
                .map_err(|e| e.to_string())?
        };

        let finding_port = node
            .outputs
            .iter()
            .find(|port| is_generation_finding_set_output(port))
            .or_else(|| (node.outputs.len() == 1).then(|| &node.outputs[0]))
            .ok_or_else(|| "ledger node has no Finding Set output".to_string())?;
        let mut outputs = ArtifactMap::from([(finding_port.name.clone(), vec![findings_artifact])]);

        if let Some(demands) = demand_entries {
            if round == 1 && self.authority.prior_demand_set_id != self.authority.demand_genesis_id
            {
                return Err("Round 1 Demand Set does not descend from Campaign genesis".into());
            }
            let (selected_demand_artifact_ids, satisfaction_artifact_ids, waiver_artifact_ids) =
                demand_artifact_ids;
            let payload = review_core::DemandSetV1 {
                subject_id: self.authority.subject_id.clone(),
                round,
                prior_demand_set_id: self.authority.prior_demand_set_id.clone(),
                reducer_version: review_core::DEMAND_REDUCER_VERSION.into(),
                selected_demand_artifact_ids,
                satisfaction_artifact_ids,
                waiver_artifact_ids,
                demands,
            };
            payload.validate()?;
            let mut reduction_inputs = vec![self.authority.prior_demand_set_id.clone()];
            reduction_inputs.extend(demand_input_artifact_ids);
            let mut unique = BTreeSet::new();
            reduction_inputs.retain(|id| unique.insert(id.clone()));
            let operation_digest = review_store::content_id(&serde_json::json!({
                "reducer_version": review_core::DEMAND_REDUCER_VERSION,
                "inputs": reduction_inputs,
            }))
            .map_err(|error| error.to_string())?;
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::DEMAND_SET_V1,
                    review_core::Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some("ledger".into()),
                        operation_id: format!(
                            "{}:{}",
                            review_core::DEMAND_REDUCER_VERSION,
                            operation_digest
                        ),
                    },
                    reduction_inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            match node.outputs.iter().find(|port| is_demand_set_port(port)) {
                Some(port) => {
                    outputs.insert(port.name.clone(), vec![record_id]);
                }
                None if !outputs.is_empty()
                    && self
                        .ledger_cache
                        .lock()
                        .expect("ledger cache")
                        .as_ref()
                        .is_some_and(|projection| {
                            !projection.ledger().demand_views().is_empty()
                        }) =>
                {
                    return Err(
                        "ledger selected Demands but declares no review.kernel/DemandSet@1 output"
                            .into(),
                    );
                }
                None => {}
            }
        }
        if !dynamic_sets.is_empty() {
            let delivered_results = results
                .iter()
                .map(|(source, artifact, _, _)| (source.clone(), artifact.clone()))
                .collect::<BTreeSet<_>>();
            let round_events = self
                .store
                .lock()
                .expect("event store")
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|event| {
                    event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
                })
                .collect::<Vec<_>>();
            for (scatter_node, shard_record, slice_set, shard_set) in dynamic_sets {
                let mut selected = Vec::new();
                for shard in &shard_set.shards {
                    if let ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } = &shard.outcome
                    {
                        for artifact in result_artifact_ids {
                            if !delivered_results
                                .contains(&(shard.runtime_node_id.clone(), artifact.clone()))
                            {
                                return Err(format!(
                                    "dynamic result `{artifact}` from `{}` did not reach Ledger",
                                    shard.runtime_node_id
                                ));
                            }
                            selected.push(artifact.clone());
                        }
                    }
                }
                let selected_outputs = BTreeMap::from([("reviewer_results".into(), selected)]);
                let mut sinks = selected_outputs
                    .values()
                    .flatten()
                    .map(|artifact| (artifact.clone(), "ledger:reviewer-result".into()))
                    .collect::<BTreeMap<String, String>>();
                let closeout_result_id = match &slice_set.closeout {
                    review_core::CloseoutPolicyV1::Required => {
                        let closeout = self.closeouts.get(&scatter_node).ok_or_else(|| {
                            format!("Scatter `{scatter_node}` has no captured closeout reviewer")
                        })?;
                        let ids = results
                            .iter()
                            .filter(|(source, _, _, _)| source == closeout)
                            .map(|(_, artifact, _, _)| artifact.clone())
                            .collect::<Vec<_>>();
                        let [artifact] = ids.as_slice() else {
                            return Err(format!(
                                "closeout reviewer `{closeout}` did not deliver exactly one result"
                            ));
                        };
                        sinks.insert(artifact.clone(), "ledger:whole-subject-closeout".into());
                        Some(artifact.clone())
                    }
                    review_core::CloseoutPolicyV1::Waived { policy_id, .. } => {
                        if !self.authority.policy_ids.contains(policy_id) {
                            return Err(format!(
                                "closeout waiver `{policy_id}` is absent from Authority Snapshot"
                            ));
                        }
                        None
                    }
                };
                let mut closure = scatter::prove_semantic_closure(
                    &slice_set,
                    &shard_set,
                    &selected_outputs,
                    &sinks,
                    closeout_result_id.clone(),
                )?;
                let mut semantic_sinks = closure
                    .dispositions
                    .iter()
                    .map(|disposition| (disposition.artifact_id.clone(), disposition.sink.clone()))
                    .collect::<BTreeMap<_, _>>();
                if let Some(closeout) = closeout_result_id {
                    semantic_sinks.insert(closeout, "ledger:whole-subject-closeout".into());
                }
                if let Some((reports, relations, demands)) = &semantic_reduction_outputs {
                    for artifact in reports {
                        semantic_sinks.insert(artifact.clone(), "ledger:finding-set".into());
                    }
                    for artifact in relations {
                        semantic_sinks.insert(artifact.clone(), "ledger:relation".into());
                    }
                    for artifact in demands {
                        semantic_sinks.insert(artifact.clone(), "ledger:demand-set".into());
                    }
                }
                for artifact in &semantic_grouping_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:grouping".into());
                }
                for artifact in &semantic_resolution_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:resolution".into());
                }
                for artifact in semantic_demand_lifecycle_ids
                    .0
                    .iter()
                    .chain(&semantic_demand_lifecycle_ids.1)
                    .chain(&semantic_demand_lifecycle_ids.2)
                {
                    semantic_sinks.insert(artifact.clone(), "ledger:demand-lifecycle".into());
                }
                for proposal_id in &proposal_ids {
                    semantic_sinks.insert(proposal_id.clone(), "proposal-store".into());
                    let proposal_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(proposal_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                    let proposal: review_core::PatchProposal =
                        serde_json::from_value(proposal_envelope.payload)
                            .map_err(|error| error.to_string())?;
                    for evidence in proposal.evidence_ids {
                        semantic_sinks.insert(evidence, "proposal-store:evidence".into());
                    }
                }
                for event in &round_events {
                    let sink = match event.event_type {
                        EventType::CheckCompletedV1 => Some("event-log:check"),
                        EventType::GateDecisionV1 => Some("event-log:policy"),
                        EventType::EvidenceAddedV1 => Some("ledger:evidence"),
                        _ => None,
                    };
                    if let Some(sink) = sink {
                        let artifact = self
                            .cas
                            .put_json(&event.payload)
                            .map_err(|error| error.to_string())?;
                        semantic_sinks.insert(artifact, sink.into());
                    }
                }
                closure.required_artifact_ids = semantic_sinks.keys().cloned().collect();
                closure.dispositions = semantic_sinks
                    .into_iter()
                    .map(|(artifact_id, sink)| review_core::SemanticDispositionV1 {
                        artifact_id,
                        sink,
                    })
                    .collect();
                closure.validate()?;
                let mut closure_inputs = vec![shard_record.clone(), shard_set.slice_set_id.clone()];
                closure_inputs.extend(closure.required_artifact_ids.iter().cloned());
                closure_inputs.sort();
                closure_inputs.dedup();
                for artifact in &closure_inputs {
                    self.cas.verify(artifact).map_err(|error| {
                        format!("semantic closure input `{artifact}` is not durable: {error}")
                    })?;
                }
                let operation_id = review_store::content_id(
                    &serde_json::to_value(&closure).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let (record_id, _) = self
                    .cas
                    .put_artifact(
                        review_core::contract::SEMANTIC_CLOSURE_V1,
                        Producer::KernelOperation {
                            run_id: self.run_id.clone(),
                            node_id: Some(node.id.clone()),
                            operation_id,
                        },
                        closure_inputs,
                        Some(self.authority.head_snapshot_id.clone()),
                        serde_json::to_value(closure).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::SemanticClosureCheckedV1,
                        serde_json::to_value(RecordedSetPayloadV1 {
                            artifact_id: record_id.clone(),
                            record_id: record_id.clone(),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .node(&node.id)
                    .referencing(vec![record_id]),
                )?;
            }
        }
        Ok(outputs)
    }
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn current_integration_authority(
    events: &[review_core::RunEvent],
    round_event_id: &str,
) -> Result<(String, String, String), String> {
    let mut finding_set = None;
    let mut demand_set = None;
    let mut semantic_closure = None;
    for event in events
        .iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_event_id))
    {
        match event.event_type {
            EventType::NodeOutputReceiptV1 => {
                let receipt: NodeOutputReceiptPayloadV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
                for port in receipt.outputs {
                    let target = if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                        Some(&mut finding_set)
                    } else if port.artifact_type == review_core::contract::DEMAND_SET_V1 {
                        Some(&mut demand_set)
                    } else {
                        None
                    };
                    if let Some(target) = target {
                        if port.artifact_ids.is_empty() && port.optional {
                            continue;
                        }
                        let [artifact] = port.artifact_ids.as_slice() else {
                            return Err(format!(
                                "Integration authority port `{}` is not singular",
                                port.port
                            ));
                        };
                        *target = Some(artifact.clone());
                    }
                }
            }
            EventType::SemanticClosureCheckedV1 => {
                let recorded: RecordedSetPayloadV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
                semantic_closure = Some(recorded.record_id);
            }
            _ => {}
        }
    }
    Ok((
        finding_set.ok_or("Integration has no exact current FindingSet@1")?,
        demand_set.ok_or("Integration has no exact current DemandSet@1")?,
        semantic_closure.ok_or("Integration has no SemanticClosure@1 proof")?,
    ))
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

/// Run every shard on at most `bound` scoped threads and return the results in input order.
/// Scoped, so a shard borrows the kernel like a static node does; bounded by the Slice policy,
/// not by the machine, so a 32-shard policy on an 8-core host still runs 32 model calls at once
/// while the filesystem executor stays free for the phases inside each shard.
fn dispatch_shards<T, R>(items: Vec<T>, bound: u32, operation: impl Fn(T) -> R + Sync) -> Vec<R>
where
    T: Send,
    R: Send,
{
    let count = items.len();
    let workers = usize::try_from(bound)
        .unwrap_or(usize::MAX)
        .clamp(1, count.max(1));
    let queue = Mutex::new(items.into_iter().enumerate().collect::<VecDeque<_>>());
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..count).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("shard queue").pop_front();
                    let Some((index, item)) = next else {
                        break;
                    };
                    let result = operation(item);
                    results.lock().expect("shard results")[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("shard results")
        .into_iter()
        .map(|result| result.expect("every shard produced a result"))
        .collect()
}

/// Reduce the exact FindingSet@1 to what `node` must examine this Round: the rows the Round's
/// pinned assignment document partitions to it under `assignments` — or, for a Round started
/// before partitions existed, the whole Round-wide union, which stays the frozen delivery for
/// those Rounds. A dynamic shard passes its Scatter's id, the node the partition is keyed by.
fn retain_round_assignment(
    set: &mut review_core::FindingSetV1,
    round_assignment: &serde_json::Value,
    node: &str,
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
    let retained: BTreeSet<&str> = match round_assignment.get("assignments") {
        None => assigned,
        Some(assignments) => {
            let partition = assignments
                .as_object()
                .ok_or("Round prior Finding assignments are not an object")?;
            let mine = partition
                .get(node)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    format!("Round prior Finding assignment has no partition for reviewer `{node}`")
                })?;
            let mut retained = BTreeSet::new();
            for key in mine {
                let key = key.as_str().ok_or_else(|| {
                    format!("Round prior Finding assignment for `{node}` contains a non-string key")
                })?;
                if !assigned.contains(key) {
                    return Err(format!(
                        "Round prior Finding assignment for `{node}` names a Finding outside the Round assignment"
                    ));
                }
                retained.insert(key);
            }
            retained
        }
    };
    set.findings
        .retain(|finding| retained.contains(finding.finding_id.as_str()));
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
        Ok(())
    }

    /// The reservation and durable dispatch of a reviewer's first Attempt happen here, on the
    /// scheduler thread as the node takes its slot — never earlier, so a run cap is consumed in
    /// dispatch order and a released reservation is available to the node dispatched next.
    fn prepare_dispatch(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        if node.kind == NodeKind::Reviewer && !self.replayed_outputs.contains_key(&node.id) {
            let binding_node = self.reviewer_binding_node(&node.id);
            if !self.reviewers.contains_key(&binding_node) {
                return Err(format!("no reviewer bound to node {}", node.id));
            }
            let prior_findings = node
                .inputs
                .iter()
                .find(|port| is_reviewer_prior_set_input(port, self.pipeline_version))
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
            NodeKind::Gate => {
                let result = self.run_gate(&node.id);
                if result.is_err() {
                    self.record_unmaterialized_cache_failures(
                        &node.id,
                        RunCacheFailureReasonV5::GateSetupFailed,
                    );
                }
                result
            }
            NodeKind::Slicer => self.run_slicer(node),
            NodeKind::Scatter => self.run_scatter(node, inputs),
            // Gather and ledger reduce whatever artifacts their edges delivered; the port
            // labels are the reviewer's concern, not theirs.
            NodeKind::Gather => self.run_gather(node, inputs),
            NodeKind::Ledger => return self.run_ledger(node, inputs),
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
                self.node_outputs
                    .lock()
                    .expect("node outputs")
                    .insert(node.id.clone(), outputs.clone());
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
        self.node_outputs
            .lock()
            .expect("node outputs")
            .insert(node.id.clone(), outputs.clone());
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

    fn reviewer_credential_mode(&self, node: &str) -> Option<BrokerCredentialModeV1> {
        self.reviewers
            .get(node)
            .map(|adapter| adapter.credential_mode())
    }

    fn broker_provider_available(&self, node: &str) -> bool {
        self.broker_providers.contains_key(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
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
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
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
        let terminal = review_core::RunEvent {
            event_id: "report-1".into(),
            run_id: "run".into(),
            sequence: 2,
            event_type: EventType::RunReportV3,
            occurred_at: "2026-08-26T00:00:02Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RunReportPayloadV3 {
                outcomes: vec![RunNodeReportV2 {
                    node: "ledger".into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: "campaign exhausted".into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::Exhausted,
                },
                spent_tokens: Some(1),
            })
            .unwrap(),
        };

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
            "correctness",
        )
        .unwrap();

        assert_eq!(set.findings.len(), 1);
        assert_eq!(set.findings[0].finding_id, keep);
    }

    /// With a partition beside the union, a reviewer is delivered only its own rows; a Round
    /// without one (started before partitions existed) still delivers the whole union; a node
    /// the partition does not name is refused rather than silently given everything.
    #[test]
    fn a_partitioned_round_assignment_delivers_only_the_reviewers_own_rows() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let entry = |finding_id: String, source: &str| review_core::FindingSetEntryV1 {
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
            source: source.into(),
            last_seen_round: 1,
            report_ids: vec![digest('d')],
        };
        let architecture_row = digest('a');
        let performance_row = digest('b');
        let orphan_row = digest('c');
        let full_set = || review_core::FindingSetV1 {
            subject_id: digest('d'),
            round: 2,
            prior_finding_set_id: digest('e'),
            reducer_version: review_core::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: vec![
                entry(architecture_row.clone(), "architecture"),
                entry(performance_row.clone(), "performance"),
                entry(orphan_row.clone(), "imported"),
            ],
        };
        let partitioned = serde_json::json!({
            "subject_id": digest('f'),
            "round": 2,
            "prior_findings": [
                {"key": architecture_row, "source": "architecture"},
                {"key": performance_row, "source": "performance"},
                {"key": orphan_row, "source": "imported"},
            ],
            "assignments": {
                "architecture": [architecture_row, orphan_row],
                "performance": [performance_row, orphan_row],
            },
        });

        let mut set = full_set();
        retain_round_assignment(&mut set, &partitioned, "architecture").unwrap();
        assert_eq!(
            set.findings
                .iter()
                .map(|finding| finding.finding_id.clone())
                .collect::<Vec<_>>(),
            vec![architecture_row.clone(), orphan_row.clone()]
        );

        let mut set = full_set();
        retain_round_assignment(&mut set, &partitioned, "performance").unwrap();
        assert_eq!(
            set.findings
                .iter()
                .map(|finding| finding.finding_id.clone())
                .collect::<Vec<_>>(),
            vec![performance_row.clone(), orphan_row.clone()]
        );

        let error = retain_round_assignment(&mut full_set(), &partitioned, "tests").unwrap_err();
        assert!(
            error.contains("no partition for reviewer `tests`"),
            "{error}"
        );

        let mut legacy = partitioned.clone();
        legacy.as_object_mut().unwrap().remove("assignments");
        let mut set = full_set();
        retain_round_assignment(&mut set, &legacy, "architecture").unwrap();
        assert_eq!(
            set.findings.len(),
            3,
            "a Round without a partition keeps the union"
        );
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
