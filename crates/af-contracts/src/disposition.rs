//! `FindingDisposition@1` — one reviewer's immutable position on one assigned prior Finding.

use serde::{Deserialize, Serialize};

use crate::is_digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingDispositionPosition {
    Corroborate,
    NotReproduced,
    Dispute,
}

/// Attempt provenance in the enclosing artifact names the selected reviewer Attempt. The
/// payload repeats the stable reviewer node, Round, and Subject binding because those are the
/// disposition's domain content and must remain queryable without interpreting producer fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingDispositionV1 {
    pub finding_id: String,
    pub source: String,
    pub position: FindingDispositionPosition,
    pub reason: String,
    pub round: u32,
    pub subject_id: String,
}

impl FindingDispositionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.finding_id.trim().is_empty()
            || self.source.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.round == 0
            || !is_digest(&self.subject_id)
        {
            return Err("FindingDisposition@1 contains invalid disposition evidence".into());
        }
        Ok(())
    }
}
