//! Actual CLI handler startup barrier: no environment backdoor or journal edits.
use super::*;
use std::process::Command;

thread_local! {
    pub(super) static AFTER_ACQUIRE: std::cell::RefCell<Option<Box<dyn FnOnce() -> Result<(), String>>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn after_acquire() -> Result<(), String> {
    AFTER_ACQUIRE
        .with(|hook| hook.borrow_mut().take())
        .map_or(Ok(()), |hook| hook())
}

#[test]
fn public_run_starts_heartbeat_before_confirming_the_plan() {
    let root = tempfile::tempdir().unwrap();
    starter::init(root.path(), "project", "document", None, true).unwrap();
    let repo = root.path().join("project");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let state = root.path().join("state");
    start(StartOptions {
        file: repo.join("document.json"),
        bindings: None,
        source_bindings: None,
        repo: repo.clone(),
        state: Some(state.clone()),
        authority: "HEAD".into(),
        uncommitted: false,
        json: true,
        plan_only: true,
        timeout_secs: None,
        optimization_history: None,
    })
    .unwrap();
    let observed_state = state.clone();
    AFTER_ACQUIRE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // The guard exposes no authority; this assertion observes its scoped ownership only.
            assert!(
                LIFECYCLE_ACTIVE.with(|active| active.get()),
                "public Task run has no heartbeat owner before plan confirmation"
            );
            let cas = Cas::open_existing(observed_state.join("cas")).unwrap();
            let observer =
                EventStore::open_read_only(observed_state.join("events.sqlite")).unwrap();
            let initial = observer
                .task_projection(&cas, "release-notes")
                .unwrap()
                .unwrap()
                .lease_until_unix_ms();
            // Preparation stays at this barrier until an actual background renewal commits.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
            loop {
                let current = observer
                    .task_projection(&cas, "release-notes")
                    .unwrap()
                    .unwrap();
                if current.lease_until_unix_ms() > initial {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "heartbeat did not renew while public CLI preparation was blocked"
                );
                std::thread::park_timeout(std::time::Duration::from_millis(20));
            }
            Ok(())
        }))
    });
    assert_eq!(
        run("release-notes", &repo, Some(&state), true, None, true).unwrap(),
        0
    );
}

thread_local! {
    pub(super) static LIFECYCLE_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
