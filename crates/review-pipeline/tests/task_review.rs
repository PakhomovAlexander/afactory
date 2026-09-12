use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::review::*;
use review_core::task::verification::TASK_CHECK_RECEIPT_V1;
use review_core::task::*;
use review_core::{DemandRequirement, PortCardinality, Producer, Severity};
use review_graph::task::{OperatorAttemptCost, OperatorSignature};
use review_pipeline::task::code::{CodeTaskPolicy, code_signatures};
use review_pipeline::task::host::*;
use review_pipeline::task::review::*;
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_pipeline::task::*;
use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore};
use serde_json::json;

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "review-task-fixture".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}
fn port(ty: &str, affinity: PortAffinityV1) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity,
        root_default: None,
        covers: BTreeSet::new(),
    }
}
fn same() -> PortAffinityV1 {
    PortAffinityV1::SameAs {
        input: "source".into(),
    }
}
fn from(node: &str, port: &str) -> ValueRefV1 {
    ValueRefV1::Node {
        node: node.into(),
        port: port.into(),
    }
}
fn root(port: &str) -> ValueRefV1 {
    ValueRefV1::Input { port: port.into() }
}
fn node(id: &str, operator: TaskOperatorV1, inputs: BTreeMap<String, ValueRefV1>) -> TaskNodeV1 {
    TaskNodeV1 {
        id: id.into(),
        operator,
        inputs,
        when: None,
    }
}

