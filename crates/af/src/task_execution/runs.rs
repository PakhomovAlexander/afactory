//! The processes that run Tasks (ADR-0205): every `af task run` records itself in its Store's
//! `runs/` directory before it takes the Task's lease, `runs/<task>-<run>.json`, and a detached
//! run writes its log beside it, `runs/<task>-<run>.log`. From those records `af task run
//! --detach` hands a run to the background, `af task wait` blocks until the Task settles, `af task
//! status` says where it is, and `af task cancel` stops the one process the Task's lease names —
//! never a process found by its name or its arguments — after recording who asked,
//! `runs/<task>-<run>.cancel.json`. While `af task run` waits it prints one line per node on
//! stderr and, every five minutes, a heartbeat.

use super::*;
use review_core::task::agent_loop::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::state::{TaskStatePhaseV1, TaskStateV1};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Set by `af task run --detach` on the run it starts in the background: that run's number,
/// whose log the parent created. Never set it yourself.
pub(crate) const DETACHED_ENV: &str = "AF_TASK_RUN_DETACHED";

/// How long `af task run --detach` waits for its run to take the Task and admit its plan before
/// it returns without claiming that it runs.
const DETACH_ADMISSION: Duration = Duration::from_secs(120);
/// The exit of `af task run --detach` whose run was not admitted within [`DETACH_ADMISSION`]:
/// incomplete, as `af help exit-codes` names 4.
const NOT_ADMITTED: i32 = 4;
/// The widest line `af task status` prints when stdout is no narrower terminal.
const WIDTH: usize = 100;
/// How long `af task cancel` waits for the run to stop after its first and its second signal.
const CANCEL_GRACE: Duration = Duration::from_secs(60);
const CANCEL_FORCE: Duration = Duration::from_secs(15);
/// The heartbeat of a waiting `af task run`.
const HEARTBEAT: Duration = Duration::from_secs(300);
/// The most log lines a run that ended before it went to the background shows.
const LOG_TAIL: usize = 40;

pub(crate) fn runs_dir(state: &Path) -> PathBuf {
    state.join("runs")
}

/// This host's name, as a run record and a cancel request name it.
pub(crate) fn this_host() -> String {
    review_process::host::host_name().unwrap_or_else(|| "unknown".into())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Whether a process of this host with `pid` exists.
fn alive(pid: u32) -> bool {
    i32::try_from(pid)
        .is_ok_and(|pid| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok())
}

/// The run number a file of `task_id` in `runs/` carries: `<task>-<run>.json`,
/// `<task>-<run>.log` or `<task>-<run>.cancel.json`, and which of the three it is.
fn run_of<'a>(task_id: &str, name: &'a str) -> Option<(u64, &'a str)> {
    let rest = name.strip_prefix(task_id)?.strip_prefix('-')?;
    let (number, kind) = rest.split_once('.')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) || number.starts_with('0') {
        return None;
    }
    let run = number.parse::<u64>().ok()?;
    matches!(kind, "json" | "log" | "cancel.json").then_some((run, kind))
}

/// Every file of `task_id` in `runs/`, by run number and kind.
fn files(state: &Path, task_id: &str) -> Vec<(u64, String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(runs_dir(state)) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (run, kind) = run_of(task_id, &name)?;
            Some((run, kind.to_owned(), entry.path()))
        })
        .collect();
    found.sort();
    found
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    // A record is small; one that is not of this release's shape is not read (ADR-0145).
    let bytes = std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 65_536)?;
    serde_json::from_slice(&bytes).ok()
}

/// The run processes the Store recorded for `task_id`, oldest first. A record of another shape
/// or version is not one: it is skipped.
pub(crate) fn records(state: &Path, task_id: &str) -> Vec<TaskRunProcessV1> {
    files(state, task_id)
        .into_iter()
        .filter(|(_, kind, _)| kind == "json")
        .filter_map(|(run, _, path)| read::<TaskRunProcessV1>(&path).filter(|r| r.run == run))
        .filter(|record| record.validate().is_ok() && record.task_id == task_id)
        .collect()
}

/// The cancel requests the Store recorded for `task_id`, oldest first.
pub(crate) fn cancel_requests(state: &Path, task_id: &str) -> Vec<TaskCancelRequestV1> {
    files(state, task_id)
        .into_iter()
        .filter(|(_, kind, _)| kind == "cancel.json")
        .filter_map(|(run, _, path)| read::<TaskCancelRequestV1>(&path).filter(|r| r.run == run))
        .filter(|request| request.validate().is_ok() && request.task_id == task_id)
        .collect()
}

/// The lease epoch of the Task's last lease, when an `af task cancel` request stopped exactly
/// that lease: what makes its state `host_cancelled`.
pub(crate) fn cancelled_epoch(state: Option<&Path>, task: &TaskProjection) -> Option<u64> {
    let state = state?;
    cancel_requests(state, &task.task_id)
        .into_iter()
        .rev()
        .find(|request| {
            request.epoch == task.lease_epoch() && request.writer == task.lease_writer()
        })
        .map(|request| request.epoch)
}

/// Write `value` as `name` in `runs/`, only when no file of that name exists: written aside and
/// linked into place, so a reader never sees half of it and two writers never share a name.
fn write_new(state: &Path, name: &str, value: &impl Serialize) -> Result<bool, String> {
    let directory = runs_dir(state);
    crate::storage::owned::private_dir(&directory)?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let aside = directory.join(format!(".{name}.{}.tmp", std::process::id()));
    review_store::local_fs::private_file_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&aside)
        .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
        .map_err(|e| format!("writing {}: {e}", aside.display()))?;
    let linked = std::fs::hard_link(&aside, directory.join(name));
    let _ = std::fs::remove_file(&aside);
    match linked {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(format!(
            "recording {} in {}: {error}",
            name,
            directory.display()
        )),
    }
}

