use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use af_contracts::task::pipeline::*;
use af_contracts::task::plan::*;
use af_contracts::task::review::*;
use af_contracts::task::verification::TASK_CHECK_RECEIPT_V2;
use af_contracts::task::*;
use af_contracts::{DemandRequirement, PortCardinality, Producer, Severity};
use af_graph::task::{OperatorAttemptCost, OperatorSignature};
use af_pipeline_config::task::catalog::*;
use af_runtime::task::code::{CodeTaskPolicy, code_signatures};
use af_runtime::task::host::*;
use af_runtime::task::review::*;
use af_runtime::task::source::SnapshotTaskEnvironment;
use af_runtime::task::*;
use af_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
use af_source_git::{Entry, EntryKind, Manifest};
use af_store::{Cas, EventStore};
use serde_json::json;
mod native;

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
        brief: None,
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

// One test per case so the runner can schedule the cases side by side: a single test that
// looped over them held one thread for the whole sequence (ADR-0124).

#[test]
fn a_clean_review_task_passes_without_a_partial_ledger() {
    run_case("clean");
}

#[test]
fn a_finding_keeps_the_review_task_at_changes_requested() {
    run_case("finding");
}

#[test]
fn a_demand_keeps_the_review_task_at_changes_requested() {
    run_case("demand");
}

#[test]
fn a_missing_reviewer_leaves_the_review_task_incomplete() {
    run_case("missing_reviewer");
}

#[test]
fn an_unavailable_reviewer_leaves_the_review_task_incomplete() {
    run_case("unavailable");
}

#[test]
fn review_scopes_dispositions_preserves_actual_producers_for_valid_dispositions() {
    run_case("valid");
}

#[test]
fn review_scopes_dispositions_refuses_a_missing_disposition() {
    run_case("missing");
}

#[test]
fn review_scopes_dispositions_refuses_a_duplicate_disposition() {
    run_case("duplicate");
}

#[test]
fn review_scopes_dispositions_refuses_an_unassigned_disposition() {
    run_case("unassigned");
}

#[test]
fn review_scopes_dispositions_reads_a_large_patch() {
    run_case("large");
}

#[test]
fn review_scopes_dispositions_refuses_a_mutated_finding() {
    run_case("mutated");
}

