//! `.af/af.lock` records the `af` release that wrote it. A newer `af` proceeds and notes the
//! difference, an older `af` refuses, and a lock without the pin stays silent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_config::lock::Lockfile;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

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

fn report(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{}", stderr(output));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A freshly onboarded repository: `.af/` scaffolded by this binary, `.git` present.
fn onboarded_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let created = onboard(
        &repo,
        &["--runner", "codex", "--gate", "check=make check", "--apply"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    repo
}

fn lock_path(repo: &Path) -> PathBuf {
    repo.join(".af/af.lock")
}

fn read_lock(repo: &Path) -> Lockfile {
    Lockfile::from_toml(&std::fs::read_to_string(lock_path(repo)).unwrap()).unwrap()
}

fn set_lock_af_version(repo: &Path, version: Option<&str>) {
    let mut lock = read_lock(repo);
    lock.af_version = version.map(str::to_string);
    std::fs::write(lock_path(repo), lock.to_toml()).unwrap();
}

/// Turns the scaffold into committed authority so `af review plan` can read it from HEAD.
fn commit_all(repo: &Path) {
    std::fs::remove_dir_all(repo.join(".git")).unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "-A"][..],
        &["commit", "-q", "-m", "authority"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
}

fn plan(repo: &Path) -> Output {
    af(&[
        "review",
        "plan",
        "--repo",
        repo.to_str().unwrap(),
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--json",
    ])
}

#[test]
fn apply_and_refresh_pin_the_running_release() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    assert_eq!(read_lock(&repo).af_version.as_deref(), Some(CURRENT));

    let validated = report(&onboard(&repo, &["--json"]));
    assert_eq!(validated["lock_af_version"], CURRENT);
    assert_eq!(validated["warnings"].as_array().unwrap().len(), 0);

    set_lock_af_version(&repo, Some("0.0.1"));
    let refreshed = onboard(&repo, &["--refresh-lock"]);
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    assert_eq!(read_lock(&repo).af_version.as_deref(), Some(CURRENT));
}

#[test]
fn a_newer_af_proceeds_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    set_lock_af_version(&repo, Some("0.0.1"));

    let validated = report(&onboard(&repo, &["--json"]));
    assert_eq!(validated["lock_af_version"], "0.0.1");
    let warnings = validated["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    let warning = warnings[0].as_str().unwrap();
    assert!(
        warning.contains("0.0.1") && warning.contains("--refresh-lock"),
        "{warning}"
    );

    commit_all(&repo);
    let planned = plan(&repo);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let document: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(document["schema"], "af/review-plan@1");
    assert!(
        stderr(&planned).contains("pinned by af 0.0.1"),
        "the note goes to stderr, leaving JSON stdout intact: {}",
        stderr(&planned)
    );
}

#[test]
fn an_older_af_refuses_a_lock_pinned_by_a_newer_af() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    set_lock_af_version(&repo, Some("999.0.0"));

    let validated = onboard(&repo, &[]);
    assert!(!validated.status.success());
    assert!(
        stderr(&validated).contains("999.0.0"),
        "{}",
        stderr(&validated)
    );
    assert!(!repo.join(".af/af.lock.tmp").exists());

    commit_all(&repo);
    let planned = plan(&repo);
    assert!(!planned.status.success());
    assert!(stderr(&planned).contains("999.0.0"), "{}", stderr(&planned));
}

#[test]
fn an_unpinned_lock_stays_silent() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    set_lock_af_version(&repo, None);

    let validated = report(&onboard(&repo, &["--json"]));
    assert!(validated.get("lock_af_version").is_none());
    assert_eq!(validated["warnings"].as_array().unwrap().len(), 0);

    commit_all(&repo);
    let planned = plan(&repo);
    assert!(planned.status.success(), "{}", stderr(&planned));
    assert!(
        !stderr(&planned).contains("pinned by af"),
        "{}",
        stderr(&planned)
    );
}
