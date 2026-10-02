use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use review_check::{CheckDefinition, CheckRunner, CheckStatus};
use review_core::Command;
use review_pipeline::task::code::{RustToolchainRequest, prepare_native_rust_toolchain};
use review_sandbox::toolchain::{ToolchainLimits, snapshot_toolchain};
use review_store::Cas;

const HOST: &str = "x86_64-unknown-linux-gnu";

fn limits() -> ToolchainLimits {
    ToolchainLimits {
        max_bytes: 1_000_000,
        max_entries: 100,
        max_copy_bytes: 1_000_000,
    }
}

fn request() -> RustToolchainRequest {
    RustToolchainRequest {
        version: "1.88.0".into(),
        host: HOST.into(),
        components: BTreeSet::from(["clippy".into(), "rustfmt".into()]),
        checks: BTreeSet::from(["native".into()]),
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf, String) {
    let source = root.join("installed");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::create_dir_all(source.join("lib")).unwrap();
    executable(
        &source.join("bin/rustc"),
        &format!("#!/bin/sh\nprintf 'release: 1.88.0\\nhost: {HOST}\\n'\n"),
    );
    executable(
        &source.join("bin/cargo"),
        "#!/bin/sh\ntest \"$CARGO_HOME\" = \"$HOME/cargo\" || exit 7\ntest \"$RUSTUP_HOME\" = \"$HOME/rustup\" || exit 8\ntest \"$RUSTUP_TOOLCHAIN\" = '1.88.0-x86_64-unknown-linux-gnu' || exit 9\ntest ! -e \"$CARGO_HOME/credentials.toml\" || exit 10\ntest \"$(cat \"$HOME/toolchain/lib/marker\")\" = pristine || exit 11\nif test \"$1\" = mutate; then printf changed > \"$HOME/toolchain/lib/marker\"; fi\n",
    );
    executable(&source.join("bin/rustfmt"), "#!/bin/sh\nexit 0\n");
    executable(&source.join("bin/clippy-driver"), "#!/bin/sh\nexit 0\n");
    executable(&source.join("bin/cargo-clippy"), "#!/bin/sh\nexit 0\n");
    executable(&source.join("bin/cargo-fmt"), "#!/bin/sh\nexit 0\n");
    fs::write(source.join("lib/marker"), "pristine").unwrap();
    let candidate = root.join("candidate");
    fs::create_dir(&candidate).unwrap();
    fs::write(
        candidate.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = '1.88.0'\n",
    )
    .unwrap();
    let seed = root.join("seed-probe");
    let digest = snapshot_toolchain(&source, &seed, limits(), None).unwrap();
    fs::remove_dir_all(seed).unwrap();
    let mapping = root.join("mapping.toml");
    write_mapping(&mapping, &source, &digest, HOST, "1.88.0", 1_000_000);
    (source, candidate, mapping, digest)
}

fn write_mapping(
    path: &Path,
    source: &Path,
    digest: &str,
    host: &str,
    version: &str,
    max_bytes: u64,
) {
    fs::write(path, format!("version = 1\n[rust]\nversion = '{version}'\nhost = '{host}'\ncomponents = ['clippy', 'rustfmt']\nsource = '{}'\nexpected_digest = '{digest}'\nmax_bytes = {max_bytes}\nmax_entries = 100\nmax_copy_bytes = {max_bytes}\n", source.display())).unwrap();
}

#[test]
fn two_native_checks_get_pristine_private_tools_and_homes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, candidate, mapping, digest) = fixture(root.as_path());
    fs::create_dir(root.as_path().join(".cargo")).unwrap();
    fs::write(
        root.as_path().join(".cargo/credentials.toml"),
        "fake secret",
    )
    .unwrap();
    let cas = Cas::open(root.as_path().join("cas")).unwrap();
    for iteration in 0..2 {
        let runtime = root.as_path().join(format!("runtime-{iteration}"));
        fs::create_dir(&runtime).unwrap();
        let env = prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
            .unwrap()
            .unwrap();
        assert!(
            env.environment
                .iter()
                .all(|(_, value)| !value.contains(&source.display().to_string()))
        );
        let mut runner =
            CheckRunner::new(&cas, &candidate).with_env("HOME", runtime.display().to_string());
        for (key, value) in env.environment {
            runner = runner.with_env(key, value);
        }
        let definition = CheckDefinition::new(
            "native",
            Command::new(
                "cargo",
                if iteration == 0 {
                    vec![review_core::Arg::literal("mutate")]
                } else {
                    vec![]
                },
            ),
        );
        assert_eq!(runner.run(&definition).status, CheckStatus::Passed);
        assert_eq!(
            fs::read_to_string(source.join("lib/marker")).unwrap(),
            "pristine"
        );
        assert_ne!(
            fs::metadata(source.join("lib/marker")).unwrap().ino(),
            fs::metadata(runtime.join("toolchain/lib/marker"))
                .unwrap()
                .ino()
        );
        assert!(!runtime.join("cargo/credentials.toml").exists());
        let independent = root.as_path().join(format!("independent-{iteration}"));
        assert_eq!(
            snapshot_toolchain(&source, &independent, limits(), Some(&digest)).unwrap(),
            digest
        );
    }
}

