//! The Findings Ledger projection, and the convergence policy it feeds.
//!
//! This is a *projection*: it holds no truth of its own and is rebuilt by folding the event log.
//! Delete it and replay; you get the same answer.
//!
//! Nothing is lost in the fold: every report stays attached, and a resolution never overwrites
//! the note that preceded it. `tests/ledger_convergence.rs` pins the transitions and verdicts.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

use review_core::reviewer_result::CHANGE_WIDE_SENTINEL;
use review_core::{
    ArtifactEnvelope, CANONICAL_FINDING_IDENTITY_POLICY, CampaignManifestV1,
    CampaignOpenedPayloadV1, ChangeAttestationV1, DemandSetEntryV1, DemandStatus, DemandV1,
    DemandWaiverV1, EventType, EvidenceReuseAdmissionV1, EvidenceSatisfactionV1, EvidenceV1,
    FindingGroupingAction, FindingGroupingEventPayloadV1, FindingGroupingV1,
    FindingResolutionOutcome, FindingResolutionV1, FixVerificationV1, PolicyTimeV1,
    RecordedArtifactPayloadV1, Relation, ResolutionChallengeV1, RoundStartedPayloadV1, Severity,
    SubjectKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cas::Cas;

pub const EVENT_FINDING_REPORTED: EventType = EventType::FindingReportedV1;
pub const EVENT_FINDING_RESOLVED: EventType = EventType::FindingResolvedV1;
pub const EVENT_GENERATION_ADVANCED: EventType = EventType::GenerationAdvancedV1;
pub const EVENT_FINDINGS_GROUPED: EventType = EventType::FindingsGroupedV1;
pub const EVENT_FINDINGS_UNGROUPED: EventType = EventType::FindingsUngroupedV1;
pub const EVENT_DEMAND_RECORDED: EventType = EventType::DemandRecordedV1;
pub const EVENT_DEMAND_WAIVED: EventType = EventType::DemandWaivedV1;
pub const EVENT_EVIDENCE_ADDED: EventType = EventType::EvidenceAddedV1;
pub const EVENT_EVIDENCE_REUSE_ADMITTED: EventType = EventType::EvidenceReuseAdmittedV1;
pub const EVENT_EVIDENCE_SATISFIED: EventType = EventType::EvidenceSatisfiedV1;
pub const EVENT_CHANGE_ATTESTED: EventType = EventType::ChangeAttestedV1;
pub const EVENT_FIX_VERIFIED: EventType = EventType::FixVerifiedV1;
pub const EVENT_FINDING_RESOLUTION_RECORDED: EventType = EventType::FindingResolutionRecordedV1;
pub const EVENT_FINDING_RESOLUTION_CHALLENGED: EventType = EventType::FindingResolutionChallengedV1;
pub const EVENT_POLICY_TIME_ADVANCED: EventType = EventType::PolicyTimeAdvancedV1;

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

/// A Finding's adjudication status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    PendingVerification,
    Fixed,
    Rejected,
    Wontfix,
    Contested,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "open" => Status::Open,
            "pending-verification" => Status::PendingVerification,
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
            Status::PendingVerification => "pending-verification",
            Status::Fixed => "fixed",
            Status::Rejected => "rejected",
            Status::Wontfix => "wontfix",
            Status::Contested => "contested",
        }
    }

    /// Blocks convergence while at or above the gate.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Status::Open | Status::PendingVerification | Status::Contested
        )
    }

    /// Never auto-reopened: reviewers only ever see open claims, so they rediscover these
    /// forever and an automatic reopen would loop the run to exhaustion.
    fn is_declined(self) -> bool {
        matches!(self, Status::Rejected | Status::Wontfix)
    }
}

/// One report, kept immutable, however many reviewers made the same claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedReport {
    /// CAS ID of the complete envelope or payload referenced by the event.
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

/// One transition, appended rather than overwritten, so a reopen never erases the fix note that
/// came before it.
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
    Attested,
    Challenged,
    Resolved(Status),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The Finding ID, derived from the first selected Report that established the claim.
    pub key: String,
    /// Other durable Finding identities projected into this adjudication view. Base Findings
    /// keep this empty; [`Ledger::finding_views`] fills it without rewriting either member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub status: Status,
    pub severity: Severity,
    pub last_seen_round: u32,
    pub source: String,
    /// The adopted Report's lowest location path: the change-wide sentinel when it has none, empty for an
    /// unreadable-authority placeholder. Stable even when Scope selects another location for
    /// presentation from a multi-location Report.
    pub identity_file: String,
    /// Line paired with `identity_file`, from the same canonical identity location.
    #[serde(default)]
    pub identity_line: Option<i64>,
    pub file: String,
    pub line: Option<i64>,
    pub title: String,
    pub body: String,
    /// The currently adopted remedy.
    pub fix: String,
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
    /// The Round in which this Finding last counted as convergence news under Report Scope.
    /// `None` while every active claim is out of Scope.
    pub scoped_news_round: Option<u32>,
    /// Every report, in arrival order, duplicates included.
    pub reports: Vec<AttachedReport>,
    /// Every transition, in order — including the notes a resolution used to overwrite.
    pub history: Vec<Transition>,
}

