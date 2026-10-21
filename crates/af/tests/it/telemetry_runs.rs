//! Telemetry's kernel integration end to end (design §15 P2): what a Task run records, the
//! consumer's bounded shutdown, `af telemetry runs`, `show`, `tail` and `gc`, and the telemetry
//! kind of the Storage Budget — each over a private HOME and private XDG roots.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

use crate::task_gc;

/// A private machine: every root af reads lives below one temporary directory.
pub(crate) struct Machine {
    _root: tempfile::TempDir,
    pub(crate) root: PathBuf,
}

impl Machine {
    pub(crate) fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for name in ["home", "config", "xstate", "data", "cache"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        Self {
            _root: directory,
            root,
        }
    }

    /// The configured telemetry directory.
    pub(crate) fn telemetry(&self) -> PathBuf {
        self.root.join("xstate/af/telemetry")
    }

    /// The directory registry.
    pub(crate) fn registry(&self) -> PathBuf {
        self.root.join("xstate/af/telemetry-dirs.v1.json")
    }

    pub(crate) fn command(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Command {
        let mut command = crate::common::af();
        command
            .current_dir(cwd)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("xstate"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("AF_VERSION")
            .env_remove("AF_TELEMETRY__LEVEL")
            .env_remove("AF_TEST_FREE_BYTES")
            .env_remove("AF_TEST_TELEMETRY_STALL_AFTER_WRITES")
            .env_remove("AF_TEST_TELEMETRY_STALL_SEAL")
            .env_remove("AF_TEST_TELEMETRY_STALL_COMMAND_END")
            .env_remove("AF_TEST_TASK_PAUSE_AFTER_RUN_OPENS")
            .env_remove("AF_TEST_TELEMETRY_EVICT_PAUSE")
            .env_remove("CARGO_TARGET_DIR")
            .args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    pub(crate) fn af(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.command(cwd, env, args).output().unwrap()
    }
}

pub(crate) fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

pub(crate) fn json(output: &Output) -> Value {
    let (stdout, stderr) = text(output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("{stdout}\n{stderr}"))
}

/// `af task start --execute` of a Task file in `repo`, with the machine settings `env`.
fn start(machine: &Machine, repo: &Path, state: &Path, file: &str, env: &[(&str, &str)]) -> Output {
    machine.af(
        repo,
        env,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file,
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    )
}

/// Every row of a Store's event log, each column as its text.
fn events(state: &Path) -> Vec<Vec<String>> {
    let connection = rusqlite::Connection::open_with_flags(
        state.join("events.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT run_id, sequence, event_id, type, occurred_at, node_id, attempt_id,
                    causation_id, correlation_id, artifact_refs, payload
             FROM events ORDER BY run_id, sequence",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((0..11)
                .map(|index| match row.get_ref(index).unwrap() {
                    rusqlite::types::ValueRef::Null => "NULL".to_string(),
                    rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                    rusqlite::types::ValueRef::Real(value) => value.to_string(),
                    rusqlite::types::ValueRef::Text(text) => {
                        String::from_utf8_lossy(text).into_owned()
                    }
                    rusqlite::types::ValueRef::Blob(bytes) => format!("{bytes:?}"),
                })
                .collect())
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn copy_tree(from: &Path, to: &Path) {
    assert!(
        Command::new("cp")
            .arg("-a")
            .arg(from)
            .arg(to)
            .status()
            .unwrap()
            .success()
    );
}

/// Every path beneath `root`, relative, sorted.
fn listing(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            out.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    out.sort();
    out
}

/// §12 and ADR-0153: `af --help`, `af --version` and every read command but `af storage` and
/// `af telemetry`, at the default level against a fresh state root, leave no telemetry directory
/// and no registry entry; a Task run at `off` opens nothing either.
#[test]
fn telemetry_off_opens_nothing() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let reads: [&[&str]; 12] = [
        &["--help"],
        &["--version"],
        &["--version", "--json"],
        &["help"],
        &["help", "telemetry"],
        &["help", "storage"],
        &["config", "show"],
        &["config", "show", "--origin"],
        &["config", "paths"],
        &["task", "list", "--state", state.to_str().unwrap()],
        &["completions", "bash"],
        &["telemetry", "--help"],
    ];
    for args in reads {
        let _ = machine.af(&repo, &[], args);
    }
    let state_root = machine.root.join("xstate/af");
    let after_reads = listing(&state_root);
    assert!(
        after_reads.iter().all(|path| !path.contains("telemetry")),
        "{after_reads:?}"
    );
    assert!(!machine.telemetry().exists() && !machine.registry().exists());

    // A Task run at `off`: no segment, no consumer, no directory, no registry entry.
    let output = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[("AF_TELEMETRY__LEVEL", "off")],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(!stderr.contains("telemetry"), "{stderr}");
    assert!(!machine.telemetry().exists(), "nothing at off");
    assert!(!machine.registry().exists(), "no registry entry at off");
    // And a read of what that run left, at the default level, still opens nothing.
    for args in [
        vec![
            "task",
            "show",
            "gc-older",
            "--state",
            state.to_str().unwrap(),
        ],
        vec!["task", "list", "--state", state.to_str().unwrap(), "--json"],
    ] {
        assert!(machine.af(&repo, &[], &args).status.success());
    }
    assert!(!machine.telemetry().exists() && !machine.registry().exists());
}

/// `af task show --json` with every wall time taken out: the Attempt walls' start and elapsed.
fn without_wall_times(mut shown: Value) -> Value {
    if let Some(walls) = shown.get_mut("attempt_walls").and_then(Value::as_array_mut) {
        for wall in walls {
            if let Some(wall) = wall.as_object_mut() {
                wall.remove("started_unix_ms");
                wall.remove("elapsed_ms");
            }
        }
    }
    shown
}

/// ADR-0153 clause 3 (design §16 item 1): under the kernel's injected clock and a budget with
/// room enough that the sweep evicts nothing, the same planned Task run at `off` and at
/// `content` leaves byte-identical Store event logs — the `storage_sweep` records included —
/// and the same `af task show --json` apart from wall times. Nothing here is normalised: the
/// clock is the injected one, and the resuming writer's name the injected process id.
#[test]
fn telemetry_never_steers() {
    let machine = Machine::new();
    let work = machine.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let (repo, state) = task_gc::fixture(&work);
    let clock = [("AF_TEST_CLOCK_FIXED_MS", "1700000000000")];
    let planned = machine.af(
        &repo,
        &clock,
        &[
            "task",
            "start",
            "--file",
            "gc-older.json",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let planned: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert!(planned["plan_id"].is_string(), "{planned}");
    let pristine = machine.root.join("pristine");
    copy_tree(&work, &pristine);
    let mut logs = Vec::new();
    let mut shown = Vec::new();
    for level in ["off", "content"] {
        std::fs::remove_dir_all(&work).unwrap();
        copy_tree(&pristine, &work);
        let output = machine.af(
            &repo,
            &[
                clock[0],
                ("AF_TEST_WRITER_PID", "4242"),
                ("AF_TELEMETRY__LEVEL", level),
            ],
            &[
                "task",
                "run",
                "gc-older",
                "--execute",
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        );
        let (stdout, stderr) = text(&output);
        let document: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| panic!("{level}: {stdout}\n{stderr}"));
        assert_eq!(document["phase"]["kind"], "finished", "{level}: {stderr}");
        assert!(!stderr.contains("telemetry problem"), "{level}: {stderr}");
        if level == "off" {
            assert!(!machine.telemetry().exists(), "nothing recorded at off");
        } else {
            let runs = json(&machine.af(&repo, &[], &["telemetry", "runs", "--json"]));
            assert_eq!(runs["runs"].as_array().unwrap().len(), 1, "{runs}");
        }
        logs.push(events(&state));
        shown.push(without_wall_times(json(&machine.af(
            &repo,
            &[],
            &[
                "task",
                "show",
                "gc-older",
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        ))));
    }
    assert!(!logs[0].is_empty());
    assert_eq!(logs[0], logs[1], "the Store's event log is byte-identical");
    let sweeps = |log: &[Vec<String>]| {
        log.iter()
            .filter(|row| row[10].contains("\"storage_sweep\""))
            .count()
    };
    assert_eq!(sweeps(&logs[0]), sweeps(&logs[1]));
    assert_eq!(shown[0], shown[1]);
}

/// ADR-0153 clause 5 (design §16 item 4): with `sink = "none"` and `harness_native = false` a
/// Task run opens no listener and no connection, however often it is looked at while it runs.
#[test]
fn telemetry_default_opens_no_socket() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let mut child = machine
        .command(
            &repo,
            &[("AF_TELEMETRY__LEVEL", "content")],
            &[
                "task",
                "start",
                "--execute",
                "--file",
                "gc-older.json",
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let mut looks = 0;
    let mut sockets = Vec::new();
    while child.try_wait().unwrap().is_none() {
        let fds = PathBuf::from(format!("/proc/{pid}/fd"));
        if fds.is_dir() {
            for entry in std::fs::read_dir(&fds).into_iter().flatten().flatten() {
                if let Ok(target) = std::fs::read_link(entry.path())
                    && target.to_string_lossy().starts_with("socket:")
                {
                    sockets.push(target.display().to_string());
                }
            }
            looks += 1;
        } else if let Ok(output) = Command::new("lsof")
            .args(["-nP", "-a", "-p", &pid, "-i"])
            .output()
        {
            let listed = String::from_utf8_lossy(&output.stdout).into_owned();
            if listed.lines().count() > 1 {
                sockets.push(listed);
            }
            looks += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(child.wait().unwrap().success());
    assert!(looks > 0, "the run was looked at while it ran");
    assert!(sockets.is_empty(), "{sockets:?}");
    // Nothing in the telemetry crate or af's telemetry side names a socket.
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut sources = Vec::new();
    for root in [
        workspace.join("crates/review-telemetry/src"),
        workspace.join("crates/af/src/telemetry"),
    ] {
        let mut pending = vec![root];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)
                .unwrap()
                .filter_map(Result::ok)
            {
                if entry.path().is_dir() {
                    pending.push(entry.path());
                } else {
                    sources.push(entry.path());
                }
            }
        }
    }
    for source in sources {
        let text = std::fs::read_to_string(&source).unwrap();
        for socket in [
            "std::net",
            "TcpListener",
            "TcpStream",
            "UdpSocket",
            "UnixListener",
        ] {
            assert!(!text.contains(socket), "{}: {socket}", source.display());
        }
    }
}

/// Design §5: a resume under a changed configuration after an idle interval is a second
/// execution of the same run; active time is the two executions' union, execution time their
/// sum, elapsed time from the first start to the last end, idle gap included.
#[test]
fn telemetry_executions_of_a_resume() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    // The first execution is stopped by the free-disk floor before its Worker starts.
    let first = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[("AF_TEST_FREE_BYTES", "1024")],
    );
    assert_eq!(first.status.code(), Some(1), "{}", text(&first).1);
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    // The resume runs under another configuration.
    let resumed = machine.af(
        &repo,
        &[("AF_TELEMETRY__FLUSH_BYTES", "8KiB")],
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&resumed);
    assert!(resumed.status.success(), "{stdout}\n{stderr}");
    let runs = json(&machine.af(&repo, &[], &["telemetry", "runs", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), &runs);
    let [run] = runs["runs"].as_array().unwrap().as_slice() else {
        panic!("one run: {runs}");
    };
    assert_eq!(run["task_id"], "gc-older");
    assert_eq!(run["executions"]["retained"], 2, "{run}");
    assert_eq!(run["executions"]["open"], serde_json::json!([]), "{run}");
    let executions = run["executions"]["list"].as_array().unwrap();
    let ordinals: Vec<u64> = executions
        .iter()
        .map(|execution| execution["ordinal"].as_u64().unwrap())
        .collect();
    assert_eq!(ordinals, [1, 2], "one lease each");
    let walls: u64 = executions
        .iter()
        .map(|execution| execution["wall_us"].as_u64().unwrap())
        .sum();
    let active = run["active_us"].as_u64().unwrap();
    let execution = run["execution_us"].as_u64().unwrap();
    let elapsed = run["elapsed_us"].as_u64().unwrap();
    // Each figure is truncated to microseconds from the nanoseconds it sums.
    assert!(execution.abs_diff(walls) <= 2, "{execution} {walls}");
    assert_eq!(active, execution, "no overlap");
    assert!(elapsed >= active + 1_500_000, "the idle gap: {run}");
    assert_eq!(run["history"], "prefix:2");
    assert_eq!(run["retained"], "retained executions 1–2");
    assert_eq!(run["outcome"], "finished");
    let first_sequence: u64 = executions[0]["store_sequence_at_start"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let second_sequence: u64 = executions[1]["store_sequence_at_start"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(second_sequence > first_sequence);
}

/// The `(epoch, change kind)` of every Task transition of the Store `state` whose change is
/// `kind`, in log order.
fn transitions(state: &Path, kind: &str) -> Vec<u64> {
    events(state)
        .iter()
        .filter(|row| row[3] == "TaskTransition@5")
        .filter_map(|row| {
            let payload: Value = serde_json::from_str(&row[10]).unwrap();
            (payload["change"]["kind"] == kind).then(|| payload["epoch"].as_u64().unwrap())
        })
        .collect()
}

/// The epochs of the execution markers (`TaskExecutionStarted@1`, the Task log's own event kind
/// beside its transitions) in the one Task Store under `state`, in log order.
fn markers(state: &Path) -> Vec<u64> {
    events(state)
        .iter()
        .filter(|row| row[3] == "TaskExecutionStarted@1")
        .map(|row| {
            let payload: Value = serde_json::from_str(&row[10]).unwrap();
            assert_eq!(payload["schema"], "af/TaskExecutionStarted@1");
            payload["epoch"].as_u64().unwrap()
        })
        .collect()
}

/// The ordinals of the retained executions of the one run of Task `task`, oldest first, with
/// the run.
fn ordinals(machine: &Machine, repo: &Path, task: &str) -> (Vec<u64>, Value) {
    let runs = json(&machine.af(repo, &[], &["telemetry", "runs", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), &runs);
    let of_task: Vec<&Value> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run["task_id"] == task)
        .collect();
    let [run] = of_task.as_slice() else {
        panic!("one run of {task}: {runs}");
    };
    let ordinals = run["executions"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|execution| execution["ordinal"].as_u64().unwrap())
        .collect();
    (ordinals, (*run).clone())
}

/// Design §5: every lease that begins kernel execution commits its execution marker
/// (`TaskExecutionStarted@1`) before its `af.run` span opens, and the ordinal counts exactly the markers before it. An
/// execution the free-disk floor stops before any Attempt starts still takes ordinal 1, and
/// the resume takes 2: never a second 1, so `history` never calls a duplicate a prefix.
#[test]
fn telemetry_refused_before_an_attempt_keeps_its_ordinal() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let refused = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[("AF_TEST_FREE_BYTES", "1024")],
    );
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("insufficient_disk"), "{stderr}");
    assert_eq!(markers(&state), [1]);
    let resumed = machine.af(
        &repo,
        &[],
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&resumed);
    assert!(resumed.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        markers(&state),
        [1, 2],
        "one marker per lease that began execution"
    );
    let (ordinals, run) = ordinals(&machine, &repo, "gc-older");
    assert_eq!(ordinals, [1, 2], "{run}");
    assert_eq!(run["history"], "prefix:2", "{run}");
    assert_eq!(run["missing_ordinals"], serde_json::json!([]), "{run}");
}

/// Design §5: an execution killed after its `af.run` span opened and before any Attempt or run
/// report still took ordinal 1 — its marker was committed before the span opened — and the
/// resume after the dead writer's lease expired is execution 2.
#[test]
fn telemetry_interrupted_before_an_attempt_keeps_its_ordinal() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let pauses = machine.root.join("pauses");
    std::fs::create_dir_all(&pauses).unwrap();
    let mut child = machine
        .command(
            &repo,
            &[(
                "AF_TEST_TASK_PAUSE_AFTER_RUN_OPENS",
                pauses.to_str().unwrap(),
            )],
            &[
                "task",
                "start",
                "--execute",
                "--file",
                "gc-older.json",
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let paused = pauses.join(format!("{}.paused", child.id()));
    let segments = machine.telemetry().join("v1/segments");
    let run_visible = || {
        std::fs::read_dir(&segments).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry.file_name().to_string_lossy().ends_with(".jsonl")
                    && std::fs::read_to_string(entry.path())
                        .is_ok_and(|text| text.contains(r#""name":"af.run""#))
            })
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !(paused.exists() && run_visible()) {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the execution pauses after af.run opened"
        );
        assert!(std::time::Instant::now() < deadline, "the execution pauses");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(markers(&state), [1]);
    assert!(
        transitions(&state, "execution_recorded").is_empty()
            && transitions(&state, "run_reported").is_empty(),
        "killed before any Attempt or run report"
    );
    // The resume takes over only once the dead writer's lease (15 s from its last renewal) has
    // observably expired: a takeover the Store refuses as premature is retried, nothing else.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let resumed = loop {
        let resumed = machine.af(
            &repo,
            &[],
            &[
                "task",
                "run",
                "gc-older",
                "--execute",
                "--state",
                state.to_str().unwrap(),
                "--json",
            ],
        );
        let (stdout, stderr) = text(&resumed);
        let premature = !resumed.status.success()
            && format!("{stdout}\n{stderr}")
                .contains("Task lease takeover is premature or has a stale epoch");
        if !premature || std::time::Instant::now() >= deadline {
            break resumed;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    let (stdout, stderr) = text(&resumed);
    assert!(resumed.status.success(), "{stdout}\n{stderr}");
    assert_eq!(markers(&state), [1, 2]);
    let (ordinals, run) = ordinals(&machine, &repo, "gc-older");
    assert_eq!(ordinals, [1, 2], "{run}");
    assert_eq!(run["missing_ordinals"], serde_json::json!([]), "{run}");
    assert_ne!(run["history"], "prefix:1", "{run}");
}

/// Each retained execution of the one run of Task `task` in `af telemetry runs --json`, as
/// (ordinal, the `af.epoch` its `af.run` span carries), oldest first, with the run.
fn ordinals_and_epochs(machine: &Machine, repo: &Path, task: &str) -> (Vec<(u64, u64)>, Value) {
    let runs = json(&machine.af(repo, &[], &["telemetry", "runs", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), &runs);
    let of_task: Vec<&Value> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run["task_id"] == task)
        .collect();
    let [run] = of_task.as_slice() else {
        panic!("one run of {task}: {runs}");
    };
    let shown = json(&machine.af(
        repo,
        &[],
        &[
            "telemetry",
            "show",
            run["run_id"].as_str().unwrap(),
            "--json",
        ],
    ));
    let epochs: std::collections::BTreeMap<&str, u64> = shown["spans"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|span| span["name"] == "af.run")
        .map(|span| {
            (
                span["span_id"].as_str().unwrap(),
                span["attributes"]["af.epoch"].as_u64().unwrap(),
            )
        })
        .collect();
    let pairs = run["executions"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|execution| {
            (
                execution["ordinal"].as_u64().unwrap(),
                epochs[execution["span_id"].as_str().unwrap()],
            )
        })
        .collect();
    (pairs, (*run).clone())
}

/// Design §5: the execution ordinal counts the committed leases that began a kernel run, never
/// the lease epoch. `af task start` without `--execute` plans under lease epoch 1 and runs
/// nothing; the `af task run` that follows (epoch 2) is the run's first execution, so its
/// history is `prefix:1` with no missing execution, and the epoch stays in `af.epoch`.
#[test]
fn telemetry_plan_only_start_is_not_a_missing_execution() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let planned = machine.af(
        &repo,
        &[],
        &[
            "task",
            "start",
            "--file",
            "gc-older.json",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let planned = json(&planned);
    assert!(planned["plan_id"].is_string(), "{planned}");
    assert!(
        !machine.telemetry().exists(),
        "planning alone runs no kernel run"
    );
    let ran = machine.af(
        &repo,
        &[],
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&ran);
    assert!(ran.status.success(), "{stdout}\n{stderr}");
    let (pairs, run) = ordinals_and_epochs(&machine, &repo, "gc-older");
    assert_eq!(pairs, [(1, 2)], "ordinal 1 under lease epoch 2: {run}");
    assert_eq!(run["history"], "prefix:1", "{run}");
    assert_eq!(run["missing_ordinals"], serde_json::json!([]), "{run}");
    assert_eq!(run["retained"], "retained executions 1–1", "{run}");
}

/// Design §5 and ADR-0144: the sweep that ends a resumable run records its removal on the Task
/// under a housekeeping lease of its own (`storage_sweep`). That lease began no kernel run, so
/// the resume that follows is execution 2, not 3: ordinals 1 and 2 under lease epochs 1 and 3,
/// history `prefix:2`, nothing missing.
#[test]
fn telemetry_resume_after_a_sweep_removal_keeps_ordinals_contiguous() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    assert!(
        start(&machine, &repo, &state, "gc-older.json", &[])
            .status
            .success()
    );
    // The free-disk floor stops gc-newer before its Worker starts, so it stays resumable; the
    // eager sweep after it collects the finished gc-older and records that removal on gc-newer.
    let eager = [
        ("AF_TEST_FREE_BYTES", "1024"),
        ("AF_STORAGE__AUTO_GC", "true"),
        ("AF_STORAGE__KEEP_DAYS", "0"),
        ("AF_STORAGE__KEEP_TASKS", "0"),
    ];
    let first = start(&machine, &repo, &state, "gc-newer.json", &eager);
    assert_eq!(first.status.code(), Some(1), "{}", text(&first).1);
    let log = events(&state);
    let sweep = log
        .iter()
        .find(|row| row[10].contains("\"storage_sweep\""))
        .unwrap_or_else(|| panic!("the sweep recorded its removal: {}", text(&first).1));
    let sweep: Value = serde_json::from_str(&sweep[10]).unwrap();
    assert_eq!(sweep["epoch"], 2, "the sweep's housekeeping lease: {sweep}");
    assert!(
        sweep["writer"]
            .as_str()
            .unwrap()
            .starts_with("af-storage-sweep-"),
        "{sweep}"
    );
    let resumed = machine.af(
        &repo,
        &[],
        &[
            "task",
            "run",
            "gc-newer",
            "--execute",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&resumed);
    assert!(resumed.status.success(), "{stdout}\n{stderr}");
    let (pairs, run) = ordinals_and_epochs(&machine, &repo, "gc-newer");
    assert_eq!(pairs, [(1, 1), (2, 3)], "{run}");
    assert_eq!(run["history"], "prefix:2", "{run}");
    assert_eq!(run["missing_ordinals"], serde_json::json!([]), "{run}");
    assert_eq!(run["retained"], "retained executions 1–2", "{run}");
}

/// Design §6, §7.3: a consumer that does not stop — its write blocked, as on a stalled volume —
/// delays the command's exit by at most the 2 s join; the exit status is the run's, the segment
/// is left unsealed and unlocked, and the next reader recovers its flushed prefix.
#[test]
fn telemetry_stalled_consumer_leaves_recoverable_segment() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let timed = |file: &str, env: &[(&str, &str)]| {
        let started = std::time::Instant::now();
        let output = start(&machine, &repo, &state, file, env);
        (output, started.elapsed())
    };
    let (control, normal) = timed("gc-older.json", &[]);
    assert!(control.status.success(), "{}", text(&control).1);
    // Every write after the first blocks on the consumer thread; the appending thread's first
    // write (the header and `af.command`) is the flushed prefix.
    let (stalled, took) = timed(
        "gc-newer.json",
        &[("AF_TEST_TELEMETRY_STALL_AFTER_WRITES", "1")],
    );
    let (stdout, stderr) = text(&stalled);
    assert!(stalled.status.success(), "{stdout}\n{stderr}");
    let document: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(document["result"]["acceptance"], "satisfied", "{document}");
    assert!(
        stderr.contains("did not stop within 2 s"),
        "one telemetry problem: {stderr}"
    );
    assert!(took >= std::time::Duration::from_secs(2), "{took:?}");
    assert!(
        took < normal + std::time::Duration::from_secs(2 + 8),
        "the join plus a margin: {took:?} against {normal:?}"
    );
    let status = json(&machine.af(&repo, &[], &["telemetry", "status", "--json"]));
    assert_eq!(status["segments"]["unsealed"], 1, "{status}");
    assert_eq!(
        status["segments"]["live"], 0,
        "the lock ended with the process"
    );
    let reindexed = json(&machine.af(&repo, &[], &["telemetry", "reindex", "--json"]));
    let [recovered] = reindexed["recovered"].as_array().unwrap().as_slice() else {
        panic!("{reindexed}");
    };
    let id = recovered["segment_id"].as_str().unwrap();
    let records = review_telemetry::segment::reader::read_sealed(
        &review_telemetry::layout::Layout::new(machine.telemetry()).sealed_segment(id),
    )
    .unwrap();
    assert!(matches!(
        records.first(),
        Some(review_telemetry::record::Record::Segment(_))
    ));
    assert!(records.iter().any(|record| matches!(
        record,
        review_telemetry::record::Record::SpanStart(start) if start.name == "af.command"
    )));
    assert!(matches!(
        records.last(),
        Some(review_telemetry::record::Record::Seal(seal))
            if seal.exit == review_telemetry::record::SealExit::Recovered
    ));
}

/// Design §6: the seal is the consumer's, under the one 2 s bound. With the consumer's write
/// of the seal record held as on a stalled volume, the command still returns within the bound
/// plus a margin with the run's own outcome; no other thread seals, compresses, replaces or
/// writes the segment, which stays one unsealed, unlocked `.jsonl`; and the next reader recovers
/// its complete prefix — every record before the seal, the command's own end included.
#[test]
fn telemetry_blocked_terminal_write_leaves_the_segment_unsealed() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let timed = |file: &str, env: &[(&str, &str)]| {
        let started = std::time::Instant::now();
        let output = start(&machine, &repo, &state, file, env);
        (output, started.elapsed())
    };
    let (control, normal) = timed("gc-older.json", &[]);
    assert!(control.status.success(), "{}", text(&control).1);
    let (held, took) = timed("gc-newer.json", &[("AF_TEST_TELEMETRY_STALL_SEAL", "1")]);
    let (stdout, stderr) = text(&held);
    assert!(held.status.success(), "{stdout}\n{stderr}");
    let document: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(document["result"]["acceptance"], "satisfied", "{document}");
    assert!(
        stderr.contains("did not stop within 2 s"),
        "one telemetry problem: {stderr}"
    );
    assert!(took >= std::time::Duration::from_secs(2), "{took:?}");
    assert!(
        took < normal + std::time::Duration::from_secs(2 + 8),
        "the bound plus a margin: {took:?} against {normal:?}"
    );
    // Nothing sealed, compressed or replaced it: the held segment is a lone `.jsonl`.
    let segments = machine.telemetry().join("v1/segments");
    let names: Vec<String> = std::fs::read_dir(&segments)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let live: Vec<&String> = names
        .iter()
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    let [live] = live.as_slice() else {
        panic!("one unsealed segment: {names:?}");
    };
    let id = live.trim_end_matches(".jsonl").to_string();
    assert!(
        names
            .iter()
            .all(|name| name == *live || !name.starts_with(&id)),
        "no manifest or compressed copy of {id}: {names:?}"
    );
    let status = json(&machine.af(&repo, &[], &["telemetry", "status", "--json"]));
    assert_eq!(status["segments"]["unsealed"], 1, "{status}");
    assert_eq!(
        status["segments"]["live"], 0,
        "the lock ended with the process"
    );
    let reindexed = json(&machine.af(&repo, &[], &["telemetry", "reindex", "--json"]));
    let [recovered] = reindexed["recovered"].as_array().unwrap().as_slice() else {
        panic!("{reindexed}");
    };
    assert_eq!(recovered["segment_id"], id.as_str());
    let records = review_telemetry::segment::reader::read_sealed(
        &review_telemetry::layout::Layout::new(machine.telemetry()).sealed_segment(&id),
    )
    .unwrap();
    use review_telemetry::record::Record;
    let started: Vec<(String, String)> = records
        .iter()
        .filter_map(|record| match record {
            Record::SpanStart(start) => Some((start.span_id.clone(), start.name.clone())),
            _ => None,
        })
        .collect();
    let ended = |name: &str| {
        started.iter().any(|(span, named)| {
            named == name
                && records
                    .iter()
                    .any(|record| matches!(record, Record::SpanEnd(end) if &end.span_id == span))
        })
    };
    for name in ["af.command", "af.run", "af.attempt", "af.sweep"] {
        assert!(ended(name), "{name} ended in the recovered prefix");
    }
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("recovered with a seal");
    };
    assert_eq!(seal.exit, review_telemetry::record::SealExit::Recovered);
    assert_eq!(seal.discarded_bytes, 0, "every written line was whole");
    let seals = records
        .iter()
        .filter(|record| matches!(record, Record::Seal(_)))
        .count();
    assert_eq!(seals, 1, "the held seal never reached the file");
}

/// Design §6: the consumer thread is the segment's only writer from the moment `af.run` starts
/// it until the seal, and the whole shutdown has one bound. With the consumer's write of the
/// terminal records — the `af.command` end — held as on a stalled volume, the command still
/// returns within the 2 s bound plus a margin with the run's own outcome; nothing else writes,
/// seals or replaces the segment, which stays one unsealed, unlocked `.jsonl`; and the next
/// reader recovers its complete prefix: every record before the command's end.
#[test]
fn telemetry_command_end_is_inside_the_shutdown_bound() {
    use review_telemetry::record::Record;
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let timed = |file: &str, env: &[(&str, &str)]| {
        let started = std::time::Instant::now();
        let output = start(&machine, &repo, &state, file, env);
        (output, started.elapsed())
    };
    let (control, normal) = timed("gc-older.json", &[]);
    assert!(control.status.success(), "{}", text(&control).1);
    let (held, took) = timed(
        "gc-newer.json",
        &[("AF_TEST_TELEMETRY_STALL_COMMAND_END", "1")],
    );
    let (stdout, stderr) = text(&held);
    assert!(held.status.success(), "{stdout}\n{stderr}");
    let document: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(document["result"]["acceptance"], "satisfied", "{document}");
    assert!(
        stderr.contains("did not stop within 2 s"),
        "one telemetry problem: {stderr}"
    );
    assert!(took >= std::time::Duration::from_secs(2), "{took:?}");
    assert!(
        took < normal + std::time::Duration::from_secs(2 + 8),
        "the bound plus a margin: {took:?} against {normal:?}"
    );
    // Nothing sealed, compressed or replaced it: the held segment is a lone `.jsonl`.
    let segments = machine.telemetry().join("v1/segments");
    let names: Vec<String> = std::fs::read_dir(&segments)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let live: Vec<&String> = names
        .iter()
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    let [live] = live.as_slice() else {
        panic!("one unsealed segment: {names:?}");
    };
    let id = live.trim_end_matches(".jsonl").to_string();
    assert!(
        names
            .iter()
            .all(|name| name == *live || !name.starts_with(&id)),
        "no manifest or compressed copy of {id}: {names:?}"
    );
    let status = json(&machine.af(&repo, &[], &["telemetry", "status", "--json"]));
    assert_eq!(status["segments"]["unsealed"], 1, "{status}");
    assert_eq!(
        status["segments"]["live"], 0,
        "the lock ended with the process"
    );
    let reindexed = json(&machine.af(&repo, &[], &["telemetry", "reindex", "--json"]));
    let [recovered] = reindexed["recovered"].as_array().unwrap().as_slice() else {
        panic!("{reindexed}");
    };
    assert_eq!(recovered["segment_id"], id.as_str());
    let records = review_telemetry::segment::reader::read_sealed(
        &review_telemetry::layout::Layout::new(machine.telemetry()).sealed_segment(&id),
    )
    .unwrap();
    let started: Vec<(String, String)> = records
        .iter()
        .filter_map(|record| match record {
            Record::SpanStart(start) => Some((start.span_id.clone(), start.name.clone())),
            _ => None,
        })
        .collect();
    let ended = |name: &str| {
        started.iter().any(|(span, named)| {
            named == name
                && records.iter().any(|record| {
                    matches!(record, Record::SpanEnd(end)
                        if &end.span_id == span && end.end_unix_ns.is_some())
                })
        })
    };
    for name in ["af.run", "af.attempt", "af.sweep"] {
        assert!(ended(name), "{name} ended in the recovered prefix");
    }
    assert!(
        started.iter().any(|(_, name)| name == "af.command"),
        "the command started in the prefix"
    );
    assert!(
        !ended("af.command"),
        "the held command end never reached the file"
    );
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("recovered with a seal");
    };
    assert_eq!(seal.exit, review_telemetry::record::SealExit::Recovered);
    assert_eq!(seal.discarded_bytes, 0, "every written line was whole");
    let seals = records
        .iter()
        .filter(|record| matches!(record, Record::Seal(_)))
        .count();
    assert_eq!(seals, 1, "only the recovered seal");
}

/// ADR-0153 clause 7 (design §16 item 6): a telemetry-only failure — a read-only telemetry
/// directory, a quota on it, a locked index — is one problem, and the run succeeds.
#[test]
fn telemetry_write_failure_is_fail_open() {
    use std::os::unix::fs::PermissionsExt as _;
    let problems = |stderr: &str| {
        stderr
            .lines()
            .filter(|line| line.starts_with("af: telemetry problem"))
            .count()
    };
    // A read-only directory (which the superuser could write anyway).
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    if !nix::unistd::geteuid().is_root() {
        let directory = machine.telemetry();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
        let output = start(&machine, &repo, &state, "gc-older.json", &[]);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (stdout, stderr) = text(&output);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert_eq!(problems(&stderr), 1, "{stderr}");
        assert!(!directory.join("v1").exists());
    }

    // A quota: every write fails.
    let output = start(
        &machine,
        &repo,
        &state,
        "gc-newer.json",
        &[("AF_TEST_TELEMETRY_QUOTA", "1")],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_eq!(problems(&stderr), 1, "{stderr}");
    assert!(
        stderr.contains("quota") || stderr.contains("Disc quota"),
        "{stderr}"
    );

    // A locked index: the sweep cannot reclaim its pages, one `problem` record in the segment.
    let locked = Machine::new();
    let (repo, state) = task_gc::fixture(&locked.root);
    assert!(
        start(&locked, &repo, &state, "gc-older.json", &[])
            .status
            .success()
    );
    json(&locked.af(&repo, &[], &["telemetry", "status", "--json"]));
    let layout = review_telemetry::layout::Layout::new(locked.telemetry());
    assert!(layout.index().exists());
    let held = review_telemetry::layout::exclusive_within(
        &layout.index_lock(),
        std::time::Duration::from_secs(2),
    )
    .unwrap()
    .unwrap();
    let output = start(&locked, &repo, &state, "gc-newer.json", &[]);
    drop(held);
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_eq!(problems(&stderr), 1, "{stderr}");
    let status = json(&locked.af(&repo, &[], &["telemetry", "status", "--json"]));
    let recorded: Vec<&Value> = status["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|problem| problem["what"] == "budget")
        .collect();
    assert_eq!(recorded.len(), 1, "{status}");
    assert_eq!(recorded[0]["segments"], 1);
    assert!(
        recorded[0]["reason"].as_str().unwrap().contains("deferred"),
        "{status}"
    );
}

/// ADR-0144's free-disk floor with Telemetry at `content` (design §16 item 6): below
/// `min_free_bytes` a Worker Attempt, a check and a measurement are refused exactly as without
/// Telemetry.
#[test]
fn insufficient_disk_still_refuses_admission() {
    let content = [("AF_TELEMETRY__LEVEL", "content")];
    crate::storage_budget::attempt_refused_below_the_floor(&content);
    crate::storage_budget::check_refused_below_the_floor(&content);
    crate::task_experiment::measurement_refused_below_the_floor(&content);
}

/// The telemetry kind of `af storage --json`, by directory and segment.
fn telemetry_entries(document: &Value) -> Vec<Value> {
    document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "telemetry")
        .cloned()
        .collect()
}

/// Every file beneath the telemetry directory `root`, last modified `age` ago, and every sealed
/// segment's seal — its last use — ended then too.
fn age(root: &Path, age: std::time::Duration) {
    let when = std::time::SystemTime::now() - age;
    backdate_seals(root, when);
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                std::fs::File::open(&path)
                    .unwrap()
                    .set_modified(when)
                    .unwrap();
            }
        }
    }
}

/// Rewrite every sealed segment beneath the telemetry directory `root` so that its seal record
/// ended at `when`: a sealed segment's last use is its seal time, never its file's.
fn backdate_seals(root: &Path, when: std::time::SystemTime) {
    use review_telemetry::record::Record;
    let ended = when
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string();
    let Ok(entries) = std::fs::read_dir(root.join("v1/segments")) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.to_string_lossy().ends_with(".jsonl.gz") {
            continue;
        }
        // A file under a sealed name that is no segment (a fixture's replacement) has no seal.
        let Ok(mut records) = review_telemetry::segment::reader::read_sealed(&path) else {
            continue;
        };
        let Some(Record::Seal(seal)) = records.last_mut() else {
            panic!("{} ends with its seal", path.display());
        };
        seal.ended_unix_ns = ended.clone();
        let mut bytes = Vec::new();
        for record in &records {
            bytes.extend(serde_json::to_vec(record).unwrap());
            bytes.push(b'\n');
        }
        std::fs::write(&path, review_telemetry::blob::compress(&bytes)).unwrap();
    }
}

/// Design §6: every directory Telemetry has written is registered, and `af storage` inventories
/// and prunes each one whatever the current level or `dir` — written to A, switched to B, then
/// with Telemetry `off`. A registered directory that no longer exists is reported and dropped by
/// an applied prune.
#[test]
fn telemetry_dirs_registry_reaches_every_directory() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let a = machine.root.join("telemetry-a");
    let b = machine.root.join("telemetry-b");
    for (file, directory) in [("gc-older.json", &a), ("gc-newer.json", &b)] {
        let output = start(
            &machine,
            &repo,
            &state,
            file,
            &[("AF_TELEMETRY__DIR", directory.to_str().unwrap())],
        );
        assert!(output.status.success(), "{}", text(&output).1);
    }
    let registry: Value =
        serde_json::from_slice(&std::fs::read(machine.registry()).unwrap()).unwrap();
    let registered: Vec<&str> = registry["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        registered,
        [a.to_str().unwrap(), b.to_str().unwrap()],
        "{registry}"
    );
    let off = [("AF_TELEMETRY__LEVEL", "off")];
    let shown = json(&machine.af(&repo, &off, &["storage", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("storage-v2.json"), &shown);
    let entries = telemetry_entries(&shown);
    for directory in [&a, &b] {
        assert!(
            entries
                .iter()
                .any(|entry| entry["directory"] == directory.to_str().unwrap()
                    && entry["state"] == "sealed"),
            "{}: {entries:?}",
            directory.display()
        );
    }
    // Old enough to collect: an applied prune with Telemetry off takes them from both.
    age(&a, std::time::Duration::from_secs(40 * 86_400));
    age(&b, std::time::Duration::from_secs(40 * 86_400));
    let pruned = json(&machine.af(
        &repo,
        &[off[0], ("AF_STORAGE__KEEP_DAYS", "14")],
        &["storage", "prune", "--apply", "--json"],
    ));
    crate::schemas::valid(&crate::schemas::validator("storage-prune-v2.json"), &pruned);
    for directory in [&a, &b] {
        assert!(
            pruned["removals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|removal| {
                    removal["kind"] == "telemetry"
                        && removal["rule"] == "collection"
                        && removal["path"]
                            .as_str()
                            .unwrap()
                            .starts_with(directory.to_str().unwrap())
                }),
            "{}: {pruned}",
            directory.display()
        );
    }
    // A registered directory that no longer exists: reported, then dropped by a prune.
    std::fs::remove_dir_all(&a).unwrap();
    let shown = json(&machine.af(&repo, &off, &["storage", "--json"]));
    assert!(
        shown["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|problem| problem["path"] == a.to_str().unwrap()),
        "{shown}"
    );
    let pruned = json(&machine.af(&repo, &off, &["storage", "prune", "--apply", "--json"]));
    assert_eq!(
        pruned["telemetry"]["registry_dropped"],
        serde_json::json!([a.to_str().unwrap()])
    );
    let registry: Value =
        serde_json::from_slice(&std::fs::read(machine.registry()).unwrap()).unwrap();
    assert_eq!(registry["dirs"].as_array().unwrap().len(), 1, "{registry}");
}

/// Run the applied telemetry eviction `args` with every telemetry file aged past collection,
/// pausing each eviction between the inventory and its check; the file `<id><suffix>` of the
/// first segment it reaches (`.jsonl.gz`, or its `.refs` manifest) is replaced meanwhile by a
/// recent regular file of the same name. Returns the command's document, the segment's id and
/// the replaced file's path.
fn evict_with_a_replacement(
    machine: &Machine,
    repo: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    suffix: &str,
    replacement: &[u8],
) -> (Value, String, PathBuf) {
    age(
        &machine.telemetry(),
        std::time::Duration::from_secs(40 * 86_400),
    );
    let pauses = machine.root.join("pauses");
    let _ = std::fs::remove_dir_all(&pauses);
    std::fs::create_dir_all(&pauses).unwrap();
    let stdout = machine.root.join("evicting.json");
    let mut env = env.to_vec();
    env.push(("AF_TEST_TELEMETRY_EVICT_PAUSE", pauses.to_str().unwrap()));
    let mut child = machine
        .command(repo, &env, args)
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let segments = machine.telemetry().join("v1/segments");
    let mut replaced: Option<(String, PathBuf)> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "the eviction ends");
        for entry in std::fs::read_dir(&pauses).unwrap().filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".paused") else {
                continue;
            };
            let resume = pauses.join(format!("{id}.resume"));
            if resume.exists() {
                continue;
            }
            if replaced.is_none() {
                // A recent regular file takes the inventoried file's name.
                let path = segments.join(format!("{id}{suffix}"));
                let staged = machine.root.join("replacement");
                std::fs::write(&staged, replacement).unwrap();
                std::fs::rename(&staged, &path).unwrap();
                replaced = Some((id.to_string(), path));
            }
            std::fs::write(&resume, b"").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
    let document: Value = serde_json::from_slice(&std::fs::read(&stdout).unwrap()).unwrap();
    let (id, path) = replaced.unwrap_or_else(|| panic!("an eviction was reached: {document}"));
    (document, id, path)
}

/// ADR-0144: telemetry eviction is bound to what the inventory found. An inventoried old sealed
/// segment replaced by a recent regular file of the same name before `af storage prune --apply`
/// — and again before `af telemetry gc --apply` — reaches it is left in place, and the document
/// reports it as changed; nothing else in the sweep is held back.
#[test]
fn telemetry_eviction_refuses_a_replaced_segment() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    for file in ["gc-older.json", "gc-newer.json"] {
        let output = start(&machine, &repo, &state, file, &[]);
        assert!(output.status.success(), "{}", text(&output).1);
    }
    let off = ("AF_TELEMETRY__LEVEL", "off");
    let (pruned, id, path) = evict_with_a_replacement(
        &machine,
        &repo,
        &["storage", "prune", "--apply", "--json"],
        &[off, ("AF_STORAGE__KEEP_DAYS", "14")],
        ".jsonl.gz",
        b"not the segment the prune inventoried\n",
    );
    crate::schemas::valid(&crate::schemas::validator("storage-prune-v2.json"), &pruned);
    let [changed] = pruned["telemetry"]["changed"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("one changed entry: {pruned}");
    };
    assert_eq!(changed["segment_id"], id.as_str(), "{pruned}");
    assert_eq!(changed["path"], path.to_str().unwrap());
    assert!(
        changed["reason"].as_str().unwrap().contains("replaced"),
        "{changed}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"not the segment the prune inventoried\n",
        "the replacement stays"
    );
    let removals: Vec<&Value> = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["kind"] == "telemetry")
        .collect();
    let [other] = removals.as_slice() else {
        panic!("the other segment is collected: {pruned}");
    };
    assert_ne!(other["segment_id"], id.as_str());
    assert_eq!(other["rule"], "collection");