#[test]
fn mapping_pin_digest_and_candidate_mismatch_refuse_before_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, candidate, mapping, digest) = fixture(root.as_path());
    let runtime = root.as_path().join("run");
    fs::create_dir(&runtime).unwrap();
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), None)
            .unwrap()
            .is_none()
    );
    write_mapping(&mapping, &source, &digest, HOST, "1.89.0", 1_000_000);
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping)).is_err()
    );
    write_mapping(
        &mapping,
        &source,
        &digest,
        "wrong-host",
        "1.88.0",
        1_000_000,
    );
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping)).is_err()
    );
    write_mapping(
        &mapping,
        &source,
        &format!("sha256:{}", "0".repeat(64)),
        HOST,
        "1.88.0",
        1_000_000,
    );
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping)).is_err()
    );
    write_mapping(&mapping, &source, &digest, HOST, "1.88.0", 1_000_000);
    let complete_mapping = fs::read_to_string(&mapping).unwrap();
    fs::write(
        &mapping,
        complete_mapping.replace("['clippy', 'rustfmt']", "['rustfmt']"),
    )
    .unwrap();
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
            .unwrap_err()
            .contains("disagrees")
    );
    fs::write(&mapping, complete_mapping).unwrap();
    fs::write(
        candidate.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = '1.89.0'\n",
    )
    .unwrap();
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping)).is_err()
    );
}

#[test]
fn unsafe_source_shapes_and_limits_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, _, _, _) = fixture(root.as_path());
    let target = root.as_path().join("copy");
    let linked_root = root.as_path().join("linked");
    symlink(&source, &linked_root).unwrap();
    assert!(snapshot_toolchain(&linked_root, &target, limits(), None).is_err());
    assert!(snapshot_toolchain(Path::new("/tmp/../tmp"), &target, limits(), None).is_err());
    symlink(source.join("lib/marker"), source.join("lib/link")).unwrap();
    assert!(snapshot_toolchain(&source, &target, limits(), None).is_err());
    fs::remove_file(source.join("lib/link")).unwrap();
    let mut small = limits();
    small.max_bytes = 1;
    small.max_copy_bytes = 1;
    assert!(snapshot_toolchain(&source, &target, small, None).is_err());
    let mut few = limits();
    few.max_entries = 1;
    assert!(snapshot_toolchain(&source, &target, few, None).is_err());
}

fn refresh_mapping(root: &Path, source: &Path, mapping: &Path) {
    let probe = root.join("refreshed-probe");
    let digest = snapshot_toolchain(source, &probe, limits(), None).unwrap();
    fs::remove_dir_all(probe).unwrap();
    write_mapping(mapping, source, &digest, HOST, "1.88.0", 1_000_000);
}