/// Remove every run record, cancel request and log of `task_id`: what a collected Task leaves
/// in `runs/`. Best effort; a file that cannot be removed stays for the next collection.
pub(crate) fn forget(state: &Path, task_id: &str) {
    for (_, _, path) in files(state, task_id) {
        let _ = std::fs::remove_file(path);
    }
}

/// The next free run number of `task_id`: one more than any file of it in `runs/`.
fn next_run(state: &Path, task_id: &str) -> u64 {
    files(state, task_id)
        .last()
        .map_or(1, |(run, _, _)| run.saturating_add(1))
}

/// A duration a debug build's fixture sets through `name`, in milliseconds.
fn test_duration(name: &str) -> Option<Duration> {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        return Some(Duration::from_millis(ms));
    }
    let _ = name;
    None
}

/// The window `af task run --detach` gives its run to be admitted: two minutes, or what a debug
/// build's fixture sets.
fn detach_admission() -> Duration {
    test_duration("AF_TEST_DETACH_ADMISSION_MS").unwrap_or(DETACH_ADMISSION)
}

/// Record the process of this `af task run` before it takes the lease `writer` names. A
/// detached run takes the number its parent reserved with the log; any other takes the next.
pub(crate) fn begin(state: &Path, task_id: &str, writer: &str) -> Result<TaskRunProcessV1, String> {
    let detached = std::env::var(DETACHED_ENV)
        .ok()
        .and_then(|run| run.parse::<u64>().ok())
        .filter(|run| *run >= 1);
    let mut record = TaskRunProcessV1 {
        schema: TASK_RUN_PROCESS_V1.into(),
        task_id: task_id.into(),
        run: detached.unwrap_or_else(|| next_run(state, task_id)),
        pid: std::process::id(),
        host: this_host(),
        writer: writer.into(),
        started_unix_ms: now_ms(),
        detached: detached.is_some(),
        log: detached.map(|run| log_path(state, task_id, run).display().to_string()),
    };
    record.validate()?;
    for _ in 0..64 {
        if write_new(state, &format!("{task_id}-{}.json", record.run), &record)? {
            // A debug build's fixture holds a detached run back before it takes the Task, as a
            // loaded machine can, so its parent's admission window passes first.
            if let Some(delay) = test_duration("AF_TEST_DETACHED_START_DELAY_MS")
                && detached.is_some()
            {
                std::thread::sleep(delay);
            }
            return Ok(record);
        }
        if detached.is_some() {
            return Err(format!(
                "run {} of Task {task_id} is already recorded in {}; start it again with `af \
                 task run {task_id} --detach`",
                record.run,
                runs_dir(state).display()
            ));
        }
        record.run = next_run(state, task_id);
    }
    Err(format!(
        "no free run number for Task {task_id} in {}",
        runs_dir(state).display()
    ))
}

fn log_path(state: &Path, task_id: &str, run: u64) -> PathBuf {
    runs_dir(state).join(format!("{task_id}-{run}.log"))
}

// ------------------------------------------------------------------------------------------
// the state, a node name, durations and token figures as the lines print them

/// A compiled node as a person names it: `root.nodes.review.nodes.bugs` is `review.bugs`.
pub(super) fn node_name(node: &str) -> String {
    let node = node.strip_prefix("root.").unwrap_or(node);
    let node = node.strip_prefix("nodes.").unwrap_or(node);
    node.replace(".nodes.", ".")
}

/// A token figure as the progress lines print it: `412`, `41.2k`, `412k`, `1.3M`.
pub(super) fn tokens(n: u128) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{}.{}k", n / 1_000, n % 1_000 / 100),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000),
    }
}

fn committed_tokens(task: &TaskProjection) -> u128 {
    task.execution
        .as_ref()
        .map_or(0, |execution| execution.budget.committed_tokens())
}

/// The node whose Attempt runs now, and when it started: the latest started Attempt that
/// neither settled nor was released.
fn running_node(task: &TaskProjection) -> Option<(String, u64)> {
    task.execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
        .filter(|attempt| attempt.started && attempt.settled_unix_ms.is_none() && !attempt.released)
        .max_by_key(|attempt| attempt.started_unix_ms)
        .map(|attempt| {
            (
                attempt.reservation.node.clone(),
                attempt.started_unix_ms.unwrap_or(0),
            )
        })
}

/// The exit code a settled Task gives a script that waited for it: its outcome's, 4 for a Task
/// that waits for a person.
fn settled_code(state: &TaskStateV1) -> i32 {
    match state.outcome {
        Some(outcome) => i32::from(outcome.exit_code()),
        None if state.phase == TaskStatePhaseV1::Waiting => 4,
        None => 0,
    }
}

// ------------------------------------------------------------------------------------------
// af task status

fn status_document(opened: &OpenedTask, state: TaskStateV1) -> TaskStatusV1 {
    let task = &opened.task;
    let running = state.phase == TaskStatePhaseV1::Running;
    let elapsed_ms = task.clock_started_unix_ms().map(|started| {
        let until = if running {
            now_ms()
        } else {
            task.last_event_unix_ms()
        };
        until.saturating_sub(started)
    });
    let cancel = cancel_requests(&opened.state, &task.task_id)
        .into_iter()
        .rev()
        .find(|request| request.epoch == task.lease_epoch());
    TaskStatusV1 {
        schema: TASK_STATUS_V1.into(),
        task_id: task.task_id.clone(),
        state,
        node: running
            .then(|| running_node(task).map(|(node, _)| node_name(&node)))
            .flatten(),
        elapsed_ms,
        chargeable_tokens: committed_tokens(task).into(),
        run: lease_run(records(&opened.state, &task.task_id), task.lease_writer()),
        cancel,
    }
}

/// The run a status names: the one whose writer took the Task's last lease, which holds it while
/// the Task runs, never a newer record whose run was refused the lease; without such a record,
/// the newest run recorded.
fn lease_run(mut records: Vec<TaskRunProcessV1>, lease_writer: &str) -> Option<TaskRunProcessV1> {
    match records
        .iter()
        .rposition(|record| !lease_writer.is_empty() && record.writer == lease_writer)
    {
        Some(at) => Some(records.swap_remove(at)),
        None => records.pop(),
    }
}

