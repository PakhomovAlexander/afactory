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
    for case in [
        "clean",
        "finding",
        "demand",
        "missing_reviewer",
        "unavailable",
    ] {
        run_case(case);
    }
}

#[test]
fn review_scopes_dispositions_preserves_actual_producers_and_reads_large_patch() {
    for case in [
        "valid",
        "missing",
        "duplicate",
        "unassigned",
        "large",
        "mutated",
        "collision",
    ] {
        run_case(case);
    }
}

#[test]
fn review_passed_receipts_do_not_hide_an_independent_failed_node() {
    run_case("independent");
}

/// Single-Round cases reply with fixed results. Two-Round cases carry a prior Finding into
/// the second Round's assignment and exercise its dispositions and the readable patch file.
fn run_case(case: &str) {
    let two_rounds = matches!(
        case,
        "valid" | "missing" | "duplicate" | "unassigned" | "large" | "mutated" | "collision"
    );
    {
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
            schema: REVIEW_TASK_POLICY_SCHEMA.into(),
            check_policy_id: code_id.clone(),
            reviewers: BTreeMap::from([
                ("correctness".into(), DemandRequirement::Required),
                ("bugs".into(), DemandRequirement::Required),
            ]),
            gate: Severity::Major,
            clean_rounds: 1,
            max_rounds: if two_rounds { 2 } else { 1 },
        };
        let policy_id = cas
            .put_json(&serde_json::to_value(&policy).unwrap())
            .unwrap();
        let source_bytes = if matches!(case, "large" | "mutated" | "collision") {
            "// readable source change\n".repeat(45000).into_bytes()
        } else {
            b"pub fn example() {}\n".to_vec()
        };
        let file = cas.put(&source_bytes).unwrap();
        let mut manifest = Manifest::new(vec![Entry {
            path: "lib.rs".into(),
            kind: EntryKind::File,
            content: file,
            size: source_bytes.len() as u64,
        }])
        .unwrap();
        if case == "collision" {
            manifest.entries.push(Entry {
                path: ".af-review-inputs".into(),
                kind: EntryKind::File,
                content: cas.put(b"owned source").unwrap(),
                size: 12,
            });
            manifest = Manifest::new(manifest.entries).unwrap();
        }
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
        let result_type = review_core::contract::REVIEWER_RESULT_V2;
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "source".into(),
                        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("subject".into(), port(TASK_REVIEW_SUBJECT_V2, same())),
                    ("assignment".into(), port(TASK_REVIEW_ASSIGNMENT_V1, same())),
                    (
                        "history".into(),
                        port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same())),
                ]),
                outputs: BTreeMap::from([("result".into(), port(result_type, same()))]),
            },
            effects: BTreeSet::from(["read-source".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::from([(
                "result".into(),
                BTreeSet::from([
                    "source".into(),
                    "subject".into(),
                    "assignment".into(),
                    "history".into(),
                    "checks".into(),
                ]),
            )]),
            roles: BTreeSet::from(["review".into()]),
            worker_input_type: Some("af/ReviewInput@1".into()),
            worker_output_type: Some(result_type.into()),
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
        // A command Worker keeps only stdout, which holds nothing but its reply, so a reviewer
        // that read the exact patch bytes says so in this file.
        let reads = directory.path().join("patch-reads.log");
        let reads = reads.to_str().unwrap();
        let mut packages = Vec::new();
        for name in policy.reviewers.keys() {
            pipeline.slots.insert(
                name.clone(),
                WorkerSlotV1 {
                    worker: format!("fixture/{name}"),
                    role: "review".into(),
                    input_type: "af/ReviewInput@1".into(),
                    output_type: result_type.into(),
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
                    ("assignment".into(), from("bind", name)),
                    ("history".into(), root("history")),
                    ("checks".into(), from("check", "result")),
                ]),
            );
            reviewer.when = Some(NodeConditionV1 {
                node: "check".into(),
                outcome: ReceiptOutcomeV1::Passed,
            });
            pipeline.nodes.push(reviewer);
            let stage = json!({"reports":if case == "finding" && name=="correctness" {json!([{"severity":"major","file":"lib.rs","line":1,"title":"Missing behavior","body":"The implementation omits the required behavior","fix":"Implement the requested behavior","confidence":0.9}])} else {json!([])},
                "benchmark_demands":if case=="demand" && name=="correctness" {json!([{"claim":"Runtime is bounded","why":"Large inputs matter","suggested_method":"Measure the scaling"}])} else {json!([])},"dispositions":[]});
            let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[stage]}});
            let script = if case == "missing_reviewer" && name == "bugs" {
                "import sys; sys.exit(9)".into()
            } else if !two_rounds {
                format!(
                    "import json,sys\nr=json.load(sys.stdin)\nassert r['inputs']['checks'][0]['payload']['outcome']=='passed'\na=r['inputs']['assignment'][0]['payload']\nassert a['reviewer']=={name:?} and a['round']==1 and a['findings']==[]\nprint({})\n",
                    serde_json::to_string(&serde_json::to_string(&reply).unwrap()).unwrap()
                )
            } else {
                format!(
                    r#"import json,sys,pathlib,os,hashlib
r=json.load(sys.stdin)
a=r['inputs']['assignment'][0]['payload']
s=r['inputs']['subject'][0]['payload']
assert a['reviewer']=={name:?}
assert all(f['source']=={name:?} for f in a['findings'])
assert len(a['findings']) == (1 if s['round']==2 and {name:?}=='correctness' else 0)
if 'change_scope' in s:
    patch=s['change_scope']['patch']
    b=pathlib.Path(patch['path']).read_bytes()
    assert len(b)==patch['bytes'] and len(b)>780*1024 and b'// readable source change' in b
    assert len(json.dumps(r))<65536 and 'canonical_patch_base64' not in json.dumps(r)
    assert 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\0'+b).hexdigest()==patch['content_id']
    with open({reads:?},'a') as evidence: evidence.write('read exact patch bytes: '+json.dumps(patch,sort_keys=True)+'\n')
    if {case:?}=='mutated':
        os.chmod(patch['path'],0o644)
        pathlib.Path(patch['path']).write_bytes(b'changed')
stage={{'reports':[],'benchmark_demands':[],'dispositions':[]}}
if s['round']==1 and {name:?}=='correctness':
    stage['reports']=[{{'severity':'major','file':'lib.rs','line':1,'title':'Missing behavior','body':'The implementation omits the required behavior','fix':'Implement it','confidence':1.0}}]
    if {case:?}=='valid':
        stage['reports'][0].pop('confidence')
        stage['reports'][0].pop('line')
    stage['benchmark_demands']=[{{'claim':'Runtime is bounded','why':'Large inputs matter','suggested_method':'Measure scaling'}}]
if s['round']==2:
    stage['dispositions']=[{{'finding_id':f['finding_id'],'position':'not_reproduced','reason':'Checked the same declared scope'}} for f in a['findings']]
    if {name:?}=='correctness':
        if {case:?}=='missing': stage['dispositions']=[]
        if {case:?}=='duplicate': stage['dispositions']*=2
        if {case:?}=='unassigned': stage['dispositions'].append({{'finding_id':'not-assigned','position':'not_reproduced','reason':'Unexpected claim'}})
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'result':[stage]}}}}))
"#
                )
            };
            let worker=TaskWorkerManifest {schema:"af.worker/1".into(),name:format!("fixture/{name}"),version:"1.0.0".into(),signature:signature.clone(),
                runner:TaskWorkerRunner::Command {command:serde_json::from_value(json!({"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]})).unwrap()}};
            let input_schema = json!({"type":"object","required":["source","subject","history","checks","assignment"],"additionalProperties":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}});
            let output_schema = json!({"type":"object","required":["reports","benchmark_demands","dispositions"],"additionalProperties":false,
                "properties":{"reports":{"type":"array"},"benchmark_demands":{"type":"array"},"dispositions":{"type":"array"}}});
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
        if case == "independent" {
            let mut slot = pipeline.slots["bugs"].clone();
            slot.worker = "fixture/failing".into();
            pipeline.slots.insert("failure".into(), slot);
            let mut failed = pipeline
                .nodes
                .iter()
                .find(|n| n.id == "bugs")
                .unwrap()
                .clone();
            failed.id = "independent_failure".into();
            failed.operator = TaskOperatorV1::Verify {
                slot: "failure".into(),
            };
            pipeline.nodes.push(failed);
            let mut files = packages
                .iter()
                .find(|(name, _)| name == "fixture/bugs")
                .unwrap()
                .1
                .clone();
            let mut worker: TaskWorkerManifest =
                toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
            worker.name = "fixture/failing".into();
            files.insert(
                "worker.toml".into(),
                toml::to_string(&worker).unwrap().into_bytes(),
            );
            files.insert("worker.py".into(), b"import sys; sys.exit(9)\n".to_vec());
            packages.push((worker.name, files));
            pipeline.max_attempts = 4;
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
        if two_rounds {
            let later: Vec<_> = pipeline
                .nodes
                .iter()
                .cloned()
                .map(|mut n| {
                    n.id = format!("second_{}", n.id);
                    for (port, value) in &mut n.inputs {
                        if port == "history" {
                            *value = from("reduce", "history");
                        } else if let ValueRefV1::Node { node, .. } = value {
                            *node = format!("second_{node}");
                        }
                    }
                    if let Some(condition) = &mut n.when {
                        condition.node = format!("second_{}", condition.node);
                    }
                    n
                })
                .collect();
            pipeline.nodes.extend(later);
            pipeline
                .outputs
                .insert("review".into(), from("second_reduce", "result"));
            pipeline
                .outputs
                .insert("history".into(), from("second_reduce", "history"));
            pipeline
                .coverage
                .insert("reviewed".into(), from("second_reduce", "result"));
            pipeline.max_attempts = 6;
        }
        let base = if matches!(case, "large" | "mutated" | "collision") {
            let manifest = Manifest::new(vec![Entry {
                path: "lib.rs".into(),
                kind: EntryKind::File,
                content: cas.put(b"old\n").unwrap(),
                size: 4,
            }])
            .unwrap();
            let snapshot = capture_snapshot(&cas, &manifest, &origin, None).unwrap();
            pipeline.contract.inputs.insert(
                "base".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
            );
            for node in &mut pipeline.nodes {
                if matches!(node.operator, TaskOperatorV1::ReviewBind {}) {
                    node.inputs.insert("base".into(), root("base"));
                }
            }
            Some(source_tree(&cas, producer(), &snapshot, vec![]).unwrap())
        } else {
            None
        };
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
        let mut task:TaskRevisionV1=serde_json::from_value(json!({"task_id":format!("review-{case}"),"revision":1,"kind":"review","goal":"Review the captured source",
            "inputs":{"source":source,"history":history},"required_outputs":{"review":{"artifact_type":TASK_REVIEW_ROUND_V1,"cardinality":"one"},"history":{"artifact_type":REVIEW_HISTORY_V1,"cardinality":"one"}},
            "acceptance":{"reviewed":{"evidence_type":TASK_REVIEW_ROUND_V1,"verifier_policy":policy_id}},"provenance":{"adapter_id":origin,"input_artifact_ids":[]},
            "authority":{"policy_id":policy_id,"allowed_effects":["execute-checks","read-source"],"data_destinations":[]},
            "limits":{"tokens":1000,"max_attempts":3,"deadline_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64+60000,"verification":{"tokens":0,"attempts":3,"wall_ms":15000}},
            "strategy":"light","pipeline":{"name":pipeline.name,"fallback":"refuse"},"facts":{}})).unwrap();
        if two_rounds {
            task.limits.max_attempts = 6;
            task.limits.verification.attempts = 6;
            task.limits.verification.wall_ms = 30000;
            if let Some(base) = base {
                task.inputs.insert("base".into(), base);
            }
        }
        if case == "independent" {
            task.limits.max_attempts = 4;
            task.limits.verification.attempts = 4;
            task.limits.verification.wall_ms = 20000;
        }
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
        if case == "valid"
            && let Some(destination) = std::env::var_os("AF_WRITE_TASK_REVIEW_V2_FIXTURE")
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
        let host = CapturedTaskHost::capture_with_models(
            &cas,
            &compiler,
            &task,
            &plan,
            graph,
            &environment,
            &domain,
            &BTreeMap::new(),
        )
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
        if case == "independent" {
            assert!(report.outcomes.iter().any(|(node, outcome)| {
                node.ends_with(".nodes.independent_failure")
                    && matches!(outcome, review_graph::NodeOutcome::Failed { .. })
            }));
            assert_eq!(result.execution, TaskExecutionV1::Exhausted);
            assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
            assert!(result.missing_obligations.is_empty());
            let receipt: TaskReviewRoundV1 = serde_json::from_value(
                cas.get_json(&result.outputs["review"].artifact_ids[0])
                    .unwrap()["payload"]
                    .clone(),
            )
            .unwrap();
            assert_eq!(receipt.conclusion, ReviewConclusionV1::Pass);
            domain.validate_result(&cas, &task, &result).unwrap();
            let id = cas
                .put_artifact(
                    TASK_RESULT_V1,
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(&result).unwrap(),
                )
                .unwrap()
                .0;
            runtime.finish(&id).unwrap();
            drop(runtime);
            drop(store);
            let reopened = EventStore::open(directory.path().join("events.sqlite")).unwrap();
            let state = reopened
                .task_projection(&cas, &task.task_id)
                .unwrap()
                .unwrap();
            assert_eq!(state.execution.unwrap().budget.begun_attempts(), 4);
            return;
        }
        if matches!(case, "large" | "mutated" | "collision") {
            let read = std::fs::read_to_string(reads).unwrap_or_default();
            assert_eq!(
                read.lines()
                    .filter(|line| line.starts_with("read exact patch bytes:"))
                    .count(),
                match case {
                    "large" => 4,
                    "mutated" => 4,
                    _ => 0,
                },
                "bounded native retrieval evidence: {report:?}"
            );
        }
        if two_rounds {
            assert_two_rounds(
                &cas,
                &domain,
                &task,
                &state,
                &result,
                case,
                &source_bytes,
                &snapshot,
                &report,
            );
            return;
        }
        assert!(result.outputs.contains_key("review"), "{case}: {report:?}");
        let review: TaskReviewRoundV1 = serde_json::from_value(
            cas.get_json(&result.outputs["review"].artifact_ids[0])
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap();
        let complete = !matches!(case, "missing_reviewer" | "unavailable");
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
        if case == "missing_reviewer" {
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
    let catalog = json!({"schema":"af.task-catalog/2","code_policy":".af/code-policy.toml","packages":pins,"independence":IndependencePolicyV1::default(),
        "review":{"generation":2,"reviewers":review.reviewers,"gate":review.gate,"clean_rounds":review.clean_rounds,"max_rounds":review.max_rounds}});
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

#[allow(clippy::too_many_arguments)]
fn assert_two_rounds(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    task: &TaskRevisionV1,
    state: &review_store::store::task::TaskProjection,
    result: &TaskResultV1,
    case: &str,
    source_bytes: &[u8],
    snapshot: &str,
    report: &review_graph::RunReport,
) {
    let execution = state.execution.as_ref().unwrap();
    let receipt = |name: &str| -> TaskReviewRoundV1 {
        let (_, (_, output)) = execution
            .outputs
            .iter()
            .find(|(node, _)| node.ends_with(name))
            .unwrap_or_else(|| panic!("missing {name}: {report:?}"));
        serde_json::from_value(
            cas.get_json(&output.outputs["result"].artifact_ids[0])
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap()
    };
    let first = receipt(".nodes.reduce");
    if matches!(case, "mutated" | "collision") {
        assert_eq!(first.conclusion, ReviewConclusionV1::Incomplete);
        assert!(first.finding_set_id.is_none());
    } else {
        let findings: review_core::FindingSetV1 = serde_json::from_value(
            cas.get_json(first.finding_set_id.as_ref().unwrap())
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap();
        let demands: review_core::DemandSetV1 = serde_json::from_value(
            cas.get_json(first.demand_set_id.as_ref().unwrap()).unwrap()["payload"].clone(),
        )
        .unwrap();
        let selected = cas
            .get_json(&first.selected_results["correctness"])
            .unwrap();
        assert!(
            selected["producer"]["node_id"]
                .as_str()
                .unwrap()
                .ends_with(".nodes.correctness")
        );
        for id in findings
            .selected_report_ids
            .iter()
            .chain(&demands.selected_demand_artifact_ids)
        {
            assert_eq!(
                cas.get_json(id).unwrap()["producer"],
                selected["producer"],
                "canonical artifacts must retain real Worker address"
            );
        }
        let original: review_core::ArtifactEnvelope =
            serde_json::from_value(selected.clone()).unwrap();
        let mut invented = original.producer.clone();
        if let Producer::Attempt { node_id, .. } = &mut invented {
            *node_id = "correctness".into();
        }
        let forged = cas
            .put_artifact(
                &original.artifact_type,
                invented,
                original.input_artifacts,
                original.subject_snapshot_id,
                original.payload,
            )
            .unwrap()
            .0;
        let mut wrong = first.invocation.clone();
        wrong.inputs.get_mut("correctness").unwrap().artifact_ids = vec![forged];
        assert!(
            domain
                .execute(cas, &wrong, None)
                .outputs
                .unwrap_err()
                .contains("declared Worker")
        );
        let second = receipt(".nodes.second_reduce");
        let complete = matches!(case, "valid" | "large");
        assert_eq!(
            second.finding_set_id.is_some(),
            complete,
            "{case}: {report:?}"
        );
        if complete {
            assert_eq!(second.conclusion, ReviewConclusionV1::ConvergenceExhausted);
            let set: review_core::FindingSetV1 = serde_json::from_value(
                cas.get_json(second.finding_set_id.as_ref().unwrap())
                    .unwrap()["payload"]
                    .clone(),
            )
            .unwrap();
            let current = cas
                .get_json(&second.selected_results["correctness"])
                .unwrap();
            let set_envelope = cas
                .get_json(second.finding_set_id.as_ref().unwrap())
                .unwrap();
            let mut dispositions = 0;
            for id in set_envelope["input_artifacts"].as_array().unwrap() {
                let artifact = cas.get_json(id.as_str().unwrap()).unwrap();
                if artifact["type"] == "review.kernel/FindingDisposition@1" {
                    assert_eq!(artifact["producer"], current["producer"]);
                    dispositions += 1;
                }
            }
            assert_eq!(dispositions, 1);
            if case == "large" {
                let (_, (_, bind)) = execution
                    .outputs
                    .iter()
                    .find(|(n, _)| n.ends_with(".nodes.bind"))
                    .unwrap();
                let subject: TaskReviewSubjectV2 = serde_json::from_value(
                    cas.get_json(&bind.outputs["subject"].artifact_ids[0])
                        .unwrap()["payload"]
                        .clone(),
                )
                .unwrap();
                let authority_bytes = cas
                    .get(subject.subject.change_set_id.as_ref().unwrap())
                    .unwrap();
                assert!(authority_bytes.len() > review_runner::task::MAX_WORKER_BYTES);
                assert!(authority_bytes.len() < review_core::MAX_CHANGE_SET_BYTES);
                assert!(subject.change_scope.unwrap().patch.bytes > 780 * 1024);
            }

            assert_eq!(set.findings.len(), 1);
            assert_eq!(
                set.findings[0].status, "open",
                "not reproduced never erases an unverified prior Finding"
            );
            let (_, (_, output)) = execution
                .outputs
                .iter()
                .find(|(node, _)| node.ends_with(".nodes.second_bind"))
                .unwrap();
            for (name, expected) in [("correctness", 1), ("bugs", 0)] {
                let assignment: TaskReviewAssignmentV1 = serde_json::from_value(
                    cas.get_json(&output.outputs[name].artifact_ids[0]).unwrap()["payload"].clone(),
                )
                .unwrap();
                assignment.validate().unwrap();
                assert_eq!(assignment.findings.len(), expected);
                assert_eq!(assignment.reviewer, name);
            }
        } else {
            assert_eq!(second.conclusion, ReviewConclusionV1::Incomplete);
        }
    }
    let (_, source) = review_source_git::task::read_snapshot(cas, snapshot).unwrap();
    assert_eq!(
        source.entries.len(),
        if case == "collision" { 2 } else { 1 },
        "host inputs cannot change the source tree"
    );
    assert_eq!(
        cas.get(
            &source
                .entries
                .iter()
                .find(|e| e.path == "lib.rs")
                .unwrap()
                .content
        )
        .unwrap(),
        source_bytes
    );
    assert!(
        source
            .entries
            .iter()
            .all(|e| !e.path.starts_with(".af-review-inputs/"))
    );
    domain.validate_result(cas, task, result).unwrap();
}
