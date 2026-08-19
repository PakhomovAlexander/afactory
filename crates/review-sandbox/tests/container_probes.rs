//! The three `malicious-check.md` probes that need real isolation, run against a live daemon.
//!
//! `trusted_local` could honestly answer three of the case's five probes; these are the other
//! two plus the absolute-write half of the checkout probe — the ones where "nothing stops a
//! process" was the whole finding. Here something does: the container has one bind (the
//! sandbox), no network, and no inherited environment, so each probe fails at a boundary
//! rather than by convention.
//!
//! Every test is `#[ignore]` because the ordinary test run must not depend on a container
//! runtime. They are *not* optional where they do run: `make review-kernel-container-probes`
//! (locally and in CI) invokes them explicitly, and a missing daemon is then a hard failure,
//! never a skip — an unrun probe that reports nothing is how a guarantee quietly stops being
//! tested.
//!
//! Each escape probe is paired with the control at the bottom, which proves the same provider
//! does run work and does land writes in the sandbox — so the probes fail for isolation
//! reasons, not because the container is broken.

use review_check::{Arg, Command};
use review_sandbox::{Availability, ContainerProvider, Mode, Sandbox};
use review_source_git::Capture;

mod common;
use common::fixture_repo;

/// Invoked explicitly means required: refusal here is a failure, not a skip.
fn provider() -> ContainerProvider {
    let provider = ContainerProvider::detect();
    assert!(
        matches!(provider.availability(), Availability::Usable { .. }),
        "these probes were invoked explicitly and need a live runtime: {}",
        provider.availability().reason()
    );
    provider
}

/// Probe: host marker. A file planted outside the sandbox is unreadable and unmodifiable —
/// the absolute path simply names nothing inside the container.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn a_host_marker_is_out_of_reach() {
    let provider = provider();
    let host = tempfile::tempdir().unwrap();
    let marker = host.path().join("review-host-marker");
    std::fs::write(&marker, "untouched").unwrap();
    let sandbox = tempfile::tempdir().unwrap();

    let read = provider
        .exec(sandbox.path(), "/bin/cat", &[marker.display().to_string()])
        .unwrap();
    assert!(!read.status.success(), "the marker must be unreadable");
    assert!(
        !String::from_utf8_lossy(&read.stdout).contains("untouched"),
        "no marker bytes may cross the boundary"
    );

    let write = provider
        .exec(
            sandbox.path(),
            "/bin/sh",
            &[
                "-c".to_string(),
                format!("echo pwned > {}", marker.display()),
            ],
        )
        .unwrap();
    assert!(!write.status.success(), "the marker must be unwritable");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "untouched",
        "and unmodified on the host"
    );
}

/// Probe: the canonical checkout on disk. `malicious_check.rs` could only prove the *snapshot*
/// immutable and printed a note when the working tree changed; here the assertion is the real
/// one — the same hostile command, and the checkout does not change.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn an_absolute_write_cannot_reach_the_checkout() {
    let provider = provider();
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let before = review_source_git::worktree_state(&repo).unwrap();

    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let output = provider
        .exec(
            sandbox.root(),
            "/bin/sh",
            &[
                "-c".to_string(),
                format!(
                    "echo building; echo pwned > {}/src/main.rs; exit 0",
                    repo.workdir().display()
                ),
            ],
        )
        .unwrap();
    assert!(output.status.success(), "the check did its job");

    let after = review_source_git::worktree_state(&repo).unwrap();
    assert_eq!(
        before, after,
        "an absolute-path write must not reach the checkout"
    );
    drop(dir);
}

/// Probe: undeclared network. `--network=none` leaves nothing to connect with — no route out
/// and no resolver, so the refusal is immediate, not a timeout. `timeout` guards the assertion
/// against hanging instead of failing.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn an_undeclared_connection_is_refused() {
    let provider = provider();
    let sandbox = tempfile::tempdir().unwrap();

    let connect = provider
        .exec(
            sandbox.path(),
            "/usr/bin/timeout",
            &[
                "10".to_string(),
                "/bin/bash".to_string(),
                "-c".to_string(),
                "echo probe > /dev/tcp/1.1.1.1/80".to_string(),
            ],
        )
        .unwrap();
    assert!(
        !connect.status.success(),
        "a direct connection must fail: {}",
        String::from_utf8_lossy(&connect.stderr)
    );

    let resolve = provider
        .exec(
            sandbox.path(),
            "/usr/bin/timeout",
            &[
                "10".to_string(),
                "/usr/bin/getent".to_string(),
                "hosts".to_string(),
                "debian.org".to_string(),
            ],
        )
        .unwrap();
    assert!(
        !resolve.status.success(),
        "name resolution must fail: {}",
        String::from_utf8_lossy(&resolve.stdout)
    );
}

/// The control, and the integration point: a project check — typed slots, `resolve()`, exactly
/// what the check runner validates — executes *inside* the container, does its work, and the
/// work lands in the sandbox bind. Without this, the probes above could pass because the
/// container runs nothing at all.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn a_check_command_runs_contained_and_its_work_lands_in_the_sandbox() {
    let provider = provider();
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();

    let check = Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            Arg::literal("cat src/main.rs > copied.rs && echo checked"),
        ],
    );
    let argv = check.resolve().unwrap();
    let output = provider
        .exec(sandbox.root(), &check.program, &argv)
        .unwrap();

    assert!(
        output.status.success(),
        "the check must run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("checked"));
    assert_eq!(
        std::fs::read_to_string(sandbox.root().join("copied.rs")).unwrap(),
        "fn main() {}\n",
        "work done in /work lands in the sandbox on the host"
    );
    drop(dir);
}
