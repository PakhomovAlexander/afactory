//! Document and Report Tasks share typed drafts and exact source references.
//! Only Reports bind repository citations to a source Snapshot.
//! Shape validation does not confer acceptance; installed domain checks own receipt identity.
use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DOCUMENT_SOURCES_V1: &str = "af/DocumentSources@1";
pub const DOCUMENT_DRAFT_V3: &str = "af/DocumentDraft@3";
pub const DOCUMENT_V1: &str = "af/Document@1";
/// The payload version of a rendered Document: its text is the canonical Markdown of its draft
/// under the rendering this version names. `af.document/2` escapes only what is Markdown markup
/// where it stands; a Document of `af.document/1`, every ASCII punctuation character escaped,
/// fails closed (ADR-0145). The artifact type keeps its name, which committed pipelines bind.
pub const DOCUMENT_SCHEMA: &str = "af.document/2";
pub const DOCUMENT_CHECK_RECEIPT_V1: &str = "af/DocumentCheckReceipt@1";
pub const DOCUMENT_EVALUATION_V1: &str = "af/DocumentEvaluation@1";
pub const DOCUMENT_VERIFICATION_V1: &str = "af/DocumentVerification@1";

pub(super) fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}
pub(super) fn line(value: &str, limit: usize) -> bool {
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

/// Citations retain their domain: captured source names or exact repository locations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftCitation {
    Source { name: String },
    Repository { location: RepositoryCitationV1 },
}
impl From<String> for DraftCitation {
    fn from(name: String) -> Self {
        Self::Source { name }
    }
}
impl From<&str> for DraftCitation {
    fn from(name: &str) -> Self {
        Self::Source { name: name.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentDraft {
    pub schema: String,
    pub title: String,
    pub sections: Vec<DocumentSectionV1>,
    #[serde(deserialize_with = "super::unique_set")]
    pub citations: BTreeSet<DraftCitation>,
}
impl DocumentDraft {
    pub fn named_citations(&self) -> impl Iterator<Item = &String> {
        self.citations.iter().filter_map(|citation| match citation {
            DraftCitation::Source { name } => Some(name),
            DraftCitation::Repository { .. } => None,
        })
    }
    pub fn repository_citations(&self) -> impl Iterator<Item = &RepositoryCitationV1> {
        self.citations.iter().filter_map(|citation| match citation {
            DraftCitation::Repository { location } => Some(location),
            DraftCitation::Source { .. } => None,
        })
    }
    pub fn validate_document(&self) -> Result<(), String> {
        self.validate()?;
        require(
            self.repository_citations().next().is_none(),
            "Document Tasks cannot cite a repository Snapshot",
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.document-draft/3"
                && line(&self.title, 256)
                && !self.sections.is_empty()
                && self.sections.len() <= 32
                && self.named_citations().count() <= 32
                && self.repository_citations().count() <= 64,
            "Document draft requires its current schema, bounded sections and typed citations",
        )?;
        for citation in &self.citations {
            match citation {
                DraftCitation::Source { name } => {
                    require(is_name(name), "Invalid captured source name")?
                }
                DraftCitation::Repository { location } => location.validate()?,
            }
        }
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

/// One cited repository location: a Manifest entry spelled exactly as
/// `af_contracts::encode_path` spells it, and optionally one 1-based line of it. The shape is
/// checked here; that the entry exists, is a text file and has the line is checked only against
/// the exact source Manifest of the Task that cites it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCitationV1 {
    pub path: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub line: Option<u64>,
}
impl RepositoryCitationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            line(&self.path, 4096)
                && self.path.trim() == self.path
                && self
                    .line
                    .is_none_or(|line| line >= 1 && super::safe_number(line)),
            "Repository citation needs one Manifest path and a positive line",
        )
    }
    /// `path` or `path:line`, as the renderer and diagnostics print it.
    pub fn display(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
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
            self.schema == DOCUMENT_SCHEMA
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
