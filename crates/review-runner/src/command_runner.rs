//! The `command` reviewer adapter.
//!
//! Runs a trusted program and reads a `ReviewerResult@1` from its stdout. Every way that can go
//! wrong is a typed outcome rather than an exception or, worse, an empty result: a reviewer that
//! crashed and a reviewer that found nothing must never be indistinguishable, because one of
//! them means the change was reviewed and the other does not.

use review_core::Command;
use review_core::{LegacyStageOutput, ReviewerResultContract};
use review_store::Cas;
use std::time::Duration;

use review_process::{SupervisedError, run_supervised};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(1_800);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// The command was refused before execution — an untrusted value in an option position.
    Refused(String),
    /// The program could not be started at all.
    Unavailable(String),
    /// The reviewer ran and failed.
    Failed {
        exit_code: i32,
        stderr_excerpt: String,
    },
    /// The reviewer ran, succeeded, and returned something that is not a result.
    MalformedOutput { raw_artifact: String, why: String },
    /// The reviewer did not answer by its deadline and was killed. Whatever it spent is gone;
    /// whether to retry is the kernel's decision, not this layer's.
    TimedOut {
        after_ms: u64,
        raw_artifact: Option<String>,
    },
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::Refused(why) => write!(f, "reviewer command refused: {why}"),
            RunnerError::Unavailable(why) => write!(f, "reviewer unavailable: {why}"),
            RunnerError::Failed {
                exit_code,
                stderr_excerpt,
            } => write!(f, "reviewer failed (exit {exit_code}): {stderr_excerpt}"),
            RunnerError::MalformedOutput { why, .. } => {
                write!(f, "reviewer output is not a ReviewerResult@1: {why}")
            }
            RunnerError::TimedOut { after_ms, .. } => {
                write!(
                    f,
                    "reviewer did not answer within {after_ms}ms and was killed"
                )
            }
        }
    }
}

impl std::error::Error for RunnerError {}

pub struct CommandRunner<'a> {
    cas: &'a Cas,
    workdir: std::path::PathBuf,
    timeout: Duration,
}

impl<'a> CommandRunner<'a> {
    pub fn new(cas: &'a Cas, workdir: impl AsRef<std::path::Path>) -> CommandRunner<'a> {
        CommandRunner {
            cas,
            workdir: workdir.as_ref().to_path_buf(),
            timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }

    /// Override the default bounded command-reviewer deadline.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Invoke a reviewer. Deterministic by construction: the same program over the same inputs
    /// returns the same result, which is what lets the scheduler's properties be proved without
    /// a model in the loop.
    pub fn invoke(&self, command: &Command) -> Result<LegacyStageOutput, RunnerError> {
        self.invoke_raw(command).map(|(output, _)| output)
    }

    /// [`invoke`](Self::invoke), also returning the CAS id of the raw answer — the receipt a
    /// reviewer adapter records so "what did it actually say" never needs re-running anything.
    pub fn invoke_raw(
        &self,
        command: &Command,
    ) -> Result<(LegacyStageOutput, String), RunnerError> {
        self.invoke_raw_inner(command, None, ReviewerResultContract::V1)
    }

    /// Invoke a command reviewer with the exact serialized `ReviewerInputs` document on stdin.
    pub fn invoke_raw_with_input(
        &self,
        command: &Command,
        input: Vec<u8>,
    ) -> Result<(LegacyStageOutput, String), RunnerError> {
        self.invoke_raw_inner(command, Some(input), ReviewerResultContract::V1)
    }

    pub fn invoke_raw_with_input_for(
        &self,
        command: &Command,
        input: Vec<u8>,
        contract: ReviewerResultContract,
    ) -> Result<(LegacyStageOutput, String), RunnerError> {
        self.invoke_raw_inner(command, Some(input), contract)
    }

    fn invoke_raw_inner(
        &self,
        command: &Command,
        input: Option<Vec<u8>>,
        contract: ReviewerResultContract,
    ) -> Result<(LegacyStageOutput, String), RunnerError> {
        let argv = command
            .resolve()
            .map_err(|e| RunnerError::Refused(e.to_string()))?;

        let mut cmd = std::process::Command::new(&command.program);
        cmd.args(&argv);
        cmd.current_dir(&self.workdir);
        cmd.env_clear();
        cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
        cmd.env("LC_ALL", "C");
        let output =
            run_supervised(&mut cmd, input, self.timeout).map_err(|error| match error {
                SupervisedError::TimedOut { stdout, .. } => RunnerError::TimedOut {
                    after_ms: self.timeout.as_millis() as u64,
                    raw_artifact: self.cas.put(&stdout).ok(),
                },
                SupervisedError::Spawn(error) => {
                    RunnerError::Unavailable(format!("{}: {error}", command.program))
                }
                error => RunnerError::Failed {
                    exit_code: -1,
                    stderr_excerpt: error.to_string(),
                },
            })?;
        let status = output.status;
        let stdout = output.stdout;
        let mut stderr = output.stderr;
        if output.stderr_held {
            stderr.extend_from_slice(b"\nstderr was still held after 5 seconds\n");
        }

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            return Err(RunnerError::Failed {
                exit_code: status.code().unwrap_or(-1),
                stderr_excerpt: stderr.lines().last().unwrap_or_default().to_string(),
            });
        }

        // The raw answer is stored before it is parsed: if the parse fails, the bytes that
        // failed must still be inspectable. Losing them would leave "malformed output" as an
        // unfalsifiable claim.
        let raw_artifact = self
            .cas
            .put(&stdout)
            .map_err(|e| RunnerError::Unavailable(format!("storing raw output: {e}")))?;

        match std::str::from_utf8(&stdout)
            .map_err(|error| error.to_string())
            .and_then(|text| crate::parse_stage_output_for(contract, text))
        {
            Ok(parsed) => Ok((parsed, raw_artifact)),
            Err(error) => Err(RunnerError::MalformedOutput {
                raw_artifact,
                why: error.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::Arg;
    use std::time::Instant;

    fn runner_dir() -> (tempfile::TempDir, Cas) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        (dir, cas)
    }

    fn emitting(json: &str) -> Command {
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal(format!("cat <<'EOF'\n{json}\nEOF")),
            ],
        )
    }

    const EMPTY_RESULT: &str = r#"{"verdict":"approve","summary":null,"findings":[],
        "benchmark_demands":[],"disputes":[]}"#;

    #[test]
    fn a_well_formed_result_parses() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        let result = runner.invoke(&emitting(EMPTY_RESULT)).unwrap();
        assert!(result.findings.is_empty());
    }

