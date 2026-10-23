//! `af review plan` refuses what `af review run` would, and its first-run refusals name the fix.
//!
//! Its help promises exactly what run would admit, so a `--provider NODE=ID` whose Provider
//! this machine does not register, or whose node is no model Worker of the pipeline, is refused
//! at plan time, exit 1, in text and under `--json`, in the words run uses. A repository with
//! no `.af/`, a policy revision git does not know, a model Worker without a binding and an
//! empty diff each say what to do next, and the plan ends with a `next` line: the exact
//! `af review run` command once the plan is ready.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// `af` in `repo`, with no Provider CLI on PATH: nothing here starts a Provider.
fn af(repo: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .current_dir(repo)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NO_COLOR", "1")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("AF_PROVIDERS_FILE")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn git(repo: &Path, args: &[&str]) {
    let done = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

/// A repository with one commit and no `.af/`.
fn bare(root: &Path) -> PathBuf {
    let repo = root.join("bare");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Bare\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The bare repository onboarded with Codex reviewers and committed, plus an uncommitted change.
fn onboarded(root: &Path) -> PathBuf {
    let repo = bare(root);
    let onboard = af(
        &repo,
        &[
            "onboard",
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ],
    );
    assert!(onboard.status.success(), "{}", err(&onboard));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "af review policy"]);
    std::fs::write(repo.join("notes.md"), "# Notes\n").unwrap();
    repo
}

/// Register `id` of `kind` with an auth directory of its own in the private home. The directory
/// holds no login and no Provider CLI is installed here, so it is registered `--unverified`
/// (ADR-0197); a token-free plan reads only the registry.
fn register(repo: &Path, id: &str, kind: &str) {
    let home = crate::common::private_home_env()
        .into_iter()
        .find(|(name, _)| *name == "HOME")
        .unwrap()
        .1;
    let auth = home.join(format!("auth-{id}"));
    std::fs::create_dir_all(&auth).unwrap();
    let added = af(
        repo,
        &[
            "provider",
            "add",
            id,
            "--kind",
            kind,
            "--auth-dir",
            auth.to_str().unwrap(),
            "--unverified",
        ],
    );
    assert!(added.status.success(), "{}", err(&added));
}

const PLAN: [&str; 8] = [
    "review",
    "plan",
    "--policy-rev",
    "HEAD",
    "--base",
    "HEAD",
    "--uncommitted",
    "--json",
];

fn plan(repo: &Path, extra: &[&str], json: bool) -> Output {
    let mut args: Vec<&str> = PLAN[..if json { 8 } else { 7 }].to_vec();
    args.extend(extra);
    af(repo, &args)
}

/// The refusal, as text on stderr and as the one `af/error@1` document `--json` prints.
fn refused(repo: &Path, extra: &[&str], expected: &str) {
    let text = plan(repo, extra, false);
    assert_eq!(text.status.code(), Some(1), "{}", err(&text));
    assert!(err(&text).contains(expected), "{}", err(&text));
    let json = plan(repo, extra, true);
    assert_eq!(json.status.code(), Some(1), "{}", err(&json));
    let document: Value = serde_json::from_slice(&json.stdout)
        .unwrap_or_else(|_| panic!("not one document: {}", out(&json)));
    assert_eq!(document["schema"], "af/error@1");
    assert!(
        document["error"].as_str().unwrap().contains(expected),
        "{document}"
    );
}

#[test]
fn plan_refuses_an_unregistered_provider_and_a_node_the_pipeline_does_not_have() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    refused(
        &repo,
        &["--provider", "architecture=nope"],
        "Provider nope not found in this machine's Provider registry",
    );
    refused(
        &repo,
        &["--provider", "architecture=nope"],
        "af provider status lists the registry, and af provider setup nope --kind codex \
         registers it",
    );
    refused(
        &repo,
        &["--provider", "nosuchnode=nope"],
        "Node nosuchnode not found in .af/pipelines/review.toml; model Workers: architecture, \
         correctness",
    );
    // A registered Provider of another kind than the Worker runs is refused too.
    register(&repo, "claude-main", "claude");
    refused(
        &repo,
        &["--provider", "architecture=claude-main"],
        "Provider claude-main is a claude Provider, and node architecture runs codex",
    );
    // `af review run` refuses the same binding in the same words.
    let run = af(
        &repo,
        &[
            "review",
            "run",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--uncommitted",
            "--provider",
            "architecture=nope",
            "--provider",
            "correctness=nope",
        ],
    );
    assert_eq!(run.status.code(), Some(1), "{}", err(&run));
    assert!(
        err(&run).contains("Provider nope not found in this machine's Provider registry"),
        "{}",
        err(&run)
    );
}

