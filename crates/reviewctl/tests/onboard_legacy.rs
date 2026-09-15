//! `af onboard` on legacy `.review/` authority (ADR-0043): preview the `.af/` it becomes, write
//! it only on `--migrate --apply`, never scaffold a second authority beside it, and refuse the
//! old layout for new Campaigns once the move is possible.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DEMAND_SET: &str = "review.kernel/DemandSet@1";

/// The hub's `.review/` policy as it was pinned before `.af/` — kept as a test fixture only.
fn legacy_fixture() -> PathBuf {
    fixtures::workspace_root().join("crates/reviewctl/tests/fixtures/legacy-hub")
}

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::copy_tree;

/// A legacy consumer checkout: the fixture plus an empty `.git` entry, which is all onboarding
/// needs.
fn legacy_repo(root: &Path) -> PathBuf {
    let repo = root.join("consumer");
    copy_tree(&legacy_fixture(), &repo);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    repo
}

fn legacy_pipeline(repo: &Path) -> PathBuf {
    repo.join(".review/pipelines/heavy.toml")
}

/// Rewinds the fixture pipeline to its pre-M4 shape — the exact state `v0.7.0` rejected.
fn make_outdated(repo: &Path) -> String {
    let path = legacy_pipeline(repo);
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains(DEMAND_SET))
        .expect("the fixture declares a DemandSet@1 output");
    let outdated = text.replace(&format!("{line}\n"), "");
    assert_ne!(outdated, text);
    std::fs::write(&path, &outdated).unwrap();
    outdated
}

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

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
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

fn plan(repo: &Path, pipeline: Option<&str>) -> Output {
    let mut args = vec!["review", "plan", "--repo", repo.to_str().unwrap()];
    if let Some(pipeline) = pipeline {
        args.extend(["--pipeline", pipeline]);
    }
    args.extend([
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD",
        "--candidate",
        "HEAD",
        "--json",
    ]);
    af(&args)
}

#[test]
fn legacy_policy_previews_its_af_conversion_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let before = std::fs::read_to_string(legacy_pipeline(&repo)).unwrap();

    for args in [&["--json"][..], &["--migrate", "--json"][..]] {
        let report = json(&onboard(&repo, args));
        assert_eq!(report["status"], "legacy", "{args:?}");
        assert_eq!(report["profile"], "legacy .review/ authority -> .af/");
        assert_eq!(report["pipeline"], ".af/pipelines/review.toml");
        assert_eq!(
            report["pipelines"][0]["path"],
            ".review/pipelines/heavy.toml"
        );
        assert_eq!(report["pipelines"][0]["to"], ".af/pipelines/review.toml");
        assert!(report["pipelines"][0].get("applied").is_none());
        let reviewers: Vec<&str> = report["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reviewer| reviewer["node"].as_str().unwrap())
            .collect();
        assert_eq!(reviewers, ["correctness"]);
        assert_eq!(report["gates"].as_array().unwrap().len(), 2);
        assert_eq!(report["attempt_tokens"], 300_000);
        assert_eq!(
            report["files"],
            serde_json::json!([
                ".af/af.lock",
                ".af/af.toml",
                ".af/pipelines/review.toml",
                ".af/workers/correctness/reviewer.md",
                ".af/workers/correctness/reviewer.toml",
            ])
        );
        let next = report["next_steps"][0].as_str().unwrap();
        let command = next.split('`').nth(1).unwrap();
        let words = shell_words::split(command).unwrap();
        assert!(words.iter().any(|word| word == "--migrate"));
        assert!(
            words
                .windows(2)
                .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
            "the copied migration must preserve the executing release: {next}"
        );
        assert_eq!(words.last().map(String::as_str), Some("--apply"));
        assert!(
            !repo.join(".af").exists(),
            "{args:?}: a preview never writes .af/"
        );
    }
    assert_eq!(
        std::fs::read_to_string(legacy_pipeline(&repo)).unwrap(),
        before
    );
}

#[test]
fn legacy_preview_preserves_the_explicit_af_release_in_the_migration_command() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let report = json(&onboard(
        &repo,
        &["--af", env!("CARGO_PKG_VERSION"), "--json"],
    ));
    let next = report["next_steps"][0].as_str().unwrap();
    let command = next.split('`').nth(1).unwrap();
    let words = shell_words::split(command).unwrap();
    assert!(
        words
            .windows(2)
            .any(|pair| pair == ["--af", env!("CARGO_PKG_VERSION")]),
        "the copied migration must run under the release that produced the preview: {next}"
    );
}

