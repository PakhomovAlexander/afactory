//! This recording runtime exercises the real Task invocation and supervisor without a daemon.
//! It proves transport, state lifetime, provenance and role isolation, not OS containment (the
//! existing live container probes own that separate claim).
use super::*;
use af_contracts::{Arg, Producer};
use af_runner::task::WorkerContract;
use af_source_git::task::{capture_snapshot, source_tree};
use af_source_git::{Entry, EntryKind, Manifest};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const RUNTIME: &str =
    include_str!("../../../../../fixtures/worker-development/recording-runtime.py");

fn identity(byte: &str) -> String {
    format!("sha256:{}", byte.repeat(64))
}
fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "development-fixture".into(),
        node_id: None,
        operation_id: "source@1".into(),
    }
}

fn source_invocation(cas: &Cas, manifest: &Manifest) -> TaskInvocationV1 {
    let origin = cas.put_json(&json!({"schema":"af.task-source-origin/3","kind":"captured","repository_id":"fixture","source_revision":null,"content_digest":manifest.content_digest()})).unwrap();
    let snapshot = capture_snapshot(cas, manifest, &origin, None).unwrap();
    let source = source_tree(cas, producer(), &snapshot, vec![]).unwrap();
    TaskInvocationV1 {
        plan_id: origin,
        node: "root.developer".into(),
        inputs: BTreeMap::from([("source".into(), source)]),
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    cas: Cas,
    manifest: Manifest,
    input: TaskInvocationV1,
    binding: WorkerDevelopmentBindingV3,
    request: WorkerDevelopmentV3,
    contract: WorkerContract,
    context: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let runtime = directory.path().join("runtime");
        std::fs::write(&runtime, RUNTIME).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = Manifest::new(vec![Entry {
            path: "value.txt".into(),
            kind: EntryKind::File,
            content: cas.put(b"7\n").unwrap(),
            size: 2,
        }])
        .unwrap();
        let input = source_invocation(&cas, &manifest);
        let limits = DevelopmentLimitsV1 {
            wall_ms: 30000,
            output_bytes: 4096,
            cpu_millicores: Some(1000),
            memory_bytes: Some(64 * 1024 * 1024),
            disk_bytes: Some(268435456),
            processes: Some(32),
        };
        // Obtain the fixture binary's plain SHA-256 through the runtime's identity protocol.
        // The production backend checks exactly these bytes before executing the harness.
        let mut probe = std::process::Command::new("python3");
        probe.args([
            "-c",
            "import hashlib; print(hashlib.sha256(open('/bin/sh','rb').read()).hexdigest())",
        ]);
        let digest = af_process::run_supervised(&mut probe, None, Duration::from_secs(30)).unwrap();
        assert!(digest.status.success());
        let tool_identity = format!(
            "sha256:{}",
            std::str::from_utf8(&digest.stdout).unwrap().trim()
        );
        let binding = WorkerDevelopmentBindingV3 {
            dependency_sources: Default::default(),
            cache_limits: None,
            schema: "af.worker-development-binding/3".into(),
            storage: af_contracts::task::development::WorkerStorageBackendV1::TmpfsVolume,
            runtime: runtime.to_str().unwrap().into(),
            image: format!("fixture.invalid/toolchain@{}", identity("a")),
            checksum_program: "/usr/bin/sha256sum".into(),
            probe_shell: "/bin/sh".into(),
            tools: BTreeMap::from([(
                "sh".into(),
                DevelopmentToolV1 {
                    program: "/bin/sh".into(),
                    identity: tool_identity.clone(),
                },
            )]),
            limits: limits.clone(),
        };
        let binding_identity = cas
            .put_json(&serde_json::to_value(&binding).unwrap())
            .unwrap();
        let request = WorkerDevelopmentV3 {
            caches: Default::default(),
            schema: DEVELOPMENT_SCHEMA.into(),
            harness: "sh".into(),
            binding_identity,
            tools: BTreeMap::from([("sh".into(), tool_identity)]),
            environment: BTreeMap::from([
                ("BUILD_ROOT".into(), "${build}".into()),
                ("PRIVATE_STATE".into(), "${state}".into()),
            ]),
            writable_roots: BTreeSet::from([
                DevelopmentRootV1::Source,
                DevelopmentRootV1::State,
                DevelopmentRootV1::Build,
                DevelopmentRootV1::Scratch,
            ]),
            state: PrivateStatePolicyV1::AttemptPrivate,
            cache: PrivateStatePolicyV1::Disabled,
            knowledge: PrivateStatePolicyV1::Disabled,
            dependencies: DependencyPolicyV1::AvailableOnly,
            network: NetworkPolicyV1::Deny,
            limits,
        };
        let contract=WorkerContract::capture(&cas,json!({"type":"object"}),BTreeMap::from([("report".into(),json!({"type":"object","required":["answer"],"properties":{"answer":{"type":"integer"}},"additionalProperties":false}))])).unwrap();
        let context = contract
            .prepare(&cas, &input, &[], "Compile and run the fixture")
            .unwrap();
        Self {
            _directory: directory,
            cas,
            manifest,
            input,
            binding,
            request,
            contract,
            context,
        }
    }
    fn run(
        &self,
        writes_source: bool,
        harness: &str,
    ) -> (WorkerReturn, Sandbox, WorkerCommandEvidenceV1) {
        let original =
            Sandbox::materialize(&self.manifest, &self.cas, Mode::EphemeralWrite).unwrap();
        let files = BTreeMap::from([("harness.sh".into(), harness.as_bytes().to_vec())]);
        let command = Command::new("@tool/sh", vec![Arg::literal("@package/harness.sh")]);
        let (returned, original) = invoke_bound(
            &self.cas,
            &self.input,
            "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            &self.context,
            ("development-fixture", "developer"),
            &self.request,
            &self.binding,
            &command,
            &files,
            &self.contract,
            original,
            writes_source,
            Instant::now() + Duration::from_secs(30),
            None,
        )
        .unwrap();
        assert_eq!(
            returned
                .raw_artifact_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len(),
            returned.raw_artifact_ids.len()
        );
        let evidence = returned
            .raw_artifact_ids
            .iter()
            .filter_map(|id| self.cas.get_json(id).ok())
            .find_map(|value| {
                serde_json::from_value::<WorkerCommandEvidenceV1>(value)
                    .ok()
                    .filter(|evidence| evidence.purpose == WorkerCommandPurposeV1::Harness)
            })
            .expect("host receipt retained beside output");
        (returned, original.unwrap(), evidence)
    }
}

