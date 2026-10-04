//! Executing a check and recording what happened.

use review_process::{ExitPolicy, SupervisedError, run_supervised_with_policy};
use review_store::{Cas, NewEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use review_core::{
    EventType,
    exec::{Arg, ArgError, Command},
    task::remote_check::RemoteCheckV1,
};

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
    /// The same check run by a remote executor (ADR-0136). Committed policy only declares it;
    /// an operator's machine-local mapping selects it. Absent, a definition serializes exactly
    /// as it did before the table existed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub remote: Option<RemoteCheckV1>,
}

impl CheckDefinition {
    pub fn new(name: impl Into<String>, command: Command) -> CheckDefinition {
        CheckDefinition {
            name: name.into(),
            command,
            required: true,
            remote: None,
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
    /// The `af/RemoteCheckEvidence@1` artifact a remote result was derived from. A result that
    /// carries it is the second shape (ADR-0136): no program, exit code, arguments or `stderr`,
    /// and as `stdout` only the log excerpt of remote jobs that did not succeed; a local result
    /// never carries it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub remote: Option<String>,
}

/// Host-clock observation of the shared check boundary. It is retained separately from the
/// reproducible `CheckResult@1` payload, whose identity must not vary with wall time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckExecution {
    pub result: CheckResult,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    /// How the process ended, typed, for a caller that names the bound itself; never recorded.
    pub ending: CheckEnding,
}

/// How a check's process ended, kept beside the recorded result for callers that classify the
/// end themselves — a measured repetition names the time bound that ended it — and never
/// serialized: the recorded result carries the human-readable reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckEnding {
    /// The process exited on its own, whatever its status.
    Exited,
    /// A signal ended it: no exit code was observed, and the recorded result's `-1` is this
    /// runner's sentinel for that, not something the command produced.
    Signaled,
    /// The kernel ended it at the runner's timeout.
    TimedOut,
    /// The kernel ended it because the Attempt was cancelled.
    Cancelled,
    /// It never ran: refused before execution, cancelled before starting, or could not start.
    NotStarted,
}

impl CheckResult {
    pub fn passed(&self) -> bool {
        self.status == CheckStatus::Passed
    }

    /// A remote result: the status its evidence derives, the evidence artifact, the kept log
    /// excerpt when there is one, and nothing a local command would have produced.
    pub fn remote(
        definition: &CheckDefinition,
        status: CheckStatus,
        reason: Option<String>,
        evidence_id: String,
        log: Option<String>,
    ) -> CheckResult {
        CheckResult {
            name: definition.name.clone(),
            status,
            exit_code: None,
            reason,
            program: None,
            args: Vec::new(),
            stdout: log,
            stderr: None,
            required: definition.required,
            remote: Some(evidence_id),
        }
    }

    /// Whether this result is exactly one of the two shapes: local (no `remote`) or remote
    /// (`remote`, and no program, exit code, arguments or `stderr`; a log excerpt as `stdout`
    /// only when the check did not pass). A mixture is neither.
    pub fn has_one_shape(&self) -> bool {
        self.remote.is_none()
            || (self.program.is_none()
                && self.exit_code.is_none()
                && self.stderr.is_none()
                && self.args.is_empty()
                && (self.stdout.is_none() || self.status != CheckStatus::Passed))
    }
}

/// Runs checks against a materialized tree.
///
/// Deliberately absent from the record: elapsed time. Nothing in any policy reads it, and its
/// presence would make an otherwise reproducible artifact differ on every run. Callers that
/// need the host clock get it beside the record, in [`CheckExecution`].
pub struct CheckRunner<'a> {
    cas: &'a Cas,
    workdir: PathBuf,
    /// Environment handed to a host-local check. Cleared and rebuilt, like the git adapter's.
    local_env: Vec<(String, String)>,
    /// Host-independent environment safe to add on top of a pinned execution image. In
    /// particular, the host PATH must never replace the image's own toolchain PATH.
    portable_env: Vec<(String, String)>,
    /// A check that never returns must not hang the whole review — the gate runs first and the
    /// scheduler blocks on its completion. A check past this deadline is killed and recorded
    /// `not_run`, exactly as an unstartable one is. Generous by default (an engine build+test
    /// is legitimately long); a pipeline may tighten it.
    timeout: std::time::Duration,
    cancellation: Option<&'a std::sync::atomic::AtomicBool>,
}

