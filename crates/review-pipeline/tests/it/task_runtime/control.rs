use super::*;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[test]
fn precancelled_runtime_records_no_new_invocation_or_attempt() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let cancellation = AtomicBool::new(true);
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host)
        .unwrap()
        .with_cancellation(&cancellation);
    assert_eq!(
        runtime.execute().unwrap_err(),
        "Task has no execution to report"
    );
    assert!(
        runtime.projection().unwrap().execution.is_none(),
        "no invocation or budget mutation exists to report"
    );
}

#[test]
fn captured_command_cancellation_retains_both_streams_and_never_retries() {
    let markers = tempfile::tempdir().unwrap();
    let ready = markers.path().join("ready");
    let script = format!(
        "import sys,subprocess,os\nsys.stdin.read()\nchild=subprocess.Popen(['sleep','30'])\nprint('command stdout',flush=True)\nprint('command stderr',file=sys.stderr,flush=True)\nopen({:?},'w').write(str(os.getpid())+' '+str(child.pid))\nchild.wait()\n",
        ready.to_str().unwrap()
    );
    let mut f = Fixture::new(&script);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let cancellation = AtomicBool::new(false);
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host)
        .unwrap()
        .with_cancellation(&cancellation);
    std::thread::scope(|scope| {
        let cancel = scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                if std::fs::read_to_string(&ready).is_ok_and(|s| s.split_whitespace().count() == 2)
                {
                    break;
                }
                if Instant::now() >= until {
                    cancellation.store(true, Ordering::Release);
                    panic!("command never became ready");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            cancellation.store(true, Ordering::Release);
        });
        let report = runtime.execute().unwrap();
        cancel.join().unwrap();
        assert!(!report.complete());
    });
    let state = runtime.projection().unwrap();
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.committed_tokens(), 0);
    assert!(execution.pending_attempts().is_empty());
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    assert_eq!(
        f.cas
            .get(&review_store::canonical::blob_content_id(
                b"command stdout\n"
            ))
            .unwrap(),
        b"command stdout\n"
    );
    assert!(!runtime.execute().unwrap().complete());
    assert_eq!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .budget
            .begun_attempts(),
        1
    );
    drop(runtime);
    let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
    let mut retained_stderr = false;
    for event in f.store.replay(&run).unwrap() {
        let transition = review_store::store::task::read_task_transition(&event).unwrap();
        if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
            transition.change
        {
            let record =
                review_store::store::task::execution::read_execution_record(&f.cas, &record_id)
                    .unwrap();
            if let TaskExecutionRecordV1::Settled {
                raw_artifact_ids, ..
            } = record.record
            {
                retained_stderr = raw_artifact_ids.iter().any(|id| {
                    String::from_utf8_lossy(&f.cas.get(id).unwrap()).contains("command stderr")
                });
            }
        }
    }
    assert!(
        retained_stderr,
        "the settled failure must reference the captured stderr"
    );
}

