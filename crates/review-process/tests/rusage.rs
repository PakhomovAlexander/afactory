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

/// The workspace's own source, every crate's `src/` but this one's (a build script runs in
/// cargo, never in a process that supervises), test code excluded: the files under a `tests`
/// directory or named `tests.rs` or `*_tests.rs`, and an inline test module from its
/// `#[cfg(test)] mod … {` to the end of its file.
fn workspace_sources() -> Vec<(std::path::PathBuf, String)> {
    fn walk(directory: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tests" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        if entry.file_name() != "review-process" {
            walk(&entry.path().join("src"), &mut files);
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let end = lines
                .windows(2)
                .position(|pair| {
                    pair[0].trim() == "#[cfg(test)]"
                        && pair[1].trim_start().starts_with("mod ")
                        && pair[1].trim_end().ends_with('{')
                })
                .unwrap_or(lines.len());
            (path, lines[..end].join("\n"))
        })
        .collect()
}

/// The reaps a line performs directly: a child's `wait`, `try_wait` or `wait_with_output`, a
/// command's `output()` or `status()`, or a `waitpid`/`waitid`. A non-blocking reap handed to
/// `review_process::reap::locked` is the locked reap itself.
fn direct_reaps(line: &str) -> Vec<&'static str> {
    if line.contains("reap::locked(") || line.trim_start().starts_with("//") {
        return Vec::new();
    }
    [
        ".wait()",
        ".try_wait()",
        ".wait_with_output()",
        ".output()",
        ".status()",
        "waitpid(",
        "waitid(",
    ]
    .into_iter()
    .filter(|pattern| line.contains(pattern))
    .collect()
}

/// Design §7.2 and ADR-0026 (P3): a supervised process's `af.rusage.*` is the change of the
/// process's children totals across its reap, which is the one child's only when no other
/// child is reaped between the two readings. So every child reap of the workspace's non-test
/// code goes through this crate's locked reap (`review_process::reap`): a direct `wait`,
/// `try_wait`, `wait_with_output`, `output()`, `status()` or `waitpid` anywhere else fails
/// this test, naming the file and line.
#[test]
fn every_child_reap_outside_review_process_is_locked() {
    let mut found = Vec::new();
    for (path, text) in workspace_sources() {
        for (number, line) in text.lines().enumerate() {
            for reap in direct_reaps(line) {
                found.push(format!("{}:{}: {reap}", path.display(), number + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "direct child reaps; reap through review_process::reap instead:\n{}",
        found.join("\n")
    );
    // The scan sees what it must.
    assert_eq!(direct_reaps("    let _ = child.wait();"), [".wait()"]);
    assert_eq!(direct_reaps("        .output()"), [".output()"]);
    assert_eq!(direct_reaps("match waitpid(pid, None) {"), ["waitpid("]);
    assert!(direct_reaps("review_process::reap::locked(|| child.try_wait())").is_empty());
}
