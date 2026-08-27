//! Binary-owned, token-free onboarding: preview first, atomic creation, then exact validation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_config::lock::{Lockfile, Pin};

fn repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

fn af(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["onboard", "--repo", repo.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn preview_detects_gate_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    std::fs::write(repo.join("Makefile"), "check:\n\t@true\n").unwrap();

    let output = af(&repo, &["--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "preview");
    assert_eq!(report["gates"][0]["program"], "make");
    assert_eq!(report["reviewers"].as_array().unwrap().len(), 2);
    assert!(!repo.join(".af").exists());
}

#[test]
fn apply_creates_valid_authority_and_never_overwrites_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--runner",
            "codex",
            "--gate",
            "check=make check",
            "--apply",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "created");
    assert!(repo.join(".af/README.md").is_file());
    assert!(repo.join(".af/pipelines/review.toml").is_file());
    assert!(repo.join(".af/workers/correctness/reviewer.md").is_file());
    assert!(
        std::fs::read_to_string(repo.join(".af/workers/architecture/reviewer.toml"))
            .unwrap()
            .contains("program = \"codex\"")
    );

    let validate = af(&repo, &["--json"]);
    assert!(validate.status.success(), "{}", stderr(&validate));
    let validated: serde_json::Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(validated["status"], "onboarded");

    let second_apply = af(&repo, &["--apply"]);
    assert!(!second_apply.status.success());
    assert!(stderr(&second_apply).contains("never overwrites"));
}

#[test]
fn tampering_fails_closed_and_explicit_refresh_preserves_unrelated_pins() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let create = af(&repo, &["--gate", "check=make check", "--apply"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let lock_path = repo.join(".af/af.lock");
    let mut lock = Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    let unrelated = Pin {
        version: "9.9.9".into(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    lock.workers.insert("unused".into(), unrelated.clone());
    std::fs::write(&lock_path, lock.to_toml()).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".af/pipelines/review.toml"))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".af/workers/correctness/reviewer.md"))
        .unwrap()
        .write_all(b"\nproject addition\n")
        .unwrap();

    let validate = af(&repo, &[]);
    assert!(!validate.status.success());
    assert!(stderr(&validate).contains("does not match `.af/af.lock`"));

    let refresh = af(&repo, &["--refresh-lock", "--json"]);
    assert!(refresh.status.success(), "{}", stderr(&refresh));
    let report: serde_json::Value = serde_json::from_slice(&refresh.stdout).unwrap();
    assert_eq!(report["status"], "lock_refreshed");
    let refreshed = Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    assert_eq!(refreshed.workers.get("unused"), Some(&unrelated));

    let validate = af(&repo, &[]);
    assert!(validate.status.success(), "{}", stderr(&validate));
}

#[test]
fn absent_gate_is_a_refusal_and_help_is_self_contained() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());
    let refused = af(&repo, &["--apply"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("no unambiguous acceptance Gate"));
    assert!(!repo.join(".af").exists());

    let help = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["onboard", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("never calls a model"));
    assert!(stdout(&help).contains("--refresh-lock"));
}
