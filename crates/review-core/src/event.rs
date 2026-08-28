//! `RunEvent@1` — the append-only source of truth.

use serde::{Deserialize, Serialize};

/// The complete event vocabulary understood by this kernel build.
///
/// The database representation is intentionally the same `Type@N` string used by the JSON
/// contract. Adding an event or a new payload version is an explicit enum and schema change;
/// arbitrary strings cannot enter a new log through the typed API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "AttemptAdmitted@1")]
    AttemptAdmittedV1,
    #[serde(rename = "AttemptDispatched@1")]
    AttemptDispatchedV1,
    #[serde(rename = "AttemptFailed@1")]
    AttemptFailedV1,
    #[serde(rename = "AttemptFenced@1")]
    AttemptFencedV1,
    #[serde(rename = "AttemptFeedback@1")]
    AttemptFeedbackV1,
    #[serde(rename = "AttemptInput@1")]
    AttemptInputV1,
    #[serde(rename = "AttemptReleased@1")]
    AttemptReleasedV1,
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
    #[serde(rename = "GenerationAdvanced@1")]
    GenerationAdvancedV1,
    #[serde(rename = "NodeInvocation@1")]
    NodeInvocationV1,
    #[serde(rename = "NodeOutputReceipt@1")]
    NodeOutputReceiptV1,
    #[serde(rename = "PolicyTimeAdvanced@1")]
    PolicyTimeAdvancedV1,
    #[serde(rename = "ProviderOperationTransition@1")]
    ProviderOperationTransitionV1,
    #[serde(rename = "RunReport@1")]
    RunReportV1,
    #[serde(rename = "RunReport@2")]
    RunReportV2,
    #[serde(rename = "RunReport@3")]
    RunReportV3,
    #[serde(rename = "RoundInputSuperseded@1")]
    RoundInputSupersededV1,
    #[serde(rename = "RoundStarted@1")]
    RoundStartedV1,
    #[serde(rename = "SourceCaptured@1")]
    SourceCapturedV1,
}

impl EventType {
    pub const ALL: [Self; 34] = [
        Self::AttemptAdmittedV1,
        Self::AttemptDispatchedV1,
        Self::AttemptFailedV1,
        Self::AttemptFencedV1,
        Self::AttemptFeedbackV1,
        Self::AttemptInputV1,
        Self::AttemptReleasedV1,
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
        Self::GenerationAdvancedV1,
        Self::NodeInvocationV1,
        Self::NodeOutputReceiptV1,
        Self::PolicyTimeAdvancedV1,
        Self::ProviderOperationTransitionV1,
        Self::RunReportV1,
        Self::RunReportV2,
        Self::RunReportV3,
        Self::RoundInputSupersededV1,
        Self::RoundStartedV1,
        Self::SourceCapturedV1,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttemptAdmittedV1 => "AttemptAdmitted@1",
            Self::AttemptDispatchedV1 => "AttemptDispatched@1",
            Self::AttemptFailedV1 => "AttemptFailed@1",
            Self::AttemptFencedV1 => "AttemptFenced@1",
            Self::AttemptFeedbackV1 => "AttemptFeedback@1",
            Self::AttemptInputV1 => "AttemptInput@1",
            Self::AttemptReleasedV1 => "AttemptReleased@1",
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
            Self::GenerationAdvancedV1 => "GenerationAdvanced@1",
            Self::NodeInvocationV1 => "NodeInvocation@1",
            Self::NodeOutputReceiptV1 => "NodeOutputReceipt@1",
            Self::PolicyTimeAdvancedV1 => "PolicyTimeAdvanced@1",
            Self::ProviderOperationTransitionV1 => "ProviderOperationTransition@1",
            Self::RunReportV1 => "RunReport@1",
            Self::RunReportV2 => "RunReport@2",
            Self::RunReportV3 => "RunReport@3",
            Self::RoundInputSupersededV1 => "RoundInputSuperseded@1",
            Self::RoundStartedV1 => "RoundStarted@1",
            Self::SourceCapturedV1 => "SourceCaptured@1",
        }
    }

    /// Whether this event is any readable generation of the durable run conclusion.
    pub const fn is_run_report(self) -> bool {
        matches!(
            self,
            Self::RunReportV1 | Self::RunReportV2 | Self::RunReportV3
        )
    }

    /// Whether this run-report generation carries plan and receipt authority.
    pub const fn run_report_requires_receipts(self) -> bool {
        matches!(self, Self::RunReportV2 | Self::RunReportV3)
    }

    pub const fn typed(self) -> (&'static str, u32) {
        match self {
            Self::AttemptAdmittedV1 => ("AttemptAdmitted", 1),
            Self::AttemptDispatchedV1 => ("AttemptDispatched", 1),
            Self::AttemptFailedV1 => ("AttemptFailed", 1),
            Self::AttemptFencedV1 => ("AttemptFenced", 1),
            Self::AttemptFeedbackV1 => ("AttemptFeedback", 1),
            Self::AttemptInputV1 => ("AttemptInput", 1),
            Self::AttemptReleasedV1 => ("AttemptReleased", 1),
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
            Self::GenerationAdvancedV1 => ("GenerationAdvanced", 1),
            Self::NodeInvocationV1 => ("NodeInvocation", 1),
            Self::NodeOutputReceiptV1 => ("NodeOutputReceipt", 1),
            Self::PolicyTimeAdvancedV1 => ("PolicyTimeAdvanced", 1),
            Self::ProviderOperationTransitionV1 => ("ProviderOperationTransition", 1),
            Self::RunReportV1 => ("RunReport", 1),
            Self::RunReportV2 => ("RunReport", 2),
            Self::RunReportV3 => ("RunReport", 3),
            Self::RoundInputSupersededV1 => ("RoundInputSuperseded", 1),
            Self::RoundStartedV1 => ("RoundStarted", 1),
            Self::SourceCapturedV1 => ("SourceCaptured", 1),
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
        write!(f, "unknown review-kernel event type: {}", self.0)
    }
}