/// The columns a line on stdout may take: the terminal's when stdout is a narrower one, 100
/// otherwise.
fn width() -> usize {
    use std::io::IsTerminal as _;
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return WIDTH;
    }
    rustix::termios::tcgetwinsize(&stdout)
        .ok()
        .map(|size| usize::from(size.ws_col))
        .filter(|columns| *columns >= 40)
        .map_or(WIDTH, |columns| columns.min(WIDTH))
}

/// `lead` and then `command` in lines of at most `width` columns, broken only between its
/// arguments, a `--flag` kept with its value: each line but the last ends with ` \\`, so the
/// lines paste into a shell as the one command, and each continuation is indented by `indent`.
/// An argument longer than a line keeps its line to itself.
pub(crate) fn command_lines(lead: &str, command: &str, width: usize, indent: &str) -> Vec<String> {
    let words: Vec<String> = match shell_words::split(command) {
        Ok(words) if !words.is_empty() => words
            .iter()
            .map(|word| shell_words::quote(word).into_owned())
            .collect(),
        _ => command.split_whitespace().map(str::to_owned).collect(),
    };
    let mut units: Vec<String> = Vec::new();
    let mut words = words.into_iter().peekable();
    while let Some(word) = words.next() {
        match words.peek() {
            Some(value) if word.starts_with("--") && !value.starts_with('-') => {
                let value = words.next().unwrap_or_default();
                units.push(format!("{word} {value}"));
            }
            _ => units.push(word),
        }
    }
    let mut lines = Vec::new();
    let mut line = lead.to_owned();
    let mut fresh = lead.trim().is_empty();
    for unit in units {
        // Room for ` UNIT` and, should another line follow, ` \\`.
        if !fresh && line.chars().count() + 1 + unit.chars().count() + 2 > width {
            line.push_str(" \\");
            lines.push(std::mem::replace(&mut line, indent.to_owned()));
            fresh = true;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(&unit);
        fresh = false;
    }
    lines.push(line);
    lines
}

/// `Task ID: running, node implement, 3 min, 41.2k tokens; run 2 pid 812 on builder`, then the
/// next command on its own indented line or lines: at most `width` columns each, the state
/// broken only between its parts and the command only between its arguments.
fn status_lines(status: &TaskStatusV1, width: usize) -> Vec<String> {
    let mut parts = vec![status.state.label()];
    if let Some(node) = &status.node {
        parts.push(format!("node {}", preview::text(node)));
    }
    if let Some(elapsed) = status.elapsed_ms {
        parts.push(state::duration_words(elapsed));
    }
    parts.push(format!("{} tokens", tokens(status.chargeable_tokens.get())));
    let mut segments = vec![format!("Task {}: {}", status.task_id, parts.join(", "))];
    if status.state.phase == TaskStatePhaseV1::Running
        && let Some(run) = &status.run
    {
        segments.push(format!(
            "run {} pid {} on {}",
            run.run,
            run.pid,
            preview::text(&run.host)
        ));
    }
    if let Some(cancel) = &status.cancel {
        segments.push(format!(
            "cancelled by {}@{}",
            preview::text(&cancel.requester.user),
            preview::text(&cancel.requester.host)
        ));
    }
    let mut lines: Vec<String> = Vec::new();
    for segment in segments {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 2 + segment.chars().count() <= width => {
                line.push_str("; ");
                line.push_str(&segment);
            }
            Some(_) => lines.push(format!("  {segment}")),
            None => lines.push(segment),
        }
    }
    if let Some(next) = &status.state.next_action {
        lines.extend(command_lines(
            "  next:",
            &preview::text(next),
            width,
            "        ",
        ));
    }
    lines
}

