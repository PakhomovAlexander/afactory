use super::*;
use review_graph::{NodeFailureClass, NodeOutcome};
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CheckedText;
impl TaskOperatorHost for CheckedText {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        DocumentDomain.prepare_context(cas, input, feedback)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        DocumentDomain.execute(cas, input, attempt)
    }
}
impl TaskDomain for CheckedText {
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        id: &str,
    ) -> Result<(), String> {
        DocumentDomain.validate_context(cas, input, feedback, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        DocumentDomain.validate_output(cas, task, plan, input, output)?;
        for value in output.outputs.values() {
            for id in &value.artifact_ids {
                if cas.get_json(id).map_err(|e| e.to_string())?["payload"]["text"]
                    == "domain-invalid"
                {
                    return Err("Private domain diagnostic must not enter retry context".into());
                }
            }
        }
        Ok(())
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        DocumentDomain.validate_result(cas, task, result)
    }
}

struct DomainModel {
    calls: AtomicUsize,
    always_reject: bool,
}
impl WorkerModelAdapter for DomainModel {
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
        bytes: Vec<u8>,
        _: std::time::Duration,
        writable: bool,
    ) -> ModelWorkerReturn {
        assert!(!writable);
        let count = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(count < 2, "The original two-Attempt limit must hold");
        let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["feedback"].as_array().unwrap().len(), count);
        if count == 1 {
            let feedback = &request["feedback"][0];
            assert_eq!(feedback["artifact_type"], "af/TaskRetryFeedback@1");
            assert_eq!(feedback["payload"]["code"], "output_admission_rejected");
            assert_eq!(feedback["payload"].as_object().unwrap().len(), 3);
            assert!(!String::from_utf8_lossy(&bytes).contains("Private domain diagnostic"));
        }
        let text = if count == 0 || self.always_reject {
            "domain-invalid"
        } else {
            "A checked migration guide"
        };
        let message = serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":text}]}})).unwrap();
        ModelWorkerReturn {
            usage_observation: None,
            raw_artifact_ids: vec![cas.put(&message).unwrap()],
            message: Ok(message),
            usage: Some(
                review_runner::TokenUsage::charge_only(if count == 0 { 17 } else { 23 }).into(),
            ),
        }
    }
}

#[test]
fn domain_output_rejection_retries_with_durable_typed_feedback_and_exact_charge() {
    for always_reject in [false, true] {
        let mut f = Fixture::with_model("unused model fixture", true);
        let model = DomainModel {
            calls: AtomicUsize::new(0),
            always_reject,
        };
        let slot = f.plan.bindings.keys().next().unwrap().clone();
        let models = BTreeMap::from([(
            slot.clone(),
            TaskModelBinding {
                binding: f.plan.bindings[&slot].clone(),
                adapter: &model as &dyn WorkerModelAdapter,
            },
        )]);
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &CheckedText,
            &models,
        )
        .unwrap();
        let planning = review_pipeline::task::planning::PlanningTaskHost {
            inner: &host,
            task: &f.task,
            validate_proposal: &|_, _, _| panic!("Business output must not invoke a Planner"),
        };
        let authority = CapturedTaskAuthority::new(&f.compiler, &planning, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "domain-retry", 60_000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let mut retained_feedback = None;
        for _ in 0..2 {
            let runtime =
                TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &planning)
                    .unwrap();
            let report = runtime.execute().unwrap();
            assert_eq!(report.complete(), !always_reject, "{report:?}");
            if always_reject && retained_feedback.is_none() {
                assert!(matches!(
                    report.outcome("root.nodes.write"),
                    Some(NodeOutcome::Failed {
                        class: Some(NodeFailureClass::RunBudgetExhausted),
                        ..
                    })
                ));
            }
            let execution = runtime.projection().unwrap().execution.unwrap();
            assert_eq!(model.calls.load(Ordering::SeqCst), 2);
            assert_eq!(execution.budget.begun_attempts(), 2);
            assert_eq!(execution.budget.committed_tokens(), 40);
            assert_eq!(execution.budget.reserved_tokens(), 0);
            assert!(execution.pending_attempts().is_empty());
            assert_eq!(
                execution.outputs.contains_key("root.nodes.write"),
                !always_reject
            );
            let feedback: Vec<_> = execution
                .retry_feedback("root.nodes.write")
                .into_iter()
                .map(|id| {
                    let bytes = f.cas.get(&id).unwrap();
                    (id, bytes)
                })
                .collect();
            assert_eq!(feedback.len(), if always_reject { 2 } else { 1 });
            if let Some(original) = &retained_feedback {
                assert_eq!(&feedback, original);
            } else {
                retained_feedback = Some(feedback);
            }
            drop(runtime);
            f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        }
    }
}

struct FeedbackFailure<'a>(&'a dyn TaskOperatorHost);
impl TaskOperatorHost for FeedbackFailure<'_> {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.0.prepare_context(cas, input, feedback)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        self.0.execute(cas, input, attempt)
    }
    fn output_rejection_feedback(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &PreparedTaskAttempt,
    ) -> Result<Option<String>, String> {
        Err("Injected feedback artifact persistence failure".into())
    }
}

#[test]
fn feedback_persistence_failure_does_not_authorize_a_retry_or_hide_known_usage() {
    let mut f = Fixture::with_model("unused model fixture", true);
    let model = DomainModel {
        calls: AtomicUsize::new(0),
        always_reject: true,
    };
    let slot = f.plan.bindings.keys().next().unwrap().clone();
    let models = BTreeMap::from([(
        slot.clone(),
        TaskModelBinding {
            binding: f.plan.bindings[&slot].clone(),
            adapter: &model as &dyn WorkerModelAdapter,
        },
    )]);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &CheckedText,
        &models,
    )
    .unwrap();
    let failing = FeedbackFailure(&host);
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "feedback-failure", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime =
        TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &failing).unwrap();
    let report = runtime.execute().unwrap();
    assert!(
        matches!(report.outcome("root.nodes.write"), Some(NodeOutcome::Failed { error, class: None }) if error.contains("Injected feedback artifact persistence failure"))
    );
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(
        execution.pending_attempts().len(),
        1,
        "Unsettled work remains available to the existing recovery path"
    );
    assert!(execution.retry_feedback("root.nodes.write").is_empty());
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    drop(runtime);
    let wall = f
        .store
        .attempt_wall(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
        .unwrap();
    assert_eq!(wall.len(), 1);
    assert_eq!(wall[0].usage.as_ref().unwrap().chargeable_tokens, 17);
}
