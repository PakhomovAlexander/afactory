//! Captured command Workers behind the common Task dispatcher. Environment and domain
//! extensions provide operations and validation, never scheduling or child accounting.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_config::task::catalog::{TaskPlanCompiler, TaskWorkerRunner};
use review_core::Producer;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::{PortAffinityV1, TaskOperatorV1};
use review_core::task::plan::{
    EffectiveWorkerBindingV1, ExecutionPlanV1, GeneratedOriginV1, PlanDecisionKindV1,
    PlanDecisionV1, WorkerExecutionV1,
};
use review_core::task::{ArtifactInputV1, TaskResultV1, TaskRevisionV1};
use review_graph::task::{CompiledOperator, CompiledTask, OperatorSignature};
use review_runner::task::WorkerContract;
use review_sandbox::{Mode, Sandbox};
use review_store::Cas;
use review_store::store::task::{DeveloperGrant, TaskAuthority, task_run_id};

use super::{TaskOperatorHost, TaskWorkOutput, envelope};
use review_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};

/// Installed host policy chooses the actual environment; a Pipeline cannot assert isolation.
pub trait TaskEnvironment: Sync {
    /// Additional outputs computed by the installed environment, such as a fully captured
    /// candidate tree. A Worker reply cannot fill these ports itself.
    fn kernel_outputs(&self, _signature: &OperatorSignature) -> BTreeSet<String> {
        BTreeSet::new()
    }
    fn validate_outputs(
        &self,
        _cas: &Cas,
        _input: &TaskInvocationV1,
        _signature: &OperatorSignature,
        _output: &TaskOutputV1,
    ) -> Result<(), String> {
        Ok(())
    }
    fn materialize(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
    ) -> Result<Sandbox, String>;
    /// The host supplies the already captured transport class so an installed environment can
    /// refuse command-only preparation for model Workers. This does not expose runner settings
    /// to package-controlled input and cannot change the selected Worker.
    fn materialize_worker(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
        _command_worker: bool,
    ) -> Result<Sandbox, String> {
        self.materialize(cas, invocation, signature)
    }
    /// Extra variables are derived only from installed environment preparation. Worker packages
    /// cannot name host paths or widen this set.
    fn command_environment(
        &self,
        _input: &TaskInvocationV1,
        _sandbox: &Sandbox,
    ) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
    /// Optional AF-owned runtime evidence produced by environment preparation for this exact
    /// Attempt. The host retains it beside process evidence before settlement.
    fn runtime_evidence(
        &self,
        _cas: &Cas,
        _input: &TaskInvocationV1,
        _attempt: &PreparedTaskAttempt,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn finish(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        signature: &OperatorSignature,
        attempt: &PreparedTaskAttempt,
        sandbox: Sandbox,
        outputs: BTreeMap<String, ArtifactInputV1>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String>;
}

/// Deterministic data-only Workers start in a fresh empty directory. They receive their
/// declared inputs on stdin and cannot return filesystem edits as an admitted artifact.
pub struct EmptyTaskEnvironment;

/// Data-only execution with the project's captured isolation requirement. No source tree is
/// synthesized for a document Task, and emitted content must use its typed reply contract.
pub struct DataTaskEnvironment {
    pub policy: review_sandbox::Policy,
}
impl TaskEnvironment for DataTaskEnvironment {
    fn materialize(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        signature: &OperatorSignature,
    ) -> Result<Sandbox, String> {
        let sandbox = EmptyTaskEnvironment.materialize(cas, input, signature)?;
        review_sandbox::admit(self.policy, &sandbox).map_err(|e| e.to_string())?;
        Ok(sandbox)
    }
    fn finish(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        signature: &OperatorSignature,
        attempt: &PreparedTaskAttempt,
        sandbox: Sandbox,
        outputs: BTreeMap<String, ArtifactInputV1>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        EmptyTaskEnvironment.finish(cas, input, signature, attempt, sandbox, outputs)
    }
}
impl TaskEnvironment for EmptyTaskEnvironment {
    fn materialize(
        &self,
        cas: &Cas,
        _: &TaskInvocationV1,
        signature: &OperatorSignature,
    ) -> Result<Sandbox, String> {
        if !signature.effects.is_empty() {
            return Err(
                "Data-only environment does not admit filesystem or network effects".into(),
            );
        }
        let manifest = review_source_git::Manifest::new(vec![]).map_err(|e| e.to_string())?;
        Sandbox::materialize(&manifest, cas, Mode::EphemeralWrite).map_err(|e| e.to_string())
    }
    fn finish(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &OperatorSignature,
        _: &PreparedTaskAttempt,
        sandbox: Sandbox,
        outputs: BTreeMap<String, ArtifactInputV1>,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if !sealed.unchanged() {
            return Err(format!(
                "Data-only Worker wrote undeclared files: {:?}",
                sealed.mutations.paths()
            ));
        }
        Ok(outputs)
    }
}

/// Domain operators retain their own receipt semantics. This interface cannot create an
/// Attempt, execute a child graph, authorize a plan, or change the parent's allowance.
pub trait TaskDomain: TaskOperatorHost {
    fn validate_experiment_preparation(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _prepared_id: &str,
        _prepared: &review_core::task::optimization_experiment::ExperimentPreparedV1,
    ) -> Result<(), String> {
        Err("Task domain has no installed experimental compiler".into())
    }
    fn validate_review_integration_selection(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        _evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        Err("Task domain has no installed Review Integration".into())
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_review_integration_completion(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        _report: &review_core::task::report::TaskRunReportV2,
        _events: &[review_store::NewEvent],
        _evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        Err("Task domain has no installed Review Integration".into())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_review_continuation(
        &self,
        _cas: &Cas,
        _previous: &TaskRevisionV1,
        _next: &TaskRevisionV1,
        _previous_plan: &ExecutionPlanV1,
        _next_plan: &ExecutionPlanV1,
        _handoff: &review_core::task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        Err("Task domain has no installed Review continuation".into())
    }
    fn validate_owned_children(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _parent: &TaskInvocationV1,
        _children: &review_core::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        Err("Task domain has no installed child admission".into())
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_owned_completion(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _parent: &TaskInvocationV1,
        _children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        _facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
        _output: &TaskOutputV1,
    ) -> Result<(), String> {
        Err("Task domain has no installed child completion".into())
    }

    /// Re-derive Broker authority from installed domain policy and exact captured inputs.
    /// A serialized Worker binding alone does not install an external capability.
    fn validate_broker_binding(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _binding: &review_core::task::broker::TaskBrokerBindingV1,
    ) -> Result<(), String> {
        Err("Task domain has no installed Broker authority".into())
    }

    fn validate_retry(
        &self,
        _cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlanV1,
        _input: &TaskInvocationV1,
        _previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Domain-specific conclusion assembled from this Task's common execution projection.
    fn assemble_result(
        &self,
        _cas: &Cas,
        _state: &review_store::store::task::TaskProjection,
        _report: &review_graph::RunReport,
    ) -> Result<TaskResultV1, String> {
        Err("This Task domain has no installed final-result assembler".into())
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        context_id: &str,
    ) -> Result<(), String>;
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        context_id: &str,
    ) -> Result<(), String> {
        self.validate_context(cas, input, attempt.feedback_ids(), context_id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String>;
    fn validate_resolved_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        self.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String>;
}

/// Supplied by the trusted host's developer session. Worker identity and serialized decision
/// JSON never implement this capability. Model-containing environments must isolate this host.
pub trait TaskDeveloper: Sync {
    fn decide(
        &self,
        task: &TaskRevisionV1,
        plan_id: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String>;
    fn current(&self, decision: &PlanDecisionV1) -> Result<(), String>;
    fn experiment_current(
        &self,
        decision: &review_core::task::optimization_experiment::ExperimentPlanDecisionV1,
    ) -> Result<(), String>;
}

pub struct NoTaskDeveloper;
impl TaskDeveloper for NoTaskDeveloper {
    fn decide(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("This execution process has no developer decision capability".into())
    }
    fn current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        Err("This host cannot authenticate a recorded developer decision".into())
    }
    fn experiment_current(
        &self,
        _: &review_core::task::optimization_experiment::ExperimentPlanDecisionV1,
    ) -> Result<(), String> {
        Err("This host cannot authenticate an experimental developer decision".into())
    }
}

pub struct CapturedTaskAuthority<'a> {
    compiler: CapturedCompiler<'a>,
    domain: &'a dyn TaskDomain,
    developer: &'a dyn TaskDeveloper,
}

enum CapturedCompiler<'a> {
    Task(Box<review_config::task::catalog::CapturedTaskPlanValidator<'a>>),
    Review(&'a super::legacy_review::plan::LegacyReviewPlanCompiler),
}

impl<'a> CapturedTaskAuthority<'a> {
    pub fn new(
        compiler: &'a TaskPlanCompiler,
        domain: &'a dyn TaskDomain,
        developer: &'a dyn TaskDeveloper,
    ) -> Self {
        Self {
            compiler: CapturedCompiler::Task(Box::new(
                review_config::task::catalog::CapturedTaskPlanValidator::new(compiler),
            )),
            domain,
            developer,
        }
    }

    pub fn for_legacy_review(
        compiler: &'a super::legacy_review::plan::LegacyReviewPlanCompiler,
        domain: &'a dyn TaskDomain,
        developer: &'a dyn TaskDeveloper,
    ) -> Self {
        Self {
            compiler: CapturedCompiler::Review(compiler),
            domain,
            developer,
        }
    }
}

impl TaskAuthority for CapturedTaskAuthority<'_> {
    fn validate_experiment_preparation(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        prepared_id: &str,
        prepared: &review_core::task::optimization_experiment::ExperimentPreparedV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_experiment_preparation(cas, task, plan, prepared_id, prepared)
    }

    fn experiment_authorization_current(
        &self,
        decision: &review_core::task::optimization_experiment::ExperimentPlanDecisionV1,
    ) -> Result<(), String> {
        self.developer.experiment_current(decision)
    }

    fn validate_resolved_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_resolved_output(cas, task, plan, input, output, definition)
    }

    fn validate_review_integration_selection(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        if !matches!(self.compiler, CapturedCompiler::Review(_)) {
            return Err("Only captured Review admits an Integration phase".into());
        }
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_review_integration_selection(cas, task, plan, phase, evidence)
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_review_integration_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        report: &review_core::task::report::TaskRunReportV2,
        events: &[review_store::NewEvent],
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        if !matches!(self.compiler, CapturedCompiler::Review(_)) {
            return Err("Only captured Review admits an Integration phase".into());
        }
        self.validate_plan(cas, task, plan)?;
        self.domain.validate_review_integration_completion(
            cas, task, plan, phase, report, events, evidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_review_continuation(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        previous_plan: &ExecutionPlanV1,
        next_plan: &ExecutionPlanV1,
        handoff: &review_core::task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        if !matches!(self.compiler, CapturedCompiler::Review(_)) {
            return Err("Only the captured Review compiler admits a Review continuation".into());
        }
        self.validate_plan(cas, next, next_plan)?;
        self.domain.validate_review_continuation(
            cas,
            previous,
            next,
            previous_plan,
            next_plan,
            handoff,
        )
    }
    fn validate_owned_children(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_owned_children(cas, task, plan, parent, children)
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_owned_completion(cas, task, plan, parent, children, facts, output)
    }

    fn validate_broker_binding(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &review_core::task::broker::TaskBrokerBindingV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        self.domain
            .validate_broker_binding(cas, task, plan, binding)
    }

    fn validate_retry(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        self.domain.validate_retry(cas, task, plan, input, previous)
    }
    fn validate_planning_inputs(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<(), String> {
        match &self.compiler {
            CapturedCompiler::Task(compiler) => {
                compiler.validate_planning_inputs(cas, previous, next, plan)
            }
            CapturedCompiler::Review(_) => {
                Err("Installed Review plans do not normalize generated planning inputs".into())
            }
        }
    }
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        match &self.compiler {
            CapturedCompiler::Task(compiler) => {
                compiler.validate_plan(cas, task, plan)?;
                Ok(plan.generated_origins.clone())
            }
            CapturedCompiler::Review(compiler) => compiler.validate_plan(cas, task, plan),
        }
    }
    fn authorize_decision(
        &self,
        task: &TaskRevisionV1,
        id: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        self.developer.decide(task, id, decision)
    }
    fn authorization_current(&self, decision: &PlanDecisionV1) -> Result<(), String> {
        self.developer.current(decision)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        feedback: &[String],
        id: &str,
    ) -> Result<(), String> {
        self.domain.validate_context(cas, input, feedback, id)
    }
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        self.domain
            .validate_context_for_attempt(cas, input, attempt, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        validate_worker_notes_outputs(cas, input, output)?;
        self.domain.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.domain.validate_result(cas, task, result)
    }
}

#[derive(Clone)]
enum WorkerTransport<'a> {
    Command(review_core::Command),
    Model(&'a dyn review_runner::task::WorkerModelAdapter),
}

/// Installed by the host after Provider admission. The complete effective binding must equal
/// the plan's captured slot; a local alias cannot substitute another principal or model.
pub struct TaskModelBinding<'a> {
    pub binding: EffectiveWorkerBindingV1,
    pub adapter: &'a dyn review_runner::task::WorkerModelAdapter,
}

struct CapturedWorker<'a> {
    package: String,
    transport: WorkerTransport<'a>,
    files: BTreeMap<String, Vec<u8>>,
    instructions: String,
    signature: OperatorSignature,
    contract: std::sync::Arc<WorkerContract>,
}

pub struct CapturedTaskHost<'a> {
    run_id: String,
    graph: CompiledTask,
    workers: BTreeMap<String, std::sync::Arc<CapturedWorker<'a>>>,
    slot_workers: BTreeMap<String, std::sync::Arc<CapturedWorker<'a>>>,
    derived_workers: std::sync::Mutex<BTreeMap<String, std::sync::Arc<CapturedWorker<'a>>>>,
    /// Contexts prepared from a resolved static or registered child definition. The common
    /// Store callback does not carry that definition, so retain its exact result under the
    /// reservation identity and require the same content ID at Attempt admission.
    resolved_contexts: std::sync::Mutex<BTreeMap<String, String>>,
    environment: &'a dyn TaskEnvironment,
    domain: &'a dyn TaskDomain,
}

pub type CommandTaskHost<'a> = CapturedTaskHost<'a>;

/// Durable admission and replay recheck of every `af/WorkerNotes@1` output: the stored payload
/// must be a closed `WorkerNotesV1` bound to the producing Attempt, this node and the head the
/// output port names. Worker JSON never establishes that identity on its own.
fn validate_worker_notes_outputs(
    cas: &Cas,
    input: &TaskInvocationV1,
    output: &TaskOutputV1,
) -> Result<(), String> {
    for value in output.outputs.values() {
        if value.artifact_type != review_core::task::WORKER_NOTES_V1 {
            continue;
        }
        for id in &value.artifact_ids {
            let stored = envelope(cas, id)?;
            if stored.artifact_type != review_core::task::WORKER_NOTES_V1 {
                return Err("Worker Notes output names an artifact of another type".into());
            }
            let Producer::Attempt {
                node_id,
                attempt_id,
                ..
            } = &stored.producer
            else {
                return Err("Worker Notes must be produced by an Attempt".into());
            };
            if node_id != &input.node {
                return Err("Worker Notes were produced by another node".into());
            }
            let head = stored
                .subject_snapshot_id
                .as_deref()
                .ok_or("Worker Notes artifact names no head Snapshot")?;
            let notes: review_core::WorkerNotesV1 = serde_json::from_value(stored.payload)
                .map_err(|error| format!("stored Worker Notes are not WorkerNotes@1: {error}"))?;
            notes.check_bound(&input.node, attempt_id, head)?;
        }
    }
    Ok(())
}

impl<'a> CapturedTaskHost<'a> {
    fn experimental_worker(&self, node: &str) -> Option<&CapturedWorker<'a>> {
        self.graph.nodes.iter().find_map(|(parent, definition)| {
            let CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::OptimizationExperiment {
                        baseline_slot,
                        candidate_slot,
                    },
                ..
            } = &definition.operator
            else {
                return None;
            };
            let slot = if node == format!("{parent}.baseline") {
                baseline_slot
            } else if node == format!("{parent}.candidate") {
                candidate_slot
            } else {
                return None;
            };
            self.slot_workers.get(slot).map(std::sync::Arc::as_ref)
        })
    }

    fn resolved_worker(
        &self,
        cas: &Cas,
        definition: &review_graph::task::CompiledNode,
    ) -> Result<Option<std::sync::Arc<CapturedWorker<'a>>>, String> {
        let CompiledOperator::Primitive {
            operator,
            signature,
        } = &definition.operator
        else {
            return Ok(None);
        };
        let slot = match operator {
            TaskOperatorV1::Worker { slot }
            | TaskOperatorV1::Verify { slot }
            | TaskOperatorV1::FixVerify { slot } => slot,
            _ => return Ok(None),
        };
        let original = self
            .slot_workers
            .get(slot)
            .cloned()
            .ok_or("Resolved Worker slot has no captured package")?;
        if definition.contract != original.signature.contract {
            return Err("Resolved Worker changed its captured contract".into());
        }
        if signature == &format!("worker/{}", original.package) {
            return Ok(Some(original));
        }
        let Some(derived_id) = signature.strip_prefix("worker-derived/") else {
            return Ok(None);
        };
        if !review_core::is_digest(derived_id) {
            return Err("Derived Worker signature has no exact package identity".into());
        }
        if let Some(worker) = self
            .derived_workers
            .lock()
            .expect("derived Worker packages")
            .get(derived_id)
            .cloned()
        {
            return Ok(Some(worker));
        }
        let derived = TaskPlanCompiler::captured_worker_package(cas, derived_id)?;
        let mut original_files = original.files.clone();
        let mut derived_files = derived.files.clone();
        let original_instructions = original_files.remove("instructions.md");
        let derived_instructions = derived_files.remove("instructions.md");
        if derived.name != original.package
            || derived.worker.signature != original.signature
            || original_files != derived_files
            || original_instructions.as_deref() == derived_instructions.as_deref()
        {
            return Err("Derived Worker changed authority outside instructions.md".into());
        }
        let instructions =
            String::from_utf8(derived_instructions.ok_or("Derived Worker lost instructions.md")?)
                .map_err(|error| error.to_string())?;
        let worker = std::sync::Arc::new(CapturedWorker {
            package: derived.name,
            transport: original.transport.clone(),
            files: derived.files,
            instructions,
            signature: original.signature.clone(),
            contract: original.contract.clone(),
        });
        self.derived_workers
            .lock()
            .expect("derived Worker packages")
            .insert(derived_id.into(), worker.clone());
        Ok(Some(worker))
    }

    fn validate_worker_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        worker: &CapturedWorker<'_>,
    ) -> Result<(), String> {
        self.environment
            .validate_outputs(cas, input, &worker.signature, output)?;
        let kernel_outputs = self.environment.kernel_outputs(&worker.signature);
        let mut values = BTreeMap::new();
        for (port, value) in &output.outputs {
            if kernel_outputs.contains(port) {
                continue;
            }
            let mut payloads = Vec::new();
            for id in &value.artifact_ids {
                let artifact = envelope(cas, id)?;
                for retained_port in worker.signature.retains.get(port).into_iter().flatten() {
                    let Some(retained) = input.inputs.get(retained_port) else {
                        if worker
                            .signature
                            .contract
                            .inputs
                            .get(retained_port)
                            .is_some_and(|port| port.optional)
                        {
                            continue;
                        }
                        return Err("Retained receipt input is absent".into());
                    };
                    for expected in &retained.artifact_ids {
                        if !artifact.input_artifacts.contains(expected) {
                            return Err("Worker output lost a retained input receipt".into());
                        }
                    }
                }
                payloads.push(artifact.payload);
            }
            values.insert(port.clone(), payloads);
        }
        worker.contract.validate_reply(
            &serde_json::to_vec(&review_runner::task::WorkerReply {
                schema: "af.worker-reply/1".into(),
                outputs: values,
            })
            .map_err(|e| e.to_string())?,
        )?;
        Ok(())
    }

