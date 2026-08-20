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
    #[serde(rename = "AttemptReleased@1")]
    AttemptReleasedV1,
    #[serde(rename = "CheckCompleted@1")]
    CheckCompletedV1,
    #[serde(rename = "FindingReported@1")]
    FindingReportedV1,
    #[serde(rename = "FindingResolved@1")]
    FindingResolvedV1,
    #[serde(rename = "GateDecision@1")]
    GateDecisionV1,
    #[serde(rename = "GenerationAdvanced@1")]
    GenerationAdvancedV1,
    #[serde(rename = "NodeInvocation@1")]
    NodeInvocationV1,
    #[serde(rename = "NodeOutputReceipt@1")]
    NodeOutputReceiptV1,
    #[serde(rename = "RunReport@1")]
    RunReportV1,
    #[serde(rename = "RunReport@2")]
    RunReportV2,
    #[serde(rename = "SourceCaptured@1")]
    SourceCapturedV1,
}

impl EventType {
    pub const ALL: [Self; 15] = [
        Self::AttemptAdmittedV1,
        Self::AttemptDispatchedV1,
        Self::AttemptFailedV1,
        Self::AttemptFencedV1,
        Self::AttemptReleasedV1,
        Self::CheckCompletedV1,
        Self::FindingReportedV1,
        Self::FindingResolvedV1,
        Self::GateDecisionV1,
        Self::GenerationAdvancedV1,
        Self::NodeInvocationV1,
        Self::NodeOutputReceiptV1,
        Self::RunReportV1,
        Self::RunReportV2,
        Self::SourceCapturedV1,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttemptAdmittedV1 => "AttemptAdmitted@1",
            Self::AttemptDispatchedV1 => "AttemptDispatched@1",
            Self::AttemptFailedV1 => "AttemptFailed@1",
            Self::AttemptFencedV1 => "AttemptFenced@1",
            Self::AttemptReleasedV1 => "AttemptReleased@1",
            Self::CheckCompletedV1 => "CheckCompleted@1",
            Self::FindingReportedV1 => "FindingReported@1",
            Self::FindingResolvedV1 => "FindingResolved@1",
            Self::GateDecisionV1 => "GateDecision@1",
            Self::GenerationAdvancedV1 => "GenerationAdvanced@1",
            Self::NodeInvocationV1 => "NodeInvocation@1",
            Self::NodeOutputReceiptV1 => "NodeOutputReceipt@1",
            Self::RunReportV1 => "RunReport@1",
            Self::RunReportV2 => "RunReport@2",
            Self::SourceCapturedV1 => "SourceCaptured@1",
        }
    }

    pub const fn typed(self) -> (&'static str, u32) {
        match self {
            Self::AttemptAdmittedV1 => ("AttemptAdmitted", 1),
            Self::AttemptDispatchedV1 => ("AttemptDispatched", 1),
            Self::AttemptFailedV1 => ("AttemptFailed", 1),
            Self::AttemptFencedV1 => ("AttemptFenced", 1),
            Self::AttemptReleasedV1 => ("AttemptReleased", 1),
            Self::CheckCompletedV1 => ("CheckCompleted", 1),
            Self::FindingReportedV1 => ("FindingReported", 1),
            Self::FindingResolvedV1 => ("FindingResolved", 1),
            Self::GateDecisionV1 => ("GateDecision", 1),
            Self::GenerationAdvancedV1 => ("GenerationAdvanced", 1),
            Self::NodeInvocationV1 => ("NodeInvocation", 1),
            Self::NodeOutputReceiptV1 => ("NodeOutputReceipt", 1),
            Self::RunReportV1 => ("RunReport", 1),
            Self::RunReportV2 => ("RunReport", 2),
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
            "AttemptReleased@1" => Ok(Self::AttemptReleasedV1),
            "CheckCompleted@1" => Ok(Self::CheckCompletedV1),
            "FindingReported@1" => Ok(Self::FindingReportedV1),
            "FindingResolved@1" => Ok(Self::FindingResolvedV1),
            "GateDecision@1" => Ok(Self::GateDecisionV1),
            "GenerationAdvanced@1" => Ok(Self::GenerationAdvancedV1),
            "NodeInvocation@1" => Ok(Self::NodeInvocationV1),
            "NodeOutputReceipt@1" => Ok(Self::NodeOutputReceiptV1),
            "RunReport@1" => Ok(Self::RunReportV1),
            "RunReport@2" => Ok(Self::RunReportV2),
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

/// Decode whether a report event closed a campaign round.
///
/// The `RunReport@1` arm is permanent: append-only logs may contain it forever. Its debug-string
/// prefix is isolated here and can no longer leak into new writes. A malformed report is an
/// error, not a closed round.
pub fn run_report_closes_round(event: &RunEvent) -> Result<Option<bool>, serde_json::Error> {
    match event.event_type {
        EventType::RunReportV1 => {
            #[derive(Deserialize)]
            struct LegacyRunReport {
                verdict: String,
            }
            let report: LegacyRunReport = serde_json::from_value(event.payload.clone())?;
            Ok(Some(!report.verdict.starts_with("Incomplete")))
        }
        EventType::RunReportV2 => {
            let report: RunReportPayloadV2 = serde_json::from_value(event.payload.clone())?;
            Ok(Some(!matches!(
                report.verdict,
                RunVerdictV2::Incomplete { .. }
            )))
        }
        _ => Ok(None),
    }
}
