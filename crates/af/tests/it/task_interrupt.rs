//! SIGINT to `af` running a Task stops its Worker process group, records the cancelled
//! Attempt, exits 130 and leaves the Task resumable (ADR-0129). Command Workers only.
use crate::task_cli;
use nix::sys::signal::Signal;
use review_core::task::TaskPhaseV1;
use review_core::task::execution::TaskAttemptResultV1;
use review_store::{Cas, EventStore};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const TASK: &str = "pagination-cli";

fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat
            .rsplit_once(')')
            .is_some_and(|(_, s)| s.trim_start().starts_with('Z'))
    {
        return false;
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        None,
    )
    .is_ok()
}

struct ChildGuard(Option<std::process::Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Kills a leftover Worker group if the test fails before af stopped it.
struct GroupGuard(Vec<u32>);
impl Drop for GroupGuard {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(i32::try_from(*pid).unwrap()),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn commit_fixture(repo: &Path) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "interrupt fixture variation"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
}

/// The implementer starts a long-running child and waits on it, unless `resume` exists. Its
/// own attempt deadline is far beyond the interrupt, so only the interrupt can stop it.
pub(crate) fn long_running_implementer(repo: &Path, ready: &Path, resume: &Path) {
    let package = repo.join(".af/task-packages/fixture/implementer");
    let worker = package.join("worker.py");
    let original = std::fs::read_to_string(&worker).unwrap();
    let marker = "request=json.load(sys.stdin)\n";
    assert_eq!(original.matches(marker).count(), 1);
    let quote = |p: &Path| {
        p.to_str()
            .unwrap()
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
    };
    let hold = format!(
        "{marker}import os,subprocess\nif not os.path.exists('{resume}'):\n    child=subprocess.Popen(['/bin/sleep','60'])\n    open('{ready}.tmp','w').write('%d %d' % (os.getpid(), child.pid))\n    os.rename('{ready}.tmp','{ready}')\n    child.wait()\n",
        resume = quote(resume),
        ready = quote(ready),
    );
    std::fs::write(&worker, original.replacen(marker, &hold, 1)).unwrap();
    let manifest = package.join("worker.toml");
    let original = std::fs::read_to_string(&manifest).unwrap();
    let deadline = "[signature.attempt]\ntokens = 0\nwall_ms = 5000\n";
    assert_eq!(original.matches(deadline).count(), 1);
    std::fs::write(
        &manifest,
        original.replacen(
            deadline,
            "[signature.attempt]\ntokens = 0\nwall_ms = 40000\n",
            1,
        ),
    )
    .unwrap();
    let pipeline = repo.join(".af/task-packages/fixture/implementation/pipeline.toml");
    let original = std::fs::read_to_string(&pipeline).unwrap();
    let slot = "worker = \"fixture/implementer\"\nrole = \"implement\"\ninput_type = \"af/ImplementationInput@1\"\noutput_type = \"af/ImplementationReport@1\"\nmin_attempts = 1\nmax_attempts = 1\n";
    assert_eq!(original.matches(slot).count(), 1);
    let root = "max_attempts = 3\nmax_parallel = 2\n";
    assert_eq!(original.matches(root).count(), 1);
    // The cancelled Attempt keeps its charge under the existing accounting, so the resumed
    // run needs a second implementer Attempt within the slot, pipeline and Task limits.
    std::fs::write(
        &pipeline,
        original
            .replacen(
                slot,
                &slot.replace("max_attempts = 1", "max_attempts = 2"),
                1,
            )
            .replacen(root, "max_attempts = 4\nmax_parallel = 2\n", 1),
    )
    .unwrap();
    let ticket = repo.join("ticket.json");
    let original = std::fs::read_to_string(&ticket).unwrap();
    assert_eq!(original.matches("\"max_attempts\": 3,").count(), 1);
    std::fs::write(
        &ticket,
        original.replacen("\"max_attempts\": 3,", "\"max_attempts\": 4,", 1),
    )
    .unwrap();
    commit_fixture(repo);
}

/// How af ended, and the status a shell reports for it: af ends by its own signal once its
/// Workers have stopped, which a shell reports as 128 plus the signal number.
fn shell_status(status: &std::process::ExitStatus) -> (Option<Signal>, i32) {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(number) => (Signal::try_from(number).ok(), 128 + number),
        None => (None, status.code().unwrap()),
    }
}

/// `af task start --execute`: capture, plan and run in one command.
const START_EXECUTE: &[&str] = &[
    "task",
    "start",
    "--file",
    "ticket.json",
    "--execute",
    "--json",
];

