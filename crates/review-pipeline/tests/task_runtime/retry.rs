use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_pipeline::task::planning::PlanningTaskHost;
use review_pipeline::task::provider::ProviderTaskDomain;
use std::sync::atomic::{AtomicUsize, Ordering};

struct NoRetry(AtomicUsize);
impl TaskOperatorHost for NoRetry {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        DocumentDomain.prepare_context(cas, input, _definition, attempt)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        _cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        DocumentDomain.execute(cas, input, _definition, attempt, _cancellation)
    }
}
impl TaskDomain for NoRetry {
    fn validate_retry(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        previous: &BTreeMap<String, TaskAttemptResultV1>,
    ) -> Result<(), String> {
        assert_eq!(input.node, "root.nodes.write");
        assert_eq!(previous.len(), 1);
        let TaskAttemptResultV1::Failed { diagnostic_id, .. } = previous.values().next().unwrap()
        else {
            panic!("retry policy must receive the recorded failure");
        };
        cas.verify(diagnostic_id).unwrap();
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("Fixture policy refuses retry of this failure".into())
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        DocumentDomain.validate_context(cas, input, attempt, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        DocumentDomain.validate_output(cas, task, plan, input, output, _definition)
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
fn every_host_layer_preserves_retry_refusal_on_reopened_execution() {
    let mut f = Fixture::new("import sys\nprint('failure')\nsys.exit(17)\n");
    let domain = NoRetry(AtomicUsize::new(0));
    let models = BTreeMap::new();
    let provider = ProviderTaskDomain {
        graph: &f.graph,
        models: &models,
        inner: &domain,
    };
    let captured = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &provider,
        &BTreeMap::new(),
    )
    .unwrap();
    let planning = PlanningTaskHost {
        inner: &captured,
        task: &f.task,
        validate_proposal: &|_, _, _| panic!("This fixture produces no proposal"),
    };
    let authority = CapturedTaskAuthority::new(&f.compiler, &planning, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "retry-policy", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    for _ in 0..2 {
        let runtime =
            TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &planning).unwrap();
        assert!(!runtime.execute().unwrap().complete());
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert!(execution.pending_attempts().is_empty());
        assert!(!execution.outputs.contains_key("root.nodes.write"));
        drop(runtime);
        f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    }
    assert!(domain.0.load(Ordering::SeqCst) >= 2);
}
