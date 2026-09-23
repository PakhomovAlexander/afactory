//! CLI lifecycle for the fixed Planner bootstrap and its exact approval barrier.
use super::*;
use review_config::task::catalog::planning::PLANNER_PIPELINE;
use review_pipeline::task::host::EmptyTaskEnvironment;
use review_pipeline::task::planning::{PlanningTaskDomain, PlanningTaskHost};

pub(super) fn restore(
    cas: &Cas,
    state: &TaskProjection,
    authority: &RunAuthority,
) -> Result<TaskPlanCompiler, String> {
    let mut compiler = restore_compiler(cas, &state.revision.authority.policy_id, authority)?;
    let plan: Option<ExecutionPlanV1> = state
        .plan_id
        .as_deref()
        .map(|id| artifact(cas, id, EXECUTION_PLAN_V1))
        .transpose()?;
    if state.planning.is_some() {
        let proof = state.planning_proof(cas).map_err(|e| e.to_string())?;
        let original: TaskRevisionV1 = artifact(cas, proof.revision_id(), TASK_REVISION_V1)?;
        let settings = authority
            .planner
            .as_ref()
            .ok_or("Task lost its captured Planner setting")?;
        compiler.install_planning_bootstrap(cas, &original, settings)?;
        let _ = bind_models(
            cas,
            &mut compiler,
            authority,
            proof.revision_id(),
            PLANNER_PIPELINE,
        )?;
        compiler.install_selected_proposal(cas, &proof)?;
    } else if plan.as_ref().is_some_and(|p| p.preparation.is_some()) {
        let settings = authority
            .planner
            .as_ref()
            .ok_or("Task lost its captured Planner setting")?;
        compiler.install_planning_bootstrap(cas, &state.revision, settings)?;
    } else if plan
        .as_ref()
        .is_some_and(|p| !p.generated_origins.is_empty())
    {
        return Err("Generated plan has no durable selected Planner proof".into());
    }
    Ok(compiler)
}

