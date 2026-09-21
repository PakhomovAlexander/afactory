//! A Task that executes a captured Review Round cannot outlive that Round's authority.
//! Compare the Review epoch under the same SQLite writer lock as new Task dispatch records.
//! Evidence retention (settlement, late usage, release and diagnostics) remains possible.

use review_core::task::execution::TaskExecutionRecordV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::{CampaignManifestV1, PortCardinality, SubjectV1};

use super::*;
use rusqlite::OptionalExtension;

pub(super) struct ReviewRoundFence {
    binding: LegacyReviewRoundV1,
    closed_report: Option<String>,
}

impl ReviewRoundFence {
    pub(super) fn capture(cas: &Cas, task: &TaskRevisionV1) -> Result<Option<Self>, StoreError> {
        let mut roots = task
            .inputs
            .values()
            .filter(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1);
        let Some(input) = roots.next() else {
            return Ok(None);
        };
        if roots.next().is_some()
            || input.cardinality != PortCardinality::One
            || input.artifact_ids.len() != 1
        {
            return Err(conflict("Task requires one exact captured Review Round"));
        }
        let wrapper = envelope(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
        let binding: LegacyReviewRoundV1 = serde_json::from_value(wrapper.payload)?;
        binding.validate().map_err(conflict)?;
        if wrapper.input_artifacts != binding.artifact_refs()
            || wrapper.subject_snapshot_id.as_deref() != Some(&binding.head_snapshot_id)
            || input.snapshot_id != wrapper.subject_snapshot_id
        {
            return Err(conflict(
                "Task Review Round has inconsistent artifact authority",
            ));
        }
        let manifest: CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&binding.campaign_manifest_id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?,
        )?;
        manifest.validate().map_err(conflict)?;
        let subject: SubjectV1 = serde_json::from_value(
            cas.get_json(&binding.subject_id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?,
        )?;
        subject.validate().map_err(conflict)?;
        if subject.head_snapshot_id != binding.head_snapshot_id
            || subject.kind != manifest.subject_kind
            || subject.base_snapshot_id != manifest.base_snapshot_id
        {
            return Err(conflict(
                "Task Review Round differs from its captured Subject",
            ));
        }
        cas.verify(&binding.head_snapshot_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        Ok(Some(Self {
            binding,
            closed_report: None,
        }))
    }

    pub(super) fn validate(&self, connection: &rusqlite::Connection) -> Result<(), StoreError> {
        let b = &self.binding;
        let Some((event, round)) = super::super::latest_round(connection, &b.campaign_id)? else {
            return Err(conflict("Task Review Round has no durable Campaign Round"));
        };
        if event != b.round_event_id
            || round.round != b.round
            || round.epoch != b.epoch
            || round.subject_id != b.subject_id
            || round.campaign_manifest_id != b.campaign_manifest_id
            || (self.closed_report.is_none()
                && super::super::round_has_terminal_report(connection, &b.campaign_id, &event)?)
        {
            return Err(conflict("Task Review Round is superseded or closed"));
        }
        if let Some(expected) = &self.closed_report {
            let latest:Option<String>=connection.query_row("SELECT event_id FROM events WHERE run_id=?1 AND type='RunReport@6' AND causation_id=?2 AND json_extract(payload,'$.verdict.kind')='pass' ORDER BY sequence DESC LIMIT 1",rusqlite::params![b.campaign_id,event],|r|r.get(0)).optional()?;
            if latest.as_ref() != Some(expected) {
                return Err(conflict("Integration lost its exact passing closed Round"));
            }
        }
        Ok(())
    }
    pub(super) fn capture_closed(
        cas: &Cas,
        revision: &TaskRevisionV1,
        report: &str,
    ) -> Result<Self, StoreError> {
        let mut fence = Self::capture(cas, revision)?
            .ok_or_else(|| conflict("Integration has no captured Review Round"))?;
        fence.closed_report = Some(report.into());
        Ok(fence)
    }
    pub(super) fn for_state(cas: &Cas, state: &TaskProjection) -> Result<Option<Self>, StoreError> {
        if let Some(phase) = state
            .execution
            .as_ref()
            .and_then(|e| e.active_review_integration())
        {
            return Self::capture_closed(
                cas,
                &state.revision,
                &phase.phase().closing_report_event_id,
            )
            .map(Some);
        }
        Self::capture(cas, &state.revision)
    }
}

pub(super) fn fence_for_transition(
    cas: &Cas,
    transition: &TaskTransitionV1,
    state: Option<&TaskProjection>,
) -> Result<Option<ReviewRoundFence>, StoreError> {
    match &transition.change {
        TaskChangeV1::ReviewIntegrationSelected { phase_id }
        | TaskChangeV1::ReviewIntegrationFinished { phase_id, .. } => {
            let phase = super::review_integration::read_task_review_integration(cas, phase_id)?;
            let state = state.ok_or_else(|| conflict("Integration phase precedes Task"))?;
            return ReviewRoundFence::capture_closed(
                cas,
                &state.revision,
                &phase.closing_report_event_id,
            )
            .map(Some);
        }
        TaskChangeV1::ReviewContinued { handoff_id } => {
            let handoff = super::review_handoff::read_task_review_handoff(cas, handoff_id)?;
            return ReviewRoundFence::capture(cas, &revision(cas, &handoff.successor_revision_id)?);
        }
        TaskChangeV1::Opened { revision_id, .. }
        | TaskChangeV1::SourceRefreshed { revision_id, .. }
        | TaskChangeV1::PlanningCompleted { revision_id, .. } => {
            return ReviewRoundFence::capture(cas, &revision(cas, revision_id)?);
        }
        TaskChangeV1::ExecutionRecorded { record_id } => {
            let record = super::execution::read_execution_record(cas, record_id)?.record;
            match record {
                TaskExecutionRecordV1::Invocation { .. }
                | TaskExecutionRecordV1::Reserved { .. }
                | TaskExecutionRecordV1::ContextBound { .. }
                | TaskExecutionRecordV1::Started { .. }
                | TaskExecutionRecordV1::Published { .. }
                | TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
                | TaskExecutionRecordV1::OwnedChildPublished { .. }
                | TaskExecutionRecordV1::OwnedChildrenCompleted { .. }
                | TaskExecutionRecordV1::ExperimentPrepared { .. }
                | TaskExecutionRecordV1::ExperimentPlanDecided { .. }
                | TaskExecutionRecordV1::ExperimentChildrenRegistered { .. } => {}
                // A stale Round stops new effects, not accounting for work already paid for.
                TaskExecutionRecordV1::Released { .. }
                | TaskExecutionRecordV1::Settled { .. }
                | TaskExecutionRecordV1::UsageObserved { .. } => return Ok(None),
            }
        }
        TaskChangeV1::PlanProposed { .. }
        | TaskChangeV1::PlanAdmitted { .. }
        | TaskChangeV1::PlanDecided { .. }
        | TaskChangeV1::RecordingResumed { .. }
        | TaskChangeV1::Resumed {} => {}
        TaskChangeV1::LeaseTaken { .. }
        | TaskChangeV1::LeaseRenewed { .. }
        | TaskChangeV1::LeaseReleased {}
        | TaskChangeV1::ApprovalRevoked { .. }
        | TaskChangeV1::Waiting { .. }
        | TaskChangeV1::Finished { .. }
        | TaskChangeV1::RunReported { .. }
        | TaskChangeV1::DeliveryRecorded { .. }
        | TaskChangeV1::AdoptionObservationRecorded { .. } => return Ok(None),
    }
    state
        .map(|state| ReviewRoundFence::for_state(cas, state))
        .transpose()
        .map(Option::flatten)
}
