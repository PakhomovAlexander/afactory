//! Immutable campaign authority and bootstrap event payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{SubjectKind, is_digest};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityFileV1 {
    pub path: String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignReviewerV1 {
    pub node: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub package_artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignConvergenceV1 {
    pub clean_rounds: u32,
    pub max_rounds: u32,
    pub gate: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignBudgetV1 {
    pub attempt_tokens: u64,
    pub run_tokens: u64,
}

/// Resolved authority for the Campaign. Selector labels deliberately do not participate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignManifestV1 {
    pub authority_snapshot_id: String,
    pub subject_kind: SubjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,
    pub pipeline: AuthorityFileV1,
    pub reviewer_lock: AuthorityFileV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<CampaignReviewerV1>,
    pub execution_policy_ids: Vec<String>,
    pub project_policy_ids: Vec<String>,
    pub convergence: CampaignConvergenceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<CampaignBudgetV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    pub finding_identity_policy: String,
    pub finding_genesis_id: String,
    pub demand_genesis_id: String,
}

impl CampaignManifestV1 {
    pub fn validate(&self) -> Result<(), String> {
        let required_ids = [
            self.authority_snapshot_id.as_str(),
            self.pipeline.artifact_id.as_str(),
            self.reviewer_lock.artifact_id.as_str(),
            self.finding_genesis_id.as_str(),
            self.demand_genesis_id.as_str(),
        ];
        if required_ids.into_iter().any(|id| !is_digest(id)) {
            return Err("CampaignManifest@1 contains an invalid artifact ID".into());
        }
        if self.pipeline.path.trim().is_empty() || self.reviewer_lock.path.trim().is_empty() {
            return Err("CampaignManifest@1 contains an empty authority path".into());
        }
        match self.subject_kind {
            SubjectKind::WholeTree if self.base_snapshot_id.is_some() => {
                return Err("a whole-tree Campaign forbids a pinned Base".into());
            }
            SubjectKind::Diff if !self.base_snapshot_id.as_deref().is_some_and(is_digest) => {
                return Err("a diff Campaign requires a pinned Base Snapshot ID".into());
            }
            _ => {}
        }
        let mut nodes = std::collections::BTreeSet::new();
        for reviewer in &self.reviewers {
            if reviewer.node.trim().is_empty()
                || reviewer.name.trim().is_empty()
                || reviewer.version.trim().is_empty()
                || !nodes.insert(reviewer.node.as_str())
                || !is_digest(&reviewer.digest)
                || !is_digest(&reviewer.package_artifact_id)
            {
                return Err("CampaignManifest@1 contains an invalid reviewer binding".into());
            }
        }
        if self
            .execution_policy_ids
            .iter()
            .chain(&self.project_policy_ids)
            .any(|id| !is_digest(id))
        {
            return Err("CampaignManifest@1 contains an invalid policy ID".into());
        }
        if self.convergence.clean_rounds == 0
            || self.convergence.max_rounds == 0
            || self.convergence.clean_rounds > self.convergence.max_rounds
            || self.convergence.gate.trim().is_empty()
        {
            return Err("CampaignManifest@1 contains an invalid convergence policy".into());
        }
        if self
            .focus
            .as_deref()
            .is_some_and(|focus| focus.trim().is_empty())
            || self.finding_identity_policy.trim().is_empty()
        {
            return Err("CampaignManifest@1 contains empty policy text".into());
        }
        if self.budgets.is_some_and(|budget| {
            budget.attempt_tokens == 0
                || budget.run_tokens == 0
                || budget.attempt_tokens > budget.run_tokens
        }) {
            return Err("CampaignManifest@1 contains an invalid budget policy".into());
        }
        Ok(())
    }
}

/// Digest-verified package bytes, represented by their CAS IDs rather than a live directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerPackageV1 {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub files: BTreeMap<String, String>,
}

impl ReviewerPackageV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.version.trim().is_empty()
            || !is_digest(&self.digest)
            || self.files.is_empty()
            || self
                .files
                .iter()
                .any(|(path, id)| path.trim().is_empty() || !is_digest(id))
        {
            return Err("ReviewerPackage@1 is incomplete or contains an invalid digest".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignOpenedPayloadV1 {
    pub campaign_manifest_id: String,
    pub authority_snapshot_id: String,
}

impl CampaignOpenedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(&self.campaign_manifest_id) || !is_digest(&self.authority_snapshot_id) {
            return Err("CampaignOpened@1 contains an invalid artifact ID".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundStartedPayloadV1 {
    pub round: u32,
    pub epoch: u32,
    pub campaign_manifest_id: String,
    pub subject_id: String,
    pub prior_finding_set_id: String,
    pub prior_demand_set_id: String,
}

impl RoundStartedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.round == 0 || self.epoch == 0 {
            return Err("RoundStarted@1 requires positive round and epoch numbers".into());
        }
        if [
            self.campaign_manifest_id.as_str(),
            self.subject_id.as_str(),
            self.prior_finding_set_id.as_str(),
            self.prior_demand_set_id.as_str(),
        ]
        .into_iter()
        .any(|id| !is_digest(id))
        {
            return Err("RoundStarted@1 contains an invalid artifact ID".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundInputSupersededPayloadV1 {
    pub round: u32,
    pub old_epoch: u32,
    pub new_epoch: u32,
    pub campaign_manifest_id: String,
    pub old_subject_id: String,
    pub replacement_subject_id: String,
}

impl RoundInputSupersededPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.round == 0 || self.old_epoch == 0 || self.new_epoch != self.old_epoch + 1 {
            return Err("RoundInputSuperseded@1 requires one monotonic epoch step".into());
        }
        if [
            self.campaign_manifest_id.as_str(),
            self.old_subject_id.as_str(),
            self.replacement_subject_id.as_str(),
        ]
        .into_iter()
        .any(|id| !is_digest(id))
        {
            return Err("RoundInputSuperseded@1 contains an invalid artifact ID".into());
        }
        Ok(())
    }
}
