//! Last measured comparable check from this Store. Preview reads only recorded evidence.

use super::*;
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1,
};
use review_graph::task::CompiledOperator;

fn identity(cas: &Cas, task: &TaskProjection) -> Option<(String, String)> {
    let policy = cas.get_json(&task.revision.authority.policy_id).ok()?;
    let code_policy = policy.get("code_policy_id")?.as_str()?.to_owned();
    let source = task.revision.inputs.get("source")?;
    let (_, snapshot, _) = review_pipeline::task::source::source_snapshot(cas, source).ok()?;
    let origin = review_source_git::task::read_origin(cas, &snapshot.origin_id).ok()?;
    Some((origin.repository_id().to_owned(), code_policy))
}

fn checks(cas: &Cas, task: &TaskProjection) -> Option<BTreeSet<String>> {
    let plan_id = task.plan_id.as_deref()?;
    let plan: ExecutionPlanV1 = artifact(cas, plan_id, EXECUTION_PLAN_V1).ok()?;
    let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1).ok()?;
    Some(
        graph
            .nodes
            .values()
            .filter_map(|node| match &node.operator {
                CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::Check {
                            checks,
                            remote_checks,
                        },
                    ..
                } => Some(checks.union(remote_checks).cloned()),
                _ => None,
            })
            .flatten()
            .collect(),
    )
}

/// One check's latest recorded span, with the same repository and code policy as `task`.
/// Incomplete records and an empty history yield no hint; no duration is estimated.
pub(super) fn last(cas: &Cas, store: &EventStore, task: &TaskProjection) -> Option<String> {
    let current_identity = identity(cas, task)?;
    let names = checks(cas, task)?;
    if names.is_empty() {
        return None;
    }
    let history = store.map_tasks(cas, |previous| previous).ok()?;
    let mut last: Option<(u64, String, u64, Option<&'static str>)> = None;
    for previous in history {
        if previous.task_id == task.task_id
            || identity(cas, &previous).as_ref() != Some(&current_identity)
        {
            continue;
        }
        let Some(execution) = &previous.execution else {
            continue;
        };
        for attempt in execution.attempt_accounting() {
            for raw_id in &attempt.raw_artifact_ids {
                let Ok(raw) = cas.get_artifact(raw_id) else {
                    continue;
                };
                if raw.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
                    continue;
                }
                let Ok(evidence) = serde_json::from_value::<TaskRuntimeEvidenceV1>(raw.payload)
                else {
                    continue;
                };
                if evidence.validate().is_err() {
                    continue;
                }
                for span in evidence.spans {
                    if span.kind != TaskRuntimeSpanKindV1::Check || !names.contains(&span.label) {
                        continue;
                    }
                    if last
                        .as_ref()
                        .is_none_or(|(when, _, _, _)| span.started_unix_ms > *when)
                    {
                        let cache = if evidence.caches.is_empty() {
                            None
                        } else if evidence
                            .caches
                            .iter()
                            .any(|cache| cache.eligible && cache.bytes_available > 0)
                        {
                            Some("warm")
                        } else {
                            Some("cold")
                        };
                        last = Some((span.started_unix_ms, span.label, span.elapsed_ms, cache));
                    }
                }
            }
        }
    }
    last.map(|(_, name, ms, cache)| {
        format!(
            "{}: {}{}",
            preview::short(&name, 48),
            state::duration_words(ms),
            cache.map_or_else(String::new, |cache| format!(", {cache}"))
        )
    })
}
