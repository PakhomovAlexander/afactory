//! In-process canonical-domain fixture. Native Task orchestration and durable replay are tested
//! in af-runtime::task_review and af-store::store::task. This fixture grants no Store authority.
#![allow(dead_code)]
use af_contracts::*;
use af_store::ledger::*;
use af_store::{CanonicalReduction, CanonicalStage, Cas, Ledger, NewEvent, StoreError};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default)]
pub struct LedgerEvents {
    subjects: BTreeMap<String, String>,
    events: BTreeMap<String, Vec<RunEvent>>,
}
impl LedgerEvents {
    pub fn open(_: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self::default())
    }
    pub fn len(&self, run: &str) -> Result<u64, StoreError> {
        Ok(self.events.get(run).map_or(0, |v| v.len() as u64))
    }
    pub fn replay(&self, run: &str) -> Result<Vec<RunEvent>, StoreError> {
        Ok(self.events.get(run).cloned().unwrap_or_default())
    }
    pub fn append(
        &mut self,
        run: &str,
        cas: &Cas,
        event: NewEvent,
    ) -> Result<RunEvent, StoreError> {
        Ok(self.append_batch(run, cas, &[event])?.remove(0))
    }
    pub fn append_batch(
        &mut self,
        run: &str,
        cas: &Cas,
        events: &[NewEvent],
    ) -> Result<Vec<RunEvent>, StoreError> {
        for e in events {
            af_contracts::event::validate_event_payload(e.event_type, &e.payload)
                .map_err(StoreError::Conflict)?;
            for id in &e.artifact_refs {
                cas.verify(id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?;
            }
        }
        let log = self.events.entry(run.into()).or_default();
        let mut out = Vec::new();
        for e in events {
            let event = RunEvent {
                event_id: format!("{:026x}", log.len()),
                run_id: run.into(),
                sequence: log.len() as u64,
                event_type: e.event_type,
                occurred_at: e.occurred_at.clone(),
                node_id: e.node_id.clone(),
                attempt_id: e.attempt_id.clone(),
                causation_id: e.causation_id.clone(),
                correlation_id: e.correlation_id.clone(),
                artifact_refs: e.artifact_refs.clone(),
                payload: e.payload.clone(),
            };
            log.push(event.clone());
            out.push(event);
        }
        Ok(out)
    }
}
pub struct NativeProjection {
    run: String,
    events: u64,
    ledger: Ledger,
}
impl NativeProjection {
    pub fn rebuild(store: &LedgerEvents, cas: &Cas, run: &str) -> Result<Self, StoreError> {
        let subject = store
            .subjects
            .get(run)
            .ok_or_else(|| StoreError::Conflict("fixture has no native Subject".into()))?;
        let mut ledger = Ledger::for_task_subject(cas, subject, 1)?;
        let events = store.replay(run)?;
        for event in &events {
            ledger.apply_event(event, cas)?;
        }
        Ok(Self {
            run: run.into(),
            events: events.len() as u64,
            ledger,
        })
    }
    pub fn into_ledger(self) -> Ledger {
        self.ledger
    }
    fn into_parts(self) -> (String, u64, Ledger) {
        (self.run, self.events, self.ledger)
    }
    fn from_parts(run: String, events: u64, ledger: Ledger) -> Self {
        Self {
            run,
            events,
            ledger,
        }
    }
    fn belongs_to(&self, run: &str) -> bool {
        self.run == run
    }
    fn fast_forward(&mut self, store: &LedgerEvents, cas: &Cas) -> Result<(), StoreError> {
        for event in store.replay(&self.run)?.iter().skip(self.events as usize) {
            self.ledger.apply_event(event, cas)?;
            self.events += 1;
        }
        Ok(())
    }
}
pub struct Authority {
    pub subject: String,
    pub head: String,
    pub round_event_id: String,
}
pub fn opened_round(store: &mut LedgerEvents, cas: &Cas, run: &str) -> Authority {
    let head = cas.put(b"native canonical head").unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    Ledger::for_task_subject(cas, &subject, 1).unwrap();
    store.subjects.insert(run.into(), subject.clone());
    Authority {
        subject,
        head,
        round_event_id: "fixture-round".into(),
    }
}
pub fn default_policy() -> af_store::ConvergencePolicy {
    af_store::ConvergencePolicy {
        clean_rounds: 1,
        max_rounds: 3,
        gate: Severity::Major,
    }
}
/// Drives a run: ingest stage outputs, record resolutions, advance generations.
pub struct Ingest<'a> {
    store: &'a mut LedgerEvents,
    cas: &'a Cas,
    run_id: String,
    event_count: u64,
    ledger: Ledger,
    round_event_id: Option<String>,
}

