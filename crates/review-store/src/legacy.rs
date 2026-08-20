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

use review_core::{LegacyStageOutput, RunEvent, Severity};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::cas::Cas;
use crate::ledger::{
    EVENT_FINDING_REPORTED, EVENT_FINDING_RESOLVED, EVENT_GENERATION_ADVANCED, Ledger, Status,
    TransitionKind,
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
    ledger: Ledger,
}

impl<'a> Ingest<'a> {
    pub fn new(
        store: &'a mut EventStore,
        cas: &'a Cas,
        run_id: impl Into<String>,
    ) -> Result<Self, StoreError> {
        let run_id = run_id.into();
        let ledger = Ledger::rebuild(store, cas, &run_id)?;
        Ok(Self {
            store,
            cas,
            run_id,
            ledger,
        })
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
            NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": round })),
        )?;
        self.ledger.apply_event(&event, self.cas)?;
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

    fn add_stage_output_inner(
        &mut self,
        source: &str,
        stage: &LegacyStageOutput,
        strict: bool,
    ) -> Result<AddSummary, StoreError> {
        let round = self.ledger.round;
        let mut summary = AddSummary::default();
        let mut projected = self.ledger.clone();
        let mut events = Vec::new();
        let existing_reports: BTreeSet<(String, String, u32, String)> = self
            .ledger
            .findings()
            .into_iter()
            .flat_map(|finding| {
                finding.reports.iter().map(|report| {
                    (
                        finding.key.clone(),
                        report.source.clone(),
                        report.round,
                        report.report_id.clone(),
                    )
                })
            })
            .collect();

        for (index, finding) in stage.findings.iter().enumerate() {
            let report = match finding.clone().into_report(index) {
                Ok(report) => report,
                Err(reason) if !strict => {
                    eprintln!("add: skipping {source} finding ({reason})");
                    continue;
                }
                Err(reason) => {
                    return Err(StoreError::Conflict(format!(
                        "{source} finding {index} violates FindingReport@1: {reason}"
                    )));
                }
            };
            let location = report.locations.first();
            let file = location
                .map(|location| location.path.as_str())
                .unwrap_or("");
            let line = location.and_then(|location| location.line).map(i64::from);
            let key = legacy_fingerprint(file, &report.title);

            // The report is an immutable artifact; the event references it. Even a duplicate
            // gets stored — that is the whole difference from the shell ledger, which counted
            // it and threw it away.
            let report_artifact = json!({
                "title": report.title,
                "severity": severity_str(report.severity),
                "file": file,
                "line": line,
                "body": report.body,
                "fix": report.fix,
                "confidence": report.confidence,
            });
            let report_id = self
                .cas
                .put_json(&report_artifact)
                .map_err(|e| StoreError::Conflict(e.to_string()))?;

            if existing_reports.contains(&(
                key.clone(),
                source.to_string(),
                round,
                report_id.clone(),
            )) {
                summary.dup += 1;
                continue;
            }

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
                Some(TransitionKind::Duplicate | TransitionKind::AdoptedWhileDeclined) => {
                    summary.dup += 1
                }
                _ => {}
            }
        }

        // Reviewer disputes are part of the contract the model is asked to answer — a `refute`
        // on a prior claim's `claim_id` says "I think this is wrong". Fold it: an active claim
        // a reviewer refutes becomes `contested`, which blocks convergence and flags the claim
        // for human adjudication rather than leaving the dispute inert in raw CAS output. A
        // `confirm` agrees with a claim that is already open and needs no transition.
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
            let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(key.to_string());
            apply_candidate(&mut projected, &event, self.cas)?;
            events.push(event);
            summary.contested += 1;
        }

        summary.open = projected
            .findings()
            .iter()
            .filter(|f| f.status == Status::Open)
            .count();
        self.store.append_batch(&self.run_id, self.cas, &events)?;
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
        let payload = json!({
            "key": key,
            "status": status.as_str(),
            "note": note,
            "round": self.ledger.round,
        });
        let event = self.store.append(
            &self.run_id,
            self.cas,
            NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(key.to_string()),
        )?;
        self.ledger.apply_event(&event, self.cas)?;
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
        let event = NewEvent::new(EVENT_FINDING_REPORTED, payload).correlating(row.fp.clone());
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
            let event = NewEvent::new(EVENT_FINDING_REPORTED, payload).correlating(row.fp.clone());
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
            let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(row.fp.clone());
            apply_candidate(&mut projected, &event, cas)?;
            events.push(event);
        }
        imported += 1;
    }

    if max_round > 1 {
        let event = NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": max_round }));
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
}
