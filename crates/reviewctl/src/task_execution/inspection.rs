//! Read-only inspection of a plan actually recorded in this Task's validated history.
use super::*;
use review_core::task::event::TaskChangeV1;
use review_store::store::task::{read_task_transition, task_run_id};

pub(super) fn explain_plan(
    cas: &Cas,
    store: &EventStore,
    task_id: &str,
    plan_id: &str,
    json_output: bool,
) -> Result<i32, String> {
    if !review_core::is_digest(plan_id) {
        return Err("--plan requires an exact recorded plan artifact ID".into());
    }
    let state = store
        .task_projection(cas, task_id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let events = store
        .replay(&task_run_id(task_id).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut recorded_event_ids = Vec::new();
    for event in &events {
        if event.event_type == review_core::EventType::TaskBrokerTransitionV1 {
            continue;
        }
        let transition = read_task_transition(event).map_err(|e| e.to_string())?;
        let recorded = match &transition.change {
            TaskChangeV1::PlanProposed { plan_id: recorded }
            | TaskChangeV1::PlanAdmitted { plan_id: recorded } => recorded == plan_id,
            TaskChangeV1::SourceRefreshed {
                plan_id: recorded, ..
            } => recorded.as_deref() == Some(plan_id),
            TaskChangeV1::PlanningCompleted {
                bootstrap_plan_id,
                plan_id: recorded,
                ..
            } => recorded == plan_id || bootstrap_plan_id == plan_id,
            TaskChangeV1::ReviewContinued { handoff_id } => {
                let handoff = state
                    .review_handoffs
                    .iter()
                    .find(|(id, _)| id == handoff_id)
                    .ok_or("Recorded Review handoff is absent from Task projection")?;
                handoff.1.predecessor_plan_id == plan_id || handoff.1.successor_plan_id == plan_id
            }
            _ => false,
        };
        if recorded {
            recorded_event_ids.push(event.event_id.clone());
        }
    }
    if recorded_event_ids.is_empty() {
        return Err("The requested plan is not recorded in this Task".into());
    }
    let plan: ExecutionPlanV1 = artifact(cas, plan_id, EXECUTION_PLAN_V1)?;
    plan.validate()?;
    let revision: TaskRevisionV1 = artifact(cas, &plan.task_revision_id, TASK_REVISION_V1)?;
    revision.validate()?;
    if revision.task_id != task_id {
        return Err("The recorded plan belongs to another Task".into());
    }
    let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
    let value = json!({
        "schema":"af/task-plan-inspection@1", "task_id":task_id, "plan_id":plan_id,
        "task_revision_id":plan.task_revision_id,
        "current_plan":state.plan_id.as_deref() == Some(plan_id),
        "recorded_event_ids":recorded_event_ids, "revision":revision, "plan":plan, "graph":graph,
    });
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
    } else {
        println!("Task {task_id}: recorded plan {plan_id}");
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
    }
    Ok(0)
}
