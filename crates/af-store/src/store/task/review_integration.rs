//! Native Review receipts bind only the common Task ledger. No Campaign projection is read.
use super::*;
use af_contracts::task::review::{
    ReviewConclusionV1, TASK_REVIEW_CONCLUSION_V1, TASK_REVIEW_ROUND_V1, TaskReviewConclusionV1,
};

/// Recomputed by the result assembler and by Store replay/finish. The ordinary Task append
/// transaction fences the entire prefix, including late usage and writer replacement.
pub fn native_review_conclusion(
    cas: &Cas,
    state: &TaskProjection,
    result: &task::TaskResultV1,
) -> Result<Option<String>, StoreError> {
    require_native_successors(cas, state, result)?;
    if state.revision.acceptance.is_empty()
        || !state
            .revision
            .acceptance
            .values()
            .all(|a| a.evidence_type == TASK_REVIEW_ROUND_V1)
    {
        return Ok(None);
    }
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Review has no common execution"))?;
    let report_id = state
        .run_reports
        .last()
        .ok_or_else(|| conflict("Review has no complete Task report"))?;
    let report = super::report::read_task_run_report(cas, report_id)?;
    if report.task_revision_id != state.revision_id
        || Some(&report.plan_id) != state.plan_id.as_ref()
    {
        return Err(conflict(
            "Native Review conclusion requires its current complete Task report",
        ));
    }
    let conclusion = match result.domain_conclusion.as_str() {
        "pass" => ReviewConclusionV1::Pass,
        "changes_requested" => ReviewConclusionV1::ChangesRequested,
        "convergence_exhausted" => ReviewConclusionV1::ConvergenceExhausted,
        "incomplete" => ReviewConclusionV1::Incomplete,
        _ => return Err(conflict("Unknown native Review conclusion")),
    };
    let round_ids = execution
        .graph
        .coverage
        .values()
        .filter_map(|address| {
            execution
                .outputs
                .get(&address.node)
                .and_then(|(_, output)| output.outputs.get(&address.port))
        })
        .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
        .flat_map(|p| p.artifact_ids.iter().cloned())
        .collect();
    let value = TaskReviewConclusionV1 {
        task_revision_id: state.revision_id.clone(),
        plan_id: report.plan_id,
        task_report_id: report_id.clone(),
        through_sequence: state.next_sequence,
        cumulative_chargeable_tokens: execution.budget.committed_tokens().into(),
        begun_attempts: execution.budget.begun_attempts(),
        round_ids,
        conclusion,
    };
    value.validate().map_err(conflict)?;
    cas.put_artifact(
        TASK_REVIEW_CONCLUSION_V1,
        af_contracts::Producer::KernelOperation {
            run_id: task_run_id(&state.task_id)?,
            node_id: None,
            operation_id: "native-review-conclusion@1".into(),
        },
        value.artifact_refs(),
        None,
        serde_json::to_value(value)?,
    )
    .map(|(id, _)| Some(id))
    .map_err(|e| StoreError::Artifact(e.to_string()))
}

