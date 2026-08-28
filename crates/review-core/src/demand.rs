//! Durable Demand, Evidence, and exact Demand Set contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemandRequirement {
    Required,
    Advisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemandStatus {
    Open,
    Satisfied,
    Stale,
    Waived,
}

/// One immutable measurement obligation selected from a reviewer result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandV1 {
    pub demand_id: String,
    pub claim: String,
    pub why: String,
    pub suggested_method: String,
    pub source: String,
    pub requirement: DemandRequirement,
    pub round: u32,
    pub subject_id: String,
}

impl DemandV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.demand_id)
            || self.claim.trim().is_empty()
            || self.why.trim().is_empty()
            || self.suggested_method.trim().is_empty()
            || self.source.trim().is_empty()
            || self.round == 0
            || !crate::is_digest(&self.subject_id)
        {
            return Err("Demand@1 contains invalid identity, content, or Subject authority".into());
        }
        Ok(())
    }
}

/// Measurement bytes stored separately in CAS and linked to one exact Demand and Subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceV1 {
    pub demand_id: String,
    pub subject_id: String,
    pub content_artifact_id: String,
    pub actor: String,
}

impl EvidenceV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.demand_id)
            || !crate::is_digest(&self.subject_id)
            || !crate::is_digest(&self.content_artifact_id)
            || self.actor.trim().is_empty()
        {
            return Err("Evidence@1 contains invalid Demand, Subject, content, or actor".into());
        }
        Ok(())
    }
}

/// A trusted policy decision that linked Evidence satisfies a Demand on one exact Subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSatisfactionV1 {
    pub demand_id: String,
    pub evidence_id: String,
    pub subject_id: String,
    pub policy_revision: String,
    pub reason: String,
}

impl EvidenceSatisfactionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.demand_id)
            || !crate::is_digest(&self.evidence_id)
            || !crate::is_digest(&self.subject_id)
            || self.policy_revision.trim().is_empty()
            || self.reason.trim().is_empty()
        {
            return Err("EvidenceSatisfaction@1 contains invalid authority or reason".into());
        }
        Ok(())
    }
}

/// The explicit authenticated escape hatch for one Demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandWaiverV1 {
    pub demand_id: String,
    pub subject_id: String,
    pub actor: String,
    pub policy_revision: String,
    pub reason: String,
}

impl DemandWaiverV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.demand_id)
            || !crate::is_digest(&self.subject_id)
            || self.actor.trim().is_empty()
            || self.policy_revision.trim().is_empty()
            || self.reason.trim().is_empty()
        {
            return Err("DemandWaiver@1 contains invalid authority or reason".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandSetEntryV1 {
    pub demand_id: String,
    pub claim: String,
    pub why: String,
    pub suggested_method: String,
    pub source: String,
    pub requirement: DemandRequirement,
    pub status: DemandStatus,
    pub subject_id: String,
    pub evidence_ids: Vec<String>,
    pub satisfaction_ids: Vec<String>,
    pub waiver_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandSetV1 {
    pub subject_id: String,
    pub round: u32,
    pub prior_demand_set_id: String,
    pub reducer_version: String,
    pub selected_demand_artifact_ids: Vec<String>,
    pub satisfaction_artifact_ids: Vec<String>,
    pub waiver_artifact_ids: Vec<String>,
    pub demands: Vec<DemandSetEntryV1>,
}

pub const DEMAND_REDUCER_VERSION: &str = "review.kernel/demand-reducer@1";

impl DemandSetV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.subject_id)
            || self.round == 0
            || !crate::is_digest(&self.prior_demand_set_id)
            || self.reducer_version != DEMAND_REDUCER_VERSION
            || self
                .selected_demand_artifact_ids
                .iter()
                .chain(&self.satisfaction_artifact_ids)
                .chain(&self.waiver_artifact_ids)
                .any(|id| !crate::is_digest(id))
        {
            return Err("DemandSet@1 contains invalid reducer authority".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for demand in &self.demands {
            if !ids.insert(demand.demand_id.as_str())
                || !crate::is_digest(&demand.demand_id)
                || demand.claim.trim().is_empty()
                || demand.why.trim().is_empty()
                || demand.suggested_method.trim().is_empty()
                || demand.source.trim().is_empty()
                || !crate::is_digest(&demand.subject_id)
                || demand
                    .evidence_ids
                    .iter()
                    .chain(&demand.satisfaction_ids)
                    .chain(&demand.waiver_ids)
                    .any(|id| !crate::is_digest(id))
            {
                return Err("DemandSet@1 contains an invalid Demand projection".into());
            }
        }
        Ok(())
    }
}

/// Event payload shared by immutable Demand/Evidence authority records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedArtifactPayloadV1 {
    pub artifact_id: String,
}

impl RecordedArtifactPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.artifact_id) {
            return Err("recorded artifact event contains an invalid artifact ID".into());
        }
        Ok(())
    }
}
