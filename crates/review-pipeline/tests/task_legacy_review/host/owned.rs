use super::*;
use review_core::task::execution::TaskExecutionRecordV1;
use review_core::task::plan::WorkerExecutionV1;
use review_graph::Dispatch;
use review_pipeline::task::host::TaskDomain;
use review_pipeline::task::legacy_review::plan::ReviewPlanSettingsV2;
use serde_json::json;

fn definition(fail: bool) -> String {
    let result = r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[]}"#;
    let command = if fail {
        "cat >/dev/null; exit 1".to_owned()
    } else {
        format!("cat >/dev/null; printf '%s' '{result}'")
    };
    include_str!("../../../../review-config/tests/fixtures/dynamic-v5.toml")
        .replace(
            "[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n",
            "",
        )
        .replace(
            "runner = { program = \"/bin/true\" }",
            &format!(
                "runner = {{ program=\"/bin/sh\", args=[{{value=\"-c\"}},{{value={}}}] }}",
                serde_json::to_string(&command).unwrap()
            ),
        )
        + "\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n"
}

fn admit_owned(
    cas: &Cas,
    store: &mut EventStore,
    fail: bool,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    admit_owned_with_limits(cas, store, fail, capture::limits())
}

fn admit_owned_with_limits(
    cas: &Cas,
    store: &mut EventStore,
    fail: bool,
    limits: review_core::task::TaskLimitsV1,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    let round = capture::open_round_with_pipeline(cas, store, &definition(fail));
    let mut settings = plan::settings();
    settings.executions = BTreeMap::from([
        ("scatter".into(), WorkerExecutionV1::Command {}),
        ("closeout".into(), WorkerExecutionV1::Command {}),
    ]);
    let compiler = LegacyReviewPlanCompiler::capture_v3(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"owned Review host fixture").unwrap(),
        ReviewPlanSettingsV2 {
            review: settings,
            provider_probes: BTreeMap::new(),
        },
    )
    .unwrap();
    let task = compiler
        .prepare_revision(cas, "owned-review", limits)
        .unwrap();
    let revision = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let compiled = compiler.compile(cas, &revision).unwrap().0;
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
fn owned_review_runs_real_slice_attempts_under_one_task_and_replays_lossless_canonical_results() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let mut store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_owned(&cas, &mut store, false);
    let shared = SharedEventStore::new(&mut store);
    let mut expected = None;
    for pass in 0..2 {
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
        if pass == 0 {
            let state = runtime.projection().unwrap();
            let captured = compiler
                .recompile(&cas, &state.revision, &runtime_plan(&cas, &state))
                .unwrap();
            let plan = captured.compilation.graph.scheduler_plan().unwrap();
            let report = review_graph::Scheduler::new(&plan)
                .with_parallelism(1)
                .run(&runtime);
            assert!(report.complete(), "one-slot owned execution: {report:?}");
        }
        let report = runtime.execute().unwrap();
        assert!(report.complete(), "{report:?}");
        let state = runtime.projection().unwrap();
        let execution = state.execution.as_ref().unwrap();
        let (owner, template) = execution.graph.owned_children.iter().next().unwrap();
        assert!(!execution.graph.allowances.contains_key(owner));
        let parent = &execution.invocations[owner].0;
        let registered = shared
            .lock()
            .unwrap()
            .get_task_owned_children(&cas, lease.task_id(), parent)
            .unwrap()
            .unwrap();
        assert_eq!(registered.child_set().children.len(), 2);
        let facts = shared
            .lock()
            .unwrap()
            .task_owned_child_evidence(&cas, &registered)
            .unwrap();
        assert!(
            facts
                .iter()
                .all(|fact| fact.completed_artifact_ids.is_some()),
            "{facts:?}"
        );
        let attempts = execution.attempt_accounting();
        assert_eq!(
            attempts.len(),
            4,
            "Gate, two Slice Workers, closeout; no paid parent"
        );
        assert!(
            attempts
                .iter()
                .all(|attempt| attempt.reservation.node != *owner)
        );
        let events = shared.lock().unwrap().replay("review").unwrap();
        for child in &registered.child_set().children {
            let slice: review_core::ReviewSliceV1 =
                serde_json::from_value(cas.get_artifact(&child.source_item_id).unwrap().payload)
                    .unwrap();
            assert!(slice.runtime_node_id.starts_with("scatter#slice:"));
            assert_eq!(
                events
                    .iter()
                    .filter(
                        |event| event.event_type == EventType::TaskReviewResultSelectedV1
                            && event.node_id.as_deref() == Some(&slice.runtime_node_id)
                    )
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event_type == EventType::NodeOutputReceiptV1
                        && event.node_id.as_deref() == Some(&slice.runtime_node_id))
                    .count(),
                1
            );
            assert_eq!(
                execution
                    .resolve_node(&child.node)
                    .unwrap()
                    .definition
                    .contract,
                template.contract
            );
        }
        let output = &execution.outputs[owner].1;
        let shards: review_core::ShardSetV1 = serde_json::from_value(
            cas.get_artifact(&output.outputs["o0"].artifact_ids[0])
                .unwrap()
                .payload,
        )
        .unwrap();
        assert!(shards.complete());
        assert_eq!(shards.shards.len(), 2);
        let records: Vec<_> = shared
            .lock()
            .unwrap()
            .replay(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                let transition: review_core::task::event::TaskTransitionV1 =
                    serde_json::from_value(event.payload).unwrap();
                match transition.change {
                    review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } => {
                        Some(
                            review_store::store::task::execution::read_execution_record(
                                &cas, &record_id,
                            )
                            .unwrap(),
                        )
                    }
                    _ => None,
                }
            })
            .collect();
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record.record,
                    TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record.record,
                    TaskExecutionRecordV1::OwnedChildPublished { .. }
                ))
                .count(),
            2
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record.record,
                    TaskExecutionRecordV1::OwnedChildrenCompleted { .. }
                ))
                .count(),
            1
        );
        if let Some((outputs, count)) = &expected {
            assert_eq!(&execution.outputs, outputs);
            assert_eq!(records.len(), *count);
        } else {
            expected = Some((execution.outputs.clone(), records.len()));
        }
        // Validation callbacks execute while the Store mutex is already held.
        let _locked = shared.lock().unwrap();
        host.validate_owned_children(
            &cas,
            &state.revision,
            &runtime_plan(&cas, &state),
            &execution.invocations[owner].1,
            registered.child_set(),
        )
        .unwrap();
        host.validate_owned_completion(
            &cas,
            &state.revision,
            &runtime_plan(&cas, &state),
            &execution.invocations[owner].1,
            registered.child_set(),
            &facts,
            output,
        )
        .unwrap();
        drop(_locked);
        if pass == 1 {
            let result = host.assemble_recorded_result(&cas).unwrap();
            assert_eq!(
                result.execution,
                review_core::task::TaskExecutionV1::Completed
            );
            assert_eq!(
                result.acceptance,
                review_core::task::TaskAcceptanceV1::Satisfied,
                "{:?}",
                shared
                    .lock()
                    .unwrap()
                    .replay("review")
                    .unwrap()
                    .into_iter()
                    .filter(|event| event.event_type.is_run_report())
                    .map(|event| event.payload)
                    .collect::<Vec<_>>()
            );
            let result_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, result);
            shared
                .lock()
                .unwrap()
                .finish_task(&cas, &lease, &result_id, &authority)
                .unwrap();
            assert_eq!(
                runtime.projection().unwrap().phase,
                review_core::task::TaskPhaseV1::Finished { result_id }
            );
        }
    }
}

