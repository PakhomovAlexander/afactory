//! Driving the kernel with the shell harness's own inputs, and importing its output.
//!
//! Two directions, both needed for a safe migration:
//!
//! - [`Ingest`] replays a `ledger.sh add` / `resolve` / `bump` sequence as events, so the
//!   frozen fixtures can be run through the new engine and compared decision by decision.
//! - [`import_ledger_jsonl`] turns a committed `ledger.jsonl` into events, so an old run can be
//!   read by new tooling. That direction is inherently lossy — the source is final state, not
//!   history — and the import is honest about it: it produces one report and at most one
//!   resolution per row, and claims nothing about what happened in between.

use review_core::{FindingReport, LegacyStageOutput, RunEvent, Severity};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::cas::Cas;
use crate::ledger::{
    EVENT_FINDING_REPORTED, EVENT_FINDING_RESOLVED, EVENT_GENERATION_ADVANCED, Ledger,
    LedgerProjection, Status, TransitionKind,
};
use crate::store::{EventStore, NewEvent, StoreError};

/// The path the harness substitutes when a reviewer leaves `file` empty. It shares the
/// fingerprint namespace with real paths, which is why the v1 contract drops it for an empty
/// location list — but the fingerprint must still be computed over it to match.
pub const CHANGE_WIDE: &str = "(change-wide)";

/// `ledger.sh`'s fingerprint: sha256 of `file|title`, first 12 hex, with the title normalized
/// for case and whitespace only.
///
/// ASCII lowercasing on purpose: the original is `tr '[:upper:]' '[:lower:]'`, which does not
/// touch non-ASCII. Unicode lowercasing here would silently disagree with every fingerprint the
/// harness has ever produced — including every row of every frozen corpus.
pub fn legacy_fingerprint(file: &str, title: &str) -> String {
    let file = if file.trim().is_empty() {
        CHANGE_WIDE
    } else {
        file
    };

    let mut normalized = String::with_capacity(title.len());
    let mut in_space = false;
    for ch in title.chars() {
        if ch.is_whitespace() {
            if !in_space {
                normalized.push(' ');
            }
            in_space = true;
        } else {
            normalized.push(ch.to_ascii_lowercase());
            in_space = false;
        }
    }
    // `sed 's/^ //; s/ $//'` — one leading and one trailing space, which is all that can remain
    // after the squeeze.
    let normalized = normalized
        .strip_prefix(' ')
        .unwrap_or(&normalized)
        .to_string();
    let normalized = normalized.strip_suffix(' ').unwrap_or(&normalized);

    let mut hasher = Sha256::new();
    hasher.update(file.as_bytes());
    hasher.update(b"|");
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())[..12].to_string()
}

/// Stable bridge identity until M3 replaces path-based fingerprints. A report's location order
/// is model output and therefore not identity; sorting is unnecessary when only the minimum is
/// needed, and the existing one-location legacy shape retains its exact key.
fn report_identity_path(report: &review_core::FindingReport) -> &str {
    report
        .locations
        .iter()
        .map(|location| location.path.as_str())
        .min()
        .unwrap_or("")
}

/// What `ledger.sh add` prints. Compared against the frozen transcripts verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddSummary {
    pub new: usize,
    pub dup: usize,
    pub reopened: usize,
    pub escalated: usize,
    pub open: usize,
    /// Prior claims a reviewer refuted this stage. Deliberately absent from [`Display`], which
    /// is compared verbatim against the frozen harness transcripts (the harness had no
    /// disputes); it is a field for callers that want it, not part of the tally line.
    pub contested: usize,
}

impl std::fmt::Display for AddSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "new={} dup={} reopened={} escalated={} open={}",
            self.new, self.dup, self.reopened, self.escalated, self.open
        )
    }
}

/// Drives a run: ingest stage outputs, record resolutions, advance generations.
pub struct Ingest<'a> {
    store: &'a mut EventStore,
    cas: &'a Cas,
    run_id: String,
    event_count: u64,
    ledger: Ledger,
    round_event_id: Option<String>,
}

