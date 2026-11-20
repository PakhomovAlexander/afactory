//! Closed records in a Project Store's backlog streams.

use crate::is_digest;
use crate::project::{is_commit_id, is_project_workspace_id};
use crate::task::usage::DecimalU64;
use serde::{Deserialize, Serialize};

pub const MAX_EVIDENCE: usize = 8;
pub const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
pub const MAX_TITLE_BYTES: usize = 256;
pub const MAX_TEXT_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum BacklogRecordV1 {
    #[serde(rename = "af/BacklogReport@1")]
    Report(Report),
    #[serde(rename = "af/BacklogItemRevision@1")]
    ItemRevision(ItemRevision),
    #[serde(rename = "af/BacklogBatch@1")]
    Batch(Batch),
    #[serde(rename = "af/BacklogDecision@1")]
    Decision(Decision),
    #[serde(rename = "af/BacklogPublication@1")]
    Publication(Publication),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub project_id: String,
    pub report_id: String,
    pub workspace_id: String,
    pub captured_unix_ms: DecimalU64,
    pub title: String,
    pub observed: String,
    pub repro: Option<String>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub source_revision: Option<String>,
    pub privacy: Privacy,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub content_id: String,
    pub media_type: EvidenceType,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceType {
    #[serde(rename = "text/plain")]
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicFields {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRevision {
    pub project_id: String,
    pub item_id: String,
    pub report_ids: Vec<String>,
    pub claim: String,
    pub evidence_ids: Vec<String>,
    pub source_revision: String,
    pub proposed: PublicFields,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub project_id: String,
    pub authority_id: String,
    pub first_sequence: DecimalU64,
    pub last_sequence: DecimalU64,
    pub report_ids: Vec<String>,
    pub scenario_id: String,
    pub policy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub project_id: String,
    pub batch_id: String,
    pub revision_id: String,
    pub task_results: Vec<String>,
    pub independent_verification_id: Option<String>,
    pub disposition: Disposition,
    pub reason: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Verified,
    AlreadyTracked,
    NeedsEvidence,
    Dismissed,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub project_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub decision_id: String,
    pub destination_id: String,
    pub intent_id: String,
    pub attempt_ids: Vec<String>,
    pub remote_id: Option<String>,
    pub provider_state_id: Option<String>,
    pub receipt_id: Option<String>,
}

pub fn is_report_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn valid_text(text: &str, max_bytes: usize, multiline: bool) -> bool {
    text.len() <= max_bytes
        && !text.trim().is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() || (multiline && matches!(c, '\t' | '\n' | '\r')))
}

impl Report {
    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(&self.project_id)
            || !is_report_id(&self.report_id)
            || !is_project_workspace_id(&self.workspace_id)
            || self
                .source_revision
                .as_deref()
                .is_some_and(|id| !is_commit_id(id))
            || [&self.task_id, &self.run_id]
                .into_iter()
                .flatten()
                .any(|id| !valid_text(id, 256, false))
        {
            return Err("invalid backlog report identity or provenance".into());
        }
        if !valid_text(&self.title, MAX_TITLE_BYTES, false)
            || !valid_text(&self.observed, MAX_TEXT_BYTES, true)
            || self
                .repro
                .as_deref()
                .is_some_and(|repro| !valid_text(repro, MAX_TEXT_BYTES, true))
        {
            return Err("a backlog report requires a bounded title and observed fact".into());
        }
        if self.evidence.len() > MAX_EVIDENCE
            || self.evidence.iter().any(|e| {
                !is_digest(&e.content_id) || !valid_text(&e.text, MAX_EVIDENCE_BYTES, true)
            })
            || self.evidence.iter().map(|e| e.text.len()).sum::<usize>() > MAX_EVIDENCE_BYTES
        {
            return Err(
                "backlog evidence must be bounded, nonempty UTF-8 text with a content digest"
                    .into(),
            );
        }
        Ok(())
    }
}

fn digests(ids: &[String]) -> bool {
    ids.iter().all(|id| is_digest(id))
}

impl BacklogRecordV1 {
    pub fn project_id(&self) -> &str {
        match self {
            Self::Report(record) => &record.project_id,
            Self::ItemRevision(record) => &record.project_id,
            Self::Batch(record) => &record.project_id,
            Self::Decision(record) => &record.project_id,
            Self::Publication(record) => &record.project_id,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(self.project_id()) {
            return Err("invalid backlog Project identity".into());
        }
        let valid = match self {
            Self::Report(report) => return report.validate(),
            Self::ItemRevision(item) => {
                is_digest(&item.item_id)
                    && !item.report_ids.is_empty()
                    && item.report_ids.iter().all(|id| is_report_id(id))
                    && valid_text(&item.claim, MAX_TEXT_BYTES, true)
                    && digests(&item.evidence_ids)
                    && is_commit_id(&item.source_revision)
                    && valid_text(&item.proposed.title, MAX_TITLE_BYTES, false)
                    && valid_text(&item.proposed.body, MAX_TEXT_BYTES, true)
            }
            Self::Batch(batch) => {
                [&batch.authority_id, &batch.scenario_id, &batch.policy_id]
                    .into_iter()
                    .all(|id| is_digest(id))
                    && batch.first_sequence.get() > 0
                    && batch.first_sequence.get() <= batch.last_sequence.get()
                    && batch.report_ids.iter().all(|id| is_report_id(id))
            }
            Self::Decision(decision) => {
                is_digest(&decision.batch_id)
                    && is_digest(&decision.revision_id)
                    && digests(&decision.task_results)
                    && digests(&decision.evidence_ids)
                    && decision
                        .independent_verification_id
                        .as_deref()
                        .is_none_or(is_digest)
                    && (decision.disposition != Disposition::Verified
                        || decision
                            .independent_verification_id
                            .as_ref()
                            .is_some_and(|id| decision.task_results.contains(id)))
                    && valid_text(&decision.reason, MAX_TEXT_BYTES, true)
            }
            Self::Publication(publication) => {
                [
                    &publication.item_id,
                    &publication.revision_id,
                    &publication.decision_id,
                    &publication.destination_id,
                    &publication.intent_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                    && digests(&publication.attempt_ids)
                    && publication
                        .provider_state_id
                        .as_deref()
                        .is_none_or(is_digest)
                    && publication.receipt_id.as_deref().is_none_or(is_digest)
                    && publication
                        .remote_id
                        .as_deref()
                        .is_none_or(|id| valid_text(id, 256, false))
            }
        };
        if valid {
            Ok(())
        } else {
            Err("invalid backlog record".into())
        }
    }
}

pub mod batch;
pub mod intake;
