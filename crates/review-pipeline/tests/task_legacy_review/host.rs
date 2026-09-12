use super::*;
use review_core::EventType;
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
use review_pipeline::task::legacy_review::host::LegacyReviewTaskHost;
use review_pipeline::task::legacy_review::plan::LegacyReviewPlanCompiler;
use review_store::SharedEventStore;

#[path = "host/native.rs"]
mod native;

fn command_pipeline() -> String {
    let result = r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#;
    command_pipeline_returning(result)
}

fn command_pipeline_returning(result: &str) -> String {
    let command = format!("cat >/dev/null; printf '%s' '{result}'");
    PIPELINE.replace(
        "runner = { program = \"/bin/true\" }",
        &format!(
            "runner = {{ program = \"/bin/sh\", args = [{{value=\"-c\"}}, {{value={}}}] }}",
            serde_json::to_string(&command).unwrap()
        ),
    )
}

fn admit(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    let round = capture::open_round_with_pipeline(cas, store, definition);
    let compiler = LegacyReviewPlanCompiler::capture(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"Review host test engine").unwrap(),
        plan::settings(),
    )
    .unwrap();
    let task = compiler
        .prepare_revision(cas, "host-review", capture::limits())
        .unwrap();
    let revision = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let (compiled, _) = compiler.compile(cas, &revision).unwrap();
    let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &compiled);
    let authority = CapturedTaskAuthority::for_legacy_review(
        &compiler,
        &plan::RefuseExecution,
        &NoTaskDeveloper,
    );
    let lease = store.open_task(cas, &revision, "developer", 60000).unwrap();
    store
        .propose_task_plan(cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, &authority).unwrap();
    (compiler, lease)
}

#[test]
fn captured_command_review_uses_common_attempt_selection_and_replays_canonical_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let definition = command_pipeline();
    let round = capture::open_round_with_pipeline(&cas, &mut store, &definition);
    let compiler = LegacyReviewPlanCompiler::capture(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        cas.put(b"Review host test engine").unwrap(),
        plan::settings(),
    )
    .unwrap();
    let task = compiler
        .prepare_revision(&cas, "host-review", capture::limits())
        .unwrap();
    let revision = plan::artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (compiled, mapping) = compiler.compile(&cas, &revision).unwrap();
    let plan_id = plan::artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &compiled);
    let admission = CapturedTaskAuthority::for_legacy_review(
        &compiler,
        &plan::RefuseExecution,
        &NoTaskDeveloper,
    );
    let lease = store
        .open_task(&cas, &revision, "developer", 60000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &admission)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &admission).unwrap();
    let shared = SharedEventStore::new(&mut store);
    let mut expected_outputs = None;
    for _ in 0..2 {
        // A new host has no process memory of the previous execution.
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
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        let report = runtime.execute().unwrap();
        assert!(report.complete(), "{report:?}");
        let state = runtime.projection().unwrap();
        let execution = state.execution.unwrap();
        if let Some(expected) = &expected_outputs {
            assert_eq!(&execution.outputs, expected);
        }
        expected_outputs = Some(execution.outputs);
        let run = review_store::store::task::task_run_id(&task.task_id).unwrap();
        let locked = shared.lock().unwrap();
        assert_eq!(locked.attempt_wall(&run).unwrap().len(), 1);
        let events = locked.replay("review").unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == EventType::TaskReviewResultSelectedV1)
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event.event_type,
            EventType::AttemptDispatchedV1 | EventType::AttemptAdmittedV1
        )));
        for node in ["generation", "reviewer", "ledger"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event_type == EventType::NodeOutputReceiptV1
                        && event.node_id.as_deref() == Some(node))
                    .count(),
                1
            );
        }
    }
    let outputs = expected_outputs.unwrap();
    let ledger = &outputs[&mapping.compilation.nodes["ledger"].task_node]
        .1
        .outputs;
    assert_eq!(ledger["o0"], ledger["finding_set"]);
    assert_eq!(
        ledger["demand_set"].artifact_type,
        review_core::contract::DEMAND_SET_V1
    );
    assert_eq!(
        cas.get_artifact(&ledger["demand_set"].artifact_ids[0])
            .unwrap()
            .payload["demands"],
        serde_json::json!([])
    );
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

#[test]
fn review_publication_recovers_before_and_after_commit_without_another_worker_attempt() {
    for after_commit in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let path = directory.path().join("events.sqlite");
        let mut store = EventStore::open(&path).unwrap();
        let (compiler, lease) = admit(&cas, &mut store, &command_pipeline());
        let (output_id, outputs) = {
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
                after_commit,
                failed: Default::default(),
            };
            let runtime =
                TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &lost).unwrap();
            assert!(!runtime.execute().unwrap().complete());
            assert!(
                host.assemble_recorded_result(&cas)
                    .unwrap_err()
                    .contains("recover domain publication")
            );
            let state = runtime.projection().unwrap();
            assert!(matches!(
                state.phase,
                review_core::task::TaskPhaseV1::Waiting { .. }
            ));
            let execution = state.execution.unwrap();
            assert_eq!(execution.budget.begun_attempts(), 1);
            execution
                .outputs
                .values()
                .find(|(_, o)| o.outputs.contains_key("metadata"))
                .unwrap()
                .clone()
        };
        drop(store);
        let mut store = EventStore::open(&path).unwrap();
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
        shared
            .lock()
            .unwrap()
            .resume_task(&cas, &lease, &authority)
            .unwrap();
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        let report = runtime.execute().unwrap();
        assert!(report.complete(), "{report:?}");
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert!(
            execution
                .outputs
                .values()
                .any(|(id, output)| id == &output_id && output == &outputs)
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
    }
}

