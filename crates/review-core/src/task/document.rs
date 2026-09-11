//! Document Tasks carry typed content and exact source references, without code Snapshots.
//! Shape validation does not confer acceptance; installed domain checks own receipt identity.
use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DOCUMENT_SOURCES_V1: &str = "af/DocumentSources@1";
pub const DOCUMENT_DRAFT_V1: &str = "af/DocumentDraft@1";
pub const DOCUMENT_V1: &str = "af/Document@1";
pub const DOCUMENT_CHECK_RECEIPT_V1: &str = "af/DocumentCheckReceipt@1";
pub const DOCUMENT_EVALUATION_V1: &str = "af/DocumentEvaluation@1";
pub const DOCUMENT_VERIFICATION_V1: &str = "af/DocumentVerification@1";

fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}
fn line(value: &str, limit: usize) -> bool {
    text(value, limit) && !value.contains(['\n', '\t'])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSourceV1 {
    pub title: String,
    /// Captured provenance, never a command or an instruction to fetch this location.
    pub uri: String,
    pub revision: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSourcesV1 {
    pub schema: String,
    pub sources: BTreeMap<String, DocumentSourceV1>,
}
impl DocumentSourcesV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.document-sources/1"
                && !self.sources.is_empty()
                && self.sources.len() <= 32,
            "Document sources require one to 32 captured entries",
        )?;
        let mut total = 0usize;
        for (name, source) in &self.sources {
            require(
                is_name(name)
                    && line(&source.title, 256)
                    && line(&source.uri, 2048)
                    && line(&source.revision, 512)
                    && text(&source.text, 65536),
                "Document source has invalid identity, revision or bounded text",
            )?;
            total = total
                .checked_add(source.text.len())
                .ok_or("Document source size overflow")?;
        }
        require(
            total <= 262144,
            "Document sources exceed the total text bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSectionV1 {
    pub heading: String,
    /// Plain text. The installed renderer escapes Markdown syntax before publication.
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentDraftV1 {
    pub schema: String,
    pub title: String,
    pub sections: Vec<DocumentSectionV1>,
    #[serde(deserialize_with = "super::unique_set")]
    pub citations: BTreeSet<String>,
}
impl DocumentDraftV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.document-draft/1"
                && line(&self.title, 256)
                && !self.sections.is_empty()
                && self.sections.len() <= 32
                && self.citations.len() <= 32
                && self.citations.iter().all(|s| is_name(s)),
            "Document draft requires bounded sections and named source citations",
        )?;
        let mut headings = BTreeSet::new();
        let mut total = self.title.len();
        for section in &self.sections {
            require(
                line(&section.heading, 256)
                    && text(&section.body, 65536)
                    && headings.insert(&section.heading),
                "Document sections need distinct headings and plain text",
            )?;
            total = total
                .checked_add(section.heading.len() + section.body.len())
                .ok_or("Document size overflow")?;
        }
        require(
            total <= 262144,
            "Document draft exceeds the total text bound",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormatV1 {
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentV1 {
    pub schema: String,
    pub draft_id: String,
    pub sources_id: String,
    pub format: DocumentFormatV1,
    pub text: String,
}
impl DocumentV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.document/1"
                && is_digest(&self.draft_id)
                && is_digest(&self.sources_id)
                && text(&self.text, 1048576),
            "Document needs exact draft/source identities and bounded rendered text",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentCheckReceiptV1 {
    pub plan_id: String,
    pub document_id: String,
    pub sources_id: String,
    pub policy_id: String,
    pub checks: BTreeMap<String, ReceiptOutcomeV1>,
    pub outcome: ReceiptOutcomeV1,
}
impl DocumentCheckReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.plan_id,
                &self.document_id,
                &self.sources_id,
                &self.policy_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && !self.checks.is_empty()
                && self.checks.len() <= 32
                && self.checks.keys().all(|s| is_name(s)),
            "Document checks need exact identities and bounded named results",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentEvaluationV1 {
    pub document_id: String,
    pub sources_id: String,
    pub requirements_id: String,
    pub check_receipt_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub summary: String,
}
impl DocumentEvaluationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.document_id,
                &self.sources_id,
                &self.requirements_id,
                &self.check_receipt_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && text(&self.summary, 16384),
            "Document evaluation needs exact inputs and a bounded conclusion",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentVerificationV1 {
    pub invocation: super::execution::TaskInvocationV1,
    pub document_id: String,
    pub policy_id: String,
    pub check_receipt_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub evaluation_id: Option<String>,
    pub outcome: ReceiptOutcomeV1,
}
impl DocumentVerificationV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            [&self.document_id, &self.policy_id, &self.check_receipt_id]
                .into_iter()
                .all(|id| is_digest(id))
                && self.evaluation_id.as_deref().is_none_or(is_digest),
            "Document acceptance requires exact document, policy and receipt identities",
        )
    }
}
