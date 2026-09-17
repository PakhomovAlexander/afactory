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

pub mod task;

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::{
    InputTransport, ModelRunner, ReceiptedReviewerReturn, RenderedInput, ReviewerAdapter,
    ReviewerInputs, ReviewerReturn, RunnerError, TokenUsage, compose_model_prompt,
    parse_notes_declaration, parse_proposal_declaration, parse_stage_output_for,
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
        let model_flags = codex_model_flags(&package.runner)
            .map_err(|error| format!("package `{}` runner args: {error}", package.name))?;
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
            prompt,
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

/// Build the adapter-owned capability smoke invocation. The production builder supplies the
/// same global/subcommand ordering, with a read-only scratch root and no output file.
pub fn smoke_command(runner: &Command, scratch: &Path) -> Result<Command, String> {
    let model_flags = codex_model_flags(runner)?;
    Ok(codex_command(
        &runner.program,
        &model_flags,
        scratch,
        "read-only",
        None,
    ))
}

fn codex_model_flags(runner: &Command) -> Result<Vec<String>, String> {
    let values = runner.resolve().map_err(|error| error.to_string())?;
    let mut model = None;
    let mut effort = None;
    let mut index = 0;
    while index < values.len() {
        let option = &values[index];
        let value = values
            .get(index + 1)
            .ok_or_else(|| format!("Codex model option `{option}` has no value"))?;
        match option.as_str() {
            "--model" | "-m" => {
                if model.replace(value.clone()).is_some() {
                    return Err("duplicate Codex package model option".into());
                }
            }
            "-c" if value.starts_with("model_reasoning_effort=") => {
                if effort.replace(value.clone()).is_some() {
                    return Err("duplicate Codex package reasoning-effort option".into());
                }
            }
            _ => {
                return Err(format!(
                    "unsupported Codex package argument `{option}`; packages may set only one model and one model_reasoning_effort"
                ));
            }
        }
        index += 2;
    }
    let mut flags = Vec::new();
    if let Some(model) = model {
        flags.extend(["--model".into(), model]);
    }
    if let Some(effort) = effort {
        flags.extend(["-c".into(), effort]);
    }
    Ok(flags)
}

fn codex_command(
    program: &str,
    model_flags: &[String],
    sandbox_root: &Path,
    mode: &str,
    output: Option<&Path>,
) -> Command {
    let mut args = vec![
        Arg::literal("exec"),
        Arg::literal("--ephemeral"),
        Arg::literal("--skip-git-repo-check"),
        Arg::literal("--json"),
        Arg::literal("-C"),
        Arg::literal(sandbox_root.display().to_string()),
        Arg::literal("-s"),
        Arg::literal(mode),
    ];
    if let Some(output) = output {
        args.extend([
            Arg::literal("-o"),
            Arg::literal(output.display().to_string()),
        ]);
    }
    args.extend(model_flags.iter().map(Arg::literal));
    args.push(Arg::literal("-"));
    Command::new(program, args)
}

impl ReviewerAdapter for CodexAdapter {
    fn credential_mode(&self) -> review_runner::BrokerCredentialModeV1 {
        review_runner::BrokerCredentialModeV1::TrustedUnsafe
    }

    fn invoke(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        self.invoke_receipted(cas, sandbox_root, inputs)
            .map(|receipt| receipt.returned)
    }

    fn render_input(&self, inputs: &ReviewerInputs) -> Result<Option<RenderedInput>, RunnerError> {
        let (prompt, manifest) =
            compose_model_prompt(&self.prompt, inputs).map_err(RunnerError::Refused)?;
        Ok(Some(RenderedInput {
            transport: InputTransport::Prompt,
            bytes: prompt.into_bytes(),
            manifest,
        }))
    }

    fn invoke_receipted(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReceiptedReviewerReturn, RunnerError> {
        // The last-message file lives outside the sandbox: seal must never see the plumbing.
        let staging = tempfile::tempdir()
            .map_err(|e| RunnerError::Unavailable(format!("staging dir: {e}")))?;
        let last_message = staging.path().join("last-message");

        // The package prompt, then this attempt's labelled inputs — data the kernel resolved,
        // rendered under an explicit heading rather than woven into the instructions. The same
        // pure composition backs `render_input`, so what is sent is what can be audited.
        let (prompt, context_manifest) =
            compose_model_prompt(&self.prompt, inputs).map_err(RunnerError::Refused)?;
        let command = codex_command(
            &self.program,
            &self.model_flags,
            sandbox_root,
            "workspace-write",
            Some(&last_message),
        );

        let mut runner = ModelRunner::new(sandbox_root, self.timeout);
        if let Some(home) = &self.codex_home {
            runner = runner.with_grant("CODEX_HOME", home);
        }
        let capture = runner.capture_with_stdin(cas, &command, prompt.into_bytes())?;

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
            .ok_or_else(|| RunnerError::MalformedOutput {
                raw_artifact: capture.raw_artifact.clone(),
                why: "codex exec succeeded but produced no final message".into(),
            })?;

        let output = parse_stage_output_for(inputs.result_contract, &answer).map_err(|e| {
            RunnerError::MalformedOutput {
                raw_artifact: capture.raw_artifact.clone(),
                why: e.to_string(),
            }
        })?;
        let proposal = parse_proposal_declaration(&answer);
        let notes = parse_notes_declaration(&answer);
        Ok(ReceiptedReviewerReturn {
            returned: ReviewerReturn {
                output,
                proposal,
                notes,
                cost_tokens: events.cost_tokens,
                raw_artifact: capture.raw_artifact,
            },
            usage: events.usage,
            context_manifest,
        })
    }
}

#[derive(Default)]
struct Events {
    cost_tokens: u64,
    usage: TokenUsage,
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
                        let input = count("input_tokens");
                        let cache_read = count("cached_input_tokens");
                        let output = count("output_tokens");
                        let reasoning = count("reasoning_output_tokens");
                        let cache_write = count("cache_write_input_tokens");
                        let chargeable = input.saturating_sub(cache_read).saturating_add(output);
                        events.cost_tokens = events.cost_tokens.saturating_add(chargeable);
                        add_usage(&mut events.usage.input_tokens, input);
                        add_usage(&mut events.usage.output_tokens, output);
                        add_usage(&mut events.usage.cache_read_tokens, cache_read);
                        add_usage(&mut events.usage.cache_write_tokens, cache_write);
                        add_usage(&mut events.usage.reasoning_tokens, reasoning);
                        events.usage.chargeable_tokens =
                            events.usage.chargeable_tokens.saturating_add(chargeable);
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

fn add_usage(total: &mut Option<u64>, amount: u64) {
    *total = Some(total.unwrap_or(0).saturating_add(amount));
}