#[test]
fn review_task_preserves_changes_requested_and_incomplete_without_partial_ledger() {
    for case in ["clean", "finding", "demand", "missing", "unavailable"] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let code = CodeTaskPolicy {
            check_process_wall_ms: None,
            schema: "af.code-task-policy/1".into(),
            checks: BTreeMap::from([(
                "syntax".into(),
                review_check::CheckDefinition::new(
                    "syntax",
                    review_core::Command::new(
                        if case == "unavailable" {
                            "/no-such-check"
                        } else {
                            "/usr/bin/true"
                        },
                        vec![],
                    ),
                ),
            )]),
            check_wall_ms: 5000,
            require_container: false,
        };
        let code_id = cas.put_json(&serde_json::to_value(&code).unwrap()).unwrap();
        let policy = ReviewTaskPolicy {
            allow_targeted_repairs: false,
            schema: "af.review-task-policy/1".into(),
            check_policy_id: code_id.clone(),
            reviewers: BTreeMap::from([
                ("correctness".into(), DemandRequirement::Required),
                ("bugs".into(), DemandRequirement::Required),
            ]),
            gate: Severity::Major,
            clean_rounds: 1,
            max_rounds: 1,
        };
        let policy_id = cas
            .put_json(&serde_json::to_value(&policy).unwrap())
            .unwrap();
        let file = cas.put(b"pub fn example() {}\n").unwrap();
        let manifest = Manifest::new(vec![Entry {
            path: "lib.rs".into(),
            kind: EntryKind::File,
            content: file,
            size: 20,
        }])
        .unwrap();
        let origin = cas.put(b"fixture origin").unwrap();
        let snapshot = capture_snapshot(&cas, &manifest, &origin, None).unwrap();
        let source = source_tree(&cas, producer(), &snapshot, vec![]).unwrap();
        let history_id = cas
            .put_artifact(
                REVIEW_HISTORY_V1,
                producer(),
                vec![],
                None,
                json!({"kind":"empty"}),
            )
            .unwrap()
            .0;
        let history = ArtifactInputV1 {
            artifact_ids: vec![history_id],
            artifact_type: REVIEW_HISTORY_V1.into(),
            cardinality: PortCardinality::One,
            snapshot_id: None,
        };
        let mut public_result = port(TASK_REVIEW_ROUND_V1, same());
        public_result.covers.insert("reviewed".into());
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "source".into(),
                        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("subject".into(), port(TASK_REVIEW_SUBJECT_V1, same())),
                    (
                        "history".into(),
                        port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same())),
                ]),
                outputs: BTreeMap::from([(
                    "result".into(),
                    port(review_core::contract::REVIEWER_RESULT_V1, same()),
                )]),
            },
            effects: BTreeSet::from(["read-source".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::from([(
                "result".into(),
                BTreeSet::from([
                    "source".into(),
                    "subject".into(),
                    "history".into(),
                    "checks".into(),
                ]),
            )]),
            roles: BTreeSet::from(["review".into()]),
            worker_input_type: Some("af/ReviewInput@1".into()),
            worker_output_type: Some(review_core::contract::REVIEWER_RESULT_V1.into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
        };
        let mut pipeline = PipelineDefinitionV1 {
            schema: PipelineSchemaV1::V1,
            name: "fixture/review".into(),
            version: "1.0.0".into(),
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "source".into(),
                        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                    ),
                    (
                        "history".into(),
                        port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}),
                    ),
                ]),
                outputs: BTreeMap::from([
                    ("review".into(), public_result),
                    (
                        "history".into(),
                        port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}),
                    ),
                ]),
            },
            accepts: PipelineApplicabilityV1 {
                kinds: BTreeSet::from(["review".into()]),
                required_facts: BTreeMap::new(),
            },
            slots: BTreeMap::new(),
            nodes: vec![
                node(
                    "bind",
                    TaskOperatorV1::ReviewBind {},
                    BTreeMap::from([
                        ("source".into(), root("source")),
                        ("history".into(), root("history")),
                    ]),
                ),
                node(
                    "check",
                    TaskOperatorV1::Check {
                        checks: BTreeSet::from(["syntax".into()]),
                    },
                    BTreeMap::from([("source".into(), root("source"))]),
                ),
            ],
            outputs: BTreeMap::from([
                ("review".into(), from("reduce", "result")),
                ("history".into(), from("reduce", "history")),
            ]),
            coverage: BTreeMap::from([("reviewed".into(), from("reduce", "result"))]),
            max_attempts: 3,
            max_parallel: 2,
        };
        let mut packages = Vec::new();
        for name in policy.reviewers.keys() {
            pipeline.slots.insert(
                name.clone(),
                WorkerSlotV1 {
                    worker: format!("fixture/{name}"),
                    role: "review".into(),
                    input_type: "af/ReviewInput@1".into(),
                    output_type: review_core::contract::REVIEWER_RESULT_V1.into(),
                    min_attempts: 1,
                    max_attempts: 1,
                    allow_local_replacement: false,
                    independent_from: BTreeSet::new(),
                },
            );
            let mut reviewer = node(
                name,
                TaskOperatorV1::Verify { slot: name.clone() },
                BTreeMap::from([
                    ("source".into(), root("source")),
                    ("subject".into(), from("bind", "subject")),
                    ("history".into(), root("history")),
                    ("checks".into(), from("check", "result")),
                ]),
            );
            reviewer.when = Some(NodeConditionV1 {
                node: "check".into(),
                outcome: ReceiptOutcomeV1::Passed,
            });
            pipeline.nodes.push(reviewer);
            let stage = json!({"verdict":if case=="finding" {"request-changes"} else {"approve"},"summary":"Fixture review",
                "reports":if case=="finding" && name=="correctness" {json!([{"severity":"major","file":"lib.rs","line":1,"title":"Missing behavior","body":"The implementation omits the required behavior","fix":"Implement the requested behavior","confidence":0.9}])} else {json!([])},
                "benchmark_demands":if case=="demand" && name=="correctness" {json!([{"claim":"Runtime is bounded","why":"Large inputs matter","suggested_method":"Measure the scaling"}])} else {json!([])},"disputes":[]});
            let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[stage]}});
            let script = if case == "missing" && name == "bugs" {
                "import sys; sys.exit(9)".into()
            } else {
                format!(
                    "import json,sys\nr=json.load(sys.stdin)\nassert r['inputs']['checks'][0]['payload']['outcome']=='passed'\nprint({})\n",
                    serde_json::to_string(&serde_json::to_string(&reply).unwrap()).unwrap()
                )
            };
            let worker=TaskWorkerManifest {schema:"af.worker/1".into(),name:format!("fixture/{name}"),version:"1.0.0".into(),signature:signature.clone(),
                runner:TaskWorkerRunner::Command {command:serde_json::from_value(json!({"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]})).unwrap()}};
            let input_schema = json!({"type":"object","required":["source","subject","history","checks"],"additionalProperties":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}});
            let output_schema = json!({"type":"object","required":["verdict","summary","reports","benchmark_demands","disputes"],"additionalProperties":false,
                "properties":{"verdict":{"enum":["approve","request-changes","block"]},"summary":{"type":["string","null"]},"reports":{"type":"array"},"benchmark_demands":{"type":"array"},"disputes":{"type":"array"}}});
            packages.push((
                worker.name.clone(),
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
                        "outputs/result.schema.json".into(),
                        serde_json::to_vec(&output_schema).unwrap(),
                    ),
                    ("worker.py".into(), script.into_bytes()),
                ]),
            ));
        }
        pipeline.nodes.push(node(
            "reduce",
            TaskOperatorV1::ReviewReduce {},
            BTreeMap::from([
                ("source".into(), root("source")),
                ("subject".into(), from("bind", "subject")),
                ("history".into(), root("history")),
                ("checks".into(), from("check", "result")),
                ("correctness".into(), from("correctness", "result")),
                ("bugs".into(), from("bugs", "result")),
            ]),
        ));
        pipeline
            .contract
            .inputs
            .get_mut("history")
            .unwrap()
            .root_default = Some(RootDefaultV1::EmptyReviewHistory);
        packages.push((
            pipeline.name.clone(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&pipeline).unwrap().into_bytes(),
            )]),
        ));
        let task:TaskRevisionV1=serde_json::from_value(json!({"task_id":format!("review-{case}"),"revision":1,"kind":"review","goal":"Review the captured source",
            "inputs":{"source":source,"history":history},"required_outputs":{"review":{"artifact_type":TASK_REVIEW_ROUND_V1,"cardinality":"one"},"history":{"artifact_type":REVIEW_HISTORY_V1,"cardinality":"one"}},
            "acceptance":{"reviewed":{"evidence_type":TASK_REVIEW_ROUND_V1,"verifier_policy":policy_id}},"provenance":{"adapter_id":origin,"input_artifact_ids":[]},
            "authority":{"policy_id":policy_id,"allowed_effects":["execute-checks","read-source"],"data_destinations":[]},
            "limits":{"tokens":1000,"max_attempts":3,"deadline_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64+60000,"verification":{"tokens":0,"attempts":3,"wall_ms":15000}},
            "strategy":"light","pipeline":{"name":pipeline.name,"fallback":"refuse"},"facts":{}})).unwrap();
        let mut signatures = code_signatures(&code_id, &code).unwrap();
        signatures.extend(review_signatures(&policy_id, &policy).unwrap());
        let mut compiler = TaskPlanCompiler::new(
            policy_id.clone(),
            policy_id.clone(),
            signatures,
            BTreeMap::from([("reviewed".into(), "review".into())]),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        if case == "finding"
            && let Some(destination) = std::env::var_os("AF_WRITE_TASK_REVIEW_FIXTURE")
        {
            export_fixture(
                std::path::Path::new(&destination),
                &code,
                &policy,
                &task,
                &packages,
            );
        }
        for (name, files) in packages {
            let pin = TaskPackagePin {
                version: "1.0.0".into(),
                path: "package".into(),
                digest: review_config::lock::package_digest_from_files(&files),
            };
            compiler
                .capture_package(
                    &cas,
                    &name,
                    &pin,
                    &files
                        .into_iter()
                        .map(|(p, b)| (format!("package/{p}"), b))
                        .collect(),
                )
                .unwrap();
            if name != pipeline.name {
                compiler
                    .bind_worker(
                        &name,
                        AdmittedWorkerSettings {
                            execution: WorkerExecutionV1::Command {},
                            invocation_policy_id: policy_id.clone(),
                        },
                    )
                    .unwrap();
            }
        }
        let revision = cas
            .put_artifact(
                TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&task).unwrap(),
            )
            .unwrap()
            .0;
        let (plan, graph) = compiler.compile(&cas, &revision, &pipeline.name).unwrap();
        let plan_id = cas
            .put_artifact(
                EXECUTION_PLAN_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&plan).unwrap(),
            )
            .unwrap()
            .0;
        let domain = ReviewTaskDomain::captured(&cas, &policy_id, graph.clone()).unwrap();
        let environment = SnapshotTaskEnvironment {
            policy: code.isolation(),
        };
        let host =
            CapturedTaskHost::capture(&cas, &compiler, &task, &plan, graph, &environment, &domain)
                .unwrap();
        let authority = CapturedTaskAuthority::new(&compiler, &host, &NoTaskDeveloper);
        let lease = store
            .open_task(&cas, &revision, "test-writer", 60000)
            .unwrap();
        store
            .propose_task_plan(&cas, &lease, &plan_id, &authority)
            .unwrap();
        store.admit_task_plan(&cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut store, &cas, lease, &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        let state = runtime.projection().unwrap();
        let result = domain.result(&cas, &state, &report).unwrap();
        assert!(result.outputs.contains_key("review"), "{case}: {report:?}");
        let review: TaskReviewRoundV1 = serde_json::from_value(
            cas.get_json(&result.outputs["review"].artifact_ids[0])
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap();
        let complete = !matches!(case, "missing" | "unavailable");
        assert_eq!(
            result.acceptance,
            if complete {
                TaskAcceptanceV1::Satisfied
            } else {
                TaskAcceptanceV1::Inconclusive
            },
            "{case}: {report:?}"
        );
        assert_eq!(
            review.conclusion,
            match case {
                "clean" => ReviewConclusionV1::Pass,
                "finding" | "demand" => ReviewConclusionV1::ChangesRequested,
                _ => ReviewConclusionV1::Incomplete,
            }
        );
        assert_eq!(review.finding_set_id.is_some(), complete);
        assert_eq!(review.demand_set_id.is_some(), complete);
        if case == "missing" {
            assert_eq!(review.selected_results.len(), 1);
            assert_eq!(review.missing_reviewers, BTreeSet::from(["bugs".into()]));
        }
        if case == "finding" {
            let set: review_core::FindingSetV1 = serde_json::from_value(
                cas.get_json(review.finding_set_id.as_ref().unwrap())
                    .unwrap()["payload"]
                    .clone(),
            )
            .unwrap();
            assert_eq!(set.findings.len(), 1);
            assert_eq!(set.findings[0].status, "open");
            assert_eq!(result.domain_conclusion, "changes_requested");
            assert_eq!(review.conclusion.exit_code(), 3);
        }
        let first_attempts = state.execution.as_ref().unwrap().budget.begun_attempts();
        if complete {
            let replay = runtime.execute().unwrap();
            let replay_state = runtime.projection().unwrap();
            assert_eq!(
                replay_state
                    .execution
                    .as_ref()
                    .unwrap()
                    .budget
                    .begun_attempts(),
                first_attempts
            );
            assert_eq!(domain.result(&cas, &replay_state, &replay).unwrap(), result);
        }
        domain.validate_result(&cas, &task, &result).unwrap();
        let result_id = cas
            .put_artifact(
                TASK_RESULT_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&result).unwrap(),
            )
            .unwrap()
            .0;
        runtime.finish(&result_id).unwrap();
    }
}

