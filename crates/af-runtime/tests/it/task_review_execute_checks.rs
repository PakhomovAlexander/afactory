//! A review Worker declaring `execute-checks` runs in an ephemeral-write clone of its source,
//! with the adapter access the same captured effects derive, and seals nothing back. The
//! reviewer/evaluator adapters run in-process without Provider calls. Opt-in evidence cases
//! really compile, privately install and execute a small captured C program after the local gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use af_contracts::task::event::TaskChangeV1;
use af_contracts::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use af_contracts::task::pipeline::*;
use af_contracts::task::plan::*;
use af_contracts::task::review::*;
use af_contracts::task::usage::TaskTokenUsageV3;
use af_contracts::task::verification::*;
use af_contracts::task::*;
use af_contracts::{DemandRequirement, PortCardinality, Producer, Severity};
use af_graph::task::{OperatorAttemptCost, OperatorSignature};
use af_pipeline_config::task::catalog::*;
use af_runner::task::{ModelWorkerReturn, WorkerAccess, WorkerModelAdapter};
use af_runtime::task::code::{CodeTaskPolicy, code_signatures};
use af_runtime::task::host::*;
use af_runtime::task::review::*;
use af_runtime::task::source::{SnapshotTaskEnvironment, worker_access};
use af_runtime::task::*;
use af_source_git::task::{SOURCE_TREE_V1, capture_snapshot, read_snapshot, source_tree};
use af_source_git::{Entry, EntryKind, Manifest};
use af_store::{Cas, EventStore};
use serde_json::json;

const LIBRARY: &str = "pub fn example() {}\n";
const MAIN: &str = "fn main() {}\n";

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "review-execute-checks-fixture".into(),
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

/// Stands in for a UIX reviewer: it builds into and writes its pseudo-terminal harness under a
/// new `target/` directory, and in the edit cases also changes the declared source.
struct Uix {
    case: &'static str,
    calls: AtomicUsize,
}

impl WorkerModelAdapter for Uix {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("uix-model".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        _: Duration,
        access: WorkerAccess,
        _: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
        if request["inputs"]["testing"].is_array() {
            assert_eq!(access, WorkerAccess::ReadOnly);
            let testing = &request["inputs"]["testing"][0];
            assert_eq!(
                testing["snapshot_id"],
                request["inputs"]["source"][0]["snapshot_id"]
            );
            assert_eq!(testing["payload"]["snapshot_id"], testing["snapshot_id"]);
            assert_ne!(
                testing["artifact_id"],
                request["inputs"]["secondary"][0]["artifact_id"]
            );
            let reply = serde_json::to_vec(&json!({"schema":"af.worker-reply/1",
                "outputs":{"result":[{"outcome":"passed","reason":"Judged exact retained test evidence"}]}})).unwrap();
            return ModelWorkerReturn {
                notes: Default::default(),
                usage_observation: None,
                raw_artifact_ids: vec![cas.put(&reply).unwrap()],
                message: Ok(reply),
                usage: Some(TaskTokenUsageV3::charge_only(1)),
                native_failure: None,
            };
        }
        assert_eq!(access, WorkerAccess::ExecuteChecks);
        assert!(environment.is_empty());
        let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
        assert!(request["inputs"]["subject"].is_array());
        // The sandbox root is a writable clone of the exact source Snapshot.
        assert_eq!(
            std::fs::read(workdir.join("lib.rs")).unwrap(),
            LIBRARY.as_bytes()
        );
        let metadata = std::fs::metadata(workdir.join("lib.rs")).unwrap();
        assert!(!metadata.permissions().readonly());
        let harness = workdir.join("target/uix-harness");
        std::fs::create_dir_all(&harness).unwrap();
        std::fs::write(harness.join("drive.py"), "import pty\n").unwrap();
        std::fs::write(harness.join("screen-100x30.txt"), "PROVIDERS\n").unwrap();
        let edited = match self.case {
            "edit" | "evidence_edit" => Some(("lib.rs", "pub fn changed() {}\n")),
            // Added beside the source — a new top-level file, a root dotfile — the clone is
            // discarded, never a source edit; added under a name the source holds, it is one.
            "add" => Some(("NOTES.md", "notes\n")),
            "inside" => Some(("src/extra.rs", "pub fn extra() {}\n")),
            "dotfile" => Some((".claude.json", "{}\n")),
            _ => None,
        };
        if let Some((path, text)) = edited {
            std::fs::write(workdir.join(path), text).unwrap();
        }
        if self.case == "delete" {
            std::fs::remove_file(workdir.join("lib.rs")).unwrap();
        }
        let mut reply = json!({"schema":"af.worker-reply/1",
            "outputs":{"result":[{"reports":[],"benchmark_demands":[],"dispositions":[]}]}});
        if self.case.starts_with("evidence_") && self.case != "evidence_missing" {
            let mut commands = Vec::new();
            if self.case != "evidence_unavailable" {
                for (purpose, command) in [
                    ("build", "cc program.c -o target/program"),
                    (
                        "install",
                        "mkdir -p target/private/bin && cp target/program target/private/bin/program",
                    ),
                    (
                        "test",
                        if self.case == "evidence_failed" {
                            "target/private/bin/program wrong"
                        } else {
                            "target/private/bin/program exact"
                        },
                    ),
                ] {
                    let output = std::process::Command::new("/bin/sh")
                        .args(["-c", command])
                        .current_dir(workdir)
                        .output()
                        .unwrap();
                    commands.push(json!({"purpose":purpose,"command":command,
                        "exit_code":output.status.code(),
                        "evidence":format!("stdout={} stderr={} exit={}",
                            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr), output.status)}));
                }
            }
            reply["outputs"]["testing"] = json!([{
                "schema":"af/implementation-test-evidence@1",
                "snapshot_id":if self.case == "evidence_stale" { json!(format!("sha256:{}", "f".repeat(64))) }
                    else { request["inputs"]["source"][0]["snapshot_id"].clone() },
                "status":match self.case { "evidence_failed"=>"failed", "evidence_unavailable"=>"unavailable", _=>"passed" },
                "reason":"Private candidate build/install/execution results; unavailable means no commands ran",
                "commands":commands
            }]);
        }
        if let Some(testing) = reply["outputs"].get("testing").cloned() {
            let mut secondary = testing;
            secondary[0]["reason"] =
                json!("Second distinct execution receipt for the same candidate");
            reply["outputs"]["secondary"] = secondary;
        }
        let reply = serde_json::to_vec(&reply).unwrap();
        ModelWorkerReturn {
            notes: Default::default(),
            usage_observation: None,
            raw_artifact_ids: vec![cas.put(&reply).unwrap()],
            message: Ok(reply),
            usage: Some(TaskTokenUsageV3::charge_only(1)),
            native_failure: None,
        }
    }
}

