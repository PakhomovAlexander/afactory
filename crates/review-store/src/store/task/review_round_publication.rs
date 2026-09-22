//! Prepare Review Round data outside a transaction, then publish it under both original
//! Task and Review prefixes. This permit never starts an Attempt or changes the Task budget.
use super::*;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::task::review_handoff::{TaskReviewHandoffEvidenceV1, TaskReviewHandoffV1};
use review_core::{RoundStartedPayloadV1, RunFailureReasonV3, RunReportPayloadV6, RunVerdictV3};

#[derive(Debug, Clone)]
pub struct TaskReviewRoundPublication {
    task_id: String,
    revision_id: String,
    plan_id: String,
    writer: String,
    epoch: u64,
    task_sequence: u64,
    review_sequence: u64,
    predecessor: LegacyReviewRoundV1,
    next_round: u32,
    next_epoch: u32,
    prior_finding_set_id: String,
    prior_demand_set_id: String,
    integrated: Option<(String, review_core::IntegrationCommittedPayloadV1)>,
    evidence: TaskReviewHandoffEvidenceV1,
}

/// Compiler data only. The exact event suffix has passed Store's current predecessor guard;
/// currentness and successor feasibility are checked again at actual publication.
#[derive(Debug, Clone)]
pub struct TaskReviewRoundPreview {
    permit: TaskReviewRoundPublication,
    history: Vec<RunEvent>,
    round_index: usize,
}
impl TaskReviewRoundPreview {
    pub fn permit(&self) -> &TaskReviewRoundPublication {
        &self.permit
    }
    pub fn history(&self) -> &[RunEvent] {
        &self.history
    }
    pub fn round_event(&self) -> &RunEvent {
        &self.history[self.round_index]
    }
    pub fn prepare_handoff(
        &self,
        cas: &Cas,
        successor_plan_id: &str,
    ) -> Result<TaskReviewHandoffV1, StoreError> {
        let next_plan: ExecutionPlanV1 = payload(cas, successor_plan_id, task::EXECUTION_PLAN_V1)?;
        let previous = revision(cas, &self.permit.revision_id)?;
        let next = revision(cas, &next_plan.task_revision_id)?;
        let round_id = |task: &TaskRevisionV1| -> Result<String, StoreError> {
            let input = task
                .inputs
                .values()
                .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
                .ok_or_else(|| conflict("Review successor has no captured Round input"))?;
            let [id] = input.artifact_ids.as_slice() else {
                return Err(conflict("Review successor has ambiguous Round input"));
            };
            Ok(id.clone())
        };
        Ok(TaskReviewHandoffV1 {
            task_id: self.permit.task_id.clone(),
            predecessor_revision_id: self.permit.revision_id.clone(),
            predecessor_plan_id: self.permit.plan_id.clone(),
            successor_revision_id: next_plan.task_revision_id,
            successor_plan_id: successor_plan_id.into(),
            predecessor_round_id: round_id(&previous)?,
            successor_round_id: round_id(&next)?,
            evidence: self.permit.evidence.clone(),
        })
    }
}

