//! The Findings Ledger projection, and the convergence policy it feeds.
//!
//! This is a *projection*: it holds no truth of its own and is rebuilt by folding the event log.
//! Delete it and replay; you get the same answer. That is the property the shell harness could
//! not have, because its JSONL file was the only copy of its own state.
//!
//! The fold reproduces `ledger.sh`'s decisions exactly — same statuses, same effective
//! severities, same news rounds, same verdict. It has to: the migration is only safe if the new
//! engine reaches the old conclusions on every case the old one has ever seen. What it does
//! *not* reproduce is the loss — every report stays attached, and a resolution never overwrites
//! the note that preceded it.

use std::{collections::BTreeMap, sync::Arc};

use review_core::{EventType, RoundStartedPayloadV1, Severity, SubjectKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cas::Cas;
use crate::store::EventStore;

pub const EVENT_FINDING_REPORTED: EventType = EventType::FindingReportedV1;
pub const EVENT_FINDING_RESOLVED: EventType = EventType::FindingResolvedV1;
pub const EVENT_GENERATION_ADVANCED: EventType = EventType::GenerationAdvancedV1;

/// Scope has exactly two durable meanings. `None` on an [`AttachedReport`] is fail-closed
/// compatibility metadata for evidence with no derivable exact Round Subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportScope {
    In,
    Out,
}

impl ReportScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

/// A Round whose Subject could not supply Report Scope authority during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeAuthorityFailure {
    pub round: u32,
    pub subject_id: String,
    pub reason: String,
}

/// The legacy status set, kept verbatim so equivalence can be checked field by field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Fixed,
    Rejected,
    Wontfix,
    Contested,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "open" => Status::Open,
            "fixed" => Status::Fixed,
            "rejected" => Status::Rejected,
            "wontfix" => Status::Wontfix,
            "contested" => Status::Contested,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Fixed => "fixed",
            Status::Rejected => "rejected",
            Status::Wontfix => "wontfix",
            Status::Contested => "contested",
        }
    }

    /// Blocks convergence while at or above the gate.
    fn is_active(self) -> bool {
        matches!(self, Status::Open | Status::Contested)
    }

    /// Never auto-reopened: reviewers only ever see open claims, so they rediscover these
    /// forever and an automatic reopen would loop the run to exhaustion.
    fn is_declined(self) -> bool {
        matches!(self, Status::Rejected | Status::Wontfix)
    }
}

/// One report, kept immutable. The shell harness discarded every report after the first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedReport {
    pub report_id: String,
    pub round: u32,
    pub source: String,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ReportScope>,
}

impl AttachedReport {
    pub fn scope_label(&self) -> &'static str {
        self.scope.map_or("unknown", ReportScope::as_str)
    }
}

/// One transition, appended rather than overwritten. `ledger.sh resolve` wrote over `.note`,
/// which is how a reopen erased the fix note that came before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub round: u32,
    pub kind: TransitionKind,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Reported,
    Duplicate,
    Escalated,
    Reopened,
    /// Severity adopted in place on a declined finding; status deliberately unchanged.
    AdoptedWhileDeclined,
    Resolved(Status),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The legacy fingerprint. A grouping hint, never proof of claim identity.
    pub key: String,
    pub status: Status,
    pub severity: Severity,
    /// `ledger.sh`'s `.round`: when this finding last counted as convergence news.
    pub news_round: u32,
    pub last_seen_round: u32,
    pub source: String,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub body: String,
    /// The currently adopted remedy. Absent only for artifact-less legacy imports.
    pub fix: Option<String>,
    pub confidence: Option<f64>,
    /// Aggregate of active Report claims for presentation, never Scope stamped onto identity.
    /// `None` means exact Scope authority was unavailable.
    pub convergence_scope: Option<ReportScope>,
    /// Highest active non-out claim severity. `None` means active claims are wholly out.
    pub convergence_severity: Option<Severity>,
    /// Scope-aware News used by convergence; separate from frozen legacy `news_round`.
    pub scoped_news_round: Option<u32>,
    /// Every report, in arrival order — including the ones the shell harness dropped.
    pub reports: Vec<AttachedReport>,
    /// Every transition, in order — including the notes a resolution used to overwrite.
    pub history: Vec<Transition>,
}

