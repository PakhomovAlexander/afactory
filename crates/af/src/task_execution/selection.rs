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

pub(super) enum SelectionAssessment {
    Prepared(PreparedSelection),
    Refused {
        revision: Box<TaskRevisionV1>,
        revision_id: String,
        decision: SelectionDecision,
        view: serde_json::Value,
        /// The smallest limits the refused candidate fits, for an infeasible refusal.
        fit: Option<Box<review_core::task::limits_fit::TaskLimitsFitV1>>,
    },
}

/// The document a refused selection prints.
pub(super) const TASK_SELECTION_V2: &str = "af/task-selection@2";

/// The clock a selection judges the deadline by: `now`, and the wall the Task has from it. A
/// plan that has not run judges its whole declared wall, since its clock starts at the first
/// `af task run`; a refresh judges what is left.
#[derive(Clone, Copy)]
pub(super) struct PlanClock {
    pub now: u64,
    pub wall_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedAdapter {
    schema: String,
    source_adapter_id: String,
    selection_id: String,
    request_revision_id: String,
}

/// Every port's artifact identity, plus the one record no port carries that a revision must
/// keep: the `af/TaskInputBindings@1` record of a Task file's `inputs` table (ADR-0117). The
/// Store derives the same list when it validates a refreshed revision. A Task without bindings
/// keeps byte for byte the list of port identities it has always had, whatever else its adapter
/// recorded beside them.
fn provenance_input_artifact_ids(cas: &Cas, revision: &TaskRevisionV1) -> Vec<String> {
    review_store::store::task::revision_provenance_inputs(cas, revision)
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
            && let TaskWorkerRunner::Command { command } = &worker.runner
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
            cas.get_json(authority.code_policy_id()?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        // A check a node lists in `remote_checks` runs in the repository's CI, never here
        // (ADR-0140): its command's executable is not this machine's to provide.
        let local: std::collections::BTreeSet<&String> = graph
            .nodes
            .values()
            .filter_map(|node| match &node.operator {
                review_graph::task::CompiledOperator::Primitive {
                    operator: pipeline::TaskOperatorV1::Check { checks, .. },
                    ..
                } => Some(checks),
                _ => None,
            })
            .flatten()
            .collect();
        if !policy.require_container {
            for check in policy
                .checks
                .values()
                .filter(|check| check.required && local.contains(&check.name))
            {
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

/// What a refused plan prints: with `--json`, the `af/task-selection@2` document on stdout; in
/// either mode, the refusal and its reasons on stderr. With `fit`, an infeasible refusal whose
/// fit no Pipeline bound blocks is handed back for `af task plan --fit` to plan again.
pub(super) enum Prepared {
    Ready(PreparedSelection),
    Refused,
    Refit(Box<review_core::task::limits_fit::TaskLimitsFitV1>),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    cas: &Cas,
    authority: &RunAuthority,
    compiler: &TaskPlanCompiler,
    request: TaskRevisionV1,
    json_output: bool,
    clock: PlanClock,
    fit_requested: bool,
    file: &Path,
) -> Result<Prepared, String> {
    match assess(cas, authority, compiler, request, None, clock)? {
        SelectionAssessment::Prepared(value) => Ok(Prepared::Ready(value)),
        SelectionAssessment::Refused {
            fit: Some(fit),
            decision: SelectionDecision::Infeasible {},
            ..
        } if fit_requested && fit.fixable() && !fit.raised().is_empty() => Ok(Prepared::Refit(fit)),
        SelectionAssessment::Refused { decision, view, .. } => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&view).map_err(|e| e.to_string())?
                );
            }
            eprint!("{}", refusal_text(&decision, &view, file)?);
            Ok(Prepared::Refused)
        }
    }
}

/// The closed decision, then each refused candidate's own reason. A Provider admission that
/// refused an incompatible CLI or one that cannot start reads as itself on stderr, not only as
/// `{"kind":"unavailable"}` (ADR-0146); the document on stdout carries the same reasons.
pub(super) fn refusal_line(
    decision: &SelectionDecision,
    view: &serde_json::Value,
) -> Result<String, String> {
    let mut line = format!(
        "No Pipeline selected: {}",
        serde_json::to_string(decision).map_err(|e| e.to_string())?
    );
    for candidate in view["selection"]["candidates"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let (Some(pipeline), Some(reason)) = (
            candidate["pipeline"].as_str(),
            candidate["state"]["reason"].as_str(),
        ) {
            line.push_str(&format!("; {pipeline}: {reason}"));
        }
    }
    Ok(line)
}

/// The refusal as stderr prints it in either mode: the decision line, one line per refused
/// candidate's reason, and for an infeasible plan the smallest limits that fit and the command
/// that plans with them.
pub(super) fn refusal_text(
    decision: &SelectionDecision,
    view: &serde_json::Value,
    file: &Path,
) -> Result<String, String> {
    let mut out = format!(
        "No Pipeline selected for Task {}: {}\n",
        preview::text(view["task_id"].as_str().unwrap_or("?")),
        decision_words(decision)
    );
    for candidate in view["selection"]["candidates"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let pipeline = preview::text(candidate["pipeline"].as_str().unwrap_or("?"));
        if let Some(reason) = candidate["state"]["reason"].as_str() {
            out.push_str(&format!("  {pipeline}: {}\n", preview::text(reason)));
        }
        for reason in candidate["state"]["reasons"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(reason) = reason.as_str() {
                out.push_str(&format!("  {pipeline}: {}\n", preview::text(reason)));
            }
        }
        if let Some(facts) = candidate["state"]["facts"].as_array() {
            let facts = facts
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(preview::text)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "  {pipeline}: needs the facts {facts}; add them to the Task file's `facts`\n"
            ));
        }
    }
    let diagnostic = &view["planning_diagnostic"];
    if let Some(required) = diagnostic.get("required_limits") {
        out.push_str(&format!(
            "  the smallest limits that fit: {}\n",
            limits_text(required)
        ));
        if required["wall_ms"] != diagnostic["supplied_limits"]["wall_ms"] {
            out.push_str(&format!(
                "  wall_ms keeps {} ms beyond the mandatory Attempts' {} ms and the verification \
                 reserve, for the start of the run\n",
                review_graph::task::FIT_WALL_START_MS,
                diagnostic["mandatory"]["wall_ms"]
            ));
        }
        for slot in diagnostic["reserves"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "  verification reserve {}: {} Attempts x {} tokens, {} ms each\n",
                preview::text(slot["node"].as_str().unwrap_or("?")),
                slot["attempts"],
                slot["tokens_per_attempt"],
                slot["wall_ms_per_attempt"]
            ));
        }
        for reason in diagnostic["unfixable"].as_array().into_iter().flatten() {
            if let Some(reason) = reason.as_str() {
                out.push_str(&format!(
                    "  no Task limit fixes: {}\n",
                    preview::text(reason)
                ));
            }
        }
    }
    if let Some(next) = view["next_action"].as_str() {
        out.push_str(&format!("next: {next}\n"));
    } else if decision == &(SelectionDecision::Infeasible {}) {
        out.push_str(&format!(
            "next: af task plan --file {} --fit\n",
            shell_words::quote(&file.display().to_string())
        ));
    }
    Ok(out)
}

