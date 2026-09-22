//! A real CAS outage after a model returns usage must leave an exact recoverable floor.
use super::*;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct OutageModel {
    objects: PathBuf,
    backup: PathBuf,
    outage: Mutex<bool>,
    calls: AtomicUsize,
    fail_at: usize,
    incomplete: bool,
}
impl OutageModel {
    fn restore(&self) {
        let mut outage = self.outage.lock().unwrap();
        if *outage {
            std::fs::remove_file(&self.objects).unwrap();
            std::fs::rename(&self.backup, &self.objects).unwrap();
            *outage = false;
        }
    }
}
impl Drop for OutageModel {
    fn drop(&mut self) {
        self.restore();
    }
}
impl WorkerModelAdapter for OutageModel {
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
        writable: bool,
        _: Option<&std::sync::atomic::AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        assert!(!writable);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            call <= self.fail_at,
            "CAS failure reached another model invocation"
        );
        if call == 0 {
            assert_eq!(input, b"Reply with exactly: OK\n");
        }
        if call == self.fail_at {
            // Replace only this fixture's object directory. Unlike permission bits, a file
            // occupying the directory path also fails deterministically under a root runner.
            std::fs::rename(&self.objects, &self.backup).unwrap();
            std::fs::write(&self.objects, b"fixture CAS unavailable").unwrap();
            *self.outage.lock().unwrap() = true;
            assert!(cas.put(b"raw output could not be stored").is_err());
            let usage = review_core::task::usage::TaskTokenUsageV3 {
                input_tokens: Some((u128::from(u64::MAX) + 20).into()),
                output_tokens: Some(30_u128.into()),
                chargeable_tokens: (u128::from(u64::MAX) + 50).into(),
                ..Default::default()
            };
            ModelWorkerReturn {
                usage_observation: self.incomplete.then(|| {
                    review_core::task::usage::TaskUsageObservationV1 {
                        reported_usage: Some(usage.clone()),
                        charge_complete: false,
                    }
                }),
                message: Err("model output CAS publication failed".into()),
                raw_artifact_ids: vec![],
                usage: Some(usage),
            }
        } else {
            ModelWorkerReturn {
                usage_observation: None,
                message: Ok(b"OK".to_vec()),
                raw_artifact_ids: vec![cas.put(b"OK").unwrap()],
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(7)),
            }
        }
    }
}

#[test]
fn worker_and_provider_cas_failure_recover_full_reported_usage_without_another_call() {
    for fail_at in [0, 1] {
        recovery_case(fail_at, false);
    }
}

#[test]
fn incomplete_native_observation_survives_cas_outage_and_both_admission_and_worker_recovery() {
    for fail_at in [0, 1] {
        recovery_case(fail_at, true);
    }
}