impl std::error::Error for UnknownEventType {}

impl std::str::FromStr for EventType {
    type Err = UnknownEventType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "AttemptAdmitted@1" => Ok(Self::AttemptAdmittedV1),
            "AttemptDispatched@1" => Ok(Self::AttemptDispatchedV1),
            "AttemptFailed@1" => Ok(Self::AttemptFailedV1),
            "AttemptFenced@1" => Ok(Self::AttemptFencedV1),
            "AttemptFeedback@1" => Ok(Self::AttemptFeedbackV1),
            "AttemptInput@1" => Ok(Self::AttemptInputV1),
            "AttemptReleased@1" => Ok(Self::AttemptReleasedV1),
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
            "GenerationAdvanced@1" => Ok(Self::GenerationAdvancedV1),
            "NodeInvocation@1" => Ok(Self::NodeInvocationV1),
            "NodeOutputReceipt@1" => Ok(Self::NodeOutputReceiptV1),
            "PolicyTimeAdvanced@1" => Ok(Self::PolicyTimeAdvancedV1),
            "ProviderOperationTransition@1" => Ok(Self::ProviderOperationTransitionV1),
            "RunReport@1" => Ok(Self::RunReportV1),
            "RunReport@2" => Ok(Self::RunReportV2),
            "RunReport@3" => Ok(Self::RunReportV3),
            "RoundInputSuperseded@1" => Ok(Self::RoundInputSupersededV1),
            "RoundStarted@1" => Ok(Self::RoundStartedV1),
            "SourceCaptured@1" => Ok(Self::SourceCapturedV1),
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

/// Stable reasons a completed run can fail its convergence gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureReasonV2 {
    NotConverged,
    Exhausted,
}

