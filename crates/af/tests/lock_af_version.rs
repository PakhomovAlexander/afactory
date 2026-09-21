//! `.af/af.lock` records the `af` release that wrote it and the bytes that release has. A
//! receipted binary pins itself with every target's digest; a source build pins nothing and keeps
//! an existing pin. A newer `af` proceeds and notes the difference, an older `af` refuses, and a
//! lock without the pin stays silent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use review_config::lock::{AfPin, Lockfile};

mod common;
use common::{AF, Sandbox, Signer, TARGET, VERSION};

/// An empty self-managed layout, so the source build under test never dispatches to a release
/// that happens to be installed on the developer's machine.
fn empty_layout() -> &'static Path {
    static LAYOUT: OnceLock<tempfile::TempDir> = OnceLock::new();
    LAYOUT.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

fn af(args: &[&str]) -> Output {
    Command::new(AF)
        .args(args)
        .env("AF_SELF_OFFLINE", "1")
        .env("HOME", empty_layout())
        .env("XDG_DATA_HOME", empty_layout().join("data"))
        .env("XDG_BIN_HOME", empty_layout().join("bin"))
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

/// A freshly onboarded repository: `.af/` scaffolded by the (receipt-less) test binary.
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
    lock.af = version.map(AfPin::version_only);
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
fn a_source_build_pins_nothing_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    assert_eq!(read_lock(&repo).af, None);

    let validated = report(&onboard(&repo, &["--json"]));
    assert!(validated.get("lock_af_version").is_none());
    assert_eq!(validated["warnings"].as_array().unwrap().len(), 0);

    // A refresh by a source build keeps whatever pin is there.
    set_lock_af_version(&repo, Some("0.7.1"));
    let refreshed = report(&onboard(&repo, &["--refresh-lock", "--json"]));
    assert_eq!(read_lock(&repo).af_version(), Some("0.7.1"));
    assert!(
        refreshed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("no install receipt")),
        "{refreshed}"
    );
}

#[test]
fn a_receipted_release_pins_itself_with_every_published_digest() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    // The "release" of the version under test: its SHA256SUMS lists this target and one more,
    // signed, as every release from 0.8.0 on is.
    let digest = sandbox.publish(VERSION, false);
    sandbox.sign(VERSION, &signer, None);
    let real = sandbox.adopt_real_binary_with(&digest);
    let repo = sandbox.path("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let created = sandbox
        .command(&real)
        .args(["onboard", "--repo"])
        .arg(&repo)
        .args([
            "--runner",
            "codex",
            "--gate",
            "check=make check",
            "--apply",
            "--json",
        ])
        .output()
        .unwrap();
    let created = report(&created);
    assert_eq!(created["lock_af_version"], VERSION);
    let targets = created["lock_af_targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2, "{targets:?}");
    let lock = read_lock(&repo);
    let pin = lock.af.as_ref().unwrap();
    assert_eq!(pin.version, VERSION);
    assert_eq!(
        pin.digest_for(TARGET),
        Some(format!("sha256:{digest}").as_str())
    );
    assert!(pin.digest_for("other-target").is_some());
    let text = std::fs::read_to_string(lock_path(&repo)).unwrap();
    assert!(text.contains("[af]\nversion = "), "{text}");
    assert!(text.contains("[af.digests]"), "{text}");

    // Without the release source, the receipt still pins this target's bytes, with a warning.
    set_lock_af_version(&repo, None);
    let refreshed = sandbox
        .command(&real)
        .args(["onboard", "--repo"])
        .arg(&repo)
        .args(["--refresh-lock", "--json"])
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    let refreshed = report(&refreshed);
    assert_eq!(refreshed["lock_af_targets"], serde_json::json!([TARGET]));
    assert!(
        refreshed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("digest for")),
        "{refreshed}"
    );
    let pin = read_lock(&repo).af.unwrap();
    assert_eq!(
        pin.digest_for(TARGET),
        Some(format!("sha256:{digest}").as_str())
    );
}

#[test]
fn a_release_whose_published_digest_disagrees_with_the_receipt_is_not_pinned() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish(VERSION, false);
    sandbox.sign(VERSION, &signer, None);
    // Installed from bytes the release no longer lists (a re-uploaded asset, or a mirror).
    let real = sandbox.adopt_real_binary_with(&"f".repeat(64));
    let repo = sandbox.path("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let refused = sandbox
        .command(&real)
        .args(["onboard", "--repo"])
        .arg(&repo)
        .args(["--runner", "codex", "--gate", "check=make check", "--apply"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("re-uploaded") && stderr(&refused).contains("af self install"),
        "{}",
        stderr(&refused)
    );
    assert!(!repo.join(".af").exists(), "nothing is written on refusal");
}

#[test]
fn a_newer_af_proceeds_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let repo = onboarded_repo(root.path());
    set_lock_af_version(&repo, Some("0.0.1"));

    let validated = report(&onboard(&repo, &["--json"]));
    assert_eq!(validated["lock_af_version"], "0.0.1");
    let warnings = validated["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings.iter().any(|warning| {
            let warning = warning.as_str().unwrap();
            warning.contains("0.0.1") && warning.contains("--refresh-lock")
        }),
        "{warnings:?}"
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
