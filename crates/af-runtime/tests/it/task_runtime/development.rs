//! Real, offline language tools through package admission, Task dispatch and command receipts.
//! The runtime translates mount paths on the host; it is not a containment implementation.
use super::*;
use af_contracts::task::development::*;
use af_source_git::task::{capture_snapshot, read_manifest, source_tree};
use af_source_git::{Entry, EntryKind, Manifest};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CHILD: &str = "AF_DEVELOPMENT_TOOLCHAIN_TEST_ROOT";

struct ToolchainDomain;
impl TaskOperatorHost for ToolchainDomain {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &af_graph::task::CompiledNode,
        _: &af_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        Ok("Seal the admitted candidate".into())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: &af_graph::task::CompiledNode,
        _: Option<&PreparedTaskAttempt>,
        _: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        TaskWorkOutput {
            outputs: af_runtime::task::source::seal_candidate(cas, input)
                .map(|source| BTreeMap::from([("snapshot".into(), source)])),
            usage_observation: None,
            usage: None,
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
            unknown_usage_cause: None,
            refused_input: false,
            notes: Default::default(),
        }
    }
}
impl TaskDomain for ToolchainDomain {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &af_store::store::task::execution::ReservedTaskAttempt,
        _: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV3,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _: &af_graph::task::CompiledNode,
    ) -> Result<(), String> {
        if output.outputs.contains_key("snapshot") {
            return af_runtime::task::source::validate_seal(cas, input, output);
        }
        let report = cas
            .get_artifact(&output.outputs["report"].artifact_ids[0])
            .map_err(|e| e.to_string())?;
        if report.payload["summary"].as_str().is_none() {
            return Err("missing toolchain report".into());
        }
        Ok(())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Ok(())
    }
}

