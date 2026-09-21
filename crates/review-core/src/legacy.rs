//! Importer for the shell harness's stage output.
//!
//! `/self-review-heavy` reviewers emit one JSON object per stage per round, validated by
//! `.agents/skills/self-review-heavy/scripts/findings.schema.json`. The acceptance corpus for
//! [`FindingReport`] is a set of frozen real review bundles under
//! `tools/review-kernel/fixtures/legacy/` — private review data, so the corpus ships only in
//! the hub it was captured in, so the tests that read it are `#[ignore]`d rather than skipped
//! at runtime — cargo shows `ignored`, where a runtime skip would print `ok`.
//! The bar it set stands: a contract that cannot ingest real reviewer output unchanged is the
//! wrong contract.
//!
//! Two places where the new contract is deliberately stricter than the old schema, both checked
//! against the corpus before being imposed:
//!
//! - `fix` was nullable and is now required. A claim with no proposed remedy is one a triager
//!   cannot act on. No real reviewer omitted it: every finding in the proving corpus
//!   carried one, so requiring it lost nothing.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_key: Option<String>,
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

/// The selected result wire contract for one reviewer node. The runner keeps one tolerant
/// internal stage shape, while the durable wire types remain explicitly versioned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewerResultContract {
    #[serde(rename = "review.kernel/ReviewerResult@1")]
    #[default]
    V1,
    #[serde(rename = "review.kernel/ReviewerResult@2")]
    V2,
}

impl ReviewerResultContract {
    pub const fn artifact_type(self) -> &'static str {
        match self {
            Self::V1 => crate::contract::REVIEWER_RESULT_V1,
            Self::V2 => crate::contract::REVIEWER_RESULT_V2,
        }
    }

    pub fn parse_artifact_type(value: &str) -> Option<Self> {
        match value {
            crate::contract::REVIEWER_RESULT_V1 => Some(Self::V1),
            crate::contract::REVIEWER_RESULT_V2 => Some(Self::V2),
            _ => None,
        }
    }
}

/// Closed, kernel-owned reasons a `ReviewerResult@1` can be refused at admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerResultRejection {
    NotObject,
    UnexpectedFields,
    TopLevelPayload,
    ReportPayload,
    MissingFix,
    EmptyTitle,
    EmptyBody,
    NoncanonicalReportPath,
    InvalidLine,
    ConfidenceOutOfRange,
    InvalidRuleId,
    EmptyOccurrenceKey,
    MalformedBenchmarkDemand,
    EmptyBenchmarkDemand,
    MalformedDispute,
    InvalidDispute,
    MalformedDisposition,
    InvalidDisposition,
    MissingDispositionCoverage,
    DuplicateDisposition,
    UnassignedDisposition,
}

impl ReviewerResultRejection {
    /// Stable code safe to place in retry prompts: it contains no reviewer-controlled bytes.
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotObject => "not_object",
            Self::UnexpectedFields => "unexpected_or_missing_fields",
            Self::TopLevelPayload => "invalid_top_level_payload",
            Self::ReportPayload => "invalid_report_payload",
            Self::MissingFix => "missing_fix",
            Self::EmptyTitle => "empty_title",
            Self::EmptyBody => "empty_body",
            Self::NoncanonicalReportPath => "noncanonical_report_path",
            Self::InvalidLine => "invalid_line",
            Self::ConfidenceOutOfRange => "confidence_out_of_range",
            Self::InvalidRuleId => "invalid_rule_id",
            Self::EmptyOccurrenceKey => "empty_occurrence_key",
            Self::MalformedBenchmarkDemand => "malformed_benchmark_demand",
            Self::EmptyBenchmarkDemand => "empty_benchmark_demand",
            Self::MalformedDispute => "malformed_dispute",
            Self::InvalidDispute => "invalid_dispute",
            Self::MalformedDisposition => "malformed_disposition",
            Self::InvalidDisposition => "invalid_disposition",
            Self::MissingDispositionCoverage => "missing_disposition_coverage",
            Self::DuplicateDisposition => "duplicate_disposition",
            Self::UnassignedDisposition => "unassigned_disposition",
        }
    }
}

impl std::fmt::Display for ReviewerResultRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reviewer result admission refused: {}", self.code())
    }
}

impl std::error::Error for ReviewerResultRejection {}

/// Validate the produced flat `ReviewerResult@1` wire value in the crate that owns its Rust
/// report types. Persistence and pipeline admission both call this one rule.
pub fn validate_reviewer_result(value: &serde_json::Value) -> Result<(), String> {
    validate_reviewer_result_classified(value).map_err(|error| error.to_string())
}

