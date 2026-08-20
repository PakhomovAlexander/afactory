//! The Claude adapter: a digest-pinned reviewer package driving `claude -p`.
//!
//! Same shape as the Codex adapter — the package's `reviewer.md` is the prompt, the manifest
//! args are the model flags (`--model opus --effort xhigh`), and both sit under the lockfile's
//! content digest — but a different provider surface, pinned by fixtures captured from a real
//! `claude` 2.1.234 run on 2026-08-18: `-p --output-format json` prints one JSON envelope
//! with `is_error`, the final text in `result`, and cumulative token usage in `usage`.
//!
//! **Auth is explicit grants, discovered by bisection against the real CLI.** Keychain auth
//! needs `USER` (the keychain account) and the real `HOME` (the keychain search path). An
//! operator-selected profile additionally needs `CLAUDE_CONFIG_DIR`; API-key auth needs
//! `ANTHROPIC_API_KEY`. Never synthesize a config directory: current Claude treats an explicit
//! `$HOME/.claude` differently from its unset default and therefore selects the wrong credential
//! profile. Granting the real `HOME` is a deliberate loosening relative to the codex adapter;
//! every grant's value — including the optional key — is redacted from everything stored.
//!
//! **Token mapping, recorded:** cost is `usage.input_tokens + usage.output_tokens` as the CLI
//! reports them — cache reads and writes excluded. An agentic reviewer re-reads its context
//! through the cache on every turn; counting those would spend the whole attempt cap on
//! bookkeeping. Codex reports cached reads inside `input_tokens`, so the two adapters differ
//! exactly where their providers do.

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::{
    ModelRunner, RESULT_CONTRACT, ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError,
    parse_stage_output,
};
use review_store::Cas;

pub struct ClaudeAdapter {
    program: String,
    model_flags: Vec<String>,
    prompt: String,
    timeout: Duration,
    /// (name, value) grants for keychain auth plus an optional API key.
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
        api_key: Option<String>,
    ) -> Self {
        self.grants = vec![
            ("USER".to_string(), user.into()),
            ("HOME".to_string(), home.into()),
        ];
        if let Some(config_dir) = config_dir {
            self.grants
                .push(("CLAUDE_CONFIG_DIR".to_string(), config_dir));
        }
        if let Some(api_key) = api_key {
            self.grants
                .push(("ANTHROPIC_API_KEY".to_string(), api_key));
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

impl ReviewerAdapter for ClaudeAdapter {
    fn invoke(
        &self,
        cas: &Cas,
        sandbox_root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let mut args = vec![
            Arg::literal("-p"),
            Arg::literal("--output-format"),
            Arg::literal("json"),
        ];
        args.extend(self.model_flags.iter().map(Arg::literal));
        // The package prompt, then this attempt's labelled inputs — data the kernel resolved,
        // rendered under an explicit heading rather than woven into the instructions.
        args.push(Arg::literal(format!("{}{}", self.prompt, inputs.render())));
        let command = Command::new(&self.program, args);

        let mut runner = ModelRunner::new(sandbox_root, self.timeout);
        for (name, value) in &self.grants {
            runner = runner.with_grant(name, value);
        }
        let capture = runner.capture(cas, &command)?;

        let envelope = Envelope::parse(&capture.stdout);
        let cost = envelope.cost_tokens;
        if capture.status.success() && !envelope.is_error {
            let text = envelope
                .result
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    RunnerError::MalformedOutput(format!(
                        "claude -p succeeded but returned no result text; the raw envelope \
                         is stored as {}",
                        capture.raw_artifact
                    ))
                })?;
            let output = parse_stage_output(&text).map_err(|e| {
                RunnerError::MalformedOutput(format!(
                    "{e}; the raw envelope is stored as {}",
                    capture.raw_artifact
                ))
            })?;
            return Ok(ReviewerReturn {
                output,
                cost_tokens: cost,
                raw_artifact: capture.raw_artifact,
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
        Envelope {
            is_error: value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            result: value
                .get("result")
                .and_then(|r| r.as_str())
                .map(str::to_string),
            cost_tokens: count("input_tokens") + count("output_tokens"),
        }
    }
}
