//! Resource use through every result type (the telemetry design, §7.2): the supervisor carries
//! the reaped leader's id and its CPU time and peak memory through its capture, the output, the
//! duplex output and the timed-out error, a cancelled and a killed process included, and every
//! conversion between them; nothing started means nothing reported, never zeros.
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_process::{
    ExitPolicy, Rusage, SupervisedError, run_supervised, run_supervised_captured,
    run_supervised_captured_cancellable, run_supervised_duplex,
};

const WALL: Duration = Duration::from_secs(120);

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

fn delivered(rusage: Option<Rusage>, what: &str) -> Rusage {
    let rusage = rusage.unwrap_or_else(|| panic!("{what}: no resource use"));
    if let Some(peak) = rusage.max_rss_bytes {
        assert!(peak > 0, "{what}: a delivered peak is never zero");
    }
    rusage
}

#[test]
fn the_capture_the_output_and_their_conversion_carry_resource_use() {
    // A process that burns measurable CPU time and touches a large buffer.
    let capture = run_supervised_captured(
        &mut sh(
            "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; dd if=/dev/zero of=/dev/null bs=67108864 count=1 2>/dev/null",
        ),
        None,
        WALL,
    );
    assert!(capture.status.as_ref().unwrap().success());
    assert!(capture.pid.is_some());
    let used = delivered(capture.rusage, "capture");
    assert!(used.user_ms + used.system_ms > 0, "{used:?}");
    let pid = capture.pid;
    let output = capture.into_result().unwrap();
    assert_eq!(output.pid, pid);
    assert_eq!(output.rusage, Some(used));
    let output = run_supervised(&mut sh("true"), None, WALL).unwrap();
    delivered(output.rusage, "output");
}

#[test]
fn a_timed_out_and_a_cancelled_process_keep_theirs() {
    let timed_out = run_supervised_captured(&mut sh("sleep 30"), None, Duration::from_millis(200));
    assert!(matches!(
        timed_out.status,
        Err(SupervisedError::TimedOut { .. })
    ));
    let used = delivered(timed_out.rusage, "timed out capture");
    match timed_out.into_result() {
        Err(SupervisedError::TimedOut { pid, rusage, .. }) => {
            assert!(pid.is_some());
            assert_eq!(rusage, Some(used));
        }
        _ => panic!("the conversion keeps the timeout"),
    }
    match run_supervised(&mut sh("sleep 30"), None, Duration::from_millis(200)) {
        Err(SupervisedError::TimedOut { pid, rusage, .. }) => {
            assert!(pid.is_some());
            delivered(rusage, "timed out output");
        }
        _ => panic!("a timeout"),
    }
    let flag = AtomicBool::new(false);
    let cancelled = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::Release);
        });
        run_supervised_captured_cancellable(&mut sh("sleep 30"), None, WALL, &flag)
    });
    assert!(matches!(cancelled.status, Err(SupervisedError::Cancelled)));
    assert!(cancelled.pid.is_some());
    delivered(cancelled.rusage, "cancelled capture");
}

#[test]
fn a_duplex_process_carries_its_own() {
    let started = Instant::now();
    let output = run_supervised_duplex(
        &mut sh("cat"),
        WALL,
        ExitPolicy::KillProcessGroup,
        |stdin| {
            stdin
                .write_all(b"duplex\n")
                .map_err(|error| error.to_string())
        },
        |stdout| {
            let mut text = String::new();
            stdout.read_to_string(&mut text).map(|_| text)
        },
    )
    .unwrap();
    assert_eq!(output.output.unwrap(), "duplex\n");
    assert!(output.pid.is_some());
    delivered(output.rusage, "duplex");
    match run_supervised_duplex(
        &mut sh("sleep 30"),
        Duration::from_millis(200),
        ExitPolicy::KillProcessGroup,
        |_| Ok::<(), String>(()),
        |stdout| {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
        },
    ) {
        Err(SupervisedError::TimedOut { pid, rusage, .. }) => {
            assert!(pid.is_some());
            delivered(rusage, "timed out duplex");
        }
        _ => panic!("a timeout"),
    }
    assert!(started.elapsed() < WALL);
}

/// A process that never started reports no id and no resource use — absent, never zero.
#[test]
fn nothing_started_reports_nothing() {
    let capture = run_supervised_captured(
        &mut Command::new("/nonexistent/af-process-fixture"),
        None,
        WALL,
    );
    assert!(matches!(capture.status, Err(SupervisedError::Spawn(_))));
    assert_eq!(capture.pid, None);
    assert_eq!(capture.rusage, None);
}
