//! Complete selected native Ledger inspection; the Round report view remains separate.
use super::{agent_loop::TaskFindingsV2, require};
use crate::{ArtifactEnvelope, FindingSetV1};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const REVIEW_LEDGER_V2: &str = "af/review-ledger@2";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewLedgerV2 {
    pub schema: String,
    pub inspection: TaskFindingsV2,
    /// Null for an incomplete Round. Recorded sibling results never become a partial Ledger.
    #[serde(deserialize_with = "Option::deserialize")]
    pub finding_set: Option<ArtifactEnvelope>,
    /// Original immutable claim envelopes, keyed by Report ID, including their real producer
    /// and original Snapshot. Current reviewer coverage does not replace claim provenance.
    pub reports: BTreeMap<String, ArtifactEnvelope>,
}
impl ReviewLedgerV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == REVIEW_LEDGER_V2,
            "Expected af/review-ledger@2",
        )?;
        self.inspection.validate()?;
        let Some(frame) = &self.finding_set else {
            return require(
                self.reports.is_empty()
                    && self.inspection.round.as_ref().is_none_or(|r| !r.complete),
                "Incomplete Ledger cannot carry canonical claims",
            );
        };
        frame.validate()?;
        require(
            frame.artifact_type == crate::contract::FINDING_SET_V1,
            "Ledger needs a FindingSet@1",
        )?;
        let set: FindingSetV1 =
            serde_json::from_value(frame.payload.clone()).map_err(|e| e.to_string())?;
        set.validate()?;
        let round = self
            .inspection
            .round
            .as_ref()
            .ok_or("Ledger needs a selected Round")?;
        let projected: BTreeMap<_, _> = round
            .reviewers
            .iter()
            .flat_map(|r| &r.findings)
            .map(|finding| (finding.finding_id.as_deref().unwrap_or_default(), finding))
            .collect();
        let count: usize = round.reviewers.iter().map(|r| r.findings.len()).sum();
        require(
            round.complete
                && set.round == round.round
                && frame.subject_snapshot_id.as_deref() == Some(round.snapshot_id.as_str())
                && projected.len() == set.findings.len()
                && count == projected.len()
                && set.findings.iter().all(|finding| {
                    projected
                        .get(finding.finding_id.as_str())
                        .is_some_and(|view| {
                            view.status.as_deref() == Some(finding.status.as_str())
                                && view.severity == finding.severity
                        })
                }),
            "Ledger must expose every selected canonical Finding with its current status and severity",
        )?;
        let required: std::collections::BTreeSet<_> =
            set.findings.iter().flat_map(|f| &f.report_ids).collect();
        require(
            required.len() == self.reports.len()
                && required.iter().all(|id| self.reports.contains_key(*id)),
            "Ledger must retain every original Report",
        )?;
        for (id, report) in &self.reports {
            report.validate()?;
            require(
                id == &report.artifact_id
                    && report.artifact_type == crate::contract::FINDING_REPORT_V1
                    && report
                        .subject_snapshot_id
                        .as_deref()
                        .is_some_and(crate::is_digest),
                "Ledger Report identity/type/Snapshot is invalid",
            )?;
            let claim: crate::FindingReport =
                serde_json::from_value(report.payload.clone()).map_err(|e| e.to_string())?;
            claim.validate()?;
        }
        Ok(())
    }
}
