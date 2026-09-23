//! An execute-checks command Worker's background children die with its Attempt; a read-only
//! command Worker keeps the process-group policy it always had.
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::task::{WorkerAccess, invoke_command_bytes};
use review_store::cas::Cas;

fn alive(pid: i32) -> bool {
    // `kill -0` probes without signalling; ESRCH means the process is gone.
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run(access: WorkerAccess) -> (i32, String) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let workdir = directory.path().join("work");
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    // The child is redirected away from the captured pipes so the leader's exit is what ends
    // the capture; its pid lands in the private TMPDIR the transport installs.
    let script = "sleep 300 >/dev/null 2>&1 & echo $! > \"$TMPDIR/pid\"; printf ok";
    let command = Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal(script)]);
    let returned = invoke_command_bytes(
        &cas,
        &workdir,
        &runtime,
        &command,
        Vec::new(),
        Duration::from_secs(30),
        None,
        &[],
        access,
    );
    let reply = String::from_utf8(returned.message.expect("the leader replies")).unwrap();
    let pid: i32 = std::fs::read_to_string(runtime.join("tmp/pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    (pid, reply)
}

#[test]
fn execute_checks_command_worker_children_die_with_the_attempt() {
    let (pid, reply) = run(WorkerAccess::ExecuteChecks);
    assert_eq!(reply, "ok");
    let mut waited = 0;
    while alive(pid) && waited < 50 {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
    }
    assert!(
        !alive(pid),
        "background child {pid} outlived the execute-checks Attempt"
    );
}

#[test]
fn read_only_command_worker_keeps_the_preserving_policy() {
    let (pid, reply) = run(WorkerAccess::ReadOnly);
    assert_eq!(reply, "ok");
    assert!(
        alive(pid),
        "read-only command Workers keep their process-group policy"
    );
    let _ = std::process::Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status();
}
