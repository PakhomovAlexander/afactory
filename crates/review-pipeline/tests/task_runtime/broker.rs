use super::*;
use review_broker::{
    BrokerError, Connector, ConnectorCall, ConnectorError, ConnectorReply, ExactBrokerClient,
};
use review_core::BrokerOperationPolicyV1;
use review_core::task::broker::TaskBrokerBindingV1;
use review_pipeline::task::broker::TaskBrokerProvider;
use review_store::store::task::task_run_id;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

// This installed fixture performs a local, zero-token Provider readiness probe. Business
// execution belongs to BrokerHost below; this adapter cannot execute a business request.
struct LocalReadiness;
impl review_runner::task::WorkerModelAdapter for LocalReadiness {
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
    ) -> review_runner::task::ModelWorkerReturn {
        assert_eq!(input, b"Reply with exactly: OK\n");
        assert!(!writable);
        review_runner::task::ModelWorkerReturn {
            message: Ok(b"OK".to_vec()),
            raw_artifact_ids: vec![cas.put(b"OK").unwrap()],
            usage: Some(review_runner::TokenUsage::charge_only(0).into()),
        }
    }
}

fn policies() -> Vec<BrokerOperationPolicyV1> {
    vec![BrokerOperationPolicyV1 {
        name: "complete".into(),
        destination: "fixture".into(),
        method: "generate".into(),
        max_request_bytes: 1024,
        max_response_bytes: 1024,
        max_calls: 2,
        max_usage: 20,
    }]
}

struct CasOutage {
    objects: std::path::PathBuf,
    backup: std::path::PathBuf,
    active: Mutex<bool>,
}
impl CasOutage {
    fn start(&self) {
        std::fs::rename(&self.objects, &self.backup).unwrap();
        std::fs::write(&self.objects, b"fixture CAS unavailable").unwrap();
        *self.active.lock().unwrap() = true;
    }
    fn restore(&self) {
        let mut active = self.active.lock().unwrap();
        if *active {
            std::fs::remove_file(&self.objects).unwrap();
            std::fs::rename(&self.backup, &self.objects).unwrap();
            *active = false;
        }
    }
}
impl Drop for CasOutage {
    fn drop(&mut self) {
        self.restore();
    }
}

struct PaidConnector {
    calls: Arc<AtomicUsize>,
    outage: Option<Arc<CasOutage>>,
}
impl Connector for PaidConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        assert_eq!(call.destination, "fixture");
        assert_eq!(call.credential(), b"local-fixture-credential");
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(ordinal < 2, "Broker dispatched after its exact overrun");
        if ordinal == 1
            && let Some(outage) = &self.outage
        {
            outage.start();
        }
        Ok(ConnectorReply::credential_free(
            b"paid response",
            if ordinal == 0 { 7 } else { u64::MAX },
        ))
    }
}

