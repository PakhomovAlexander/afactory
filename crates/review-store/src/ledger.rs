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

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

use review_core::{
    ArtifactEnvelope, CampaignManifestV1, CampaignOpenedPayloadV1, EventType,
    LEGACY_FINDING_IDENTITY_POLICY, Relation, RoundStartedPayloadV1, Severity, SubjectKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cas::Cas;

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

/// The authority boundary that failed to supply a trustworthy Scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeAuthorityKind {
    Subject,
    Report,
    RoundBinding,
}

/// A Round whose Subject or Report could not supply Report Scope authority during replay.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeAuthorityFailure {
    pub round: u32,
    pub authority: ScopeAuthorityKind,
    pub authority_id: String,
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
    /// CAS ID of the complete envelope or frozen payload referenced by the event.
    pub report_id: String,
    /// Domain-separated typed Report identity. Absent for pre-envelope history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    pub round: u32,
    pub source: String,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
    /// The Subject-dependent location selected for presentation from this immutable Report.
    pub file: String,
    pub line: Option<i64>,
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
    /// Readable claim content replaced an authority-failure placeholder for the same key.
    AuthorityRecovered,
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
    /// Path used by the legacy path/title bridge key. Stable even when Scope selects another
    /// location for presentation from a multi-location Report.
    pub identity_file: String,
    /// Line paired with `identity_file`, from the same canonical identity location.
    #[serde(default)]
    pub identity_line: Option<i64>,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub body: String,
    /// The currently adopted remedy. Absent only for artifact-less legacy imports.
    pub fix: Option<String>,
    pub confidence: Option<f64>,
    /// The claim content is an actionable placeholder for unreadable Report authority. The first
    /// readable Report for this key replaces it regardless of relative severity.
    pub authority_diagnostic: bool,
    /// Report artifacts whose claim content could not be read. This remains authoritative even
    /// when an older readable claim is fixed: resolution cannot erase a later authority failure.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unreadable_reports: BTreeSet<String>,
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
    scope_authority_failure_keys: HashSet<ScopeAuthorityFailure>,
    subject_scope_cache: BTreeMap<String, CachedSubjectScope>,
    finding_identity_policy: String,
    pub round: u32,
}

/// A Ledger projection bound to the exact run log it was rebuilt from. The private binding lets
/// trusted callers carry one replay across layers without making an arbitrary hand-built Ledger
/// admissible as convergence authority.
#[derive(Debug, Clone)]
pub struct LedgerProjection {
    run_id: String,
    event_count: u64,
    ledger: Ledger,
}

#[derive(Debug, Clone)]
struct ActiveScope {
    round: u32,
    subject_id: String,
    head_snapshot_id: String,
    subject: Arc<SubjectScope>,
}

#[derive(Debug, Clone)]
struct CachedSubjectScope {
    scope: Arc<SubjectScope>,
    change_set_id: Option<String>,
    head_snapshot_id: String,
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
    pub authority_failures_recent: usize,
    pub verdict: Verdict,
}

impl Ledger {
    pub fn finding_identity_policy(&self) -> &str {
        if self.finding_identity_policy.is_empty() {
            LEGACY_FINDING_IDENTITY_POLICY
        } else {
            &self.finding_identity_policy
        }
    }