impl Finding {
    /// The last non-empty note written.
    pub fn current_note(&self) -> Option<&str> {
        self.history
            .iter()
            .rev()
            .find_map(|t| t.note.as_deref().filter(|n| !n.is_empty()))
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
    finding_identity_policy_unavailable: bool,
    active_groupings: BTreeMap<String, GroupingRelation>,
    grouping_history: Vec<GroupingEvidence>,
    demands: BTreeMap<String, DemandRecord>,
    demand_order: Vec<String>,
    attestations: BTreeMap<String, RecordedAttestation>,
    verifications: BTreeMap<String, RecordedVerification>,
    /// Resolutions retain the Finding ID named by their immutable artifact. Grouping never
    /// rewrites this map; `resolution_authority` records exactly which member claims a
    /// Resolution covered when it was admitted.
    resolutions: BTreeMap<String, RecordedResolution>,
    resolution_authority: BTreeMap<String, String>,
    resolution_history: Vec<ResolutionEvidence>,
    /// Task-only projection evidence. Never serialized as legacy FindingResolution authority.
    task_fixed_subjects: BTreeMap<String, String>,
    policy_time: u64,
    pub round: u32,
}

#[derive(Debug, Clone)]
struct GroupingRelation {
    into: String,
}

#[derive(Debug, Clone)]
struct GroupingEvidence {
    record_id: String,
    artifact_id: String,
}

#[derive(Debug, Clone)]
pub struct DemandRecord {
    pub demand: DemandV1,
    pub record_ids: Vec<String>,
    pub artifact_ids: Vec<String>,
    pub evidence: Vec<RecordedEvidence>,
    pub satisfactions: Vec<RecordedSatisfaction>,
    pub reuse_admissions: Vec<RecordedReuseAdmission>,
    pub waivers: Vec<RecordedWaiver>,
}

#[derive(Debug, Clone)]
pub struct RecordedEvidence {
    pub record_id: String,
    pub artifact_id: String,
    pub evidence: EvidenceV1,
}

#[derive(Debug, Clone)]
pub struct RecordedSatisfaction {
    pub record_id: String,
    pub artifact_id: String,
    pub satisfaction: EvidenceSatisfactionV1,
}

#[derive(Debug, Clone)]
pub struct RecordedReuseAdmission {
    pub record_id: String,
    pub artifact_id: String,
    pub admission: EvidenceReuseAdmissionV1,
}

#[derive(Debug, Clone)]
pub struct RecordedWaiver {
    pub record_id: String,
    pub artifact_id: String,
    pub waiver: DemandWaiverV1,
}

#[derive(Debug, Clone)]
pub struct RecordedAttestation {
    pub record_id: String,
    pub artifact_id: String,
    pub attestation: ChangeAttestationV1,
}

#[derive(Debug, Clone)]
pub struct RecordedVerification {
    pub record_id: String,
    pub artifact_id: String,
    pub verification: FixVerificationV1,
}

#[derive(Debug, Clone)]
pub struct RecordedResolution {
    pub record_id: String,
    pub artifact_id: String,
    pub resolution: FindingResolutionV1,
}

#[derive(Debug, Clone)]
struct ResolutionEvidence {
    record_id: String,
    artifact_id: String,
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

#[derive(Debug, Clone, Copy)]
pub struct ConvergencePolicy {
    pub clean_rounds: u32,
    pub max_rounds: u32,
    pub gate: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Convergence {
    pub round: u32,
    pub open_blocking: usize,
    pub new_recent: usize,
    pub open_required_demands: usize,
    pub authority_failures_recent: usize,
    pub verdict: Verdict,
}

impl Ledger {
    /// Projection initialization for the Task Review domain. It grants no execution or Store
    /// authority. The Task host separately verifies sealed Snapshot and plan provenance.
    pub fn for_task_subject(
        cas: &Cas,
        subject_id: &str,
        round: u32,
    ) -> Result<Self, crate::StoreError> {
        let mut ledger = Self {
            finding_identity_policy: CANONICAL_FINDING_IDENTITY_POLICY.into(),
            ..Self::default()
        };
        ledger.bind_task_subject(cas, subject_id, round)?;
        Ok(ledger)
    }

    /// Keep original claims and resolution history while an admitted Task continuation changes
    /// the current Subject. This is a projection operation, not a discovery Round closeout.
    pub fn bind_task_subject(
        &mut self,
        cas: &Cas,
        subject_id: &str,
        round: u32,
    ) -> Result<(), crate::StoreError> {
        if round == 0
            || round < self.round
            || self.finding_identity_policy() != Some(CANONICAL_FINDING_IDENTITY_POLICY)
        {
            return Err(crate::StoreError::Conflict("Task review Subject requires canonical identity and a nondecreasing positive Round".into()));
        }
        let resolved = crate::resolve_subject_scope(cas, subject_id)?;
        for (key, fixed_subject) in &self.task_fixed_subjects {
            if fixed_subject != subject_id {
                let finding = self.findings.get_mut(key).ok_or_else(|| {
                    crate::StoreError::Conflict("Task fix projection lost its Finding".into())
                })?;
                if finding.status == Status::Fixed {
                    finding.status = Status::Open;
                    finding.history.push(Transition {
                        round,
                        kind: TransitionKind::Reopened,
                        note: Some("Task fix verification belongs to an earlier Subject".into()),
                    });
                }
            }
        }
        for id in std::iter::once(&resolved.subject.head_snapshot_id)
            .chain(resolved.subject.base_snapshot_id.iter())
        {
            cas.verify(id)
                .map_err(|e| crate::StoreError::Artifact(e.to_string()))?;
        }
        let scope = match resolved.changed_paths {
            Some(paths) => Arc::new(SubjectScope::Diff(paths)),
            None => Arc::new(SubjectScope::WholeTree),
        };
        self.subject_scope_cache.insert(
            subject_id.into(),
            CachedSubjectScope {
                scope: Arc::clone(&scope),
                change_set_id: resolved.subject.change_set_id,
                head_snapshot_id: resolved.subject.head_snapshot_id.clone(),
            },
        );
        self.active_scope = Some(ActiveScope {
            round,
            subject_id: subject_id.into(),
            head_snapshot_id: resolved.subject.head_snapshot_id,
            subject: scope,
        });
        self.round = round;
        Ok(())
    }

