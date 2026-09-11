use super::*;
use review_core::task::pipeline::PipelineContractV1;
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    cas: Cas,
    compiler: TaskPlanCompiler,
    task: TaskRevisionV1,
    revision_id: String,
    project: BTreeMap<String, Vec<u8>>,
    pins: BTreeMap<String, TaskPackagePin>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
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
        let policy = cas.put_json(&json!({"test_kind_policy": 1})).unwrap();
        let engine = cas.put_json(&json!({"test_engine": 1})).unwrap();
        task.authority.policy_id = policy.clone();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        task.provenance.adapter_id = policy.clone();
        let source = cas
            .put_artifact(
                "af/Requirements@1",
                capture_producer(),
                vec![],
                None,
                json!({"text":"Create the guide"}),
            )
            .unwrap()
            .0;
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source.clone()];
        task.provenance.input_artifact_ids = vec![source];
        let revision_id = cas
            .put_artifact(
                review_core::task::TASK_REVISION_V1,
                capture_producer(),
                vec![],
                None,
                serde_json::to_value(&task).unwrap(),
            )
            .unwrap()
            .0;
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let worker = TaskWorkerManifest {
            schema: "af.worker/1".into(),
            name: "builtin/document-author".into(),
            version: "1.0.0".into(),
            signature: OperatorSignature {
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([(
                        "input".into(),
                        pipeline.contract.inputs["requirements"].clone(),
                    )]),
                    outputs: BTreeMap::from([("output".into(), output)]),
                },
                effects: BTreeSet::from(["read-source".into()]),
                evidence: BTreeMap::from([("output".into(), BTreeSet::from([policy.clone()]))]),
                roles: BTreeSet::from(["author".into()]),
                retains: BTreeMap::new(),
                worker_input_type: Some("af/Requirements@1".into()),
                worker_output_type: Some("af/CheckedDocument@1".into()),
                outcome_port: None,
                attempt: Some(review_graph::task::OperatorAttemptCost {
                    tokens: 0,
                    wall_ms: 1000,
                }),
            },
            runner: TaskWorkerRunner::Command {
                command: CommandSpec {
                    program: "/usr/bin/true".into(),
                    args: vec![],
                },
            },
        };
        let mut project = BTreeMap::new();
        let mut pins: BTreeMap<String, TaskPackagePin> = BTreeMap::new();
        for (name, path, filename, text) in [
            (
                "builtin/document",
                "packages/document",
                "pipeline.toml",
                toml::to_string(&pipeline).unwrap(),
            ),
            (
                "builtin/document-author",
                "packages/author",
                "worker.toml",
                toml::to_string(&worker).unwrap(),
            ),
        ] {
            let files = BTreeMap::from([(filename.into(), text.into_bytes())]);
            pins.insert(
                name.into(),
                TaskPackagePin {
                    version: "1.0.0".into(),
                    digest: package_digest_from_files(&files),
                    path: path.into(),
                },
            );
            for (relative, bytes) in files {
                project.insert(format!("{path}/{relative}"), bytes);
            }
        }
        let mut compiler = TaskPlanCompiler::new(
            engine,
            policy.clone(),
            BTreeMap::new(),
            BTreeMap::from([("checked".into(), "document".into())]),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        for (name, pin) in &pins {
            compiler.capture_package(&cas, name, pin, &project).unwrap();
        }
        compiler
            .bind_worker(
                "builtin/document-author",
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: policy,
                },
            )
            .unwrap();
        Self {
            _dir: dir,
            cas,
            compiler,
            task,
            revision_id,
            project,
            pins,
        }
    }
}

#[test]
fn exact_captured_plan_recompiles_without_registry_reads_and_changed_authority_fails() {
    let mut f = Fixture::new();
    let (plan, graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    assert_eq!(plan.dependencies.len(), 2);
    assert_eq!(
        graph.coverage["checked"].qualified(),
        "root.nodes.write.output"
    );
    assert!(f.compiler.validate_plan(&f.cas, &f.task, &plan).is_ok());
    f.project.clear();
    let (repeated, _) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    assert_eq!(plan, repeated, "no mutable registry or working-tree read");
    for defect in [
        "bindings", "coverage", "closure", "engine", "limits", "graph",
    ] {
        let mut modified = plan.clone();
        match defect {
            "bindings" => modified.bindings.clear(),
            "coverage" => modified.acceptance.clear(),
            "closure" => modified.dependencies.clear(),
            "engine" => modified.engine_id = f.compiler.policy_id.clone(),
            "limits" => modified.limits.max_attempts += 1,
            "graph" => {
                let mut changed = graph.clone();
                changed.order.reverse();
                modified.compiled_graph_id = f
                    .cas
                    .put_artifact(
                        COMPILED_TASK_V1,
                        capture_producer(),
                        vec![f.revision_id.clone(), plan.engine_id.clone()],
                        None,
                        serde_json::to_value(changed).unwrap(),
                    )
                    .unwrap()
                    .0;
            }
            _ => unreachable!(),
        }
        assert!(
            f.compiler
                .validate_plan(&f.cas, &f.task, &modified)
                .is_err(),
            "{defect}"
        );
    }
}

#[test]
fn package_capture_checks_the_same_bytes_before_parsing_and_never_falls_through() {
    let f = Fixture::new();
    let pin = &f.pins["builtin/document"];
    let mut changed = f.project.clone();
    changed
        .get_mut("packages/document/pipeline.toml")
        .unwrap()
        .extend(b"\n# modified\n");
    assert!(
        f.compiler
            .clone()
            .capture_package(&f.cas, "builtin/document", pin, &changed)
            .unwrap_err()
            .contains("changed since")
    );
    for path in [
        "../packages/document",
        "/packages/document",
        "packages/../document",
        "packages\\document",
    ] {
        let mut unsafe_pin = pin.clone();
        unsafe_pin.path = path.into();
        assert!(
            f.compiler
                .clone()
                .capture_package(&f.cas, "builtin/document", &unsafe_pin, &f.project)
                .is_err()
        );
    }
    let mut malformed = pin.clone();
    malformed.version = "latest".into();
    assert!(
        f.compiler
            .clone()
            .capture_package(&f.cas, "builtin/document", &malformed, &f.project)
            .is_err()
    );
}

#[test]
fn restored_packages_keep_identity_and_missing_graph_is_not_recreated_during_validation() {
    let f = Fixture::new();
    let (plan, _) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    let mut restored = TaskPlanCompiler::new(
        f.compiler.engine_id.clone(),
        f.compiler.policy_id.clone(),
        BTreeMap::new(),
        f.compiler.acceptance_outputs.clone(),
        f.compiler.independence,
    )
    .unwrap();
    for (name, package) in &f.compiler.packages {
        restored
            .restore_package(
                &f.cas,
                name,
                &package.bytes.digest,
                &package.dependency.artifact_id,
            )
            .unwrap();
    }
    restored
        .bind_worker(
            "builtin/document-author",
            f.compiler.settings["builtin/document-author"].clone(),
        )
        .unwrap();
    assert_eq!(
        restored
            .compile(&f.cas, &f.revision_id, "builtin/document")
            .unwrap()
            .0,
        plan
    );
    let hex = plan.compiled_graph_id.strip_prefix("sha256:").unwrap();
    let path = f
        ._dir
        .path()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::remove_file(&path).unwrap();
    assert!(restored.validate_plan(&f.cas, &f.task, &plan).is_err());
    assert!(
        !path.exists(),
        "validation must not repair missing authority"
    );
}