    pub fn capture(
        cas: &Cas,
        compiler: &TaskPlanCompiler,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        graph: CompiledTask,
        environment: &'a dyn TaskEnvironment,
        domain: &'a dyn TaskDomain,
    ) -> Result<Self, String> {
        Self::capture_with_models(
            cas,
            compiler,
            task,
            plan,
            graph,
            environment,
            domain,
            &BTreeMap::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn capture_with_models(
        cas: &Cas,
        compiler: &TaskPlanCompiler,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        graph: CompiledTask,
        environment: &'a dyn TaskEnvironment,
        domain: &'a dyn TaskDomain,
        models: &BTreeMap<String, TaskModelBinding<'a>>,
    ) -> Result<Self, String> {
        compiler.validate_plan(cas, task, plan)?;
        if serde_json::to_value(&graph).map_err(|e| e.to_string())?
            != envelope(cas, &plan.compiled_graph_id)?.payload
        {
            return Err("Task host received a different compiled graph".into());
        }
        let mut workers = BTreeMap::new();
        let mut slot_workers = BTreeMap::new();
        let capture_worker = |slot: &str| -> Result<std::sync::Arc<CapturedWorker<'a>>, String> {
            let name = &graph.slots[slot].worker;
            let manifest = compiler.worker(name).ok_or("Missing captured Worker")?;
            let transport = match &manifest.runner {
                TaskWorkerRunner::Command { command } => WorkerTransport::Command(command.build()),
                TaskWorkerRunner::Model { provider_kind, .. } => {
                    let model = models
                        .get(slot)
                        .ok_or("Model Worker requires its admitted Provider adapter")?;
                    if plan.bindings.get(slot) != Some(&model.binding)
                        || !matches!(&model.binding.execution, WorkerExecutionV1::Model {provider_kind:kind, model:model_id, effort, ..} if kind == provider_kind && kind == model.adapter.provider_kind() && model.adapter.model_settings().as_ref() == Some(&(model_id.clone(), effort.clone())))
                    {
                        return Err(
                            "Model adapter differs from the exact admitted Worker binding".into(),
                        );
                    }
                    WorkerTransport::Model(model.adapter)
                }
            };
            let files = compiler
                .package_files(name)
                .ok_or("Missing captured Worker files")?;
            let contract = compiler.worker_contract(
                cas,
                name,
                &environment.kernel_outputs(&manifest.signature),
            )?;
            let instructions =
                String::from_utf8(files.get("instructions.md").cloned().unwrap_or_default())
                    .map_err(|e| e.to_string())?;
            Ok(std::sync::Arc::new(CapturedWorker {
                package: name.clone(),
                transport,
                files: files.clone(),
                instructions,
                signature: manifest.signature.clone(),
                contract: std::sync::Arc::new(contract),
            }))
        };
        for (id, node) in &graph.nodes {
            let CompiledOperator::Primitive { operator, .. } = &node.operator else {
                continue;
            };
            let slot = match operator {
                TaskOperatorV1::Worker { slot }
                | TaskOperatorV1::Verify { slot }
                | TaskOperatorV1::FixVerify { slot } => slot,
                _ => continue,
            };
            let worker = capture_worker(slot)?;
            workers.insert(id.clone(), worker.clone());
            slot_workers.insert(slot.clone(), worker);
        }
        // Experimental slots are executable authority even though their coordinator is not a
        // static Worker node. Capture these exact Workers now so registered children resolve
        // through the same command/model validation path as ordinary nodes.
        let experimental_workers = graph.nodes.values().flat_map(|node| match &node.operator {
            CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::OptimizationExperiment {
                        baseline_slot,
                        candidate_slot,
                    },
                ..
            } => vec![baseline_slot, candidate_slot],
            _ => vec![],
        });
        for slot in experimental_workers {
            if !slot_workers.contains_key(slot) {
                slot_workers.insert(slot.clone(), capture_worker(slot)?);
            }
        }
        Ok(Self {
            run_id: task_run_id(&task.task_id).map_err(|e| e.to_string())?,
            graph,
            workers,
            slot_workers,
            derived_workers: std::sync::Mutex::new(BTreeMap::new()),
            resolved_contexts: std::sync::Mutex::new(BTreeMap::new()),
            environment,
            domain,
        })
    }

