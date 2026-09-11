//! Recompute independent repair receipts before projecting them into a later discovery Round.
//! No legacy resolution artifact or Campaign event is manufactured by this bridge.
use super::*;
use review_core::task::repair::*;

pub(super) fn signature() -> OperatorSignature {
    let inputs = BTreeMap::from([
        (
            "source".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
        ),
        ("repair".into(), port(TASK_REPAIR_CONTEXT_V1, same(), false)),
        ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same(), false)),
        (
            "verification".into(),
            port(TASK_FIX_VERIFICATION_V1, same(), true),
        ),
    ]);
    OperatorSignature {
        retains: BTreeMap::from([("continuation".into(), inputs.keys().cloned().collect())]),
        contract: PipelineContractV1 {
            inputs,
            outputs: BTreeMap::from([(
                "continuation".into(),
                port(TASK_REVIEW_CONTINUATION_V1, same(), false),
            )]),
        },
        effects: BTreeSet::new(),
        evidence: BTreeMap::new(),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        attempt: None,
        outcome_port: None,
    }
}

impl ReviewTaskDomain {
    fn continuation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewContinuationV1, String> {
        let context = self.current_repair(cas, input)?;
        let original_round_report_id = context
            .invocation
            .inputs
            .get("review")
            .ok_or("Review continuation has no original Round")?
            .artifact_ids[0]
            .clone();
        let (_, original) = self.restore_round(cas, &original_round_report_id, 0)?;
        if original.round >= self.policy.max_rounds {
            return Err("Captured Review policy permits no further discovery Round".into());
        }
        let (_, assessment) = self.repair_acceptance(cas, input)?;
        let value = TaskReviewContinuationV1 {
            invocation: input.clone(),
            original_round_report_id,
            prior_history_id: context.continuation.prior_history_id,
            assessment,
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn continue_review(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let value = self.continuation(cas, input)?;
        let refs = value
            .assessment
            .claims
            .values()
            .map(|c| c.receipt_id.clone())
            .chain([
                value.original_round_report_id.clone(),
                value.prior_history_id.clone(),
            ])
            .collect();
        let output = self.put(
            cas,
            input,
            TASK_REVIEW_CONTINUATION_V1,
            Some(&value.assessment.current_snapshot_id),
            &value,
            refs,
        )?;
        Ok(BTreeMap::from([("continuation".into(), output)]))
    }

    pub(super) fn restore_continuation(
        &self,
        cas: &Cas,
        id: &str,
        subject: &TaskReviewSubjectV1,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewContinuationV1, String> {
        let artifact = envelope(cas, id)?;
        let value: TaskReviewContinuationV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        value.validate()?;
        if artifact.artifact_type != TASK_REVIEW_CONTINUATION_V1
            || artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
            || artifact.producer != invocation_producer(cas, &value.invocation, None)?
            || value.invocation.plan_id != input.plan_id
            || !matches!(
                self.operator(&value.invocation)?,
                TaskOperatorV1::ReviewContinue {}
            )
            || value.prior_history_id != subject.prior_history_id
            || value.assessment.current_subject_id != subject.subject_id
            || value.assessment.current_snapshot_id != subject.snapshot_id
            || self.continuation(cas, &value.invocation)? != value
        {
            return Err("Review continuation changed its exact plan, history, Subject or independent evidence".into());
        }
        let (_, original) = self.restore_round(cas, &value.original_round_report_id, 0)?;
        if original.round + 1 != subject.round {
            return Err("Review continuation must enter the next bounded discovery Round".into());
        }
        Ok(value)
    }
}
