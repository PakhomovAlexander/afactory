//! `RunEvent@3` — the append-only source of truth.

use serde::{Deserialize, Serialize};

/// The complete event vocabulary understood by this kernel build.
///
/// The database representation is intentionally the same `Type@N` string used by the JSON
/// contract. Adding an event or a new payload version is an explicit enum and schema change;
/// arbitrary strings cannot enter a new log through the typed API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "TaskTransition@6")]
    TaskTransitionV6,
    #[serde(rename = "TaskExecutionStarted@1")]
    TaskExecutionStartedV1,
    #[serde(rename = "TaskGateRetried@1")]
    TaskGateRetriedV1,
    #[serde(rename = "TaskProviderEvent@1")]
    TaskProviderEventV1,
    #[serde(rename = "CheckCompleted@1")]
    CheckCompletedV1,
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
    #[serde(rename = "GenerationAdvanced@1")]
    GenerationAdvancedV1,
    #[serde(rename = "PolicyTimeAdvanced@1")]
    PolicyTimeAdvancedV1,
    #[serde(rename = "SourceCaptured@1")]
    SourceCapturedV1,
    #[serde(rename = "ScenarioTemplatePinned@1")]
    ScenarioTemplatePinnedV1,
    #[serde(rename = "ProjectRegistered@1")]
    ProjectRegisteredV1,
    #[serde(rename = "WorkspaceRegistered@1")]
    WorkspaceRegisteredV1,
    #[serde(rename = "WorkspaceRemoved@1")]
    WorkspaceRemovedV1,
    #[serde(rename = "WorkspaceDelivered@1")]
    WorkspaceDeliveredV1,
    #[serde(rename = "SurveyRecorded@1")]
    SurveyRecordedV1,
    #[serde(rename = "SurveyFailed@1")]
    SurveyFailedV1,
    #[serde(rename = "SurveyCollected@1")]
    SurveyCollectedV1,
}

impl EventType {
    pub const ALL: [Self; 29] = [
        Self::TaskTransitionV6,
        Self::TaskExecutionStartedV1,
        Self::TaskGateRetriedV1,
        Self::TaskProviderEventV1,
        Self::CheckCompletedV1,
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
        Self::GenerationAdvancedV1,
        Self::PolicyTimeAdvancedV1,
        Self::SourceCapturedV1,
        Self::ScenarioTemplatePinnedV1,
        Self::ProjectRegisteredV1,
        Self::WorkspaceRegisteredV1,
        Self::WorkspaceRemovedV1,
        Self::WorkspaceDeliveredV1,
        Self::SurveyRecordedV1,
        Self::SurveyFailedV1,
        Self::SurveyCollectedV1,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskTransitionV6 => "TaskTransition@6",
            Self::TaskExecutionStartedV1 => "TaskExecutionStarted@1",
            Self::TaskGateRetriedV1 => "TaskGateRetried@1",
            Self::TaskProviderEventV1 => "TaskProviderEvent@1",
            Self::CheckCompletedV1 => "CheckCompleted@1",
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
            Self::GenerationAdvancedV1 => "GenerationAdvanced@1",
            Self::PolicyTimeAdvancedV1 => "PolicyTimeAdvanced@1",
            Self::SourceCapturedV1 => "SourceCaptured@1",
            Self::ScenarioTemplatePinnedV1 => "ScenarioTemplatePinned@1",
            Self::ProjectRegisteredV1 => "ProjectRegistered@1",
            Self::WorkspaceRegisteredV1 => "WorkspaceRegistered@1",
            Self::WorkspaceRemovedV1 => "WorkspaceRemoved@1",
            Self::WorkspaceDeliveredV1 => "WorkspaceDelivered@1",
            Self::SurveyRecordedV1 => "SurveyRecorded@1",
            Self::SurveyFailedV1 => "SurveyFailed@1",
            Self::SurveyCollectedV1 => "SurveyCollected@1",
        }
    }

    pub const fn typed(self) -> (&'static str, u32) {
        match self {
            Self::TaskTransitionV6 => ("TaskTransition", 6),
            Self::TaskExecutionStartedV1 => ("TaskExecutionStarted", 1),
            Self::TaskGateRetriedV1 => ("TaskGateRetried", 1),
            Self::TaskProviderEventV1 => ("TaskProviderEvent", 1),
            Self::CheckCompletedV1 => ("CheckCompleted", 1),
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
            Self::GenerationAdvancedV1 => ("GenerationAdvanced", 1),
            Self::PolicyTimeAdvancedV1 => ("PolicyTimeAdvanced", 1),
            Self::SourceCapturedV1 => ("SourceCaptured", 1),
            Self::ScenarioTemplatePinnedV1 => ("ScenarioTemplatePinned", 1),
            Self::ProjectRegisteredV1 => ("ProjectRegistered", 1),
            Self::WorkspaceRegisteredV1 => ("WorkspaceRegistered", 1),
            Self::WorkspaceRemovedV1 => ("WorkspaceRemoved", 1),
            Self::WorkspaceDeliveredV1 => ("WorkspaceDelivered", 1),
            Self::SurveyRecordedV1 => ("SurveyRecorded", 1),
            Self::SurveyFailedV1 => ("SurveyFailed", 1),
            Self::SurveyCollectedV1 => ("SurveyCollected", 1),
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

/// What an event log another af release wrote says when this release reads it: its readers
/// recognise such a log by this phrase in the refusal.
pub const ANOTHER_RELEASE: &str = "this log was written by another af release";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEventType(pub String);

impl std::fmt::Display for UnknownEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown review-kernel event type: {}; {ANOTHER_RELEASE}; start a new Campaign or Task",
            self.0
        )
    }
}

impl std::error::Error for UnknownEventType {}