    #[test]
    fn a_reviewer_may_ignore_wired_inputs() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        // Larger than an OS pipe, so a command that never reads stdin closes it while the
        // parent is still writing. Its valid answer must win over that expected EPIPE.
        let input = vec![b'x'; 1024 * 1024];
        let (result, _) = runner
            .invoke_raw_with_input(&emitting(EMPTY_RESULT), input)
            .unwrap();
        assert!(result.findings.is_empty());
    }

    #[test]
    fn large_stdin_and_stderr_are_drained_concurrently() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path()).with_timeout(Duration::from_secs(5));
        let command = Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal(format!(
                    "head -c 1048576 /dev/zero >&2; cat >/dev/null; cat <<'EOF'\n{EMPTY_RESULT}\nEOF"
                )),
            ],
        );
        let input = vec![b'x'; 1024 * 1024];
        let (result, _) = runner.invoke_raw_with_input(&command, input).unwrap();
        assert!(result.findings.is_empty());
    }

    #[test]
    fn a_hung_command_reviewer_is_killed_at_its_deadline() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path()).with_timeout(Duration::from_millis(100));
        let command = Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("printf partial; sleep 60")],
        );
        let started = Instant::now();
        let Err(RunnerError::TimedOut {
            raw_artifact: Some(raw_artifact),
            ..
        }) = runner.invoke_raw(&command)
        else {
            panic!("command timeout did not retain partial output");
        };
        assert_eq!(cas.get(&raw_artifact).unwrap(), b"partial");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_parent_exit_with_a_lingering_descendant_obeys_the_deadline() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path()).with_timeout(Duration::from_millis(100));
        let command = Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                // A non-interactive POSIX shell may redirect an asynchronous command's fd 0 to
                // /dev/null before applying `<&0`. Preserve the original input pipe on fd 3 in
                // the parent first, so the descendant definitely keeps the writer blocked.
                Arg::literal(format!(
                    "exec 3<&0; sleep 2 <&3 & cat <<'EOF'\n{EMPTY_RESULT}\nEOF"
                )),
            ],
        );
        let started = Instant::now();
        // Linux may grow a pipe to 1 MiB, so the old 1 MiB input could fit completely and let
        // the writer finish before the descendant released stdin. Exceed that ceiling so this
        // test actually exercises the post-exit writer deadline on every supported platform.
        let input = vec![b'x'; 16 * 1024 * 1024];
        assert!(matches!(
            runner.invoke_raw_with_input(&command, input),
            Err(RunnerError::TimedOut { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn post_exit_drain_preserves_a_large_valid_answer() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path()).with_timeout(Duration::from_secs(5));
        let command = Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal(
                    "sleep 1 & printf '%s' '{\"verdict\":\"approve\",\"summary\":\"'; \
                     head -c 1048576 /dev/zero | tr '\\000' x; \
                     printf '%s' '\",\"findings\":[],\"benchmark_demands\":[],\"disputes\":[]}'",
                ),
            ],
        );

        let (result, raw) = runner.invoke_raw(&command).unwrap();
        assert_eq!(result.summary.as_deref().map(str::len), Some(1024 * 1024));
        assert_ne!(raw, review_store::canonical::blob_content_id(b""));
    }

    #[test]
    fn a_crash_is_not_an_empty_result() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        let command = Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("echo boom >&2; exit 3")],
        );
        match runner.invoke(&command) {
            Err(RunnerError::Failed {
                exit_code,
                stderr_excerpt,
            }) => {
                assert_eq!(exit_code, 3);
                assert_eq!(stderr_excerpt, "boom");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_reviewer_is_unavailable_not_silent() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        let command = Command::new("/nonexistent/reviewer", vec![]);
        assert!(matches!(
            runner.invoke(&command),
            Err(RunnerError::Unavailable(_))
        ));
    }

    #[test]
    fn malformed_output_keeps_the_bytes_that_failed() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        let command = Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("echo 'not json at all'")],
        );
        assert!(matches!(
            runner.invoke(&command),
            Err(RunnerError::MalformedOutput { .. })
        ));
        assert!(
            cas.contains(&review_store::canonical::blob_content_id(
                b"not json at all\n"
            )),
            "the unparseable answer must remain inspectable"
        );
    }

    #[test]
    fn an_untrusted_option_is_refused_before_the_reviewer_starts() {
        let (dir, cas) = runner_dir();
        let runner = CommandRunner::new(&cas, dir.path());
        let command = Command::new("/bin/sh", vec![Arg::untrusted("--exec=evil")]);
        assert!(matches!(
            runner.invoke(&command),
            Err(RunnerError::Refused(_))
        ));
    }
}
