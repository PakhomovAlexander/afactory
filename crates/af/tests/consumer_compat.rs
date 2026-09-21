//! Consumer compatibility: the built `af` must still plan the policies real consumers pin.
//!
//! `fixtures/consumers/<name>/` mirrors one consuming repository's committed review policy. A
//! pipeline-format change that rejects a fixture is a compatibility break for that consumer and
//! must be migrated deliberately, never absorbed by editing the fixture (see the README there).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures_root() -> PathBuf {
    fixtures::workspace_root().join("fixtures/consumers")
}

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::copy_tree;

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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Copies a fixture into a fresh repository and commits it: `af review plan` reads policy from
/// a git revision, never from the working tree.
fn materialize(fixture: &Path) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("consumer");
    copy_tree(fixture, &repo);
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "consumer fixture"]);
    (root, repo)
}

fn plan(repo: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    command.args(["review", "plan", "--repo", repo.to_str().unwrap()]);
    command
        .env("AF_SELF_OFFLINE", "1")
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
        .unwrap()
}

fn consumer_fixtures() -> Vec<PathBuf> {
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(fixtures_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join(".af").is_dir())
        .collect();
    fixtures.sort();
    fixtures
}

#[test]
fn every_consumer_fixture_plans_with_this_binary() {
    let fixtures = consumer_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no consumer fixtures under fixtures/consumers/"
    );
    for fixture in fixtures {
        let (_root, repo) = materialize(&fixture);
        let output = plan(&repo);
        assert!(
            output.status.success(),
            "{} is rejected by this binary — a consumer compatibility break:\n{}",
            fixture.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            document["schema"],
            "af/review-plan@1",
            "{}",
            fixture.display()
        );
        assert_eq!(document["token_free"], true, "{}", fixture.display());
        assert!(
            document["external_effects"].is_object(),
            "{}: a plan must report its external effects",
            fixture.display()
        );
        // Reservation-aware planning: every static Worker's first Attempt, and their sum.
        let reservations = document["pipeline"]["reservations"].as_array().unwrap();
        assert!(!reservations.is_empty(), "{}", fixture.display());
        let sum: u64 = reservations
            .iter()
            .map(|reservation| reservation["tokens"].as_u64().unwrap())
            .sum();
        assert_eq!(
            document["pipeline"]["max_simultaneous_reservation"].as_u64(),
            Some(sum),
            "{}",
            fixture.display()
        );
        assert!(
            reservations.iter().all(|reservation| matches!(
                reservation["source"].as_str(),
                Some("node" | "pipeline")
            )),
            "{}",
            fixture.display()
        );
        // Every model Worker's first-Attempt input is measured against its cap, token-free.
        for reservation in reservations {
            assert!(
                reservation["input_bytes"].as_u64().unwrap() > 0,
                "{reservation}"
            );
            assert_eq!(reservation["fits"], true, "{reservation}");
        }
        assert_eq!(
            document["pipeline"]["inputs_fit"],
            true,
            "{}",
            fixture.display()
        );
    }
}

/// The break that motivated this fixture (v0.7.0 rejected the hub's pipeline for lacking a
/// `DemandSet@1` Ledger output) must stay detectable: the pre-fix shape is still refused, even
/// once the lock has been refreshed to pin the edited pipeline.
#[test]
fn the_hub_pipeline_without_a_demand_set_output_is_still_rejected() {
    let (_root, repo) = materialize(&fixtures_root().join("hub"));
    let pipeline = repo.join(".af/pipelines/review.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    let demands_line = text
        .lines()
        .find(|line| line.contains("review.kernel/DemandSet@1"))
        .expect("the hub fixture declares a DemandSet@1 Ledger output");
    let without = text.replace(&format!("{demands_line}\n"), "");
    assert_ne!(text, without);
    std::fs::write(&pipeline, without).unwrap();
    let refreshed = Command::new(env!("CARGO_BIN_EXE_af"))
        .args([
            "onboard",
            "--repo",
            repo.to_str().unwrap(),
            "--refresh-lock",
        ])
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    git(&repo, &["commit", "-q", "-am", "pre-v0.7.0 pipeline"]);

    let output = plan(&repo);
    assert!(
        !output.status.success(),
        "the pre-fix hub pipeline must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DemandSet@1"),
        "rejection must name the missing output, got: {stderr}"
    );
}
