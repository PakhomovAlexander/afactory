//! The Codex adapter: a digest-pinned reviewer package driving `codex exec`.
//!
//! Everything the model sees comes from the package: `reviewer.md` is the prompt, the
//! manifest's runner args are the model flags, and both sit under the lockfile's content
//! digest — so "which reviewer ran" is a verifiable claim, not a deployment accident.
//!
//! The invocation shape (captured from `codex-cli 0.147.0` and pinned by the fixtures in
//! `tests/`): `codex exec --ephemeral --skip-git-repo-check --json -C <sandbox> -s
//! workspace-write -o <staging>/last-message <flags> -`, with the prompt streamed on stdin.
//! Events arrive as JSONL on
//! stdout; the final agent message is the reviewer's answer; `turn.completed` events carry
//! token usage. `--ephemeral` keeps session files off the host, `-C` roots the model in the
//! kernel's sandbox, and `workspace-write` matches `Mode::EphemeralWrite` — the reviewer may
//! edit its own copy and nothing else.
//!
//! Failure classification is an accounting decision: a failed exec that *reported usage*
//! spent real tokens and surfaces as `Failed` (the kernel charges); one that reported none —
//! at capacity, auth refused, never reached a model — is `Unavailable` (the kernel releases).
//!
//! Chargeable usage is uncached input plus output. Codex includes cache reads in
//! `input_tokens` but also reports `cached_input_tokens`, so subtracting the latter makes the
//! budget unit match the Claude adapter instead of charging one repeatedly cached context as
//! if it were fresh on every agent turn.

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::{
    ModelRunner, RESULT_CONTRACT, ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError,
    parse_stage_output,
};
use review_store::Cas;

pub struct CodexAdapter {
    program: String,
    model_flags: Vec<String>,
    prompt: String,
    timeout: Duration,
    /// Where codex finds its credentials. Granted to the child explicitly — the supervisor
    /// rebuilds the environment, so nothing is inherited by accident.
    codex_home: Option<String>,
}

impl CodexAdapter {
    /// Build from a digest-verified package. The manifest must name `codex` (any path with
    /// that basename, so tests can point at a stub); its args become model flags; the prompt
    /// is the package's `reviewer.md` plus the result contract.
    pub fn from_package(
        package: &ResolvedReviewer,
        timeout: Duration,
    ) -> Result<CodexAdapter, String> {
        let basename = Path::new(&package.runner.program)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if basename != "codex" {
            return Err(format!(
                "package `{}` names runner `{}`; this adapter drives codex",
                package.name, package.runner.program
            ));
        }
        let model_flags = package
            .runner
            .resolve()
            .map_err(|e| format!("package `{}` runner args: {e}", package.name))?;
        // From the verified bytes, never a fresh disk read: the prompt is the one file that
        // decides what the reviewer does, so it must be exactly what the digest covered.
        let prompt = String::from_utf8(
            package
                .file("reviewer.md")
                .ok_or_else(|| format!("package `{}` has no reviewer.md", package.name))?
                .to_vec(),
        )
        .map_err(|_| format!("package `{}`: reviewer.md is not UTF-8", package.name))?;
        Ok(CodexAdapter {
            program: package.runner.program.clone(),
            model_flags,
            prompt: format!("{prompt}{RESULT_CONTRACT}"),
            timeout,
            codex_home: None,
        })
    }

    pub fn with_codex_home(mut self, home: impl Into<String>) -> Self {
        self.codex_home = Some(home.into());
        self
    }

    /// Narrow this invocation's attention. A narrowing only — the package prompt still
    /// governs; this cannot grant anything the package did not.
    pub fn with_focus(mut self, focus: impl AsRef<str>) -> Self {
        self.prompt = format!(
            "{}\n\n## Focus for this run\n\n{}",
            self.prompt,
            focus.as_ref()
        );
        self
    }
}