    fn worker_feedback(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        worker: &CapturedWorker<'_>,
        code: review_core::task::feedback::TaskFeedbackCodeV1,
    ) -> Result<String, String> {
        use review_core::task::feedback::*;
        let (context, _) = worker.contract.read_context(cas, attempt.context_id())?;
        if context.invocation != *input || attempt.node() != input.node {
            return Err("Retry feedback belongs to another invocation".into());
        }
        let feedback = TaskRetryFeedbackV1 {
            attempt_id: attempt.id().into(),
            contract_id: worker.contract.id().into(),
            code,
            compiler: None,
        };
        feedback.validate()?;
        cas.put_artifact(
            TASK_RETRY_FEEDBACK_V1,
            Producer::Attempt {
                run_id: self.run_id.clone(),
                node_id: input.node.clone(),
                attempt_id: attempt.id().into(),
            },
            vec![attempt.context_id().into(), worker.contract.id().into()],
            None,
            serde_json::to_value(feedback).map_err(|e| e.to_string())?,
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }

    fn worker_execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        worker: &CapturedWorker<'_>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        use review_core::task::feedback::*;
        let mut raw_artifact_ids = Vec::new();
        let mut charged_tokens = Some(0);
        let mut token_usage = None;
        let mut usage_observation = None;
        let mut feedback_code = None;
        let outputs = (|| {
            let brokered = match &worker.transport {
                WorkerTransport::Command(_) => false,
                WorkerTransport::Model(adapter) => {
                    adapter.credential_mode() == review_core::BrokerCredentialModeV1::Brokered
                }
            };
            if brokered != broker.is_some() {
                return Err("Worker transport differs from its runtime Broker capability".into());
            }
            let (context, _) = worker.contract.read_context(cas, attempt.context_id())?;
            if context.invocation != *input {
                return Err("Worker context belongs to another invocation".into());
            }
            if matches!(worker.transport, WorkerTransport::Model(_))
                && context.manifest.estimated_tokens > attempt.reservation().tokens
            {
                return Err(
                    "Rendered model context exceeds its admitted Attempt token reservation".into(),
                );
            }
            let sandbox = self.environment.materialize_worker(
                cas,
                input,
                &worker.signature,
                matches!(worker.transport, WorkerTransport::Command(_)),
            )?;
            let command_environment = self.environment.command_environment(input, &sandbox)?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64;
            let remaining = attempt
                .reservation()
                .deadline_unix_ms
                .checked_sub(now)
                .filter(|ms| *ms > 0)
                .ok_or("Worker deadline expired before process start")?;
            let result = match &worker.transport {
                WorkerTransport::Command(command) => {
                    let package = tempfile::tempdir().map_err(|e| e.to_string())?;
                    for (path, bytes) in &worker.files {
                        let path = package.path().join(path);
                        std::fs::create_dir_all(path.parent().ok_or("Worker file has no parent")?)
                            .map_err(|e| e.to_string())?;
                        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                    }
                    let mut command = command.clone();
                    for arg in &mut command.args {
                        if let Some(path) = arg.value.strip_prefix("@package/") {
                            if !worker.files.contains_key(path) {
                                return Err(
                                    "Worker command references an uncaptured package file".into()
                                );
                            }
                            arg.value = package
                                .path()
                                .join(path)
                                .to_str()
                                .ok_or("Worker path is not UTF-8")?
                                .into();
                        }
                    }
                    review_runner::task::invoke_command_controlled_with_environment(
                        cas,
                        sandbox.root(),
                        tempfile::tempdir().map_err(|e| e.to_string())?.path(),
                        &command,
                        &worker.contract,
                        attempt.context_id(),
                        Duration::from_millis(remaining),
                        cancellation,
                        &command_environment,
                    )
                }
                WorkerTransport::Model(adapter) => review_runner::task::invoke_model_controlled(
                    cas,
                    sandbox.root(),
                    *adapter,
                    &worker.contract,
                    attempt.context_id(),
                    Duration::from_millis(remaining),
                    worker.signature.effects.contains("write-source"),
                    broker,
                    cancellation,
                ),
            };
            raw_artifact_ids = result.raw_artifact_ids;
            feedback_code = result.feedback_code;
            charged_tokens = result
                .usage
                .as_ref()
                .map(|usage| usage.chargeable_tokens.get());
            token_usage = result.usage;
            usage_observation = result.usage_observation;
            let reply = result.reply?;
            let producer = Producer::Attempt {
                run_id: self.run_id.clone(),
                node_id: input.node.clone(),
                attempt_id: attempt.id().into(),
            };
            let mut outputs = BTreeMap::new();
            for (port, values) in reply.outputs {
                let declaration = &worker.signature.contract.outputs[&port];
                let snapshot_id = match &declaration.affinity {
                    PortAffinityV1::Unbound {} => None,
                    PortAffinityV1::SameAs { input: port } => input
                        .inputs
                        .get(port)
                        .ok_or("Worker output affinity input is absent")?
                        .snapshot_id
                        .clone(),
                    PortAffinityV1::DerivedFrom { .. } => {
                        return Err(
                            "Only an installed seal operator can establish a derived Snapshot"
                                .into(),
                        );
                    }
                };
                // A Worker supplies the inspection map; the kernel binds the identity. Unknown
                // fields, invalid paths and a foreign node, Attempt or head are refused here and
                // rechecked against the stored artifact at durable admission.
                let values: Vec<serde_json::Value> =
                    if declaration.artifact_type == review_core::task::WORKER_NOTES_V1 {
                        let head = snapshot_id
                            .clone()
                            .or_else(|| {
                                input
                                    .inputs
                                    .get("source")
                                    .and_then(|source| source.snapshot_id.clone())
                            })
                            .ok_or("Worker Notes need a head Snapshot to bind to")?;
                        values
                            .into_iter()
                            .map(|value| {
                                review_core::WorkerNotesV1::bind(
                                    value,
                                    &input.node,
                                    attempt.id(),
                                    &head,
                                )
                                .and_then(|notes| {
                                    serde_json::to_value(notes).map_err(|error| error.to_string())
                                })
                            })
                            .collect::<Result<_, String>>()?
                    } else {
                        values
                    };
                let retained: BTreeSet<_> = worker
                    .signature
                    .retains
                    .get(&port)
                    .into_iter()
                    .flatten()
                    .flat_map(|p| {
                        input
                            .inputs
                            .get(p)
                            .into_iter()
                            .flat_map(|i| i.artifact_ids.iter().cloned())
                    })
                    .collect();
                let mut ids = Vec::new();
                for value in values {
                    ids.push(
                        cas.put_artifact(
                            &declaration.artifact_type,
                            producer.clone(),
                            retained.iter().cloned().collect(),
                            snapshot_id.clone(),
                            value,
                        )
                        .map_err(|e| e.to_string())?
                        .0,
                    );
                }
                let value = ArtifactInputV1 {
                    artifact_ids: ids,
                    artifact_type: declaration.artifact_type.clone(),
                    cardinality: declaration.cardinality,
                    snapshot_id,
                };
                value.validate()?;
                outputs.insert(port, value);
            }
            let outputs = self.environment.finish(
                cas,
                input,
                &worker.signature,
                attempt,
                sandbox,
                outputs,
            )?;
            Ok(outputs)
        })();
        let outputs = match self.environment.runtime_evidence(cas, input, attempt) {
            Ok(Some(evidence_id)) => {
                raw_artifact_ids.push(evidence_id);
                outputs
            }
            Ok(None) => outputs,
            Err(error) => Err(error),
        };
        let feedback_id = if outputs.is_err() {
            self.worker_feedback(
                cas,
                input,
                attempt,
                worker,
                feedback_code.unwrap_or(TaskFeedbackCodeV1::OutputAdmissionRejected),
            )
            .ok()
        } else {
            None
        };
        TaskWorkOutput {
            usage_observation,
            usage: token_usage,
            outputs,
            charged_tokens,
            raw_artifact_ids,
            usage_id: None,
            feedback_id,
        }
    }
}

