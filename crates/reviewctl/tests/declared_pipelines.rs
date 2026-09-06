//! A pipeline file under `.af/pipelines/` is declared policy once the lock pins it: `af onboard
//! --refresh-lock` pins every file there (Task pipelines by digest only), `af onboard` refuses
//! an unpinned one, and `af review plan --pipeline` accepts any pinned pipeline whether or not a
//! route or the default ever selects it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn af(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap()
}

fn onboard(repo: &Path, args: &[&str]) -> Output {
    let mut all = vec!["onboard", "--repo", repo.to_str().unwrap()];
    all.extend_from_slice(args);
    af(&all)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
}

fn plan(repo: &Path, pipeline: &str) -> Output {
    af(&[
        "review",
        "plan",
        "--repo",
        repo.to_str().unwrap(),
        "--pipeline",
        pipeline,
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--json",
    ])
}

fn onboarded_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let created = onboard(
        &repo,
        &["--runner", "codex", "--gate", "check=make check", "--apply"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    repo
}

#[test]
fn a_pipeline_file_is_declared_by_its_pin() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    // A second review pipeline nothing routes to, plus a Task pipeline the review graph cannot
    // load; both are files under `.af/pipelines/`.
    std::fs::copy(
        repo.join(".af/pipelines/review.toml"),
        repo.join(".af/pipelines/audit.toml"),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/pipelines/implement.toml"),
        "version = 1\nkind = \"implement\"\nimplementer = \"implementer\"\nevaluator = \"evaluator\"\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "unpinned pipelines"]);

    let unpinned = plan(&repo, ".af/pipelines/audit.toml");
    assert!(!unpinned.status.success());
    assert!(
        stderr(&unpinned).contains("--refresh-lock") && stderr(&unpinned).contains("audit"),
        "{}",
        stderr(&unpinned)
    );
    let validated = onboard(&repo, &[]);
    assert!(!validated.status.success());
    assert!(
        stderr(&validated).contains("audit") && stderr(&validated).contains("not pinned"),
        "{}",
        stderr(&validated)
    );

    let refreshed = onboard(&repo, &["--refresh-lock", "--json"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    let lock = std::fs::read_to_string(repo.join(".af/af.lock")).unwrap();
    for pinned in [
        "[pipelines.audit]",
        "[pipelines.implement]",
        "[pipelines.review]",
    ] {
        assert!(lock.contains(pinned), "{pinned} missing from\n{lock}");
    }
    let validated = onboard(&repo, &["--json"]);
    assert!(validated.status.success(), "{}", stderr(&validated));

    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "pinned"]);
    let planned = plan(&repo, ".af/pipelines/audit.toml");
    assert!(planned.status.success(), "{}", stderr(&planned));
    let document: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(document["schema"], "af/review-plan@1");
    assert_eq!(document["route"]["policy"], "explicit");

    // A pinned pipeline whose file is edited fails its pin until refreshed, like any other.
    let audit = repo.join(".af/pipelines/audit.toml");
    let text = std::fs::read_to_string(&audit).unwrap();
    std::fs::write(&audit, text.replace("max_rounds = 2", "max_rounds = 3")).unwrap();
    let stale = onboard(&repo, &[]);
    assert!(!stale.status.success());
    assert!(
        stderr(&stale).contains("audit") && stderr(&stale).contains("does not match"),
        "{}",
        stderr(&stale)
    );
    let refreshed = onboard(&repo, &["--refresh-lock"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    assert!(onboard(&repo, &[]).status.success());
}