fn runtime_plan(
    cas: &Cas,
    state: &review_store::store::task::TaskProjection,
) -> review_core::task::plan::ExecutionPlanV1 {
    serde_json::from_value(
        cas.get_artifact(state.plan_id.as_ref().unwrap())
            .unwrap()
            .payload,
    )
    .unwrap()
}

#[test]
fn failed_owned_workers_still_publish_every_slice_and_cannot_be_changed_to_complete() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let mut store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_owned(&cas, &mut store, true);
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
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let _ = runtime.execute().unwrap();
    let state = runtime.projection().unwrap();
    let execution = state.execution.as_ref().unwrap();
    let owner = execution.graph.owned_children.keys().next().unwrap();
    let registered = shared
        .lock()
        .unwrap()
        .get_task_owned_children(&cas, lease.task_id(), &execution.invocations[owner].0)
        .unwrap()
        .unwrap();
    let facts = shared
        .lock()
        .unwrap()
        .task_owned_child_evidence(&cas, &registered)
        .unwrap();
    let output = &execution.outputs[owner].1;
    let id = &output.outputs["o0"].artifact_ids[0];
    let envelope = cas.get_artifact(id).unwrap();
    let shards: review_core::ShardSetV1 = serde_json::from_value(envelope.payload.clone()).unwrap();
    assert_eq!(shards.shards.len(), 2);
    assert!(!shards.complete());
    assert!(
        shards
            .shards
            .iter()
            .all(|shard| !matches!(shard.outcome, review_core::ShardOutcomeV1::Completed { .. }))
    );
    let mut forged = output.clone();
    let mut payload = envelope.payload;
    payload["shards"][0]["outcome"] = json!({"kind":"completed","result_artifact_ids":[id]});
    let bad = cas
        .put_artifact(
            envelope.artifact_type,
            envelope.producer,
            envelope.input_artifacts,
            envelope.subject_snapshot_id,
            payload,
        )
        .unwrap()
        .0;
    forged.outputs.get_mut("o0").unwrap().artifact_ids = vec![bad];
    let _locked = shared.lock().unwrap();
    assert!(
        host.validate_owned_completion(
            &cas,
            &state.revision,
            &runtime_plan(&cas, &state),
            &execution.invocations[owner].1,
            registered.child_set(),
            &facts,
            &forged
        )
        .is_err()
    );
    assert!(
        host.validate_owned_completion(
            &cas,
            &state.revision,
            &runtime_plan(&cas, &state),
            &execution.invocations[owner].1,
            registered.child_set(),
            &facts[..1],
            output
        )
        .is_err()
    );
}

