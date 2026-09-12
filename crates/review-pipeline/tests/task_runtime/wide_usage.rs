use super::*;

pub(super) fn configured_fixture() -> Fixture {
    let mut f = Fixture::with_model("unused", true);
    f.task.limits.verification.tokens = 1100;
    f.task.limits.verification.attempts = 2;
    f.task.limits.verification.wall_ms = 6000;
    f.revision_id = f
        .cas
        .put_artifact(
            TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.task).unwrap(),
        )
        .unwrap()
        .0;
    f.compiler = f.compiler.with_provider_admission(OperatorAttemptCost {
        tokens: 100,
        wall_ms: 1000,
    });
    (f.plan, f.graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    f.plan_id = f
        .cas
        .put_artifact(
            EXECUTION_PLAN_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;
    f
}

#[test]
fn failed_worker_and_provider_overruns_retain_exact_usage_in_the_common_runtime() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model {
        calls: AtomicUsize,
        overrun_at: usize,
    }
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
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let (bytes, cost) = if n == 0 {
                assert_eq!(input, b"Reply with exactly: OK\n");
                (b"OK".to_vec(), 7)
            } else {
                assert_eq!(self.overrun_at, 1, "an overrun reached another Worker");
                let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
                assert_eq!(request["inputs"].as_object().unwrap().len(), 1);
                assert!(request["inputs"]["input"].is_array());
                (serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}})).unwrap(),11)
            };
            ModelWorkerReturn {
                raw_artifact_ids: vec![cas.put(&bytes).unwrap()],
                message: if n == self.overrun_at {
                    Err("fixture transport failed after reporting usage".into())
                } else {
                    Ok(bytes)
                },
                usage: Some(
                    review_runner::TokenUsage {
                        input_tokens: Some(if n == self.overrun_at { u64::MAX } else { cost }),
                        chargeable_tokens: if n == self.overrun_at { u64::MAX } else { cost },
                        ..Default::default()
                    }
                    .into(),
                ),
            }
        }
    }
    for overrun_at in [0, 1] {
        let model = Model {
            calls: AtomicUsize::new(0),
            overrun_at,
        };
        let mut f = configured_fixture();
        assert_eq!(
            f.graph.allowances["root.providers.admit0"].verification_attempts,
            1
        );
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
            .open_task(&f.cas, &f.revision_id, "writer", 60000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        assert!(!report.complete(), "{report:?}");
        assert!(!runtime.execute().unwrap().complete());
        assert_eq!(model.calls.load(Ordering::SeqCst), overrun_at + 1);
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(
            execution.budget.committed_tokens(),
            u128::from(u64::MAX) + if overrun_at == 1 { 7 } else { 0 }
        );
        assert_eq!(execution.budget.begun_attempts(), (overrun_at + 1) as u64);
        assert!(execution.budget.breached());
        assert!(!execution.outputs.contains_key("root.nodes.write"));
        assert_eq!(
            execution.outputs.contains_key("root.providers.admit0"),
            overrun_at == 1
        );
        assert!(execution.pending_attempts().is_empty());
        drop(runtime);
        f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        let task_id = f.task.task_id.as_str();
        let state = f.store.task_projection(&f.cas, task_id).unwrap().unwrap();
        assert_eq!(
            state.execution.unwrap().budget.committed_tokens(),
            u128::from(u64::MAX) + if overrun_at == 1 { 7 } else { 0 }
        );
        let run = review_store::store::task::task_run_id(task_id).unwrap();
        let walls = f.store.attempt_wall(&run).unwrap();
        assert_eq!(walls.len(), overrun_at + 1);
        assert!(walls.iter().any(|wall| wall.usage.as_ref().is_some_and(
            |usage| usage.input_tokens == Some(u64::MAX) && usage.chargeable_tokens == u64::MAX
        )));
        let mut wide_receipts = 0;
        for event in f.store.replay(&run).unwrap() {
            let transition: review_core::task::event::TaskTransitionV1 =
                serde_json::from_value(event.payload).unwrap();
            if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            {
                let decoded =
                    review_store::store::task::execution::read_execution_record(&f.cas, &record_id)
                        .unwrap();
                if let review_core::task::execution::TaskExecutionRecordV1::Settled {
                    charged_tokens,
                    usage_id: Some(usage_id),
                    raw_artifact_ids,
                    ..
                } = decoded.record
                    && charged_tokens == u128::from(u64::MAX)
                {
                    assert_eq!(
                        decoded.envelope.artifact_type,
                        review_core::task::execution::TASK_EXECUTION_RECORD_V3
                    );
                    assert_eq!(raw_artifact_ids.len(), 1);
                    let usage =
                        review_runner::task::usage::read_task_usage_exact(&f.cas, &usage_id)
                            .unwrap();
                    assert_eq!(
                        usage.input_tokens.map(|n| n.get()),
                        Some(u128::from(u64::MAX))
                    );
                    assert_eq!(usage.chargeable_tokens.get(), u128::from(u64::MAX));
                    wide_receipts += 1;
                }
            }
        }
        assert_eq!(wide_receipts, 1);
    }
}