pub(super) fn prepare_bootstrap(
    cas: &Cas,
    authority: &RunAuthority,
    mut compiler: TaskPlanCompiler,
    revision: TaskRevisionV1,
    revision_id: String,
) -> Result<selection::SelectedTask, String> {
    let settings = authority
        .planner
        .as_ref()
        .ok_or("No Planner is configured")?;
    authority
        .developers
        .as_ref()
        .ok_or("Generated plans require captured developer keys")?
        .validate()?;
    compiler.install_planning_bootstrap(cas, &revision, settings)?;
    compiler.planning_request(&revision)?;
    let adapters = bind_models(
        cas,
        &mut compiler,
        authority,
        &revision_id,
        PLANNER_PIPELINE,
    )?;
    let (plan, graph) = compiler.compile(cas, &revision_id, PLANNER_PIPELINE)?;
    selection::available_tools(cas, &compiler, authority, &graph)?;
    Ok(selection::SelectedTask {
        revision,
        revision_id,
        compiler,
        adapters,
        plan,
        graph,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start(
    options: StartOptions,
    cas: Cas,
    mut store: EventStore,
    authority: RunAuthority,
    compiler: TaskPlanCompiler,
    revision: TaskRevisionV1,
    revision_id: String,
    undeclared: review_config::layout::PathGroup,
) -> Result<i32, String> {
    let selection::SelectedTask {
        revision,
        revision_id,
        compiler,
        adapters,
        plan,
        graph,
    } = prepare_bootstrap(&cas, &authority, compiler, revision, revision_id)?;
    let resources = compiler.compiled_resources(&graph, &revision.limits, clock()?)?;
    if !resources.is_empty() {
        return Err(resources.join("; "));
    }
    let plan_id = persist_plan(&cas, &plan)?;
    let developer = developer::host(&cas, &authority, None);
    let trusted = developer::DecisionAuthority {
        compiler: &compiler,
        developer: developer.as_ref(),
    };
    let lease = store
        .open_task(
            &cas,
            &revision_id,
            &format!("cli-{}", std::process::id()),
            15_000,
        )
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        store
            .propose_task_plan(&cas, &lease, &plan_id, &trusted)
            .map_err(|e| e.to_string())?;
        if options.plan_only {
            return Ok(());
        }
        store
            .admit_task_plan(&cas, &lease, &trusted)
            .map_err(|e| e.to_string())?;
        run_planner(
            &cas, &mut store, &lease, &authority, &compiler, &revision, &plan, &graph, &adapters,
        )
    })();
    release(&cas, &mut store, &lease, outcome)?;
    present_with_advisory(
        &cas,
        &store,
        &revision.task_id,
        options.json,
        true,
        &undeclared,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume(
    cas: Cas,
    mut store: EventStore,
    authority: RunAuthority,
    mut compiler: TaskPlanCompiler,
    state: TaskProjection,
    plan: ExecutionPlanV1,
    graph: CompiledTask,
    json: bool,
) -> Result<i32, String> {
    let adapters = bind_models(
        &cas,
        &mut compiler,
        &authority,
        &state.revision_id,
        PLANNER_PIPELINE,
    )?;
    compiler.validate_plan(&cas, &state.revision, &plan)?;
    let developer = developer::host(&cas, &authority, None);
    let trusted = developer::DecisionAuthority {
        compiler: &compiler,
        developer: developer.as_ref(),
    };
    let lease = store
        .take_task_lease(
            &cas,
            &state.task_id,
            &format!("cli-{}", std::process::id()),
            15_000,
        )
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        confirm_current_plan(&cas, &store, &state.task_id, state.plan_id.as_deref())?;
        store
            .recover_task_attempts(&cas, &lease)
            .map_err(|e| e.to_string())?;
        if state
            .waiting_for_domain_publication(&cas)
            .map_err(|e| e.to_string())?
        {
            store
                .resume_task(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
        if !state.admitted {
            store
                .admit_task_plan(&cas, &lease, &trusted)
                .map_err(|e| e.to_string())?;
        }
        run_planner(
            &cas,
            &mut store,
            &lease,
            &authority,
            &compiler,
            &state.revision,
            &plan,
            &graph,
            &adapters,
        )
    })();
    release(&cas, &mut store, &lease, outcome)?;
    present(&cas, &store, &state.task_id, json, true)
}

pub(super) fn persist_plan(cas: &Cas, plan: &ExecutionPlanV1) -> Result<String, String> {
    cas.put_artifact(
        EXECUTION_PLAN_V1,
        producer(),
        vec![plan.task_revision_id.clone()],
        None,
        serde_json::to_value(plan).map_err(|e| e.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|e| e.to_string())
}

fn finish_incomplete(
    cas: &Cas,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    conclusion: &str,
    diagnostic: Option<&str>,
) -> Result<(), String> {
    let outputs = state
        .execution
        .as_ref()
        .into_iter()
        .flat_map(|e| {
            e.graph.outputs.iter().filter_map(|(name, address)| {
                e.outputs
                    .get(&address.node)
                    .and_then(|(_, output)| output.outputs.get(&address.port))
                    .map(|value| (name.clone(), value.clone()))
            })
        })
        .collect();
    let mut refs = vec![state.revision_id.clone()];
    if let Some(reason) = diagnostic {
        refs.push(cas.put_json(&json!({"schema":"af.planning-diagnostic/1","plan_id":state.plan_id,"reason":reason.chars().take(8192).collect::<String>()})).map_err(|e| e.to_string())?);
    }
    let result = TaskResultV1 {
        task_revision_id: state.revision_id.clone(),
        execution: TaskExecutionV1::Incomplete,
        acceptance: TaskAcceptanceV1::Inconclusive,
        domain_conclusion: conclusion.into(),
        outputs,
        evidence: BTreeSet::new(),
        missing_obligations: state.revision.acceptance.keys().cloned().collect(),
    };
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            refs,
            None,
            serde_json::to_value(result).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    runtime.finish(&id)
}

#[allow(clippy::too_many_arguments)]
fn run_planner(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    authority: &RunAuthority,
    compiler: &TaskPlanCompiler,
    task: &TaskRevisionV1,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    adapters: &BTreeMap<String, Box<dyn review_runner::task::WorkerModelAdapter>>,
) -> Result<(), String> {
    let inner = PlanningTaskDomain {
        compiler,
        task,
        graph,
    };
    let models = model_bindings(plan, graph, adapters)?;
    let domain = ProviderTaskDomain {
        graph,
        models: &models,
        inner: &inner,
    };
    let host = CapturedTaskHost::capture_with_models(
        cas,
        compiler,
        task,
        plan,
        graph.clone(),
        &EmptyTaskEnvironment,
        &domain,
        &models,
    )?;
    let validate_proposal =
        |cas: &Cas,
         task: &TaskRevisionV1,
         proposal: &review_core::task::planning::PipelineProposalV1| {
            let required = compiler.required_proposal_workers(cas, task, proposal)?;
            let mut candidate = compiler.clone();
            // Only proposed Workers need token-free account identity reads. This runs outside
            // the Store lock; paid Provider capability admission remains after plan approval.
            let _adapters = bind_named_models(&mut candidate, authority, required)?;
            candidate.check_pipeline_proposal(cas, task, proposal)
        };
    let host = PlanningTaskHost {
        inner: &host,
        validate_proposal: &validate_proposal,
        task,
    };
    let developer = developer::host(cas, authority, None);
    let trusted = CapturedTaskAuthority::new(compiler, &host, developer.as_ref());
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let state = {
        let runtime = TaskRuntime::new(store, cas, lease.clone(), &trusted, &host)?
            .with_cancellation(&cancellation);
        let _report = runtime.execute()?;
        let state = runtime.projection()?;
        if matches!(state.phase, TaskPhaseV1::Waiting { .. }) {
            return Ok(());
        }
        if state.planning_proof(cas).is_err() {
            finish_incomplete(cas, &runtime, &state, "planning_incomplete", None)?;
            return Ok(());
        }
        state
    };
    let admission = (|| {
        let proof = state.planning_proof(cas).map_err(|e| e.to_string())?;
        let mut compiler = compiler.clone();
        let root = compiler.install_selected_proposal(cas, &proof)?;
        let mut next = task.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("Task revision overflow")?;
        next.previous_revision_id = Some(state.revision_id.clone());
        next.inputs = compiler.normalize_root_inputs(cas, &root, next.inputs)?;
        next.provenance.input_artifact_ids.extend(
            next.inputs
                .values()
                .flat_map(|p| p.artifact_ids.iter().cloned()),
        );
        next.provenance.input_artifact_ids.sort();
        next.provenance.input_artifact_ids.dedup();
        let revision_id = selection::capture_revision(cas, &next)?;
        let _adapters = bind_models(cas, &mut compiler, authority, &revision_id, &root)?;
        let (generated, graph) = compiler.compile(cas, &revision_id, &root)?;
        selection::available_tools(cas, &compiler, authority, &graph)?;
        let remaining = state
            .execution
            .as_ref()
            .ok_or("Planner has no accounting")?
            .budget
            .remaining_limits();
        let resources = compiler.candidate_resources(cas, graph, &remaining, clock()?)?;
        if !resources.is_empty() {
            return Err(resources.join("; "));
        }
        let generated_id = persist_plan(cas, &generated)?;
        let decision = developer::DecisionAuthority {
            compiler: &compiler,
            developer: developer.as_ref(),
        };
        store
            .complete_task_planning(cas, lease, &revision_id, &generated_id, &decision)
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if let Err(error) = admission {
        let runtime = TaskRuntime::new(store, cas, lease.clone(), &trusted, &host)?
            .with_cancellation(&cancellation);
        finish_incomplete(
            cas,
            &runtime,
            &state,
            "planning_admission_failed",
            Some(&error),
        )?;
    }
    Ok(())
}
