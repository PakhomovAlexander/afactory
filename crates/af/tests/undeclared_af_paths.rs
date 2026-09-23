//! Undeclared `.af/` paths of a captured Snapshot: reported at plan time, recorded in the
//! delivery receipt, and refused before delivery when the project's captured policy says so.
//!
//! Nothing here removes a path. Every assertion about a delivered worktree is that the stray
//! files are still in it, byte for byte, exactly as the Snapshot had them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/task_cli.rs"]
mod task_cli;

const AF: &str = env!("CARGO_BIN_EXE_af");

/// Two files a coordinator would once have handed to the next Task through the checkout.
const CANDIDATE: &str = "--- a/x\n+++ b/x\n";
const REVIEWS: &str = "{\"reports\":[]}\n";

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap()
}

fn af(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(AF)
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap()
}

fn document(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The total size of the two stray files, which is what the classification must report.
fn strewn_bytes() -> u64 {
    (CANDIDATE.len() + REVIEWS.len()) as u64
}

/// The pagination fixture, optionally carrying a `[delivery]` policy in `.af/af.toml`.
fn fixture(root: &Path, policy: Option<&str>) -> (PathBuf, PathBuf) {
    let (repo, state) = task_cli::fixture_named(root, "pagination");
    if let Some(policy) = policy {
        let table = format!("[delivery]\nundeclared_af_paths = \"{policy}\"\n");
        let text = format!(
            "version = 1\n[project]\nname = \"pagination\"\nmin_af = \"0.8\"\n\
             [defaults]\npipeline = \"review\"\n{table}"
        );
        std::fs::write(repo.join(".af/af.toml"), text).unwrap();
        commit(&repo, "delivery policy");
    }
    (repo, state)
}

/// The same fixture with two undeclared paths committed under the authority directory.
fn strewn(root: &Path, policy: &str) -> (PathBuf, PathBuf) {
    let (repo, state) = fixture(root, Some(policy));
    std::fs::create_dir_all(repo.join(".af/tasks/x")).unwrap();
    std::fs::write(repo.join(".af/tasks/x/candidate.patch"), CANDIDATE).unwrap();
    std::fs::write(repo.join(".af/tasks/x/reviews.json"), REVIEWS).unwrap();
    commit(&repo, "stray Task artifacts");
    (repo, state)
}

fn commit(repo: &Path, message: &str) {
    for args in [vec!["add", "-A"], vec!["commit", "-qm", message]] {
        let output = git(repo, &args);
        assert!(output.status.success(), "git {args:?}: {}", err(&output));
    }
}

fn plan(repo: &Path, state: &Path) -> Output {
    af(repo, state, &["task", "plan", "--file", "ticket.json"])
}

fn plan_and_run(repo: &Path, state: &Path) {
    let planned = plan(repo, state);
    assert_eq!(planned.status.code(), Some(0), "{}", err(&planned));
    let args = ["task", "run", "--execute", "pagination-cli"];
    let run = af(repo, state, &args);
    assert_eq!(run.status.code(), Some(0), "{}", err(&run));
}

fn deliver(repo: &Path, state: &Path, worktree: &Path, branch: &str) -> Output {
    af(
        repo,
        state,
        &[
            "task",
            "deliver",
            "pagination-cli",
            "--branch",
            branch,
            "--worktree",
            worktree.to_str().unwrap(),
            "--confirm",
            "pagination-cli",
        ],
    )
}

#[test]
fn plan_reports_every_undeclared_path_on_stderr_and_in_the_json_document() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = strewn(root.path(), "warn");
    let planned = plan(&repo, &state);
    assert_eq!(planned.status.code(), Some(0), "{}", err(&planned));

    let warning = err(&planned);
    let counted = "2 undeclared path(s) under .af/";
    assert!(warning.contains(counted), "{warning}");
    assert!(warning.contains(".af/tasks/x/candidate.patch"), "{warning}");
    assert!(warning.contains(".af/tasks/x/reviews.json"), "{warning}");
    assert!(warning.contains("not a refusal"), "{warning}");

    // Advisory only: the plan document is the ordinary one, plus the whole typed list the
    // single advisory line could only summarize.
    let planned = document(&planned);
    assert_eq!(planned["attempts"], 0);
    assert_eq!(planned["schema"], "af/task-inspection@11");
    let group = &planned["undeclared_af_paths"];
    let expected = [".af/tasks/x/candidate.patch", ".af/tasks/x/reviews.json"];
    assert_eq!(group["paths"], serde_json::json!(expected));
    assert_eq!(group["bytes"], strewn_bytes());
}

