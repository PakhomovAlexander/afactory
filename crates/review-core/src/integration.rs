//! Checked internal Integration contracts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCandidateV1 {
    pub proposal_id: String,
    pub candidate_artifact_id: String,
    pub node_id: String,
    pub priority: u32,
    pub patch_artifact_id: String,
    pub derived_manifest_artifact_id: String,
    pub paths: Vec<String>,
    pub finding_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
}

impl IntegrationCandidateV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty()
            || !crate::is_digest(&self.proposal_id)
            || !crate::is_digest(&self.candidate_artifact_id)
            || !crate::is_digest(&self.patch_artifact_id)
            || !crate::is_digest(&self.derived_manifest_artifact_id)
            || self.finding_ids.is_empty()
            || self.finding_ids.iter().any(|id| !crate::is_digest(id))
            || self.evidence_ids.iter().any(|id| !crate::is_digest(id))
        {
            return Err("Integration candidate contains invalid authority".into());
        }
        let mut paths = BTreeSet::new();
        if self.paths.is_empty()
            || self
                .paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path) || !paths.insert(path))
        {
            return Err("Integration candidate paths are empty, invalid, or repeated".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPlanV1 {
    pub subject_id: String,
    pub base_snapshot_id: String,
    pub policy_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_paths: Vec<String>,
    pub candidates: Vec<IntegrationCandidateV1>,
    pub derived_manifest_artifact_id: String,
}

impl IntegrationPlanV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.subject_id)
            || !crate::is_digest(&self.base_snapshot_id)
            || !crate::is_digest(&self.policy_id)
            || !crate::is_digest(&self.derived_manifest_artifact_id)
            || self.candidates.is_empty()
        {
            return Err("IntegrationPlan@1 contains invalid authority".into());
        }
        let mut protected = BTreeSet::new();
        if self
            .protected_paths
            .iter()
            .any(|path| !crate::is_valid_repo_path(path) || !protected.insert(path.as_str()))
        {
            return Err("IntegrationPlan@1 protected paths are invalid or repeated".into());
        }
        let mut proposals = BTreeSet::new();
        let mut nodes = BTreeSet::new();
        let mut patches = BTreeSet::new();
        for candidate in &self.candidates {
            candidate.validate()?;
            if !proposals.insert(candidate.proposal_id.as_str())
                || !nodes.insert((candidate.priority, candidate.node_id.as_str()))
                || !patches.insert(candidate.patch_artifact_id.as_str())
            {
                return Err("IntegrationPlan@1 repeats a Proposal, priority/node, or patch".into());
            }
        }
        if self.candidates.windows(2).any(|pair| {
            (
                pair[0].priority,
                pair[0].node_id.as_str(),
                pair[0].proposal_id.as_str(),
            ) >= (
                pair[1].priority,
                pair[1].node_id.as_str(),
                pair[1].proposal_id.as_str(),
            )
        }) {
            return Err("IntegrationPlan@1 candidates are not in canonical priority order".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCheckV1 {
    pub name: String,
    pub passed: bool,
    pub result_artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationChecksV1 {
    pub derived_snapshot_id: String,
    pub checks: Vec<IntegrationCheckV1>,
}

impl IntegrationChecksV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.derived_snapshot_id) || self.checks.is_empty() {
            return Err("IntegrationChecks@1 has invalid or empty authority".into());
        }
        let mut names = BTreeSet::new();
        for check in &self.checks {
            if check.name.trim().is_empty()
                || !names.insert(check.name.as_str())
                || !crate::is_digest(&check.result_artifact_id)
            {
                return Err("IntegrationChecks@1 contains an invalid or repeated check".into());
            }
        }
        Ok(())
    }

    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPreparedPayloadV1 {
    pub batch_id: String,
    pub plan_artifact_id: String,
    pub derived_snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationConflictPayloadV1 {
    pub base_snapshot_id: String,
    pub proposal_ids: Vec<String>,
    pub paths: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationChecksCompletedPayloadV1 {
    pub batch_id: String,
    pub checks_artifact_id: String,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCommittedPayloadV1 {
    pub batch_id: String,
    pub prior_subject_id: String,
    pub derived_subject_id: String,
    pub prior_snapshot_id: String,
    pub derived_snapshot_id: String,
    pub proposal_ids: Vec<String>,
    pub attestation_ids: Vec<String>,
    pub expected_finding_set_id: String,
    pub expected_demand_set_id: String,
    pub policy_id: String,
    pub semantic_closure_id: String,
}

fn valid_ids(ids: &[String], require_nonempty: bool) -> bool {
    (!require_nonempty || !ids.is_empty())
        && ids.iter().all(|id| crate::is_digest(id))
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}

impl IntegrationPreparedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.batch_id.trim().is_empty()
            || !crate::is_digest(&self.plan_artifact_id)
            || !crate::is_digest(&self.derived_snapshot_id)
        {
            return Err("IntegrationPrepared@1 contains invalid authority".into());
        }
        Ok(())
    }
}

impl IntegrationConflictPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        let mut paths = BTreeSet::new();
        if !crate::is_digest(&self.base_snapshot_id)
            || !valid_ids(&self.proposal_ids, true)
            || self.paths.is_empty()
            || self
                .paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path) || !paths.insert(path.as_str()))
            || self.reason.trim().is_empty()
        {
            return Err("IntegrationConflict@1 contains invalid conflict evidence".into());
        }
        Ok(())
    }
}

impl IntegrationChecksCompletedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.batch_id.trim().is_empty() || !crate::is_digest(&self.checks_artifact_id) {
            return Err("IntegrationChecksCompleted@1 contains invalid authority".into());
        }
        Ok(())
    }
}

impl IntegrationCommittedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        let ids = [
            &self.prior_subject_id,
            &self.derived_subject_id,
            &self.prior_snapshot_id,
            &self.derived_snapshot_id,
            &self.expected_finding_set_id,
            &self.expected_demand_set_id,
            &self.policy_id,
            &self.semantic_closure_id,
        ];
        if self.batch_id.trim().is_empty()
            || ids.into_iter().any(|id| !crate::is_digest(id))
            || !valid_ids(&self.proposal_ids, true)
            || !valid_ids(&self.attestation_ids, true)
        {
            return Err("IntegrationCommitted@1 contains invalid authority".into());
        }
        Ok(())
    }
}
