//! Ledger ingestion: reducing flat reviewer results into Finding events, and the operator
//! transitions (resolution, grouping, Demand evidence) that follow them.
//!
//! [`Ingest`] appends to one run's log under its active Round and keeps the Ledger projection
//! folded in step. The pure reduction it shares with the Task Review host lives in
//! `reduction`.

mod reduction;
pub use reduction::{PreparedReviewReduction, prepare_canonical_task_review};

use review_core::legacy::LegacyBenchmarkDemand;
use review_core::{
    CANONICAL_FINDING_IDENTITY_POLICY, EventType, FindingDispositionPosition, FindingDispositionV1,
    FindingGroupingAction, FindingGroupingEventPayloadV1, FindingGroupingV1, FindingReport,
    LegacyStageOutput, Producer, RunEvent, Severity,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::cas::Cas;
use crate::ledger::{
    EVENT_FINDING_REPORTED, EVENT_FINDING_RESOLVED, EVENT_FINDINGS_GROUPED,
    EVENT_FINDINGS_UNGROUPED, EVENT_GENERATION_ADVANCED, Ledger, LedgerProjection, ReportScope,
    Status,
};
use crate::store::{EventStore, NewEvent, StoreError};

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
    pub demand_requirement: review_core::DemandRequirement,
    pub stage: &'a LegacyStageOutput,
    pub attempt_id: &'a str,
    pub result_artifact_id: &'a str,
    pub input_artifacts: &'a [String],
    pub subject_snapshot_id: &'a str,
    pub subject_id: &'a str,
}

