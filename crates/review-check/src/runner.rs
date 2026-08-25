//! Executing a check and recording what happened.

use review_process::{ExitPolicy, SupervisedError, run_supervised_with_policy};
use review_store::{Cas, EventStore, NewEvent, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

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
    /// Use the shared process supervisor with the check-specific exit policy: a successful check
    /// is over when its leader exits, so background descendants are reaped immediately rather
    /// than being allowed to hold evidence pipes open.
    fn run_with_deadline(&self, cmd: &mut std::process::Command) -> RunResult {
        match run_supervised_with_policy(cmd, None, self.timeout, ExitPolicy::KillProcessGroup) {
            Ok(output) => RunResult::Completed(std::process::Output {
                status: output.status,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Err(SupervisedError::TimedOut { .. }) => RunResult::TimedOut,
            Err(SupervisedError::Spawn(error)) => RunResult::CouldNotStart(error),
            Err(error) => RunResult::CouldNotStart(std::io::Error::other(error)),
        }
    }
}

fn describe(error: &ArgError) -> String {
    format!("refused before execution: {error}")
}
