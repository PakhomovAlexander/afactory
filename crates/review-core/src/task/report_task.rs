//! Report Tasks: a Document written against one exact source Snapshot and accepted by an
//! independent verifier. Every receipt here names that Snapshot, so a check, an evaluation and
//! an acceptance of another tree can never be combined into one report's acceptance.
use super::document::{DocumentSourceV1, RepositoryCitationV1, line, text};
use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const REPORT_SOURCES_V1: &str = "af/ReportSources@1";
pub const REPORT_CHECK_RECEIPT_V1: &str = "af/ReportCheckReceipt@1";
pub const REPORT_EVALUATION_V1: &str = "af/ReportEvaluation@1";
pub const REPORT_VERIFICATION_V1: &str = "af/ReportVerification@1";

/// The most captured entries one report may read.
pub const MAX_REPORT_SOURCES: usize = 256;
/// The most text one captured entry may carry.
pub const MAX_REPORT_SOURCE_BYTES: usize = 256 * 1024;
/// The most text all captured entries together may carry: a report's context — these sources,
/// its requirements, any bound measurements — must fit the 1 MiB Worker request.
pub const MAX_REPORT_SOURCES_BYTES: usize = 512 * 1024;
/// The most bytes the captured sources file may hold: the text bound plus room for the entries'
/// titles, URIs and revisions. Checked before the file is parsed.
pub const MAX_REPORT_SOURCES_FILE_BYTES: usize = MAX_REPORT_SOURCES_BYTES + 128 * 1024;

/// A report's captured sources, in the `af.document-sources/1` file shape the Document profile
/// reads, with the report's own bounds: zero to 256 entries, each at most 256 KiB of text and
/// at most 512 KiB in total. Entries are data; they never become execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSourcesV1 {
    pub schema: String,
    pub sources: BTreeMap<String, DocumentSourceV1>,
}
impl ReportSourcesV1 {
    /// The set a report Task without `report_sources` reads.
    pub fn empty() -> Self {
        Self {
            schema: "af.document-sources/1".into(),
            sources: BTreeMap::new(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.document-sources/1" && self.sources.len() <= MAX_REPORT_SOURCES,
            "Report sources hold zero to 256 entries in the af.document-sources/1 shape",
        )?;
        let mut total = 0usize;
        for (name, source) in &self.sources {
            require(
                is_name(name)
                    && line(&source.title, 256)
                    && line(&source.uri, 2048)
                    && line(&source.revision, 512)
                    && text(&source.text, MAX_REPORT_SOURCE_BYTES),
                "Report source has an invalid identity or revision, or text over 256 KiB",
            )?;
            total = total
                .checked_add(source.text.len())
                .ok_or("Report source size overflow")?;
        }
        require(
            total <= MAX_REPORT_SOURCES_BYTES,
            "Report sources exceed their 512 KiB total text bound",
        )
    }
}

/// Why one repository citation does not resolve against the exact source Manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportCitationFailureReasonV1 {
    /// No Manifest entry has this spelling, and no entry lies beneath it.
    Absent,
    /// Entries lie beneath the path: it names a directory, not a file.
    Directory,
    /// The entry is a symbolic link, whose target the Manifest does not vouch for.
    Symlink,
    /// The entry's first 8 KiB hold a NUL byte.
    Binary,
    /// The cited line is past the end of the file.
    LineOutOfRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportCitationFailureV1 {
    pub citation: RepositoryCitationV1,
    pub reason: ReportCitationFailureReasonV1,
}

/// The report's Document checks plus its repository citations, judged against one exact source
/// Snapshot and the Manifest that Snapshot names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportCheckReceiptV1 {
    pub plan_id: String,
    pub document_id: String,
    pub sources_id: String,
    pub policy_id: String,
    pub source_snapshot_id: String,
    pub manifest_id: String,
    pub checks: BTreeMap<String, ReceiptOutcomeV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_failures: Vec<ReportCitationFailureV1>,
    pub outcome: ReceiptOutcomeV1,
}
impl ReportCheckReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.plan_id,
                &self.document_id,
                &self.sources_id,
                &self.policy_id,
                &self.source_snapshot_id,
                &self.manifest_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && !self.checks.is_empty()
                && self.checks.len() <= 32
                && self.checks.keys().all(|s| is_name(s))
                && self.citation_failures.len() <= 64,
            "Report checks need exact identities, their Snapshot and bounded named results",
        )?;
        for failure in &self.citation_failures {
            failure.citation.validate()?;
        }
        Ok(())
    }
}