#[test]
fn a_repository_whose_authority_tree_is_declared_reports_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path(), None);
    let planned = plan(&repo, &state);
    assert_eq!(planned.status.code(), Some(0), "{}", err(&planned));
    let warning = err(&planned);
    assert!(!warning.contains("undeclared path(s)"), "{warning}");
    let shown = document(&planned);
    assert!(shown.get("undeclared_af_paths").is_none(), "{shown}");
}

#[test]
fn delivery_under_warn_records_the_paths_and_still_delivers_all_of_them() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = strewn(root.path(), "warn");
    plan_and_run(&repo, &state);
    let worktree = root.path().join("delivered");
    let delivered = deliver(&repo, &state, &worktree, "task/warned");
    assert_eq!(delivered.status.code(), Some(0), "{}", err(&delivered));

    let receipt = document(&delivered);
    assert_eq!(receipt["schema"], "af/task-delivery@1");
    assert_eq!(receipt["outcome"]["kind"], "delivered");
    let group = &receipt["undeclared_af_paths"];
    let expected = [".af/tasks/x/candidate.patch", ".af/tasks/x/reviews.json"];
    assert_eq!(group["paths"], serde_json::json!(expected));
    assert_eq!(group["bytes"], strewn_bytes());
    // `ignored_paths` is a different question and this package did not change its answer.
    assert_eq!(receipt["ignored_paths"], serde_json::json!([]));

    // Recorded, never acted on: both paths are in the worktree, byte for byte.
    let patch = worktree.join(".af/tasks/x/candidate.patch");
    assert_eq!(std::fs::read_to_string(patch).unwrap(), CANDIDATE);
    let reviews = worktree.join(".af/tasks/x/reviews.json");
    assert_eq!(std::fs::read_to_string(reviews).unwrap(), REVIEWS);

    // The receipt is public inspection output and survives the round trip through the Store.
    let shown = af(&repo, &state, &["task", "show", "pagination-cli"]);
    assert_eq!(shown.status.code(), Some(0), "{}", err(&shown));
    assert_eq!(document(&shown)["delivery"], receipt);
}

#[test]
fn delivery_under_refuse_stops_before_a_branch_a_worktree_or_a_record() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = strewn(root.path(), "refuse");
    plan_and_run(&repo, &state);
    let worktree = root.path().join("refused");
    let refused = deliver(&repo, &state, &worktree, "task/refused");
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));

    let error = document(&refused);
    assert_eq!(error["schema"], "af/error@1");
    let reason = error["error"].as_str().unwrap();
    assert!(reason.contains(".af/tasks/x/candidate.patch"), "{reason}");
    assert!(reason.contains(".af/tasks/x/reviews.json"), "{reason}");
    assert!(reason.contains("no prepared delivery record"), "{reason}");

    assert!(!worktree.exists(), "a refused delivery made a worktree");
    let branch = git(&repo, &["rev-parse", "--verify", "task/refused"]);
    assert!(!branch.status.success(), "a refused delivery made a branch");
    let shown = af(&repo, &state, &["task", "show", "pagination-cli"]);
    assert_eq!(shown.status.code(), Some(0), "{}", err(&shown));
    let shown = document(&shown);
    assert_eq!(shown["result"]["acceptance"], "satisfied");
    assert!(shown.get("delivery").is_none(), "{shown}");
}

#[test]
fn the_captured_policy_belongs_to_the_project_policy_identity() {
    let root = tempfile::tempdir().unwrap();
    let identity = |name: &str, policy: Option<&str>| -> String {
        let (repo, state) = fixture(&root.path().join(name), policy);
        let planned = plan(&repo, &state);
        assert_eq!(planned.status.code(), Some(0), "{}", err(&planned));
        let shown = af(&repo, &state, &["task", "explain", "pagination-cli"]);
        assert_eq!(shown.status.code(), Some(0), "{}", err(&shown));
        let explained = document(&shown);
        let policy = &explained["plan"]["authority"]["policy_id"];
        policy.as_str().expect("no captured policy").to_owned()
    };
    let silent = identity("silent", None);
    let warn = identity("warn", Some("warn"));
    let refuse = identity("refuse", Some("refuse"));
    assert_eq!(silent, warn, "the default must not move a policy identity");
    assert_ne!(warn, refuse, "the policy must bind the admitted plan");
}
