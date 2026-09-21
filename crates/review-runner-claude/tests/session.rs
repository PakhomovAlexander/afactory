//! The Claude half of the Session Snapshot protocol: the kernel's own `--session-id`, the
//! forked `--resume`, and a capture and deletion that never follow a symlink and never depend on
//! reconstructing the Attempt's working directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_config::lock::{Lockfile, Registry};
use review_core::{SessionCleanupRefusalV1, session_id_for_attempt};
use review_runner::{
    CapturedSession, ReviewerAdapter, ReviewerInputs, SessionCapture, SessionDeletion,
    SessionLayer, SessionResume,
};
use review_runner_claude::ClaudeSessionStore;
use review_store::Cas;

const ANSWER: &str = r#"{"verdict":"approve","summary":null,"findings":[],
    "benchmark_demands":[],"disputes":[]}"#;

fn envelope() -> String {
    serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": ANSWER, "total_cost_usd": 0.1, "num_turns": 2,
        "usage": {
            "input_tokens": 1_200, "output_tokens": 800,
            "cache_read_input_tokens": 480_000, "cache_creation_input_tokens": 0
        }
    })
    .to_string()
}

/// A stub that records the exact argv it was invoked with, so the pinned session flags are
/// asserted against what the adapter really passes rather than against a comment.
fn recording_stub(dir: &Path) -> (PathBuf, PathBuf) {
    let argv = dir.join("argv");
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat <<'ENVELOPE'\n{}\nENVELOPE\n",
            argv.display(),
            envelope()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (path, argv)
}

fn package(dir: &Path, stub_path: &Path) -> review_config::lock::ResolvedReviewer {
    let registry_root = dir.join("registry");
    let package = registry_root.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n[runner]\nprogram = \"{}\"\n\
             args = [{{ value = \"--model\" }}, {{ value = \"opus\" }}]\n",
            stub_path.display()
        ),
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), "You are a test reviewer.\n").unwrap();
    let registry = Registry::new([registry_root]);
    let mut lockfile = Lockfile::empty();
    lockfile.workers.insert(
        "tester".to_string(),
        Lockfile::pin("tester", &registry).unwrap(),
    );
    lockfile
        .resolve_for_subject("tester", &registry, review_core::SubjectKind::WholeTree)
        .unwrap()
}

fn store(root: &Path) -> ClaudeSessionStore {
    ClaudeSessionStore::from_grants(None, root.to_str().unwrap())
}