struct PreparedStage {
    source: String,
    reports: Vec<FindingReport>,
    disputes: Vec<review_core::legacy::LegacyDispute>,
}

impl<'a> Ingest<'a> {
    pub fn new(
        store: &'a mut EventStore,
        cas: &'a Cas,
        run_id: impl Into<String>,
    ) -> Result<Self, StoreError> {
        let run_id = run_id.into();
        let projection = LedgerProjection::rebuild(store, cas, &run_id)?;
        let (_, event_count, ledger) = projection.into_parts();
        Ok(Self {
            store,
            cas,
            run_id,
            event_count,
            ledger,
            round_event_id: None,
        })
    }

    /// Continue from a projection already rebuilt from this exact run's log. Callers that append
    /// the current Round input can fold that event once and avoid replaying the same history
    /// again before ingest.
    pub fn from_projection(
        store: &'a mut EventStore,
        cas: &'a Cas,
        run_id: impl Into<String>,
        projection: LedgerProjection,
    ) -> Result<Self, StoreError> {
        let run_id = run_id.into();
        let (projection_run_id, event_count, ledger) = projection.into_parts();
        if projection_run_id != run_id {
            return Err(StoreError::Conflict(format!(
                "Ledger projection for `{projection_run_id}` cannot ingest run `{run_id}`"
            )));
        }
        let durable_count = store.len(&run_id)?;
        if event_count != durable_count {
            return Err(StoreError::Conflict(format!(
                "Ledger projection for `{run_id}` covers {event_count} events, but the log contains {durable_count}"
            )));
        }
        Ok(Self {
            store,
            cas,
            run_id,
            event_count,
            ledger,
            round_event_id: None,
        })
    }

    pub fn into_projection(self) -> LedgerProjection {
        LedgerProjection::from_parts(self.run_id, self.event_count, self.ledger)
    }

    /// Bind reducer and generation events to the active durable Round epoch.
    pub fn under_round(mut self, round_event_id: impl Into<String>) -> Self {
        self.round_event_id = Some(round_event_id.into());
        self
    }