/// Validate a result while retaining a stable, reviewer-byte-free rejection classification.
pub fn validate_reviewer_result_classified(
    value: &serde_json::Value,
) -> Result<(), ReviewerResultRejection> {
    let object = value
        .as_object()
        .ok_or(ReviewerResultRejection::NotObject)?;
    exact_reviewer_keys(
        object,
        &[
            "verdict",
            "summary",
            "reports",
            "benchmark_demands",
            "disputes",
        ],
        ReviewerResultRejection::UnexpectedFields,
    )?;
    if !matches!(
        value["verdict"].as_str(),
        Some("approve" | "request-changes" | "block")
    ) || value["reports"]
        .as_array()
        .is_none_or(|reports| reports.iter().any(|report| !report.is_object()))
        || (!value["summary"].is_null() && value["summary"].as_str().is_none())
        || value["benchmark_demands"].as_array().is_none()
        || value["disputes"].as_array().is_none()
    {
        return Err(ReviewerResultRejection::TopLevelPayload);
    }
    for (index, report) in value["reports"]
        .as_array()
        .expect("top-level contract checked reports")
        .iter()
        .enumerate()
    {
        let legacy: LegacyFinding = serde::Deserialize::deserialize(report)
            .map_err(|_| ReviewerResultRejection::ReportPayload)?;
        legacy.validate(index).map_err(|error| match error.reason {
            ImportReason::MissingFix => ReviewerResultRejection::MissingFix,
            ImportReason::EmptyTitle => ReviewerResultRejection::EmptyTitle,
            ImportReason::EmptyBody => ReviewerResultRejection::EmptyBody,
            ImportReason::InvalidPath => ReviewerResultRejection::NoncanonicalReportPath,
            ImportReason::InvalidLine => ReviewerResultRejection::InvalidLine,
            ImportReason::ConfidenceOutOfRange => ReviewerResultRejection::ConfidenceOutOfRange,
            ImportReason::InvalidRuleId => ReviewerResultRejection::InvalidRuleId,
            ImportReason::EmptyOccurrenceKey => ReviewerResultRejection::EmptyOccurrenceKey,
            ImportReason::ReportContract => ReviewerResultRejection::ReportPayload,
        })?;
    }
    for demand in value["benchmark_demands"]
        .as_array()
        .expect("top-level contract checked demands")
    {
        let demand = demand
            .as_object()
            .ok_or(ReviewerResultRejection::MalformedBenchmarkDemand)?;
        exact_reviewer_keys(
            demand,
            &["claim", "why", "suggested_method"],
            ReviewerResultRejection::UnexpectedFields,
        )?;
        if demand
            .values()
            .any(|field| field.as_str().is_none_or(str::is_empty))
        {
            return Err(ReviewerResultRejection::EmptyBenchmarkDemand);
        }
    }
    for dispute in value["disputes"]
        .as_array()
        .expect("top-level contract checked disputes")
    {
        let dispute = dispute
            .as_object()
            .ok_or(ReviewerResultRejection::MalformedDispute)?;
        exact_reviewer_keys(
            dispute,
            &["claim_id", "position", "reason"],
            ReviewerResultRejection::UnexpectedFields,
        )?;
        if dispute["claim_id"].as_str().is_none_or(str::is_empty)
            || !matches!(dispute["position"].as_str(), Some("confirm" | "refute"))
            || dispute["reason"].as_str().is_none_or(str::is_empty)
        {
            return Err(ReviewerResultRejection::InvalidDispute);
        }
    }
    Ok(())
}

/// Validate the additive `ReviewerResult@2` wire value. Coverage of the exact assigned prior
/// Finding Set is deliberately enforced by the pipeline, which owns that input authority.
pub fn validate_reviewer_result_v2(value: &serde_json::Value) -> Result<(), String> {
    validate_reviewer_result_v2_classified(value)
        .map_err(|error| format!("ReviewerResult@2 admission refused: {}", error.code()))
}

pub fn validate_reviewer_result_v2_classified(
    value: &serde_json::Value,
) -> Result<(), ReviewerResultRejection> {
    let object = value
        .as_object()
        .ok_or(ReviewerResultRejection::NotObject)?;
    exact_reviewer_keys(
        object,
        &[
            "verdict",
            "summary",
            "reports",
            "benchmark_demands",
            "dispositions",
        ],
        ReviewerResultRejection::UnexpectedFields,
    )?;
    if !matches!(
        value["verdict"].as_str(),
        Some("approve" | "request-changes" | "block")
    ) || value["reports"]
        .as_array()
        .is_none_or(|reports| reports.iter().any(|report| !report.is_object()))
        || (!value["summary"].is_null() && value["summary"].as_str().is_none())
        || value["benchmark_demands"].as_array().is_none()
        || value["dispositions"].as_array().is_none()
    {
        return Err(ReviewerResultRejection::TopLevelPayload);
    }
    validate_reports_and_demands(value)?;
    for disposition in value["dispositions"]
        .as_array()
        .expect("top-level contract checked dispositions")
    {
        let disposition = disposition
            .as_object()
            .ok_or(ReviewerResultRejection::MalformedDisposition)?;
        exact_reviewer_keys(
            disposition,
            &["finding_id", "position", "reason"],
            ReviewerResultRejection::UnexpectedFields,
        )?;
        if disposition["finding_id"].as_str().is_none_or(str::is_empty)
            || !matches!(
                disposition["position"].as_str(),
                Some("corroborate" | "not_reproduced" | "dispute")
            )
            || disposition["reason"].as_str().is_none_or(str::is_empty)
        {
            return Err(ReviewerResultRejection::InvalidDisposition);
        }
    }
    Ok(())
}