    /// Apply already-admitted Task evidence to an in-memory projection. The Task domain must
    /// first recompute the assessment, prove current checks and the independent selected Attempt.
    /// This method additionally checks exact receipt bytes and current views before any mutation.
    /// It emits no Campaign event and creates no Campaign Resolution or discovery Round.
    pub fn project_task_fixes(
        &mut self,
        cas: &Cas,
        assessment: &review_core::task::review::RepairAssessmentV1,
    ) -> Result<(), crate::StoreError> {
        use review_core::task::repair::{TASK_FIX_RECEIPT_V1, TaskFixReceiptV1};
        use review_core::task::review::VerificationOutcomeV1;
        let conflict = |message: &str| crate::StoreError::Conflict(message.into());
        assessment.validate().map_err(|e| conflict(&e))?;
        if self.active_subject_id() != Some(&assessment.current_subject_id)
            || self.active_head_snapshot_id() != Some(&assessment.current_snapshot_id)
        {
            return Err(conflict(
                "Task fix assessment belongs to another active Subject",
            ));
        }
        let mut changes = Vec::new();
        for (finding_id, claim) in &assessment.claims {
            let value = cas
                .get_json(&claim.receipt_id)
                .map_err(|e| crate::StoreError::Artifact(e.to_string()))?;
            let artifact: review_core::ArtifactEnvelope =
                serde_json::from_value(value).map_err(|e| conflict(&e.to_string()))?;
            let receipt: TaskFixReceiptV1 =
                serde_json::from_value(artifact.payload).map_err(|e| conflict(&e.to_string()))?;
            receipt.validate().map_err(|e| conflict(&e))?;
            if artifact.artifact_type != TASK_FIX_RECEIPT_V1
                || artifact.subject_snapshot_id.as_ref() != Some(&assessment.current_snapshot_id)
                || receipt.finding_id != *finding_id
                || receipt.subject_id != assessment.current_subject_id
                || receipt.continuation_id != assessment.continuation_id
                || receipt.decision.expected_view_id != claim.expected_view_id
                || receipt.decision.attestation_id != claim.attestation_id
                || receipt.decision.outcome != claim.outcome
                || self.finding_view_id(finding_id).as_ref() != Some(&claim.expected_view_id)
            {
                return Err(conflict(
                    "Task fix receipt changed its original Finding or current view",
                ));
            }
            if claim.outcome == VerificationOutcomeV1::Positive {
                for key in self.finding_member_keys(finding_id)? {
                    changes.push((key, claim.receipt_id.clone()));
                }
            }
        }
        for (key, receipt_id) in changes {
            let finding = self
                .findings
                .get_mut(&key)
                .expect("validated Finding member");
            finding.status = Status::Fixed;
            finding.history.push(Transition {
                round: self.round,
                kind: TransitionKind::Resolved(Status::Fixed),
                note: Some(format!("Verified on current Task Subject by {receipt_id}")),
            });
            self.task_fixed_subjects
                .insert(key, assessment.current_subject_id.clone());
        }
        Ok(())
    }

    /// The Campaign's recorded Finding identity policy; `None` before `CampaignOpened@1` or when
    /// its manifest is unreadable.
    pub fn finding_identity_policy(&self) -> Option<&str> {
        (!self.finding_identity_policy_unavailable && !self.finding_identity_policy.is_empty())
            .then_some(self.finding_identity_policy.as_str())
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
            EVENT_FINDINGS_GROUPED | EVENT_FINDINGS_UNGROUPED => {
                self.apply_grouping(event_type, payload, artifact_refs, cas)?
            }
            EVENT_DEMAND_RECORDED
            | EVENT_DEMAND_WAIVED
            | EVENT_EVIDENCE_ADDED
            | EVENT_EVIDENCE_REUSE_ADMITTED
            | EVENT_EVIDENCE_SATISFIED => {
                self.apply_demand_evidence(event_type, payload, artifact_refs, cas)?
            }
            EVENT_CHANGE_ATTESTED
            | EVENT_FIX_VERIFIED
            | EVENT_FINDING_RESOLUTION_RECORDED
            | EVENT_FINDING_RESOLUTION_CHALLENGED
            | EVENT_POLICY_TIME_ADVANCED => {
                self.apply_resolution_authority(event_type, payload, artifact_refs, cas)?
            }
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
        let (report_id, mut report) = match artifact_refs {
            [report_id] => {
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
                (report_id.clone(), report)
            }
            _ => {
                return Err(malformed(
                    "FindingReported@1",
                    "expected exactly one report artifact",
                ));
            }
        };
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
        let resolution_challenge = (scope != Some(ReportScope::Out))
            .then(|| self.resolution_challenge_for_report(&key, severity))
            .flatten();

        // Authority failure evidence must not replace readable claim content. If this key has
        // readable history, retain only the diagnostic attachment. A first unreadable Report gets
        // an actionable placeholder that the first readable Report replaces unconditionally.
        if unreadable && self.findings.contains_key(&key) {
            if let Some(existing) = self.findings.get_mut(&key) {
                existing
                    .unreadable_reports
                    .insert(attached.report_id.clone());
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
                    aliases: Vec::new(),
                    status: Status::Open,
                    severity,
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
        let kind = if resolution_challenge.is_some() {
            if higher {
                TransitionKind::Escalated
            } else {
                TransitionKind::Challenged
            }
        } else if existing.status.is_declined() {
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
                adopt(existing, &report, &source);
            }
            TransitionKind::Escalated | TransitionKind::AdoptedWhileDeclined => {
                adopt(existing, &report, &source);
                if resolution_challenge.is_some() {
                    existing.status = Status::Contested;
                }
            }
            TransitionKind::Challenged => {
                existing.status = Status::Contested;
            }
            _ => {}
        }

        let note = match kind {
            TransitionKind::Reopened => Some(format!(
                "reopened: re-reported by {source} in round {round}"
            )),
            TransitionKind::Escalated => Some(if let Some(challenge) = resolution_challenge {
                format!(
                    "resolution challenge {challenge:?}: re-reported as {} by {source} in round {round}",
                    severity_name(severity)
                )
            } else {
                format!(
                    "escalated: re-reported as {} by {source} in round {round}",
                    severity_name(severity)
                )
            }),
            TransitionKind::Challenged => resolution_challenge.map(|challenge| {
                format!(
                    "resolution challenge {challenge:?}: re-reported by {source} in round {round}"
                )
            }),
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
        let manifest = cas
            .get_json(&opened.campaign_manifest_id)
            .map_err(|error| error.to_string())
            .and_then(|value| {
                serde_json::from_value::<CampaignManifestV1>(value)
                    .map_err(|error| error.to_string())
            })
            .and_then(|manifest| manifest.validate().map(|()| manifest));
        match manifest {
            Ok(manifest) => {
                self.finding_identity_policy = manifest.finding_identity_policy;
                self.finding_identity_policy_unavailable = false;
            }
            Err(reason) => {
                self.finding_identity_policy.clear();
                self.finding_identity_policy_unavailable = true;
                self.record_authority_failure(
                    self.round,
                    ScopeAuthorityKind::Subject,
                    &opened.campaign_manifest_id,
                    &format!("CampaignManifest authority unavailable: {reason}"),
                );
            }
        }
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

    fn apply_grouping(
        &mut self,
        event_type: EventType,
        payload: &Value,
        artifact_refs: &[String],
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let event: FindingGroupingEventPayloadV1 = serde_json::from_value(payload.clone())
            .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        event
            .validate()
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        if artifact_refs != [event.grouping_artifact_id.as_str()] {
            return Err(malformed(
                event_type.as_str(),
                "grouping artifact ID disagrees with its sole reference",
            ));
        }
        let envelope: ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&event.grouping_artifact_id)
                .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?,
        )
        .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        crate::canonical::validate_envelope(&envelope)
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        if envelope.artifact_type != review_core::contract::FINDING_GROUPING_V1 {
            return Err(malformed(
                event_type.as_str(),
                "grouping artifact carries the wrong type",
            ));
        }
        let grouping: FindingGroupingV1 = serde_json::from_value(envelope.payload)
            .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        grouping
            .validate()
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        let expected_action = if event_type == EVENT_FINDINGS_GROUPED {
            FindingGroupingAction::Group
        } else {
            FindingGroupingAction::Ungroup
        };
        if grouping.from != event.from
            || grouping.into != event.into
            || grouping.action != expected_action
        {
            return Err(malformed(
                event_type.as_str(),
                "grouping event disagrees with its immutable artifact",
            ));
        }

        match grouping.action {
            FindingGroupingAction::Group => self.validate_group(&grouping.from, &grouping.into)?,
            FindingGroupingAction::Ungroup => {
                self.validate_ungroup(&grouping.from, &grouping.into)?
            }
        }
        self.grouping_history.push(GroupingEvidence {
            record_id: event.grouping_artifact_id,
            artifact_id: envelope.artifact_id.clone(),
        });
        match grouping.action {
            FindingGroupingAction::Group => {
                self.active_groupings.insert(
                    grouping.from,
                    GroupingRelation {
                        into: grouping.into,
                    },
                );
            }
            FindingGroupingAction::Ungroup => {
                self.active_groupings.remove(&grouping.from);
            }
        }
        Ok(())
    }

    fn apply_demand_evidence(
        &mut self,
        event_type: EventType,
        payload: &Value,
        artifact_refs: &[String],
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let recorded: RecordedArtifactPayloadV1 = serde_json::from_value(payload.clone())
            .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        recorded
            .validate()
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        if artifact_refs.len() != 1 || artifact_refs[0] != recorded.artifact_id {
            return Err(malformed(
                event_type.as_str(),
                "recorded artifact disagrees with its sole reference",
            ));
        }
        let envelope: ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&recorded.artifact_id)
                .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?,
        )
        .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        crate::canonical::validate_envelope(&envelope)
            .map_err(|error| malformed(event_type.as_str(), &error))?;