fn root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn probe(program: &str, args: &[&str]) -> String {
    let mut command = std::process::Command::new(program);
    command.args(args).env("RUSTUP_AUTO_INSTALL", "0");
    let output = af_process::run_supervised(&mut command, None, LOAD_SAFE_WALL).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn admitted_implementer_and_reviewer_tasks_compile_test_rust_and_python_with_private_state() {
    run_toolchain_fixture("task_runtime::development::real_toolchain_task_child");
}

#[test]
fn exact_source_checkpoint_follows_admitted_edited_candidate_and_replays() {
    run_toolchain_fixture("task_runtime::development::checkpoint_task_child");
}

fn run_toolchain_fixture(child: &str) {
    // The machine binding is process-local authority. Pass it at exec, without mutating the
    // multithreaded test process's environment or borrowing an operator binding.
    let target = std::env::var_os("AF_GATE_NEXTEST_TARGET")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target"));
    std::fs::create_dir_all(&target).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("worker-toolchains-")
        .tempdir_in(target)
        .unwrap();
    let private = directory.path().canonicalize().unwrap();
    let runtime = private.join("runtime");
    std::fs::write(
        &runtime,
        include_bytes!("../../../../../fixtures/worker-development/recording-runtime.py"),
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Use installed real binaries, including under the gate's empty Rustup registry. No
    // project builds or acquisition occur in this fixture's tool discovery or host preflight.
    let sysroot = PathBuf::from(probe("rustc", &["--print", "sysroot"]));
    let python = probe(
        "python3",
        &["-I", "-c", "import sys; print(sys.executable)"],
    );
    let mut tools = BTreeMap::new();
    for (name, program) in [
        ("sh", PathBuf::from("/bin/sh")),
        ("cargo", sysroot.join("bin/cargo")),
        ("rustc", sysroot.join("bin/rustc")),
        ("cc", PathBuf::from("/usr/bin/cc")),
        ("python3", PathBuf::from(python)),
    ] {
        let program = program.to_str().unwrap();
        let digest = probe(
            "python3",
            &[
                "-I",
                "-c",
                "import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())",
                program,
            ],
        );
        tools.insert(
            name.into(),
            DevelopmentToolV1 {
                program: program.into(),
                identity: format!("sha256:{digest}"),
            },
        );
    }
    let binding = WorkerDevelopmentBindingV3 {
        schema: "af.worker-development-binding/3".into(),
        storage: af_contracts::task::development::WorkerStorageBackendV1::TmpfsVolume,
        runtime: runtime.to_str().unwrap().into(),
        image: format!("fixture.invalid/toolchain@sha256:{}", "a".repeat(64)),
        checksum_program: "/usr/bin/sha256sum".into(),
        probe_shell: "/bin/sh".into(),
        tools,
        limits: DevelopmentLimitsV1 {
            wall_ms: 120000,
            output_bytes: 65536,
            cpu_millicores: Some(2000),
            memory_bytes: Some(1073741824),
            processes: Some(128),
            disk_bytes: Some(268435456),
        },
        cache_limits: (child == "task_runtime::development::checkpoint_task_child")
            .then_some(af_contracts::BuildCacheLimitsV1::default_v1()),
        dependency_sources: BTreeMap::new(),
    };
    let binding_path = private.join("binding.toml");
    std::fs::write(&binding_path, toml::to_string(&binding).unwrap()).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", child, "--nocapture"])
        .env(CHILD, &private)
        .env("AF_WORKER_DEVELOPMENT_BINDING_FILE", &binding_path);
    let output = af_process::run_supervised(&mut command, None, Duration::from_secs(600)).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_toolchain_task_child() {
    let Some(private) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    // Even the fixture's real Cargo build directories stay under this checkout's target and
    // are removed by the parent. No writable host toolchain/cache/HOME is mounted or shared.
    af_sandbox::set_sandbox_root(private.clone()).unwrap();
    let binding =
        af_pipeline_config::task::development::read_binding(&private.join("binding.toml")).unwrap();
    for language in ["rust", "python"] {
        for role in ["implement", "review"] {
            run_task(&private, &binding, language, role, false);
        }
    }
}

#[test]
fn checkpoint_task_child() {
    let Some(private) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    af_sandbox::set_sandbox_root(private.clone()).unwrap();
    let binding =
        af_pipeline_config::task::development::read_binding(&private.join("binding.toml")).unwrap();
    run_task(&private, &binding, "python", "implement", true);
}

fn run_task(
    private: &Path,
    binding: &WorkerDevelopmentBindingV3,
    language: &str,
    role: &str,
    continuation: bool,
) {
    let directory = private.join(format!("{language}-{role}"));
    std::fs::create_dir(&directory).unwrap();
    let cas = Cas::open(directory.join("cas")).unwrap();
    let store_path = directory.join("events.sqlite");
    let mut store = EventStore::open(&store_path).unwrap();
    let examples = root().join("fixtures/worker-development").join(language);
    let policy = cas
        .put_json(&json!({"fixture":"toolchain receipt policy"}))
        .unwrap();
    let mut worker: TaskWorkerManifest =
        toml::from_str(&std::fs::read_to_string(examples.join("worker.toml")).unwrap()).unwrap();
    worker.name = format!("development/{language}-{role}");
    worker.signature.roles = BTreeSet::from([role.into()]);
    worker
        .signature
        .contract
        .inputs
        .retain(|name, _| name == "source");
    worker
        .signature
        .contract
        .outputs
        .retain(|name, _| name == "report");
    worker.signature.retains.retain(|name, _| name == "report");
    if role == "implement" {
        worker.signature.effects.insert("write-source".into());
        let mut candidate = worker.signature.contract.outputs["report"].clone();
        candidate.artifact_type = "af/CandidateTree@1".into();
        worker
            .signature
            .contract
            .outputs
            .insert("candidate".into(), candidate);
        worker
            .signature
            .retains
            .insert("candidate".into(), BTreeSet::from(["source".into()]));
    } else {
        worker.signature.effects.remove("write-source");
    }
    worker.signature.evidence =
        BTreeMap::from([("report".into(), BTreeSet::from([policy.clone()]))]);
    let binding_identity = cas
        .put_json(&serde_json::to_value(binding).unwrap())
        .unwrap();
    let mut text = std::fs::read_to_string(examples.join("development.template.toml"))
        .unwrap()
        .replace("@BINDING_ID@", &binding_identity);
    for (name, tool) in &binding.tools {
        text = text.replace(&format!("@{}_ID@", name.to_uppercase()), &tool.identity);
    }
    let mut request: WorkerDevelopmentV3 = toml::from_str(&text).unwrap();
    // This regression concerns local tools, not immutable cache carry (covered separately).
    request.caches.clear();
    request
        .environment
        .insert("BUILD_ROOT".into(), "${build}".into());
    if continuation {
        let mut cache_port = worker.signature.contract.outputs["report"].clone();
        cache_port.artifact_type = af_contracts::contract::BUILD_CACHE_V2.into();
        cache_port.optional = true;
        cache_port.affinity = PortAffinityV1::Unbound {};
        worker
            .signature
            .contract
            .inputs
            .insert("cache_in".into(), cache_port.clone());
        worker
            .signature
            .contract
            .outputs
            .insert("cache_out".into(), cache_port);
        let mut warm = worker.signature.contract.inputs["cache_in"].clone();
        warm.artifact_type = af_contracts::contract::WARM_SET_V3.into();
        worker
            .signature
            .contract
            .inputs
            .insert("development_warm".into(), warm);
        request
            .environment
            .insert("PRIVATE_STATE".into(), "${state}".into());
        request.caches.insert(
            "checkpoint".into(),
            DevelopmentCachePolicyV1 {
                role: DevelopmentCacheRoleV1::Continuation,
                destination: DevelopmentRootV1::State,
                path: "checkpoint".into(),
                selection: DevelopmentCacheSelectionV1::ExactSource,
                persistence: DevelopmentCachePersistenceV1::AdmittedOutput,
                lifecycle: DevelopmentCacheLifecycleV1::Task,
                limits: af_contracts::BuildCacheLimitsV1 {
                    max_bytes: 4096,
                    max_entries: 32,
                    max_depth: 8,
                    max_path_bytes: 256,
                },
                fingerprints: BTreeSet::from(["fixture.py".into()]),
                keys: BTreeMap::new(),
                input: Some("cache_in".into()),
                output: Some("cache_out".into()),
                dependency_source: None,
            },
        );
    }
    binding.admit(&request).unwrap();
    let mut harness = std::fs::read_to_string(examples.join("harness.sh")).unwrap();
    // Quiet Cargo omits transport-fixture host paths from progress messages. Actual test
    // summaries still come from Cargo/libtest; no runtime output is rewritten or invented.
    if language == "rust" {
        harness = harness.replace("cargo test ", "cargo test --quiet ");
    }
    // Test execution must observe this edit. Reviewers make it only in their experiment.
    // A compiler/test failure stops the real harness before it can emit a Worker reply.
    let source_file = if language == "rust" {
        "src/lib.rs"
    } else {
        "fixture.py"
    };
    let addition = if language == "rust" {
        "#[test] fn private_experiment_is_compiled_and_tested() { assert_eq!(twice(9), 18); }"
    } else {
        "assert twice(9) == 18, \"private experiment must run\""
    };
    harness = harness.replacen("set -eu", &format!(
        "set -eu\ntest ! -e \"$HOME/first-command\"\nprintf '%s\\n' '{addition}' >> {source_file}"
    ), 1);
    // A distinct process reads state written by the earlier command and checks the actual
    // build product. This fails if commands receive fresh state or tools skip compilation.
    let check = if language == "rust" {
        "test -d \"$BUILD_ROOT/debug\""
    } else {
        "test -f \"$BUILD_ROOT/fixture.pyc\""
    };
    harness = harness.replace("read -r first", &format!(
        "sh -c 'test \"$(cat \"$HOME/first-command\")\" = \"first command completed\"'\n{check}\nread -r first"
    ));
    if continuation {
        // Only the producer edits source. The consumer has no rebuild/checkpoint fallback.
        harness = r#"#!/bin/sh
set -eu
test ! -e "$HOME/conversation"
edited=no
while IFS= read -r line; do
    case "$line" in '# CHECKPOINT_SOURCE') edited=yes ;; esac
done < fixture.py
if test "$edited" = yes; then
    test "$(cat "$PRIVATE_STATE/checkpoint/step")" = 'admitted step 77'
    printf '%s\n' '{"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"restored admitted step 77"}]}}'
else
    test ! -e "$PRIVATE_STATE/checkpoint/step"
    printf '\n# CHECKPOINT_SOURCE\n' >> fixture.py
    mkdir -p "$PRIVATE_STATE/checkpoint"
    printf 'admitted step 77' > "$PRIVATE_STATE/checkpoint/step"
    printf private > "$HOME/conversation"
    printf '%s\n' '{"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"produced checkpoint"}]}}'
fi
"#.into();
    }
    let files = BTreeMap::from([
        ("worker.toml".into(), toml::to_string(&worker).unwrap().into_bytes()),
        ("development.toml".into(), toml::to_string(&request).unwrap().into_bytes()),
        ("harness.sh".into(), harness.into_bytes()),
        ("instructions.md".into(), std::fs::read(examples.join("instructions.md")).unwrap()),
        ("input.schema.json".into(), serde_json::to_vec(&json!({"type":"object","required":["source"],"properties":worker.signature.contract.inputs.keys().map(|name| (name.clone(), json!({"type":"array"}))).collect::<BTreeMap<_,_>>(),"additionalProperties":false})).unwrap()),
        ("outputs/report.schema.json".into(), std::fs::read(examples.join("outputs/report.schema.json")).unwrap()),
    ]);
    let paths: &[&str] = if language == "rust" {
        &["Cargo.toml", "Cargo.lock", "src/lib.rs"]
    } else {
        &["fixture.py", "test_fixture.py"]
    };
    let manifest = Manifest::new(
        paths
            .iter()
            .map(|path| {
                let bytes = std::fs::read(examples.join(path)).unwrap();
                Entry {
                    path: (*path).into(),
                    kind: EntryKind::File,
                    content: cas.put(&bytes).unwrap(),
                    size: bytes.len() as u64,
                }
            })
            .collect(),
    )
    .unwrap();
    let snapshot = capture_snapshot(&cas, &manifest, &policy, None).unwrap();
    let source = source_tree(&cas, producer(), &snapshot, vec![]).unwrap();
    let base = root().join("fixtures/task-contracts/v1");
    let mut pipeline: PipelineDefinitionV1 =
        serde_json::from_slice(&std::fs::read(base.join("pipeline-definition.json")).unwrap())
            .unwrap();
    pipeline.contract.inputs = worker.signature.contract.inputs.clone();
    pipeline.contract.inputs.retain(|name, _| name == "source");
    let output = pipeline.contract.outputs.get_mut("document").unwrap();
    output.artifact_type = worker.signature.contract.outputs["report"]
        .artifact_type
        .clone();
    output.affinity = if continuation {
        PortAffinityV1::Unbound {}
    } else {
        PortAffinityV1::SameAs {
            input: "source".into(),
        }
    };
    let slot = pipeline.slots.get_mut("author").unwrap();
    slot.worker = worker.name.clone();
    slot.role = role.into();
    slot.input_type = worker.signature.worker_input_type.clone().unwrap();
    slot.output_type = worker.signature.worker_output_type.clone().unwrap();
    slot.max_attempts = 1;
    pipeline.nodes[0].inputs = BTreeMap::from([(
        "source".into(),
        ValueRefV1::Input {
            port: "source".into(),
        },
    )]);
    for reference in [
        pipeline.outputs.get_mut("document").unwrap(),
        pipeline.coverage.get_mut("checked").unwrap(),
    ] {
        *reference = ValueRefV1::Node {
            node: "write".into(),
            port: "report".into(),
        };
    }
    if continuation {
        let from = |node: &str, port: &str| ValueRefV1::Node {
            node: node.into(),
            port: port.into(),
        };
        pipeline.nodes.push(TaskNodeV1 {
            id: "seal".into(),
            operator: TaskOperatorV1::Seal {},
            inputs: BTreeMap::from([("candidate".into(), from("write", "candidate"))]),
            when: None,
        });
        let mut next = pipeline.nodes[0].clone();
        next.id = "resume".into();
        next.inputs = BTreeMap::from([
            ("source".into(), from("seal", "snapshot")),
            ("cache_in".into(), from("write", "cache_out")),
        ]);
        pipeline.nodes.push(next);
        let mut old = pipeline.nodes[0].clone();
        old.id = "old_source".into();
        old.inputs
            .insert("cache_in".into(), from("write", "cache_out"));
        pipeline.nodes.push(old);
        pipeline
            .outputs
            .insert("document".into(), from("resume", "report"));
        pipeline
            .coverage
            .insert("checked".into(), from("resume", "report"));
        pipeline.max_attempts = 4;
    }
    let mut task: TaskRevisionV1 =
        serde_json::from_slice(&std::fs::read(base.join("task-revision.json")).unwrap()).unwrap();
    task.inputs = BTreeMap::from([("source".into(), source)]);
    task.required_outputs
        .get_mut("document")
        .unwrap()
        .artifact_type = output.artifact_type.clone();
    task.acceptance.get_mut("checked").unwrap().evidence_type = output.artifact_type.clone();
    task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
    task.provenance.adapter_id = policy.clone();
    task.provenance.input_artifact_ids = task.inputs["source"].artifact_ids.clone();
    task.authority.policy_id = policy.clone();
    task.authority.allowed_effects = worker.signature.effects.clone();
    task.limits.verification.wall_ms = request.limits.wall_ms;
    if continuation {
        // Three real Worker invocations each reserve the declared command allowance.
        task.limits.verification.attempts = 3;
        task.limits.verification.wall_ms = request.limits.wall_ms * 3;
    }
    task.limits.deadline_unix_ms = af_store::store::task::now().unwrap() + 600000;
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
    let mut compiler = TaskPlanCompiler::new(
        policy.clone(),
        policy.clone(),
        if continuation {
            let mut seal = worker.signature.clone();
            seal.contract.inputs = BTreeMap::from([(
                "candidate".into(),
                worker.signature.contract.outputs["candidate"].clone(),
            )]);
            seal.contract.inputs.get_mut("candidate").unwrap().affinity =
                PortAffinityV1::Unbound {};
            let mut snapshot = worker.signature.contract.inputs["source"].clone();
            snapshot.affinity = PortAffinityV1::DerivedFrom {
                input: "candidate".into(),
            };
            seal.contract.outputs = BTreeMap::from([("snapshot".into(), snapshot)]);
            seal.effects.clear();
            seal.evidence.clear();
            seal.retains.clear();
            seal.roles.clear();
            seal.worker_input_type = None;
            seal.worker_output_type = None;
            seal.attempt = None;
            BTreeMap::from([("operator/seal".into(), seal)])
        } else {
            BTreeMap::new()
        },
        BTreeMap::from([("checked".into(), "document".into())]),
        IndependencePolicyV1::default(),
    )
    .unwrap();
    for (name, files) in [
        (
            pipeline.name.as_str(),
            BTreeMap::from([(
                "pipeline.toml".into(),
                toml::to_string(&pipeline).unwrap().into_bytes(),
            )]),
        ),
        (worker.name.as_str(), files),
    ] {
        let pin = TaskPackagePin {
            version: "1.0.0".into(),
            digest: af_pipeline_config::lock::package_digest_from_files(&files),
            path: "package".into(),
        };
        let project = files
            .into_iter()
            .map(|(p, b)| (format!("package/{p}"), b))
            .collect();
        compiler
            .capture_package(&cas, name, &pin, &project)
            .unwrap();
    }
    compiler
        .bind_worker(
            &worker.name,
            AdmittedWorkerSettings {
                execution: WorkerExecutionV3::Command {},
                invocation_policy_id: policy,
            },
        )
        .unwrap();
    let (plan, graph) = compiler.compile(&cas, &revision, &pipeline.name).unwrap();
    let plan_id = cas
        .put_artifact(
            EXECUTION_PLAN_V4,
            producer(),
            vec![revision.clone()],
            None,
            serde_json::to_value(&plan).unwrap(),
        )
        .unwrap()
        .0;
    let environment = af_runtime::task::source::SnapshotTaskEnvironment {
        policy: af_sandbox::Policy::trusted_local(),
    };
    let domain = ToolchainDomain;
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
        .open_task(&cas, &revision, "toolchain-writer", 600000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut store, &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    let projection = runtime.projection().unwrap().execution.unwrap();
    if !report.complete() {
        for attempt in projection.attempt_accounting() {
            for id in attempt.raw_artifact_ids {
                if let Ok(value) = cas.get_json(&id) {
                    if let Ok(receipt) = serde_json::from_value::<WorkerCommandEvidenceV1>(value) {
                        eprintln!(
                            "{}",
                            String::from_utf8_lossy(&cas.get(&receipt.stderr_id).unwrap())
                        );
                    }
                }
            }
        }
        panic!("{language}/{role}: {report:?}");
    }
    let attempts = projection.attempt_accounting();
    if continuation {
        assert_eq!(
            attempts.len(),
            3,
            "producer, resumed Worker and old-source control"
        );
        let produced = &projection.outputs["root.nodes.write"].1.outputs;
        let resumed = &projection.outputs["root.nodes.resume"].1.outputs;
        let sealed = &projection.outputs["root.nodes.seal"].1.outputs["snapshot"];
        let output_snapshot = sealed.snapshot_id.as_ref().unwrap();
        assert_ne!(output_snapshot, &snapshot);
        let envelope = cas
            .get_artifact(&produced["cache_out"].artifact_ids[0])
            .unwrap();
        let cache: af_contracts::BuildCacheV2 = serde_json::from_value(envelope.payload).unwrap();
        assert_eq!(
            cache.head_snapshot_id, snapshot,
            "original input provenance"
        );
        assert_eq!(
            envelope.subject_snapshot_id.as_ref(),
            Some(output_snapshot),
            "admitted output binding"
        );
        let provenance = cache.worker.unwrap();
        assert_eq!(provenance.input_tree_digest, manifest.content_digest());
        let (_, after) = af_source_git::task::read_snapshot(&cas, output_snapshot).unwrap();
        assert_eq!(provenance.output_tree_digest, after.content_digest());
        assert_ne!(provenance.input_tree_digest, provenance.output_tree_digest);
        let resumed_cache: af_contracts::BuildCacheV2 = serde_json::from_value(
            cas.get_artifact(&resumed["cache_out"].artifact_ids[0])
                .unwrap()
                .payload,
        )
        .unwrap();
        let checkpoint = read_manifest(&cas, &resumed_cache.manifest_id).unwrap();
        assert_eq!(
            cas.get(&checkpoint.get("step").unwrap().content).unwrap(),
            b"admitted step 77"
        );
        assert_eq!(
            resumed_cache.manifest_id, cache.manifest_id,
            "restored bytes remain immutable"
        );
        assert_eq!(
            cas.get_artifact(&resumed["report"].artifact_ids[0])
                .unwrap()
                .payload["summary"],
            "restored admitted step 77"
        );
        let next = &projection.invocations["root.nodes.resume"].1;
        assert_eq!(next.inputs["source"], *sealed);
        assert_eq!(next.inputs["cache_in"], produced["cache_out"]);
        for (node, selected) in [
            ("root.nodes.resume", true),
            ("root.nodes.old_source", false),
        ] {
            let invocation = &projection.invocations[node].1;
            let warm: af_contracts::WarmSetV3 = serde_json::from_value(
                cas.get_artifact(&invocation.inputs["development_warm"].artifact_ids[0])
                    .unwrap()
                    .payload,
            )
            .unwrap();
            if selected {
                assert!(matches!(&warm.development["checkpoint"],
                    DevelopmentCacheSelectionRecordV1::Selected { artifact_id, compatibility_id, .. }
                    if artifact_id == &produced["cache_out"].artifact_ids[0]
                        && compatibility_id == &provenance.compatibility_id));
            } else {
                assert!(matches!(
                    warm.development["checkpoint"],
                    DevelopmentCacheSelectionRecordV1::Omitted {
                        reason: DevelopmentCacheMissV1::Incompatible,
                        ..
                    }
                ));
            }
        }
    }
    if !continuation {
        assert_eq!(attempts.len(), 1);
        let attempt = &attempts[0];
        let receipts: Vec<WorkerCommandEvidenceV1> = attempt
            .raw_artifact_ids
            .iter()
            .filter_map(|id| cas.get_json(id).ok())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        assert_eq!(receipts.len(), 2, "tool probe and actual harness");
        let evidence = receipts
            .iter()
            .find(|r| r.purpose == WorkerCommandPurposeV1::Harness)
            .unwrap();
        evidence.validate().unwrap();
        assert_eq!(evidence.attempt_id, attempt.attempt_id);
        assert_eq!(Some(&evidence.context_id), attempt.context_id.as_ref());
        assert_eq!(evidence.source_snapshot_id, snapshot);
        assert_eq!(evidence.binding_id, binding_identity);
        assert_eq!(evidence.experiment, role == "review");
        assert_eq!(evidence.command, ["/bin/sh", "/af/package/harness.sh"]);
        assert_eq!(evidence.cwd, "/work");
        assert_eq!(evidence.exit_code, Some(0));
        assert!(evidence.failure.is_none(), "{evidence:?}");
        assert!(!evidence.redacted);
        assert_eq!(evidence.input_tree_digest, manifest.content_digest());
        let before = read_manifest(&cas, &evidence.input_manifest_id).unwrap();
        // An experiment receipt retains its exact Manifest, not a reusable source Snapshot or
        // its edited file blobs. The implementer's candidate below must be fully readable.
        let after: Manifest = serde_json::from_value(
            cas.get_json(evidence.final_manifest_id.as_ref().unwrap())
                .unwrap(),
        )
        .unwrap();
        after.validate().unwrap();
        assert_eq!(before, manifest);
        assert_ne!(before.get(source_file), after.get(source_file));
        let mut edited = std::fs::read(examples.join(source_file)).unwrap();
        edited.extend_from_slice(format!("{addition}\n").as_bytes());
        assert_eq!(
            after.get(source_file).unwrap().content,
            af_source_git::digest_bytes(&edited)
        );
        assert_eq!(Some(after.content_digest()), evidence.final_tree_digest);
        let stderr = String::from_utf8(cas.get(&evidence.stderr_id).unwrap()).unwrap();
        if language == "rust" {
            assert_eq!(stderr.matches("2 passed").count(), 2, "{stderr}");
            assert_eq!(stderr.matches("running 2 tests").count(), 2, "{stderr}");
        } else {
            assert!(stderr.contains("Ran 1 test"), "{stderr}");
            assert!(stderr.contains("OK"), "{stderr}");
        }
        let returned = cas
            .get(evidence.returned_output_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(returned, cas.get(&evidence.stdout_id).unwrap());
        let reply: serde_json::Value = serde_json::from_slice(&returned).unwrap();
        let outputs = &projection.outputs["root.nodes.write"].1.outputs;
        assert_eq!(
            cas.get_artifact(&outputs["report"].artifact_ids[0])
                .unwrap()
                .payload,
            reply["outputs"]["report"][0]
        );
        if role == "implement" {
            let candidate: af_source_git::task::CandidateTree = serde_json::from_value(
                cas.get_artifact(&outputs["candidate"].artifact_ids[0])
                    .unwrap()
                    .payload,
            )
            .unwrap();
            assert_eq!(read_manifest(&cas, &candidate.manifest_id).unwrap(), after);
        } else {
            assert!(!outputs.contains_key("candidate"));
            assert_eq!(
                read_manifest(
                    &cas,
                    &af_source_git::task::read_snapshot(&cas, &snapshot)
                        .unwrap()
                        .0
                        .manifest_id
                )
                .unwrap(),
                manifest
            );
        }
    }
    // Replay after reopening the Store cannot turn private reviewer experiments into source.
    store.release_task_lease(&cas, &lease).unwrap();
    drop(store);
    let mut store = EventStore::open(&store_path).unwrap();
    let lease = store
        .take_task_lease(&cas, &task.task_id, "replay", 600000)
        .unwrap();
    let runtime = TaskRuntime::new(&mut store, &cas, lease, &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let replay = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(replay.outputs, projection.outputs);
    assert_eq!(replay.invocations, projection.invocations);
}
