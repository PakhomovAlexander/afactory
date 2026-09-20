use super::*;
use review_core::task::pipeline::{PipelineContractV1, PipelinePortV1, PortAffinityV1};
use serde_json::json;

#[test]
fn instruction_derivation_changes_only_the_captured_instruction_bytes() {
    let mut f = Fixture::new();
    let name = "fixture/instruction-worker";
    let mut worker = f.compiler.workers["builtin/document-author"].clone();
    worker.name = name.into();
    let files = BTreeMap::from([
        (
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        ),
        (
            "instructions.md".into(),
            b"baseline instructions\n".to_vec(),
        ),
    ]);
    let pin = TaskPackagePin {
        version: "1.0.0".into(),
        digest: package_digest_from_files(&files),
        path: "packages/instruction-worker".into(),
    };
    let project = files
        .iter()
        .map(|(path, bytes)| (format!("{}/{path}", pin.path), bytes.clone()))
        .collect();
    f.compiler
        .capture_package(&f.cas, name, &pin, &project)
        .unwrap();
    let package = &f.compiler.packages[name];
    let original_id = package.dependency.artifact_id.clone();
    let original = TaskPlanCompiler::captured_worker_package(&f.cas, &original_id).unwrap();
    let instructions = "candidate instructions only\n";
    let instructions_id = review_store::canonical::blob_content_id(instructions.as_bytes());
    let mut expected_files = original.files.clone();
    expected_files.insert("instructions.md".into(), instructions.as_bytes().to_vec());
    let expected_digest = package_digest_from_files(&expected_files);
    let derived_id = TaskPlanCompiler::derive_worker_instructions_package(
        &f.cas,
        &original_id,
        &original.digest,
        &expected_digest,
        &instructions_id,
        instructions,
        capture_producer(),
        vec![f.revision_id.clone()],
    )
    .unwrap();
    let derived = TaskPlanCompiler::captured_worker_package(&f.cas, &derived_id).unwrap();
    assert_eq!(derived.worker, original.worker);
    assert_eq!(derived.files, expected_files);
    assert_eq!(derived.digest, expected_digest);
    assert_ne!(derived_id, original_id);

    assert!(
        TaskPlanCompiler::derive_worker_instructions_package(
            &f.cas,
            &original_id,
            &original.digest,
            &original.digest,
            &instructions_id,
            instructions,
            capture_producer(),
            vec![],
        )
        .is_err()
    );
    let old = String::from_utf8(original.files["instructions.md"].clone()).unwrap();
    let old_id = review_store::canonical::blob_content_id(old.as_bytes());
    assert!(
        TaskPlanCompiler::derive_worker_instructions_package(
            &f.cas,
            &original_id,
            &original.digest,
            &original.digest,
            &old_id,
            &old,
            capture_producer(),
            vec![],
        )
        .is_err()
    );
}

fn notes_port(optional: bool, affinity: PortAffinityV1) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: review_core::task::WORKER_NOTES_V1.into(),
        cardinality: review_core::PortCardinality::One,
        optional,
        affinity,
        root_default: None,
        covers: BTreeSet::new(),
    }
}

fn same_as_input() -> PortAffinityV1 {
    PortAffinityV1::SameAs {
        input: "input".into(),
    }
}

#[test]
fn worker_notes_ports_must_be_optional_single_and_unbound() {
    let f = Fixture::new();
    let package = |edit: &dyn Fn(&mut PipelineContractV1)| {
        let mut worker = f.compiler.workers["builtin/document-author"].clone();
        edit(&mut worker.signature.contract);
        let files = BTreeMap::from([(
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        )]);
        PackageBytes {
            schema: "af.task-package/1".into(),
            name: worker.name.clone(),
            version: worker.version.clone(),
            digest: package_digest_from_files(&files),
            files,
        }
    };
    let valid = package(&|contract| {
        let unbound = notes_port(true, PortAffinityV1::Unbound {});
        contract.inputs.insert("notes".into(), unbound);
        let same_as = notes_port(true, same_as_input());
        contract.outputs.insert("notes".into(), same_as);
    });
    assert!(TaskPlanCompiler::parse_package(&valid).is_ok());
    let required = package(&|contract| {
        let required = notes_port(false, PortAffinityV1::Unbound {});
        contract.inputs.insert("notes".into(), required);
    });
    let error = TaskPlanCompiler::parse_package(&required).unwrap_err();
    assert!(error.contains("notes input"), "{error}");
    let bound = package(&|contract| {
        let bound = notes_port(true, same_as_input());
        contract.inputs.insert("notes".into(), bound);
    });
    assert!(TaskPlanCompiler::parse_package(&bound).is_err());
    let required_output = package(&|contract| {
        let required = notes_port(false, PortAffinityV1::Unbound {});
        contract.outputs.insert("notes".into(), required);
    });
    let error = TaskPlanCompiler::parse_package(&required_output).unwrap_err();
    assert!(error.contains("notes output"), "{error}");
}