impl ReviewerAdapter for CodexAdapter {
    fn invoke(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        // The last-message file lives outside the sandbox: seal must never see the plumbing.
        let staging = tempfile::tempdir()
            .map_err(|e| RunnerError::Unavailable(format!("staging dir: {e}")))?;
        let last_message = staging.path().join("last-message");

        let mut args = vec![
            Arg::literal("exec"),
            Arg::literal("--ephemeral"),
            Arg::literal("--skip-git-repo-check"),
            Arg::literal("--json"),
            Arg::literal("-C"),
            Arg::literal(sandbox_root.display().to_string()),
            Arg::literal("-s"),
            Arg::literal("workspace-write"),
            Arg::literal("-o"),
            Arg::literal(last_message.display().to_string()),
        ];
        args.extend(self.model_flags.iter().map(Arg::literal));
        // The package prompt, then this attempt's labelled inputs — data the kernel resolved,
        // rendered under an explicit heading rather than woven into the instructions.
        let inputs = inputs.render().map_err(RunnerError::Refused)?;
        args.push(Arg::literal("-"));
        let prompt = format!("{}{}", self.prompt, inputs);
        let command = Command::new(&self.program, args);

        let mut runner = ModelRunner::new(sandbox_root, self.timeout);
        if let Some(home) = &self.codex_home {
            runner = runner.with_grant("CODEX_HOME", home);
        }
        let capture = runner.capture_with_stdin(cas, &command, prompt.as_bytes())?;

        let events = Events::parse(&capture.stdout);
        if !capture.status.success() {
            let message = events.error.unwrap_or_else(|| {
                String::from_utf8_lossy(&capture.stderr)
                    .lines()
                    .last()
                    .unwrap_or("codex exec failed with no error event")
                    .to_string()
            });
            // Usage reported means tokens were spent: Failed, and the kernel charges. None
            // reported means the model was never reached: Unavailable, and the kernel
            // releases the reservation.
            return Err(if events.cost_tokens > 0 {
                RunnerError::Failed {
                    exit_code: capture.status.code().unwrap_or(-1),
                    stderr_excerpt: message,
                }
            } else {
                RunnerError::Unavailable(message)
            });
        }

        // The -o file is the authoritative final message; the agent_message event is the
        // fallback when a stub or an older CLI omits the file.
        let answer = std::fs::read_to_string(&last_message)
            .ok()
            .filter(|text| !text.trim().is_empty())
            .or(events.final_message)
            .ok_or_else(|| {
                RunnerError::MalformedOutput(
                    "codex exec succeeded but produced no final message".to_string(),
                )
            })?;

        let output = parse_stage_output(&answer).map_err(|e| {
            RunnerError::MalformedOutput(format!(
                "{e}; the raw stream is stored as {}",
                capture.raw_artifact
            ))
        })?;
        Ok(ReviewerReturn {
            output,
            cost_tokens: events.cost_tokens,
            raw_artifact: capture.raw_artifact,
        })
    }
}

#[derive(Default)]
struct Events {
    cost_tokens: u64,
    final_message: Option<String>,
    error: Option<String>,
}

impl Events {
    /// Fold the JSONL stream. Unknown event types are ignored — the CLI adds kinds freely —
    /// but the three that matter are pinned by fixtures captured from a real run.
    fn parse(stdout: &[u8]) -> Events {
        let mut events = Events::default();
        for line in stdout.split(|b| *b == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            match value.get("type").and_then(|t| t.as_str()) {
                Some("turn.completed") => {
                    if let Some(usage) = value.get("usage") {
                        let count =
                            |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                        events.cost_tokens += count("input_tokens")
                            .saturating_sub(count("cached_input_tokens"))
                            + count("output_tokens");
                    }
                }
                Some("item.completed") => {
                    if let Some(item) = value.get("item")
                        && item.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                        && let Some(text) = item.get("text").and_then(|t| t.as_str())
                    {
                        events.final_message = Some(text.to_string());
                    }
                }
                Some("error") | Some("turn.failed") => {
                    let message = value
                        .get("message")
                        .or_else(|| value.get("error").and_then(|e| e.get("message")))
                        .and_then(|m| m.as_str());
                    if let Some(message) = message {
                        events.error = Some(message.to_string());
                    }
                }
                _ => {}
            }
        }
        events
    }
}
