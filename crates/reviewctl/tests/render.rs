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

/// `af review run` requires `HOME`; a sandboxed gate (the kernel's own review pipeline) runs
/// `make check` without one, so the test provides it instead of inheriting the machine's.
fn af(state_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("HOME", state_home.parent().unwrap_or(state_home))
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Re-pin one `.af/` pipeline after editing it: the lock binds pipelines by digest.
fn repin(repo: &Path, name: &str) {
    let lock_path = repo.join(".af/af.lock");
    let mut lock = std::fs::read_to_string(&lock_path)
        .ok()
        .map(|text| review_config::lock::Lockfile::from_toml(&text).unwrap())
        .unwrap_or_else(review_config::lock::Lockfile::empty);
    let bytes = std::fs::read(repo.join(format!(".af/pipelines/{name}.toml"))).unwrap();
    lock.pipelines.insert(
        name.into(),
        review_config::lock::Pin {
            version: "1.0.0".into(),
            digest: review_store::canonical::blob_content_id(&bytes),
        },
    );
    std::fs::write(lock_path, lock.to_toml()).unwrap();
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
        ".af/pipelines/review.toml",
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
        std::fs::read_to_string(hub_fixture().join(".af/workers/correctness/reviewer.md")).unwrap();
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
    assert_eq!(
        view["cap_tokens"], 300_000,
        "the fixture caps every Attempt at 300k"
    );
    assert_eq!(view["fits"], true);
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
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"consumer\"\nmin_af = \"0.6\"\n[defaults]\npipeline = \"check\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/pipelines/check.toml"),
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
    repin(&repo, "check");
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
        ".af/pipelines/check.toml",
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

/// An input that alone exhausts its Worker's Attempt cap is refused before any Gate, Provider
/// admission, or Worker — nothing charged — and `plan` says so first.
#[test]
fn an_input_that_exhausts_its_attempt_cap_is_refused_before_admission() {
    let root = tempfile::tempdir().unwrap();
    let repo = hub_repo_with_a_change(root.path());
    // A 100-token cap on the correctness Worker: the 20 KB prompt cannot fit.
    let pipeline = repo.join(".af/pipelines/review.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    let capped = text.replace(
        "id = \"correctness\"\nkind = \"reviewer\"\n",
        "id = \"correctness\"\nkind = \"reviewer\"\nbudget = { attempt = 100 }\n",
    );
    assert_ne!(capped, text);
    std::fs::write(&pipeline, capped).unwrap();
    repin(&repo, "review");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "tiny cap"]);
    let state_home = root.path().join("state");
    let repo_arg = repo.to_str().unwrap();
    let selectors = [
        "--repo",
        repo_arg,
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~2",
        "--candidate",
        "HEAD",
    ];

    let mut plan_args = vec!["review", "plan"];
    plan_args.extend(selectors);
    plan_args.push("--json");
    let planned = af(&state_home, &plan_args);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["pipeline"]["inputs_fit"], false);
    let reservation = &plan["pipeline"]["reservations"][0];
    assert_eq!(reservation["node"], "correctness");
    assert_eq!(reservation["tokens"], 100);
    assert_eq!(reservation["fits"], false);
    assert!(reservation["input_tokens"].as_u64().unwrap() > 100);

    let campaign_state = root.path().join("campaign");
    let mut run_args = vec!["review", "run", "--campaign", "fit", "--state"];
    run_args.push(campaign_state.to_str().unwrap());
    run_args.extend(selectors);
    run_args.extend(["--provider", "correctness=codex-ambient"]);
    let refused = af(&state_home, &run_args);
    assert!(!refused.status.success(), "the run must refuse");
    let message = stderr(&refused);
    assert!(message.contains("exhausts its Attempt cap"), "{message}");
    assert!(message.contains("`correctness` receives"), "{message}");
    assert!(
        message.contains("Nothing was dispatched or charged"),
        "{message}"
    );
    assert!(message.contains("Scatter"), "{message}");
}
