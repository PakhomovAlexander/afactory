//! Explicit source observation followed by one fenced revision/plan barrier. No Worker runs.
use super::*;
use review_config::task::selection::SelectionDecision;
use review_core::task::source::*;
use std::sync::Mutex;

struct CapturedSource {
    requirements: ArtifactEnvelope,
    capture_id: String,
    capture: TaskSourceCaptureV1,
    file_id: String,
    file: TaskFile,
}

fn original(cas: &Cas, revision: &TaskRevisionV1) -> Result<CapturedSource, String> {
    let input = revision
        .inputs
        .get("requirements")
        .ok_or("Task has no issue requirements")?;
    if input.artifact_ids.len() != 1 || input.artifact_type != "af/Requirements@1" {
        return Err("Task has no single issue requirements artifact".into());
    }
    let requirements: ArtifactEnvelope = serde_json::from_value(
        cas.get_json(&input.artifact_ids[0])
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    validate_envelope(&requirements)?;
    let mut captures = Vec::new();
    let mut files = Vec::new();
    for id in &requirements.input_artifacts {
        if cas
            .get_json(id)
            .ok()
            .is_some_and(|v| v["type"] == TASK_SOURCE_CAPTURE_V1)
        {
            let capture: TaskSourceCaptureV1 = artifact(cas, id, TASK_SOURCE_CAPTURE_V1)?;
            capture.validate()?;
            captures.push((id.clone(), capture));
        } else {
            let bytes = cas
                .get_bounded(id, 16 * 1024 * 1024)
                .map_err(|e| e.to_string())?;
            let file: TaskFile = parse(Path::new("task.json"), &bytes)
                .or_else(|_| parse(Path::new("task.toml"), &bytes))?;
            if file.schema != "af.task-file/1" || file.issue.is_none() {
                return Err("Task has no captured issue directive".into());
            }
            files.push((id.clone(), file));
        }
    }
    if captures.len() != 1 || files.len() != 1 {
        return Err("Refresh requires one captured Task definition and issue source".into());
    }
    let (capture_id, capture) = captures.pop().unwrap();
    let (file_id, file) = files.pop().unwrap();
    Ok(CapturedSource {
        requirements,
        capture_id,
        capture,
        file_id,
        file,
    })
}

fn same_observation(a: &TaskSourceCaptureV1, b: &TaskSourceCaptureV1) -> bool {
    a.adapter == b.adapter
        && a.external_id == b.external_id
        && a.external_key == b.external_key
        && a.source_revision == b.source_revision
        && a.fields == b.fields
        && (a.adapter != TaskSourceAdapterV1::JiraCloud || a.locator == b.locator)
}

pub(crate) fn refresh(
    id: &str,
    source_file: Option<&Path>,
    source_bindings: Option<&Path>,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let (repo, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    if !state.join("events.sqlite").is_file() {
        return Err("No common Task Store exists".into());
    }
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let before = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    if before.lease_until_unix_ms() > clock()? {
        return Err("Task has an active writer; refresh later".into());
    }
    if before.deliveries.last().is_some_and(|(_, delivery)| {
        delivery.status == review_core::task::delivery::TaskDeliveryStatusV1::Prepared
    }) {
        return Err("Resolve the pending local delivery before refreshing its Task".into());
    }
    let original = original(&cas, &before.revision)?;
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&before.revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // Check captured authority before any external read; model identity restoration is kept
    // under the renewable lease below because it can involve bounded adapter subprocesses.
    let base = restore_compiler(&cas, &before.revision.authority.policy_id, &authority)?;
    let requirements: NormalizedRequirementsV1 =
        serde_json::from_value(original.requirements.payload.clone()).map_err(|e| e.to_string())?;
    requirements.validate()?;
    let fresh = issue::capture_fresh(
        &cas,
        &repo,
        original.file.issue.as_ref().unwrap(),
        source_bindings,
        source_file,
        requirements.specification,
        &original.capture,
    )?;
    let capture: TaskSourceCaptureV1 = artifact(&cas, &fresh.capture_id, TASK_SOURCE_CAPTURE_V1)?;
    let current = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    if current.revision_id != before.revision_id || current.lease_until_unix_ms() > clock()? {
        return Err(
            "Task changed or acquired a writer during source capture; refresh again".into(),
        );
    }
    // Whitespace and unselected response fields alone do not invalidate an exact plan.
    if same_observation(&original.capture, &capture) {
        return present(&cas, &store, id, inspect.json, true);
    }
    let mut refs: Vec<_> = original
        .requirements
        .input_artifacts
        .into_iter()
        .filter(|r| r != &original.capture_id)
        .collect();
    refs.push(fresh.capture_id.clone());
    let requirements_id = cas
        .put_artifact(
            "af/Requirements@1",
            producer(),
            refs,
            None,
            serde_json::to_value(&fresh.requirements).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let mut revision = before.revision.clone();
    revision.revision = revision
        .revision
        .checked_add(1)
        .ok_or("Task revision overflow")?;
    revision.previous_revision_id = Some(before.revision_id.clone());
    revision.goal = format!("{}\n\n{}", original.file.goal, fresh.requirements.text);
    revision
        .inputs
        .get_mut("requirements")
        .unwrap()
        .artifact_ids = vec![requirements_id];
    revision.provenance.adapter_id = cas
        .put_json(&json!({"schema":"af.task-file-adapter/1",
        "source_file_id":original.file_id,"source_capture_id":fresh.capture_id}))
        .map_err(|e| e.to_string())?;
    revision.provenance.input_artifact_ids = revision
        .inputs
        .values()
        .flat_map(|p| p.artifact_ids.iter().cloned())
        .collect();
    revision.validate()?;
    let lease = store
        .take_task_lease(&cas, id, &format!("cli-{}", std::process::id()), 15_000)
        .map_err(|e| e.to_string())?;
    let locked = Mutex::new(&mut store);
    let outcome = review_pipeline::task::lease::with_heartbeat(&locked, &cas, &lease, || {
        let state = {
            let mut store = locked.lock().expect("Task Store");
            let state = store
                .task_projection(&cas, id)
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
            if state.revision_id != before.revision_id {
                return Err("Task revision changed before refresh acquired its lease".into());
            }
            store
                .recover_task_attempts(&cas, &lease)
                .map_err(|e| e.to_string())?;
            store
                .task_projection(&cas, id)
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?
        };
        let compiler = planning::restore(&cas, &state, &authority)?;
        let capacity = state.execution.as_ref().map_or_else(
            || state.revision.limits.clone(),
            |e| e.budget.remaining_limits(),
        );
        let assessment = selection::assess(&cas, &authority, &compiler, revision, Some(&capacity))?;
        let (compiler, revision_id, plan_id, waiting) = match assessment {
            selection::SelectionAssessment::Prepared(selection::PreparedSelection::Selected(
                selected,
            )) => {
                let plan_id = planning::persist_plan(&cas, &selected.plan)?;
                (selected.compiler, selected.revision_id, Some(plan_id), None)
            }
            selection::SelectionAssessment::Prepared(
                selection::PreparedSelection::Generation {
                    revision,
                    revision_id,
                },
            ) => {
                match planning::prepare_bootstrap(
                    &cas,
                    &authority,
                    base.clone(),
                    *revision,
                    revision_id.clone(),
                ) {
                    Ok(selected) => {
                        let plan_id = planning::persist_plan(&cas, &selected.plan)?;
                        (selected.compiler, selected.revision_id, Some(plan_id), None)
                    }
                    Err(reason) => {
                        eprintln!("Refreshed source needs planning prerequisites: {reason}");
                        (
                            base,
                            revision_id,
                            None,
                            Some(TaskWaitingReasonV1::NeedsHuman),
                        )
                    }
                }
            }
            selection::SelectionAssessment::Refused {
                revision,
                revision_id,
                decision,
                ..
            } => {
                debug_assert_eq!(revision.task_id, id);
                let reason = match decision {
                    SelectionDecision::Infeasible {} => TaskWaitingReasonV1::NeedsResources,
                    SelectionDecision::NeedsFacts { .. } => TaskWaitingReasonV1::NeedsInput,
                    _ => TaskWaitingReasonV1::NeedsHuman,
                };
                (compiler, revision_id, None, Some(reason))
            }
        };
        let developer = developer::host(&cas, &authority, None);
        let trusted = developer::DecisionAuthority {
            compiler: &compiler,
            developer: developer.as_ref(),
        };
        locked
            .lock()
            .expect("Task Store")
            .refresh_task_source(
                &cas,
                &lease,
                &revision_id,
                plan_id.as_deref(),
                waiting,
                &trusted,
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    drop(locked);
    release(&cas, &mut store, &lease, outcome)?;
    present(&cas, &store, id, inspect.json, true)
}
