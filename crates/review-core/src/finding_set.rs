//! `FindingSet@1` — one immutable, Subject-bound reducer projection.

use serde::{Deserialize, Serialize};

use crate::{Severity, is_digest};

/// The deterministic reducer implementation recorded in every new Finding Set.
pub const FINDING_REDUCER_VERSION: &str = "review.kernel/finding-reducer@1";

/// One Finding as exposed by an immutable `FindingSet@1` projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingSetEntryV1 {
    pub finding_id: String,
    pub status: String,
    pub severity: Severity,
    #[serde(default)]
    pub effective_severity: Option<Severity>,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub location_unrecorded: bool,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub source: String,
    pub last_seen_round: u32,
    pub report_ids: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Payload reduced at a ledger barrier. Provenance and the exact reduction inputs live in its
/// `ArtifactEnvelope`; this payload is the rebuildable state consumers need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingSetV1 {
    pub subject_id: String,
    pub round: u32,
    pub prior_finding_set_id: String,
    pub reducer_version: String,
    pub identity_policy: String,
    pub selected_report_ids: Vec<String>,
    pub relation_ids: Vec<String>,
    pub resolution_ids: Vec<String>,
    pub findings: Vec<FindingSetEntryV1>,
}

impl FindingSetV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_digest(&self.subject_id)
            || !is_digest(&self.prior_finding_set_id)
            || self
                .selected_report_ids
                .iter()
                .chain(&self.relation_ids)
                .chain(&self.resolution_ids)
                .any(|id| !is_digest(id))
        {
            return Err("FindingSet@1 contains an invalid artifact ID".into());
        }
        if self.round == 0
            || self.reducer_version != FINDING_REDUCER_VERSION
            || self.identity_policy.trim().is_empty()
        {
            return Err("FindingSet@1 contains invalid reducer authority".into());
        }
        if self.findings.iter().any(|finding| {
            finding.finding_id.trim().is_empty()
                || !matches!(
                    finding.status.as_str(),
                    "open" | "fixed" | "rejected" | "wontfix" | "contested"
                )
                || !matches!(finding.scope.as_str(), "in" | "out" | "unknown")
                || finding.title.trim().is_empty()
                || finding.body.trim().is_empty()
                || finding.source.trim().is_empty()
                || finding.file.as_deref().is_some_and(str::is_empty)
                || finding.line.is_some_and(|line| line < 1)
                || finding.fix.as_deref().is_some_and(str::is_empty)
                || finding
                    .confidence
                    .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
                || finding.last_seen_round == 0
                || finding.report_ids.iter().any(|id| !is_digest(id))
        }) {
            return Err("FindingSet@1 contains an invalid Finding projection".into());
        }
        Ok(())
    }
}
