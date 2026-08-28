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

use review_core::{
    CANONICAL_FINDING_IDENTITY_POLICY, FindingDispositionPosition, FindingDispositionV1,
    FindingReport, LegacyStageOutput, Producer, ReviewerResultContract, RunEvent, Severity,
};
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

/// Permanent bridge identity for legacy campaigns. A report's location order is model output;
/// selecting the minimum retains deterministic replay and the one-location legacy key exactly.
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

/// One selected flat reviewer result plus the exact authority needed by the typed-report bridge.
pub struct CanonicalStage<'a> {
    pub source: &'a str,
    pub stage: &'a LegacyStageOutput,
    pub attempt_id: &'a str,
    pub result_artifact_id: &'a str,
    pub input_artifacts: &'a [String],
    pub subject_snapshot_id: &'a str,
    pub subject_id: &'a str,
    pub result_contract: ReviewerResultContract,
}

/// The exact immutable inputs emitted by one canonical ledger reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalReduction {
    pub summary: AddSummary,
    /// Domain-separated typed Report IDs recorded in `FindingSet@1`.
    pub selected_report_ids: Vec<String>,
    /// Domain-separated relation IDs recorded in `FindingSet@1`.
    pub relation_ids: Vec<String>,
    /// CAS IDs of all Report and relation envelopes consumed by the Set reducer.
    pub input_artifact_ids: Vec<String>,
    /// The reducer contract selected by the admitted evidence kind.
    pub reducer_version: &'static str,
}

#[derive(Clone)]
struct ReportProvenance {
    producer: Producer,
    input_artifacts: Vec<String>,
    subject_snapshot_id: String,
    subject_id: String,
}

