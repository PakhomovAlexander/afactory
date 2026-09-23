//! `RunEvent@1` — the append-only source of truth.

mod task_report;
pub use task_report::{RunReportExecutionV6, RunReportPayloadV6, TaskReviewAccountingV1};

use serde::{Deserialize, Serialize};

/// The complete event vocabulary understood by this kernel build.
///
/// The database representation is intentionally the same `Type@N` string used by the JSON
/// contract. Adding an event or a new payload version is an explicit enum and schema change;
/// arbitrary strings cannot enter a new log through the typed API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "TaskTransition@5")]
    TaskTransitionV5,
    #[serde(rename = "TaskReviewResultSelected@1")]
    TaskReviewResultSelectedV1,
    #[serde(rename = "CheckCompleted@1")]
    CheckCompletedV1,
    #[serde(rename = "CampaignOpened@1")]
    CampaignOpenedV1,
    #[serde(rename = "ChangeAttested@1")]
    ChangeAttestedV1,
    #[serde(rename = "DemandRecorded@1")]
    DemandRecordedV1,
    #[serde(rename = "DemandWaived@1")]
    DemandWaivedV1,
    #[serde(rename = "EvidenceAdded@1")]
    EvidenceAddedV1,
    #[serde(rename = "EvidenceReuseAdmitted@1")]
    EvidenceReuseAdmittedV1,
    #[serde(rename = "EvidenceSatisfied@1")]
    EvidenceSatisfiedV1,
    #[serde(rename = "FindingReported@1")]
    FindingReportedV1,
    #[serde(rename = "FindingResolutionChallenged@1")]
    FindingResolutionChallengedV1,
    #[serde(rename = "FindingResolutionRecorded@1")]
    FindingResolutionRecordedV1,
    #[serde(rename = "FindingResolved@1")]
    FindingResolvedV1,
    #[serde(rename = "FindingsGrouped@1")]
    FindingsGroupedV1,
    #[serde(rename = "FindingsUngrouped@1")]
    FindingsUngroupedV1,
    #[serde(rename = "FixVerified@1")]
    FixVerifiedV1,
    #[serde(rename = "GateDecision@1")]
    GateDecisionV1,
    #[serde(rename = "GateExecutionBound@1")]
    GateExecutionBoundV1,
    #[serde(rename = "CacheSnapshotMaterialized@1")]
    CacheSnapshotMaterializedV1,
    #[serde(rename = "GenerationAdvanced@1")]
    GenerationAdvancedV1,
    #[serde(rename = "IntegrationPrepared@1")]
    IntegrationPreparedV1,
    #[serde(rename = "IntegrationConflict@1")]
    IntegrationConflictV1,
    #[serde(rename = "IntegrationChecksCompleted@1")]
    IntegrationChecksCompletedV1,
    #[serde(rename = "IntegrationCommitted@1")]
    IntegrationCommittedV1,
    #[serde(rename = "NodeInvocation@1")]
    NodeInvocationV1,
    #[serde(rename = "NodeOutputReceipt@1")]
    NodeOutputReceiptV1,
    #[serde(rename = "PolicyTimeAdvanced@1")]
    PolicyTimeAdvancedV1,
    #[serde(rename = "ProposalAccepted@1")]
    ProposalAcceptedV1,
    #[serde(rename = "ProposalPrepared@1")]
    ProposalPreparedV1,
    #[serde(rename = "ProposalRefused@1")]
    ProposalRefusedV1,
    #[serde(rename = "RunReport@6")]
    RunReportV6,
    #[serde(rename = "RoundInputSuperseded@1")]
    RoundInputSupersededV1,
    #[serde(rename = "RoundStarted@1")]
    RoundStartedV1,
    #[serde(rename = "SourceCaptured@1")]
    SourceCapturedV1,
    #[serde(rename = "SliceSetAccepted@1")]
    SliceSetAcceptedV1,
    #[serde(rename = "ShardSetRecorded@1")]
    ShardSetRecordedV1,
    #[serde(rename = "SemanticClosureChecked@1")]
    SemanticClosureCheckedV1,
    #[serde(rename = "WarmSetSelected@1")]
    WarmSetSelectedV1,
    #[serde(rename = "WorkerNotesRecorded@1")]
    WorkerNotesRecordedV1,
    #[serde(rename = "BuildCacheCaptured@1")]
    BuildCacheCapturedV1,
    #[serde(rename = "WorkspaceRebased@1")]
    WorkspaceRebasedV1,
    #[serde(rename = "SessionSnapshotPrepared@1")]
    SessionSnapshotPreparedV1,
    #[serde(rename = "SessionSnapshotCleaned@1")]
    SessionSnapshotCleanedV1,
    #[serde(rename = "ColdCloseoutDispatched@1")]
    ColdCloseoutDispatchedV1,
}