#[test]
fn ready_comes_from_the_registry_and_a_ready_plan_names_the_run_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded(&root.path().canonicalize().unwrap());
    // Nothing registered: each missing binding names how to register a Provider of its kind.
    let text = plan(&repo, &[], false);
    assert!(text.status.success(), "{}", err(&text));
    assert!(
        out(&text).contains(
            "provider architecture -> MISSING; fix: no codex Provider is registered: af provider \
             status lists what this machine has, af provider setup codex-main --kind codex \
             --login registers one"
        ),
        "{}",
        out(&text)
    );
    // Registered: the fix is the binding, and next is the plan that binds it.
    register(&repo, "codex-main", "codex");
    let text = out(&plan(&repo, &[], false));
    assert!(
        text.contains("provider architecture -> MISSING; fix: --provider architecture=codex-main"),
        "{text}"
    );
    assert!(
        text.lines().last().unwrap().starts_with(
            "next:    af review plan --policy-rev HEAD --base HEAD --uncommitted --provider"
        ),
        "{text}"
    );
    let bound = [
        "--provider",
        "architecture=codex-main",
        "--provider",
        "correctness=codex-main",
    ];
    let document: Value = serde_json::from_slice(&plan(&repo, &bound, true).stdout).unwrap();
    assert_eq!(document["schema"], "af/review-plan@1");
    for provider in document["providers"].as_array().unwrap() {
        assert_eq!(provider["ready"], true, "{provider}");
    }
    let text = out(&plan(&repo, &bound, false));
    assert_eq!(
        text.lines().last().unwrap(),
        "next:    af review run --policy-rev HEAD --base HEAD --uncommitted --provider \
         'architecture=codex-main' --provider 'correctness=codex-main'",
        "{text}"
    );
}

#[test]
fn first_run_refusals_name_the_fix() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    // No `.af/` at the policy revision.
    let bare = bare(&root);
    let refused = plan(&bare, &[], false);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        err(&refused).contains(
            "this repository has no .af/ authority at HEAD; fix: af onboard (preview), then af \
             onboard --apply, commit, and retry"
        ),
        "{}",
        err(&refused)
    );
    // A policy revision git does not know: its own one line, not the command af ran.
    let repo = onboarded(&root.join("onboarded"));
    let refused = af(
        &repo,
        &[
            "review",
            "plan",
            "--policy-rev",
            "nope",
            "--base",
            "HEAD",
            "--uncommitted",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    let message = err(&refused);
    assert!(
        message.contains(&format!(
            "policy revision nope is not a git revision of {}: fatal: ",
            repo.display()
        )),
        "{message}"
    );
    assert!(!message.contains("rev-parse"), "{message}");
    // No policy revision at all: what it is and the command to try.
    let refused = af(&repo, &["review", "plan"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        err(&refused).contains(
            "af review plan needs --policy-rev REV, the commit whose committed .af/ governs the \
             review (usually HEAD); try: af review plan --policy-rev HEAD --base HEAD \
             --uncommitted"
        ),
        "{}",
        err(&refused)
    );
    // An empty diff says what is empty and the selectors that would fill it.
    std::fs::remove_file(repo.join("notes.md")).unwrap();
    let empty = out(&plan(&repo, &[], false));
    assert!(empty.contains("EMPTY — run will refuse"), "{empty}");
    assert!(
        empty.contains(
            "  empty  the diff from base HEAD to candidate worktree changes no file; change \
             files and review them with --uncommitted, or name other revisions with --base REV \
             and --candidate REV"
        ),
        "{empty}"
    );
    assert!(
        empty
            .lines()
            .last()
            .unwrap()
            .starts_with("next:    change files first; the diff is empty"),
        "{empty}"
    );
}
