//! Capture: identity, the read boundary, and read-only behaviour.

mod common;

use common::{Fixture, cas_of, repo_of};
use review_source_git::{Capture, CaptureError, CaptureObserver, materialize, worktree_state};

#[test]
fn a_committed_capture_is_stable_and_content_identified() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);

    let first = capture.committed("HEAD").unwrap();
    let second = capture.committed("HEAD").unwrap();
    assert_eq!(first.content_digest, second.content_digest);
    assert!(!first.dirty);
    assert_eq!(first.attempts, 1);

    // The ignored file is not part of the committed tree, and the symlink is a symlink.
    assert!(first.manifest.get("ignored/secret.txt").is_none());
    assert_eq!(
        first.manifest.get("latest.rs").unwrap().kind,
        review_source_git::EntryKind::Symlink
    );
    assert_eq!(
        first.manifest.get("scripts/run.sh").unwrap().kind,
        review_source_git::EntryKind::Executable
    );
}

/// Identity is over content, so the same tree in two unrelated repositories — different paths,
/// different commit IDs, different history — is the same snapshot.
#[test]
fn identical_content_in_two_repositories_is_one_snapshot() {
    let mut digests = Vec::new();
    for message in ["initial", "a completely different commit message"] {
        let fixture = Fixture::new();
        fixture.with_content();
        fixture.commit_all(message);
        let repo = repo_of(&fixture);
        let cas = cas_of(&fixture);
        let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
        digests.push(snapshot.content_digest);
    }
    assert_eq!(digests[0], digests[1]);
}

#[test]
fn a_dirty_capture_sees_the_worktree_including_untracked_files() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    fixture.write("src/main.rs", b"fn main() { println!(\"changed\"); }\n");
    fixture.write("src/new_file.rs", b"// brand new, never committed\n");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);

    let committed = capture.committed("HEAD").unwrap();
    let dirty = capture.dirty().unwrap();

    assert_ne!(committed.content_digest, dirty.content_digest);
    assert!(dirty.dirty);
    assert!(
        dirty.manifest.get("src/new_file.rs").is_some(),
        "an untracked file is part of what a reviewer would see"
    );
    assert!(
        dirty.manifest.get("ignored/secret.txt").is_none(),
        "an ignored file is not"
    );
    assert_ne!(
        committed.manifest.get("src/main.rs").unwrap().content,
        dirty.manifest.get("src/main.rs").unwrap().content
    );
}

/// Capture may not disturb the thing it is capturing.
#[test]
fn capture_leaves_the_checkout_exactly_as_it_found_it() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    fixture.write("src/main.rs", b"fn main() { println!(\"changed\"); }\n");
    fixture.write("staged.rs", b"// staged\n");
    fixture.git(&["add", "staged.rs"]);

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let before = worktree_state(&repo).unwrap();

    let capture = Capture::new(&repo, &cas);
    capture.committed("HEAD").unwrap();
    capture.dirty().unwrap();

    assert_eq!(
        before,
        worktree_state(&repo).unwrap(),
        "HEAD, index and worktree must be untouched"
    );
}

struct MutateEveryPass<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for MutateEveryPass<'_> {
    fn between_passes(&self, attempt: u32) {
        self.fixture.write(
            "src/main.rs",
            format!("fn main() {{ /* edit {attempt} */ }}\n").as_bytes(),
        );
    }
}

struct MutateAndRestoreEveryPass<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for MutateAndRestoreEveryPass<'_> {
    fn between_passes(&self, _attempt: u32) {
        self.fixture
            .write("src/main.rs", b"fn main() { /* transient edit */ }\n");
        self.fixture
            .write("src/main.rs", b"fn main() { println!(\"hi\"); }\n");
    }
}

/// A worktree that keeps changing must fail closed rather than admit a tree that never existed.
#[test]
fn a_worktree_changing_under_the_read_fails_closed() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);

    let err = capture
        .dirty_observed(&MutateEveryPass { fixture: &fixture })
        .unwrap_err();
    assert!(
        matches!(err, CaptureError::Unstable { attempts: 3 }),
        "expected a refused capture, got {err}"
    );
}

