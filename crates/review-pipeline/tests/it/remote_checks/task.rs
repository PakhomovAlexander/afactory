//! Remote Checks through the code check operator of a real Task: selection by the pipeline's
//! check node, the plan authority that says so, the push target read again at run time, local
//! checks first, the recorded evidence and result shape, and the reader.

use super::*;
use review_check::{CheckDefinition, CheckResult, CheckStatus};
use review_config::task::catalog::*;
use review_core::PortCardinality;
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::plan::IndependencePolicyV1;
use review_core::task::remote_check::{
    PUBLISH_GATE_EFFECT, REMOTE_CHECK_EVIDENCE_V1, RemoteCheckEvidenceV1,
};
use review_core::task::verification::*;
use review_core::task::*;
use review_core::{Arg, Command};
use review_graph::task::CompiledTask;
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::code::{CodeTaskDomain, CodeTaskPolicy, code_signatures};
use review_pipeline::task::host::*;
use review_pipeline::task::remote_check::RemoteCheckHost;
use review_pipeline::task::source::SnapshotTaskEnvironment;
use review_source_git::task::{SOURCE_TREE_V1, source_tree};
use review_store::EventStore;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn shell(script: &str) -> Command {
    Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)])
}

/// A local `fmt` check and a `kernel` check whose remote table is declared or not.
pub fn policy(fmt_passes: bool, remote: bool) -> CodeTaskPolicy {
    let kernel = CheckDefinition {
        remote: remote.then(|| RemoteCheckV1 {
            executor: RemoteExecutorV1::GithubPr,
            workflow: ".github/workflows/ci.yml".into(),
            required: vec![LINT.into(), CHECK.into()],
        }),
        ..CheckDefinition::new("kernel", shell("exit 0"))
    };
    CodeTaskPolicy {
        schema: "af.code-task-policy/1".into(),
        checks: BTreeMap::from([
            (
                "fmt".into(),
                CheckDefinition::new(
                    "fmt",
                    shell(if fmt_passes {
                        "exit 0"
                    } else {
                        "echo 'src/lib.txt is not formatted' >&2; exit 1"
                    }),
                ),
            ),
            ("kernel".into(), kernel),
        ]),
        check_wall_ms: 60_000,
        check_process_wall_ms: Some(30_000),
        require_container: false,
        warm: None,
        measures: BTreeMap::new(),
        objectives: BTreeMap::new(),
        rust_toolchain: None,
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

fn from(node: &str, port: &str) -> ValueRefV1 {
    ValueRefV1::Node {
        node: node.into(),
        port: port.into(),
    }
}

/// The fixture pipeline's name; a parent that calls it is `PARENT`.
pub const PIPELINE: &str = "fixture/remote-checks";
pub const PARENT: &str = "fixture/remote-checks-parent";

fn contract() -> PipelineContractV1 {
    let derived = PortAffinityV1::SameAs {
        input: "source".into(),
    };
    let mut verification = port(VERIFICATION_RESULT_V1, derived.clone());
    verification.covers.insert("verified".into());
    PipelineContractV1 {
        inputs: BTreeMap::from([(
            "source".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
        )]),
        outputs: BTreeMap::from([
            ("snapshot".into(), port(SOURCE_TREE_V1, derived)),
            ("verification".into(), verification),
        ]),
    }
}

/// A Pipeline of one Check node over the Task's source — `checks` run here, `remote` by their
/// declared executor — and the accept node that reads it.
pub fn pipeline(checks: &[&str], remote: &[&str]) -> PipelineDefinitionV1 {
    let node = |id: &str, operator, inputs| TaskNodeV1 {
        id: id.into(),
        operator,
        inputs,
        when: None,
    };
    let source = ValueRefV1::Input {
        port: "source".into(),
    };
    PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
        name: PIPELINE.into(),
        version: "1.0.0".into(),
        contract: contract(),
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from(["implement".into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::new(),
        nodes: vec![
            node(
                "check",
                TaskOperatorV1::Check {
                    checks: names(checks),
                    remote_checks: names(remote),
                },
                BTreeMap::from([("source".into(), source.clone())]),
            ),
            node(
                "accept",
                TaskOperatorV1::Accept {},
                BTreeMap::from([
                    ("source".into(), source),
                    ("checks".into(), from("check", "result")),
                ]),
            ),
        ],
        outputs: BTreeMap::from([
            ("snapshot".into(), from("accept", "snapshot")),
            ("verification".into(), from("accept", "result")),
        ]),
        coverage: BTreeMap::from([("verified".into(), from("accept", "result"))]),
        max_attempts: 3,
        max_parallel: 1,
    }
}

/// A parent Pipeline whose only node calls `PIPELINE`, so its check node is a child's.
pub fn parent() -> PipelineDefinitionV1 {
    PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
        name: PARENT.into(),
        version: "1.0.0".into(),
        contract: contract(),
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from(["implement".into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::new(),
        nodes: vec![TaskNodeV1 {
            id: "gate".into(),
            operator: TaskOperatorV1::Call {
                pipeline: PIPELINE.into(),
                bindings: BTreeMap::new(),
            },
            inputs: BTreeMap::from([(
                "source".into(),
                ValueRefV1::Input {
                    port: "source".into(),
                },
            )]),
            when: None,
        }],
        outputs: BTreeMap::from([
            ("snapshot".into(), from("gate", "snapshot")),
            ("verification".into(), from("gate", "verification")),
        ]),
        coverage: BTreeMap::from([("verified".into(), from("gate", "verification"))]),
        max_attempts: 3,
        max_parallel: 1,
    }
}

/// The recorded destination of the fixture's remote plans: the coordinator adds it, with
/// `publish-gate`, from the mapping's target when the graph has remote checks.
pub const DESTINATION: &str = "github:octo/gate";

/// One captured Task, planned but not yet run: what the coordinator holds after `af task plan`.
pub struct Planned {
    pub directory: tempfile::TempDir,
    pub cas: Cas,
    pub policy_id: String,
    pub candidate: String,
    pub task: TaskRevisionV1,
    pub compiler: TaskPlanCompiler,
    pub revision: String,
}

impl Planned {
    /// Capture `pipelines` and the Task over a candidate derived from the fixture source,
    /// with the authority `destination` describes: `None` is the authority of a Task whose
    /// graph has no remote check.
    pub fn new(
        policy: &CodeTaskPolicy,
        pipelines: &[PipelineDefinitionV1],
        destination: Option<&str>,
    ) -> Result<Planned, String> {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let policy_id = cas
            .put_json(&serde_json::to_value(policy).unwrap())
            .unwrap();
        let snapshots = Snapshots::new(&cas);
        let (candidate, _) = snapshots.candidate(&cas, "version 2\n");
        let source = source_tree(&cas, producer(), &candidate, vec![]).unwrap();
        let mut authority = TaskAuthorityV1 {
            policy_id: policy_id.clone(),
            allowed_effects: BTreeSet::from(["read-source".into(), "execute-checks".into()]),
            data_destinations: BTreeSet::new(),
        };
        if let Some(destination) = destination {
            authority.allowed_effects.insert(PUBLISH_GATE_EFFECT.into());
            authority.data_destinations.insert(destination.into());
        }
        let task = TaskRevisionV1 {
            previous_revision_id: None,
            task_id: TASK.into(),
            revision: 1,
            kind: "implement".into(),
            goal: "Hold a remote Gate".into(),
            inputs: BTreeMap::from([("source".into(), source)]),
            required_outputs: serde_json::from_value(json!({
                "snapshot": {"artifact_type": SOURCE_TREE_V1, "cardinality": "one"},
                "verification": {"artifact_type": VERIFICATION_RESULT_V1, "cardinality": "one"}}))
            .unwrap(),
            acceptance: serde_json::from_value(json!({"verified": {
                "evidence_type": VERIFICATION_RESULT_V1, "verifier_policy": policy_id}}))
            .unwrap(),
            provenance: TaskProvenanceV1 {
                adapter_id: policy_id.clone(),
                input_artifact_ids: vec![],
            },
            authority,
            limits: TaskLimitsV1 {
                tokens: 1000,
                max_attempts: 3,
                deadline_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
                    + 180_000,
                verification: VerificationReserveV1 {
                    tokens: 0,
                    attempts: 2,
                    wall_ms: 120_000,
                },
            },
            strategy: "small".into(),
            pipeline: serde_json::from_value(
                json!({"name": pipelines[0].name, "fallback": "refuse"}),
            )
            .unwrap(),
            facts: BTreeMap::new(),
        };
        let mut compiler = TaskPlanCompiler::new(
            policy_id.clone(),
            policy_id.clone(),
            code_signatures(&policy_id, policy).unwrap(),
            BTreeMap::from([("verified".into(), "snapshot".into())]),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        for pipeline in pipelines {
            let files = BTreeMap::from([(
                "pipeline.toml".to_string(),
                toml::to_string(pipeline).unwrap().into_bytes(),
            )]);
            let pin = TaskPackagePin {
                version: "1.0.0".into(),
                path: "package".into(),
                digest: review_config::lock::package_digest_from_files(&files),
            };
            compiler.capture_package(
                &cas,
                &pipeline.name,
                &pin,
                &files
                    .into_iter()
                    .map(|(path, bytes)| (format!("package/{path}"), bytes))
                    .collect(),
            )?;
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
        Ok(Planned {
            directory,
            cas,
            policy_id,
            candidate,
            task,
            compiler,
            revision,
        })
    }

    /// The plan of the root pipeline, as the catalog compiler captures it.
    pub fn compile(&self) -> Result<(ExecutionPlanV1, CompiledTask), String> {
        self.compiler.compile(
            &self.cas,
            &self.revision,
            &self.task.pipeline.as_ref().unwrap().name,
        )
    }
}

/// One recorded Task: its Store directory, CAS, domain, policy identity and check receipt.
pub struct Recorded {
    pub directory: tempfile::TempDir,
    pub cas: Cas,
    pub domain: CodeTaskDomain,
    pub policy_id: String,
    pub plan: ExecutionPlanV1,
    pub receipt: Option<TaskCheckReceiptV1>,
    pub candidate: String,
}

impl Recorded {
    pub fn receipt(&self) -> &TaskCheckReceiptV1 {
        self.receipt
            .as_ref()
            .expect("the check node recorded a receipt")
    }

    pub fn result(&self, name: &str) -> (String, CheckResult) {
        let id = self.receipt().checks[name].clone();
        let result = serde_json::from_value(self.cas.get_json(&id).unwrap()).unwrap();
        (id, result)
    }

    pub fn evidence(&self, name: &str) -> RemoteCheckEvidenceV1 {
        let (_, result) = self.result(name);
        let artifact = self
            .cas
            .get_artifact(result.remote.as_ref().unwrap())
            .unwrap();
        assert_eq!(artifact.artifact_type, REMOTE_CHECK_EVIDENCE_V1);
        assert_eq!(artifact.subject_snapshot_id.as_ref(), Some(&self.candidate));
        serde_json::from_value(artifact.payload).unwrap()
    }
}

/// Run one Task named `TASK` whose check node runs `checks` here and `remote` remotely, with
/// the machine-local configuration `host` builds from the Task's directory.
pub fn run(
    policy: &CodeTaskPolicy,
    checks: &[&str],
    remote: &[&str],
    host: impl FnOnce(&Path) -> RemoteCheckHost,
) -> Recorded {
    let destination = (!remote.is_empty()).then_some(DESTINATION);
    run_planned(
        Planned::new(policy, &[pipeline(checks, remote)], destination).unwrap(),
        host,
    )
}

/// Execute a planned Task to its end and read back its one check receipt.
pub fn run_planned(planned: Planned, host: impl FnOnce(&Path) -> RemoteCheckHost) -> Recorded {
    let Planned {
        directory,
        cas,
        policy_id,
        candidate,
        task,
        compiler,
        revision,
    } = planned;
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let pipeline_name = task.pipeline.as_ref().unwrap().name.clone();
    let (plan, graph) = compiler.compile(&cas, &revision, &pipeline_name).unwrap();
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
    let domain = CodeTaskDomain::captured(&cas, &policy_id, graph.clone())
        .unwrap()
        .with_remote_checks(host(directory.path()));
    let policy: CodeTaskPolicy = serde_json::from_value(cas.get_json(&policy_id).unwrap()).unwrap();
    let environment = SnapshotTaskEnvironment {
        policy: policy.isolation(),
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
        .open_task(&cas, &revision, "test-writer", 60_000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut store, &cas, lease, &authority, &host).unwrap();
    runtime.execute().unwrap();
    let state = runtime.projection().unwrap();
    let receipt = state
        .execution
        .as_ref()
        .unwrap()
        .outputs
        .iter()
        .find(|(node, _)| node.ends_with(".nodes.check"))
        .map(|(_, (_, output))| {
            let id = &output.outputs["result"].artifact_ids[0];
            serde_json::from_value(cas.get_artifact(id).unwrap().payload).unwrap()
        });
    drop(runtime);
    drop(host);
    Recorded {
        directory,
        cas,
        domain,
        policy_id,
        plan,
        receipt,
        candidate,
    }
}

/// The operator's configuration with a mapping file in the operator's own directory, never
/// inside the Store.
fn mapped(remote: &Remote, text: String) -> impl FnOnce(&Path) -> RemoteCheckHost + '_ {
    move |_: &Path| {
        let config = remote
            .directory
            .path()
            .canonicalize()
            .unwrap()
            .join("config");
        std::fs::create_dir_all(&config).unwrap();
        let mapping = config.join("remote-checks.toml");
        std::fs::write(&mapping, text).unwrap();
        RemoteCheckHost {
            mapping: Some(mapping),
            owner: Some(Arc::new(|_: &str| Ok(OWNER.to_string()))),
            github_pr: remote.settings(),
        }
    }
}

#[test]
fn a_node_without_remote_checks_runs_every_check_locally_whatever_the_mapping_holds() {
    let plain = run(&policy(true, false), &["fmt", "kernel"], &[], |_| {
        RemoteCheckHost::default()
    });
    let remote = Remote::new();
    for host in [
        Box::new(|_: &Path| RemoteCheckHost::default())
            as Box<dyn FnOnce(&Path) -> RemoteCheckHost>,
        // A configured but absent mapping file.
        Box::new(|directory: &Path| RemoteCheckHost {
            mapping: Some(directory.canonicalize().unwrap().join("absent.toml")),
            owner: Some(Arc::new(|_: &str| Ok(OWNER.to_string()))),
            github_pr: Default::default(),
        }),
        // A mapping with a target for this very repository selects nothing.
        Box::new(mapped(&remote, remote.mapping_text())),
        // Nor does a mapping that is not even valid: a node without remote checks never reads it.
        Box::new(mapped(
            &remote,
            format!("{}checks = [\"kernel\"]\n", remote.mapping_text()),
        )),
    ] {
        let declared = run(&policy(true, true), &["fmt", "kernel"], &[], host);
        assert_ne!(declared.policy_id, plain.policy_id);
        assert_eq!(
            declared.receipt().checks,
            plain.receipt().checks,
            "byte-identical check results"
        );
        assert_eq!(declared.receipt().outcome, ReceiptOutcomeV1::Passed);
        assert_eq!(declared.receipt().outcome, plain.receipt().outcome);
        assert_eq!(declared.receipt().snapshot_id, plain.receipt().snapshot_id);
        assert_eq!(declared.plan.authority.data_destinations, BTreeSet::new());
        assert!(
            !declared
                .plan
                .authority
                .allowed_effects
                .contains(PUBLISH_GATE_EFFECT)
        );
        let (_, kernel) = declared.result("kernel");
        assert_eq!(kernel.remote, None);
        assert_eq!(kernel.program.as_deref(), Some("/bin/sh"));
    }
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
    // A policy without the table is captured exactly as before it existed, and so is a check
    // node without `remote_checks`.
    let value = serde_json::to_value(policy(true, false)).unwrap();
    assert!(value["checks"]["kernel"].get("remote").is_none());
    let node = serde_json::to_value(&pipeline(&["fmt", "kernel"], &[]).nodes[0].operator).unwrap();
    assert_eq!(node, json!({"op": "check", "checks": ["fmt", "kernel"]}));
}

#[test]
fn a_remote_check_the_pipeline_lists_runs_after_the_local_checks_and_passes() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    // The local check fails if `gh` was asked anything before it ran. It reads the fake's call
    // log through a link of its own, so the captured policy never names the remote's directory.
    let probe = tempfile::tempdir().unwrap();
    let calls = probe.path().canonicalize().unwrap().join("gh-calls");
    std::os::unix::fs::symlink(remote.state.join("calls.log"), &calls).unwrap();
    let mut ordered = policy(true, true);
    ordered.checks.get_mut("fmt").unwrap().command = shell(&format!(
        "if [ -s '{}' ]; then echo 'the remote phase ran first' >&2; exit 1; fi",
        calls.display()
    ));
    let recorded = run(
        &ordered,
        &["fmt"],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    let receipt = recorded.receipt();
    assert_eq!(receipt.outcome, ReceiptOutcomeV1::Passed);
    assert_eq!(
        receipt.checks.keys().collect::<Vec<_>>(),
        ["fmt", "kernel"],
        "the receipt names every check of both lists"
    );
    let (_, fmt) = recorded.result("fmt");
    assert_eq!(
        (fmt.status, fmt.remote.as_ref()),
        (CheckStatus::Passed, None)
    );
    assert!(remote.calls().contains("auth"), "then the remote phase ran");
    let (_, kernel) = recorded.result("kernel");
    assert_eq!(kernel.status, CheckStatus::Passed);
    assert!(kernel.has_one_shape() && kernel.program.is_none() && kernel.args.is_empty());
    let evidence = recorded.evidence("kernel");
    evidence.validate().unwrap();
    assert_eq!(evidence.state, RemoteCheckStateV1::Observed);
    assert_eq!(evidence.snapshot_id, recorded.candidate);
    assert_eq!(evidence.github, "octo/gate");
    assert_eq!(evidence.head_commit, remote.branch("head"));
    assert_eq!(
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, receipt),
        Ok(ReceiptOutcomeV1::Passed)
    );
    // The plan said what it would do: publish to this repository.
    assert!(
        recorded
            .plan
            .authority
            .allowed_effects
            .contains(PUBLISH_GATE_EFFECT)
    );
    assert_eq!(
        recorded.plan.authority.data_destinations,
        BTreeSet::from([DESTINATION.to_string()])
    );
    // No record holds the push URL, the mapping's path or job log text.
    let bytes = store_bytes(recorded.directory.path());
    assert!(!contains(&bytes, &remote.push_url()));
    assert!(!contains(&bytes, remote.directory.path().to_str().unwrap()));
    assert!(
        !remote.calls().contains("/logs"),
        "a passing check asks for no log"
    );
}

#[test]
fn a_node_whose_checks_all_run_remotely_records_no_local_runtime_evidence() {
    use review_core::task::runtime::TASK_RUNTIME_EVIDENCE_V1;
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    let recorded = run(
        &policy(true, true),
        &[],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    assert_eq!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
    assert_eq!(
        recorded.receipt().checks.keys().collect::<Vec<_>>(),
        ["kernel"]
    );
    assert_eq!(
        recorded.evidence("kernel").state,
        RemoteCheckStateV1::Observed
    );
    // It ran nothing on this machine, so there is no span to group.
    assert!(
        recorded
            .cas
            .filed_objects()
            .unwrap()
            .into_iter()
            .filter_map(|object| recorded.cas.get_artifact(&object.digest).ok())
            .all(|artifact| artifact.artifact_type != TASK_RUNTIME_EVIDENCE_V1)
    );
}

#[test]
fn a_child_pipelines_remote_check_runs_remotely_for_its_parent() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    let planned = Planned::new(
        &policy(true, true),
        &[parent(), pipeline(&["fmt"], &["kernel"])],
        Some(DESTINATION),
    )
    .unwrap();
    let (_, graph) = planned.compile().unwrap();
    assert_eq!(
        graph.remote_checks(),
        BTreeMap::from([("root.nodes.gate.nodes.check".into(), names(&["kernel"]))])
    );
    let recorded = run_planned(planned, mapped(&remote, remote.mapping_text()));
    assert_eq!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
    assert_eq!(
        recorded.evidence("kernel").state,
        RemoteCheckStateV1::Observed
    );
    let (_, fmt) = recorded.result("fmt");
    assert_eq!(fmt.remote, None);
}

#[test]
fn a_failed_remote_check_keeps_its_log_excerpt_as_the_results_stdout() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-failure.json");
    remote.serve_log(
        1002,
        b"FAIL [ 1.0s] af::it task_repair\nerror: test run failed\n",
    );
    let recorded = run(
        &policy(true, true),
        &["fmt"],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    let receipt = recorded.receipt();
    assert_eq!(receipt.outcome, ReceiptOutcomeV1::Failed);
    let (_, kernel) = recorded.result("kernel");
    assert_eq!(kernel.status, CheckStatus::Failed);
    assert!(kernel.has_one_shape() && kernel.stderr.is_none() && kernel.exit_code.is_none());
    let log = recorded
        .cas
        .get(kernel.stdout.as_ref().expect("a log excerpt"))
        .unwrap();
    let log = String::from_utf8(log).unwrap();
    assert!(log.starts_with("==> job ") && log.contains("FAIL [ 1.0s] af::it task_repair"));
    assert_eq!(
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, receipt),
        Ok(ReceiptOutcomeV1::Failed)
    );
    // The excerpt is the only place the log text lives: the evidence names jobs and steps.
    assert!(
        !serde_json::to_string(&recorded.evidence("kernel"))
            .unwrap()
            .contains("task_repair")
    );
}

#[test]
fn a_failing_local_check_pushes_nothing() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    let recorded = run(
        &policy(false, true),
        &["fmt"],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    assert_ne!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
    let (_, fmt) = recorded.result("fmt");
    assert_eq!(fmt.status, CheckStatus::Failed);
    let (_, kernel) = recorded.result("kernel");
    assert_eq!(kernel.status, CheckStatus::NotRun);
    let reason = kernel.reason.clone().unwrap();
    assert!(
        reason.starts_with("remote_skipped_local_failed: "),
        "{reason}"
    );
    assert!(reason.contains("`fmt`") && reason.contains("fix the local failure"));
    let evidence = recorded.evidence("kernel");
    assert_eq!(
        (evidence.state, evidence.reason),
        (
            RemoteCheckStateV1::Refused,
            Some(RemoteCheckReasonV1::RemoteSkippedLocalFailed)
        )
    );
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
}

#[test]
fn the_reader_refuses_mixed_and_underived_remote_results() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    let recorded = run(
        &policy(true, true),
        &["fmt"],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    let (_, kernel) = recorded.result("kernel");
    let forge = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value = serde_json::to_value(&kernel).unwrap();
        edit(&mut value);
        let id = recorded.cas.put_json(&value).unwrap();
        let mut receipt = recorded.receipt().clone();
        receipt.checks.insert("kernel".into(), id);
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, &receipt)
    };
    assert_eq!(forge(&|_| {}), Ok(ReceiptOutcomeV1::Passed));
    for (why, edit) in [
        (
            "a program beside the evidence",
            Box::new(|v: &mut serde_json::Value| v["program"] = json!("/bin/sh"))
                as Box<dyn Fn(&mut serde_json::Value)>,
        ),
        ("an exit code", Box::new(|v| v["exit_code"] = json!(0))),
        (
            "arguments",
            Box::new(|v| v["args"] = json!([{"value": "-c", "provenance": "literal"}])),
        ),
        (
            "a status the evidence does not derive",
            Box::new(|v| v["status"] = json!("failed")),
        ),
        (
            "a not-run whose reason is not the evidence's",
            Box::new(|v| {
                v["status"] = json!("not_run");
                v["reason"] = json!("remote_check_missing: invented");
            }),
        ),
        (
            "evidence that is not evidence",
            Box::new(|v| v["remote"] = v["args"].clone()),
        ),
        (
            "a log excerpt on a check that passed",
            Box::new(|v| v["stdout"] = v["remote"].clone()),
        ),
        ("stderr", Box::new(|v| v["stderr"] = v["remote"].clone())),
    ] {
        assert!(forge(&*edit).is_err(), "the reader accepted {why}");
    }
    // Evidence of another declaration is refused, even when its status would derive.
    let mut evidence = recorded.evidence("kernel");
    evidence.workflow = ".github/workflows/other.yml".into();
    evidence.run.as_mut().unwrap().workflow = evidence.workflow.clone();
    let other = recorded
        .cas
        .put_artifact(
            REMOTE_CHECK_EVIDENCE_V1,
            producer(),
            vec![],
            Some(recorded.candidate.clone()),
            serde_json::to_value(&evidence).unwrap(),
        )
        .unwrap()
        .0;
    assert!(forge(&|v| v["remote"] = json!(other)).is_err());
    // Evidence gathered at another repository than the plan's recorded destination is not this
    // plan's, even when everything it says about the Snapshot is right.
    let mut elsewhere = recorded.evidence("kernel");
    assert_eq!(elsewhere.github, "octo/gate");
    elsewhere.github = "octo/elsewhere".into();
    elsewhere.validate().unwrap();
    let elsewhere = recorded
        .cas
        .put_artifact(
            REMOTE_CHECK_EVIDENCE_V1,
            producer(),
            vec![],
            Some(recorded.candidate.clone()),
            serde_json::to_value(&elsewhere).unwrap(),
        )
        .unwrap()
        .0;
    let refused = forge(&|v| v["remote"] = json!(elsewhere)).unwrap_err();
    assert!(refused.contains("another repository"), "{refused}");
    // A remote result for a definition without `remote` is refused like a changed definition.
    let local = run(&policy(true, false), &["fmt", "kernel"], &[], |_| {
        RemoteCheckHost::default()
    });
    let id = local
        .cas
        .put_json(&serde_json::to_value(&kernel).unwrap())
        .unwrap();
    let evidence_id = kernel.remote.clone().unwrap();
    local
        .cas
        .put(&recorded.cas.get(&evidence_id).unwrap())
        .unwrap();
    let mut receipt = local.receipt().clone();
    receipt.checks.insert("kernel".into(), id);
    assert!(
        local
            .domain
            .check_receipt_outcome(&local.cas, &receipt)
            .is_err()
    );
}

#[test]
fn a_mapping_with_user_information_is_refused_before_any_check() {
    let remote = Remote::new();
    let text = remote.mapping_text().replace(
        &remote.push_url(),
        "https://octo:s3cret-token@example.invalid/gate.git",
    );
    let recorded = run(
        &policy(true, true),
        &["fmt"],
        &["kernel"],
        mapped(&remote, text),
    );
    assert!(recorded.receipt.is_none(), "no check ran");
    let bytes = store_bytes(recorded.directory.path());
    assert!(!contains(&bytes, "s3cret-token"));
    // The refusal names the knob, never the mapping's own path.
    assert!(!contains(&bytes, remote.directory.path().to_str().unwrap()));
    assert!(
        contains(&bytes, "user information"),
        "the refusal is recorded"
    );
    assert_eq!(remote.refs(), "");
}

#[test]
fn a_mapping_that_still_selects_checks_is_refused_before_any_check() {
    let remote = Remote::new();
    let recorded = run(
        &policy(true, true),
        &["fmt"],
        &["kernel"],
        mapped(
            &remote,
            format!("{}checks = [\"kernel\"]\n", remote.mapping_text()),
        ),
    );
    assert!(recorded.receipt.is_none(), "no check ran");
    let bytes = store_bytes(recorded.directory.path());
    assert!(contains(&bytes, "carries `checks`"));
    assert!(contains(&bytes, "pipeline's check node"));
    assert!(!contains(&bytes, remote.directory.path().to_str().unwrap()));
    assert_eq!(remote.refs(), "");
    assert_eq!(remote.calls(), "");
}

#[test]
fn a_target_removed_or_changed_after_planning_ends_the_attempt_before_any_push() {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    for (host, expected) in [
        (
            Box::new(|directory: &Path| RemoteCheckHost {
                mapping: Some(directory.canonicalize().unwrap().join("absent.toml")),
                owner: Some(Arc::new(|_: &str| Ok(OWNER.to_string()))),
                github_pr: remote.settings(),
            }) as Box<dyn FnOnce(&Path) -> RemoteCheckHost>,
            "no longer names a push target for repository",
        ),
        (
            Box::new(|_: &Path| RemoteCheckHost {
                mapping: None,
                owner: Some(Arc::new(|_: &str| Ok(OWNER.to_string()))),
                github_pr: remote.settings(),
            }),
            "no longer names a push target for repository",
        ),
        (
            Box::new(mapped(&remote, "version = 1\n".into())),
            "no longer names a push target for repository",
        ),
        (
            Box::new(mapped(
                &remote,
                remote.mapping_text().replace("octo/gate", "octo/elsewhere"),
            )),
            "now names github octo/elsewhere",
        ),
    ] {
        let recorded = run(&policy(true, true), &["fmt"], &["kernel"], host);
        assert!(recorded.receipt.is_none(), "no check ran: {expected}");
        let bytes = store_bytes(recorded.directory.path());
        assert!(contains(&bytes, expected), "{expected}");
        assert!(contains(&bytes, ROOT), "the refusal names the repository");
        assert!(!contains(&bytes, &remote.push_url()));
        assert!(!contains(&bytes, remote.directory.path().to_str().unwrap()));
    }
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
}

#[test]
fn the_plan_refuses_a_remote_check_node_the_policy_or_the_authority_does_not_back() {
    // A name in both lists, and a node that names no check, are refused with the pipeline's
    // package, naming the pipeline, the node and the check.
    for (checks, remote, expected) in [
        (
            &["fmt", "kernel"][..],
            &["kernel"][..],
            "Pipeline fixture/remote-checks node check lists check kernel in both",
        ),
        (
            &[][..],
            &[][..],
            "Pipeline fixture/remote-checks node check names no check",
        ),
    ] {
        let error = Planned::new(
            &policy(true, true),
            &[pipeline(checks, remote)],
            Some(DESTINATION),
        )
        .err()
        .expect("refused at capture");
        assert!(error.contains(expected), "{error}");
    }
    // A remote check the policy declares without a `remote` table, directly or in a child.
    for pipelines in [
        vec![pipeline(&["kernel"], &["fmt"])],
        vec![parent(), pipeline(&["kernel"], &["fmt"])],
    ] {
        let error = Planned::new(&policy(true, true), &pipelines, Some(DESTINATION))
            .unwrap()
            .compile()
            .unwrap_err();
        assert!(
            error.contains("Pipeline fixture/remote-checks node check lists check fmt")
                && error.contains("without a `remote` table"),
            "{error}"
        );
    }
    // A remote check node needs the authority the coordinator adds from the mapping's target;
    // a node without one may not carry it.
    let error = Planned::new(
        &policy(true, true),
        &[pipeline(&["fmt"], &["kernel"])],
        None,
    )
    .unwrap()
    .compile()
    .unwrap_err();
    assert!(error.contains("publish-gate"), "{error}");
    let error = Planned::new(
        &policy(true, true),
        &[pipeline(&["fmt", "kernel"], &[])],
        Some(DESTINATION),
    )
    .unwrap()
    .compile()
    .unwrap_err();
    assert!(error.contains("runs no remote check"), "{error}");
    // A node with only remote checks is a check node like any other.
    let (plan, graph) = Planned::new(
        &policy(true, true),
        &[pipeline(&[], &["kernel"])],
        Some(DESTINATION),
    )
    .unwrap()
    .compile()
    .unwrap();
    assert_eq!(
        graph.remote_checks(),
        BTreeMap::from([("root.nodes.check".into(), names(&["kernel"]))])
    );
    assert!(plan.authority.allowed_effects.contains(PUBLISH_GATE_EFFECT));
}

#[test]
fn under_warm_a_remote_check_keeps_its_evidence_group() {
    use review_core::task::runtime::{TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1};
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    let mut warm = policy(true, true);
    warm.warm = Some(serde_json::from_value(json!({"build_cache": ["cargo_target"]})).unwrap());
    let recorded = run(
        &warm,
        &[],
        &["kernel"],
        mapped(&remote, remote.mapping_text()),
    );
    assert_eq!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
    let groups: Vec<TaskRuntimeEvidenceV1> = recorded
        .cas
        .filed_objects()
        .unwrap()
        .into_iter()
        .filter_map(|object| recorded.cas.get_artifact(&object.digest).ok())
        .filter(|artifact| artifact.artifact_type == TASK_RUNTIME_EVIDENCE_V1)
        .map(|artifact| serde_json::from_value(artifact.payload).unwrap())
        .collect();
    let [group] = groups.as_slice() else {
        panic!("one evidence group per check: {groups:#?}");
    };
    assert_eq!(group.check.as_ref().unwrap().name, "kernel");
    assert!(group.spans.is_empty(), "it ran nothing on this machine");
    assert_eq!(
        group
            .caches
            .iter()
            .map(|cache| cache.kind.as_str())
            .collect::<Vec<_>>(),
        ["cargo_target:remote"]
    );
}
