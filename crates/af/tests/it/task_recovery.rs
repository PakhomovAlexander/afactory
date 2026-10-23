//! A writer lost inside an Attempt is resumable and not charged its reservation, and a SIGTERM
//! resume dispatches the interrupted node again (ADR-0197). The implementer slot keeps
//! `max_attempts = 1`, as every `implementation-reviewed` slot does, so each resume here would
//! have been refused with `exhausted its Attempt limit` before. Command Workers only.
use crate::{schemas, task_cli, task_interrupt};
use nix::sys::signal::Signal;
use review_core::task::TaskPhaseV1;
use review_core::task::execution::TaskAttemptResultV1;
use review_store::store::task::execution::TaskAbandonCause;
use review_store::{Cas, EventStore};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const TASK: &str = task_interrupt::TASK;
const NODE: &str = "root.nodes.implement";
/// The lease the runs here take (`AF_TEST_TASK_LEASE_MS`, debug builds only), so a killed run's
/// successor waits seconds, not the production minute.
const LEASE_MS: &str = "10000";

struct Fixture {
    _directory: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    ready: PathBuf,
    resume: PathBuf,
}

/// The pagination fixture with an implementer that starts a long-running child and waits on it
/// until `resume` exists, and keeps its slot's single Attempt. A command Worker reserves no
/// model tokens, so what an abandoned Attempt is charged against a nonzero reservation is
/// proven by the Store's own tests (ADR-0197); here every reservation must still come back.
/// `recovery` declares the slot's `recovery_attempts`, which makes the pipeline `af.pipeline/3`.
fn fixture(recovery: Option<u32>) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    let resume = directory.path().join("worker-resume");
    task_cli::load_safe_walls(&repo);
    let package = repo.join(".af/task-packages/fixture/implementer");
    let worker = package.join("worker.py");
    let original = std::fs::read_to_string(&worker).unwrap();
    let marker = "request=json.load(sys.stdin)\n";
    assert_eq!(original.matches(marker).count(), 1);
    let hold = format!(
        "{marker}import os,subprocess\nif not os.path.exists({resume:?}):\n    child=subprocess.Popen(['/bin/sleep','90'])\n    open({ready:?}+'.tmp','w').write('%d %d' % (os.getpid(), child.pid))\n    os.rename({ready:?}+'.tmp',{ready:?})\n    child.wait()\n",
        resume = resume.to_str().unwrap(),
        ready = ready.to_str().unwrap(),
    );
    std::fs::write(&worker, original.replacen(marker, &hold, 1)).unwrap();
    if let Some(count) = recovery {
        let pipeline = repo.join(".af/task-packages/fixture/implementation/pipeline.toml");
        let text = std::fs::read_to_string(&pipeline).unwrap();
        let slot = "role = \"implement\"\n";
        assert_eq!(text.matches(slot).count(), 1);
        std::fs::write(
            &pipeline,
            text.replacen(
                "schema = \"af.pipeline/1\"",
                "schema = \"af.pipeline/3\"",
                1,
            )
            .replacen(slot, &format!("{slot}recovery_attempts = {count}\n"), 1),
        )
        .unwrap();
    }
    let ticket = repo.join("ticket.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&ticket).unwrap()).unwrap();
    task_cli::load_safe_limits(&mut file["limits"]);
    std::fs::write(&ticket, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    task_interrupt::commit_fixture(&repo);
    Fixture {
        _directory: directory,
        repo,
        state,
        ready,
        resume,
    }
}

fn af(f: &Fixture, args: &[&str]) -> std::process::Command {
    let mut command = crate::common::af();
    command
        .current_dir(&f.repo)
        .args(args)
        .arg("--state")
        .arg(&f.state)
        .env("AF_TEST_TASK_LEASE_MS", LEASE_MS);
    command
}

/// Kills af and its Worker group if a test fails before they ended.
struct Running {
    af: Option<Child>,
    worker_group: i32,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.worker_group),
            Signal::SIGKILL,
        );
        if let Some(child) = &mut self.af {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// Start `af ARGS` and return once its implementer and the implementer's child run.
fn until_the_worker_runs(f: &Fixture, args: &[&str]) -> (Running, Vec<i32>) {
    let _ = std::fs::remove_file(&f.ready);
    let mut child = af(f, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    let pids: Vec<i32> = loop {
        if let Ok(text) = std::fs::read_to_string(&f.ready) {
            let pids: Vec<i32> = text
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if pids.len() == 2 {
                break pids;
            }
        }
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            panic!(
                "the Worker did not start: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(Instant::now() < until, "Worker readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    (
        Running {
            af: Some(child),
            worker_group: pids[0],
        },
        pids,
    )
}

/// SIGTERM to af mid-Attempt: af stops its Worker, settles the Attempt and exits 143.
fn sigterm(f: &Fixture, args: &[&str]) {
    let (mut running, _) = until_the_worker_runs(f, args);
    let child = running.af.take().unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        Signal::SIGTERM,
    )
    .unwrap();
    let out = child.wait_with_output().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        out.status.signal(),
        Some(Signal::SIGTERM as i32),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SIGKILL to af mid-Attempt, as a closed lid, a reboot or a killed launcher ends it: nothing
/// settles the Attempt or releases the lease. The orphaned Worker group is then killed too.
fn sigkill(f: &Fixture, args: &[&str]) {
    let (mut running, pids) = until_the_worker_runs(f, args);
    let mut child = running.af.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pids[0]), Signal::SIGKILL).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|pid| alive(*pid)) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// `af task show --json` once the killed run's lease expired and the Task reads abandoned.
fn abandoned_inspection(f: &Fixture) -> Value {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let shown = json(&af(f, &["task", "show", TASK, "--json"]).output().unwrap());
        if shown.get("cause").is_some() {
            return shown;
        }
        assert!(Instant::now() < until, "never abandoned: {shown:#}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn first_line(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `resuming Task X: node N was interrupted at 17:41:03Z<rest>`, with any time of day.
fn assert_resumed(line: &str, rest: &str) {
    let prefix = format!("resuming Task {TASK}: node {NODE} was interrupted at ");
    let time = line
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{line}"));
    assert_eq!(&time[8..], rest, "{line}");
    assert!(
        time[..9].chars().enumerate().all(|(n, c)| match n {
            2 | 5 => c == ':',
            8 => c == 'Z',
            _ => c.is_ascii_digit(),
        }),
        "{line}"
    );
}

fn projection(f: &Fixture) -> review_store::store::task::TaskProjection {
    let cas = Cas::open_existing(f.state.join("cas")).unwrap();
    let store = EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
    store.task_projection(&cas, TASK).unwrap().unwrap()
}

/// Items 2, 3 and 6: `af task run` killed mid-Attempt reads `phase abandoned` with cause
/// `writer_lost`; `af task run ID` then settles the lost Attempt as abandoned, charges it only
/// what was reported (nothing, so its usage is unknown) with every reservation back in the
/// Task's budget, dispatches the node again and the Task is accepted with one chargeable and
/// one abandoned Attempt.
#[test]
fn a_killed_run_is_abandoned_then_resumed_with_one_chargeable_and_one_abandoned_attempt() {
    let f = fixture(None);
    let planned = json(
        &af(&f, &["task", "start", "--file", "ticket.json", "--json"])
            .output()
            .unwrap(),
    );
    let plan = planned["plan_id"].as_str().unwrap().to_owned();
    sigkill(
        &f,
        &["task", "run", TASK, "--confirm-plan", &plan, "--json"],
    );

    // Snapshot after the kill: abandoned by a lost writer, and what continues it.
    let shown = abandoned_inspection(&f);
    schemas::valid(&schemas::validator("task-inspection-v12.json"), &shown);
    assert_eq!(shown["schema"], "af/task-inspection@12");
    assert_eq!(
        shown["phase"]["kind"], "running",
        "the recorded phase stays"
    );
    assert_eq!(shown["cause"], "writer_lost");
    assert_eq!(shown["next_action"], format!("af task run {TASK}"));
    let text = String::from_utf8(af(&f, &["task", "show", TASK]).output().unwrap().stdout).unwrap();
    assert!(
        text.contains(&format!(
            "phase abandoned (cause writer_lost): node {NODE} stopped at "
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nnext: af task run {TASK}\n")),
        "{text}"
    );
    let listed = json(&af(&f, &["task", "list", "--json"]).output().unwrap());
    let entry = &listed["tasks"][0];
    schemas::valid(&schemas::validator("task-list-entry-v3.json"), entry);
    assert_eq!(
        (&entry["cause"], &entry["next_action"]),
        (
            &Value::from("writer_lost"),
            &Value::from(format!("af task run {TASK}"))
        )
    );
    let listed = String::from_utf8(af(&f, &["task", "list"]).output().unwrap().stdout).unwrap();
    assert!(
        listed.contains(&format!(
            "abandoned (writer_lost); next: af task run {TASK}"
        )),
        "{listed}"
    );

    std::fs::write(&f.resume, b"").unwrap();
    let resumed = af(&f, &["task", "run", TASK, "--json"]).output().unwrap();
    let value = json(&resumed);
    assert_eq!(resumed.status.code(), Some(0), "{value:#}");
    assert_eq!(value["result"]["acceptance"], "satisfied", "{value:#}");
    assert_resumed(
        &first_line(&resumed),
        "Z (its writer was lost); settling the lost Attempt and dispatching a new one (1 \
         recovery of 1)",
    );
    assert!(
        value.get("cause").is_none(),
        "a finished Task is not abandoned"
    );

    let task = projection(&f);
    assert!(matches!(task.phase, TaskPhaseV1::Finished { .. }));
    let execution = task.execution.as_ref().unwrap();
    let implement: Vec<_> = execution
        .attempt_accounting()
        .into_iter()
        .filter(|row| row.started && row.reservation.node == NODE)
        .collect();
    assert_eq!(implement.len(), 2, "{implement:#?}");
    let recovery = execution.node_recovery(NODE).unwrap();
    assert_eq!(
        (recovery.chargeable, recovery.abandoned),
        (1, 1),
        "attempts: 1 chargeable, 1 abandoned"
    );
    let lost = implement
        .iter()
        .find(|row| row.abandoned.is_some())
        .unwrap();
    assert_eq!(lost.abandoned, Some(TaskAbandonCause::WriterLost));
    assert!(matches!(
        lost.result,
        Some(TaskAttemptResultV1::Abandoned { .. })
    ));
    assert_eq!(lost.charged_tokens, 0, "never its reservation");
    assert_eq!(
        lost.unknown_usage,
        Some(review_core::task::usage::TaskUnknownUsageCauseV1::LeaseExpired),
        "nothing was reported before the loss"
    );
    assert!(
        implement
            .iter()
            .any(|row| matches!(row.result, Some(TaskAttemptResultV1::Succeeded { .. })))
    );
    // Every reservation is back: the Task's remaining tokens are its whole limit.
    assert_eq!(execution.budget.reserved_tokens(), 0);
    assert_eq!(execution.budget.committed_tokens(), 0);
    assert_eq!(
        execution.budget.remaining_limits().tokens,
        task.revision.limits.tokens
    );
}

/// Item 4: SIGTERM mid-Attempt, then `af task run ID`: its first line says the node runs again,
/// and it does, though the slot allows one Attempt.
#[test]
fn a_sigterm_resume_dispatches_the_interrupted_node_again() {
    let f = fixture(None);
    sigterm(
        &f,
        &[
            "task",
            "start",
            "--file",
            "ticket.json",
            "--execute",
            "--json",
        ],
    );
    let shown = json(&af(&f, &["task", "show", TASK, "--json"]).output().unwrap());
    assert_eq!(shown["cause"], "host_cancelled", "{shown:#}");
    assert_eq!(shown["next_action"], format!("af task run {TASK}"));

    std::fs::write(&f.resume, b"").unwrap();
    let resumed = af(&f, &["task", "run", TASK, "--json"]).output().unwrap();
    let value = json(&resumed);
    assert_eq!(resumed.status.code(), Some(0), "{value:#}");
    assert_eq!(value["result"]["acceptance"], "satisfied", "{value:#}");
    assert_resumed(
        &first_line(&resumed),
        "Z; dispatching a new Attempt (1 recovery of 1)",
    );
    let task = projection(&f);
    let recovery = task
        .execution
        .as_ref()
        .unwrap()
        .node_recovery(NODE)
        .unwrap();
    assert_eq!((recovery.chargeable, recovery.abandoned), (1, 1));
    let cancelled = task
        .execution
        .as_ref()
        .unwrap()
        .attempt_accounting()
        .into_iter()
        .find(|row| row.abandoned.is_some())
        .unwrap();
    assert_eq!(cancelled.abandoned, Some(TaskAbandonCause::HostCancelled));
}

/// Item 4, recovery exhausted: interrupted once, then its writer lost on the resumed run, the
/// node has used its one recovery. The next `af task run` says on its first line why it cannot
/// continue and what to do, exits 1 and runs nothing.
#[test]
fn a_node_past_its_recovery_allowance_cannot_continue_and_says_why_on_line_one() {
    let f = fixture(None);
    sigterm(
        &f,
        &[
            "task",
            "start",
            "--file",
            "ticket.json",
            "--execute",
            "--json",
        ],
    );
    sigkill(&f, &["task", "run", TASK, "--json"]);
    abandoned_inspection(&f);
    let refused = af(&f, &["task", "run", TASK]).output().unwrap();
    assert_eq!(refused.status.code(), Some(1));
    let line = first_line(&refused);
    assert_eq!(
        line,
        format!(
            "af task: cannot continue Task {TASK}: node {NODE} lost its writer 1 time and was \
             interrupted 1 time (recovery allowance 1); fix: start it again with `af task start \
             --file TASK_FILE` (the Task file it was started from), or allow more with \
             `[slots.implementer] recovery_attempts` in its pipeline (af.pipeline/3)"
        )
    );
    let shown = json(&af(&f, &["task", "show", TASK, "--json"]).output().unwrap());
    assert_eq!(shown["cause"], "writer_lost");
    assert_eq!(shown["next_action"], "af task start --file TASK_FILE");
    let task = projection(&f);
    let execution = task.execution.as_ref().unwrap();
    let recovery = execution.node_recovery(NODE).unwrap();
    assert_eq!((recovery.chargeable, recovery.abandoned), (0, 2));
    assert!(execution.pending_attempts().is_empty());
}

/// A slot that declares `recovery_attempts = 2` (`af.pipeline/3`) compiles to an
/// `af.compiled-task/2` graph and recovers twice.
#[test]
fn a_declared_recovery_allowance_dispatches_the_node_again_as_often_as_it_allows() {
    let f = fixture(Some(2));
    let planned = json(
        &af(&f, &["task", "start", "--file", "ticket.json", "--json"])
            .output()
            .unwrap(),
    );
    schemas::valid(&schemas::validator("task-inspection-v12.json"), &planned);
    assert_eq!(planned["graph"]["schema"], "af.compiled-task/2");
    schemas::valid(
        &schemas::validator("compiled-task-v2.json"),
        &planned["graph"],
    );
    let slot = planned["graph"]["slots"]
        .as_object()
        .unwrap()
        .values()
        .find(|slot| slot["role"] == "implement")
        .unwrap();
    assert_eq!(slot["recovery_attempts"], 2);
    let plan = planned["plan_id"].as_str().unwrap().to_owned();
    sigterm(
        &f,
        &["task", "run", TASK, "--confirm-plan", &plan, "--json"],
    );
    sigterm(&f, &["task", "run", TASK, "--json"]);
    std::fs::write(&f.resume, b"").unwrap();
    let resumed = af(&f, &["task", "run", TASK, "--json"]).output().unwrap();
    assert_eq!(resumed.status.code(), Some(0), "{:#}", json(&resumed));
    assert_resumed(
        &first_line(&resumed),
        "Z; dispatching a new Attempt (2 recovery of 2)",
    );
    let recovery = projection(&f)
        .execution
        .unwrap()
        .node_recovery(NODE)
        .unwrap();
    assert_eq!(
        (recovery.chargeable, recovery.abandoned, recovery.allowance),
        (1, 2, 2)
    );
}
