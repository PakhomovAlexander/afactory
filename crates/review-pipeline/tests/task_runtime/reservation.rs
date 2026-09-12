use super::*;
use review_store::store::task::execution::ReservedTaskAttempt;
use review_store::store::task::task_run_id;
use std::sync::atomic::{AtomicUsize, Ordering};

struct IdentityHost {
    task_id: String,
    change_identity: bool,
    calls: AtomicUsize,
}

fn exact_context(input: &TaskInvocationV1, attempt: &ReservedTaskAttempt) -> serde_json::Value {
    json!({"attempt_id":attempt.id(), "reservation_id":attempt.reservation().id,
        "deadline":attempt.reservation().deadline_unix_ms, "tokens":attempt.reservation().tokens,
        "invocation_id":attempt.invocation_id(), "input":input, "feedback":attempt.feedback_ids()})
}

impl TaskOperatorHost for IdentityHost {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        panic!("Attempt-dependent context cannot render before reservation")
    }
    fn prepare_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        let mut value = exact_context(input, attempt);
        if self.change_identity {
            value["attempt_id"] = json!("invented");
        }
        cas.put_json(&value).map_err(|e| e.to_string())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let attempt = attempt.unwrap();
        let context = cas.get_json(attempt.context_id()).unwrap();
        assert_eq!(context["attempt_id"], attempt.id());
        assert_eq!(context["reservation_id"], attempt.reservation().id);
        assert_eq!(context["input"], serde_json::to_value(input).unwrap());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let id = cas
            .put_artifact(
                "af/CheckedDocument@1",
                Producer::Attempt {
                    run_id: task_run_id(&self.task_id).unwrap(),
                    node_id: input.node.clone(),
                    attempt_id: attempt.id().into(),
                },
                vec![],
                None,
                json!({"outcome":"passed", "text":"exact reserved identity inspected"}),
            )
            .unwrap()
            .0;
        TaskWorkOutput {
            outputs: Ok(BTreeMap::from([(
                "output".into(),
                ArtifactInputV1 {
                    artifact_type: "af/CheckedDocument@1".into(),
                    artifact_ids: vec![id],
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: None,
                },
            )])),
            charged_tokens: Some(7),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskDomain for IdentityHost {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Err("Context admission requires the real reservation".into())
    }
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        if cas.get_json(id).map_err(|e| e.to_string())? != exact_context(input, attempt) {
            return Err("Context changed the real Attempt authority".into());
        }
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if input.node == "root.inputs" {
            assert_eq!(output.outputs, task.inputs);
            return Ok(());
        }
        DocumentDomain.validate_output(cas, task, plan, input, output)
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

#[test]
fn runtime_renders_and_admits_the_actual_reservation_before_work_and_replay() {
    for change_identity in [false, true] {
        let mut f = Fixture::with_model(SUCCESS, true);
        let host = IdentityHost {
            task_id: f.task.task_id.clone(),
            change_identity,
            calls: AtomicUsize::new(0),
        };
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "exact-context", 60_000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        for _ in 0..2 {
            let runtime =
                TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
            let report = runtime.execute().unwrap();
            assert_eq!(report.complete(), !change_identity, "{report:?}");
            drop(runtime);
            f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        }
        let execution = f
            .store
            .task_projection(&f.cas, &f.task.task_id)
            .unwrap()
            .unwrap()
            .execution
            .unwrap();
        assert_eq!(
            execution.budget.begun_attempts(),
            u64::from(!change_identity)
        );
        assert_eq!(
            execution.budget.committed_tokens(),
            if change_identity { 0 } else { 7 }
        );
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert!(execution.pending_attempts().is_empty());
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            usize::from(!change_identity)
        );
    }
}