#[test]
fn one_attempt_retains_aggregate_charge_above_u64_through_failure_and_reopen() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Aggregate<'a> {
        inner: &'a dyn TaskOperatorHost,
        calls: AtomicUsize,
    }
    impl TaskOperatorHost for Aggregate<'_> {
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
            let mut result = self.inner.execute(cas, input, attempt);
            result.usage = Some(review_core::task::usage::TaskTokenUsageV3 {
                input_tokens: Some((u128::from(u64::MAX) + 17).into()),
                chargeable_tokens: (u128::from(u64::MAX) + 17).into(),
                ..Default::default()
            });
            result.charged_tokens = Some(u128::from(u64::MAX) + 17);
            result.outputs =
                Err("aggregate external work exceeded its original reservation".into());
            result
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
        .open_task(&f.cas, &f.revision_id, "aggregate", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let aggregate = Aggregate {
        inner: &host,
        calls: AtomicUsize::new(0),
    };
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &aggregate).unwrap();
    assert!(!runtime.execute().unwrap().complete());
    assert!(!runtime.execute().unwrap().complete());
    assert_eq!(aggregate.calls.load(Ordering::SeqCst), 1);
    let exact = u128::from(u64::MAX) + 17;
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), exact);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.budget.breached());
    assert!(execution.pending_attempts().is_empty());
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, exact);
    drop(runtime);
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(&f.task.task_id).unwrap();
    let walls = f.store.task_attempt_wall(&run).unwrap();
    assert_eq!(walls.len(), 1);
    let usage = walls[0].usage.as_ref().unwrap();
    assert_eq!(usage.chargeable_tokens.get(), exact);
    assert_eq!(usage.input_tokens.map(|n| n.get()), Some(exact));
    assert!(f.store.attempt_wall(&run).is_err());
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.execution.unwrap().budget.committed_tokens(), exact);
    let mut settled = 0;
    for event in f.store.replay(&run).unwrap() {
        let transition: review_core::task::event::TaskTransitionV1 =
            serde_json::from_value(event.payload).unwrap();
        if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
            transition.change
        {
            let decoded =
                review_store::store::task::execution::read_execution_record(&f.cas, &record_id)
                    .unwrap();
            if let review_core::task::execution::TaskExecutionRecordV1::Settled {
                charged_tokens,
                usage_id: Some(id),
                ..
            } = decoded.record
            {
                assert_eq!(charged_tokens, exact);
                assert_eq!(
                    decoded.envelope.artifact_type,
                    review_core::task::execution::TASK_EXECUTION_RECORD_V3
                );
                let envelope = f.cas.get_artifact(&id).unwrap();
                assert_eq!(
                    envelope.artifact_type,
                    review_core::task::usage::TASK_TOKEN_USAGE_V3
                );
                assert_eq!(envelope.payload["chargeable_tokens"], exact.to_string());
                settled += 1;
            }
        }
    }
    assert_eq!(settled, 1);
}
