//! Scenario templates use the ordinary Task-file adapter and its input resolver. Authority is
//! restored from sealed native artifacts before planning; generated plans retain their approval gate.
use super::*;
use af_scenario::contract::{TaskReceipt, TaskTarget};
use af_scenario::template::Template;

pub(crate) fn validate_document(
    document: &serde_json::Value,
    bindings: Option<&serde_json::Value>,
    maximum: u64,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(document).map_err(|e| e.to_string())?;
    let file = parse_task_file(Path::new("template.json"), &bytes)?;
    task_file_identity(Path::new("template.json"), &file)?;
    if file
        .base
        .as_ref()
        .is_some_and(|b| b.commit.len() != 40 || !b.commit.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("Template base must name an exact commit".into());
    }
    if file
        .pipeline
        .as_ref()
        .is_some_and(|p| p.fallback != PipelineFallbackV1::Refuse)
    {
        return Err(
            "Template pipeline fallback must be refuse; no generated or default plan authority"
                .into(),
        );
    }
    if bytes.len() > 65536
        || document
            .get("requirements")
            .and_then(|v| v.as_object())
            .is_some_and(|r| {
                r.contains_key("scenario_evidence")
                    || r.contains_key("scenario_goal")
                    || r.contains_key("scenario_native_outcome")
            })
        || file.inputs.is_some()
        || file.continue_from.is_some()
        || file.issue.is_some()
    {
        return Err(
            "Template must be bounded and cannot prefill inputs, continuation or issue authority"
                .into(),
        );
    }
    let limits = file
        .limits
        .as_ref()
        .ok_or("Template must declare all native Task limits")?;
    if limits.tokens != maximum
        || file.pipeline.is_none()
        || file.strategy.is_none()
        || (file.kind == "implement" && file.verification.is_none())
    {
        return Err(
            "Template must pin its pipeline, strategy, implementation verification and exact token limit".into(),
        );
    }
    super::scenario_portable::bound_sources(file.requirements.as_ref())?;
    if let Some(value) = bindings {
        let _local: bindings::LocalBindings =
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if value["schema"] != "af.task-bindings/1" {
            return Err("Template bindings require af.task-bindings/1".into());
        }
    }
    Ok(())
}
fn capture(template: &Template) -> Result<(String, String, TaskKindProfile), TemplatePlanError> {
    if crate::resolve_filesystem_path(&template.repo)? != template.repo
        || crate::resolve_filesystem_path(&template.state)? != template.state
    {
        return Err("Pinned template repository or Task Store path changed".into());
    }
    let (_, state) = state_path(&template.repo, Some(&template.state))?;
    crate::storage::owned::prepare(&state, true)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    // Only explicit native pinning establishes the source/authority association. A machine
    // policy grant for a recomputed template digest cannot manufacture that association.
    let store = EventStore::open_read_only(state.join("events.sqlite")).map_err(journal_error)?;
    validate_pin(&store, &cas, template)?;
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let repo = Repo::open(&template.repo, home.path());
    let source = Capture::new(&repo, &cas)
        .committed(&template.authority_commit)
        .map_err(|e| e.to_string())?;
    let file: TaskFile =
        serde_json::from_value(template.document.clone()).map_err(|e| e.to_string())?;
    let authority = template.authority_id.clone();
    let settings: RunAuthority =
        serde_json::from_value(cas.get_json(&authority).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let compiler = restore_compiler(&cas, &authority, &settings)?;
    let recorded_bindings = settings
        .local_bindings_id
        .as_ref()
        .map(|id| cas.get_json(id).map_err(|e| e.to_string()))
        .transpose()?;
    if recorded_bindings != template.bindings {
        return Err("Template bindings differ from captured authority".into());
    }
    let profile = selected_profile(&compiler, &settings, &file.kind, file.verification)?;
    Ok((source.repository_id, source.content_digest, profile))
}
/// Restore actual sealed native authority before any Task dispatch. No model admission Attempt, inference or publication occurs here; native
/// planning and account prerequisite probes are token-free.
pub(crate) fn preflight_implementation(
    template: &Template,
    expected: &BTreeMap<String, af_scenario::implementation::ModelBinding>,
) -> Result<(), String> {
    template.validate(&crate::scenario::host::AfHost)?;
    let (repository, content, _) = capture(template).map_err(plan_diagnostic)?;
    if repository != template.repository_id || content != template.source_content_digest {
        return Err("Startup template source differs from native capture".into());
    }
    let cas = Cas::open(template.state.join("cas")).map_err(|e| e.to_string())?;
    let settings: RunAuthority = serde_json::from_value(
        cas.get_json(&template.authority_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let compiler = restore_compiler(&cas, &template.authority_id, &settings)?;
    let file: TaskFile =
        serde_json::from_value(template.document.clone()).map_err(|e| e.to_string())?;
    let root = &file
        .pipeline
        .ok_or("Startup template needs a public Pipeline")?
        .name;
    let workers = compiler.startup_workers(root)?;
    let mut actual = BTreeMap::new();
    for name in workers {
        let worker = compiler.worker(&name).ok_or("Startup Worker missing")?;
        if file.kind == "verification" && worker.signature.effects.contains("write-source") {
            return Err("Startup verification must be read-only".into());
        }
        let TaskWorkerRunner::Model {
            provider_kind,
            model,
            effort,
        } = &worker.runner
        else {
            return Err("Startup refuses command Workers".into());
        };
        let provider = settings
            .providers
            .get(&name)
            .ok_or("Startup Worker lacks Provider binding")?;
        crate::model_check::static_rules(&crate::model_check::Binding {
            owner: name.clone(),
            label: &name,
            provider: Some(provider),
            kind: provider_kind,
            model,
            effort,
        })?;
        actual.insert(
            name,
            af_scenario::implementation::ModelBinding {
                provider: provider.clone(),
                provider_kind: provider_kind.clone(),
                model: model.clone(),
                effort: effort.clone(),
            },
        );
    }
    if &actual != expected {
        return Err("Startup effective Pipeline/provider/model pool differs".into());
    }
    for binding in actual.values() {
        if binding.provider != "codex-personal" || binding.provider_kind != "codex" {
            return Err("Startup requires codex-personal".into());
        }
    }
    // Probe only after the entire closure has passed static and owner-scope checks.
    let identity = crate::providers::task::TaskProviderIdentity::probe("codex-personal", "codex")?;
    for binding in actual.values() {
        identity.execution(&binding.model, &binding.effort)?;
    }
    // Exercise the ordinary native planner as well as the static closure. This is an
    // unpaid, never-executed Task with a deterministic identity, not a fabricated plan or
    // receipt. It proves contracts, resource feasibility, local check configuration and
    // required native constructors using the template's actual captured source. Future
    // predecessor content remains checked at dispatch by the ordinary native resolver.
    let mut document = template.document.clone();
    document["task_id"] =
        format!("startup-{}", template.id()?.trim_start_matches("sha256:")).into();
    plan(template, document, clock()?, false, &BTreeMap::new()).map_err(plan_diagnostic)?;
    Ok(())
}

fn validate_pin(
    store: &EventStore,
    cas: &Cas,
    template: &Template,
) -> Result<(), TemplatePlanError> {
    let run = format!(
        "scenario-template-{}",
        template.id()?.trim_start_matches("sha256:")
    );
    let artifact = store.scenario_template_pin(&run).map_err(journal_error)?;
    verify_pin_artifact(cas, template, &artifact).map_err(Into::into)
}

fn verify_pin_artifact(cas: &Cas, template: &Template, artifact: &str) -> Result<(), String> {
    let expected = serde_json::to_value(template).map_err(|e| e.to_string())?;
    if cas.get_json(artifact).map_err(|e| e.to_string())? != expected {
        return Err("Template differs from its durable native pin".into());
    }
    Ok(())
}

// SQLite and in-memory comparisons only: pin publication calls this under the writer lock.
fn validate_pin_event(
    store: &EventStore,
    template: &Template,
    artifact: &str,
) -> Result<(), String> {
    let run = format!(
        "scenario-template-{}",
        template.id()?.trim_start_matches("sha256:")
    );
    if store
        .scenario_template_pin(&run)
        .map_err(|e| e.to_string())?
        != artifact
    {
        return Err("Template differs from its durable native pin".into());
    }
    Ok(())
}
pub(crate) fn pin(
    repo: PathBuf,
    state: PathBuf,
    authority_commit: String,
    document: serde_json::Value,
    local: Option<&Path>,
) -> Result<Template, String> {
    let maximum = document["limits"]["tokens"]
        .as_u64()
        .ok_or("Template must declare tokens")?;
    validate_document(&document, None, maximum)?;
    pin_with(repo, state, authority_commit, |cas, manifest, delivery| {
        let file: TaskFile = serde_json::from_value(document.clone()).map_err(|e| e.to_string())?;
        let (authority, settings, compiler) =
            capture_authority(cas, manifest, local, &file.kind, delivery)?;
        selected_profile(&compiler, &settings, &file.kind, file.verification)?;
        Ok((document, authority, settings, ()))
    })
    .map(|(template, ())| template)
}

pub(super) fn pin_with<T>(
    repo: PathBuf,
    state: PathBuf,
    authority_commit: String,
    load: impl FnOnce(
        &Cas,
        &Manifest,
        af_pipeline_config::layout::UndeclaredAfPathsPolicy,
    ) -> Result<(serde_json::Value, String, RunAuthority, T), String>,
) -> Result<(Template, T), String> {
    let repo_path = crate::resolve_filesystem_path(&repo)?;
    let state_path_resolved = crate::resolve_filesystem_path(&state)?;
    let authority_commit = authority_commit.to_ascii_lowercase();
    let (_, state) = state_path(&repo_path, Some(&state_path_resolved))?;
    crate::storage::owned::prepare(&state, true)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    store
        .capture_scenario_template(
            &cas,
            || {
                let home = tempfile::tempdir().map_err(|e| e.to_string())?;
                let repo = Repo::open(&repo_path, home.path());
                let source = Capture::new(&repo, &cas)
                    .committed(&authority_commit)
                    .map_err(|e| e.to_string())?;
                let delivery = captured_delivery_policy(&cas, &source.manifest)?;
                let (document, authority_id, settings, extra) =
                    load(&cas, &source.manifest, delivery)?;
                let max_tokens = document["limits"]["tokens"]
                    .as_u64()
                    .ok_or("Template must declare tokens")?;
                #[cfg(debug_assertions)]
                runs::observation_barrier("template-captured")?;
                restore_compiler(&cas, &authority_id, &settings)?;
                let bindings = settings
                    .local_bindings_id
                    .as_ref()
                    .map(|id| cas.get_json(id).map_err(|e| e.to_string()))
                    .transpose()?;
                let template = Template {
                    schema: "af/scenario-task-template@2".into(),
                    repo: repo_path,
                    state: state_path_resolved,
                    authority_commit,
                    repository_id: source.repository_id,
                    source_content_digest: source.content_digest,
                    authority_id,
                    document,
                    bindings,
                    max_tokens,
                };
                template.validate(&crate::scenario::host::AfHost)?;
                // A template pin is a durable non-Task root. Task collection traverses its native
                // authority and package references even before the first Scenario Run exists.
                let template_artifact = cas
                    .put_json(&serde_json::to_value(&template).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                #[cfg(debug_assertions)]
                runs::observation_barrier("template-verifying")?;
                verify_pin_artifact(&cas, &template, &template_artifact)?;
                let run = format!(
                    "scenario-template-{}",
                    template.id()?.trim_start_matches("sha256:")
                );
                Ok((
                    run,
                    af_store::NewEvent::new(
                        af_contracts::EventType::ScenarioTemplatePinnedV1,
                        json!({"artifact_id":template_artifact}),
                    )
                    .referencing(vec![template_artifact.clone()]),
                    (template, template_artifact, extra),
                ))
            },
            |store, (template, artifact, _)| validate_pin_event(store, template, artifact),
        )
        .map(|(template, _, extra)| (template, extra))
}
pub(crate) fn check_predecessor(
    template: &Template,
    receipt: &TaskReceipt,
    input: &str,
    port: &str,
    artifact_type: &str,
) -> Result<(), String> {
    let opened = open_task(&receipt.task_id, &template.repo, Some(&template.state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase,TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
        || receipt.result.execution != TaskExecutionV1::Completed
        || receipt.result.acceptance != TaskAcceptanceV1::Satisfied
    {
        return Err(
            "Predecessor is not the exact accepted native result in this Task Store".into(),
        );
    }
    let output = receipt
        .result
        .outputs
        .get(port)
        .ok_or("Predecessor output is missing")?;
    if output.artifact_type != artifact_type || output.artifact_ids.is_empty() {
        return Err("Predecessor output has the wrong type or no artifacts".into());
    }
    for id in &output.artifact_ids {
        let envelope = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_type != artifact_type {
            return Err("Predecessor envelope type differs".into());
        }
        if input == "research_report" {
            use af_contracts::task::document::{DOCUMENT_V1, DocumentV1};
            if artifact_type != DOCUMENT_V1
                || output.artifact_ids.len() != 1
                || output.cardinality != PortCardinality::One
            {
                return Err("Research requires one accepted native report Document".into());
            }
            let document: DocumentV1 =
                serde_json::from_value(envelope.payload.clone()).map_err(|e| e.to_string())?;
            document.validate()?;
            if serde_json::to_vec(&envelope)
                .map_err(|e| e.to_string())?
                .len()
                > 32768
            {
                return Err(
                    "Accepted research report exceeds 32 KiB; narrow the research Task".into(),
                );
            }
            let source = opened
                .task
                .revision
                .inputs
                .get("source")
                .and_then(|p| p.snapshot_id.as_deref())
                .ok_or("Research has no native source")?;
            if output.snapshot_id.as_deref() != Some(source)
                || envelope.subject_snapshot_id.as_deref() != Some(source)
            {
                return Err("Research report source differs from its accepted Task".into());
            }
            let (snapshot, _) = af_source_git::task::read_snapshot(&opened.cas, source)?;
            let origin = af_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
            if origin.repository_id() != template.repository_id
                || snapshot.content_digest != template.source_content_digest
                || origin.source_revision() != Some(template.authority_commit.as_str())
                || snapshot.parent_snapshot_id.is_some()
            {
                return Err("Research report belongs to a different source".into());
            }
        }
        if artifact_type == SOURCE_TREE_V1 {
            let tree: af_source_git::task::SourceTree =
                serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
            let (snapshot, _) = af_source_git::task::read_snapshot(&opened.cas, &tree.snapshot_id)?;
            let origin = af_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
            if origin.repository_id() != template.repository_id {
                return Err("Predecessor source belongs to a different repository".into());
            }
        }
    }
    // The native resolver additionally checks exact Snapshot provenance and root-port types.
    Ok(())
}

pub(crate) fn check_experiment_source(
    template: &Template,
    receipt: &TaskReceipt,
    port: &str,
    expected: &str,
) -> Result<(), String> {
    let opened = open_task(&receipt.task_id, &template.repo, Some(&template.state))?;
    let output = receipt
        .result
        .outputs
        .get(port)
        .ok_or("Native experiment source output absent")?;
    let [artifact] = output.artifact_ids.as_slice() else {
        return Err("Native experiment source requires one SourceTree artifact".into());
    };
    let snapshot = input_bindings::source_tree_snapshot(&opened.cas, artifact)?;
    if output.snapshot_id.as_deref() != Some(&snapshot) {
        return Err("Experiment source output disagrees with its artifact".into());
    }
    experiment_source_matches(&opened.cas, &snapshot, expected)
}

pub(crate) fn experiment_source_matches(
    cas: &Cas,
    source: &str,
    expected: &str,
) -> Result<(), String> {
    if !af_contracts::is_digest(expected) {
        return Err("Native experiment evidence revision is not an exact Snapshot".into());
    }
    let mut current = source.to_owned();
    let (root, _) = af_source_git::task::read_snapshot(cas, source)?;
    let root_origin = af_source_git::task::read_origin(cas, &root.origin_id)?;
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        if !seen.insert(current.clone()) {
            return Err("Cyclic experiment source provenance".into());
        }
        let (snapshot, _) = af_source_git::task::read_snapshot(cas, &current)?;
        let origin = af_source_git::task::read_origin(cas, &snapshot.origin_id)?;
        if origin.repository_id() != root_origin.repository_id() {
            return Err("Experiment source provenance changed repository".into());
        }
        if snapshot.content_digest != root.content_digest {
            return Err("Task source changed from the verified experiment Snapshot".into());
        }
        if current == expected {
            return Ok(());
        }
        let Some(bound) = origin.bound_from() else {
            return Err("Task source differs from the verified experiment Snapshot".into());
        };
        current = bound.snapshot_id.clone();
    }
    Err("Experiment source provenance exceeds its bound".into())
}
pub(super) fn plan_diagnostic(error: TemplatePlanError) -> String {
    match error {
        TemplatePlanError::StorageCapacity(diagnostic) | TemplatePlanError::Refused(diagnostic) => {
            diagnostic
        }
    }
}

pub(super) fn journal_error(error: af_store::StoreError) -> TemplatePlanError {
    if error.is_journal_capacity() {
        TemplatePlanError::StorageCapacity(format!(
            "Native Task event journal capacity exhausted: {error}"
        ))
    } else {
        TemplatePlanError::Refused(error.to_string())
    }
}

fn prepare<T>(
    template: &Template,
    operation: impl FnOnce() -> Result<T, TemplatePlanError>,
) -> Result<T, TemplatePlanError> {
    af_store::cas::observe_write_capacity(&template.state.join("cas"), operation).map_err(
        |(error, capacity)| match (error, capacity) {
            (TemplatePlanError::Refused(diagnostic), Some(capacity)) => TemplatePlanError::StorageCapacity(format!(
                "Task artifact Store capacity exhausted writing {} ({}). Native preparation reported {diagnostic}",
                capacity.path.display(),
                capacity.diagnostic
            )),
            (error, _) => error,
        },
    )
}

pub(super) fn propose_prepared_plan(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    plan_id: &str,
    trusted: &dyn af_store::store::task::TaskAuthority,
) -> Result<(), TemplatePlanError> {
    let proposed = store.propose_task_plan(cas, lease, plan_id, trusted);
    if proposed
        .as_ref()
        .is_err_and(|error| error.is_journal_capacity())
    {
        // A failed commit can be uncertain. Let a newer writer inspect the journal after this
        // lease expires instead of trying to append another transition under pressure.
        return proposed.map(|_| ()).map_err(journal_error);
    }
    let released = store.release_task_lease(cas, lease);
    proposed.and(released).map_err(journal_error)?;
    #[cfg(debug_assertions)]
    runs::observation_barrier("scenario-task-proposed")?;
    Ok(())
}

/// The planner flushed the complete plan before opening this revision. Verify that exact
/// compilation against the Store without publishing any replacement artifacts.
pub(super) fn recover_opening(
    cas: &Cas,
    store: &mut EventStore,
    opened: &TaskProjection,
    verification: Option<FileVerification>,
) -> Result<(), TemplatePlanError> {
    let check_opening = |current: &TaskProjection| -> Result<(), TemplatePlanError> {
        if current.revision_id != opened.revision_id
            || current.prepared_plan_id != opened.prepared_plan_id
            || current.plan_id.is_some()
            || !matches!(current.phase, TaskPhaseV1::Submitted {})
            || state::started(current)
            || current.revision.revision != 1
        {
            return Err(
                "Interrupted Scenario Task opening changed or is no longer unplanned".into(),
            );
        }
        Ok(())
    };
    check_opening(opened)?;
    let verified = cas.verifying_existing();
    let revision = &opened.revision;
    let revision_id = &opened.revision_id;
    let policy = &revision.authority.policy_id;
    let authority: RunAuthority =
        serde_json::from_value(verified.get_json(policy).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let root = &revision
        .pipeline
        .as_ref()
        .ok_or("Fresh Task has no pinned Pipeline")?
        .name;
    let mut compiler = restore_compiler(&verified, policy, &authority)?;
    let profile = selected_profile(&compiler, &authority, &revision.kind, verification)?;
    let _adapters = bind_models(&verified, &mut compiler, &authority, revision_id, root)?;
    let (mut plan, graph) = compiler.compile(&verified, revision_id, root)?;
    if profile == TaskKindProfile::OptimizationCandidate {
        (compiler, plan, _) = install_candidate_experiment(
            &verified,
            compiler,
            &authority,
            revision,
            revision_id,
            &graph,
            &plan,
        )?;
    }
    let plan_id = capture_execution_plan(&verified, revision_id, &plan)?;
    if opened.prepared_plan_id.as_deref() != Some(&plan_id) {
        return Err("Recompiled plan differs from the prepared Scenario Task opening".into());
    }
    let already_proposed = |current: &TaskProjection| {
        current.revision_id == opened.revision_id && current.plan_id.as_deref() == Some(&plan_id)
    };
    let developer = developer::host(cas, &authority, None);
    let trusted = developer::DecisionAuthority {
        compiler: &compiler,
        developer: developer.as_ref(),
    };
    let writer = format!("cli-{}", writer_pid());
    let Some(lease) = recovery::take_lease_checked_with(
        cas,
        store,
        &revision.task_id,
        &writer,
        already_proposed,
        |_, current| check_opening(current),
        journal_error,
    )?
    else {
        return Ok(());
    };
    let current = store
        .task_projection(cas, &revision.task_id)
        .map_err(journal_error)?
        .ok_or("Interrupted Scenario Task disappeared")?;
    // A writer could have committed between our pre-lease observation and the lease acquisition.
    let needs_proposal = if already_proposed(&current) {
        Ok(false)
    } else {
        check_opening(&current).map(|()| true)
    };
    match needs_proposal {
        Ok(true) => propose_prepared_plan(cas, store, &lease, &plan_id, &trusted),
        checked => {
            let released = store
                .release_task_lease(cas, &lease)
                .map(|_| ())
                .map_err(journal_error);
            checked.and(released)
        }
    }
}

pub(crate) fn plan(
    template: &Template,
    document: serde_json::Value,
    started_unix_ms: u64,
    performance: bool,
    predecessors: &BTreeMap<String, (TaskReceipt, String)>,
) -> Result<TaskTarget, TemplatePlanError> {
    let (repository, content, profile) = prepare(template, || capture(template))?;
    if (repository, content)
        != (
            template.repository_id.clone(),
            template.source_content_digest.clone(),
        )
    {
        return Err("Template catalog, pipeline, Provider or source authority changed".into());
    }
    let id = document["task_id"]
        .as_str()
        .ok_or("No fresh Task ID")?
        .to_owned();
    {
        let options = StartOptions {
            review_inputs: None,
            review_selectors: None,
            input_json: Vec::new(),
            file: PathBuf::from("scenario-task.json"),
            bindings: None,
            source_bindings: None,
            repo: template.repo.clone(),
            state: Some(template.state.clone()),
            authority: template.authority_commit.clone(),
            uncommitted: false,
            json: true,
            plan_only: true,
            timeout_secs: None,
            optimization_history: None,
            fit: false,
            replace: false,
        };
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        let code = prepare(template, || {
            start_kind_bytes_with_authority(
                options,
                bytes,
                None,
                Presentation::Silent,
                Some((&template.authority_id, started_unix_ms)),
            )
        })?;
        if code != 0 {
            return Err(format!("Native template planning refused with exit {code}").into());
        }
    }
    let opened = open_task(&id, &template.repo, Some(&template.state))?;
    validate_recorded_plan(template, &opened, predecessors, &document)?;
    if performance
        && matches!(
            profile,
            TaskKindProfile::Implementation
                | TaskKindProfile::ReviewedImplementation
                | TaskKindProfile::RepairAllowedImplementation
                | TaskKindProfile::Review
                | TaskKindProfile::OptimizationCandidate
        )
    {
        let plan_id = opened
            .task
            .plan_id
            .as_deref()
            .ok_or("Template Task has no native plan")?;
        let plan: ExecutionPlan = artifact(&opened.cas, plan_id, EXECUTION_PLAN_V4)?;
        let file: TaskFile = serde_json::from_value(document.clone()).map_err(|e| e.to_string())?;
        let limits = file
            .limits
            .ok_or("Performance template has no native limits")?;
        performance_wall(limits.wall_ms, limits.verification.wall_ms)?;
        performance_roles(
            &plan,
            if document["kind"] == "verification" || profile == TaskKindProfile::Review {
                "verification"
            } else {
                "implement"
            },
        )?;
    }
    if opened.task.revision.limits.tokens > template.max_tokens {
        return Err("Native plan widens template budget".into());
    }
    Ok(TaskTarget {
        repo: template.repo.clone(),
        state: template.state.clone(),
        task_id: id,
        plan_id: opened
            .task
            .plan_id
            .clone()
            .ok_or("Template Task has no plan")?,
        revision_id: opened.task.revision_id.clone(),
        max_tokens: template.max_tokens,
    })
}

/// `af task plan` deliberately reuses a matching Task file. A Scenario additionally pins
/// authority and exact predecessor receipts: the same file text alone cannot authorize reuse.
fn validate_recorded_plan(
    template: &Template,
    opened: &OpenedTask,
    predecessors: &BTreeMap<String, (TaskReceipt, String)>,
    document: &serde_json::Value,
) -> Result<(), String> {
    if opened.task.revision.authority.policy_id != template.authority_id {
        return Err("Recorded fresh Task widens pinned template authority".into());
    }
    let file: TaskFile =
        serde_json::from_value(template.document.clone()).map_err(|e| e.to_string())?;
    let limits = file.limits.ok_or("Template has no pinned limits")?;
    let revision = &opened.task.revision;
    if revision.limits.tokens != limits.tokens
        || revision.limits.max_attempts != limits.max_attempts
        || revision.limits.verification != limits.verification
        || opened.task.wall_ms() > limits.wall_ms
        || revision.kind != file.kind
        || revision.strategy != file.strategy.as_deref().unwrap_or_default()
        || revision.pipeline != file.pipeline
    {
        return Err(
            "Recorded fresh Task widens or changes pinned template limits or selection".into(),
        );
    }
    let bindings = recorded_input_bindings(&opened.cas, &opened.task.revision)?;
    let recorded = bindings.as_ref().map(|b| &b.bindings);
    if recorded.map_or(0, |b| b.len()) != predecessors.len() {
        return Err("Recorded fresh Task predecessor set differs".into());
    }
    for (input, (receipt, port)) in predecessors {
        let binding = recorded
            .and_then(|b| b.get(input))
            .ok_or("Missing recorded predecessor binding")?;
        let reference = binding
            .task
            .as_ref()
            .ok_or("Recorded binding lost native Task provenance")?;
        if reference.task_id != receipt.task_id
            || reference.task_revision_id != receipt.revision_id
            || reference.result_id != receipt.result_id
            || reference.port != *port
            || reference.acceptance != TaskAcceptanceV1::Satisfied
            || reference.domain_conclusion != receipt.result.domain_conclusion
        {
            return Err("Recorded fresh Task predecessor receipt changed".into());
        }
        let output = receipt
            .result
            .outputs
            .get(port)
            .ok_or("Predecessor output disappeared")?;
        if !output.artifact_ids.contains(&binding.artifact_id) {
            return Err("Recorded binding names a different predecessor artifact".into());
        }
    }
    if let Some(reference) = document.get("continue") {
        let (findings_id, findings) = continuation::recorded(&opened.cas, revision)?
            .ok_or("Native repair lost its recorded continuation")?;
        if Some(findings.continues.task_id.as_str()) != reference["task"].as_str() {
            return Err("Native repair continued another Task".into());
        }
        continuation::verify_findings(&opened.cas, &opened.store, &findings)?;
        let evidence: af_scenario::task_outcome::TaskOutcome =
            serde_json::from_value(document["requirements"]["scenario_native_outcome"].clone())
                .map_err(|e| e.to_string())?;
        let repair = evidence
            .repair
            .as_ref()
            .ok_or("Missing exact repair evidence")?;
        if findings_id != repair.findings_id
            || findings.continues.result_id != evidence.receipt.result_id
            || findings.continues.snapshot_id != repair.snapshot_id
        {
            return Err("Native repair continuation differs from exact outcome".into());
        }
    } else if !predecessors.contains_key("source")
        && let Some(source) = opened.task.revision.inputs.get("source")
    {
        let snapshot = source
            .snapshot_id
            .as_deref()
            .ok_or("Fresh Task source has no Snapshot")?;
        let (snapshot, manifest) = af_source_git::task::read_snapshot(&opened.cas, snapshot)?;
        let origin = af_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
        if manifest.content_digest() != template.source_content_digest
            || origin.repository_id() != template.repository_id
            || origin.source_revision() != Some(template.authority_commit.as_str())
            || snapshot.parent_snapshot_id.is_some()
        {
            return Err("Recorded fresh Task source differs from pinned committed source".into());
        }
    }
    Ok(())
}

/// Model-backed performance development must leave time for its native Provider admissions.
/// Command-only generic Tasks may use their whole wall for verification and are unchanged.
fn performance_wall(total: u64, verification: u64) -> Result<(), String> {
    if verification >= total {
        return Err(
            "Performance verification reserve must leave Task wall for Provider admission".into(),
        );
    }
    Ok(())
}

/// Production performance development requires the complete exact model team.
pub(super) fn performance_roles(plan: &ExecutionPlan, kind: &str) -> Result<(), String> {
    performance_executions(
        plan.bindings
            .iter()
            .map(|(slot, binding)| (slot.as_str(), &binding.execution)),
        kind,
    )
}
fn performance_executions<'a>(
    bindings: impl IntoIterator<Item = (&'a str, &'a WorkerExecutionV3)>,
    kind: &str,
) -> Result<(), String> {
    let mut roles = BTreeSet::new();
    for (slot, execution) in bindings {
        let WorkerExecutionV3::Model {
            provider,
            provider_kind,
            model,
            effort,
            ..
        } = execution
        else {
            return Err("Performance development roles require exact model bindings".into());
        };
        let role = slot.rsplit('.').next().ok_or("Missing performance role")?;
        let (expected_model, expected_effort) = match role {
            "implementer" => ("gpt-6.2-sol", "medium"),
            "bugs" => ("gpt-6.1-terra", "high"),
            "correctness" => ("gpt-6.1-terra", "high"),
            "evaluator" => ("gpt-6.1-terra", "medium"),
            _ => {
                return Err(format!(
                    "Performance roadmap refuses model role {role}; no advisory/default-light fallback"
                ));
            }
        };
        if provider != "codex-personal"
            || provider_kind != "codex"
            || model != expected_model
            || effort != expected_effort
        {
            return Err(format!(
                "Performance role {role} requires {expected_model} {expected_effort} on codex-personal"
            ));
        }
        if !roles.insert(role) {
            return Err("Performance model role is duplicated".into());
        }
    }
    if (kind == "verification" && roles.contains("implementer"))
        || (kind == "implement" && !roles.contains("implementer"))
    {
        return Err(
            "Performance implementation requires its implementer; verification is read-only".into(),
        );
    }
    if !["bugs", "correctness", "evaluator"]
        .iter()
        .all(|r| roles.contains(r))
    {
        return Err(
            "Performance loops require bugs, correctness and evaluator; no default-light fallback"
                .into(),
        );
    }
    Ok(())
}

/// Capture only checked native evidence; a missing continuation remains non-repairable.
pub(crate) fn terminal_outcome(
    opened: &OpenedTask,
    receipt: TaskReceipt,
    unknown_usage: bool,
) -> Result<af_scenario::task_outcome::TaskOutcome, String> {
    use af_scenario::task_outcome::{RepairEvidence, TaskOutcome};
    let actual = opened.task.execution.as_ref().map_or(Ok(0), |execution| {
        u64::try_from(execution.budget.committed_tokens()).map_err(|_| "Native charge overflow")
    })?;
    let unknown = opened.task.execution.as_ref().is_some_and(|execution| {
        execution
            .attempt_accounting()
            .iter()
            .any(|a| a.unknown_usage.is_some())
    });
    if actual != receipt.charged_tokens || unknown != unknown_usage {
        return Err("Native accounting differs from outcome".into());
    }
    let (_, view) = opened.view()?;
    let repair = match continuation::resolve(
        &opened.cas,
        &opened.store,
        &continuation::ContinueRefV1 {
            task: receipt.task_id.clone(),
            round: None,
        },
    ) {
        Ok(continued) => match continued.lineage {
            continuation::Lineage::Reviewed { findings_id } => {
                let record: af_contracts::task::findings::TaskReviewFindingsV1 = artifact(
                    &opened.cas,
                    &findings_id,
                    af_contracts::task::findings::TASK_REVIEW_FINDINGS_V1,
                )?;
                Some(RepairEvidence {
                    findings_id,
                    round_id: record.continues.round_id,
                    snapshot_id: record.continues.snapshot_id,
                })
            }
            _ => None,
        },
        Err(_) => None,
    };
    let candidate_snapshot_id = repair
        .as_ref()
        .map(|r| r.snapshot_id.clone())
        .or_else(|| {
            continuation::partial_candidate(&opened.cas, &opened.task)
                .map(|(_, _, _, partial)| partial.snapshot_id)
        })
        .or_else(|| {
            receipt
                .result
                .outputs
                .get("snapshot")
                .and_then(|p| p.snapshot_id.clone())
        });
    let value = TaskOutcome {
        schema: "af/scenario-task-outcome@2".into(),
        receipt,
        outcome: view
            .state
            .outcome
            .ok_or("Finished native Task has no outcome")?,
        cause: view.state.cause,
        unknown_usage,
        candidate_snapshot_id,
        checks_id: view.checks.map(|c| c.receipt_id),
        repair,
    };
    value.validate()?;
    Ok(value)
}

/// A Scenario continuation may name only the exact checked, terminal native Task in its Store.
pub(crate) fn check_repair(
    template: &Template,
    outcome: &af_scenario::task_outcome::TaskOutcome,
) -> Result<(), String> {
    outcome.validate()?;
    if !outcome.repairable() {
        return Err("Native outcome is not repairable".into());
    }
    let receipt = &outcome.receipt;
    let opened = open_task(&receipt.task_id, &template.repo, Some(&template.state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
    {
        return Err("Native repair outcome differs from recorded Task".into());
    }
    if terminal_outcome(&opened, receipt.clone(), outcome.unknown_usage)? != *outcome {
        return Err("Native repair evidence differs from recorded Task".into());
    }
    let repair = outcome
        .repair
        .as_ref()
        .ok_or("Missing native repair evidence")?;
    let (snapshot, _) = af_source_git::task::read_snapshot(&opened.cas, &repair.snapshot_id)?;
    let origin = af_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
    if origin.repository_id() != template.repository_id {
        return Err("Native repair belongs to another repository".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_experiment_evidence_follows_exact_unchanged_source_provenance_and_refuses_other_trees()
     {
        use af_source_git::task::capture_snapshot;
        use af_source_git::{Entry, EntryKind, Manifest};
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let empty = Manifest::new(vec![]).unwrap();
        let origin = cas
            .put_json(
                &json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"fixture",
            "source_revision":null,"content_digest":empty.content_digest()}),
            )
            .unwrap();
        let original = capture_snapshot(&cas, &empty, &origin, None).unwrap();
        let bound = cas.put_json(&json!({"schema":"af.task-source-origin/3","kind":"bound","repository_id":"fixture",
            "content_digest":empty.content_digest(),"bound_from":{"artifact_id":format!("sha256:{}","1".repeat(64)),
                "snapshot_id":original}})).unwrap();
        let rerooted = capture_snapshot(&cas, &empty, &bound, None).unwrap();
        experiment_source_matches(&cas, &rerooted, &original).unwrap();
        let foreign_origin = cas
            .put_json(
                &json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"other",
            "source_revision":null,"content_digest":empty.content_digest()}),
            )
            .unwrap();
        let unrelated = capture_snapshot(&cas, &empty, &foreign_origin, None).unwrap();
        assert!(experiment_source_matches(&cas, &unrelated, &original).is_err());
        let content = cas.put(b"changed").unwrap();
        let changed = Manifest::new(vec![Entry {
            path: "changed".into(),
            kind: EntryKind::File,
            content,
            size: 7,
        }])
        .unwrap();
        let changed = capture_snapshot(&cas, &changed, &bound, Some(&rerooted)).unwrap();
        assert!(experiment_source_matches(&cas, &changed, &original).is_err());
    }
    fn team() -> BTreeMap<String, WorkerExecutionV3> {
        [
            ("implementer", "gpt-6.2-sol", "medium"),
            ("bugs", "gpt-6.1-terra", "high"),
            ("correctness", "gpt-6.1-terra", "high"),
            ("evaluator", "gpt-6.1-terra", "medium"),
        ]
        .into_iter()
        .map(|(role, model, effort)| {
            (
                format!("root.slots.{role}"),
                WorkerExecutionV3::Model {
                    provider: "codex-personal".into(),
                    provider_kind: "codex".into(),
                    principal_id: Some("fixture".into()),
                    model: model.into(),
                    effort: effort.into(),
                    pi: None,
                },
            )
        })
        .collect()
    }
    fn check(team: &BTreeMap<String, WorkerExecutionV3>, kind: &str) -> Result<(), String> {
        performance_executions(
            team.iter()
                .map(|(name, execution)| (name.as_str(), execution)),
            kind,
        )
    }
    #[test]
    fn performance_team_is_exact_and_verification_has_no_implementer_or_advisory() {
        let team = team();
        check(&team, "implement").unwrap();
        assert!(check(&team, "verification").is_err());
        let mut verification = team.clone();
        verification.remove("root.slots.implementer");
        check(&verification, "verification").unwrap();
        assert!(check(&verification, "implement").is_err());
        for (slot, field, value) in [
            ("implementer", "model", "gpt-6.1-sol"),
            ("implementer", "effort", "high"),
            ("bugs", "effort", "medium"),
            ("evaluator", "model", "gpt-6-sol"),
            ("correctness", "model", "gpt-6-sol"),
            ("evaluator", "model", "gpt-6-astra"),
            ("evaluator", "model", "gpt-6.1-astra"),
            ("correctness", "model", "gpt-6.1-sol"),
            ("evaluator", "model", "gpt-6.1-sol"),
            ("correctness", "effort", "medium"),
            ("evaluator", "effort", "high"),
            ("correctness", "provider", "default"),
            ("implementer", "provider_kind", "pi"),
        ] {
            let mut wrong = team.clone();
            let execution = wrong.get_mut(&format!("root.slots.{slot}")).unwrap();
            let WorkerExecutionV3::Model {
                model,
                effort,
                provider,
                provider_kind,
                ..
            } = execution
            else {
                unreachable!()
            };
            match field {
                "model" => *model = value.into(),
                "effort" => *effort = value.into(),
                "provider_kind" => *provider_kind = value.into(),
                _ => *provider = value.into(),
            };
            assert!(check(&wrong, "implement").is_err());
            if slot != "implementer" {
                wrong.remove("root.slots.implementer");
                assert!(check(&wrong, "verification").is_err());
            }
        }
        assert!(check(&BTreeMap::new(), "implement").is_err());
        assert!(check(&BTreeMap::new(), "verification").is_err());
        let mut mixed = team.clone();
        mixed.insert("root.slots.bugs".into(), WorkerExecutionV3::Command {});
        assert!(check(&mixed, "implement").is_err());
        let command_only = team
            .keys()
            .map(|k| (k.clone(), WorkerExecutionV3::Command {}))
            .collect();
        assert!(check(&command_only, "implement").is_err());
        assert!(check(&command_only, "verification").is_err());
        let mut advisory = team.clone();
        advisory.insert("root.slots.uix".into(), team["root.slots.bugs"].clone());
        assert!(check(&advisory, "implement").is_err());
        let mut light = team;
        light.remove("root.slots.bugs");
        assert!(check(&light, "implement").is_err());
    }
    #[test]
    fn performance_reserve_leaves_provider_admission_wall_without_restricting_generic_tasks() {
        assert!(
            performance_wall(86400000, 86400000)
                .unwrap_err()
                .contains("Provider admission")
        );
        performance_wall(86400000, 43200000).unwrap();
        let doc = serde_json::json!({"schema":"af.task-file/1","task_id":"generic","kind":"verification","goal":"Read-only verification","pipeline":{"name":"fixture/verification","fallback":"refuse"},"strategy":"reviewed","limits":{"tokens":100,"max_attempts":4,"wall_ms":10000,"verification":{"tokens":50,"attempts":2,"wall_ms":10000}}});
        validate_document(&doc, None, 100).unwrap();
    }
    #[test]
    fn template_refuses_mutable_inputs_fallbacks_and_missing_limits() {
        let doc = serde_json::json!({"schema":"af.task-file/1","task_id":"template","kind":"implement","goal":"Bounded loop","pipeline":{"name":"fixture/implementation","fallback":"refuse"},"strategy":"small","verification":"evaluation","limits":{"tokens":20,"max_attempts":1,"wall_ms":60000,"verification":{"tokens":5,"attempts":1,"wall_ms":10000}}});
        validate_document(&doc, None, 20).unwrap();
        let mut native_verification = doc.clone();
        native_verification["kind"] = json!("verification");
        native_verification
            .as_object_mut()
            .unwrap()
            .remove("verification");
        validate_document(&native_verification, None, 20).unwrap();
        let mut research = native_verification.clone();
        research["kind"] = json!("document");
        validate_document(&research, None, 20).unwrap();
        let mut missing_verification = doc.clone();
        missing_verification
            .as_object_mut()
            .unwrap()
            .remove("verification");
        assert!(validate_document(&missing_verification, None, 20).is_err());
        for (field, value) in [
            (
                "inputs",
                json!({"source":{"task":"other","port":"snapshot"}}),
            ),
            ("base", json!({"commit":"HEAD"})),
            (
                "pipeline",
                json!({"name":"fixture/implementation","fallback":"generate"}),
            ),
            ("limits", json!(null)),
        ] {
            let mut wrong = doc.clone();
            wrong[field] = value;
            assert!(validate_document(&wrong, None, 20).is_err(), "{field}");
        }
        assert!(validate_document(&doc,Some(&json!({"schema":"af.task-bindings/1","packages":{"local/wider":{"path":"mutable","version":"1.0.0","digest":format!("sha256:{}","a".repeat(64))}}})),20).is_ok());
    }
}