impl std::str::FromStr for EventType {
    type Err = UnknownEventType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "TaskTransition@6" => Ok(Self::TaskTransitionV6),
            "TaskExecutionStarted@1" => Ok(Self::TaskExecutionStartedV1),
            "TaskGateRetried@1" => Ok(Self::TaskGateRetriedV1),
            "TaskProviderEvent@1" => Ok(Self::TaskProviderEventV1),
            "CheckCompleted@1" => Ok(Self::CheckCompletedV1),
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
            "GenerationAdvanced@1" => Ok(Self::GenerationAdvancedV1),
            "PolicyTimeAdvanced@1" => Ok(Self::PolicyTimeAdvancedV1),
            "SourceCaptured@1" => Ok(Self::SourceCapturedV1),
            "ScenarioTemplatePinned@1" => Ok(Self::ScenarioTemplatePinnedV1),
            "ProjectRegistered@1" => Ok(Self::ProjectRegisteredV1),
            "WorkspaceRegistered@1" => Ok(Self::WorkspaceRegisteredV1),
            "WorkspaceRemoved@1" => Ok(Self::WorkspaceRemovedV1),
            "WorkspaceDelivered@1" => Ok(Self::WorkspaceDeliveredV1),
            "SurveyRecorded@1" => Ok(Self::SurveyRecordedV1),
            "SurveyFailed@1" => Ok(Self::SurveyFailedV1),
            "SurveyCollected@1" => Ok(Self::SurveyCollectedV1),
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
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "schema",
    rename = "review.kernel/RunEvent@3",
    deny_unknown_fields
)]
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
#[derive(Deserialize)]
#[serde(remote = "RunEvent", deny_unknown_fields)]
struct RunEventFields {
    pub event_id: String,
    pub run_id: String,
    pub sequence: u64,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub occurred_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    pub payload: serde_json::Value,
}

impl<'de> Deserialize<'de> for RunEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "schema")]
        enum Wire {
            #[serde(rename = "review.kernel/RunEvent@3")]
            Current(#[serde(with = "RunEventFields")] RunEvent),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Current(value) => Ok(value),
        }
    }
}

impl RunEvent {
    /// Split `Type@N` into its name and version.
    pub fn typed(&self) -> (&'static str, u32) {
        self.event_type.typed()
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
#[serde(deny_unknown_fields)]
struct GenerationAdvancedPayloadV1 {
    round: u32,
}

/// Validate payloads whose versioned Rust contract is authoritative at the event boundary.
/// Finding event payloads are checked for shape here; `af-store` projects the Report
/// artifacts they reference. Invalid payloads are rejected before append and again during replay.
pub fn validate_event_payload(
    event_type: EventType,
    payload: &serde_json::Value,
) -> Result<(), String> {
    match event_type {
        EventType::TaskTransitionV6 => {
            serde_json::from_value::<crate::task::event::TaskTransitionV1>(payload.clone())
                .map_err(|e| e.to_string())?
                .validate()
        }
        EventType::TaskExecutionStartedV1 => serde_json::from_value::<
            crate::task::execution_started::TaskExecutionStartedV1,
        >(payload.clone())
        .map_err(|error| format!("TaskExecutionStarted@1: {error}"))?
        .validate()
        .map_err(|error| format!("TaskExecutionStarted@1: {error}")),
        EventType::TaskGateRetriedV1 => {
            serde_json::from_value::<crate::task::gate_retry::TaskGateRetriedV1>(payload.clone())
                .map_err(|error| format!("TaskGateRetried@1: {error}"))?
                .validate()
                .map_err(|error| format!("TaskGateRetried@1: {error}"))
        }
        EventType::TaskProviderEventV1 => serde_json::from_value::<
            crate::task::provider_event::TaskProviderEventV1,
        >(payload.clone())
        .map_err(|error| format!("TaskProviderEvent@1: {error}"))?
        .validate()
        .map_err(|error| format!("TaskProviderEvent@1: {error}")),
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
        EventType::ScenarioTemplatePinnedV1
        | EventType::DemandRecordedV1
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
        EventType::SourceCapturedV1 => Ok(()),
        EventType::ProjectRegisteredV1 => {
            serde_json::from_value::<crate::ProjectRegisteredPayloadV1>(payload.clone())
                .map_err(|error| format!("ProjectRegistered@1: {error}"))?
                .validate()
        }
        EventType::WorkspaceRegisteredV1 => {
            serde_json::from_value::<crate::WorkspaceRegisteredPayloadV1>(payload.clone())
                .map_err(|error| format!("WorkspaceRegistered@1: {error}"))?
                .validate()
        }
        EventType::WorkspaceRemovedV1 => {
            serde_json::from_value::<crate::WorkspaceRemovedPayloadV1>(payload.clone())
                .map_err(|error| format!("WorkspaceRemoved@1: {error}"))?
                .validate()
        }
        EventType::WorkspaceDeliveredV1 => {
            serde_json::from_value::<crate::WorkspaceDeliveredPayloadV1>(payload.clone())
                .map_err(|error| format!("WorkspaceDelivered@1: {error}"))?
                .validate()
        }
        EventType::SurveyRecordedV1 => {
            serde_json::from_value::<crate::SurveyRecordedPayloadV1>(payload.clone())
                .map_err(|error| format!("SurveyRecorded@1: {error}"))?
                .validate()
        }
        EventType::SurveyFailedV1 => {
            serde_json::from_value::<crate::SurveyFailedPayloadV1>(payload.clone())
                .map_err(|error| format!("SurveyFailed@1: {error}"))?
                .validate()
        }
        EventType::SurveyCollectedV1 => {
            serde_json::from_value::<crate::project::SurveyCollectedPayloadV1>(payload.clone())
                .map_err(|error| format!("SurveyCollected@1: {error}"))?
                .validate()
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