#[test]
fn matching_manifests_do_not_hide_an_intervening_change_event() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let error = Capture::new(&repo, &cas)
        .dirty_observed(&MutateAndRestoreEveryPass { fixture: &fixture })
        .unwrap_err();

    assert!(matches!(error, CaptureError::Unstable { attempts: 3 }));
}

#[cfg(unix)]
#[test]
fn dirty_capture_refuses_a_symlink_in_a_tracked_paths_parent() {
    let fixture = Fixture::new();
    fixture.write("dir/token", b"repository bytes\n");
    fixture.commit_all("tracked nested path");
    std::fs::remove_dir_all(fixture.repo_path().join("dir")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), b"host secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.repo_path().join("dir")).unwrap();

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    assert!(matches!(
        Capture::new(&repo, &cas).dirty(),
        Err(CaptureError::UnsafePath { .. })
    ));
}

struct MutateOnce<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for MutateOnce<'_> {
    fn between_passes(&self, attempt: u32) {
        if attempt == 1 {
            self.fixture
                .write("src/main.rs", b"fn main() { /* one edit */ }\n");
        }
    }
}

/// One edit is a retry, not a failure — and the snapshot records how many passes it took.
#[test]
fn a_settled_worktree_is_admitted_on_retry() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas)
        .dirty_observed(&MutateOnce { fixture: &fixture })
        .unwrap();

    assert_eq!(snapshot.attempts, 2);
    assert_eq!(
        snapshot.manifest.get("src/main.rs").unwrap().content,
        review_source_git::digest_bytes(b"fn main() { /* one edit */ }\n"),
        "the admitted content is the settled content"
    );
}

/// A staged change with no file-content change still moves the index, and the boundary sees it.
struct StageBetweenPasses<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for StageBetweenPasses<'_> {
    fn between_passes(&self, attempt: u32) {
        if attempt == 1 {
            self.fixture
                .write("newly_staged.rs", b"// staged mid-capture\n");
            self.fixture.git(&["add", "newly_staged.rs"]);
        }
    }
}

#[test]
fn an_index_change_alone_invalidates_a_pass() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas)
        .dirty_observed(&StageBetweenPasses { fixture: &fixture })
        .unwrap();
    assert_eq!(snapshot.attempts, 2, "the index fingerprint caught it");
}

#[test]
fn materialization_reproduces_the_tree_from_the_manifest_alone() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let sandbox = tempfile::tempdir().unwrap();
    materialize(&snapshot.manifest, &cas, sandbox.path()).unwrap();

    assert_eq!(
        std::fs::read(sandbox.path().join("src/main.rs")).unwrap(),
        b"fn main() { println!(\"hi\"); }\n"
    );
    assert!(
        std::fs::symlink_metadata(sandbox.path().join("latest.rs"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sandbox.path().join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "the executable bit is carried");
    }
}

/// A capture must produce a payload the contract actually accepts.
#[test]
fn the_snapshot_payload_matches_source_snapshot_v1() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);

    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../schemas/source-snapshot-v1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let snapshot = capture.committed("HEAD").unwrap();
    let payload = snapshot.to_payload(None).unwrap();
    if !validator.is_valid(&payload) {
        let errors: Vec<String> = validator
            .iter_errors(&payload)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();
        panic!("payload rejected: {}", errors.join("; "));
    }
    let parsed: review_core::SourceSnapshot = serde_json::from_value(payload).unwrap();
    assert!(!parsed.is_derived());
    let mut dirty = capture.dirty().unwrap();
    assert!(dirty.to_payload(None).is_err());
    dirty.tree_id = Some(repo.synthetic_tree(&dirty.manifest, &cas).unwrap());
    let payload = dirty.to_payload(None).unwrap();
    assert!(validator.is_valid(&payload));
    let parsed: review_core::SourceSnapshot = serde_json::from_value(payload).unwrap();
    assert!(matches!(
        parsed.capture,
        review_core::snapshot::Capture::SyntheticWorktree { .. }
    ));
}

