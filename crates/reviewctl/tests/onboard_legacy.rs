//! `af onboard` on legacy `.review/` authority: validate without writing, upgrade in place only
//! on `--migrate --apply`, never scaffold a second authority beside it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DEMAND_SET: &str = "review.kernel/DemandSet@1";

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

/// A legacy consumer checkout: the hub fixture plus an empty `.git` entry, which is all
/// onboarding needs.
fn legacy_repo(root: &Path) -> PathBuf {
    let repo = root.join("consumer");
    copy_tree(&hub_fixture(), &repo);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

fn pipeline_path(repo: &Path) -> PathBuf {
    repo.join(".review/pipelines/heavy.toml")
}

/// Rewinds the fixture pipeline to its pre-M4 shape — the exact state `v0.7.0` rejected.
fn make_outdated(repo: &Path) -> String {
    let path = pipeline_path(repo);
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains(DEMAND_SET))
        .expect("the hub fixture declares a DemandSet@1 output");
    let outdated = text.replace(&format!("{line}\n"), "");
    assert_ne!(outdated, text);
    std::fs::write(&path, &outdated).unwrap();
    outdated
}

fn onboard(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["onboard", "--repo", repo.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn current_legacy_policy_is_reported_as_legacy_with_nothing_pending() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let before = std::fs::read_to_string(pipeline_path(&repo)).unwrap();

    let report = json(&onboard(&repo, &["--json"]));
    assert_eq!(report["status"], "legacy");
    assert_eq!(report["profile"], "legacy .review/ authority");
    assert_eq!(report["pipeline"], ".review/pipelines/heavy.toml");
    assert_eq!(
        report["pipelines"][0]["path"],
        ".review/pipelines/heavy.toml"
    );
    assert!(report["pipelines"][0].get("pending").is_none());
    let reviewers: Vec<&str> = report["reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reviewer| reviewer["node"].as_str().unwrap())
        .collect();
    assert_eq!(reviewers, ["correctness"]);
    assert_eq!(report["gates"].as_array().unwrap().len(), 2);
    assert_eq!(report["attempt_tokens"], 300_000);
    assert!(
        !repo.join(".af").exists(),
        "legacy validation never scaffolds .af/"
    );
    assert_eq!(
        std::fs::read_to_string(pipeline_path(&repo)).unwrap(),
        before
    );
}

#[test]
fn outdated_legacy_policy_names_the_pending_upgrade_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let outdated = make_outdated(&repo);

    for args in [&["--json"][..], &["--migrate", "--json"][..]] {
        let report = json(&onboard(&repo, args));
        assert_eq!(report["status"], "legacy-outdated", "{args:?}");
        let pending = report["pipelines"][0]["pending"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].as_str().unwrap().contains(DEMAND_SET));
        assert!(
            report["next_steps"][0]
                .as_str()
                .unwrap()
                .contains("--migrate --apply")
        );
        assert_eq!(
            std::fs::read_to_string(pipeline_path(&repo)).unwrap(),
            outdated,
            "{args:?} must not write"
        );
    }
    assert!(!repo.join(".af").exists());
}

#[test]
fn migrate_apply_rewrites_in_place_and_the_built_af_then_plans_it() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let outdated = make_outdated(&repo);

    let report = json(&onboard(&repo, &["--migrate", "--apply", "--json"]));
    assert_eq!(report["status"], "migrated");
    let applied = report["pipelines"][0]["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 1);
    assert!(report["pipelines"][0].get("pending").is_none());
    let migrated = std::fs::read_to_string(pipeline_path(&repo)).unwrap();
    assert!(migrated.contains(DEMAND_SET));
    for line in outdated.lines() {
        assert!(migrated.contains(line), "migration dropped: {line}");
    }
    assert_eq!(
        std::fs::read_to_string(repo.join(".review/review.lock")).unwrap(),
        std::fs::read_to_string(hub_fixture().join(".review/review.lock")).unwrap(),
        "the legacy lock pins only reviewer packages and must not change"
    );

    let again = json(&onboard(&repo, &["--json"]));
    assert_eq!(again["status"], "legacy");
    assert!(again["pipelines"][0].get("pending").is_none());

    // The migrated policy must be what this very binary accepts: commit it and plan it.
    std::fs::remove_dir_all(repo.join(".git")).unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "-A"][..],
        &["commit", "-q", "-m", "migrated"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=consumer",
                "-c",
                "user.email=consumer@example.invalid",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let plan = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["review", "plan", "--repo", repo.to_str().unwrap()])
        .args(["--pipeline", ".review/pipelines/heavy.toml"])
        .args([
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD",
            "--candidate",
            "HEAD",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(plan.status.success(), "{}", stderr(&plan));
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["schema"], "af/review-plan@1");
}

#[test]
fn apply_without_migrate_never_scaffolds_beside_legacy_authority() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let output = onboard(&repo, &["--apply"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--migrate"), "{}", stderr(&output));
    assert!(!repo.join(".af").exists());

    let refresh = onboard(&repo, &["--refresh-lock"]);
    assert!(!refresh.status.success());
    assert!(
        stderr(&refresh).contains("review.lock"),
        "{}",
        stderr(&refresh)
    );
}

#[test]
fn migrate_applies_only_to_legacy_authority() {
    let root = tempfile::tempdir().unwrap();
    let empty = root.path().join("empty");
    std::fs::create_dir_all(empty.join(".git")).unwrap();
    let none = onboard(&empty, &["--migrate"]);
    assert!(!none.status.success());
    assert!(stderr(&none).contains("has none"), "{}", stderr(&none));

    let current = root.path().join("current");
    std::fs::create_dir_all(current.join(".git")).unwrap();
    let created = onboard(
        &current,
        &["--runner", "codex", "--gate", "check=make check", "--apply"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let refused = onboard(&current, &["--migrate"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("legacy"), "{}", stderr(&refused));

    let conflicting = onboard(&current, &["--migrate", "--gate", "x=true"]);
    assert!(!conflicting.status.success());
    assert!(
        stderr(&conflicting).contains("--migrate"),
        "{}",
        stderr(&conflicting)
    );
}