#[test]
fn a_worker_declares_at_most_one_notes_input_and_output() {
    let f = Fixture::new();
    let package = |edit: &dyn Fn(&mut PipelineContractV1)| {
        let mut worker = f.compiler.workers["builtin/document-author"].clone();
        edit(&mut worker.signature.contract);
        let files = BTreeMap::from([(
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        )]);
        PackageBytes {
            schema: "af.task-package/1".into(),
            name: worker.name.clone(),
            version: worker.version.clone(),
            digest: package_digest_from_files(&files),
            files,
        }
    };
    let two_inputs = package(&|contract| {
        let unbound = notes_port(true, PortAffinityV1::Unbound {});
        contract.inputs.insert("notes".into(), unbound.clone());
        contract.inputs.insert("more_notes".into(), unbound);
    });
    let error = TaskPlanCompiler::parse_package(&two_inputs).unwrap_err();
    assert!(error.contains("at most one Notes input"), "{error}");
    let two_outputs = package(&|contract| {
        let same_as = notes_port(true, same_as_input());
        contract.outputs.insert("notes".into(), same_as.clone());
        contract.outputs.insert("more_notes".into(), same_as);
    });
    let error = TaskPlanCompiler::parse_package(&two_outputs).unwrap_err();
    assert!(error.contains("at most one Notes input"), "{error}");
}

#[test]
fn same_slot_notes_are_wired_by_the_compiler_when_left_unbound() {
    use review_core::task::pipeline::{TaskNodeV1, TaskOperatorV1};
    let mut f = Fixture::new();
    let name = "builtin/document-author";
    let worker = f.compiler.workers.get_mut(name).unwrap();
    let unbound = notes_port(true, PortAffinityV1::Unbound {});
    worker
        .signature
        .contract
        .inputs
        .insert("notes".into(), unbound.clone());
    worker
        .signature
        .contract
        .outputs
        .insert("notes".into(), unbound.clone());
    let key = format!("worker/{name}");
    let signature = f.compiler.signatures.get_mut(&key).unwrap();
    signature
        .contract
        .inputs
        .insert("notes".into(), unbound.clone());
    signature.contract.outputs.insert("notes".into(), unbound);
    let pipeline = f.compiler.pipelines.get_mut("builtin/document").unwrap();
    let author_node = pipeline.nodes[0].id.clone();
    let inputs = pipeline.nodes[0].inputs.clone();
    pipeline.nodes.push(TaskNodeV1 {
        id: "second".into(),
        operator: TaskOperatorV1::Worker {
            slot: "author".into(),
        },
        inputs,
        when: None,
    });
    // Two evidence Workers on one slot need a reserve for both; the Task revision records it.
    f.task.limits.verification.attempts = 2;
    f.task.limits.verification.wall_ms = 2000;
    f.task.limits.max_attempts = 5;
    let revision_id = f
        .cas
        .put_artifact(
            review_core::task::TASK_REVISION_V1,
            capture_producer(),
            vec![],
            None,
            serde_json::to_value(&f.task).unwrap(),
        )
        .unwrap()
        .0;
    let (_, compiled) = f
        .compiler
        .compile(&f.cas, &revision_id, "builtin/document")
        .unwrap();
    let second = &compiled.nodes["root.nodes.second"];
    let source = second
        .inputs
        .get("notes")
        .expect("the unbound Notes input was wired from the same slot");
    assert_eq!(source.node, format!("root.nodes.{author_node}"));
    assert_eq!(source.port, "notes");
    let first = &compiled.nodes[&format!("root.nodes.{author_node}")];
    assert!(
        !first.inputs.contains_key("notes"),
        "the first node on the slot has no earlier Notes source"
    );
}

