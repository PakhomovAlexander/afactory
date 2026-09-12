//! Fixed, data-only planning on the common runtime. Compiler rejection is typed retry input;
//! it never imports process diagnostics, a parent transcript, or developer credentials.
use super::host::TaskDomain;
use super::*;
use review_config::task::catalog::TaskPlanCompiler;
use review_core::task::feedback::*;
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::planning::*;
use review_core::task::{TaskAcceptanceV1, TaskResultV1};

pub struct PlanningTaskDomain<'a> {
    pub compiler: &'a TaskPlanCompiler,
    pub task: &'a TaskRevisionV1,
    pub graph: &'a CompiledTask,
}
impl PlanningTaskDomain<'_> {
    fn is_context(&self, input: &TaskInvocationV1) -> bool {
        matches!(
            self.graph.nodes.get(&input.node).map(|n| &n.operator),
            Some(CompiledOperator::Primitive {
                operator: TaskOperatorV1::PlanningContext {},
                ..
            })
        )
    }
    fn request(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        if !self.is_context(input) || !input.inputs.is_empty() {
            return Err("Planning context requires its fixed input-free operator".into());
        }
        let id = cas
            .put_artifact(
                PLANNING_REQUEST_V1,
                super::source::invocation_producer(cas, input, None)?,
                vec![input.plan_id.clone()],
                None,
                self.compiler.planning_request(self.task)?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(BTreeMap::from([(
            "request".into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: PLANNING_REQUEST_V1.into(),
                cardinality: review_core::PortCardinality::One,
                snapshot_id: None,
            },
        )]))
    }
}
impl TaskOperatorHost for PlanningTaskDomain<'_> {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        Err("Pure planning context does not launch an Attempt".into())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        TaskWorkOutput {
            usage: None,
            outputs: if attempt.is_some() {
                Err("Planning context cannot consume an Attempt".into())
            } else {
                self.request(cas, input)
            },
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskDomain for PlanningTaskDomain<'_> {
    fn validate_context(
        &self,
        _: &Cas,
        input: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        if !matches!(self.graph.nodes.get(&input.node).map(|n| &n.operator),
            Some(CompiledOperator::Primitive { operator: TaskOperatorV1::Worker { slot }, .. })
                if self.graph.slots.get(slot).is_some_and(|s| s.role == "plan"))
        {
            return Err("Only the fixed Planner Worker receives planning context".into());
        }
        // The captured host has already reconstructed the exact Worker contract and context.
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if task != self.task || plan.preparation.is_none() {
            return Err("Planning output belongs to another Task or execution purpose".into());
        }
        if self.is_context(input) {
            if output.outputs != self.request(cas, input)? {
                return Err("Planning request changed its captured interfaces".into());
            }
            return Ok(());
        }
        let port = output
            .outputs
            .get("proposal")
            .ok_or("Planner omitted its proposal")?;
        if port.artifact_type != PIPELINE_PROPOSAL_V1 || port.artifact_ids.len() != 1 {
            return Err("Planner violated its proposal contract".into());
        }
        let proposal: PipelineProposalV1 =
            serde_json::from_value(envelope(cas, &port.artifact_ids[0])?.payload)
                .map_err(|e| e.to_string())?;
        self.compiler.check_proposal_structure(cas, task, &proposal)
    }
    fn validate_result(
        &self,
        _: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        if task != self.task || result.acceptance != TaskAcceptanceV1::Inconclusive {
            return Err("Planning cannot satisfy business acceptance".into());
        }
        result.validate()
    }
}

/// Intercepts only a Planner's proposal before publication. The inner captured host still
/// owns execution, schemas, native usage, raw artifacts and isolated Worker context.
pub struct PlanningTaskHost<'a> {
    pub inner: &'a dyn TaskDomain,
    pub validate_proposal: &'a ProposalValidator<'a>,
    pub task: &'a TaskRevisionV1,
}

pub type ProposalValidator<'a> =
    dyn Fn(&Cas, &TaskRevisionV1, &PipelineProposalV1) -> Result<(), String> + Sync + 'a;
impl TaskOperatorHost for PlanningTaskHost<'_> {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.inner.prepare_context(cas, input, feedback)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let mut returned = self.inner.execute(cas, input, attempt);
        let Some(attempt) = attempt else {
            return returned;
        };
        let Some(port) = returned
            .outputs
            .as_ref()
            .ok()
            .and_then(|o| o.get("proposal"))
        else {
            return returned;
        };
        let checked = (|| {
            if port.artifact_type != PIPELINE_PROPOSAL_V1 || port.artifact_ids.len() != 1 {
                return Err("Planner output violates its fixed public contract".to_owned());
            }
            let id = &port.artifact_ids[0];
            let proposal: PipelineProposalV1 =
                serde_json::from_value(envelope(cas, id)?.payload).map_err(|e| e.to_string())?;
            proposal.validate()?;
            if let Err(error) = (self.validate_proposal)(cas, self.task, &proposal) {
                let context: review_runner::task::TaskContext =
                    serde_json::from_value(envelope(cas, attempt.context_id())?.payload)
                        .map_err(|e| e.to_string())?;
                let diagnostic: String = error.chars().take(2048).collect();
                let feedback = TaskRetryFeedbackV1 {
                    attempt_id: attempt.id().into(),
                    contract_id: context.contract_id.clone(),
                    code: TaskFeedbackCodeV1::CompilerRejected,
                    compiler: Some(TaskCompilerFeedbackV1 {
                        proposal_id: id.clone(),
                        proposal,
                        diagnostics: vec![diagnostic],
                    }),
                };
                feedback.validate()?;
                returned.feedback_id = Some(
                    cas.put_artifact(
                        TASK_RETRY_FEEDBACK_V1,
                        super::source::invocation_producer(cas, input, Some(attempt))?,
                        vec![attempt.context_id().into(), context.contract_id, id.clone()],
                        None,
                        serde_json::to_value(feedback).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                    .0,
                );
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = checked {
            returned.outputs = Err(error);
        }
        returned
    }
}
impl TaskDomain for PlanningTaskHost<'_> {
    fn validate_retry(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        self.inner.validate_retry(cas, task, plan, input, previous)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        context: &str,
    ) -> Result<(), String> {
        self.inner.validate_context(cas, input, feedback, context)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.inner.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.inner.validate_result(cas, task, result)
    }
}
