use review_config::task::catalog::*;
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::verification::*;
use review_core::task::*;
use review_core::{PortCardinality, Producer};
use review_graph::task::{OperatorAttemptCost, OperatorSignature};
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::code::*;
use review_pipeline::task::host::*;
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_source_git::task::*;
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-implementation-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}
fn same(input: &str) -> PortAffinityV1 {
    PortAffinityV1::SameAs {
        input: input.into(),
    }
}
fn port(kind: &str, affinity: PortAffinityV1) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: kind.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity,
        root_default: None,
        covers: BTreeSet::new(),
    }
}
fn root(port: &str) -> ValueRefV1 {
    ValueRefV1::Input { port: port.into() }
}
fn from(node: &str, port: &str) -> ValueRefV1 {
    ValueRefV1::Node {
        node: node.into(),
        port: port.into(),
    }
}

fn pipeline(implement: &OperatorSignature, evaluate: &OperatorSignature) -> PipelineDefinitionV1 {
    let slot = |name: &str,
                role: &str,
                signature: &OperatorSignature,
                independent: BTreeSet<String>| WorkerSlotV1 {
        worker: format!("fixture/{name}"),
        role: role.into(),
        input_type: signature.worker_input_type.clone().unwrap(),
        output_type: signature.worker_output_type.clone().unwrap(),
        min_attempts: 1,
        max_attempts: 1,
        allow_local_replacement: true,
        independent_from: independent,
    };
    let node = |id: &str, operator, inputs| TaskNodeV1 {
        id: id.into(),
        operator,
        inputs,
        when: None,
    };
    let mut evaluation = node(
        "evaluate",
        TaskOperatorV1::Verify {
            slot: "evaluator".into(),
        },
        BTreeMap::from([
            ("source".into(), from("seal", "snapshot")),
            ("checks".into(), from("check", "result")),
            ("requirements".into(), root("requirements")),
        ]),
    );
    evaluation.when = Some(NodeConditionV1 {
        node: "check".into(),
        outcome: ReceiptOutcomeV1::Passed,
    });
    let mut verification = port(
        VERIFICATION_RESULT_V1,
        PortAffinityV1::DerivedFrom {
            input: "source".into(),
        },
    );
    verification.covers.insert("verified".into());
    PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
        name: "fixture/implementation".into(),
        version: "1.0.0".into(),
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([
                (
                    "source".into(),
                    port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                ),
                (
                    "requirements".into(),
                    port("af/Requirements@1", PortAffinityV1::Unbound {}),
                ),
            ]),
            outputs: BTreeMap::from([
                (
                    "snapshot".into(),
                    port(
                        SOURCE_TREE_V1,
                        PortAffinityV1::DerivedFrom {
                            input: "source".into(),
                        },
                    ),
                ),
                ("verification".into(), verification),
            ]),
        },
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from(["implement".into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::from([
            (
                "implementer".into(),
                slot("implementer", "implement", implement, BTreeSet::new()),
            ),
            (
                "evaluator".into(),
                slot(
                    "evaluator",
                    "evaluate",
                    evaluate,
                    BTreeSet::from(["implementer".into()]),
                ),
            ),
        ]),
        nodes: vec![
            node(
                "implement",
                TaskOperatorV1::Worker {
                    slot: "implementer".into(),
                },
                BTreeMap::from([
                    ("source".into(), root("source")),
                    ("requirements".into(), root("requirements")),
                ]),
            ),
            node(
                "seal",
                TaskOperatorV1::Seal {},
                BTreeMap::from([("candidate".into(), from("implement", "candidate"))]),
            ),
            node(
                "check",
                TaskOperatorV1::Check {
                    checks: BTreeSet::from(["pagination".into()]),
                },
                BTreeMap::from([("source".into(), from("seal", "snapshot"))]),
            ),
            evaluation,
            node(
                "accept",
                TaskOperatorV1::Accept {},
                BTreeMap::from([
                    ("source".into(), from("seal", "snapshot")),
                    ("checks".into(), from("check", "result")),
                    ("evaluation".into(), from("evaluate", "result")),
                ]),
            ),
        ],
        outputs: BTreeMap::from([
            ("snapshot".into(), from("accept", "snapshot")),
            ("verification".into(), from("accept", "result")),
        ]),
        coverage: BTreeMap::from([("verified".into(), from("accept", "result"))]),
        max_attempts: 3,
        max_parallel: 2,
    }
}