#[test]
fn concurrent_checks_copy_host_hardlinks_without_aliases_or_host_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, candidate, mapping, _) = fixture(root.as_path());
    let external = root.as_path().join("host-marker");
    fs::hard_link(source.join("lib/marker"), &external).unwrap();
    fs::hard_link(&external, source.join("lib/marker-alias")).unwrap();
    refresh_mapping(root.as_path(), &source, &mapping);
    let before = fs::metadata(&external).unwrap();
    let runtimes: Vec<_> = (0..2)
        .map(|i| root.as_path().join(format!("concurrent-{i}")))
        .collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (i, runtime) in runtimes.iter().enumerate() {
            let candidate = &candidate;
            let mapping = &mapping;
            handles.push(scope.spawn(move || {
                fs::create_dir(runtime).unwrap();
                let env =
                    prepare_native_rust_toolchain(candidate, runtime, &request(), Some(mapping))
                        .unwrap()
                        .unwrap();
                assert!(
                    env.environment
                        .iter()
                        .any(|(key, value)| key == "CARGO_HOME"
                            && value == &runtime.join("cargo").display().to_string())
                );
                fs::write(runtime.join("toolchain/lib/marker"), format!("private-{i}")).unwrap();
                assert_eq!(
                    fs::read_to_string(runtime.join("toolchain/lib/marker")).unwrap(),
                    format!("private-{i}")
                );
                assert_eq!(
                    fs::read_to_string(runtime.join("toolchain/lib/marker-alias")).unwrap(),
                    "pristine"
                );
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    let mut identities = BTreeSet::new();
    identities.insert((before.dev(), before.ino()));
    for runtime in &runtimes {
        for name in ["marker", "marker-alias"] {
            let metadata = fs::metadata(runtime.join("toolchain/lib").join(name)).unwrap();
            assert_eq!(metadata.nlink(), 1);
            assert!(
                identities.insert((metadata.dev(), metadata.ino())),
                "copy aliases host or another private file"
            );
        }
    }
    for path in [
        external,
        source.join("lib/marker"),
        source.join("lib/marker-alias"),
    ] {
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.nlink(), after.mode()),
            (before.dev(), before.ino(), before.nlink(), before.mode())
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "pristine");
    }
}

#[test]
fn corrupt_digest_removes_partial_copy_and_does_not_publish_homes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, candidate, mapping, digest) = fixture(root.as_path());
    let runtime = root.as_path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    write_mapping(
        &mapping,
        &source,
        &format!("sha256:{}", "0".repeat(64)),
        HOST,
        "1.88.0",
        1_000_000,
    );
    let error = prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
        .unwrap_err();
    assert!(error.contains("digest"), "{error}");
    assert_eq!(
        fs::read_dir(&runtime).unwrap().count(),
        0,
        "failed digest published private state"
    );
    // Cleanup permits a retry at the identical destination, not just a fresh one.
    write_mapping(&mapping, &source, &digest, HOST, "1.88.0", 1_000_000);
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
            .unwrap()
            .is_some()
    );
}

#[test]
fn credential_and_configuration_shaped_source_entries_are_refused() {
    for name in [
        "credentials",
        "credentials.toml",
        "credentials.json",
        "config",
        "config.toml",
        ".netrc",
        ".cargo",
        ".git-credentials",
        "CREDENTIALS.TOML",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (source, _, _, _) = fixture(root.as_path());
        let forbidden = source.join("lib").join(name);
        if name == ".cargo" {
            fs::create_dir(&forbidden).unwrap();
        } else {
            fs::write(&forbidden, "synthetic fixture only").unwrap();
        }
        let copy = root.as_path().join("copy");
        let error = snapshot_toolchain(&source, &copy, limits(), None).unwrap_err();
        assert!(error.contains("credential/config"), "{name}: {error}");
        assert!(!copy.exists(), "{name}: rejected source published a copy");
    }
}

