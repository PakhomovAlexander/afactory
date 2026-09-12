#![cfg(unix)]
use review_process::{SupervisedError, run_supervised_captured_cancellable};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "af-cancel-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn gone(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) == Err(nix::errno::Errno::ESRCH)
}

fn dead(pid: i32) -> bool {
    if gone(pid) {
        return true;
    }
    // An orphan may briefly remain a zombie until init reaps it. It cannot execute or hold
    // a pipe; the direct child, separately asserted gone, is reaped by the owned waiter.
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'));
    }
    false
}

#[test]
fn cancellation_before_spawn_cannot_execute_the_command() {
    let directory = Directory::new();
    let marker = directory.0.join("spawned");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "touch \"$1\"", "fixture"]).arg(&marker);
    let output = run_supervised_captured_cancellable(
        &mut command,
        None,
        Duration::from_secs(5),
        &AtomicBool::new(true),
    );
    assert!(matches!(output.status, Err(SupervisedError::Cancelled)));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(!marker.exists());
}

#[test]
fn cancellation_retains_prefixes_and_stops_running_stdin_and_each_held_drain() {
    for mode in ["running", "stdin", "stdout", "stderr"] {
        let directory = Directory::new();
        let marker = directory.0.join("ready");
        let script = match mode {
            "running" => {
                "sleep 30 & child=$!; printf 'prefix'; printf 'diagnostic' >&2; printf '%s %s' $$ $child >\"$1\"; wait"
            }
            // The descendant deliberately retains the parent's stdin, which blocks a large
            // writer after the leader has exited. Save it before background execution: some
            // /bin/sh implementations first replace a background command's fd 0 with /dev/null,
            // so <&0 would duplicate that replacement. The other cases isolate each drain.
            "stdin" => {
                "exec 3<&0; sleep 30 <&3 3<&- >/dev/null 2>/dev/null & child=$!; printf 'prefix'; printf 'diagnostic' >&2; printf '%s %s' $$ $child >\"$1\"; exit 0"
            }
            "stdout" => {
                "sleep 30 </dev/null 2>/dev/null & child=$!; printf 'prefix'; printf 'diagnostic' >&2; printf '%s %s' $$ $child >\"$1\"; exit 0"
            }
            _ => {
                "sleep 30 </dev/null >/dev/null & child=$!; printf 'prefix'; printf 'diagnostic' >&2; printf '%s %s' $$ $child >\"$1\"; exit 0"
            }
        };
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script, "fixture"]).arg(&marker);
        let flag = AtomicBool::new(false);
        let (capture, ids, cancelled_at) = std::thread::scope(|scope| {
            let cancel = scope.spawn(|| {
                let limit = Instant::now() + Duration::from_secs(5);
                let mut ids = Vec::new();
                while Instant::now() < limit {
                    if let Ok(bytes) = std::fs::read_to_string(&marker) {
                        ids = bytes
                            .split_whitespace()
                            .filter_map(|n| n.parse::<i32>().ok())
                            .collect();
                    }
                    if ids.len() == 2 && (mode == "running" || gone(ids[0])) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(
                    ids.len(),
                    2,
                    "fixture must execute before cancellation: {mode}"
                );
                if mode != "running" {
                    assert!(gone(ids[0]), "must cancel after leader reaping: {mode}");
                }
                assert!(
                    !dead(ids[1]),
                    "descendant must be live at cancellation: {mode}"
                );
                let at = Instant::now();
                flag.store(true, Ordering::Release);
                (ids, at)
            });
            let input = (mode == "stdin").then(|| vec![b'x'; 4 * 1024 * 1024]);
            let capture = run_supervised_captured_cancellable(
                &mut command,
                input,
                Duration::from_secs(10),
                &flag,
            );
            let (ids, at) = cancel.join().unwrap();
            (capture, ids, at)
        });
        assert!(
            matches!(capture.status, Err(SupervisedError::Cancelled)),
            "{mode}: {:?}",
            capture.status
        );
        assert_eq!(capture.stdout, b"prefix", "{mode}");
        assert_eq!(capture.stderr, b"diagnostic", "{mode}");
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(2),
            "cancellation must interrupt the current wait: {mode}"
        );
        assert!(gone(ids[0]), "direct child was not reaped: {mode}");
        let limit = Instant::now() + Duration::from_secs(2);
        while !dead(ids[1]) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            dead(ids[1]),
            "descendant survived group cancellation: {mode}"
        );
    }
}