fn print_status(status: &TaskStatusV1, json: bool) -> Result<(), String> {
    status.validate()?;
    if json {
        println!(
            "{}",
            serde_json::to_string(status).map_err(|e| e.to_string())?
        );
    } else {
        for line in status_lines(status, width()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `af task status ID`: the state now, without waiting, and the next command. Exits 0: the state
/// is data.
pub(crate) fn status(id: &str, inspect: &crate::cli::TaskInspectArgs) -> Result<i32, String> {
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let (_, view) = opened.view()?;
    print_status(&status_document(&opened, view.state), inspect.json)?;
    Ok(0)
}

// ------------------------------------------------------------------------------------------
// af task wait

/// Whether a run of `task` is on its way here though it holds no lease yet: its newest record
/// names a live process of this host, recorded within the window a detached run has to take the
/// Task.
fn starting_run(state: &Path, task: &TaskProjection) -> Option<TaskRunProcessV1> {
    let record = records(state, &task.task_id).pop()?;
    (record.host == this_host()
        && alive(record.pid)
        && now_ms().saturating_sub(record.started_unix_ms)
            < u64::try_from(detach_admission().as_millis()).unwrap_or(u64::MAX)
        && record.writer != task.lease_writer())
    .then_some(record)
}

/// `af task wait ID [--timeout S]`: block until the Task settles — no writer holds it and no
/// run of this host is starting — then print its status and exit with its exit code; 124 when
/// `--timeout` passes first.
pub(crate) fn wait(
    id: &str,
    timeout: Option<u64>,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let mut opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let started = std::time::Instant::now();
    let limit = timeout.map(Duration::from_secs);
    let mut said = false;
    loop {
        let (_, view) = opened.view()?;
        let running = view.state.phase == TaskStatePhaseV1::Running;
        let starting = !running && starting_run(&opened.state, &opened.task).is_some();
        if !running && !starting {
            if view.state.phase == TaskStatePhaseV1::Planned && !said {
                let next = view
                    .state
                    .next_action
                    .as_deref()
                    .unwrap_or("af task run ID");
                return Err(format!(
                    "Task {id} is planned and no run of it is in progress, so there is nothing \
                     to wait for; run it with `{}`, or in the background with `{} --detach`",
                    preview::text(next),
                    preview::text(next)
                ));
            }
            let code = settled_code(&view.state);
            print_status(&status_document(&opened, view.state), inspect.json)?;
            return Ok(code);
        }
        if !said {
            eprintln!("af task wait: Task {id} is {}; waiting", view.state.label());
            said = true;
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            print_status(&status_document(&opened, view.state), inspect.json)?;
            eprintln!(
                "af task wait: Task {id} still runs after {}; wait again with `af task wait \
                 {id}`, or see where it is with `af task status {id}`",
                state::duration_words(u64::try_from(started.elapsed().as_millis()).unwrap_or(0))
            );
            return Ok(124);
        }
        std::thread::sleep(Duration::from_millis(250));
        opened.refresh()?;
    }
}

// ------------------------------------------------------------------------------------------
// af task run --detach

/// `af task run ID … --detach`: start the same command in the background, its stdin empty and
/// its output in `runs/<task>-<run>.log`, in its own process group and deaf to the terminal's
/// hangup, so it outlives the shell that started it; return once it holds the Task and its plan
/// is admitted, with its pid and its log. A run that is neither within the admission window is
/// not claimed to run: the command exits 4 and names `af task status ID`.
pub(crate) fn detach(id: &str, inspect: &crate::cli::TaskInspectArgs) -> Result<i32, String> {
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let state = opened.state.clone();
    crate::storage::owned::private_dir(&runs_dir(&state))?;
    // The log reserves the run number: created only where no file of it exists.
    let (run, log_file, log) = (|| {
        for _ in 0..64 {
            let run = next_run(&state, id);
            let path = log_path(&state, id, run);
            match review_store::local_fs::private_file_options()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((run, file, path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("creating {}: {error}", path.display())),
            }
        }
        Err(format!(
            "no free run number for Task {id} in {}",
            runs_dir(&state).display()
        ))
    })()?;
    let exe = std::env::current_exe().map_err(|e| format!("finding the af binary: {e}"))?;
    let arguments: Vec<std::ffi::OsString> = std::env::args_os()
        .skip(1)
        .filter(|word| word != "--detach")
        .collect();
    let mut command = std::process::Command::new(exe);
    command
        .args(&arguments)
        .env(DETACHED_ENV, run.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(
            log_file
                .try_clone()
                .map_err(|e| format!("opening {}: {e}", log.display()))?,
        )
        .stderr(log_file);
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting the background run of Task {id}: {e}"))?;
    let pid = child.id();
    let mut opened = opened;
    let since = std::time::Instant::now();
    let window = detach_admission();
    let admission = loop {
        if let Some(status) =
            review_process::reap::try_wait(&mut child).map_err(|e| e.to_string())?
        {
            // It ended before it went to the background: a refusal, or a Task with nothing
            // left to run. Its log says which.
            let code = status.code().unwrap_or(1);
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
            if code != 0 {
                return Err(format!(
                    "the run of Task {id} ended before it went to the background (exit {code}); \
                     the end of its log {}:\n{tail}",
                    log.display()
                ));
            }
            eprintln!("{tail}");
            break Admission::Ended;
        }
        opened.refresh()?;
        let record = records(&state, id).into_iter().find(|r| r.run == run);
        let holds = record.as_ref().is_some_and(|record| {
            opened.task.lease_writer() == record.writer
                && opened.task.lease_until_unix_ms() > now_ms()
        });
        if holds && opened.task.admitted {
            break Admission::Admitted;
        }
        if since.elapsed() >= window {
            // Still starting, or stuck: it neither took the Task nor admitted its plan, so this
            // says so and never that the Task runs.
            break Admission::Starting;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (_, view) = opened.view()?;
    let next_action = match admission {
        Admission::Admitted => Some(format!("af task wait {id}")),
        Admission::Starting => Some(format!("af task status {id}")),
        Admission::Ended => view.state.next_action.clone(),
    };
    let document = TaskDetachedV2 {
        schema: TASK_DETACHED_V2.into(),
        task_id: id.into(),
        run,
        pid,
        host: this_host(),
        log: log.display().to_string(),
        admitted: admission == Admission::Admitted,
        state: view.state,
        next_action,
    };
    document.validate()?;
    let next = document.next_action.as_deref().unwrap_or("none");
    if inspect.json {
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
    } else {
        let host = preview::text(&document.host);
        match admission {
            Admission::Admitted => println!(
                "Task {id} runs in the background: run {run}, pid {pid} on {host}\nlog: {}\n\
                 next: {next}",
                log.display()
            ),
            Admission::Starting => println!(
                "Task {id} is not admitted yet: run {run}, pid {pid} on {host}, has not taken \
                 the Task\nlog: {}\nnext: {next}",
                log.display()
            ),
            Admission::Ended => println!(
                "Task {id}: {}; its run ended at once, its log is {}\nnext: {next}",
                document.state.label(),
                log.display()
            ),
        }
    }
    if admission == Admission::Starting {
        let waited = state::duration_words(u64::try_from(window.as_millis()).unwrap_or(u64::MAX));
        eprintln!(
            "af task run --detach: run {run} of Task {id} (pid {pid}) neither took the Task nor\n\
             admitted its plan within {waited}; it may still start, or it stopped: read its log,\n\
             or see where it is with `af task status {id}`"
        );
        return Ok(NOT_ADMITTED);
    }
    Ok(0)
}

/// What became of the run `af task run --detach` started, when it returned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// It holds the Task's lease and its plan is admitted: it runs in the background.
    Admitted,
    /// Neither, within the window: still starting, or stuck before the Task.
    Starting,
    /// It ended before it went to the background, exit 0: the Task had nothing left to run.
    Ended,
}

// ------------------------------------------------------------------------------------------
// af task cancel

fn requester() -> TaskCancelRequesterV1 {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.name)
        .or_else(|| std::env::var("USER").ok())
        .filter(|name| !name.trim().is_empty() && name.len() <= 255)
        .unwrap_or_else(|| format!("uid {}", nix::unistd::getuid()));
    TaskCancelRequesterV1 {
        user,
        host: this_host(),
        pid: std::process::id(),
    }
}

/// Whether the run that held `epoch` stopped: the Task's lease is no longer that run's, or it
/// is not held any more.
fn stopped(opened: &mut OpenedTask, epoch: u64) -> Result<bool, String> {
    opened.refresh()?;
    Ok(opened.task.lease_epoch() != epoch || opened.task.lease_until_unix_ms() <= now_ms())
}

fn signal(pid: u32) -> Result<(), String> {
    let target = nix::unistd::Pid::from_raw(
        i32::try_from(pid).map_err(|_| format!("pid {pid} is not a process id"))?,
    );
    match nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(format!("signalling pid {pid}: {error}")),
    }
}

fn wait_stopped(opened: &mut OpenedTask, epoch: u64, limit: Duration) -> Result<bool, String> {
    let since = std::time::Instant::now();
    while since.elapsed() < limit {
        if stopped(opened, epoch)? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stopped(opened, epoch)
}

/// `af task cancel ID`: stop the `af task run` that holds the Task's lease, found by the
/// lease's writer in the Store's run records, and so its Worker processes, which that run stops
/// as it does on Ctrl-C (ADR-0129). The request is recorded first, with who asked; the Task
/// stays resumable with `af task run ID`. A writer on another host is refused with its pid and
/// host: only the recorded pid is ever signalled, and only here.
pub(crate) fn cancel(id: &str, inspect: &crate::cli::TaskInspectArgs) -> Result<i32, String> {
    let mut opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let (_, view) = opened.view()?;
    let held = opened.task.lease_until_unix_ms() > now_ms();
    if !held {
        return Err(format!(
            "Task {id} is not running: it is {}, and no writer holds it, so nothing was \
             cancelled; next: {}",
            view.state.label(),
            view.state.next_action.as_deref().unwrap_or("none")
        ));
    }
    let writer = opened.task.lease_writer().to_owned();
    let epoch = opened.task.lease_epoch();
    if writer.starts_with("delivery-") {
        return Err(format!(
            "Task {id} is being delivered (writer {writer}), not run: `af task cancel` stops \
             only an `af task run`; let the delivery finish"
        ));
    }
    let Some(record) = records(&opened.state, id)
        .into_iter()
        .rev()
        .find(|record| record.writer == writer)
    else {
        return Err(format!(
            "Task {id} runs under writer `{writer}`, and {} records no run of that writer, so \
             af does not know which process it is: stop it where it runs (Ctrl-C in its \
             terminal), then `af task run {id}` resumes it",
            runs_dir(&opened.state).display()
        ));
    };
    let host = this_host();
    if record.host != host {
        return Err(format!(
            "Task {id} runs as pid {} on host {}, not on this host ({}): af signals only a \
             process of its own host; run `af task cancel {id}` on {}",
            record.pid,
            preview::text(&record.host),
            preview::text(&host),
            preview::text(&record.host)
        ));
    }
    let request = TaskCancelRequestV1 {
        schema: TASK_CANCEL_REQUEST_V1.into(),
        task_id: id.into(),
        run: record.run,
        epoch,
        writer: writer.clone(),
        pid: record.pid,
        host: record.host.clone(),
        requester: requester(),
        requested_unix_ms: now_ms(),
    };
    request.validate()?;
    let (request, ours) = record_request(&opened.state, request)?;
    if ours {
        eprintln!(
            "af task cancel: stopping run {} of Task {id} (pid {} on {})",
            record.run,
            record.pid,
            preview::text(&record.host)
        );
        signal(record.pid)?;
    } else {
        // Another `af task cancel` asked first and signalled the run: that request stands, and
        // this one waits for the same stop rather than signalling again, which would force it.
        eprintln!(
            "af task cancel: run {} of Task {id} was already asked to stop by {}@{} at {}; \
             waiting for it to stop",
            record.run,
            preview::text(&request.requester.user),
            preview::text(&request.requester.host),
            state::local_time(request.requested_unix_ms)
        );
    }
    if !wait_stopped(&mut opened, epoch, CANCEL_GRACE)? {
        // A second signal: the run kills its Worker process groups and exits at once.
        eprintln!(
            "af task cancel: pid {} did not stop within {}; signalling it again",
            record.pid,
            state::duration_words(u64::try_from(CANCEL_GRACE.as_millis()).unwrap_or(0))
        );
        signal(record.pid)?;
        if !wait_stopped(&mut opened, epoch, CANCEL_FORCE)? {
            return Err(format!(
                "Task {id}'s run (pid {} on {}) did not stop after two signals; it still holds \
                 the Task: see `af task status {id}`",
                record.pid,
                preview::text(&record.host)
            ));
        }
    }
    let (_, view) = opened.view()?;
    let document = TaskCancelledV1 {
        schema: TASK_CANCELLED_V1.into(),
        task_id: id.into(),
        request,
        state: view.state,
    };
    document.validate()?;
    if inspect.json {
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Task {id}: cancelled run {} (pid {} on {}) and its Worker processes\nstate: {}\n\
             next: {}",
            document.request.run,
            document.request.pid,
            preview::text(&document.request.host),
            document.state.label(),
            document.state.next_action.as_deref().unwrap_or("none")
        );
    }
    Ok(0)
}

/// Record `request` as the cancel request of its run, unless one is recorded already: then the
/// recorded one stands, the first asker's, and is returned instead. The second value says
/// whether `request` is the one recorded.
fn record_request(
    state: &Path,
    request: TaskCancelRequestV1,
) -> Result<(TaskCancelRequestV1, bool), String> {
    let name = format!("{}-{}.cancel.json", request.task_id, request.run);
    if write_new(state, &name, &request)? {
        return Ok((request, true));
    }
    let path = runs_dir(state).join(&name);
    let recorded = read::<TaskCancelRequestV1>(&path)
        .filter(|recorded| {
            recorded.validate().is_ok()
                && recorded.task_id == request.task_id
                && recorded.run == request.run
        })
        .ok_or_else(|| {
            format!(
                "{} holds a cancel request of run {} that this release cannot read; remove it, \
                 then run `af task cancel {}` again",
                path.display(),
                request.run,
                request.task_id
            )
        })?;
    Ok((recorded, false))
}

/// The line `af task show` prints for a Task an `af task cancel` stopped: who asked, and when.
pub(super) fn cancel_line(state: &Path, task: &TaskProjection) -> Option<String> {
    let request = cancel_requests(state, &task.task_id)
        .into_iter()
        .rev()
        .find(|request| {
            request.epoch == task.lease_epoch() && request.writer == task.lease_writer()
        })?;
    Some(format!(
        "cancelled: run {} by {}@{} at {} (`af task cancel`)",
        request.run,
        preview::text(&request.requester.user),
        preview::text(&request.requester.host),
        state::local_time(request.requested_unix_ms)
    ))
}

/// Lines a presentation of the Task `af task show` names prints above its `next:` line.
static SHOW_NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn note_for_show(line: String) {
    SHOW_NOTES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(line);
}

pub(super) fn take_show_notes() -> Vec<String> {
    std::mem::take(
        &mut *SHOW_NOTES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

// ------------------------------------------------------------------------------------------
// progress of af task run

/// The heartbeat interval: five minutes, or what a debug build's fixture sets.
fn heartbeat() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("AF_TEST_HEARTBEAT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        return Duration::from_millis(ms);
    }
    HEARTBEAT
}

/// What a progress watcher printed already: the Attempts it saw start and settle, the nodes it
/// printed a `started` line for that have not finished or failed since, and the nodes it saw
/// finish.
#[derive(Default)]
struct Seen {
    started: BTreeSet<String>,
    settled: BTreeSet<String>,
    open: BTreeSet<String>,
    finished: BTreeSet<String>,
}

impl Seen {
    /// Everything `task` already records, so a resumed run prints only what it does itself.
    fn of(task: &TaskProjection) -> Self {
        let mut seen = Self::default();
        seen.lines(task, 0);
        seen
    }

    /// The lines for what `task` records and this watcher has not printed yet: every node that
    /// runs gets one `started` line before each `finished` or `failed` line of it — a node with
    /// Attempts when one starts, a node the kernel runs itself (seal, check, reduce, accept)
    /// when it is invoked, or, seen only once it published, just before its `finished`.
    fn lines(&mut self, task: &TaskProjection, run_started: u64) -> Vec<String> {
        let Some(execution) = &task.execution else {
            return Vec::new();
        };
        let figure = |at: u64| {
            format!(
                "{} elapsed, {} tokens so far",
                state::duration_words(at.saturating_sub(run_started)),
                tokens(execution.budget.committed_tokens())
            )
        };
        let mut events: Vec<(u64, String)> = Vec::new();
        let mut attempts = execution.attempt_accounting();
        attempts.sort_by_key(|attempt| (attempt.started_unix_ms, attempt.settled_unix_ms));
        for attempt in &attempts {
            let node = &attempt.reservation.node;
            let name = node_name(node);
            if attempt.started && self.started.insert(attempt.attempt_id.clone()) {
                let at = attempt.started_unix_ms.unwrap_or(run_started);
                // A node announced when it was invoked has its start line already.
                if self.open.insert(node.clone()) {
                    events.push((at, format!("{name} started ({})", figure(at))));
                }
            }
            if let Some(at) = attempt.settled_unix_ms
                && self.settled.insert(attempt.attempt_id.clone())
            {
                if self.open.insert(node.clone()) {
                    // Settled before this watcher saw it start.
                    let begun = attempt.started_unix_ms.unwrap_or(at);
                    events.push((begun, format!("{name} started ({})", figure(begun))));
                }
                self.open.remove(node);
                let word = match &attempt.result {
                    Some(TaskAttemptResultV1::Succeeded { .. }) => {
                        self.finished.insert(node.clone());
                        "finished"
                    }
                    _ => "failed",
                };
                events.push((at, format!("{name} {word} ({})", figure(at))));
            }
        }
        let order = &execution.graph.order;
        let position = |node: &String| order.iter().position(|n| n == node);
        // The root's inputs are the Task's, not a node that ran.
        let root = |node: &String| {
            execution.graph.nodes.get(node).is_some_and(|compiled| {
                matches!(
                    compiled.operator,
                    review_graph::task::CompiledOperator::RootInputs
                )
            })
        };
        let has_attempts = |node: &String| {
            attempts
                .iter()
                .any(|attempt| &attempt.reservation.node == node)
        };
        // A node the kernel runs without an Attempt starts when it is invoked, and finishes
        // when it publishes its output.
        let mut invoked: Vec<&String> = execution.invocations.keys().collect();
        invoked.sort_by_key(|node| position(node));
        let now = now_ms();
        for node in invoked {
            if root(node)
                || has_attempts(node)
                || self.finished.contains(node)
                || execution.outputs.contains_key(node)
                || !self.open.insert(node.clone())
            {
                continue;
            }
            events.push((
                u64::MAX - 1,
                format!("{} started ({})", node_name(node), figure(now)),
            ));
        }
        let mut published: Vec<&String> = execution.outputs.keys().collect();
        published.sort_by_key(|node| position(node));
        for node in published {
            if root(node) || !self.finished.insert(node.clone()) {
                continue;
            }
            if self.open.insert(node.clone()) {
                events.push((
                    u64::MAX,
                    format!("{} started ({})", node_name(node), figure(now)),
                ));
            }
            self.open.remove(node);
            events.push((
                u64::MAX,
                format!("{} finished ({})", node_name(node), figure(now)),
            ));
        }
        // Stable: a node's start keeps its place before its finish at the same time.
        events.sort_by_key(|(at, _)| *at);
        events
            .into_iter()
            .map(|(_, line)| format!("af task run: {line}"))
            .collect()
    }
}

/// The heartbeat line: `af task run: implement running 23 min, 412k tokens, host load 11.2`.
fn heartbeat_line(task: &TaskProjection, run_started: u64) -> String {
    let now = now_ms();
    let what = match running_node(task) {
        Some((node, since)) => format!(
            "{} running {}",
            node_name(&node),
            state::duration_words(now.saturating_sub(since.max(run_started)))
        ),
        None => format!(
            "running {}",
            state::duration_words(now.saturating_sub(run_started))
        ),
    };
    let load = review_process::host::load_average_1m()
        .map(|load| format!(", host load {load:.1}"))
        .unwrap_or_default();
    format!(
        "af task run: {what}, {} tokens{load}",
        tokens(committed_tokens(task))
    )
}

/// A watcher of one run of `af task run`: it reads the Task from its own read-only Store
/// connection, prints one line on stderr per node that starts or finishes with the time since
/// the run started and the tokens charged so far, and a heartbeat every five minutes. Dropped,
/// it prints what it has not yet and stops.
pub(crate) struct Progress {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Progress {
    pub(crate) fn start(state: &Path, task_id: &str, run_started: u64) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&stop);
        let (state, task_id) = (state.to_path_buf(), task_id.to_owned());
        let thread = std::thread::Builder::new()
            .name("af-task-progress".into())
            .spawn(move || watch(&state, &task_id, run_started, &watching))
            .ok();
        Self { stop, thread }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(state: &Path, task_id: &str, run_started: u64, stop: &AtomicBool) {
    let (Ok(cas), Ok(store)) = (
        Cas::open_existing(state.join("cas")),
        EventStore::open_read_only(state.join("events.sqlite")),
    ) else {
        return;
    };
    let Ok(run_id) = review_store::store::task::task_run_id(task_id) else {
        return;
    };
    let project = || store.task_projection(&cas, task_id).ok().flatten();
    let Some(first) = project() else {
        return;
    };
    let mut seen = Seen::of(&first);
    let mut sequence = store.latest_run_sequence(&run_id).ok().flatten();
    let interval = heartbeat();
    let mut beat = std::time::Instant::now();
    let mut poll = Duration::from_millis(500);
    loop {
        let finishing = stop.load(Ordering::Acquire);
        let latest = store.latest_run_sequence(&run_id).ok().flatten();
        if latest != sequence || finishing {
            sequence = latest;
            let began = std::time::Instant::now();
            if let Some(task) = project() {
                for line in seen.lines(&task, run_started) {
                    eprintln!("{line}");
                }
            }
            // A long log takes long to project: poll it less often than it takes.
            poll = (began.elapsed() * 4).clamp(Duration::from_millis(500), Duration::from_secs(10));
        }
        if finishing {
            return;
        }
        if beat.elapsed() >= interval {
            beat = std::time::Instant::now();
            if let Some(task) = project() {
                eprintln!("{}", heartbeat_line(&task, run_started));
            }
        }
        let until = std::time::Instant::now() + poll.min(interval);
        while std::time::Instant::now() < until && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_files_are_read_by_their_task_and_number_only() {
        assert_eq!(run_of("t", "t-1.json"), Some((1, "json")));
        assert_eq!(run_of("t", "t-12.log"), Some((12, "log")));
        assert_eq!(run_of("t", "t-3.cancel.json"), Some((3, "cancel.json")));
        // Another Task whose ID starts with this one's, a leading zero and other names.
        assert_eq!(run_of("t", "t-1-2.json"), None);
        assert_eq!(run_of("t-1", "t-1-2.json"), Some((2, "json")));
        assert_eq!(run_of("t", "t-01.json"), None);
        assert_eq!(run_of("t", "t-1.txt"), None);
        assert_eq!(run_of("t", ".t-1.json.4.tmp"), None);
    }

    #[test]
    fn records_take_the_next_number_and_a_cancel_request_names_its_lease() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path();
        let first = begin(state, "t", "cli-1").unwrap();
        let second = begin(state, "t", "cli-2").unwrap();
        assert_eq!((first.run, second.run), (1, 2));
        assert!(!first.detached && first.log.is_none());
        assert_eq!(records(state, "t"), vec![first.clone(), second.clone()]);
        assert!(records(state, "other").is_empty());
        // A record of another version is not read.
        std::fs::write(
            runs_dir(state).join("t-3.json"),
            serde_json::to_vec(&serde_json::json!({"schema":"af/task-run-process@0"})).unwrap(),
        )
        .unwrap();
        assert_eq!(records(state, "t").len(), 2);
        assert_eq!(next_run(state, "t"), 4);
        let request = TaskCancelRequestV1 {
            schema: TASK_CANCEL_REQUEST_V1.into(),
            task_id: "t".into(),
            run: 2,
            epoch: 5,
            writer: "cli-2".into(),
            pid: 2,
            host: second.host.clone(),
            requester: requester(),
            requested_unix_ms: 1,
        };
        assert!(write_new(state, "t-2.cancel.json", &request).unwrap());
        assert!(
            !write_new(state, "t-2.cancel.json", &request).unwrap(),
            "never twice"
        );
        assert_eq!(cancel_requests(state, "t"), vec![request]);
        let other = begin(state, "t-1", "cli-3").unwrap();
        forget(state, "t");
        assert!(records(state, "t").is_empty() && cancel_requests(state, "t").is_empty());
        assert_eq!(
            records(state, "t-1"),
            vec![other],
            "another Task's records stay"
        );
    }

    fn record(run: u64, pid: u32, writer: &str) -> TaskRunProcessV1 {
        TaskRunProcessV1 {
            schema: TASK_RUN_PROCESS_V1.into(),
            task_id: "t".into(),
            run,
            pid,
            host: "builder".into(),
            writer: writer.into(),
            started_unix_ms: run,
            detached: false,
            log: None,
        }
    }

    /// Run 1 holds the lease; run 2 recorded itself and was refused the lease. The status
    /// names run 1, whose writer the lease names, never the newer record.
    #[test]
    fn a_status_names_the_run_that_holds_the_lease() {
        let records = vec![record(1, 101, "cli-101"), record(2, 202, "cli-202")];
        assert_eq!(lease_run(records.clone(), "cli-101").unwrap().pid, 101);
        assert_eq!(lease_run(records.clone(), "cli-202").unwrap().pid, 202);
        // A lease no record names (a delivery, a run of an earlier release): the newest run.
        assert_eq!(lease_run(records.clone(), "delivery-7").unwrap().pid, 202);
        assert_eq!(lease_run(records, "").unwrap().pid, 202);
        assert_eq!(lease_run(Vec::new(), "cli-101"), None);
    }

    fn finished(next: &str) -> TaskStatusV1 {
        TaskStatusV1 {
            schema: TASK_STATUS_V1.into(),
            task_id: "implementation-small".into(),
            state: TaskStateV1::ended(
                TaskStatePhaseV1::Finished,
                review_core::task::state::TaskOutcomeV1::Verified,
                None,
            )
            .with_next(next),
            node: None,
            elapsed_ms: Some(1_000),
            chargeable_tokens: 0_u128.into(),
            run: None,
            cancel: None,
        }
    }

    #[test]
    fn a_status_puts_the_next_command_on_its_own_lines_within_the_width() {
        let next = "af task deliver implementation-small --branch af/implementation-small \
                    --worktree ../implementation-small --confirm implementation-small";
        let status = finished(next);
        for width in [100, 80] {
            let lines = status_lines(&status, width);
            assert_eq!(
                lines[0], "Task implementation-small: finished/verified, 1 s, 0 tokens",
                "{lines:#?}"
            );
            assert!(
                lines[1].starts_with("  next: af task deliver"),
                "{lines:#?}"
            );
            for line in &lines {
                assert!(line.chars().count() <= width, "{width}: {line}");
            }
            // Broken only between arguments, each line but the last continued: the lines are
            // the one command a shell reads.
            let joined: Vec<String> = lines[1..]
                .iter()
                .map(|line| line.trim().trim_end_matches('\\').trim().to_owned())
                .collect();
            assert_eq!(joined.join(" "), format!("next: {next}"));
            for line in &lines[1..lines.len() - 1] {
                assert!(line.ends_with(" \\"), "{line}");
            }
        }
        assert_eq!(
            status_lines(&status, 80),
            [
                "Task implementation-small: finished/verified, 1 s, 0 tokens",
                "  next: af task deliver implementation-small --branch af/implementation-small \\",
                "        --worktree ../implementation-small --confirm implementation-small",
            ]
        );
        // A short command stays on its one line.
        assert_eq!(
            status_lines(&finished("af task show implementation-small"), 100)[1],
            "  next: af task show implementation-small"
        );
    }

    #[test]
    fn a_command_breaks_between_its_arguments_and_keeps_its_quoting() {
        let lines = command_lines(
            "  next:",
            "af review run --state '/a b/c' --campaign x",
            30,
            "    ",
        );
        assert_eq!(
            lines,
            [
                "  next: af review run \\",
                "    --state '/a b/c' \\",
                "    --campaign x",
            ]
        );
        let long = format!("af task show {}", "x".repeat(60));
        let lines = command_lines("", &long, 40, "  ");
        assert_eq!(
            lines[1],
            format!("  {}", "x".repeat(60)),
            "a long word keeps its line"
        );
    }

    /// Two cancels of one run: the first request is recorded and stands; the second gets the
    /// first's requester and time back, never its own.
    #[test]
    fn a_second_cancel_returns_the_first_request() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path();
        let request = |user: &str, at: u64| TaskCancelRequestV1 {
            schema: TASK_CANCEL_REQUEST_V1.into(),
            task_id: "t".into(),
            run: 1,
            epoch: 3,
            writer: "cli-1".into(),
            pid: 1,
            host: "builder".into(),
            requester: TaskCancelRequesterV1 {
                user: user.into(),
                host: "builder".into(),
                pid: 9,
            },
            requested_unix_ms: at,
        };
        let (first, ours) = record_request(state, request("first", 10)).unwrap();
        assert!(ours);
        assert_eq!(first.requester.user, "first");
        let (second, ours) = record_request(state, request("second", 20)).unwrap();
        assert!(!ours, "the second request is not recorded");
        assert_eq!(second, first, "the second cancel reports the first request");
        assert_eq!(cancel_requests(state, "t"), vec![first]);
        // A request file this release cannot read is refused with what to do.
        std::fs::write(runs_dir(state).join("t-2.cancel.json"), b"{}").unwrap();
        let mut other = request("third", 30);
        other.run = 2;
        let error = record_request(state, other).unwrap_err();
        assert!(
            error.contains("t-2.cancel.json") && error.contains("af task cancel t"),
            "{error}"
        );
    }

    #[test]
    fn nodes_tokens_and_codes_read_as_the_lines_print_them() {
        assert_eq!(node_name("root.nodes.implement"), "implement");
        assert_eq!(node_name("root.nodes.review.nodes.bugs"), "review.bugs");
        assert_eq!(node_name("root.inputs"), "inputs");
        assert_eq!(tokens(412), "412");
        assert_eq!(tokens(41_200), "41k");
        assert_eq!(tokens(4_120), "4.1k");
        assert_eq!(tokens(412_000), "412k");
        assert_eq!(tokens(1_250_000), "1.2M");
        let cancelled = TaskStateV1::ended(
            TaskStatePhaseV1::Abandoned,
            review_core::task::state::TaskOutcomeV1::Cancelled,
            Some(review_core::task::state::TaskCauseV1::HostCancelled),
        );
        assert_eq!(settled_code(&cancelled), 4);
        assert_eq!(
            settled_code(&TaskStateV1::new(TaskStatePhaseV1::Waiting)),
            4
        );
        assert_eq!(
            settled_code(&TaskStateV1::ended(
                TaskStatePhaseV1::Finished,
                review_core::task::state::TaskOutcomeV1::Verified,
                None
            )),
            0
        );
    }
}