#[test]
fn worker_notes_never_cross_slots() {
    use review_core::task::pipeline::{TaskNodeV1, TaskOperatorV1, ValueRefV1};
    let mut f = Fixture::new();
    let name = "builtin/document-author";
    let worker = f.compiler.workers.get_mut(name).unwrap();
    let unbound = notes_port(true, PortAffinityV1::Unbound {});
    worker
        .signature
        .contract
        .inputs
        .insert("notes".into(), unbound.clone());
    worker
        .signature
        .contract
        .outputs
        .insert("notes".into(), unbound.clone());
    let key = format!("worker/{name}");
    let signature = f.compiler.signatures.get_mut(&key).unwrap();
    signature
        .contract
        .inputs
        .insert("notes".into(), unbound.clone());
    signature.contract.outputs.insert("notes".into(), unbound);
    let pipeline = f.compiler.pipelines.get_mut("builtin/document").unwrap();
    let author_node = pipeline.nodes[0].id.clone();
    let second = pipeline.slots["author"].clone();
    pipeline.slots.insert("second".into(), second);
    let mut inputs = pipeline.nodes[0].inputs.clone();
    let carried = ValueRefV1::Node {
        node: author_node,
        port: "notes".into(),
    };
    inputs.insert("notes".into(), carried);
    pipeline.nodes.push(TaskNodeV1 {
        id: "second".into(),
        operator: TaskOperatorV1::Worker {
            slot: "second".into(),
        },
        inputs,
        when: None,
    });
    let error = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap_err();
    assert!(error.contains("Notes never cross slots"), "{error}");
}

#[test]
fn installed_document_authorship_cannot_be_removed_by_omitting_worker_effects() {
    let mut f = Fixture::new();
    let pipeline = f.compiler.pipelines.get_mut("builtin/document").unwrap();
    let slot = pipeline.slots.keys().next().unwrap().clone();
    pipeline.nodes[0].operator = review_core::task::pipeline::TaskOperatorV1::Verify { slot };
    // The installed profile chooses which data artifacts establish authorship. A package's
    // empty effects declaration cannot turn that author into an independent verifier.
    let authored = f.compiler.workers["builtin/document-author"]
        .signature
        .contract
        .outputs
        .values()
        .map(|p| p.artifact_type.clone())
        .collect();
    f.compiler = f.compiler.with_authored_artifacts(authored).unwrap();
    let error = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap_err();
    assert!(error.contains("Independent slots"), "{error}");
}

#[test]
fn export_uses_shared_defaults_and_checked_renaming_without_local_authority() {
    let mut f = Fixture::new();
    f.replacement(|_, files| {
        files.insert("private-local.txt".into(), b"PRIVATE LOCAL WORKER".to_vec());
    });
    let exported = f
        .compiler
        .export_catalog("builtin/document", "team/document")
        .unwrap();
    assert_eq!(exported.catalog.packages.len(), 2);
    assert!(
        exported
            .catalog
            .packages
            .contains_key("builtin/document-author")
    );
    assert!(!exported.catalog.packages.contains_key("local/author"));
    assert!(
        !exported
            .files
            .values()
            .any(|b| String::from_utf8_lossy(b).contains("PRIVATE LOCAL"))
    );
    let mut compiler = TaskPlanCompiler::new(
        f.compiler.engine_id.clone(),
        f.compiler.policy_id.clone(),
        BTreeMap::new(),
        f.compiler.acceptance_outputs.clone(),
        f.compiler.independence,
    )
    .unwrap();
    for (name, pin) in &exported.catalog.packages {
        compiler
            .capture_package(&f.cas, name, pin, &exported.files)
            .unwrap();
    }
    compiler.validate_dependency_closure().unwrap();
    compiler
        .check_contract_fixtures(&exported.contracts)
        .unwrap();
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let schema = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(root.join("schemas").join(name)).unwrap()).unwrap()
    };
    let mut options = jsonschema::options();
    for name in [
        "task-contracts-v1.json",
        "task-operator-signature-v1.json",
        "task-kind-v1.json",
    ] {
        let value = schema(name);
        options.with_resource(
            value["$id"].as_str().unwrap().to_owned(),
            jsonschema::Resource::from_contents(value).unwrap(),
        );
    }
    for (name, value) in [
        (
            "catalog-contract-fixtures-v1.json",
            serde_json::to_value(&exported.contracts).unwrap(),
        ),
        (
            "shared-task-catalog-v1.json",
            serde_json::to_value(&exported.catalog).unwrap(),
        ),
    ] {
        let validator = options.build(&schema(name)).unwrap();
        assert!(
            validator.is_valid(&value),
            "{name}: {:?}",
            validator.iter_errors(&value).collect::<Vec<_>>()
        );
        let mut invalid = value;
        invalid["approval"] = json!(true);
        assert!(!validator.is_valid(&invalid));
    }
    let mut changed = exported.contracts.clone();
    changed
        .pipelines
        .get_mut("team/document")
        .unwrap()
        .coverage
        .clear();
    assert!(compiler.check_contract_fixtures(&changed).is_err());
    for name in [
        "generated/document",
        "local/document",
        "af-internal/document",
        "builtin/document-author",
    ] {
        assert!(
            f.compiler.export_catalog("builtin/document", name).is_err(),
            "{name}"
        );
    }
    f.compiler
        .pipelines
        .get_mut("builtin/document")
        .unwrap()
        .slots
        .get_mut("author")
        .unwrap()
        .worker = "local/author".into();
    assert!(
        f.compiler
            .export_catalog("builtin/document", "team/document")
            .unwrap_err()
            .contains("shared Worker")
    );
}

