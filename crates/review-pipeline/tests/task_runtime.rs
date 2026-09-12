use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_core::Producer;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::*;
use review_graph::task::{CompiledTask, OperatorAttemptCost, OperatorSignature};
use review_pipeline::task::host::*;
use review_pipeline::task::*;
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::{Cas, EventStore};
use serde_json::json;

#[path = "task_runtime/publication.rs"]
mod publication;
#[path = "task_runtime/reservation.rs"]
mod reservation;
#[path = "task_runtime/retry.rs"]
mod retry;

struct Fixture {
    _directory: tempfile::TempDir,
    cas: Cas,
    store: EventStore,
    compiler: TaskPlanCompiler,
    task: TaskRevisionV1,
    revision_id: String,
    plan: ExecutionPlanV1,
    plan_id: String,
    graph: CompiledTask,
}

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-runtime-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}

impl Fixture {
    fn new(script: &str) -> Self {
        Self::with_model(script, false)
    }
    fn with_model(script: &str, model: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1");
        let pipeline: PipelineDefinitionV1 =
            serde_json::from_slice(&std::fs::read(root.join("pipeline-definition.json")).unwrap())
                .unwrap();
        let mut task: TaskRevisionV1 =
            serde_json::from_slice(&std::fs::read(root.join("task-revision.json")).unwrap())
                .unwrap();
        let policy = cas
            .put_json(&json!({"fixture":"trusted kind and invocation policy"}))
            .unwrap();
        let source = cas
            .put_artifact(
                "af/Requirements@1",
                producer(),
                vec![],
                None,
                json!({"text":"A checked migration guide"}),
            )
            .unwrap()
            .0;
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source.clone()];
        task.provenance.input_artifact_ids = vec![source];
        task.provenance.adapter_id = policy.clone();
        task.authority.policy_id = policy.clone();
        task.authority.allowed_effects.clear();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        task.limits.deadline_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000;
        task.limits.verification.wall_ms = 5000;
        if model {
            task.limits.tokens = 5000;
            task.limits.verification.tokens = 1000;
        }
        let revision_id = cas
            .put_artifact(
                TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&task).unwrap(),
            )
            .unwrap()
            .0;
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([(
                    "input".into(),
                    pipeline.contract.inputs["requirements"].clone(),
                )]),
                outputs: BTreeMap::from([("output".into(), output)]),
            },
            effects: BTreeSet::new(),
            evidence: BTreeMap::from([("output".into(), BTreeSet::from([policy.clone()]))]),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["author".into()]),
            worker_input_type: Some("af/Requirements@1".into()),
            worker_output_type: Some("af/CheckedDocument@1".into()),
            outcome_port: Some("output".into()),
            attempt: Some(OperatorAttemptCost {
                tokens: if model { 1000 } else { 0 },
                wall_ms: 5000,
            }),
        };
        let worker = TaskWorkerManifest {
            schema: "af.worker/1".into(),
            name: "builtin/document-author".into(),
            version: "1.0.0".into(),
            signature,
            runner: if model {
                TaskWorkerRunner::Model {
                    provider_kind: "fixture".into(),
                    model: "typed-model".into(),
                    effort: "high".into(),
                }
            } else {
                TaskWorkerRunner::Command { command: serde_json::from_value(json!({"program":"/usr/bin/python3", "args":[{"value":"@package/worker.py", "provenance":"literal"}]})).unwrap() }
            },
        };
        let input_schema = json!({"type":"object", "additionalProperties":false, "required":["input"], "properties":{
            "input":{"type":"array", "minItems":1, "maxItems":1, "items":{"type":"object", "additionalProperties":false,
                "required":["artifact_id","artifact_type","payload"], "properties":{
                    "artifact_id":{"type":"string"}, "artifact_type":{"const":"af/Requirements@1"},
                    "payload":{"type":"object", "additionalProperties":false, "required":["text"], "properties":{"text":{"type":"string"}}}
                }}}
        }});
        let output_schema = json!({"type":"object", "additionalProperties":false, "required":["outcome","text"],
            "properties":{"outcome":{"enum":["passed","failed","inconclusive"]},"text":{"type":"string"}}});
        let mut compiler = TaskPlanCompiler::new(
            policy.clone(),
            policy.clone(),
            BTreeMap::new(),
            BTreeMap::from([("checked".into(), "document".into())]),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        for (name, files) in [
            (
                "builtin/document",
                BTreeMap::from([(
                    "pipeline.toml".into(),
                    toml::to_string(&pipeline).unwrap().into_bytes(),
                )]),
            ),
            (
                "builtin/document-author",
                BTreeMap::from([
                    (
                        "worker.toml".into(),
                        toml::to_string(&worker).unwrap().into_bytes(),
                    ),
                    (
                        "input.schema.json".into(),
                        serde_json::to_vec(&input_schema).unwrap(),
                    ),
                    (
                        "outputs/output.schema.json".into(),
                        serde_json::to_vec(&output_schema).unwrap(),
                    ),
                    (
                        "instructions.md".into(),
                        b"Return the checked document using the declared output contract.".to_vec(),
                    ),
                    ("worker.py".into(), script.as_bytes().to_vec()),
                ]),
            ),
        ] {
            let pin = TaskPackagePin {
                version: "1.0.0".into(),
                digest: review_config::lock::package_digest_from_files(&files),
                path: "package".into(),
            };
            let files = files
                .into_iter()
                .map(|(path, bytes)| (format!("package/{path}"), bytes))
                .collect();
            compiler.capture_package(&cas, name, &pin, &files).unwrap();
        }
        compiler
            .bind_worker(
                "builtin/document-author",
                AdmittedWorkerSettings {
                    execution: if model {
                        WorkerExecutionV1::Model {
                            provider: "personal".into(),
                            provider_kind: "fixture".into(),
                            principal_id: policy.clone(),
                            model: "typed-model".into(),
                            effort: "high".into(),
                        }
                    } else {
                        WorkerExecutionV1::Command {}
                    },
                    invocation_policy_id: policy,
                },
            )
            .unwrap();
        let (plan, graph) = compiler
            .compile(&cas, &revision_id, "builtin/document")
            .unwrap();
        let plan_id = cas
            .put_artifact(
                EXECUTION_PLAN_V1,
                producer(),
                vec![revision_id.clone()],
                None,
                serde_json::to_value(&plan).unwrap(),
            )
            .unwrap()
            .0;
        Self {
            _directory: directory,
            cas,
            store,
            compiler,
            task,
            revision_id,
            plan,
            plan_id,
            graph,
        }
    }
}