    fn bind_round(&self, event: NewEvent) -> NewEvent {
        match &self.round_event_id {
            Some(round) => event.caused_by(round),
            None => event.legacy_import(),
        }
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn round(&self) -> u32 {
        self.ledger.round
    }

    /// `ledger.sh bump`.
    pub fn advance(&mut self) -> Result<u32, StoreError> {
        let round = self.ledger.round + 1;
        let event = self.store.append(
            &self.run_id,
            self.cas,
            self.bind_round(NewEvent::new(
                EVENT_GENERATION_ADVANCED,
                json!({ "round": round }),
            )),
        )?;
        self.validate_watermark(&event)?;
        self.ledger.apply_event(&event, self.cas)?;
        self.event_count += 1;
        Ok(round)
    }

    /// `ledger.sh add --source <source> <findings.json>`.
    ///
    /// Every ingested finding is validated against the `FindingReport@1` contract first
    /// ([`LegacyFinding::into_report`]): a finding without a fix, with an empty title or body,
    /// with an out-of-range confidence, or with a non-positive line is **not** ingested. This
    /// is the enforcement point `FindingReport@1` was written for — before, the contract and
    /// its acceptance corpus governed a conversion no run performed. An unusable entry is
    /// skipped, not fatal: like the harness skipping an empty title, one bad finding must not
    /// discard a batch that cannot be re-requested.
    pub fn add_stage_output(
        &mut self,
        source: &str,
        stage: &LegacyStageOutput,
    ) -> Result<AddSummary, StoreError> {
        self.add_stage_output_inner(source, stage, false)
    }

    /// Strict live admission. Unlike the frozen legacy bridge, one malformed finding rejects
    /// the complete reviewer result so a blocking verdict cannot degrade into an empty pass.
    pub fn add_live_stage_output(
        &mut self,
        source: &str,
        stage: &LegacyStageOutput,
    ) -> Result<AddSummary, StoreError> {
        self.add_stage_output_inner(source, stage, true)
    }

    /// Atomically admit every live reviewer result feeding one ledger node. Validation or
    /// storage failure leaves the event log untouched, so a retry cannot inherit half a
    /// reduction and duplicate the reviewers that were committed first.
    pub fn add_live_stage_outputs(
        &mut self,
        stages: &[(&str, &LegacyStageOutput)],
    ) -> Result<AddSummary, StoreError> {
        self.add_stage_outputs_inner(stages, true)
    }

    fn add_stage_output_inner(
        &mut self,
        source: &str,
        stage: &LegacyStageOutput,
        strict: bool,
    ) -> Result<AddSummary, StoreError> {
        self.add_stage_outputs_inner(&[(source, stage)], strict)
    }

    fn add_stage_outputs_inner(
        &mut self,
        stages: &[(&str, &LegacyStageOutput)],
        strict: bool,
    ) -> Result<AddSummary, StoreError> {
        let mut prepared = Vec::with_capacity(stages.len());
        for (source, stage) in stages {
            let mut reports = Vec::with_capacity(stage.findings.len());
            for (index, finding) in stage.findings.iter().enumerate() {
                // The frozen shell bridge trimmed titles before admission. Preserve that
                // historical projection here, while the live LegacyFinding reader follows
                // reviewer-result-v1 literally (where any non-empty string is content).
                if !strict && finding.title.trim().is_empty() {
                    eprintln!("add: skipping {source} finding (finding {index}: empty title)");
                    continue;
                }
                match finding.clone().into_report(index) {
                    Ok(report) => reports.push(report),
                    Err(reason) if !strict => {
                        eprintln!("add: skipping {source} finding ({reason})");
                    }
                    Err(reason) => {
                        return Err(StoreError::Conflict(format!(
                            "{source} finding {index} violates FindingReport@1: {reason}"
                        )));
                    }
                }
            }
            prepared.push(PreparedStage {
                source: (*source).to_string(),
                reports,
                disputes: stage.disputes.clone(),
            });
        }
        self.add_prepared_outputs(&prepared)
    }

    fn add_prepared_outputs(&mut self, stages: &[PreparedStage]) -> Result<AddSummary, StoreError> {
        let round = self.ledger.round;
        let mut summary = AddSummary::default();
        let mut projected = self.ledger.clone();
        let mut events = Vec::new();
        let existing_reports: BTreeSet<(&str, &str, u32, &str)> = self
            .ledger
            .findings()
            .into_iter()
            .flat_map(|finding| {
                finding.reports.iter().map(|report| {
                    (
                        finding.key.as_str(),
                        report.source.as_str(),
                        report.round,
                        report.report_id.as_str(),
                    )
                })
            })
            .collect();
        let mut pending_reports: BTreeSet<(String, String, u32, String)> = BTreeSet::new();

        for stage in stages {
            let source = stage.source.as_str();
            for report in &stage.reports {
                let file = report_identity_path(report);
                let key = legacy_fingerprint(file, &report.title);

                // The report is an immutable artifact; the event references it. Even a duplicate
                // gets stored — that is the whole difference from the shell ledger, which counted
                // it and threw it away.
                let report_artifact = serde_json::to_value(report)?;
                let report_id = self
                    .cas
                    .put_json(&report_artifact)
                    .map_err(|e| StoreError::Conflict(e.to_string()))?;
                let report_identity = (key.clone(), source.to_string(), round, report_id.clone());

                if existing_reports.contains(&(key.as_str(), source, round, report_id.as_str()))
                    || pending_reports.contains(&report_identity)
                {
                    summary.dup += 1;
                    continue;
                }
                pending_reports.insert(report_identity);

                let payload = json!({
                    "key": key,
                    "round": round,
                    "source": source,
                    "report_id": report_id,
                });
                let event = NewEvent::new(EVENT_FINDING_REPORTED, payload)
                    .correlating(key.clone())
                    .referencing(vec![report_id]);
                apply_candidate(&mut projected, &event, self.cas)?;
                events.push(event);

                match projected
                    .get(&key)
                    .and_then(|f| f.history.last())
                    .map(|t| t.kind)
                {
                    Some(TransitionKind::Reported) => summary.new += 1,
                    Some(TransitionKind::Reopened) => summary.reopened += 1,
                    Some(TransitionKind::Escalated) => summary.escalated += 1,
                    // `AdoptedWhileDeclined` counts as a duplicate in the harness's tally, even
                    // though it adopts the higher severity — the entry did not become actionable.
                    Some(
                        TransitionKind::Duplicate
                        | TransitionKind::AdoptedWhileDeclined
                        | TransitionKind::AuthorityRecovered,
                    ) => summary.dup += 1,
                    _ => {}
                }
            }

            // Reviewer disputes are part of the contract the model is asked to answer — a
            // `refute` on a prior claim's `claim_id` says "I think this is wrong". Fold it:
            // an active claim a reviewer refutes becomes `contested`, which blocks convergence
            // and flags the claim for human adjudication rather than leaving the dispute inert in
            // raw CAS output. A `confirm` agrees with an open claim and needs no transition.
            for dispute in &stage.disputes {
                if dispute.position.trim() != "refute" {
                    continue;
                }
                let key = dispute.fp.trim();
                let contestable = matches!(
                    projected.get(key).map(|f| f.status),
                    Some(Status::Open | Status::Fixed)
                );
                if !contestable {
                    continue;
                }
                let payload = json!({
                    "key": key,
                    "status": Status::Contested.as_str(),
                    "note": format!("contested by {source}: {}", dispute.reason),
                    "round": round,
                });
                let event =
                    NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(key.to_string());
                apply_candidate(&mut projected, &event, self.cas)?;
                events.push(event);
                summary.contested += 1;
            }
        }

        summary.open = projected
            .findings()
            .iter()
            .filter(|f| f.status == Status::Open)
            .count();
        let events: Vec<NewEvent> = events
            .into_iter()
            .map(|event| self.bind_round(event))
            .collect();
        let appended = self.store.append_batch(&self.run_id, self.cas, &events)?;
        for event in &appended {
            self.advance_watermark(event)?;
        }
        self.ledger = projected;
        Ok(summary)
    }

    /// `ledger.sh resolve <fp> <status> [--note ...]`.
    pub fn resolve(
        &mut self,
        key: &str,
        status: Status,
        note: Option<&str>,
    ) -> Result<(), StoreError> {
        if self.ledger.get(key).is_none() {
            return Err(StoreError::Conflict(format!(
                "cannot resolve unknown finding key '{key}'"
            )));
        }
        let payload = json!({
            "key": key,
            "status": status.as_str(),
            "note": note,
            "round": self.ledger.round,
        });
        let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(key.to_string());
        let event = if self.round_event_id.is_none() {
            event.legacy_import()
        } else {
            event
        };
        let event = self.store.append(&self.run_id, self.cas, event)?;
        self.validate_watermark(&event)?;
        self.ledger.apply_event(&event, self.cas)?;
        self.event_count += 1;
        Ok(())
    }

    fn advance_watermark(&mut self, event: &RunEvent) -> Result<(), StoreError> {
        self.validate_watermark(event)?;
        self.event_count += 1;
        Ok(())
    }

    fn validate_watermark(&self, event: &RunEvent) -> Result<(), StoreError> {
        if event.run_id != self.run_id || event.sequence != self.event_count {
            return Err(StoreError::Conflict(format!(
                "Ledger ingest for `{}` expected sequence {}, got {} for `{}`",
                self.run_id, self.event_count, event.sequence, event.run_id
            )));
        }
        Ok(())
    }
}

/// One row of a committed `ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyRow {
    pub fp: String,
    pub round: u32,
    pub last_seen_round: u32,
    pub source: String,
    pub status: String,
    pub severity: Severity,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub body: String,
    pub confidence: Option<f64>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Import a committed `ledger.jsonl` as events.
///
/// Lossy by nature, and deliberately not pretending otherwise: the file records final state, so
/// each row becomes one report plus at most one resolution. Whether that finding was ever
/// duplicated, escalated or reopened is not in the source and is not invented here.
pub fn import_ledger_jsonl(
    store: &mut EventStore,
    cas: &Cas,
    run_id: &str,
    jsonl: &str,
) -> Result<usize, StoreError> {
    let mut imported = 0;
    let mut max_round = 1;
    let mut projected = Ledger::rebuild(store, cas, run_id)?;
    let mut events = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: LegacyRow = serde_json::from_str(line)?;
        if row.fp.trim().is_empty()
            || row.round == 0
            || row.last_seen_round < row.round
            || row.source.trim().is_empty()
            || row.file.trim().is_empty()
            || row.title.trim().is_empty()
            || row.body.trim().is_empty()
            || row
                .line
                .is_some_and(|line| line <= 0 || u32::try_from(line).is_err())
            || row
                .confidence
                .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
        {
            return Err(StoreError::Conflict(format!(
                "legacy row `{}` violates persisted ledger invariants",
                row.fp
            )));
        }
        let status = Status::parse(&row.status).ok_or_else(|| {
            StoreError::Conflict(format!(
                "legacy row {} has invalid status `{}`",
                row.fp, row.status
            ))
        })?;
        let expected_fingerprint = legacy_fingerprint(&row.file, &row.title);
        if row.fp != expected_fingerprint {
            return Err(StoreError::Conflict(format!(
                "legacy row fingerprint `{}` does not match `{expected_fingerprint}` for its file and title",
                row.fp
            )));
        }
        max_round = max_round.max(row.last_seen_round);

        let payload = json!({
            "key": row.fp,
            "round": row.round,
            "source": row.source,
            "severity": severity_str(row.severity),
            "file": row.file,
            "line": row.line,
            "title": row.title,
            "body": row.body,
            "confidence": row.confidence,
            "imported": true,
        });
        let event = NewEvent::new(EVENT_FINDING_REPORTED, payload)
            .correlating(row.fp.clone())
            .legacy_import();
        apply_candidate(&mut projected, &event, cas)?;
        events.push(event);

        // The row's own last_seen_round is restored by a second report only when it differs,
        // so an imported finding keeps both round columns the file recorded.
        if row.last_seen_round > row.round {
            let payload = json!({
                "key": row.fp,
                "round": row.last_seen_round,
                "source": row.source,
                "severity": severity_str(row.severity),
                "file": row.file,
                "line": row.line,
                "title": row.title,
                "body": row.body,
                "confidence": row.confidence,
                "imported": true,
            });
            let event = NewEvent::new(EVENT_FINDING_REPORTED, payload)
                .correlating(row.fp.clone())
                .legacy_import();
            apply_candidate(&mut projected, &event, cas)?;
            events.push(event);
        }

        if status != Status::Open {
            let payload = json!({
                "key": row.fp,
                "status": status.as_str(),
                "note": row.note,
                "round": row.last_seen_round,
                "imported": true,
            });
            let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload)
                .correlating(row.fp.clone())
                .legacy_import();
            apply_candidate(&mut projected, &event, cas)?;
            events.push(event);
        }
        imported += 1;
    }

    if max_round > 1 {
        let event =
            NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": max_round })).legacy_import();
        apply_candidate(&mut projected, &event, cas)?;
        events.push(event);
    }
    store.append_batch(run_id, cas, &events)?;
    Ok(imported)
}