#[test]
fn proposal_feedback_uses_full_effective_binding_independence_without_installing_a_plan() {
    use review_core::task::pipeline::*;
    use review_core::task::planning::PipelineProposalV1;
    let mut f = Fixture::new();
    // This proposal declares two evidence Workers. Give this new test an explicit reserve
    // for both so the intended independence rejection is reached after resource admission.
    f.task.limits.verification.attempts = 2;
    f.task.limits.verification.wall_ms = 2000;
    let mut pipeline = f.compiler.pipelines["builtin/document"].clone();
    pipeline.name = "generated/document".into();
    let mut second = pipeline.slots["author"].clone();
    second.independent_from = BTreeSet::from(["author".into()]);
    second.min_attempts = 1;
    second.max_attempts = 1;
    pipeline.slots.insert("second".into(), second);
    pipeline.nodes.push(TaskNodeV1 {
        id: "second".into(),
        operator: TaskOperatorV1::Worker {
            slot: "second".into(),
        },
        inputs: pipeline.nodes[0].inputs.clone(),
        when: None,
    });
    let mut proposal = PipelineProposalV1 {
        schema: "af.pipeline-proposal/1".into(),
        root: pipeline.name.clone(),
        definitions: BTreeMap::from([(pipeline.name.clone(), toml::to_string(&pipeline).unwrap())]),
    };
    f.compiler
        .check_proposal_structure(&f.cas, &f.task, &proposal)
        .unwrap();
    let error = f
        .compiler
        .check_pipeline_proposal(&f.cas, &f.task, &proposal)
        .unwrap_err();
    assert!(error.contains("Independent slots"), "{error}");
    assert!(!f.compiler.pipelines.contains_key(&pipeline.name));
    pipeline.nodes.pop();
    pipeline.slots.remove("second");
    proposal
        .definitions
        .insert(pipeline.name.clone(), toml::to_string(&pipeline).unwrap());
    f.compiler
        .check_pipeline_proposal(&f.cas, &f.task, &proposal)
        .unwrap();
    assert!(
        !f.compiler.pipelines.contains_key(&pipeline.name),
        "A preview cannot install executable authority"
    );
}

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
    fn replacement(
        &mut self,
        edit: impl FnOnce(&mut TaskWorkerManifest, &mut BTreeMap<String, Vec<u8>>),
    ) {
        let mut worker = self.compiler.workers["builtin/document-author"].clone();
        let mut files = self.compiler.packages["builtin/document-author"]
            .bytes
            .files
            .clone();
        worker.name = "local/author".into();
        edit(&mut worker, &mut files);
        files.insert(
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        );
        let pin = TaskPackagePin {
            version: "1.0.0".into(),
            digest: package_digest_from_files(&files),
            path: "packages/local".into(),
        };
        let project = files
            .into_iter()
            .map(|(name, bytes)| (format!("packages/local/{name}"), bytes))
            .collect();
        self.compiler
            .capture_package(&self.cas, "local/author", &pin, &project)
            .unwrap();
        self.compiler
            .bind_worker(
                "local/author",
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: self.task.authority.policy_id.clone(),
                },
            )
            .unwrap();
        self.compiler
            .replace_slot_workers(BTreeMap::from([(
                "root.slots.author".into(),
                "local/author".into(),
            )]))
            .unwrap();
    }

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
fn local_worker_replacement_preserves_default_authority_and_exact_replay() {
    let mut f = Fixture::new();
    f.replacement(|worker, _| worker.signature.attempt.as_mut().unwrap().wall_ms = 500);
    let (plan, graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    assert_eq!(graph.slots["root.slots.author"].worker, "local/author");
    assert_eq!(
        graph.replaced_workers["root.slots.author"],
        BTreeSet::from(["builtin/document-author".into()])
    );
    assert_eq!(
        plan.dependencies
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "builtin/document",
            "builtin/document-author",
            "local/author"
        ]
    );
    f.compiler.validate_plan(&f.cas, &f.task, &plan).unwrap();
    f.compiler.replace_slot_workers(BTreeMap::new()).unwrap();
    assert!(f.compiler.validate_plan(&f.cas, &f.task, &plan).is_err());
}

