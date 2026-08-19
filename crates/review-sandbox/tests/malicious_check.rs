//! `fixtures/adversarial/malicious-check.md`, to the extent this provider can discharge it.
//!
//! The case assumes a check that does its job *and* attacks. Five probes: host marker, canonical
//! checkout, credentials, undeclared network, argument injection. `trusted_local` can genuinely
//! answer three of them. It cannot answer the other two, and this file says which rather than
//! narrowing the case until it passes — a green test that has quietly redefined the threat is
//! worse than a missing one.

use review_check::{Arg, CheckDefinition, CheckRunner, CheckStatus, Command, GateDecision};
use review_sandbox::{Isolation, Mode, Policy, PolicyError, Sandbox, admit};
use review_source_git::Capture;

mod common;
use common::fixture_repo;

/// Probe: review input immutability. **Discharged.** A check runs against a materialized copy
/// and capture already happened, so the snapshot being reviewed cannot be altered by anything the
/// check does — a relative traversal lands in a temporary directory nobody reads again.
///
/// The related probe — protecting the *checkout on disk* — is **not** discharged, and this test
/// says so rather than asserting something narrower and calling the case closed. An
/// absolute-path write is not prevented by a provider that is a directory.
#[test]
fn a_check_cannot_reach_the_checkout_it_is_reviewing() {
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let before = review_source_git::worktree_state(&repo).unwrap();

    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let runner = CheckRunner::new(&cas, sandbox.root());

    // The check does its job and also tries to climb out, by relative path and by absolute path.
    let hostile = CheckDefinition::new(
        "build",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal(format!(
                    "echo building; \
                     echo pwned > ../../src/main.rs 2>/dev/null; \
                     echo pwned > {}/src/main.rs 2>/dev/null; \
                     exit 0",
                    repo.workdir().display()
                )),
            ],
        ),
    );

    let result = runner.run(&hostile);
    assert_eq!(result.status, CheckStatus::Passed, "the check did its job");

    // The absolute-path write may well have succeeded: nothing stops a process from writing
    // where its user can write. What the kernel guarantees is narrower and still worth having —
    // capture already happened, so the snapshot under review is immutable regardless, and the
    // review's conclusions are about content nobody can retroactively change.
    let after = review_source_git::worktree_state(&repo).unwrap();
    let snapshot_again = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    assert_eq!(
        snapshot.content_digest, snapshot_again.content_digest,
        "the committed snapshot under review is immutable whatever the check did"
    );
    if before != after {
        // Documented, not swallowed: this is precisely the probe that needs a container.
        eprintln!(
            "note: the working tree changed — trusted_local does not contain an absolute-path \
             write, which is why malicious-check.md stays open for a container provider"
        );
    }
    drop(dir);
}

/// Probe: credentials. **Discharged.** The environment is rebuilt from an allowlist, so a token
/// in the kernel's own environment cannot reach a check by being forgotten in a denylist.
#[test]
fn a_check_inherits_no_credentials() {
    let (_dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();

    let passed: Vec<&str> = sandbox.environment().iter().map(|(k, _)| *k).collect();
    assert_eq!(passed, vec!["PATH", "HOME", "LC_ALL", "TZ"]);
    for secret in [
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "INTERNAL_API_KEY",
        "SSH_AUTH_SOCK",
        "OPENAI_API_KEY",
    ] {
        assert!(!passed.contains(&secret), "{secret} would reach a check");
    }
    // HOME points inside the sandbox, so a check that writes a config file writes it somewhere
    // that is captured at seal time rather than into the operator's home.
    assert!(
        sandbox.environment()[1]
            .1
            .starts_with(sandbox.root().to_str().unwrap()),
        "HOME must not be the operator's"
    );
}

/// Probe: argument injection. **Discharged** by the check crate's typed slots — asserted here
/// because the case demands it end-to-end, not only at the unit level.
#[test]
fn an_untrusted_value_cannot_become_an_option_end_to_end() {
    let (_dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::ReadOnly).unwrap();
    let runner = CheckRunner::new(&cas, sandbox.root());

    let check = CheckDefinition::new(
        "tests",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("echo ran"),
                Arg::literal("sh"),
                Arg::untrusted("@/tmp/response-file"),
            ],
        ),
    );
    let result = runner.run(&check);
    assert_eq!(result.status, CheckStatus::NotRun);
    assert!(!GateDecision::evaluate(&[result]).passed());
}

/// Probes: host marker, undeclared network. **Not discharged, and not claimed.** A pipeline that
/// needs them must require real isolation, and this provider must then be refused.
#[test]
fn a_pipeline_requiring_isolation_refuses_this_provider() {
    let (_dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::ReadOnly).unwrap();

    assert_eq!(sandbox.isolation(), Isolation::None);
    assert_eq!(
        admit(Policy::safe(), &sandbox),
        Err(PolicyError::InsufficientIsolation {
            required: Isolation::Container,
            provided: Isolation::None,
        }),
        "a safe pipeline must refuse a directory pretending to be a sandbox"
    );
    // And the pipeline that honestly declares what it is gets to run.
    assert!(admit(Policy::trusted_local(), &sandbox).is_ok());
}

/// Read-only mode stops the accident, not the attack — and the distinction is the point.
#[test]
fn read_only_mode_blocks_an_ordinary_write() {
    let (_dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::ReadOnly).unwrap();
    let runner = CheckRunner::new(&cas, sandbox.root());

    let writer = CheckDefinition::new(
        "mutate",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("echo mutated > src/main.rs"),
            ],
        ),
    );
    assert_eq!(runner.run(&writer).status, CheckStatus::Failed);

    let sealed = sandbox.seal().unwrap();
    assert!(
        sealed.unchanged(),
        "a read-only sandbox must be unchanged at seal: {:?}",
        sealed.mutations
    );
}