    /// Resolve only an exact rule-owned occurrence key. Ambiguous historical state fails closed.
    pub fn finding_for_occurrence(&self, rule_id: &str, occurrence_key: &str) -> Option<&str> {
        let mut matches = self.findings.values().filter(|finding| {
            finding.reports.iter().any(|report| {
                report.rule_id.as_deref() == Some(rule_id)
                    && report.occurrence_key.as_deref() == Some(occurrence_key)
            })
        });
        let finding = matches.next()?;
        matches.next().is_none().then_some(finding.key.as_str())
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
            EventType::CampaignOpenedV1 => self.apply_campaign_opened(payload, cas)?,
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
        let (report_id, mut report) = match (artifact_refs, imported) {
            ([report_id], false) => {
                if payload.get("report_id").and_then(Value::as_str) != Some(report_id) {
                    return Err(malformed(
                        "FindingReported@1",
                        "payload `report_id` does not match its sole artifact reference",
                    ));
                }
                let projected = cas
                    .get_json(report_id)
                    .map_err(|error| format!("FindingReported@1 references {report_id}: {error}"))
                    .and_then(|value| {
                        ReportProjection::from_artifact(report_id, &value)
                            .map_err(|error| error.to_string())
                    });
                let report = match projected {
                    Ok(report) => report,
                    Err(reason) => {
                        self.record_authority_failure(
                            round,
                            ScopeAuthorityKind::Report,
                            report_id,
                            &reason,
                        );
                        ReportProjection::unreadable(report_id, &reason)
                    }
                };
                if self.finding_identity_policy() == review_core::CANONICAL_FINDING_IDENTITY_POLICY
                    && report.artifact_id.is_none()
                {
                    return Err(malformed(
                        "FindingReported@1",
                        "canonical identity requires an enveloped FindingReport@1",
                    ));
                }
                (report_id.clone(), report)
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
        if let Some(reason) = report.scope_authority_reason.as_deref() {
            self.record_authority_failure(round, ScopeAuthorityKind::Report, &report_id, reason);
        }
        if let (Some(active), Some(snapshot_id)) = (
            self.active_scope.as_ref(),
            report.subject_snapshot_id.as_deref(),
        ) && !active.head_snapshot_id.is_empty()
            && snapshot_id != active.head_snapshot_id
        {
            let reason = format!(
                "Report Subject Snapshot {snapshot_id} disagrees with active Snapshot {}",
                active.head_snapshot_id
            );
            self.record_authority_failure(
                round,
                ScopeAuthorityKind::RoundBinding,
                &report_id,
                &reason,
            );
            report.location = ReportLocation::Unrecorded;
        }
        let severity = report.severity;
        let unreadable = report.unreadable;
        let (identity_file, identity_line) = report.identity_location();
        let (scope, selected_location) = self.report_scope(round, &report.location);
        report.select_location(selected_location);
        let attached = AttachedReport {
            report_id,
            artifact_id: report.artifact_id.clone(),
            round,
            source: source.clone(),
            severity,
            rule_id: report.rule_id.clone(),
            occurrence_key: report.occurrence_key.clone(),
            relations: report.relations.clone(),
            file: report.file.clone(),
            line: report.line,
            scope,
        };

        // Authority failure evidence must not replace readable claim content. If this key has
        // readable history, retain only the diagnostic attachment. A first unreadable Report gets
        // an actionable placeholder that the first readable Report replaces unconditionally.
        if unreadable && self.findings.contains_key(&key) {
            if let Some(existing) = self.findings.get_mut(&key) {
                if !attached.report_id.is_empty() {
                    existing
                        .unreadable_reports
                        .insert(attached.report_id.clone());
                }
                existing.reports.push(attached);
            }
            return Ok(());
        }

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
                    identity_file,
                    identity_line,
                    file: report.file,
                    line: report.line,
                    title: report.title,
                    body: report.body,
                    fix: report.fix,
                    confidence: report.confidence,
                    authority_diagnostic: unreadable,
                    unreadable_reports: if unreadable {
                        BTreeSet::from([attached.report_id.clone()])
                    } else {
                        BTreeSet::new()
                    },
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

        // Every report is kept, whatever the projection then decides about it.
        existing.reports.push(attached);
        let recovered_report_ids = std::mem::take(&mut existing.unreadable_reports);

        if existing.authority_diagnostic {
            existing.authority_diagnostic = false;
            existing.last_seen_round = round;
            existing.status = Status::Open;
            existing.news_round = round;
            existing.identity_file = identity_file;
            existing.identity_line = identity_line;
            adopt(existing, &report, &source);
            existing.convergence_scope = scope;
            existing.convergence_severity = (scope != Some(ReportScope::Out)).then_some(severity);
            existing.scoped_news_round = existing.convergence_severity.map(|_| round);
            existing.history.push(Transition {
                round,
                kind: TransitionKind::AuthorityRecovered,
                note: Some(format!(
                    "authority recovered for Reports {}: readable Report supplied by {source} in \
                     round {round}; any prior resolution applied only to the authority placeholder",
                    recovered_report_ids
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            });
            return Ok(());
        }

        if !recovered_report_ids.is_empty() {
            existing.history.push(Transition {
                round,
                kind: TransitionKind::AuthorityRecovered,
                note: Some(format!(
                    "authority recovered for Reports {}: readable Report supplied by {source} in round {round}",
                    recovered_report_ids.into_iter().collect::<Vec<_>>().join(", ")
                )),
            });
        }

        let previous_scoped_severity = existing.convergence_severity;
        let scoped_news = scope != Some(ReportScope::Out)
            && previous_scoped_severity.is_none_or(|previous| severity.rank() > previous.rank());

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

    fn apply_campaign_opened(
        &mut self,
        payload: &Value,
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let opened: CampaignOpenedPayloadV1 = serde_json::from_value(payload.clone())
            .map_err(|error| malformed("CampaignOpened@1", &error.to_string()))?;
        let manifest: CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&opened.campaign_manifest_id)
                .map_err(|error| {
                    crate::store::StoreError::Artifact(format!(
                        "CampaignOpened@1 references unreadable manifest {}: {error}",
                        opened.campaign_manifest_id
                    ))
                })?,
        )
        .map_err(|error| malformed("CampaignManifest@1", &error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| malformed("CampaignManifest@1", &error))?;
        self.finding_identity_policy = manifest.finding_identity_policy;
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
        // A parsed scope may be reused, but its immutable artifact authority is reverified for
        // every Round. Otherwise a warm live projection and a cold replay can disagree after CAS
        // loss or corruption.
        let verified = cas
            .verify(&started.subject_id)
            .map(|_| ())
            .map_err(|error| error.to_string());
        let resolved_scope = if let Err(error) = verified {
            Err(error)
        } else if let Some(cached) = self.subject_scope_cache.get(&started.subject_id) {
            cached
                .change_set_id
                .as_deref()
                .map_or(Ok(()), |change_set_id| {
                    cas.verify(change_set_id)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .map(|()| (cached.scope.clone(), cached.head_snapshot_id.clone()))
        } else {
            let resolved = crate::resolve_subject_scope(cas, &started.subject_id)
                .map_err(|error| error.to_string())
                .and_then(|resolved| {
                    let change_set_id = resolved.subject.change_set_id.clone();
                    let scope = match resolved.subject.kind {
                        SubjectKind::WholeTree => Arc::new(SubjectScope::WholeTree),
                        SubjectKind::Diff => resolved
                            .changed_paths
                            .map(|paths| Arc::new(SubjectScope::Diff(paths)))
                            .ok_or_else(|| {
                                format!(
                                    "diff Subject {} resolved without changed paths",
                                    started.subject_id
                                )
                            })?,
                    };
                    Ok(CachedSubjectScope {
                        scope,
                        change_set_id,
                        head_snapshot_id: resolved.subject.head_snapshot_id,
                    })
                });
            if let Ok(cached) = &resolved {
                self.subject_scope_cache
                    .insert(started.subject_id.clone(), cached.clone());
            }
            resolved.map(|cached| (cached.scope, cached.head_snapshot_id))
        };
        let (subject_scope, head_snapshot_id) = match resolved_scope {
            Ok(authority) => authority,
            Err(reason) => {
                self.record_authority_failure(
                    started.round,
                    ScopeAuthorityKind::Subject,
                    &started.subject_id,
                    &reason,
                );
                (Arc::new(SubjectScope::Unavailable), String::new())
            }
        };
        self.active_scope = Some(ActiveScope {
            round: started.round,
            subject_id: started.subject_id,
            head_snapshot_id,
            subject: subject_scope,
        });
        Ok(())
    }

    fn report_scope(
        &mut self,
        round: u32,
        location: &ReportLocation,
    ) -> (Option<ReportScope>, Option<usize>) {
        if let Some(active) = self
            .active_scope
            .as_ref()
            .filter(|active| active.round != round)
        {
            let reason = format!(
                "Report round {round} disagrees with active RoundStarted@1 round {}",
                active.round
            );
            let subject_id = active.subject_id.clone();
            self.record_authority_failure(
                round,
                ScopeAuthorityKind::RoundBinding,
                &subject_id,
                &reason,
            );
            return (None, location.first_index());
        }
        let Some(active) = self.active_scope.as_ref() else {
            return (None, location.first_index());
        };
        match (active.subject.as_ref(), location) {
            (SubjectScope::Unavailable, _) | (_, ReportLocation::Unrecorded) => {
                (None, location.first_index())
            }
            (SubjectScope::WholeTree, _) | (SubjectScope::Diff(_), ReportLocation::ChangeWide) => {
                (Some(ReportScope::In), location.first_index())
            }
            (SubjectScope::Diff(changed_paths), ReportLocation::Paths(paths)) => {
                if let Some(index) = paths.iter().position(|location| {
                    review_core::contains_report_path(changed_paths, &location.path)
                }) {
                    (Some(ReportScope::In), Some(index))
                } else {
                    (Some(ReportScope::Out), location.first_index())
                }
            }
        }
    }

    fn record_authority_failure(
        &mut self,
        round: u32,
        authority: ScopeAuthorityKind,
        authority_id: &str,
        reason: &str,
    ) {
        let failure = ScopeAuthorityFailure {
            round,
            authority,
            authority_id: authority_id.to_string(),
            reason: reason.to_string(),
        };
        if self.scope_authority_failure_keys.insert(failure.clone()) {
            self.scope_authority_failures.push(failure);
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
                    && !finding.authority_diagnostic
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
                !finding.authority_diagnostic
                    && finding
                        .scoped_news_round
                        .is_some_and(|round| i64::from(round) > since)
                    && finding
                        .convergence_severity
                        .is_some_and(|severity| severity.rank() >= gate)
            })
            .count();

        let recent_authority_failures: Vec<&ScopeAuthorityFailure> = self
            .scope_authority_failures
            .iter()
            .filter(|failure| i64::from(failure.round) > since)
            .collect();
        let recent_authority_ids: BTreeSet<&str> = recent_authority_failures
            .iter()
            .map(|failure| failure.authority_id.as_str())
            .collect();
        let unresolved_unrecent_reports: BTreeSet<&str> = self
            .findings
            .values()
            .flat_map(|finding| finding.unreadable_reports.iter().map(String::as_str))
            .filter(|report_id| !recent_authority_ids.contains(*report_id))
            .collect();
        let authority_failures_recent =
            recent_authority_failures.len() + unresolved_unrecent_reports.len();
        let verdict = if authority_failures_recent == 0
            && open_blocking == 0
            && new_recent == 0
            && self.round >= policy.clean_rounds
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
            authority_failures_recent,
            verdict,
        }
    }
}

impl LedgerProjection {
    /// Fold an already-loaded run log into one run-bound projection.
    pub fn from_events(
        run_id: &str,
        events: &[review_core::RunEvent],
        cas: &Cas,
    ) -> Result<Self, crate::store::StoreError> {
        let mut projection = Self {
            run_id: run_id.to_string(),
            event_count: 0,
            ledger: Ledger {
                round: 1,
                ..Default::default()
            },
        };
        for event in events {
            projection.apply_event(event, cas)?;
        }
        Ok(projection)
    }

    pub fn rebuild(
        store: &crate::EventStore,
        cas: &Cas,
        run_id: &str,
    ) -> Result<Self, crate::store::StoreError> {
        let events = store.replay(run_id)?;
        Self::from_events(run_id, &events, cas)
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn into_ledger(self) -> Ledger {
        self.ledger
    }

    pub fn belongs_to(&self, run_id: &str) -> bool {
        self.run_id == run_id
    }

    /// Number of durable events folded into this projection; also the next required sequence.
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    pub fn apply_event(
        &mut self,
        event: &review_core::RunEvent,
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        if event.run_id != self.run_id {
            return Err(crate::store::StoreError::Conflict(format!(
                "Ledger projection for `{}` cannot apply event from `{}`",
                self.run_id, event.run_id
            )));
        }
        if event.sequence != self.event_count {
            return Err(crate::store::StoreError::Conflict(format!(
                "Ledger projection for `{}` expected sequence {}, got {}",
                self.run_id, self.event_count, event.sequence
            )));
        }
        self.ledger.apply_event(event, cas)?;
        self.event_count += 1;
        Ok(())
    }

    /// Fold the exact durable suffix after this projection's watermark. This tolerates runtime
    /// events appended between preparation and installation while preserving dense ordering and
    /// still applying every event type the Ledger declares as projection input.
    pub fn fast_forward(
        &mut self,
        store: &crate::EventStore,
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let durable_count = store.len(&self.run_id)?;
        if self.event_count > durable_count {
            return Err(crate::store::StoreError::Conflict(format!(
                "Ledger projection for `{}` covers {} events, but the log contains {durable_count}",
                self.run_id, self.event_count
            )));
        }
        for event in store.replay_from(&self.run_id, self.event_count)? {
            self.apply_event(&event, cas)?;
        }
        Ok(())
    }

    pub(crate) fn from_parts(run_id: String, event_count: u64, ledger: Ledger) -> Self {
        Self {
            run_id,
            event_count,
            ledger,
        }
    }

    pub(crate) fn into_parts(self) -> (String, u64, Ledger) {
        (self.run_id, self.event_count, self.ledger)
    }
}

struct ReportProjection {
    artifact_id: Option<String>,
    subject_snapshot_id: Option<String>,
    severity: Severity,
    file: String,
    location: ReportLocation,
    line: Option<i64>,
    title: String,
    body: String,
    fix: Option<String>,
    confidence: Option<f64>,
    rule_id: Option<String>,
    occurrence_key: Option<String>,
    relations: Vec<Relation>,
    unreadable: bool,
    scope_authority_reason: Option<String>,
}

enum ReportLocation {
    ChangeWide,
    Paths(Vec<ProjectedLocation>),
    Unrecorded,
}

struct ProjectedLocation {
    path: String,
    line: Option<i64>,
}

impl ReportLocation {
    fn first_index(&self) -> Option<usize> {
        matches!(self, Self::Paths(paths) if !paths.is_empty()).then_some(0)
    }
}

impl ReportProjection {
    fn identity_location(&self) -> (String, Option<i64>) {
        match &self.location {
            ReportLocation::ChangeWide => ("(change-wide)".to_string(), None),
            ReportLocation::Paths(locations) => locations
                .iter()
                .min_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()))
                .map_or_else(
                    || (self.file.clone(), self.line),
                    |location| (location.path.clone(), location.line),
                ),
            ReportLocation::Unrecorded => (self.file.clone(), self.line),
        }
    }

    fn from_artifact(report_id: &str, value: &Value) -> Result<Self, crate::store::StoreError> {
        let (value, artifact_id, subject_snapshot_id) = if value.get("type").is_some() {
            let envelope: ArtifactEnvelope =
                serde_json::from_value(value.clone()).map_err(|error| {
                    crate::store::StoreError::Artifact(format!(
                        "report {report_id} is not an ArtifactEnvelope: {error}"
                    ))
                })?;
            crate::canonical::validate_envelope(&envelope).map_err(|error| {
                crate::store::StoreError::Artifact(format!("report {report_id}: {error}"))
            })?;
            if envelope.artifact_type != review_core::contract::FINDING_REPORT_V1 {
                return Err(crate::store::StoreError::Artifact(format!(
                    "report {report_id} has type {}, expected {}",
                    envelope.artifact_type,
                    review_core::contract::FINDING_REPORT_V1
                )));
            }
            (
                envelope.payload,
                Some(envelope.artifact_id),
                envelope.subject_snapshot_id,
            )
        } else {
            (value.clone(), None, None)
        };
        if value.get("locations").is_some() {
            let report: review_core::FindingReport = serde::Deserialize::deserialize(&value)
                .map_err(|error| {
                    crate::store::StoreError::Artifact(format!(
                        "report {report_id} is not FindingReport@1: {error}"
                    ))
                })?;
            // Frozen typed artifacts with noncanonical paths remain readable but have unknown
            // Scope: location authority is handled below. Validate every other semantic field
            // by borrow, and keep positive line bounds mandatory for every location.
            if report
                .locations
                .iter()
                .any(|location| location.line == Some(0) || location.end_line == Some(0))
            {
                return Err(crate::store::StoreError::Artifact(format!(
                    "report {report_id} is not FindingReport@1: location lines must be positive"
                )));
            }
            report.validate_claim_fields().map_err(|error| {
                crate::store::StoreError::Artifact(format!(
                    "report {report_id} is not FindingReport@1: {error}"
                ))
            })?;
            let valid_locations: Vec<_> = report
                .locations
                .iter()
                .filter(|location| review_core::is_valid_repo_path(&location.path))
                .map(|location| ProjectedLocation {
                    path: location.path.clone(),
                    line: location.line.map(i64::from),
                })
                .collect();
            let invalid_locations: Vec<_> = report
                .locations
                .iter()
                .filter(|location| !review_core::is_valid_repo_path(&location.path))
                .map(|location| location.path.as_str())
                .collect();
            let scope_authority_reason = (!invalid_locations.is_empty()).then(|| {
                format!(
                    "report {report_id} has noncanonical repository-relative location(s) \
                         {invalid_locations:?}; claim content remains readable with unknown Scope"
                )
            });
            let first = valid_locations.first();
            let file = first
                .map(|location| location.path.clone())
                .or_else(|| {
                    report
                        .locations
                        .first()
                        .map(|location| location.path.clone())
                })
                .unwrap_or_else(|| "(change-wide)".to_string());
            let line = first.and_then(|location| location.line).or_else(|| {
                report
                    .locations
                    .first()
                    .and_then(|location| location.line.map(i64::from))
            });
            let location = if report.locations.is_empty() {
                ReportLocation::ChangeWide
            } else if !invalid_locations.is_empty() {
                ReportLocation::Unrecorded
            } else {
                ReportLocation::Paths(valid_locations)
            };
            return Ok(Self {
                artifact_id,
                subject_snapshot_id,
                severity: report.severity,
                file,
                location,
                line,
                title: report.title,
                body: report.body,
                fix: Some(report.fix),
                confidence: Some(report.confidence),
                rule_id: report.rule_id,
                occurrence_key: report.occurrence_key,
                relations: report.relations,
                unreadable: false,
                scope_authority_reason,
            });
        }
        let required = |field: &str| {
            value[field]
                .as_str()
                .filter(|text| !text.is_empty())
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
        let invalid_location = !file.is_empty()
            && file != review_core::legacy::CHANGE_WIDE_SENTINEL
            && (file.trim().is_empty() || !review_core::is_valid_repo_path(file));
        let scope_authority_reason = invalid_location.then(|| {
            format!(
                "report {report_id} has noncanonical legacy location `{file}`; \
                 claim content remains readable with unknown Scope"
            )
        });
        let location = if file.is_empty() || file == review_core::legacy::CHANGE_WIDE_SENTINEL {
            ReportLocation::ChangeWide
        } else if invalid_location {
            ReportLocation::Unrecorded
        } else {
            ReportLocation::Paths(vec![ProjectedLocation {
                path: file.to_string(),
                line,
            }])
        };
        Ok(Self {
            artifact_id,
            subject_snapshot_id,
            severity,
            file: if file.is_empty() || file == review_core::legacy::CHANGE_WIDE_SENTINEL {
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
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
            unreadable: false,
            scope_authority_reason,
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
            artifact_id: None,
            subject_snapshot_id: None,
            severity,
            file: required_string(payload, "imported FindingReported@1", "file")?,
            location: ReportLocation::Unrecorded,
            line: payload["line"].as_i64(),
            title: required_string(payload, "imported FindingReported@1", "title")?,
            body: required_string(payload, "imported FindingReported@1", "body")?,
            fix: None,
            confidence: payload["confidence"].as_f64(),
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
            unreadable: false,
            scope_authority_reason: None,
        })
    }

    fn unreadable(report_id: &str, reason: &str) -> Self {
        Self {
            artifact_id: None,
            subject_snapshot_id: None,
            severity: Severity::Blocker,
            file: String::new(),
            location: ReportLocation::Unrecorded,
            line: None,
            title: format!("Unreadable Report artifact {report_id}"),
            body: reason.to_string(),
            fix: Some("Restore or migrate the exact content-addressed Report artifact".into()),
            confidence: None,
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
            unreadable: true,
            scope_authority_reason: None,
        }
    }

    fn select_location(&mut self, selected: Option<usize>) {
        let Some(selected) = selected else {
            return;
        };
        let ReportLocation::Paths(locations) = &self.location else {
            return;
        };
        if let Some(location) = locations.get(selected) {
            self.file.clone_from(&location.path);
            self.line = location.line;
        }
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