#[test]
fn local_worker_cannot_weaken_schema_evidence_role_effects_or_slot_permission() {
    for case in [
        "schema",
        "evidence",
        "role",
        "effects",
        "type",
        "forbidden",
        "unknown_slot",
    ] {
        let mut f = Fixture::new();
        f.replacement(|worker, files| match case {
            "schema" => {
                files.insert("input.schema.json".into(), b"{}".to_vec());
            }
            "evidence" => worker.signature.evidence.clear(),
            "role" => {
                worker.signature.roles = BTreeSet::from(["another".into()]);
            }
            "effects" => {
                worker.signature.effects.insert("write-source".into());
            }
            "type" => {
                worker.signature.worker_input_type = Some("af/Other@1".into());
            }
            _ => (),
        });
        if case == "forbidden" {
            f.compiler
                .pipelines
                .get_mut("builtin/document")
                .unwrap()
                .slots
                .get_mut("author")
                .unwrap()
                .allow_local_replacement = false;
        }
        if case == "unknown_slot" {
            f.compiler
                .replace_slot_workers(BTreeMap::from([(
                    "root.slots.misspelled".into(),
                    "local/author".into(),
                )]))
                .unwrap();
        }
        assert!(
            f.compiler
                .required_worker_packages(&f.cas, &f.revision_id, "builtin/document")
                .is_err(),
            "{case} reached account admission"
        );
        assert!(
            f.compiler
                .compile(&f.cas, &f.revision_id, "builtin/document")
                .is_err(),
            "{case} compiled"
        );
    }
}

