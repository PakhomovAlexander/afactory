//! Executing a check and recording what happened.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use review_store::{Cas, EventStore, NewEvent, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;

use review_core::{
    EventType,
    exec::{Arg, ArgError, Command},
};

pub const EVENT_CHECK_COMPLETED: EventType = EventType::CheckCompletedV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Could not execute. Never a pass — the gate treats it exactly as a failure, and the
    /// reason is recorded so the difference stays visible to a human.
    NotRun,
}

/// What a project declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckDefinition {
    pub name: String,
    pub command: Command,
    /// A required check blocks the gate. Optional checks are recorded and reported, never gating.
    pub required: bool,
}

impl CheckDefinition {
    pub fn new(name: impl Into<String>, command: Command) -> CheckDefinition {
        CheckDefinition {
            name: name.into(),
            command,
            required: true,
        }
    }

    pub fn optional(mut self) -> CheckDefinition {
        self.required = false;
        self
    }
}

/// One execution. Immutable, and content-addressed via its artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Absent when the definition named no program; an empty string would claim one existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub args: Vec<Arg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub required: bool,
}

impl CheckResult {
    pub fn passed(&self) -> bool {
        self.status == CheckStatus::Passed
    }

    /// Whether this result blocks a gate: a required check that did not pass, for either reason.
    pub fn blocks(&self) -> bool {
        self.required && !self.passed()
    }
}

/// Runs checks against a materialized tree.
///
/// Deliberately absent from the record: elapsed time. Nothing in any policy reads it, and its
/// presence would make an otherwise reproducible artifact differ on every run — the legacy
/// `checks.tsv` carried seconds, and the fixture corpus has to normalize them away to reproduce
/// at all.
pub struct CheckRunner<'a> {
    cas: &'a Cas,
    workdir: PathBuf,
    /// Environment handed to a check. Cleared and rebuilt, like the git adapter's.
    env: Vec<(String, String)>,
    /// A check that never returns must not hang the whole review — the gate runs first and the
    /// scheduler blocks on its completion. A check past this deadline is killed and recorded
    /// `not_run`, exactly as an unstartable one is. Generous by default (an engine build+test
    /// is legitimately long); a pipeline may tighten it.
    timeout: std::time::Duration,
}

