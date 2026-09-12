//! Prepare successor intent under the same captured Campaign policy. The Store separately
//! proves the canonical Round transition and installs it without replacing the Task ledger.
use super::*;

impl LegacyReviewPlanCompiler {
    /// This compiler must have been reopened for the successor Round using the original
    /// policy ID. Only the revision chain and exact captured root inputs change here.
    pub fn prepare_continuation_revision(
        &self,
        cas: &Cas,
        previous_revision_id: &str,
        previous: &TaskRevisionV1,
    ) -> Result<TaskRevisionV1, String> {
        let recorded = cas
            .get_artifact(previous_revision_id)
            .map_err(|e| e.to_string())?;
        if recorded.artifact_type != review_core::task::TASK_REVISION_V1
            || serde_json::from_value::<TaskRevisionV1>(recorded.payload)
                .map_err(|e| e.to_string())?
                != *previous
        {
            return Err("Review continuation changed its exact predecessor revision".into());
        }
        previous.validate()?;
        if previous.authority != self.authority() {
            return Err("Review continuation changed its captured Task policy".into());
        }
        let old_input = previous
            .inputs
            .get("round")
            .ok_or("Review predecessor has no captured Round")?;
        let [old_round_id] = old_input.artifact_ids.as_slice() else {
            return Err("Review predecessor has ambiguous Round authority".into());
        };
        let old = cas.get_artifact(old_round_id).map_err(|e| e.to_string())?;
        let binding: LegacyReviewRoundV1 =
            serde_json::from_value(old.payload).map_err(|e| e.to_string())?;
        binding.validate()?;
        if old_input.artifact_type != LEGACY_REVIEW_ROUND_V1
            || old_input.cardinality != PortCardinality::One
            || old.artifact_type != LEGACY_REVIEW_ROUND_V1
            || old.input_artifacts != binding.artifact_refs()
            || old.subject_snapshot_id.as_ref() != Some(&binding.head_snapshot_id)
            || old_input.snapshot_id != old.subject_snapshot_id
        {
            return Err("Review predecessor has inconsistent Round inputs".into());
        }
        let next_round = self.round.binding();
        let next_numeric_round =
            binding.round.checked_add(1) == Some(next_round.round) && next_round.epoch == 1;
        let next_epoch = binding.round == next_round.round
            && binding.epoch.checked_add(1) == Some(next_round.epoch);
        if binding.campaign_id != next_round.campaign_id
            || binding.campaign_manifest_id != next_round.campaign_manifest_id
            || binding.round_event_id == next_round.round_event_id
            || !(next_numeric_round || next_epoch)
            || (next_numeric_round && self.mode()? != ReviewMode::Heavy)
        {
            return Err(
                "Review continuation is not an adjacent captured Campaign Round or epoch".into(),
            );
        }
        let mut expected =
            self.prepare_revision(cas, &previous.task_id, previous.limits.clone())?;
        let inputs = expected.inputs.clone();
        // A Task goal is intent, not Round authority. Preserve the original text verbatim.
        expected.goal = previous.goal.clone();
        expected.revision = previous.revision;
        expected.previous_revision_id = previous.previous_revision_id.clone();
        expected.inputs = previous.inputs.clone();
        if expected != *previous {
            return Err("Review continuation changed invariant Task fields".into());
        }
        let mut next = previous.clone();
        next.revision = previous
            .revision
            .checked_add(1)
            .ok_or("Task revision overflow")?;
        next.previous_revision_id = Some(previous_revision_id.into());
        next.inputs = inputs;
        next.validate()?;
        Ok(next)
    }
}