fn write_transcript(
    store: &ClaudeSessionStore,
    cwd: &Path,
    session_id: &str,
    bytes: &[u8],
) -> PathBuf {
    let path = store.transcript_path(cwd, session_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn the_session_identity_is_the_kernels_and_a_resume_always_forks() {
    let dir = tempfile::tempdir().unwrap();
    let (stub_path, argv) = recording_stub(dir.path());
    let package = package(dir.path(), &stub_path);
    let adapter =
        review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(10))
            .unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();

    let own = session_id_for_attempt(&"b".repeat(26)).unwrap();
    let source = session_id_for_attempt(&"a".repeat(26)).unwrap();
    let inputs = ReviewerInputs {
        session_id: Some(own.clone()),
        session_resume: Some(SessionResume {
            session_id: source.clone(),
            artifact_id: format!("sha256:{}", "9".repeat(64)),
            transcript_bytes: 1_024,
            estimated_tokens: 256,
        }),
        ..ReviewerInputs::default()
    };
    adapter.invoke(&cas, &sandbox, &inputs).unwrap();
    let recorded: Vec<String> = std::fs::read_to_string(&argv)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let index = |flag: &str| recorded.iter().position(|value| value == flag);
    assert_eq!(recorded[0], "-p");
    assert_eq!(
        recorded.get(index("--session-id").expect("the kernel assigns the session") + 1),
        Some(&own)
    );
    assert_eq!(
        recorded.get(index("--resume").expect("the carried transcript is resumed") + 1),
        Some(&source)
    );
    assert!(
        index("--fork-session").is_some(),
        "a resume always forks, so the captured transcript is never mutated"
    );
    assert!(
        index("--session-id").unwrap() < index("--model").unwrap(),
        "session flags precede package model flags, like the security flags"
    );
    assert!(index("--safe-mode").unwrap() > index("--model").unwrap());

    // A cold Attempt of the same adapter passes neither flag, so its argv is what it always was.
    adapter
        .invoke(&cas, &sandbox, &ReviewerInputs::default())
        .unwrap();
    let cold = std::fs::read_to_string(&argv).unwrap();
    assert!(!cold.contains("--session-id"));
    assert!(!cold.contains("--resume"));
    assert!(!cold.contains("--fork-session"));
}

#[test]
fn an_adapter_without_granted_auth_hosts_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let (stub_path, _) = recording_stub(dir.path());
    let package = package(dir.path(), &stub_path);
    let adapter =
        review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(10))
            .unwrap();
    assert!(
        adapter.session_layer().is_none(),
        "without granted auth there is no harness directory to address"
    );
    let granted =
        review_runner_claude::ClaudeAdapter::from_package(&package, Duration::from_secs(10))
            .unwrap()
            .with_auth(None, "operator", dir.path().to_str().unwrap());
    assert_eq!(
        granted.session_layer().map(|layer| layer.provider_kind()),
        Some("claude")
    );
    // Every adapter that does not implement the protocol inherits the refusing default. That is
    // how Codex stays out of the session layer without naming itself anywhere in it.
    let command = review_runner::CommandAdapter::new(
        review_core::Command::new("true", Vec::new()),
        Duration::from_secs(1),
    );
    assert!(command.session_layer().is_none());
}

#[test]
fn a_capture_finds_its_session_by_identity_and_the_deletion_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = session_id_for_attempt(&"a".repeat(26)).unwrap();
    // Two sandboxes, one per Attempt: the transcript is found by its kernel-assigned identity,
    // never by reconstructing the working directory that produced it.
    let gone = dir.path().join("sandbox-of-a-round-that-ended");
    let path = write_transcript(&store, &gone, &session, b"{\"role\":\"user\"}\n");

    let SessionCapture::Captured(CapturedSession {
        transcript,
        path_digest,
    }) = store.capture(&session, 1_000_000).unwrap()
    else {
        panic!("the transcript was not captured");
    };
    assert_eq!(transcript, b"{\"role\":\"user\"}\n");
    assert!(path_digest.starts_with("sha256:"));
    assert!(path.exists(), "capture reads; it never deletes");

    assert_eq!(
        store.delete(&session, Some(&path_digest)),
        SessionDeletion::Deleted
    );
    assert!(!path.exists());
    assert_eq!(
        store.delete(&session, Some(&path_digest)),
        SessionDeletion::AlreadyAbsent,
        "an idempotent second deletion completes the protocol"
    );
    assert!(matches!(
        store.capture(&session, 1_000_000).unwrap(),
        SessionCapture::Absent
    ));
}

#[test]
fn a_transcript_over_its_bound_is_never_filed_and_a_non_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let cwd = dir.path().join("sandbox");
    let big = session_id_for_attempt(&"a".repeat(26)).unwrap();
    write_transcript(&store, &cwd, &big, &vec![b'x'; 4_096]);
    assert!(matches!(
        store.capture(&big, 1_024).unwrap(),
        SessionCapture::OverBound { bytes: 4_096 }
    ));

    let directory = session_id_for_attempt(&"b".repeat(26)).unwrap();
    std::fs::create_dir_all(store.transcript_path(&cwd, &directory)).unwrap();
    assert!(matches!(
        store.capture(&directory, 1_024).unwrap(),
        SessionCapture::Refused(SessionCleanupRefusalV1::NotRegularFile)
    ));
    assert_eq!(
        store.delete(&directory, None),
        SessionDeletion::Refused(SessionCleanupRefusalV1::NotRegularFile),
        "what is not the regular file a transcript is, is never unlinked"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_project_directory_is_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = session_id_for_attempt(&"a".repeat(26)).unwrap();
    // The transcript exists outside the harness directory, reachable only through a symlinked
    // project entry. The search skips the link rather than reaching through it.
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let sentinel = outside.join(format!("{session}.jsonl"));
    std::fs::write(&sentinel, b"secret").unwrap();
    let projects = dir.path().join(".claude").join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::os::unix::fs::symlink(&outside, projects.join("-linked")).unwrap();

    assert!(matches!(
        store.capture(&session, 1_000_000).unwrap(),
        SessionCapture::Absent
    ));
    assert_eq!(store.delete(&session, None), SessionDeletion::AlreadyAbsent);
    assert!(
        sentinel.exists(),
        "nothing outside the harness directory was read or unlinked"
    );
}

