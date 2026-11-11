//! `FindingGrouping@1` — one reversible operator adjudication between two Findings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingGroupingAction {
    Group,
    Ungroup,
}

/// Grouping changes the projected adjudication view, never Finding or Report identity. The
/// compensating `ungroup` action names the same directed relation and restores independent views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingGroupingV1 {
    pub from: String,
    pub into: String,
    pub action: FindingGroupingAction,
    pub round: u32,
}

impl FindingGroupingV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.from.trim().is_empty()
            || self.into.trim().is_empty()
            || self.from == self.into
            || self.round == 0
        {
            return Err("FindingGrouping@1 contains invalid Finding identities or Round".into());
        }
        Ok(())
    }
}

/// Event payload that binds a grouping transition to its immutable artifact record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingGroupingEventPayloadV1 {
    pub from: String,
    pub into: String,
    pub grouping_artifact_id: String,
}

impl FindingGroupingEventPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.from.trim().is_empty()
            || self.into.trim().is_empty()
            || self.from == self.into
            || !crate::is_digest(&self.grouping_artifact_id)
        {
            return Err("Finding grouping event contains invalid identities or artifact ID".into());
        }
        Ok(())
    }
}
