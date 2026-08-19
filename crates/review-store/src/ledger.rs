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

use std::collections::BTreeMap;

use review_core::Severity;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::EventStore;

pub const EVENT_FINDING_REPORTED: &str = "FindingReported@1";
pub const EVENT_FINDING_RESOLVED: &str = "FindingResolved@1";
pub const EVENT_GENERATION_ADVANCED: &str = "GenerationAdvanced@1";

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
    pub confidence: Option<f64>,
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
}

#[derive(Debug, Clone, Default)]
pub struct Ledger {
    findings: BTreeMap<String, Finding>,
    order: Vec<String>,
    pub round: u32,
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
    pub fn rebuild(store: &EventStore, run_id: &str) -> Result<Ledger, crate::store::StoreError> {
        let mut ledger = Ledger {
            round: 1,
            ..Default::default()
        };
        for event in store.replay(run_id)? {
            ledger.apply(&event.event_type, &event.payload, &event.artifact_refs);
        }
        Ok(ledger)
    }

    /// Fold one event in. Public so an ingest can keep a live projection without re-reading the
    /// whole log after every append — the fold is the same code either way.
    pub fn apply_event(&mut self, event: &review_core::RunEvent) {
        self.apply(&event.event_type, &event.payload, &event.artifact_refs);
    }

    fn apply(&mut self, event_type: &str, payload: &Value, artifact_refs: &[String]) {
        match event_type {
            EVENT_GENERATION_ADVANCED => {
                if let Some(round) = payload.get("round").and_then(Value::as_u64) {
                    self.round = round as u32;
                }
            }
            EVENT_FINDING_REPORTED => self.apply_report(payload, artifact_refs),
            EVENT_FINDING_RESOLVED => self.apply_resolution(payload),
            _ => {}
        }
    }

    fn apply_report(&mut self, payload: &Value, artifact_refs: &[String]) {
        let key = payload["key"].as_str().unwrap_or_default().to_string();
        let round = payload["round"].as_u64().unwrap_or(1) as u32;
        let source = payload["source"].as_str().unwrap_or_default().to_string();
        let severity = payload["severity"]
            .as_str()
            .and_then(parse_severity)
            .unwrap_or(Severity::Minor);
        let report_id = artifact_refs.first().cloned().unwrap_or_else(|| {
            payload["report_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        });

        let attached = AttachedReport {
            report_id,
            round,
            source: source.clone(),
            severity,
        };

        let Some(existing) = self.findings.get_mut(&key) else {
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
                    file: payload["file"].as_str().unwrap_or_default().to_string(),
                    line: payload["line"].as_i64(),
                    title: payload["title"].as_str().unwrap_or_default().to_string(),
                    body: payload["body"].as_str().unwrap_or_default().to_string(),
                    confidence: payload["confidence"].as_f64(),
                    reports: vec![attached],
                    history: vec![Transition {
                        round,
                        kind: TransitionKind::Reported,
                        note: None,
                    }],
                },
            );
            return;
        };

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
                adopt(existing, payload, severity, &source);
            }
            TransitionKind::Escalated | TransitionKind::AdoptedWhileDeclined => {
                existing.news_round = round;
                adopt(existing, payload, severity, &source);
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
        existing.history.push(Transition { round, kind, note });
    }

    fn apply_resolution(&mut self, payload: &Value) {
        let key = payload["key"].as_str().unwrap_or_default();
        let Some(status) = payload["status"].as_str().and_then(Status::parse) else {
            return;
        };
        let round = payload["round"].as_u64().unwrap_or(self.round as u64) as u32;
        if let Some(finding) = self.findings.get_mut(key) {
            finding.status = status;
            finding.history.push(Transition {
                round,
                kind: TransitionKind::Resolved(status),
                note: payload["note"].as_str().map(str::to_string),
            });
        }
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
            .filter(|f| f.status.is_active() && f.severity.rank() >= gate)
            .count();
        let since = self.round as i64 - policy.clean_rounds as i64;
        let new_recent = self
            .findings
            .values()
            .filter(|f| (f.news_round as i64) > since && f.severity.rank() >= gate)
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

fn adopt(finding: &mut Finding, payload: &Value, severity: Severity, source: &str) {
    finding.severity = severity;
    finding.source = source.to_string();
    if let Some(line) = payload["line"].as_i64() {
        finding.line = Some(line);
    }
    if let Some(body) = payload["body"].as_str() {
        finding.body = body.to_string();
    }
    finding.confidence = payload["confidence"].as_f64();
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