struct Interrupted<'a, 'store, 'host> {
    runtime: &'a TaskRuntime<'store, 'host>,
    after_registration: bool,
}
impl review_graph::Dispatch for Interrupted<'_, '_, '_> {
    fn requires_successful_predecessors(&self, n: &review_graph::Node) -> bool {
        self.runtime.requires_successful_predecessors(n)
    }
    fn task_node_selected(
        &self,
        n: &review_graph::Node,
        i: &review_graph::ArtifactMap,
    ) -> Result<bool, String> {
        self.runtime.task_node_selected(n, i)
    }
    fn coordinates_owned_children(&self, n: &review_graph::Node) -> bool {
        self.runtime.coordinates_owned_children(n)
    }
    fn expand_owned_children(
        &self,
        n: &review_graph::Node,
        i: &review_graph::ArtifactMap,
    ) -> Result<Vec<review_graph::OwnedChildDispatch>, String> {
        let children = self.runtime.expand_owned_children(n, i)?;
        if self.after_registration {
            Err("fixture stopped after durable complete registration".into())
        } else {
            Ok(children)
        }
    }
    fn complete_owned_children(
        &self,
        _: &review_graph::Node,
        _: &review_graph::ArtifactMap,
        _: &[(String, review_graph::NodeOutcome)],
    ) -> Result<review_graph::ArtifactMap, String> {
        Err("fixture stopped before parent sealing".into())
    }
    fn record_invocation(
        &self,
        n: &review_graph::Node,
        i: &review_graph::ArtifactMap,
    ) -> Result<(), String> {
        self.runtime.record_invocation(n, i)
    }
    fn run(
        &self,
        n: &review_graph::Node,
        i: &review_graph::ArtifactMap,
    ) -> Result<review_graph::ArtifactMap, String> {
        self.runtime.run(n, i)
    }
    fn record_outputs(
        &self,
        n: &review_graph::Node,
        o: &review_graph::ArtifactMap,
    ) -> Result<(), String> {
        if n.id.contains(".slice") {
            Err("fixture lost publication before common child output".into())
        } else {
            self.runtime.record_outputs(n, o)
        }
    }
    fn gate_passed(&self, n: &str, o: &review_graph::ArtifactMap) -> bool {
        self.runtime.gate_passed(n, o)
    }
    fn failure_class(&self, n: &str) -> Option<review_graph::NodeFailureClass> {
        self.runtime.failure_class(n)
    }
}

