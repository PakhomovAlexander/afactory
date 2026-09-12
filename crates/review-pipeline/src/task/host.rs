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
    transport: WorkerTransport<'a>,
    files: BTreeMap<String, Vec<u8>>,
    instructions: String,
    signature: OperatorSignature,
    contract: WorkerContract,
}

pub struct CapturedTaskHost<'a> {
    run_id: String,
    graph: CompiledTask,
    workers: BTreeMap<String, CapturedWorker<'a>>,
    environment: &'a dyn TaskEnvironment,
    domain: &'a dyn TaskDomain,
}

pub type CommandTaskHost<'a> = CapturedTaskHost<'a>;

impl<'a> CapturedTaskHost<'a> {
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
            let name = &graph.slots[slot].worker;
            let manifest = compiler.worker(name).ok_or("Missing captured Worker")?;
            let transport = match &manifest.runner {
                TaskWorkerRunner::Command { command }
                | TaskWorkerRunner::LegacyTaskCommand { command, .. } => {
                    WorkerTransport::Command(command.build())
                }
                TaskWorkerRunner::Model { provider_kind, .. } => {
                    let model = models
                        .get(slot)
                        .ok_or("Model Worker requires its admitted Provider adapter")?;
                    if plan.bindings.get(slot) != Some(&model.binding)
                        || !matches!(&model.binding.execution, WorkerExecutionV1::Model { provider_kind: kind, model: model_id, effort, .. } if kind == provider_kind && kind == model.adapter.provider_kind() && model.adapter.model_settings().as_ref() == Some(&(model_id.clone(), effort.clone())))
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
            workers.insert(
                id.clone(),
                CapturedWorker {
                    transport,
                    files: files.clone(),
                    instructions,
                    signature: manifest.signature.clone(),
                    contract,
                },
            );
        }
        Ok(Self {
            run_id: task_run_id(&task.task_id).map_err(|e| e.to_string())?,
            graph,
            workers,
            environment,
            domain,
        })
    }

    fn worker_execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        worker: &CapturedWorker<'_>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> TaskWorkOutput {
        use review_core::task::feedback::*;
        let mut raw_artifact_ids = Vec::new();
        let mut charged_tokens = Some(0);
        let mut token_usage = None;
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
            let sandbox = self
                .environment
                .materialize(cas, input, &worker.signature)?;
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
                    review_runner::task::invoke_command(
                        cas,
                        sandbox.root(),
                        tempfile::tempdir().map_err(|e| e.to_string())?.path(),
                        &command,
                        &worker.contract,
                        attempt.context_id(),
                        Duration::from_millis(remaining),
                    )
                }
                WorkerTransport::Model(adapter) => review_runner::task::invoke_model_with_broker(
                    cas,
                    sandbox.root(),
                    *adapter,
                    &worker.contract,
                    attempt.context_id(),
                    Duration::from_millis(remaining),
                    worker.signature.effects.contains("write-source"),
                    broker,
                ),
            };
            raw_artifact_ids = result.raw_artifact_ids;
            feedback_code = result.feedback_code;
            charged_tokens = result.usage.as_ref().map(|usage| usage.chargeable_tokens);
            token_usage = result.usage;
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
            self.environment
                .finish(cas, input, &worker.signature, attempt, sandbox, outputs)
        })();
        let feedback_id = if outputs.is_err() {
            let feedback = TaskRetryFeedbackV1 {
                attempt_id: attempt.id().into(),
                contract_id: worker.contract.id().into(),
                code: feedback_code.unwrap_or(TaskFeedbackCodeV1::OutputAdmissionRejected),
                compiler: None,
            };
            feedback
                .validate()
                .and_then(|()| {
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
                })
                .ok()
        } else {
            None
        };
        TaskWorkOutput {
            usage: token_usage,
            outputs,
            charged_tokens: charged_tokens.map(u128::from),
            raw_artifact_ids,
            usage_id: None,
            feedback_id,
        }
    }
}

impl TaskOperatorHost for CapturedTaskHost<'_> {
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
        match (self.workers.get(&input.node), attempt) {
            (Some(worker), Some(attempt)) => {
                self.worker_execute(cas, input, attempt, worker, broker)
            }
            (Some(_), None) => TaskWorkOutput {
                usage: None,
                outputs: Err("Worker has no durably started Attempt".into()),
                charged_tokens: Some(0),
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            },
            (None, _) => self.domain.execute_with_broker(cas, input, attempt, broker),
        }
    }
}

impl TaskDomain for CapturedTaskHost<'_> {
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
        if let Some(worker) = self.workers.get(&input.node) {
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
        if self.workers.contains_key(&input.node) {
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
            worker.contract.validate_typed_reply(
                &serde_json::to_vec(&review_runner::task::WorkerReply {
                    schema: "af.worker-reply/1".into(),
                    outputs: values,
                })
                .map_err(|e| e.to_string())?,
            )?;
        }
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