/// The exact immutable inputs emitted by one canonical ledger reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalReduction {
    /// Domain-separated typed Report IDs recorded in `FindingSet@1`.
    pub selected_report_ids: Vec<String>,
    /// Same-result Report index authority for post-reduction Proposal finalization.
    pub report_ids_by_source: std::collections::BTreeMap<String, Vec<String>>,
    /// Domain-separated relation IDs recorded in `FindingSet@1`.
    pub relation_ids: Vec<String>,
    /// Domain-separated typed Demand artifacts selected by this reduction.
    pub selected_demand_artifact_ids: Vec<String>,
    /// CAS IDs of all Report and relation envelopes consumed by the Set reducer.
    pub input_artifact_ids: Vec<String>,
    /// CAS records for selected Demand envelopes.
    pub demand_input_artifact_ids: Vec<String>,
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
    demand_requirement: review_core::DemandRequirement,
    reports: Vec<FindingReport>,
    demands: Vec<LegacyBenchmarkDemand>,
    disputes: Vec<review_core::legacy::LegacyDispute>,
    provenance: ReportProvenance,
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

    /// The store refuses a Round-runtime event that no Round is bound to.
    fn bind_round(&self, event: NewEvent) -> NewEvent {
        match &self.round_event_id {
            Some(round) => event.caused_by(round),
            None => event,
        }
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
            )),
        )?;
        self.validate_watermark(&event)?;
        self.ledger.apply_event(&event, self.cas)?;
        self.event_count += 1;
        Ok(round)
    }

    /// Bridge selected flat results into typed, provenance-carrying Report artifacts and reduce
    /// them with the canonical path-independent identity policy. Every finding must satisfy
    /// `FindingReport@1` ([`LegacyFinding::into_report`]), and one violation refuses the complete
    /// set, so a blocking verdict cannot degrade into an empty pass. Validation or storage
    /// failure leaves the event log untouched, so a retry cannot inherit half a reduction and
    /// duplicate the reviewers that were committed first.
    ///
    /// [`LegacyFinding::into_report`]: review_core::legacy::LegacyFinding::into_report
    pub fn add_canonical_stage_outputs(
        &mut self,
        stages: &[CanonicalStage<'_>],
    ) -> Result<CanonicalReduction, StoreError> {
        let prepared = reduction::prepare_canonical_inputs(&self.run_id, &self.ledger, stages)?;
        self.add_prepared_outputs(&prepared)
    }

    fn add_prepared_outputs(
        &mut self,
        stages: &[PreparedStage],
    ) -> Result<CanonicalReduction, StoreError> {
        let prepared =
            reduction::prepare_review_outputs(self.cas, &self.run_id, &self.ledger, stages)?;
        let events: Vec<_> = prepared
            .events
            .into_iter()
            .map(|event| self.bind_round(event))
            .collect();
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
        let payload = review_core::EvidenceV1 {
            demand_id: demand_id.to_string(),
            subject_id,
            content_artifact_id: content_artifact_id.to_string(),
            actor: actor.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            review_core::contract::EVIDENCE_V1,
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
        let payload = review_core::EvidenceSatisfactionV1 {
            demand_id: demand_id.to_string(),
            evidence_id: evidence_id.to_string(),
            subject_id,
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            review_core::contract::EVIDENCE_SATISFACTION_V1,
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
        let payload = review_core::EvidenceReuseAdmissionV1 {
            demand_id: demand_id.to_string(),
            satisfaction_id: satisfaction_id.to_string(),
            subject_id: satisfaction.satisfaction.subject_id.clone(),
            actor: actor.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
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
        let payload = review_core::DemandWaiverV1 {
            demand_id: demand_id.to_string(),
            subject_id,
            actor: actor.to_string(),
            policy_revision: policy_revision.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        self.publish_operator_artifact(
            review_core::contract::DEMAND_WAIVER_V1,
            EventType::DemandWaivedV1,
            demand_id,
            vec![demand_record_id],
            serde_json::to_value(payload)?,
        )
    }

    pub fn attest_change(
        &mut self,
        finding_id: &str,
        changed_regions: Vec<review_core::ChangedRegionV1>,
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
        let payload = review_core::ChangeAttestationV1 {
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
            review_core::contract::CHANGE_ATTESTATION_V1,
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
        let verification = review_core::FixVerificationV1 {
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
            review_core::contract::FIX_VERIFICATION_V1,
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
        let resolution = review_core::FindingResolutionV1 {
            finding_id: finding_id.to_string(),
            expected_finding_view_id: self
                .ledger
                .finding_view_id(finding_id)
                .expect("verified Finding still exists"),
            subject_id,
            outcome: review_core::FindingResolutionOutcome::Fixed,
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
            review_core::contract::FINDING_RESOLUTION_V1,
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
        outcome: review_core::FindingResolutionOutcome,
        actor: &str,
        policy_revision: &str,
        reason: &str,
        evidence_ids: Vec<String>,
        max_accepted_severity: Option<Severity>,
        tracking_reference: Option<String>,
        expires_at_policy_time: Option<u64>,
    ) -> Result<String, StoreError> {
        if outcome == review_core::FindingResolutionOutcome::Fixed {
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
        if outcome == review_core::FindingResolutionOutcome::WontfixTracked
            && expires_at_policy_time.is_some_and(|expiry| expiry <= self.ledger.policy_time())
        {
            return Err(StoreError::Conflict(format!(
                "tracked-wontfix expiry must be later than current persisted policy time {}",
                self.ledger.policy_time()
            )));
        }
        let payload = review_core::FindingResolutionV1 {
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
            review_core::contract::FINDING_RESOLUTION_V1,
            EventType::FindingResolutionRecordedV1,
            finding_id,
            Vec::new(),
            serde_json::to_value(payload)?,
        )
    }

    pub fn challenge_resolution(
        &mut self,
        finding_id: &str,
        kind: review_core::ResolutionChallengeKind,
        actor: &str,
        reason: &str,
        evidence_ids: Vec<String>,
    ) -> Result<String, StoreError> {
        let resolution = self.ledger.resolution(finding_id).ok_or_else(|| {
            StoreError::Conflict(format!("Finding `{finding_id}` has no active Resolution"))
        })?;
        let resolution_id = resolution.artifact_id.clone();
        let resolution_record_id = resolution.record_id.clone();
        let payload = review_core::ResolutionChallengeV1 {
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
            review_core::contract::RESOLUTION_CHALLENGE_V1,
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
        let payload = review_core::PolicyTimeV1 {
            tick,
            actor: actor.to_string(),
            reason: reason.to_string(),
        };
        payload.validate().map_err(StoreError::Conflict)?;
        let time_id = self.publish_operator_artifact(
            review_core::contract::POLICY_TIME_V1,
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
                review_core::ResolutionChallengeKind::Expired,
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
            crate::content_id(&payload).map_err(|error| StoreError::Conflict(error.to_string()))?
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
            serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
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
            crate::content_id(&serde_json::to_value(&grouping)?)
                .map_err(|error| StoreError::Conflict(error.to_string()))?
        );
        let (record_id, _) = self
            .cas
            .put_artifact(
                review_core::contract::FINDING_GROUPING_V1,
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

/// A new canonical Finding is derived from the first selected Report that establishes it.
pub fn canonical_finding_id(report_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/finding-id/v1\0");
    hasher.update(report_id.as_bytes());
    format!("sha256:{}", review_core::hex::encode(&hasher.finalize()))
}

/// A reviewer re-stating the exact same obligation keeps one Demand identity across Rounds.
pub fn canonical_demand_id(source: &str, demand: &LegacyBenchmarkDemand) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/demand-id/v1\0");
    for field in [
        source,
        demand.claim.as_str(),
        demand.why.as_str(),
        demand.suggested_method.as_str(),
    ] {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{}", review_core::hex::encode(&hasher.finalize()))
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
        // A relation can only corroborate a Finding in the input Finding Set.
        for relation in &report.relations {
            if prior.get(&relation.target.id).is_none() {
                return Err(StoreError::Conflict(format!(
                    "Report {} relates to Finding `{}` outside its input Finding Set",
                    reports[index].2, relation.target.id
                )));
            }
            candidates
                .entry(root)
                .or_default()
                .insert(relation.target.id.clone());
        }
    }

    let mut keys = Vec::with_capacity(reports.len());
    for index in 0..reports.len() {
        let root = find(&mut parent, index);
        let first = &reports[first_report[&root]].2;
        let group = candidates.get(&root);
        if group.is_some_and(|group| group.len() > 1) {
            return Err(StoreError::Conflict(format!(
                "corroboration and occurrence authority disagree for Report {first}"
            )));
        }
        keys.push(
            group
                .and_then(BTreeSet::first)
                .cloned()
                .unwrap_or_else(|| canonical_finding_id(first)),
        );
    }
    Ok(keys)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An event the store admits without a Campaign Round, so sequencing can be tested alone.
    fn unscoped_event() -> NewEvent {
        NewEvent::new(EventType::SourceCapturedV1, json!({}))
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
        store.append("run", &cas, unscoped_event()).unwrap();

        let ingest = Ingest::from_projection(&mut store, &cas, "run", projection).unwrap();
        assert_eq!(ingest.event_count, 1);
    }

    #[test]
    fn a_projection_ahead_of_the_run_log_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut source = EventStore::open(directory.path().join("source.sqlite")).unwrap();
        source.append("run", &cas, unscoped_event()).unwrap();
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
        let event = store.append("run", &cas, unscoped_event()).unwrap();
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

    /// Store `report` the way a reduction does: as an enveloped `FindingReport@1`.
    fn put_report(cas: &Cas, report: &FindingReport) -> String {
        cas.put_artifact(
            review_core::contract::FINDING_REPORT_V1,
            Producer::KernelOperation {
                run_id: "run".into(),
                node_id: Some("first".into()),
                operation_id: "prior-report".into(),
            },
            Vec::new(),
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0
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
    fn an_exact_occurrence_key_attaches_to_the_prior_finding() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut prior = Ledger::default();
        let first = typed_report(Some("afactory/retry-loop@1"), Some("loop-7"));
        let first_id = put_report(&cas, &first);
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
    }

    #[test]
    fn explicit_corroboration_attaches_to_the_named_prior_finding() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut prior = Ledger::default();
        let first = typed_report(None, None);
        let first_id = put_report(&cas, &first);
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
}