struct PreparedStage {
    source: String,
    reports: Vec<FindingReport>,
    disputes: Vec<review_core::legacy::LegacyDispute>,
    provenance: Option<ReportProvenance>,
    result_contract: ReviewerResultContract,
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
        mut projection: LedgerProjection,
    ) -> Result<Self, StoreError> {
        let run_id = run_id.into();
        if !projection.belongs_to(&run_id) {
            return Err(StoreError::Conflict(format!(
                "Ledger projection cannot ingest run `{run_id}` because it belongs to a different run"
            )));
        }
        projection.fast_forward(store, cas)?;
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
                provenance: None,
                result_contract: ReviewerResultContract::V1,
            });
        }
        self.add_prepared_outputs(&prepared)
            .map(|reduction| reduction.summary)
    }

    /// Bridge selected flat results into typed, provenance-carrying Report artifacts and reduce
    /// them with the canonical path-independent identity policy.
    pub fn add_canonical_stage_outputs(
        &mut self,
        stages: &[CanonicalStage<'_>],
    ) -> Result<CanonicalReduction, StoreError> {
        if self.ledger.finding_identity_policy() != Some(CANONICAL_FINDING_IDENTITY_POLICY) {
            return Err(StoreError::Conflict(format!(
                "canonical report ingestion disagrees with Campaign policy `{}`",
                self.ledger
                    .finding_identity_policy()
                    .unwrap_or("unavailable")
            )));
        }
        let mut prepared = Vec::with_capacity(stages.len());
        for stage in stages {
            let reports = stage
                .stage
                .findings
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, finding)| {
                    finding.into_report(index).map_err(|reason| {
                        StoreError::Conflict(format!(
                            "{} finding {index} violates FindingReport@1: {reason}",
                            stage.source
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut input_artifacts = Vec::with_capacity(stage.input_artifacts.len() + 1);
            input_artifacts.push(stage.result_artifact_id.to_string());
            input_artifacts.extend(stage.input_artifacts.iter().cloned());
            let mut seen = BTreeSet::new();
            input_artifacts.retain(|id| seen.insert(id.clone()));
            prepared.push(PreparedStage {
                source: stage.source.to_string(),
                reports,
                disputes: stage.stage.disputes.clone(),
                provenance: Some(ReportProvenance {
                    producer: Producer::Attempt {
                        run_id: self.run_id.clone(),
                        node_id: stage.source.to_string(),
                        attempt_id: stage.attempt_id.to_string(),
                    },
                    input_artifacts,
                    subject_snapshot_id: stage.subject_snapshot_id.to_string(),
                    subject_id: stage.subject_id.to_string(),
                }),
                result_contract: stage.result_contract,
            });
        }
        self.add_prepared_outputs(&prepared)
    }

    fn add_prepared_outputs(
        &mut self,
        stages: &[PreparedStage],
    ) -> Result<CanonicalReduction, StoreError> {
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
        let mut selected_report_ids = Vec::new();
        let mut relation_ids = Vec::new();
        let mut input_artifact_ids = Vec::new();
        let mut pending_occurrences: std::collections::BTreeMap<(String, String), String> =
            std::collections::BTreeMap::new();

        for stage in stages {
            let source = stage.source.as_str();
            let mut reports = stage.reports.clone();
            if let Some(provenance) = &stage.provenance {
                for dispute in &stage.disputes {
                    let corroborates = match stage.result_contract {
                        ReviewerResultContract::V1 => dispute.position.trim() == "confirm",
                        ReviewerResultContract::V2 => dispute.position.trim() == "corroborate",
                    };
                    if !corroborates {
                        continue;
                    }
                    let key = dispute.fp.trim();
                    let Some(finding) = self.ledger.get(key) else {
                        // A model may mistype a long canonical ID. Like an unresolvable refute,
                        // it carries no safe authority and must not discard the other selected
                        // reviewers' evidence.
                        continue;
                    };
                    let mut replayed = false;
                    for existing in finding.reports.iter().filter(|report| {
                        report.source == source
                            && report.round == round
                            && report.relations.iter().any(|relation| {
                                relation.kind == review_core::RelationKind::Corroborates
                                    && relation.target.kind
                                        == review_core::finding::ClaimTargetKind::Finding
                                    && relation.target.id == key
                            })
                    }) {
                        let value = self.cas.get_json(&existing.report_id).map_err(|error| {
                            StoreError::Artifact(format!(
                                "corroborating Report {} is unreadable during replay: {error}",
                                existing.report_id
                            ))
                        })?;
                        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value)
                            .map_err(|error| {
                                StoreError::Artifact(format!(
                                    "corroborating Report {} is not an ArtifactEnvelope: {error}",
                                    existing.report_id
                                ))
                            })?;
                        crate::canonical::validate_envelope(&envelope).map_err(|error| {
                            StoreError::Artifact(format!(
                                "corroborating Report {}: {error}",
                                existing.report_id
                            ))
                        })?;
                        if envelope.artifact_type != review_core::contract::FINDING_REPORT_V1 {
                            return Err(StoreError::Artifact(format!(
                                "corroborating Report {} has type {}",
                                existing.report_id, envelope.artifact_type
                            )));
                        }
                        if envelope.producer != provenance.producer
                            || envelope.input_artifacts != provenance.input_artifacts
                            || envelope.subject_snapshot_id.as_deref()
                                != Some(provenance.subject_snapshot_id.as_str())
                        {
                            continue;
                        }
                        reports.push(serde_json::from_value(envelope.payload).map_err(
                            |error| {
                                StoreError::Artifact(format!(
                                    "corroborating Report {} is not FindingReport@1: {error}",
                                    existing.report_id
                                ))
                            },
                        )?);
                        replayed = true;
                        break;
                    }
                    if replayed {
                        continue;
                    }
                    let (Some(fix), Some(confidence)) = (finding.fix.clone(), finding.confidence)
                    else {
                        continue;
                    };
                    let locations =
                        if finding.identity_file == review_core::legacy::CHANGE_WIDE_SENTINEL {
                            Vec::new()
                        } else if review_core::is_valid_repo_path(&finding.identity_file) {
                            let line = match finding.identity_line.map(u32::try_from).transpose() {
                                Ok(line) => line,
                                Err(_) => continue,
                            };
                            vec![review_core::Location {
                                path: finding.identity_file.clone(),
                                line,
                                end_line: None,
                            }]
                        } else {
                            continue;
                        };
                    reports.push(FindingReport {
                        title: finding.title.clone(),
                        severity: finding.severity,
                        locations,
                        body: finding.body.clone(),
                        fix,
                        confidence,
                        failure_trace: None,
                        rule_id: None,
                        occurrence_key: None,
                        relations: vec![review_core::Relation {
                            kind: review_core::RelationKind::Corroborates,
                            target: review_core::finding::RelationTarget {
                                kind: review_core::finding::ClaimTargetKind::Finding,
                                id: key.to_string(),
                            },
                            reason: Some(dispute.reason.clone()),
                        }],
                    });
                }
            }
            let mut published = Vec::with_capacity(reports.len());
            for report in &reports {
                let (report_id, semantic_id) = match &stage.provenance {
                    Some(provenance) => {
                        let (record_id, envelope) = self
                            .cas
                            .put_artifact(
                                review_core::contract::FINDING_REPORT_V1,
                                provenance.producer.clone(),
                                provenance.input_artifacts.clone(),
                                Some(provenance.subject_snapshot_id.clone()),
                                serde_json::to_value(report)?,
                            )
                            .map_err(|error| StoreError::Conflict(error.to_string()))?;
                        (record_id, envelope.artifact_id)
                    }
                    None => {
                        let value = serde_json::to_value(report)?;
                        let record_id = self
                            .cas
                            .put_json(&value)
                            .map_err(|error| StoreError::Conflict(error.to_string()))?;
                        (record_id.clone(), record_id)
                    }
                };
                published.push((report, report_id, semantic_id));
            }
            let keys = match &stage.provenance {
                Some(_) => canonical_stage_keys(&published, &self.ledger, &pending_occurrences)?,
                None => published
                    .iter()
                    .map(|(report, _, _)| {
                        legacy_fingerprint(report_identity_path(report), &report.title)
                    })
                    .collect(),
            };

            for ((report, report_id, semantic_id), key) in
                published.into_iter().zip(keys.into_iter())
            {
                if let (Some(rule_id), Some(occurrence_key)) =
                    (&report.rule_id, &report.occurrence_key)
                {
                    pending_occurrences
                        .insert((rule_id.clone(), occurrence_key.clone()), key.clone());
                }

                // The report is an immutable artifact; the event references it. Even a duplicate
                // gets stored — that is the whole difference from the shell ledger, which counted
                // it and threw it away.
                let report_identity = (key.clone(), source.to_string(), round, report_id.clone());

                if stage.provenance.is_some() {
                    selected_report_ids.push(semantic_id);
                    input_artifact_ids.push(report_id.clone());
                }

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

            if stage.result_contract == ReviewerResultContract::V2 {
                let provenance = stage.provenance.as_ref().ok_or_else(|| {
                    StoreError::Conflict(
                        "ReviewerResult@2 dispositions require canonical Attempt provenance".into(),
                    )
                })?;
                for disposition in &stage.disputes {
                    let finding_id = disposition.fp.trim();
                    if self.ledger.get(finding_id).is_none() {
                        return Err(StoreError::Conflict(format!(
                            "{source} disposition names Finding `{finding_id}` outside its assigned prior Finding Set"
                        )));
                    }
                    let position = match disposition.position.trim() {
                        "corroborate" => FindingDispositionPosition::Corroborate,
                        "not_reproduced" => FindingDispositionPosition::NotReproduced,
                        "dispute" => FindingDispositionPosition::Dispute,
                        _ => {
                            return Err(StoreError::Conflict(format!(
                                "{source} disposition has an invalid position"
                            )));
                        }
                    };
                    let payload = FindingDispositionV1 {
                        finding_id: finding_id.to_string(),
                        source: source.to_string(),
                        position,
                        reason: disposition.reason.clone(),
                        round,
                        subject_id: provenance.subject_id.clone(),
                    };
                    payload.validate().map_err(StoreError::Conflict)?;
                    let (record_id, envelope) = self
                        .cas
                        .put_artifact(
                            review_core::contract::FINDING_DISPOSITION_V1,
                            provenance.producer.clone(),
                            provenance.input_artifacts.clone(),
                            Some(provenance.subject_snapshot_id.clone()),
                            serde_json::to_value(payload)?,
                        )
                        .map_err(|error| StoreError::Conflict(error.to_string()))?;
                    relation_ids.push(envelope.artifact_id);
                    input_artifact_ids.push(record_id.clone());

                    if position != FindingDispositionPosition::Dispute {
                        continue;
                    }
                    let contestable = matches!(
                        projected.get(finding_id).map(|finding| finding.status),
                        Some(Status::Open | Status::Fixed)
                    );
                    if !contestable {
                        continue;
                    }
                    let payload = json!({
                        "key": finding_id,
                        "status": Status::Contested.as_str(),
                        "note": format!("contested by {source}: {}", disposition.reason),
                        "round": round,
                    });
                    let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload)
                        .correlating(finding_id.to_string())
                        .referencing(vec![record_id]);
                    apply_candidate(&mut projected, &event, self.cas)?;
                    events.push(event);
                    summary.contested += 1;
                }
            }

            // Reviewer disputes are part of the legacy contract the model is asked to answer. A
            // `confirm` above becomes a current, provenance-carrying Report with an explicit
            // corroborates relation. A `refute` on a prior claim's `claim_id` says "I think this
            // is wrong". Fold it:
            // an active claim a reviewer refutes becomes `contested`, which blocks convergence
            // and flags the claim for human adjudication rather than leaving the dispute inert in
            // raw CAS output.
            for dispute in &stage.disputes {
                if stage.result_contract != ReviewerResultContract::V1 {
                    continue;
                }
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
        Ok(CanonicalReduction {
            summary,
            selected_report_ids,
            relation_ids,
            input_artifact_ids,
            reducer_version: if stages
                .iter()
                .any(|stage| stage.result_contract == ReviewerResultContract::V2)
            {
                review_core::FINDING_REDUCER_VERSION_V2
            } else {
                review_core::FINDING_REDUCER_VERSION
            },
        })
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

/// A new canonical Finding is derived from the first selected Report that establishes it.
pub fn canonical_finding_id(report_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/finding-id/v1\0");
    hasher.update(report_id.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn canonical_stage_keys(
    reports: &[(&FindingReport, String, String)],
    prior: &Ledger,
    pending_occurrences: &std::collections::BTreeMap<(String, String), String>,
) -> Result<Vec<String>, StoreError> {
    let mut parent: Vec<usize> = (0..reports.len()).collect();
    let find = |parent: &mut Vec<usize>, mut index: usize| {
        while parent[index] != index {
            let grandparent = parent[parent[index]];
            parent[index] = grandparent;
            index = grandparent;
        }
        index
    };
    let union = |parent: &mut Vec<usize>, left: usize, right: usize| {
        let find_root = |mut index: usize| {
            while parent[index] != index {
                index = parent[index];
            }
            index
        };
        let left = find_root(left);
        let right = find_root(right);
        if left != right {
            let (keep, merge) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            parent[merge] = keep;
        }
    };

    let mut by_report = std::collections::BTreeMap::new();
    for (index, (_, _, artifact_id)) in reports.iter().enumerate() {
        if let Some(previous) = by_report.insert(artifact_id.as_str(), index) {
            union(&mut parent, previous, index);
        }
    }

    let mut occurrence_first = std::collections::BTreeMap::new();
    for (index, (report, _, _)) in reports.iter().enumerate() {
        if let (Some(rule_id), Some(occurrence_key)) = (&report.rule_id, &report.occurrence_key) {
            if let Some(previous) = occurrence_first.insert((rule_id, occurrence_key), index) {
                union(&mut parent, previous, index);
            }
        }
    }

    let mut first_report = std::collections::BTreeMap::new();
    for index in 0..reports.len() {
        let root = find(&mut parent, index);
        first_report.entry(root).or_insert(index);
    }
    let mut candidates: std::collections::BTreeMap<usize, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut outgoing: std::collections::BTreeMap<usize, BTreeSet<usize>> =
        std::collections::BTreeMap::new();
    for (index, (report, _, _)) in reports.iter().enumerate() {
        let root = find(&mut parent, index);
        if let (Some(rule_id), Some(occurrence_key)) = (&report.rule_id, &report.occurrence_key) {
            let occurrence = (rule_id.clone(), occurrence_key.clone());
            if let Some(finding) = pending_occurrences
                .get(&occurrence)
                .map(String::as_str)
                .or_else(|| prior.finding_for_occurrence(rule_id, occurrence_key))
            {
                candidates
                    .entry(root)
                    .or_default()
                    .insert(finding.to_string());
            }
        }
        for relation in &report.relations {
            match (relation.kind, relation.target.kind) {
                (
                    review_core::RelationKind::Corroborates,
                    review_core::finding::ClaimTargetKind::Report,
                ) => {
                    let target = by_report.get(relation.target.id.as_str()).ok_or_else(|| {
                        StoreError::Conflict(format!(
                            "Report {} corroborates Report `{}` outside its selected attempt",
                            reports[index].2, relation.target.id
                        ))
                    })?;
                    let target_root = find(&mut parent, *target);
                    if target_root != root {
                        outgoing.entry(root).or_default().insert(target_root);
                    }
                }
                (_, review_core::finding::ClaimTargetKind::Report) => {
                    if !by_report.contains_key(relation.target.id.as_str()) {
                        return Err(StoreError::Conflict(format!(
                            "Report {} disputes Report `{}` outside its selected attempt",
                            reports[index].2, relation.target.id
                        )));
                    }
                }
                (_, review_core::finding::ClaimTargetKind::Finding) => {
                    if prior.get(&relation.target.id).is_none() {
                        return Err(StoreError::Conflict(format!(
                            "Report {} relates to Finding `{}` outside its input Finding Set",
                            reports[index].2, relation.target.id
                        )));
                    }
                    if relation.kind == review_core::RelationKind::Corroborates {
                        candidates
                            .entry(root)
                            .or_default()
                            .insert(relation.target.id.clone());
                    }
                }
            }
        }
    }

    let mut states = std::collections::BTreeMap::new();
    let mut resolved = std::collections::BTreeMap::new();
    let mut keys = Vec::with_capacity(reports.len());
    for index in 0..reports.len() {
        let root = find(&mut parent, index);
        keys.push(resolve_canonical_group(
            root,
            reports,
            &first_report,
            &outgoing,
            &candidates,
            &mut states,
            &mut resolved,
        )?);
    }
    Ok(keys)
}

fn resolve_canonical_group(
    root: usize,
    reports: &[(&FindingReport, String, String)],
    first_report: &std::collections::BTreeMap<usize, usize>,
    outgoing: &std::collections::BTreeMap<usize, BTreeSet<usize>>,
    external: &std::collections::BTreeMap<usize, BTreeSet<String>>,
    states: &mut std::collections::BTreeMap<usize, u8>,
    resolved: &mut std::collections::BTreeMap<usize, String>,
) -> Result<String, StoreError> {
    if let Some(finding) = resolved.get(&root) {
        return Ok(finding.clone());
    }
    if states.get(&root) == Some(&1) {
        return Err(StoreError::Conflict(
            "cyclic Report corroboration has no authoritative first Report".into(),
        ));
    }
    states.insert(root, 1);
    let mut candidates = external.get(&root).cloned().unwrap_or_default();
    for target in outgoing.get(&root).into_iter().flatten() {
        candidates.insert(resolve_canonical_group(
            *target,
            reports,
            first_report,
            outgoing,
            external,
            states,
            resolved,
        )?);
    }
    if candidates.len() > 1 {
        return Err(StoreError::Conflict(format!(
            "corroboration and occurrence authority disagree for Report {}",
            reports[first_report[&root]].2
        )));
    }
    let finding = candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| canonical_finding_id(&reports[first_report[&root]].2));
    states.insert(root, 2);
    resolved.insert(root, finding.clone());
    Ok(finding)
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
    let mut projected = LedgerProjection::rebuild(store, cas, run_id)?.into_ledger();
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
    fn a_projection_fast_forwards_after_its_run_log_advances() {
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

        let ingest = Ingest::from_projection(&mut store, &cas, "run", projection).unwrap();
        assert_eq!(ingest.event_count, 1);
        assert_eq!(ingest.ledger().round, 2);
    }

    #[test]
    fn a_projection_ahead_of_the_run_log_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut source = EventStore::open(directory.path().join("source.sqlite")).unwrap();
        source
            .append_legacy(
                "run",
                &cas,
                NewEvent::new(EVENT_GENERATION_ADVANCED, json!({ "round": 2 })),
            )
            .unwrap();
        let projection = LedgerProjection::rebuild(&source, &cas, "run").unwrap();
        let mut empty = EventStore::open(directory.path().join("empty.sqlite")).unwrap();

        let error = match Ingest::from_projection(&mut empty, &cas, "run", projection) {
            Ok(_) => panic!("projection ahead of the log was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("covers 1 events"), "{error}");
        assert!(error.to_string().contains("log contains 0"), "{error}");
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

    fn typed_report(rule: Option<&str>, occurrence: Option<&str>) -> FindingReport {
        FindingReport {
            title: "claim".into(),
            severity: Severity::Major,
            locations: vec![review_core::Location::file("src/lib.rs")],
            body: "body".into(),
            fix: "fix".into(),
            confidence: 0.9,
            failure_trace: None,
            rule_id: rule.map(str::to_string),
            occurrence_key: occurrence.map(str::to_string),
            relations: Vec::new(),
        }
    }

    #[test]
    fn exact_occurrence_keys_attach_but_disputes_do_not_collapse_claims() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut prior = Ledger::default();
        let first = typed_report(Some("afactory/retry-loop@1"), Some("loop-7"));
        let first_id = cas
            .put_json(&serde_json::to_value(&first).unwrap())
            .unwrap();
        apply_candidate(
            &mut prior,
            &NewEvent::new(
                EVENT_FINDING_REPORTED,
                json!({
                    "key": "existing-finding",
                    "round": 1,
                    "source": "first",
                    "report_id": first_id,
                }),
            )
            .referencing(vec![first_id]),
            &cas,
        )
        .unwrap();

        let repeated = typed_report(Some("afactory/retry-loop@1"), Some("loop-7"));
        let repeated_id = format!("sha256:{}", "b".repeat(64));
        let keys = canonical_stage_keys(
            &[(&repeated, repeated_id.clone(), repeated_id)],
            &prior,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(keys, ["existing-finding"]);

        let mut disputed = typed_report(None, None);
        disputed.relations.push(review_core::Relation {
            kind: review_core::RelationKind::Disputes,
            target: review_core::finding::RelationTarget {
                kind: review_core::finding::ClaimTargetKind::Finding,
                id: "existing-finding".into(),
            },
            reason: Some("different evidence".into()),
        });
        let disputed_id = format!("sha256:{}", "c".repeat(64));
        let keys = canonical_stage_keys(
            &[(&disputed, disputed_id.clone(), disputed_id.clone())],
            &prior,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(keys, [canonical_finding_id(&disputed_id)]);
    }

    #[test]
    fn explicit_corroboration_attaches_to_the_named_prior_finding() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut prior = Ledger::default();
        let first = typed_report(None, None);
        let first_id = cas
            .put_json(&serde_json::to_value(&first).unwrap())
            .unwrap();
        apply_candidate(
            &mut prior,
            &NewEvent::new(
                EVENT_FINDING_REPORTED,
                json!({
                    "key": "existing-finding",
                    "round": 1,
                    "source": "first",
                    "report_id": first_id,
                }),
            )
            .referencing(vec![first_id]),
            &cas,
        )
        .unwrap();
        let mut corroborating = typed_report(None, None);
        corroborating.relations.push(review_core::Relation {
            kind: review_core::RelationKind::Corroborates,
            target: review_core::finding::RelationTarget {
                kind: review_core::finding::ClaimTargetKind::Finding,
                id: "existing-finding".into(),
            },
            reason: None,
        });
        let report_id = format!("sha256:{}", "d".repeat(64));
        let keys = canonical_stage_keys(
            &[(&corroborating, report_id.clone(), report_id)],
            &prior,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(keys, ["existing-finding"]);
    }

    #[test]
    fn same_attempt_corroboration_uses_the_target_reports_identity() {
        let prior = Ledger::default();
        let target_id = format!("sha256:{}", "f".repeat(64));
        let source_id = format!("sha256:{}", "0".repeat(64));
        let target = typed_report(None, None);
        let mut corroborating = typed_report(None, None);
        corroborating.relations.push(review_core::Relation {
            kind: review_core::RelationKind::Corroborates,
            target: review_core::finding::RelationTarget {
                kind: review_core::finding::ClaimTargetKind::Report,
                id: target_id.clone(),
            },
            reason: None,
        });
        let keys = canonical_stage_keys(
            &[
                (&corroborating, source_id.clone(), source_id),
                (&target, target_id.clone(), target_id.clone()),
            ],
            &prior,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            keys,
            [
                canonical_finding_id(&target_id),
                canonical_finding_id(&target_id)
            ]
        );
    }
}