#[test]
fn selected_owned_outputs_recover_after_publication_loss_without_another_child_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let mut store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_owned(&cas, &mut store, false);
    let shared = SharedEventStore::new(&mut store);
    let selected;
    {
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
        let state = runtime.projection().unwrap();
        let graph = compiler
            .recompile(&cas, &state.revision, &runtime_plan(&cas, &state))
            .unwrap()
            .compilation
            .graph;
        let plan = graph.scheduler_plan().unwrap();
        let interrupted = Interrupted {
            runtime: &runtime,
            after_registration: false,
        };
        let _ = review_graph::Scheduler::new(&plan)
            .with_parallelism(1)
            .run(&interrupted);
        let state = runtime.projection().unwrap();
        let execution = state.execution.unwrap();
        selected = execution
            .attempt_accounting()
            .into_iter()
            .filter(|a| a.reservation.node.contains(".slice"))
            .map(|a| a.attempt_id)
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 2);
        assert!(
            execution
                .outputs
                .keys()
                .all(|node| !node.contains(".slice"))
        );
        assert!(
            !interrupted.coordinates_owned_children(&review_graph::Node::new(
                "invented",
                review_graph::NodeKind::Task
            ))
        );
    }
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(
        selected,
        execution
            .attempt_accounting()
            .into_iter()
            .filter(|a| a.reservation.node.contains(".slice"))
            .map(|a| a.attempt_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(execution.attempt_accounting().len(), 4);
}

#[test]
fn expired_resume_records_all_unstarted_slices_as_missing_without_new_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let mut store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
    let mut limits = capture::limits();
    limits.deadline_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 5000;
    let deadline = limits.deadline_unix_ms;
    let (compiler, lease) = admit_owned_with_limits(&cas, &mut store, false, limits);
    let shared = SharedEventStore::new(&mut store);
    {
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
        let state = runtime.projection().unwrap();
        let graph = compiler
            .recompile(&cas, &state.revision, &runtime_plan(&cas, &state))
            .unwrap()
            .compilation
            .graph;
        let plan = graph.scheduler_plan().unwrap();
        let _ = review_graph::Scheduler::new(&plan)
            .with_parallelism(1)
            .run(&Interrupted {
                runtime: &runtime,
                after_registration: true,
            });
        let state = runtime.projection().unwrap();
        let execution = state.execution.unwrap();
        let owner = graph.owned_children.keys().next().unwrap();
        assert!(
            shared
                .lock()
                .unwrap()
                .get_task_owned_children(&cas, lease.task_id(), &execution.invocations[owner].0)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            execution.attempt_accounting().len(),
            0,
            "the short deadline cannot fit the Gate allowance; registered children never began"
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    std::thread::sleep(std::time::Duration::from_millis(
        deadline.saturating_sub(now) + 5,
    ));
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(!report.complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    let owner = execution.graph.owned_children.keys().next().unwrap();
    assert_eq!(execution.attempt_accounting().len(), 0);
    let output = execution
        .outputs
        .get(owner)
        .unwrap_or_else(|| panic!("expired owner did not seal factual ShardSet: {report:?}"));
    let shards: review_core::ShardSetV1 = serde_json::from_value(
        cas.get_artifact(&output.1.outputs["o0"].artifact_ids[0])
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(shards.shards.len(), 2);
    assert!(
        shards
            .shards
            .iter()
            .all(|shard| matches!(shard.outcome, review_core::ShardOutcomeV1::Missing { .. }))
    );
}