/// The independent verifier's judgement of one report against the Task's requirements, the
/// report's passing checks and the same source Snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportEvaluationV1 {
    pub document_id: String,
    pub sources_id: String,
    pub requirements_id: String,
    pub check_receipt_id: String,
    pub source_snapshot_id: String,
    pub outcome: ReceiptOutcomeV1,
    pub summary: String,
}
impl ReportEvaluationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.document_id,
                &self.sources_id,
                &self.requirements_id,
                &self.check_receipt_id,
                &self.source_snapshot_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && text(&self.summary, 16384),
            "Report evaluation needs exact inputs, its Snapshot and a bounded conclusion",
        )
    }
}

/// Public report acceptance: the acceptance invocation, the exact Document, policy, check
/// receipt, selected evaluation and the source Snapshot all of them were bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportVerificationV1 {
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
    pub source_snapshot_id: String,
    pub outcome: ReceiptOutcomeV1,
}
impl ReportVerificationV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        require(
            [
                &self.document_id,
                &self.policy_id,
                &self.check_receipt_id,
                &self.source_snapshot_id,
            ]
            .into_iter()
            .all(|id| is_digest(id))
                && self.evaluation_id.as_deref().is_none_or(is_digest),
            "Report acceptance requires exact document, policy, receipt and Snapshot identities",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: String) -> DocumentSourceV1 {
        DocumentSourceV1 {
            title: "Task".into(),
            uri: "repo:README.md".into(),
            revision: "r1".into(),
            text,
        }
    }

    #[test]
    fn report_sources_admit_an_empty_set_and_refuse_their_bounds() {
        ReportSourcesV1::empty().validate().unwrap();
        let mut sources = ReportSourcesV1::empty();
        sources
            .sources
            .insert("one".into(), source("x".repeat(MAX_REPORT_SOURCE_BYTES)));
        sources.validate().unwrap();
        sources.sources.insert(
            "big".into(),
            source("x".repeat(MAX_REPORT_SOURCE_BYTES + 1)),
        );
        assert!(sources.validate().unwrap_err().contains("256 KiB"));
        // Three full entries are 768 KiB: every entry fits, the total does not.
        let mut total = ReportSourcesV1::empty();
        for n in 0..3 {
            total.sources.insert(
                format!("task-{n}"),
                source("x".repeat(MAX_REPORT_SOURCE_BYTES)),
            );
        }
        assert!(total.validate().unwrap_err().contains("512 KiB"));
        let mut many = ReportSourcesV1::empty();
        for n in 0..=MAX_REPORT_SOURCES {
            many.sources.insert(format!("s{n}"), source("x".into()));
        }
        assert!(many.validate().is_err());
        let mut schema = ReportSourcesV1::empty();
        schema.schema = "af.report-sources/1".into();
        assert!(schema.validate().is_err());
    }

    #[test]
    fn report_receipts_name_their_snapshot() {
        let id = format!("sha256:{}", "1".repeat(64));
        let mut receipt = ReportCheckReceiptV1 {
            plan_id: id.clone(),
            document_id: id.clone(),
            sources_id: id.clone(),
            policy_id: id.clone(),
            source_snapshot_id: id.clone(),
            manifest_id: id.clone(),
            checks: BTreeMap::from([("repository_citations".into(), ReceiptOutcomeV1::Failed)]),
            citation_failures: vec![ReportCitationFailureV1 {
                citation: RepositoryCitationV1 {
                    path: "src/lib.rs".into(),
                    line: Some(9),
                },
                reason: ReportCitationFailureReasonV1::LineOutOfRange,
            }],
            outcome: ReceiptOutcomeV1::Failed,
        };
        receipt.validate().unwrap();
        receipt.source_snapshot_id = "HEAD".into();
        assert!(receipt.validate().is_err());
        let evaluation = ReportEvaluationV1 {
            document_id: id.clone(),
            sources_id: id.clone(),
            requirements_id: id.clone(),
            check_receipt_id: id.clone(),
            source_snapshot_id: id,
            outcome: ReceiptOutcomeV1::Passed,
            summary: "Cited files read.".into(),
        };
        evaluation.validate().unwrap();
        let mut blank = evaluation;
        blank.summary = " ".into();
        assert!(blank.validate().is_err());
    }
}
