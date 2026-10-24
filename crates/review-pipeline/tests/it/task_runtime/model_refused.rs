//! ADR-0147 through the common runtime: an Attempt the bound model's safety classifier refused
//! (`model_refused`) fails its node at once. The same input is flagged again, so the runtime does
//! not spend the node's remaining Attempts on it, and the Attempt is charged exactly the usage
//! the result reported.
use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::usage::TaskTokenUsageV3;
use review_runner::native_failure::NativeFailureKind;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Answers the first `answered` calls, then refuses every later one with `ending`.
struct Model {
    calls: AtomicUsize,
    answered: usize,
    ending: Option<NativeFailureKind>,
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
        _: Option<&AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        let raw = vec![cas.put(b"native refusal fixture").unwrap()];
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.answered {
            return ModelWorkerReturn {
                notes: Default::default(),
                native_failure: None,
                usage_observation: None,
                usage: Some(TaskTokenUsageV3::charge_only(7)),
                raw_artifact_ids: raw,
                message: Ok(b"OK".to_vec()),
            };
        }
        ModelWorkerReturn {
            notes: Default::default(),
            native_failure: self.ending,
            usage_observation: None,
            usage: Some(TaskTokenUsageV3::charge_only(13)),
            raw_artifact_ids: raw,
            message: Err("Claude Worker failed: fixture refusal".into()),
        }
    }
}

/// Run the configured fixture's Task once against `model`; return its settled Attempts by node.
fn run(model: &Model) -> (Fixture, Vec<(String, u128, bool)>) {
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
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let execution = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    assert_eq!(execution.budget.reserved_tokens(), 0);
    let rows = execution
        .attempt_accounting()
        .into_iter()
        .filter(|row| row.started)
        .map(|row| {
            assert!(row.unknown_usage.is_none());
            let failed = matches!(row.result, Some(TaskAttemptResultV1::Failed { .. }));
            (row.reservation.node, row.charged_tokens, failed)
        })
        .collect();
    (f, rows)
}

#[test]
fn a_refused_worker_input_is_not_retried_and_is_charged_what_it_reported() {
    let model = Model {
        calls: AtomicUsize::new(0),
        answered: 1,
        ending: Some(NativeFailureKind::ModelRefused),
    };
    let (f, rows) = run(&model);
    // The node could have retried: its allowance holds more than one Attempt.
    assert!(f.graph.allowances["root.nodes.write"].max_attempts > 1);
    let written: Vec<_> = rows
        .iter()
        .filter(|(node, ..)| node == "root.nodes.write")
        .collect();
    assert_eq!(written.len(), 1, "{rows:?}");
    assert_eq!(written[0].1, 13);
    assert!(written[0].2);
    // The admission call and the one refused Worker call; no retry.
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_refused_admission_starts_no_business_worker() {
    let model = Model {
        calls: AtomicUsize::new(0),
        answered: 0,
        ending: Some(NativeFailureKind::ModelRefused),
    };
    let (_, rows) = run(&model);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (node, charged, failed) = &rows[0];
    assert!(node.starts_with("root.providers.admit"), "{node}");
    // The refused probe and its alternate wording, sent once in the same Attempt (ADR-0208),
    // both refused and both charged; no business Worker starts.
    assert_eq!(*charged, 26);
    assert!(failed);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

/// Any other native failure keeps the ordinary retry policy.
#[test]
fn another_native_failure_is_still_retried() {
    let model = Model {
        calls: AtomicUsize::new(0),
        answered: 1,
        ending: Some(NativeFailureKind::Capacity),
    };
    let (f, rows) = run(&model);
    let allowed = usize::try_from(f.graph.allowances["root.nodes.write"].max_attempts).unwrap();
    let written = rows
        .iter()
        .filter(|(node, ..)| node == "root.nodes.write")
        .count();
    assert_eq!(written, allowed, "{rows:?}");
    assert_eq!(model.calls.load(Ordering::SeqCst), 1 + allowed);
}
