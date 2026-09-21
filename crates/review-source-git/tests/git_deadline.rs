//! Repo's per-subprocess deadline, end to end: a wedged `git` is killed when it expires.
//!
//! Repo always runs `git` from the kernel's PATH, so the only way to hand it a wedged one is a
//! fake `git` first on PATH. PATH belongs to the whole process, so this binary holds one test,
//! and that test re-runs itself as a child process with the altered PATH instead of changing
//! its own environment.

#![cfg(unix)]

use std::time::{Duration, Instant};

/// Set only in the child run; names the directory holding the fake `git` and its marker.
const CHILD: &str = "AF_TEST_WEDGED_GIT_DIR";
const TEST: &str = "a_wedged_git_process_is_killed_at_the_capture_deadline";

#[test]
fn a_wedged_git_process_is_killed_at_the_capture_deadline() {
    match std::env::var_os(CHILD) {
        Some(dir) => wedged_git_is_killed(std::path::Path::new(&dir)),
        None => run_with_a_wedged_git_first_on_path(),
    }
}

fn run_with_a_wedged_git_first_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake_git = bin.join("git");
    std::fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\n: > '{}'\nsleep 30\n",
            dir.path().join("started").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    // macOS vets a freshly written executable on its first exec, which can outlast the
    // deadline below. Pay that here, so the timed run reaches the script body.
    let warm = std::process::Command::new(&fake_git)
        .arg("warm")
        .status()
        .unwrap();
    assert!(warm.success());

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--test-threads=1"])
        .env("PATH", path)
        .env(CHILD, dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wedged_git_is_killed(dir: &std::path::Path) {
    let workdir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = review_source_git::Repo::open(workdir.path(), home.path())
        .with_timeout(Duration::from_millis(500));

    let started = Instant::now();
    let error = repo.line(&["rev-parse", "HEAD"]).unwrap_err().to_string();
    assert!(
        error.contains("deadline") || error.contains("exceeded"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        dir.join("started").exists(),
        "the fake git on PATH is the one that ran"
    );
}