    // `af telemetry gc --apply`, the same replacement under it.
    let (collected, again, path) = evict_with_a_replacement(
        &machine,
        &repo,
        &["telemetry", "gc", "--apply", "--json"],
        &[("AF_STORAGE__KEEP_DAYS", "1")],
        ".jsonl.gz",
        b"not the segment gc inventoried\n",
    );
    crate::schemas::valid(
        &crate::schemas::validator("telemetry-gc-v1.json"),
        &collected,
    );
    assert_eq!(again, id, "the one segment left");
    let [changed] = collected["changed"].as_array().unwrap().as_slice() else {
        panic!("one changed entry: {collected}");
    };
    assert_eq!(changed["segment_id"], id.as_str(), "{collected}");
    assert!(
        changed["reason"].as_str().unwrap().contains("replaced"),
        "{changed}"
    );
    assert_eq!(collected["removals"], serde_json::json!([]), "{collected}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"not the segment gc inventoried\n",
        "the replacement stays"
    );
}

/// ADR-0144: an entry is evicted whole or not at all, and the check before either unlink covers
/// every file it owns. An inventoried old sealed segment whose `.refs` manifest is replaced by a
/// recent regular file — the compressed segment intact — before `af storage prune --apply`
/// reaches it loses nothing: neither file is removed, and the document reports the entry as
/// changed because of its manifest.
#[test]
fn telemetry_eviction_refuses_a_replaced_manifest() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    for file in ["gc-older.json", "gc-newer.json"] {
        let output = start(&machine, &repo, &state, file, &[]);
        assert!(output.status.success(), "{}", text(&output).1);
    }
    let segments = machine.telemetry().join("v1/segments");
    let (pruned, id, manifest) = evict_with_a_replacement(
        &machine,
        &repo,
        &["storage", "prune", "--apply", "--json"],
        &[
            ("AF_TELEMETRY__LEVEL", "off"),
            ("AF_STORAGE__KEEP_DAYS", "14"),
        ],
        ".refs",
        b"not the manifest the prune inventoried\n",
    );
    crate::schemas::valid(&crate::schemas::validator("storage-prune-v2.json"), &pruned);
    let [changed] = pruned["telemetry"]["changed"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("one changed entry: {pruned}");
    };
    assert_eq!(changed["segment_id"], id.as_str(), "{pruned}");
    assert!(
        changed["reason"]
            .as_str()
            .unwrap()
            .contains("manifest was replaced"),
        "{changed}"
    );
    // Nothing of the entry is removed: the compressed segment and the replacement both stay.
    let sealed = segments.join(format!("{id}.jsonl.gz"));
    review_telemetry::segment::reader::read_sealed(&sealed).unwrap();
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        b"not the manifest the prune inventoried\n"
    );
    assert!(
        pruned["removals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|removal| removal["segment_id"] != id.as_str()),
        "{pruned}"
    );
}