impl<'a> CheckRunner<'a> {
    pub fn new(cas: &'a Cas, workdir: impl AsRef<Path>) -> CheckRunner<'a> {
        CheckRunner {
            cas,
            workdir: workdir.as_ref().to_path_buf(),
            env: vec![
                (
                    "PATH".to_string(),
                    std::env::var("PATH").unwrap_or_default(),
                ),
                ("LC_ALL".to_string(), "C".to_string()),
                ("TZ".to_string(), "UTC".to_string()),
            ],
            timeout: std::time::Duration::from_secs(3600),
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run one check. Never panics and never propagates a spawn failure as an error: a check
    /// that could not start is a *result*, because losing it would be the same as passing it.
    pub fn run(&self, definition: &CheckDefinition) -> CheckResult {
        let base = CheckResult {
            name: definition.name.clone(),
            status: CheckStatus::NotRun,
            exit_code: None,
            reason: None,
            program: (!definition.command.program.trim().is_empty())
                .then(|| definition.command.program.clone()),
            args: definition.command.args.clone(),
            stdout: None,
            stderr: None,
            required: definition.required,
        };

        let argv = match definition.command.resolve() {
            Ok(argv) => argv,
            Err(error) => {
                return CheckResult {
                    reason: Some(describe(&error)),
                    ..base
                };
            }
        };

        let mut cmd = std::process::Command::new(&definition.command.program);
        cmd.args(&argv);
        cmd.current_dir(&self.workdir);
        cmd.env_clear();
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        // A check that reads stdin would otherwise consume whatever the parent had open — the
        // shell harness hit exactly this, where one `ssh` swallowed the rest of the check list.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match self.run_with_deadline(&mut cmd) {
            RunResult::Completed(output) => output,
            RunResult::TimedOut => {
                return CheckResult {
                    reason: Some(format!(
                        "no result within {}s; the check was killed",
                        self.timeout.as_secs()
                    )),
                    ..base
                };
            }
            RunResult::CouldNotStart(error) => {
                return CheckResult {
                    reason: Some(format!(
                        "could not start `{}`: {error}",
                        definition.command.program
                    )),
                    ..base
                };
            }
        };

        let stdout = self.cas.put(&output.stdout);
        let stderr = self.cas.put(&output.stderr);
        let code = output.status.code();

        // Evidence that could not be preserved makes the result unverifiable, whatever the
        // exit code said: a Passed with silently missing output is "unverified reads as
        // verified", the exact shape `not_run` exists to block.
        if stdout.is_err() || stderr.is_err() {
            let detail: Vec<String> = [stdout.as_ref().err(), stderr.as_ref().err()]
                .into_iter()
                .flatten()
                .map(|e| e.to_string())
                .collect();
            return CheckResult {
                status: CheckStatus::NotRun,
                // No exit code on a not_run: the contract reserves it for checks that ran to
                // a verdict, and this one's verdict is unverifiable.
                exit_code: None,
                reason: Some(format!("evidence was not preserved: {}", detail.join("; "))),
                stdout: stdout.ok(),
                stderr: stderr.ok(),
                ..base
            };
        }
        let (stdout, stderr) = (stdout.ok(), stderr.ok());

        CheckResult {
            status: if code == Some(0) {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
            // A process killed by a signal has no exit code; -1 records "ran, did not exit
            // cleanly" rather than dropping the fact that it ran at all.
            exit_code: Some(code.unwrap_or(-1)),
            reason: code.is_none().then(|| "terminated by a signal".to_string()),
            stdout,
            stderr,
            ..base
        }
    }

    /// Run a list, recording each execution as its own event.
    pub fn run_all(
        &self,
        definitions: &[CheckDefinition],
        store: &mut EventStore,
        run_id: &str,
        node_id: &str,
    ) -> Result<Vec<CheckResult>, StoreError> {
        let mut results = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let result = self.run(definition);
            store.append_legacy(run_id, self.cas, check_event(&result, node_id))?;
            results.push(result);
        }
        Ok(results)
    }
}

/// The `CheckCompleted@1` event for one result. Exposed so a caller that must not hold a lock
/// across the check process — every check is a build or a test — can run the check first and
/// append this afterward, under a lock held only for the append.
pub fn check_event(result: &CheckResult, node_id: &str) -> NewEvent {
    let payload = serde_json::to_value(result).unwrap_or(json!({}));
    let refs: Vec<String> = [result.stdout.clone(), result.stderr.clone()]
        .into_iter()
        .flatten()
        .collect();
    NewEvent::new(EVENT_CHECK_COMPLETED, payload)
        .node(node_id)
        .correlating(result.name.clone())
        .referencing(refs)
}

enum RunResult {
    Completed(std::process::Output),
    TimedOut,
    CouldNotStart(std::io::Error),
}

impl CheckRunner<'_> {
    /// Spawn the command in its own process group and wait for it, killing the whole group past
    /// the deadline. The supervision mirrors `ModelRunner` because it must survive the same
    /// hazard: a wrapper check (`npx` → node, `sh` → a backgrounded child) leaves a grandchild
    /// holding the stdout pipe, so an unbounded `join` on the drain never returns EOF and the
    /// deadline the gate depends on is not enforceable. Two defenses, together: the child is a
    /// group leader (`process_group(0)`) so `killpg` reaps every descendant, and the drains
    /// report over channels collected with a bounded `recv_timeout` — never a plain `join` —
    /// so even a process that escaped the group (a `setsid` daemon) cannot hold the gate
    /// hostage.
    fn run_with_deadline(&self, cmd: &mut std::process::Command) -> RunResult {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => return RunResult::CouldNotStart(error),
        };

        let stdout_pipe = child.stdout.take().expect("stdout piped");
        let stderr_pipe = child.stderr.take().expect("stderr piped");
        let (out_send, out_recv) = std::sync::mpsc::channel();
        let (err_send, err_recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = out_send.send(drain(stdout_pipe));
        });
        std::thread::spawn(move || {
            let _ = err_send.send(drain(stderr_pipe));
        });

        let deadline = std::time::Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => break None,
            }
        };
        // Reap the whole group whatever the outcome: the check is over, and any surviving
        // descendant is an orphan holding the pipe. Killing it closes the write ends, so the
        // drains hit EOF at once — a check that passes but backgrounded a child returns now,
        // not after the 5s collection grace. (On the timeout path the leader is already gone;
        // this reaps what it left.)
        kill_process_group(child.id());
        let collect = |receiver: std::sync::mpsc::Receiver<Vec<u8>>| {
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_default()
        };
        let stdout = collect(out_recv);
        let stderr = collect(err_recv);
        match status {
            Some(status) => RunResult::Completed(std::process::Output {
                status,
                stdout,
                stderr,
            }),
            None => RunResult::TimedOut,
        }
    }
}

fn drain(mut pipe: impl std::io::Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    buffer
}

/// Kill everything in the child's process group, not only the child — a wrapper's grandchild
/// holding the pipe is exactly what the deadline must reach.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

fn describe(error: &ArgError) -> String {
    format!("refused before execution: {error}")
}