impl TaskOperatorHost for CapturedTaskHost<'_> {
    fn prepare_context_for_resolved_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &review_graph::task::CompiledNode,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        let context_id = match self.resolved_worker(cas, definition)? {
            Some(worker) => {
                worker
                    .contract
                    .prepare(cas, input, attempt.feedback_ids(), &worker.instructions)
            }
            None => self.domain.prepare_context_for_attempt(cas, input, attempt),
        }?;
        let previous = self
            .resolved_contexts
            .lock()
            .expect("resolved Task contexts")
            .insert(attempt.id().into(), context_id.clone());
        if previous
            .as_ref()
            .is_some_and(|previous| previous != &context_id)
        {
            return Err("Resolved Task context changed for one reservation".into());
        }
        Ok(context_id)
    }

    fn execute_resolved_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &review_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        let worker = match self.resolved_worker(cas, definition) {
            Ok(worker) => worker,
            Err(error) => return crate::task::control::refused(error),
        };
        match (worker, attempt) {
            (Some(worker), Some(attempt)) => {
                self.worker_execute(cas, input, attempt, &worker, broker, cancellation)
            }
            (Some(_), None) => TaskWorkOutput {
                usage_observation: None,
                usage: None,
                outputs: Err("Worker has no durably started Attempt".into()),
                charged_tokens: Some(0),
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            },
            (None, _) => self
                .domain
                .execute_controlled(cas, input, attempt, broker, cancellation),
        }
    }
    fn prepare_experiment(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        writer_epoch: u64,
    ) -> Result<super::TaskExperimentInputs, String> {
        self.domain.prepare_experiment(cas, parent, writer_epoch)
    }
    fn complete_experiment(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        experiment: &review_store::store::task::execution::experiment::RegisteredTaskExperiment,
        facts: &[review_store::store::task::execution::experiment::ExperimentChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        self.domain
            .complete_experiment(cas, parent, experiment, facts)
    }
    fn prepare_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
    ) -> Result<super::TaskOwnedChildrenInputs, String> {
        self.domain.prepare_owned_children(cas, parent)
    }
    fn complete_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        self.domain
            .complete_owned_children(cas, parent, children, facts)
    }

    fn broker_operations(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<Option<Vec<review_core::BrokerOperationPolicyV1>>, String> {
        if self
            .workers
            .get(&input.node)
            .is_some_and(|worker| matches!(worker.transport, WorkerTransport::Command(_)))
        {
            return Ok(None);
        }
        self.domain.broker_operations(cas, input)
    }

    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        self.domain.commit_domain_invocation(cas, id, input)
    }

    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.domain.commit_domain_output(cas, input, id, output)
    }

    fn output_rejection_feedback(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
    ) -> Result<Option<String>, String> {
        match self.workers.get(&input.node) {
            Some(worker) => self
                .worker_feedback(
                    cas,
                    input,
                    attempt,
                    worker,
                    review_core::task::feedback::TaskFeedbackCodeV1::OutputAdmissionRejected,
                )
                .map(Some),
            None => self.domain.output_rejection_feedback(cas, input, attempt),
        }
    }

    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        match self.workers.get(&input.node) {
            Some(worker) => worker
                .contract
                .prepare(cas, input, feedback, &worker.instructions),
            None => self.domain.prepare_context(cas, input, feedback),
        }
    }
    fn prepare_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        if self.workers.contains_key(&input.node) {
            self.prepare_context(cas, input, attempt.feedback_ids())
        } else {
            self.domain.prepare_context_for_attempt(cas, input, attempt)
        }
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        self.execute_with_broker(cas, input, attempt, None)
    }

    fn execute_with_broker(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> TaskWorkOutput {
        self.execute_controlled(cas, input, attempt, broker, None)
    }

    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = crate::task::control::check(cancellation) {
            return crate::task::control::refused(error);
        }

        match (self.workers.get(&input.node), attempt) {
            (Some(worker), Some(attempt)) => {
                self.worker_execute(cas, input, attempt, worker, broker, cancellation)
            }
            (Some(_), None) => TaskWorkOutput {
                usage_observation: None,
                usage: None,
                outputs: Err("Worker has no durably started Attempt".into()),
                charged_tokens: Some(0),
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            },
            (None, _) => self
                .domain
                .execute_controlled(cas, input, attempt, broker, cancellation),
        }
    }
}

