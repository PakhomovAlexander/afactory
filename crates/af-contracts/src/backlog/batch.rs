//! Local Project backlog batch policy and journal. No record authorizes a publication.

use super::is_report_id;
use crate::{is_digest, task::usage::DecimalU64};
use serde::{Deserialize, Serialize};

const MAX_CALENDAR_MS: u64 = 8_640_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicySchema {
    #[serde(rename = "af/backlog-batch-policy@1")]
    V1,
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
    },
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
            } => is_digest(batch_id) && valid_time(*at_unix_ms),
        };
        if valid {
            Ok(())
        } else {
            Err("invalid backlog batch journal record".into())
        }
    }
}
