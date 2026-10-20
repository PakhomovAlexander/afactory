//! A descendant that leaves its run's process group with `setsid` does not outlive the run
//! under `ExitPolicy::KillDescendants` (L7). Linux only: there the supervisor is a child
//! subreaper; elsewhere the policy is the group kill alone, which `setsid` escapes.
#![cfg(target_os = "linux")]

use review_process::{ExitPolicy, run_supervised_captured_with_policy};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOAD_SAFE_WALL: Duration = Duration::from_secs(120);

fn scratch() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "af-escape-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn a_setsid_child_is_ended_before_the_run_returns() {
    let directory = scratch();
    let marker = directory.join("x");
    let mut command = Command::new("/bin/sh");
    command.current_dir(&directory).args([
        "-c",
        "setsid sh -c 'sleep 2; touch x' >/dev/null 2>&1 </dev/null & \
         setsid sh -c 'sleep 2; touch y' & \
         sleep 0.2",
    ]);
    let capture = run_supervised_captured_with_policy(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillDescendants,
    );
    // The second child held both pipes; it was ended, so neither is reported held.
    assert!(capture.status.unwrap().success());
    assert!(!capture.stderr_held);
    std::thread::sleep(Duration::from_secs(3));
    assert!(!marker.exists(), "a setsid child outlived its run");
    assert!(
        !directory.join("y").exists(),
        "a setsid child outlived its run"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

/// A `set -m` background job moves to a process group of its own in the same session, and its
/// shell exits at once: it is this run's descendant all the same, and is ended before the run
/// returns, so it never writes into the tree after it is sealed (V5).
#[test]
fn a_job_control_background_job_is_ended_before_the_run_returns() {
    let directory = scratch();
    let mut command = Command::new("bash");
    command.current_dir(&directory).args([
        "-c",
        "set -m; (sleep 2; touch x) </dev/null >/dev/null 2>&1 & disown; exit 0",
    ]);
    let capture = run_supervised_captured_with_policy(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillDescendants,
    );
    assert!(capture.status.unwrap().success());
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !directory.join("x").exists(),
        "a set -m job outlived its run"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

/// Two runs at once in one process each end only their own descendants: the first run's
/// cleanup leaves alone the second run's orphan, though it started after the first run's
/// leader in a session of its own, while the second run is still running.
#[test]
fn concurrent_runs_each_end_only_their_own_descendants() {
    let directory = scratch();
    let started = directory.join("started");
    let second = {
        let directory = directory.clone();
        std::thread::spawn(move || {
            let mut command = Command::new("bash");
            command.current_dir(&directory).args([
                "-c",
                r#"touch started; sleep 0.5
                   bash -c 'setsid sh -c "sleep 1.5; touch theirs" </dev/null >/dev/null 2>&1 &
                            exit 0'
                   sleep 3"#,
            ]);
            run_supervised_captured_with_policy(
                &mut command,
                None,
                LOAD_SAFE_WALL,
                ExitPolicy::KillDescendants,
            )
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the second run never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut command = Command::new("bash");
    command.current_dir(&directory).args([
        "-c",
        "set -m; (sleep 2; touch ours) </dev/null >/dev/null 2>&1 & disown; sleep 1; exit 0",
    ]);
    let capture = run_supervised_captured_with_policy(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillDescendants,
    );
    assert!(capture.status.unwrap().success());
    let second = second.join().unwrap();
    assert!(second.status.unwrap().success());
    assert!(
        directory.join("theirs").exists(),
        "the other run's descendant was ended by this run's cleanup"
    );
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        !directory.join("ours").exists(),
        "a set -m job outlived its run"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}