#[test]
fn root_constructors_are_explicit_and_never_fill_unrelated_required_inputs() {
    use review_core::task::pipeline::{PipelinePortV1, PortAffinityV1, RootDefaultV1};
    let mut f = Fixture::new();
    let pipeline = f.compiler.pipelines.get_mut("builtin/document").unwrap();
    pipeline.contract.inputs.insert(
        "history".into(),
        PipelinePortV1 {
            artifact_type: review_core::task::REVIEW_HISTORY_V1.into(),
            cardinality: review_core::PortCardinality::One,
            optional: false,
            affinity: PortAffinityV1::Unbound {},
            root_default: Some(RootDefaultV1::EmptyReviewHistory),
            covers: BTreeSet::new(),
        },
    );
    assert!(
        f.compiler
            .normalize_root_inputs(&f.cas, "builtin/document", BTreeMap::new())
            .is_err()
    );
    let inputs = f
        .compiler
        .normalize_root_inputs(&f.cas, "builtin/document", f.task.inputs.clone())
        .unwrap();
    assert_eq!(inputs["requirements"], f.task.inputs["requirements"]);
    let history = read_envelope(
        &f.cas,
        &inputs["history"].artifact_ids[0],
        review_core::task::REVIEW_HISTORY_V1,
    )
    .unwrap();
    assert_eq!(history.payload, json!({"kind":"empty"}));
    assert_eq!(
        f.compiler
            .normalize_root_inputs(&f.cas, "builtin/document", inputs.clone())
            .unwrap(),
        inputs
    );
    f.compiler
        .pipelines
        .get_mut("builtin/document")
        .unwrap()
        .contract
        .inputs
        .get_mut("history")
        .unwrap()
        .root_default = None;
    assert!(
        f.compiler
            .normalize_root_inputs(&f.cas, "builtin/document", f.task.inputs.clone())
            .is_err()
    );
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
fn captured_plan_validation_rechecks_bytes_and_never_reuses_changed_authority() {
    for generated in [false, true] {
        let mut f = Fixture::new();
        if generated {
            f.compiler.generated.insert(
                "builtin/document".into(),
                GeneratedOriginV1 {
                    pipeline_id: f.compiler.packages["builtin/document"]
                        .dependency
                        .artifact_id
                        .clone(),
                    proposal_id: f.revision_id.clone(),
                    bootstrap_plan_id: f.compiler.policy_id.clone(),
                },
            );
        }
        let (plan, _) = f
            .compiler
            .compile(&f.cas, &f.revision_id, "builtin/document")
            .unwrap();
        let validator = CapturedTaskPlanValidator::new(&f.compiler);
        assert_eq!(
            validator.validate_plan(&f.cas, &f.task, &plan).unwrap(),
            plan.generated_origins
        );
        assert_eq!(
            validator.validate_plan(&f.cas, &f.task, &plan).unwrap(),
            plan.generated_origins
        );
        let mut changed_task = f.task.clone();
        changed_task.goal.push_str(" changed");
        assert!(
            validator
                .validate_plan(&f.cas, &changed_task, &plan)
                .is_err()
        );
        let mut changed = plan.clone();
        changed.limits.tokens += 1;
        assert!(validator.validate_plan(&f.cas, &f.task, &changed).is_err());
        let mut changed = plan.clone();
        changed
            .bindings
            .values_mut()
            .next()
            .unwrap()
            .invocation_policy_id = f.revision_id.clone();
        assert!(validator.validate_plan(&f.cas, &f.task, &changed).is_err());
        if generated {
            let mut stripped = plan.clone();
            stripped.generated_origins.clear();
            assert!(validator.validate_plan(&f.cas, &f.task, &stripped).is_err());
        }
        let mut ids = BTreeSet::from([
            plan.task_revision_id.clone(),
            plan.compiled_graph_id.clone(),
            plan.engine_id.clone(),
            plan.authority.policy_id.clone(),
        ]);
        ids.extend(plan.dependencies.values().map(|d| d.artifact_id.clone()));
        ids.extend(
            plan.bindings
                .values()
                .map(|b| b.invocation_policy_id.clone()),
        );
        for id in ids {
            let hex = id.strip_prefix("sha256:").unwrap();
            let file = f
                ._dir
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]);
            let original = std::fs::read(&file).unwrap();
            std::fs::write(&file, b"corrupted after validation").unwrap();
            assert!(
                validator.validate_plan(&f.cas, &f.task, &plan).is_err(),
                "{id}"
            );
            std::fs::remove_file(&file).unwrap();
            assert!(
                validator.validate_plan(&f.cas, &f.task, &plan).is_err(),
                "{id}"
            );
            assert!(
                !file.exists(),
                "memoized validation must not recreate missing authority"
            );
            std::fs::write(&file, original).unwrap();
            assert_eq!(
                validator.validate_plan(&f.cas, &f.task, &plan).unwrap(),
                plan.generated_origins
            );
        }
    }
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

