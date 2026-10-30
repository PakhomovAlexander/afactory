//! Capture: identity, the read boundary, and read-only behaviour.

use crate::common;

use common::{Fixture, cas_of, repo_of};
use review_source_git::{Capture, CaptureError, CaptureObserver, materialize, worktree_state};

fn workspace_root() -> std::path::PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

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

struct CreateUntrackedOnce<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for CreateUntrackedOnce<'_> {
    fn between_passes(&self, attempt: u32) {
        if attempt == 1 {
            self.fixture.write("new.txt", b"stable after creation\n");
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

#[test]
fn an_untracked_file_created_between_passes_retries_without_a_cas_error() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas)
        .dirty_observed(&CreateUntrackedOnce { fixture: &fixture })
        .unwrap();

    assert_eq!(snapshot.attempts, 2);
    assert!(
        snapshot
            .manifest
            .entries
            .iter()
            .any(|entry| entry.path == "new.txt")
    );
}

/// A staged change with no file-content change still moves the index, and the boundary sees it.
struct StageBetweenPasses<'a> {
    fixture: &'a Fixture,
}

impl CaptureObserver for StageBetweenPasses<'_> {
    fn between_passes(&self, attempt: u32) {
        if attempt == 1 {
            settled_change(self.fixture, || {
                self.fixture
                    .write("newly_staged.rs", b"// staged mid-capture\n");
                self.fixture.git(&["add", "newly_staged.rs"]);
            });
        }
    }
}

