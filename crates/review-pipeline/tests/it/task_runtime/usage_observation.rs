use super::*;
use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Model {
    calls: AtomicUsize,
    fail_at: usize,
    complete: bool,
    floor: u128,
    /// A charge the observation reports instead of the returned usage's.
    observed: Option<u128>,
}
impl WorkerModelAdapter for Model {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("typed-model".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        _: &std::path::Path,
        input: Vec<u8>,
        _: std::time::Duration,
        access: review_runner::task::WorkerAccess,
        _: Option<&std::sync::atomic::AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        assert_eq!(access, review_runner::task::WorkerAccess::ReadOnly);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            call <= if self.fail_at == 0 { 0 } else { 2 },
            "no retries beyond the original Provider or Worker allowance"
        );
        if call == 0 {
            assert_eq!(input, b"Reply with exactly: OK\n");
        }
        let usage =
            TaskTokenUsageV3::charge_only(if call >= self.fail_at { self.floor } else { 7 });
        let observed = self
            .observed
            .map_or_else(|| usage.clone(), TaskTokenUsageV3::charge_only);
        ModelWorkerReturn {
            usage_observation: (call >= self.fail_at).then_some(TaskUsageObservationV1 {
                reported_usage: Some(observed),
                charge_complete: self.complete,
            }),
            usage: Some(usage),
            raw_artifact_ids: vec![cas.put(b"native usage fixture").unwrap()],
            message: if call >= self.fail_at {
                Err("fixture malformed native protocol".into())
            } else {
                Ok(b"OK".to_vec())
            },
        }
    }
}

/// Runs the configured fixture's Task against `model` until it stops incomplete.
fn executed(model: &Model) -> Fixture {
    let mut f = super::wide_usage::configured_fixture();
    {
        let models = f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let domain = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &f.graph,
            models: &models,
            inner: &DocumentDomain,
        };
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &domain,
            &models,
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer", 60000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        assert!(!runtime.execute().unwrap().complete());
    }
    f
}

/// The error a failed Attempt settled with.
fn diagnostic(
    f: &Fixture,
    result: &Option<review_core::task::execution::TaskAttemptResultV1>,
) -> serde_json::Value {
    let Some(review_core::task::execution::TaskAttemptResultV1::Failed { diagnostic_id, .. }) =
        result
    else {
        panic!("a refused Attempt settles with a diagnostic");
    };
    f.cas.get_json(diagnostic_id).unwrap()["error"].clone()
}

#[test]
fn incomplete_billing_uses_original_reservation_but_known_failed_calls_keep_exact_charge() {
    for fail_at in [0, 1] {
        for complete in [false, true] {
            for floor in [0, 7, u128::from(u64::MAX) + 30] {
                let model = Model {
                    calls: AtomicUsize::new(0),
                    fail_at,
                    complete,
                    floor,
                    observed: None,
                };
                let mut f = executed(&model);
                let failed_node = if fail_at == 0 {
                    "root.providers.admit0"
                } else {
                    "root.nodes.write"
                };
                let expected_failures = if floor > u128::from(f.task.limits.verification.tokens) {
                    1
                } else {
                    usize::try_from(f.graph.allowances[failed_node].max_attempts).unwrap()
                };
                assert_eq!(
                    model.calls.load(Ordering::SeqCst),
                    fail_at + expected_failures
                );
                let execution = f
                    .store
                    .task_projection(&f.cas, &f.task.task_id)
                    .unwrap()
                    .unwrap()
                    .execution
                    .unwrap();
                let failed_rows: Vec<_> = execution
                    .attempt_accounting()
                    .into_iter()
                    .filter(|row| row.reservation.node == failed_node)
                    .collect();
                assert_eq!(failed_rows.len(), expected_failures);
                let row = &failed_rows[0];
                let charge = if complete {
                    floor
                } else {
                    floor.max(u128::from(row.reservation.tokens))
                };
                assert!(failed_rows.iter().all(|row| row.charged_tokens == charge));
                // The adapter's own reason stays in the diagnostic behind the billing refusal.
                assert_eq!(
                    diagnostic(&f, &row.result),
                    if complete {
                        "fixture malformed native protocol"
                    } else {
                        "Native billing usage is incomplete: fixture malformed native protocol"
                    }
                );
                let total = charge * expected_failures as u128 + if fail_at == 1 { 7 } else { 0 };
                assert_eq!(execution.budget.committed_tokens(), total);
                assert_eq!(
                    execution.budget.begun_attempts(),
                    (fail_at + expected_failures) as u64
                );
                assert!(!execution.outputs.contains_key("root.nodes.write"));
                let original = f.task.limits.clone();
                f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
                let state = f
                    .store
                    .task_projection(&f.cas, &f.task.task_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(state.revision.limits, original);
                assert_eq!(state.execution.unwrap().budget.committed_tokens(), total);
                let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
                let observation = f
                    .store
                    .task_attempt_usage_observation(&run, &row.attempt_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(observation.charge_complete, complete);
                assert_eq!(
                    observation.reported_usage.unwrap().chargeable_tokens.get(),
                    floor
                );
                let wall = f
                    .store
                    .task_attempt_wall(&run)
                    .unwrap()
                    .into_iter()
                    .find(|w| w.attempt_id == row.attempt_id)
                    .unwrap();
                assert_eq!(wall.usage.unwrap().chargeable_tokens.get(), charge);
            }
        }
    }
}

#[test]
fn a_counter_mismatch_keeps_the_worker_error_behind_each_billing_refusal() {
    for (complete, expected) in [
        (
            true,
            "Native usage observation differs from the returned counters: \
             fixture malformed native protocol",
        ),
        (
            false,
            "Native billing usage is incomplete: Native usage observation differs from the \
             returned counters: fixture malformed native protocol",
        ),
    ] {
        let model = Model {
            calls: AtomicUsize::new(0),
            fail_at: 0,
            complete,
            floor: 7,
            observed: Some(9),
        };
        let f = executed(&model);
        let execution = f
            .store
            .task_projection(&f.cas, &f.task.task_id)
            .unwrap()
            .unwrap()
            .execution
            .unwrap();
        let rows = execution.attempt_accounting();
        let row = rows
            .iter()
            .find(|row| row.reservation.node == "root.providers.admit0")
            .unwrap();
        assert_eq!(diagnostic(&f, &row.result), expected);
    }
}
