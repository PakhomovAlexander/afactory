//! Importer for the shell harness's stage output.
//!
//! `/self-review-heavy` reviewers emit one JSON object per stage per round, validated by
//! `.agents/skills/self-review-heavy/scripts/findings.schema.json`. The acceptance corpus for
//! [`FindingReport`] is a set of frozen real review bundles under
//! `tools/review-kernel/fixtures/legacy/` — private review data, so the corpus ships only in
//! the hub it was captured in; the tests that read it skip with a notice when it is absent.
//! The bar it set stands: a contract that cannot ingest real reviewer output unchanged is the
//! wrong contract.
//!
//! Two places where the new contract is deliberately stricter than the old schema, both checked
//! against the corpus before being imposed:
//!
//! - `fix` was nullable and is now required. A claim with no proposed remedy is one a triager
//!   cannot act on. No real reviewer ever omitted it — 63 of 63 carry one.
//! - `file` was a required string, with the harness substituting the literal path
//!   `(change-wide)` when a reviewer left it empty. That sentinel shares a namespace with real
//!   paths, so it is dropped in favour of an empty location list.

use serde::{Deserialize, Serialize};

use crate::finding::{FindingReport, Location, Severity};

/// The sentinel the shell harness wrote into the path field for a change-wide finding.
pub const CHANGE_WIDE_SENTINEL: &str = "(change-wide)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyVerdict {
    Approve,
    RequestChanges,
    Block,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyFinding {
    pub severity: Severity,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub body: String,
    pub fix: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyBenchmarkDemand {
    pub claim: String,
    pub why: String,
    pub suggested_method: String,
}

/// A reviewer's position on an existing claim, keyed by the legacy 12-hex fingerprint. These
/// become explicit `corroborates`/`disputes` relations once Findings have canonical IDs; the
/// fingerprint alone cannot name one, which is the whole reason the new model keeps relations
/// explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyDispute {
    /// The v1 contract says `claim_id`; the legacy harness said `fp`. Both parse into the
    /// same slot — a model following the newer contract must not have its disputes refused.
    #[serde(alias = "claim_id")]
    pub fp: String,
    pub position: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyStageOutput {
    pub verdict: LegacyVerdict,
    pub summary: Option<String>,
    pub findings: Vec<LegacyFinding>,
    pub benchmark_demands: Vec<LegacyBenchmarkDemand>,
    pub disputes: Vec<LegacyDispute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyImportError {
    /// Index of the offending finding within the stage output.
    pub index: usize,
    pub reason: ImportReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportReason {
    /// The legacy schema allowed a null fix; the contract requires a remedy.
    MissingFix,
    EmptyTitle,
    EmptyBody,
    /// A line number that is not a positive 32-bit value.
    InvalidLine,
    /// Outside 0.0..=1.0.
    ConfidenceOutOfRange,
}

impl std::fmt::Display for LegacyImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.reason {
            ImportReason::MissingFix => "no fix: FindingReport@1 requires a proposed remedy",
            ImportReason::EmptyTitle => "empty title",
            ImportReason::EmptyBody => "empty body",
            ImportReason::InvalidLine => "line is not a positive 32-bit number",
            ImportReason::ConfidenceOutOfRange => "confidence outside 0.0..=1.0",
        };
        write!(f, "finding {}: {what}", self.index)
    }
}

impl std::error::Error for LegacyImportError {}

impl LegacyFinding {
    /// Validate one legacy finding against the `FindingReport@1` contract and convert it.
    /// The live ledger ingest calls this per finding so the contract governs what a run
    /// actually produces, not only the acceptance corpus.
    pub fn into_report(self, index: usize) -> Result<FindingReport, LegacyImportError> {
        let err = |reason| LegacyImportError { index, reason };

        let fix = self.fix.unwrap_or_default();
        if fix.trim().is_empty() {
            return Err(err(ImportReason::MissingFix));
        }
        if self.title.trim().is_empty() {
            return Err(err(ImportReason::EmptyTitle));
        }
        if self.body.trim().is_empty() {
            return Err(err(ImportReason::EmptyBody));
        }

        let confidence = self.confidence.unwrap_or(0.0);
        if !(0.0..=1.0).contains(&confidence) {
            return Err(err(ImportReason::ConfidenceOutOfRange));
        }

        let path = self.file.trim();
        let locations = if path.is_empty() || path == CHANGE_WIDE_SENTINEL {
            Vec::new()
        } else {
            let line = match self.line {
                None => None,
                Some(n) => Some(u32::try_from(n).map_err(|_| err(ImportReason::InvalidLine))?),
            };
            if line == Some(0) {
                return Err(err(ImportReason::InvalidLine));
            }
            vec![Location {
                path: path.to_string(),
                line,
                end_line: None,
            }]
        };

        Ok(FindingReport {
            title: self.title,
            severity: self.severity,
            locations,
            body: self.body,
            fix,
            confidence,
            failure_trace: None,
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
        })
    }
}

impl LegacyStageOutput {
    /// Convert every finding in this stage output into a report.
    ///
    /// Deliberately all-or-nothing per stage: the shell harness skipped an unusable finding and
    /// ingested its siblings, which is right for a batch it cannot re-request, but an importer
    /// that silently drops claims would make the migration's ledger-equivalence test meaningless.
    pub fn into_reports(self) -> Result<Vec<FindingReport>, LegacyImportError> {
        self.findings
            .into_iter()
            .enumerate()
            .map(|(index, finding)| finding.into_report(index))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> LegacyFinding {
        LegacyFinding {
            severity: Severity::Major,
            file: "src/a.rs".into(),
            line: Some(12),
            title: "Retry loop can spin forever".into(),
            body: "no backoff, no cap".into(),
            fix: Some("cap the retries".into()),
            confidence: Some(0.9),
        }
    }

    #[test]
    fn maps_path_and_line_to_one_location() {
        let report = finding().into_report(0).unwrap();
        assert_eq!(report.locations, vec![Location::at("src/a.rs", 12)]);
        assert!(!report.is_change_wide());
    }

    #[test]
    fn change_wide_sentinel_becomes_no_location() {
        let report = LegacyFinding {
            file: CHANGE_WIDE_SENTINEL.into(),
            line: None,
            ..finding()
        }
        .into_report(0)
        .unwrap();
        assert!(report.is_change_wide());
        assert!(report.locations.is_empty());
    }

    #[test]
    fn a_null_fix_is_refused() {
        let err = LegacyFinding {
            fix: None,
            ..finding()
        }
        .into_report(3)
        .unwrap_err();
        assert_eq!(err.reason, ImportReason::MissingFix);
        assert_eq!(err.index, 3);
    }

    #[test]
    fn one_bad_finding_fails_the_whole_stage() {
        let stage = LegacyStageOutput {
            verdict: LegacyVerdict::RequestChanges,
            summary: None,
            findings: vec![
                finding(),
                LegacyFinding {
                    fix: None,
                    ..finding()
                },
            ],
            benchmark_demands: Vec::new(),
            disputes: Vec::new(),
        };
        assert!(stage.into_reports().is_err());
    }
}
