//! Shared proof of expired selected-output recovery; setup is supplied by the captured compiler fixture.
use review_core::task::{TaskPhaseV1, TaskWaitingReasonV1};
use review_core::{EventType, task::TaskLimitsV1};
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
use review_pipeline::task::legacy_review::{
    host::LegacyReviewTaskHost, plan::LegacyReviewPlanCompiler,
};
use review_store::{Cas, EventStore, SharedEventStore, store::task::TaskLease};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

pub fn run_expired_waiting(
    admit: impl FnOnce(&Cas, &mut EventStore, TaskLimitsV1) -> (LegacyReviewPlanCompiler, TaskLease),
) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let path = directory.path().join("events.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let mut limits = TaskLimitsV1 {
        tokens: 100,
        max_attempts: 4,
        deadline_unix_ms: 1,
        verification: review_core::task::VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        },
    };
    // The captured reviewer allowance is seven seconds. Leave room for all local
    // preparation, then cross the actual immutable deadline only after selection.
    limits.deadline_unix_ms = now() + 15_000;
    let (compiler, lease) = admit(&cas, &mut store, limits.clone());
    let (output_id, original_execution) = {
        let shared = SharedEventStore::new(&mut store);
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let lost = LostPublication {
            inner: &host,
            after_commit: false,
            failed: Default::default(),
        };
        let runtime =
            TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &lost).unwrap();
        assert!(!runtime.execute().unwrap().complete());
        let state = runtime.projection().unwrap();
        assert_eq!(
            state.phase,
            TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsHuman
            }
        );
        assert!(state.waiting_for_domain_publication(&cas).unwrap());
        let execution = state.execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 1);
        let output = execution
            .outputs
            .values()
            .find(|(_, out)| out.outputs.contains_key("metadata"))
            .unwrap()
            .0
            .clone();
        assert!(
            now() < limits.deadline_unix_ms,
            "selection must precede the original deadline"
        );
        (output, execution)
    };
    let task_run = review_store::store::task::task_run_id(lease.task_id()).unwrap();
    let frozen_events = store.replay(&task_run).unwrap();
    store.release_task_lease(&cas, &lease).unwrap();
    drop(store);
    std::thread::sleep(Duration::from_millis(
        limits.deadline_unix_ms.saturating_sub(now()) + 5,
    ));
    let mut store = EventStore::open(&path).unwrap();
    let lease = store
        .take_task_lease(&cas, lease.task_id(), "recovery", 15_000)
        .unwrap();
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let error = shared
        .lock()
        .unwrap()
        .resume_task(&cas, &lease, &authority)
        .unwrap_err();
    assert!(error.to_string().contains("deadline expired"), "{error}");
    assert!(
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).is_err()
    );
    let transition = shared
        .lock()
        .unwrap()
        .resume_task_for_recording(&cas, &lease, &authority)
        .unwrap();
    assert_eq!(transition.event_type, EventType::TaskTransitionV4);
    assert_eq!(transition.payload["change"]["kind"], "recording_resumed");
    assert!(
        serde_json::from_value::<review_core::task::event::TaskTransitionV1>(
            transition.payload.clone()
        )
        .is_err()
    );
    assert!(
        shared
            .lock()
            .unwrap()
            .check_task_dispatch(&cas, &lease, &authority)
            .is_err()
    );
    assert!(
        shared
            .lock()
            .unwrap()
            .publish_task_review_result(&cas, &lease, &output_id, &authority)
            .is_err(),
        "ordinary selected-result publication keeps its frozen deadline fence"
    );
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let recovering = RecordingOnly {
        inner: &host,
        calls: &calls,
    };
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &recovering)
            .unwrap();
    let report = runtime.execute().unwrap();
    assert!(
        !report.complete(),
        "the unexecuted Ledger cannot become completed"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no paid or pure business operation may execute during recovery"
    );
    let events = shared.lock().unwrap().replay("review").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == EventType::TaskReviewResultSelectedV1)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == EventType::NodeOutputReceiptV1
                && e.node_id.as_deref() == Some("reviewer"))
            .count(),
        1
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(
        result.acceptance,
        review_core::task::TaskAcceptanceV1::Inconclusive
    );
    assert_ne!(
        result.execution,
        review_core::task::TaskExecutionV1::Completed
    );
    let result_id = artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&result_id).unwrap();
    let state = shared
        .lock()
        .unwrap()
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert_eq!(state.revision.limits, limits);
    let execution = state.execution.unwrap();
    assert_eq!(execution.outputs, original_execution.outputs);
    assert_eq!(
        execution.invocations, original_execution.invocations,
        "recovery must not admit even a pure new invocation"
    );
    assert_eq!(
        execution.budget.begun_attempts(),
        original_execution.budget.begun_attempts()
    );
    assert_eq!(
        execution.budget.committed_tokens(),
        original_execution.budget.committed_tokens()
    );
    assert!(execution.outputs.values().any(|(id, _)| id == &output_id));
    shared
        .lock()
        .unwrap()
        .release_task_lease(&cas, &lease)
        .unwrap();
    let reopened = EventStore::open_read_only(&path).unwrap();
    let events = reopened.replay(&task_run).unwrap();
    assert_eq!(
        &events[..frozen_events.len()],
        frozen_events.as_slice(),
        "old lifecycle and selected evidence bytes stay immutable"
    );
    let state = reopened
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert!(state.has_recording_recovery());
    assert_eq!(state.phase, TaskPhaseV1::Finished { result_id });
    assert_eq!(state.revision.limits, limits);
    (directory, lease.task_id().to_owned())
}

struct RecordingOnly<'a> {
    inner: &'a dyn review_pipeline::task::TaskOperatorHost,
    calls: &'a std::sync::atomic::AtomicUsize,
}
impl review_pipeline::task::TaskOperatorHost for RecordingOnly<'_> {
    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &review_core::task::execution::TaskInvocationV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_invocation(cas, id, input)
    }
    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        id: &str,
        output: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_output(cas, input, id, output)
    }
    fn prepare_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        panic!("recording recovery prepared a new context")
    }
    fn execute(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: Option<&review_store::store::task::execution::PreparedTaskAttempt>,
    ) -> review_pipeline::task::TaskWorkOutput {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("recording recovery executed new work")
    }
}

struct LostPublication<'a> {
    inner: &'a dyn review_pipeline::task::TaskOperatorHost,
    after_commit: bool,
    failed: std::sync::atomic::AtomicBool,
}
impl review_pipeline::task::TaskOperatorHost for LostPublication<'_> {
    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &review_core::task::execution::TaskInvocationV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_invocation(cas, id, input)
    }
    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        id: &str,
        output: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        if output.outputs.contains_key("metadata")
            && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            if self.after_commit {
                self.inner.commit_domain_output(cas, input, id, output)?;
            }
            return Err("simulated lost Review publication acknowledgement".into());
        }
        self.inner.commit_domain_output(cas, input, id, output)
    }
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.inner.prepare_context(cas, input, feedback)
    }
    fn prepare_context_for_attempt(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.inner.prepare_context_for_attempt(cas, input, attempt)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &review_core::task::execution::TaskInvocationV1,
        attempt: Option<&review_store::store::task::execution::PreparedTaskAttempt>,
    ) -> review_pipeline::task::TaskWorkOutput {
        self.inner.execute(cas, input, attempt)
    }
}

fn artifact(cas: &Cas, ty: &str, value: &impl serde::Serialize) -> String {
    cas.put_artifact(
        ty,
        review_core::Producer::KernelOperation {
            run_id: "review-plan-test".into(),
            node_id: None,
            operation_id: "capture".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}
