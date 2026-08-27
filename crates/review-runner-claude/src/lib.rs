//! The Claude adapter: a digest-pinned reviewer package driving `claude -p`.
//!
//! Same shape as the Codex adapter — the package's `reviewer.md` is the prompt, the manifest
//! args are the model flags (`--model opus --effort xhigh`), and both sit under the lockfile's
//! content digest — but a different provider surface, pinned by fixtures captured from a real
//! `claude` 2.1.234 run on 2026-08-18: `-p --output-format json` prints one JSON envelope
//! with `is_error`, the final text in `result`, and cumulative token usage in `usage`; the
//! prompt is streamed on stdin so Change Sets are not constrained by the argv ceiling.
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

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::{
    ContextManifest, ModelRunner, RESULT_CONTRACT, ReceiptedReviewerReturn, ReviewerAdapter,
    ReviewerInputs, ReviewerReturn, RunnerError, TokenUsage, parse_stage_output,
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
        Ok(ClaudeAdapter {
            program: package.runner.program.clone(),
            model_flags,
            prompt: format!("{prompt}{RESULT_CONTRACT}"),
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
    let model_flags = runner.resolve().map_err(|error| error.to_string())?;
    Ok(claude_command(&runner.program, &model_flags))
}

fn claude_command(program: &str, model_flags: &[String]) -> Command {
    let mut args = vec![
        Arg::literal("-p"),
        Arg::literal("--output-format"),
        Arg::literal("json"),
    ];
    args.extend(model_flags.iter().map(Arg::literal));
    Command::new(program, args)
}

impl ReviewerAdapter for ClaudeAdapter {
    fn invoke(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        self.invoke_receipted(cas, sandbox_root, inputs)
            .map(|receipt| receipt.returned)
    }

    fn invoke_receipted(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReceiptedReviewerReturn, RunnerError> {
        // The package prompt, then this attempt's labelled inputs — data the kernel resolved,
        // rendered under an explicit heading rather than woven into the instructions.
        let mut prompt = self.prompt.clone();
        let instruction_bytes = prompt.len();
        inputs
            .render_into(&mut prompt)
            .map_err(RunnerError::Refused)?;
        let mut context_manifest = ContextManifest::default();
        context_manifest.record(
            "worker_instructions",
            "digest-pinned Worker package and output contract",
            inputs
                .attempt_context
                .as_ref()
                .and_then(|context| context.reviewer_package_artifact_id.clone()),
            Some("review.kernel/ReviewerPackage@1".into()),
            instruction_bytes,
        );
        context_manifest.record(
            "role_scoped_inputs",
            "exact Worker Input",
            inputs
                .attempt_context
                .as_ref()
                .map(|context| context.campaign_manifest_id.clone()),
            None,
            prompt.len() - instruction_bytes,
        );
        context_manifest.finish(prompt.len());
        let command = claude_command(&self.program, &self.model_flags);

        let mut runner = ModelRunner::new(sandbox_root, self.timeout);
        for (name, value) in &self.grants {
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
            let output = parse_stage_output(&text).map_err(|e| RunnerError::MalformedOutput {
                raw_artifact: capture.raw_artifact.clone(),
                why: e.to_string(),
            })?;
            return Ok(ReceiptedReviewerReturn {
                returned: ReviewerReturn {
                    output,
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