impl TaskDomain for CapturedTaskHost<'_> {
    fn validate_experiment_preparation(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        prepared_id: &str,
        prepared: &review_core::task::optimization_experiment::ExperimentPreparedV1,
    ) -> Result<(), String> {
        self.domain
            .validate_experiment_preparation(cas, task, plan, prepared_id, prepared)
    }
    fn validate_review_integration_selection(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        self.domain
            .validate_review_integration_selection(cas, task, plan, phase, evidence)
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_review_integration_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        report: &review_core::task::report::TaskRunReportV2,
        events: &[review_store::NewEvent],
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        self.domain.validate_review_integration_completion(
            cas, task, plan, phase, report, events, evidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_review_continuation(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        previous_plan: &ExecutionPlanV1,
        next_plan: &ExecutionPlanV1,
        handoff: &review_core::task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        self.domain.validate_review_continuation(
            cas,
            previous,
            next,
            previous_plan,
            next_plan,
            handoff,
        )
    }
    fn validate_owned_children(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        self.domain
            .validate_owned_children(cas, task, plan, parent, children)
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.domain
            .validate_owned_completion(cas, task, plan, parent, children, facts, output)
    }

    fn validate_broker_binding(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &review_core::task::broker::TaskBrokerBindingV1,
    ) -> Result<(), String> {
        if self
            .workers
            .get(&binding.node)
            .is_some_and(|worker| matches!(worker.transport, WorkerTransport::Command(_)))
        {
            return Err("Command Workers have no installed Broker transport".into());
        }
        self.domain
            .validate_broker_binding(cas, task, plan, binding)
    }

    fn validate_retry(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        self.domain.validate_retry(cas, task, plan, input, previous)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        context_id: &str,
    ) -> Result<(), String> {
        if let Some(worker) = self
            .workers
            .get(&input.node)
            .map(std::sync::Arc::as_ref)
            .or_else(|| self.experimental_worker(&input.node))
        {
            let (context, _) = worker.contract.read_context(cas, context_id)?;
            if matches!(worker.transport, WorkerTransport::Model(_))
                && self
                    .graph
                    .allowances
                    .get(&input.node)
                    .is_none_or(|allowance| {
                        context.manifest.estimated_tokens > allowance.tokens_per_attempt
                    })
            {
                return Err(
                    "Rendered model context exceeds its admitted Attempt token reservation".into(),
                );
            }
            if worker
                .contract
                .prepare(cas, input, feedback, &worker.instructions)?
                != context_id
            {
                return Err(
                    "Task context changed captured inputs, instructions, contracts or feedback"
                        .into(),
                );
            }
            self.domain
                .validate_context(cas, input, feedback, context_id)
        } else {
            self.domain
                .validate_context(cas, input, feedback, context_id)
        }
    }
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        context_id: &str,
    ) -> Result<(), String> {
        if let Some(expected) = self
            .resolved_contexts
            .lock()
            .expect("resolved Task contexts")
            .get(attempt.id())
            .cloned()
        {
            if expected != context_id {
                return Err(
                    "Task context differs from the exact registered Worker definition".into(),
                );
            }
        } else if self.workers.contains_key(&input.node)
            || self.experimental_worker(&input.node).is_some()
        {
            self.validate_context(cas, input, attempt.feedback_ids(), context_id)?;
        }
        self.domain
            .validate_context_for_attempt(cas, input, attempt, context_id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let node = self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown Task output node")?;
        if matches!(
            node.operator,
            CompiledOperator::RootInputs | CompiledOperator::Select
        ) {
            return Ok(());
        }
        if let Some(worker) = self.workers.get(&input.node) {
            self.validate_worker_output(cas, input, output, worker)?;
        }
        self.domain.validate_output(cas, task, plan, input, output)
    }
    fn validate_resolved_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        if self.graph.nodes.contains_key(&input.node) {
            return self.validate_output(cas, task, plan, input, output);
        }
        let worker = self
            .resolved_worker(cas, definition)?
            .ok_or("Registered Task output has no captured Worker")?;
        self.validate_worker_output(cas, input, output, &worker)?;
        self.domain.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.domain.validate_result(cas, task, result)
    }
}
