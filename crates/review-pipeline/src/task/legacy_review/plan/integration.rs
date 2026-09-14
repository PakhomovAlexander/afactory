//! The installed post-Round check allowance is captured with the original plan. Activation
//! selects only bounded data and never creates a new execution effect or resource grant.
use super::*;
use review_core::task::review_integration::{
    TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1, TaskReviewCheckSequencePolicyV1,
};

const INTEGRATION_DEPENDENCY: &str = "af/review-integration-checks";
const INTEGRATION_NODE: &str = "root.integration_checks";

impl LegacyReviewPlanCompiler {
    pub(super) fn install_integration_phase(
        &self,
        cas: &Cas,
        loaded: &review_config::Loaded,
        graph: &mut review_graph::task::CompiledTask,
        dependencies: &mut BTreeMap<String, PlanDependencyV1>,
        recorded: Option<&ExecutionPlanV1>,
    ) -> Result<Option<String>, String> {
        let Some(policy) = loaded.integration().filter(|_| {
            self.integration
                && self.policy.settings.mode == "heavy"
                && self.round.binding().round < loaded.convergence().max_rounds
        }) else {
            return Ok(None);
        };
        loaded
            .gate_execution()
            .ok_or("Integration requires captured Gate execution authority")?;
        let selected: BTreeSet<_> = policy.post_apply_checks.iter().collect();
        let ordered_check_names: Vec<_> = loaded
            .checks()
            .iter()
            .filter(|definition| selected.contains(&definition.name))
            .map(|definition| definition.name.clone())
            .collect();
        if ordered_check_names != policy.post_apply_checks {
            return Err(
                "Captured Integration check declaration order differs from post_apply_checks; choose one explicit sequence before Task admission".into(),
            );
        }
        // One raw result per Check plus the complete sequence summary must fit the
        // existing 64-artifact settlement bound. This generation grants no bundle format.
        if ordered_check_names.len() > 63 {
            return Err(
                "Captured Integration supports at most 63 post-apply checks so every raw CheckResult and the sequence summary fit the 64-artifact settlement bound".into(),
            );
        }
        let check_timeout_ms = loaded
            .check_timeout_seconds()
            .checked_mul(1000)
            .filter(|value| *value > 0 && *value <= review_core::json::SAFE_INTEGER_MAX as u64)
            .ok_or("Integration check timeout exceeds Task wire bounds")?;
        let wall_ms_per_attempt = check_timeout_ms
            .checked_mul(
                u64::try_from(ordered_check_names.len())
                    .map_err(|_| "Too many Integration checks")?,
            )
            .filter(|value| *value > 0 && *value <= review_core::json::SAFE_INTEGER_MAX as u64)
            .ok_or("Integration check sequence exceeds Task wall-time bounds")?;
        let pipeline_policy_id = self.round.authority().pipeline_policy_id.clone();
        let sequence = TaskReviewCheckSequencePolicyV1 {
            authority_policy_id: self.policy_id.clone(),
            pipeline_policy_id: pipeline_policy_id.clone(),
            // The Gate binding is declared in the captured pipeline, not a standalone artifact.
            gate_execution_policy_id: pipeline_policy_id,
            ordered_check_names,
            check_timeout_ms,
        };
        sequence.validate()?;
        let recorded_id = recorded
            .map(|plan| {
                plan.dependencies
                    .get(INTEGRATION_DEPENDENCY)
                    .map(|dependency| dependency.artifact_id.as_str())
                    .ok_or("Recorded Review plan lacks its captured Integration sequence")
            })
            .transpose()?;
        let envelope = capture_or_read(
            cas,
            TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1,
            "integration-check-sequence",
            BTreeSet::from([
                sequence.authority_policy_id.clone(),
                sequence.pipeline_policy_id.clone(),
            ])
            .into_iter()
            .collect(),
            &sequence,
            recorded_id,
        )?;
        let sequence_policy_id = envelope.artifact_id.clone();
        dependencies.insert(
            INTEGRATION_DEPENDENCY.into(),
            PlanDependencyV1 {
                name: INTEGRATION_DEPENDENCY.into(),
                artifact_id: envelope.artifact_id,
                content_digest: envelope.content_id,
            },
        );
        graph.review_integration = Some(review_graph::task::CompiledReviewIntegrationV1 {
            node: INTEGRATION_NODE.into(),
            sequence_policy_id: sequence_policy_id.clone(),
            allowance: review_attempt::task_budget::NodeAllowance {
                tokens_per_attempt: 0,
                wall_ms_per_attempt,
                max_attempts: 1,
                verification_attempts: 0,
            },
        });
        Ok(Some(sequence_policy_id))
    }
}