/// Apply a not-yet-persisted event through the authoritative projection. Sequence and identity
/// are irrelevant to Ledger, so placeholders let ingest validate the exact payload and artifact
/// set before the atomic append makes any part of the batch durable.
fn apply_candidate(ledger: &mut Ledger, event: &NewEvent, cas: &Cas) -> Result<(), StoreError> {
    ledger.apply_event(
        &RunEvent {
            event_id: String::new(),
            run_id: String::new(),
            sequence: 0,
            event_type: event.event_type,
            occurred_at: event.occurred_at.clone(),
            node_id: event.node_id.clone(),
            attempt_id: event.attempt_id.clone(),
            causation_id: event.causation_id.clone(),
            correlation_id: event.correlation_id.clone(),
            artifact_refs: event.artifact_refs.clone(),
            payload: event.payload.clone(),
        },
        cas,
    )
}

pub fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker => "blocker",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors traced through the actual scripts, not computed by hand — a digest this file
    /// produced for itself would agree with itself and prove nothing.
    ///
    /// Only generic vectors live here. The broad check runs against every row of every ledger
    /// under `fixtures/synthetic/`, which the real `ledger.sh` wrote and which carries nothing
    /// private: `tests/legacy_ledgers.rs::the_synthetic_fingerprints_match_the_shell`.
    #[test]
    fn fingerprints_match_the_shell_implementation() {
        assert_eq!(
            legacy_fingerprint("src/parser.rs", "Retry loop can spin forever"),
            "de15e7f49066"
        );
        assert_eq!(
            legacy_fingerprint("", "No rollback path for the migration"),
            "a724be9f6afa"
        );
    }

    #[test]
    fn normalization_matches_tr_and_sed() {
        // case-folded, runs of whitespace squeezed, one leading/trailing space trimmed
        assert_eq!(
            legacy_fingerprint("f", "  Retry   LOOP\tcan\nspin forever "),
            legacy_fingerprint("f", "retry loop can spin forever")
        );
        // ASCII-only lowercasing, exactly as `tr '[:upper:]' '[:lower:]'` behaves
        assert_ne!(
            legacy_fingerprint("f", "ПРОВЕРКА"),
            legacy_fingerprint("f", "проверка")
        );
    }

    #[test]
    fn an_empty_path_is_the_change_wide_sentinel() {
        assert_eq!(
            legacy_fingerprint("", "x"),
            legacy_fingerprint(CHANGE_WIDE, "x")
        );
    }

    #[test]
    fn a_projection_cannot_be_reused_for_another_run() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let projection = LedgerProjection::rebuild(&store, &cas, "run-a").unwrap();

        let error = match Ingest::from_projection(&mut store, &cas, "run-b", projection) {
            Ok(_) => panic!("cross-run projection was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("cannot ingest run `run-b`"));
    }

    #[test]
    fn a_projection_cannot_be_reused_after_its_run_log_advances() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let projection = LedgerProjection::rebuild(&store, &cas, "run").unwrap();
        store
            .append_legacy(
                "run",
                &cas,
                NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": 2 })),
            )
            .unwrap();

        let error = match Ingest::from_projection(&mut store, &cas, "run", projection) {
            Ok(_) => panic!("stale projection was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("covers 0 events"), "{error}");
        assert!(error.to_string().contains("log contains 1"), "{error}");
    }

    #[test]
    fn a_projection_rejects_a_repeated_or_gapped_event_sequence() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let mut projection = LedgerProjection::rebuild(&store, &cas, "run").unwrap();
        let event = store
            .append_legacy(
                "run",
                &cas,
                NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": 2 })),
            )
            .unwrap();
        projection.apply_event(&event, &cas).unwrap();

        let repeated = projection.apply_event(&event, &cas).unwrap_err();
        assert!(
            repeated.to_string().contains("expected sequence 1, got 0"),
            "{repeated}"
        );
        let mut gapped = event;
        gapped.sequence = 2;
        let gapped = LedgerProjection::from_events("run", &[gapped], &cas).unwrap_err();
        assert!(
            gapped.to_string().contains("expected sequence 0, got 2"),
            "{gapped}"
        );
    }

    #[test]
    fn multi_location_bridge_identity_is_independent_of_model_order() {
        let report = review_core::FindingReport {
            title: "same claim".into(),
            severity: Severity::Major,
            locations: vec![
                review_core::Location::file("src/z.rs"),
                review_core::Location::file("src/a.rs"),
            ],
            body: "body".into(),
            fix: "fix".into(),
            confidence: 0.9,
            failure_trace: None,
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
        };
        let mut reversed = report.clone();
        reversed.locations.reverse();

        assert_eq!(report_identity_path(&report), "src/a.rs");
        assert_eq!(
            legacy_fingerprint(report_identity_path(&report), &report.title),
            legacy_fingerprint(report_identity_path(&reversed), &reversed.title)
        );
    }
}
