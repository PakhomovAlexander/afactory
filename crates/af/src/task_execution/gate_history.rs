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
    let plan: ExecutionPlanV3 = artifact(cas, plan_id, EXECUTION_PLAN_V4).ok()?;
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

#[derive(Debug, PartialEq, Eq)]
struct LastCheck {
    started: u64,
    task_id: String,
    name: String,
    elapsed_ms: u64,
    cache: Option<&'static str>,
}

fn consider(
    last: &mut Option<LastCheck>,
    task_id: &str,
    names: &BTreeSet<String>,
    evidence: TaskRuntimeEvidenceV1,
) {
    if evidence.validate().is_err() {
        return;
    }
    for span in evidence.spans {
        if span.kind != TaskRuntimeSpanKindV1::Check || !names.contains(&span.label) {
            continue;
        }
        if last.as_ref().is_none_or(|old| {
            span.started_unix_ms > old.started
                || (span.started_unix_ms == old.started && task_id < old.task_id.as_str())
        }) {
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
            *last = Some(LastCheck {
                started: span.started_unix_ms,
                task_id: task_id.to_owned(),
                name: span.label,
                elapsed_ms: span.elapsed_ms,
                cache,
            });
        }
    }
}

/// One check's latest recorded span, with the same repository and code policy as `task`.
/// Incomplete records and an empty history yield no hint; no duration is estimated.
pub(super) fn last(cas: &Cas, store: &EventStore, task: &TaskProjection) -> Option<String> {
    let current_identity = identity(cas, task)?;
    let names = checks(cas, task)?;
    if names.is_empty() {
        return None;
    }
    // The partial event index excludes renewals and Tasks with no recorded execution.
    // First retain only a small upper bound per Task with a matching measured check. Full
    // replay is reserved for candidates that can still win the most-recent comparison.
    let current_run = review_store::store::task::task_run_id(&task.task_id).ok()?;
    let mut bounds = BTreeMap::<String, u64>::new();
    let mut history_intact = true;
    store
        .visit_task_execution_history(cas, |run, record| {
            if run == current_run {
                return;
            }
            let review_core::task::execution::TaskExecutionRecordV1::Settled {
                raw_artifact_ids,
                ..
            } = record
            else {
                return;
            };
            for id in raw_artifact_ids {
                let raw = match cas.get_optional_artifact_shared(&id) {
                    Ok(Some(raw)) => raw,
                    Ok(None) => continue,
                    Err(_) => {
                        history_intact = false;
                        continue;
                    }
                };
                if raw.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
                    continue;
                }
                let Ok(evidence) = TaskRuntimeEvidenceV1::deserialize(&raw.payload) else {
                    continue;
                };
                if evidence.validate().is_err() {
                    continue;
                }
                for span in evidence.spans {
                    if span.kind == TaskRuntimeSpanKindV1::Check && names.contains(&span.label) {
                        bounds
                            .entry(run.to_owned())
                            .and_modify(|time| *time = (*time).max(span.started_unix_ms))
                            .or_insert(span.started_unix_ms);
                    }
                }
            }
        })
        .ok()?;
    if !history_intact {
        return None;
    }
    let mut candidates = Vec::with_capacity(bounds.len());
    for (run, upper) in bounds {
        let id = store.task_label_for_run(cas, &run).ok()?;
        if id != task.task_id {
            candidates.push((upper, id));
        }
    }
    candidates.sort_by(|(a_time, a_id), (b_time, b_id)| b_time.cmp(a_time).then(a_id.cmp(b_id)));
    let mut last: Option<LastCheck> = None;
    for (upper, id) in candidates {
        if last.as_ref().is_some_and(|old| upper < old.started) {
            break;
        }
        let previous = match store.task_projection(cas, &id) {
            Ok(Some(previous)) => previous,
            Err(review_store::StoreError::Collected { .. }) => continue,
            _ => return None,
        };
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
                let Ok(raw) = cas.get_artifact_shared(raw_id) else {
                    continue;
                };
                if raw.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
                    continue;
                }
                let Ok(evidence) = TaskRuntimeEvidenceV1::deserialize(&raw.payload) else {
                    continue;
                };
                consider(&mut last, &previous.task_id, &names, evidence);
            }
        }
    }
    last.map(
        |LastCheck {
             name,
             elapsed_ms,
             cache,
             ..
         }| {
            format!(
                "{}: {}{}",
                preview::short(&name, 48),
                state::duration_words(elapsed_ms),
                cache.map_or_else(String::new, |cache| format!(", {cache}"))
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::runtime::{TaskCacheObservationV1, TaskRuntimeSpanV1};

    fn evidence(
        cas: &Cas,
        spans: &[(TaskRuntimeSpanKindV1, &str, u64, u64)],
    ) -> TaskRuntimeEvidenceV1 {
        let context = cas.put(b"history-context").unwrap();
        let evidence = TaskRuntimeEvidenceV1 {
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            node: "check".into(),
            context_id: context,
            check: None,
            caches: vec![],
            spans: spans
                .iter()
                .map(
                    |&(kind, label, started_unix_ms, elapsed_ms)| TaskRuntimeSpanV1 {
                        span_id: review_store::content_id(&json!([
                            label,
                            started_unix_ms,
                            elapsed_ms
                        ]))
                        .unwrap(),
                        kind,
                        label: label.into(),
                        started_unix_ms,
                        elapsed_ms,
                    },
                )
                .collect(),
        };
        // Select from a real, canonical runtime artifact, as the history reader does.
        let id = cas
            .put_artifact(
                TASK_RUNTIME_EVIDENCE_V1,
                Producer::KernelOperation {
                    run_id: "history".into(),
                    node_id: None,
                    operation_id: "measure".into(),
                },
                vec![],
                None,
                serde_json::to_value(evidence).unwrap(),
            )
            .unwrap()
            .0;
        TaskRuntimeEvidenceV1::deserialize(&cas.get_artifact_shared(&id).unwrap().payload).unwrap()
    }

    #[test]
    fn latest_check_uses_start_time_check_identity_and_stable_task_and_span_ties() {
        use TaskRuntimeSpanKindV1::{Check, DependencyPreparation};
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path()).unwrap();
        let names = BTreeSet::from(["fast".into(), "slow".into()]);
        let mut last = None;
        consider(
            &mut last,
            "z-task",
            &names,
            evidence(
                &cas,
                &[
                    (Check, "fast", 100, 5000),
                    (Check, "slow", 300, 22),
                    (Check, "unrelated", 9000, 1),
                    (DependencyPreparation, "fast", 10000, 1),
                ],
            ),
        );
        assert_eq!(
            last.as_ref()
                .map(|hint| (hint.name.as_str(), hint.elapsed_ms)),
            Some(("slow", 22))
        );
        // A late completion of older work does not replace the most recently started span.
        consider(
            &mut last,
            "a-task",
            &names,
            evidence(&cas, &[(Check, "fast", 200, 99_999)]),
        );
        assert_eq!(last.as_ref().unwrap().task_id, "z-task");
        // Old map_tasks traversed labels first, then Attempt/raw/span order. Preserve both ties.
        consider(
            &mut last,
            "a-task",
            &names,
            evidence(&cas, &[(Check, "fast", 300, 44), (Check, "slow", 300, 55)]),
        );
        assert_eq!(last.as_ref().unwrap().name, "fast");
        assert_eq!(last.as_ref().unwrap().elapsed_ms, 44);
        consider(
            &mut last,
            "z-task",
            &names,
            evidence(&cas, &[(Check, "slow", 300, 66)]),
        );
        assert_eq!(last.as_ref().unwrap().task_id, "a-task");
        let mut invalid = evidence(&cas, &[(Check, "fast", 400, 77)]);
        invalid.spans[0].started_unix_ms = 0;
        consider(&mut last, "a-task", &names, invalid);
        assert_eq!(last.as_ref().unwrap().elapsed_ms, 44);

        let mut warm = evidence(&cas, &[(Check, "slow", 400, 88)]);
        warm.caches.push(TaskCacheObservationV1 {
            observation_id: cas.put(b"cache").unwrap(),
            kind: "cargo_target".into(),
            eligible: true,
            source_digest: cas.put(b"source").unwrap(),
            toolchain_id: None,
            bytes_available: 1,
            lookup_ms: 0,
            materialization_ms: 0,
            evicted_bytes: None,
            evicted_reason: None,
            bound: None,
        });
        consider(&mut last, "a-task", &names, warm.clone());
        assert_eq!(last.as_ref().unwrap().cache, Some("warm"));
        warm.spans[0].started_unix_ms = 500;
        warm.caches[0].bytes_available = 0;
        consider(&mut last, "a-task", &names, warm);
        assert_eq!(last.as_ref().unwrap().cache, Some("cold"));
    }

    fn task_with_identity(
        cas: &Cas,
        store: &mut EventStore,
        label: &str,
        repository: &str,
        policy_label: &str,
    ) -> TaskProjection {
        let producer = Producer::KernelOperation {
            run_id: "history".into(),
            node_id: None,
            operation_id: "capture".into(),
        };
        let code_policy = cas.put_json(&json!({"code_policy": policy_label})).unwrap();
        let policy = cas
            .put_json(&json!({"code_policy_id": code_policy}))
            .unwrap();
        let requirements = cas
            .put_artifact(
                "af/Requirements@1",
                producer.clone(),
                vec![],
                None,
                json!({"text": "history"}),
            )
            .unwrap()
            .0;
        let manifest = review_source_git::Manifest::new(vec![]).unwrap();
        let origin = cas
            .put_json(
                &json!({"schema": "af.task-source-origin/1", "repository_id": repository,
            "content_digest": manifest.content_digest()}),
            )
            .unwrap();
        let snapshot =
            review_source_git::task::capture_snapshot(cas, &manifest, &origin, None).unwrap();
        let source =
            review_source_git::task::source_tree(cas, producer.clone(), &snapshot, vec![]).unwrap();
        let template = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1/task-revision.json");
        let mut task: TaskRevisionV1 =
            serde_json::from_slice(&std::fs::read(template).unwrap()).unwrap();
        task.task_id = label.into();
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![requirements.clone()];
        task.inputs.insert("source".into(), source);
        task.authority.policy_id = policy.clone();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        task.provenance.adapter_id = policy;
        task.provenance.input_artifact_ids = vec![requirements];
        task.limits.deadline_unix_ms = review_store::store::task::now().unwrap() + 1_000_000;
        let id = cas
            .put_artifact(
                TASK_REVISION_V1,
                producer,
                vec![],
                None,
                serde_json::to_value(task).unwrap(),
            )
            .unwrap()
            .0;
        store.open_task(cas, &id, "fixture", 60_000).unwrap();
        store.task_projection(cas, label).unwrap().unwrap()
    }

    #[test]
    fn history_identity_requires_both_repository_and_code_policy_and_verified_origin() {
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let mut store = EventStore::open(root.path().join("events.sqlite")).unwrap();
        let a = task_with_identity(&cas, &mut store, "a", "repository-a", "policy-a");
        let same = task_with_identity(&cas, &mut store, "same", "repository-a", "policy-a");
        let other_repo = task_with_identity(&cas, &mut store, "repo", "repository-b", "policy-a");
        let other_policy =
            task_with_identity(&cas, &mut store, "policy", "repository-a", "policy-b");
        assert_eq!(identity(&cas, &a), identity(&cas, &same));
        assert_ne!(identity(&cas, &a), identity(&cas, &other_repo));
        assert_ne!(identity(&cas, &a), identity(&cas, &other_policy));
        let (_, snapshot, _) =
            review_pipeline::task::source::source_snapshot(&cas, &a.revision.inputs["source"])
                .unwrap();
        let id = snapshot.origin_id;
        let path = root
            .path()
            .join("cas/objects")
            .join(&id[7..9])
            .join(&id[9..]);
        std::fs::remove_file(path).unwrap();
        assert!(identity(&cas, &a).is_none());
    }
}