impl<'a> Ingest<'a> {
    pub fn new(
        store: &'a mut LedgerEvents,
        cas: &'a Cas,
        run_id: impl Into<String>,
    ) -> Result<Self, StoreError> {
        let run_id = run_id.into();
        let projection = NativeProjection::rebuild(store, cas, &run_id)?;
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
        store: &'a mut LedgerEvents,
        cas: &'a Cas,
        run_id: impl Into<String>,
        mut projection: NativeProjection,
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

    pub fn into_projection(self) -> NativeProjection {
        NativeProjection::from_parts(self.run_id, self.event_count, self.ledger)
    }

    /// Bind reducer and generation events to the active durable Round epoch.
    pub fn under_round(mut self, round_event_id: impl Into<String>) -> Self {
        self.round_event_id = Some(round_event_id.into());
        self
    }

    /// Reduction and generation events are Round-runtime facts, so the caller must have bound
    /// the durable Round epoch first. The operator adjudication verbs run without one.
    fn bind_round(&self, event: NewEvent) -> Result<NewEvent, StoreError> {
        let round = self.round_event_id.as_deref().ok_or_else(|| {
            StoreError::Conflict("Ledger reduction needs a bound Round epoch".into())
        })?;
        Ok(event.caused_by(round))
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Advance the Ledger's Round counter under the bound Round.
    pub fn advance(&mut self) -> Result<u32, StoreError> {
        let round = self.ledger.round + 1;
        let event = self.store.append(
            &self.run_id,
            self.cas,
            self.bind_round(NewEvent::new(
                EVENT_GENERATION_ADVANCED,
                json!({ "round": round }),
            ))?,
        )?;
        self.validate_watermark(&event)?;
        self.ledger.apply_event(&event, self.cas)?;
        self.event_count += 1;
        Ok(round)
    }

    /// Bridge selected flat results into typed, provenance-carrying Report artifacts and reduce
    /// them with the canonical path-independent identity policy. Every finding must satisfy
    /// `FindingReport@1` ([`ReviewerReport::into_report`]), and one violation refuses the complete
    /// set, so a blocking Finding cannot degrade into an empty pass. Validation or storage
    /// failure leaves the event log untouched, so a retry cannot inherit half a reduction and
    /// duplicate the reviewers that were committed first.
    ///
    /// [`ReviewerReport::into_report`]: af_contracts::reviewer_result::ReviewerReport::into_report
    pub fn add_canonical_stage_outputs(
        &mut self,
        stages: &[CanonicalStage<'_>],
    ) -> Result<CanonicalReduction, StoreError> {
        // Supply actual selected Attempt envelopes, then exercise the exact native reducer.
        let ids = stages
            .iter()
            .map(|stage| {
                self.cas
                    .put_artifact(
                        af_contracts::contract::REVIEWER_RESULT_V2,
                        Producer::Attempt {
                            run_id: self.run_id.clone(),
                            node_id: stage.source.into(),
                            attempt_id: stage.attempt_id.into(),
                        },
                        stage.input_artifacts.to_vec(),
                        Some(stage.subject_snapshot_id.into()),
                        serde_json::to_value(stage.stage).unwrap(),
                    )
                    .map(|(id, _)| id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let stages = stages
            .iter()
            .zip(&ids)
            .map(|(s, id)| CanonicalStage {
                result_artifact_id: id,
                ..*s
            })
            .collect::<Vec<_>>();
        let prepared =
            af_store::prepare_canonical_task_review(self.cas, &self.run_id, &self.ledger, &stages)?;
        let events = prepared
            .events
            .into_iter()
            .map(|e| self.bind_round(e))
            .collect::<Result<Vec<_>, _>>()?;
        let appended = self.store.append_batch(&self.run_id, self.cas, &events)?;
        for event in &appended {
            self.advance_watermark(event)?;
        }
        self.ledger = prepared.ledger;
        Ok(prepared.reduction)
    }

    pub fn group(&mut self, from: &str, into: &str) -> Result<(), StoreError> {
        self.grouping_transition(from, into, FindingGroupingAction::Group)
    }

    pub fn add_evidence(
        &mut self,
        demand_id: &str,
        content_artifact_id: &str,
        actor: &str,
    ) -> Result<String, StoreError> {
        self.cas
            .verify(content_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let demand = self.ledger.demand(demand_id).ok_or_else(|| {
            StoreError::Conflict(format!(
                "cannot add Evidence for unknown Demand `{demand_id}`"
            ))
        })?;
        let demand_record_id = demand
            .record_ids
            .last()
            .cloned()
            .ok_or_else(|| StoreError::Conflict("Demand has no artifact record".into()))?;
        let subject_id = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        let payload = af_contracts::EvidenceV1 {
            demand_id: demand_id.to_string(),
            subject_id,
            content_artifact_id: content_artifact_id.to_string(),
            actor: actor.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::EVIDENCE_V1,
            EventType::EvidenceAddedV1,
            demand_id,
            vec![demand_record_id, content_artifact_id.to_string()],
            serde_json::to_value(payload)?,
        )
    }

    pub fn satisfy_demand(
        &mut self,
        demand_id: &str,
        evidence_id: &str,
        policy_revision: &str,
        reason: &str,
    ) -> Result<String, StoreError> {
        let demand = self.ledger.demand(demand_id).ok_or_else(|| {
            StoreError::Conflict(format!("cannot satisfy unknown Demand `{demand_id}`"))
        })?;
        let evidence = demand
            .evidence
            .iter()
            .find(|evidence| evidence.artifact_id == evidence_id)
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "Evidence `{evidence_id}` is not linked to Demand `{demand_id}`"
                ))
            })?;
        let subject_id = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        if evidence.evidence.subject_id != subject_id {
            return Err(StoreError::Conflict(
                "Evidence is stale for the active Subject".into(),
            ));
        }
        let payload = af_contracts::EvidenceSatisfactionV1 {
            demand_id: demand_id.to_string(),
            evidence_id: evidence_id.to_string(),
            subject_id,
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::EVIDENCE_SATISFACTION_V1,
            EventType::EvidenceSatisfiedV1,
            demand_id,
            vec![evidence.record_id.clone()],
            serde_json::to_value(payload)?,
        )
    }

    pub fn admit_evidence_reuse(
        &mut self,
        demand_id: &str,
        satisfaction_id: &str,
        actor: &str,
        policy_revision: &str,
        reason: &str,
    ) -> Result<String, StoreError> {
        let demand = self.ledger.demand(demand_id).ok_or_else(|| {
            StoreError::Conflict(format!(
                "cannot admit reuse for unknown Demand `{demand_id}`"
            ))
        })?;
        let satisfaction = demand
            .satisfactions
            .iter()
            .find(|satisfaction| satisfaction.artifact_id == satisfaction_id)
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "Satisfaction `{satisfaction_id}` is not linked to Demand `{demand_id}`"
                ))
            })?;
        let payload = af_contracts::EvidenceReuseAdmissionV1 {
            demand_id: demand_id.to_string(),
            satisfaction_id: satisfaction_id.to_string(),
            subject_id: satisfaction.satisfaction.subject_id.clone(),
            actor: actor.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::EVIDENCE_REUSE_ADMISSION_V1,
            EventType::EvidenceReuseAdmittedV1,
            demand_id,
            vec![satisfaction.record_id.clone()],
            serde_json::to_value(payload)?,
        )
    }

    pub fn waive_demand(
        &mut self,
        demand_id: &str,
        actor: &str,
        policy_revision: &str,
        reason: &str,
    ) -> Result<String, StoreError> {
        let demand = self.ledger.demand(demand_id).ok_or_else(|| {
            StoreError::Conflict(format!("cannot waive unknown Demand `{demand_id}`"))
        })?;
        let demand_record_id = demand
            .record_ids
            .last()
            .cloned()
            .ok_or_else(|| StoreError::Conflict("Demand has no artifact record".into()))?;
        let subject_id = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        let payload = af_contracts::DemandWaiverV1 {
            demand_id: demand_id.to_string(),
            subject_id,
            actor: actor.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::DEMAND_WAIVER_V1,
            EventType::DemandWaivedV1,
            demand_id,
            vec![demand_record_id],
            serde_json::to_value(payload)?,
        )
    }

    pub fn attest_change(
        &mut self,
        finding_id: &str,
        changed_regions: Vec<af_contracts::ChangedRegionV1>,
        actor: &str,
        reason: &str,
        evidence_ids: Vec<String>,
    ) -> Result<String, StoreError> {
        let expected_finding_view_id =
            self.ledger.finding_view_id(finding_id).ok_or_else(|| {
                StoreError::Conflict(format!("cannot attest unknown Finding `{finding_id}`"))
            })?;
        let subject_id = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        let change_set_id = self.ledger.active_change_set_id().map(str::to_string);
        let head_snapshot_id = self
            .ledger
            .active_head_snapshot_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject Snapshot".into()))?
            .to_string();
        let payload = af_contracts::ChangeAttestationV1 {
            finding_id: finding_id.to_string(),
            expected_finding_view_id,
            subject_id,
            change_set_id: change_set_id.clone(),
            changed_regions,
            actor: actor.to_string(),
            reason: reason.to_string(),
            evidence_ids,
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::CHANGE_ATTESTATION_V1,
            EventType::ChangeAttestedV1,
            finding_id,
            change_set_id
                .into_iter()
                .chain([head_snapshot_id])
                .collect(),
            serde_json::to_value(payload)?,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verify_fix(
        &mut self,
        finding_id: &str,
        attestation_id: &str,
        verifier: &str,
        policy_revision: &str,
        positive: bool,
        reason: &str,
        evidence_ids: Vec<String>,
    ) -> Result<(String, Option<String>), StoreError> {
        let attestation = self.ledger.attestation(attestation_id).ok_or_else(|| {
            StoreError::Conflict(format!("unknown Change Attestation `{attestation_id}`"))
        })?;
        if attestation.attestation.finding_id != finding_id {
            return Err(StoreError::Conflict(
                "Fix Verification Finding disagrees with its Attestation".into(),
            ));
        }
        let attestation_record_id = attestation.record_id.clone();
        let expected_finding_view_id =
            self.ledger.finding_view_id(finding_id).ok_or_else(|| {
                StoreError::Conflict(format!("cannot verify unknown Finding `{finding_id}`"))
            })?;
        let subject_id = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        let verification = af_contracts::FixVerificationV1 {
            finding_id: finding_id.to_string(),
            attestation_id: attestation_id.to_string(),
            expected_finding_view_id,
            subject_id: subject_id.clone(),
            verifier: verifier.to_string(),
            policy_revision: policy_revision.to_string(),
            positive,
            reason: reason.to_string(),
            evidence_ids: evidence_ids.clone(),
        };
        verification.validate().map_err(StoreError::Conflict)?;
        let verification_id = self.publish_operator_artifact(
            af_contracts::contract::FIX_VERIFICATION_V1,
            EventType::FixVerifiedV1,
            finding_id,
            vec![attestation_record_id],
            serde_json::to_value(verification)?,
        )?;
        if !positive {
            return Ok((verification_id, None));
        }
        let verification_record_id = self
            .ledger
            .verification(&verification_id)
            .expect("published verification is projected")
            .record_id
            .clone();
        let resolution = af_contracts::FindingResolutionV1 {
            finding_id: finding_id.to_string(),
            expected_finding_view_id: self
                .ledger
                .finding_view_id(finding_id)
                .expect("verified Finding still exists"),
            subject_id,
            outcome: af_contracts::FindingResolutionOutcome::Fixed,
            actor: verifier.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
            evidence_ids,
            verification_id: Some(verification_id.clone()),
            max_accepted_severity: None,
            tracking_reference: None,
            expires_at_policy_time: None,
        };
        resolution.validate().map_err(StoreError::Conflict)?;
        let resolution_id = self.publish_operator_artifact(
            af_contracts::contract::FINDING_RESOLUTION_V1,
            EventType::FindingResolutionRecordedV1,
            finding_id,
            vec![verification_record_id],
            serde_json::to_value(resolution)?,
        )?;
        Ok((verification_id, Some(resolution_id)))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resolve_nonfixed(
        &mut self,
        finding_id: &str,
        outcome: af_contracts::FindingResolutionOutcome,
        actor: &str,
        policy_revision: &str,
        reason: &str,
        evidence_ids: Vec<String>,
        max_accepted_severity: Option<Severity>,
        tracking_reference: Option<String>,
        expires_at_policy_time: Option<u64>,
    ) -> Result<String, StoreError> {
        if outcome == af_contracts::FindingResolutionOutcome::Fixed {
            return Err(StoreError::Conflict(
                "fixed requires Change Attestation and Fix Verification".into(),
            ));
        }
        let active_subject = self
            .ledger
            .active_subject_id()
            .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
            .to_string();
        if let Some(existing) = self.ledger.resolution(finding_id)
            && existing.resolution.subject_id == active_subject
            && existing.resolution.outcome == outcome
            && existing.resolution.actor == actor
            && existing.resolution.policy_revision == policy_revision
            && existing.resolution.reason == reason
            && existing.resolution.evidence_ids == evidence_ids
            && existing.resolution.max_accepted_severity == max_accepted_severity
            && existing.resolution.tracking_reference == tracking_reference
            && existing.resolution.expires_at_policy_time == expires_at_policy_time
        {
            return Ok(existing.artifact_id.clone());
        }
        if outcome == af_contracts::FindingResolutionOutcome::WontfixTracked
            && expires_at_policy_time.is_some_and(|expiry| expiry <= self.ledger.policy_time())
        {
            return Err(StoreError::Conflict(format!(
                "tracked-wontfix expiry must be later than current persisted policy time {}",
                self.ledger.policy_time()
            )));
        }
        let payload = af_contracts::FindingResolutionV1 {
            finding_id: finding_id.to_string(),
            expected_finding_view_id: self.ledger.finding_view_id(finding_id).ok_or_else(|| {
                StoreError::Conflict(format!("cannot resolve unknown Finding `{finding_id}`"))
            })?,
            subject_id: active_subject,
            outcome,
            actor: actor.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
            evidence_ids,
            verification_id: None,
            max_accepted_severity,
            tracking_reference,
            expires_at_policy_time,
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::FINDING_RESOLUTION_V1,
            EventType::FindingResolutionRecordedV1,
            finding_id,
            Vec::new(),
            serde_json::to_value(payload)?,
        )
    }

    pub fn challenge_resolution(
        &mut self,
        finding_id: &str,
        kind: af_contracts::ResolutionChallengeKind,
        actor: &str,
        reason: &str,
        evidence_ids: Vec<String>,
    ) -> Result<String, StoreError> {
        let resolution = self.ledger.resolution(finding_id).ok_or_else(|| {
            StoreError::Conflict(format!("Finding `{finding_id}` has no active Resolution"))
        })?;
        let resolution_id = resolution.artifact_id.clone();
        let resolution_record_id = resolution.record_id.clone();
        let payload = af_contracts::ResolutionChallengeV1 {
            finding_id: finding_id.to_string(),
            resolution_id,
            subject_id: self
                .ledger
                .active_subject_id()
                .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
                .to_string(),
            kind,
            actor: actor.to_string(),
            reason: reason.to_string(),
            evidence_ids,
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            af_contracts::contract::RESOLUTION_CHALLENGE_V1,
            EventType::FindingResolutionChallengedV1,
            finding_id,
            vec![resolution_record_id],
            serde_json::to_value(payload)?,
        )
    }

    pub fn advance_policy_time(
        &mut self,
        tick: u64,
        actor: &str,
        reason: &str,
    ) -> Result<(String, Vec<String>), StoreError> {
        if tick <= self.ledger.policy_time() {
            return Err(StoreError::Conflict(
                "policy time must advance monotonically".into(),
            ));
        }
        let payload = af_contracts::PolicyTimeV1 {
            tick,
            actor: actor.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        let time_id = self.publish_operator_artifact(
            af_contracts::contract::POLICY_TIME_V1,
            EventType::PolicyTimeAdvancedV1,
            "policy-time",
            Vec::new(),
            serde_json::to_value(payload)?,
        )?;
        let expired: Vec<String> = self
            .ledger
            .expiring_resolutions(tick)
            .iter()
            .map(|resolution| resolution.resolution.finding_id.clone())
            .collect();
        let mut challenges = Vec::new();
        for finding_id in expired {
            challenges.push(self.challenge_resolution(
                &finding_id,
                af_contracts::ResolutionChallengeKind::Expired,
                actor,
                "tracked wontfix expired at persisted policy time",
                Vec::new(),
            )?);
        }
        Ok((time_id, challenges))
    }

    fn publish_operator_artifact(
        &mut self,
        artifact_type: &str,
        event_type: EventType,
        correlation_id: &str,
        input_artifacts: Vec<String>,
        payload: serde_json::Value,
    ) -> Result<String, StoreError> {
        let operation_id = format!(
            "{artifact_type}:{}",
            af_store::content_id(&payload)
                .map_err(|error| StoreError::Conflict(error.to_string()))?
        );
        let subject_snapshot_id = self
            .ledger
            .active_head_snapshot_id()
            .ok_or_else(|| {
                StoreError::Conflict("Campaign has no readable Subject Snapshot".into())
            })?
            .to_string();
        let (record_id, envelope) = self
            .cas
            .put_artifact(
                artifact_type,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: None,
                    operation_id,
                },
                input_artifacts,
                Some(subject_snapshot_id),
                payload,
            )
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let event = NewEvent::new(
            event_type,
            serde_json::to_value(af_contracts::RecordedArtifactPayloadV1 {
                artifact_id: record_id.clone(),
            })?,
        )
        .correlating(correlation_id.to_string())
        .referencing(vec![record_id]);
        let event = self.store.append(&self.run_id, self.cas, event)?;
        self.validate_watermark(&event)?;
        self.ledger.apply_event(&event, self.cas)?;
        self.event_count += 1;
        Ok(envelope.artifact_id)
    }

    pub fn ungroup(&mut self, from: &str, into: &str) -> Result<(), StoreError> {
        self.grouping_transition(from, into, FindingGroupingAction::Ungroup)
    }

    fn grouping_transition(
        &mut self,
        from: &str,
        into: &str,
        action: FindingGroupingAction,
    ) -> Result<(), StoreError> {
        match action {
            FindingGroupingAction::Group => self.ledger.validate_group(from, into)?,
            FindingGroupingAction::Ungroup => self.ledger.validate_ungroup(from, into)?,
        }
        let grouping = FindingGroupingV1 {
            from: from.to_string(),
            into: into.to_string(),
            action,
            round: self.ledger.round,
        };
        grouping.validate().map_err(StoreError::Conflict)?;
        let operation_id = format!(
            "review.kernel/finding-grouping@1:{}",
            af_store::content_id(&serde_json::to_value(&grouping)?)
                .map_err(|error| StoreError::Conflict(error.to_string()))?
        );
        let (record_id, _) = self
            .cas
            .put_artifact(
                af_contracts::contract::FINDING_GROUPING_V1,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: None,
                    operation_id,
                },
                Vec::new(),
                None,
                serde_json::to_value(&grouping)?,
            )
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let payload = FindingGroupingEventPayloadV1 {
            from: from.to_string(),
            into: into.to_string(),
            grouping_artifact_id: record_id.clone(),
        };
        let event_type = match action {
            FindingGroupingAction::Group => EVENT_FINDINGS_GROUPED,
            FindingGroupingAction::Ungroup => EVENT_FINDINGS_UNGROUPED,
        };
        let event = NewEvent::new(event_type, serde_json::to_value(payload)?)
            .correlating(from.to_string())
            .referencing(vec![record_id]);
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

pub fn add_flat_results(
    ingest: &mut Ingest<'_>,
    cas: &Cas,
    run_id: &str,
    round: &Authority,
    results: &[(&str, &str, &ReviewerStageOutput)],
) -> Result<CanonicalReduction, StoreError> {
    let ids = results
        .iter()
        .map(|(_, _, out)| cas.put_json(&serde_json::to_value(out).unwrap()).unwrap())
        .collect::<Vec<_>>();
    let stages = results
        .iter()
        .zip(&ids)
        .map(|((source, attempt_id, stage), id)| CanonicalStage {
            source,
            demand_requirement: DemandRequirement::Required,
            stage,
            attempt_id,
            result_artifact_id: id,
            input_artifacts: &[],
            subject_snapshot_id: &round.head,
            subject_id: &round.subject,
        })
        .collect::<Vec<_>>();
    let _ = run_id;
    ingest.add_canonical_stage_outputs(&stages)
}
pub fn resolve(
    store: &mut LedgerEvents,
    cas: &Cas,
    run_id: &str,
    round: &Authority,
    key: &str,
    status: af_store::Status,
    note: &str,
) {
    let n = NativeProjection::rebuild(store, cas, run_id)
        .unwrap()
        .ledger
        .round;
    store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::FindingResolvedV1,
                serde_json::json!({"key":key,"status":status.as_str(),"note":note,"round":n}),
            )
            .caused_by(&round.round_event_id)
            .correlating(key),
        )
        .unwrap();
}
