//! Local backlog capture and read projections for a Project Store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use af_contracts::backlog::intake::Receipt;
use af_contracts::backlog::{
    BacklogRecordV1, Batch, Decision, Disposition, Evidence, EvidenceType, ItemRevision,
    MAX_EVIDENCE, MAX_EVIDENCE_BYTES, Privacy, PublicFields, Report, is_report_id,
};
use af_contracts::{EventType, RunEvent};
use af_store::{NewEvent, canonical::blob_content_id, content_id};
use serde::Serialize;

use crate::{Checkout, Git, ProjectStore, now_unix_ms, workspace_id};

mod intake;

const REPORT_PREFIX: &str = "backlog-report/";
const PROCESSING: &str = "backlog-processing";

#[derive(Serialize)]
pub struct CaptureReceipt {
    pub project_id: String,
    pub report_id: String,
    pub captured_unix_ms: af_contracts::task::usage::DecimalU64,
    pub local_capture: bool,
    pub authority_receipt: Option<String>,
}

/// The only fields ordinary readers can receive. This type contains no private references.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VerifiedItem {
    pub item_id: String,
    pub revision_id: String,
    pub public: PublicFields,
}

#[derive(Serialize)]
pub struct ReportOutcome {
    pub decision_id: String,
    pub batch_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub disposition: Disposition,
    pub reason: String,
}

pub struct PrivateReport {
    pub report: Report,
    pub authority_receipt: Option<Receipt>,
    pub outcomes: Vec<ReportOutcome>,
}

pub fn read_evidence(paths: &[PathBuf]) -> Result<Vec<Evidence>, String> {
    if paths.len() > MAX_EVIDENCE {
        return Err(format!(
            "backlog accepts at most {MAX_EVIDENCE} evidence files"
        ));
    }
    let mut remaining = MAX_EVIDENCE_BYTES;
    paths
        .iter()
        .map(|path| {
            let bytes = af_store::local_fs::read_bounded(path, remaining as u64)
                .map_err(|error| format!("reading backlog evidence: {error}"))?;
            remaining -= bytes.len();
            let content_id = blob_content_id(&bytes);
            let text =
                String::from_utf8(bytes).map_err(|_| "backlog evidence must be UTF-8 text")?;
            Ok(Evidence {
                content_id,
                media_type: EvidenceType::Text,
                text,
            })
        })
        .collect()
}

pub fn prepare_report(
    checkout: &Checkout,
    report_id: String,
    title: String,
    observed: String,
    repro: Option<String>,
    evidence: Vec<Evidence>,
) -> Result<Report, String> {
    let git = Git::open(&checkout.toplevel)?;
    let source_revision = git.require(["rev-parse", "--verify", "HEAD^{commit}"], None)?;
    if git.roots(&source_revision)? != checkout.repository_id {
        return Err(
            "checkout Project changed during backlog capture; retry from the intended Workspace"
                .into(),
        );
    }
    let report = Report {
        project_id: checkout.project_id.clone(),
        report_id,
        workspace_id: workspace_id(&checkout.project_id, &checkout.toplevel),
        captured_unix_ms: now_unix_ms().into(),
        title,
        observed,
        repro,
        task_id: None,
        run_id: None,
        source_revision: Some(source_revision),
        privacy: Privacy::Private,
        evidence,
    };
    report.validate()?;
    Ok(report)
}

fn read_record(project_id: &str, event: &RunEvent) -> Result<BacklogRecordV1, String> {
    if event.event_type != EventType::BacklogRecordStoredV1 || !event.artifact_refs.is_empty() {
        return Err("invalid backlog stream event".into());
    }
    let record: BacklogRecordV1 =
        serde_json::from_value(event.payload.clone()).map_err(|_| "invalid backlog record")?;
    record.validate()?;
    if record.project_id() != project_id {
        return Err("backlog record belongs to a different Project".into());
    }
    Ok(record)
}

fn check_evidence(report: &Report) -> Result<(), String> {
    if report
        .evidence
        .iter()
        .any(|e| blob_content_id(e.text.as_bytes()) != e.content_id)
    {
        return Err("backlog evidence differs from its recorded content identity".into());
    }
    Ok(())
}

