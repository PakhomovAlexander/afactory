//! The Claude adapter: a digest-pinned reviewer package driving `claude -p`.
//!
//! Same shape as the Codex adapter — the package's `reviewer.md` is the prompt, the manifest
//! args are the model flags (`--model opus --effort xhigh`), and both sit under the lockfile's
//! content digest — but a different provider surface, pinned by fixtures captured from a real
//! `claude` 2.1.234 run on 2026-08-18: `-p --output-format json` prints one JSON envelope
//! with `is_error`, the final text in `result`, and cumulative token usage in `usage`; the
//! prompt is streamed on stdin so Change Sets are not constrained by the argv ceiling. The
//! adapter appends `--safe-mode --restricted` and an explicit read-only tool grant after package
//! model flags, so repository settings, Hooks, plugins, MCP servers, and package arguments cannot
//! widen reviewer authority.
//!
//! **Auth is explicit grants, discovered by bisection against the real CLI.** Keychain auth
//! needs `USER` (the keychain account) and the real `HOME` (the keychain search path). An
//! operator-selected profile additionally needs `CLAUDE_CONFIG_DIR`. Never synthesize a config
//! directory: current Claude treats an explicit `$HOME/.claude` differently from its unset
//! default and therefore selects the wrong credential profile. Ambient API keys are not
//! forwarded because they can silently override the selected subscription identity. Granting
//! the real `HOME` is a deliberate loosening relative to the codex adapter; every grant's value
//! is redacted from everything stored.
//!
//! **Token mapping, recorded:** cost is uncached input plus cache creation plus output as the CLI
//! reports them — cache reads excluded. An agentic reviewer re-reads its context
//! through the cache on every turn; counting those would spend the whole attempt cap on
//! bookkeeping. Codex reports cached reads inside `input_tokens`, so the two adapters differ
//! exactly where their providers do.

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

pub struct ClaudeAdapter {
    program: String,
    model_flags: Vec<String>,
    prompt: String,
    timeout: Duration,
    /// (name, value) grants for subscription/keychain auth.
    grants: Vec<(String, String)>,
}

impl ClaudeAdapter {
    /// Build from a digest-verified package. The manifest must name `claude` (any path with
    /// that basename, so tests can point at a stub); its args become model flags; the prompt
    /// is the package's `reviewer.md` plus the shared result contract.
    pub fn from_package(
        package: &ResolvedReviewer,
        timeout: Duration,
    ) -> Result<ClaudeAdapter, String> {
        let basename = Path::new(&package.runner.program)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if basename != "claude" {
            return Err(format!(
                "package `{}` names runner `{}`; this adapter drives claude",
                package.name, package.runner.program
            ));
        }
        let model_flags = claude_model_flags(&package.runner)
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
        Ok(ClaudeAdapter {
            program: package.runner.program.clone(),
            model_flags,
            prompt,
            timeout,
            grants: Vec::new(),
        })
    }

