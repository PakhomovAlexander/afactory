//! The common Task runtime and native Review domain. Canonical Ledger and shard contracts
//! are shared; execution never translates a Campaign into a Task.
mod head_delta;
pub mod scatter;
pub mod storage;
pub mod task;
pub mod task_lifecycle;
pub mod telemetry;
pub mod wake;
use af_contracts::{ReviewerResultRejection, ReviewerStageOutput};
use std::collections::BTreeSet;

fn reviewer_stage_output(value: serde_json::Value) -> Result<ReviewerStageOutput, String> {
    af_contracts::validate_reviewer_result_v2(&value)?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn finding_set_entries(ledger: &af_store::Ledger) -> Vec<af_contracts::FindingSetEntryV1> {
    ledger
        .finding_views()
        .iter()
        .map(|finding| {
            let (file, line, location_unrecorded) =
                if finding.identity_file == af_contracts::reviewer_result::CHANGE_WIDE_SENTINEL {
                    (None, None, false)
                } else if af_contracts::is_valid_repo_path(&finding.identity_file) {
                    (
                        Some(finding.identity_file.clone()),
                        finding.identity_line,
                        false,
                    )
                } else {
                    (None, None, true)
                };
            af_contracts::FindingSetEntryV1 {
                finding_id: finding.key.clone(),
                status: finding.status.as_str().to_string(),
                severity: finding.severity,
                effective_severity: finding.convergence_severity,
                scope: finding.convergence_scope_label().to_string(),
                file,
                line,
                location_unrecorded,
                title: finding.title.clone(),
                body: finding.body.clone(),
                fix: Some(finding.fix.clone()),
                confidence: finding.confidence,
                source: finding.source.clone(),
                last_seen_round: finding.last_seen_round,
                report_ids: finding
                    .reports
                    .iter()
                    .map(|report| {
                        report
                            .artifact_id
                            .clone()
                            .unwrap_or_else(|| report.report_id.clone())
                    })
                    .collect(),
            }
        })
        .collect()
}

fn reviewer_result_value(
    stage: &ReviewerStageOutput,
    assigned_finding_ids: &[String],
) -> Result<serde_json::Value, ReviewerResultRejection> {
    let value = serde_json::to_value(stage).map_err(|_| ReviewerResultRejection::ReportPayload)?;
    af_contracts::validate_reviewer_result_v2_classified(&value)?;
    let expected: BTreeSet<_> = assigned_finding_ids.iter().map(String::as_str).collect();
    let mut actual = BTreeSet::new();
    for disposition in &stage.dispositions {
        let finding_id = disposition.finding_id.as_str();
        if !actual.insert(finding_id) {
            return Err(ReviewerResultRejection::DuplicateDisposition);
        }
        if !expected.contains(finding_id) {
            return Err(ReviewerResultRejection::UnassignedDisposition);
        }
    }
    if actual != expected {
        return Err(ReviewerResultRejection::MissingDispositionCoverage);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewer_result_v2_requires_exact_disposition_coverage() {
        let stage = |ids: &[&str]| ReviewerStageOutput {
            reports: Vec::new(),
            benchmark_demands: Vec::new(),
            dispositions: ids
                .iter()
                .map(|id| af_contracts::reviewer_result::ReviewerDisposition {
                    finding_id: (*id).into(),
                    position: af_contracts::FindingDispositionPosition::NotReproduced,
                    reason: "the current Subject no longer reaches the failing branch".into(),
                })
                .collect(),
        };
        let assigned = vec!["finding:a".to_string(), "finding:b".to_string()];

        assert_eq!(
            reviewer_result_value(&stage(&["finding:a"]), &assigned).unwrap_err(),
            ReviewerResultRejection::MissingDispositionCoverage
        );
        assert_eq!(
            reviewer_result_value(&stage(&["finding:a", "finding:a"]), &assigned).unwrap_err(),
            ReviewerResultRejection::DuplicateDisposition
        );
        assert_eq!(
            reviewer_result_value(&stage(&["finding:a", "finding:outside"]), &assigned)
                .unwrap_err(),
            ReviewerResultRejection::UnassignedDisposition
        );

        let value = reviewer_result_value(&stage(&["finding:b", "finding:a"]), &assigned).unwrap();
        assert!(value.get("disputes").is_none());
        assert_eq!(value["dispositions"].as_array().unwrap().len(), 2);
    }
}
