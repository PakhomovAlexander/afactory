//! The Review Subject separates bounded initial context from exact, readable patch file bytes.
use super::require;
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_SUBJECT_V3: &str = "af/TaskReviewSubject@3";
pub const TASK_REVIEW_ASSIGNMENT_V1: &str = "af/TaskReviewAssignment@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewFileV1 {
    pub path: String,
    pub content_id: String,
    pub bytes: u64,
}
impl TaskReviewFileV1 {
    pub fn path_for(content_id: &str) -> String {
        format!(
            ".af-review-inputs/{}.patch",
            content_id.trim_start_matches("sha256:")
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            crate::is_digest(&self.content_id)
                && self.path == Self::path_for(&self.content_id)
                && self.bytes <= crate::MAX_CHANGE_SET_BYTES as u64,
            "Review file requires its exact bounded content identity and reserved relative path",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewChangeScopeV1 {
    pub changed_paths: Vec<String>,
    pub renames: Vec<crate::PathRenameV1>,
    pub rename_detection_truncated: bool,
    pub git_version: String,
    pub diff_policy_version: String,
    pub patch: TaskReviewFileV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewSubject {
    pub schema: String,
    pub subject_id: String,
    pub subject: crate::SubjectV1,
    pub snapshot_id: String,
    pub prior_history_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub continuation_id: Option<String>,
    pub round: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub change_scope: Option<TaskReviewChangeScopeV1>,
}
impl TaskReviewSubject {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.task-review-subject/3",
            "Unsupported Review Subject schema",
        )?;
        validate_subject_identity(
            &self.subject_id,
            &self.subject,
            &self.snapshot_id,
            &self.prior_history_id,
            self.continuation_id.as_deref(),
            self.round,
        )?;
        match (&self.subject.kind, &self.change_scope) {
            (crate::SubjectKind::WholeTree, None) => Ok(()),
            (crate::SubjectKind::Diff, Some(scope)) => {
                scope.patch.validate()?;
                require(
                    !scope.changed_paths.is_empty(),
                    "Review Diff requires exact nonempty scope",
                )?;
                crate::change_set::validate_change_scope(
                    self.subject
                        .base_snapshot_id
                        .as_deref()
                        .ok_or("Diff has no Base")?,
                    &self.snapshot_id,
                    &scope.changed_paths,
                    &scope.renames,
                    &scope.git_version,
                    &scope.diff_policy_version,
                )
            }
            _ => Err("Review Subject must retain its exact readable Change Set scope".into()),
        }
    }
}

pub(super) fn validate_subject_identity(
    subject_id: &str,
    subject: &crate::SubjectV1,
    snapshot_id: &str,
    prior_history_id: &str,
    continuation_id: Option<&str>,
    round: u32,
) -> Result<(), String> {
    subject.validate()?;
    require(
        subject.head_snapshot_id == snapshot_id,
        "Review Subject has a stale Snapshot",
    )?;
    require(
        [subject_id, snapshot_id, prior_history_id]
            .into_iter()
            .all(crate::is_digest)
            && (1..=16).contains(&round)
            && continuation_id.is_none_or(crate::is_digest)
            && (continuation_id.is_none() || round > 1),
        "Review Subject requires bounded Round and exact Subject, Snapshot and history",
    )
}

/// Only the named logical reviewer's prior claims. Other reviewers' history stays host-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewAssignmentV1 {
    pub subject_id: String,
    pub prior_history_id: String,
    pub round: u32,
    pub reviewer: String,
    pub findings: Vec<crate::FindingSetEntryV1>,
}
impl TaskReviewAssignmentV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            super::is_name(&self.reviewer)
                && (1..=16).contains(&self.round)
                && self.findings.iter().all(|finding| {
                    finding.source == self.reviewer
                        && !matches!(finding.status.as_str(), "rejected" | "wontfix")
                })
                && self
                    .findings
                    .iter()
                    .map(|f| &f.finding_id)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.findings.len(),
            "Review assignment requires unique source-scoped current prior Findings",
        )?;
        crate::FindingSetV1 {
            subject_id: self.subject_id.clone(),
            round: self.round,
            prior_finding_set_id: self.prior_history_id.clone(),
            reducer_version: crate::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: crate::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: vec![],
            relation_ids: vec![],
            resolution_ids: vec![],
            findings: self.findings.clone(),
        }
        .validate()?;
        require(
            serde_json::to_vec(self).map_err(|e| e.to_string())?.len()
                <= crate::MAX_PRIOR_FINDINGS_BYTES,
            "Review assignment exceeds the existing 64 KiB prior-Finding bound",
        )
    }
}