impl Finding {
    /// The note the shell harness would have been left holding: the last one written.
    pub fn current_note(&self) -> Option<&str> {
        self.history
            .iter()
            .rev()
            .find_map(|t| t.note.as_deref().filter(|n| !n.is_empty()))
    }

    pub fn corroborating_sources(&self) -> Vec<&str> {
        let mut sources: Vec<&str> = self.reports.iter().map(|r| r.source.as_str()).collect();
        sources.dedup();
        sources
    }

    pub fn convergence_scope_label(&self) -> &'static str {
        self.convergence_scope
            .map_or("unknown", ReportScope::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Ledger {
    findings: BTreeMap<String, Finding>,
    order: Vec<String>,
    active_scope: Option<ActiveScope>,
    scope_authority_failures: Vec<ScopeAuthorityFailure>,
    pub round: u32,
}

#[derive(Debug, Clone)]
struct ActiveScope {
    round: u32,
    subject: SubjectScope,
}

#[derive(Debug, Clone)]
enum SubjectScope {
    WholeTree,
    Diff(Arc<[String]>),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Converged,
    NotConverged,
    /// The round cap was reached with work outstanding. A third verdict, never a pass.
    Exhausted,
}

impl Verdict {
    /// The exit codes `ledger.sh converged` uses, preserved so callers can be compared directly.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Converged => 0,
            Verdict::NotConverged => 1,
            Verdict::Exhausted => 3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConvergencePolicy {
    pub clean_rounds: u32,
    pub max_rounds: u32,
    pub gate: Severity,
}

impl Default for ConvergencePolicy {
    fn default() -> Self {
        Self {
            clean_rounds: 1,
            max_rounds: 3,
            gate: Severity::Major,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Convergence {
    pub round: u32,
    pub open_blocking: usize,
    pub new_recent: usize,
    pub verdict: Verdict,
}

impl Ledger {
    /// Rebuild from the event log. The only constructor — there is no way to hand-edit state in.
    pub fn rebuild(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
    ) -> Result<Ledger, crate::store::StoreError> {
        let mut ledger = Ledger {
            round: 1,
            ..Default::default()
        };
        for event in store.replay(run_id)? {
            ledger.apply(event.event_type, &event.payload, &event.artifact_refs, cas)?;
        }
        Ok(ledger)
    }

    /// Fold one event in. Public so an ingest can keep a live projection without re-reading the
    /// whole log after every append — the fold is the same code either way.
    pub fn apply_event(
        &mut self,
        event: &review_core::RunEvent,
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        self.apply(event.event_type, &event.payload, &event.artifact_refs, cas)
    }

    fn apply(
        &mut self,
        event_type: EventType,
        payload: &Value,
        artifact_refs: &[String],
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        match event_type {
            EventType::RoundStartedV1 => self.apply_round_started(payload, cas)?,
            EVENT_GENERATION_ADVANCED => {
                let round = payload
                    .get("round")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| malformed("GenerationAdvanced@1", "missing integer `round`"))?;
                self.round = u32::try_from(round)
                    .map_err(|_| malformed("GenerationAdvanced@1", "`round` exceeds u32"))?;
            }
            EVENT_FINDING_REPORTED => self.apply_report(payload, artifact_refs, cas)?,
            EVENT_FINDING_RESOLVED => self.apply_resolution(payload)?,
            _ => {}
        }
        Ok(())
    }

    fn apply_report(
        &mut self,
        payload: &Value,
        artifact_refs: &[String],
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let key = required_string(payload, "FindingReported@1", "key")?;
        let source = required_string(payload, "FindingReported@1", "source")?;
        let round = required_round(payload, "FindingReported@1")?;
        let imported = payload.get("imported").and_then(Value::as_bool) == Some(true);
        let (report_id, report) = match (artifact_refs, imported) {
            ([report_id], false) => {
                if payload.get("report_id").and_then(Value::as_str) != Some(report_id) {
                    return Err(malformed(
                        "FindingReported@1",
                        "payload `report_id` does not match its sole artifact reference",
                    ));
                }
                let value = cas.get_json(report_id).map_err(|error| {
                    crate::store::StoreError::Artifact(format!(
                        "FindingReported@1 references {report_id}: {error}"
                    ))
                })?;
                (
                    report_id.clone(),
                    ReportProjection::from_artifact(report_id, &value)?,
                )
            }
            ([], true) => (
                String::new(),
                ReportProjection::from_legacy_payload(payload)?,
            ),
            ([], false) => {
                return Err(malformed(
                    "FindingReported@1",
                    "artifact-less reports require `imported: true`",
                ));
            }
            _ => {
                return Err(malformed(
                    "FindingReported@1",
                    "expected exactly one report artifact, or none for an explicit import",
                ));
            }
        };
        let severity = report.severity;
        let scope = self.report_scope(round, &report.location);
        let attached = AttachedReport {
            report_id,
            round,
            source: source.clone(),
            severity,
            scope,
        };

        let Some(existing) = self.findings.get_mut(&key) else {
            let convergence_severity = (scope != Some(ReportScope::Out)).then_some(severity);
            self.order.push(key.clone());
            self.findings.insert(
                key.clone(),
                Finding {
                    key,
                    status: Status::Open,
                    severity,
                    news_round: round,
                    last_seen_round: round,
                    source,
                    file: report.file,
                    line: report.line,
                    title: report.title,
                    body: report.body,
                    fix: report.fix,
                    confidence: report.confidence,
                    convergence_scope: scope,
                    convergence_severity,
                    scoped_news_round: convergence_severity.map(|_| round),
                    reports: vec![attached],
                    history: vec![Transition {
                        round,
                        kind: TransitionKind::Reported,
                        note: None,
                    }],
                },
            );
            return Ok(());
        };

        let previous_scoped_severity = existing.convergence_severity;
        let scoped_news = scope != Some(ReportScope::Out)
            && previous_scoped_severity.is_none_or(|previous| severity.rank() > previous.rank());

        // Every report is kept, whatever the projection then decides about it.
        existing.reports.push(attached);

        let higher = severity.rank() > existing.severity.rank();
        let kind = if existing.status.is_declined() {
            if higher {
                TransitionKind::AdoptedWhileDeclined
            } else {
                TransitionKind::Duplicate
            }
        } else if existing.status == Status::Fixed && existing.last_seen_round < round {
            TransitionKind::Reopened
        } else if existing.status.is_active() && higher {
            TransitionKind::Escalated
        } else {
            TransitionKind::Duplicate
        };

        existing.last_seen_round = round;
        match kind {
            TransitionKind::Duplicate => {}
            TransitionKind::Reopened => {
                existing.status = Status::Open;
                existing.news_round = round;
                adopt(existing, &report, &source);
            }
            TransitionKind::Escalated | TransitionKind::AdoptedWhileDeclined => {
                existing.news_round = round;
                adopt(existing, &report, &source);
            }
            _ => {}
        }

        let note = match kind {
            TransitionKind::Reopened => Some(format!(
                "reopened: re-reported by {source} in round {round}"
            )),
            TransitionKind::Escalated => Some(format!(
                "escalated: re-reported as {} by {source} in round {round}",
                severity_name(severity)
            )),
            _ => None,
        };
        if kind == TransitionKind::Reopened {
            existing.convergence_scope = scope;
            existing.convergence_severity = (scope != Some(ReportScope::Out)).then_some(severity);
        } else {
            existing.convergence_scope = combine_scope(existing.convergence_scope, scope);
            if scope != Some(ReportScope::Out)
                && existing
                    .convergence_severity
                    .is_none_or(|current| severity.rank() > current.rank())
            {
                existing.convergence_severity = Some(severity);
            }
        }
        if scoped_news || (scope != Some(ReportScope::Out) && kind == TransitionKind::Reopened) {
            existing.scoped_news_round = Some(round);
        }
        existing.history.push(Transition { round, kind, note });
        Ok(())
    }

    fn apply_round_started(
        &mut self,
        payload: &Value,
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let started: RoundStartedPayloadV1 = serde_json::from_value(payload.clone())
            .map_err(|error| malformed("RoundStarted@1", &error.to_string()))?;
        started
            .validate()
            .map_err(|error| malformed("RoundStarted@1", &error))?;
        let subject_scope = match crate::resolve_subject_scope(cas, &started.subject_id)
            .map_err(|error| error.to_string())
            .and_then(|resolved| match resolved.subject.kind {
                SubjectKind::WholeTree => Ok(SubjectScope::WholeTree),
                SubjectKind::Diff => {
                    resolved
                        .changed_paths
                        .map(SubjectScope::Diff)
                        .ok_or_else(|| {
                            format!(
                                "diff Subject {} resolved without changed paths",
                                started.subject_id
                            )
                        })
                }
            }) {
            Ok(scope) => scope,
            Err(reason) => {
                self.scope_authority_failures.push(ScopeAuthorityFailure {
                    round: started.round,
                    subject_id: started.subject_id.clone(),
                    reason,
                });
                SubjectScope::Unavailable
            }
        };
        self.active_scope = Some(ActiveScope {
            round: started.round,
            subject: subject_scope,
        });
        Ok(())
    }

    fn report_scope(&self, round: u32, location: &ReportLocation) -> Option<ReportScope> {
        let Some(active) = self
            .active_scope
            .as_ref()
            .filter(|active| active.round == round)
        else {
            return None;
        };
        match (&active.subject, location) {
            (SubjectScope::Unavailable, _) | (_, ReportLocation::Unrecorded) => None,
            (SubjectScope::WholeTree, _) | (SubjectScope::Diff(_), ReportLocation::ChangeWide) => {
                Some(ReportScope::In)
            }
            (SubjectScope::Diff(paths), ReportLocation::Path(path)) => Some(
                if paths
                    .binary_search_by(|item| item.as_str().cmp(path))
                    .is_ok()
                {
                    ReportScope::In
                } else {
                    ReportScope::Out
                },
            ),
        }
    }

    fn apply_resolution(&mut self, payload: &Value) -> Result<(), crate::store::StoreError> {
        let key = required_string(payload, "FindingResolved@1", "key")?;
        let status_name = required_string(payload, "FindingResolved@1", "status")?;
        let status = Status::parse(&status_name).ok_or_else(|| {
            malformed(
                "FindingResolved@1",
                &format!("invalid status `{status_name}`"),
            )
        })?;
        let round = required_round(payload, "FindingResolved@1")?;
        let note = match payload.get("note") {
            None | Some(Value::Null) => None,
            Some(Value::String(note)) => Some(note.clone()),
            Some(_) => {
                return Err(malformed(
                    "FindingResolved@1",
                    "`note` must be a string or null",
                ));
            }
        };
        let finding = self.findings.get_mut(&key).ok_or_else(|| {
            malformed("FindingResolved@1", &format!("unknown finding key `{key}`"))
        })?;
        finding.status = status;
        finding.history.push(Transition {
            round,
            kind: TransitionKind::Resolved(status),
            note,
        });
        Ok(())
    }

    /// Findings in first-reported order, which is the order the shell ledger's file had.
    pub fn findings(&self) -> Vec<&Finding> {
        self.order
            .iter()
            .filter_map(|key| self.findings.get(key))
            .collect()
    }

    pub fn get(&self, key: &str) -> Option<&Finding> {
        self.findings.get(key)
    }

    pub fn len(&self) -> usize {
        self.findings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Scope failures are diagnostics, not replay failures: affected reports remain unknown.
    pub fn scope_authority_failures(&self) -> &[ScopeAuthorityFailure] {
        &self.scope_authority_failures
    }

    /// The convergence decision, computed exactly as `ledger.sh converged` computes it.
    ///
    /// `new_recent` counts by news round and **ignores status** — so a finding fixed in the
    /// current round still blocks. That is not an oversight in the original: it is what forces a
    /// fix to survive another review before the run may call itself converged.
    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        let gate = policy.gate.rank();
        let open_blocking = self
            .findings
            .values()
            .filter(|finding| {
                finding.status.is_active()
                    && finding
                        .convergence_severity
                        .is_some_and(|severity| severity.rank() >= gate)
            })
            .count();
        let since = self.round as i64 - policy.clean_rounds as i64;
        let new_recent = self
            .findings
            .values()
            .filter(|finding| {
                finding
                    .scoped_news_round
                    .is_some_and(|round| i64::from(round) > since)
                    && finding
                        .convergence_severity
                        .is_some_and(|severity| severity.rank() >= gate)
            })
            .count();

        let verdict = if open_blocking == 0 && new_recent == 0 && self.round >= policy.clean_rounds
        {
            Verdict::Converged
        } else if self.round >= policy.max_rounds {
            Verdict::Exhausted
        } else {
            Verdict::NotConverged
        };
        Convergence {
            round: self.round,
            open_blocking,
            new_recent,
            verdict,
        }
    }
}

struct ReportProjection {
    severity: Severity,
    file: String,
    location: ReportLocation,
    line: Option<i64>,
    title: String,
    body: String,
    fix: Option<String>,
    confidence: Option<f64>,
}

enum ReportLocation {
    ChangeWide,
    Path(String),
    Unrecorded,
}

impl ReportProjection {
    fn from_artifact(report_id: &str, value: &Value) -> Result<Self, crate::store::StoreError> {
        let required = |field: &str| {
            value[field]
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| {
                    crate::store::StoreError::Artifact(format!(
                        "report {report_id} has no non-empty string `{field}`"
                    ))
                })
        };
        let severity_name = required("severity")?;
        let severity = parse_severity(severity_name).ok_or_else(|| {
            crate::store::StoreError::Artifact(format!(
                "report {report_id} has invalid severity `{severity_name}`"
            ))
        })?;
        let file = value["file"].as_str().ok_or_else(|| {
            crate::store::StoreError::Artifact(format!("report {report_id} has no string `file`"))
        })?;
        let line = match value.get("line") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_i64().filter(|line| *line > 0).ok_or_else(|| {
                crate::store::StoreError::Artifact(format!(
                    "report {report_id} has invalid positive integer `line`"
                ))
            })?),
        };
        let confidence = match value.get("confidence") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|value| (0.0..=1.0).contains(value))
                    .ok_or_else(|| {
                        crate::store::StoreError::Artifact(format!(
                            "report {report_id} has invalid `confidence` outside [0,1]"
                        ))
                    })?,
            ),
        };
        let location = if file.trim().is_empty() {
            ReportLocation::ChangeWide
        } else {
            ReportLocation::Path(file.to_string())
        };
        Ok(Self {
            severity,
            file: if file.trim().is_empty() {
                "(change-wide)".to_string()
            } else {
                file.to_string()
            },
            location,
            line,
            title: required("title")?.to_string(),
            body: required("body")?.to_string(),
            fix: Some(required("fix")?.to_string()),
            confidence,
        })
    }

    fn from_legacy_payload(payload: &Value) -> Result<Self, crate::store::StoreError> {
        let severity_name = required_string(payload, "imported FindingReported@1", "severity")?;
        let severity = parse_severity(&severity_name).ok_or_else(|| {
            malformed(
                "imported FindingReported@1",
                &format!("invalid severity `{severity_name}`"),
            )
        })?;
        Ok(Self {
            severity,
            file: required_string(payload, "imported FindingReported@1", "file")?,
            location: ReportLocation::Unrecorded,
            line: payload["line"].as_i64(),
            title: required_string(payload, "imported FindingReported@1", "title")?,
            body: required_string(payload, "imported FindingReported@1", "body")?,
            fix: None,
            confidence: payload["confidence"].as_f64(),
        })
    }
}