#[test]
fn captured_gate_records_checks_once_and_blocks_reviewers_when_required_check_fails() {
    for pass in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let definition = command_pipeline().replace(
            "id = \"reviewer\"",
            "id = \"reviewer\"\ngated_by = \"gate\"",
        ) + &format!(
            "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{{value=\"-c\"}},{{value=\"exit {}\"}}]\n",
            if pass { 0 } else { 1 }
        );
        let (compiler, lease) = admit(&cas, &mut store, &definition);
        let shared = SharedEventStore::new(&mut store);
        for _ in 0..2 {
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
            let runtime =
                TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                    .unwrap();
            let report = runtime.execute().unwrap();
            assert_eq!(
                report.complete(),
                pass,
                "{report:?}; checks: {:?}",
                shared
                    .lock()
                    .unwrap()
                    .replay("review")
                    .unwrap()
                    .iter()
                    .filter(|event| event.event_type == EventType::CheckCompletedV1)
                    .map(|event| &event.payload)
                    .collect::<Vec<_>>()
            );
            let execution = runtime.projection().unwrap().execution.unwrap();
            assert_eq!(execution.budget.begun_attempts(), if pass { 2 } else { 1 });
            assert_eq!(
                execution
                    .outputs
                    .values()
                    .any(|(_, output)| output.outputs.contains_key("metadata")),
                pass
            );
            let events = shared.lock().unwrap().replay("review").unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event_type == EventType::CheckCompletedV1)
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event_type == EventType::GateDecisionV1)
                    .count(),
                1
            );
        }
    }
}