impl<'a> CheckRunner<'a> {
    pub fn new(cas: &'a Cas, workdir: impl AsRef<Path>) -> CheckRunner<'a> {
        CheckRunner {
            cas,
            workdir: workdir.as_ref().to_path_buf(),
            local_env: vec![
                (
                    "PATH".to_string(),
                    std::env::var("PATH").unwrap_or_default(),
                ),
                ("LC_ALL".to_string(), "C".to_string()),
                ("TZ".to_string(), "UTC".to_string()),
            ],
            portable_env: vec![
                ("LC_ALL".to_string(), "C".to_string()),
                ("TZ".to_string(), "UTC".to_string()),
            ],
            timeout: std::time::Duration::from_secs(3600),
            cancellation: None,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let entry = (key.into(), value.into());
        self.local_env.push(entry.clone());
        self.portable_env.push(entry);
        self
    }

    /// Add execution-location-specific values for one semantic environment variable. Cache
    /// roots are absolute host paths for trusted-local checks and fixed `/work` paths inside a
    /// container, so treating them as one portable value would break one provider or the other.
    pub fn with_split_env(
        mut self,
        key: impl Into<String>,
        local_value: impl Into<String>,
        portable_value: impl Into<String>,
    ) -> Self {
        let key = key.into();
        self.local_env.push((key.clone(), local_value.into()));
        self.portable_env.push((key, portable_value.into()));
        self
    }

    /// The exact environment a host-local check receives, in the order it is applied. A caller
    /// that must run a probe under the check's own environment reads it here rather than
    /// rebuilding it.
    pub fn local_environment(&self) -> &[(String, String)] {
        &self.local_env
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cancellation(
        mut self,
        cancellation: Option<&'a std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Run one check. Never panics and never propagates a spawn failure as an error: a check
    /// that could not start is a *result*, because losing it would be the same as passing it.
    pub fn run(&self, definition: &CheckDefinition) -> CheckResult {
        self.run_traced(definition).0
    }

    /// [`Self::run`], with how the process ended beside the recorded result.
    pub fn run_traced(&self, definition: &CheckDefinition) -> (CheckResult, CheckEnding) {
        let base = base_result(definition);
        let argv = match resolved_args(definition, &base) {
            Ok(argv) => argv,
            Err(result) => return (*result, CheckEnding::NotStarted),
        };

        let mut cmd = std::process::Command::new(&definition.command.program);
        cmd.args(&argv);
        cmd.current_dir(&self.workdir);
        cmd.env_clear();
        for (key, value) in &self.local_env {
            cmd.env(key, value);
        }
        let (output, stderr_held) = match self.run_with_deadline(&mut cmd) {
            RunResult::Completed {
                output,
                stderr_held,
            } => (output, stderr_held),
            RunResult::Interrupted {
                reason,
                stdout,
                stderr,
                ending,
            } => {
                return (
                    CheckResult {
                        reason: Some(reason),
                        stdout: self.cas.put(&stdout).ok(),
                        stderr: self.cas.put(&stderr).ok(),
                        ..base
                    },
                    ending,
                );
            }
            RunResult::TimedOut => {
                return (
                    CheckResult {
                        reason: Some(format!(
                            "no result within {}s; the check was killed",
                            self.timeout.as_secs()
                        )),
                        ..base
                    },
                    CheckEnding::TimedOut,
                );
            }
            RunResult::CouldNotStart(error) => {
                return (
                    CheckResult {
                        reason: Some(format!(
                            "could not start `{}`: {error}",
                            definition.command.program
                        )),
                        ..base
                    },
                    CheckEnding::NotStarted,
                );
            }
        };

        let ending = if output.status.code().is_some() {
            CheckEnding::Exited
        } else {
            CheckEnding::Signaled
        };
        (self.finish(base, output, stderr_held), ending)
    }

    /// Execute through the normal shared runner and retain the actual AF-observed host interval.
    pub fn run_observed(&self, definition: &CheckDefinition) -> CheckExecution {
        let started = SystemTime::now();
        let timer = Instant::now();
        let (result, ending) = self.run_traced(definition);
        CheckExecution {
            result,
            started_unix_ms: started
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as u64),
            elapsed_ms: timer.elapsed().as_millis() as u64,
            ending,
        }
    }

    /// Run one typed check through an admitted external execution provider. Command resolution
    /// remains owned here, so a container route cannot bypass argument provenance validation;
    /// the provider owns only where the already-resolved program executes.
    pub fn run_with<F>(&self, definition: &CheckDefinition, execute: F) -> CheckResult
    where
        F: FnOnce(
            &str,
            &[String],
            &[(String, String)],
            std::time::Duration,
        ) -> Result<(std::process::Output, bool), String>,
    {
        let base = base_result(definition);
        if self
            .cancellation
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return CheckResult {
                reason: Some("check cancelled before execution".into()),
                ..base
            };
        }

        let argv = match resolved_args(definition, &base) {
            Ok(argv) => argv,
            Err(result) => return *result,
        };
        match execute(
            &definition.command.program,
            &argv,
            &self.portable_env,
            self.timeout,
        ) {
            Ok((output, stderr_held)) => self.finish(base, output, stderr_held),
            Err(error) => CheckResult {
                reason: Some(format!("execution provider refused or failed: {error}")),
                ..base
            },
        }
    }

    /// Container/provider counterpart to [`Self::run_observed`]. The interval covers only the
    /// admitted check invocation; provider-internal phases remain unknown.
    pub fn run_with_observed<F>(&self, definition: &CheckDefinition, execute: F) -> CheckExecution
    where
        F: FnOnce(
            &str,
            &[String],
            &[(String, String)],
            std::time::Duration,
        ) -> Result<(std::process::Output, bool), String>,
    {
        let started = SystemTime::now();
        let timer = Instant::now();
        let result = self.run_with(definition, execute);
        // A provider reports no typed ending: a result without an exit code never ran as far
        // as this runner can tell, and everything else exited.
        let ending = if result.status == CheckStatus::NotRun && result.exit_code.is_none() {
            CheckEnding::NotStarted
        } else {
            CheckEnding::Exited
        };
        CheckExecution {
            result,
            started_unix_ms: started
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as u64),
            elapsed_ms: timer.elapsed().as_millis() as u64,
            ending,
        }
    }

    fn finish(
        &self,
        base: CheckResult,
        output: std::process::Output,
        stderr_held: bool,
    ) -> CheckResult {
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

        if let Some(reason) = stderr_held_reason(stderr_held) {
            return CheckResult {
                status: CheckStatus::NotRun,
                exit_code: None,
                reason: Some(reason.to_string()),
                stdout,
                stderr,
                ..base
            };
        }

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
}

fn base_result(definition: &CheckDefinition) -> CheckResult {
    CheckResult {
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
        remote: None,
    }
}

fn resolved_args(
    definition: &CheckDefinition,
    base: &CheckResult,
) -> Result<Vec<String>, Box<CheckResult>> {
    definition.command.resolve().map_err(|error| {
        Box::new(CheckResult {
            reason: Some(describe(&error)),
            ..base.clone()
        })
    })
}

fn stderr_held_reason(stderr_held: bool) -> Option<&'static str> {
    stderr_held.then_some(
        "stderr evidence was not preserved: a descendant held the pipe past the drain grace",
    )
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
    NewEvent::new(EventType::CheckCompletedV1, payload)
        .node(node_id)
        .correlating(result.name.clone())
        .referencing(refs)
}

enum RunResult {
    Interrupted {
        reason: String,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        ending: CheckEnding,
    },
    Completed {
        output: std::process::Output,
        stderr_held: bool,
    },
    TimedOut,
    CouldNotStart(std::io::Error),
}

impl CheckRunner<'_> {
    /// Use the shared process supervisor with the check-specific exit policy: a successful check
    /// is over when its leader exits, so background descendants are reaped immediately rather
    /// than being allowed to hold evidence pipes open.
    fn run_with_deadline(&self, cmd: &mut std::process::Command) -> RunResult {
        if let Some(flag) = self.cancellation {
            let captured = review_process::run_supervised_captured_cancellable_with_policy(
                cmd,
                None,
                self.timeout,
                ExitPolicy::KillProcessGroup,
                flag,
            );
            return match captured.status {
                Ok(status) => RunResult::Completed {
                    output: std::process::Output {
                        status,
                        stdout: captured.stdout,
                        stderr: captured.stderr,
                    },
                    stderr_held: captured.stderr_held,
                },
                Err(error) => RunResult::Interrupted {
                    ending: match &error {
                        SupervisedError::TimedOut { .. } => CheckEnding::TimedOut,
                        SupervisedError::Cancelled => CheckEnding::Cancelled,
                        SupervisedError::Spawn(_) => CheckEnding::NotStarted,
                        _ => CheckEnding::Exited,
                    },
                    reason: error.to_string(),
                    stdout: captured.stdout,
                    stderr: captured.stderr,
                },
            };
        }

        match run_supervised_with_policy(cmd, None, self.timeout, ExitPolicy::KillProcessGroup) {
            Ok(output) => RunResult::Completed {
                stderr_held: output.stderr_held,
                output: std::process::Output {
                    status: output.status,
                    stdout: output.stdout,
                    stderr: output.stderr,
                },
            },
            Err(SupervisedError::TimedOut { .. }) => RunResult::TimedOut,
            Err(SupervisedError::Spawn(error)) => RunResult::CouldNotStart(error),
            Err(error) => RunResult::CouldNotStart(std::io::Error::other(error)),
        }
    }
}

fn describe(error: &ArgError) -> String {
    format!("refused before execution: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckDefinition, Command};
    use review_store::Cas;

    #[test]
    fn held_stderr_is_explicitly_unverifiable() {
        assert!(
            super::stderr_held_reason(true)
                .unwrap()
                .contains("evidence was not preserved")
        );
        assert_eq!(super::stderr_held_reason(false), None);
    }

    #[test]
    fn an_external_provider_gets_only_portable_and_explicit_environment() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let runner =
            CheckRunner::new(&cas, directory.path()).with_env("CARGO_TARGET_DIR", "/work/.target");
        let check = CheckDefinition::new("probe", Command::new("/bin/true", vec![]));

        let result = runner.run_with(&check, |_, _, environment, _| {
            assert_eq!(
                environment,
                [
                    ("LC_ALL".to_string(), "C".to_string()),
                    ("TZ".to_string(), "UTC".to_string()),
                    ("CARGO_TARGET_DIR".to_string(), "/work/.target".to_string()),
                ]
            );
            assert!(environment.iter().all(|(key, _)| key != "PATH"));
            Err("assertion provider stops here".to_string())
        });
        assert_eq!(result.status, CheckStatus::NotRun);
    }
}