#[test]
fn a_re_materialized_transcript_is_found_under_the_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let source = session_id_for_attempt(&"a".repeat(26)).unwrap();
    let resuming_sandbox = dir.path().join("sandbox-of-the-next-attempt");
    let digest = store
        .materialize(&resuming_sandbox, &source, b"{\"role\":\"assistant\"}\n")
        .unwrap();
    assert_eq!(
        digest,
        review_store::canonical::blob_content_id(
            store
                .transcript_path(&resuming_sandbox, &source)
                .as_os_str()
                .as_encoded_bytes()
        )
    );
    let SessionCapture::Captured(captured) = store.capture(&source, 1_000_000).unwrap() else {
        panic!("the re-materialized transcript is where --resume will look for it");
    };
    assert_eq!(captured.transcript, b"{\"role\":\"assistant\"}\n");
    assert_eq!(captured.path_digest, digest);
    assert_eq!(
        store.delete(&source, Some(&digest)),
        SessionDeletion::Deleted
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_projects_directory_is_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = session_id_for_attempt(&"a".repeat(26)).unwrap();
    // The whole projects directory is a link out of the harness store, with a transcript of the
    // right name below it. Everything under the granted root is opened no-follow, so the search
    // refuses rather than reading or unlinking through the link.
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(outside.join("-project")).unwrap();
    let sentinel = outside.join("-project").join(format!("{session}.jsonl"));
    std::fs::write(&sentinel, b"secret").unwrap();
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.path().join(".claude").join("projects")).unwrap();

    assert!(matches!(
        store.capture(&session, 1_000_000).unwrap(),
        SessionCapture::Refused(SessionCleanupRefusalV1::SymlinkedParent)
    ));
    assert_eq!(
        store.delete(&session, None),
        SessionDeletion::Refused(SessionCleanupRefusalV1::SymlinkedParent)
    );
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"secret",
        "nothing outside the harness directory was read or unlinked"
    );
}

#[cfg(unix)]
#[test]
fn a_deletion_unlinks_in_the_directory_the_search_validated() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = session_id_for_attempt(&"a".repeat(26)).unwrap();
    let cwd = dir.path().join("sandbox");
    let path = write_transcript(&store, &cwd, &session, b"{\"role\":\"user\"}\n");
    let project = path.parent().unwrap().to_path_buf();

    assert_eq!(store.delete(&session, None), SessionDeletion::Deleted);
    assert!(!path.exists());
    assert!(
        !project.exists(),
        "an emptied project directory is removed relative to the projects directory"
    );
    assert!(
        dir.path().join(".claude").join("projects").is_dir(),
        "and the store itself stays"
    );
}

#[cfg(unix)]
#[test]
fn a_materialized_transcript_never_writes_through_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = session_id_for_attempt(&"b".repeat(26)).unwrap();
    let cwd = dir.path().join("sandbox");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.path().join(".claude").join("projects")).unwrap();

    let refused = store
        .materialize(&cwd, &session, b"transcript")
        .unwrap_err();
    assert!(refused.contains("harness session store"), "{refused}");
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "nothing was written outside the harness directory"
    );
}