#[test]
fn command_role_transport_retains_attempt_private_state_and_experiment_receipts() {
    let fixture = Fixture::new();
    for (harness, answer) in [
        (
            "set -eu\nread value < value.txt\nprintf 'value=%s\\n' \"$value\" > \"$BUILD_ROOT/compiled\"\n. \"$BUILD_ROOT/compiled\"\nprintf '%s\\n' \"$value\" > \"$PRIVATE_STATE/first\"\nread again < \"$PRIVATE_STATE/first\"\ntest \"$(printf '%s' \"$value\")\" = \"$again\"\nprintf '8\\n' > value.txt\nprintf '{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"report\":[{\"answer\":%s}]}}' \"$value\"\n",
            7,
        ),
        (
            "set -eu\nread value < value.txt\nprintf 'answer=%s\\n' \"$((value+2))\" > \"$BUILD_ROOT/program\"\n. \"$BUILD_ROOT/program\"\nprintf '%s\\n' \"$answer\" > \"$HOME/state\"\nread remembered < \"$HOME/state\"\ntest \"$remembered\" = \"$answer\"\nprintf '9\\n' > value.txt\nprintf '{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"report\":[{\"answer\":%s}]}}' \"$remembered\"\n",
            9,
        ),
    ] {
        for writes_source in [true, false] {
            let (returned, original, evidence) = fixture.run(writes_source, harness);
            assert_eq!(
                returned.reply.unwrap().outputs["report"][0]["answer"],
                answer
            );
            evidence.validate().unwrap();
            assert_eq!(evidence.experiment, !writes_source);
            assert_ne!(
                evidence.input_tree_digest,
                evidence.final_tree_digest.clone().unwrap()
            );
            let returned_bytes = fixture
                .cas
                .get(evidence.returned_output_id.as_ref().unwrap())
                .unwrap();
            assert_eq!(
                fixture.cas.get(&evidence.stdout_id).unwrap(),
                returned_bytes
            );
            assert_eq!(evidence.context_id, fixture.context);
            assert_eq!(
                evidence.source_snapshot_id,
                fixture.input.inputs["source"]
                    .snapshot_id
                    .as_ref()
                    .unwrap()
                    .as_str()
            );
            let before: Manifest =
                serde_json::from_value(fixture.cas.get_json(&evidence.input_manifest_id).unwrap())
                    .unwrap();
            let after: Manifest = serde_json::from_value(
                fixture
                    .cas
                    .get_json(evidence.final_manifest_id.as_ref().unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(before.content_digest(), evidence.input_tree_digest);
            assert_eq!(Some(after.content_digest()), evidence.final_tree_digest);
            assert_eq!(
                after.get("value.txt").unwrap().content,
                af_source_git::digest_bytes(if answer == 7 { b"8\n" } else { b"9\n" })
            );
            let env = fixture.cas.get_json(&evidence.environment_id).unwrap();
            assert!(
                !env.to_string()
                    .contains(fixture._directory.path().to_str().unwrap())
            );
            let sealed = original.seal().unwrap();
            assert_eq!(
                sealed.unchanged(),
                !writes_source,
                "reviewer experiments never change the accepted source"
            );
        }
    }
}

#[test]
fn bounded_command_evidence_keeps_failed_and_redacted_output_without_accepting_it() {
    let fixture = Fixture::new();
    for (harness, redacted, truncated) in [
        ("printf failure; exit 7", false, false),
        (
            "i=0; while test $i -lt 5000; do printf x; i=$((i+1)); done",
            false,
            true,
        ),
        (
            "printf 'Authorization: Bearer fixture-credential-material'",
            true,
            false,
        ),
    ] {
        let (returned, original, evidence) = fixture.run(false, harness);
        assert!(returned.reply.is_err());
        assert!(evidence.returned_output_id.is_none());
        assert_eq!(evidence.redacted, redacted);
        assert_eq!(evidence.stdout_total_bytes > evidence.output_cap, truncated);
        assert!(fixture.cas.get(&evidence.stdout_id).unwrap().len() <= 4096);
        assert!(original.seal().unwrap().unchanged());
    }
}

#[test]
fn development_mount_arguments_refuse_host_delimiters() {
    for path in ["/tmp/source:ro", "/tmp/source,bind", "/tmp/source\nfile"] {
        assert!(safe_mount(Path::new(path)).is_err());
    }
    assert!(safe_mount(Path::new("/absolute/private/root")).is_ok());
}

#[test]
fn changed_tool_identity_retains_probe_evidence_and_refuses_harness_dispatch() {
    let mut fixture = Fixture::new();
    let changed = identity("b");
    fixture.binding.tools.get_mut("sh").unwrap().identity = changed.clone();
    fixture.request.tools.insert("sh".into(), changed);
    fixture.request.binding_identity = fixture
        .cas
        .put_json(&serde_json::to_value(&fixture.binding).unwrap())
        .unwrap();
    let original =
        Sandbox::materialize(&fixture.manifest, &fixture.cas, Mode::EphemeralWrite).unwrap();
    let command = Command::new("@tool/sh", vec![Arg::literal("@package/harness.sh")]);
    let files = BTreeMap::from([("harness.sh".into(), b"printf changed > value.txt".to_vec())]);
    let (returned, original) = invoke_bound(
        &fixture.cas,
        &fixture.input,
        "bbbbbbbbbbbbbbbbbbbbbbbbbb",
        &fixture.context,
        ("development-fixture", "developer"),
        &fixture.request,
        &fixture.binding,
        &command,
        &files,
        &fixture.contract,
        original,
        true,
        Instant::now() + Duration::from_secs(30),
        None,
    )
    .unwrap();
    assert!(returned.reply.is_err());
    let records: Vec<WorkerCommandEvidenceV1> = returned
        .raw_artifact_ids
        .iter()
        .filter_map(|id| fixture.cas.get_json(id).ok())
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].purpose, WorkerCommandPurposeV1::ToolProbe);
    assert!(records[0].failure.is_some());
    assert!(records[0].returned_output_id.is_none());
    assert!(
        !records[0].redacted,
        "actual well-framed checksum evidence is retained"
    );
    assert!(
        original.unwrap().seal().unwrap().unchanged(),
        "no project harness ran on changed tools"
    );
}