/// Make `change` and return once a watcher of the repository started afterwards, as the next
/// pass's is, is no longer handed stale events. A loaded macOS host hands a newly started
/// FSEvents stream events of its paths that happened before it started, seconds of them, the
/// fixture's own construction included: one replayed to the next pass's watcher reads as a
/// change during that pass, a third attempt, which the boundary rightly makes and this test,
/// counting the attempts an index change alone causes, must not see. The same host also drops
/// events, so no one event of `change` (the index rename included) is sure to be reported to any
/// watcher, and waiting for one can wait forever.
///
/// So the wait is on observable delivery. First a watcher of a fences directory beside the
/// repository, started before `change`, must report a fence file written there after it: a
/// stalled host reports nothing for seconds, to watchers already running too, and then hands it
/// all on. Then each round starts a probe watching the repository, as the capture's watcher
/// does, and the fences directory, then writes a fresh fence file there, never inside the
/// repository. A probe that reports its own fence with no repository event before it or in the
/// same batch was handed no replay; a probe handed one, or that loses its fence for a second, is
/// replaced by the next round until the bounded deadline. One clean probe can still be ahead of
/// a replay delivered late, so two in a row settle the change. Fences are written one second
/// apart at most, each once the last is reported or lost; the wait is on that observable state,
/// never a sleep.
fn settled_change(fixture: &Fixture, change: impl FnOnce()) {
    use notify::Watcher as _;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};
    let fences = fixture.dir.path().join("fences");
    std::fs::create_dir_all(&fences).unwrap();
    // Event paths are canonical (`/private/var/…` for `/var/…` on macOS).
    let fences = fences.canonicalize().unwrap();
    let (send, before) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = send.send(event);
    })
    .unwrap();
    watcher
        .watch(&fences, notify::RecursiveMode::Recursive)
        .unwrap();
    change();
    let deadline = Instant::now() + Duration::from_secs(120);
    'stall: for round in 0.. {
        let fence = fences.join(format!("after-{round}"));
        std::fs::write(&fence, b"").unwrap();
        let replace_at = Instant::now() + Duration::from_secs(1);
        loop {
            let now = Instant::now();
            assert!(
                now < deadline,
                "no watcher reported a fence after the change"
            );
            match before.recv_timeout(replace_at.min(deadline).saturating_duration_since(now)) {
                Ok(Ok(event))
                    if event.paths.iter().any(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with("after-"))
                    }) =>
                {
                    break 'stall;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(error)) => panic!("the watcher failed: {error}"),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => panic!("the watcher disconnected"),
            }
        }
    }
    drop(watcher);
    let mut clean_in_a_row = 0;
    for round in 0.. {
        assert!(
            Instant::now() < deadline,
            "a watcher started after the change was still handed stale events"
        );
        let fence = fences.join(format!("fence-{round}"));
        let (send, events) = std::sync::mpsc::channel();
        let mut probe = notify::recommended_watcher(move |event| {
            let _ = send.send(event);
        })
        .unwrap();
        for root in [fixture.repo_path(), fences.clone()] {
            probe
                .watch(&root, notify::RecursiveMode::Recursive)
                .unwrap();
        }
        std::fs::write(&fence, b"").unwrap();
        let replace_at = Instant::now() + Duration::from_secs(1);
        let mut own_fence = false;
        let clean = loop {
            let event = if own_fence {
                // The rest of the batch that carried the probe's own fence.
                match events.try_recv() {
                    Ok(event) => event,
                    Err(_) => break true,
                }
            } else {
                let left = replace_at
                    .min(deadline)
                    .saturating_duration_since(Instant::now());
                match events.recv_timeout(left) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => break false,
                    Err(RecvTimeoutError::Disconnected) => panic!("the probe disconnected"),
                }
            };
            let event = event.unwrap_or_else(|error| panic!("the probe failed: {error}"));
            if event.paths.iter().any(|path| !path.starts_with(&fences)) {
                break false;
            }
            own_fence |= event.paths.contains(&fence);
        };
        clean_in_a_row = if clean { clean_in_a_row + 1 } else { 0 };
        if clean_in_a_row == 2 {
            return;
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
        &std::fs::read_to_string(workspace_root().join("schemas/source-snapshot-v1.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let snapshot = capture.committed("HEAD").unwrap();
    let payload = snapshot.to_payload(None).unwrap();
    if !validator.is_valid(&payload) {
        let errors: Vec<String> = validator
            .iter_errors(&payload)
            .map(|e| format!("{} at {}", e, e.instance_path()))
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
        std::fs::read_link(root.join("link"))
            .unwrap()
            .as_os_str()
            .as_bytes(),
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
fn encoded_filenames_survive_capture_and_materialize() {
    let fixture = Fixture::new();
    // The reviewer's example: a literal '%' in the name. encode_path escapes it to %25 in the
    // manifest key; without a decoder, materialize would create `docs/50%25-off.md`.
    fixture.write("docs/50%-off.md", b"half price\n");
    fixture.write(" notes.md", b"leading whitespace\n");
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
    assert!(snapshot.manifest.get("%20notes.md").is_some());
    assert_eq!(
        std::fs::read(out.path().join(" notes.md")).unwrap(),
        b"leading whitespace\n"
    );
}

#[test]
#[ignore = "release measurement; run with --release -- --ignored --nocapture"]
fn dirty_capture_large_tree_measurement() {
    let fixture = Fixture::new();
    fixture.write("seed.txt", b"tracked seed\n");
    fixture.commit_all("seed");

    let count = 5_000_u32;
    let mut bytes = vec![0x42_u8; 40 * 1024];
    for index in 0..count {
        bytes[..4].copy_from_slice(&index.to_le_bytes());
        fixture.write(&format!("large/{index:05}.bin"), &bytes);
    }

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let start = std::time::Instant::now();
    let snapshot = Capture::new(&repo, &cas).dirty().unwrap();
    let cold_elapsed = start.elapsed();
    let start = std::time::Instant::now();
    let warm_snapshot = Capture::new(&repo, &cas).dirty().unwrap();
    let warm_elapsed = start.elapsed();
    let start = std::time::Instant::now();
    let synthetic_tree = repo.synthetic_tree(&snapshot.manifest, &cas).unwrap();
    let synthetic_tree_elapsed = start.elapsed();

    assert_eq!(snapshot.manifest.entries.len(), count as usize + 1);
    assert_eq!(warm_snapshot.manifest, snapshot.manifest);
    assert!(!synthetic_tree.as_str().is_empty());
    eprintln!(
        "dirty capture: {count} x 40KiB ({:.1} MiB) cold {cold_elapsed:?}, warm {warm_elapsed:?}, synthetic tree {synthetic_tree_elapsed:?}",
        f64::from(count) * 40.0 / 1024.0
    );
}

/// Run git in the fixture with `input` on stdin, for the trees `git add` would refuse to build.
fn git_with_input(fixture: &Fixture, args: &[&str], input: &[u8]) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .current_dir(fixture.repo_path())
        .env("HOME", fixture.home_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A commit of a tree listed as `<mode> <type> <id>\t<name>` lines, built with `git mktree`,
/// which, unlike `git add`, writes any name and any mode it is given.
fn commit_tree(fixture: &Fixture, listing: &str) -> String {
    let tree = git_with_input(fixture, &["mktree"], listing.as_bytes());
    git_with_input(fixture, &["commit-tree", "-m", "crafted", &tree], b"")
}

/// `git add` refuses `.git/…`, but `git mktree` and a fetch without fsck do not: such a tree is
/// refused at capture in every spelling git itself refuses, never captured or silently dropped.
#[test]
fn a_tree_naming_git_administration_is_refused_at_capture() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    let blob = git_with_input(&fixture, &["hash-object", "-w", "--stdin"], b"[core]\n");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    for (directory, name) in [
        (".git", "config"),
        (".GIT", "HEAD"),
        (".git.", "x"),
        ("git~1", "x"),
    ] {
        let inner = git_with_input(
            &fixture,
            &["mktree"],
            format!("100644 blob {blob}\t{name}\n").as_bytes(),
        );
        let commit = commit_tree(
            &fixture,
            &format!("040000 tree {inner}\t{directory}\n100644 blob {blob}\tREADME\n"),
        );
        let error = Capture::new(&repo, &cas).committed(&commit).unwrap_err();
        let path = format!("{directory}/{name}");
        assert!(
            matches!(&error, CaptureError::GitPath { path: refused } if *refused == path),
            "{path}: {error}"
        );
        assert!(error.to_string().contains(&path), "{error}");
    }
    let link = commit_tree(&fixture, &format!("120000 blob {blob}\t.gitmodules\n"));
    let error = Capture::new(&repo, &cas).committed(&link).unwrap_err();
    assert!(
        matches!(&error, CaptureError::GitPath { path } if path == ".gitmodules"),
        "{error}"
    );
    let file = commit_tree(&fixture, &format!("100644 blob {blob}\t.gitmodules\n"));
    Capture::new(&repo, &cas).committed(&file).unwrap();
}

/// A legacy `100664` blob, as old trees hold, is a regular file: captured, materialized and
/// diffed like `100644`, rather than failing the whole review.
#[test]
fn a_legacy_group_writable_blob_is_captured_as_a_regular_file() {
    let fixture = Fixture::new();
    fixture.with_content();
    fixture.commit_all("initial");
    let blob = git_with_input(&fixture, &["hash-object", "-w", "--stdin"], b"legacy\n");
    let commit = commit_tree(
        &fixture,
        &format!("100664 blob {blob}\tlegacy.txt\n100755 blob {blob}\trun.sh\n"),
    );
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).committed(&commit).unwrap();
    let legacy = snapshot.manifest.get("legacy.txt").unwrap();
    assert_eq!(legacy.kind, review_source_git::EntryKind::File);
    assert_eq!(legacy.content, review_source_git::digest_bytes(b"legacy\n"));
    assert_eq!(
        snapshot.manifest.get("run.sh").unwrap().kind,
        review_source_git::EntryKind::Executable
    );
    let tree = fixture.dir.path().join("materialized");
    materialize(&snapshot.manifest, &cas, &tree).unwrap();
    assert_eq!(std::fs::read(tree.join("legacy.txt")).unwrap(), b"legacy\n");
    let base = repo.resolve_tree(&commit).unwrap();
    let diff = repo
        .tree_diff_synthetic_head(&base, &snapshot.manifest, &cas)
        .unwrap();
    assert!(diff.1.changes.is_empty(), "{:?}", diff.1.changes);
}

/// A committed blob several times the capture buffer is stored byte for byte, under the digest
/// the whole blob has.
#[test]
fn a_committed_blob_larger_than_the_buffer_is_stored_byte_identical() {
    let fixture = Fixture::new();
    let large: Vec<u8> = (0..3 * 1024 * 1024 + 7)
        .map(|index: usize| (index * 131 % 253) as u8)
        .collect();
    fixture.write("large.bin", &large);
    fixture.write("small.txt", b"small\n");
    let commit = fixture.commit_all("a large blob");
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).committed(&commit).unwrap();
    let entry = snapshot.manifest.get("large.bin").unwrap();
    assert_eq!(entry.size, large.len() as u64);
    assert_eq!(entry.content, review_source_git::digest_bytes(&large));
    assert_eq!(cas.get(&entry.content).unwrap(), large);
    let small = snapshot.manifest.get("small.txt").unwrap();
    assert_eq!(cas.get(&small.content).unwrap(), b"small\n");
    // The same tree captured from the worktree is the same snapshot.
    let dirty = Capture::new(&repo, &cas).dirty().unwrap();
    assert_eq!(dirty.content_digest, snapshot.content_digest);
}