impl ProjectStore {
    pub fn record_backlog_report(&mut self, mut report: Report) -> Result<CaptureReceipt, String> {
        report.validate()?;
        if report.project_id != self.project_id {
            return Err("cannot record a report for another Project".into());
        }
        check_evidence(&report)?;
        if let Some(previous) = self.local_backlog_report(&report.report_id)? {
            report.captured_unix_ms = previous.captured_unix_ms;
            if report != previous {
                return Err(format!(
                    "backlog report ID {} already names different content",
                    report.report_id
                ));
            }
        } else {
            if self.backlog_intake()?.contains_key(&report.report_id) {
                return Err("backlog report ID already names an authority intake report".into());
            }
            let payload = serde_json::to_value(BacklogRecordV1::Report(report.clone()))
                .map_err(|_| "cannot encode backlog report")?;
            let event = NewEvent::new(EventType::BacklogRecordStoredV1, payload).at(
                af_contracts::task::collection::collected_time(report.captured_unix_ms.get()),
            );
            if self
                .events
                .append_if_empty(
                    &format!("{REPORT_PREFIX}{}", report.report_id),
                    &self.cas,
                    event,
                )
                .map_err(|_| "could not commit backlog report")?
                .is_none()
            {
                return Err(
                    "backlog report stream changed during capture; retry with the same ID".into(),
                );
            }
        }
        let authority_receipt = self
            .backlog_acknowledgment(&report)?
            .map(|receipt| intake::value_id(&receipt))
            .transpose()?;
        Ok(CaptureReceipt {
            project_id: report.project_id,
            report_id: report.report_id,
            captured_unix_ms: report.captured_unix_ms,
            local_capture: true,
            authority_receipt,
        })
    }

    fn local_backlog_report(&self, id: &str) -> Result<Option<Report>, String> {
        if !is_report_id(id) {
            return Err("invalid backlog report ID".into());
        }
        let events = self
            .events
            .replay(&format!("{REPORT_PREFIX}{id}"))
            .map_err(|_| "could not replay backlog report")?;
        match events.as_slice() {
            [] => Ok(None),
            [event] => {
                let BacklogRecordV1::Report(report) = read_record(&self.project_id, event)? else {
                    return Err("invalid record in backlog report stream".into());
                };
                if report.report_id != id {
                    return Err("backlog report ID differs from its stream".into());
                }
                check_evidence(&report)?;
                Ok(Some(report))
            }
            _ => Err("backlog report stream holds multiple records".into()),
        }
    }

    fn local_backlog_reports(&self) -> Result<Vec<Report>, String> {
        self.events
            .run_ids()
            .map_err(|_| "could not enumerate backlog reports")?
            .iter()
            .filter_map(|run| run.strip_prefix(REPORT_PREFIX))
            .map(|id| {
                self.local_backlog_report(id)?
                    .ok_or_else(|| "missing backlog report".into())
            })
            .collect()
    }

    pub fn backlog_inbox(&self) -> Result<Vec<Report>, String> {
        let mut reports: BTreeMap<_, _> = self
            .local_backlog_reports()?
            .into_iter()
            .map(|report| (report.report_id.clone(), report))
            .collect();
        for (id, accepted) in self.backlog_intake()? {
            let report = accepted.report;
            if reports
                .insert(id, report.clone())
                .is_some_and(|local| local != report)
            {
                return Err("backlog inbox has conflicting reports".into());
            }
        }
        Ok(reports.into_values().collect())
    }

    pub fn backlog_outbox(&self) -> Result<Vec<String>, String> {
        let mut pending = Vec::new();
        for report in self.local_backlog_reports()? {
            if self.backlog_acknowledgment(&report)?.is_none() {
                pending.push(report.report_id);
            }
        }
        Ok(pending)
    }