#[test]
fn final_experiment_manifest_evidence_refuses_entry_and_byte_amplification_before_hashing() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one"), b"one").unwrap();
    std::fs::write(directory.path().join("two"), b"two").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    assert!(bounded_final_manifest(directory.path(), deadline, 1, 1024).is_none());
    assert!(bounded_final_manifest(directory.path(), deadline, 2, 5).is_none());
    assert_eq!(
        bounded_final_manifest(directory.path(), deadline, 2, 6)
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[test]
fn source_symlinks_are_refused_before_any_development_tool_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let manifest = Manifest::new(vec![Entry {
        path: "link".into(),
        kind: EntryKind::Symlink,
        content: cas.put(b"/host").unwrap(),
        size: 5,
    }])
    .unwrap();
    let input = source_invocation(&cas, &manifest);
    assert!(
        checked_source(&cas, &input, false)
            .unwrap_err()
            .contains("symlinks")
    );
}

const BUILD_ONCE: &str = r#"set -eu
if [ ! -f "$BUILD_ROOT/object" ]; then printf '77' > "$BUILD_ROOT/object"; fi
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":%s}]}}\n' "$(cat "$BUILD_ROOT/object")"
"#;
const REUSE_ONLY: &str = r#"set -eu
test -f "$BUILD_ROOT/object"
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":%s}]}}\n' "$(cat "$BUILD_ROOT/object")"
"#;