struct Outcome {
    result: TaskResultV1,
    review: TaskReviewRoundV1,
    verification: Option<VerificationResultV1>,
    /// Every failed Attempt's recorded diagnostic, in log order.
    diagnostics: Vec<String>,
    calls: usize,
}

// Worker schemas are local-only: standalone public schema metadata is not captured.
fn local_schema(bytes: &[u8]) -> Vec<u8> {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.remove("$id");
                map.remove("$schema");
                for value in map.values_mut() {
                    strip(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    strip(value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::from_slice(bytes).unwrap();
    strip(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn run(case: &'static str) -> Outcome {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let code = CodeTaskPolicy {
        check_process_wall_ms: None,
        schema: "af.code-task-policy/1".into(),
        checks: BTreeMap::from([(
            "syntax".into(),
            af_check::CheckDefinition::new(
                "syntax",
                af_contracts::Command::new("/usr/bin/true", vec![]),
            ),
        )]),
        check_wall_ms: 5000,
        require_container: false,
        warm: None,
        measures: BTreeMap::new(),
        objectives: BTreeMap::new(),
        rust_toolchain: None,
    };
    let code_id = cas.put_json(&serde_json::to_value(&code).unwrap()).unwrap();
    let policy = ReviewTaskPolicy {
        allow_targeted_repairs: false,
        schema: REVIEW_TASK_POLICY_SCHEMA.into(),
        check_policy_id: code_id.clone(),
        reviewers: BTreeMap::from([("uix".into(), DemandRequirement::Required)]),
        gate: Severity::Major,
        clean_rounds: 1,
        max_rounds: 1,
    };
    let policy_id = cas
        .put_json(&serde_json::to_value(&policy).unwrap())
        .unwrap();
    let entries: Vec<Entry> = [
        ("lib.rs", LIBRARY),
        ("src/main.rs", MAIN),
        (
            "program.c",
            include_str!("../../../../fixtures/executable-review/program.c"),
        ),
    ]
    .into_iter()
    .map(|(path, text)| Entry {
        path: path.into(),
        kind: EntryKind::File,
        content: cas.put(text.as_bytes()).unwrap(),
        size: text.len() as u64,
    })
    .collect();
    let manifest = Manifest::new(entries).unwrap();
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
    let result_type = af_contracts::contract::REVIEWER_RESULT_V2;
    let mut signature = OperatorSignature {
        unbound_allowed: Default::default(),
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
                ("checks".into(), port(TASK_CHECK_RECEIPT_V2, same())),
            ]),
            outputs: BTreeMap::from([("result".into(), port(result_type, same()))]),
        },
        effects: BTreeSet::from(["read-source".into(), "execute-checks".into()]),
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
            tokens: 100_000,
            wall_ms: 5000,
        }),
    };
    let executable = case.starts_with("evidence_");
    if executable {
        signature.contract.inputs.insert(
            "requirements".into(),
            port("af/Requirements@1", PortAffinityV1::Unbound {}),
        );
        signature
            .retains
            .get_mut("result")
            .unwrap()
            .insert("requirements".into());
        signature.contract.outputs.insert(
            "testing".into(),
            port(IMPLEMENTATION_TEST_EVIDENCE_V1, same()),
        );
        signature.contract.outputs.insert(
            "secondary".into(),
            port(IMPLEMENTATION_TEST_EVIDENCE_V1, same()),
        );
        signature.retains.insert(
            "secondary".into(),
            BTreeSet::from(["source".into(), "checks".into()]),
        );
        signature.retains.insert(
            "testing".into(),
            BTreeSet::from(["source".into(), "checks".into()]),
        );
    }
    assert_eq!(worker_access(&signature), WorkerAccess::ExecuteChecks);
    let mut public_result = port(TASK_REVIEW_ROUND_V1, same());
    public_result.covers.insert("reviewed".into());
    let mut history_port = port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {});
    history_port.root_default = Some(RootDefaultV1::EmptyReviewHistory);
    let mut reviewer = node(
        "uix",
        TaskOperatorV1::Verify { slot: "uix".into() },
        BTreeMap::from([
            ("source".into(), root("source")),
            ("subject".into(), from("bind", "subject")),
            ("assignment".into(), from("bind", "uix")),
            ("history".into(), root("history")),
            ("checks".into(), from("check", "result")),
        ]),
    );
    reviewer.when = Some(NodeConditionV1 {
        node: "check".into(),
        outcome: ReceiptOutcomeV1::Passed,
    });
    let mut pipeline = PipelineDefinitionV1 {
        schema: PipelineSchema::V1,
        name: "fixture/review".into(),
        version: "1.0.0".into(),
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([
                (
                    "source".into(),
                    port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                ),
                ("history".into(), history_port),
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
        slots: BTreeMap::from([(
            "uix".into(),
            WorkerSlotV1 {
                worker: "fixture/uix".into(),
                role: "review".into(),
                input_type: "af/ReviewInput@1".into(),
                output_type: result_type.into(),
                min_attempts: 1,
                max_attempts: 1,
                allow_local_replacement: false,
                independent_from: BTreeSet::new(),
                recovery_attempts: None,
            },
        )]),
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
            reviewer,
            node(
                "reduce",
                TaskOperatorV1::ReviewReduce {},
                BTreeMap::from([
                    ("source".into(), root("source")),
                    ("subject".into(), from("bind", "subject")),
                    ("history".into(), root("history")),
                    ("checks".into(), from("check", "result")),
                    ("uix".into(), from("uix", "result")),
                ]),
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
    let mut evaluator = signature.clone();
    evaluator.effects = BTreeSet::from(["read-source".into()]);
    evaluator.roles = BTreeSet::from(["evaluate".into()]);
    evaluator.worker_input_type = Some(IMPLEMENTATION_EVALUATION_INPUT_V1.into());
    evaluator.worker_output_type = Some(TASK_EVALUATION_V1.into());
    evaluator
        .contract
        .inputs
        .retain(|name, _| ["source", "checks"].contains(&name.as_str()));
    evaluator.contract.inputs.insert(
        "testing".into(),
        port(IMPLEMENTATION_TEST_EVIDENCE_V1, same()),
    );
    evaluator.contract.inputs.insert(
        "secondary".into(),
        port(IMPLEMENTATION_TEST_EVIDENCE_V1, same()),
    );
    evaluator.contract.inputs.insert(
        "requirements".into(),
        port("af/Requirements@1", PortAffinityV1::Unbound {}),
    );
    evaluator.contract.outputs =
        BTreeMap::from([("result".into(), port(TASK_EVALUATION_V1, same()))]);
    evaluator.retains = BTreeMap::from([(
        "result".into(),
        BTreeSet::from([
            "testing".into(),
            "secondary".into(),
            "checks".into(),
            "requirements".into(),
        ]),
    )]);
    evaluator.outcome_port = Some("result".into());
    if executable {
        pipeline.accepts.kinds = BTreeSet::from(["implement".into()]);
        let mut reviewed = port(REVIEWED_IMPLEMENTATION_V1, same());
        reviewed.covers.insert("reviewed".into());
        pipeline
            .contract
            .outputs
            .insert("reviewed".into(), reviewed);
        pipeline
            .outputs
            .insert("reviewed".into(), from("review_accept", "result"));
        pipeline.nodes.push(node(
            "review_accept",
            TaskOperatorV1::ReviewAccept {},
            BTreeMap::from([
                ("source".into(), root("source")),
                ("checks".into(), from("check", "result")),
                ("review".into(), from("reduce", "result")),
            ]),
        ));
        pipeline
            .contract
            .outputs
            .get_mut("review")
            .unwrap()
            .covers
            .clear();
        pipeline.coverage = BTreeMap::from([
            ("verified".into(), from("accept", "result")),
            ("reviewed".into(), from("review_accept", "result")),
        ]);
        let mut verification = port(VERIFICATION_RESULT_V1, same());
        verification.covers.insert("verified".into());
        pipeline
            .contract
            .outputs
            .insert("verification".into(), verification);
        pipeline
            .outputs
            .insert("verification".into(), from("accept", "result"));
        pipeline
            .contract
            .outputs
            .insert("snapshot".into(), port(SOURCE_TREE_V1, same()));
        pipeline
            .outputs
            .insert("snapshot".into(), from("accept", "snapshot"));
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "uix")
            .unwrap()
            .inputs
            .insert("requirements".into(), root("requirements"));
        pipeline.max_attempts = 4;
        let mut slot = pipeline.slots["uix"].clone();
        slot.worker = "fixture/evaluator".into();
        slot.role = "evaluate".into();
        slot.input_type = IMPLEMENTATION_EVALUATION_INPUT_V1.into();
        slot.output_type = TASK_EVALUATION_V1.into();
        pipeline.slots.insert("judge".into(), slot);
        pipeline.contract.inputs.insert(
            "requirements".into(),
            port("af/Requirements@1", PortAffinityV1::Unbound {}),
        );
        let mut evaluate = node(
            "evaluate",
            TaskOperatorV1::Verify {
                slot: "judge".into(),
            },
            BTreeMap::from([
                ("source".into(), root("source")),
                ("checks".into(), from("check", "result")),
                ("requirements".into(), root("requirements")),
                ("testing".into(), from("uix", "testing")),
                ("secondary".into(), from("uix", "secondary")),
            ]),
        );
        evaluate.when = Some(NodeConditionV1 {
            node: "check".into(),
            outcome: ReceiptOutcomeV1::Passed,
        });
        pipeline.nodes.push(evaluate);
        pipeline.nodes.push(node(
            "accept",
            TaskOperatorV1::Accept {},
            BTreeMap::from([
                ("source".into(), root("source")),
                ("checks".into(), from("check", "result")),
                ("evaluation".into(), from("evaluate", "result")),
            ]),
        ));
    }
    // Keep Review's authoritative public coverage at a child boundary, as native
    // implementation acceptance requires. Export each evidence edge explicitly.
    let review_child = if executable {
        let mut child = pipeline.clone();
        child.name = "fixture/executable-review".into();
        child.accepts.kinds = BTreeSet::from(["review".into()]);
        child
            .nodes
            .retain(|n| ["bind", "check", "uix", "reduce"].contains(&n.id.as_str()));
        child.slots.retain(|name, _| name == "uix");
        child.nodes.retain(|n| n.id != "check");
        child
            .contract
            .inputs
            .insert("checks".into(), port(TASK_CHECK_RECEIPT_V2, same()));
        for n in &mut child.nodes {
            if n.inputs.contains_key("checks") {
                n.inputs.insert("checks".into(), root("checks"));
            }
        }
        // The check receipt is a required dependency; native context admission refuses
        // reviewer dispatch unless it passed. Export required evidence on every graph path.
        child.nodes.iter_mut().find(|n| n.id == "uix").unwrap().when = None;
        child
            .contract
            .outputs
            .retain(|name, _| ["review", "history"].contains(&name.as_str()));
        child
            .outputs
            .retain(|name, _| ["review", "history"].contains(&name.as_str()));
        child
            .contract
            .outputs
            .get_mut("review")
            .unwrap()
            .covers
            .insert("reviewed".into());
        child.coverage = BTreeMap::from([("reviewed".into(), from("reduce", "result"))]);
        for name in ["testing", "secondary"] {
            child
                .contract
                .outputs
                .insert(name.into(), port(IMPLEMENTATION_TEST_EVIDENCE_V1, same()));
            child.outputs.insert(name.into(), from("uix", name));
        }
        child.max_attempts = 1;
        pipeline.name = "fixture/implementation".into();
        pipeline
            .nodes
            .retain(|n| !["bind", "uix", "reduce"].contains(&n.id.as_str()));
        pipeline.slots.retain(|name, _| name != "uix");
        pipeline.nodes.push(node(
            "review_child",
            TaskOperatorV1::Call {
                pipeline: child.name.clone(),
                bindings: BTreeMap::new(),
            },
            BTreeMap::from([
                ("source".into(), root("source")),
                ("history".into(), root("history")),
                ("requirements".into(), root("requirements")),
                ("checks".into(), from("check", "result")),
            ]),
        ));
        pipeline
            .outputs
            .insert("snapshot".into(), from("review_accept", "snapshot"));
        for name in ["review", "history"] {
            pipeline
                .outputs
                .insert(name.into(), from("review_child", name));
        }
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "review_accept")
            .unwrap()
            .inputs
            .insert("review".into(), from("review_child", "review"));
        let evaluate = pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "evaluate")
            .unwrap();
        for name in ["testing", "secondary"] {
            evaluate
                .inputs
                .insert(name.into(), from("review_child", name));
        }
        Some(child)
    } else {
        None
    };
    let worker = TaskWorkerManifest {
        schema: "af.worker/1".into(),
        name: "fixture/uix".into(),
        version: "1.0.0".into(),
        signature,
        runner: TaskWorkerRunner::Model {
            provider_kind: "fixture".into(),
            model: "uix-model".into(),
            effort: "high".into(),
        },
    };
    let input_schema = json!({"type":"object","required":["source","subject","history","checks","assignment"],
        "additionalProperties":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}});
    let output_schema = json!({"type":"object","required":["reports","benchmark_demands","dispositions"],"additionalProperties":false,
        "properties":{"reports":{"type":"array"},"benchmark_demands":{"type":"array"},"dispositions":{"type":"array"}}});
    let mut packages: Vec<(String, BTreeMap<String, Vec<u8>>)> = vec![
        (
            pipeline.name.clone(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&pipeline).unwrap().into_bytes(),
            )]),
        ),
        (
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
                (
                    "instructions.md".into(),
                    b"Build the candidate and drive it in a pseudo-terminal.".to_vec(),
                ),
            ]),
        ),
    ];
    if let Some(child) = review_child {
        packages.push((
            child.name.clone(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&child).unwrap().into_bytes(),
            )]),
        ));
    }
    if executable {
        packages[1].1.insert(
            "outputs/testing.schema.json".into(),
            local_schema(include_bytes!(
                "../../../../schemas/implementation-test-evidence-v1.json"
            )),
        );
        let secondary_schema = packages[1].1["outputs/testing.schema.json"].clone();
        packages[1]
            .1
            .insert("outputs/secondary.schema.json".into(), secondary_schema);
        let mut judge = worker.clone();
        judge.name = "fixture/evaluator".into();
        judge.signature = evaluator;
        packages.push((
            judge.name.clone(),
            BTreeMap::from([
                (
                    "worker.toml".into(),
                    toml::to_string(&judge).unwrap().into_bytes(),
                ),
                ("input.schema.json".into(), {
                    let mut schema: serde_json::Value =
                        serde_json::from_slice(&local_schema(include_bytes!(
                            "../../../../schemas/implementation-evaluation-input-v1.json"
                        )))
                        .unwrap();
                    schema["properties"]["secondary"] = schema["properties"]["testing"].clone();
                    schema["required"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("secondary"));
                    serde_json::to_vec(&schema).unwrap()
                }),
                (
                    "outputs/result.schema.json".into(),
                    br#"{"type":"object","required":["outcome","reason"]}"#.to_vec(),
                ),
                (
                    "instructions.md".into(),
                    b"Judge the exact testing evidence".to_vec(),
                ),
            ]),
        ));
    }
    if case == "evidence_passed" {
        let mut missing = pipeline.clone();
        missing.name = "fixture/missing-testing".into();
        missing
            .nodes
            .iter_mut()
            .find(|n| n.id == "evaluate")
            .unwrap()
            .inputs
            .remove("testing");
        packages.push((
            missing.name.clone(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&missing).unwrap().into_bytes(),
            )]),
        ));
    }
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000;
    let mut task:TaskRevisionV1=serde_json::from_value(json!({"task_id":format!("uix-{case}"),"revision":1,"kind":"review","goal":"Review the captured source by running it",
        "inputs":{"source":source,"history":history},"required_outputs":{"review":{"artifact_type":TASK_REVIEW_ROUND_V1,"cardinality":"one"},"history":{"artifact_type":REVIEW_HISTORY_V1,"cardinality":"one"}},
        "acceptance":{"reviewed":{"evidence_type":TASK_REVIEW_ROUND_V1,"verifier_policy":policy_id}},"provenance":{"adapter_id":origin,"input_artifact_ids":[]},
        "authority":{"policy_id":policy_id,"allowed_effects":["execute-checks","read-source"],"data_destinations":[]},
        "limits":{"tokens":200000,"max_attempts":3,"deadline_unix_ms":deadline,"verification":{"tokens":100000,"attempts":3,"wall_ms":15000}},
        "strategy":"light","pipeline":{"name":pipeline.name,"fallback":"refuse"},"facts":{}})).unwrap();
    if executable {
        task.kind = "implement".into();
        task.goal = "Evaluate implementation against exact executable evidence".into();
        task.acceptance = serde_json::from_value(
            json!({"verified":{"evidence_type":VERIFICATION_RESULT_V1,"verifier_policy":code_id}}),
        )
        .unwrap();
        task.acceptance.insert(
            "reviewed".into(),
            serde_json::from_value(json!({
                "evidence_type":REVIEWED_IMPLEMENTATION_V1,"verifier_policy":policy_id
            }))
            .unwrap(),
        );
        task.required_outputs.insert(
            "reviewed".into(),
            serde_json::from_value(json!({
                "artifact_type":REVIEWED_IMPLEMENTATION_V1,"cardinality":"one"
            }))
            .unwrap(),
        );
        for name in ["snapshot", "verification"] {
            task.required_outputs.insert(name.into(), serde_json::from_value(json!({
                "artifact_type":if name == "snapshot" {SOURCE_TREE_V1} else {VERIFICATION_RESULT_V1}, "cardinality":"one"
            })).unwrap());
        }
        task.limits.max_attempts = 4;
        task.limits.verification.attempts = 4;
        task.limits.verification.tokens = 200000;
        task.limits.verification.wall_ms = 20000;
        let requirements = cas
            .put_artifact(
                "af/Requirements@1",
                producer(),
                vec![],
                None,
                json!({"text":"Build/install/test the candidate"}),
            )
            .unwrap()
            .0;
        task.inputs.insert(
            "requirements".into(),
            ArtifactInputV1 {
                artifact_ids: vec![requirements],
                artifact_type: "af/Requirements@1".into(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        );
    }
    let mut signatures = code_signatures(&code_id, &code).unwrap();
    signatures.extend(review_signatures(&policy_id, &policy).unwrap());
    let mut compiler = TaskPlanCompiler::new(
        policy_id.clone(),
        policy_id.clone(),
        signatures,
        if executable {
            BTreeMap::from([
                ("verified".into(), "snapshot".into()),
                ("reviewed".into(), "snapshot".into()),
            ])
        } else {
            BTreeMap::from([("reviewed".into(), "review".into())])
        },
        IndependencePolicyV1::default(),
    )
    .unwrap();
    // The catalog admits the review Worker's `execute-checks` declaration as captured bytes.
    for (name, files) in packages {
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
    }
    for worker_name in if executable {
        vec!["fixture/uix", "fixture/evaluator"]
    } else {
        vec!["fixture/uix"]
    } {
        compiler
            .bind_worker(
                worker_name,
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV3::Model {
                        provider: "personal".into(),
                        provider_kind: "fixture".into(),
                        principal_id: Some(policy_id.clone()),
                        model: "uix-model".into(),
                        pi: None,
                        effort: "high".into(),
                    },
                    invocation_policy_id: policy_id.clone(),
                },
            )
            .unwrap();
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
    // So does the plan compiler, within the Task's captured effect authority.
    if case == "evidence_passed" {
        let error = compiler
            .candidate_structure(&cas, &task, "fixture/missing-testing")
            .unwrap_err();
        assert!(error.contains("lacks testing"), "{error}");
    }
    let (plan, graph) = compiler.compile(&cas, &revision, &pipeline.name).unwrap();
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
    let domain = ReviewTaskDomain::captured(&cas, &policy_id, graph.clone())
        .unwrap()
        .with_review_task(!executable);
    let environment = SnapshotTaskEnvironment {
        policy: code.isolation(),
    };
    let model = Uix {
        case,
        calls: AtomicUsize::new(0),
    };
    let models: BTreeMap<String, TaskModelBinding<'_>> = plan
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
    let host = CapturedTaskHost::capture_with_models(
        &cas,
        &compiler,
        &task,
        &plan,
        graph.clone(),
        &environment,
        &domain,
        &models,
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
    domain.validate_result(&cas, &task, &result).unwrap();
    if case == "evidence_passed" {
        assert_eq!(
            result.acceptance,
            TaskAcceptanceV1::Satisfied,
            "{result:?} {report:?}"
        );
        let code_domain =
            af_runtime::task::code::CodeTaskDomain::captured(&cas, &code_id, graph).unwrap();
        let mut goal_task = task.clone();
        goal_task.acceptance.retain(|name, _| name == "verified");
        let mut goal_result = result.clone();
        goal_result
            .evidence
            .retain(|id| cas.get_artifact(id).unwrap().artifact_type == VERIFICATION_RESULT_V1);
        terminal_evidence_regressions(&cas, &code_domain, &goal_task, &goal_result);
    }
    let review: TaskReviewRoundV1 = serde_json::from_value(
        cas.get_json(&result.outputs["review"].artifact_ids[0])
            .unwrap()["payload"]
            .clone(),
    )
    .unwrap();
    // Nothing is sealed back: the declared source Snapshot still holds exactly its captured files.
    let (_, sealed) = read_snapshot(&cas, &snapshot).unwrap();
    assert_eq!(sealed, manifest);
    let reader = EventStore::open_read_only(directory.path().join("events.sqlite")).unwrap();
    let mut diagnostics = Vec::new();
    for event in reader
        .replay(&af_store::store::task::task_run_id(&task.task_id).unwrap())
        .unwrap()
    {
        let Ok(transition) = af_store::store::task::read_task_transition(&event) else {
            continue;
        };
        let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
            continue;
        };
        let record = af_store::store::task::execution::read_execution_record(&cas, &record_id)
            .unwrap()
            .record;
        if let TaskExecutionRecordV1::Settled {
            result: TaskAttemptResultV1::Failed { diagnostic_id, .. },
            ..
        } = record
        {
            let diagnostic = cas.get_json(&diagnostic_id).unwrap();
            diagnostics.push(diagnostic["error"].as_str().unwrap().to_owned());
        }
    }
    let verification = result.outputs.get("verification").map(|port| {
        serde_json::from_value(cas.get_json(&port.artifact_ids[0]).unwrap()["payload"].clone())
            .unwrap()
    });
    Outcome {
        result,
        review,
        verification,
        diagnostics,
        calls: model.calls.load(Ordering::SeqCst),
    }
}

/// Rebuild content-addressed descendants to challenge terminal validation itself, after a
/// valid implementation has already passed admission. Keep the first evidence input valid.
fn terminal_evidence_regressions(
    cas: &Cas,
    domain: &af_runtime::task::code::CodeTaskDomain,
    task: &TaskRevisionV1,
    result: &TaskResultV1,
) {
    fn read(cas: &Cas, id: &str) -> af_contracts::ArtifactEnvelope {
        serde_json::from_value(cas.get_json(id).unwrap()).unwrap()
    }
    fn write(cas: &Cas, artifact: af_contracts::ArtifactEnvelope) -> String {
        cas.put_artifact(
            &artifact.artifact_type,
            artifact.producer,
            artifact.input_artifacts,
            artifact.subject_snapshot_id,
            artifact.payload,
        )
        .unwrap()
        .0
    }
    domain.validate_result(cas, task, result).unwrap();
    let verification_id = &result.outputs["verification"].artifact_ids[0];
    let verification = read(cas, verification_id);
    let evaluation_id = verification.payload["evaluation_id"].as_str().unwrap();
    let evaluation = read(cas, evaluation_id);
    let context_id = evaluation
        .input_artifacts
        .iter()
        .find(|id| read(cas, id).artifact_type == af_runner::task::TASK_CONTEXT_V1)
        .unwrap();
    let context = read(cas, context_id);
    let invocation: af_contracts::task::execution::TaskInvocationV1 =
        serde_json::from_value(context.payload["invocation"].clone()).unwrap();
    let primary = &invocation.inputs["testing"].artifact_ids[0];
    let secondary = &invocation.inputs["secondary"].artifact_ids[0];
    assert_ne!(primary, secondary);
    assert!(evaluation.input_artifacts.contains(primary));
    assert!(evaluation.input_artifacts.contains(secondary));
    for (case, expected) in [
        ("failed", "Acceptance cannot pass"),
        ("unavailable", "Acceptance cannot pass"),
        ("stale_payload", "Acceptance cannot pass"),
        ("stale_envelope", "stale type or Snapshot"),
        ("wrong_type", "stale type or Snapshot"),
        ("wrong_cardinality", "stale type or Snapshot"),
        (
            "missing_port",
            "Missing required executable-review evidence",
        ),
        ("many", "Task input requires an exact artifact ID"),
        ("lost_retained", "lost its exact testing input"),
        ("missing_context", "one exact recorded Worker context"),
        ("foreign_context", "another invocation"),
    ] {
        let mut context = context.clone();
        let mut evaluation = evaluation.clone();
        let mut invocation = invocation.clone();
        if matches!(
            case,
            "failed" | "unavailable" | "stale_payload" | "stale_envelope" | "wrong_type"
        ) {
            let mut evidence = read(cas, secondary);
            match case {
                "failed" => {
                    evidence.payload["status"] = json!("failed");
                    evidence.payload["commands"][2]["exit_code"] = json!(1);
                }
                "unavailable" => {
                    evidence.payload["status"] = json!("unavailable");
                    evidence.payload["commands"] = json!([]);
                }
                "stale_payload" => {
                    evidence.payload["snapshot_id"] = json!(format!("sha256:{}", "f".repeat(64)))
                }
                "stale_envelope" => {
                    evidence.subject_snapshot_id = Some(format!("sha256:{}", "f".repeat(64)))
                }
                "wrong_type" => evidence.artifact_type = "af/Requirements@1".into(),
                _ => unreachable!(),
            }
            let changed = write(cas, evidence);
            invocation.inputs.get_mut("secondary").unwrap().artifact_ids = vec![changed.clone()];
            evaluation.input_artifacts.retain(|id| id != secondary);
            evaluation.input_artifacts.push(changed);
        }
        match case {
            "missing_port" => {
                invocation.inputs.remove("secondary");
            }
            "many" => invocation
                .inputs
                .get_mut("secondary")
                .unwrap()
                .artifact_ids
                .push(primary.clone()),
            "lost_retained" => evaluation.input_artifacts.retain(|id| id != secondary),
            "wrong_cardinality" => {
                invocation.inputs.get_mut("secondary").unwrap().cardinality = PortCardinality::Many
            }
            "foreign_context" => invocation.node = "foreign".into(),
            _ => {}
        }
        context.payload["invocation"] = serde_json::to_value(invocation).unwrap();
        let changed_context = write(cas, context);
        evaluation.input_artifacts.retain(|id| id != context_id);
        if case != "missing_context" {
            evaluation.input_artifacts.push(changed_context);
        }
        let changed_evaluation = write(cas, evaluation);
        let mut verification = verification.clone();
        verification.payload["evaluation_id"] = json!(changed_evaluation);
        verification
            .input_artifacts
            .retain(|id| id != evaluation_id);
        verification.input_artifacts.push(changed_evaluation);
        let changed_verification = write(cas, verification);
        let mut challenged = result.clone();
        for output in challenged.outputs.values_mut() {
            for id in &mut output.artifact_ids {
                if id == verification_id {
                    *id = changed_verification.clone();
                }
            }
        }
        if challenged.evidence.remove(verification_id) {
            challenged.evidence.insert(changed_verification);
        }
        let error = domain.validate_result(cas, task, &challenged).unwrap_err();
        assert!(error.contains(expected), "{case}: {error}");
    }
}

fn bare_signature(effects: &str, roles: &str) -> OperatorSignature {
    OperatorSignature {
        unbound_allowed: Default::default(),
        contract: PipelineContractV1 {
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        },
        effects: effects.split_whitespace().map(str::to_owned).collect(),
        evidence: BTreeMap::new(),
        retains: BTreeMap::new(),
        roles: roles.split_whitespace().map(str::to_owned).collect(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: None,
        attempt: None,
    }
}

#[test]
fn adapter_flags_derive_from_the_captured_effects_alone() {
    use af_runner_claude::task::task_tools;
    use af_runner_codex::task::task_sandbox_mode;
    let reviewer = worker_access(&bare_signature("read-source execute-checks", "review"));
    assert_eq!(reviewer, WorkerAccess::ExecuteChecks);
    assert_eq!(task_tools(reviewer), "Read,Glob,Grep,Bash");
    assert_eq!(task_sandbox_mode(reviewer), "workspace-write");
    // A report author that reads source gets the same shell and clone (ADR-0133).
    let author = worker_access(&bare_signature("read-source execute-checks", "author"));
    assert_eq!(author, WorkerAccess::ExecuteChecks);
    assert_eq!(task_tools(author), "Read,Glob,Grep,Bash");
    assert_eq!(task_sandbox_mode(author), "workspace-write");
    // The same declaration under any other non-writing role, and a reviewer declaring only
    // `read-source`, keep `Read,Glob,Grep` and a read-only sandbox.
    for (effects, roles) in [
        ("read-source execute-checks", "implement"),
        ("read-source execute-checks", "evaluate"),
        ("read-source", "review"),
    ] {
        let access = worker_access(&bare_signature(effects, roles));
        assert_eq!(access, WorkerAccess::ReadOnly);
        assert_eq!(task_tools(access), "Read,Glob,Grep");
        assert_eq!(task_sandbox_mode(access), "read-only");
    }
    let writer = worker_access(&bare_signature("write-source", "implement"));
    assert_eq!(writer, WorkerAccess::WriteSource);
    assert_eq!(task_tools(writer), "Read,Glob,Grep,Edit,Write");
    assert_eq!(task_sandbox_mode(writer), "workspace-write");
    // A writer that also declares `execute-checks` gets a shell to format, lint and test.
    let shell = worker_access(&bare_signature("write-source execute-checks", "implement"));
    assert_eq!(shell, WorkerAccess::WriteSourceWithShell);
    assert_eq!(task_tools(shell), "Read,Glob,Grep,Edit,Write,Bash");
    assert_eq!(task_sandbox_mode(shell), "workspace-write");
}

#[test]
fn execute_checks_reviewer_builds_in_an_ephemeral_clone_and_its_source_seals_unchanged() {
    // Scratch under a new `target/`, a file added beside the source, and a dotfile a tool wrote
    // into `HOME` at the sandbox root are all discarded with the clone.
    for case in ["scratch", "add", "dotfile"] {
        let outcome = run(case);
        assert_eq!(outcome.calls, 1, "{case}");
        assert!(
            outcome.diagnostics.is_empty(),
            "{case}: {:?}",
            outcome.diagnostics
        );
        assert_eq!(outcome.result.acceptance, TaskAcceptanceV1::Satisfied);
        assert_eq!(outcome.review.conclusion, ReviewConclusionV1::Pass);
        assert!(outcome.review.missing_reviewers.is_empty());
        assert_eq!(outcome.review.selected_results.len(), 1);
    }
}

#[test]
fn a_source_edit_by_an_execute_checks_reviewer_fails_its_attempt_naming_the_paths() {
    // An addition under a top-level name the source holds is an edit of the declared source.
    for (case, path) in [
        ("edit", "lib.rs"),
        ("delete", "lib.rs"),
        ("inside", "src/extra.rs"),
    ] {
        let outcome = run(case);
        assert_eq!(outcome.calls, 1, "{case}");
        assert_eq!(
            outcome.diagnostics.len(),
            1,
            "{case}: {:?}",
            outcome.diagnostics
        );
        let diagnostic = &outcome.diagnostics[0];
        assert!(
            diagnostic.contains("Execute-checks reviewer changed its declared source"),
            "{case}: {diagnostic}"
        );
        assert!(diagnostic.contains(path), "{case}: {diagnostic}");
        // Scratch beneath the new `target/` directory is never named as a source edit.
        assert!(!diagnostic.contains("target/"), "{case}: {diagnostic}");
        assert_eq!(outcome.result.acceptance, TaskAcceptanceV1::Inconclusive);
        assert_eq!(outcome.review.conclusion, ReviewConclusionV1::Incomplete);
        assert_eq!(
            outcome.review.missing_reviewers,
            BTreeSet::from(["uix".into()])
        );
    }
}

#[test]
fn executable_review_evidence_drives_implementation_acceptance_and_terminal_revalidation() {
    for case in [
        "evidence_passed",
        "evidence_failed",
        "evidence_unavailable",
        "evidence_stale",
        "evidence_missing",
        "evidence_edit",
    ] {
        let outcome = run(case);
        if case == "evidence_passed" {
            assert_eq!(outcome.calls, 2);
            assert_eq!(outcome.result.acceptance, TaskAcceptanceV1::Satisfied);
            assert!(outcome.result.outputs.contains_key("verification"));
            assert_eq!(
                outcome.verification.as_ref().unwrap().outcome,
                ReceiptOutcomeV1::Passed
            );
            assert!(outcome.result.missing_obligations.is_empty());
            assert!(outcome.diagnostics.is_empty(), "{:?}", outcome.diagnostics);
        } else {
            assert!(!outcome.diagnostics.is_empty(), "{case}");
            assert!(
                outcome.result.missing_obligations.contains("verified"),
                "{case}"
            );
            if let Some(verification) = &outcome.verification {
                assert_ne!(verification.outcome, ReceiptOutcomeV1::Passed, "{case}");
            }
            assert_ne!(
                outcome.result.acceptance,
                TaskAcceptanceV1::Satisfied,
                "{case}"
            );

            if matches!(
                case,
                "evidence_stale" | "evidence_missing" | "evidence_edit"
            ) {
                assert_eq!(outcome.calls, 1, "{case}");
            }
        }
    }
}