    pub fn private_backlog_report(&self, id: &str) -> Result<Option<PrivateReport>, String> {
        let local = self.local_backlog_report(id)?;
        let accepted = self.backlog_intake()?.remove(id);
        let (report, authority_receipt) = match (local, accepted) {
            (Some(local), Some(accepted)) if accepted.report != local => {
                return Err("backlog inbox has conflicting reports".into());
            }
            (_, Some(accepted)) => (accepted.report, Some(accepted.receipt.body)),
            (Some(report), None) => {
                let receipt = self.backlog_acknowledgment(&report)?;
                (report, receipt)
            }
            (None, None) => return Ok(None),
        };
        let (_, outcomes) = self.backlog_projection(Some(id))?;
        Ok(Some(PrivateReport {
            report,
            authority_receipt,
            outcomes,
        }))
    }

    pub fn verified_backlog(&self) -> Result<Vec<VerifiedItem>, String> {
        self.backlog_projection(None).map(|(items, _)| items)
    }

    fn backlog_projection(
        &self,
        report: Option<&str>,
    ) -> Result<(Vec<VerifiedItem>, Vec<ReportOutcome>), String> {
        let events = self
            .events
            .replay(PROCESSING)
            .map_err(|_| "could not replay backlog decisions")?;
        if events.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let authority = self.backlog_authority_id()?;
        project_backlog(&self.project_id, authority.as_deref(), &events, report)
    }
}

/// Fold only the processing stream. Raw Reports are outside this read path.
fn project_backlog(
    project: &str,
    authority: Option<&str>,
    events: &[RunEvent],
    report: Option<&str>,
) -> Result<(Vec<VerifiedItem>, Vec<ReportOutcome>), String> {
    let mut outcomes = Vec::new();
    let mut batches: BTreeMap<String, Batch> = BTreeMap::new();
    let mut revisions: BTreeMap<String, ItemRevision> = BTreeMap::new();
    let mut current = BTreeMap::new();
    let mut decisions: BTreeMap<String, Decision> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for event in events {
        let record = read_record(project, event)?;
        let id = content_id(&event.payload).map_err(|_| "invalid backlog record identity")?;
        if !seen.insert(id.clone()) {
            continue;
        }
        match record {
            BacklogRecordV1::Report(_) => {
                return Err("raw report in backlog processing stream".into());
            }
            BacklogRecordV1::Batch(batch) => {
                let authority =
                    authority.ok_or("no backlog authority configured for Project batches")?;
                if authority != batch.authority_id {
                    return Err("backlog batch names a different Project authority".into());
                }
                batches.insert(id, batch);
            }
            BacklogRecordV1::ItemRevision(item) => {
                current.insert(item.item_id.clone(), id.clone());
                revisions.insert(id, item);
            }
            BacklogRecordV1::Decision(decision) => {
                let batch = batches
                    .get(&decision.batch_id)
                    .ok_or("backlog decision names an unknown batch")?;
                let revision = revisions
                    .get(&decision.revision_id)
                    .ok_or("backlog decision names an unknown revision")?;
                if !revision
                    .report_ids
                    .iter()
                    .all(|id| batch.report_ids.contains(id))
                    || revision.evidence_ids != decision.evidence_ids
                {
                    return Err("backlog decision does not cover its revision and evidence".into());
                }
                if report
                    .is_some_and(|id| revision.report_ids.iter().any(|report_id| report_id == id))
                {
                    outcomes.push(ReportOutcome {
                        decision_id: id,
                        batch_id: decision.batch_id.clone(),
                        item_id: revision.item_id.clone(),
                        revision_id: decision.revision_id.clone(),
                        disposition: decision.disposition,
                        reason: decision.reason.clone(),
                    });
                }
                decisions.insert(decision.revision_id.clone(), decision);
            }
            BacklogRecordV1::Publication(_) => {}
        }
    }
    let items = current
        .into_iter()
        .filter_map(|(item_id, revision_id)| {
            let decision = decisions.get(&revision_id)?;
            (decision.disposition == Disposition::Verified).then(|| VerifiedItem {
                item_id,
                public: revisions[&revision_id].proposed.clone(),
                revision_id,
            })
        })
        .collect();
    Ok((items, outcomes))
}

#[cfg(test)]
mod tests;