fn combine_scope(current: Option<ReportScope>, next: Option<ReportScope>) -> Option<ReportScope> {
    match (current, next) {
        (Some(ReportScope::In), _) | (_, Some(ReportScope::In)) => Some(ReportScope::In),
        (Some(ReportScope::Out), Some(ReportScope::Out)) => Some(ReportScope::Out),
        _ => None,
    }
}

fn malformed(event: &str, detail: &str) -> crate::store::StoreError {
    crate::store::StoreError::Conflict(format!("malformed {event}: {detail}"))
}

fn required_string(
    payload: &Value,
    event: &str,
    field: &str,
) -> Result<String, crate::store::StoreError> {
    let value = payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| malformed(event, &format!("missing non-empty string `{field}`")))?;
    Ok(value.to_string())
}

fn required_round(payload: &Value, event: &str) -> Result<u32, crate::store::StoreError> {
    let round = payload
        .get("round")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed(event, "missing integer `round`"))?;
    u32::try_from(round).map_err(|_| malformed(event, "`round` exceeds u32"))
}

fn adopt(finding: &mut Finding, report: &ReportProjection, source: &str) {
    finding.severity = report.severity;
    finding.source = source.to_string();
    finding.file.clone_from(&report.file);
    finding.line = report.line;
    finding.title.clone_from(&report.title);
    finding.body.clone_from(&report.body);
    finding.fix.clone_from(&report.fix);
    finding.confidence = report.confidence;
}

fn parse_severity(s: &str) -> Option<Severity> {
    Some(match s {
        "blocker" => Severity::Blocker,
        "major" => Severity::Major,
        "minor" => Severity::Minor,
        _ => return None,
    })
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "blocker",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}
