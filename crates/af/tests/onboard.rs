//! Binary-owned, token-free onboarding: preview first, atomic creation, then exact validation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_config::lock::{Lockfile, Pin};

fn repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
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

/// A repository directory whose name is not UTF-8, or `None` where the filesystem refuses to
/// hold one. The constraint is the filesystem's own encoding rule, not the platform: APFS
/// rejects with `EILSEQ` the byte that ext4 stores without complaint. Skipping there keeps a
/// Mac from failing these tests against a limitation of its disk rather than anything in `af`;
/// Linux CI still exercises them.
fn non_utf8_repository(root: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let path = root.join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir(&path).ok().map(|()| path)
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
    assert!(
        report["topology"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line == "correctness.result -> gather.correctness")
    );
    // A source build (this test binary) pins no af release, and the preview says so.
    let warnings = report["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].as_str().unwrap().contains("no install receipt"),
        "{warnings:?}"
    );
    assert!(!repo.join(".af").exists());

    let apply = report["next_steps"][0].as_str().unwrap();
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--gate", "check=make check"])
    );
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
        "the printed command must preserve the executing release: {apply}"
    );
    assert_eq!(words.last().map(String::as_str), Some("--apply"));
}

#[test]
fn preview_preserves_explicit_gate_quoting_in_the_printed_apply_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--runner",
            "codex",
            "--gate",
            "lint=sh -c 'echo ready'",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let apply = report["next_steps"][0].as_str().unwrap();
    assert!(apply.contains("--runner codex"), "{apply}");
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    let gate = words
        .windows(2)
        .find(|pair| pair[0] == "--gate")
        .map(|pair| pair[1].as_str());
    assert_eq!(
        gate,
        Some("lint=sh -c 'echo ready'"),
        "the printed command must be shell-copyable and semantically exact: {apply}"
    );
    assert_eq!(words.last().map(String::as_str), Some("--apply"));
}

#[test]
fn preview_preserves_the_explicit_af_release_in_the_printed_apply_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo(root.path());

    let output = af(
        &repo,
        &[
            "--gate",
            "check=true",
            "--af",
            env!("CARGO_PKG_VERSION"),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let apply = report["next_steps"][0].as_str().unwrap();
    let command = apply.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
        "the copied apply must run under the release that produced the preview: {apply}"
    );
}

#[test]
fn preview_rejects_a_repository_path_that_cannot_be_copied_as_utf8() {
    let root = tempfile::tempdir().unwrap();
    let Some(repo) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .arg("onboard")
        .arg("--repo")
        .arg(&repo)
        .args(["--gate", "check=true", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not valid UTF-8"),
        "{}",
        stderr(&output)
    );
    assert!(!repo.join(".af").exists());
}

#[test]
fn existing_authority_can_be_validated_under_a_non_utf8_repository_path() {
    let root = tempfile::tempdir().unwrap();
    let source = repo(root.path());
    std::fs::write(source.join("Makefile"), "check:\n\t@true\n").unwrap();
    let created = af(&source, &["--apply", "--json"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let Some(target) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::remove_dir(&target).unwrap();
    std::fs::rename(&source, &target).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .arg("onboard")
        .args(["--repo", "."])
        .arg("--json")
        .current_dir(&target)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "onboarded");
}

#[test]
fn preview_rejects_a_non_utf8_canonical_repository_reached_as_dot() {
    let root = tempfile::tempdir().unwrap();
    let Some(repo) = non_utf8_repository(root.path()) else {
        return;
    };
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["onboard", "--repo", ".", "--gate", "check=true", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not valid UTF-8"),
        "{}",
        stderr(&output)
    );
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
    let readme = std::fs::read_to_string(repo.join(".af/README.md")).unwrap();
    assert!(readme.contains("## Worker data authorization"));
    assert!(readme.contains("should not ask for additional per-Worker"));
    assert!(readme.contains("Plain `af review run` is light"));
    assert!(readme.contains("Do not open a follow-up review Campaign"));
    assert!(readme.contains("human-requested `--heavy`"));
    assert!(repo.join(".af/pipelines/review.toml").is_file());
    let pipeline = std::fs::read_to_string(repo.join(".af/pipelines/review.toml")).unwrap();
    assert_eq!(pipeline.matches("demands = \"required\"").count(), 2);
    let pipeline_value: toml::Value = toml::from_str(&pipeline).unwrap();
    assert_eq!(pipeline_value["version"].as_integer(), Some(3));
    assert_eq!(
        pipeline_value["gate"]["provider"].as_str(),
        Some("trusted_local")
    );
    assert_eq!(
        pipeline_value["gate"]["required_isolation"].as_str(),
        Some("none")
    );
    assert_eq!(
        pipeline_value["gate"]["mode"].as_str(),
        Some("ephemeral-write")
    );
    assert!(repo.join(".af/workers/correctness/reviewer.md").is_file());
    let architecture_manifest =
        std::fs::read_to_string(repo.join(".af/workers/architecture/reviewer.toml")).unwrap();
    assert!(architecture_manifest.contains("program = \"codex\""));
    let architecture_manifest: toml::Value = toml::from_str(&architecture_manifest).unwrap();
    assert_eq!(
        architecture_manifest["runner"]["args"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "the Codex adapter owns the exec/sandbox/stdin flags"
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
fn tampering_fails_closed_and_explicit_refresh_prunes_absent_pins() {
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
    let pipeline_path = repo.join(".af/pipelines/review.toml");
    let mut pipeline: toml::Value =
        toml::from_str(&std::fs::read_to_string(&pipeline_path).unwrap()).unwrap();
    pipeline.as_table_mut().unwrap().remove("budgets");
    std::fs::write(&pipeline_path, toml::to_string_pretty(&pipeline).unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".af/workers/correctness/reviewer.md"))
        .unwrap()
        .write_all(b"\nproject addition\n")
        .unwrap();

    let validate = af(&repo, &[]);
    assert!(!validate.status.success());
    assert!(stderr(&validate).contains("stale Worker pin `unused`"));

    let refresh = af(&repo, &["--refresh-lock", "--json"]);
    assert!(refresh.status.success(), "{}", stderr(&refresh));
    let report: serde_json::Value = serde_json::from_slice(&refresh.stdout).unwrap();
    assert_eq!(report["status"], "lock_refreshed");
    assert!(report["attempt_tokens"].is_null());
    assert!(report["run_tokens"].is_null());
    let refreshed = Lockfile::from_toml(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    assert!(!refreshed.workers.contains_key("unused"));

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
    assert!(stdout(&help).contains("does not ask for per-call confirmation"));
    assert!(stdout(&help).contains("Review Campaigns are light by default"));
    assert!(stdout(&help).contains("Use `--heavy` only when a human"));
    assert!(stdout(&help).contains("--refresh-lock"));
}