/// ADR-0144 and design §6: a sealed segment's last use is its validated seal record's end time,
/// never its file's modification time. A segment sealed just now whose files are set a day back
/// is still used within the last hour: `af storage` reports its seal time, and neither an
/// applied age prune (`af storage prune --apply`) nor `af telemetry gc --apply` removes it.
#[test]
fn telemetry_sealed_last_use_is_the_seal_time() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let output = start(&machine, &repo, &state, "gc-older.json", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let segments = machine.telemetry().join("v1/segments");
    let names: Vec<String> = std::fs::read_dir(&segments)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let sealed: Vec<&String> = names
        .iter()
        .filter(|name| name.ends_with(".jsonl.gz"))
        .collect();
    let [sealed] = sealed.as_slice() else {
        panic!("one sealed segment: {names:?}");
    };
    let sealed = (*sealed).clone();
    let id = sealed.trim_end_matches(".jsonl.gz").to_string();
    let records =
        review_telemetry::segment::reader::read_sealed(&segments.join(sealed.as_str())).unwrap();
    let Some(review_telemetry::record::Record::Seal(seal)) = records.last() else {
        panic!("sealed");
    };
    let sealed_ms = u64::try_from(seal.ended_unix_ns.parse::<u128>().unwrap() / 1_000_000).unwrap();
    let day_back = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    for name in [sealed.clone(), format!("{id}.refs")] {
        std::fs::File::open(segments.join(name))
            .unwrap()
            .set_modified(day_back)
            .unwrap();
    }
    let off = ("AF_TELEMETRY__LEVEL", "off");
    let shown = json(&machine.af(&repo, &[off], &["storage", "--json"]));
    let entry = telemetry_entries(&shown)
        .into_iter()
        .find(|entry| entry["segment_id"] == id.as_str())
        .unwrap_or_else(|| panic!("{shown}"));
    assert_eq!(entry["last_use_unix_ms"], sealed_ms, "{entry}");
    for args in [
        &["storage", "prune", "--apply", "--json"][..],
        &["telemetry", "gc", "--apply", "--json"][..],
    ] {
        let applied = json(&machine.af(&repo, &[off, ("AF_STORAGE__KEEP_DAYS", "0")], args));
        assert!(
            applied["removals"]
                .as_array()
                .unwrap()
                .iter()
                .all(|removal| removal["kind"] != "telemetry"),
            "{args:?}: {applied}"
        );
        assert!(segments.join(sealed.as_str()).exists(), "{args:?}");
        assert!(segments.join(format!("{id}.refs")).exists(), "{args:?}");
    }
}