fn validate_reports_and_demands(value: &serde_json::Value) -> Result<(), ReviewerResultRejection> {
    for (index, report) in value["reports"]
        .as_array()
        .expect("top-level contract checked reports")
        .iter()
        .enumerate()
    {
        let legacy: LegacyFinding = serde::Deserialize::deserialize(report)
            .map_err(|_| ReviewerResultRejection::ReportPayload)?;
        legacy.validate(index).map_err(|error| match error.reason {
            ImportReason::MissingFix => ReviewerResultRejection::MissingFix,
            ImportReason::EmptyTitle => ReviewerResultRejection::EmptyTitle,
            ImportReason::EmptyBody => ReviewerResultRejection::EmptyBody,
            ImportReason::InvalidPath => ReviewerResultRejection::NoncanonicalReportPath,
            ImportReason::InvalidLine => ReviewerResultRejection::InvalidLine,
            ImportReason::ConfidenceOutOfRange => ReviewerResultRejection::ConfidenceOutOfRange,
            ImportReason::InvalidRuleId => ReviewerResultRejection::InvalidRuleId,
            ImportReason::EmptyOccurrenceKey => ReviewerResultRejection::EmptyOccurrenceKey,
            ImportReason::ReportContract => ReviewerResultRejection::ReportPayload,
        })?;
    }
    for demand in value["benchmark_demands"]
        .as_array()
        .expect("top-level contract checked demands")
    {
        let demand = demand
            .as_object()
            .ok_or(ReviewerResultRejection::MalformedBenchmarkDemand)?;
        exact_reviewer_keys(
            demand,
            &["claim", "why", "suggested_method"],
            ReviewerResultRejection::UnexpectedFields,
        )?;
        if demand
            .values()
            .any(|field| field.as_str().is_none_or(str::is_empty))
        {
            return Err(ReviewerResultRejection::EmptyBenchmarkDemand);
        }
    }
    Ok(())
}

fn exact_reviewer_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &[&str],
    rejection: ReviewerResultRejection,
) -> Result<(), ReviewerResultRejection> {
    if object.len() != expected.len() || object.keys().any(|key| !expected.contains(&key.as_str()))
    {
        return Err(rejection);
    }
    Ok(())
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
    /// A non-empty location that is not a canonical repository-relative path.
    InvalidPath,
    /// A line number that is not a positive 32-bit value.
    InvalidLine,
    /// Outside 0.0..=1.0.
    ConfidenceOutOfRange,
    InvalidRuleId,
    EmptyOccurrenceKey,
    /// A future FindingReport invariant not represented by this compatibility shape.
    ReportContract,
}

impl std::fmt::Display for LegacyImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.reason {
            ImportReason::MissingFix => "no fix: FindingReport@1 requires a proposed remedy",
            ImportReason::EmptyTitle => "empty title",
            ImportReason::EmptyBody => "empty body",
            ImportReason::InvalidPath => "file is not a canonical repository-relative path",
            ImportReason::InvalidLine => "line is not a positive 32-bit number",
            ImportReason::ConfidenceOutOfRange => "confidence outside 0.0..=1.0",
            ImportReason::InvalidRuleId => "rule_id is not a namespaced versioned rule",
            ImportReason::EmptyOccurrenceKey => "occurrence_key is empty",
            ImportReason::ReportContract => "finding violates the FindingReport@1 contract",
        };
        write!(f, "finding {}: {what}", self.index)
    }
}

impl std::error::Error for LegacyImportError {}

