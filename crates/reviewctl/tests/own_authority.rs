//! This repository's own `.af/` authority, held to what `af onboard` does with it. The committed
//! lock must be exactly what `af onboard --refresh-lock` writes for the tree — a source build
//! pins no `[af]` release and keeps an existing pin, so the comparison is byte for byte — and
//! plain `af onboard` must validate the authority. A Worker package or pipeline edited without a
//! re-lock fails `make check` here, not at the next Campaign's authority load.

use std::path::{Path, PathBuf};

mod common;
use common::{AF, Sandbox, err, out};

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of this repository's `.af/` in a directory `af onboard` accepts as a repository root.
fn authority_copy(sandbox: &Sandbox) -> PathBuf {
    let repo = sandbox.path("kernel");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    copy_tree(&workspace_root().join(".af"), &repo.join(".af"));
    repo
}

fn onboard(sandbox: &Sandbox, repo: &Path, flags: &[&str]) -> std::process::Output {
    sandbox
        .command(Path::new(AF))
        .env("AF_SELF_OFFLINE", "1")
        .args(["onboard", "--repo"])
        .arg(repo)
        .args(flags)
        .arg("--json")
        .output()
        .unwrap()
}

#[test]
fn the_committed_lock_is_what_refresh_lock_writes_for_this_tree() {
    let sandbox = Sandbox::new();
    let repo = authority_copy(&sandbox);

    let refreshed = onboard(&sandbox, &repo, &["--refresh-lock"]);
    assert!(refreshed.status.success(), "{}", err(&refreshed));
    let report: serde_json::Value = serde_json::from_str(&out(&refreshed)).unwrap();
    assert_eq!(report["status"], "lock_refreshed", "{report}");
    assert!(
        report["lock_af_version"].is_null(),
        "a source build pins no af release, so the kernel's own lock carries no [af] table: {report}"
    );

    let committed = std::fs::read_to_string(workspace_root().join(".af/af.lock")).unwrap();
    let written = std::fs::read_to_string(repo.join(".af/af.lock")).unwrap();
    assert!(
        committed == written,
        ".af/af.lock is not what `af onboard --refresh-lock` writes for this tree — fix: \
         `cargo run -p reviewctl --bin af -- onboard --refresh-lock`, then commit the lock with \
         the authority edit that changed it.\n--- committed\n{committed}\n--- refreshed\n{written}"
    );
}

#[test]
fn the_committed_authority_validates_as_the_first_party_form() {
    let sandbox = Sandbox::new();
    let repo = authority_copy(&sandbox);

    let validated = onboard(&sandbox, &repo, &[]);
    assert!(validated.status.success(), "{}", err(&validated));
    let report: serde_json::Value = serde_json::from_str(&out(&validated)).unwrap();
    assert_eq!(report["status"], "onboarded", "{report}");
    assert_eq!(report["pipeline"], ".af/pipelines/review.toml", "{report}");

    // The review pipeline gates on the project's own deterministic check, as the audit pipeline
    // does, and the pipeline is the v3 form `af onboard --apply` emits.
    let gates: Vec<String> = report["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gate| {
            format!(
                "{} {}",
                gate["program"].as_str().unwrap(),
                gate["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|arg| arg.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    assert_eq!(
        gates,
        ["bash scripts/verify.sh", "bash scripts/markdownlint.sh"],
        "{report}"
    );
    let pipeline =
        std::fs::read_to_string(workspace_root().join(".af/pipelines/review.toml")).unwrap();
    let definition = review_config::Definition::from_toml(&pipeline).unwrap();
    assert_eq!(definition.version, 3);
    assert!(definition.gate.is_some(), "an explicit [gate] binding");
    assert!(
        definition.checks.iter().all(|check| check.required),
        "every Gate Check is required"
    );
}