// ------------------------------------------------------------------------------------------
// One recorded fixture run, and the four commands' goldens.

const GOLDEN_STORE: &str = "5555555555555555555555555555555555555555555555555555555555555555";
const GOLDEN_RUN: &str =
    "task:sha256:7777777777777777777777777777777777777777777777777777777777777777";

/// A writer whose clock is injected — each reading one millisecond after the last, from
/// `start_ms` — with a fixed segment id, argv and working directory: what it records is the
/// same on every machine.
fn golden_writer(
    root: &Path,
    segment: &str,
    start_ms: u64,
) -> review_telemetry::segment::writer::Writer {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let clock = Arc::new(AtomicU64::new(start_ms * 1_000_000));
    let mut options = review_telemetry::segment::writer::WriterOptions::new(
        root,
        review_telemetry::level::Level::Spans,
    );
    options.argv = vec!["af".into(), "task".into(), "run".into(), "golden".into()];
    options.cwd = PathBuf::from("/work");
    options.af_version = "0.0.0-golden".into();
    options.af_commit = "0123456789ab".into();
    options.redactor = review_telemetry::redact::Redactor::new();
    options.segment_id = Some(segment.into());
    options.clock = Some(Arc::new(move || {
        u128::from(clock.fetch_add(1_000_000, Ordering::SeqCst))
    }));
    review_telemetry::segment::writer::Writer::new(options).unwrap()
}

