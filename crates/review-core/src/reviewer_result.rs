//! `ReviewerResult@2`: the flat result one reviewer Attempt returns, and its per-report bridge
//! to [`FindingReport`].
//!
//! The Rust types spell the wire shape exactly — `reports`, `benchmark_demands` and
//! `dispositions` — so a parsed result serializes straight back to the durable artifact with no
//! renaming step in between. [`ReviewerReport::into_report`] admits one flat report only if it
//! also satisfies the stricter typed contract:
//!
//! - `fix` parses as nullable but is required. A claim with no proposed remedy is one a triager
//!   cannot act on.
//! - An empty `file`, or the literal `(change-wide)` sentinel, is a change-wide claim. The
//!   sentinel shares a namespace with real paths, so the report carries an empty location list
//!   instead.

use serde::{Deserialize, Serialize};

use crate::disposition::FindingDispositionPosition;
use crate::finding::{FindingReport, Location, Severity};

/// The path-field sentinel for a change-wide finding.
pub const CHANGE_WIDE_SENTINEL: &str = "(change-wide)";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerReport {
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
pub struct BenchmarkDemand {
    pub claim: String,
    pub why: String,
    pub suggested_method: String,
}

/// A reviewer's position on one prior Finding it was assigned, named by that Finding's
/// canonical ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerDisposition {
    pub finding_id: String,
    pub position: FindingDispositionPosition,
    pub reason: String,
}

/// One reviewer Attempt's complete result, in the `ReviewerResult@2` wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerStageOutput {
    pub reports: Vec<ReviewerReport>,
    pub benchmark_demands: Vec<BenchmarkDemand>,
    pub dispositions: Vec<ReviewerDisposition>,
}

/// The result wire contract a reviewer node declares. The runner keeps one tolerant internal
/// stage shape, while the durable wire type stays explicitly versioned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewerResultContract {
    #[serde(rename = "review.kernel/ReviewerResult@2")]
    #[default]
    V2,
}

impl ReviewerResultContract {
    pub const fn artifact_type(self) -> &'static str {
        match self {
            Self::V2 => crate::contract::REVIEWER_RESULT_V2,
        }
    }

    pub fn parse_artifact_type(value: &str) -> Option<Self> {
        (value == crate::contract::REVIEWER_RESULT_V2).then_some(Self::V2)
    }
}

/// Closed, kernel-owned reasons a `ReviewerResult@2` can be refused at admission.
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

/// Validate the produced flat `ReviewerResult@2` wire value in the crate that owns its Rust
/// report types. Persistence and pipeline admission both call this one rule. Coverage of the
/// exact assigned prior Finding Set is deliberately enforced by the pipeline, which owns that
/// input authority.
pub fn validate_reviewer_result_v2(value: &serde_json::Value) -> Result<(), String> {
    validate_reviewer_result_v2_classified(value)
        .map_err(|error| format!("ReviewerResult@2 admission refused: {}", error.code()))
}