#[test]
fn a_persisted_committed_tree_is_rehydrated_only_when_all_authority_agrees() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);
    let snapshot = capture.committed("HEAD").unwrap();
    let payload = snapshot.to_payload(None).unwrap();
    let source: review_core::SourceSnapshot = serde_json::from_value(payload.clone()).unwrap();

    assert_eq!(
        capture
            .rehydrate_committed(&source, &snapshot.manifest)
            .unwrap(),
        snapshot.tree_id.unwrap()
    );

    let mut contradictory = payload;
    contradictory["capture"]["tree_id"] =
        serde_json::json!("0000000000000000000000000000000000000000");
    let source: review_core::SourceSnapshot = serde_json::from_value(contradictory).unwrap();
    assert!(
        capture
            .rehydrate_committed(&source, &snapshot.manifest)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_symlink_targets_roundtrip_through_both_capture_modes() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new();
    let target = std::ffi::OsStr::from_bytes(b"target-\xff");
    std::os::unix::fs::symlink(target, fixture.repo_path().join("link")).unwrap();
    fixture.commit_all("non-utf8 symlink");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);
    let committed = capture.committed("HEAD").unwrap();
    let dirty = capture.dirty().unwrap();

    assert_eq!(committed.content_digest, dirty.content_digest);
    let root = fixture.dir.path().join("materialized");
    review_source_git::materialize(&dirty.manifest, &cas, &root).unwrap();
    assert_eq!(
        std::fs::read_link(root.join("link")).unwrap().as_os_str().as_bytes(),
        b"target-\xff"
    );
}

#[test]
fn dirty_capture_fails_closed_when_the_index_contains_a_gitlink() {
    let fixture = Fixture::new();
    fixture.write("tracked.txt", b"content\n");
    let revision = fixture.commit_all("base");
    let cacheinfo = format!("160000,{revision},submodule");
    fixture.git(&["update-index", "--add", "--cacheinfo", &cacheinfo]);
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);

    assert!(matches!(
        Capture::new(&repo, &cas).dirty(),
        Err(review_source_git::CaptureError::UnsupportedSubmodules { .. })
    ));
}

#[test]
fn an_object_git_cannot_produce_is_refused_not_zeroed() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);

    // Remove one blob's loose object. `ls-tree` still lists the path — the tree object is
    // intact — but `cat-file` can no longer produce the content.
    let oid = fixture
        .git(&["rev-parse", "HEAD:src/main.rs"])
        .trim()
        .to_string();
    let object = fixture
        .repo_path()
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    std::fs::remove_file(&object).expect("fixture blob should be a loose object");

    let error = Capture::new(&repo, &cas).committed("HEAD").unwrap_err();
    match &error {
        CaptureError::ObjectUnproducible { oid: reported, .. } => assert_eq!(reported, &oid),
        other => panic!("expected ObjectUnproducible, got: {other}"),
    }
}

#[test]
fn a_percent_in_a_filename_survives_capture_and_materialize() {
    let fixture = Fixture::new();
    // The reviewer's example: a literal '%' in the name. encode_path escapes it to %25 in the
    // manifest key; without a decoder, materialize would create `docs/50%25-off.md`.
    fixture.write("docs/50%-off.md", b"half price\n");
    fixture.write("src/main.rs", b"fn main() {}\n");
    fixture.commit_all("initial");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);

    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let out = tempfile::tempdir().unwrap();
    materialize(&snapshot.manifest, &cas, out.path()).unwrap();

    let real = out.path().join("docs/50%-off.md");
    assert!(
        real.exists(),
        "the real filename must be on disk, not its encoding"
    );
    assert_eq!(std::fs::read(&real).unwrap(), b"half price\n");
    assert!(
        !out.path().join("docs/50%25-off.md").exists(),
        "the encoded name must not leak to the filesystem"
    );
}