#[test]
fn completed_review_retains_blocking_findings_and_required_demands() {
    for demand in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let mut returned = serde_json::json!({"verdict":"request-changes","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]});
        if demand {
            returned["benchmark_demands"] = serde_json::json!([{"claim":"latency is bounded", "why":"measure against the acceptance limit", "suggested_method":"run the latency benchmark"}]);
        } else {
            returned["findings"] = serde_json::json!([{"severity":"major","file":".af/pipelines/review.toml","line":1,"title":"Missing required behavior","body":"The required behavior is absent","fix":"Implement the missing behavior","confidence":0.9}]);
        }
        let (compiler, lease) = admit(
            &cas,
            &mut store,
            &command_pipeline_returning(&returned.to_string()),
        );
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
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        assert!(runtime.execute().unwrap().complete());
        let result = host.assemble_recorded_result(&cas).unwrap();
        assert_eq!(
            result.execution,
            review_core::task::TaskExecutionV1::Completed
        );
        assert_eq!(
            result.acceptance,
            review_core::task::TaskAcceptanceV1::Unsatisfied
        );
        assert_eq!(result.domain_conclusion, "review_changes_requested");
        assert!(result.missing_obligations.is_empty());
        let set = result
            .outputs
            .values()
            .find(|output| {
                output.artifact_type
                    == if demand {
                        review_core::contract::DEMAND_SET_V1
                    } else {
                        review_core::contract::FINDING_SET_V1
                    }
            })
            .unwrap();
        let payload = cas.get_artifact(&set.artifact_ids[0]).unwrap().payload;
        assert_eq!(
            payload[if demand { "demands" } else { "findings" }]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn cache_receipts_and_failed_gate_observations_survive_store_reopen() {
    use review_sandbox::{CacheError, CacheErrorKind, CacheKind, CacheLimits, CacheSource};
    for available in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let path = directory.path().join("events.sqlite");
        let mut store = EventStore::open(&path).unwrap();
        let definition = command_pipeline()
            .replace("version = 2", "version = 3\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"\ncaches=[\"cargo\"]")
            .replace("id = \"reviewer\"", "id = \"reviewer\"\ngated_by=\"gate\"")
            + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n";
        let (compiler, lease) = admit(&cas, &mut store, &definition);
        let cache_root = directory.path().join("cargo-cache");
        let cached = cache_root.join("registry/cache/index/example.crate");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(cached, b"offline crate").unwrap();
        let source = CacheSource {
            kind: CacheKind::Cargo,
            source: cache_root,
            limits: CacheLimits {
                max_bytes: 1024,
                max_files: 10,
                max_copy_bytes: 1024,
            },
        };
        let attempts = {
            let shared = SharedEventStore::new(&mut store);
            let host = LegacyReviewTaskHost::new(
                &cas,
                shared.clone(),
                &compiler,
                lease.clone(),
                BTreeMap::new(),
            )
            .unwrap();
            let host = if available {
                host.with_cache_sources(BTreeMap::from([(CacheKind::Cargo, source)]))
            } else {
                host.with_cache_source_resolver(|_| -> Result<CacheSource, CacheError> {
                    Err(CacheError::new(
                        CacheErrorKind::PolicyUnavailable,
                        "private path omitted",
                    ))
                })
            };
            let authority =
                CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
            let runtime =
                TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &host).unwrap();
            let report = runtime.execute().unwrap();
            assert_eq!(report.complete(), available, "{report:?}");
            runtime
                .projection()
                .unwrap()
                .execution
                .unwrap()
                .budget
                .begun_attempts()
        };
        drop(store);
        let mut store = EventStore::open(&path).unwrap();
        let shared = SharedEventStore::new(&mut store);
        // No cache mapping on the reopened host: conclusion uses captured execution facts.
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let result = host.assemble_recorded_result(&cas).unwrap();
        assert_eq!(
            result.acceptance == review_core::task::TaskAcceptanceV1::Satisfied,
            available
        );
        let locked = shared.lock().unwrap();
        assert_eq!(
            locked
                .task_projection(&cas, lease.task_id())
                .unwrap()
                .unwrap()
                .execution
                .unwrap()
                .budget
                .begun_attempts(),
            attempts
        );
        let events = locked.replay("review").unwrap();
        let event = events
            .iter()
            .find(|e| e.event_type == EventType::RunReportV5)
            .unwrap();
        let report: review_core::RunReportPayloadV5 =
            serde_json::from_value(event.payload.clone()).unwrap();
        assert_eq!(report.cache_snapshots.len(), usize::from(available));
        assert_eq!(report.cache_failures.len(), usize::from(!available));
        if !available {
            assert_eq!(
                report.cache_failures[0].reason,
                review_core::RunCacheFailureReasonV5::PolicyUnavailable
            );
            assert_eq!(report.cache_failures[0].node, "gate");
            assert!(
                !events
                    .iter()
                    .any(|e| e.event_type == EventType::TaskReviewResultSelectedV1)
            );
        }
    }
}

#[test]
fn a_stale_task_writer_cannot_publish_a_canonical_review_conclusion() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit(&cas, &mut store, &command_pipeline());
    let shared = SharedEventStore::new(&mut store);
    let old = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &old, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &old).unwrap();
    assert!(runtime.execute().unwrap().complete());
    shared
        .lock()
        .unwrap()
        .release_task_lease(&cas, &lease)
        .unwrap();
    let fresh_lease = shared
        .lock()
        .unwrap()
        .take_task_lease(&cas, lease.task_id(), "new-writer", 60000)
        .unwrap();
    assert!(
        old.assemble_recorded_result(&cas)
            .unwrap_err()
            .contains("lease")
    );
    assert!(
        !shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .iter()
            .any(|e| e.event_type.is_run_report())
    );
    let fresh = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        fresh_lease,
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        fresh.assemble_recorded_result(&cas).unwrap().acceptance,
        review_core::task::TaskAcceptanceV1::Satisfied
    );
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .iter()
            .filter(|e| e.event_type.is_run_report())
            .count(),
        1
    );
}

#[test]
fn canonical_review_conclusion_recovers_before_task_finish_without_reexecution() {
    use review_pipeline::task::host::TaskDomain;
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let path = directory.path().join("events.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let (compiler, lease) = admit(&cas, &mut store, &command_pipeline());
    let expected = {
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
        let runtime =
            TaskRuntime::with_store(shared, &cas, lease.clone(), &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        assert!(report.complete());
        let result = host
            .assemble_result(&cas, &runtime.projection().unwrap(), &report)
            .unwrap();
        assert_eq!(
            result.acceptance,
            review_core::task::TaskAcceptanceV1::Satisfied
        );
        result
    };
    drop(store);
    let mut store = EventStore::open(&path).unwrap();
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
    // The canonical Round is closed. Recovery must use the finish-only path.
    assert!(
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).is_err()
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result, expected);
    assert_eq!(host.assemble_recorded_result(&cas).unwrap(), expected);
    let mut forged = result.clone();
    forged.domain_conclusion = "a forged conclusion".into();
    let forged_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, forged);
    assert!(
        shared
            .lock()
            .unwrap()
            .finish_task(&cas, &lease, &forged_id, &authority)
            .is_err()
    );
    let result_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, result);
    shared
        .lock()
        .unwrap()
        .finish_task(&cas, &lease, &result_id, &authority)
        .unwrap();
    let locked = shared.lock().unwrap();
    let state = locked
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert!(matches!(
        state.phase,
        review_core::task::TaskPhaseV1::Finished { .. }
    ));
    assert_eq!(state.execution.unwrap().budget.begun_attempts(), 1);
    let events = locked.replay("review").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type.is_run_report())
            .count(),
        1
    );
}