fn decision_words(decision: &SelectionDecision) -> String {
    match decision {
        SelectionDecision::Infeasible {} => {
            "infeasible: the Task's limits are too small for the plan".into()
        }
        SelectionDecision::NeedsFacts { facts } => format!(
            "needs facts: {}",
            facts.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        SelectionDecision::Unavailable {} => {
            "unavailable: a Worker or check cannot run here".into()
        }
        SelectionDecision::Ambiguous { pipelines } => format!(
            "ambiguous: {} fit equally; name one with `pipeline` in the Task file",
            pipelines.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        SelectionDecision::Invalid {} => "invalid: no candidate compiles".into(),
        SelectionDecision::Refused {} => "refused: no candidate accepts the Task".into(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `tokens 400, max_attempts 3, wall_ms 60000, verification tokens 200 attempts 2 wall_ms 10000`.
pub(super) fn limits_text(limits: &serde_json::Value) -> String {
    format!(
        "tokens {}, max_attempts {}, wall_ms {}, verification tokens {} attempts {} wall_ms {}",
        limits["tokens"],
        limits["max_attempts"],
        limits["wall_ms"],
        limits["verification"]["tokens"],
        limits["verification"]["attempts"],
        limits["verification"]["wall_ms"]
    )
}

pub(super) fn assess(
    cas: &Cas,
    authority: &RunAuthority,
    compiler: &TaskPlanCompiler,
    mut request: TaskRevisionV1,
    capacity: Option<&TaskLimitsV1>,
    clock: PlanClock,
) -> Result<SelectionAssessment, String> {
    let mut fits: BTreeMap<String, review_core::task::limits_fit::TaskLimitsFitV1> =
        BTreeMap::new();
    request.provenance.input_artifact_ids = provenance_input_artifact_ids(cas, &request);
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
                // The Workers are not bound yet, so the fit leaves Provider admission out;
                // `--fit` plans again and refits once they are.
                if let Ok(fit) = candidate.candidate_fit(
                    cas,
                    graph.clone(),
                    capacity.unwrap_or(&revision.limits),
                    clock.wall_ms,
                ) {
                    fits.insert(root.to_owned(), fit);
                }
                return CandidateState::Infeasible {
                    reason: resources.join("; "),
                };
            }
            // Each Brief port's receipt is this revision's, and no Worker wired to a Brief may
            // be handed more than the request renderer's cap (ADR-0160).
            let briefs = candidate.pipelines().get(root).map_or(Ok(()), |pipeline| {
                crate::project_brief::record_receipts(cas, &pipeline.contract, &mut revision)
            });
            if let Err(reason) = briefs.and_then(|()| {
                crate::project_brief::check_request_cap(cas, &candidate, &graph, &revision)
            }) {
                return CandidateState::Invalid { reason };
            }
            revision.provenance.input_artifact_ids = provenance_input_artifact_ids(cas, &revision);
            // A pipeline with remote checks publishes to its repository's push target, and its
            // authority says so before the revision has an identity (ADR-0140).
            if let Err(reason) = domain::remote_check_authority(cas, &mut revision, &graph) {
                return CandidateState::Unavailable { reason };
            }
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
            let limits = capacity.unwrap_or(&revision.limits);
            match candidate.candidate_resources(cas, graph.clone(), limits, clock.now) {
                Err(reason) => return CandidateState::Invalid { reason },
                Ok(reasons) if !reasons.is_empty() => {
                    if let Ok(fit) = candidate.candidate_fit(cas, graph, limits, clock.wall_ms) {
                        fits.insert(root.to_owned(), fit);
                    }
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
            return Ok(SelectionAssessment::Prepared(
                PreparedSelection::Generation {
                    revision: Box::new(request),
                    revision_id,
                },
            ));
        }
        // The infeasible candidate the Task named, or the first one assessed: its smallest
        // limits that fit, computed by the planner from the reserves it protects.
        let fit = (selection.decision == SelectionDecision::Infeasible {})
            .then(|| {
                selection
                    .candidates
                    .iter()
                    .filter(|c| matches!(c.state, CandidateState::Infeasible { .. }))
                    .find_map(|c| fits.get(&c.pipeline).map(|fit| (c, fit.clone())))
            })
            .flatten();
        let mut view = json!({"schema":TASK_SELECTION_V2, "task_id":request.task_id,
            "selection_id": selection_id, "request_revision_id":request_id,
            "attempts":0, "chargeable_tokens":0, "selection":selection});
        if let Some((candidate, fit)) = &fit {
            let CandidateState::Infeasible { reason } = &candidate.state else {
                unreachable!("filtered to infeasible candidates");
            };
            view["planning_diagnostic"] = json!({
                "kind": "infeasible",
                "pipeline": candidate.pipeline,
                "reason": reason,
                "supplied_limits": fit.supplied,
                "required_limits": fit.required,
                "raised": fit.raised().into_iter().map(|(name, supplied, required)| {
                    json!({"limit": name, "supplied": supplied, "required": required})
                }).collect::<Vec<_>>(),
                "reserves": fit.reserves,
                "mandatory": fit.mandatory,
                "unfixable": fit.unfixable,
            });
        }
        let fit = fit.map(|(_, fit)| Box::new(fit));
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
        return Ok(SelectionAssessment::Refused {
            revision: Box::new(request),
            revision_id,
            decision: selection.decision,
            view,
            fit,
        });
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
    Ok(SelectionAssessment::Prepared(PreparedSelection::Selected(
        Box::new(SelectedTask {
            revision,
            revision_id,
            compiler,
            adapters,
            plan,
            graph,
        }),
    )))
}

pub(super) fn recorded(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<serde_json::Value>, String> {
    let bytes = cas
        .get(&revision.provenance.adapter_id)
        .map_err(|e| e.to_string())?;
    // Adapter provenance may be a verified opaque engine blob. Selection is optional
    // metadata; only its declared JSON schema selects the strict adapter decoder below.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A Task whose adapter recorded an observation no port carries — ADR-0116's
    /// `af/UndeclaredAfPaths@1`, written for a source-less profile — keeps the provenance list
    /// it had before bindings existed, so its revision bytes, and with them its revision ID,
    /// its plan and every inspection document, are exactly what they were. Only the binding
    /// record is preserved beside the port identities.
    #[test]
    fn only_a_binding_record_is_preserved_beside_the_port_identities() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut revision: TaskRevisionV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/task-revision.json"
        ))
        .unwrap();
        let put = |kind: &str| {
            let payload = json!({"fixture": kind});
            let stored = cas.put_artifact(kind, producer(), vec![], None, payload);
            stored.unwrap().0
        };
        let ports: Vec<String> = revision
            .inputs
            .values()
            .flat_map(|port| port.artifact_ids.iter().cloned())
            .collect();
        // Exactly what this adapter wrote before bindings existed: the port identities alone.
        let mut unchanged = revision.clone();
        unchanged.provenance.input_artifact_ids = ports.clone();
        let expected = serde_json::to_vec(&unchanged).unwrap();

        let undeclared = put(UNDECLARED_AF_PATHS_V1);
        let blob = cas
            .put(b"an opaque engine blob is not a typed record")
            .unwrap();
        let absent = format!("sha256:{}", "7".repeat(64));
        revision.provenance.input_artifact_ids = vec![undeclared.clone(), blob, absent];
        revision.provenance.input_artifact_ids = provenance_input_artifact_ids(&cas, &revision);
        assert_eq!(revision.provenance.input_artifact_ids, ports);
        assert_eq!(serde_json::to_vec(&revision).unwrap(), expected);

        let record = put(review_core::task::input_bindings::TASK_INPUT_BINDINGS_V1);
        revision.provenance.input_artifact_ids = vec![undeclared, record.clone()];
        let kept = provenance_input_artifact_ids(&cas, &revision);
        assert_eq!(kept, [ports, vec![record]].concat());
    }

    #[test]
    fn optional_selection_preserves_opaque_provenance_and_rejects_invalid_selected_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut revision: TaskRevisionV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/task-revision.json"
        ))
        .unwrap();
        for bytes in [
            b"engine identity\0\xff".as_slice(),
            b"{\"schema\":\"another-adapter/1\"}",
        ] {
            revision.provenance.adapter_id = cas.put(bytes).unwrap();
            assert!(recorded(&cas, &revision).unwrap().is_none());
        }
        revision.provenance.adapter_id = cas
            .put_json(&json!({"schema":"af.selected-task-adapter/1"}))
            .unwrap();
        assert!(recorded(&cas, &revision).is_err());
        revision.provenance.adapter_id = format!("sha256:{}", "0".repeat(64));
        assert!(recorded(&cas, &revision).is_err());
    }
}
