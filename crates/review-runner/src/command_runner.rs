//! The `command` reviewer adapter.
//!
//! Runs a trusted program and reads a `ReviewerResult@1` from its stdout. Every way that can go
//! wrong is a typed outcome rather than an exception or, worse, an empty result: a reviewer that
//! crashed and a reviewer that found nothing must never be indistinguishable, because one of
//! them means the change was reviewed and the other does not.

use review_core::Command;
use review_core::LegacyStageOutput;
use review_store::Cas;
use std::process::Stdio;

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
    MalformedOutput(String),
    /// The reviewer did not answer by its deadline and was killed. Whatever it spent is gone;
    /// whether to retry is the kernel's decision, not this layer's.
    TimedOut { after_ms: u64 },
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
            RunnerError::MalformedOutput(why) => {
                write!(f, "reviewer output is not a ReviewerResult@1: {why}")
            }
            RunnerError::TimedOut { after_ms } => {
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
}

impl<'a> CommandRunner<'a> {
    pub fn new(cas: &'a Cas, workdir: impl AsRef<std::path::Path>) -> CommandRunner<'a> {
        CommandRunner {
            cas,
            workdir: workdir.as_ref().to_path_buf(),
        }
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
        let argv = command
            .resolve()
            .map_err(|e| RunnerError::Refused(e.to_string()))?;

        let mut cmd = std::process::Command::new(&command.program);
        cmd.args(&argv);
        cmd.current_dir(&self.workdir);
        cmd.env_clear();
        cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
        cmd.env("LC_ALL", "C");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = cmd
            .output()
            .map_err(|e| RunnerError::Unavailable(format!("{}: {e}", command.program)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RunnerError::Failed {
                exit_code: output.status.code().unwrap_or(-1),
                stderr_excerpt: stderr.lines().last().unwrap_or_default().to_string(),
            });
        }

        // The raw answer is stored before it is parsed: if the parse fails, the bytes that
        // failed must still be inspectable. Losing them would leave "malformed output" as an
        // unfalsifiable claim.
        let raw_artifact = self
            .cas
            .put(&output.stdout)
            .map_err(|e| RunnerError::Unavailable(format!("storing raw output: {e}")))?;

        serde_json::from_slice::<LegacyStageOutput>(&output.stdout)
            .map(|parsed| (parsed, raw_artifact))
            .map_err(|e| RunnerError::MalformedOutput(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::Arg;

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
            Err(RunnerError::MalformedOutput(_))
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