impl EventType {
    pub const ALL: [Self; 45] = [
        Self::TaskTransitionV5,
        Self::TaskReviewResultSelectedV1,
        Self::CheckCompletedV1,
        Self::CampaignOpenedV1,
        Self::ChangeAttestedV1,
        Self::DemandRecordedV1,
        Self::DemandWaivedV1,
        Self::EvidenceAddedV1,
        Self::EvidenceReuseAdmittedV1,
        Self::EvidenceSatisfiedV1,
        Self::FindingReportedV1,
        Self::FindingResolutionChallengedV1,
        Self::FindingResolutionRecordedV1,
        Self::FindingResolvedV1,
        Self::FindingsGroupedV1,
        Self::FindingsUngroupedV1,
        Self::FixVerifiedV1,
        Self::GateDecisionV1,
        Self::GateExecutionBoundV1,
        Self::CacheSnapshotMaterializedV1,
        Self::GenerationAdvancedV1,
        Self::IntegrationPreparedV1,
        Self::IntegrationConflictV1,
        Self::IntegrationChecksCompletedV1,
        Self::IntegrationCommittedV1,
        Self::NodeInvocationV1,
        Self::NodeOutputReceiptV1,
        Self::PolicyTimeAdvancedV1,
        Self::ProposalAcceptedV1,
        Self::ProposalPreparedV1,
        Self::ProposalRefusedV1,
        Self::RunReportV6,
        Self::RoundInputSupersededV1,
        Self::RoundStartedV1,
        Self::SourceCapturedV1,
        Self::SliceSetAcceptedV1,
        Self::ShardSetRecordedV1,
        Self::SemanticClosureCheckedV1,
        Self::WarmSetSelectedV1,
        Self::WorkerNotesRecordedV1,
        Self::BuildCacheCapturedV1,
        Self::WorkspaceRebasedV1,
        Self::SessionSnapshotPreparedV1,
        Self::SessionSnapshotCleanedV1,
        Self::ColdCloseoutDispatchedV1,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskTransitionV5 => "TaskTransition@5",
            Self::TaskReviewResultSelectedV1 => "TaskReviewResultSelected@1",
            Self::CheckCompletedV1 => "CheckCompleted@1",
            Self::CampaignOpenedV1 => "CampaignOpened@1",
            Self::ChangeAttestedV1 => "ChangeAttested@1",
            Self::DemandRecordedV1 => "DemandRecorded@1",
            Self::DemandWaivedV1 => "DemandWaived@1",
            Self::EvidenceAddedV1 => "EvidenceAdded@1",
            Self::EvidenceReuseAdmittedV1 => "EvidenceReuseAdmitted@1",
            Self::EvidenceSatisfiedV1 => "EvidenceSatisfied@1",
            Self::FindingReportedV1 => "FindingReported@1",
            Self::FindingResolutionChallengedV1 => "FindingResolutionChallenged@1",
            Self::FindingResolutionRecordedV1 => "FindingResolutionRecorded@1",
            Self::FindingsGroupedV1 => "FindingsGrouped@1",
            Self::FindingsUngroupedV1 => "FindingsUngrouped@1",
            Self::FixVerifiedV1 => "FixVerified@1",
            Self::FindingResolvedV1 => "FindingResolved@1",
            Self::GateDecisionV1 => "GateDecision@1",
            Self::GateExecutionBoundV1 => "GateExecutionBound@1",
            Self::CacheSnapshotMaterializedV1 => "CacheSnapshotMaterialized@1",
            Self::GenerationAdvancedV1 => "GenerationAdvanced@1",
            Self::IntegrationPreparedV1 => "IntegrationPrepared@1",
            Self::IntegrationConflictV1 => "IntegrationConflict@1",
            Self::IntegrationChecksCompletedV1 => "IntegrationChecksCompleted@1",
            Self::IntegrationCommittedV1 => "IntegrationCommitted@1",
            Self::NodeInvocationV1 => "NodeInvocation@1",
            Self::NodeOutputReceiptV1 => "NodeOutputReceipt@1",
            Self::PolicyTimeAdvancedV1 => "PolicyTimeAdvanced@1",
            Self::ProposalAcceptedV1 => "ProposalAccepted@1",
            Self::ProposalPreparedV1 => "ProposalPrepared@1",
            Self::ProposalRefusedV1 => "ProposalRefused@1",
            Self::RunReportV6 => "RunReport@6",
            Self::RoundInputSupersededV1 => "RoundInputSuperseded@1",
            Self::RoundStartedV1 => "RoundStarted@1",
            Self::SourceCapturedV1 => "SourceCaptured@1",
            Self::SliceSetAcceptedV1 => "SliceSetAccepted@1",
            Self::ShardSetRecordedV1 => "ShardSetRecorded@1",
            Self::SemanticClosureCheckedV1 => "SemanticClosureChecked@1",
            Self::WarmSetSelectedV1 => "WarmSetSelected@1",
            Self::WorkerNotesRecordedV1 => "WorkerNotesRecorded@1",
            Self::BuildCacheCapturedV1 => "BuildCacheCaptured@1",
            Self::WorkspaceRebasedV1 => "WorkspaceRebased@1",
            Self::SessionSnapshotPreparedV1 => "SessionSnapshotPrepared@1",
            Self::SessionSnapshotCleanedV1 => "SessionSnapshotCleaned@1",
            Self::ColdCloseoutDispatchedV1 => "ColdCloseoutDispatched@1",
        }
    }

    /// Whether this event is the durable run conclusion.
    pub const fn is_run_report(self) -> bool {
        matches!(self, Self::RunReportV6)
    }

    pub const fn typed(self) -> (&'static str, u32) {
        match self {
            Self::TaskTransitionV5 => ("TaskTransition", 5),
            Self::TaskReviewResultSelectedV1 => ("TaskReviewResultSelected", 1),
            Self::CheckCompletedV1 => ("CheckCompleted", 1),
            Self::CampaignOpenedV1 => ("CampaignOpened", 1),
            Self::ChangeAttestedV1 => ("ChangeAttested", 1),
            Self::DemandRecordedV1 => ("DemandRecorded", 1),
            Self::DemandWaivedV1 => ("DemandWaived", 1),
            Self::EvidenceAddedV1 => ("EvidenceAdded", 1),
            Self::EvidenceReuseAdmittedV1 => ("EvidenceReuseAdmitted", 1),
            Self::EvidenceSatisfiedV1 => ("EvidenceSatisfied", 1),
            Self::FindingReportedV1 => ("FindingReported", 1),
            Self::FindingResolutionChallengedV1 => ("FindingResolutionChallenged", 1),
            Self::FindingResolutionRecordedV1 => ("FindingResolutionRecorded", 1),
            Self::FindingsGroupedV1 => ("FindingsGrouped", 1),
            Self::FindingsUngroupedV1 => ("FindingsUngrouped", 1),
            Self::FixVerifiedV1 => ("FixVerified", 1),
            Self::FindingResolvedV1 => ("FindingResolved", 1),
            Self::GateDecisionV1 => ("GateDecision", 1),
            Self::GateExecutionBoundV1 => ("GateExecutionBound", 1),
            Self::CacheSnapshotMaterializedV1 => ("CacheSnapshotMaterialized", 1),
            Self::GenerationAdvancedV1 => ("GenerationAdvanced", 1),
            Self::IntegrationPreparedV1 => ("IntegrationPrepared", 1),
            Self::IntegrationConflictV1 => ("IntegrationConflict", 1),
            Self::IntegrationChecksCompletedV1 => ("IntegrationChecksCompleted", 1),
            Self::IntegrationCommittedV1 => ("IntegrationCommitted", 1),
            Self::NodeInvocationV1 => ("NodeInvocation", 1),
            Self::NodeOutputReceiptV1 => ("NodeOutputReceipt", 1),
            Self::PolicyTimeAdvancedV1 => ("PolicyTimeAdvanced", 1),
            Self::ProposalAcceptedV1 => ("ProposalAccepted", 1),
            Self::ProposalPreparedV1 => ("ProposalPrepared", 1),
            Self::ProposalRefusedV1 => ("ProposalRefused", 1),
            Self::RunReportV6 => ("RunReport", 6),
            Self::RoundInputSupersededV1 => ("RoundInputSuperseded", 1),
            Self::RoundStartedV1 => ("RoundStarted", 1),
            Self::SourceCapturedV1 => ("SourceCaptured", 1),
            Self::SliceSetAcceptedV1 => ("SliceSetAccepted", 1),
            Self::ShardSetRecordedV1 => ("ShardSetRecorded", 1),
            Self::SemanticClosureCheckedV1 => ("SemanticClosureChecked", 1),
            Self::WarmSetSelectedV1 => ("WarmSetSelected", 1),
            Self::WorkerNotesRecordedV1 => ("WorkerNotesRecorded", 1),
            Self::BuildCacheCapturedV1 => ("BuildCacheCaptured", 1),
            Self::WorkspaceRebasedV1 => ("WorkspaceRebased", 1),
            Self::SessionSnapshotPreparedV1 => ("SessionSnapshotPrepared", 1),
            Self::SessionSnapshotCleanedV1 => ("SessionSnapshotCleaned", 1),
            Self::ColdCloseoutDispatchedV1 => ("ColdCloseoutDispatched", 1),
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for EventType {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<EventType> for &str {
    fn eq(&self, other: &EventType) -> bool {
        *self == other.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEventType(pub String);

impl std::fmt::Display for UnknownEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown review-kernel event type: {}; this log was written by another af release; \
             start a new Campaign or Task",
            self.0
        )
    }
}

impl std::error::Error for UnknownEventType {}

impl std::str::FromStr for EventType {
    type Err = UnknownEventType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "TaskTransition@5" => Ok(Self::TaskTransitionV5),
            "TaskReviewResultSelected@1" => Ok(Self::TaskReviewResultSelectedV1),
            "CheckCompleted@1" => Ok(Self::CheckCompletedV1),
            "CampaignOpened@1" => Ok(Self::CampaignOpenedV1),
            "ChangeAttested@1" => Ok(Self::ChangeAttestedV1),
            "DemandRecorded@1" => Ok(Self::DemandRecordedV1),
            "DemandWaived@1" => Ok(Self::DemandWaivedV1),
            "EvidenceAdded@1" => Ok(Self::EvidenceAddedV1),
            "EvidenceReuseAdmitted@1" => Ok(Self::EvidenceReuseAdmittedV1),
            "EvidenceSatisfied@1" => Ok(Self::EvidenceSatisfiedV1),
            "FindingReported@1" => Ok(Self::FindingReportedV1),
            "FindingResolutionChallenged@1" => Ok(Self::FindingResolutionChallengedV1),
            "FindingResolutionRecorded@1" => Ok(Self::FindingResolutionRecordedV1),
            "FindingsGrouped@1" => Ok(Self::FindingsGroupedV1),
            "FindingsUngrouped@1" => Ok(Self::FindingsUngroupedV1),
            "FixVerified@1" => Ok(Self::FixVerifiedV1),
            "FindingResolved@1" => Ok(Self::FindingResolvedV1),
            "GateDecision@1" => Ok(Self::GateDecisionV1),
            "GateExecutionBound@1" => Ok(Self::GateExecutionBoundV1),
            "CacheSnapshotMaterialized@1" => Ok(Self::CacheSnapshotMaterializedV1),
            "GenerationAdvanced@1" => Ok(Self::GenerationAdvancedV1),
            "IntegrationPrepared@1" => Ok(Self::IntegrationPreparedV1),
            "IntegrationConflict@1" => Ok(Self::IntegrationConflictV1),
            "IntegrationChecksCompleted@1" => Ok(Self::IntegrationChecksCompletedV1),
            "IntegrationCommitted@1" => Ok(Self::IntegrationCommittedV1),
            "NodeInvocation@1" => Ok(Self::NodeInvocationV1),
            "NodeOutputReceipt@1" => Ok(Self::NodeOutputReceiptV1),
            "PolicyTimeAdvanced@1" => Ok(Self::PolicyTimeAdvancedV1),
            "ProposalAccepted@1" => Ok(Self::ProposalAcceptedV1),
            "ProposalPrepared@1" => Ok(Self::ProposalPreparedV1),
            "ProposalRefused@1" => Ok(Self::ProposalRefusedV1),
            "RunReport@6" => Ok(Self::RunReportV6),
            "RoundInputSuperseded@1" => Ok(Self::RoundInputSupersededV1),
            "RoundStarted@1" => Ok(Self::RoundStartedV1),
            "SourceCaptured@1" => Ok(Self::SourceCapturedV1),
            "SliceSetAccepted@1" => Ok(Self::SliceSetAcceptedV1),
            "ShardSetRecorded@1" => Ok(Self::ShardSetRecordedV1),
            "SemanticClosureChecked@1" => Ok(Self::SemanticClosureCheckedV1),
            "WarmSetSelected@1" => Ok(Self::WarmSetSelectedV1),
            "WorkerNotesRecorded@1" => Ok(Self::WorkerNotesRecordedV1),
            "BuildCacheCaptured@1" => Ok(Self::BuildCacheCapturedV1),
            "WorkspaceRebased@1" => Ok(Self::WorkspaceRebasedV1),
            "SessionSnapshotPrepared@1" => Ok(Self::SessionSnapshotPreparedV1),
            "SessionSnapshotCleaned@1" => Ok(Self::SessionSnapshotCleanedV1),
            "ColdCloseoutDispatched@1" => Ok(Self::ColdCloseoutDispatchedV1),
            other => Err(UnknownEventType(other.to_string())),
        }
    }
}