/// Only this test document kind is installed; production kinds must provide their own
/// acceptance and receipt validation rather than inheriting an always-successful handler.
struct DocumentDomain;
impl TaskOperatorHost for DocumentDomain {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        Err("No built-in Worker".into())
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("Only the command Worker should execute")
    }
}
impl TaskDomain for DocumentDomain {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        for port in output.outputs.values() {
            if port.artifact_type != "af/CheckedDocument@1" {
                return Err("Unknown document output".into());
            }
            for id in &port.artifact_ids {
                if cas.get_json(id).map_err(|e| e.to_string())?["payload"]["text"]
                    .as_str()
                    .is_none()
                {
                    return Err("Document text is absent".into());
                }
            }
        }
        Ok(())
    }
    fn validate_result(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        if result.acceptance == TaskAcceptanceV1::Satisfied
            && result.evidence.iter().any(|id| {
                cas.get_json(id)
                    .map_or(true, |v| v["payload"]["outcome"] != "passed")
            })
        {
            return Err("Document has no positive verifier receipt".into());
        }
        Ok(())
    }
}

const SUCCESS: &str = r#"import json, sys
request = json.load(sys.stdin)
assert request['schema'] == 'af.worker-request/1'
assert set(request['inputs']) == {'input'}
assert 'goal' not in request and 'task' not in request
assert request['feedback'] == []
text = request['inputs']['input'][0]['payload']['text']
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'output':[{'outcome':'passed','text':text}]}}))
"#;