#[test]
fn installed_planner_bootstrap_is_exact_fixed_and_cannot_be_reclassified_by_wire_text() {
    use review_core::task::planning::{PIPELINE_PROPOSAL_V1, PLANNING_REQUEST_V1};
    for native in [false, true] {
        let mut f = Fixture::new();
        let mut worker = f.compiler.workers["builtin/document-author"].clone();
        worker.name = "builtin/planner".into();
        let request = planning::planning_context_signature().contract.outputs["request"].clone();
        let mut proposal = request.clone();
        proposal.artifact_type = PIPELINE_PROPOSAL_V1.into();
        worker.signature.contract = PipelineContractV1 {
            inputs: BTreeMap::from([("request".into(), request)]),
            outputs: BTreeMap::from([("proposal".into(), proposal)]),
        };
        worker.signature.effects.clear();
        worker.signature.evidence.clear();
        worker.signature.roles = BTreeSet::from(["plan".into()]);
        worker.signature.retains =
            BTreeMap::from([("proposal".into(), BTreeSet::from(["request".into()]))]);
        worker.signature.worker_input_type = Some("af/PlannerInput@1".into());
        worker.signature.worker_output_type = Some(PIPELINE_PROPOSAL_V1.into());
        if native {
            worker.signature.attempt.as_mut().unwrap().tokens = 100;
            worker.runner = TaskWorkerRunner::Model {
                provider_kind: "claude".into(),
                model: "fixture-model".into(),
                effort: "high".into(),
            };
            f.compiler =
                f.compiler
                    .with_provider_admission(review_graph::task::OperatorAttemptCost {
                        tokens: 100,
                        wall_ms: 500,
                    });
        }
        let bytes = toml::to_string(&worker).unwrap().into_bytes();
        let files = BTreeMap::from([("worker.toml".into(), bytes.clone())]);
        let pin = TaskPackagePin {
            version: "1.0.0".into(),
            digest: package_digest_from_files(&files),
            path: "planner".into(),
        };
        f.compiler
            .capture_package(
                &f.cas,
                "builtin/planner",
                &pin,
                &BTreeMap::from([("planner/worker.toml".into(), bytes)]),
            )
            .unwrap();
        f.compiler
            .bind_worker(
                "builtin/planner",
                AdmittedWorkerSettings {
                    execution: if native {
                        WorkerExecutionV1::Model {
                            provider: "personal".into(),
                            provider_kind: "claude".into(),
                            principal_id: f.task.authority.policy_id.clone(),
                            model: "fixture-model".into(),
                            effort: "high".into(),
                        }
                    } else {
                        WorkerExecutionV1::Command {}
                    },
                    invocation_policy_id: f.task.authority.policy_id.clone(),
                },
            )
            .unwrap();
        let settings = planning::PlannerSettings {
            worker: "builtin/planner".into(),
            max_attempts: 2,
        };
        let name = f
            .compiler
            .install_planning_bootstrap(&f.cas, &f.task, &settings)
            .unwrap();
        assert_eq!(
            f.compiler
                .install_planning_bootstrap(&f.cas, &f.task, &settings)
                .unwrap(),
            name
        );
        assert_eq!(
            f.compiler
                .required_worker_packages(&f.cas, &f.revision_id, &name)
                .unwrap(),
            BTreeSet::from(["builtin/planner".into()])
        );
        let (plan, graph) = f.compiler.compile(&f.cas, &f.revision_id, &name).unwrap();
        assert_eq!(
            graph
                .nodes
                .values()
                .filter(|n| matches!(n.operator, CompiledOperator::ProviderAdmission { .. }))
                .count(),
            usize::from(native)
        );
        let now = f.task.limits.deadline_unix_ms - 5000;
        assert!(
            f.compiler
                .compiled_resources(&graph, &f.task.limits, now)
                .unwrap()
                .is_empty()
        );
        assert!(
            !f.compiler
                .compiled_resources(&graph, &f.task.limits, f.task.limits.deadline_unix_ms - 500)
                .unwrap()
                .is_empty()
        );
        let mut exhausted = f.task.limits.clone();
        exhausted.max_attempts = exhausted.verification.attempts;
        assert!(
            !f.compiler
                .compiled_resources(&graph, &exhausted, now)
                .unwrap()
                .is_empty()
        );
        assert!(plan.preparation.is_some());
        assert!(plan.acceptance.is_empty());
        assert!(plan.generated_origins.is_empty());
        assert_eq!(plan.inputs, f.task.inputs);
        assert_eq!(graph.inputs, f.task.inputs);
        assert_eq!(
            graph.nodes["root.nodes.context"].contract.outputs["request"].artifact_type,
            PLANNING_REQUEST_V1
        );
        f.compiler.validate_plan(&f.cas, &f.task, &plan).unwrap();
        let request = f.compiler.planning_request(&f.task).unwrap();
        let mut options = jsonschema::options();
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let schema = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(root.join("schemas").join(name)).unwrap())
                .unwrap()
        };
        for name in ["task-contracts-v1.json", "task-operator-signature-v1.json"] {
            let value = schema(name);
            options.with_resource(
                value["$id"].as_str().unwrap().to_owned(),
                jsonschema::Resource::from_contents(value).unwrap(),
            );
        }
        let validator = options.build(&schema("planning-request-v1.json")).unwrap();
        assert!(
            validator.is_valid(&request),
            "{:?}",
            validator
                .iter_errors(&request)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
        assert!(request["workers"]["builtin/planner"].is_null());
        assert!(request["pipelines"][planning::PLANNER_PIPELINE].is_null());
        for pointer in [
            "",
            "/task",
            "/workers/builtin~1document-author",
            "/pipelines/builtin~1document",
        ] {
            let mut invalid = request.clone();
            invalid.pointer_mut(pointer).unwrap()["private_worker_instructions"] =
                json!("undeclared context");
            assert!(!validator.is_valid(&invalid));
        }
        let mut stripped = plan.clone();
        stripped.preparation = None;
        assert!(
            f.compiler
                .validate_plan(&f.cas, &f.task, &stripped)
                .is_err()
        );
        let mut forged = f.compiler.clone();
        forged.preparation_roots.clear();
        assert!(
            forged.validate_plan(&f.cas, &f.task, &plan).is_err(),
            "Pipeline text supplied the preparation capability"
        );
        assert!(
            f.compiler
                .install_planning_bootstrap(
                    &f.cas,
                    &f.task,
                    &planning::PlannerSettings {
                        max_attempts: 3,
                        ..settings
                    }
                )
                .is_err()
        );
        assert!(
            f.compiler
                .install_planning_bootstrap(
                    &f.cas,
                    &f.task,
                    &planning::PlannerSettings {
                        worker: "builtin/document-author".into(),
                        max_attempts: 1
                    }
                )
                .is_err()
        );
    }
}