/// One event in a run's stream.
///
/// Ordering authority is [`RunEvent::sequence`], allocated by the single kernel sequencer, and
/// never `occurred_at`: replay that depended on wall-clock time would stop being deterministic
/// the moment two events shared a timestamp.
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvent {
    pub event_id: String,
    pub run_id: String,
    /// Dense and gapless within a run. A gap means loss, not reordering.
    pub sequence: u64,
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// Observation time, for humans and forensics only.
    pub occurred_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// The event that caused this one. Absent for a run's first event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    /// The subject this event is about across its lifetime, e.g. a Finding ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    pub payload: serde_json::Value,
}

impl RunEvent {
    /// Split `Type@N` into its name and version.
    pub fn typed(&self) -> (&'static str, u32) {
        self.event_type.typed()
    }
}

/// Stable reasons a completed run report can fail its convergence gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureReasonV3 {
    NotConverged,
    AuthorityUnavailable,
    Exhausted,
}

/// Stable reasons the scheduler can suppress a node without dispatching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSuppressionReasonV2 {
    UpstreamMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunNodeOutcomeV2 {
    Completed { output_artifacts: Vec<String> },
    Failed { error: String },
    Suppressed { reason: RunSuppressionReasonV2 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunNodeReportV2 {
    pub node: String,
    pub outcome: RunNodeOutcomeV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissingNodeV2 {
    pub node: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunVerdictV3 {
    Pass,
    Fail { reason: RunFailureReasonV3 },
    Incomplete { missing_nodes: Vec<MissingNodeV2> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunIsolationV4 {
    None,
    Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunExecutionProviderV4 {
    TrustedLocal,
    Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunSandboxModeV4 {
    EphemeralWrite,
}

/// Operator-visible evidence for one resolved Gate Execution Binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunExecutionBindingV4 {
    pub node: String,
    pub provider: RunExecutionProviderV4,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub required_isolation: RunIsolationV4,
    pub provided_isolation: RunIsolationV4,
    pub mode: RunSandboxModeV4,
    pub admitted: bool,
}

impl RunExecutionBindingV4 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("Gate Execution Binding has an empty node".into());
        }
        let provider_usable = match self.provider {
            RunExecutionProviderV4::TrustedLocal => {
                if self.image.is_some() || self.provided_isolation != RunIsolationV4::None {
                    return Err(
                        "trusted_local cannot name an image or claim non-none isolation".into(),
                    );
                }
                true
            }
            RunExecutionProviderV4::Container => {
                let image = self
                    .image
                    .as_deref()
                    .ok_or("container Gate Execution Binding has no pinned image")?;
                if !is_pinned_container_image(image) {
                    return Err(
                        "container Gate Execution Binding image is not digest-pinned".into(),
                    );
                }
                self.provided_isolation == RunIsolationV4::Container
            }
        };
        if self.admitted != (provider_usable && self.provided_isolation >= self.required_isolation)
        {
            return Err(format!(
                "Gate Execution Binding admission for `{}` contradicts provider usability or isolation",
                self.node
            ));
        }
        Ok(())
    }
}

fn is_pinned_container_image(image: &str) -> bool {
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with('-')
        && !name.chars().any(char::is_whitespace)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCacheKindV5 {
    Cargo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCacheMaterializationV5 {
    Reflink,
    Copy,
}

/// Machine-path-free evidence for the exact cache bytes made available to one Gate clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCacheSnapshotV5 {
    pub node: String,
    pub kind: RunCacheKindV5,
    pub source_digest: String,
    pub bytes: u64,
    pub files: u64,
    pub materialization: RunCacheMaterializationV5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCacheFailureReasonV5 {
    GateSetupFailed,
    PolicyUnavailable,
    SourceUnavailable,
    UnsafeContent,
    LimitExceeded,
    CopyLimitExceeded,
    ConcurrentChange,
    MaterializationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCacheFailureV5 {
    pub node: String,
    pub kind: RunCacheKindV5,
    pub reason: RunCacheFailureReasonV5,
}

impl RunCacheFailureV5 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty() {
            return Err("Cache failure has an empty Gate node".into());
        }
        Ok(())
    }
}

impl RunCacheSnapshotV5 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node.trim().is_empty()
            || !crate::is_digest(&self.source_digest)
            || self.files == 0
            || self.bytes > 9_007_199_254_740_991
            || self.files > 9_007_199_254_740_991
        {
            return Err("Cache Snapshot has invalid identity, digest, or bounds".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatusV1 {
    Passed,
    Failed,
    NotRun,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckCompletedPayloadV1 {
    name: String,
    status: CheckStatusV1,
    exit_code: Option<i32>,
    reason: Option<String>,
    program: Option<String>,
    args: Vec<crate::Arg>,
    stdout: Option<String>,
    stderr: Option<String>,
    required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
enum GateOutcomeV1 {
    Passed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GateDecisionPayloadV1 {
    outcome: GateOutcomeV1,
    blocking: Vec<String>,
    reasons: Vec<String>,
    executed: usize,
    required: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationAdvancedPayloadV1 {
    round: u32,
}

/// Validate payloads whose versioned Rust contract is authoritative at the event boundary.
/// Finding event payloads are checked for shape here; `review-store` projects the Report
/// artifacts they reference. Invalid payloads are rejected before append and again during replay.
pub fn validate_event_payload(
    event_type: EventType,
    payload: &serde_json::Value,
) -> Result<(), String> {
    match event_type {
        EventType::TaskReviewResultSelectedV1 => serde_json::from_value::<
            crate::task::campaign_review::TaskReviewResultSelectedV1,
        >(payload.clone())
        .map_err(|e| e.to_string())?
        .validate(),
        EventType::TaskTransitionV5 => {
            serde_json::from_value::<crate::task::event::TaskTransitionV1>(payload.clone())
                .map_err(|e| e.to_string())?
                .validate()
        }
        EventType::CheckCompletedV1 => {
            let value: CheckCompletedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("CheckCompleted@1: {error}"))?;
            if value.name.trim().is_empty()
                || value
                    .stdout
                    .as_deref()
                    .into_iter()
                    .chain(value.stderr.as_deref())
                    .any(|artifact| !crate::is_digest(artifact))
            {
                return Err("CheckCompleted@1 has invalid identity or artifacts".into());
            }
            Ok(())
        }
        EventType::GateDecisionV1 => {
            let value: GateDecisionPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("GateDecision@1: {error}"))?;
            if value.required > value.executed
                || value.blocking.iter().any(|name| name.trim().is_empty())
                || value.reasons.iter().any(|reason| reason.trim().is_empty())
                || (value.outcome == GateOutcomeV1::Passed
                    && (!value.blocking.is_empty() || !value.reasons.is_empty()))
            {
                return Err("GateDecision@1 is internally inconsistent".into());
            }
            Ok(())
        }
        EventType::GateExecutionBoundV1 => {
            let binding = serde_json::from_value::<RunExecutionBindingV4>(payload.clone())
                .map_err(|error| format!("GateExecutionBound@1: {error}"))?;
            binding
                .validate()
                .map_err(|error| format!("GateExecutionBound@1: {error}"))
        }
        EventType::CacheSnapshotMaterializedV1 => {
            let snapshot = serde_json::from_value::<RunCacheSnapshotV5>(payload.clone())
                .map_err(|error| format!("CacheSnapshotMaterialized@1: {error}"))?;
            snapshot
                .validate()
                .map_err(|error| format!("CacheSnapshotMaterialized@1: {error}"))
        }
        EventType::FindingReportedV1 => validate_finding_reported(payload),
        EventType::FindingResolvedV1 => validate_finding_resolved(payload),
        EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1 => {
            let grouping =
                serde_json::from_value::<crate::FindingGroupingEventPayloadV1>(payload.clone())
                    .map_err(|error| format!("{event_type}: {error}"))?;
            grouping
                .validate()
                .map_err(|error| format!("{event_type}: {error}"))
        }
        EventType::GenerationAdvancedV1 => {
            let value: GenerationAdvancedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("GenerationAdvanced@1: {error}"))?;
            if value.round == 0 {
                return Err("GenerationAdvanced@1 has a zero round".into());
            }
            Ok(())
        }
        EventType::CampaignOpenedV1 => {
            let opened = serde_json::from_value::<crate::CampaignOpenedPayloadV1>(payload.clone())
                .map_err(|error| format!("CampaignOpened@1: {error}"))?;
            opened
                .validate()
                .map_err(|error| format!("CampaignOpened@1: {error}"))
        }
        EventType::DemandRecordedV1
        | EventType::DemandWaivedV1
        | EventType::EvidenceAddedV1
        | EventType::EvidenceReuseAdmittedV1
        | EventType::EvidenceSatisfiedV1
        | EventType::ChangeAttestedV1
        | EventType::FixVerifiedV1
        | EventType::FindingResolutionRecordedV1
        | EventType::FindingResolutionChallengedV1
        | EventType::PolicyTimeAdvancedV1 => {
            let recorded =
                serde_json::from_value::<crate::RecordedArtifactPayloadV1>(payload.clone())
                    .map_err(|error| format!("{event_type}: {error}"))?;
            recorded
                .validate()
                .map_err(|error| format!("{event_type}: {error}"))
        }
        EventType::RoundStartedV1 => {
            let started = serde_json::from_value::<crate::RoundStartedPayloadV1>(payload.clone())
                .map_err(|error| format!("RoundStarted@1: {error}"))?;
            started
                .validate()
                .map_err(|error| format!("RoundStarted@1: {error}"))
        }
        EventType::RoundInputSupersededV1 => {
            let superseded =
                serde_json::from_value::<crate::RoundInputSupersededPayloadV1>(payload.clone())
                    .map_err(|error| format!("RoundInputSuperseded@1: {error}"))?;
            superseded
                .validate()
                .map_err(|error| format!("RoundInputSuperseded@1: {error}"))
        }
        EventType::NodeInvocationV1 => {
            let invocation = serde_json::from_value::<NodeInvocationPayloadV1>(payload.clone())
                .map_err(|error| format!("NodeInvocation@1: {error}"))?;
            invocation
                .validate()
                .map_err(|error| format!("NodeInvocation@1: {error}"))
        }
        EventType::NodeOutputReceiptV1 => {
            let receipt = serde_json::from_value::<NodeOutputReceiptPayloadV1>(payload.clone())
                .map_err(|error| format!("NodeOutputReceipt@1: {error}"))?;
            receipt
                .validate()
                .map_err(|error| format!("NodeOutputReceipt@1: {error}"))
        }
        EventType::ProposalAcceptedV1 => {
            let accepted =
                serde_json::from_value::<crate::ProposalAcceptedPayloadV1>(payload.clone())
                    .map_err(|error| format!("ProposalAccepted@1: {error}"))?;
            accepted
                .validate()
                .map_err(|error| format!("ProposalAccepted@1: {error}"))
        }
        EventType::ProposalPreparedV1 => {
            let prepared =
                serde_json::from_value::<crate::ProposalPreparedPayloadV1>(payload.clone())
                    .map_err(|error| format!("ProposalPrepared@1: {error}"))?;
            prepared
                .validate()
                .map_err(|error| format!("ProposalPrepared@1: {error}"))
        }
        EventType::ProposalRefusedV1 => {
            let refused =
                serde_json::from_value::<crate::ProposalRefusedPayloadV1>(payload.clone())
                    .map_err(|error| format!("ProposalRefused@1: {error}"))?;
            refused
                .validate()
                .map_err(|error| format!("ProposalRefused@1: {error}"))
        }
        EventType::RunReportV6 => {
            let report = serde_json::from_value::<RunReportPayloadV6>(payload.clone())
                .map_err(|error| format!("RunReport@6: {error}"))?;
            report
                .validate()
                .map_err(|error| format!("RunReport@6: {error}"))
        }
        EventType::IntegrationPreparedV1 => {
            let value =
                serde_json::from_value::<crate::IntegrationPreparedPayloadV1>(payload.clone())
                    .map_err(|error| format!("IntegrationPrepared@1: {error}"))?;
            value
                .validate()
                .map_err(|error| format!("IntegrationPrepared@1: {error}"))
        }
        EventType::IntegrationConflictV1 => {
            let value =
                serde_json::from_value::<crate::IntegrationConflictPayloadV1>(payload.clone())
                    .map_err(|error| format!("IntegrationConflict@1: {error}"))?;
            value
                .validate()
                .map_err(|error| format!("IntegrationConflict@1: {error}"))
        }
        EventType::IntegrationChecksCompletedV1 => {
            let value = serde_json::from_value::<crate::IntegrationChecksCompletedPayloadV1>(
                payload.clone(),
            )
            .map_err(|error| format!("IntegrationChecksCompleted@1: {error}"))?;
            value
                .validate()
                .map_err(|error| format!("IntegrationChecksCompleted@1: {error}"))
        }
        EventType::IntegrationCommittedV1 => {
            let value =
                serde_json::from_value::<crate::IntegrationCommittedPayloadV1>(payload.clone())
                    .map_err(|error| format!("IntegrationCommitted@1: {error}"))?;
            value
                .validate()
                .map_err(|error| format!("IntegrationCommitted@1: {error}"))
        }
        EventType::SourceCapturedV1 => Ok(()),
        EventType::SliceSetAcceptedV1 => {
            let accepted =
                serde_json::from_value::<crate::SliceSetAcceptedPayloadV1>(payload.clone())
                    .map_err(|error| format!("SliceSetAccepted@1: {error}"))?;
            accepted
                .validate()
                .map_err(|error| format!("SliceSetAccepted@1: {error}"))
        }
        EventType::ShardSetRecordedV1 | EventType::SemanticClosureCheckedV1 => {
            let recorded = serde_json::from_value::<crate::RecordedSetPayloadV1>(payload.clone())
                .map_err(|error| format!("{event_type}: {error}"))?;
            recorded
                .validate()
                .map_err(|error| format!("{event_type}: {error}"))
        }
        EventType::WarmSetSelectedV1 => {
            let selected =
                serde_json::from_value::<crate::WarmSetSelectedPayloadV1>(payload.clone())
                    .map_err(|error| format!("WarmSetSelected@1: {error}"))?;
            selected
                .validate()
                .map_err(|error| format!("WarmSetSelected@1: {error}"))
        }
        EventType::WorkerNotesRecordedV1 => {
            let recorded =
                serde_json::from_value::<crate::WorkerNotesRecordedPayloadV1>(payload.clone())
                    .map_err(|error| format!("WorkerNotesRecorded@1: {error}"))?;
            recorded
                .validate()
                .map_err(|error| format!("WorkerNotesRecorded@1: {error}"))
        }
        EventType::BuildCacheCapturedV1 => {
            let captured =
                serde_json::from_value::<crate::BuildCacheCapturedPayloadV1>(payload.clone())
                    .map_err(|error| format!("BuildCacheCaptured@1: {error}"))?;
            captured
                .validate()
                .map_err(|error| format!("BuildCacheCaptured@1: {error}"))
        }
        EventType::WorkspaceRebasedV1 => {
            let rebased =
                serde_json::from_value::<crate::WorkspaceRebasedPayloadV1>(payload.clone())
                    .map_err(|error| format!("WorkspaceRebased@1: {error}"))?;
            rebased
                .validate()
                .map_err(|error| format!("WorkspaceRebased@1: {error}"))
        }
        EventType::SessionSnapshotPreparedV1 => {
            let prepared =
                serde_json::from_value::<crate::SessionSnapshotPreparedPayloadV1>(payload.clone())
                    .map_err(|error| format!("SessionSnapshotPrepared@1: {error}"))?;
            prepared
                .validate()
                .map_err(|error| format!("SessionSnapshotPrepared@1: {error}"))
        }
        EventType::SessionSnapshotCleanedV1 => {
            let cleaned =
                serde_json::from_value::<crate::SessionSnapshotCleanedPayloadV1>(payload.clone())
                    .map_err(|error| format!("SessionSnapshotCleaned@1: {error}"))?;
            cleaned
                .validate()
                .map_err(|error| format!("SessionSnapshotCleaned@1: {error}"))
        }
        EventType::ColdCloseoutDispatchedV1 => {
            let dispatched =
                serde_json::from_value::<crate::ColdCloseoutDispatchedPayloadV1>(payload.clone())
                    .map_err(|error| format!("ColdCloseoutDispatched@1: {error}"))?;
            dispatched
                .validate()
                .map_err(|error| format!("ColdCloseoutDispatched@1: {error}"))
        }
    }
}

fn validate_finding_reported(payload: &serde_json::Value) -> Result<(), String> {
    let object = payload
        .as_object()
        .ok_or("FindingReported@1 payload is not an object")?;
    let round = object
        .get("round")
        .and_then(serde_json::Value::as_u64)
        .filter(|round| *round > 0)
        .ok_or("FindingReported@1 has an invalid round")?;
    let _ = round;
    let key = object
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or("FindingReported@1 has no key")?;
    if key.trim().is_empty() {
        return Err("FindingReported@1 has an empty key".into());
    }
    let source = object
        .get("source")
        .and_then(serde_json::Value::as_str)
        .filter(|source| !source.is_empty())
        .ok_or("FindingReported@1 has no source")?;
    let _ = source;
    if !object
        .get("report_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(crate::is_digest)
    {
        return Err("FindingReported@1 has no valid report provenance".into());
    }
    Ok(())
}

fn validate_finding_resolved(payload: &serde_json::Value) -> Result<(), String> {
    let object = payload
        .as_object()
        .ok_or("FindingResolved@1 payload is not an object")?;
    let valid_status = matches!(
        object.get("status").and_then(serde_json::Value::as_str),
        Some("open" | "fixed" | "rejected" | "wontfix" | "contested")
    );
    if object
        .get("key")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
        || object
            .get("round")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|round| round == 0)
        || !valid_status
        || object
            .get("note")
            .is_some_and(|note| !note.is_null() && note.as_str().is_none())
    {
        return Err("FindingResolved@1 has invalid identity, round, or status".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortCardinality {
    One,
    Many,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotAffinity {
    /// The artifact is bound to the run's current Subject snapshot.
    SameSubject,
    /// The artifact is deliberately independent of any Subject snapshot.
    Unbound,
    /// The consumer accepts either affinity. Intended for generic infrastructure only.
    Any,
}

/// One complete resolved port entry in an invocation or output receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortArtifactsV1 {
    pub port: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub cardinality: PortCardinality,
    pub optional: bool,
    pub snapshot_affinity: SnapshotAffinity,
    pub artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeInvocationPayloadV1 {
    pub node: String,
    pub inputs: Vec<PortArtifactsV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeOutputReceiptPayloadV1 {
    pub node: String,
    pub outputs: Vec<PortArtifactsV1>,
}

pub fn is_artifact_type(value: &str) -> bool {
    let Some((namespace, versioned_name)) = value.split_once('/') else {
        return false;
    };
    let Some((name, version)) = versioned_name.rsplit_once('@') else {
        return false;
    };
    let namespace_ok = namespace.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'.'))
    });
    let name_ok = name
        .bytes()
        .enumerate()
        .all(|(index, byte)| byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit()));
    let version_ok = version
        .bytes()
        .enumerate()
        .all(|(index, byte)| byte.is_ascii_digit() && (index > 0 || byte != b'0'));
    !namespace.is_empty()
        && !name.is_empty()
        && !version.is_empty()
        && namespace_ok
        && name_ok
        && version_ok
}

fn validate_ports(node: &str, ports: &[PortArtifactsV1]) -> Result<(), String> {
    if node.trim().is_empty() {
        return Err("node id is empty".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for port in ports {
        if port.port.trim().is_empty() || !names.insert(port.port.as_str()) {
            return Err("port names must be non-empty and unique".into());
        }
        if !is_artifact_type(&port.artifact_type) {
            return Err(format!("port `{}` has an invalid artifact type", port.port));
        }
        let ids: std::collections::BTreeSet<&str> =
            port.artifact_ids.iter().map(String::as_str).collect();
        if ids.len() != port.artifact_ids.len() || ids.iter().any(|id| !crate::is_digest(id)) {
            return Err(format!(
                "port `{}` has invalid or duplicate artifacts",
                port.port
            ));
        }
        if port.cardinality == PortCardinality::One && port.artifact_ids.len() > 1 {
            return Err(format!(
                "one-valued port `{}` has multiple artifacts",
                port.port
            ));
        }
        if !port.optional && port.artifact_ids.is_empty() {
            return Err(format!("required port `{}` has no artifact", port.port));
        }
        if port.snapshot_affinity == SnapshotAffinity::SameSubject
            && port.subject_snapshot_id.is_none()
        {
            return Err(format!(
                "same-subject port `{}` has no subject snapshot",
                port.port
            ));
        }
        if port
            .subject_snapshot_id
            .as_deref()
            .is_some_and(|id| !crate::is_digest(id))
        {
            return Err(format!(
                "port `{}` has an invalid subject snapshot",
                port.port
            ));
        }
    }
    Ok(())
}

impl NodeInvocationPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_ports(&self.node, &self.inputs)
    }
}

impl NodeOutputReceiptPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_ports(&self.node, &self.outputs)
    }
}

/// Decode whether a report event closed a campaign round.
///
/// A malformed report is an error, not a closed round.
pub fn run_report_closes_round(event: &RunEvent) -> Result<Option<bool>, serde_json::Error> {
    match event.event_type {
        EventType::RunReportV6 => {
            let report: RunReportPayloadV6 = serde_json::from_value(event.payload.clone())?;
            report
                .validate()
                .map_err(<serde_json::Error as serde::de::Error>::custom)?;
            Ok(Some(!matches!(
                report.verdict,
                RunVerdictV3::Incomplete { .. }
            )))
        }
        _ => Ok(None),
    }
}