#[test]
fn review_scopes_dispositions_refuses_a_colliding_finding() {
    run_case("collision");
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
        // The observer consumes real native Published outputs. It never reconstructs a
        // Campaign bootstrap or reads a partial Round as an authoritative Ledger.
        let telemetry = if matches!(case, "finding" | "demand" | "valid" | "missing_reviewer") {
            Some(NativeRecording::start(
                directory.path(),
                &af_store::store::task::task_run_id(&format!("review-{case}")).unwrap(),
            ))
        } else {
            None
        };
        let _dispatch = telemetry
            .as_ref()
            .map(|r| tracing::dispatcher::set_default(&r.dispatch));
        let _entered = telemetry.as_ref().map(|r| r.run.enter());
        if let Some(recording) = &telemetry {
            let mirror = af_runtime::telemetry::StoreMirror::with_writer(recording.writer.clone());
            if case == "valid" {
                store.observe_commits(std::sync::Arc::new(LateFirstNativeRound {
                    mirror,
                    state: Default::default(),
                }));
            } else {
                store.observe_commits(std::sync::Arc::new(mirror));
            }
        }
        let mut code = CodeTaskPolicy {
            check_process_wall_ms: None,
            schema: "af.code-task-policy/1".into(),
            checks: BTreeMap::from([(
                "syntax".into(),
                serde_json::from_value(json!({
                    "name": "syntax",
                    "command": af_contracts::Command::new(
                        if case == "unavailable" { "/no-such-check" } else { "/usr/bin/true" },
                        vec![],
                    ),
                    "required": true,
                }))
                .unwrap(),
            )]),
            check_wall_ms: 5000,
            require_container: false,
            warm: None,
            measures: BTreeMap::new(),
            objectives: BTreeMap::new(),
            rust_toolchain: None,
        };
        native::code(case, &mut code);
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
            max_rounds: if case.ends_with("blocking") || case.ends_with("later") {
                3
            } else if two_rounds || case.starts_with("native_integration") {
                2
            } else {
                1
            },
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
        if case.starts_with("native_") {
            manifest.entries.push(Entry {
                path: "other.rs".into(),
                kind: EntryKind::File,
                content: cas.put(b"original\n").unwrap(),
                size: 9,
            });
            manifest = Manifest::new(manifest.entries).unwrap();
        }
        if case == "collision" {
            manifest.entries.push(Entry {
                path: ".af-review-inputs".into(),
                kind: EntryKind::File,
                content: cas.put(b"owned source").unwrap(),
                size: 12,
            });
            manifest = Manifest::new(manifest.entries).unwrap();
        }
        let origin = cas.put_json(&serde_json::json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"fixture","source_revision":null,"content_digest":manifest.content_digest()})).unwrap();
        let parent = if case.starts_with("native_scatter_diff") {
            let base = Manifest::new(vec![]).unwrap();
            Some(capture_snapshot(&cas, &base, &origin, None).unwrap())
        } else {
            None
        };
        let snapshot = capture_snapshot(&cas, &manifest, &origin, parent.as_deref()).unwrap();
        let source_producer = if case.contains("_transport_") {
            Producer::KernelOperation {
                run_id: af_store::store::task::task_run_id("previous-root-review").unwrap(),
                node_id: Some("root.nodes.review_accept".into()),
                operation_id: "task-builtin@1".into(),
            }
        } else {
            producer()
        };
        let source = source_tree(&cas, source_producer, &snapshot, vec![]).unwrap();
        let sibling = if case.contains("_sibling_later") {
            let mut sibling = manifest.clone();
            sibling.entries[0].content = cas.put(b"sibling\n").unwrap();
            sibling.entries[0].size = 8;
            let id = capture_snapshot(&cas, &sibling, &origin, Some(&snapshot)).unwrap();
            Some(source_tree(&cas, producer(), &id, vec![]).unwrap())
        } else {
            None
        };
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
        let result_type = af_contracts::contract::REVIEWER_RESULT_V2;
        let signature = OperatorSignature {
            unbound_allowed: Default::default(),
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "source".into(),
                        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("subject".into(), port(TASK_REVIEW_SUBJECT_V3, same())),
                    ("assignment".into(), port(TASK_REVIEW_ASSIGNMENT_V1, same())),
                    (
                        "history".into(),
                        port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}),
                    ),
                    ("checks".into(), port(TASK_CHECK_RECEIPT_V2, same())),
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
        let mut pipeline = PipelineDefinition {
            schema: PipelineSchema::V6,
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
                        remote_checks: BTreeSet::new(),
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
                    recovery_attempts: None,
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
            let stage = json!({"reports":if case == "finding" && name=="correctness" {json!([{"severity":"major","file":"lib.rs","line":1,"title":"Missing behavior","body":"The implementation omits the required behavior\npassword=hunter2-planted-7781\n","fix":"Implement the requested behavior","confidence":0.9}])} else {json!([])},
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
            let base_origin = cas.put_json(&serde_json::json!({"schema":"af.task-source-origin/3","kind":"captured",
                "repository_id":"fixture","source_revision":null,"content_digest":manifest.content_digest()})).unwrap();
            let snapshot = capture_snapshot(&cas, &manifest, &base_origin, None).unwrap();
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
        if case.starts_with("native_") {
            native::configure(
                case.strip_suffix("_recover").unwrap_or(case),
                &mut pipeline,
                &mut packages,
            );
            native::configure_integration(
                case.strip_suffix("_recover").unwrap_or(case),
                &mut pipeline,
                &mut packages,
            );
            native::configure_sharded_integration(case, &mut pipeline, &mut packages);
            native::embed(case, &mut pipeline, &mut packages);
        }
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
        if case.starts_with("native_") {
            if let Some(sibling) = sibling {
                task.inputs.insert("sibling".into(), sibling);
            }
            if case.starts_with("native_integration") {
                task.authority.allowed_effects.insert("write-source".into());
            }
            task.limits.max_attempts = pipeline.max_attempts;
            task.limits.verification.attempts = if case == "native_integration_sharded" {
                5
            } else if case.ends_with("later") {
                8 + u32::from(case.contains("_sibling_later"))
                    + u32::from(case.contains("_descendant_later"))
            } else if case.starts_with("native_integration") {
                6
            } else if case.starts_with("native_scatter") {
                2
            } else {
                3
            };
            task.limits.verification.wall_ms = u64::from(task.limits.verification.attempts) * 5000;
        }
        native::embedded_task(case, &mut task);
        let mut signatures = code_signatures(&code_id, &code).unwrap();
        signatures.extend(review_signatures(&policy_id, &policy).unwrap());
        let mut compiler = TaskPlanCompiler::new(
            policy_id.clone(),
            policy_id.clone(),
            signatures,
            BTreeMap::from([(
                "reviewed".into(),
                if case.contains("_embedded_") {
                    "snapshot"
                } else {
                    "review"
                }
                .into(),
            )]),
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
            let worker_package = files.contains_key("worker.toml");
            let pin = TaskPackagePin {
                version: "1.0.0".into(),
                path: "package".into(),
                digest: af_pipeline_config::lock::package_digest_from_files(&files),
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
            if worker_package {
                compiler
                    .bind_worker(
                        &name,
                        AdmittedWorkerSettings {
                            execution: WorkerExecutionV3::Command {},
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
        let compiled = compiler.compile(&cas, &revision, &pipeline.name);
        if matches!(
            case,
            "native_integration_no_successor"
                | "native_integration_unrelated_successor"
                | "native_integration_embedded_no_successor"
                | "native_integration_condition_only"
                | "native_integration_embedded_condition_only"
        ) {
            let error = compiled.unwrap_err();
            assert!(error.contains("requires a full successor Round"), "{error}");
            assert!(store.run_ids().unwrap().is_empty());
            return;
        }
        if case.contains("_static_original_later") {
            let error = compiled.unwrap_err();
            assert!(error.contains("returns to a pre-commit source"), "{error}");
            assert!(store.run_ids().unwrap().is_empty());
            return;
        }
        let (plan, graph) = compiled.unwrap();
        if matches!(case, "native_integration" | "native_scatter") {
            native::assert_current_plan_schemas(&pipeline, &graph);
        }
        let plan_id = cas
            .put_artifact(
                EXECUTION_PLAN_V4,
                producer(),
                vec![],
                None,
                serde_json::to_value(&plan).unwrap(),
            )
            .unwrap()
            .0;
        let domain = ReviewTaskDomain::captured(&cas, &policy_id, graph.clone());
        if case.ends_with("no_closeout") || case == "native_scatter_sliced_closeout" {
            assert!(
                domain
                    .err()
                    .unwrap()
                    .contains("Whole-Subject closeout must consume")
            );
            return;
        }
        if case == "native_scatter_stage_bound" {
            assert!(domain.err().unwrap().contains("64-stage bound"));
            return;
        }
        let domain = domain
            .unwrap()
            .with_review_task(!case.contains("_embedded_"));
        let host_graph = graph.clone();
        let environment = SnapshotTaskEnvironment {
            policy: code.isolation(),
        };
        let fault = native::PublicationFault::new(&domain, case);
        let active_domain: &dyn TaskDomain = if case.ends_with("_recover") || case == "native_floor"
        {
            &fault
        } else {
            &domain
        };
        let host = CapturedTaskHost::capture_with_models(
            &cas,
            &compiler,
            &task,
            &plan,
            graph,
            &environment,
            active_domain,
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
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let runtime = TaskRuntime::new(&mut store, &cas, lease.clone(), &authority, &host).unwrap();
        let runtime = if case == "native_floor" {
            runtime.with_cancellation(&cancellation)
        } else {
            runtime
        };
        let report = runtime.execute().unwrap();
        let state = runtime.projection().unwrap();
        if case == "native_floor" {
            assert_eq!(
                runtime.disk_refusal().as_deref(),
                Some("insufficient_disk: native fixture floor")
            );
            let execution = state.execution.as_ref().unwrap();
            assert_eq!(execution.budget.begun_attempts(), 1);
            assert_eq!(execution.budget.committed_tokens(), 0);
            assert_eq!(execution.budget.reserved_tokens(), 0);
            assert!(execution.outputs.contains_key("root.nodes.check"));
            assert!(!execution.outputs.contains_key("root.nodes.correctness"));
            assert!(!execution.outputs.contains_key("root.nodes.bugs"));
            assert!(!execution.outputs.contains_key("root.nodes.reduce"));
            let result = domain.result(&cas, &state, &report).unwrap();
            assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
            assert!(!result.outputs.contains_key("review"));
            let initial_check = execution.outputs["root.nodes.check"].clone();
            drop(runtime);
            native::free_floor();
            cancellation.store(false, std::sync::atomic::Ordering::Release);
            let runtime = TaskRuntime::new(&mut store, &cas, lease.clone(), &authority, &host)
                .unwrap()
                .with_cancellation(&cancellation);
            let report = runtime.execute().unwrap();
            let state = runtime.projection().unwrap();
            let execution = state.execution.as_ref().unwrap();
            assert_eq!(execution.budget.begun_attempts(), 3);
            assert_eq!(execution.outputs["root.nodes.check"], initial_check);
            let result = domain.result(&cas, &state, &report).unwrap();
            assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
            let id = cas
                .put_artifact(
                    TASK_RESULT_V1,
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(result).unwrap(),
                )
                .unwrap()
                .0;
            runtime.finish(&id).unwrap();
            return;
        }
        if case.ends_with("_recover") {
            let prior_attempts = state.execution.as_ref().unwrap().budget.begun_attempts();
            assert_eq!(
                prior_attempts,
                if case.starts_with("native_integration") {
                    6
                } else {
                    3
                }
            );
            assert!(state.waiting_for_domain_publication(&cas).unwrap());
            let point = if case.contains("_embedded_") {
                "root.nodes.child.nodes.commit"
            } else if case.starts_with("native_integration") {
                "root.nodes.commit"
            } else {
                "root.nodes.bugs"
            };
            assert!(
                state
                    .execution
                    .as_ref()
                    .unwrap()
                    .outputs
                    .contains_key(point)
            );
            drop(runtime);
            store.release_task_lease(&cas, &lease).unwrap();
            let mut reopened = EventStore::open(directory.path().join("events.sqlite")).unwrap();
            let lease = reopened
                .take_task_lease(&cas, &task.task_id, "native-recovered", 60000)
                .unwrap();
            let host = CapturedTaskHost::capture_with_models(
                &cas,
                &compiler,
                &task,
                &plan,
                host_graph.clone(),
                &environment,
                &domain,
                &BTreeMap::new(),
            )
            .unwrap();
            let authority = CapturedTaskAuthority::new(&compiler, &host, &NoTaskDeveloper);
            reopened.resume_task(&cas, &lease, &authority).unwrap();
            let runtime =
                TaskRuntime::new(&mut reopened, &cas, lease.clone(), &authority, &host).unwrap();
            let report = runtime.execute().unwrap();
            let state = runtime.projection().unwrap();
            let result = domain.result(&cas, &state, &report).unwrap();
            if case.contains("_embedded_") {
                native::assert_embedded(
                    case.strip_suffix("_recover").unwrap(),
                    &cas,
                    &domain,
                    &runtime,
                    &state,
                    &result,
                    &report,
                    directory.path(),
                );
                return;
            }
            native::assert_native(
                case.strip_suffix("_recover").unwrap(),
                &cas,
                &domain,
                &runtime,
                &state,
                &result,
                &report,
                &snapshot,
                directory.path(),
                &lease,
            );
            return;
        }
        if native::rejects_later_source(case) || case.contains("_mismatched_snapshot_") {
            native::assert_rejected_later_source(
                &cas,
                &domain,
                &runtime,
                &state,
                &report,
                directory.path(),
            );
            return;
        }
        if case.ends_with("selected_old") {
            native::assert_discarded_successor(
                &cas,
                &domain,
                &runtime,
                &state,
                &report,
                directory.path(),
            );
            return;
        }
        let mut result = domain.result(&cas, &state, &report).unwrap();
        if case.contains("_transport_") {
            native::assert_source_transport(
                &cas,
                &domain,
                &runtime,
                &state,
                &result,
                &report,
                directory.path(),
            );
            return;
        }
        if case.contains("_embedded_") {
            native::assert_embedded(
                case,
                &cas,
                &domain,
                &runtime,
                &state,
                &result,
                &report,
                directory.path(),
            );
            return;
        }
        if case.starts_with("native_") {
            native::assert_native(
                case,
                &cas,
                &domain,
                &runtime,
                &state,
                &result,
                &report,
                &snapshot,
                directory.path(),
                &lease,
            );
            return;
        }
        if case == "independent" {
            assert!(report.outcomes.iter().any(|(node, outcome)| {
                node.ends_with(".nodes.independent_failure")
                    && matches!(outcome, af_graph::NodeOutcome::Failed { .. })
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
            let expected = match case {
                "large" | "mutated" => 4,
                _ => 0,
            };
            let read = std::fs::read_to_string(reads).unwrap_or_default();
            assert_eq!(
                read.lines()
                    .filter(|line| line.starts_with("read exact patch bytes:"))
                    .count(),
                expected,
                "bounded native retrieval evidence: {report:?}"
            );
            // The file proves the Worker read the exact bytes; it cannot prove the reading
            // Attempt's reply was stored durably. The same count of reviewer Attempts must also
            // reach a Settled record that retains its raw reply.
            let reader =
                EventStore::open_read_only(directory.path().join("events.sqlite")).unwrap();
            let mut settled = 0;
            for event in reader
                .replay(&af_store::store::task::task_run_id(&task.task_id).unwrap())
                .unwrap()
            {
                let Ok(transition) = af_store::store::task::read_task_transition(&event) else {
                    continue;
                };
                let af_contracts::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                    transition.change
                else {
                    continue;
                };
                let record =
                    af_store::store::task::execution::read_execution_record(&cas, &record_id)
                        .unwrap()
                        .record;
                if let af_contracts::task::execution::TaskExecutionRecordV1::Settled {
                    raw_artifact_ids,
                    ..
                } = record
                    && raw_artifact_ids.iter().any(|id| {
                        cas.get_json(id).is_ok_and(|raw| {
                            ["reports", "benchmark_demands", "dispositions"]
                                .iter()
                                .all(|key| {
                                    raw.pointer(&format!("/outputs/result/0/{key}")).is_some()
                                })
                        })
                    })
                {
                    settled += 1;
                }
            }
            assert_eq!(
                settled, expected,
                "durable reviewer settlements: {report:?}"
            );
        }
        if two_rounds {
            assert_two_rounds(
                &cas,
                &domain,
                &host_graph,
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
            let set: af_contracts::FindingSetV1 = serde_json::from_value(
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
            let replay_result = domain.result(&cas, &replay_state, &replay).unwrap();
            let mut expected = result.clone();
            let original = expected
                .evidence
                .iter()
                .find(|id| {
                    cas.get_artifact(id)
                        .is_ok_and(|a| a.artifact_type == TASK_REVIEW_CONCLUSION_V1)
                })
                .unwrap()
                .clone();
            let frozen: TaskReviewConclusionV1 =
                serde_json::from_value(cas.get_artifact(&original).unwrap().payload).unwrap();
            expected.evidence.remove(&original);
            let current = af_store::store::task::review_integration::native_review_conclusion(
                &cas,
                &replay_state,
                &expected,
            )
            .unwrap()
            .unwrap();
            let receipt: TaskReviewConclusionV1 =
                serde_json::from_value(cas.get_artifact(&current).unwrap().payload).unwrap();
            assert!(receipt.through_sequence > frozen.through_sequence);
            assert_eq!(
                receipt.cumulative_chargeable_tokens,
                frozen.cumulative_chargeable_tokens
            );
            assert_eq!(receipt.begun_attempts, frozen.begun_attempts);
            expected.evidence.insert(current);
            assert_eq!(replay_result, expected);
            result = replay_result;
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
        drop(runtime);
        af_runtime::telemetry::end_runs();
        drop(_entered);
        drop(_dispatch);
        if let Some(recording) = telemetry {
            recording.assert_summaries(case);
            if case == "finding" {
                let round_id = result
                    .evidence
                    .iter()
                    .find(|id| {
                        cas.get_artifact(id)
                            .is_ok_and(|r| r.artifact_type == TASK_REVIEW_ROUND_V1)
                    })
                    .unwrap();
                let round: TaskReviewRoundV1 =
                    serde_json::from_value(cas.get_artifact(round_id).unwrap().payload).unwrap();
                let view_id = round.finding_set_id.unwrap();
                let hex = view_id.strip_prefix("sha256:").unwrap();
                let path = directory
                    .path()
                    .join("cas/objects")
                    .join(&hex[..2])
                    .join(&hex[2..]);
                std::fs::remove_file(&path).unwrap();
                let round_id = result
                    .evidence
                    .iter()
                    .find(|id| {
                        cas.get_artifact(id)
                            .is_ok_and(|r| r.artifact_type == TASK_REVIEW_ROUND_V1)
                    })
                    .unwrap();
                assert!(ReviewTaskDomain::recorded_ledger(&cas, round_id).is_err());
                assert!(
                    !path.exists(),
                    "an observer cannot repair missing canonical evidence"
                );
            }
        }
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
                digest: af_pipeline_config::lock::package_digest_from_files(files),
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
    graph: &af_graph::task::CompiledTask,
    task: &TaskRevisionV1,
    state: &af_store::store::task::TaskProjection,
    result: &TaskResultV1,
    case: &str,
    source_bytes: &[u8],
    snapshot: &str,
    report: &af_graph::RunReport,
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
        let findings: af_contracts::FindingSetV1 = serde_json::from_value(
            cas.get_json(first.finding_set_id.as_ref().unwrap())
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap();
        let demands: af_contracts::DemandSetV1 = serde_json::from_value(
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
        let original: af_contracts::ArtifactEnvelope =
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
                .execute(cas, &wrong, &graph.nodes[&wrong.node], None, None)
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
            let set: af_contracts::FindingSetV1 = serde_json::from_value(
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
                let subject: TaskReviewSubject = serde_json::from_value(
                    cas.get_json(&bind.outputs["subject"].artifact_ids[0])
                        .unwrap()["payload"]
                        .clone(),
                )
                .unwrap();
                let authority_bytes = cas
                    .get(subject.subject.change_set_id.as_ref().unwrap())
                    .unwrap();
                assert!(authority_bytes.len() > af_runner::task::MAX_WORKER_BYTES);
                assert!(authority_bytes.len() < af_contracts::MAX_CHANGE_SET_BYTES);
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
    let (_, source) = af_source_git::task::read_snapshot(cas, snapshot).unwrap();
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

/// Native observer test setup; all semantic state is produced by the real Task runtime above.
struct NativeRecording {
    root: std::path::PathBuf,
    writer: af_telemetry::segment::writer::Writer,
    dispatch: tracing::Dispatch,
    run: tracing::Span,
    run_id: String,
}
impl NativeRecording {
    fn start(directory: &std::path::Path, run_id: &str) -> Self {
        use af_telemetry::subscriber::{TelemetrySubscriber, Typed};
        let root = directory.join("telemetry");
        let writer = af_telemetry::segment::writer::Writer::new(
            af_telemetry::segment::writer::WriterOptions::new(
                &root,
                if run_id == af_store::store::task::task_run_id("review-finding").unwrap() {
                    af_telemetry::level::Level::Content
                } else {
                    af_telemetry::level::Level::Spans
                },
            ),
        )
        .unwrap();
        let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
        let store = af_telemetry::trace::store_id(directory).unwrap();
        let run = tracing::dispatcher::with_default(&dispatch, || {
            tracing::info_span!(
                "af.run",
                af.store_id = Typed::identity(store).unwrap().field(),
                af.run_id = Typed::identity(run_id).unwrap().field(),
                af.run.kind = "task",
                af.execution.ordinal = 1_u64,
                af.ledger.coverage = tracing::field::Empty
            )
        });
        run.record("af.ledger.coverage", "complete");
        Self {
            root,
            writer,
            dispatch,
            run,
            run_id: run_id.into(),
        }
    }
    fn assert_summaries(self, case: &str) {
        let Self {
            root,
            writer,
            dispatch,
            run,
            run_id,
        } = self;
        tracing::dispatcher::with_default(&dispatch, || drop(run));
        writer.finish(af_telemetry::record::SealExit::Ok).unwrap();
        let records = af_telemetry::segment::reader::read_sealed(
            &writer
                .layout()
                .sealed_segment(&writer.segment_id().unwrap()),
        )
        .unwrap();
        use af_telemetry::record::Record;
        let bodies: Vec<_> = records
            .iter()
            .filter_map(|record| match record {
                Record::Event(event) if event.name == "af.finding.body" => Some(event),
                _ => None,
            })
            .collect();
        if case == "finding" {
            assert_eq!(bodies.len(), 1, "one body for the admitted native Finding");
            let occurrence = bodies[0].blob.as_ref().unwrap();
            let reference = records
                .iter()
                .find_map(|record| match record {
                    Record::BlobRef(reference) if &reference.occurrence_id == occurrence => {
                        Some(reference)
                    }
                    _ => None,
                })
                .unwrap();
            let af_telemetry::blob::reader::Content::Bytes(bytes) =
                af_telemetry::blob::reader::content(
                    &af_telemetry::layout::Layout::new(&root),
                    reference,
                )
            else {
                panic!("missing native Finding body")
            };
            let body = String::from_utf8(bytes).unwrap();
            assert!(body.contains("omits the required behavior"));
            assert!(body.contains(af_telemetry::redact::WITHHELD_LINE));
            assert!(!body.contains("hunter2"));
            let payloads: Vec<_> = records
                .iter()
                .filter_map(|record| match record {
                    Record::Event(event) if event.name == "af.store.event" => Some(event),
                    _ => None,
                })
                .collect();
            assert!(!payloads.is_empty());
            for event in payloads {
                let occurrence = event.blob.as_ref().expect("native committed payload blob");
                let reference = records
                    .iter()
                    .find_map(|record| match record {
                        Record::BlobRef(reference) if &reference.occurrence_id == occurrence => {
                            Some(reference)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    event.attributes["af.store.event.payload_digest"],
                    reference.source_digest.as_str()
                );
            }
        } else {
            assert!(bodies.is_empty(), "spans do not retain Finding bodies");
        }
        assert!(!format!("{records:?}").contains("hunter2"));

        let (index, read, unreadable) = af_telemetry::view::open(&root).unwrap();
        let af_telemetry::view::Resolved::Run {
            store_id,
            trace_id,
            kind,
        } = af_telemetry::view::resolve(&index, &run_id, None).unwrap()
        else {
            panic!("missing native trace")
        };
        let ledger = af_telemetry::ledger::ledger(
            &index,
            &root,
            read,
            unreadable,
            (&store_id, &run_id, &trace_id, kind),
        )
        .unwrap();
        assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
        if case == "finding" || case == "valid" {
            assert_eq!(
                ledger.findings.len(),
                1,
                "native canonical Findings must be summarized"
            );
            let finding = &ledger.findings[0];
            assert_eq!(finding.status, "open");
            assert!(!finding.node_id.is_empty());
            assert!(finding.report_artifact_id.starts_with("sha256:"));
            assert!(
                finding.model.is_none(),
                "command Workers have no invented model"
            );
        }
        if case == "demand" {
            assert_eq!(ledger.demands.len(), 1);
            assert_eq!(ledger.demands[0].status, "open");
        }
        if case == "missing_reviewer" {
            assert!(ledger.findings.is_empty());
            assert!(ledger.demands.is_empty());
        }
    }
}

/// Delay the first real Round publication until after the second callback. Other Task
/// accounting commits keep their exact original projection and are observed immediately.
struct LateFirstNativeRound {
    mirror: af_runtime::telemetry::StoreMirror,
    state: std::sync::Mutex<(bool, Option<Vec<af_contracts::RunEvent>>)>,
}
impl af_store::store::observer::CommitObserver for LateFirstNativeRound {
    fn committed(&self, commit: &af_store::store::observer::Commit<'_>) {
        use af_contracts::task::event::TaskChangeV1;
        use af_contracts::task::execution::{TaskExecutionRecordV1, TaskOutputV1};
        use af_store::store::task::{execution::read_execution_record, read_task_transition};
        let round = commit.events.iter().any(|event| {
            let Ok(transition) = read_task_transition(event) else {
                return false;
            };
            let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                return false;
            };
            let Ok(record) = read_execution_record(commit.cas, &record_id) else {
                return false;
            };
            let TaskExecutionRecordV1::Published { output_id, .. } = record.record else {
                return false;
            };
            let output: TaskOutputV1 =
                serde_json::from_value(commit.cas.get_artifact(&output_id).unwrap().payload)
                    .unwrap();
            output
                .outputs
                .values()
                .any(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
        });
        let mut state = self.state.lock().unwrap();
        if round && !state.0 {
            state.0 = true;
            state.1 = Some(commit.events.to_vec());
            return;
        }
        self.mirror.committed(commit);
        if round && let Some(events) = state.1.take() {
            self.mirror.committed(&af_store::store::observer::Commit {
                store: commit.store,
                cas: commit.cas,
                events: &events,
                task: None,
            });
        }
    }
    fn wall_recorded(&self, commit: &af_store::store::observer::WallCommit<'_>) {
        self.mirror.wall_recorded(commit);
    }
}