    /// Explicit auth grants. Values come from the operator's own environment, read by the
    /// caller — this crate never reads env itself, so what reaches the child is explicit.
    pub fn with_auth(
        mut self,
        config_dir: Option<String>,
        user: impl Into<String>,
        home: impl Into<String>,
    ) -> Self {
        self.grants = vec![
            ("USER".to_string(), user.into()),
            ("HOME".to_string(), home.into()),
        ];
        if let Some(config_dir) = config_dir {
            self.grants
                .push(("CLAUDE_CONFIG_DIR".to_string(), config_dir));
        }
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

/// Build the adapter-owned capability smoke invocation. It shares the production argument
/// ordering while disabling tools: admission proves auth/model inference, not filesystem access.
pub fn smoke_command(runner: &Command) -> Result<Command, String> {
    let model_flags = claude_model_flags(runner)?;
    Ok(claude_command(&runner.program, &model_flags))
}

fn claude_model_flags(runner: &Command) -> Result<Vec<String>, String> {
    let values = runner.resolve().map_err(|error| error.to_string())?;
    let mut model = None;
    let mut effort = None;
    let mut index = 0;
    while index < values.len() {
        let option = &values[index];
        let value = values
            .get(index + 1)
            .ok_or_else(|| format!("Claude model option `{option}` has no value"))?;
        let slot = match option.as_str() {
            "--model" => &mut model,
            "--effort" => &mut effort,
            _ => {
                return Err(format!(
                    "unsupported Claude package argument `{option}`; packages may set only one --model and one --effort"
                ));
            }
        };
        if slot.replace(value.clone()).is_some() {
            return Err(format!("duplicate Claude package option `{option}`"));
        }
        index += 2;
    }
    let mut flags = Vec::new();
    if let Some(model) = model {
        flags.extend(["--model".into(), model]);
    }
    if let Some(effort) = effort {
        flags.extend(["--effort".into(), effort]);
    }
    Ok(flags)
}

fn claude_command(program: &str, model_flags: &[String]) -> Command {
    let mut args = vec![
        Arg::literal("-p"),
        Arg::literal("--output-format"),
        Arg::literal("json"),
    ];
    args.extend(model_flags.iter().map(Arg::literal));
    // Security flags are adapter-owned and deliberately follow package-controlled model flags.
    // Claude applies the last value for valued flags; packages therefore cannot replace the
    // permission mode or tool set. Safe/restricted modes disable repository customizations and
    // confine built-in file access to the sandbox supplied by ModelRunner.
    args.extend([
        Arg::literal("--safe-mode"),
        Arg::literal("--restricted"),
        Arg::literal("--permission-mode"),
        Arg::literal("dontAsk"),
        Arg::literal("--strict-mcp-config"),
        Arg::literal("--tools"),
        Arg::literal("Read,Glob,Grep"),
        Arg::literal("--allowedTools"),
        Arg::literal("Read,Glob,Grep"),
    ]);
    Command::new(program, args)
}

impl ReviewerAdapter for ClaudeAdapter {
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
        // The package prompt, then this attempt's labelled inputs — data the kernel resolved,
        // rendered under an explicit heading rather than woven into the instructions. The same
        // pure composition backs `render_input`, so what is sent is what can be audited.
        let (prompt, context_manifest) =
            compose_model_prompt(&self.prompt, inputs).map_err(RunnerError::Refused)?;
        let command = claude_command(&self.program, &self.model_flags);

        let mut runner = ModelRunner::new(sandbox_root, self.timeout);
        for (name, value) in &self.grants {
            runner = runner.with_env(name, value);
        }
        for (name, value) in &inputs.sandbox_environment {
            runner = runner.with_env(name, value);
        }
        let capture = runner.capture_with_stdin(cas, &command, prompt.into_bytes())?;

        let envelope = Envelope::parse(&capture.stdout);
        let cost = envelope.cost_tokens;
        if capture.status.success() && !envelope.is_error {
            let text = envelope
                .result
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| RunnerError::MalformedOutput {
                    raw_artifact: capture.raw_artifact.clone(),
                    why: "claude -p succeeded but returned no result text".into(),
                })?;
            let output = parse_stage_output_for(inputs.result_contract, &text).map_err(|e| {
                RunnerError::MalformedOutput {
                    raw_artifact: capture.raw_artifact.clone(),
                    why: e.to_string(),
                }
            })?;
            let proposal = parse_proposal_declaration(&text);
            let notes = parse_notes_declaration(&text);
            return Ok(ReceiptedReviewerReturn {
                returned: ReviewerReturn {
                    output,
                    proposal,
                    notes,
                    cost_tokens: cost,
                    raw_artifact: capture.raw_artifact,
                },
                usage: envelope.usage,
                context_manifest,
            });
        }

        // Same accounting rule as codex: usage reported means tokens were spent (Failed, the
        // kernel charges); none means the model was never reached (Unavailable, released).
        let message = envelope
            .result
            .unwrap_or_else(|| "claude -p failed with no envelope".to_string());
        Err(if cost > 0 {
            RunnerError::Failed {
                exit_code: capture.status.code().unwrap_or(-1),
                stderr_excerpt: message,
            }
        } else {
            RunnerError::Unavailable(message)
        })
    }
}

#[derive(Default)]
struct Envelope {
    is_error: bool,
    result: Option<String>,
    cost_tokens: u64,
    usage: TokenUsage,
}

impl Envelope {
    /// The `-p --output-format json` envelope: one object on stdout. An unparseable stream
    /// leaves the default (no usage, no result), which classifies as Unavailable on a failed
    /// exit and MalformedOutput on a clean one — both honest.
    fn parse(stdout: &[u8]) -> Envelope {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(stdout) else {
            return Envelope::default();
        };
        let usage = value.get("usage");
        let count = |key: &str| {
            usage
                .and_then(|u| u.get(key))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let input = count("input_tokens");
        let output = count("output_tokens");
        let cache_read = count("cache_read_input_tokens");
        let cache_write = count("cache_creation_input_tokens");
        let cost_tokens = input + cache_write + output;
        Envelope {
            is_error: value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            result: value
                .get("result")
                .and_then(|r| r.as_str())
                .map(str::to_string),
            cost_tokens,
            usage: TokenUsage {
                input_tokens: Some(input),
                output_tokens: Some(output),
                cache_read_tokens: Some(cache_read),
                cache_write_tokens: Some(cache_write),
                reasoning_tokens: None,
                chargeable_tokens: cost_tokens,
            },
        }
    }
}
