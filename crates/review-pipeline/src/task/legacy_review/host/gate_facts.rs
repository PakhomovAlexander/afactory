use super::*;
use review_core::task::review_compat::{TASK_REVIEW_GATE_FACTS_V1, TaskReviewGateFactsV1};

impl LegacyReviewTaskHost<'_, '_> {
    pub(super) fn capture_gate_facts(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
    ) -> Result<String, String> {
        let (node, _, operation) = self.operation(input)?.ok_or("Not a Review Gate")?;
        if !matches!(operation, ReviewOperation::Gate) {
            return Err("Not a Review Gate".into());
        }
        let facts = TaskReviewGateFactsV1 {
            round_event_id: self.domain.authority.round_event_id.clone(),
            review_node: node.id.clone(),
            attempt_id: attempt.id().into(),
            cache_failures: self
                .domain
                .cache_failures
                .lock()
                .expect("Review cache failures")
                .values()
                .filter(|failure| failure.node == node.id)
                .cloned()
                .collect(),
        };
        facts.validate()?;
        cas.put_artifact(
            TASK_REVIEW_GATE_FACTS_V1,
            self.producer(input, Some(attempt))?,
            vec![attempt.context_id().into()],
            Some(self.domain.authority.head_snapshot_id.clone()),
            serde_json::to_value(facts).map_err(|e| e.to_string())?,
        )
        .map(|v| v.0)
        .map_err(|e| e.to_string())
    }

    pub(super) fn restore_gate_facts(
        &self,
        cas: &Cas,
        execution: &review_store::store::task::execution::TaskExecutionProjection,
    ) -> Result<(), String> {
        for (attempt, (task_node, ids)) in execution.settled_artifacts() {
            let Some(node) = self.captured.compilation.graph.nodes.get(&task_node) else {
                continue;
            };
            let CompiledOperator::ReviewDomain {
                review_node,
                operation: ReviewOperation::Gate,
            } = &node.operator
            else {
                continue;
            };
            // Gate observations are installed typed facts. Other Workers' raw responses never
            // enter this decoder, even if their JSON claims to be a Gate fact or producer.
            for id in ids {
                let artifact = cas.get_artifact(&id).map_err(|e| e.to_string())?;
                if artifact.artifact_type != TASK_REVIEW_GATE_FACTS_V1 {
                    return Err("Review Gate settlement contains unsupported observations".into());
                }
                let facts: TaskReviewGateFactsV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                facts.validate()?;
                if facts.round_event_id != self.domain.authority.round_event_id {
                    continue;
                }
                if facts.review_node != *review_node
                    || facts.attempt_id != attempt
                    || artifact.producer
                        != (Producer::Attempt {
                            run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                            node_id: task_node.clone(),
                            attempt_id: attempt.clone(),
                        })
                    || artifact.subject_snapshot_id.as_deref()
                        != Some(&self.domain.authority.head_snapshot_id)
                {
                    return Err(
                        "Review Gate facts differ from their settled Attempt and Subject".into(),
                    );
                }
                for reference in artifact.input_artifacts {
                    cas.verify(&reference).map_err(|e| e.to_string())?;
                }
                for failure in facts.cache_failures {
                    if self
                        .captured
                        .loaded
                        .gate_execution()
                        .is_none_or(|policy| policy.caches.is_empty())
                    {
                        return Err("Review Gate retained an undeclared cache failure".into());
                    }
                    self.domain
                        .cache_failures
                        .lock()
                        .expect("Review cache failures")
                        .insert((review_node.clone(), failure.kind), failure);
                }
            }
        }
        Ok(())
    }
}
