//! Worker warm layers, package P2: a trusted-local Gate's candidate-built output is captured as
//! an explicitly unsafe `BuildCache@1`, carried through the Round's Warm Set into the sandbox
//! of every reviewer that declares the kind, pointed at through `CARGO_TARGET_DIR`, and removed
//! before seal so the sealed diff is byte-identical to a cold run. A link or special file in
//! the Gate's cache directory refuses capture with a recorded reason, and the safe policy
//! refuses the handoff before any Worker dispatch.

mod support;

use std::path::PathBuf;

use review_check::{Arg, CheckDefinition, Command};
use review_config::{ConfigError, Definition};
use review_core::event::AttemptAdmittedPayloadV1;
use review_core::{
    BuildCacheCapturedPayloadV1, BuildCacheDropReasonV1, BuildCacheKindV1, BuildCacheLimitsV1,
    BuildCacheRefusalReasonV1, BuildCacheTrustV1, BuildCacheV1, EventType, RunEvent, WarmLayerV1,
    WarmSetSelectedPayloadV1, WarmSetV1,
};
use review_pipeline::Kernel;
use review_source_git::{Capture, Repo};
use review_store::{Cas, EventStore};

const TRUSTED_GATE: &str = r#"
version = 3
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
build_caches = ["cargo_target"]
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "tdd"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
warm = { notes = false, build_cache = ["cargo_target"] }
runner = { program = "/bin/true" }
[[nodes]]
id = "reader"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["tdd", "reader"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["set"]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "tdd", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "reader", port = "gate" }
[[edges]]
from = { node = "tdd", port = "result" }
to = { node = "gather", port = "tdd" }
[[edges]]
from = { node = "reader", port = "result" }
to = { node = "gather", port = "reader" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const APPROVE: &str =
    r#"'{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}'"#;

/// A repository whose Gate "build" is a shell script writing into the declared target.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "p2@example.invalid"]);
    git(&["config", "user.name", "P2"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);
    (dir, repo, home)
}

fn shell(script: &str) -> Command {
    Command::new(
        "/bin/sh",
        vec![Arg::literal("-c"), Arg::literal(script.to_string())],
    )
}

fn build_check(script: &str) -> CheckDefinition {
    CheckDefinition::new("build", shell(script))
}

/// The Gate's "build": compile artifacts and a test binary into `$CARGO_TARGET_DIR`.
fn gate_build() -> CheckDefinition {
    build_check(
        "test -n \"$CARGO_TARGET_DIR\" && mkdir -p \"$CARGO_TARGET_DIR/debug/deps\" \
         && printf compiled > \"$CARGO_TARGET_DIR/debug/deps/libfixture.rlib\" \
         && printf '#!/bin/sh\\nexit 0\\n' > \"$CARGO_TARGET_DIR/debug/fixture-test\" \
         && chmod 755 \"$CARGO_TARGET_DIR/debug/fixture-test\"",
    )
}

/// A TDD reviewer that reuses the Gate build instead of rebuilding, then edits one file.
fn warm_tdd_reviewer() -> Command {
    shell(&format!(
        "test -f \"$CARGO_TARGET_DIR/debug/deps/libfixture.rlib\" \
         && test \"$(cat \"$CARGO_TARGET_DIR/debug/deps/libfixture.rlib\")\" = compiled \
         && \"$CARGO_TARGET_DIR/debug/fixture-test\" \
         && case \"$CARGO_TARGET_DIR\" in */.af-cache/cargo_target) ;; *) exit 7 ;; esac \
         && test -f .af-cache/cargo_target/debug/deps/libfixture.rlib \
         && printf edited > edited.txt && printf '%s\\n' {APPROVE}"
    ))
}

/// The same reviewer without a carried build: it edits the same file and answers the same.
fn cold_tdd_reviewer() -> Command {
    shell(&format!(
        "test -z \"${{CARGO_TARGET_DIR:-}}\" && test ! -e .af-cache \
         && printf edited > edited.txt && printf '%s\\n' {APPROVE}"
    ))
}

fn reading_reviewer() -> Command {
    shell(&format!(
        "test -z \"${{CARGO_TARGET_DIR:-}}\" && test ! -e .af-cache && cat src/main.rs > /dev/null \
         && printf '%s\\n' {APPROVE}"
    ))
}

fn events_of(store: &EventStore, event_type: EventType) -> Vec<RunEvent> {
    store
        .replay("run")
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .collect()
}

/// The full sealed mutation set of the selected Attempt of `node`, as the CAS id of its
/// complete `{added, modified, deleted}` artifact plus the decoded value.
fn sealed_mutations(
    cas: &Cas,
    store: &EventStore,
    node: &str,
) -> (String, serde_json::Value, serde_json::Value) {
    let admitted = events_of(store, EventType::AttemptAdmittedV1)
        .into_iter()
        .find(|event| event.node_id.as_deref() == Some(node))
        .expect("admitted Attempt");
    let payload: AttemptAdmittedPayloadV1 = serde_json::from_value(admitted.payload).unwrap();
    assert_eq!(payload.selection, "selected");
    let provenance = cas
        .get_json(payload.provenance_artifact.as_deref().unwrap())
        .unwrap();
    let artifact = provenance["sandbox_mutations"]["artifact"]
        .as_str()
        .unwrap()
        .to_string();
    let mutations = cas.get_json(&artifact).unwrap();
    (artifact, mutations, provenance)
}

#[test]
fn a_tdd_reviewer_reuses_the_gate_build_and_seals_the_same_diff_as_a_cold_run() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(TRUSTED_GATE).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let manifest = snapshot.manifest;
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &manifest,
        TRUSTED_GATE,
    );

    // The first Attempt of the warm node cannot start: its program does not exist, so the
    // Attempt is released and the Round stays open. The Gate ran once and captured its build.
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap()
    .with_checks(vec![gate_build()])
    .with_reviewer("tdd", Command::new("/nonexistent/tdd-reviewer", vec![]))
    .with_reviewer("reader", reading_reviewer());
    let first = loaded.run(&kernel).unwrap();
    assert!(!first.complete(), "{:?}", first.outcomes);
    assert!(kernel.gate_decision("gate").unwrap().passed());
    drop(kernel);

    let captured = events_of(&store, EventType::BuildCacheCapturedV1);
    assert_eq!(captured.len(), 1, "one capture per Gate per Round");
    assert_eq!(captured[0].node_id.as_deref(), Some("gate"));
    assert_eq!(
        captured[0].causation_id.as_deref(),
        Some(authority.round_event_id())
    );
    let capture: BuildCacheCapturedPayloadV1 =
        serde_json::from_value(captured[0].payload.clone()).unwrap();
    capture.validate().unwrap();
    assert_eq!(capture.gate_node, "gate");
    assert_eq!(
        capture.gate_attempt_id, None,
        "the legacy Gate is not a Task Attempt"
    );
    assert_eq!(capture.head_snapshot_id, authority.head_snapshot_id());
    assert_eq!(capture.kind, BuildCacheKindV1::CargoTarget);
    assert_eq!(capture.limits, BuildCacheLimitsV1::default_v1());
    assert_eq!(capture.refused, None);
    assert_eq!(capture.entries, 2);
    assert_eq!(capture.bytes, 8 + 17);
    let build_cache_id = capture.build_cache_artifact_id.clone().unwrap();
    assert!(captured[0].artifact_refs.contains(&build_cache_id));
    let envelope = cas.get_artifact(&build_cache_id).unwrap();
    assert_eq!(
        envelope.artifact_type,
        review_core::contract::BUILD_CACHE_V1
    );
    assert_eq!(
        envelope.subject_snapshot_id.as_deref(),
        Some(authority.head_snapshot_id())
    );
    let cache: BuildCacheV1 = serde_json::from_value(envelope.payload).unwrap();
    cache.validate().unwrap();
    assert_eq!(cache.trust, BuildCacheTrustV1::CandidateBuilt);
    assert_eq!(cache.gate_node, "gate");
    assert_eq!(cache.entries, 2);
    assert!(captured[0].artifact_refs.contains(&cache.manifest_id));
    let cache_manifest: review_source_git::Manifest =
        serde_json::from_value(cas.get_json(&cache.manifest_id).unwrap()).unwrap();
    assert_eq!(cache_manifest.content_digest(), cache.content_digest);
    assert_eq!(
        cache_manifest.get("debug/fixture-test").unwrap().kind,
        review_source_git::EntryKind::Executable
    );
    let gate_decision = events_of(&store, EventType::GateDecisionV1);
    assert!(
        captured[0].sequence < gate_decision[0].sequence,
        "the capture is durable before the Gate publishes its decision"
    );

    // The warm node's Warm Set carries the build cache and was recorded before its first
    // dispatch; the cold node has none.
    let selections = events_of(&store, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 1);
    assert_eq!(selections[0].node_id.as_deref(), Some("tdd"));
    let first_dispatch = events_of(&store, EventType::AttemptDispatchedV1)
        .into_iter()
        .find(|event| event.node_id.as_deref() == Some("tdd"))
        .expect("the warm node was dispatched");
    assert!(selections[0].sequence < first_dispatch.sequence);
    let selection: WarmSetSelectedPayloadV1 =
        serde_json::from_value(selections[0].payload.clone()).unwrap();
    assert_eq!(selection.layers, vec![WarmLayerV1::BuildCache]);
    assert_eq!(
        selection.source_attempt_id, None,
        "Round one has no previous Attempt"
    );
    let warm_set: WarmSetV1 = serde_json::from_value(
        cas.get_artifact(&selection.warm_set_artifact_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    warm_set.validate().unwrap();
    assert_eq!(warm_set.round, 1);
    assert_eq!(
        warm_set.build_cache_artifact_id.as_deref(),
        Some(build_cache_id.as_str())
    );
    assert!(
        events_of(&store, EventType::AttemptReleasedV1)
            .iter()
            .any(|event| event.node_id.as_deref() == Some("tdd"))
    );

    // The resumed Round replays the Gate from its receipts, reuses the recorded Warm Set and
    // clones the build cache from the CAS: the Gate sandbox is long gone.
    let resumed = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_checks(vec![build_check("exit 1")])
    .with_reviewer("tdd", warm_tdd_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&resumed).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    resumed
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    drop(resumed);
    assert_eq!(events_of(&store, EventType::BuildCacheCapturedV1).len(), 1);
    assert_eq!(events_of(&store, EventType::WarmSetSelectedV1).len(), 1);
    assert!(!repo_path.join(".af-cache").exists());
    assert!(!repo_path.join("edited.txt").exists());

    let (warm_artifact, warm_mutations, warm_provenance) = sealed_mutations(&cas, &store, "tdd");
    assert_eq!(
        warm_mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []}),
        "the cloned build cache never enters the sealed diff"
    );
    let entries = warm_provenance["context_manifest"]["entries"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"warm_set"), "{names:?}");
    let build_cache_entry = entries
        .iter()
        .find(|entry| entry["name"] == "warm_build_cache")
        .expect("the manifest names the carried build cache");
    assert_eq!(build_cache_entry["artifact_id"], build_cache_id);
    assert_eq!(build_cache_entry["rendered_bytes"], 0);
    let (_, reader_mutations, reader_provenance) = sealed_mutations(&cas, &store, "reader");
    assert_eq!(
        reader_mutations,
        serde_json::json!({"added": [], "modified": [], "deleted": []})
    );
    assert!(
        reader_provenance["context_manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"] != "warm_build_cache"),
        "a node that declares no kind receives nothing"
    );

    // The same pipeline without the carry: the cold reviewer edits the same file, and the
    // sealed diff is byte-identical.
    let cold_definition = TRUSTED_GATE
        .replace("build_caches = [\"cargo_target\"]\n", "")
        .replace(
            "warm = { notes = false, build_cache = [\"cargo_target\"] }\n",
            "",
        );
    let cold_loaded = Definition::from_toml(&cold_definition)
        .unwrap()
        .load()
        .unwrap();
    let cold_workspace = tempfile::tempdir().unwrap();
    let cold_cas = Cas::open(cold_workspace.path().join("cas")).unwrap();
    let mut cold_store = EventStore::open(cold_workspace.path().join("events.sqlite")).unwrap();
    let cold_snapshot = Capture::new(&repo, &cold_cas).committed("HEAD").unwrap();
    let cold_authority = support::test_round_authority_for_pipeline(
        &cold_cas,
        &mut cold_store,
        "run",
        &cold_snapshot.manifest,
        &cold_definition,
    );
    let cold = Kernel::from_loaded(
        &cold_cas,
        &mut cold_store,
        "run",
        cold_snapshot.manifest,
        &cold_loaded,
        cold_authority,
    )
    .unwrap()
    .with_checks(vec![build_check("exit 0")])
    .with_reviewer("tdd", cold_tdd_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let cold_report = cold_loaded.run(&cold).unwrap();
    assert!(cold_report.complete(), "{:?}", cold_report.outcomes);
    drop(cold);
    assert!(events_of(&cold_store, EventType::BuildCacheCapturedV1).is_empty());
    assert!(events_of(&cold_store, EventType::WarmSetSelectedV1).is_empty());
    let (cold_artifact, cold_mutations, cold_provenance) =
        sealed_mutations(&cold_cas, &cold_store, "tdd");
    assert_eq!(cold_mutations, warm_mutations);
    assert_eq!(
        cold_artifact, warm_artifact,
        "the sealed diff of the warm Attempt is byte-identical to the cold run's"
    );
    assert_eq!(
        cold_provenance["sandbox_mutations"],
        warm_provenance["sandbox_mutations"]
    );
    assert!(
        cold_provenance["context_manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"] != "warm_build_cache"),
    );
}

