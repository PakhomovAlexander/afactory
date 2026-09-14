//! A selected proposal is a Store capability, not a flag supplied in Pipeline text.
use super::*;
use review_core::task::planning::{PIPELINE_PROPOSAL_V1, PipelineProposalV1};

/// Constructed only from a published, selected Planner output in the common Task log.
/// Kept on the projection after the barrier so restart can reconstruct generated packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlanningProof {
    revision_id: String,
    bootstrap_plan_id: String,
    proposal_id: String,
}
impl TaskPlanningProof {
    pub fn revision_id(&self) -> &str {
        &self.revision_id
    }
    pub fn bootstrap_plan_id(&self) -> &str {
        &self.bootstrap_plan_id
    }
    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }
}

impl TaskProjection {
    /// The current preparation plan must have a durably selected public proposal. Merely
    /// writing a proposal artifact, or settling without publishing it, grants no capability.
    pub fn planning_proof(&self, cas: &Cas) -> Result<TaskPlanningProof, StoreError> {
        if let Some(proof) = &self.planning {
            cas.verify(proof.revision_id())
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            cas.verify(proof.bootstrap_plan_id())
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            cas.verify(proof.proposal_id())
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            return Ok(proof.clone());
        }
        let plan_id = self
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Task has no Planner plan"))?;
        let plan = plan(cas, plan_id, self)?;
        if plan.preparation.is_none() || !self.admitted || self.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict("Task is not running its fixed Planner bootstrap"));
        }
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Planner has no execution"))?;
        if !execution.pending_attempts().is_empty() {
            return Err(conflict("Planner still has unsettled work"));
        }
        let address = execution
            .graph
            .outputs
            .get("proposal")
            .ok_or_else(|| conflict("Planner has no public proposal port"))?;
        let node = &execution.graph.nodes[&address.node];
        let review_graph::task::CompiledOperator::Primitive {
            operator: task::pipeline::TaskOperatorV1::Worker { slot },
            ..
        } = &node.operator
        else {
            return Err(conflict("Planner proposal was not produced by a Worker"));
        };
        if execution
            .graph
            .slots
            .get(slot)
            .is_none_or(|s| s.role != "plan")
        {
            return Err(conflict("Proposal requires the captured plan-role Worker"));
        }
        let (wrapper_id, output) = execution
            .outputs
            .get(&address.node)
            .ok_or_else(|| conflict("Planner proposal has not been published"))?;
        let (selected_id, _) = execution
            .reusable_output(&address.node)
            .ok_or_else(|| conflict("Planner proposal has no selected Attempt"))?;
        if wrapper_id != &selected_id {
            return Err(conflict(
                "Planner publication differs from its selected output",
            ));
        }
        let port = output
            .outputs
            .get(&address.port)
            .ok_or_else(|| conflict("Planner omitted its public proposal"))?;
        if port.artifact_type != PIPELINE_PROPOSAL_V1
            || port.artifact_ids.len() != 1
            || port.snapshot_id.is_some()
        {
            return Err(conflict("Planner proposal has the wrong public contract"));
        }
        let proposal: PipelineProposalV1 =
            payload(cas, &port.artifact_ids[0], PIPELINE_PROPOSAL_V1)?;
        proposal.validate().map_err(conflict)?;
        Ok(TaskPlanningProof {
            revision_id: self.revision_id.clone(),
            bootstrap_plan_id: plan_id.clone(),
            proposal_id: port.artifact_ids[0].clone(),
        })
    }

    pub(super) fn apply_planning_completed(
        &mut self,
        cas: &Cas,
        bootstrap_plan_id: &str,
        proposal_id: &str,
        revision_id: &str,
        plan_id: &str,
        time: u64,
    ) -> Result<(), StoreError> {
        if self.planning.is_some() {
            return Err(conflict("Task has already crossed its planning barrier"));
        }
        let proof = self.planning_proof(cas)?;
        if proof.bootstrap_plan_id() != bootstrap_plan_id || proof.proposal_id() != proposal_id {
            return Err(conflict(
                "Planning barrier names another proposal or bootstrap",
            ));
        }
        let next = revision(cas, revision_id)?;
        let mut expected = self.revision.clone();
        expected.revision = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| conflict("Task revision overflow"))?;
        expected.previous_revision_id = Some(self.revision_id.clone());
        // The trusted authority recomputes constructors before append. Replay additionally
        // preserves every original input and requires provenance for every added artifact.
        for (name, value) in &next.inputs {
            if !expected.inputs.contains_key(name) {
                expected.inputs.insert(name.clone(), value.clone());
                expected
                    .provenance
                    .input_artifact_ids
                    .extend(value.artifact_ids.clone());
            }
        }
        expected.provenance.input_artifact_ids.sort();
        expected.provenance.input_artifact_ids.dedup();
        if next != expected {
            return Err(conflict(
                "Planning changed the business Task, existing inputs, authority or original limits",
            ));
        }
        let mut binding = self.clone();
        binding.revision = next.clone();
        binding.revision_id = revision_id.into();
        let plan = plan(cas, plan_id, &binding)?;
        if plan.preparation.is_some()
            || plan.generated_origins.is_empty()
            || !plan
                .generated_origins
                .iter()
                .any(|o| o.pipeline_id == plan.pipeline_id)
            || plan
                .generated_origins
                .iter()
                .any(|o| o.proposal_id != proposal_id || o.bootstrap_plan_id != bootstrap_plan_id)
        {
            return Err(conflict(
                "Execution plan does not retain its exact generated proposal closure",
            ));
        }
        let graph: review_graph::task::CompiledTask =
            payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
        if graph.inputs != next.inputs
            || graph.schema != "af.compiled-task/1"
            || graph.scheduler_plan().map_err(conflict)?.order != graph.order
        {
            return Err(conflict("Generated execution graph is not canonical"));
        }
        self.execution
            .as_mut()
            .ok_or_else(|| conflict("Planner has no accounting"))?
            .enter_execution(graph, time)?;
        self.revision = next;
        self.revision_id = revision_id.into();
        self.plan_id = Some(plan_id.into());
        self.planning = Some(proof);
        self.admitted = false;
        self.phase = TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsPlanReview,
        };
        self.resume_phase = None;
        Ok(())
    }
}

impl EventStore {
    pub fn complete_task_planning(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        revision_id: &str,
        plan_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let proof = state.planning_proof(cas)?;
        let mut next = state.clone();
        next.revision = revision(cas, revision_id)?;
        next.revision_id = revision_id.into();
        let plan = self.authorized_plan(cas, &next, plan_id, authority)?;
        authority
            .validate_planning_inputs(cas, &state.revision, &next.revision, &plan)
            .map_err(conflict)?;
        self.task_change(
            cas,
            lease,
            TaskChangeV1::PlanningCompleted {
                bootstrap_plan_id: proof.bootstrap_plan_id,
                proposal_id: proof.proposal_id,
                revision_id: revision_id.into(),
                plan_id: plan_id.into(),
            },
            now()?,
        )
    }
}