/// A passing commit cannot be accepted on initial, unrelated or incomplete Review evidence.
/// Called even for embedded Review, before the standalone conclusion-specific projection.
pub fn require_native_successors(
    cas: &Cas,
    state: &TaskProjection,
    result: &task::TaskResultV1,
) -> Result<(), StoreError> {
    if result.acceptance != task::TaskAcceptanceV1::Satisfied
        && !matches!(result.domain_conclusion.as_str(), "pass" | "verified")
    {
        return Ok(());
    }
    let Some(execution) = &state.execution else {
        return Ok(());
    };
    let routes = execution
        .graph
        .native_integration_successors()
        .map_err(conflict)?;
    if routes.is_empty() {
        return Ok(());
    }
    if result.acceptance != task::TaskAcceptanceV1::Satisfied {
        return Err(conflict(
            "Native Integration success requires satisfied exact successor acceptance",
        ));
    }
    let selected = selected_native_rounds(cas, state)?;
    for (node, successors) in routes {
        let Some((_, output)) = execution.outputs.get(&node) else {
            continue;
        };
        let port = output
            .outputs
            .get("result")
            .ok_or_else(|| conflict("Native commit lost its receipt"))?;
        let id = port
            .artifact_ids
            .first()
            .ok_or_else(|| conflict("Native commit lost its identity"))?;
        let commit: task::native_review::TaskReviewCommitV1 =
            payload(cas, id, task::native_review::TASK_REVIEW_COMMIT_V1)?;
        commit.validate().map_err(conflict)?;
        if execution.invocations.get(&node).map(|(_, input)| input) != Some(&commit.invocation) {
            return Err(conflict(
                "Native commit changed its exact admitted invocation",
            ));
        }
        if commit.outcome != task::pipeline::ReceiptOutcomeV1::Passed {
            continue;
        }
        let public_snapshot = selected_public_snapshot(cas, result)?;
        let mut complete = false;
        for successor in successors {
            let Some((_, output)) = execution.outputs.get(&successor) else {
                continue;
            };
            let Some(port) = output.outputs.get("result") else {
                continue;
            };
            for round_id in &port.artifact_ids {
                let round: task::review::TaskReviewRoundV1 =
                    payload(cas, round_id, TASK_REVIEW_ROUND_V1)?;
                round.validate().map_err(conflict)?;
                if execution
                    .invocations
                    .get(&successor)
                    .map(|(_, input)| input)
                    != Some(&round.invocation)
                {
                    return Err(conflict(
                        "Native successor changed its exact admitted invocation",
                    ));
                }
                let history = round.invocation.inputs.get("history");
                let subject = round.invocation.inputs.get("subject");
                let Some(subject_id) = subject.and_then(|p| p.artifact_ids.first()) else {
                    continue;
                };
                let subject: task::review::TaskReviewSubject =
                    payload(cas, subject_id, task::review::TASK_REVIEW_SUBJECT_V3)?;
                subject.validate().map_err(conflict)?;
                let mut selected_successor = false;
                for (selected_id, selected_round) in &selected {
                    selected_successor |= selected_round.conclusion
                        != ReviewConclusionV1::Incomplete
                        && native_round_descends_from(cas, state, selected_id, round_id)?
                        && public_snapshot
                            .as_ref()
                            .is_none_or(|snapshot| snapshot == &selected_round.snapshot_id);
                }
                complete |= selected_successor
                    && round.conclusion != ReviewConclusionV1::Incomplete
                    && round.invocation.plan_id == commit.invocation.plan_id
                    && round.snapshot_id == commit.snapshot_id
                    && history.is_some_and(|p| p.artifact_ids == [commit.prior_history_id.clone()])
                    && subject.round == round.round
                    && subject.snapshot_id == commit.snapshot_id
                    && subject.prior_history_id == commit.prior_history_id
                    && subject.continuation_id.as_ref() == Some(id);
            }
        }
        if !complete {
            return Err(conflict(
                "Native Integration requires its complete exact committed successor Round before acceptance",
            ));
        }
        let standalone = state
            .revision
            .acceptance
            .values()
            .all(|a| a.evidence_type == TASK_REVIEW_ROUND_V1);
        let expected = if selected
            .iter()
            .any(|(_, r)| r.conclusion == ReviewConclusionV1::ConvergenceExhausted)
        {
            "convergence_exhausted"
        } else if selected
            .iter()
            .any(|(_, r)| r.conclusion == ReviewConclusionV1::ChangesRequested)
        {
            "changes_requested"
        } else {
            "pass"
        };
        if (standalone && result.domain_conclusion != expected)
            || (!standalone
                && selected
                    .iter()
                    .any(|(_, r)| r.outcome != task::pipeline::ReceiptOutcomeV1::Passed))
        {
            return Err(conflict(
                "Native Integration acceptance discarded its selected successor verdict",
            ));
        }
    }
    Ok(())
}