/// Stable reasons a completed RunReport@3 can fail its convergence gate.
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
    GateBlocked,
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
pub enum RunVerdictV2 {
    Pass,
    Fail { reason: RunFailureReasonV2 },
    Incomplete { missing_nodes: Vec<MissingNodeV2> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReportPayloadV2 {
    pub outcomes: Vec<RunNodeReportV2>,
    pub blocked_gates: Vec<String>,
    pub verdict: RunVerdictV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunVerdictV3 {
    Pass,
    Fail { reason: RunFailureReasonV3 },
    Incomplete { missing_nodes: Vec<MissingNodeV2> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReportPayloadV3 {
    pub outcomes: Vec<RunNodeReportV2>,
    pub blocked_gates: Vec<String>,
    pub verdict: RunVerdictV3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOperationStateV1 {
    Running,
    WaitingForHuman,
    Resumed,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureClassV1 {
    InvalidOrExpiredAuthentication,
    InteractiveLoginRequired,
    TransientTransportFailure,
    RateLimitOrQuotaExhaustion,
    UnavailableModelOrCapability,
    SmokeTimeout,
    UnknownProviderFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNextActionV1 {
    CompleteInteractiveLogin,
    RefreshAuthentication,
    RetryExplicitly,
    CheckTransport,
    WaitForQuota,
    SelectAvailableModel,
    IncreaseSmokeTimeout,
    InspectProviderFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOperationTransitionPayloadV1 {
    pub operation_id: String,
    pub provider_id: String,
    pub capability_id: String,
    pub node_id: String,
    pub round: u32,
    pub round_epoch: u32,
    pub operation_epoch: u64,
    pub state: ProviderOperationStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<ProviderFailureClassV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_handle: Option<String>,
    pub reserved_tokens: u64,
    pub charged_tokens: u64,
    pub elapsed_ms: u64,
    pub retry_permitted: bool,
    pub circuit_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<ProviderNextActionV1>,
}

impl ProviderOperationTransitionPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        let valid_id = |value: &str| {
            value.len() == 26
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        };
        let valid_provider = !self.provider_id.is_empty()
            && self.provider_id.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
            });
        if !valid_id(&self.operation_id)
            || !valid_provider
            || !crate::is_digest(&self.capability_id)
            || self.node_id.trim().is_empty()
            || self.round == 0
            || self.round_epoch == 0
            || self.operation_epoch == 0
            || self.attempt_id.as_deref().is_some_and(|id| !valid_id(id))
            || self
                .failure_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !crate::is_digest(fingerprint))
            || self
                .continuation_handle
                .as_deref()
                .is_some_and(|handle| !valid_id(handle))
            || [self.reserved_tokens, self.charged_tokens, self.elapsed_ms]
                .into_iter()
                .any(|value| value > 9_007_199_254_740_991)
        {
            return Err(
                "ProviderOperationTransition@1 has invalid identity or numeric bounds".into(),
            );
        }
        let has_attempt = self.attempt.is_some() && self.attempt_id.is_some();
        let has_failure = self.failure_class.is_some() && self.failure_fingerprint.is_some();
        let valid_shape = match self.state {
            ProviderOperationStateV1::Running => {
                has_attempt
                    && self.continuation_handle.is_none()
                    && self.next_action.is_none()
                    && !self.circuit_open
                    && if has_failure {
                        self.retry_permitted
                    } else {
                        self.failure_class.is_none()
                            && self.failure_fingerprint.is_none()
                            && self.charged_tokens == 0
                            && !self.retry_permitted
                    }
            }
            ProviderOperationStateV1::WaitingForHuman => {
                has_attempt
                    && has_failure
                    && self.continuation_handle.is_some()
                    && self.retry_permitted
                    && !self.circuit_open
                    && self.next_action == Some(ProviderNextActionV1::CompleteInteractiveLogin)
            }
            ProviderOperationStateV1::Resumed => {
                has_attempt
                    && self.failure_class.is_none()
                    && self.failure_fingerprint.is_none()
                    && self.reserved_tokens == 0
                    && self.charged_tokens == 0
                    && self.elapsed_ms == 0
                    && !self.retry_permitted
                    && !self.circuit_open
                    && self.next_action.is_none()
            }
            ProviderOperationStateV1::Done => {
                has_attempt
                    && !has_failure
                    && self.failure_class.is_none()
                    && self.failure_fingerprint.is_none()
                    && self.continuation_handle.is_none()
                    && !self.retry_permitted
                    && !self.circuit_open
                    && self.next_action.is_none()
            }
            ProviderOperationStateV1::Failed => {
                has_attempt
                    && has_failure
                    && self.continuation_handle.is_none()
                    && self.next_action.is_some()
                    && (!self.circuit_open || !self.retry_permitted)
            }
        };
        if !valid_shape {
            return Err("ProviderOperationTransition@1 fields contradict its state".into());
        }
        Ok(())
    }

    pub fn validate_after(&self, previous: Option<&Self>) -> Result<(), String> {
        self.validate()?;
        let Some(previous) = previous else {
            if self.state != ProviderOperationStateV1::Running
                || self.operation_epoch != 1
                || self.attempt != Some(1)
                || self.failure_class.is_some()
            {
                return Err(
                    "a provider operation must start running at operation epoch 1, attempt 1"
                        .into(),
                );
            }
            return Ok(());
        };
        if self.operation_id != previous.operation_id
            || self.provider_id != previous.provider_id
            || self.capability_id != previous.capability_id
            || self.node_id != previous.node_id
            || self.round != previous.round
            || self.round_epoch != previous.round_epoch
        {
            return Err("a provider operation transition changes immutable authority".into());
        }
        let valid = match (previous.state, self.state) {
            (ProviderOperationStateV1::Running, ProviderOperationStateV1::Running) => {
                if previous.failure_class.is_none() {
                    self.operation_epoch == previous.operation_epoch
                        && self.attempt == previous.attempt
                        && self.attempt_id == previous.attempt_id
                        && self.failure_class.is_some()
                } else {
                    self.operation_epoch == previous.operation_epoch
                        && self.attempt == previous.attempt.and_then(|value| value.checked_add(1))
                        && self.attempt_id != previous.attempt_id
                        && self.failure_class.is_none()
                }
            }
            (ProviderOperationStateV1::Running, ProviderOperationStateV1::WaitingForHuman)
            | (ProviderOperationStateV1::Running, ProviderOperationStateV1::Done) => {
                self.operation_epoch == previous.operation_epoch
                    && self.attempt == previous.attempt
                    && self.attempt_id == previous.attempt_id
                    && previous.failure_class.is_none()
            }
            (ProviderOperationStateV1::Running, ProviderOperationStateV1::Failed) => {
                self.operation_epoch == previous.operation_epoch
                    && self.attempt == previous.attempt
                    && self.attempt_id == previous.attempt_id
                    && (previous.failure_class.is_none()
                        || (self.failure_class == previous.failure_class
                            && self.failure_fingerprint == previous.failure_fingerprint
                            && self.charged_tokens == 0))
            }
            (ProviderOperationStateV1::WaitingForHuman, ProviderOperationStateV1::Resumed)
            | (ProviderOperationStateV1::Failed, ProviderOperationStateV1::Resumed) => {
                previous.retry_permitted
                    && self.operation_epoch == previous.operation_epoch + 1
                    && self.continuation_handle == previous.continuation_handle
                    && self.attempt == previous.attempt.and_then(|value| value.checked_add(1))
                    && self.attempt_id != previous.attempt_id
            }
            (ProviderOperationStateV1::Resumed, ProviderOperationStateV1::Running) => {
                self.operation_epoch == previous.operation_epoch
                    && self.attempt == previous.attempt
                    && self.attempt_id == previous.attempt_id
                    && self.failure_class.is_none()
            }
            _ => false,
        };
        if !valid {
            return Err("invalid or stale provider operation transition".into());
        }
        Ok(())
    }
}

impl RunReportPayloadV2 {
    pub fn validate(&self) -> Result<(), String> {
        if self.outcomes.is_empty() {
            return Err("a run report must contain at least one node outcome".into());
        }
        let mut nodes = std::collections::BTreeSet::new();
        for outcome in &self.outcomes {
            if outcome.node.trim().is_empty() {
                return Err("a run report contains an empty node id".into());
            }
            if !nodes.insert(outcome.node.as_str()) {
                return Err(format!(
                    "a run report contains duplicate outcome for node `{}`",
                    outcome.node
                ));
            }
            match &outcome.outcome {
                RunNodeOutcomeV2::Completed { output_artifacts } => {
                    let unique: std::collections::BTreeSet<&str> =
                        output_artifacts.iter().map(String::as_str).collect();
                    if unique.len() != output_artifacts.len() {
                        return Err(format!(
                            "completed node `{}` contains duplicate output artifacts",
                            outcome.node
                        ));
                    }
                    if let Some(artifact) = output_artifacts.iter().find(|id| !crate::is_digest(id))
                    {
                        return Err(format!(
                            "completed node `{}` contains invalid artifact id `{artifact}`",
                            outcome.node
                        ));
                    }
                }
                RunNodeOutcomeV2::Failed { error } if error.trim().is_empty() => {
                    return Err(format!("failed node `{}` has an empty error", outcome.node));
                }
                _ => {}
            }
        }
        if let Some(gate) = self
            .blocked_gates
            .iter()
            .find(|gate| gate.trim().is_empty())
        {
            return Err(format!("a run report contains empty blocked gate `{gate}`"));
        }
        let blocked: std::collections::BTreeSet<&str> =
            self.blocked_gates.iter().map(String::as_str).collect();
        if blocked.len() != self.blocked_gates.len() {
            return Err("a run report contains duplicate blocked gates".into());
        }
        if let Some(gate) = blocked.iter().find(|gate| !nodes.contains(**gate)) {
            return Err(format!(
                "blocked gate `{gate}` has no corresponding node outcome"
            ));
        }
        let unresolved: std::collections::BTreeSet<&str> = self
            .outcomes
            .iter()
            .filter_map(|outcome| match &outcome.outcome {
                RunNodeOutcomeV2::Completed { .. } => None,
                RunNodeOutcomeV2::Failed { .. } | RunNodeOutcomeV2::Suppressed { .. } => {
                    Some(outcome.node.as_str())
                }
            })
            .collect();
        let verdict_validation: Result<(), String> = match &self.verdict {
            RunVerdictV2::Pass
            | RunVerdictV2::Fail {
                reason: RunFailureReasonV2::NotConverged,
            } if !unresolved.is_empty() => {
                Err("a terminal pass/fail report cannot contain failed or suppressed nodes".into())
            }
            RunVerdictV2::Pass if !self.blocked_gates.is_empty() => {
                Err("a passing report cannot contain blocked gates".into())
            }
            RunVerdictV2::Incomplete { missing_nodes } => {
                if missing_nodes.is_empty() {
                    return Err("an incomplete report must name at least one missing node".into());
                }
                if let Some(missing) = missing_nodes
                    .iter()
                    .find(|missing| missing.reason.trim().is_empty())
                {
                    return Err(format!(
                        "missing node `{}` has an empty reason",
                        missing.node
                    ));
                }
                let missing: std::collections::BTreeSet<&str> = missing_nodes
                    .iter()
                    .map(|missing| missing.node.as_str())
                    .collect();
                if missing.len() != missing_nodes.len() {
                    return Err("an incomplete report contains duplicate missing nodes".into());
                }
                if missing != unresolved {
                    return Err(
                        "an incomplete report's missing nodes must match failed and suppressed outcomes"
                            .into(),
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        };
        verdict_validation?;
        if self.spent_tokens.unwrap_or(0) > 9_007_199_254_740_991 {
            return Err("spent_tokens exceeds the JSON safe-integer bound".into());
        }
        Ok(())
    }
}

impl RunReportPayloadV3 {
    pub fn validate(&self) -> Result<(), String> {
        // RunReport@3 changes only the durable failure-reason vocabulary. Reuse the frozen
        // structural rules from @2 so the two readers cannot drift on node or receipt shape.
        let verdict = match &self.verdict {
            RunVerdictV3::Pass => RunVerdictV2::Pass,
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::Exhausted,
            } => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::Exhausted,
            },
            RunVerdictV3::Fail { .. } => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::NotConverged,
            },
            RunVerdictV3::Incomplete { missing_nodes } => RunVerdictV2::Incomplete {
                missing_nodes: missing_nodes.clone(),
            },
        };
        RunReportPayloadV2 {
            outcomes: self.outcomes.clone(),
            blocked_gates: self.blocked_gates.clone(),
            verdict,
            spent_tokens: self.spent_tokens,
        }
        .validate()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRunReportV1 {
    outcomes: Vec<LegacyRunNodeV1>,
    blocked_gates: Vec<String>,
    verdict: String,
    spent_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRunNodeV1 {
    node: String,
    status: String,
    detail: serde_json::Value,
}

impl LegacyRunReportV1 {
    fn validate(&self) -> Result<bool, String> {
        if self.outcomes.is_empty() {
            return Err("a frozen RunReport@1 must contain at least one node outcome".into());
        }
        let mut nodes = std::collections::BTreeSet::new();
        let mut unresolved = Vec::new();
        for outcome in &self.outcomes {
            if outcome.node.trim().is_empty() || !nodes.insert(outcome.node.as_str()) {
                return Err("a frozen RunReport@1 contains an empty or duplicate node".into());
            }
            match outcome.status.as_str() {
                "completed" if outcome.detail.is_object() => {}
                "failed" if outcome.detail.as_str().is_some_and(|text| !text.is_empty()) => {
                    unresolved.push((
                        outcome.node.clone(),
                        outcome.detail.as_str().unwrap_or_default().to_string(),
                    ));
                }
                "suppressed"
                    if matches!(
                        outcome.detail.as_str(),
                        Some("GateBlocked" | "UpstreamMissing")
                    ) =>
                {
                    unresolved.push((
                        outcome.node.clone(),
                        outcome.detail.as_str().unwrap_or_default().to_string(),
                    ));
                }
                status => {
                    return Err(format!(
                        "invalid frozen RunReport@1 outcome `{status}` for node `{}`",
                        outcome.node
                    ));
                }
            }
        }
        let blocked: std::collections::BTreeSet<&str> =
            self.blocked_gates.iter().map(String::as_str).collect();
        if blocked.len() != self.blocked_gates.len()
            || blocked
                .iter()
                .any(|gate| gate.is_empty() || !nodes.contains(gate))
        {
            return Err("a frozen RunReport@1 contains an invalid blocked gate".into());
        }
        if self.spent_tokens.unwrap_or(0) > 9_007_199_254_740_991 {
            return Err("frozen RunReport@1 spent_tokens exceeds the safe-integer bound".into());
        }
        match self.verdict.as_str() {
            "Pass" if unresolved.is_empty() && blocked.is_empty() => Ok(true),
            "Fail(NotConverged)" | "Fail(Exhausted)" if unresolved.is_empty() => Ok(true),
            verdict if verdict == frozen_incomplete_verdict(&unresolved) => Ok(false),
            verdict => Err(format!(
                "frozen RunReport@1 verdict `{verdict}` contradicts its outcomes"
            )),
        }
    }
}

fn frozen_incomplete_verdict(missing: &[(String, String)]) -> String {
    let entries = missing
        .iter()
        .map(|(node, reason)| {
            format!(
                "({}, {})",
                serde_json::to_string(node).expect("String is JSON"),
                serde_json::to_string(reason).expect("String is JSON")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Incomplete {{ missing: [{entries}] }}")
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptDispatchedPayloadV1 {
    pub reserved: Option<u64>,
    pub prior_findings: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptInputPayloadV1 {
    pub refusal_history_id: String,
}

/// Exact feedback produced by one retryable terminal attempt. Unlike the terminal diagnostic,
/// this artifact is prompt-input authority and may be carried into a later `AttemptInput@1`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFeedbackPayloadV1 {
    pub refusal_history_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptAdmittedPayloadV1 {
    pub selection: String,
    pub cost_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_artifact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFailedPayloadV1 {
    pub error: String,
    pub charged: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFencedPayloadV1 {
    pub reason: String,
    pub charged: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptReleasedPayloadV1 {
    pub error: String,
    pub released: Option<u64>,
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
/// Legacy finding events retain their frozen projection validator in `review-store`; new typed
/// node and report payloads are rejected before append and again during replay.
pub fn validate_event_payload(
    event_type: EventType,
    payload: &serde_json::Value,
) -> Result<(), String> {
    match event_type {
        EventType::AttemptDispatchedV1 => {
            let value: AttemptDispatchedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptDispatched@1: {error}"))?;
            if value
                .prior_findings
                .as_deref()
                .is_some_and(|artifact| !crate::is_digest(artifact))
            {
                return Err("AttemptDispatched@1 has an invalid prior Finding Set ID".into());
            }
            Ok(())
        }
        EventType::AttemptInputV1 => {
            let value: AttemptInputPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptInput@1: {error}"))?;
            if !crate::is_digest(&value.refusal_history_id) {
                return Err("AttemptInput@1 has an invalid refusal history ID".into());
            }
            Ok(())
        }
        EventType::AttemptFeedbackV1 => {
            let value: AttemptFeedbackPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptFeedback@1: {error}"))?;
            if !crate::is_digest(&value.refusal_history_id) {
                return Err("AttemptFeedback@1 has an invalid refusal history ID".into());
            }
            Ok(())
        }
        EventType::AttemptAdmittedV1 => {
            let value: AttemptAdmittedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptAdmitted@1: {error}"))?;
            if !matches!(value.selection.as_str(), "selected" | "quarantined")
                || value
                    .result_artifact
                    .as_deref()
                    .is_some_and(|artifact| !crate::is_digest(artifact))
                || value
                    .provenance_artifact
                    .as_deref()
                    .is_some_and(|artifact| !crate::is_digest(artifact))
            {
                return Err("AttemptAdmitted@1 has invalid selection or artifact IDs".into());
            }
            Ok(())
        }
        EventType::AttemptFailedV1 => {
            let value: AttemptFailedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptFailed@1: {error}"))?;
            if value.error.trim().is_empty() {
                return Err("AttemptFailed@1 has an empty error".into());
            }
            Ok(())
        }
        EventType::AttemptFencedV1 => {
            let value: AttemptFencedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptFenced@1: {error}"))?;
            if value.reason.trim().is_empty() {
                return Err("AttemptFenced@1 has an empty reason".into());
            }
            Ok(())
        }
        EventType::AttemptReleasedV1 => {
            let value: AttemptReleasedPayloadV1 = serde_json::from_value(payload.clone())
                .map_err(|error| format!("AttemptReleased@1: {error}"))?;
            if value.error.trim().is_empty() {
                return Err("AttemptReleased@1 has an empty error".into());
            }
            Ok(())
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
        EventType::ProviderOperationTransitionV1 => {
            let transition =
                serde_json::from_value::<ProviderOperationTransitionPayloadV1>(payload.clone())
                    .map_err(|error| format!("ProviderOperationTransition@1: {error}"))?;
            transition
                .validate()
                .map_err(|error| format!("ProviderOperationTransition@1: {error}"))
        }
        EventType::RunReportV1 => {
            let report = serde_json::from_value::<LegacyRunReportV1>(payload.clone())
                .map_err(|error| format!("RunReport@1: {error}"))?;
            report
                .validate()
                .map(|_| ())
                .map_err(|error| format!("RunReport@1: {error}"))
        }
        EventType::RunReportV2 => {
            let report = serde_json::from_value::<RunReportPayloadV2>(payload.clone())
                .map_err(|error| format!("RunReport@2: {error}"))?;
            report
                .validate()
                .map_err(|error| format!("RunReport@2: {error}"))
        }
        EventType::RunReportV3 => {
            let report = serde_json::from_value::<RunReportPayloadV3>(payload.clone())
                .map_err(|error| format!("RunReport@3: {error}"))?;
            report
                .validate()
                .map_err(|error| format!("RunReport@3: {error}"))
        }
        EventType::SourceCapturedV1 => Ok(()),
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
    if let Some(report_id) = object.get("report_id").and_then(serde_json::Value::as_str) {
        if !crate::is_digest(report_id) {
            return Err("FindingReported@1 has invalid live report provenance".into());
        }
    } else {
        if object.get("imported").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err("FindingReported@1 is neither a live report nor a legacy import".into());
        }
        if !matches!(
            object.get("severity").and_then(serde_json::Value::as_str),
            Some("minor" | "major" | "blocker")
        ) || object
            .get("file")
            .and_then(serde_json::Value::as_str)
            .is_none()
            || object
                .get("title")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            || object
                .get("body")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            || object
                .get("line")
                .is_some_and(|line| !line.is_null() && line.as_i64().is_none_or(|line| line <= 0))
            || object.get("confidence").is_some_and(|confidence| {
                !confidence.is_null()
                    && confidence
                        .as_f64()
                        .is_none_or(|confidence| !(0.0..=1.0).contains(&confidence))
            })
        {
            return Err("FindingReported@1 has an invalid legacy projection".into());
        }
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
/// The `RunReport@1` arm is permanent: append-only logs may contain it forever. Its debug-string
/// prefix is isolated here and can no longer leak into new writes. A malformed report is an
/// error, not a closed round.
pub fn run_report_closes_round(event: &RunEvent) -> Result<Option<bool>, serde_json::Error> {
    match event.event_type {
        EventType::RunReportV1 => {
            let report: LegacyRunReportV1 = serde_json::from_value(event.payload.clone())?;
            report
                .validate()
                .map(Some)
                .map_err(<serde_json::Error as serde::de::Error>::custom)
        }
        EventType::RunReportV2 => {
            let report: RunReportPayloadV2 = serde_json::from_value(event.payload.clone())?;
            report
                .validate()
                .map_err(<serde_json::Error as serde::de::Error>::custom)?;
            Ok(Some(!matches!(
                report.verdict,
                RunVerdictV2::Incomplete { .. }
            )))
        }
        EventType::RunReportV3 => {
            let report: RunReportPayloadV3 = serde_json::from_value(event.payload.clone())?;
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
