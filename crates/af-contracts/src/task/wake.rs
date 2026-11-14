//! Host-neutral wake foundations. These records describe facts, never executable authority.
use super::state::{TaskCauseV1, TaskOutcomeV1, TaskStatePhaseV1};
use super::{TaskWaitingReasonV1, is_name, require, safe_number};
use crate::is_digest;
use serde::{Deserialize, Serialize};

pub const WAKE_SUBSCRIPTION_V1: &str = "af/TaskWakeSubscription@1";
pub const WAKE_OBSERVATION_V1: &str = "af/TaskWakeObservation@1";
pub const WAKE_ENVELOPE_V1: &str = "af/TaskWakeEnvelope@1";
pub const WAKE_RECEIPT_V1: &str = "af/TaskWakeReceipt@1";
pub const MAX_WAKE_BYTES: usize = 16384;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeTaskV1 {
    pub store_incarnation: String,
    pub task_id: String,
    pub task_run_id: String,
    pub opening_event_id: String,
}
impl WakeTaskV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.store_incarnation)
                && is_name(&self.task_id)
                && self
                    .task_run_id
                    .strip_prefix("task:")
                    .is_some_and(is_digest)
                && event_id(&self.opening_event_id),
            "Invalid wake Task identity",
        )
    }
}
fn event_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn positive(value: u64) -> bool {
    value > 0 && safe_number(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeSubscriptionV1 {
    pub schema: String,
    pub subscription_id: String,
    pub registration_key: String,
    pub configuration_digest: String,
    pub task: WakeTaskV1,
    /// Opaque reference only. It supplies neither routing nor inference authority.
    pub recipient_ref: String,
    pub registered_unix_ms: u64,
    pub expires_unix_ms: u64,
    pub max_observations: u64,
}
impl WakeSubscriptionV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.task.validate()?;
        require(
            self.schema == WAKE_SUBSCRIPTION_V1
                && [
                    &self.subscription_id,
                    &self.registration_key,
                    &self.configuration_digest,
                    &self.recipient_ref,
                ]
                .into_iter()
                .all(|s| is_digest(s))
                && positive(self.registered_unix_ms)
                && positive(self.expires_unix_ms)
                && self.expires_unix_ms > self.registered_unix_ms
                && (1..=4096).contains(&self.max_observations),
            "Invalid bounded wake subscription",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeKindV1 {
    AttentionRequired,
    ExecutionStopped,
    Completed,
}

/// Structured semantics exclude the CLI's next-action command and diagnostic prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeStateV1 {
    pub phase: TaskStatePhaseV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub outcome: Option<TaskOutcomeV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cause: Option<TaskCauseV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub waiting_reason: Option<TaskWaitingReasonV1>,
}
impl WakeStateV1 {
    pub fn validate(&self) -> Result<(), String> {
        use TaskStatePhaseV1::*;
        require(
            self.waiting_reason.is_none() || self.phase == Waiting,
            "Waiting reason outside waiting",
        )?;
        match self.phase {
            Planned | Running | Waiting => require(
                self.outcome.is_none() && self.cause.is_none(),
                "Active state has an outcome",
            ),
            Abandoned => require(
                self.outcome == Some(TaskOutcomeV1::Cancelled)
                    && matches!(
                        self.cause,
                        Some(TaskCauseV1::WriterLost | TaskCauseV1::HostCancelled)
                    ),
                "Invalid stopped state",
            ),
            Finished => require(
                match (self.outcome, self.cause) {
                    (Some(TaskOutcomeV1::Verified), None) => true,
                    (Some(outcome), Some(cause)) => cause.outcome() == Some(outcome),
                    _ => false,
                },
                "Invalid completion state",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeBoundaryV1 {
    pub sequence: u64,
    pub event_id: String,
}
impl WakeBoundaryV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            safe_number(self.sequence) && event_id(&self.event_id),
            "Invalid wake boundary",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WakeEvidenceV1 {
    History {},
    LeaseExpired { epoch: u64, deadline_unix_ms: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeObservationV1 {
    pub schema: String,
    pub task: WakeTaskV1,
    pub observation_id: String,
    pub causal: WakeBoundaryV1,
    pub checked_prefix: WakeBoundaryV1,
    pub revision_id: String,
    pub episode: u64,
    pub kind: WakeKindV1,
    pub state: WakeStateV1,
    pub evidence: WakeEvidenceV1,
    pub observed_unix_ms: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub result_id: Option<String>,
}
impl WakeObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.task.validate()?;
        self.causal.validate()?;
        self.checked_prefix.validate()?;
        self.state.validate()?;
        require(
            self.schema == WAKE_OBSERVATION_V1
                && is_digest(&self.observation_id)
                && is_digest(&self.revision_id)
                && positive(self.episode)
                && positive(self.observed_unix_ms)
                && self.causal.sequence <= self.checked_prefix.sequence,
            "Invalid wake observation identity",
        )?;
        require(
            match self.kind {
                WakeKindV1::AttentionRequired => {
                    self.state.phase == TaskStatePhaseV1::Waiting && self.result_id.is_none()
                }
                WakeKindV1::ExecutionStopped => {
                    self.state.phase == TaskStatePhaseV1::Abandoned && self.result_id.is_none()
                }
                WakeKindV1::Completed => {
                    self.state.phase == TaskStatePhaseV1::Finished
                        && self.result_id.as_deref().is_some_and(is_digest)
                }
            },
            "Wake kind disagrees with semantic state",
        )?;
        if let WakeEvidenceV1::LeaseExpired {
            epoch,
            deadline_unix_ms,
        } = self.evidence
        {
            require(
                positive(epoch)
                    && positive(deadline_unix_ms)
                    && self.observed_unix_ms >= deadline_unix_ms
                    && self.kind != WakeKindV1::Completed,
                "Invalid lease observation",
            )?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeEnvelopeV1 {
    pub schema: String,
    pub subscription_id: String,
    pub delivery_id: String,
    pub payload_digest: String,
    pub observation: WakeObservationV1,
    pub expires_unix_ms: u64,
}
impl WakeEnvelopeV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.observation.validate()?;
        require(
            self.schema == WAKE_ENVELOPE_V1
                && [
                    &self.subscription_id,
                    &self.delivery_id,
                    &self.payload_digest,
                ]
                .into_iter()
                .all(|s| is_digest(s))
                && positive(self.expires_unix_ms),
            "Invalid wake envelope",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WakeReceiptStageV1 {
    OutboxExtracted {},
    InboxAccepted {
        inbox_ref: String,
    },
    NativeAdmissionIntent {
        intent_id: String,
    },
    NativeAdmitted {
        intent_id: String,
        native_ref: String,
    },
    AdmissionUnknown {
        intent_id: String,
    },
    TurnStarted {
        native_ref: String,
        turn_ref: String,
    },
    ReportObserved {
        turn_ref: String,
        report_ref: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeReceiptV1 {
    pub schema: String,
    pub receipt_id: String,
    pub delivery_id: String,
    pub payload_digest: String,
    pub attempt_id: String,
    pub observed_unix_ms: u64,
    pub stage: WakeReceiptStageV1,
}
impl WakeReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        use WakeReceiptStageV1::*;
        let refs: Vec<&str> = match &self.stage {
            OutboxExtracted {} => vec![],
            InboxAccepted { inbox_ref } => vec![inbox_ref],
            NativeAdmissionIntent { intent_id } | AdmissionUnknown { intent_id } => vec![intent_id],
            NativeAdmitted {
                intent_id,
                native_ref,
            } => vec![intent_id, native_ref],
            TurnStarted {
                native_ref,
                turn_ref,
            } => vec![native_ref, turn_ref],
            ReportObserved {
                turn_ref,
                report_ref,
            } => vec![turn_ref, report_ref],
        };
        require(
            self.schema == WAKE_RECEIPT_V1
                && positive(self.observed_unix_ms)
                && [
                    &self.receipt_id,
                    &self.delivery_id,
                    &self.payload_digest,
                    &self.attempt_id,
                ]
                .into_iter()
                .all(|s| is_digest(s))
                && refs.into_iter().all(is_digest),
            "Invalid wake receipt",
        )
    }
}

/// Bound frames before decoding; validators then enforce the closed current version.
pub trait WakeContract: serde::de::DeserializeOwned {
    fn validate_wake(&self) -> Result<(), String>;
}
macro_rules! wake_contract {
    ($($t:ty),+ $(,)?) => { $(impl WakeContract for $t {
        fn validate_wake(&self) -> Result<(),String> { self.validate() }
    })+ };
}
pub mod delivery;

wake_contract!(
    WakeSubscriptionV1,
    WakeObservationV1,
    WakeEnvelopeV1,
    WakeReceiptV1
);

pub fn decode_bounded<T: WakeContract>(bytes: &[u8]) -> Result<T, String> {
    require(
        bytes.len() <= MAX_WAKE_BYTES,
        "Wake frame exceeds byte limit",
    )?;
    let value: T = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    value.validate_wake()?;
    Ok(value)
}

/// Closed durable projector control state. Shape validation grants no checked-prefix authority;
/// only the projector/Store provenance fence can admit this as a continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeCheckpointV1 {
    pub schema: String,
    pub configuration_digest: String,
    pub run_facts_digest: String,
    pub through: WakeBoundaryV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub prior_semantic: Option<(u64, String, WakeStateV1)>,
    pub lease_boundary: WakeBoundaryV1,
    pub observations: u64,
    pub observed_unix_ms: u64,
    pub prefix_digest: String,
    pub clock_observed: bool,
}
impl WakeCheckpointV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.through.validate()?;
        self.lease_boundary.validate()?;
        if let Some((episode, revision, state)) = &self.prior_semantic {
            state.validate()?;
            require(
                positive(*episode) && is_digest(revision),
                "Invalid checkpoint lifecycle identity",
            )?;
        }
        require(
            self.schema == "af/task-wake-checkpoint@1"
                && is_digest(&self.configuration_digest)
                && is_digest(&self.run_facts_digest)
                && is_digest(&self.prefix_digest)
                && positive(self.observed_unix_ms)
                && self.observations <= 4096
                && self.lease_boundary.sequence <= self.through.sequence,
            "Invalid wake checkpoint",
        )
    }
}
