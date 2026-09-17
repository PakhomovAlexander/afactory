//! Build Cache capture and clone: the explicitly unsafe carry of a Gate's candidate-built
//! output. The closed layout admits regular files only, refuses links and special files with a
//! typed reason, applies fixed modes, and leaves no bytes behind once the sandbox is sealed.

mod common;

use common::fixture_repo;
use review_core::{BuildCacheKindV1, BuildCacheLimitsV1};
use review_sandbox::{
    CacheErrorKind, Mode, Sandbox, build_cache_environment, capture_build_cache,
    materialize_build_cache, prepare_build_cache_root, remove_materialized_caches,
};
use review_source_git::{Capture, EntryKind};

const KIND: BuildCacheKindV1 = BuildCacheKindV1::CargoTarget;

fn limits() -> BuildCacheLimitsV1 {
    BuildCacheLimitsV1 {
        max_bytes: 1024 * 1024,
        max_entries: 64,
        max_depth: 8,
        max_path_bytes: 256,
    }
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
#[cfg(unix)]
fn a_gate_build_is_captured_cloned_with_fixed_modes_and_removed_before_seal() {
    let (_directory, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let gate = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();

    // The Gate builds into the prepared root exactly as `CARGO_TARGET_DIR` would point it.
    let target = prepare_build_cache_root(KIND, &gate).unwrap();
    let environment = build_cache_environment(KIND, gate.root());
    assert_eq!(
        environment.local,
        vec![("CARGO_TARGET_DIR".to_string(), target.display().to_string())]
    );
    assert_eq!(
        environment.container,
        vec![(
            "CARGO_TARGET_DIR".to_string(),
            "/work/.af-cache/cargo_target".to_string()
        )]
    );
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/libfixture.rlib"), b"compiled").unwrap();
    std::fs::write(target.join("debug/fixture-test"), b"#!/bin/sh\nexit 0\n").unwrap();
    set_mode(&target.join("debug/fixture-test"), 0o755);
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();

    let captured = capture_build_cache(KIND, &gate, &limits(), &cas).unwrap();
    assert_eq!(captured.kind, KIND);
    assert_eq!(captured.entries, 3);
    assert_eq!(captured.bytes, 8 + 17 + 43);
    assert!(cas.verify(&captured.manifest_id).is_ok());
    assert_eq!(captured.content_digest, captured.manifest.content_digest());
    let test_binary = captured.manifest.get("debug/fixture-test").unwrap();
    assert_eq!(test_binary.kind, EntryKind::Executable);
    assert_eq!(
        captured
            .manifest
            .get("debug/deps/libfixture.rlib")
            .unwrap()
            .kind,
        EntryKind::File
    );
    for entry in &captured.manifest.entries {
        assert!(
            cas.verify(&entry.content).is_ok(),
            "{} is a CAS object",
            entry.path
        );
    }
    remove_materialized_caches(&gate).unwrap();
    assert!(gate.seal().unwrap().unchanged());

    // A Worker sandbox receives a clone from the CAS: same bytes, fixed private modes, and the
    // executable bit the build produced. Nothing of it survives the seal.
    let worker = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let cloned =
        materialize_build_cache(KIND, &captured.manifest, &worker, &limits(), &cas).unwrap();
    assert_eq!(cloned.entries, 3);
    assert_eq!(cloned.bytes, captured.bytes);
    let root = worker.root().join(".af-cache/cargo_target");
    assert_eq!(
        std::fs::read(root.join("debug/deps/libfixture.rlib")).unwrap(),
        b"compiled"
    );
    assert_eq!(mode_of(&root.join("debug/deps/libfixture.rlib")), 0o600);
    assert_eq!(mode_of(&root.join("debug/fixture-test")), 0o700);
    assert_eq!(mode_of(&root.join("debug")), 0o700);
    assert_eq!(mode_of(&root), 0o700);
    std::fs::write(
        worker.root().join("src/main.rs"),
        b"fn main() { edited() }\n",
    )
    .unwrap();
    remove_materialized_caches(&worker).unwrap();
    assert!(!worker.root().join(".af-cache").exists());
    let sealed = worker.seal().unwrap();
    assert_eq!(sealed.mutations.paths(), vec!["src/main.rs".to_string()]);
}

#[test]
#[cfg(unix)]
fn a_symlink_in_the_gate_cache_directory_refuses_capture_as_unsafe_content() {
    use std::os::unix::fs::symlink;

    let (directory, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let gate = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let target = prepare_build_cache_root(KIND, &gate).unwrap();
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(target.join("debug/ok"), b"ok").unwrap();
    std::fs::write(directory.path().join("outside"), b"operator secret").unwrap();
    symlink(
        directory.path().join("outside"),
        target.join("debug/linked"),
    )
    .unwrap();

    let error = capture_build_cache(KIND, &gate, &limits(), &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::UnsafeContent);
    assert!(
        error
            .operator_detail()
            .contains("without following links or special files"),
        "{}",
        error.operator_detail()
    );
    assert!(
        !cas.contains(&review_source_git::digest_bytes(b"operator secret")),
        "a linked file is never read, let alone published"
    );
}

#[test]
#[cfg(unix)]
fn a_fifo_in_the_gate_cache_directory_refuses_capture_as_unsafe_content() {
    let (_directory, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let gate = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let target = prepare_build_cache_root(KIND, &gate).unwrap();
    std::fs::write(target.join("ok"), b"ok").unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(target.join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());

    let error = capture_build_cache(KIND, &gate, &limits(), &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::UnsafeContent);
    assert!(
        error
            .operator_detail()
            .contains("not a regular file or directory"),
        "{}",
        error.operator_detail()
    );
}

#[test]
#[cfg(unix)]
fn capture_is_bounded_and_refuses_credential_shaped_and_empty_trees() {
    let (_directory, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let gate = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let target = prepare_build_cache_root(KIND, &gate).unwrap();
    assert_eq!(
        capture_build_cache(KIND, &gate, &limits(), &cas)
            .unwrap_err()
            .kind(),
        CacheErrorKind::SourceUnavailable,
        "a Gate that built nothing carries nothing"
    );

    for index in 0..8 {
        std::fs::create_dir_all(target.join(format!("debug/build/unit-{index}"))).unwrap();
    }
    std::fs::write(target.join("debug/build/unit-0/output"), b"artifact").unwrap();
    let mut bounded = limits();
    bounded.max_entries = 5;
    let error = capture_build_cache(KIND, &gate, &bounded, &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::LimitExceeded);
    assert!(error.operator_detail().contains("filesystem-entry limit"));

    let mut bounded = limits();
    bounded.max_bytes = 4;
    let error = capture_build_cache(KIND, &gate, &bounded, &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::LimitExceeded);
    assert!(error.operator_detail().contains("byte limit"));

    let mut bounded = limits();
    bounded.max_depth = 2;
    let error = capture_build_cache(KIND, &gate, &bounded, &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::LimitExceeded);
    assert!(error.operator_detail().contains("depth limit"));

    std::fs::write(
        target.join("debug/credentials.toml"),
        b"[registry]\ntoken = 'x'\n",
    )
    .unwrap();
    let error = capture_build_cache(KIND, &gate, &limits(), &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::UnsafeContent);
    assert!(error.operator_detail().contains("credential-shaped"));
}

#[test]
fn a_stored_manifest_is_rechecked_before_it_is_cloned() {
    let (_directory, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let worker = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let link = cas.put(b"../../outside").unwrap();
    let manifest = review_source_git::Manifest::new(vec![review_source_git::Entry {
        path: "debug/escape".into(),
        kind: EntryKind::Symlink,
        content: link,
        size: 13,
    }])
    .unwrap();
    let error = materialize_build_cache(KIND, &manifest, &worker, &limits(), &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::UnsafeContent);
    assert!(!worker.root().join(".af-cache").exists());

    let content = cas.put(b"artifact").unwrap();
    let manifest = review_source_git::Manifest::new(vec![review_source_git::Entry {
        path: "debug/artifact".into(),
        kind: EntryKind::File,
        content,
        size: 8,
    }])
    .unwrap();
    let mut bounded = limits();
    bounded.max_bytes = 4;
    let error = materialize_build_cache(KIND, &manifest, &worker, &bounded, &cas).unwrap_err();
    assert_eq!(error.kind(), CacheErrorKind::LimitExceeded);
    assert!(!worker.root().join(".af-cache").exists());
}
