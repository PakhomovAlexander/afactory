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
        Self::with_model_instructions(
            script,
            model,
            "Return the checked document using the declared output contract.",
        )
    }
    fn with_model_instructions(script: &str, model: bool, instructions: &str) -> Self {
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
        let mut configuration_port = pipeline.contract.inputs["requirements"].clone();
        configuration_port.artifact_type = "af/OptimizationConfiguration@1".into();
        configuration_port.optional = true;
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "input".into(),
                        pipeline.contract.inputs["requirements"].clone(),
                    ),
                    ("configuration".into(), configuration_port),
                ]),
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
                }}},
            "configuration":{"type":"array", "minItems":1, "maxItems":1, "items":{"type":"object", "additionalProperties":false,
                "required":["artifact_id","artifact_type","payload"], "properties":{
                    "artifact_id":{"type":"string"}, "artifact_type":{"const":"af/OptimizationConfiguration@1"},
                    "payload":{"type":"object"}
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
                    ("instructions.md".into(), instructions.as_bytes().to_vec()),
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
        _definition: &review_graph::task::CompiledNode,
        _attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        Err("No built-in Worker".into())
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        _: Option<&PreparedTaskAttempt>,
        _cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        panic!("Only the command Worker should execute")
    }
}
impl TaskDomain for DocumentDomain {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &review_store::store::task::execution::ReservedTaskAttempt,
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
        _definition: &review_graph::task::CompiledNode,
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