/// Validate a result while retaining a stable, reviewer-byte-free rejection classification.
pub fn validate_reviewer_result_v2_classified(
    value: &serde_json::Value,
) -> Result<(), ReviewerResultRejection> {
    let object = value
        .as_object()
        .ok_or(ReviewerResultRejection::NotObject)?;
    exact_reviewer_keys(
        object,
        &["reports", "benchmark_demands", "dispositions"],
        ReviewerResultRejection::UnexpectedFields,
    )?;
    if value["reports"]
        .as_array()
        .is_none_or(|reports| reports.iter().any(|report| !report.is_object()))
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
        let parsed: ReviewerReport = serde::Deserialize::deserialize(report)
            .map_err(|_| ReviewerResultRejection::ReportPayload)?;
        parsed.validate(index).map_err(|error| match error.reason {
            ReportAdmissionReason::MissingFix => ReviewerResultRejection::MissingFix,
            ReportAdmissionReason::EmptyTitle => ReviewerResultRejection::EmptyTitle,
            ReportAdmissionReason::EmptyBody => ReviewerResultRejection::EmptyBody,
            ReportAdmissionReason::InvalidPath => ReviewerResultRejection::NoncanonicalReportPath,
            ReportAdmissionReason::InvalidLine => ReviewerResultRejection::InvalidLine,
            ReportAdmissionReason::ConfidenceOutOfRange => {
                ReviewerResultRejection::ConfidenceOutOfRange
            }
            ReportAdmissionReason::InvalidRuleId => ReviewerResultRejection::InvalidRuleId,
            ReportAdmissionReason::EmptyOccurrenceKey => {
                ReviewerResultRejection::EmptyOccurrenceKey
            }
            ReportAdmissionReason::ReportContract => ReviewerResultRejection::ReportPayload,
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
pub struct ReportAdmissionError {
    /// Index of the offending report within the reviewer result.
    pub index: usize,
    pub reason: ReportAdmissionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportAdmissionReason {
    /// The wire shape allows a null fix; the contract requires a remedy.
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
    /// A `FindingReport@1` invariant the flat result shape cannot express.
    ReportContract,
}

impl std::fmt::Display for ReportAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.reason {
            ReportAdmissionReason::MissingFix => {
                "no fix: FindingReport@1 requires a proposed remedy"
            }
            ReportAdmissionReason::EmptyTitle => "empty title",
            ReportAdmissionReason::EmptyBody => "empty body",
            ReportAdmissionReason::InvalidPath => {
                "file is not a canonical repository-relative path"
            }
            ReportAdmissionReason::InvalidLine => "line is not a positive 32-bit number",
            ReportAdmissionReason::ConfidenceOutOfRange => "confidence outside 0.0..=1.0",
            ReportAdmissionReason::InvalidRuleId => "rule_id is not a namespaced versioned rule",
            ReportAdmissionReason::EmptyOccurrenceKey => "occurrence_key is empty",
            ReportAdmissionReason::ReportContract => "report violates the FindingReport@1 contract",
        };
        write!(f, "report {}: {what}", self.index)
    }
}

impl std::error::Error for ReportAdmissionError {}

impl ReviewerReport {
    /// Validate one flat report without cloning or converting its owned text.
    pub fn validate(&self, index: usize) -> Result<(), ReportAdmissionError> {
        let err = |reason| ReportAdmissionError { index, reason };
        if self.fix.as_deref().is_none_or(|fix| fix.trim().is_empty()) {
            return Err(err(ReportAdmissionReason::MissingFix));
        }
        if self.title.trim().is_empty() {
            return Err(err(ReportAdmissionReason::EmptyTitle));
        }
        if self.body.trim().is_empty() {
            return Err(err(ReportAdmissionReason::EmptyBody));
        }
        if self
            .confidence
            .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
        {
            return Err(err(ReportAdmissionReason::ConfidenceOutOfRange));
        }
        if self
            .rule_id
            .as_deref()
            .is_some_and(|rule| !crate::finding::valid_rule_id(rule))
        {
            return Err(err(ReportAdmissionReason::InvalidRuleId));
        }
        if self.occurrence_key.as_deref().is_some_and(str::is_empty) {
            return Err(err(ReportAdmissionReason::EmptyOccurrenceKey));
        }
        let line = self
            .line
            .map(|line| u32::try_from(line).map_err(|_| err(ReportAdmissionReason::InvalidLine)))
            .transpose()?;
        if line == Some(0) {
            return Err(err(ReportAdmissionReason::InvalidLine));
        }
        let path = self.file.as_str();
        if path.is_empty() || path == CHANGE_WIDE_SENTINEL {
            if line.is_some() {
                return Err(err(ReportAdmissionReason::InvalidLine));
            }
        } else if !crate::is_valid_repo_path(path) {
            return Err(err(ReportAdmissionReason::InvalidPath));
        }
        Ok(())
    }

    /// Validate one flat report against the `FindingReport@1` contract and convert it.
    /// Every ledger ingest calls this per report, so the contract governs what a run produces.
    pub fn into_report(self, index: usize) -> Result<FindingReport, ReportAdmissionError> {
        let err = |reason| ReportAdmissionError { index, reason };

        self.validate(index)?;

        let fix = self.fix.unwrap_or_default();

        let confidence = self.confidence.unwrap_or(0.0);

        let line = match self.line {
            None => None,
            Some(n) => Some(u32::try_from(n).map_err(|_| err(ReportAdmissionReason::InvalidLine))?),
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
            .map_err(|_| err(ReportAdmissionReason::ReportContract))?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> ReviewerReport {
        ReviewerReport {
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
        let report = report().into_report(0).unwrap();
        assert_eq!(report.locations, vec![Location::at("src/a.rs", 12)]);
    }

    #[test]
    fn change_wide_sentinel_becomes_no_location() {
        let report = ReviewerReport {
            file: CHANGE_WIDE_SENTINEL.into(),
            line: None,
            ..report()
        }
        .into_report(0)
        .unwrap();
        assert!(report.locations.is_empty());
    }

    #[test]
    fn a_null_fix_is_refused() {
        let err = ReviewerReport {
            fix: None,
            ..report()
        }
        .into_report(3)
        .unwrap_err();
        assert_eq!(err.reason, ReportAdmissionReason::MissingFix);
        assert_eq!(err.index, 3);
    }

    #[test]
    fn whitespace_only_claim_content_is_not_admissible() {
        for (title, body, fix, expected) in [
            (
                "   ",
                "body",
                Some("fix"),
                ReportAdmissionReason::EmptyTitle,
            ),
            (
                "title",
                "\n\t",
                Some("fix"),
                ReportAdmissionReason::EmptyBody,
            ),
            (
                "title",
                "body",
                Some("  "),
                ReportAdmissionReason::MissingFix,
            ),
        ] {
            let mut candidate = report();
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
                "dispositions": []
            })
        };

        assert_eq!(
            validate_reviewer_result_v2_classified(&result(
                "test.rules/loop_safety@1",
                "main-loop"
            )),
            Err(ReviewerResultRejection::InvalidRuleId)
        );
        assert_eq!(
            validate_reviewer_result_v2_classified(&result("test.rules/loop-safety@1", "")),
            Err(ReviewerResultRejection::EmptyOccurrenceKey)
        );
        validate_reviewer_result_v2_classified(&result("test.rules/loop-safety@1", "main-loop"))
            .unwrap();

        let mut malformed = report();
        malformed.rule_id = Some("test.rules/loop_safety@1".into());
        assert_eq!(
            malformed.into_report(4).unwrap_err(),
            ReportAdmissionError {
                index: 4,
                reason: ReportAdmissionReason::InvalidRuleId,
            }
        );
    }
}
