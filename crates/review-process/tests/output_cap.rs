//! A child that writes without end costs the kernel at most the cap per stream (M2): the drain
//! keeps the cap's prefix, reads the rest away so the child is never blocked on a full pipe,
//! and records the total.

use review_process::{
    ExitPolicy, MAX_CAPTURED_STREAM_BYTES, run_supervised_captured_bounded,
    run_supervised_captured_with_policy, run_supervised_with_policy,
};
use std::process::Command;
use std::time::Duration;

/// Bounds a hung fixture only; a passing test never waits for it (ADR-0114).
const LOAD_SAFE_WALL: Duration = Duration::from_secs(120);
const TWO_HUNDRED_MIB: u64 = 200 * 1024 * 1024;

#[test]
fn a_child_writing_200_mib_keeps_the_cap_resident_and_exits_normally() {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        &format!("head -c {TWO_HUNDRED_MIB} /dev/zero; printf diagnostic >&2"),
    ]);
    let capture = run_supervised_captured_with_policy(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillProcessGroup,
    );
    // The child was never blocked: it wrote everything and exited on its own, successfully.
    assert!(capture.status.as_ref().unwrap().success());
    assert_eq!(capture.stdout.len(), MAX_CAPTURED_STREAM_BYTES);
    assert!(capture.stdout.iter().all(|byte| *byte == 0));
    assert_eq!(capture.stderr, b"diagnostic");
    assert_eq!(capture.truncation.stdout, Some(TWO_HUNDRED_MIB));
    assert_eq!(capture.truncation.stderr, None);
    assert_eq!(capture.truncation.cap, MAX_CAPTURED_STREAM_BYTES);
    assert!(
        capture
            .truncation
            .reason()
            .unwrap()
            .contains("stdout was truncated at the 64 MiB output cap: it wrote 209715200 bytes")
    );
}

#[test]
fn a_per_call_cap_bounds_both_streams_and_leaves_a_short_one_whole() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "head -c 10000 /dev/zero; head -c 9000 /dev/zero >&2"]);
    let capture = run_supervised_captured_bounded(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillProcessGroup,
        None,
        4096,
    );
    assert!(capture.status.unwrap().success());
    assert_eq!((capture.stdout.len(), capture.stderr.len()), (4096, 4096));
    assert_eq!(capture.truncation.stdout, Some(10_000));
    assert_eq!(capture.truncation.stderr, Some(9_000));

    let mut short = Command::new("/bin/sh");
    short.args(["-c", "printf whole"]);
    let capture = run_supervised_captured_bounded(
        &mut short,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillProcessGroup,
        None,
        4096,
    );
    assert_eq!(capture.stdout, b"whole");
    assert!(!capture.truncation.is_truncated());
}

#[test]
fn a_tools_whole_answer_keeps_stdout_and_caps_only_stderr() {
    // Git plumbing parses its stdout as one document: a prefix would be a wrong answer.
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "head -c 70000000 /dev/zero"]);
    let output = run_supervised_with_policy(
        &mut command,
        None,
        LOAD_SAFE_WALL,
        ExitPolicy::KillProcessGroup,
    )
    .unwrap();
    assert_eq!(output.stdout.len(), 70_000_000);
    assert!(!output.truncation.is_truncated());
}