impl Fixture {
    fn enable_build_cache(&mut self) {
        let limits = af_contracts::BuildCacheLimitsV1 {
            max_bytes: 4096,
            max_entries: 32,
            max_depth: 8,
            max_path_bytes: 256,
        };
        self.request.caches.insert(
            "objects".into(),
            DevelopmentCachePolicyV1 {
                role: DevelopmentCacheRoleV1::Build,
                destination: DevelopmentRootV1::Build,
                path: String::new(),
                selection: DevelopmentCacheSelectionV1::Compatible,
                persistence: DevelopmentCachePersistenceV1::AdmittedOutput,
                lifecycle: DevelopmentCacheLifecycleV1::Task,
                limits,
                fingerprints: BTreeSet::from(["value.txt".into()]),
                keys: BTreeMap::from([("profile".into(), "debug".into())]),
                input: Some("cache_in".into()),
                output: Some("cache_out".into()),
                dependency_source: None,
            },
        );
        self.binding.cache_limits = Some(limits);
        self.pin_and_select();
    }
    fn pin_and_select(&mut self) {
        self.request.binding_identity = self
            .cas
            .put_json(&serde_json::to_value(&self.binding).unwrap())
            .unwrap();
        self.binding.admit(&self.request).unwrap();
        self.input.inputs.remove(DEVELOPMENT_WARM_PORT);
        cache::select(
            &self.cas,
            &mut self.input,
            &self.request,
            &self.binding,
            "development-fixture",
            "developer",
        )
        .unwrap();
        self.context = self
            .contract
            .prepare(&self.cas, &self.input, &[], "Compile and run the fixture")
            .unwrap();
    }
    fn captured_output(&self, returned: &WorkerReturn) -> af_contracts::task::ArtifactInputV1 {
        assert!(returned.reply.is_ok(), "{:?}", returned.reply);
        let mut outputs = BTreeMap::new();
        cache::attach_outputs(
            &self.cas,
            &self.input,
            "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            &self.request,
            &returned.raw_artifact_ids,
            &mut outputs,
        )
        .unwrap();
        let value = outputs
            .remove("cache_out")
            .expect("kernel admitted a captured cache output");
        cache::validate_output(
            &self.cas,
            &self.input,
            "cache_out",
            &value,
            &outputs,
            &self.request,
            "development-fixture",
        )
        .unwrap();
        value
    }
    fn warm(&self) -> af_contracts::WarmSetV3 {
        cache::selection(&self.cas, &self.input, &self.request)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn worker_cache_reuses_compiled_bytes_without_rebuilding_and_parallel_clones_are_independently_writable()
 {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    assert!(matches!(
        fixture.warm().development["objects"],
        DevelopmentCacheSelectionRecordV1::Omitted {
            reason: DevelopmentCacheMissV1::NoInput,
            ..
        }
    ));
    let (first, _, _) = fixture.run(true, BUILD_ONCE);
    let output = fixture.captured_output(&first);
    fixture.input.node = "root.continue".into();
    fixture
        .input
        .inputs
        .insert("cache_in".into(), output.clone());
    fixture.pin_and_select();
    assert!(matches!(
        fixture.warm().development["objects"],
        DevelopmentCacheSelectionRecordV1::Selected { .. }
    ));
    // Both real invocations must use the existing compiled object: there is no build fallback.
    std::thread::scope(|scope| {
        let first = scope.spawn(|| fixture.run(true, REUSE_ONLY));
        let second = scope.spawn(|| {
            fixture.run(
                true,
                r#"set -eu
test "$(cat "$BUILD_ROOT/object")" = 77
printf 88 > "$BUILD_ROOT/object"
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":88}]}}\n'
"#,
            )
        });
        assert_eq!(
            first.join().unwrap().0.reply.unwrap().outputs["report"][0]["answer"],
            77
        );
        assert_eq!(
            second.join().unwrap().0.reply.unwrap().outputs["report"][0]["answer"],
            88
        );
    });
    let (replayed, _, _) = fixture.run(true, REUSE_ONLY);
    assert_eq!(
        replayed.reply.unwrap().outputs["report"][0]["answer"],
        77,
        "no clone mutated immutable producer bytes"
    );
    let (context, _) = fixture
        .contract
        .read_context(&fixture.cas, &fixture.context)
        .unwrap();
    assert!(
        context
            .manifest
            .entries
            .iter()
            .any(|entry| entry.name == "development-cache:objects")
    );
    assert_eq!(
        fixture
            .cas
            .get_artifact(&output.artifact_ids[0])
            .unwrap()
            .artifact_type,
        af_contracts::contract::BUILD_CACHE_V2
    );
}

#[test]
fn worker_cache_compatibility_invalidates_declared_tools_configuration_dependency_and_profile_keys()
{
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let (returned, _, _) = fixture.run(true, BUILD_ONCE);
    let output = fixture.captured_output(&returned);
    fixture.input.inputs.insert("cache_in".into(), output);
    fixture.pin_and_select();
    let original_request = fixture.request.clone();
    let original_input = fixture.input.clone();
    for change in ["tool", "config", "profile", "dependency"] {
        fixture.request = original_request.clone();
        fixture.input = original_input.clone();
        match change {
            "tool" => {
                fixture.request.tools.insert("sh".into(), identity("b"));
            }
            "config" => {
                fixture
                    .request
                    .environment
                    .insert("MODE".into(), "changed".into());
            }
            "profile" => {
                fixture
                    .request
                    .caches
                    .get_mut("objects")
                    .unwrap()
                    .keys
                    .insert("profile".into(), "release".into());
            }
            _ => {
                let mut manifest = fixture.manifest.clone();
                manifest.entries[0].content = fixture.cas.put(b"8\n").unwrap();
                fixture.input = source_invocation(&fixture.cas, &manifest);
                fixture
                    .input
                    .inputs
                    .insert("cache_in".into(), original_input.inputs["cache_in"].clone());
            }
        }
        // Selection itself is pure: no unadmitted tool or changed source is ever executed.
        fixture.input.inputs.remove(DEVELOPMENT_WARM_PORT);
        cache::select(
            &fixture.cas,
            &mut fixture.input,
            &fixture.request,
            &fixture.binding,
            "development-fixture",
            "developer",
        )
        .unwrap();
        assert!(
            matches!(
                fixture.warm().development["objects"],
                DevelopmentCacheSelectionRecordV1::Omitted {
                    reason: DevelopmentCacheMissV1::Incompatible,
                    ..
                }
            ),
            "{change}"
        );
    }
}

#[test]
fn retry_replays_the_recorded_warm_set_and_never_inherits_failed_sibling_edits_or_cache_state() {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let (returned, _, _) = fixture.run(true, BUILD_ONCE);
    let output = fixture.captured_output(&returned);
    fixture.input.inputs.insert("cache_in".into(), output);
    fixture.pin_and_select();
    let recorded = fixture.input.inputs[DEVELOPMENT_WARM_PORT].clone();
    let (failed, _, _) = fixture.run(
        true,
        r#"set -eu
printf bad > value.txt
printf 99 > "$BUILD_ROOT/object"
exit 1
"#,
    );
    assert!(failed.reply.is_err());
    let (retry, sandbox, _) = fixture.run(true, REUSE_ONLY);
    assert_eq!(retry.reply.unwrap().outputs["report"][0]["answer"], 77);
    assert_eq!(
        std::fs::read(sandbox.root().join("value.txt")).unwrap(),
        b"7\n"
    );
    assert_eq!(fixture.input.inputs[DEVELOPMENT_WARM_PORT], recorded);
    // The producer envelope is provenance too, even when every cloned file still exists.
    let set = fixture.warm();
    let artifact_id = match &set.development["objects"] {
        DevelopmentCacheSelectionRecordV1::Selected { artifact_id, .. } => artifact_id.clone(),
        _ => panic!("selected"),
    };
    let envelope = fixture.cas.get_artifact(&artifact_id).unwrap();
    fixture.cas.remove_unreachable(&artifact_id).unwrap();
    let runtime = CheckRuntime::new().unwrap();
    assert!(
        cache::restore(
            &fixture.cas,
            Some(&set),
            &fixture.request,
            DevelopmentRootV1::Build,
            &runtime.path().join("missing-provenance"),
            None,
            None
        )
        .unwrap_err()
        .contains("Missing")
    );
    let restored = fixture
        .cas
        .put_artifact(
            envelope.artifact_type,
            envelope.producer,
            envelope.input_artifacts,
            envelope.subject_snapshot_id,
            envelope.payload,
        )
        .unwrap()
        .0;
    assert_eq!(restored, artifact_id);
    // Removing the selected CAS content does not authorize a new selection or a cold fallback.
    let manifest_id = match &fixture.warm().development["objects"] {
        DevelopmentCacheSelectionRecordV1::Selected { manifest_id, .. } => manifest_id.clone(),
        _ => panic!("selected"),
    };
    fixture.cas.remove_unreachable(&manifest_id).unwrap();
    let runtime = CheckRuntime::new().unwrap();
    assert!(
        cache::restore(
            &fixture.cas,
            Some(&fixture.warm()),
            &fixture.request,
            DevelopmentRootV1::Build,
            &runtime.path().join("clone"),
            None,
            None
        )
        .unwrap_err()
        .contains("Missing")
    );
    fixture.pin_and_select();
    assert!(matches!(
        fixture.warm().development["objects"],
        DevelopmentCacheSelectionRecordV1::Omitted {
            reason: DevelopmentCacheMissV1::Missing,
            ..
        }
    ));
}

#[test]
fn worker_cache_wrong_slot_unapproved_dependencies_and_corrupt_manifests_have_explicit_outcomes() {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let (returned, _, _) = fixture.run(true, BUILD_ONCE);
    let output = fixture.captured_output(&returned);
    fixture
        .input
        .inputs
        .insert("cache_in".into(), output.clone());
    cache::select(
        &fixture.cas,
        &mut fixture.input,
        &fixture.request,
        &fixture.binding,
        "development-fixture",
        "independent-reviewer",
    )
    .unwrap();
    assert!(matches!(
        fixture.warm().development["objects"],
        DevelopmentCacheSelectionRecordV1::Omitted {
            reason: DevelopmentCacheMissV1::WrongLineage,
            ..
        }
    ));
    let envelope = fixture.cas.get_artifact(&output.artifact_ids[0]).unwrap();
    let cache: af_contracts::BuildCacheV2 = serde_json::from_value(envelope.payload).unwrap();
    std::fs::write(
        fixture
            ._directory
            .path()
            .join("cas/objects")
            .join(&cache.manifest_id[7..9])
            .join(&cache.manifest_id[9..]),
        b"corrupt",
    )
    .unwrap();
    fixture.pin_and_select();
    assert!(matches!(
        fixture.warm().development["objects"],
        DevelopmentCacheSelectionRecordV1::Omitted {
            reason: DevelopmentCacheMissV1::Corrupt,
            ..
        }
    ));
    let policy = fixture.request.caches.get_mut("objects").unwrap();
    policy.role = DevelopmentCacheRoleV1::Dependency;
    policy.destination = DevelopmentRootV1::Cache;
    policy.input = None;
    policy.output = None;
    policy.persistence = DevelopmentCachePersistenceV1::Discard;
    policy.dependency_source = Some("unapproved".into());
    fixture
        .request
        .writable_roots
        .insert(DevelopmentRootV1::Cache);
    fixture.request.cache = PrivateStatePolicyV1::AttemptPrivate;
    assert!(
        fixture
            .binding
            .admit(&fixture.request)
            .unwrap_err()
            .contains("Unapproved")
    );
}

#[test]
fn cancellation_and_incomplete_capture_never_publish_a_partial_continuation() {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let runtime = CheckRuntime::new().unwrap();
    let root = runtime.path().join("development-build");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("object"), b"77").unwrap();
    let cancellation = AtomicBool::new(true);
    for (clean, flag, expected) in [
        (
            true,
            Some(&cancellation),
            WorkerCacheCaptureOmissionV1::Cancelled,
        ),
        (
            false,
            None,
            WorkerCacheCaptureOmissionV1::CleanupUnconfirmed,
        ),
    ] {
        let ids = cache::capture(
            &fixture.cas,
            &fixture.input,
            "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            ("development-fixture", "developer"),
            &fixture.request,
            runtime.path(),
            clean,
            true,
            Some((&fixture.manifest.content_digest(), &fixture.manifest)),
            Instant::now() + Duration::from_secs(30),
            flag,
        )
        .unwrap();
        assert_eq!(ids.len(), 1);
        let receipt: WorkerCacheCaptureV1 =
            serde_json::from_value(fixture.cas.get_json(&ids[0]).unwrap()).unwrap();
        assert_eq!(receipt.omission, Some(expected));
        assert!(receipt.artifact_id.is_none());
    }
    std::os::unix::fs::symlink("object", root.join("unsafe")).unwrap();
    let capture = || {
        cache::capture(
            &fixture.cas,
            &fixture.input,
            "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            ("development-fixture", "developer"),
            &fixture.request,
            runtime.path(),
            true,
            true,
            Some((&fixture.manifest.content_digest(), &fixture.manifest)),
            Instant::now() + Duration::from_secs(30),
            None,
        )
        .unwrap()
    };
    let refused = capture();
    assert_eq!(refused.len(), 1);
    let receipt: WorkerCacheCaptureV1 =
        serde_json::from_value(fixture.cas.get_json(&refused[0]).unwrap()).unwrap();
    assert_eq!(
        receipt.omission,
        Some(WorkerCacheCaptureOmissionV1::CaptureRefused)
    );
    std::fs::remove_file(root.join("unsafe")).unwrap();
    let recovered = capture();
    assert_eq!(
        recovered.len(),
        2,
        "only a complete retry capture publishes a cache artifact"
    );
}

#[test]
fn explicit_continuation_restores_only_admitted_checkpoint_on_exact_source_and_keeps_home_fresh() {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let policy = fixture.request.caches.get_mut("objects").unwrap();
    policy.role = DevelopmentCacheRoleV1::Continuation;
    policy.destination = DevelopmentRootV1::State;
    policy.path = "checkpoint".into();
    policy.selection = DevelopmentCacheSelectionV1::ExactSource;
    fixture.pin_and_select();
    let (returned, _, _) = fixture.run(
        true,
        r#"set -eu
mkdir -p "$PRIVATE_STATE/checkpoint"
printf 77 > "$PRIVATE_STATE/checkpoint/step"
printf private > "$HOME/conversation"
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":77}]}}\n'
"#,
    );
    let output = fixture.captured_output(&returned);
    let raw_id = returned
        .raw_artifact_ids
        .iter()
        .find(|id| {
            fixture
                .cas
                .get_optional_artifact(id)
                .ok()
                .flatten()
                .is_some_and(|artifact| {
                    artifact.artifact_type == af_contracts::contract::BUILD_CACHE_V2
                })
        })
        .unwrap()
        .clone();
    let mut raw = output.clone();
    raw.artifact_ids = vec![raw_id];
    raw.snapshot_id = None;
    fixture.input.inputs.insert("cache_in".into(), raw);
    fixture.pin_and_select();
    assert!(
        matches!(
            fixture.warm().development["objects"],
            DevelopmentCacheSelectionRecordV1::Omitted {
                reason: DevelopmentCacheMissV1::Incompatible,
                ..
            }
        ),
        "matching bytes without an admitted output binding cannot authorize continuation"
    );
    fixture.input.inputs.insert("cache_in".into(), output);
    fixture.pin_and_select();
    let check = r#"set -eu
test "$(cat "$PRIVATE_STATE/checkpoint/step")" = 77
test ! -e "$HOME/conversation"
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":77}]}}\n'
"#;
    assert!(fixture.run(true, check).0.reply.is_ok());
    assert!(
        fixture
            .run(
                true,
                "printf 99 > \"$PRIVATE_STATE/checkpoint/step\"; printf bad > value.txt; exit 1"
            )
            .0
            .reply
            .is_err()
    );
    assert!(
        fixture.run(true, check).0.reply.is_ok(),
        "failed sibling checkpoint and source never carry"
    );
    let (changed, _, _) = fixture.run(
        true,
        r#"set -eu
printf '8\n' > value.txt
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":77}]}}\n'
"#,
    );
    let changed = af_contracts::task::ArtifactInputV1 {
        artifact_ids: changed
            .raw_artifact_ids
            .iter()
            .filter(|id| {
                fixture
                    .cas
                    .get_optional_artifact(id)
                    .ok()
                    .flatten()
                    .is_some_and(|a| a.artifact_type == af_contracts::contract::BUILD_CACHE_V2)
            })
            .cloned()
            .collect(),
        artifact_type: af_contracts::contract::BUILD_CACHE_V2.into(),
        cardinality: af_contracts::PortCardinality::One,
        snapshot_id: None,
    };
    fixture.input.inputs.insert("cache_in".into(), changed);
    fixture.pin_and_select();
    assert!(
        matches!(
            fixture.warm().development["objects"],
            DevelopmentCacheSelectionRecordV1::Omitted {
                reason: DevelopmentCacheMissV1::Incompatible,
                ..
            }
        ),
        "checkpoint cannot implicitly restore producer source edits"
    );
}

#[test]
fn approved_dependency_snapshot_is_cloned_privately_and_worker_mutations_cannot_populate_its_source()
 {
    let mut fixture = Fixture::new();
    fixture.enable_build_cache();
    let manifest = Manifest::new(vec![Entry {
        path: "package".into(),
        kind: EntryKind::File,
        content: fixture.cas.put(b"approved").unwrap(),
        size: 8,
    }])
    .unwrap();
    let manifest_id = fixture
        .cas
        .put_json(&serde_json::to_value(&manifest).unwrap())
        .unwrap();
    fixture
        .binding
        .dependency_sources
        .insert("packages".into(), manifest_id.clone());
    let policy = fixture.request.caches.get_mut("objects").unwrap();
    policy.role = DevelopmentCacheRoleV1::Dependency;
    policy.destination = DevelopmentRootV1::Cache;
    policy.path = "packages".into();
    policy.input = None;
    policy.output = None;
    policy.persistence = DevelopmentCachePersistenceV1::Discard;
    policy.dependency_source = Some("packages".into());
    fixture.request.cache = PrivateStatePolicyV1::AttemptPrivate;
    fixture
        .request
        .writable_roots
        .insert(DevelopmentRootV1::Cache);
    fixture
        .request
        .environment
        .insert("PACKAGES".into(), "${cache}/packages".into());
    fixture.pin_and_select();
    let mutate = r#"set -eu
test "$(cat "$PACKAGES/package")" = approved
printf candidate > "$PACKAGES/package"
printf '{"schema":"af.worker-reply/1","outputs":{"report":[{"answer":77}]}}\n'
"#;
    for _ in 0..2 {
        let (returned, _, _) = fixture.run(true, mutate);
        assert!(returned.reply.is_ok());
        assert!(!returned.raw_artifact_ids.iter().any(|id| {
            fixture
                .cas
                .get_optional_artifact(id)
                .ok()
                .flatten()
                .is_some_and(|envelope| {
                    envelope.artifact_type == af_contracts::contract::BUILD_CACHE_V2
                })
        }));
    }
    assert_eq!(
        fixture.cas.get(&manifest.entries[0].content).unwrap(),
        b"approved"
    );
    assert_eq!(
        cache::read_manifest(
            &fixture.cas,
            &manifest_id,
            &fixture.request.caches["objects"].limits
        )
        .unwrap(),
        manifest
    );
}

#[test]
fn predicted_warm_port_matches_rendered_empty_policy_and_cold_cache_selection() {
    for caches in [false, true] {
        let mut f = Fixture::new();
        let ordinary = f
            .input
            .inputs
            .keys()
            .map(|name| (name.clone(), ()))
            .collect();
        if caches {
            f.enable_build_cache();
            assert!(matches!(
                f.warm().development["objects"],
                DevelopmentCacheSelectionRecordV1::Omitted {
                    reason: DevelopmentCacheMissV1::NoInput,
                    ..
                }
            ));
        } else {
            f.pin_and_select();
        }
        let predicted = af_contracts::task::worker_inputs::project(
            ordinary,
            caches.then_some(()),
            None,
            &BTreeSet::new(),
        );
        let (_, bytes) = f.contract.read_context(&f.cas, &f.context).unwrap();
        let rendered: af_runner::task::WorkerRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            predicted.keys().collect::<Vec<_>>(),
            rendered.inputs.keys().collect::<Vec<_>>()
        );
        assert_eq!(rendered.inputs.contains_key(DEVELOPMENT_WARM_PORT), caches);
    }
}
