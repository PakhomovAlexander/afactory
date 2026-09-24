//! A review Worker declaring `execute-checks` runs in an ephemeral-write clone of its source,
//! with the adapter access the same captured effects derive, and seals nothing back. The
//! reviewer is an in-process model adapter; no Provider or Worker command runs, and the only
//! process is the fixture's `/usr/bin/true` check Gate the reviewer waits on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_core::task::event::TaskChangeV1;
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::review::*;
use review_core::task::usage::TaskTokenUsageV3;
use review_core::task::verification::TASK_CHECK_RECEIPT_V1;
use review_core::task::*;
use review_core::{DemandRequirement, PortCardinality, Producer, Severity};
use review_graph::task::{OperatorAttemptCost, OperatorSignature};
use review_pipeline::task::code::{CodeTaskPolicy, code_signatures};
use review_pipeline::task::host::*;
use review_pipeline::task::review::*;
use review_pipeline::task::source::{SnapshotTaskEnvironment, worker_access};
use review_pipeline::task::*;
use review_runner::task::{ModelWorkerReturn, WorkerAccess, WorkerModelAdapter};
use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, read_snapshot, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore};
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
            "edit" => Some(("lib.rs", "pub fn changed() {}\n")),
            // Added anywhere, root dotfile included: the clone is discarded, never a source edit.
            "add" => Some(("src/extra.rs", "pub fn extra() {}\n")),
            "dotfile" => Some((".claude.json", "{}\n")),
            _ => None,
        };
        if let Some((path, text)) = edited {
            std::fs::write(workdir.join(path), text).unwrap();
        }
        if self.case == "delete" {
            std::fs::remove_file(workdir.join("lib.rs")).unwrap();
        }
        let reply = json!({"schema":"af.worker-reply/1",
            "outputs":{"result":[{"reports":[],"benchmark_demands":[],"dispositions":[]}]}});
        let reply = serde_json::to_vec(&reply).unwrap();
        ModelWorkerReturn {
            usage_observation: None,
            raw_artifact_ids: vec![cas.put(&reply).unwrap()],
            message: Ok(reply),
            usage: Some(TaskTokenUsageV3::charge_only(1)),
        }
    }
}

struct Outcome {
    result: TaskResultV1,
    review: TaskReviewRoundV1,
    /// Every failed Attempt's recorded diagnostic, in log order.
    diagnostics: Vec<String>,
    calls: usize,
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
            review_check::CheckDefinition::new(
                "syntax",
                review_core::Command::new("/usr/bin/true", vec![]),
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
        reviewers: BTreeMap::from([("uix".into(), DemandRequirement::Required)]),
        gate: Severity::Major,
        clean_rounds: 1,
        max_rounds: 1,
    };
    let policy_id = cas
        .put_json(&serde_json::to_value(&policy).unwrap())
        .unwrap();
    let entries: Vec<Entry> = [("lib.rs", LIBRARY), ("src/main.rs", MAIN)]
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
    let pipeline = PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
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
    let packages: [(String, BTreeMap<String, Vec<u8>>); 2] = [
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
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 60_000;
    let task:TaskRevisionV1=serde_json::from_value(json!({"task_id":format!("uix-{case}"),"revision":1,"kind":"review","goal":"Review the captured source by running it",
        "inputs":{"source":source,"history":history},"required_outputs":{"review":{"artifact_type":TASK_REVIEW_ROUND_V1,"cardinality":"one"},"history":{"artifact_type":REVIEW_HISTORY_V1,"cardinality":"one"}},
        "acceptance":{"reviewed":{"evidence_type":TASK_REVIEW_ROUND_V1,"verifier_policy":policy_id}},"provenance":{"adapter_id":origin,"input_artifact_ids":[]},
        "authority":{"policy_id":policy_id,"allowed_effects":["execute-checks","read-source"],"data_destinations":[]},
        "limits":{"tokens":200000,"max_attempts":3,"deadline_unix_ms":deadline,"verification":{"tokens":100000,"attempts":3,"wall_ms":15000}},
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
    // The catalog admits the review Worker's `execute-checks` declaration as captured bytes.
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
    }
    compiler
        .bind_worker(
            "fixture/uix",
            AdmittedWorkerSettings {
                execution: WorkerExecutionV1::Model {
                    provider: "personal".into(),
                    provider_kind: "fixture".into(),
                    principal_id: policy_id.clone(),
                    model: "uix-model".into(),
                    effort: "high".into(),
                },
                invocation_policy_id: policy_id.clone(),
            },
        )
        .unwrap();
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
        graph,
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
    let review: TaskReviewRoundV1 = serde_json::from_value(
        cas.get_json(&result.outputs["review"].artifact_ids[0])
            .unwrap()["payload"]
            .clone(),
    )
    .unwrap();
    // Nothing is sealed back: the declared source Snapshot still holds exactly its two files.
    let (_, sealed) = read_snapshot(&cas, &snapshot).unwrap();
    assert_eq!(sealed, manifest);
    let reader = EventStore::open_read_only(directory.path().join("events.sqlite")).unwrap();
    let mut diagnostics = Vec::new();
    for event in reader
        .replay(&review_store::store::task::task_run_id(&task.task_id).unwrap())
        .unwrap()
    {
        let Ok(transition) = review_store::store::task::read_task_transition(&event) else {
            continue;
        };
        let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
            continue;
        };
        let record = review_store::store::task::execution::read_execution_record(&cas, &record_id)
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
    Outcome {
        result,
        review,
        diagnostics,
        calls: model.calls.load(Ordering::SeqCst),
    }
}

fn bare_signature(effects: &str, roles: &str) -> OperatorSignature {
    OperatorSignature {
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
    use review_runner_claude::task::task_tools;
    use review_runner_codex::task::task_sandbox_mode;
    let reviewer = worker_access(&bare_signature("read-source execute-checks", "review"));
    assert_eq!(reviewer, WorkerAccess::ExecuteChecks);
    assert_eq!(task_tools(reviewer), "Read,Glob,Grep,Bash");
    assert_eq!(task_sandbox_mode(reviewer), "workspace-write");
    // The same declaration without the review role, and a reviewer declaring only
    // `read-source`, keep `Read,Glob,Grep` and a read-only sandbox.
    for (effects, roles) in [
        ("read-source execute-checks", "author"),
        ("read-source", "review"),
    ] {
        let access = worker_access(&bare_signature(effects, roles));
        assert_eq!(access, WorkerAccess::ReadOnly);
        assert_eq!(task_tools(access), "Read,Glob,Grep");
        assert_eq!(task_sandbox_mode(access), "read-only");
    }
    let writer = worker_access(&bare_signature("write-source execute-checks", "review"));
    assert_eq!(task_tools(writer), "Read,Glob,Grep,Edit,Write");
    assert_eq!(task_sandbox_mode(writer), "workspace-write");
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
    for (case, path) in [("edit", "lib.rs"), ("delete", "lib.rs")] {
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
