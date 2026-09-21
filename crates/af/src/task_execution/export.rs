//! A read-only projection can export definitions, but cannot mint a live Provider capability.
//! Recorded bindings below are used only in this private compiler, which never leaves export.
use super::*;
use review_config::task::catalog::planning::PLANNER_PIPELINE;

fn recorded_bindings(
    compiler: &mut TaskPlanCompiler,
    authority: &RunAuthority,
    plan: &ExecutionPlanV1,
) -> Result<(), String> {
    let mut admitted = BTreeMap::new();
    for binding in plan.bindings.values() {
        let (name, package) = authority
            .packages
            .iter()
            .find(|(_, p)| p.artifact_id == binding.package_artifact_id)
            .ok_or("Recorded Worker binding is outside captured authority")?;
        if package.digest != binding.package_digest {
            return Err("Recorded Worker binding changed its package digest".into());
        }
        if let Some(previous) = admitted.insert(name.clone(), binding)
            && previous != binding
        {
            return Err("Recorded Worker package has conflicting bindings".into());
        }
        compiler.bind_worker(
            name,
            AdmittedWorkerSettings {
                execution: binding.execution.clone(),
                invocation_policy_id: binding.invocation_policy_id.clone(),
            },
        )?;
    }
    Ok(())
}

pub(crate) fn run(
    id: &str,
    name: &str,
    destination: &str,
    repo: &Path,
    state: Option<&Path>,
    json: bool,
) -> Result<i32, String> {
    let destination = catalog::absent_destination(repo, destination)?;
    let (_, state_path) = state_path(repo, state)?;
    let cas = Cas::open_existing(state_path.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state_path.join("events.sqlite")).map_err(|e| e.to_string())?;
    let state = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let plan_id = state
        .plan_id
        .as_deref()
        .ok_or("Task has no compiled plan to export")?;
    let plan: ExecutionPlanV1 = artifact(&cas, plan_id, EXECUTION_PLAN_V1)?;
    if plan.preparation.is_some() {
        return Err("Planning has not produced an executable definition to export".into());
    }
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&state.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut compiler = restore_compiler(&cas, &state.revision.authority.policy_id, &authority)?;
    let mut private_values = BTreeSet::from([
        state.revision_id.clone(),
        plan_id.into(),
        state.revision.authority.policy_id.clone(),
        plan.compiled_graph_id.clone(),
    ]);
    if state.planning.is_some() {
        let proof = state.planning_proof(&cas).map_err(|e| e.to_string())?;
        let original: TaskRevisionV1 = artifact(&cas, proof.revision_id(), TASK_REVISION_V1)?;
        compiler.install_planning_bootstrap(
            &cas,
            &original,
            authority
                .planner
                .as_ref()
                .ok_or("Captured Planner setting is absent")?,
        )?;
        let bootstrap: ExecutionPlanV1 =
            artifact(&cas, proof.bootstrap_plan_id(), EXECUTION_PLAN_V1)?;
        recorded_bindings(&mut compiler, &authority, &bootstrap)?;
        compiler.install_selected_proposal(&cas, &proof)?;
        private_values.extend([
            proof.revision_id().into(),
            proof.bootstrap_plan_id().into(),
            proof.proposal_id().into(),
        ]);
    } else if !plan.generated_origins.is_empty() {
        return Err("Generated plan lacks durable Planner proof".into());
    }
    recorded_bindings(&mut compiler, &authority, &plan)?;
    compiler.validate_plan(&cas, &state.revision, &plan)?;
    let root = plan
        .dependencies
        .iter()
        .find(|(_, p)| p.artifact_id == plan.pipeline_id)
        .map(|(name, _)| name.as_str())
        .ok_or("Plan has no captured root")?;
    if root == PLANNER_PIPELINE {
        return Err("The trusted Planner bootstrap is not an exportable execution plan".into());
    }
    let exported = compiler.export_catalog(root, name)?;
    for input in state.revision.inputs.values() {
        private_values.extend(input.artifact_ids.iter().cloned());
        private_values.extend(input.snapshot_id.iter().cloned());
    }
    private_values.extend(state.revision.provenance.input_artifact_ids.iter().cloned());
    // Short labels are not unique enough to identify private context. The export itself never
    // serializes Task goal/ID; reject recognizable copies embedded in a generated definition.
    for value in [&state.task_id, &state.revision.goal] {
        if value.len() >= 8 {
            private_values.insert(value.clone());
        }
    }
    for (path, bytes) in &exported.files {
        if private_values
            .iter()
            .any(|value| bytes.windows(value.len()).any(|w| w == value.as_bytes()))
        {
            return Err(format!(
                "Exported file {path} embeds originating Task state; move that value to a typed input before sharing"
            ));
        }
    }
    catalog::publish_absent(&destination, &exported.files)?;
    if json {
        println!(
            "{}",
            json!({"schema":"af.task-export/1", "root":exported.root,
            "destination":destination, "catalog":"catalog.toml", "contracts":"contracts.json",
            "packages":exported.catalog.packages, "execution_authorized":false})
        );
    } else {
        println!(
            "Exported {} to {}\nReview and test the definitions, commit them, then explicitly sync the catalog for reuse.",
            exported.root,
            destination.display()
        );
    }
    Ok(0)
}
