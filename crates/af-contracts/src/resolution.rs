//! Immutable inputs to trusted Finding resolution policy.

use serde::{Deserialize, Serialize};

use crate::{Severity, is_digest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingResolutionOutcome {
    Fixed,
    Rejected,
    WontfixTracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionChallengeKind {
    NewEvidence,
    HigherSeverity,
    OutsideScope,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedRegionV1 {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
}

impl ChangedRegionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_valid_repo_path(&self.path)
            || self.start_line == Some(0)
            || self.end_line == Some(0)
            || matches!((self.start_line, self.end_line), (Some(start), Some(end)) if end < start)
            || self.start_line.is_none() != self.end_line.is_none()
        {
            return Err("ChangeAttestation@1 contains an invalid changed region".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeAttestationV1 {
    pub finding_id: String,
    pub expected_finding_view_id: String,
    pub subject_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<String>,
    pub changed_regions: Vec<ChangedRegionV1>,
    pub actor: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

impl ChangeAttestationV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.finding_id.trim().is_empty()
            || !is_digest(&self.expected_finding_view_id)
            || !is_digest(&self.subject_id)
            || self
                .change_set_id
                .as_deref()
                .is_some_and(|id| !is_digest(id))
            || self.changed_regions.is_empty()
            || self
                .changed_regions
                .iter()
                .any(|region| region.validate().is_err())
            || self.actor.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.evidence_ids.iter().any(|id| !is_digest(id))
        {
            return Err("ChangeAttestation@1 contains invalid authority or scope".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixVerificationV1 {
    pub finding_id: String,
    pub attestation_id: String,
    pub expected_finding_view_id: String,
    pub subject_id: String,
    pub verifier: String,
    pub policy_revision: String,
    pub positive: bool,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

impl FixVerificationV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.finding_id.trim().is_empty()
            || !is_digest(&self.attestation_id)
            || !is_digest(&self.expected_finding_view_id)
            || !is_digest(&self.subject_id)
            || self.verifier.trim().is_empty()
            || self.policy_revision.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.evidence_ids.iter().any(|id| !is_digest(id))
        {
            return Err("FixVerification@1 contains invalid authority or evidence".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingResolutionV1 {
    pub finding_id: String,
    pub expected_finding_view_id: String,
    pub subject_id: String,
    pub outcome: FindingResolutionOutcome,
    pub actor: String,
    pub policy_revision: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_accepted_severity: Option<Severity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_policy_time: Option<u64>,
}

impl FindingResolutionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.finding_id.trim().is_empty()
            || !is_digest(&self.expected_finding_view_id)
            || !is_digest(&self.subject_id)
            || self.actor.trim().is_empty()
            || self.policy_revision.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.evidence_ids.iter().any(|id| !is_digest(id))
            || self
                .verification_id
                .as_deref()
                .is_some_and(|id| !is_digest(id))
            || self
                .tracking_reference
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err("FindingResolution@1 contains invalid authority or evidence".into());
        }
        match self.outcome {
            FindingResolutionOutcome::Fixed => {
                if self.verification_id.is_none()
                    || self.max_accepted_severity.is_some()
                    || self.tracking_reference.is_some()
                    || self.expires_at_policy_time.is_some()
                {
                    return Err("fixed Resolution requires only a FixVerification".into());
                }
            }
            FindingResolutionOutcome::Rejected => {
                if self.verification_id.is_some()
                    || self.max_accepted_severity.is_some()
                    || self.tracking_reference.is_some()
                    || self.expires_at_policy_time.is_some()
                {
                    return Err("rejected Resolution has invalid tracked-wontfix fields".into());
                }
            }
            FindingResolutionOutcome::WontfixTracked => {
                if self.verification_id.is_some()
                    || self.max_accepted_severity.is_none()
                    || self.tracking_reference.is_none()
                    || self.expires_at_policy_time.is_none_or(|time| time == 0)
                {
                    return Err(
                        "wontfix-tracked Resolution requires ceiling, tracking, and expiry".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionChallengeV1 {
    pub finding_id: String,
    pub resolution_id: String,
    pub subject_id: String,
    pub kind: ResolutionChallengeKind,
    pub actor: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

impl ResolutionChallengeV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.finding_id.trim().is_empty()
            || !is_digest(&self.resolution_id)
            || !is_digest(&self.subject_id)
            || self.actor.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.evidence_ids.iter().any(|id| !is_digest(id))
        {
            return Err("ResolutionChallenge@1 contains invalid authority or evidence".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyTimeV1 {
    pub tick: u64,
    pub actor: String,
    pub reason: String,
}

impl PolicyTimeV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.tick == 0 || self.actor.trim().is_empty() || self.reason.trim().is_empty() {
            return Err("PolicyTime@1 contains invalid persisted policy time".into());
        }
        Ok(())
    }
}
