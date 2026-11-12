//! Native Ledger inspection reads the complete selected Finding Set, never the report filter.
use super::*;
use af_contracts::task::review_ledger::{REVIEW_LEDGER_V3, ReviewLedgerV3};

fn document(opened: &OpenedTask, wanted: Option<u64>) -> Result<ReviewLedgerV3, String> {
    let (inspection, selected) = findings::ledger_selection(opened, wanted)?;
    let mut finding_set = None;
    let mut reports = BTreeMap::new();
    if let Some(id) = selected {
        let frame = opened.cas.get_artifact(&id).map_err(|e| e.to_string())?;
        validate_envelope(&frame)?;
        let set: af_contracts::FindingSetV1 =
            serde_json::from_value(frame.payload.clone()).map_err(|e| e.to_string())?;
        for id in set.findings.iter().flat_map(|f| &f.report_ids) {
            if let std::collections::btree_map::Entry::Vacant(entry) = reports.entry(id.clone()) {
                let report = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
                validate_envelope(&report)?;
                entry.insert(report);
            }
        }
        finding_set = Some(frame);
    }
    let ledger = ReviewLedgerV3 {
        schema: REVIEW_LEDGER_V3.into(),
        inspection,
        finding_set,
        reports,
    };
    ledger.validate()?;
    Ok(ledger)
}

pub(crate) fn print(
    id: &str,
    round: Option<u64>,
    format: crate::cli::FindingsFormatArg,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    use crate::cli::FindingsFormatArg as Format;
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let ledger = document(&opened, round)?;
    match (inspect.json, format) {
        (true, _) | (_, Format::Json) => println!(
            "{}",
            serde_json::to_string(&ledger).map_err(|e| e.to_string())?
        ),
        (_, Format::Text) => {
            for line in findings::lines(&ledger.inspection) {
                println!("{line}");
            }
            if let Some(set) = &ledger.finding_set {
                println!("Finding Set: {}", set.artifact_id);
                let canonical: af_contracts::FindingSetV1 =
                    serde_json::from_value(set.payload.clone()).map_err(|e| e.to_string())?;
                for finding in canonical.findings {
                    println!(
                        "Finding {}: {}; Reports {}",
                        finding.finding_id,
                        finding.status,
                        finding.report_ids.join(", ")
                    );
                }
            }
            for (id, claim) in &ledger.reports {
                println!(
                    "Report {id} on Snapshot {}; producer {}",
                    claim.subject_snapshot_id.as_deref().unwrap_or("unknown"),
                    serde_json::to_string(&claim.producer).map_err(|e| e.to_string())?
                );
            }
        }
        // SARIF keeps its existing shape/profile. Prompts target the selected Round's
        // Snapshot; complete original Report/Snapshot/producer provenance travels in messages.
        (_, Format::Sarif) => {
            let mut log = crate::sarif::document(&findings::sarif(&ledger.inspection));
            if let Some(frame) = &ledger.finding_set {
                let set: af_contracts::FindingSetV1 =
                    serde_json::from_value(frame.payload.clone()).map_err(|e| e.to_string())?;
                let canonical: BTreeMap<_, _> = set
                    .findings
                    .iter()
                    .map(|f| (f.finding_id.as_str(), f))
                    .collect();
                for result in log["runs"][0]["results"]
                    .as_array_mut()
                    .ok_or("SARIF results missing")?
                {
                    let id = result["partialFingerprints"]["afFindingId/v1"]
                        .as_str()
                        .unwrap_or_default();
                    let finding = canonical.get(id).ok_or("SARIF lost canonical Finding")?;
                    let mut text = result["message"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    for report_id in &finding.report_ids {
                        let report = &ledger.reports[report_id];
                        text.push_str(&format!(
                            "\nReport {report_id} on Snapshot {}; producer {}",
                            report.subject_snapshot_id.as_deref().unwrap_or("unknown"),
                            serde_json::to_string(&report.producer).map_err(|e| e.to_string())?
                        ));
                    }
                    result["message"]["text"] = json!(text);
                }
            }
            crate::sarif::check(&log)?;
            println!(
                "{}",
                serde_json::to_string(&log).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(0)
}