fn recovery_case(fail_at: usize, incomplete: bool) {
    let mut f = super::wide_usage::configured_fixture();
    let model = OutageModel {
        objects: f._directory.path().join("cas/objects"),
        backup: f._directory.path().join("cas/objects-held"),
        outage: Mutex::new(false),
        calls: AtomicUsize::new(0),
        fail_at,
        incomplete,
    };
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
        .open_task(&f.cas, &f.revision_id, "writer", 5000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().is_err());
    drop(runtime);
    assert_eq!(model.calls.load(Ordering::SeqCst), fail_at + 1);
    let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
    // Sidecar access does not need the unavailable CAS. The full optional counters,
    // including zero, were persisted by the common runtime before canonical publication.
    let wall = f
        .store
        .task_attempt_wall(&run)
        .unwrap()
        .into_iter()
        .find(|wall| {
            wall.usage
                .as_ref()
                .is_some_and(|usage| usage.chargeable_tokens.get() == u128::from(u64::MAX) + 50)
        })
        .unwrap();
    let usage = wall.usage.as_ref().unwrap();
    assert_eq!(
        usage.input_tokens.map(|n| n.get()),
        Some(u128::from(u64::MAX) + 20)
    );
    assert_eq!(usage.output_tokens.map(|n| n.get()), Some(30));
    assert_eq!(usage.cache_read_tokens, None);
    let observation = f
        .store
        .task_attempt_usage_observation(&run, &wall.attempt_id)
        .unwrap();
    assert_eq!(observation.is_some(), incomplete);
    if let Some(observation) = &observation {
        assert!(!observation.charge_complete);
        assert_eq!(observation.reported_usage.as_ref(), Some(usage));
    }
    model.restore();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let execution = state.execution.as_ref().unwrap();
    assert_eq!(execution.budget.begun_attempts(), (fail_at + 1) as u64);
    assert!(execution.pending_attempts().contains(&wall.attempt_id));
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    assert!(
        f.store.recover_task_attempts(&f.cas, &lease).is_err(),
        "the old writer cannot abandon its own pending Attempt"
    );
    // Use real lease expiry without moving the Task's original absolute deadline.
    while SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        <= u128::from(state.lease_until_unix_ms())
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let recovered = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "recovery", 60000)
        .unwrap();
    f.store.recover_task_attempts(&f.cas, &recovered).unwrap();
    let events = f.store.len(&run).unwrap();
    f.store.recover_task_attempts(&f.cas, &recovered).unwrap();
    assert_eq!(f.store.len(&run).unwrap(), events);
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let execution = state.execution.unwrap();
    assert_eq!(
        execution.budget.committed_tokens(),
        u128::from(u64::MAX) + 50 + if fail_at == 1 { 7 } else { 0 }
    );
    assert!(execution.budget.breached());
    assert!(execution.pending_attempts().is_empty());
    assert_eq!(state.revision.limits, f.task.limits);
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    // A recovered charge cannot authorize fresh work or satisfy acceptance.
    let resumed = TaskRuntime::new(&mut f.store, &f.cas, recovered, &authority, &host).unwrap();
    assert!(!resumed.execute().unwrap().complete());
    assert!(!resumed.execute().unwrap().complete());
    assert!(
        !resumed
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .outputs
            .contains_key("root.nodes.write")
    );
    drop(resumed);
    assert_eq!(model.calls.load(Ordering::SeqCst), fail_at + 1);
    let settlement = f
        .store
        .replay(&run)
        .unwrap()
        .into_iter()
        .filter_map(|event| {
            let transition: review_core::task::event::TaskTransitionV1 =
                serde_json::from_value(event.payload).unwrap();
            let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            else {
                return None;
            };
            let decoded =
                review_store::store::task::execution::read_execution_record(&f.cas, &record_id)
                    .unwrap();
            match decoded.record {
                review_core::task::execution::TaskExecutionRecordV1::Settled {
                    attempt_id,
                    usage_id,
                    ..
                } if attempt_id == wall.attempt_id => {
                    Some((decoded.envelope.artifact_type, usage_id.unwrap()))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(settlement.len(), 1);
    let recorded = state_observations(&f.cas, &f.store, &run, &wall.attempt_id);
    assert_eq!(recorded.len(), usize::from(incomplete));
    if let Some(value) = recorded.first() {
        assert_eq!(value, &observation.unwrap());
    }
    assert_eq!(
        settlement[0].0,
        review_core::task::execution::TASK_EXECUTION_RECORD_V5
    );
    let recovered_usage =
        review_runner::task::usage::read_task_usage_exact(&f.cas, &settlement[0].1).unwrap();
    assert_eq!(
        recovered_usage.input_tokens.map(|n| n.get()),
        Some(u128::from(u64::MAX) + 20)
    );
    assert_eq!(recovered_usage.output_tokens.map(|n| n.get()), Some(30));
    assert_eq!(
        recovered_usage.chargeable_tokens.get(),
        u128::from(u64::MAX) + 50
    );
}

fn state_observations(
    cas: &Cas,
    store: &EventStore,
    run: &str,
    attempt: &str,
) -> Vec<review_core::task::usage::TaskUsageObservationV1> {
    let mut values = vec![];
    for event in store.replay(run).unwrap() {
        let transition = review_store::store::task::read_task_transition(&event).unwrap();
        if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
            transition.change
        {
            let record =
                review_store::store::task::execution::read_execution_record(cas, &record_id)
                    .unwrap();
            if let review_core::task::execution::TaskExecutionRecordV1::Settled {
                attempt_id,
                raw_artifact_ids,
                ..
            } = record.record
                && attempt_id == attempt
            {
                for id in raw_artifact_ids {
                    let envelope = cas.get_artifact(&id).unwrap();
                    if envelope.artifact_type == review_core::task::usage::TASK_USAGE_OBSERVATION_V1
                    {
                        assert!(
                            matches!(envelope.producer,Producer::Attempt{attempt_id,..} if attempt_id==attempt)
                        );
                        assert_eq!(envelope.input_artifacts.len(), 1);
                        values.push(serde_json::from_value(envelope.payload).unwrap());
                    }
                }
            }
        }
    }
    values
}
