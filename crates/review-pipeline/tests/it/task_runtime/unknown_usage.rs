//! ADR-0143 through the common runtime: an Attempt whose Provider reported no usage settles at
//! zero with its usage unknown and the cause af knows, releases its reservation, and still
//! counts against the Attempt limits; reported usage is charged exactly as before.
use super::*;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use review_core::task::usage::{
    TaskTokenUsageV3, TaskUnknownUsageCauseV1 as Cause, TaskUsageObservationV1,
};
use review_runner::native_failure::NativeFailureKind;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How the author's call ends after the admission call reported its usage.
#[derive(Clone, Copy)]
enum Ending {
    /// A Codex `turn.failed` with `Selected model is at capacity`, and no usage.
    Capacity,
    /// A Claude result envelope without a `usage` object.
    NoUsageObject,
    /// af is interrupted while the call runs, and no usage comes back.
    Interrupted,
}

struct Model {
    calls: AtomicUsize,
    ending: Ending,
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
        _: Vec<u8>,
        _: std::time::Duration,
        _: review_runner::task::WorkerAccess,
        cancellation: Option<&AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        let raw = vec![cas.put(b"native failure fixture").unwrap()];
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return ModelWorkerReturn {
                native_failure: None,
                usage_observation: None,
                usage: Some(TaskTokenUsageV3::charge_only(7)),
                raw_artifact_ids: raw,
                message: Ok(b"OK".to_vec()),
            };
        }
        let (native_failure, usage_observation, message) = match self.ending {
            Ending::Capacity => (
                Some(NativeFailureKind::Capacity),
                None,
                "Codex Worker failed: Provider model at capacity (capacity)",
            ),
            Ending::NoUsageObject => (
                None,
                Some(TaskUsageObservationV1 {
                    reported_usage: None,
                    charge_complete: false,
                }),
                "Claude Worker failed with fixture status",
            ),
            Ending::Interrupted => {
                cancellation.unwrap().store(true, Ordering::Release);
                (None, None, "Worker was stopped")
            }
        };
        ModelWorkerReturn {
            native_failure,
            usage_observation,
            usage: None,
            raw_artifact_ids: raw,
            message: Err(message.into()),
        }
    }
}

#[test]
fn an_attempt_without_a_usage_report_is_charged_zero_with_its_cause() {
    for (ending, cause) in [
        (Ending::Capacity, Cause::Capacity),
        (Ending::NoUsageObject, Cause::Unreported),
        (Ending::Interrupted, Cause::Interrupted),
    ] {
        let model = Model {
            calls: AtomicUsize::new(0),
            ending,
        };
        let mut f = super::wide_usage::configured_fixture();
        let cancellation = AtomicBool::new(false);
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
                            adapter: &model as &dyn WorkerModelAdapter,
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
            let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host)
                .unwrap()
                .with_cancellation(&cancellation);
            // An interrupted run stops with an error; the others end incomplete.
            if let Ok(report) = runtime.execute() {
                assert!(!report.complete());
            }
        }
        f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        let execution = f
            .store
            .task_projection(&f.cas, &f.task.task_id)
            .unwrap()
            .unwrap()
            .execution
            .unwrap();
        let rows: Vec<_> = execution
            .attempt_accounting()
            .into_iter()
            .filter(|row| row.reservation.node == "root.nodes.write" && row.started)
            .collect();
        // Every failed call is an Attempt: without the reservation charge, the Attempt limits
        // are what bound them.
        let expected = match ending {
            Ending::Interrupted => 1,
            _ => usize::try_from(f.graph.allowances["root.nodes.write"].max_attempts).unwrap(),
        };
        assert_eq!(rows.len(), expected);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1 + expected);
        for row in &rows {
            assert_eq!(row.charged_tokens, 0);
            assert_eq!(row.unknown_usage, Some(cause));
            assert!(row.usage_id.is_none());
            assert!(matches!(
                row.result,
                Some(TaskAttemptResultV1::Failed { .. })
            ));
        }
        // Only the admission call's reported usage is charged, exactly; nothing stays held.
        assert_eq!(execution.budget.committed_tokens(), 7);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.begun_attempts(), 1 + expected as u64);
        // The settlement record carries the marker at zero.
        let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
        let mut settled = 0;
        for event in f.store.replay(&run).unwrap() {
            let transition = review_store::store::task::read_task_transition(&event).unwrap();
            let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            else {
                continue;
            };
            let record =
                review_store::store::task::execution::read_execution_record(&f.cas, &record_id)
                    .unwrap();
            if let TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens,
                unknown_usage,
                ..
            } = &record.record
                && rows.iter().any(|row| &row.attempt_id == attempt_id)
            {
                assert_eq!(*charged_tokens, 0);
                assert_eq!(unknown_usage.map(|u| u.cause), Some(cause));
                assert_eq!(
                    record.envelope.payload["unknown_usage"],
                    json!({"cause": cause.as_str()})
                );
                settled += 1;
            }
        }
        assert_eq!(settled, expected);
    }
}