/// Read the actual selected coverage, unwrapping only typed Review acceptance. Dependency
/// reachability is intentionally irrelevant: Select has already transported one exact value.
fn selected_native_rounds(
    cas: &Cas,
    state: &TaskProjection,
) -> Result<BTreeMap<String, task::review::TaskReviewRoundV1>, StoreError> {
    use af_graph::task::{Address, CompiledOperator};
    use task::pipeline::TaskOperatorV1;
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Review has no execution"))?;
    let mut addresses: Vec<&Address> = execution.graph.coverage.values().collect();
    if let Some(address) = execution.graph.outputs.get("snapshot") {
        addresses.push(address);
    }
    let mut ids = BTreeSet::new();
    for address in addresses {
        let Some((_, output)) = execution.outputs.get(&address.node) else {
            continue;
        };
        let Some(port) = output.outputs.get(&address.port) else {
            continue;
        };
        if port.artifact_type == TASK_REVIEW_ROUND_V1 {
            ids.extend(port.artifact_ids.iter().cloned());
            continue;
        }
        // Bound SourceTrees transport source authority, not ReviewAccept evidence. A root
        // capture imported from another Task keeps its original producer (ADR-0117).
        if port.artifact_type == "af/SourceTree@1"
            && execution.graph.inputs.values().any(|input| input == port)
        {
            continue;
        }
        // An embedded Review verdict is retained by ReviewedImplementation or its public
        // SourceTree. Both must name the exact admitted ReviewAccept invocation and output.
        for id in &port.artifact_ids {
            let artifact = cas
                .get_artifact(id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            let af_contracts::Producer::KernelOperation {
                run_id,
                node_id: Some(node),
                ..
            } = &artifact.producer
            else {
                continue;
            };
            if run_id != &task_run_id(&state.task_id)? {
                return Err(conflict("Native acceptance changed its Task producer"));
            }
            if !matches!(
                execution.graph.nodes.get(node).map(|n| &n.operator),
                Some(CompiledOperator::Primitive {
                    operator: TaskOperatorV1::ReviewAccept {},
                    ..
                })
            ) {
                continue;
            }
            let input = &execution
                .invocations
                .get(node)
                .ok_or_else(|| conflict("Native acceptance lost its admitted invocation"))?
                .1;
            if !execution
                .outputs
                .get(node)
                .is_some_and(|(_, out)| out.outputs.values().any(|p| p == port))
            {
                return Err(conflict(
                    "Native acceptance changed its exact selected output",
                ));
            }
            if port.artifact_type == task::verification::REVIEWED_IMPLEMENTATION_V1
                || port.artifact_type == task::verification::REPAIR_ALLOWED_IMPLEMENTATION_V1
            {
                let value: task::verification::ReviewedImplementationV1 =
                    serde_json::from_value(artifact.payload)?;
                value.validate().map_err(conflict)?;
                if &value.invocation != input {
                    return Err(conflict(
                        "Native acceptance changed its exact admitted invocation",
                    ));
                }
            } else if port.artifact_type != "af/SourceTree@1" {
                continue;
            }
            let review = input
                .inputs
                .get("review")
                .ok_or_else(|| conflict("Native acceptance omitted its Review"))?;
            if review.artifact_type != TASK_REVIEW_ROUND_V1 {
                return Err(conflict("Native acceptance changed its Review type"));
            }
            ids.extend(review.artifact_ids.iter().cloned());
        }
    }
    ids.into_iter()
        .map(|id| {
            let round = admitted_native_round(cas, state, &id)?;
            Ok((id, round))
        })
        .collect()
}

fn admitted_native_round(
    cas: &Cas,
    state: &TaskProjection,
    id: &str,
) -> Result<task::review::TaskReviewRoundV1, StoreError> {
    let round: task::review::TaskReviewRoundV1 = payload(cas, id, TASK_REVIEW_ROUND_V1)?;
    round.validate().map_err(conflict)?;
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Review has no execution"))?;
    let artifact = cas
        .get_artifact(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    let source = round
        .invocation
        .inputs
        .get("source")
        .ok_or_else(|| conflict("Native successor lost its actual source"))?;
    if native_source_snapshot(cas, source)? != round.snapshot_id {
        return Err(conflict(
            "Native successor changed its actual source Snapshot",
        ));
    }
    if execution
        .invocations
        .get(&round.invocation.node)
        .map(|(_, input)| input)
        != Some(&round.invocation)
        || Some(&round.invocation.plan_id) != state.plan_id.as_ref()
        || artifact.subject_snapshot_id.as_ref() != Some(&round.snapshot_id)
        || !matches!(
            execution
                .graph
                .nodes
                .get(&round.invocation.node)
                .map(|n| &n.operator),
            Some(af_graph::task::CompiledOperator::Primitive {
                operator: task::pipeline::TaskOperatorV1::ReviewReduce {}
                    | task::pipeline::TaskOperatorV1::ReviewReduceShards { .. },
                ..
            })
        )
        || !matches!(&artifact.producer, af_contracts::Producer::KernelOperation {run_id, node_id: Some(node), ..} if run_id == &task_run_id(&state.task_id)? && node == &round.invocation.node)
        || execution
            .outputs
            .get(&round.invocation.node)
            .and_then(|(_, output)| output.outputs.get("result"))
            .is_none_or(|port| port.artifact_ids != [id])
    {
        return Err(conflict(
            "Native successor changed its exact admitted invocation or selected receipt",
        ));
    }
    Ok(round)
}

/// A later selected canonical Round may discharge the successor obligation only through its
/// exact recorded history, never through checks, a Select condition or another dependency.
fn native_round_descends_from(
    cas: &Cas,
    state: &TaskProjection,
    selected: &str,
    successor: &str,
) -> Result<bool, StoreError> {
    let mut id = selected.to_string();
    for _ in 0..16 {
        let round = admitted_native_round(cas, state, &id)?;
        if id == successor {
            return Ok(true);
        }
        let Some(history_id) = round
            .invocation
            .inputs
            .get("history")
            .and_then(|p| p.artifact_ids.first())
        else {
            return Ok(false);
        };
        let history: task::review::ReviewHistoryV1 =
            payload(cas, history_id, task::REVIEW_HISTORY_V1)?;
        history.validate().map_err(conflict)?;
        let task::review::ReviewHistoryV1::Recorded {
            round_report_id,
            subject_id,
            finding_set_id,
            demand_set_id,
            ..
        } = history
        else {
            return Ok(false);
        };
        let prior = admitted_native_round(cas, state, &round_report_id)?;
        let prior_history = state.execution.as_ref().and_then(|execution| {
            execution
                .outputs
                .get(&prior.invocation.node)
                .and_then(|(_, output)| output.outputs.get("history"))
        });
        if prior_history.is_none_or(|port| port.artifact_ids != [history_id.clone()])
            || round.round != prior.round + 1
            || round.policy_id != prior.policy_id
            || round.invocation.plan_id != prior.invocation.plan_id
            || prior.subject_id != subject_id
            || prior.finding_set_id.as_ref() != Some(&finding_set_id)
            || prior.demand_set_id.as_ref() != Some(&demand_set_id)
        {
            return Err(conflict(
                "Native successor changed its exact canonical history",
            ));
        }
        if round.snapshot_id != prior.snapshot_id {
            if !task::source::snapshot_descends_from(&round.snapshot_id, &prior.snapshot_id, |id| {
                admitted_snapshot(cas, id)
            })
            .map_err(conflict)?
            {
                return Err(conflict(
                    "Native successor left its committed Snapshot ancestry",
                ));
            }
            require_checked_native_source(cas, state, &round)?;
        }
        id = round_report_id;
    }
    Err(conflict("Native successor history exceeds the Round bound"))
}

/// The trees and origin were validated when each source/check invocation was admitted. Replay
/// re-reads their immutable metadata and referenced CAS identities, never a live worktree.
fn admitted_snapshot(cas: &Cas, id: &str) -> Result<task::source::TaskSnapshot, String> {
    let snapshot: task::source::TaskSnapshot =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    snapshot.validate(id)?;
    for reference in [&snapshot.origin_id, &snapshot.manifest_id] {
        cas.verify(reference).map_err(|e| e.to_string())?;
    }
    Ok(snapshot)
}

fn require_checked_native_source(
    cas: &Cas,
    state: &TaskProjection,
    round: &task::review::TaskReviewRoundV1,
) -> Result<(), StoreError> {
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Review has no execution"))?;
    let source = round
        .invocation
        .inputs
        .get("source")
        .ok_or_else(|| conflict("Native successor lost its source"))?;
    let checks = round
        .invocation
        .inputs
        .get("checks")
        .ok_or_else(|| conflict("Native successor lost its checks"))?;
    let check_id = checks
        .artifact_ids
        .first()
        .ok_or_else(|| conflict("Native successor lost its checks identity"))?;
    let frame = envelope(cas, check_id, task::verification::TASK_CHECK_RECEIPT_V2)?;
    let value: task::verification::TaskCheckReceiptV2 = serde_json::from_value(frame.payload)?;
    value.validate().map_err(conflict)?;
    let af_contracts::Producer::Attempt {
        run_id,
        node_id: node,
        ..
    } = frame.producer
    else {
        return Err(conflict(
            "Native descendant has no admitted checks producer",
        ));
    };
    if run_id != task_run_id(&state.task_id)?
        || source.snapshot_id.as_ref() != Some(&round.snapshot_id)
        || checks.snapshot_id.as_ref() != Some(&round.snapshot_id)
        || frame.subject_snapshot_id.as_ref() != Some(&round.snapshot_id)
        || value.snapshot_id != round.snapshot_id
        || value.plan_id != round.invocation.plan_id
        || value.outcome != task::pipeline::ReceiptOutcomeV1::Passed
        || !execution
            .outputs
            .get(&node)
            .is_some_and(|(_, output)| output.outputs.values().any(|p| p == checks))
        || !execution.invocations.get(&node).is_some_and(|(_, input)| {
            input
                .inputs
                .get("source")
                .and_then(|p| p.snapshot_id.as_ref())
                == Some(&round.snapshot_id)
        })
        || !matches!(
            execution.graph.nodes.get(&node).map(|n| &n.operator),
            Some(af_graph::task::CompiledOperator::Primitive {
                operator: task::pipeline::TaskOperatorV1::Check { .. },
                ..
            })
        )
    {
        return Err(conflict(
            "Native descendant requires its exact admitted passing source checks",
        ));
    }
    Ok(())
}

fn selected_public_snapshot(
    cas: &Cas,
    result: &task::TaskResultV1,
) -> Result<Option<String>, StoreError> {
    result
        .outputs
        .get("snapshot")
        .map(|port| native_source_snapshot(cas, port))
        .transpose()
}

fn native_source_snapshot(cas: &Cas, port: &task::ArtifactInputV1) -> Result<String, StoreError> {
    port.validate().map_err(conflict)?;
    if port.artifact_type != "af/SourceTree@1"
        || port.cardinality != af_contracts::PortCardinality::One
        || port.artifact_ids.len() != 1
    {
        return Err(conflict("Native acceptance changed its source type"));
    }
    let frame = envelope(cas, &port.artifact_ids[0], "af/SourceTree@1")?;
    let snapshot = frame
        .payload
        .get("snapshot_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| conflict("Native acceptance lost its source Snapshot"))?;
    if frame.subject_snapshot_id.as_deref() != Some(snapshot)
        || port.snapshot_id.as_deref() != Some(snapshot)
        || frame
            .payload
            .as_object()
            .is_none_or(|value| value.len() != 1)
    {
        return Err(conflict(
            "Native acceptance changed its exact source Snapshot",
        ));
    }
    Ok(snapshot.to_owned())
}
