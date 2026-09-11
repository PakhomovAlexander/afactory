//! Selection records preserve the exact request and trusted candidate assessments. Account
//! identity reads are token-free; capability admission remains in the selected runtime graph.
use super::*;
use review_config::task::selection::{CandidateState, PipelineSelection, SelectionDecision};

const SELECTION_V1: &str = "af/PipelineSelection@1";
type Adapters = BTreeMap<String, Box<dyn review_runner::task::WorkerModelAdapter>>;

pub(super) struct SelectedTask {
    pub revision: TaskRevisionV1,
    pub revision_id: String,
    pub compiler: TaskPlanCompiler,
    pub adapters: Adapters,
    pub plan: ExecutionPlanV1,
    pub graph: CompiledTask,
}

pub(super) enum PreparedSelection {
    Selected(Box<SelectedTask>),
    Generation {
        revision: Box<TaskRevisionV1>,
        revision_id: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedAdapter {
    schema: String,
    source_adapter_id: String,
    selection_id: String,
    request_revision_id: String,
}

pub(super) fn capture_revision(cas: &Cas, revision: &TaskRevisionV1) -> Result<String, String> {
    revision.validate()?;
    cas.put_artifact(
        TASK_REVISION_V1,
        producer(),
        vec![],
        None,
        serde_json::to_value(revision).map_err(|e| e.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|e| e.to_string())
}

pub(super) fn available_tools(
    cas: &Cas,
    compiler: &TaskPlanCompiler,
    authority: &RunAuthority,
    graph: &CompiledTask,
) -> Result<(), String> {
    for slot in graph.slots.values() {
        if let Some(worker) = compiler.worker(&slot.worker)
            && let TaskWorkerRunner::Command { command }
            | TaskWorkerRunner::LegacyTaskCommand { command, .. } = &worker.runner
            && crate::providers::resolve_program(&command.program).is_none()
        {
            return Err(format!(
                "Worker {} executable {} is unavailable",
                slot.worker, command.program
            ));
        }
    }
    if graph.nodes.values().any(|node| {
        matches!(
            &node.operator,
            review_graph::task::CompiledOperator::Primitive {
                operator: pipeline::TaskOperatorV1::Check { .. },
                ..
            }
        )
    }) {
        let policy: CodeTaskPolicy = serde_json::from_value(
            cas.get_json(&authority.code_policy_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if !policy.require_container {
            for check in policy.checks.values().filter(|check| check.required) {
                // Relative source executables may be produced by the implementation. A
                // host capability lookup cannot decide whether those future artifacts exist.
                if !check.command.program.contains('/')
                    && crate::providers::resolve_program(&check.command.program).is_none()
                {
                    return Err(format!(
                        "Required check {} executable {} is unavailable",
                        check.name, check.command.program
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn prepare(
    cas: &Cas,
    authority: &RunAuthority,
    compiler: &TaskPlanCompiler,
    mut request: TaskRevisionV1,
    json_output: bool,
) -> Result<Option<PreparedSelection>, String> {
    request.provenance.input_artifact_ids = request
        .inputs
        .values()
        .flat_map(|port| port.artifact_ids.iter().cloned())
        .collect();
    let request_id = capture_revision(cas, &request)?;
    let requested = request.pipeline.as_ref();
    let empty = BTreeMap::new();
    let priorities = authority.selection.get(&request.strategy).unwrap_or(&empty);
    let mut admitted = BTreeMap::new();
    let selection = review_config::task::selection::select_with_policy(
        &request,
        requested,
        compiler.pipelines(),
        priorities,
        authority.no_match,
        |root| {
            let mut candidate = compiler.clone();
            let (mut revision, graph, resources) =
                match candidate.candidate_structure(cas, &request, root) {
                    Ok(value) => value,
                    Err(reason) => return CandidateState::Invalid { reason },
                };
            if !resources.is_empty() {
                return CandidateState::Infeasible {
                    reason: resources.join("; "),
                };
            }
            revision.provenance.input_artifact_ids = revision
                .inputs
                .values()
                .flat_map(|port| port.artifact_ids.iter().cloned())
                .collect();
            let revision_id = match capture_revision(cas, &revision) {
                Ok(id) => id,
                Err(reason) => return CandidateState::Invalid { reason },
            };
            if let Err(reason) = available_tools(cas, &candidate, authority, &graph) {
                return CandidateState::Unavailable { reason };
            }
            let adapters = match bind_models(cas, &mut candidate, authority, &revision_id, root) {
                Ok(adapters) => adapters,
                Err(reason) => return CandidateState::Unavailable { reason },
            };
            let now = match clock() {
                Ok(now) => now,
                Err(reason) => return CandidateState::Unavailable { reason },
            };
            match candidate.candidate_resources(cas, graph, &revision.limits, now) {
                Err(reason) => return CandidateState::Invalid { reason },
                Ok(reasons) if !reasons.is_empty() => {
                    return CandidateState::Infeasible {
                        reason: reasons.join("; "),
                    };
                }
                Ok(_) => (),
            }
            if let Err(reason) = candidate.compile(cas, &revision_id, root) {
                return CandidateState::Invalid { reason };
            }
            admitted.insert(root.to_owned(), (revision, candidate, adapters));
            CandidateState::Fit {}
        },
    )?;
    let selection_id = cas
        .put_artifact(
            SELECTION_V1,
            producer(),
            vec![request_id.clone()],
            None,
            serde_json::to_value(&selection).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let SelectionDecision::Selected { pipeline } = &selection.decision else {
        if selection.decision == (SelectionDecision::NeedsGeneration {})
            && authority.planner.is_some()
        {
            request.provenance.adapter_id = cas
                .put_json(
                    &serde_json::to_value(SelectedAdapter {
                        schema: "af.selected-task-adapter/1".into(),
                        source_adapter_id: request.provenance.adapter_id,
                        selection_id,
                        request_revision_id: request_id,
                    })
                    .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            let revision_id = capture_revision(cas, &request)?;
            return Ok(Some(PreparedSelection::Generation {
                revision: Box::new(request),
                revision_id,
            }));
        }
        let view = json!({"schema":"af/task-selection@1", "task_id":request.task_id,
            "selection_id": selection_id, "request_revision_id":request_id,
            "attempts":0, "chargeable_tokens":0, "selection":selection});
        if json_output {
            println!(
                "{}",
                serde_json::to_string(&view).map_err(|e| e.to_string())?
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
            );
        }
        eprintln!(
            "No Pipeline selected: {}",
            serde_json::to_string(&selection.decision).map_err(|e| e.to_string())?
        );
        return Ok(None);
    };
    let (mut revision, compiler, adapters) = admitted
        .remove(pipeline)
        .ok_or("Selected Pipeline was not admitted")?;
    revision.provenance.adapter_id = cas
        .put_json(
            &serde_json::to_value(SelectedAdapter {
                schema: "af.selected-task-adapter/1".into(),
                source_adapter_id: request.provenance.adapter_id,
                selection_id,
                request_revision_id: request_id,
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let revision_id = capture_revision(cas, &revision)?;
    let (plan, graph) = compiler.compile(cas, &revision_id, pipeline)?;
    Ok(Some(PreparedSelection::Selected(Box::new(SelectedTask {
        revision,
        revision_id,
        compiler,
        adapters,
        plan,
        graph,
    }))))
}

pub(super) fn recorded(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<serde_json::Value>, String> {
    let value = cas
        .get_json(&revision.provenance.adapter_id)
        .map_err(|e| e.to_string())?;
    if value["schema"] != "af.selected-task-adapter/1" {
        return Ok(None);
    }
    let adapter: SelectedAdapter = serde_json::from_value(value).map_err(|e| e.to_string())?;
    cas.verify(&adapter.source_adapter_id)
        .map_err(|e| e.to_string())?;
    let _: TaskRevisionV1 = artifact(cas, &adapter.request_revision_id, TASK_REVISION_V1)?;
    let selection: PipelineSelection = artifact(cas, &adapter.selection_id, SELECTION_V1)?;
    Ok(Some(
        json!({"artifact_id": adapter.selection_id, "request_revision_id":adapter.request_revision_id, "assessment": selection}),
    ))
}