#[test]
fn an_outdated_legacy_pipeline_is_upgraded_on_the_way() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let outdated = make_outdated(&repo);

    let report = json(&onboard(&repo, &["--migrate", "--json"]));
    assert_eq!(report["status"], "legacy");
    let applied = report["pipelines"][0]["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 1);
    assert!(applied[0].as_str().unwrap().contains(DEMAND_SET));
    assert!(!repo.join(".af").exists());

    let report = json(&onboard(&repo, &["--migrate", "--apply", "--json"]));
    assert_eq!(report["status"], "migrated");
    let migrated = std::fs::read_to_string(repo.join(".af/pipelines/review.toml")).unwrap();
    assert!(migrated.contains(DEMAND_SET));
    for line in outdated.lines() {
        assert!(migrated.contains(line), "migration dropped: {line}");
    }
    assert_eq!(
        std::fs::read_to_string(legacy_pipeline(&repo)).unwrap(),
        outdated,
        "the legacy tree is left for the consumer to delete"
    );
}

#[test]
fn migrate_apply_writes_af_and_the_built_af_then_plans_only_the_new_layout() {
    let root = tempfile::tempdir().unwrap();
    let repo = legacy_repo(root.path());
    let legacy_bytes = std::fs::read(legacy_pipeline(&repo)).unwrap();

    let report = json(&onboard(&repo, &["--migrate", "--apply", "--json"]));
    assert_eq!(report["status"], "migrated");
    assert!(
        report["next_steps"][0]
            .as_str()
            .unwrap()
            .contains("delete `.review/`")
    );
    for file in [
        ".af/af.lock",
        ".af/af.toml",
        ".af/pipelines/review.toml",
        ".af/workers/correctness/reviewer.md",
        ".af/workers/correctness/reviewer.toml",
    ] {
        assert!(repo.join(file).is_file(), "{file} missing");
    }
    assert_eq!(
        std::fs::read(repo.join(".af/pipelines/review.toml")).unwrap(),
        legacy_bytes,
        "a current pipeline moves byte for byte"
    );
    assert_eq!(
        std::fs::read(repo.join(".af/workers/correctness/reviewer.md")).unwrap(),
        std::fs::read(repo.join(".review/reviewers/correctness/reviewer.md")).unwrap()
    );
    let project = std::fs::read_to_string(repo.join(".af/af.toml")).unwrap();
    assert!(project.contains("pipeline = \"review\""), "{project}");
    assert!(
        project.contains("[worker.correctness]\npackage = \"correctness\""),
        "{project}"
    );
    let lock = std::fs::read_to_string(repo.join(".af/af.lock")).unwrap();
    assert!(lock.contains("[workers.correctness]"), "{lock}");
    assert!(lock.contains("[pipelines.review]"), "{lock}");
    assert!(
        !lock.contains("[af]"),
        "a source build pins no af release: {lock}"
    );
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("no install receipt")),
        "{report}"
    );

    // With both trees present, `.af/` is the authority onboarding validates.
    let again = json(&onboard(&repo, &["--json"]));
    assert_eq!(again["status"], "onboarded");
    assert_eq!(again["pipeline"], ".af/pipelines/review.toml");

    // Applying twice never overwrites.
    let twice = onboard(&repo, &["--migrate", "--apply"]);
    assert!(!twice.status.success());
    assert!(stderr(&twice).contains("--migrate"), "{}", stderr(&twice));

    // The migrated policy is what this very binary plans; the legacy path no longer is.
    std::fs::remove_dir_all(repo.join(".git")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "migrated"]);
    let planned = plan(&repo, None);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let planned: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(planned["schema"], "af/review-plan@1");

    let legacy = plan(&repo, Some(".review/pipelines/heavy.toml"));
    assert!(!legacy.status.success());
    assert!(
        stderr(&legacy).contains("no longer read") && stderr(&legacy).contains("--migrate --apply"),
        "{}",
        stderr(&legacy)
    );
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
        stderr(&refresh).contains("--migrate --apply"),
        "{}",
        stderr(&refresh)
    );
    assert!(!repo.join(".af").exists());
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