fn export_fixture(
    destination: &std::path::Path,
    code: &CodeTaskPolicy,
    review: &ReviewTaskPolicy,
    task: &TaskRevisionV1,
    packages: &[(String, BTreeMap<String, Vec<u8>>)],
) {
    assert!(
        !destination.exists(),
        "Fixture export requires an absent destination"
    );
    std::fs::create_dir_all(destination.join(".af")).unwrap();
    std::fs::write(destination.join("lib.rs"), "pub fn example() {}\n").unwrap();
    let mut pins = BTreeMap::new();
    for (name, files) in packages {
        let path = format!(".af/task-packages/{name}");
        pins.insert(
            name,
            TaskPackagePin {
                version: "1.0.0".into(),
                path: path.clone(),
                digest: review_config::lock::package_digest_from_files(files),
            },
        );
        for (file, bytes) in files {
            let target = destination.join(&path).join(file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
    }
    std::fs::write(
        destination.join(".af/code-policy.toml"),
        toml::to_string(code).unwrap(),
    )
    .unwrap();
    let catalog = json!({"schema":"af.task-catalog/1","code_policy":".af/code-policy.toml","packages":pins,"independence":IndependencePolicyV1::default(),
        "review":{"reviewers":review.reviewers,"gate":review.gate,"clean_rounds":review.clean_rounds,"max_rounds":review.max_rounds}});
    std::fs::write(
        destination.join(".af/task-catalog.toml"),
        toml::to_string(&catalog).unwrap(),
    )
    .unwrap();
    let file = json!({"schema":"af.task-file/1","task_id":"review-cli","kind":"review","goal":task.goal,"pipeline":task.pipeline,"strategy":task.strategy,"facts":task.facts,
        "limits":{"tokens":task.limits.tokens,"max_attempts":task.limits.max_attempts,"wall_ms":60000,"verification":task.limits.verification}});
    std::fs::write(
        destination.join("review.json"),
        serde_json::to_string_pretty(&file).unwrap() + "\n",
    )
    .unwrap();
}
