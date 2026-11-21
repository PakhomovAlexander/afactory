//! Local Project backlog batch policy and journal. No record authorizes a publication.

use super::is_report_id;
use crate::{is_digest, task::usage::DecimalU64};
use serde::{Deserialize, Serialize};

pub mod assessment;
pub mod provider_index;

const MAX_CALENDAR_MS: u64 = 8_640_000_000_000;

/// A receiver selects an admitted batch by identity. Its contents come from the Project Store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchReference {
    pub schema: BatchReferenceSchema,
    pub project_id: String,
    pub batch_id: String,
    pub policy_id: String,
    pub high_water: DecimalU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchReferenceSchema {
    #[serde(rename = "af/backlog-batch-reference@1")]
    V1,
}

impl BatchReference {
    pub fn validate(&self) -> Result<(), String> {
        if [&self.project_id, &self.batch_id, &self.policy_id]
            .into_iter()
            .all(|id| is_digest(id))
            && self.high_water.get() > 0
        {
            Ok(())
        } else {
            Err("invalid backlog batch reference".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicySchema {
    #[serde(rename = "af/backlog-batch-policy@2")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: PolicySchema,
    pub project_id: String,
    pub authority_id: String,
    pub scenario_id: String,
    pub destination_id: String,
    pub timezone: String,
    pub hour: u8,
    pub minute: u8,
    pub start_unix_ms: u64,
    pub lateness_ms: u64,
    pub run_tokens: u64,
    pub run_wall_ms: u64,
    pub run_dispatches: u32,
    pub rolling_tokens: u64,
    pub rolling_window_ms: u64,
    pub publication_limit: u32,
    pub evidence_access: EvidenceAccess,
    pub hold_policy: HoldPolicy,
}

impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        let max = crate::json::SAFE_INTEGER_MAX as u64;
        if [
            &self.project_id,
            &self.authority_id,
            &self.scenario_id,
            &self.destination_id,
        ]
        .into_iter()
        .all(|id| is_digest(id))
            && !self.timezone.is_empty()
            && self.timezone.len() <= 128
            && self.hour < 24
            && self.minute < 60
            && (1..=MAX_CALENDAR_MS).contains(&self.start_unix_ms)
            && self.lateness_ms < 86_400_000
            && self.run_tokens <= self.rolling_tokens
            && self.rolling_tokens <= max
            && (1..=MAX_CALENDAR_MS).contains(&self.run_wall_ms)
            && (1..=10_000).contains(&self.run_dispatches)
            && (1..=MAX_CALENDAR_MS).contains(&self.rolling_window_ms)
        {
            Ok(())
        } else {
            Err("invalid backlog batch policy".into())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAccess {
    MetadataOnly,
    EvidenceIds,
    PrivateReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldPolicy {
    HoldAll,
    AllowLocalReservations,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneSnapshot {
    pub name: String,
    pub initial_offset: i32,
    pub transitions: Vec<(i64, i32)>,
    pub valid_until_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconsideration {
    pub item_id: String,
    pub report_id: String,
    pub evidence_report_id: Option<String>,
    pub eligible_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Settlement {
    Accounted,
    Held,
    NeedsEvidence,
    ReservePublication { item_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Journal {
    Pinned {
        policy: Box<Policy>,
        zone: ZoneSnapshot,
    },
    Ticked {
        at_unix_ms: u64,
        paused: bool,
        high_water: DecimalU64,
        members: Vec<String>,
    },
    Reconsidered {
        request: Reconsideration,
    },
    Settled {
        batch_id: String,
        member_id: String,
        settlement: Settlement,
    },
    Closed {
        batch_id: String,
        at_unix_ms: u64,
        pending_deliveries: Vec<PendingDelivery>,
        resolved_deliveries: Vec<DeliveryResolution>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDelivery {
    pub revision_id: String,
    pub decision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryResolution {
    pub revision_id: String,
    pub outcome: DeliveryOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Delivered,
    RequiresReverification,
}

pub fn is_member_id(id: &str) -> bool {
    is_report_id(id) || is_digest(id)
}

impl Journal {
    pub fn validate(&self) -> Result<(), String> {
        let valid_time = |at| (1..=MAX_CALENDAR_MS).contains(&at);
        let valid = match self {
            Self::Pinned { policy, zone } => {
                policy.validate()?;
                policy.timezone == zone.name
                    && zone.initial_offset.unsigned_abs() <= 86_400
                    && zone.transitions.len() <= 100_000
                    && zone
                        .transitions
                        .iter()
                        .all(|&(_, offset)| offset.unsigned_abs() <= 86_400)
                    && zone
                        .transitions
                        .windows(2)
                        .all(|pair| pair[0].0 < pair[1].0)
                    && zone.valid_until_unix_seconds.is_none_or(|horizon| {
                        zone.transitions
                            .last()
                            .is_some_and(|&(at, _)| at == horizon)
                    })
            }
            Self::Ticked {
                at_unix_ms,
                members,
                ..
            } => {
                valid_time(*at_unix_ms)
                    && members.len() <= crate::task::document::MAX_REPOSITORY_CITATIONS
                    && members.iter().all(|id| is_member_id(id))
                    && members
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == members.len()
            }
            Self::Reconsidered { request } => {
                is_digest(&request.item_id)
                    && is_report_id(&request.report_id)
                    && request
                        .evidence_report_id
                        .as_ref()
                        .is_none_or(|id| is_report_id(id) && id != &request.report_id)
                    && valid_time(request.eligible_unix_ms)
            }
            Self::Settled {
                batch_id,
                member_id,
                settlement,
            } => {
                is_digest(batch_id)
                    && is_member_id(member_id)
                    && match settlement {
                        Settlement::ReservePublication { item_id } => is_digest(item_id),
                        _ => true,
                    }
            }
            Self::Closed {
                batch_id,
                at_unix_ms,
                pending_deliveries,
                resolved_deliveries,
            } => {
                is_digest(batch_id)
                    && valid_time(*at_unix_ms)
                    && pending_deliveries
                        .iter()
                        .all(|work| is_digest(&work.revision_id) && is_digest(&work.decision_id))
                    && resolved_deliveries
                        .iter()
                        .all(|work| is_digest(&work.revision_id))
            }
        };
        if valid {
            Ok(())
        } else {
            Err("invalid backlog batch journal record".into())
        }
    }
}
