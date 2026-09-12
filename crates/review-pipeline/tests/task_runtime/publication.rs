use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use review_core::task::report::*;
use review_store::SharedEventStore;
use review_store::store::task::TaskLease;

use super::*;

struct RecoveringHost<'a> {
    store: SharedEventStore<'a>,
    lease: TaskLease,
    inner: &'a dyn TaskDomain,
    proof: &'a std::path::Path,
    fail_after_commit: bool,
    before_attempt: bool,
    calls: &'a AtomicUsize,
}

impl TaskOperatorHost for RecoveringHost<'_> {
    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        if !self.before_attempt || input.node != "root.nodes.write" {
            return Ok(());
        }
        {
            let store = self.store.lock().unwrap();
            let state = store
                .task_projection(cas, self.lease.task_id())
                .unwrap()
                .unwrap();
            let execution = state.execution.unwrap();
            assert_eq!(execution.invocations[&input.node].0, id);
            if self.fail_after_commit {
                assert_eq!(execution.budget.begun_attempts(), 0);
                assert_eq!(execution.budget.reserved_tokens(), 0);
                assert!(execution.pending_attempts().is_empty());
            }
        }
        self.publish_proof(id)
    }

    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.inner.prepare_context(cas, input, feedback)
    }

    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.execute(cas, input, attempt)
    }

    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
        _: &TaskOutputV1,
    ) -> Result<(), String> {
        if self.before_attempt || input.node != "root.nodes.write" {
            return Ok(());
        }
        {
            let store = self.store.lock().unwrap();
            let state = store
                .task_projection(cas, self.lease.task_id())
                .unwrap()
                .unwrap();
            let execution = state.execution.unwrap();
            assert_eq!(execution.outputs[&input.node].0, id);
            assert_eq!(execution.budget.committed_tokens(), 7);
            assert!(execution.pending_attempts().is_empty());
        }
        self.publish_proof(id)
    }
}

impl RecoveringHost<'_> {
    fn publish_proof(&self, id: &str) -> Result<(), String> {
        // A domain publication succeeds durably, then its caller loses the acknowledgement.
        // Reopening must use the same evidence without invoking the Worker a second time.
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.proof)
        {
            Ok(mut file) => {
                file.write_all(id.as_bytes()).unwrap();
                file.sync_all().unwrap();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                assert_eq!(std::fs::read_to_string(self.proof).unwrap(), id);
            }
            Err(error) => return Err(error.to_string()),
        }
        if self.fail_after_commit {
            Err("domain acknowledgement was lost".into())
        } else {
            Ok(())
        }
    }
}

impl TaskDomain for RecoveringHost<'_> {
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        context: &str,
    ) -> Result<(), String> {
        self.inner.validate_context(cas, input, feedback, context)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.inner.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.inner.validate_result(cas, task, result)
    }
}

#[test]
fn domain_publication_recovers_after_restart_without_another_paid_attempt() {
    publication_recovers(false);
}

#[test]
fn domain_invocation_publication_recovers_before_any_attempt_or_context() {
    publication_recovers(true);
}

