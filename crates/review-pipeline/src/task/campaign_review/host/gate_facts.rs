use super::*;
use review_core::task::campaign_review::{TASK_REVIEW_GATE_FACTS_V1, TaskReviewGateFactsV1};
use review_core::task::runtime::{TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1};

impl CampaignReviewTaskHost<'_, '_> {
    pub(super) fn capture_gate_facts(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
    ) -> Result<Vec<String>, String> {
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
        let facts_id = cas
            .put_artifact(
                TASK_REVIEW_GATE_FACTS_V1,
                self.producer(input, Some(attempt))?,
                vec![attempt.context_id().into()],
                Some(self.domain.authority.head_snapshot_id.clone()),
                serde_json::to_value(facts).map_err(|e| e.to_string())?,
            )
            .map(|v| v.0)
            .map_err(|e| e.to_string())?;
        let runtime = TaskRuntimeEvidenceV1 {
            task_id: attempt.task_id().into(),
            attempt_id: attempt.id().into(),
            node: input.node.clone(),
            context_id: attempt.context_id().into(),
            spans: self
                .domain
                .runtime_spans
                .lock()
                .expect("runtime spans")
                .get(&node.id)
                .cloned()
                .unwrap_or_default(),
            caches: self
                .domain
                .runtime_caches
                .lock()
                .expect("runtime caches")
                .get(&node.id)
                .cloned()
                .unwrap_or_default(),
        };
        if runtime.spans.is_empty() && runtime.caches.is_empty() {
            return Ok(vec![facts_id]);
        }
        runtime.validate()?;
        let runtime_id = cas
            .put_artifact(
                TASK_RUNTIME_EVIDENCE_V1,
                self.producer(input, Some(attempt))?,
                std::iter::once(attempt.context_id().to_owned())
                    .chain(runtime.spans.iter().map(|span| span.span_id.clone()))
                    .chain(runtime.caches.iter().flat_map(|cache| {
                        [cache.observation_id.clone(), cache.source_digest.clone()]
                    }))
                    .collect(),
                Some(self.domain.authority.head_snapshot_id.clone()),
                serde_json::to_value(runtime).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(vec![facts_id, runtime_id])
    }

    /// Retain one Worker Attempt's clone of a carried Build Cache as that Attempt's own
    /// `TaskRuntimeEvidence@1`, the same shape a Gate settles its checks and caches in. It is
    /// read back by `af task show` per Attempt and grants nothing: no selection, no acceptance.
    pub(super) fn retain_worker_runtime_evidence(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        evidence: crate::build_cache::BuildCacheEvidence,
    ) -> Result<String, String> {
        let runtime = TaskRuntimeEvidenceV1 {
            task_id: attempt.task_id().into(),
            attempt_id: attempt.id().into(),
            node: input.node.clone(),
            context_id: attempt.context_id().into(),
            spans: vec![evidence.span],
            caches: vec![evidence.observation],
        };
        runtime.validate()?;
        cas.put_artifact(
            TASK_RUNTIME_EVIDENCE_V1,
            self.producer(input, Some(attempt))?,
            std::iter::once(attempt.context_id().to_owned())
                .chain(runtime.spans.iter().map(|span| span.span_id.clone()))
                .chain(
                    runtime.caches.iter().flat_map(|cache| {
                        [cache.observation_id.clone(), cache.source_digest.clone()]
                    }),
                )
                .collect(),
            Some(self.domain.authority.head_snapshot_id.clone()),
            serde_json::to_value(runtime).map_err(|e| e.to_string())?,
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
                if artifact.artifact_type == TASK_RUNTIME_EVIDENCE_V1 {
                    let evidence: TaskRuntimeEvidenceV1 =
                        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                    evidence.validate()?;
                    if evidence.task_id != self.task.task_id
                        || evidence.attempt_id != attempt
                        || evidence.node != task_node
                        || artifact.producer
                            != (Producer::Attempt {
                                run_id: task_run_id(&self.task.task_id)
                                    .map_err(|e| e.to_string())?,
                                node_id: task_node.clone(),
                                attempt_id: attempt.clone(),
                            })
                    {
                        return Err(
                            "Review Gate runtime evidence differs from its Task Attempt".into()
                        );
                    }
                    continue;
                }
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
