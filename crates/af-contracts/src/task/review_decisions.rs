//! Human decisions are captured Task inputs, never generic Store append requests.
use serde::{Deserialize, Serialize};

pub const REVIEW_DECISIONS_V1: &str = "af/ReviewDecisions@1";
pub const REVIEW_DECISIONS_SCHEMA: &str = "af.review-decisions/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDecisionContextV1 {
    pub schema: String,
    pub review_id: String,
    pub review: super::review::TaskReviewRoundV1,
    pub subject: crate::SubjectV1,
    pub finding_set: crate::FindingSetV1,
    pub demand_set: crate::DemandSetV1,
    pub finding_view_ids: std::collections::BTreeMap<String, String>,
    pub policy_time: u64,
    pub resolutions: std::collections::BTreeMap<String, DecisionResolutionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResolutionV1 {
    pub artifact_id: String,
    pub resolution: crate::FindingResolutionV1,
}

impl ReviewDecisionContextV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.review.validate()?;
        self.subject.validate()?;
        self.finding_set.validate()?;
        self.demand_set.validate()?;
        if self.schema != "af/review-decision-context@1"
            || !crate::is_digest(&self.review_id)
            || self.review.subject_id != self.finding_set.subject_id
            || self.review.subject_id != self.demand_set.subject_id
            || self.review.snapshot_id != self.subject.head_snapshot_id
            || self.review.round != self.finding_set.round
            || self.review.round != self.demand_set.round
            || self
                .finding_view_ids
                .values()
                .any(|id| !crate::is_digest(id))
            || self
                .resolutions
                .values()
                .any(|r| !crate::is_digest(&r.artifact_id) || r.resolution.validate().is_err())
            || !super::safe_number(self.policy_time)
        {
            return Err("Invalid native Review decision context".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDecisionsV1 {
    pub schema: String,
    pub review_id: String,
    pub policy_id: String,
    pub subject_id: String,
    pub snapshot_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub base_snapshot_id: Option<String>,
    pub actor: String,
    pub reason: String,
    pub actions: Vec<ReviewDecisionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionEvidenceV1 {
    Recorded {
        artifact_id: String,
    },
    /// Zero-based index of an earlier attachment in this exact batch.
    Attached {
        action: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewDecisionV1 {
    Attach {
        demand_id: String,
        content: String,
    },
    Satisfy {
        demand_id: String,
        evidence: DecisionEvidenceV1,
        reason: String,
    },
    /// Explicitly authorize an existing satisfaction on the current Subject.
    Reuse {
        demand_id: String,
        satisfaction_id: String,
        reason: String,
    },
    /// Copy compatible evidence to another Demand, retaining the exact original Evidence.
    ReuseEvidence {
        demand_id: String,
        evidence_id: String,
        reason: String,
    },
    Waive {
        waiver: crate::DemandWaiverV1,
    },
    Resolve {
        resolution: crate::FindingResolutionV1,
    },
    Challenge {
        challenge: crate::ResolutionChallengeV1,
    },
    PolicyTime {
        time: crate::PolicyTimeV1,
    },
    Group {
        grouping: crate::FindingGroupingV1,
        from_view_id: String,
        into_view_id: String,
    },
}

impl ReviewDecisionsV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REVIEW_DECISIONS_SCHEMA
            || [
                &self.review_id,
                &self.policy_id,
                &self.subject_id,
                &self.snapshot_id,
            ]
            .into_iter()
            .any(|id| !crate::is_digest(id))
            || self
                .base_snapshot_id
                .as_deref()
                .is_some_and(|id| !crate::is_digest(id))
            || self.actor.trim().is_empty()
            || self.actor.len() > 256
            || self.reason.trim().is_empty()
            || self.reason.len() > 4096
            || self.actions.is_empty()
            || self.actions.len() > 32
        {
            return Err("Review decisions require exact original Review, policy, Subject, actor and a bounded nonempty batch".into());
        }
        for (index, action) in self.actions.iter().enumerate() {
            let valid = match action {
                ReviewDecisionV1::Attach { demand_id, content } => {
                    crate::is_digest(demand_id) && !content.is_empty() && content.len() <= 65536
                }
                ReviewDecisionV1::Satisfy {
                    demand_id,
                    evidence,
                    reason,
                } => {
                    crate::is_digest(demand_id)
                        && !reason.trim().is_empty()
                        && match evidence {
                            DecisionEvidenceV1::Recorded { artifact_id } => {
                                crate::is_digest(artifact_id)
                            }
                            DecisionEvidenceV1::Attached { action } => {
                                (*action as usize) < index
                                    && matches!(
                                        self.actions.get(*action as usize),
                                        Some(ReviewDecisionV1::Attach { .. })
                                    )
                            }
                        }
                }
                ReviewDecisionV1::Reuse {
                    demand_id,
                    satisfaction_id,
                    reason,
                } => {
                    crate::is_digest(demand_id)
                        && crate::is_digest(satisfaction_id)
                        && !reason.trim().is_empty()
                }
                ReviewDecisionV1::ReuseEvidence {
                    demand_id,
                    evidence_id,
                    reason,
                } => {
                    crate::is_digest(demand_id)
                        && crate::is_digest(evidence_id)
                        && !reason.trim().is_empty()
                }
                ReviewDecisionV1::Waive { waiver } => {
                    waiver.validate().is_ok()
                        && waiver.subject_id == self.subject_id
                        && waiver.policy_revision == self.policy_id
                        && waiver.actor == self.actor
                }
                ReviewDecisionV1::Resolve { resolution } => {
                    resolution.validate().is_ok()
                        && resolution.subject_id == self.subject_id
                        && resolution.policy_revision == self.policy_id
                        && resolution.actor == self.actor
                        && resolution.outcome != crate::FindingResolutionOutcome::Fixed
                        && !resolution.evidence_ids.is_empty()
                }
                ReviewDecisionV1::Challenge { challenge } => {
                    challenge.validate().is_ok()
                        && challenge.subject_id == self.subject_id
                        && challenge.actor == self.actor
                }
                ReviewDecisionV1::PolicyTime { time } => {
                    time.validate().is_ok()
                        && time.actor == self.actor
                        && super::safe_number(time.tick)
                }
                ReviewDecisionV1::Group {
                    grouping,
                    from_view_id,
                    into_view_id,
                } => {
                    grouping.validate().is_ok()
                        && crate::is_digest(from_view_id)
                        && crate::is_digest(into_view_id)
                }
            };
            if !valid {
                return Err(format!(
                    "Invalid Review decision at action {index}; fixed claims require native independent repair verification"
                ));
            }
        }
        // The complete input, including attachments, shares one explicit bound.
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 256 * 1024 {
            return Err("Review decisions exceed 256 KiB".into());
        }
        Ok(())
    }
}