/// Start the Task, wait until its Worker and the Worker's child run, send `signals` to af and
/// return af's output once it exited and no Worker process is left.
pub(crate) fn interrupt_running_worker(
    repo: &Path,
    state: &Path,
    ready: &Path,
    signals: &[Signal],
) -> std::process::Output {
    interrupt_running_af(repo, state, ready, signals, START_EXECUTE)
}

/// Run `af ARGS --state STATE`, wait until its Worker and the Worker's child run, send
/// `signals` to af and return af's output once it exited and no Worker process is left.
fn interrupt_running_af(
    repo: &Path,
    state: &Path,
    ready: &Path,
    signals: &[Signal],
    args: &[&str],
) -> std::process::Output {
    let mut af = ChildGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(repo)
            .args(args)
            .arg("--state")
            .arg(state)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let until = Instant::now() + Duration::from_secs(60);
    let pids: Vec<u32> = loop {
        if let Ok(text) = std::fs::read_to_string(ready) {
            let pids: Vec<u32> = text
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if pids.len() == 2 {
                break pids;
            }
        }
        if af.0.as_mut().unwrap().try_wait().unwrap().is_some() {
            let out = af.0.take().unwrap().wait_with_output().unwrap();
            panic!(
                "Worker did not start: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(Instant::now() < until, "Worker readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    let _groups = GroupGuard(vec![pids[0]]);
    assert!(
        pids.iter().all(|pid| alive(*pid)),
        "observe the Worker and its child before the interrupt"
    );
    let af_pid = nix::unistd::Pid::from_raw(i32::try_from(af.0.as_ref().unwrap().id()).unwrap());
    for (n, signal) in signals.iter().enumerate() {
        // Apart, so a second signal is delivered as a second signal: two standard signals sent
        // back to back may coalesce into one.
        if n > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        nix::sys::signal::kill(af_pid, *signal).unwrap();
    }
    let interrupted = Instant::now();
    while af.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            interrupted.elapsed() < Duration::from_secs(30),
            "af must stop its Workers and exit after {signals:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = af.0.take().unwrap().wait_with_output().unwrap();
    // af reaped or killed the Worker group before it exited; the killed child has no owner
    // left to wait for it, so allow init a moment to collect it.
    let reap_until = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|pid| alive(*pid)) {
        assert!(
            Instant::now() < reap_until,
            "a Worker process survived {signals:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    out
}

#[test]
fn sigint_stops_the_worker_group_records_a_cancelled_attempt_and_the_task_resumes() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    let resume = directory.path().join("worker-resume");
    long_running_implementer(&repo, &ready, &resume);

    let out = interrupt_running_worker(&repo, &state, &ready, &[Signal::SIGINT]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        shell_status(&out.status),
        (Some(Signal::SIGINT), 130),
        "{stderr}"
    );
    assert!(stderr.contains("interrupted by SIGINT"), "{stderr}");
    assert!(stderr.contains(&format!("af task run {TASK}")), "{stderr}");
    let document: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(document["schema"], "af/error@1");
    assert_eq!(document["exit_code"], 130);

    let cas = Cas::open_existing(state.join("cas")).unwrap();
    {
        let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let task = store.task_projection(&cas, TASK).unwrap().unwrap();
        assert!(
            !matches!(task.phase, TaskPhaseV1::Finished { .. }),
            "an interrupted Task is not finished"
        );
        let execution = task.execution.as_ref().unwrap();
        assert!(execution.pending_attempts().is_empty());
        let attempts = execution.attempt_accounting();
        let started: Vec<_> = attempts.iter().filter(|a| a.started).collect();
        assert_eq!(started.len(), 1, "{attempts:#?}");
        let Some(TaskAttemptResultV1::Failed { diagnostic_id, .. }) = &started[0].result else {
            panic!("the interrupted Attempt is settled, not succeeded: {attempts:#?}");
        };
        let diagnostic = cas.get_json(diagnostic_id).unwrap();
        assert!(diagnostic.to_string().contains("cancelled"), "{diagnostic}");
        assert!(
            !execution.outputs.contains_key("root.nodes.implement"),
            "the interrupted node published no output"
        );
        assert!(execution.invocations.contains_key("root.nodes.implement"));
    }

    std::fs::write(&resume, b"").unwrap();
    let resumed = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args(["task", "run", TASK, "--json", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {} {}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        )
    });
    assert_eq!(resumed.status.code(), Some(0), "{value:#}");
    assert_eq!(value["result"]["acceptance"], "satisfied", "{value:#}");
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, TASK).unwrap().unwrap();
    assert!(matches!(task.phase, TaskPhaseV1::Finished { .. }));
    let attempts = task.execution.as_ref().unwrap().attempt_accounting();
    let implement: Vec<_> = attempts
        .iter()
        .filter(|a| a.started && a.invocation_id == attempts_invocation(&attempts))
        .collect();
    assert_eq!(implement.len(), 2, "the resumed run made a second Attempt");
    assert!(
        implement
            .iter()
            .any(|a| matches!(a.result, Some(TaskAttemptResultV1::Succeeded { .. })))
    );
}

/// The interrupted Attempt's invocation: the first started Attempt is the implementer's.
fn attempts_invocation(
    attempts: &[review_store::store::task::execution::TaskAttemptAccounting],
) -> String {
    attempts
        .iter()
        .filter(|a| a.started)
        .min_by_key(|a| a.started_unix_ms)
        .unwrap()
        .invocation_id
        .clone()
}

#[test]
fn sigterm_stops_the_worker_group_and_exits_143() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    long_running_implementer(&repo, &ready, &directory.path().join("worker-resume"));
    let out = interrupt_running_worker(&repo, &state, &ready, &[Signal::SIGTERM]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        shell_status(&out.status),
        (Some(Signal::SIGTERM), 143),
        "{stderr}"
    );
    assert!(stderr.contains("interrupted by SIGTERM"), "{stderr}");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, TASK).unwrap().unwrap();
    assert!(!matches!(task.phase, TaskPhaseV1::Finished { .. }));
    assert!(task.execution.unwrap().pending_attempts().is_empty());
}