struct BrokerHost<'a> {
    inner: &'a dyn TaskDomain,
    panic_after_payment: bool,
    receipt_unavailable: bool,
    calls: AtomicUsize,
}
impl TaskOperatorHost for BrokerHost<'_> {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.inner.prepare_context(cas, input, feedback)
    }
    fn broker_operations(
        &self,
        _: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<Option<Vec<BrokerOperationPolicyV1>>, String> {
        Ok((input.node == "root.nodes.write").then(policies))
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("Broker-aware dispatch was bypassed")
    }
    fn execute_with_broker(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn ExactBrokerClient>,
    ) -> TaskWorkOutput {
        assert!(attempt.is_some());
        if input.node != "root.nodes.write" {
            assert!(broker.is_none());
            return self.inner.execute(cas, input, attempt);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let broker = broker.expect("started, durably bound capability");
        assert_eq!(
            broker.call("complete", b"first", 10).unwrap().body,
            b"paid response"
        );
        let second = broker.call("complete", b"second", 10);
        assert!(matches!(
            (&second, self.receipt_unavailable),
            (Err(BrokerError::ReceiptFailed), true) | (Err(BrokerError::UsageOverrun), false)
        ));
        assert!(broker.call("complete", b"third", 1).is_err());
        assert!(
            !self.panic_after_payment,
            "fixture panic after exact paid overrun"
        );
        TaskWorkOutput {
            usage: None,
            outputs: Err("malformed fixture output after payment".into()),
            charged_tokens: Some(3),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskDomain for BrokerHost<'_> {
    fn validate_broker_binding(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &TaskBrokerBindingV1,
    ) -> Result<(), String> {
        if binding.operations != policies()
            || !matches!(&binding.target, review_core::task::broker::TaskBrokerTargetV1::Worker { slot, invocation_policy_id }
                if plan.bindings.get(slot).map(|b| &b.invocation_policy_id) == Some(invocation_policy_id))
        {
            return Err("Test host admits only its exact fixed capability".into());
        }
        Ok(())
    }
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
fn broker_scope_preserves_exact_paid_usage_across_malformed_output_panic_and_cas_outage() {
    for (panic_after_payment, receipt_unavailable) in [(false, false), (true, false), (false, true)]
    {
        // Retain the existing separately accounted readiness Attempt. The business Worker
        // has its original captured model allowance; operations never create more Attempts.
        let mut f = super::wide_usage::configured_fixture();
        let models = f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &LocalReadiness as &dyn review_runner::task::WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let domain = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &f.graph,
            models: &models,
            inner: &DocumentDomain,
        };
        let inner = CapturedTaskHost::capture_with_models(
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
        let host = BrokerHost {
            inner: &inner,
            panic_after_payment,
            receipt_unavailable,
            calls: AtomicUsize::new(0),
        };
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(
                &f.cas,
                &f.revision_id,
                "broker-scope-test",
                if receipt_unavailable { 5000 } else { 60_000 },
            )
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let deadline = Arc::new(AtomicU64::new(0));
        let connector_calls = calls.clone();
        let captured_deadline = deadline.clone();
        let outage = receipt_unavailable.then(|| {
            Arc::new(CasOutage {
                objects: f._directory.path().join("cas/objects"),
                backup: f._directory.path().join("cas/objects-held"),
                active: Mutex::new(false),
            })
        });
        let connector_outage = outage.clone();
        let (slot, binding) = f.plan.bindings.iter().next().unwrap();
        let provider = TaskBrokerProvider::new(
            binding.clone(),
            b"local-fixture-credential".to_vec(),
            move |until| {
                captured_deadline.store(until, Ordering::SeqCst);
                Ok(Arc::new(PaidConnector {
                    calls: connector_calls.clone(),
                    outage: connector_outage.clone(),
                }))
            },
        )
        .unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host)
            .unwrap()
            .with_broker_provider(slot, &provider)
            .unwrap();
        let report = runtime.execute();
        let charged = u128::from(u64::MAX) + 7;
        if let Some(outage) = outage {
            assert!(report.is_err());
            drop(runtime);
            let run = task_run_id(lease.task_id()).unwrap();
            let wall = f
                .store
                .task_attempt_wall(&run)
                .unwrap()
                .into_iter()
                .find(|wall| wall.node_id == "root.nodes.write")
                .unwrap();
            assert_eq!(
                wall.usage.as_ref().unwrap().chargeable_tokens.get(),
                charged
            );
            outage.restore();
            f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
            assert_eq!(
                f.store
                    .task_projection(&f.cas, lease.task_id())
                    .unwrap()
                    .unwrap()
                    .execution
                    .unwrap()
                    .budget
                    .committed_tokens(),
                7
            );
            assert!(f.store.recover_task_attempts(&f.cas, &lease).is_err());
            // Pending work cannot be abandoned by releasing its current writer. Wait for
            // actual lease expiry; the original Task/Attempt deadlines remain unchanged.
            let until = f
                .store
                .task_projection(&f.cas, lease.task_id())
                .unwrap()
                .unwrap()
                .lease_until_unix_ms();
            while SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
                <= u128::from(until)
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let recovered = f
                .store
                .take_task_lease(&f.cas, lease.task_id(), "broker-recovery", 60_000)
                .unwrap();
            f.store.recover_task_attempts(&f.cas, &recovered).unwrap();
            let count = f.store.len(&run).unwrap();
            f.store.recover_task_attempts(&f.cas, &recovered).unwrap();
            assert_eq!(f.store.len(&run).unwrap(), count);
            let state = f
                .store
                .task_projection(&f.cas, lease.task_id())
                .unwrap()
                .unwrap();
            let execution = state.execution.unwrap();
            assert_eq!(execution.budget.committed_tokens(), charged);
            assert_eq!(execution.budget.begun_attempts(), 2);
            assert!(execution.pending_attempts().is_empty());
            assert!(!execution.outputs.contains_key("root.nodes.write"));
            assert_eq!(state.revision.limits, f.task.limits);
            assert_eq!(host.calls.load(Ordering::SeqCst), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            continue;
        }
        let report = report.unwrap();
        assert!(!report.complete());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{report:?}");
        let projection = runtime.projection().unwrap();
        let execution = projection.execution.unwrap();
        assert_eq!(
            execution.budget.begun_attempts(),
            2,
            "one readiness Attempt and one business Attempt"
        );
        assert_eq!(execution.budget.committed_tokens(), charged);
        assert!(execution.budget.breached());
        let attempt = execution
            .attempt_accounting()
            .into_iter()
            .find(|a| a.reservation.node == "root.nodes.write")
            .unwrap();
        assert_eq!(attempt.charged_tokens, charged);
        assert_eq!(
            deadline.load(Ordering::SeqCst),
            attempt.reservation.deadline_unix_ms
        );
        let _ = runtime.execute();
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        drop(runtime);
        let run = task_run_id(lease.task_id()).unwrap();
        let walls = f.store.task_attempt_wall(&run).unwrap();
        assert_eq!(walls.len(), 2);
        let wall = walls
            .iter()
            .find(|wall| wall.attempt_id == attempt.attempt_id)
            .unwrap();
        assert_eq!(
            wall.usage.as_ref().unwrap().chargeable_tokens.get(),
            charged
        );
        let receipts: Vec<_> = f
            .store
            .replay(&run)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == review_core::EventType::TaskBrokerTransitionV1)
            .collect();
        assert_eq!(
            receipts.len(),
            3,
            "one binding and two paid operations; the revoked handle admits no third operation"
        );
        let reopened = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        assert_eq!(
            reopened
                .task_projection(&f.cas, lease.task_id())
                .unwrap()
                .unwrap()
                .execution
                .unwrap()
                .budget
                .committed_tokens(),
            charged
        );
    }
}
