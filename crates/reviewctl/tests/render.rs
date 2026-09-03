//! `af review render` shows the exact input a Worker would receive, token-free: no Campaign
//! state, no Provider, no Gate, no spend — and byte-for-byte what the adapter would send.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn hub_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/consumers/hub")
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn af(state_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The hub fixture plus one committed change, so the Diff Subject has a real patch.
fn hub_repo_with_a_change(root: &Path) -> PathBuf {
    let repo = root.join("consumer");
    copy_tree(&hub_fixture(), &repo);
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "policy"]);
    std::fs::write(repo.join("notes.md"), "# Notes\n\nrender me exactly\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "a change to review"]);
    repo
}

#[test]
fn a_model_worker_input_is_the_package_contract_and_change_set_with_no_effects() {
    let root = tempfile::tempdir().unwrap();
    let repo = hub_repo_with_a_change(root.path());
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let selectors = [
        "review",
        "render",
        "--repo",
        repo_arg,
        "--pipeline",
        ".review/pipelines/heavy.toml",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~1",
        "--candidate",
        "HEAD",
        "--node",
        "correctness",
    ];

    let mut json_args = selectors.to_vec();
    json_args.push("--json");
    let output = af(&state_home, &json_args);
    assert!(output.status.success(), "{}", stderr(&output));
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["schema"], "af/review-render@1");
    assert_eq!(view["token_free"], true);
    assert_eq!(view["node"], "correctness");
    assert_eq!(view["runner"], "codex");
    assert_eq!(view["transport"], "prompt");
    assert_eq!(view["package"]["name"], "correctness");
    let input = view["input"].as_str().unwrap();
    let instructions =
        std::fs::read_to_string(hub_fixture().join(".review/reviewers/correctness/reviewer.md"))
            .unwrap();
    assert!(
        input.starts_with(&instructions),
        "the verified package bytes lead"
    );
    assert!(input.contains("## Diff Subject Change Set"), "{input}");
    assert!(
        input.contains("render me exactly"),
        "the exact patch is in the input"
    );
    assert!(input.contains("notes.md"), "{input}");
    assert_eq!(view["bytes"].as_u64().unwrap(), input.len() as u64);
    assert_eq!(view["manifest"]["rendered_bytes"], view["bytes"]);
    for effect in [
        "campaign_state",
        "gates",
        "provider_admission",
        "worker_dispatch",
        "token_spend",
    ] {
        assert_eq!(view["external_effects"][effect], false, "{effect}");
    }
    let omitted = view["not_rendered"].as_array().unwrap();
    assert!(
        omitted
            .iter()
            .any(|item| item.as_str().unwrap().contains("attempt authority")),
        "{omitted:?}"
    );
    assert!(
        !state_home.join("af").exists(),
        "render must create no Campaign state"
    );

    // Without --json the raw input is stdout, byte for byte; the header is stderr.
    let raw = af(&state_home, &selectors);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert_eq!(raw.stdout, input.as_bytes());
    assert!(stderr(&raw).contains("review render (token-free; no Campaign state)"));
    assert!(stderr(&raw).contains("omitted"));
}

#[test]
fn a_command_worker_receives_the_typed_document_and_non_workers_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("consumer");
    std::fs::create_dir_all(repo.join(".review/pipelines")).unwrap();
    std::fs::write(repo.join(".review/review.lock"), "version = 1\n").unwrap();
    std::fs::write(
        repo.join(".review/pipelines/check.toml"),
        r#"version = 2

[subject]
kind = "whole-tree"

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "lint"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
[nodes.runner]
program = "/bin/sh"
args = [{ value = "-c" }, { value = "cat >/dev/null; printf '%s' '{}'" }]

[[nodes]]
id = "gather"
kind = "gather"
inputs = ["lint"]
outputs = ["reports"]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "lint", port = "gate" }

[[edges]]
from = { node = "lint", port = "result" }
to = { node = "gather", port = "lint" }

[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#,
    )
    .unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "policy"]);
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let base = [
        "review",
        "render",
        "--repo",
        repo_arg,
        "--pipeline",
        ".review/pipelines/check.toml",
        "--policy-rev",
        "HEAD",
        "--json",
    ];

    let mut args = base.to_vec();
    args.extend(["--node", "lint"]);
    let output = af(&state_home, &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["transport"], "json");
    assert_eq!(view["runner"], "sh");
    assert!(
        view.get("package").is_none(),
        "a command Worker has no package"
    );
    let document: serde_json::Value =
        serde_json::from_str(view["input"].as_str().unwrap()).unwrap();
    assert!(document.is_object(), "{document}");
    assert!(
        view["not_rendered"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().starts_with("gate:")),
        "{}",
        view["not_rendered"]
    );

    let mut args = base.to_vec();
    args.extend(["--node", "gather"]);
    let refused = af(&state_home, &args);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("not a reviewer Worker"),
        "{}",
        stderr(&refused)
    );

    let missing = af(&state_home, &base);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("--node"), "{}", stderr(&missing));
}
