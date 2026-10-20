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