#[test]
fn domain_observes_started_attempt_and_persists_through_the_runtime_store() {
    use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
    use review_core::task::execution::TaskExecutionRecordV1;
    use review_store::SharedEventStore;
    use review_store::store::task::{TaskLease, task_run_id};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Observer<'a> {
        store: SharedEventStore<'a>,
        lease: TaskLease,
        inner: &'a dyn TaskOperatorHost,
        calls: AtomicUsize,
    }
    impl<'a> Observer<'a> {
        fn lock(&self) -> std::sync::MutexGuard<'_, &'a mut EventStore> {
            // A heartbeat may briefly own the connection. Bound the wait so a runtime that
            // accidentally calls the host while holding its lock fails instead of hanging.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match self.store.try_lock() {
                    Ok(store) => return store,
                    Err(std::sync::TryLockError::WouldBlock)
                        if std::time::Instant::now() < until =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("host cannot access the shared Store: {error}"),
                }
            }
        }
    }
    impl TaskOperatorHost for Observer<'_> {
        fn prepare_context(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            feedback: &[String],
        ) -> Result<String, String> {
            {
                let store = self.lock();
                let state = store
                    .task_projection(cas, self.lease.task_id())
                    .unwrap()
                    .unwrap();
                assert_eq!(state.plan_id.as_deref(), Some(input.plan_id.as_str()));
            }
            self.inner.prepare_context(cas, input, feedback)
        }

        fn execute(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            attempt: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            let attempt = attempt.expect("the fixture invokes one Worker");
            {
                let mut store = self.lock();
                let events = store
                    .replay(&task_run_id(self.lease.task_id()).unwrap())
                    .unwrap();
                let started = events.iter().any(|event| {
                    let transition: TaskTransitionV1 = serde_json::from_value(event.payload.clone()).unwrap();
                    let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                        return false;
                    };
                    let record: TaskExecutionRecordV1 = serde_json::from_value(cas.get_json(&record_id).unwrap()["payload"].clone()).unwrap();
                    matches!(record, TaskExecutionRecordV1::Started { attempt_id } if attempt_id == attempt.id())
                });
                assert!(
                    started,
                    "the domain must observe the durable Started barrier"
                );
                // Exercise a domain-side durable mutation on the same connection while the
                // runtime owns a live Attempt. The subsequent settlement must see this lease.
                store.renew_task_lease(cas, &self.lease, 60_000).unwrap();
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(cas, input, Some(attempt))
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
        .open_task(&f.cas, &f.revision_id, "shared-store", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let shared = SharedEventStore::new(&mut f.store);
    let observer = Observer {
        store: shared.clone(),
        lease: lease.clone(),
        inner: &host,
        calls: AtomicUsize::new(0),
    };
    let runtime =
        TaskRuntime::with_store(shared.clone(), &f.cas, lease.clone(), &authority, &observer)
            .unwrap();
    assert!(runtime.execute().unwrap().complete());
    assert!(runtime.execute().unwrap().complete());
    assert_eq!(observer.calls.load(Ordering::SeqCst), 1);
    let projection = shared
        .lock()
        .unwrap()
        .task_projection(&f.cas, lease.task_id())
        .unwrap()
        .unwrap();
    let execution = projection.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.pending_attempts().is_empty());
    assert!(execution.outputs.contains_key("root.nodes.write"));
}

#[test]
fn provider_admission_is_charged_once_and_failed_admission_dispatches_no_business_worker() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model {
        calls: AtomicUsize,
        pass: bool,
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
                (
                    if self.pass {
                        b"OK".to_vec()
                    } else {
                        b"capability unavailable".to_vec()
                    },
                    7,
                )
            } else {
                assert!(self.pass, "failed admission reached a business Worker");
                let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
                assert_eq!(request["inputs"].as_object().unwrap().len(), 1);
                assert!(request["inputs"]["input"].is_array());
                (serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}})).unwrap(),11)
            };
            ModelWorkerReturn {
                raw_artifact_ids: vec![cas.put(&bytes).unwrap()],
                message: Ok(bytes),
                usage: Some(review_runner::TokenUsage::charge_only(cost)),
            }
        }
    }
    for pass in [true, false] {
        let model = Model {
            calls: AtomicUsize::new(0),
            pass,
        };
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
        assert_eq!(report.complete(), pass, "{report:?}");
        assert_eq!(runtime.execute().unwrap().complete(), pass);
        assert_eq!(model.calls.load(Ordering::SeqCst), if pass { 2 } else { 1 });
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(
            execution.budget.committed_tokens(),
            if pass { 18 } else { 7 }
        );
        assert_eq!(execution.budget.begun_attempts(), if pass { 2 } else { 1 });
        assert_eq!(execution.outputs.contains_key("root.nodes.write"), pass);
        assert_eq!(
            execution.outputs.contains_key("root.providers.admit0"),
            pass
        );
        assert!(execution.pending_attempts().is_empty());
    }
}

