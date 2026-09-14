//! Generation two separates bounded initial context from exact, readable patch file bytes.
use super::require;
use serde::{Deserialize, Serialize};

pub const TASK_REVIEW_SUBJECT_V2: &str = "af/TaskReviewSubject@2";
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
pub struct TaskReviewSubjectV2 {
    pub subject_id: String,
    pub subject: crate::SubjectV1,
    pub snapshot_id: String,
    pub prior_history_id: String,
    pub round: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub change_scope: Option<TaskReviewChangeScopeV1>,
}
impl TaskReviewSubjectV2 {
    pub fn validate(&self) -> Result<(), String> {
        let changes = match (&self.subject.kind, &self.change_scope) {
            (crate::SubjectKind::WholeTree, None) => None,
            (crate::SubjectKind::Diff, Some(scope)) => {
                scope.patch.validate()?;
                Some(crate::ChangeSetV1 {
                    base_snapshot_id: self
                        .subject
                        .base_snapshot_id
                        .clone()
                        .ok_or("Diff has no Base")?,
                    head_snapshot_id: self.snapshot_id.clone(),
                    changed_paths: scope.changed_paths.clone(),
                    renames: scope.renames.clone(),
                    rename_detection_truncated: scope.rename_detection_truncated,
                    canonical_patch_base64: String::new(),
                    git_version: scope.git_version.clone(),
                    diff_policy_version: scope.diff_policy_version.clone(),
                })
            }
            _ => {
                return Err(
                    "Review Subject must retain its exact readable Change Set scope".into(),
                );
            }
        };
        super::review::TaskReviewSubjectV1 {
            subject_id: self.subject_id.clone(),
            subject: self.subject.clone(),
            change_set: changes,
            snapshot_id: self.snapshot_id.clone(),
            prior_history_id: self.prior_history_id.clone(),
            round: self.round,
        }
        .validate()
    }
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