#[test]
fn actual_rustc_identity_must_match_even_with_matching_content_digest() {
    for (version, host, status) in [
        ("1.89.0", HOST, 0),
        ("1.88.0", "wrong-host", 0),
        ("1.88.0", HOST, 1),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (source, candidate, mapping, _) = fixture(root.as_path());
        executable(
            &source.join("bin/rustc"),
            &format!("#!/bin/sh\nprintf 'release: {version}\\nhost: {host}\\n'\nexit {status}\n"),
        );
        refresh_mapping(root.as_path(), &source, &mapping);
        let runtime = root.as_path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let error = prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
            .unwrap_err();
        assert!(error.contains("release or host"), "{error}");
        assert!(!runtime.join("cargo").exists());
        assert!(!runtime.join("rustup").exists());
    }
}

#[test]
fn declared_toolchain_requires_each_component_file() {
    for name in [
        "rustc",
        "cargo",
        "rustfmt",
        "clippy-driver",
        "cargo-fmt",
        "cargo-clippy",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (source, candidate, mapping, _) = fixture(root.as_path());
        fs::remove_file(source.join("bin").join(name)).unwrap();
        refresh_mapping(root.as_path(), &source, &mapping);
        let runtime = root.as_path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        assert!(
            prepare_native_rust_toolchain(&candidate, &runtime, &request(), Some(&mapping))
                .is_err(),
            "missing {name} was admitted"
        );
        assert!(!runtime.join("cargo").exists());
    }
}

#[test]
fn absent_mapping_preserves_cold_fallback_without_creating_runtime_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let candidate = root.as_path().join("candidate");
    let runtime = root.as_path().join("runtime");
    fs::create_dir(&candidate).unwrap();
    fs::create_dir(&runtime).unwrap();
    assert!(
        prepare_native_rust_toolchain(&candidate, &runtime, &request(), None)
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    // An explicitly selected missing file is an operator error, never silent fallback.
    assert!(
        prepare_native_rust_toolchain(
            &candidate,
            &runtime,
            &request(),
            Some(&root.as_path().join("missing.toml"))
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
}

#[test]
fn code_task_domain_dispatches_private_toolchain_checks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, _, mapping, _) = fixture(&root);
    run_code_task_domain_fixture(Some(mapping), false, false);
    assert_eq!(
        fs::read_to_string(source.join("lib/marker")).unwrap(),
        "pristine"
    );
}

#[test]
fn code_task_domain_cold_fallback_preserves_existing_environment() {
    run_code_task_domain_fixture(None, true, false);
}