#[test]
fn model_schema_failure_keeps_usage_and_retry_runs_through_the_same_task_budget() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model(AtomicUsize);
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
            bytes: Vec<u8>,
            _: std::time::Duration,
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                request["inputs"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>(),
                ["input"]
            );
            assert_eq!(
                request["reply_format"],
                review_runner::task::WORKER_REPLY_FORMAT
            );
            assert!(request.get("task").is_none());
            let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
            assert_eq!(
                request["feedback"].as_array().unwrap().len(),
                usize::from(!first)
            );
            if !first {
                assert_eq!(
                    request["feedback"][0]["payload"]["code"],
                    "invalid_output_contract"
                );
                assert!(!String::from_utf8_lossy(&bytes).contains("malformed response"));
            }
            let message = if first {
                b"malformed response".to_vec()
            } else {
                serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"A checked migration guide"}]}})).unwrap()
            };
            ModelWorkerReturn {
                raw_artifact_ids: vec![cas.put(&message).unwrap()],
                message: Ok(message),
                usage: Some(review_runner::TokenUsage::charge_only(if first {
                    20
                } else {
                    30
                })),
            }
        }
    }
    let model = Model(AtomicUsize::new(0));
    let mut f = Fixture::with_model("unused model package file", true);
    let slot = f.plan.bindings.keys().next().unwrap().clone();
    let mut models = BTreeMap::from([(
        slot.clone(),
        TaskModelBinding {
            binding: f.plan.bindings[&slot].clone(),
            adapter: &model as &dyn WorkerModelAdapter,
        },
    )]);
    models.get_mut(&slot).unwrap().binding.invocation_policy_id = f.revision_id.clone();
    assert!(
        CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &DocumentDomain,
            &models
        )
        .is_err()
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    models.get_mut(&slot).unwrap().binding = f.plan.bindings[&slot].clone();
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
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "model-test", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    assert!(runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 2);
    assert_eq!(execution.budget.committed_tokens(), 50);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    assert!(execution.pending_attempts().is_empty());
    drop(runtime);
    let wall = f
        .store
        .attempt_wall(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
        .unwrap();
    assert_eq!(wall.len(), 2);
    assert_eq!(
        wall.iter()
            .map(|a| a.usage.as_ref().unwrap().chargeable_tokens)
            .sum::<u64>(),
        50
    );
}

#[test]
fn captured_command_worker_executes_and_replays_through_the_common_task_runtime() {
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
        .open_task(&f.cas, &f.revision_id, "test-writer", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    assert!(runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.committed_tokens(), 0);
    let output = execution.outputs["root.nodes.write"].1.outputs["output"].clone();
    assert_eq!(
        f.cas.get_json(&output.artifact_ids[0]).unwrap()["payload"]["text"],
        "A checked migration guide"
    );
    let result = TaskResultV1 {
        task_revision_id: f.revision_id.clone(),
        execution: TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: "checked document produced".into(),
        evidence: output.artifact_ids.iter().cloned().collect(),
        outputs: BTreeMap::from([("document".into(), output)]),
        missing_obligations: BTreeSet::new(),
    };
    let result_id = f
        .cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&result_id).unwrap();
    drop(runtime);
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &authority)
            .is_err()
    );
}

#[test]
fn failed_command_and_schema_refusal_exhaust_bounded_attempts_without_publishing_a_result() {
    for script in [
        "import sys\nprint('paid process failed')\nsys.exit(17)\n",
        "print('{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"output\":[{\"outcome\":\"passed\"}]}}')\n",
    ] {
        let mut f = Fixture::new(script);
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
            .open_task(&f.cas, &f.revision_id, "test-writer", 60_000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        assert!(!runtime.execute().unwrap().complete());
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 2);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert!(!execution.outputs.contains_key("root.nodes.write"));
        assert!(execution.pending_attempts().is_empty());
    }
}