impl LegacyFinding {
    /// Validate one legacy-shaped report without cloning or converting its owned text.
    pub fn validate(&self, index: usize) -> Result<(), LegacyImportError> {
        let err = |reason| LegacyImportError { index, reason };
        if self.fix.as_deref().is_none_or(|fix| fix.trim().is_empty()) {
            return Err(err(ImportReason::MissingFix));
        }
        if self.title.trim().is_empty() {
            return Err(err(ImportReason::EmptyTitle));
        }
        if self.body.trim().is_empty() {
            return Err(err(ImportReason::EmptyBody));
        }
        if self
            .confidence
            .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
        {
            return Err(err(ImportReason::ConfidenceOutOfRange));
        }
        if self
            .rule_id
            .as_deref()
            .is_some_and(|rule| !crate::finding::valid_rule_id(rule))
        {
            return Err(err(ImportReason::InvalidRuleId));
        }
        if self.occurrence_key.as_deref().is_some_and(str::is_empty) {
            return Err(err(ImportReason::EmptyOccurrenceKey));
        }
        let line = self
            .line
            .map(|line| u32::try_from(line).map_err(|_| err(ImportReason::InvalidLine)))
            .transpose()?;
        if line == Some(0) {
            return Err(err(ImportReason::InvalidLine));
        }
        let path = self.file.as_str();
        if path.is_empty() || path == CHANGE_WIDE_SENTINEL {
            if line.is_some() {
                return Err(err(ImportReason::InvalidLine));
            }
        } else if !crate::is_valid_repo_path(path) {
            return Err(err(ImportReason::InvalidPath));
        }
        Ok(())
    }

    /// Validate one legacy finding against the `FindingReport@1` contract and convert it.
    /// The live ledger ingest calls this per finding so the contract governs what a run
    /// actually produces, not only the acceptance corpus.
    pub fn into_report(self, index: usize) -> Result<FindingReport, LegacyImportError> {
        let err = |reason| LegacyImportError { index, reason };

        self.validate(index)?;

        let fix = self.fix.unwrap_or_default();

        let confidence = self.confidence.unwrap_or(0.0);

        let line = match self.line {
            None => None,
            Some(n) => Some(u32::try_from(n).map_err(|_| err(ImportReason::InvalidLine))?),
        };

        let path = self.file.as_str();
        let locations = if path.is_empty() || path == CHANGE_WIDE_SENTINEL {
            Vec::new()
        } else {
            vec![Location {
                path: path.to_string(),
                line,
                end_line: None,
            }]
        };

        let report = FindingReport {
            title: self.title,
            severity: self.severity,
            locations,
            body: self.body,
            fix,
            confidence,
            failure_trace: None,
            rule_id: self.rule_id,
            occurrence_key: self.occurrence_key,
            relations: Vec::new(),
        };
        report
            .validate()
            .map_err(|_| err(ImportReason::ReportContract))?;
        Ok(report)
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
            rule_id: None,
            occurrence_key: None,
        }
    }

    #[test]
    fn maps_path_and_line_to_one_location() {
        let report = finding().into_report(0).unwrap();
        assert_eq!(report.locations, vec![Location::at("src/a.rs", 12)]);
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
    fn whitespace_only_claim_content_is_not_admissible() {
        for (title, body, fix, expected) in [
            ("   ", "body", Some("fix"), ImportReason::EmptyTitle),
            ("title", "\n\t", Some("fix"), ImportReason::EmptyBody),
            ("title", "body", Some("  "), ImportReason::MissingFix),
        ] {
            let mut candidate = finding();
            candidate.title = title.into();
            candidate.body = body.into();
            candidate.fix = fix.map(str::to_string);
            assert_eq!(candidate.validate(0).unwrap_err().reason, expected);
        }
    }

    #[test]
    fn stable_claim_identity_is_validated_at_reviewer_result_admission() {
        let result = |rule_id: &str, occurrence_key: &str| {
            serde_json::json!({
                "verdict": "request-changes",
                "summary": null,
                "reports": [{
                    "severity": "major",
                    "file": "src/a.rs",
                    "line": 12,
                    "title": "Retry loop can spin forever",
                    "body": "no backoff, no cap",
                    "fix": "cap the retries",
                    "confidence": 0.9,
                    "rule_id": rule_id,
                    "occurrence_key": occurrence_key
                }],
                "benchmark_demands": [],
                "disputes": []
            })
        };

        assert_eq!(
            validate_reviewer_result_classified(&result("test.rules/loop_safety@1", "main-loop")),
            Err(ReviewerResultRejection::InvalidRuleId)
        );
        assert_eq!(
            validate_reviewer_result_classified(&result("test.rules/loop-safety@1", "")),
            Err(ReviewerResultRejection::EmptyOccurrenceKey)
        );
        validate_reviewer_result_classified(&result("test.rules/loop-safety@1", "main-loop"))
            .unwrap();

        let mut malformed = finding();
        malformed.rule_id = Some("test.rules/loop_safety@1".into());
        assert_eq!(
            malformed.into_report(4).unwrap_err(),
            LegacyImportError {
                index: 4,
                reason: ImportReason::InvalidRuleId,
            }
        );
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
