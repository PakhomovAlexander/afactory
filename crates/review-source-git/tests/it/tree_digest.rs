//! The digest-only path (ADR-0158 clause 9): the committed capture's content digest without a
//! capture, equal to it whatever the tree holds, and absent with its reason, nothing left in
//! temporary storage, when a ceiling stops it.

use crate::common;

use common::{Fixture, cas_of, repo_of};
use review_source_git::digest::{Ceilings, Stopped};
use review_source_git::{Capture, committed_digest};

/// A tree of every entry kind, a gitlink, a path whose encoded spelling sorts apart from git's
/// tree order, a duplicated blob and a blob of several buffers.
fn varied(fixture: &Fixture) {
    fixture.with_content();
    fixture.write("a b/one.txt", b"spaced\n");
    fixture.write("a-b/two.txt", b"dashed\n");
    fixture.write("a/b.txt", b"slashed\n");
    fixture.write("copy/main.rs", b"fn main() { println!(\"hi\"); }\n");
    fixture.write("caf\u{e9}.md", "caf\u{e9}\n".as_bytes());
    fixture.write("big.bin", &vec![7_u8; 300 * 1024]);
    fixture.commit_all("varied");
    let head = fixture.git(&["rev-parse", "HEAD"]).trim().to_string();
    fixture.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},vendor/sub"),
    ]);
    fixture.git(&["commit", "-q", "-m", "gitlink"]);
}

fn empty_dir_count(path: &std::path::Path) -> usize {
    std::fs::read_dir(path).unwrap().count()
}

#[test]
fn the_digest_only_path_equals_the_committed_capture_held_or_spilled() {
    let fixture = Fixture::new();
    varied(&fixture);
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let captured = Capture::new(&repo, &cas)
        .committed("HEAD")
        .unwrap()
        .content_digest;
    let scratch = tempfile::tempdir().unwrap();
    let held = committed_digest(&repo, "HEAD", Ceilings::FIXED, scratch.path()).unwrap();
    assert_eq!(held, Ok(captured.clone()));
    // A manifest ceiling of a few entries spills many runs; their merge is the same digest.
    let spilled = committed_digest(
        &repo,
        "HEAD",
        Ceilings {
            manifest_bytes: 200,
            temp_bytes: 1 << 20,
        },
        scratch.path(),
    )
    .unwrap();
    assert_eq!(spilled, Ok(captured));
    assert_eq!(empty_dir_count(scratch.path()), 0, "the runs are removed");
}

#[test]
fn an_empty_tree_has_the_empty_manifests_digest() {
    let fixture = Fixture::new();
    fixture.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let captured = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(
        committed_digest(&repo, "HEAD", Ceilings::FIXED, scratch.path()).unwrap(),
        Ok(captured.content_digest)
    );
}

#[test]
fn a_ceiling_leaves_no_digest_its_reason_and_nothing_in_temporary_storage() {
    let fixture = Fixture::new();
    varied(&fixture);
    let repo = repo_of(&fixture);
    let scratch = tempfile::tempdir().unwrap();
    for (ceilings, reason) in [
        (
            Ceilings {
                manifest_bytes: 200,
                temp_bytes: 300,
            },
            Stopped::TempBytes,
        ),
        (
            Ceilings {
                manifest_bytes: 20,
                temp_bytes: 1 << 20,
            },
            Stopped::ManifestBytes,
        ),
    ] {
        assert_eq!(
            committed_digest(&repo, "HEAD", ceilings, scratch.path()).unwrap(),
            Err(reason)
        );
        assert_eq!(empty_dir_count(scratch.path()), 0, "{reason:?}");
    }
    // A deadline that already passed is the deadline, never a failure or a part.
    let late = repo_of(&fixture).with_deadline(std::time::Instant::now());
    assert_eq!(
        committed_digest(&late, "HEAD", Ceilings::FIXED, scratch.path()).unwrap(),
        Err(Stopped::Deadline)
    );
    assert_eq!(empty_dir_count(scratch.path()), 0);
}

#[test]
fn the_object_repository_reads_objects_and_nothing_of_the_checkout() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"one\ntwo\n");
    let head = fixture.commit_all("two lines");
    // The checkout's own opinion: untracked and `.git/info` attributes, a local diff driver.
    fixture.write(".gitattributes", b"*.txt -diff\n");
    std::fs::write(
        fixture.repo_path().join(".git/info/attributes"),
        b"*.txt diff=opaque\n",
    )
    .unwrap();
    fixture.git(&["config", "diff.opaque.binary", "true"]);
    let checkout = fixture.git(&["log", "--numstat", "--format=", &head]);
    assert!(checkout.starts_with("-\t-\t"), "{checkout}");

    let objects = repo_of(&fixture).object_repository().unwrap();
    let output = std::process::Command::new("git")
        .current_dir(fixture.home_path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fixture.home_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(objects.environment())
        .arg("--git-dir")
        .arg(objects.git_dir())
        .args(["log", "--numstat", "--format=", &head])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "2\t0\ta.txt\n");
    let directory = objects.git_dir().to_path_buf();
    drop(objects);
    assert!(!directory.exists(), "removed when dropped");
}
