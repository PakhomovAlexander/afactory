//! Human-directed native Proposal export grants no automatic Integration or delivery authority.
use super::*;
use af_contracts::task::event::TaskChangeV1;
use af_contracts::task::execution::TaskExecutionRecordV1;
use af_store::store::task::execution::read_execution_record;
use af_store::store::task::{read_task_transition, task_run_id};
use std::io::Write;

pub(crate) fn run(
    id: &str,
    proposal: Option<&str>,
    finding: Option<&str>,
    allow_stale: bool,
    plan: Option<&str>,
    repo: &Path,
    state: Option<&Path>,
) -> Result<i32, String> {
    let opened = open_task(id, repo, state)?;
    let events = opened
        .store
        .replay(&task_run_id(id).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let current_domain = domain(&opened.cas, &opened.task)?;
    let current = current_head(&opened.cas, &opened.task, &events, &current_domain)?;
    let archived;
    let task = if let Some(plan) = plan.filter(|p| opened.task.plan_id.as_deref() != Some(*p)) {
        if !af_contracts::is_digest(plan) {
            return Err("Expected an exact recorded plan ID".into());
        }
        let mut selected = None;
        // Prefix replay admits only records written by this release. The full current log was
        // validated by open_task first; historical selection grants no execution authority.
        for event in &events {
            if af_store::store::task::is_beside_transitions(event) {
                continue;
            }
            let transition = read_task_transition(event).map_err(|e| e.to_string())?;
            if matches!(
                transition.change,
                TaskChangeV1::SourceRefreshed { .. } | TaskChangeV1::PlanningCompleted { .. }
            ) && event.sequence > 0
            {
                let prefix = opened
                    .store
                    .task_projection_at_sequence(&opened.cas, id, event.sequence - 1)
                    .map_err(|e| e.to_string())?;
                if prefix
                    .as_ref()
                    .is_some_and(|p| p.plan_id.as_deref() == Some(plan))
                {
                    selected = prefix;
                }
            }
        }
        archived = selected
            .ok_or("The requested archived plan has no recorded native execution in this Task")?;
        &archived
    } else {
        &opened.task
    };
    let domain = domain(&opened.cas, task)?;
    let proposals = domain.sealed_proposals(&opened.cas, task)?;
    let selected = match (proposal, finding) {
        (Some(id), None) => proposals
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("No admitted sealed Proposal with ID {id}"))?,
        (None, Some(id)) => {
            let matching: Vec<_> = proposals
                .iter()
                .filter(|p| {
                    p.finding_ids.contains(id) && (allow_stale || p.base_snapshot_id == current)
                })
                .collect();
            if matching.len() != 1 {
                return Err(format!(
                    "Finding {id} has {} applicable sealed Proposals; export by exact Proposal ID",
                    matching.len()
                ));
            }
            matching[0]
        }
        _ => return Err("Export names exactly one Proposal ID or --finding FINDING_ID".into()),
    };
    if selected.base_snapshot_id != current && !allow_stale {
        return Err(format!(
            "Proposal {} is stale: base {} differs from current Review head {}; pass --allow-stale only for deliberate three-way application",
            selected.id, selected.base_snapshot_id, current
        ));
    }
    let (_, before) = af_source_git::task::read_snapshot(&opened.cas, &selected.base_snapshot_id)?;
    let after = af_source_git::task::read_manifest(&opened.cas, &selected.candidate_manifest_id)?;
    let diff = af_source_git::git::manifest_diff(&before, &after, &opened.cas)
        .map_err(|e| e.to_string())?;
    std::io::stdout()
        .lock()
        .write_all(diff.patch())
        .map_err(|e| e.to_string())?;
    Ok(0)
}

/// Review can seal explicit proposed repairs while its verification Workers stay independent.
/// Ordinary Review workers retain read-only contracts. This does not authorize delivery.
pub(super) fn validate_authors(
    compiler: &TaskPlanCompiler,
    graph: &CompiledTask,
) -> Result<(), String> {
    use af_contracts::task::native_review::TASK_REVIEW_PROPOSAL_V1;
    let reviewers: BTreeSet<_> = graph
        .slots
        .iter()
        .filter(|(_, slot)| {
            compiler.worker(&slot.worker).is_some_and(|worker| {
                worker
                    .signature
                    .contract
                    .outputs
                    .values()
                    .any(|p| p.artifact_type == af_contracts::contract::REVIEWER_RESULT_V2)
            })
        })
        .map(|(name, _)| name)
        .collect();
    for (name, slot) in &graph.slots {
        let worker = compiler
            .worker(&slot.worker)
            .ok_or("Review slot has no captured Worker")?;
        if !worker.signature.effects.contains("write-source") {
            continue;
        }
        let types: BTreeSet<_> = worker
            .signature
            .contract
            .outputs
            .values()
            .map(|p| p.artifact_type.as_str())
            .collect();
        if !types.contains(TASK_REVIEW_PROPOSAL_V1)
            || !types.contains(af_source_git::task::CANDIDATE_TREE_V1)
            || reviewers.contains(name)
        {
            return Err(
                "A native Review source writer must be a separate sealed Proposal author".into(),
            );
        }
        if reviewers.iter().any(|reviewer| {
            !slot.independent_from.contains(*reviewer)
                && !graph.slots[*reviewer].independent_from.contains(name)
        }) {
            return Err("Every native Proposal author must declare independence from every Review verifier under the captured policy".into());
        }
    }
    Ok(())
}

fn domain(cas: &Cas, task: &TaskProjection) -> Result<ReviewTaskDomain, String> {
    let policies: BTreeSet<_> = task
        .revision
        .acceptance
        .values()
        .filter(|o| {
            matches!(
                o.evidence_type.as_str(),
                TASK_REVIEW_ROUND_V1
                    | af_contracts::task::verification::REVIEWED_IMPLEMENTATION_V1
                    | af_contracts::task::verification::REPAIR_ALLOWED_IMPLEMENTATION_V1
            )
        })
        .map(|o| o.verifier_policy.as_str())
        .collect();
    if policies.len() != 1 {
        return Err("Proposal export requires one captured native Review policy".into());
    }
    ReviewTaskDomain::captured(
        cas,
        policies.first().unwrap(),
        task.execution
            .as_ref()
            .ok_or("Task has no recorded Proposal outputs")?
            .graph
            .clone(),
    )
}

fn current_head(
    cas: &Cas,
    task: &TaskProjection,
    events: &[af_contracts::RunEvent],
    domain: &ReviewTaskDomain,
) -> Result<String, String> {
    let mut current = task
        .revision
        .inputs
        .get("source")
        .and_then(|p| p.snapshot_id.clone())
        .ok_or("Review has no recorded source")?;
    // Publication order is the durable authority. Graph order is only a scheduling plan;
    // unrelated later Round outputs never move this head.
    for event in events {
        if af_store::store::task::is_beside_transitions(event) {
            continue;
        }
        let transition = read_task_transition(event).map_err(|e| e.to_string())?;
        let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
            continue;
        };
        let record = read_execution_record(cas, &record_id)
            .map_err(|e| e.to_string())?
            .record;
        let TaskExecutionRecordV1::Published { output_id, .. } = record else {
            continue;
        };
        if let Some((base, head)) = domain.export_commit(cas, task, &output_id)? {
            if base != current {
                return Err(
                    "Review commits do not form one exact authoritative source lineage".into(),
                );
            }
            current = head;
        }
    }
    Ok(current)
}