pub struct TaskReviewRoundSuccessor<'a> {
    pub handoff_id: &'a str,
    /// The prospective successor compiler with the original predecessor's pure domain
    /// continuation validator. This authority never admits or approves the new plan here.
    pub authority: &'a dyn TaskAuthority,
}
impl TaskReviewRoundPublication {
    pub fn campaign_id(&self) -> &str {
        &self.predecessor.campaign_id
    }
    pub fn predecessor_round_event_id(&self) -> &str {
        &self.predecessor.round_event_id
    }
    pub fn next_review_sequence(&self) -> u64 {
        self.review_sequence
    }
    pub fn predecessor(&self) -> &LegacyReviewRoundV1 {
        &self.predecessor
    }
    pub fn next_round(&self) -> u32 {
        self.next_round
    }
    pub fn next_epoch(&self) -> u32 {
        self.next_epoch
    }
    pub fn is_restart(&self) -> bool {
        self.next_epoch > 1
    }
    pub fn prior_demand_set_id(&self) -> &str {
        &self.prior_demand_set_id
    }
    pub fn restart_prior_finding_set_id(&self) -> Option<&str> {
        self.is_restart().then_some(&self.prior_finding_set_id)
    }
    pub fn integrated(&self) -> Option<(&str, &review_core::IntegrationCommittedPayloadV1)> {
        self.integrated
            .as_ref()
            .map(|(id, value)| (id.as_str(), value))
    }
    pub fn event_id_at(&self, offset: usize) -> Result<String, StoreError> {
        let sequence = self
            .review_sequence
            .checked_add(u64::try_from(offset).map_err(|_| conflict("Review sequence overflow"))?)
            .filter(|n| *n <= review_core::json::SAFE_INTEGER_MAX as u64)
            .ok_or_else(|| conflict("Review sequence overflow"))?;
        Ok(super::super::derive_event_id(
            self.campaign_id(),
            sequence as i64,
        ))
    }
}
impl EventStore {
    pub fn prepare_task_review_round_publication(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<TaskReviewRoundPublication, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        if !state.admitted || state.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict(
                "Review Round publication requires its admitted running Task",
            ));
        }
        self.current_task_plan(cas, &state, authority, time)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Review Round publication has no common execution"))?;
        if !execution.pending_attempts().is_empty() || execution.budget.breached() {
            return Err(conflict(
                "Review Round publication refuses pending Attempts or original Task resource breach",
            ));
        }
        review_round::ReviewRoundFence::capture(cas, &state.revision)?
            .ok_or_else(|| conflict("Task has no captured Review Round"))?;
        let input = state
            .revision
            .inputs
            .values()
            .find(|v| v.artifact_type == LEGACY_REVIEW_ROUND_V1)
            .expect("checked");
        let predecessor: LegacyReviewRoundV1 =
            payload(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
        let events = self.replay(&predecessor.campaign_id)?;
        let event = events
            .iter()
            .rev()
            .find(|e| e.event_type == EventType::RoundStartedV1)
            .ok_or_else(|| conflict("Review Campaign has no Round"))?;
        let started: RoundStartedPayloadV1 = serde_json::from_value(event.payload.clone())?;
        if event.event_id != predecessor.round_event_id
            || started.round != predecessor.round
            || started.epoch != predecessor.epoch
            || started.subject_id != predecessor.subject_id
            || started.campaign_manifest_id != predecessor.campaign_manifest_id
        {
            return Err(conflict("Task lost its exact current Review Round"));
        }
        let terminal = events
            .iter()
            .rev()
            .filter(|e| e.causation_id.as_deref() == Some(&predecessor.round_event_id))
            .find_map(|e| match review_core::run_report_closes_round(e) {
                Ok(Some(true)) => Some(Ok(e)),
                Err(e) => Some(Err(StoreError::Json(e))),
                _ => None,
            })
            .transpose()?;
        let mut integrated = None;
        let evidence;
        let (next_round, next_epoch, prior_demand_set_id) = if let Some(terminal) = terminal {
            let report: RunReportPayloadV6 = serde_json::from_value(terminal.payload.clone())?;
            report.validate().map_err(conflict)?;
            if report.task_accounting.task_id != state.task_id
                || report.task_accounting.task_revision_id != state.revision_id
                || Some(&report.task_accounting.plan_id) != state.plan_id.as_ref()
            {
                return Err(conflict(
                    "Review conclusion belongs to another original Task plan",
                ));
            }
            let mut can_advance = report.verdict
                == (RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::NotConverged,
                });
            if let Some(phase) = execution.active_review_integration() {
                if !phase.finished() {
                    return Err(conflict("Review Integration is unfinished"));
                }
                if let Some(id) = phase.integration_committed_event_id() {
                    let event = events
                        .iter()
                        .find(|e| {
                            e.event_id == id && e.event_type == EventType::IntegrationCommittedV1
                        })
                        .ok_or_else(|| conflict("Review Integration lost its exact commit"))?;
                    integrated = Some((
                        id.to_string(),
                        serde_json::from_value(event.payload.clone())?,
                    ));
                    can_advance = true;
                }
            } else if report.verdict == (RunVerdictV3::Pass {})
                && execution.graph.review_integration.is_some()
            {
                return Err(conflict(
                    "Passing Review has unresolved captured Integration",
                ));
            }
            if !can_advance {
                return Err(conflict("Closed Review has no authorized successor Round"));
            }
            let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
                cas.get_json(&predecessor.campaign_manifest_id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?,
            )?;
            manifest.validate().map_err(conflict)?;
            let next = predecessor
                .round
                .checked_add(1)
                .filter(|r| *r <= manifest.convergence.max_rounds)
                .ok_or_else(|| conflict("Review successor exceeds original Campaign Round cap"))?;
            let (_, demands) = review_integration::canonical_task_integration_views(cas, &report)?;
            evidence = if let Some((id, _)) = &integrated {
                TaskReviewHandoffEvidenceV1::IntegratedRound {
                    report_event_id: terminal.event_id.clone(),
                    phase_id: execution
                        .active_review_integration()
                        .expect("checked phase")
                        .phase_id()
                        .into(),
                    integration_committed_event_id: id.clone(),
                }
            } else {
                TaskReviewHandoffEvidenceV1::ClosedRound {
                    report_event_id: terminal.event_id.clone(),
                }
            };
            (next, 1, demands)
        } else {
            if execution.active_review_integration().is_some() {
                return Err(conflict(
                    "Open Review cannot carry post-Round Integration authority",
                ));
            }
            if events.iter().any(|later| {
                later.sequence > event.sequence
                    && (later.event_type == EventType::FindingReportedV1
                        || (later.event_type == EventType::FindingResolvedV1
                            && later.causation_id.as_deref() == Some(&predecessor.round_event_id)))
            }) {
                return Err(conflict(
                    "Cannot restart an incomplete Review after it published finding state",
                ));
            }
            let epoch = predecessor
                .epoch
                .checked_add(1)
                .ok_or_else(|| conflict("Review epoch overflow"))?;
            evidence = TaskReviewHandoffEvidenceV1::SupersededInput {
                superseded_event_id: super::super::derive_event_id(
                    &predecessor.campaign_id,
                    i64::try_from(events.len())
                        .map_err(|_| conflict("Review sequence overflow"))?
                        .checked_add(1)
                        .ok_or_else(|| conflict("Review sequence overflow"))?,
                ),
            };
            (
                predecessor.round,
                epoch,
                started.prior_demand_set_id.clone(),
            )
        };
        Ok(TaskReviewRoundPublication {
            task_id: state.task_id,
            revision_id: state.revision_id,
            plan_id: state.plan_id.expect("admitted"),
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            task_sequence: state.next_sequence,
            review_sequence: events.len() as u64,
            predecessor,
            next_round,
            next_epoch,
            prior_finding_set_id: started.prior_finding_set_id,
            prior_demand_set_id,
            integrated,
            evidence,
        })
    }

    pub fn preview_task_review_round(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        permit: &TaskReviewRoundPublication,
        events: &[NewEvent],
        authority: &dyn TaskAuthority,
    ) -> Result<TaskReviewRoundPreview, StoreError> {
        let current = self.prepare_task_review_round_publication(cas, lease, authority)?;
        if current.task_id != permit.task_id
            || current.revision_id != permit.revision_id
            || current.plan_id != permit.plan_id
            || current.writer != permit.writer
            || current.epoch != permit.epoch
            || current.task_sequence != permit.task_sequence
            || current.review_sequence != permit.review_sequence
            || current.predecessor != permit.predecessor
            || current.next_round != permit.next_round
            || current.next_epoch != permit.next_epoch
            || current.prior_finding_set_id != permit.prior_finding_set_id
            || current.prior_demand_set_id != permit.prior_demand_set_id
            || current.integrated != permit.integrated
            || current.evidence != permit.evidence
        {
            return Err(conflict(
                "Review preparation lost its exact Task or Review prefix",
            ));
        }
        validate_events(cas, permit, events)?;
        let mut history = self.replay(permit.campaign_id())?;
        if history.len() as u64 != permit.review_sequence {
            return Err(conflict("Review preview lost its exact Campaign prefix"));
        }
        let round_offset = events
            .iter()
            .position(|event| event.event_type == EventType::RoundStartedV1)
            .expect("validated sequence");
        let round_index = history.len() + round_offset;
        for (offset, event) in events.iter().enumerate() {
            history.push(RunEvent {
                event_id: permit.event_id_at(offset)?,
                run_id: permit.campaign_id().into(),
                sequence: permit.review_sequence + offset as u64,
                event_type: event.event_type,
                occurred_at: event.occurred_at.clone(),
                node_id: event.node_id.clone(),
                attempt_id: event.attempt_id.clone(),
                causation_id: event.causation_id.clone(),
                correlation_id: event.correlation_id.clone(),
                artifact_refs: event.artifact_refs.clone(),
                payload: event.payload.clone(),
            });
        }
        Ok(TaskReviewRoundPreview {
            permit: permit.clone(),
            history,
            round_index,
        })
    }

    pub fn publish_task_review_round(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        permit: &TaskReviewRoundPublication,
        events: &[NewEvent],
        authority: &dyn TaskAuthority,
        successor: &TaskReviewRoundSuccessor<'_>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let preview = self.preview_task_review_round(cas, lease, permit, events, authority)?;
        self.validate_review_round_successor(cas, lease, &preview, successor)?;
        // Trusted callbacks can observe changing external approval or another writer. Never
        // carry their pre-callback currentness check into the transaction without rechecking.
        self.preview_task_review_round(cas, lease, permit, events, authority)?;
        let state = self
            .task_projection(cas, lease.task_id())?
            .expect("checked");
        let approval_until = state
            .decisions
            .get(&permit.plan_id)
            .map_or(u64::MAX, |d| d.valid_until);
        let valid_until = state
            .lease_until
            .min(approval_until)
            .min(state.revision.limits.deadline_unix_ms);
        let prepared = self.prepare_event_artifacts(cas, events)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task_current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence)+1,0) FROM events WHERE run_id=?1",
            [task_run_id(&permit.task_id)?],
            |r| r.get(0),
        )?;
        let review_current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence)+1,0) FROM events WHERE run_id=?1",
            [permit.campaign_id()],
            |r| r.get(0),
        )?;
        if task_current != permit.task_sequence as i64
            || review_current != permit.review_sequence as i64
            || now()? >= valid_until
        {
            return Err(conflict(
                "Review publication lost its Task/Review prefix or current authority",
            ));
        }
        let cut = usize::from(
            events
                .first()
                .is_some_and(|e| e.event_type == EventType::SourceCapturedV1),
        );
        let mut appended = Vec::new();
        for chunk in [&events[..cut], &events[cut..]] {
            if chunk.is_empty() {
                continue;
            }
            let at = review_current + appended.len() as i64;
            super::super::validate_campaign_transition(
                &tx,
                cas,
                permit.campaign_id(),
                chunk,
                at,
                &prepared,
            )?;
            appended.extend(super::super::insert_events(
                &tx,
                permit.campaign_id(),
                chunk,
                at,
            )?);
        }
        tx.commit()?;
        Ok(appended)
    }

    fn validate_review_round_successor(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        preview: &TaskReviewRoundPreview,
        successor: &TaskReviewRoundSuccessor<'_>,
    ) -> Result<(), StoreError> {
        let handoff = review_handoff::read_task_review_handoff(cas, successor.handoff_id)?;
        if preview.prepare_handoff(cas, &handoff.successor_plan_id)? != handoff {
            return Err(conflict(
                "Prospective Review handoff changed its original Task, plan or evidence",
            ));
        }
        let (previous, next, _, round) = review_handoff::validate_revisions(cas, &handoff)?;
        let started: RoundStartedPayloadV1 =
            serde_json::from_value(preview.round_event().payload.clone())?;
        if round.round_event_id != preview.round_event().event_id
            || round.subject_id != started.subject_id
            || round.round != started.round
            || round.epoch != started.epoch
            || round.campaign_id != preview.permit.campaign_id()
        {
            return Err(conflict(
                "Prospective successor plan changed its exact prepared Round",
            ));
        }
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Review Task disappeared"))?;
        if state.next_sequence != preview.permit.task_sequence {
            return Err(conflict("Review successor validation lost its Task prefix"));
        }
        let previous_plan = plan(cas, &handoff.predecessor_plan_id, &state)?;
        let mut candidate = state.clone();
        candidate.revision = next.clone();
        candidate.revision_id = handoff.successor_revision_id.clone();
        candidate.plan_id = Some(handoff.successor_plan_id.clone());
        let next_plan = self.authorized_plan(
            cas,
            &candidate,
            &handoff.successor_plan_id,
            successor.authority,
        )?;
        successor
            .authority
            .validate_review_continuation(
                cas,
                &previous,
                &next,
                &previous_plan,
                &next_plan,
                &handoff,
            )
            .map_err(conflict)?;
        // Exercise the same exact budget installation and immutable Task invariants as
        // handoff replay, on a clone before the canonical successor Round can become visible.
        let mut candidate = state;
        candidate.apply_review_handoff(cas, successor.handoff_id, now()?)?;
        Ok(())
    }
}
fn validate_events(
    cas: &Cas,
    permit: &TaskReviewRoundPublication,
    events: &[NewEvent],
) -> Result<(), StoreError> {
    let source = usize::from(permit.integrated.is_none());
    let superseded = usize::from(permit.is_restart());
    let offset = source + superseded;
    if events.len() != offset + 1 + usize::from(!permit.is_restart())
        || (source == 1 && events[0].event_type != EventType::SourceCapturedV1)
        || (superseded == 1 && events[source].event_type != EventType::RoundInputSupersededV1)
        || events[offset].event_type != EventType::RoundStartedV1
        || events
            .get(offset + 1)
            .is_some_and(|e| e.event_type != EventType::GenerationAdvancedV1)
    {
        return Err(conflict(
            "Review publication contains another effect or an incomplete Round sequence",
        ));
    }
    let started: RoundStartedPayloadV1 = serde_json::from_value(events[offset].payload.clone())?;
    started.validate().map_err(conflict)?;
    if started.round != permit.next_round
        || started.epoch != permit.next_epoch
        || started.campaign_manifest_id != permit.predecessor.campaign_manifest_id
        || started.prior_demand_set_id != permit.prior_demand_set_id
        || (permit.is_restart() && started.prior_finding_set_id != permit.prior_finding_set_id)
    {
        return Err(conflict(
            "Review successor changed its exact Round step, Campaign or selected DemandSet",
        ));
    }
    validate_source(cas, permit, events, offset, &started)?;
    if let Some((id, committed)) = &permit.integrated {
        if events[offset].causation_id.as_deref() != Some(id)
            || started.subject_id != committed.derived_subject_id
        {
            return Err(conflict(
                "Integrated successor changed its exact checked head",
            ));
        }
    }
    if permit.is_restart() {
        let value: review_core::RoundInputSupersededPayloadV1 =
            serde_json::from_value(events[source].payload.clone())?;
        if value.round != permit.predecessor.round
            || value.old_epoch != permit.predecessor.epoch
            || value.new_epoch != permit.next_epoch
            || value.campaign_manifest_id != permit.predecessor.campaign_manifest_id
            || value.old_subject_id != permit.predecessor.subject_id
            || value.replacement_subject_id != started.subject_id
            || events[source].causation_id.as_deref() != Some(permit.predecessor_round_event_id())
            || events[offset].causation_id.as_deref() != Some(permit.predecessor_round_event_id())
        {
            return Err(conflict(
                "Review restart changed its exact predecessor epoch",
            ));
        }
    }
    if let Some(advanced) = events.get(offset + 1) {
        if advanced.causation_id.as_deref() != Some(permit.event_id_at(offset)?.as_str())
            || advanced.payload != json!({"round":permit.next_round})
        {
            return Err(conflict("Ledger advance changed the exact successor Round"));
        }
    }
    // Supersession retains the exact original prior artifact, including its original Subject
    // header. Only a new numeric Round builds a fresh bounded raw legacy view.
    if permit.is_restart() {
        return Ok(());
    }
    // A prior FindingSet slot is the bounded raw legacy view, not the canonical set envelope.
    let prior = cas
        .get_json(&started.prior_finding_set_id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    if prior.get("subject_id").and_then(|v| v.as_str()) != Some(&started.subject_id)
        || prior.get("round").and_then(|v| v.as_u64()) != Some(u64::from(started.round))
        || !prior.get("prior_findings").is_some_and(|v| v.is_array())
    {
        return Err(conflict(
            "Review successor lacks its exact raw prior Findings view",
        ));
    }
    Ok(())
}

fn validate_source(
    cas: &Cas,
    permit: &TaskReviewRoundPublication,
    events: &[NewEvent],
    offset: usize,
    started: &RoundStartedPayloadV1,
) -> Result<(), StoreError> {
    let read = |id: &str| {
        cas.get_json(id)
            .map_err(|e| StoreError::Artifact(e.to_string()))
    };
    let manifest: review_core::CampaignManifestV1 = read(&started.campaign_manifest_id)
        .and_then(|v| serde_json::from_value(v).map_err(StoreError::Json))?;
    manifest.validate().map_err(conflict)?;
    let subject: review_core::SubjectV1 = serde_json::from_value(read(&started.subject_id)?)?;
    subject.validate().map_err(conflict)?;
    if subject.kind != manifest.subject_kind
        || subject.base_snapshot_id != manifest.base_snapshot_id
    {
        return Err(conflict(
            "Review successor changed its captured Subject kind or Base",
        ));
    }
    let snapshot_value = read(&subject.head_snapshot_id)?;
    let snapshot: review_core::SourceSnapshot = serde_json::from_value(snapshot_value.clone())?;
    let authority: review_core::SourceSnapshot =
        serde_json::from_value(read(&manifest.authority_snapshot_id)?)?;
    if snapshot.repository_id != authority.repository_id {
        return Err(conflict(
            "Review successor belongs to another Source repository",
        ));
    }
    let manifest_id = snapshot
        .artifact_manifest
        .as_deref()
        .ok_or_else(|| conflict("Review successor Source has no exact Manifest"))?;
    read(manifest_id)?;
    let mut refs = vec![
        manifest.authority_snapshot_id.as_str(),
        started.campaign_manifest_id.as_str(),
        subject.head_snapshot_id.as_str(),
        started.subject_id.as_str(),
        started.prior_finding_set_id.as_str(),
        started.prior_demand_set_id.as_str(),
    ];
    refs.extend(subject.base_snapshot_id.as_deref());
    refs.extend(subject.change_set_id.as_deref());
    if refs
        .iter()
        .any(|id| !events[offset].artifact_refs.iter().any(|r| r == id))
    {
        return Err(conflict(
            "Review successor Round omits its exact captured input references",
        ));
    }
    if let Some(id) = &subject.change_set_id {
        let change: review_core::ChangeSetV1 = serde_json::from_value(read(id)?)?;
        change.validate().map_err(conflict)?;
        if change.head_snapshot_id != subject.head_snapshot_id
            || Some(&change.base_snapshot_id) != subject.base_snapshot_id.as_ref()
        {
            return Err(conflict(
                "Review successor Change Set changed its exact Base or head",
            ));
        }
    }
    if permit.integrated.is_none() {
        if snapshot.is_derived()
            || events[0].payload != snapshot_value
            || events[0].correlation_id.as_deref() != Some(&subject.head_snapshot_id)
            || [
                manifest.authority_snapshot_id.as_str(),
                started.campaign_manifest_id.as_str(),
                subject.head_snapshot_id.as_str(),
                manifest_id,
            ]
            .iter()
            .any(|id| !events[0].artifact_refs.iter().any(|r| r == id))
        {
            return Err(conflict(
                "SourceCaptured does not publish the exact successor Source",
            ));
        }
    } else if !snapshot.is_derived()
        || !events[offset]
            .artifact_refs
            .iter()
            .any(|id| id == manifest_id)
    {
        return Err(conflict(
            "Integrated successor lost its exact derived Source Manifest",
        ));
    }
    Ok(())
}