#[test]
fn heartbeat_detects_a_replaced_writer_even_when_its_new_lease_is_far_from_renewal() {
    let mut f = Fixture::new(SUCCESS);
    let old = f
        .store
        .open_task(&f.cas, &f.revision_id, "old", 60_000)
        .unwrap();
    f.store.release_task_lease(&f.cas, &old).unwrap();
    let new = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "new", 60_000)
        .unwrap();
    let cancellation = AtomicBool::new(false);
    let shared = review_store::SharedEventStore::new(&mut f.store);
    let started = Instant::now();
    let result = review_pipeline::task::lease::with_heartbeat_controlled(
        &shared,
        &f.cas,
        &old,
        Some(&cancellation),
        || {
            while !cancellation.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_secs(3)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(cancellation.load(Ordering::Acquire));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(shared.lock().unwrap().task_lease_state(&new).is_ok());
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// Issue #134: the heartbeat renewed only through the connection the work was holding, so a
// Store operation longer than the lease's remaining time fenced a live writer. With the
// production 15 s lease, 10 s threshold and 1 s tick, the work keeps the Store from the start
// of its section until a second after that lease would have expired.
#[test]
fn a_live_writer_keeps_its_lease_while_its_work_holds_the_store_past_the_renewal_margin() {
    let mut f = Fixture::new(SUCCESS);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 15_000)
        .unwrap();
    let original = f.store.task_lease_state(&lease).unwrap();
    let cancellation = AtomicBool::new(false);
    let shared = review_store::SharedEventStore::new(&mut f.store);
    let result = review_pipeline::task::lease::with_heartbeat_controlled(
        &shared,
        &f.cas,
        &lease,
        Some(&cancellation),
        || {
            let held = shared.lock().unwrap();
            while unix_ms() < original + 1_000 {
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(held);
            // The same writer still holds authority and can keep mutating the Task.
            let mut store = shared.lock().unwrap();
            let until = store.task_lease_state(&lease).map_err(|e| e.to_string())?;
            store
                .renew_task_lease(&f.cas, &lease, 15_000)
                .map_err(|e| e.to_string())?;
            Ok(until)
        },
    );
    let until = result.expect("a live writer must not be fenced by its own long Store hold");
    assert!(
        until > original,
        "the heartbeat renewed while the work held the Store"
    );
    assert!(!cancellation.load(Ordering::Acquire));
    assert!(shared.lock().unwrap().task_lease_state(&lease).is_ok());
}

#[test]
fn a_successor_fences_the_old_writer_even_while_its_work_holds_the_store() {
    let mut f = Fixture::new(SUCCESS);
    let old = f
        .store
        .open_task(&f.cas, &f.revision_id, "old", 60_000)
        .unwrap();
    f.store.release_task_lease(&f.cas, &old).unwrap();
    let new = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "new", 60_000)
        .unwrap();
    let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
    let prefix = f.store.len(&run).unwrap();
    let cancellation = AtomicBool::new(false);
    let shared = review_store::SharedEventStore::new(&mut f.store);
    let started = Instant::now();
    let result = review_pipeline::task::lease::with_heartbeat_controlled(
        &shared,
        &f.cas,
        &old,
        Some(&cancellation),
        || {
            let _held = shared.lock().unwrap();
            while !cancellation.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_secs(4)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(cancellation.load(Ordering::Acquire));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the heartbeat observes the successor without waiting for the held Store"
    );
    let store = shared.lock().unwrap();
    assert_eq!(
        store.len(&run).unwrap(),
        prefix,
        "the old writer appended nothing"
    );
    assert!(store.task_lease_state(&new).is_ok());
}

#[test]
fn an_expired_lease_is_not_revived_while_its_work_holds_the_store() {
    let mut f = Fixture::new(SUCCESS);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 1)
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
    let prefix = f.store.len(&run).unwrap();
    let cancellation = AtomicBool::new(false);
    let shared = review_store::SharedEventStore::new(&mut f.store);
    let started = Instant::now();
    let result = review_pipeline::task::lease::with_heartbeat_controlled(
        &shared,
        &f.cas,
        &lease,
        Some(&cancellation),
        || {
            let _held = shared.lock().unwrap();
            while !cancellation.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_secs(4)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(cancellation.load(Ordering::Acquire));
    assert!(started.elapsed() < Duration::from_secs(3));
    let store = shared.lock().unwrap();
    assert_eq!(
        store.len(&run).unwrap(),
        prefix,
        "no replacement lease was minted"
    );
    assert!(store.task_lease_state(&lease).is_err());
}

#[test]
fn a_host_that_cannot_honor_an_interruption_refuses_instead_of_ignoring_it() {
    // There is no forwarding default any more: every operator takes the cancellation and owes
    // an explicit refusal when one is raised. PlanningTaskDomain is one of the pure domains that
    // answer this way, so it stands here for the contract DocumentTaskDomain and the pure
    // Optimization steps keep too.
    let f = Fixture::new(SUCCESS);
    let domain = review_pipeline::task::planning::PlanningTaskDomain {
        compiler: &f.compiler,
        task: &f.task,
        graph: &f.graph,
    };
    let input = TaskInvocationV1 {
        plan_id: f.plan_id.clone(),
        node: "root.nodes.work".into(),
        inputs: BTreeMap::new(),
    };
    let node = review_graph::task::CompiledNode {
        operator: review_graph::task::CompiledOperator::Select,
        contract: review_core::task::pipeline::PipelineContractV1 {
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        },
        inputs: BTreeMap::new(),
        conditions: vec![],
    };
    let raised = AtomicBool::new(true);
    let refused = domain.execute(&f.cas, &input, &node, None, Some(&raised));
    assert_eq!(
        refused.outputs.unwrap_err(),
        "Task execution was cancelled by its host"
    );
    assert_eq!(
        refused.charged_tokens,
        Some(0),
        "an unhonorable interruption is refused before any work runs"
    );
    // The same call without a raised interruption reaches the operator itself, so the refusal
    // above is the cancellation check and not the domain declining the node.
    let lowered = AtomicBool::new(false);
    for cancellation in [None, Some(&lowered)] {
        let reached = domain.execute(&f.cas, &input, &node, None, cancellation);
        assert_eq!(
            reached.outputs.unwrap_err(),
            "Planning context requires its fixed input-free operator"
        );
    }
}

#[test]
fn cancellation_between_successful_work_and_selection_retains_spend_but_refuses_output() {
    struct Host<'a>(&'a dyn TaskOperatorHost);
    impl TaskOperatorHost for Host<'_> {
        fn prepare_context(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            definition: &review_graph::task::CompiledNode,
            attempt: &review_store::store::task::execution::ReservedTaskAttempt,
        ) -> Result<String, String> {
            self.0.prepare_context(cas, input, definition, attempt)
        }
        fn execute(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            definition: &review_graph::task::CompiledNode,
            attempt: Option<&PreparedTaskAttempt>,
            cancellation: Option<&AtomicBool>,
        ) -> TaskWorkOutput {
            let mut returned = self
                .0
                .execute(cas, input, definition, attempt, cancellation);
            assert!(returned.outputs.is_ok());
            returned.charged_tokens = Some(7);
            cancellation.unwrap().store(true, Ordering::Release);
            returned
        }
    }
    let mut f = Fixture::new(SUCCESS);
    let inner = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &inner, &NoTaskDeveloper);
    let host = Host(&inner);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let flag = AtomicBool::new(false);
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host)
        .unwrap()
        .with_cancellation(&flag);
    assert!(!runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    assert!(matches!(
        execution.attempt_accounting()[0].result,
        Some(TaskAttemptResultV1::Failed { .. })
    ));
}