#[test]
fn new_legacy_capture_requires_explicit_safe_wire_budget_but_old_packages_restore() {
    for budget in [None, Some(0), Some(777), Some(9_007_199_254_740_992)] {
        let mut f = Fixture::new();
        let name = "builtin/legacy-author";
        let mut worker = f.compiler.workers["builtin/document-author"].clone();
        worker.name = name.into();
        worker.runner = TaskWorkerRunner::LegacyTaskCommand {
            command: CommandSpec {
                program: "/usr/bin/true".into(),
                args: vec![],
            },
            protocol: review_runner::task::legacy::LegacyTaskProtocol::ImplementV1,
            legacy_budget_tokens: budget,
        };
        let files = BTreeMap::from([(
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        )]);
        let digest = package_digest_from_files(&files);
        let pin = TaskPackagePin {
            version: "1.0.0".into(),
            digest: digest.clone(),
            path: "packages/legacy".into(),
        };
        let project = files
            .iter()
            .map(|(p, b)| (format!("packages/legacy/{p}"), b.clone()))
            .collect();
        let result = f.compiler.capture_package(&f.cas, name, &pin, &project);
        assert_eq!(
            result.is_ok(),
            matches!(budget, Some(0 | 777)),
            "{budget:?}: {result:?}"
        );
        if budget.is_none() {
            // Construct the old captured package using its original serialization. New
            // capture refuses it, but trusted persisted authority still restores exactly.
            let old = PackageBytes {
                schema: "af.task-package/1".into(),
                name: name.into(),
                version: "1.0.0".into(),
                digest: digest.clone(),
                files,
            };
            let id = f
                .cas
                .put_artifact(
                    TASK_PACKAGE_V1,
                    capture_producer(),
                    vec![],
                    None,
                    serde_json::to_value(&old).unwrap(),
                )
                .unwrap()
                .0;
            let bytes = f.cas.get(&id).unwrap();
            f.compiler
                .restore_package(&f.cas, name, &digest, &id)
                .unwrap();
            assert_eq!(f.cas.get(&id).unwrap(), bytes);
            assert_eq!(f.compiler.workers[name], worker);
            assert!(
                !toml::to_string(&worker)
                    .unwrap()
                    .contains("legacy_budget_tokens")
            );
        }
    }
    for invalid in [json!(null), json!(-1), json!(1.5), json!("777")] {
        let runner = json!({"kind":"legacy_task_command","command":{"program":"/usr/bin/true","args":[]},"protocol":"implement_v1","legacy_budget_tokens":invalid});
        assert!(serde_json::from_value::<TaskWorkerRunner>(runner).is_err());
    }
}