#[test]
fn a_symlink_in_the_gate_cache_directory_refuses_capture_with_a_recorded_reason() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(TRUSTED_GATE).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &snapshot.manifest,
        TRUSTED_GATE,
    );
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &loaded,
        authority,
    )
    .unwrap()
    .with_checks(vec![build_check(
        "mkdir -p \"$CARGO_TARGET_DIR/debug\" && printf built > \"$CARGO_TARGET_DIR/debug/artifact\" \
         && ln -s /etc/hosts \"$CARGO_TARGET_DIR/debug/linked\"",
    )])
    .with_reviewer("tdd", cold_tdd_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(
        kernel.gate_decision("gate").unwrap().passed(),
        "a refused capture never changes the Gate verdict"
    );
    kernel
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    drop(kernel);

    let captured = events_of(&store, EventType::BuildCacheCapturedV1);
    assert_eq!(captured.len(), 1);
    assert!(captured[0].artifact_refs.iter().all(|artifact| {
        cas.get_optional_artifact(artifact)
            .ok()
            .flatten()
            .is_none_or(|envelope| envelope.artifact_type != review_core::contract::BUILD_CACHE_V1)
    }));
    let capture: BuildCacheCapturedPayloadV1 =
        serde_json::from_value(captured[0].payload.clone()).unwrap();
    capture.validate().unwrap();
    assert_eq!(capture.build_cache_artifact_id, None);
    assert_eq!(
        capture.refused,
        Some(BuildCacheRefusalReasonV1::UnsafeContent)
    );
    assert_eq!((capture.entries, capture.bytes), (0, 0));

    let selections = events_of(&store, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 1);
    let selection: WarmSetSelectedPayloadV1 =
        serde_json::from_value(selections[0].payload.clone()).unwrap();
    assert!(selection.layers.is_empty());
    let warm_set: WarmSetV1 = serde_json::from_value(
        cas.get_artifact(&selection.warm_set_artifact_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(warm_set.build_cache_artifact_id, None);
    assert_eq!(
        warm_set.build_cache_dropped,
        Some(BuildCacheDropReasonV1::Refused)
    );
    let (_, mutations, _) = sealed_mutations(&cas, &store, "tdd");
    assert_eq!(
        mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
}

#[test]
fn a_safe_pipeline_refuses_the_handoff_before_any_worker_dispatch() {
    // At load: the Gate binding is the safe policy, so the declaration itself is refused.
    let container = TRUSTED_GATE.replace(
        "provider = \"trusted_local\"\nrequired_isolation = \"none\"",
        &format!(
            "provider = \"container\"\nrequired_isolation = \"container\"\nimage = \"ghcr.io/example/gate@sha256:{}\"",
            "b".repeat(64)
        ),
    );
    assert!(matches!(
        Definition::from_toml(&container).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("refused under the safe policy")
    ));

    // Without a Gate that declares the kind, a reviewer cannot ask for it either.
    let undeclared = TRUSTED_GATE.replace("build_caches = [\"cargo_target\"]\n", "");
    assert!(matches!(
        Definition::from_toml(&undeclared).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("declares it in `build_caches`")
    ));
}