fn digest(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

/// One execution of the golden run, as the Task runtime records it: `af.command` >
/// `af.run` > `af.authority.load`, `af.plan` (with its node records), `af.node` > `af.attempt`
/// (one failed Attempt and its retry), then `af.sweep` under the command. `between` runs after
/// each node, while the segment is live.
fn golden_execution(
    root: &Path,
    segment: &str,
    start_ms: u64,
    ordinal: u64,
    between: &mut dyn FnMut(&review_telemetry::segment::writer::Writer),
) {
    execution_in_store(root, GOLDEN_STORE, segment, start_ms, ordinal, between);
}

/// [`golden_execution`] of the golden run id in the Store `store`.
fn execution_in_store(
    root: &Path,
    store: &str,
    segment: &str,
    start_ms: u64,
    ordinal: u64,
    between: &mut dyn FnMut(&review_telemetry::segment::writer::Writer),
) {
    use review_telemetry::record::{Attrs, Node, SealExit, SpanKind, SpanStatus};
    let writer = golden_writer(root, segment, start_ms);
    let execution = review_telemetry::trace::Execution {
        store_id: store.into(),
        run_id: GOLDEN_RUN.into(),
        execution_id: segment.into(),
        ordinal,
        store_sequence_at_start: 2 + 12 * (ordinal - 1),
    };
    let trace = execution.trace_id();
    let span = |name: &str, parent: &str, attrs: Attrs| {
        writer
            .span_start(Some(&trace), Some(parent), name, SpanKind::Internal, attrs)
            .unwrap()
    };
    let command = writer
        .span_start(
            None,
            None,
            "af.command",
            SpanKind::Internal,
            Attrs::new().text("process.command", "af"),
        )
        .unwrap();
    let run = span(
        "af.run",
        &command,
        execution
            .attributes()
            .enumerated("af.run.kind", "task")
            .text("af.task_id", "golden")
            .int("af.epoch", i64::try_from(ordinal).unwrap()),
    );
    let load = span("af.authority.load", &run, Attrs::new());
    writer.span_end(
        &load,
        SpanStatus::Ok,
        Attrs::new()
            .id("af.policy_rev", digest('1'))
            .text("af.pipeline", "fixture/plain")
            .id("af.pipeline.digest", digest('2')),
        2,
    );
    let plan = span(
        "af.plan",
        &run,
        Attrs::new()
            .id("af.plan_digest", digest('3'))
            .text("af.pipeline", "fixture/plain")
            .int("af.node_count", 3),
    );
    for (node, kind, package, depends_on) in [
        (
            "root.nodes.implement",
            "worker",
            Some("fixture/implementer"),
            vec![],
        ),
        (
            "root.nodes.check",
            "check",
            None,
            vec!["root.nodes.implement"],
        ),
        (
            "root.nodes.accept",
            "accept",
            None,
            vec!["root.nodes.check", "root.nodes.implement"],
        ),
    ] {
        writer.node(Node {
            seq: 0,
            schema: String::new(),
            execution_id: segment.into(),
            plan_digest: digest('3'),
            node_id: node.into(),
            kind: kind.into(),
            package: package.map(str::to_string),
            depends_on: depends_on.into_iter().map(str::to_string).collect(),
            expected_nodes: 3,
        });
    }
    writer.span_end(&plan, SpanStatus::Ok, Attrs::new(), 2);
    let usage = |charge: &str| {
        Attrs::new()
            .decimal("af.usage.input_tokens", "1000")
            .decimal("af.usage.output_tokens", "200")
            .decimal("af.usage.chargeable_tokens", charge)
            .text("af.usage.observation", "complete")
    };
    // The implementer: one Attempt.
    let implement = span(
        "af.node",
        &run,
        Attrs::new()
            .text("af.node_id", "root.nodes.implement")
            .text("af.node.kind", "worker"),
    );
    let attempt = span(
        "af.attempt",
        &implement,
        Attrs::new()
            .text("af.attempt_id", format!("attempt-{ordinal}-1"))
            .text("af.node_id", "root.nodes.implement")
            .text("af.worker", "fixture/implementer")
            .text("af.provider", "claude-main")
            .text("af.provider.kind", "claude"),
    );
    writer.event(
        &attempt,
        "af.note",
        Attrs::new().text("af.note", "the Worker replied"),
    );
    writer.span_end(
        &attempt,
        SpanStatus::Ok,
        usage("1200").text("af.outcome", "succeeded"),
        3,
    );
    writer.span_end(
        &implement,
        SpanStatus::Ok,
        Attrs::new().text("af.outcome", "succeeded"),
        2,
    );
    between(&writer);
    // The check: a failed Attempt, then its retry.
    let check = span(
        "af.node",
        &run,
        Attrs::new()
            .text("af.node_id", "root.nodes.check")
            .text("af.node.kind", "check"),
    );
    for (index, failed) in [(1, true), (2, false)] {
        let attempt = span(
            "af.attempt",
            &check,
            Attrs::new()
                .text("af.attempt_id", format!("attempt-{ordinal}-check-{index}"))
                .text("af.node_id", "root.nodes.check"),
        );
        let ended = Attrs::new()
            .decimal("af.usage.chargeable_tokens", "0")
            .text("af.usage.observation", "complete");
        if failed {
            writer.span_end(
                &attempt,
                SpanStatus::Error,
                ended
                    .text("af.outcome", "failed")
                    .text("af.failure.class", "failed")
                    .text("error.type", "failed")
                    .text("af.failure.reason", "the pagination check failed"),
                3,
            );
        } else {
            writer.span_end(
                &attempt,
                SpanStatus::Ok,
                ended.text("af.outcome", "succeeded"),
                3,
            );
        }
    }
    writer.span_end(
        &check,
        SpanStatus::Ok,
        Attrs::new().text("af.outcome", "succeeded"),
        2,
    );
    between(&writer);
    let accept = span(
        "af.node",
        &run,
        Attrs::new()
            .text("af.node_id", "root.nodes.accept")
            .text("af.node.kind", "accept"),
    );
    writer.span_end(
        &accept,
        SpanStatus::Ok,
        Attrs::new().text("af.outcome", "succeeded"),
        2,
    );
    writer.span_end(
        &run,
        SpanStatus::Ok,
        Attrs::new()
            .text("af.outcome", "finished")
            .decimal("af.usage.chargeable_tokens", "1200"),
        1,
    );
    let sweep = writer
        .span_start(
            None,
            Some(&command),
            "af.sweep",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.span_end(
        &sweep,
        SpanStatus::Ok,
        Attrs::new()
            .int("af.sweep.bytes_before", 4096)
            .int("af.sweep.bytes_after", 4096)
            .int("af.sweep.evicted", 0),
        1,
    );
    writer.span_end(
        &command,
        SpanStatus::Ok,
        Attrs::new().int("process.exit_code", 0),
        0,
    );
    between(&writer);
    writer.finish(SealExit::Ok).unwrap();
}

fn golden_directory() -> PathBuf {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    workspace.join("crates/af/tests/it/telemetry_golden")
}

/// Compare `actual` with the golden `name`; `AF_UPDATE_TELEMETRY_GOLDEN=1` rewrites it.
fn golden(name: &str, actual: &str) {
    let path = golden_directory().join(name);
    if std::env::var_os("AF_UPDATE_TELEMETRY_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(actual, expected, "{name} differs from its golden");
}

/// A document's absolute paths below the machine as `<root>`; with `machine_bytes`, every
/// allocated byte count and modification time as `<n>` (they are the file system's).
fn normalized(document: &Value, root: &Path, machine_bytes: bool) -> String {
    fn walk(value: &mut Value, root: &str, machine_bytes: bool) {
        match value {
            Value::String(text) => *text = text.replace(root, "<root>"),
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| walk(item, root, machine_bytes)),
            Value::Object(members) => {
                for (key, member) in members.iter_mut() {
                    let machine = matches!(
                        key.as_str(),
                        "bytes"
                            | "total_before"
                            | "total_after"
                            | "storage_total_before"
                            | "storage_total_after"
                            | "temporary_bytes"
                            | "blob_bytes"
                            | "index_bytes"
                            | "last_use_unix_ms"
                    );
                    if machine_bytes && machine && member.is_u64() {
                        *member = Value::String("<n>".into());
                    } else {
                        walk(member, root, machine_bytes);
                    }
                }
            }
            _ => {}
        }
    }
    let mut document = document.clone();
    walk(&mut document, &root.display().to_string(), machine_bytes);
    serde_json::to_string_pretty(&document).unwrap() + "\n"
}

/// Design §10: `runs`, `show`, `tail` and `gc` over one recorded fixture run of two executions,
/// each in text and as its versioned document, against goldens; every document matches its
/// schema and refuses a member its schema does not name.
#[test]
fn telemetry_commands_match_their_goldens() {
    let machine = Machine::new();
    let root = machine.telemetry();
    golden_execution(
        &root,
        "1700000000000-4242-0a1b2c3d",
        1_700_000_000_000,
        1,
        &mut |_| {},
    );

    // `tail` follows the second execution while it is written, and exits at its seal: it is
    // started once the segment is live and has printed its first line before the rest is
    // written.
    let (sender, received) = std::sync::mpsc::channel::<String>();
    let mut tail: Option<std::process::Child> = None;
    let mut tailed = String::new();
    golden_execution(
        &root,
        "1700000100000-4243-0b1c2d3e",
        1_700_000_100_000,
        2,
        &mut |writer| {
            writer.flush();
            if tail.is_some() {
                return;
            }
            let mut child = machine
                .command(&machine.root, &[], &["telemetry", "tail", "--json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let sender = sender.clone();
            std::thread::spawn(move || {
                use std::io::BufRead as _;
                for line in std::io::BufReader::new(stdout).lines() {
                    if sender.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            let first = received
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("tail prints the live segment's first span end");
            tailed.push_str(&first);
            tailed.push('\n');
            tail = Some(child);
        },
    );
    drop(sender);
    while let Ok(line) = received.recv_timeout(std::time::Duration::from_secs(60)) {
        tailed.push_str(&line);
        tailed.push('\n');
    }
    let tail = tail.unwrap().wait_with_output().unwrap();
    assert!(tail.status.success(), "{}", text(&tail).1);
    let tail_schema = crate::schemas::validator("telemetry-tail-v1.json");
    for line in tailed.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        crate::schemas::valid(&tail_schema, &value);
    }
    golden("tail.jsonl", &tailed);

    let at = &machine.root;
    let runs = json(&machine.af(at, &[], &["telemetry", "runs", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), &runs);
    golden("runs.json", &normalized(&runs, &machine.root, false));
    golden(
        "runs.txt",
        &text(&machine.af(at, &[], &["telemetry", "runs"])).0,
    );
    let show = json(&machine.af(at, &[], &["telemetry", "show", GOLDEN_RUN, "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), &show);
    golden("show.json", &normalized(&show, &machine.root, false));
    golden(
        "show.txt",
        &text(&machine.af(at, &[], &["telemetry", "show", GOLDEN_RUN])).0,
    );
    golden(
        "show-attempt.txt",
        &text(&machine.af(
            at,
            &[],
            &[
                "telemetry",
                "show",
                GOLDEN_RUN,
                "--attempt",
                "attempt-2-check-1",
            ],
        ))
        .0,
    );
    golden(
        "show-depth.txt",
        &text(&machine.af(at, &[], &["telemetry", "show", GOLDEN_RUN, "--depth", "2"])).0,
    );
    let attempt = show["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|span| span["attributes"]["af.attempt_id"] == "attempt-1-1")
        .unwrap()["span_id"]
        .as_str()
        .unwrap()
        .to_string();
    let one = json(&machine.af(
        at,
        &[],
        &[
            "telemetry",
            "show",
            GOLDEN_RUN,
            "--span",
            &attempt,
            "--json",
        ],
    ));
    crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), &one);
    golden("show-span.json", &normalized(&one, &machine.root, false));
    let gc = json(&machine.af(at, &[], &["telemetry", "gc", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-gc-v1.json"), &gc);
    golden("gc.json", &normalized(&gc, &machine.root, true));

    // Every document refuses a member its schema does not name.
    for (schema, document, pointer) in [
        ("telemetry-runs-v1.json", &runs, "/runs/0"),
        ("telemetry-show-v1.json", &show, "/spans/0"),
        ("telemetry-show-v1.json", &show, "/run"),
        ("telemetry-gc-v1.json", &gc, ""),
    ] {
        let mut extended = document.clone();
        extended
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unnamed".into(), Value::from(1));
        assert!(
            !crate::schemas::validator(schema).is_valid(&extended),
            "{schema} {pointer}"
        );
    }
    // An unknown run exits 1 naming it; a run two Stores share asks for `--store`.
    let unknown = machine.af(at, &[], &["telemetry", "show", "task:none"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown).1.contains("unknown run task:none"));
}

/// Design §5: totals never claim a scope they do not have. Execution 1 sealed, execution 2
/// live: `runs` and `show` list the live one beside the sealed prefix (`executions.open`), the
/// label names it (`retained executions 1–1, open: 2`), and `runs` says its totals cover both
/// executions with the live one marked `open` — the charge includes its Attempt. A `reindex`
/// changes none of it.
#[test]
fn telemetry_totals_name_the_open_execution() {
    let machine = Machine::new();
    let root = machine.telemetry();
    let first = "1700000000000-4242-0a1b2c3d";
    let second = "1700000100000-4243-0b1c2d3e";
    golden_execution(&root, first, 1_700_000_000_000, 1, &mut |_| {});
    let mut looked = false;
    golden_execution(&root, second, 1_700_000_100_000, 2, &mut |writer| {
        if looked {
            return;
        }
        looked = true;
        writer.flush();
        let read = || {
            let runs = json(&machine.af(&machine.root, &[], &["telemetry", "runs", "--json"]));
            let show = json(&machine.af(
                &machine.root,
                &[],
                &["telemetry", "show", GOLDEN_RUN, "--json"],
            ));
            let runs_text = text(&machine.af(&machine.root, &[], &["telemetry", "runs"])).0;
            let show_text =
                text(&machine.af(&machine.root, &[], &["telemetry", "show", GOLDEN_RUN])).0;
            (runs, show, runs_text, show_text)
        };
        let before = read();
        let (runs, show, runs_text, show_text) = &before;
        crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), runs);
        crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), show);
        let run = &runs["runs"][0];
        let open = serde_json::json!([{"execution_id": second, "ordinal": 2, "segment": "live"}]);
        assert_eq!(run["history"], "prefix:1", "{run}");
        assert_eq!(run["retained"], "retained executions 1–1, open: 2", "{run}");
        assert_eq!(run["executions"]["retained"], 2, "{run}");
        assert_eq!(run["executions"]["sealed"], serde_json::json!([1]), "{run}");
        assert_eq!(run["executions"]["open"], open, "{run}");
        assert_eq!(run["open"], true);
        assert_eq!(
            run["totals_cover"],
            serde_json::json!([
                {"execution_id": first, "ordinal": 1, "open": false},
                {"execution_id": second, "ordinal": 2, "open": true},
            ]),
            "{run}"
        );
        // The totals include the open execution's ended Attempt.
        assert_eq!(run["charge"], "2400", "{run}");
        assert_eq!(show["run"]["retained"], run["retained"], "{show}");
        assert_eq!(show["run"]["executions"]["open"], open, "{show}");
        assert!(
            runs_text.contains("retained executions 1–1, open: 2"),
            "{runs_text}"
        );
        assert!(
            runs_text.contains(&format!("open: execution 2 ({second}, segment live)")),
            "{runs_text}"
        );
        assert!(
            show_text.contains("retained executions 1–1, open: 2"),
            "{show_text}"
        );
        assert!(
            show_text
                .lines()
                .any(|line| line.contains(second) && line.contains("(open)")),
            "{show_text}"
        );
        let reindexed = machine.af(&machine.root, &[], &["telemetry", "reindex", "--json"]);
        assert!(reindexed.status.success(), "{}", text(&reindexed).1);
        assert_eq!(read(), before, "identical after a reindex");
    });
    assert!(looked);
}

/// Design §5: a run's outcome is its latest execution's own and nothing else. Execution 1
/// sealed `finished`, execution 2 live with its `af.run` not yet ended: `runs` and `show` print
/// no outcome for the run — never the earlier execution's — and a `reindex` changes none of it.
#[test]
fn telemetry_live_execution_has_no_inherited_outcome() {
    let machine = Machine::new();
    let root = machine.telemetry();
    let first = "1700000000000-4242-0a1b2c3d";
    let second = "1700000100000-4243-0b1c2d3e";
    golden_execution(&root, first, 1_700_000_000_000, 1, &mut |_| {});
    let mut looked = false;
    golden_execution(&root, second, 1_700_000_100_000, 2, &mut |writer| {
        if looked {
            return;
        }
        looked = true;
        writer.flush();
        let read = || {
            let runs = json(&machine.af(&machine.root, &[], &["telemetry", "runs", "--json"]));
            let show = json(&machine.af(
                &machine.root,
                &[],
                &["telemetry", "show", GOLDEN_RUN, "--json"],
            ));
            let runs_text = text(&machine.af(&machine.root, &[], &["telemetry", "runs"])).0;
            (runs, show, runs_text)
        };
        let before = read();
        let (runs, show, runs_text) = &before;
        crate::schemas::valid(&crate::schemas::validator("telemetry-runs-v1.json"), runs);
        crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), show);
        let run = &runs["runs"][0];
        let list = run["executions"]["list"].as_array().unwrap();
        assert_eq!(list[0]["outcome"], "finished", "{run}");
        assert!(list[1].get("outcome").is_none(), "{run}");
        assert!(run.get("outcome").is_none(), "no inherited outcome: {run}");
        assert!(show["run"].get("outcome").is_none(), "{show}");
        assert!(runs_text.contains("outcome -"), "{runs_text}");
        assert!(!runs_text.contains("outcome finished"), "{runs_text}");
        let reindexed = machine.af(&machine.root, &[], &["telemetry", "reindex", "--json"]);
        assert!(reindexed.status.success(), "{}", text(&reindexed).1);
        assert_eq!(read(), before, "identical after a reindex");
    });
    assert!(looked);
}

/// `af telemetry gc` lists exactly what `af storage` lists for the kind `telemetry`, and with
/// `--apply` removes nothing of another kind.
#[test]
fn telemetry_gc_lists_what_storage_lists() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    for file in ["gc-older.json", "gc-newer.json"] {
        assert!(start(&machine, &repo, &state, file, &[]).status.success());
    }
    let storage = json(&machine.af(&repo, &[], &["storage", "--json"]));
    let gc = json(&machine.af(&repo, &[], &["telemetry", "gc", "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-gc-v1.json"), &gc);
    assert_eq!(
        gc["entries"].as_array().unwrap(),
        &telemetry_entries(&storage)
    );
    assert_eq!(gc["applied"], false);
    let tasks_before = json(&machine.af(
        &repo,
        &[],
        &["task", "list", "--state", state.to_str().unwrap(), "--json"],
    ));
    // Everything old enough, collection by age on: only telemetry goes.
    age(
        &machine.telemetry(),
        std::time::Duration::from_secs(40 * 86_400),
    );
    let applied = json(&machine.af(
        &repo,
        &[("AF_STORAGE__KEEP_DAYS", "1")],
        &["telemetry", "gc", "--apply", "--json"],
    ));
    crate::schemas::valid(&crate::schemas::validator("telemetry-gc-v1.json"), &applied);
    let removals = applied["removals"].as_array().unwrap();
    assert_eq!(removals.len(), 2, "{applied}");
    assert!(
        removals
            .iter()
            .all(|removal| removal["kind"] == "telemetry" && removal["rule"] == "collection")
    );
    let tasks_after = json(&machine.af(
        &repo,
        &[],
        &["task", "list", "--state", state.to_str().unwrap(), "--json"],
    ));
    assert_eq!(tasks_before, tasks_after, "nothing of another kind");
    let (stdout, _) = text(&machine.af(&repo, &[], &["telemetry", "gc"]));
    assert!(stdout.contains("[telemetry] max_bytes"), "{stdout}");
}

/// On the Task fixture at `spans` (design §16): `af telemetry runs` lists the run with its
/// execution, and `show` prints the tree `af.command > af.run > af.authority.load, af.plan,
/// af.node > af.attempt` and the command's `af.sweep`, every span with total and self time and
/// every span of the run naming its Store, run, kind and execution; `af.plan`'s node records
/// cover the plan.
#[test]
fn telemetry_task_run_records_its_span_tree() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let output = start(&machine, &repo, &state, "gc-older.json", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let runs = json(&machine.af(&repo, &[], &["telemetry", "runs", "--json"]));
    let [run] = runs["runs"].as_array().unwrap().as_slice() else {
        panic!("{runs}");
    };
    assert_eq!(run["kind"], "task");
    assert_eq!(run["task_id"], "gc-older");
    assert_eq!(run["executions"]["retained"], 1);
    assert_eq!(run["outcome"], "finished");
    assert_eq!(run["history"], "prefix:1");
    assert_eq!(run["coverage"], "complete");
    let run_id = run["run_id"].as_str().unwrap();
    assert!(run_id.starts_with("task:sha256:"), "{run_id}");
    let show = json(&machine.af(&repo, &[], &["telemetry", "show", run_id, "--json"]));
    crate::schemas::valid(&crate::schemas::validator("telemetry-show-v1.json"), &show);
    let spans = show["spans"].as_array().unwrap();
    let tree: Vec<(u64, &str)> = spans
        .iter()
        .map(|span| {
            (
                span["depth"].as_u64().unwrap(),
                span["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(tree[0], (0, "af.command"), "{tree:?}");
    assert_eq!(tree[1], (1, "af.run"), "{tree:?}");
    assert_eq!(tree[2], (2, "af.authority.load"), "{tree:?}");
    assert_eq!(tree[3], (2, "af.plan"), "{tree:?}");
    assert_eq!(*tree.last().unwrap(), (1, "af.sweep"), "{tree:?}");
    let nodes = tree
        .iter()
        .filter(|(depth, name)| *depth == 2 && *name == "af.node")
        .count();
    let attempts = tree
        .iter()
        .filter(|(depth, name)| *depth == 3 && *name == "af.attempt")
        .count();
    assert!(nodes >= 5 && attempts >= 2, "{tree:?}");
    for span in spans {
        assert_eq!(span["status"], "ok", "{span}");
        assert!(
            span["total_us"].is_u64() && span["self_us"].is_u64(),
            "{span}"
        );
        assert!(
            span["self_us"].as_u64() <= span["total_us"].as_u64(),
            "{span}"
        );
    }
    let worker = spans
        .iter()
        .find(|span| span["attributes"]["af.worker"] == "fixture/implementer")
        .unwrap_or_else(|| panic!("an implementer Attempt: {show}"));
    assert_eq!(worker["name"], "af.attempt");
    assert_eq!(worker["attributes"]["af.usage.observation"], "complete");
    assert!(worker["attributes"]["af.usage.chargeable_tokens"].is_string());
    let run_span = &spans[1]["attributes"];
    assert!(
        run_span["af.policy_rev"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(run_span["af.pipeline"], "fixture/plain");
    // Every span of the run names its Store, run, kind and execution: read through `--span`.
    let execution = run["executions"]["list"][0]["execution_id"]
        .as_str()
        .unwrap();
    for span in spans
        .iter()
        .skip(1)
        .filter(|span| span["name"] != "af.sweep")
    {
        let one = json(&machine.af(
            &repo,
            &[],
            &[
                "telemetry",
                "show",
                run_id,
                "--span",
                span["span_id"].as_str().unwrap(),
                "--json",
            ],
        ));
        let attributes = &one["spans"][0]["attributes"];
        assert_eq!(attributes["af.run_id"], run_id, "{one}");
        assert_eq!(attributes["af.store_id"], run["store_id"], "{one}");
        assert_eq!(attributes["af.run.kind"], "task", "{one}");
        assert_eq!(attributes["af.execution_id"], execution, "{one}");
    }
    // The command names the run it worked on, once known.
    assert_eq!(spans[0]["attributes"]["process.exit_code"], 0);
    // The plan's node records: one per compiled node, each with the expected count.
    let mut index = review_telemetry::index::Index::open(&review_telemetry::layout::Layout::new(
        machine.telemetry(),
    ))
    .unwrap();
    index.ingest().unwrap();
    let (records, expected): (i64, i64) = index
        .connection()
        .query_row(
            "SELECT count(*), max(expected_nodes) FROM nodes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(records, expected);
    assert_eq!(records, 6, "root.inputs and the five plan nodes");
}

/// Design §6 and §16: `af telemetry tail`, run against a live Task run, prints each span end
/// within `flush_latency_ms` of its creation, and exits when the writer seals.
#[test]
fn telemetry_tail_follows_a_live_run_within_the_flush_latency() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let latency_ms: u128 = 1_000;
    let mut tail = machine
        .command(&repo, &[], &["telemetry", "tail", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = tail.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        let mut lines = Vec::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let arrived = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            lines.push((arrived, line.unwrap()));
        }
        lines
    });
    // Let tail start and look before the run opens its segment.
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    let output = start(&machine, &repo, &state, "gc-older.json", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let started = std::time::Instant::now();
    while tail.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(60) {
            let _ = tail.kill();
            panic!("tail did not exit at the seal");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(tail.wait().unwrap().success());
    let lines = reader.join().unwrap();
    let ends: Vec<(u128, Value)> = lines
        .into_iter()
        .map(|(arrived, line)| (arrived, serde_json::from_str::<Value>(&line).unwrap()))
        .filter(|(_, line)| line["record"] == "span_end")
        .collect();
    assert!(
        ends.iter().any(|(_, line)| line["span"] == "af.attempt"),
        "{ends:?}"
    );
    // The records made while the run was open (everything before `af.run` ends).
    let run_end = ends
        .iter()
        .position(|(_, line)| line["span"] == "af.run")
        .unwrap_or_else(|| panic!("{ends:?}"));
    for (arrived, line) in &ends[..=run_end] {
        let created: u128 = line["time_unix_ns"].as_str().unwrap().parse().unwrap();
        let waited = arrived.saturating_sub(created) / 1_000_000;
        assert!(
            waited < latency_ms,
            "{} visible {waited} ms after its creation",
            line["span"]
        );
    }
}

/// Send `signal` (`STOP`, `CONT`) to the process `pid`; a stop is waited for until the process
/// is stopped.
fn signal(pid: u32, signal: &str) {
    let pid = pid.to_string();
    assert!(
        Command::new("kill")
            .args([&format!("-{signal}"), &pid])
            .status()
            .unwrap()
            .success()
    );
    if signal == "STOP" {
        let started = std::time::Instant::now();
        loop {
            let state = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&state.stdout)
                .trim()
                .starts_with('T')
            {
                break;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// Design §6 and §10: a reader refuses what it cannot read. `af telemetry tail` follows a live
/// segment; its writer seals it and the sealed gzip is corrupted before tail's next read (tail
/// held stopped in between). Tail never takes the segment as complete with nothing more to
/// say: it fails, exits non-zero and names the sealed file.
#[test]
fn telemetry_tail_refuses_an_unreadable_sealed_segment() {
    use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
    let machine = Machine::new();
    let writer = review_telemetry::segment::writer::Writer::new(
        review_telemetry::segment::writer::WriterOptions::new(
            machine.telemetry(),
            review_telemetry::level::Level::Spans,
        ),
    )
    .unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&command, "af.note", Attrs::new());
    writer.flush();
    let segment = writer.segment_id().unwrap();
    let mut tail = machine
        .command(&machine.root, &[], &["telemetry", "tail", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = tail.stdout.take().unwrap();
    let (sender, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout).lines() {
            let _ = sender.send(line.unwrap());
        }
    });
    // Tail follows the live segment: the event reached it.
    let first: Value = serde_json::from_str(
        &received
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("tail prints the live event"),
    )
    .unwrap();
    assert_eq!(first["name"], "af.note", "{first}");
    signal(tail.id(), "STOP");
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    assert_eq!(sealed.segment_id, segment);
    assert!(
        !writer.path().unwrap().exists(),
        "sealed: no live file left"
    );
    std::fs::write(&sealed.path, b"\x1f\x8b not a gzip stream any more").unwrap();
    signal(tail.id(), "CONT");
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = tail.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > std::time::Duration::from_secs(60) {
            let _ = tail.kill();
            panic!("tail did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut tail.stderr.take().unwrap(), &mut stderr).unwrap();
    reader.join().unwrap();
    assert!(!status.success(), "tail fails: {stderr}");
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&sealed.path.display().to_string()),
        "names the sealed file: {stderr}"
    );
    // Nothing after the seal was taken from the unreadable file.
    assert!(
        received
            .try_iter()
            .all(|line| !line.contains("\"span_end\"")),
        "no line from the unreadable file"
    );
}

/// Design §10: a run id two Stores share is two runs; `show` and `tail` list both Stores and ask
/// for `--store ID`, and with it read the one named.
#[test]
fn telemetry_a_run_two_stores_share_asks_for_the_store() {
    let machine = Machine::new();
    let root = machine.telemetry();
    let other = "6".repeat(64);
    golden_execution(
        &root,
        "1700000000000-4242-0a1b2c3d",
        1_700_000_000_000,
        1,
        &mut |_| {},
    );
    execution_in_store(
        &root,
        &other,
        "1700000200000-4244-0c1d2e3f",
        1_700_000_200_000,
        1,
        &mut |_| {},
    );
    let at = &machine.root;
    let runs = json(&machine.af(at, &[], &["telemetry", "runs", "--json"]));
    let stores: std::collections::BTreeSet<&str> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["store_id"].as_str().unwrap())
        .collect();
    assert_eq!(stores, [GOLDEN_STORE, other.as_str()].into_iter().collect());
    for command in ["show", "tail"] {
        let refused = machine.af(at, &[], &["telemetry", command, GOLDEN_RUN]);
        assert_eq!(refused.status.code(), Some(2), "{command}");
        let (_, stderr) = text(&refused);
        assert!(stderr.contains("--store"), "{stderr}");
        assert!(
            stderr.contains(GOLDEN_STORE) && stderr.contains(&other),
            "{stderr}"
        );
    }
    let one = json(&machine.af(
        at,
        &[],
        &["telemetry", "show", GOLDEN_RUN, "--store", &other, "--json"],
    ));
    assert_eq!(one["run"]["store_id"], other.as_str());
    assert_eq!(
        one["run"]["executions"]["list"].as_array().unwrap().len(),
        1
    );
}

/// A spawned tail: the child, its stdout lines, and the thread reading them.
type Following = (
    std::process::Child,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
);

/// `af telemetry tail GOLDEN_RUN --json`, its stdout lines on a channel.
fn spawn_tail(machine: &Machine) -> Following {
    let mut tail = machine
        .command(
            &machine.root,
            &[],
            &["telemetry", "tail", GOLDEN_RUN, "--json"],
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = tail.stdout.take().unwrap();
    let (sender, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout).lines() {
            let _ = sender.send(line.unwrap());
        }
    });
    (tail, received, reader)
}

/// Wait for a spawned tail to end on its own (at most a minute), then its status, its stderr
/// and every line it printed, `first` first.
fn ended_tail(
    (mut tail, received, reader): Following,
    first: String,
) -> (std::process::ExitStatus, String, Vec<Value>) {
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = tail.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > std::time::Duration::from_secs(60) {
            let _ = tail.kill();
            panic!("tail did not refuse the second Store");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut tail.stderr.take().unwrap(), &mut stderr).unwrap();
    reader.join().unwrap();
    let lines = std::iter::once(first)
        .chain(received.try_iter())
        .map(|line| serde_json::from_str::<Value>(&line).unwrap())
        .collect();
    (status, stderr, lines)
}

/// Design §10: `tail RUN` binds the Store it follows for its whole lifetime. Started while Store
/// A alone records RUN (live), it follows A; when Store B starts the same run id before A seals,
/// tail emits A's records only and then refuses the ambiguity as a run two Stores share at the
/// start is refused: exit 2, both Stores named, `--store` asked for. B appears in two ways, each
/// between two of tail's scans (tail held stopped):
/// - live, its segment id sorting before A's (as after a wall-clock rollback), while A has
///   flushed a span end tail has not read yet: tail first prints A's pending span end, then
///   refuses;
/// - created, flushed and sealed while A stays live: tail inspects the sealed segment and
///   refuses before any of B's records is emitted, never waiting for A to exit 0.
#[test]
fn telemetry_tail_refuses_a_second_store_that_appears_while_following() {
    let other = "6".repeat(64);
    let a_segment = "1700000000000-4242-0a1b2c3d";
    let refused = |status: &std::process::ExitStatus, stderr: &str, lines: &[Value]| {
        assert_eq!(status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("--store"), "{stderr}");
        assert!(
            stderr.contains(GOLDEN_STORE) && stderr.contains(&other),
            "names both Stores: {stderr}"
        );
        assert!(!lines.is_empty());
        assert!(
            lines.iter().all(|line| line["segment_id"] == a_segment),
            "A's records only: {lines:?}"
        );
    };

    // B live and sorting before A, while A's flushed failed check Attempt is still unread.
    let machine = Machine::new();
    let root = machine.telemetry();
    let mut following: Option<(Following, String)> = None;
    let mut tailed: Option<(std::process::ExitStatus, String, Vec<Value>)> = None;
    golden_execution(&root, a_segment, 1_700_000_000_000, 1, &mut |a| {
        if tailed.is_some() {
            return;
        }
        a.flush();
        let Some(pending) = following.take() else {
            // Tail follows A: its first record reached it. Held stopped, it looks no more
            // until A's check node and B are both there.
            let tail = spawn_tail(&machine);
            let first = tail
                .1
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("tail prints A's record");
            signal(tail.0.id(), "STOP");
            following = Some((tail, first));
            return;
        };
        // B starts the same run id while A is still live, and stays live until tail has ended.
        let mut pending = Some(pending);
        let mut b_tailed = None;
        execution_in_store(
            &root,
            &other,
            "1699999900000-4244-0c1d2e3f",
            1_700_000_200_000,
            1,
            &mut |b| {
                let Some((tail, first)) = pending.take() else {
                    return;
                };
                b.flush();
                signal(tail.0.id(), "CONT");
                b_tailed = Some(ended_tail(tail, first));
            },
        );
        tailed = b_tailed;
    });
    let (status, stderr, lines) = tailed.unwrap();
    refused(&status, &stderr, &lines);
    assert!(
        lines.iter().any(|line| line["record"] == "span_end"
            && line["span"] == "af.attempt"
            && line["attributes"]["af.outcome"] == "failed"),
        "A's flushed span end is printed before the refusal: {lines:?}"
    );

    // B created, flushed and sealed between two scans while A stays live.
    let machine = Machine::new();
    let root = machine.telemetry();
    let mut tailed: Option<(std::process::ExitStatus, String, Vec<Value>)> = None;
    golden_execution(&root, a_segment, 1_700_000_000_000, 1, &mut |a| {
        if tailed.is_some() {
            return;
        }
        a.flush();
        let tail = spawn_tail(&machine);
        let first = tail
            .1
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("tail prints A's record");
        signal(tail.0.id(), "STOP");
        let b_segment = "1700000200000-4244-0c1d2e3f";
        execution_in_store(&root, &other, b_segment, 1_700_000_200_000, 1, &mut |_| {});
        let files = listing(&root);
        assert!(
            files
                .iter()
                .any(|name| name.ends_with(&format!("{b_segment}.jsonl.gz"))),
            "B is sealed before tail looks again: {files:?}"
        );
        signal(tail.0.id(), "CONT");
        tailed = Some(ended_tail(tail, first));
    });
    let (status, stderr, lines) = tailed.unwrap();
    refused(&status, &stderr, &lines);
}

/// Wait for `child` to exit on its own within `within`, else kill it and fail with `what`; its
/// status and stderr.
fn exited(
    child: &mut std::process::Child,
    within: std::time::Duration,
    what: &str,
) -> (std::process::ExitStatus, String) {
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > within {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    (status, stderr)
}

/// `af telemetry tail --json` spawned on `machine`, its stdout lines on a channel.
fn spawn_plain_tail(machine: &Machine, args: &[&str]) -> Following {
    let mut tail = machine
        .command(&machine.root, &[], args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = tail.stdout.take().unwrap();
    let (sender, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout).lines() {
            let _ = sender.send(line.unwrap());
        }
    });
    (tail, received, reader)
}

/// Design §6: tail is the reader able to lock an unlocked unsealed segment, so it recovers it
/// before following it, never waiting on a prefix that cannot change. A writer flushes an
/// ended span and an event and is killed before its seal (its lock ends with it), once while
/// tail already follows its segment and once before tail starts: tail prints the complete
/// flushed prefix and exits 0, the segment recovered — sealed with `exit = "recovered"`.
#[test]
fn telemetry_tail_recovers_a_crashed_writer() {
    use review_telemetry::record::{Attrs, Record, SealExit, SpanKind, SpanStatus};
    for started_first in [true, false] {
        let machine = Machine::new();
        let writer = review_telemetry::segment::writer::Writer::new(
            review_telemetry::segment::writer::WriterOptions::new(
                machine.telemetry(),
                review_telemetry::level::Level::Spans,
            ),
        )
        .unwrap();
        let command = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        let load = writer
            .span_start(
                None,
                Some(&command),
                "af.authority.load",
                SpanKind::Internal,
                Attrs::new(),
            )
            .unwrap();
        writer.event(&load, "af.note", Attrs::new());
        writer.span_end(&load, SpanStatus::Ok, Attrs::new(), 1);
        writer.flush();
        let segment = writer.segment_id().unwrap();
        let layout = review_telemetry::layout::Layout::new(machine.telemetry());
        let mut lines = Vec::new();
        let (mut tail, received, reader) = if started_first {
            let following = spawn_plain_tail(&machine, &["telemetry", "tail", "--json"]);
            lines.push(
                following
                    .1
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("tail follows the live segment"),
            );
            // Killed after its flush and before its seal.
            writer.abandon();
            following
        } else {
            writer.abandon();
            spawn_plain_tail(&machine, &["telemetry", "tail", "--json"])
        };
        let (status, stderr) = exited(
            &mut tail,
            std::time::Duration::from_secs(60),
            "tail waited on a crashed writer's segment",
        );
        reader.join().unwrap();
        assert!(status.success(), "{started_first}: {stderr}");
        lines.extend(received.try_iter());
        let lines: Vec<Value> = lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (record, name) in [("event", "af.note"), ("span_end", "af.authority.load")] {
            assert!(
                lines.iter().any(|line| line["record"] == record
                    && (line["name"] == name || line["span"] == name)),
                "{started_first}: {name} printed: {lines:?}"
            );
        }
        assert!(
            lines
                .iter()
                .all(|line| line["segment_id"] == segment.as_str()),
            "{lines:?}"
        );
        assert!(!layout.live_segment(&segment).exists(), "{started_first}");
        let records =
            review_telemetry::segment::reader::read_sealed(&layout.sealed_segment(&segment))
                .unwrap();
        let Some(Record::Seal(seal)) = records.last() else {
            panic!("recovered with a seal");
        };
        assert_eq!(seal.exit, SealExit::Recovered, "{started_first}");
    }
}

/// Design §6 and §10: interior corruption in a live segment fails closed in tail. Tail follows
/// a live segment and is held stopped while its writer flushes one more event, an invalid line
/// is written, and the writer flushes another event after it. Tail refuses the segment naming
/// its file, exits non-zero, and prints nothing of that read — not even the valid event before
/// the invalid line.
#[test]
fn telemetry_tail_refuses_interior_corruption_of_a_live_segment() {
    use review_telemetry::record::{Attrs, SpanKind};
    let machine = Machine::new();
    let writer = review_telemetry::segment::writer::Writer::new(
        review_telemetry::segment::writer::WriterOptions::new(
            machine.telemetry(),
            review_telemetry::level::Level::Spans,
        ),
    )
    .unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&command, "af.first", Attrs::new());
    writer.flush();
    let path = writer.path().unwrap();
    let (mut tail, received, reader) = spawn_plain_tail(&machine, &["telemetry", "tail", "--json"]);
    let first: Value = serde_json::from_str(
        &received
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("tail prints the live event"),
    )
    .unwrap();
    assert_eq!(first["name"], "af.first", "{first}");
    signal(tail.id(), "STOP");
    writer.event(&command, "af.second", Attrs::new());
    writer.flush();
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"t\":\"event\",\"not\":\"a record\"}\n")
            .unwrap();
    }
    writer.event(&command, "af.third", Attrs::new());
    writer.flush();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        review_telemetry::segment::reader::scan(&bytes).interior_corruption,
        "an invalid line before a valid one"
    );
    signal(tail.id(), "CONT");
    let (status, stderr) = exited(
        &mut tail,
        std::time::Duration::from_secs(60),
        "tail went on following a corrupt segment",
    );
    reader.join().unwrap();
    assert_eq!(status.code(), Some(1), "{stderr}");
    // With `--json` the refusal is the `af/error@1` document on stdout.
    let rest: Vec<Value> = received
        .try_iter()
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect();
    let [refusal] = rest.as_slice() else {
        panic!("only the refusal, nothing of the corrupt read: {rest:?}");
    };
    assert_eq!(refusal["schema"], "af/error@1", "{refusal}");
    assert_eq!(refusal["exit_code"], 1, "{refusal}");
    let error = refusal["error"].as_str().unwrap();
    assert!(
        error.contains(&path.display().to_string()) && error.contains("interior corruption"),
        "names the segment: {error}"
    );
    writer.abandon();
}

/// Design §10: `tail RUN` of a run no Store records exits 1 naming it, before following
/// anything: with no telemetry directory, and over a directory that records another run, with
/// and without `--store` (a Store that does not record the run is the same unknown run).
#[test]
fn telemetry_tail_of_an_unknown_run_exits_1() {
    let unknown = |machine: &Machine, args: &[&str], run: &str| {
        let mut tail = machine
            .command(&machine.root, &[], args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (status, stderr) = exited(
            &mut tail,
            std::time::Duration::from_secs(60),
            "tail followed an unknown run",
        );
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut tail.stdout.take().unwrap(), &mut stdout).unwrap();
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        if args.contains(&"--json") {
            // The refusal is the `af/error@1` document, and nothing else.
            let refusal: Value = serde_json::from_str(stdout.trim()).unwrap();
            assert_eq!(refusal["schema"], "af/error@1", "{refusal}");
            assert_eq!(refusal["exit_code"], 1, "{refusal}");
            assert!(
                refusal["error"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("unknown run {run}")),
                "{refusal}"
            );
        } else {
            assert!(
                stderr.contains(&format!("unknown run {run}")),
                "{args:?}: {stderr}"
            );
            assert!(stdout.is_empty(), "{args:?}: {stdout}");
        }
    };
    let machine = Machine::new();
    for args in [
        &["telemetry", "tail", "task:none"][..],
        &["telemetry", "tail", "task:none", "--store", GOLDEN_STORE],
    ] {
        unknown(&machine, args, "task:none");
    }
    assert!(!machine.telemetry().exists(), "nothing was created");
    golden_execution(
        &machine.telemetry(),
        "1700000000000-4242-0a1b2c3d",
        1_700_000_000_000,
        1,
        &mut |_| {},
    );
    let other = "6".repeat(64);
    for (args, run) in [
        (&["telemetry", "tail", "task:none"][..], "task:none"),
        (
            &["telemetry", "tail", "task:none", "--store", GOLDEN_STORE][..],
            "task:none",
        ),
        (
            &["telemetry", "tail", GOLDEN_RUN, "--store", &other][..],
            GOLDEN_RUN,
        ),
        (
            &["telemetry", "tail", "task:none", "--json"][..],
            "task:none",
        ),
    ] {
        unknown(&machine, args, run);
    }
}

/// Design §6 and §10: a reader refuses what it cannot read. A live segment holds RUN's `af.run`
/// span, then an invalid complete line followed by a valid record, before `tail RUN` starts: the
/// index drops the segment's rows, so the run cannot be resolved. Tail never calls it unknown:
/// with and without `--store` it names the `.jsonl` and its interior corruption and exits as the
/// fail-closed refusal does (1, as the unknown run's exit is too: the message tells them apart),
/// following nothing.
#[test]
fn telemetry_tail_refuses_an_unreadable_segment_before_startup() {
    use review_telemetry::record::{Attrs, SpanKind};
    let machine = Machine::new();
    let segment = "1700000000000-4242-0a1b2c3d";
    let writer = golden_writer(&machine.telemetry(), segment, 1_700_000_000_000);
    let execution = review_telemetry::trace::Execution {
        store_id: GOLDEN_STORE.into(),
        run_id: GOLDEN_RUN.into(),
        execution_id: segment.into(),
        ordinal: 1,
        store_sequence_at_start: 2,
    };
    let trace = execution.trace_id();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            Some(&trace),
            Some(command.as_str()),
            "af.run",
            SpanKind::Internal,
            execution.attributes().enumerated("af.run.kind", "task"),
        )
        .unwrap();
    writer.flush();
    let path = writer.path().unwrap();
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"t\":\"event\",\"not\":\"a record\"}\n")
            .unwrap();
    }
    writer.event(&run, "af.after", Attrs::new());
    writer.flush();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        review_telemetry::segment::reader::scan(&bytes).interior_corruption,
        "an invalid line before a valid one"
    );
    let path = path.display().to_string();
    for args in [
        &["telemetry", "tail", GOLDEN_RUN][..],
        &["telemetry", "tail", GOLDEN_RUN, "--store", GOLDEN_STORE],
        &["telemetry", "tail", GOLDEN_RUN, "--json"],
        &["telemetry", "tail", "task:none"],
    ] {
        let mut tail = machine
            .command(&machine.root, &[], args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (status, stderr) = exited(
            &mut tail,
            std::time::Duration::from_secs(60),
            "tail followed a run it could not resolve",
        );
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut tail.stdout.take().unwrap(), &mut stdout).unwrap();
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        let error = if args.contains(&"--json") {
            // The refusal is the `af/error@1` document, and nothing else.
            let refusal: Value = serde_json::from_str(stdout.trim()).unwrap();
            assert_eq!(refusal["schema"], "af/error@1", "{refusal}");
            assert_eq!(refusal["exit_code"], 1, "{refusal}");
            refusal["error"].as_str().unwrap().to_string()
        } else {
            assert!(stdout.is_empty(), "{args:?}: nothing followed: {stdout}");
            stderr
        };
        assert!(
            error.contains(&path) && error.contains("interior corruption"),
            "{args:?}: names the segment and its corruption: {error}"
        );
        assert!(
            !error.contains("unknown run"),
            "{args:?}: never claims the run is unknown: {error}"
        );
    }
    writer.abandon();
}

/// Design §10: `tail RUN` of a run whose every segment was sealed before tail starts finishes at
/// once, with and without `--store`: nobody writes the run, so tail prints its sealed records as
/// the follow would have printed them — the second execution's exactly the lines the golden
/// follow printed for the run — and exits 0, never waiting on a run nobody is writing.
#[test]
fn telemetry_tail_of_a_sealed_run_finishes_at_once() {
    let machine = Machine::new();
    let root = machine.telemetry();
    let (first, second) = ("1700000000000-4242-0a1b2c3d", "1700000100000-4243-0b1c2d3e");
    golden_execution(&root, first, 1_700_000_000_000, 1, &mut |_| {});
    golden_execution(&root, second, 1_700_000_100_000, 2, &mut |_| {});
    // The run's lines of the golden follow of the second execution (that follow selected every
    // record, those outside the run's trace included).
    let followed: Vec<Value> = std::fs::read_to_string(golden_directory().join("tail.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["trace_id"].is_string())
        .collect();
    assert!(!followed.is_empty());
    for args in [
        &["telemetry", "tail", GOLDEN_RUN, "--json"][..],
        &[
            "telemetry",
            "tail",
            GOLDEN_RUN,
            "--store",
            GOLDEN_STORE,
            "--json",
        ],
        &["telemetry", "tail", GOLDEN_RUN],
        &["telemetry", "tail", GOLDEN_RUN, "--store", GOLDEN_STORE],
    ] {
        let mut tail = machine
            .command(&machine.root, &[], args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (status, stderr) = exited(
            &mut tail,
            std::time::Duration::from_secs(30),
            "tail waited on a run nobody is writing",
        );
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut tail.stdout.take().unwrap(), &mut stdout).unwrap();
        assert!(status.success(), "{args:?}: {stderr}");
        if !args.contains(&"--json") {
            assert!(stdout.contains("af.run"), "{args:?}: {stdout}");
            continue;
        }
        let lines: Vec<Value> = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let tail_schema = crate::schemas::validator("telemetry-tail-v1.json");
        for line in &lines {
            crate::schemas::valid(&tail_schema, line);
        }
        let of = |segment: &str| -> Vec<Value> {
            lines
                .iter()
                .filter(|line| line["segment_id"] == segment)
                .cloned()
                .collect()
        };
        assert_eq!(of(second), followed, "{args:?}");
        assert_eq!(of(first).len(), followed.len(), "{args:?}: {lines:?}");
        assert_eq!(
            of(first).len() + of(second).len(),
            lines.len(),
            "{args:?}: the run's two segments only"
        );
        assert!(
            lines
                .iter()
                .any(|line| line["segment_id"] == first && line["span"] == "af.run"),
            "{lines:?}"
        );
    }
}

/// Design §6 and §10: a followed segment that disappears without a validated sealed file or a
/// recovery tail itself completed is an error naming its path, never a seal. Tail follows a live
/// segment and is held stopped while its writer flushes, an invalid line and a valid record
/// after it are written, the writer dies before its seal, and a second reader recovers the
/// segment, finds the interior corruption and renames it `.corrupt`. Tail's next look reports
/// the `.corrupt` file and exits 1, printing nothing more.
#[test]
fn telemetry_tail_refuses_a_segment_recovered_as_corrupt_underneath() {
    use review_telemetry::record::{Attrs, SpanKind};
    let machine = Machine::new();
    let writer = review_telemetry::segment::writer::Writer::new(
        review_telemetry::segment::writer::WriterOptions::new(
            machine.telemetry(),
            review_telemetry::level::Level::Spans,
        ),
    )
    .unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(&command, "af.first", Attrs::new());
    writer.flush();
    let path = writer.path().unwrap();
    let segment = writer.segment_id().unwrap();
    let (mut tail, received, reader) = spawn_plain_tail(&machine, &["telemetry", "tail", "--json"]);
    let first: Value = serde_json::from_str(
        &received
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("tail prints the live event"),
    )
    .unwrap();
    assert_eq!(first["name"], "af.first", "{first}");
    signal(tail.id(), "STOP");
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"t\":\"event\",\"not\":\"a record\"}\n")
            .unwrap();
    }
    writer.event(&command, "af.after", Attrs::new());
    writer.flush();
    // Killed before its seal: its lock ends with it.
    writer.abandon();
    let layout = review_telemetry::layout::Layout::new(machine.telemetry());
    assert_eq!(
        review_telemetry::segment::reader::recover(&layout, &segment).unwrap(),
        review_telemetry::segment::reader::Recovery::Corrupt {
            segment_id: segment.clone()
        },
        "the second reader recovers it as corrupt"
    );
    let corrupt = layout.corrupt_segment(&segment);
    assert!(corrupt.exists() && !path.exists());
    signal(tail.id(), "CONT");
    let (status, stderr) = exited(
        &mut tail,
        std::time::Duration::from_secs(60),
        "tail went on following a segment recovered as corrupt",
    );
    reader.join().unwrap();
    assert_eq!(status.code(), Some(1), "never a seal: {stderr}");
    let rest: Vec<Value> = received
        .try_iter()
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect();
    let [refusal] = rest.as_slice() else {
        panic!("only the refusal, nothing of the corrupt segment: {rest:?}");
    };
    assert_eq!(refusal["schema"], "af/error@1", "{refusal}");
    assert_eq!(refusal["exit_code"], 1, "{refusal}");
    let error = refusal["error"].as_str().unwrap();
    assert!(
        error.contains(&corrupt.display().to_string()),
        "names the .corrupt file: {error}"
    );
}

/// An unsealed segment `segment` of the golden run's execution `ordinal`: its `af.run`, an
/// invalid complete line, then a valid record, its writer dead before its seal. `af telemetry
/// reindex` recovers it, finds the interior corruption and renames it `.corrupt`; the path of
/// that file.
fn corrupt_execution(machine: &Machine, segment: &str, start_ms: u64, ordinal: u64) -> PathBuf {
    use review_telemetry::record::{Attrs, SpanKind};
    let writer = golden_writer(&machine.telemetry(), segment, start_ms);
    let execution = review_telemetry::trace::Execution {
        store_id: GOLDEN_STORE.into(),
        run_id: GOLDEN_RUN.into(),
        execution_id: segment.into(),
        ordinal,
        store_sequence_at_start: 2,
    };
    let trace = execution.trace_id();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            Some(&trace),
            Some(command.as_str()),
            "af.run",
            SpanKind::Internal,
            execution.attributes().enumerated("af.run.kind", "task"),
        )
        .unwrap();
    writer.flush();
    let path = writer.path().unwrap();
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"t\":\"event\",\"not\":\"a record\"}\n")
            .unwrap();
    }
    writer.event(&run, "af.after", Attrs::new());
    writer.flush();
    // Killed before its seal: its lock ends with it.
    writer.abandon();
    let reindexed = json(&machine.af(&machine.root, &[], &["telemetry", "reindex", "--json"]));
    assert_eq!(
        reindexed["corrupt"],
        serde_json::json!([segment]),
        "{reindexed}"
    );
    let corrupt =
        review_telemetry::layout::Layout::new(machine.telemetry()).corrupt_segment(segment);
    assert!(corrupt.is_file() && !path.exists());
    corrupt
}

/// `tail` with `args` exits 1 with the fail-closed refusal naming `corrupt`, printing nothing
/// else and never calling the run unknown.
fn tail_refuses_naming(machine: &Machine, corrupt: &Path) {
    let corrupt = corrupt.display().to_string();
    for args in [
        &["telemetry", "tail", GOLDEN_RUN][..],
        &["telemetry", "tail", GOLDEN_RUN, "--store", GOLDEN_STORE],
        &["telemetry", "tail", GOLDEN_RUN, "--json"],
        &[
            "telemetry",
            "tail",
            GOLDEN_RUN,
            "--store",
            GOLDEN_STORE,
            "--json",
        ],
    ] {
        let mut tail = machine
            .command(&machine.root, &[], args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (status, stderr) = exited(
            &mut tail,
            std::time::Duration::from_secs(60),
            "tail followed a run with a corrupt execution",
        );
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut tail.stdout.take().unwrap(), &mut stdout).unwrap();
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        let error = if args.contains(&"--json") {
            // The refusal is the `af/error@1` document, and nothing else.
            let refusal: Value = serde_json::from_str(stdout.trim()).unwrap();
            assert_eq!(refusal["schema"], "af/error@1", "{refusal}");
            assert_eq!(refusal["exit_code"], 1, "{refusal}");
            refusal["error"].as_str().unwrap().to_string()
        } else {
            assert!(stdout.is_empty(), "{args:?}: nothing followed: {stdout}");
            stderr
        };
        assert!(
            error.contains(&corrupt) && error.contains("interior corruption"),
            "{args:?}: names the .corrupt file: {error}"
        );
        assert!(
            !error.contains("unknown run"),
            "{args:?}: never claims the run is unknown: {error}"
        );
    }
}

/// Design §6 and §10: a reader refuses what it cannot read. RUN's only execution is in a
/// segment recovery renamed `.corrupt` (index ingestion omits it, so the index cannot resolve
/// the run): `tail RUN`, with and without `--store`, names the `.corrupt` file and exits 1,
/// never reporting the run as unknown.
#[test]
fn telemetry_tail_refuses_a_corrupt_segment_of_an_unknown_run() {
    let machine = Machine::new();
    let corrupt = corrupt_execution(
        &machine,
        "1700000000000-4242-0a1b2c3d",
        1_700_000_000_000,
        1,
    );
    tail_refuses_naming(&machine, &corrupt);
}

/// Design §6 and §10: RUN has a sealed execution and a second one in a segment recovery renamed
/// `.corrupt`. `tail RUN`, with and without `--store`, refuses naming the `.corrupt` file
/// instead of finishing over the valid execution while omitting the corrupt one.
#[test]
fn telemetry_tail_refuses_a_corrupt_execution_beside_a_valid_one() {
    let machine = Machine::new();
    golden_execution(
        &machine.telemetry(),
        "1700000000000-4242-0a1b2c3d",
        1_700_000_000_000,
        1,
        &mut |_| {},
    );
    let corrupt = corrupt_execution(
        &machine,
        "1700000100000-4243-0b1c2d3e",
        1_700_000_100_000,
        2,
    );
    tail_refuses_naming(&machine, &corrupt);
}