fn run_code_task_domain_fixture(mapping: Option<PathBuf>, cold: bool, review_wrapper: bool) {
    use review_config::task::catalog::{TaskPackagePin, TaskPlanCompiler};
    use review_core::Producer;
    use review_core::task::EXECUTION_PLAN_V1;
    use review_core::task::pipeline::{PipelineDefinitionV1, ReceiptOutcomeV1};
    use review_core::task::plan::IndependencePolicyV1;
    use review_core::task::verification::{TASK_CHECK_RECEIPT_V1, TaskCheckReceiptV1};
    use review_core::task::{TASK_REVISION_V1, TaskRevisionV1};
    use review_pipeline::task::TaskRuntime;
    use review_pipeline::task::code::{CodeTaskDomain, CodeTaskPolicy, code_signatures};
    use review_pipeline::task::host::{CapturedTaskAuthority, CapturedTaskHost, NoTaskDeveloper};
    use review_pipeline::task::source::SnapshotTaskEnvironment;
    use review_source_git::task::{SOURCE_TREE_V1, capture_snapshot, source_tree};
    use review_source_git::{Entry, EntryKind, Manifest};
    use review_store::EventStore;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let cas = Cas::open(root.as_path().join("cas")).unwrap();
    let mut store = EventStore::open(root.as_path().join("events.sqlite")).unwrap();
    let producer = || Producer::KernelOperation {
        run_id: "toolchain-domain-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    };
    let names = BTreeSet::from(["first".into(), "second".into()]);
    let mut policy = CodeTaskPolicy {
        schema: "af.code-task-policy/1".into(),
        checks: BTreeMap::from([
            (
                "first".into(),
                CheckDefinition::new(
                    "first",
                    Command::new("cargo", vec![review_core::Arg::literal("mutate")]),
                ),
            ),
            (
                "second".into(),
                CheckDefinition::new("second", Command::new("cargo", vec![])),
            ),
        ]),
        check_wall_ms: 20_000,
        check_process_wall_ms: Some(10_000),
        require_container: false,
        warm: None,
        measures: BTreeMap::new(),
        objectives: BTreeMap::new(),
        rust_toolchain: Some(RustToolchainRequest {
            checks: names,
            ..request()
        }),
    };
    if cold {
        // No private Rust homes are injected. Existing network policy is unchanged;
        // the fixture itself uses no network operations.
        for check in policy.checks.values_mut() {
            check.command = Command::new(
                "sh",
                vec![
                    review_core::Arg::literal("-c"),
                    review_core::Arg::literal(
                        r#"test -n "$PATH" && test -z "$CARGO_HOME$RUSTUP_HOME$RUSTUP_TOOLCHAIN" && test ! -e "$HOME/toolchain""#,
                    ),
                ],
            );
        }
    }
    let policy_id = cas
        .put_json(&serde_json::to_value(&policy).unwrap())
        .unwrap();
    let bytes = b"[toolchain]\nchannel = '1.88.0'\n";
    let manifest = Manifest::new(vec![Entry {
        path: "rust-toolchain.toml".into(),
        kind: EntryKind::File,
        content: cas.put(bytes).unwrap(),
        size: bytes.len() as u64,
    }])
    .unwrap();
    let origin = cas
        .put_json(&json!({"fixture":"toolchain-domain"}))
        .unwrap();
    let snapshot = capture_snapshot(&cas, &manifest, &origin, None).unwrap();
    let source = source_tree(&cas, producer(), &snapshot, vec![]).unwrap();
    let port = |kind: &str, affinity: serde_json::Value, covers: Vec<&str>| {
        json!({
            "artifact_type":kind,"cardinality":"one","optional":false,"affinity":affinity,"covers":covers
        })
    };
    let pipeline: PipelineDefinitionV1 = serde_json::from_value(json!({
        "schema":"af.pipeline/1","name":"fixture/toolchain","version":"1.0.0",
        "contract":{"inputs":{"source":port(SOURCE_TREE_V1,json!({"kind":"unbound"}),vec![])},
            "outputs":{"receipt":port(TASK_CHECK_RECEIPT_V1,json!({"kind":"same_as","input":"source"}),vec!["checked"])}},
        "accepts":{"kinds":["implement"],"required_facts":{}},"slots":{},
        "nodes":[{"id":"check","operator":{"op":"check","checks":["first","second"]},"inputs":{"source":{"kind":"input","port":"source"}}}],
        "outputs":{"receipt":{"kind":"node","node":"check","port":"result"}},
        "coverage":{"checked":{"kind":"node","node":"check","port":"result"}},
        "max_attempts":1,"max_parallel":1
    })).unwrap();
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 120_000;
    let task: TaskRevisionV1 = serde_json::from_value(json!({
        "task_id":"toolchain-domain","revision":1,"kind":"implement","goal":"Verify private toolchains through captured CodeTaskDomain",
        "inputs":{"source":source},"required_outputs":{"receipt":{"artifact_type":TASK_CHECK_RECEIPT_V1,"cardinality":"one"}},
        "acceptance":{"checked":{"evidence_type":TASK_CHECK_RECEIPT_V1,"verifier_policy":policy_id}},
        "provenance":{"adapter_id":origin,"input_artifact_ids":source.artifact_ids},
        "authority":{"policy_id":policy_id,"allowed_effects":["execute-checks"],"data_destinations":[]},
        "limits":{"tokens":1000,"max_attempts":2,"deadline_unix_ms":deadline,"verification":{"tokens":200,"attempts":1,"wall_ms":20_000}},
        "strategy":"small","pipeline":{"name":"fixture/toolchain","fallback":"refuse"},"facts":{}
    })).unwrap();
    let mut compiler = TaskPlanCompiler::new(
        policy_id.clone(),
        policy_id.clone(),
        code_signatures(&policy_id, &policy).unwrap(),
        BTreeMap::from([("checked".into(), "receipt".into())]),
        IndependencePolicyV1::default(),
    )
    .unwrap();
    let files = BTreeMap::from([(
        "pipeline.toml".to_string(),
        toml::to_string(&pipeline).unwrap().into_bytes(),
    )]);
    let pin = TaskPackagePin {
        version: "1.0.0".into(),
        path: "package".into(),
        digest: review_config::lock::package_digest_from_files(&files),
    };
    compiler
        .capture_package(
            &cas,
            &pipeline.name,
            &pin,
            &files
                .into_iter()
                .map(|(name, bytes)| (format!("package/{name}"), bytes))
                .collect(),
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
    let domain: Box<dyn review_pipeline::task::host::TaskDomain> = if review_wrapper {
        let review_policy = cas
            .put_json(&json!({
                "schema":"af.review-task-policy/2", "check_policy_id":policy_id,
                "reviewers":{"correctness":"required"}, "gate":"major", "clean_rounds":1,
                "max_rounds":1, "allow_targeted_repairs":false
            }))
            .unwrap();
        Box::new(
            review_pipeline::task::review::ReviewTaskDomain::captured(
                &cas,
                &review_policy,
                graph.clone(),
            )
            .unwrap()
            .with_rust_toolchain_mapping(mapping),
        )
    } else {
        Box::new(
            CodeTaskDomain::captured(&cas, &policy_id, graph.clone())
                .unwrap()
                .with_rust_toolchain_mapping(mapping),
        )
    };
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
        domain.as_ref(),
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&compiler, &host, &NoTaskDeveloper);
    let lease = store
        .open_task(&cas, &revision, "test-writer", 120_000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut store, &cas, lease, &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    let state = runtime.projection().unwrap();
    let execution = state.execution.as_ref().unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1, "{report:?}");
    let (_, output) = execution
        .outputs
        .get("root.nodes.check")
        .expect("real check domain must publish its receipt");
    let receipt: TaskCheckReceiptV1 = serde_json::from_value(
        cas.get_artifact(&output.outputs["result"].artifact_ids[0])
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(receipt.outcome, ReceiptOutcomeV1::Passed, "{receipt:?}");
    assert_eq!(receipt.snapshot_id, snapshot);
    assert_eq!(receipt.checks.len(), 2);
    for id in receipt.checks.values() {
        let check: review_check::CheckResult =
            serde_json::from_value(cas.get_json(id).unwrap()).unwrap();
        assert_eq!(check.status, CheckStatus::Passed, "{check:?}");
        let stderr = String::from_utf8(cas.get(check.stderr.as_ref().unwrap()).unwrap()).unwrap();
        let evidence: serde_json::Value = serde_json::from_str(
            stderr
                .split("AF_TOOLCHAIN_SNAPSHOT ")
                .nth(1)
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(
            evidence["materialization"],
            if cold { "cold" } else { "private_copy" }
        );
        if !cold {
            assert_eq!(evidence["resolved_host"], HOST);
            assert_eq!(evidence["verified_release"], "1.88.0");
        }
    }
}

#[test]
fn ancestor_links_special_files_depth_and_plain_copy_caps_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (source, _, _, _) = fixture(&root);
    let alias = root.join("ancestor");
    symlink(&root, &alias).unwrap();
    assert!(
        snapshot_toolchain(&alias.join("installed"), &root.join("copy"), limits(), None).is_err()
    );
    assert!(
        std::process::Command::new("mkfifo")
            .arg(source.join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    assert!(snapshot_toolchain(&source, &root.join("copy"), limits(), None).is_err());
    fs::remove_file(source.join("pipe")).unwrap();
    let mut copy_limit = limits();
    copy_limit.max_copy_bytes = 1;
    assert!(snapshot_toolchain(&source, &root.join("copy"), copy_limit, None).is_err());
    let mut nested = source.clone();
    for _ in 0..130 {
        nested = nested.join("d");
        fs::create_dir(&nested).unwrap();
    }
    let mut deep_limit = limits();
    deep_limit.max_entries = 1000;
    assert!(snapshot_toolchain(&source, &root.join("copy"), deep_limit, None).is_err());
    assert!(!root.join("copy").exists());
}

#[test]
fn captured_policy_refuses_null_native_request_and_container_injection() {
    use review_pipeline::task::code::CodeTaskPolicy;
    let mut value = serde_json::json!({"schema":"af.code-task-policy/1", "check_wall_ms":10000,
        "require_container":false,"checks":{"native":{"name":"native","required":true,
        "command":{"program":"true","args":[]}}},"rust_toolchain":null});
    assert!(serde_json::from_value::<CodeTaskPolicy>(value.clone()).is_err());
    value["rust_toolchain"] = serde_json::to_value(request()).unwrap();
    let policy: CodeTaskPolicy = serde_json::from_value(value.clone()).unwrap();
    policy.validate().unwrap();
    value["require_container"] = serde_json::json!(true);
    let policy: CodeTaskPolicy = serde_json::from_value(value).unwrap();
    assert!(policy.validate().is_err());
}

// PATH must be set before the test process starts; never mutate global env in
// the multithreaded runner. ADR-0124: exact module-aware selection + one-pass guard.
#[test]
fn inherited_cargo_bin_subcommands_survive_and_rustup_is_not_called() {
    const NAME: &str = "inherited_cargo_bin_subcommands_survive_and_rustup_is_not_called";
    if let Some(root) = std::env::var_os("AF_TOOLCHAIN_PATH_FIXTURE") {
        let root = PathBuf::from(root);
        let (source, _, mapping, _) = fixture(&root);
        let cargo = source.join("bin/cargo");
        let body = fs::read_to_string(&cargo).unwrap();
        executable(
            &cargo,
            &(body + "\ncargo-nextest nextest --version || exit 12\n"),
        );
        refresh_mapping(&root, &source, &mapping);
        run_code_task_domain_fixture(Some(mapping), false, false);
        assert!(!root.join("rustup-called").exists());
        assert!(root.join("nextest-called").exists());
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let bin = root.join(".cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    executable(
        &bin.join("cargo-nextest"),
        &format!(
            "#!/bin/sh\ntest \"$1\" = nextest || exit 1\ntouch '{}'\n",
            root.join("nextest-called").display()
        ),
    );
    executable(
        &bin.join("rustup"),
        &format!(
            "#!/bin/sh\ntouch '{}'\nexit 99\n",
            root.join("rustup-called").display()
        ),
    );
    for name in ["cargo", "rustc", "rustfmt", "clippy-driver"] {
        executable(&bin.join(name), "#!/bin/sh\nexec rustup proxy\n");
    }
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))
        .unwrap();
    let module = module_path!()
        .split_once("::")
        .map(|(_, module)| format!("{module}::"))
        .unwrap_or_default();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{module}{NAME}"), "--nocapture"])
        .env("AF_TOOLCHAIN_PATH_FIXTURE", &root)
        .env("PATH", path)
        .env("RUSTUP_DIST_SERVER", "http://127.0.0.1:9")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("rustup-called").exists());
    assert!(root.join("nextest-called").exists());
}

#[test]
fn trusted_kernel_temp_ancestor_link_does_not_reject_candidate_declaration() {
    const NAME: &str = "trusted_kernel_temp_ancestor_link_does_not_reject_candidate_declaration";
    if std::env::var_os("AF_TOOLCHAIN_LINKED_TEMP").is_some() {
        code_task_domain_dispatches_private_toolchain_checks();
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let alias = root.join("trusted-temp-alias");
    symlink(&root, &alias).unwrap();
    let module = module_path!()
        .split_once("::")
        .map(|(_, m)| format!("{m}::"))
        .unwrap_or_default();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{module}{NAME}"), "--nocapture"])
        .env("AF_TOOLCHAIN_LINKED_TEMP", "1")
        .env("TMPDIR", &alias)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reviewed_task_wrapper_forwards_explicit_machine_mapping() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (_, _, mapping, _) = fixture(&root);
    run_code_task_domain_fixture(Some(mapping), false, true);
}