fn publication_recovers(before_attempt: bool) {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    struct Model;
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
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
            assert!(request["inputs"]["input"].is_array());
            let bytes = serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}})).unwrap();
            ModelWorkerReturn {
                raw_artifact_ids: vec![cas.put(&bytes).unwrap()],
                message: Ok(bytes),
                usage: Some(review_runner::TokenUsage::charge_only(7)),
            }
        }
    }
    let mut f = Fixture::with_model("unused", true);
    let proof = f._directory.path().join("domain-proof");
    let calls = AtomicUsize::new(0);
    for pass in 0..3 {
        f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        let models = f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &Model,
                    },
                )
            })
            .collect();
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &DocumentDomain,
            &models,
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = if pass == 0 {
            let lease = f
                .store
                .open_task(&f.cas, &f.revision_id, "publication-test", 60_000)
                .unwrap();
            f.store
                .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
                .unwrap();
            f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
            lease
        } else {
            let lease = f
                .store
                .take_task_lease(&f.cas, &f.task.task_id, "publication-test", 60_000)
                .unwrap();
            if pass == 1 {
                assert!(
                    f.store
                        .task_projection(&f.cas, &f.task.task_id)
                        .unwrap()
                        .unwrap()
                        .waiting_for_domain_publication(&f.cas)
                        .unwrap()
                );
                f.store.resume_task(&f.cas, &lease, &authority).unwrap();
            }
            lease
        };
        let shared = SharedEventStore::new(&mut f.store);
        let recovering = RecoveringHost {
            store: shared.clone(),
            lease: lease.clone(),
            inner: &host,
            proof: &proof,
            fail_after_commit: pass == 0,
            before_attempt,
            calls: &calls,
        };
        // The real Provider wrapper must preserve invocation publication and lost-ack
        // recovery even when this graph has no Provider operation of its own.
        let provider = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &f.graph,
            models: &models,
            inner: &recovering,
        };
        let runtime =
            TaskRuntime::with_store(shared.clone(), &f.cas, lease.clone(), &authority, &provider)
                .unwrap();
        let report = runtime.execute().unwrap();
        assert_eq!(report.complete(), pass != 0, "{report:?}");
        let state = runtime.projection().unwrap();
        assert_eq!(state.run_reports.len(), pass + 1);
        let id = state.run_reports.last().unwrap();
        let report: TaskRunReportV1 =
            serde_json::from_value(f.cas.get_json(id).unwrap()["payload"].clone()).unwrap();
        assert_eq!(
            report.nodes.iter().map(|n| &n.node).collect::<Vec<_>>(),
            f.graph.order.iter().collect::<Vec<_>>()
        );
        let node = report
            .nodes
            .iter()
            .find(|n| n.node == "root.nodes.write")
            .unwrap();
        if pass == 0 {
            assert_eq!(
                state.phase,
                TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsHuman
                }
            );
            let TaskNodeOutcomeV1::Failed {
                diagnostic_id,
                class,
            } = &node.outcome
            else {
                panic!("missing failure");
            };
            assert_eq!(*class, TaskFailureClassV1::DomainPublication);
            assert_eq!(
                f.cas.get_json(diagnostic_id).unwrap()["payload"]["message"],
                "domain acknowledgement was lost"
            );
        } else {
            assert!(matches!(node.outcome, TaskNodeOutcomeV1::Completed { .. }));
        }
        let execution = state.execution.unwrap();
        let executed = !before_attempt || pass != 0;
        assert_eq!(execution.budget.begun_attempts(), u64::from(executed));
        assert_eq!(
            execution.budget.committed_tokens(),
            if executed { 7 } else { 0 }
        );
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(executed));
        drop(runtime);
        shared
            .lock()
            .unwrap()
            .release_task_lease(&f.cas, &lease)
            .unwrap();
    }
}

#[test]
fn pre_attempt_context_failure_remains_inspectable_after_reopening() {
    struct Refused;
    impl TaskOperatorHost for Refused {
        fn prepare_context(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &[String],
        ) -> Result<String, String> {
            Err("declared context cannot be prepared".into())
        }
        fn execute(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            panic!("preparation failure cannot dispatch a Worker")
        }
    }
    let mut f = Fixture::new(SUCCESS);
    let host = CommandTaskHost::capture(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "context-refusal", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &Refused).unwrap();
    assert!(!runtime.execute().unwrap().complete());
    drop(runtime);
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    assert!(execution.pending_attempts().is_empty());
    assert_eq!(execution.budget.committed_tokens(), 0);
    let report: TaskRunReportV1 =
        serde_json::from_value(f.cas.get_json(&state.run_reports[0]).unwrap()["payload"].clone())
            .unwrap();
    let node = report
        .nodes
        .iter()
        .find(|n| n.node == "root.nodes.write")
        .unwrap();
    let TaskNodeOutcomeV1::Failed { diagnostic_id, .. } = &node.outcome else {
        panic!("missing context failure");
    };
    assert_eq!(
        f.cas.get_json(diagnostic_id).unwrap()["payload"]["message"],
        "declared context cannot be prepared"
    );
}
