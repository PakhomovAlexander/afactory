//! One real `codex exec` through the whole stack: digest-verified package, kernel sandbox,
//! supervised process, JSONL parse, cost receipt. Spends real tokens (roughly 15k, mostly the
//! CLI's own preamble), needs the operator's codex credentials, and therefore is `#[ignore]`d
//! everywhere except `make review-kernel-codex-smoke` — where, as with the container probes,
//! a missing provider is a hard failure rather than a skip.

use std::time::Duration;

use review_config::lock::{Lockfile, Registry};
use review_runner::ReviewerAdapter;
use review_sandbox::{Mode, Sandbox};
use review_source_git::{Capture, Repo};
use review_store::Cas;

#[test]
#[ignore = "spends real tokens; run via make review-kernel-codex-smoke"]
fn one_real_review_parses_and_reports_its_cost() {
    let dir = tempfile::tempdir().unwrap();

    // A tiny repository for the reviewer to look at.
    let repo_path = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(repo_path.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo_path.join("src/main.rs"), b"fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo_path)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "smoke@example.invalid"]);
    git(&["config", "user.name", "Smoke"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);

    // A minimal locked package. The prompt asks for the cheapest honest answer the contract
    // allows: an approval with no findings.
    let registry_root = dir.path().join("registry");
    let package_dir = registry_root.join("smoke");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::write(
        package_dir.join("reviewer.toml"),
        "name = \"smoke\"\nversion = \"1.0.0\"\n\n[runner]\nprogram = \"codex\"\nargs = []\n",
    )
    .unwrap();
    std::fs::write(
        package_dir.join("reviewer.md"),
        "This is a smoke test of the review plumbing. Do not read any files and do not use \
         any tools. Approve with an empty findings list.\n",
    )
    .unwrap();
    let registry = Registry::new([&registry_root]);
    let mut lockfile = Lockfile::empty();
    lockfile.reviewers.insert(
        "smoke".to_string(),
        Lockfile::pin("smoke", &registry).unwrap(),
    );
    let package = lockfile
        .resolve_for_subject("smoke", &registry, review_core::SubjectKind::WholeTree)
        .unwrap();

    // The real credentials, granted explicitly — the supervisor's environment rebuild would
    // otherwise leave codex logged out.
    let codex_home = std::env::var("CODEX_HOME")
        .unwrap_or_else(|_| format!("{}/.codex", std::env::var("HOME").expect("HOME is set")));

    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();

    let adapter =
        review_runner_codex::CodexAdapter::from_package(&package, Duration::from_secs(600))
            .unwrap()
            .with_codex_home(codex_home);
    let returned = adapter
        .invoke(&cas, sandbox.root(), &Default::default())
        .unwrap();

    assert!(
        returned.cost_tokens > 0,
        "a real model reports what it spent"
    );
    assert!(
        cas.contains(&returned.raw_artifact),
        "the raw stream is kept"
    );
    eprintln!(
        "smoke: verdict={:?}, findings={}, cost={} tokens",
        returned.output.verdict,
        returned.output.findings.len(),
        returned.cost_tokens
    );
}
