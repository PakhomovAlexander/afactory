//! The run's own words about an Attempt the bound model did not answer (ADR-0147). When a
//! Provider-admission or Worker Attempt fails because the bound model's safety classifier
//! refused it, Claude Code switched to a model the Worker did not bind, or Claude Code does not
//! know the model, `af task run` and `af review run` print one stderr line for it: the node, the
//! Provider ID and kind, the bound model and effort, then the hint its diagnostic starts with,
//! which names the classification and the next step. `af task show` keeps the full diagnostic.

use review_core::task::EXECUTION_PLAN_V1;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_graph::task::{CompiledNode, CompiledOperator, ReviewOperation};
use review_runner_claude::hint;
use review_store::store::task::TaskProjection;
use review_store::{Cas, EventStore};

/// Print [`lines`] for the Task's Attempts that settled at or after `since_unix_ms`, on stderr.
/// Reading them is best effort: a Store that cannot be read prints nothing, and never changes
/// the command's outcome.
pub(crate) fn print(cas: &Cas, store: &EventStore, task_id: &str, since_unix_ms: u64) {
    let Ok(Some(state)) = store.task_projection(cas, task_id) else {
        return;
    };
    for line in lines(cas, &state, since_unix_ms) {
        eprintln!("{line}");
    }
}

/// One line per failed Attempt settled at or after `since_unix_ms` whose diagnostic starts with
/// a model hint, in settlement order. Only af's own words are read: the hint at the head of the
/// diagnostic, and the node and binding the plan records.
pub(crate) fn lines(cas: &Cas, state: &TaskProjection, since_unix_ms: u64) -> Vec<String> {
    let Some(execution) = &state.execution else {
        return Vec::new();
    };
    let plan = state.plan_id.as_deref().and_then(|id| {
        let frame = cas.get_artifact(id).ok()?;
        (frame.artifact_type == EXECUTION_PLAN_V1)
            .then(|| serde_json::from_value::<ExecutionPlanV1>(frame.payload).ok())
            .flatten()
    });
    let mut failed: Vec<_> = execution
        .attempt_accounting()
        .into_iter()
        .filter(|attempt| {
            attempt
                .settled_unix_ms
                .is_some_and(|at| at >= since_unix_ms)
        })
        .collect();
    failed.sort_by_key(|attempt| attempt.settled_unix_ms);
    let mut lines = Vec::new();
    for attempt in failed {
        let Some(TaskAttemptResultV1::Failed { diagnostic_id, .. }) = &attempt.result else {
            continue;
        };
        let Ok(diagnostic) = cas.get_json(diagnostic_id) else {
            continue;
        };
        let Some(found) = diagnostic["error"].as_str().and_then(hint::find) else {
            continue;
        };
        let node = &attempt.reservation.node;
        let execution = execution
            .resolve_node(node)
            .ok()
            .and_then(|resolved| slot(&resolved.definition).map(str::to_owned))
            .and_then(|slot| Some(plan.as_ref()?.bindings.get(&slot)?.execution.clone()));
        let (provider, kind, model, effort) = match &execution {
            Some(WorkerExecutionV1::Model {
                provider,
                provider_kind,
                model,
                effort,
                ..
            }) => (
                provider.as_str(),
                provider_kind.as_str(),
                model.as_str(),
                effort.as_str(),
            ),
            _ => ("?", "?", "?", "?"),
        };
        lines.push(hint::attempt_line(
            node, provider, kind, model, effort, found,
        ));
    }
    lines
}

/// The binding slot a node's Attempt runs under: an admission's first slot (every slot it admits
/// shares one execution), or a Worker's own.
fn slot(node: &CompiledNode) -> Option<&str> {
    match &node.operator {
        CompiledOperator::ProviderAdmission { bindings } => bindings.first().map(String::as_str),
        CompiledOperator::Primitive {
            operator:
                review_core::task::pipeline::TaskOperatorV1::Worker { slot }
                | review_core::task::pipeline::TaskOperatorV1::Verify { slot }
                | review_core::task::pipeline::TaskOperatorV1::FixVerify { slot },
            ..
        } => Some(slot),
        CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        } => Some(slot),
        _ => None,
    }
}

/// The hint behind each missing node of an incomplete verdict, for the line after `incomplete (N
/// missing nodes)`: `reason   NODE: HINT`. A reason without a model hint adds nothing.
pub(crate) fn missing_node_reasons<'a>(
    missing: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<String> {
    missing
        .into_iter()
        .filter_map(|(node, reason)| hint::find(reason).map(|found| format!("{node}: {found}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_reason_with_a_model_hint_is_repeated_and_only_its_hint() {
        let found = hint::compose(
            hint::BoundModel {
                model: "claude-fable-5-1",
                effort: Some("xhigh"),
            },
            &hint::ModelOutcome::Unavailable,
        );
        let reason = format!(
            "{}{found}; transport: exited 1; Worker said something",
            hint::DIAGNOSTIC_PREFIX
        );
        assert_eq!(
            missing_node_reasons([
                ("root.providers.admit0", reason.as_str()),
                ("root.review", "upstream missing"),
            ]),
            [format!("root.providers.admit0: {found}")]
        );
    }
}