/// A second SIGINT may arrive while the first is still stopping the Workers or after; either
/// way af exits 130 at once and leaves no Worker process behind.
#[test]
fn a_second_sigint_still_leaves_no_worker_process() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    long_running_implementer(&repo, &ready, &directory.path().join("worker-resume"));
    let out = interrupt_running_worker(&repo, &state, &ready, &[Signal::SIGINT, Signal::SIGINT]);
    assert_eq!(
        shell_status(&out.status),
        (Some(Signal::SIGINT), 130),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The issue's own command: a Task planned first and then run with `af task run`; SIGINT to
/// that `af task run` stops the Worker and its child, settles the Attempt as cancelled, exits
/// as SIGINT (130 in a shell) and leaves the Task resumable.
#[test]
fn sigint_to_af_task_run_stops_its_worker_and_the_task_resumes() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    let resume = directory.path().join("worker-resume");
    long_running_implementer(&repo, &ready, &resume);
    let planned = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--file",
            "ticket.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    let preview: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {} {}",
            String::from_utf8_lossy(&planned.stdout),
            String::from_utf8_lossy(&planned.stderr)
        )
    });
    let plan = preview["plan_id"]
        .as_str()
        .expect("the preview names its plan")
        .to_owned();

    let run = [
        "task",
        "run",
        TASK,
        "--confirm-plan",
        plan.as_str(),
        "--json",
    ];
    let out = interrupt_running_af(&repo, &state, &ready, &[Signal::SIGINT], &run);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        shell_status(&out.status),
        (Some(Signal::SIGINT), 130),
        "{stderr}"
    );
    assert!(stderr.contains("interrupted by SIGINT"), "{stderr}");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    {
        let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let task = store.task_projection(&cas, TASK).unwrap().unwrap();
        assert!(!matches!(task.phase, TaskPhaseV1::Finished { .. }));
        let execution = task.execution.as_ref().unwrap();
        assert!(execution.pending_attempts().is_empty());
        let attempts = execution.attempt_accounting();
        let started: Vec<_> = attempts.iter().filter(|a| a.started).collect();
        assert_eq!(started.len(), 1, "{attempts:#?}");
        let Some(TaskAttemptResultV1::Failed { diagnostic_id, .. }) = &started[0].result else {
            panic!("the interrupted Attempt is settled, not succeeded: {attempts:#?}");
        };
        assert!(
            cas.get_json(diagnostic_id)
                .unwrap()
                .to_string()
                .contains("cancelled"),
            "the Attempt is recorded as cancelled"
        );
    }

    std::fs::write(&resume, b"").unwrap();
    let resumed = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args(["task", "run", TASK, "--json", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {} {}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        )
    });
    assert_eq!(resumed.status.code(), Some(0), "{value:#}");
    assert_eq!(value["result"]["acceptance"], "satisfied", "{value:#}");
}