        match event_type {
            EVENT_DEMAND_RECORDED => {
                if envelope.artifact_type != review_core::contract::DEMAND_V1 {
                    return Err(malformed(event_type.as_str(), "artifact is not Demand@1"));
                }
                let demand: DemandV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                demand
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                match self.demands.get_mut(&demand.demand_id) {
                    Some(existing) => {
                        if existing.demand.claim != demand.claim
                            || existing.demand.why != demand.why
                            || existing.demand.suggested_method != demand.suggested_method
                            || existing.demand.source != demand.source
                            || existing.demand.requirement != demand.requirement
                        {
                            return Err(malformed(
                                event_type.as_str(),
                                "stable Demand identity changes immutable content",
                            ));
                        }
                        existing.record_ids.push(recorded.artifact_id);
                        existing.artifact_ids.push(envelope.artifact_id);
                    }
                    None => {
                        self.demand_order.push(demand.demand_id.clone());
                        self.demands.insert(
                            demand.demand_id.clone(),
                            DemandRecord {
                                demand,
                                record_ids: vec![recorded.artifact_id],
                                artifact_ids: vec![envelope.artifact_id],
                                evidence: Vec::new(),
                                satisfactions: Vec::new(),
                                reuse_admissions: Vec::new(),
                                waivers: Vec::new(),
                            },
                        );
                    }
                }
            }
            EVENT_EVIDENCE_ADDED => {
                if envelope.artifact_type != review_core::contract::EVIDENCE_V1 {
                    return Err(malformed(event_type.as_str(), "artifact is not Evidence@1"));
                }
                let evidence: EvidenceV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                evidence
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                let demand = self.demands.get_mut(&evidence.demand_id).ok_or_else(|| {
                    malformed(event_type.as_str(), "Evidence names an unknown Demand")
                })?;
                demand.evidence.push(RecordedEvidence {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                    evidence,
                });
            }
            EVENT_EVIDENCE_SATISFIED => {
                if envelope.artifact_type != review_core::contract::EVIDENCE_SATISFACTION_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not EvidenceSatisfaction@1",
                    ));
                }
                let satisfaction: EvidenceSatisfactionV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                satisfaction
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                let demand = self
                    .demands
                    .get_mut(&satisfaction.demand_id)
                    .ok_or_else(|| {
                        malformed(event_type.as_str(), "satisfaction names an unknown Demand")
                    })?;
                if !demand
                    .evidence
                    .iter()
                    .any(|evidence| evidence.artifact_id == satisfaction.evidence_id)
                {
                    return Err(malformed(
                        event_type.as_str(),
                        "satisfaction names Evidence outside its Demand",
                    ));
                }
                demand.satisfactions.push(RecordedSatisfaction {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                    satisfaction,
                });
            }
            EVENT_EVIDENCE_REUSE_ADMITTED => {
                if envelope.artifact_type != review_core::contract::EVIDENCE_REUSE_ADMISSION_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not EvidenceReuseAdmission@1",
                    ));
                }
                let admission: EvidenceReuseAdmissionV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                admission
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                let demand = self.demands.get_mut(&admission.demand_id).ok_or_else(|| {
                    malformed(event_type.as_str(), "reuse names an unknown Demand")
                })?;
                if !demand.satisfactions.iter().any(|satisfaction| {
                    satisfaction.artifact_id == admission.satisfaction_id
                        && satisfaction.satisfaction.subject_id == admission.subject_id
                }) {
                    return Err(malformed(
                        event_type.as_str(),
                        "reuse does not name a satisfaction for its Demand and Subject",
                    ));
                }
                demand.reuse_admissions.push(RecordedReuseAdmission {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                    admission,
                });
            }
            EVENT_DEMAND_WAIVED => {
                if envelope.artifact_type != review_core::contract::DEMAND_WAIVER_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not DemandWaiver@1",
                    ));
                }
                let waiver: DemandWaiverV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                waiver
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                let demand = self.demands.get_mut(&waiver.demand_id).ok_or_else(|| {
                    malformed(event_type.as_str(), "waiver names an unknown Demand")
                })?;
                demand.waivers.push(RecordedWaiver {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                    waiver,
                });
            }
            _ => unreachable!("demand evidence event was matched by caller"),
        }
        Ok(())
    }

    fn apply_resolution_authority(
        &mut self,
        event_type: EventType,
        payload: &Value,
        artifact_refs: &[String],
        cas: &Cas,
    ) -> Result<(), crate::store::StoreError> {
        let recorded: RecordedArtifactPayloadV1 = serde_json::from_value(payload.clone())
            .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        recorded
            .validate()
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        if artifact_refs != [recorded.artifact_id.as_str()] {
            return Err(malformed(
                event_type.as_str(),
                "recorded artifact disagrees with its sole reference",
            ));
        }
        let envelope: ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&recorded.artifact_id)
                .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?,
        )
        .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
        crate::canonical::validate_envelope(&envelope)
            .map_err(|error| malformed(event_type.as_str(), &error))?;
        let active_subject = self
            .active_subject_id()
            .ok_or_else(|| malformed(event_type.as_str(), "Campaign has no active Subject"))?
            .to_string();

        match event_type {
            EVENT_CHANGE_ATTESTED => {
                if envelope.artifact_type != review_core::contract::CHANGE_ATTESTATION_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not ChangeAttestation@1",
                    ));
                }
                let attestation: ChangeAttestationV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                attestation
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                self.validate_expected_finding_view(
                    &attestation.finding_id,
                    &attestation.expected_finding_view_id,
                    event_type,
                )?;
                if attestation.subject_id != active_subject
                    || self.active_change_set_id() != attestation.change_set_id.as_deref()
                    || envelope.subject_snapshot_id.as_deref() != self.active_head_snapshot_id()
                {
                    return Err(malformed(
                        event_type.as_str(),
                        "Attestation does not cover the active Subject and Change Set",
                    ));
                }
                let keys = self.finding_member_keys(&attestation.finding_id)?;
                for key in keys {
                    let finding = self.findings.get_mut(&key).expect("member exists");
                    finding.status = Status::PendingVerification;
                    finding.history.push(Transition {
                        round: self.round,
                        kind: TransitionKind::Attested,
                        note: Some(attestation.reason.clone()),
                    });
                }
                self.attestations.insert(
                    envelope.artifact_id.clone(),
                    RecordedAttestation {
                        record_id: recorded.artifact_id,
                        artifact_id: envelope.artifact_id,
                        attestation,
                    },
                );
            }
            EVENT_FIX_VERIFIED => {
                if envelope.artifact_type != review_core::contract::FIX_VERIFICATION_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not FixVerification@1",
                    ));
                }
                let verification: FixVerificationV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                verification
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                self.validate_expected_finding_view(
                    &verification.finding_id,
                    &verification.expected_finding_view_id,
                    event_type,
                )?;
                let attestation = self
                    .attestations
                    .get(&verification.attestation_id)
                    .ok_or_else(|| {
                        malformed(
                            event_type.as_str(),
                            "verification names unknown Attestation",
                        )
                    })?;
                if verification.subject_id != active_subject
                    || attestation.attestation.finding_id != verification.finding_id
                    || attestation.attestation.subject_id != verification.subject_id
                {
                    return Err(malformed(
                        event_type.as_str(),
                        "verification does not cover its Attestation and active Subject",
                    ));
                }
                self.verifications.insert(
                    envelope.artifact_id.clone(),
                    RecordedVerification {
                        record_id: recorded.artifact_id,
                        artifact_id: envelope.artifact_id,
                        verification,
                    },
                );
            }
            EVENT_FINDING_RESOLUTION_RECORDED => {
                if envelope.artifact_type != review_core::contract::FINDING_RESOLUTION_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not FindingResolution@1",
                    ));
                }
                let resolution: FindingResolutionV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                resolution
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                self.validate_expected_finding_view(
                    &resolution.finding_id,
                    &resolution.expected_finding_view_id,
                    event_type,
                )?;
                if resolution.subject_id != active_subject {
                    return Err(malformed(
                        event_type.as_str(),
                        "Resolution does not cover the active Subject",
                    ));
                }
                if self.group_root(&resolution.finding_id).is_none() {
                    return Err(malformed(
                        event_type.as_str(),
                        "Resolution names an unknown Finding",
                    ));
                }
                let status = match resolution.outcome {
                    FindingResolutionOutcome::Fixed => {
                        let verification = self
                            .verifications
                            .get(resolution.verification_id.as_deref().expect("validated"))
                            .ok_or_else(|| {
                                malformed(
                                    event_type.as_str(),
                                    "fixed Resolution names unknown FixVerification",
                                )
                            })?;
                        if !verification.verification.positive
                            || verification.verification.finding_id != resolution.finding_id
                            || verification.verification.subject_id != resolution.subject_id
                        {
                            return Err(malformed(
                                event_type.as_str(),
                                "fixed Resolution lacks positive current-Subject verification",
                            ));
                        }
                        Status::Fixed
                    }
                    FindingResolutionOutcome::Rejected => Status::Rejected,
                    FindingResolutionOutcome::WontfixTracked => {
                        let ceiling = resolution.max_accepted_severity.expect("validated");
                        let current = self
                            .finding_view(&resolution.finding_id)
                            .and_then(|finding| finding.convergence_severity);
                        if current.is_some_and(|severity| severity.rank() > ceiling.rank()) {
                            return Err(malformed(
                                event_type.as_str(),
                                "tracked-wontfix severity ceiling is below the current Finding severity",
                            ));
                        }
                        if resolution
                            .expires_at_policy_time
                            .is_some_and(|expiry| expiry <= self.policy_time)
                        {
                            return Err(malformed(
                                event_type.as_str(),
                                "tracked-wontfix expiry is not later than current persisted policy time",
                            ));
                        }
                        Status::Wontfix
                    }
                };
                let keys = self.finding_member_keys(&resolution.finding_id)?;
                let resolution_key = resolution.finding_id.clone();
                for key in &keys {
                    let finding = self.findings.get_mut(key).expect("member exists");
                    finding.status = status;
                    finding.history.push(Transition {
                        round: self.round,
                        kind: TransitionKind::Resolved(status),
                        note: Some(resolution.reason.clone()),
                    });
                    self.resolution_authority
                        .insert(key.clone(), resolution_key.clone());
                }
                self.resolution_history.push(ResolutionEvidence {
                    record_id: recorded.artifact_id.clone(),
                    artifact_id: envelope.artifact_id.clone(),
                });
                self.resolutions.insert(
                    resolution_key,
                    RecordedResolution {
                        record_id: recorded.artifact_id,
                        artifact_id: envelope.artifact_id,
                        resolution,
                    },
                );
            }
            EVENT_FINDING_RESOLUTION_CHALLENGED => {
                if envelope.artifact_type != review_core::contract::RESOLUTION_CHALLENGE_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not ResolutionChallenge@1",
                    ));
                }
                let challenge: ResolutionChallengeV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                challenge
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                let resolution = self.resolution(&challenge.finding_id).ok_or_else(|| {
                    malformed(event_type.as_str(), "challenge names no active Resolution")
                })?;
                if resolution.artifact_id != challenge.resolution_id
                    || challenge.subject_id != active_subject
                {
                    return Err(malformed(
                        event_type.as_str(),
                        "challenge does not name the active Resolution and Subject",
                    ));
                }
                let keys = self.finding_member_keys(&challenge.finding_id)?;
                for key in keys {
                    let finding = self.findings.get_mut(&key).expect("member exists");
                    finding.status = Status::Contested;
                    finding.history.push(Transition {
                        round: self.round,
                        kind: TransitionKind::Challenged,
                        note: Some(format!("{:?}: {}", challenge.kind, challenge.reason)),
                    });
                }
                self.resolution_history.push(ResolutionEvidence {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                });
            }
            EVENT_POLICY_TIME_ADVANCED => {
                if envelope.artifact_type != review_core::contract::POLICY_TIME_V1 {
                    return Err(malformed(
                        event_type.as_str(),
                        "artifact is not PolicyTime@1",
                    ));
                }
                let policy_time: PolicyTimeV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| malformed(event_type.as_str(), &error.to_string()))?;
                policy_time
                    .validate()
                    .map_err(|error| malformed(event_type.as_str(), &error))?;
                if policy_time.tick <= self.policy_time {
                    return Err(malformed(
                        event_type.as_str(),
                        "policy time must advance monotonically",
                    ));
                }
                self.policy_time = policy_time.tick;
                self.resolution_history.push(ResolutionEvidence {
                    record_id: recorded.artifact_id,
                    artifact_id: envelope.artifact_id,
                });
            }
            _ => unreachable!("resolution authority event was matched by caller"),
        }
        Ok(())
    }

    fn validate_expected_finding_view(
        &self,
        finding_id: &str,
        expected: &str,
        event_type: EventType,
    ) -> Result<(), crate::store::StoreError> {
        let actual = self
            .finding_view_id(finding_id)
            .ok_or_else(|| malformed(event_type.as_str(), "artifact names an unknown Finding"))?;
        if actual != expected {
            return Err(malformed(
                event_type.as_str(),
                "expected Finding view is stale",
            ));
        }
        Ok(())
    }

    fn finding_member_keys(
        &self,
        finding_id: &str,
    ) -> Result<Vec<String>, crate::store::StoreError> {
        let root = self.group_root(finding_id).ok_or_else(|| {
            crate::store::StoreError::Conflict(format!("unknown Finding `{finding_id}`"))
        })?;
        Ok(self
            .group_members(root)
            .into_iter()
            .map(|finding| finding.key.clone())
            .collect())
    }

    pub fn validate_group(&self, from: &str, into: &str) -> Result<(), crate::store::StoreError> {
        if from == into || !self.findings.contains_key(from) || !self.findings.contains_key(into) {
            return Err(crate::store::StoreError::Conflict(
                "grouping requires two distinct existing Findings".into(),
            ));
        }
        if self.active_groupings.contains_key(from) {
            return Err(crate::store::StoreError::Conflict(format!(
                "Finding `{from}` is already grouped into another Finding"
            )));
        }
        if self.group_root(into) != Some(into) {
            return Err(crate::store::StoreError::Conflict(format!(
                "group target `{into}` is an alias; name its visible root instead"
            )));
        }
        if self.group_has_unexpired_tracked_resolution(from)
            || self.group_has_unexpired_tracked_resolution(into)
        {
            return Err(crate::store::StoreError::Conflict(
                "cannot group a Finding carrying an unexpired tracked-wontfix Resolution; challenge the Resolution first"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn validate_ungroup(&self, from: &str, into: &str) -> Result<(), crate::store::StoreError> {
        if self
            .active_groupings
            .get(from)
            .is_none_or(|relation| relation.into != into)
        {
            return Err(crate::store::StoreError::Conflict(format!(
                "Findings `{from}` and `{into}` do not have that active grouping"
            )));
        }
        let root = self.group_root(from).ok_or_else(|| {
            crate::store::StoreError::Conflict(format!("unknown Finding `{from}`"))
        })?;
        if self.resolution(root).is_some()
            && self
                .finding_view(root)
                .is_some_and(|finding| !finding.status.is_active())
        {
            return Err(crate::store::StoreError::Conflict(
                "cannot ungroup a terminally resolved Finding group; challenge the Resolution first"
                    .into(),
            ));
        }
        Ok(())
    }

    fn group_root<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        self.findings.get(key)?;
        let mut root = key;
        for _ in 0..=self.active_groupings.len() {
            let Some(relation) = self.active_groupings.get(root) else {
                return Some(root);
            };
            root = &relation.into;
        }
        None
    }

    fn group_members<'a>(&'a self, root: &str) -> Vec<&'a Finding> {
        self.order
            .iter()
            .filter_map(|key| {
                (self.group_root(key) == Some(root))
                    .then(|| self.findings.get(key))
                    .flatten()
            })
            .collect()
    }

    fn group_has_unexpired_tracked_resolution(&self, finding_id: &str) -> bool {
        let Some(root) = self.group_root(finding_id) else {
            return false;
        };
        self.group_members(root).into_iter().any(|finding| {
            finding.status == Status::Wontfix
                && self.resolution(&finding.key).is_some_and(|resolution| {
                    resolution.resolution.outcome == FindingResolutionOutcome::WontfixTracked
                        && resolution
                            .resolution
                            .expires_at_policy_time
                            .is_some_and(|expiry| expiry > self.policy_time)
                })
        })
    }

    fn finding_view_for_root(&self, root: &str) -> Option<Finding> {
        let members = self.group_members(root);
        let root_finding = self.findings.get(root)?;
        let mut view = root_finding.clone();
        view.aliases = members
            .iter()
            .filter(|finding| finding.key != root)
            .map(|finding| finding.key.clone())
            .collect();
        if view.aliases.is_empty() {
            return Some(view);
        }
        view.status = if members
            .iter()
            .any(|finding| finding.status == Status::Contested)
        {
            Status::Contested
        } else if members.iter().any(|finding| finding.status == Status::Open) {
            Status::Open
        } else if members
            .iter()
            .any(|finding| finding.status == Status::PendingVerification)
        {
            Status::PendingVerification
        } else {
            root_finding.status
        };
        if let Some(severity) = members
            .iter()
            .map(|finding| finding.severity)
            .max_by_key(|s| s.rank())
        {
            view.severity = severity;
        }
        view.last_seen_round = members
            .iter()
            .map(|finding| finding.last_seen_round)
            .max()
            .unwrap_or(view.last_seen_round);
        view.scoped_news_round = members
            .iter()
            .filter_map(|finding| finding.scoped_news_round)
            .max();
        view.convergence_scope = members
            .iter()
            .map(|finding| finding.convergence_scope)
            .reduce(combine_scope)
            .flatten();
        view.convergence_severity = members
            .iter()
            .filter_map(|finding| finding.convergence_severity)
            .max_by_key(|severity| severity.rank());
        view.authority_diagnostic = members.iter().any(|finding| finding.authority_diagnostic);
        view.unreadable_reports = members
            .iter()
            .flat_map(|finding| finding.unreadable_reports.iter().cloned())
            .collect();
        view.reports = members
            .iter()
            .flat_map(|finding| finding.reports.iter().cloned())
            .collect();
        view.history = members
            .iter()
            .flat_map(|finding| finding.history.iter().cloned())
            .collect();
        view.history.sort_by_key(|transition| transition.round);
        Some(view)
    }

    /// Findings in first-reported order.
    pub fn findings(&self) -> Vec<&Finding> {
        self.order
            .iter()
            .filter_map(|key| self.findings.get(key))
            .collect()
    }

    /// Operator and reducer views after applying every active reversible Grouping. Base Finding
    /// records remain independently addressable through [`Self::get`].
    pub fn finding_views(&self) -> Vec<Finding> {
        let mut seen = BTreeSet::new();
        self.order
            .iter()
            .filter_map(|key| self.group_root(key))
            .filter(|root| seen.insert((*root).to_string()))
            .filter_map(|root| self.finding_view_for_root(root))
            .collect()
    }

    /// Resolve either a visible Finding ID or one of its preserved aliases.
    pub fn finding_view(&self, key: &str) -> Option<Finding> {
        self.group_root(key)
            .and_then(|root| self.finding_view_for_root(root))
    }

    pub fn finding_view_id(&self, key: &str) -> Option<String> {
        let view = self.finding_view(key)?;
        let value = serde_json::to_value(view).ok()?;
        crate::content_id(&value).ok()
    }

    pub fn grouping_relation_ids(&self) -> Vec<String> {
        self.grouping_history
            .iter()
            .map(|evidence| evidence.artifact_id.clone())
            .collect()
    }

    pub fn grouping_input_artifact_ids(&self) -> Vec<String> {
        self.grouping_history
            .iter()
            .map(|evidence| evidence.record_id.clone())
            .collect()
    }

    pub fn demand(&self, demand_id: &str) -> Option<&DemandRecord> {
        self.demands.get(demand_id)
    }

    pub fn active_subject_id(&self) -> Option<&str> {
        self.active_scope
            .as_ref()
            .map(|scope| scope.subject_id.as_str())
    }

    pub fn active_head_snapshot_id(&self) -> Option<&str> {
        self.active_scope
            .as_ref()
            .map(|scope| scope.head_snapshot_id.as_str())
            .filter(|id| !id.is_empty())
    }

    pub fn active_change_set_id(&self) -> Option<&str> {
        let subject_id = self.active_subject_id()?;
        self.subject_scope_cache
            .get(subject_id)
            .and_then(|scope| scope.change_set_id.as_deref())
    }

    pub fn attestation(&self, artifact_id: &str) -> Option<&RecordedAttestation> {
        self.attestations.get(artifact_id)
    }

    pub fn verification(&self, artifact_id: &str) -> Option<&RecordedVerification> {
        self.verifications.get(artifact_id)
    }

    pub fn resolution(&self, finding_id: &str) -> Option<&RecordedResolution> {
        self.resolution_authority
            .get(finding_id)
            .and_then(|key| self.resolutions.get(key))
    }

    pub fn resolution_challenge_for_report(
        &self,
        finding_id: &str,
        severity: Severity,
    ) -> Option<review_core::ResolutionChallengeKind> {
        let finding = self.finding_view(finding_id)?;
        if !finding.status.is_declined() {
            return None;
        }
        let resolution = self.resolution(finding_id)?;
        if Some(resolution.resolution.subject_id.as_str()) != self.active_subject_id() {
            return Some(review_core::ResolutionChallengeKind::OutsideScope);
        }
        match resolution.resolution.outcome {
            FindingResolutionOutcome::WontfixTracked
                if severity.rank()
                    > resolution
                        .resolution
                        .max_accepted_severity
                        .expect("validated tracked-wontfix")
                        .rank() =>
            {
                Some(review_core::ResolutionChallengeKind::HigherSeverity)
            }
            FindingResolutionOutcome::Rejected if severity.rank() > finding.severity.rank() => {
                Some(review_core::ResolutionChallengeKind::HigherSeverity)
            }
            _ => None,
        }
    }

    pub fn policy_time(&self) -> u64 {
        self.policy_time
    }

    pub fn expiring_resolutions(&self, tick: u64) -> Vec<&RecordedResolution> {
        self.resolutions
            .values()
            .filter(|resolution| {
                resolution
                    .resolution
                    .expires_at_policy_time
                    .is_some_and(|expiry| expiry <= tick)
                    && self
                        .findings
                        .get(&resolution.resolution.finding_id)
                        .is_some_and(|finding| finding.status == Status::Wontfix)
            })
            .collect()
    }

    pub fn resolution_artifact_ids(&self) -> Vec<String> {
        self.resolution_history
            .iter()
            .map(|evidence| evidence.artifact_id.clone())
            .collect()
    }

    pub fn resolution_input_artifact_ids(&self) -> Vec<String> {
        self.resolution_history
            .iter()
            .map(|evidence| evidence.record_id.clone())
            .collect()
    }

    pub fn demand_views(&self) -> Vec<DemandSetEntryV1> {
        let active_subject = self.active_subject_id();
        self.demand_order
            .iter()
            .filter_map(|id| self.demands.get(id))
            .map(|record| {
                let current_waivers: Vec<_> = record.waivers.iter().collect();
                let current_satisfactions: Vec<_> = record
                    .satisfactions
                    .iter()
                    .filter(|satisfaction| {
                        Some(satisfaction.satisfaction.subject_id.as_str()) == active_subject
                            || record.reuse_admissions.iter().any(|admission| {
                                admission.admission.satisfaction_id == satisfaction.artifact_id
                            })
                    })
                    .collect();
                let status = if !current_waivers.is_empty() {
                    DemandStatus::Waived
                } else if !current_satisfactions.is_empty() {
                    DemandStatus::Satisfied
                } else if !record.satisfactions.is_empty() || !record.waivers.is_empty() {
                    DemandStatus::Stale
                } else {
                    DemandStatus::Open
                };
                DemandSetEntryV1 {
                    demand_id: record.demand.demand_id.clone(),
                    claim: record.demand.claim.clone(),
                    why: record.demand.why.clone(),
                    suggested_method: record.demand.suggested_method.clone(),
                    source: record.demand.source.clone(),
                    requirement: record.demand.requirement,
                    status,
                    subject_id: active_subject
                        .unwrap_or(record.demand.subject_id.as_str())
                        .to_string(),
                    evidence_ids: record
                        .evidence
                        .iter()
                        .map(|evidence| evidence.artifact_id.clone())
                        .collect(),
                    satisfaction_ids: current_satisfactions
                        .iter()
                        .map(|satisfaction| satisfaction.artifact_id.clone())
                        .chain(
                            record
                                .reuse_admissions
                                .iter()
                                .map(|admission| admission.artifact_id.clone()),
                        )
                        .collect(),
                    waiver_ids: current_waivers
                        .iter()
                        .map(|waiver| waiver.artifact_id.clone())
                        .collect(),
                }
            })
            .collect()
    }

    pub fn demand_reduction_artifact_ids(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let selected = self
            .demands
            .values()
            .flat_map(|demand| demand.artifact_ids.iter().cloned())
            .collect();
        let satisfactions = self
            .demands
            .values()
            .flat_map(|demand| {
                demand
                    .satisfactions
                    .iter()
                    .map(|item| item.artifact_id.clone())
                    .chain(
                        demand
                            .reuse_admissions
                            .iter()
                            .map(|item| item.artifact_id.clone()),
                    )
            })
            .collect();
        let waivers = self
            .demands
            .values()
            .flat_map(|demand| demand.waivers.iter().map(|item| item.artifact_id.clone()))
            .collect();
        (selected, satisfactions, waivers)
    }

    pub fn demand_reduction_input_ids(&self) -> Vec<String> {
        self.demands
            .values()
            .flat_map(|demand| {
                demand
                    .record_ids
                    .iter()
                    .cloned()
                    .chain(demand.evidence.iter().map(|item| item.record_id.clone()))
                    .chain(
                        demand
                            .satisfactions
                            .iter()
                            .map(|item| item.record_id.clone()),
                    )
                    .chain(
                        demand
                            .reuse_admissions
                            .iter()
                            .map(|item| item.record_id.clone()),
                    )
                    .chain(demand.waivers.iter().map(|item| item.record_id.clone()))
            })
            .collect()
    }

    pub fn required_open_demands(&self) -> usize {
        self.demand_views()
            .iter()
            .filter(|demand| {
                demand.requirement == review_core::DemandRequirement::Required
                    && matches!(demand.status, DemandStatus::Open | DemandStatus::Stale)
            })
            .count()
    }

    pub fn get(&self, key: &str) -> Option<&Finding> {
        self.findings.get(key)
    }

    /// Scope failures are diagnostics, not replay failures: affected reports remain unknown.
    pub fn scope_authority_failures(&self) -> &[ScopeAuthorityFailure] {
        &self.scope_authority_failures
    }

    /// The convergence decision.
    ///
    /// `new_recent` counts by news round and **ignores status** — so a finding fixed in the
    /// current round still blocks. That is deliberate: it is what forces a fix to survive
    /// another review before the run may call itself converged.
    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        let gate = policy.gate.rank();
        let views = self.finding_views();
        let open_blocking = views
            .iter()
            .filter(|finding| {
                finding.status.is_active()
                    && !finding.authority_diagnostic
                    && finding
                        .convergence_severity
                        .is_some_and(|severity| severity.rank() >= gate)
            })
            .count();
        let since = self.round as i64 - policy.clean_rounds as i64;
        let new_recent = views
            .iter()
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
        let authority_failures_recent = recent_authority_failures.len()
            + unresolved_unrecent_reports.len()
            + usize::from(self.finding_identity_policy_unavailable);
        let open_required_demands = self.required_open_demands();
        let verdict = if authority_failures_recent == 0
            && open_blocking == 0
            && new_recent == 0
            && open_required_demands == 0
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
            open_required_demands,
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
    fix: String,
    confidence: Option<f64>,
    rule_id: Option<String>,
    occurrence_key: Option<String>,
    relations: Vec<Relation>,
    unreadable: bool,
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
            ReportLocation::ChangeWide => (CHANGE_WIDE_SENTINEL.to_string(), None),
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

    /// Project an enveloped `FindingReport@1`. Anything else, including a report whose
    /// locations are not canonical repository paths, is unreadable Report authority.
    fn from_artifact(report_id: &str, value: &Value) -> Result<Self, crate::store::StoreError> {
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
        let not_a_report = |error: &dyn std::fmt::Display| {
            crate::store::StoreError::Artifact(format!(
                "report {report_id} is not FindingReport@1: {error}"
            ))
        };
        let report: review_core::FindingReport =
            serde_json::from_value(envelope.payload).map_err(|error| not_a_report(&error))?;
        report.validate().map_err(|error| not_a_report(&error))?;
        let locations: Vec<_> = report
            .locations
            .iter()
            .map(|location| ProjectedLocation {
                path: location.path.clone(),
                line: location.line.map(i64::from),
            })
            .collect();
        let (file, line, location) = match locations.first() {
            Some(first) => (
                first.path.clone(),
                first.line,
                ReportLocation::Paths(locations),
            ),
            None => (
                CHANGE_WIDE_SENTINEL.to_string(),
                None,
                ReportLocation::ChangeWide,
            ),
        };
        Ok(Self {
            artifact_id: Some(envelope.artifact_id),
            subject_snapshot_id: envelope.subject_snapshot_id,
            severity: report.severity,
            file,
            location,
            line,
            title: report.title,
            body: report.body,
            fix: report.fix,
            confidence: Some(report.confidence),
            rule_id: report.rule_id,
            occurrence_key: report.occurrence_key,
            relations: report.relations,
            unreadable: false,
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
            fix: "Restore or migrate the exact content-addressed Report artifact".into(),
            confidence: None,
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
            unreadable: true,
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

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "blocker",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}