fn worker_inputs(ports: &[&str]) -> Value {
    let properties: serde_json::Map<String,Value> = ports.iter().map(|port| (port.to_string(),json!({"type":"array","minItems":1,"maxItems":1,
        "items":{"type":"object","required":["artifact_id","artifact_type","payload"],"properties":{
            "artifact_id":{"type":"string"},"artifact_type":{"type":"string"},"snapshot_id":{"type":"string"},"payload":{"type":"object"}},"additionalProperties":false}}))).collect();
    json!({"type":"object","required":ports,"properties":properties,"additionalProperties":false})
}

#[test]
fn implementation_seals_s1_and_negative_or_unavailable_checks_skip_the_evaluator() {
    for case in [
        "passed",
        "failed",
        "inconclusive",
        "process_timeout",
        "named_failure",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let command: review_core::Command = match case {
            "passed" | "named_failure" => serde_json::from_value(json!({"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"-c","provenance":"literal"},{"value":"import pagination; assert pagination.paginate(list(range(7)),2,3) == [2,3,4]","provenance":"literal"}]})).unwrap(),
            "failed" => review_core::Command::new("/usr/bin/false",vec![]),
            "process_timeout" => review_core::Command::new("/bin/sh",vec![review_core::Arg::literal("-c"),review_core::Arg::literal("sleep 1; exit 0")]),
            _ => review_core::Command::new("/no-such-af-check",vec![]),
        };
        let policy = CodeTaskPolicy {
            check_process_wall_ms: (case == "process_timeout").then_some(100),
            schema: "af.code-task-policy/1".into(),
            checks: BTreeMap::from([(
                "pagination".into(),
                review_check::CheckDefinition::new("pagination", command),
            )]),
            check_wall_ms: 5000,
            require_container: false,
        };
        let policy_id = cas
            .put_json(&serde_json::to_value(&policy).unwrap())
            .unwrap();
        let source = b"def paginate(items, offset=0, limit=2):\n    return items\n";
        let file = cas.put(source).unwrap();
        let original = Manifest::new(vec![Entry {
            path: "pagination.py".into(),
            kind: EntryKind::File,
            content: file,
            size: source.len() as u64,
        }])
        .unwrap();
        let origin = cas
            .put_json(&json!({"source":"fixture repository","revision":"original"}))
            .unwrap();
        let s0 = capture_snapshot(&cas, &original, &origin, None).unwrap();
        let source_input = source_tree(&cas, producer(), &s0, vec![]).unwrap();
        let requirements = cas
            .put_artifact(
                "af/Requirements@1",
                producer(),
                vec![],
                None,
                json!({"text":"Implement offset/limit pagination"}),
            )
            .unwrap()
            .0;
        let mut task = TaskRevisionV1 {
            previous_revision_id: None,
            task_id:format!("pagination-{case}"),revision:1,kind:"implement".into(),goal:"Implement this Jira ticket: offset/limit pagination".into(),
            inputs:BTreeMap::from([("source".into(),source_input), ("requirements".into(),ArtifactInputV1 {artifact_ids:vec![requirements.clone()],artifact_type:"af/Requirements@1".into(),cardinality:PortCardinality::One,snapshot_id:None})]),
            required_outputs:serde_json::from_value(json!({"snapshot":{"artifact_type":SOURCE_TREE_V1,"cardinality":"one"},"verification":{"artifact_type":VERIFICATION_RESULT_V1,"cardinality":"one"}})).unwrap(),
            acceptance:serde_json::from_value(json!({"verified":{"evidence_type":VERIFICATION_RESULT_V1,"verifier_policy":policy_id}})).unwrap(),
            provenance:TaskProvenanceV1 {adapter_id:origin,input_artifact_ids:vec![requirements]},
            authority:TaskAuthorityV1 {policy_id:policy_id.clone(),allowed_effects:BTreeSet::from(["read-source".into(),"write-source".into(),"execute-checks".into()]),data_destinations:BTreeSet::new()},
            limits:TaskLimitsV1 {tokens:1000,max_attempts:3,deadline_unix_ms:SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64+60_000,
                verification:VerificationReserveV1 {tokens:200,attempts:2,wall_ms:10000}},
            strategy:"small".into(),pipeline:serde_json::from_value(json!({"name":"fixture/implementation","fallback":"refuse"})).unwrap(),facts:BTreeMap::new(),
        };
        let implement = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "source".into(),
                        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                    ),
                    (
                        "requirements".into(),
                        port("af/Requirements@1", PortAffinityV1::Unbound {}),
                    ),
                ]),
                outputs: BTreeMap::from([
                    (
                        "report".into(),
                        port("af/ImplementationReport@1", same("source")),
                    ),
                    ("candidate".into(), port(CANDIDATE_TREE_V1, same("source"))),
                ]),
            },
            effects: BTreeSet::from(["write-source".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["implement".into()]),
            worker_input_type: Some("af/ImplementationInput@1".into()),
            worker_output_type: Some("af/ImplementationReport@1".into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
        };
        let mut evaluate = implement.clone();
        evaluate
            .contract
            .inputs
            .insert("checks".into(), port(TASK_CHECK_RECEIPT_V1, same("source")));
        evaluate.contract.outputs =
            BTreeMap::from([("result".into(), port(TASK_EVALUATION_V1, same("source")))]);
        evaluate.effects = BTreeSet::from(["read-source".into()]);
        evaluate.evidence =
            BTreeMap::from([("result".into(), BTreeSet::from([policy_id.clone()]))]);
        evaluate.roles = BTreeSet::from(["evaluate".into()]);
        evaluate.worker_input_type = Some("af/EvaluationInput@1".into());
        evaluate.worker_output_type = Some(TASK_EVALUATION_V1.into());
        evaluate.outcome_port = Some("result".into());
        evaluate.retains = BTreeMap::from([(
            "result".into(),
            BTreeSet::from(["requirements".into(), "checks".into()]),
        )]);
        let mut pipeline = pipeline(&implement, &evaluate);
        if case == "named_failure" {
            task.acceptance
                .insert("second".into(), task.acceptance["verified"].clone());
            task.required_outputs.insert(
                "second_verification".into(),
                task.required_outputs["verification"].clone(),
            );
            task.limits.max_attempts = 4;
            task.limits.verification.attempts = 3;
            task.limits.verification.wall_ms = 15000;
            pipeline.max_attempts = 4;
            let mut slot = pipeline.slots["evaluator"].clone();
            slot.worker = "fixture/negative".into();
            pipeline.slots.insert("negative".into(), slot);
            let mut verifier = pipeline
                .nodes
                .iter()
                .find(|n| n.id == "evaluate")
                .unwrap()
                .clone();
            verifier.id = "evaluate_negative".into();
            verifier.operator = TaskOperatorV1::Verify {
                slot: "negative".into(),
            };
            pipeline.nodes.push(verifier);
            let mut accept = pipeline
                .nodes
                .iter()
                .find(|n| n.id == "accept")
                .unwrap()
                .clone();
            accept.id = "accept_negative".into();
            accept
                .inputs
                .insert("evaluation".into(), from("evaluate_negative", "result"));
            pipeline.nodes.push(accept);
            let mut port = pipeline.contract.outputs["verification"].clone();
            port.covers = BTreeSet::from(["second".into()]);
            pipeline
                .contract
                .outputs
                .insert("second_verification".into(), port);
            pipeline.outputs.insert(
                "second_verification".into(),
                from("accept_negative", "result"),
            );
            pipeline
                .coverage
                .insert("second".into(), from("accept_negative", "result"));
        }
        let mut compiler = TaskPlanCompiler::new(
            policy_id.clone(),
            policy_id.clone(),
            code_signatures(&policy_id, &policy).unwrap(),
            task.acceptance
                .keys()
                .map(|name| (name.clone(), "snapshot".into()))
                .collect(),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        let mut packages = vec![(
            pipeline.name.clone(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&pipeline).unwrap().into_bytes(),
            )]),
        )];
        for (name, signature, script, output_port, output_schema) in [
            (
                "implementer",
                implement,
                r#"import json,sys
request=json.load(sys.stdin)
open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Implemented pagination'}]}}))
"#,
                "report",
                json!({"type":"object","additionalProperties":false,"required":["summary"],"properties":{"summary":{"type":"string"}}}),
            ),
            (
                "evaluator",
                evaluate,
                r#"import json,sys,runpy
request=json.load(sys.stdin)
assert request['inputs']['checks'][0]['payload']['outcome']=='passed'
scope=runpy.run_path('pagination.py')
assert scope['paginate'](list(range(7)),2,3)==[2,3,4]
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'passed','reason':'Verified offset and limit on the sealed source'}]}}))
"#,
                "result",
                json!({"type":"object","additionalProperties":false,"required":["outcome","reason"],"properties":{"outcome":{"enum":["passed","failed","inconclusive"]},"reason":{"type":"string","minLength":1}}}),
            ),
        ] {
            let inputs: Vec<_> = signature
                .contract
                .inputs
                .keys()
                .map(String::as_str)
                .collect();
            let input_schema = worker_inputs(&inputs);
            let manifest = TaskWorkerManifest {schema:"af.worker/1".into(),name:format!("fixture/{name}"),version:"1.0.0".into(),signature,
                runner:TaskWorkerRunner::Command {command:serde_json::from_value(json!({"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]})).unwrap()}};
            packages.push((
                manifest.name.clone(),
                BTreeMap::from([
                    (
                        "worker.toml".into(),
                        toml::to_string(&manifest).unwrap().into_bytes(),
                    ),
                    (
                        "input.schema.json".into(),
                        serde_json::to_vec(&input_schema).unwrap(),
                    ),
                    (
                        format!("outputs/{output_port}.schema.json"),
                        serde_json::to_vec(&output_schema).unwrap(),
                    ),
                    ("worker.py".into(), script.as_bytes().to_vec()),
                ]),
            ));
        }
        if case == "named_failure" {
            let mut files = packages
                .iter()
                .find(|(name, _)| name == "fixture/evaluator")
                .unwrap()
                .1
                .clone();
            let mut worker: TaskWorkerManifest =
                toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
            worker.name = "fixture/negative".into();
            files.insert(
                "worker.toml".into(),
                toml::to_string(&worker).unwrap().into_bytes(),
            );
            let script = String::from_utf8(files["worker.py"].clone())
                .unwrap()
                .replace("'outcome':'passed'", "'outcome':'failed'");
            files.insert("worker.py".into(), script.into_bytes());
            packages.push(("fixture/negative".into(), files));
        }
        if case == "passed"
            && let Some(destination) = std::env::var_os("AF_WRITE_TASK_FIXTURE")
        {
            export_fixture(
                std::path::Path::new(&destination),
                source,
                &task,
                &policy,
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
        let domain = CodeTaskDomain::captured(&cas, &policy_id, graph.clone()).unwrap();
        let environment = SnapshotTaskEnvironment {
            policy: policy.isolation(),
        };
        let host =
            CommandTaskHost::capture(&cas, &compiler, &task, &plan, graph, &environment, &domain)
                .unwrap();
        let authority = CapturedTaskAuthority {
            compiler: &compiler,
            domain: &host,
            developer: &NoTaskDeveloper,
        };
        let lease = store
            .open_task(&cas, &revision, "test-writer", 60_000)
            .unwrap();
        store
            .propose_task_plan(&cas, &lease, &plan_id, &authority)
            .unwrap();
        store.admit_task_plan(&cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut store, &cas, lease, &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        let state = runtime.projection().unwrap();
        let execution = state.execution.as_ref().unwrap();
        assert!(
            execution.outputs.contains_key("root.nodes.accept"),
            "{case}: {report:?}"
        );
        assert_eq!(
            execution.budget.begun_attempts(),
            if case == "named_failure" {
                4
            } else if case == "passed" {
                3
            } else {
                2
            },
            "{case}: {report:?}"
        );
        assert_eq!(
            execution.outputs.contains_key("root.nodes.evaluate"),
            matches!(case, "passed" | "named_failure")
        );
        let result = domain.result(&cas, &state, &report).unwrap();
        assert_eq!(
            result.acceptance,
            match case {
                "passed" => TaskAcceptanceV1::Satisfied,
                "failed" | "named_failure" => TaskAcceptanceV1::Unsatisfied,
                _ => TaskAcceptanceV1::Inconclusive,
            }
        );
        if case == "named_failure" {
            assert_eq!(
                result.missing_obligations,
                BTreeSet::from(["second".into()])
            );
            assert_eq!(
                result.evidence.len(),
                2,
                "The unrelated passing receipt cannot mask the named failed obligation"
            );
        }
        let s1 = result.outputs["snapshot"].snapshot_id.as_ref().unwrap();
        assert_ne!(s1, &s0);
        let (snapshot, manifest) = read_snapshot(&cas, s1).unwrap();
        assert_eq!(snapshot.parent_snapshot_id.as_ref(), Some(&s0));
        assert!(
            String::from_utf8(
                cas.get(&manifest.get("pagination.py").unwrap().content)
                    .unwrap()
            )
            .unwrap()
            .contains("items[offset:offset+limit]")
        );
        assert_eq!(read_snapshot(&cas, &s0).unwrap().1, original);
        if case == "passed" {
            // A plausible positive wrapper is insufficient: final acceptance must independently
            // recheck the actual check/evaluator chain and the plan that produced it.
            for field in ["evaluation_id", "plan_id"] {
                let evidence_id = result.evidence.iter().next().unwrap();
                let mut evidence: review_core::ArtifactEnvelope =
                    serde_json::from_value(cas.get_json(evidence_id).unwrap()).unwrap();
                evidence.payload[field] = json!(policy_id);
                let forged_id = cas
                    .put_artifact(
                        &evidence.artifact_type,
                        evidence.producer,
                        evidence.input_artifacts,
                        evidence.subject_snapshot_id,
                        evidence.payload,
                    )
                    .unwrap()
                    .0;
                let mut forged = result.clone();
                forged.evidence = BTreeSet::from([forged_id]);
                assert!(
                    domain.validate_result(&cas, &task, &forged).is_err(),
                    "forged {field} accepted"
                );
            }
        }
        if case == "passed" {
            // Reconstruct a valid envelope chain with unchanged positive payloads but replace
            // the exact requirements provenance. Domain validation must reject this proof.
            for stale in [false, true] {
                let original_id = result.evidence.iter().next().unwrap();
                let mut receipt: review_core::ArtifactEnvelope =
                    serde_json::from_value(cas.get_json(original_id).unwrap()).unwrap();
                let evaluation_id = receipt.payload["evaluation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let mut evaluation: review_core::ArtifactEnvelope =
                    serde_json::from_value(cas.get_json(&evaluation_id).unwrap()).unwrap();
                evaluation
                    .input_artifacts
                    .retain(|id| !task.inputs["requirements"].artifact_ids.contains(id));
                if stale {
                    let other = cas
                        .put_artifact(
                            "af/Requirements@1",
                            producer(),
                            vec![],
                            None,
                            json!({"text":"Another ticket"}),
                        )
                        .unwrap()
                        .0;
                    evaluation.input_artifacts.push(other);
                }
                let forged_evaluation = cas
                    .put_artifact(
                        &evaluation.artifact_type,
                        evaluation.producer,
                        evaluation.input_artifacts,
                        evaluation.subject_snapshot_id,
                        evaluation.payload,
                    )
                    .unwrap()
                    .0;
                receipt.payload["evaluation_id"] = json!(forged_evaluation);
                for id in &mut receipt.input_artifacts {
                    if id == &evaluation_id {
                        *id = forged_evaluation.clone();
                    }
                }
                let forged_id = cas
                    .put_artifact(
                        &receipt.artifact_type,
                        receipt.producer,
                        receipt.input_artifacts,
                        receipt.subject_snapshot_id,
                        receipt.payload,
                    )
                    .unwrap()
                    .0;
                let mut forged = result.clone();
                forged.evidence = BTreeSet::from([forged_id]);
                let error = domain.validate_result(&cas, &task, &forged).unwrap_err();
                assert!(error.contains("exact Task Requirements"), "{error}");
            }
        }
        let result_id = cas
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
    }
}

/// Explicit fixture-generation mode. Normal tests never write into the repository. The
/// exported CLI fixture uses the same typed definitions and digest routine as this test.
fn export_fixture(
    destination: &std::path::Path,
    source: &[u8],
    task: &TaskRevisionV1,
    policy: &CodeTaskPolicy,
    packages: &[(String, BTreeMap<String, Vec<u8>>)],
) {
    assert!(
        !destination.exists(),
        "fixture export requires an absent destination"
    );
    std::fs::create_dir_all(destination.join(".af")).unwrap();
    std::fs::write(destination.join("pagination.py"), source).unwrap();
    let mut pins = BTreeMap::new();
    for (name, files) in packages {
        let path = format!(".af/task-packages/{name}");
        pins.insert(
            name.clone(),
            TaskPackagePin {
                version: "1.0.0".into(),
                path: path.clone(),
                digest: review_config::lock::package_digest_from_files(files),
            },
        );
        for (file, bytes) in files {
            let path = destination.join(&path).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }
    std::fs::write(
        destination.join(".af/code-policy.toml"),
        toml::to_string(policy).unwrap(),
    )
    .unwrap();
    let catalog = json!({"schema":"af.task-catalog/1","code_policy":".af/code-policy.toml","packages":pins,"independence":IndependencePolicyV1::default()});
    std::fs::write(
        destination.join(".af/task-catalog.toml"),
        toml::to_string(&catalog).unwrap(),
    )
    .unwrap();
    let file = json!({"schema":"af.task-file/1","task_id":"pagination-cli","kind":"implement","goal":task.goal,
        "pipeline":task.pipeline,"strategy":task.strategy,"facts":task.facts,"limits":{"tokens":task.limits.tokens,
            "max_attempts":task.limits.max_attempts,"wall_ms":60000,"verification":task.limits.verification}});
    std::fs::write(
        destination.join("ticket.json"),
        serde_json::to_string_pretty(&file).unwrap() + "\n",
    )
    .unwrap();
}
