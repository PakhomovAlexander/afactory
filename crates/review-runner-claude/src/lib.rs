//! The Claude adapter: a digest-pinned Worker package driving `claude -p`.
//!
//! Same shape as the Codex adapter — the package's `reviewer.md` is the prompt, the manifest
//! args are the model flags (`--model opus --effort xhigh`), and both sit under the lockfile's
//! content digest — but a different provider surface: `-p --output-format json` prints one JSON
//! envelope with `is_error`, the final text in `result`, and cumulative token usage in `usage`;
//! the prompt is streamed on stdin so Change Sets are not constrained by the argv ceiling. The
//! adapter appends `--safe-mode --restricted` and an explicit read-only tool grant after package
//! model flags, so repository settings, Hooks, plugins, MCP servers, and package arguments cannot
//! widen Worker authority. A Task Attempt widens that grant only as far as the kernel-derived
//! [`WorkerAccess`](review_runner::task::WorkerAccess) allows ([`task::task_tools`]).
//! [`ClaudeAdapter`] renders a package's input for `af review render`; [`task::ClaudeTaskAdapter`]
//! runs it.
//!
//! **The invocation is a requirement on the installed CLI.** [`CLAUDE_INVOCATION`] lists every
//! option the command passes; af reads `claude --help` before any paid dispatch and refuses a CLI
//! that does not declare one of them (Claude Code before 2.1.248 has no `--restricted`, before
//! 2.1.169 no `--safe-mode`), instead of dropping or replacing an isolation flag. A Worker that
//! still exits on its own argument parser is classified
//! [`UnsupportedInvocation`](review_runner::native_failure::NativeFailureKind::UnsupportedInvocation),
//! with its exit status and a bounded stderr excerpt in the diagnostic (ADR-0146).
//!
//! **Auth is explicit grants, discovered by bisection against the real CLI.** Keychain auth
//! needs `USER` (the keychain account) and the real `HOME` (the keychain search path). An
//! operator-selected profile additionally needs `CLAUDE_CONFIG_DIR`. Never synthesize a config
//! directory: current Claude treats an explicit `$HOME/.claude` differently from its unset
//! default and therefore selects the wrong credential profile. Ambient API keys are not
//! forwarded because they can silently override the selected subscription identity. Granting
//! the real `HOME` is a deliberate loosening relative to the codex adapter. `CLAUDE_CONFIG_DIR` is
//! redacted from everything stored, as granted and canonical; `HOME` and `USER` are not, since a
//! finding may legitimately name a home path or a user.
//!
//! **The bound model is the only model.** A Task Attempt runs with
//! `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, beside the nonessential-traffic and terminal-title
//! guards: when the bound model's safety classifier flags a request, Claude Code would otherwise
//! re-run it on a fallback model and keep the session there. With it the turn ends as a refusal,
//! classified `model_refused`, charged what it reports and never retried.
//! `CLAUDE_CODE_NO_MODEL_FALLBACK` is not set, since it also disables compaction's model
//! substitution. The guard is a client mitigation; the identity check over `modelUsage` stays the
//! authority and refuses, by name, any reply another model took part in. [`hint`] words all of it
//! for the run's own stderr (ADR-0147). [`ClaudeAdapter`] starts no process, so it sets no guard.
//!
//! **The package names an exact ID.** The package argument parser applies the static rules of
//! [`models`], the one validation authority for model and effort spellings (ADR-0148): an alias,
//! a misspelling of a known exact ID, an unversioned model or an effort level Claude Code does not
//! document is refused, with a hint naming the rule, the nearest accepted spellings and `af
//! provider models`, before any process starts.
//!
//! **No session is carried.** An Attempt's command assigns no session identity and resumes
//! nothing; the history the CLI keeps for an Attempt's own working directory is removed when the
//! process exits ([`session::ClaudeSessionStore::remove_attempt_project`]).
//!
//! **Token mapping, recorded:** cost is uncached input plus cache creation plus output as the CLI
//! reports them — cache reads excluded. An agentic Worker re-reads its context
//! through the cache on every turn; counting those would spend the whole attempt cap on
//! bookkeeping. Codex reports cached reads inside `input_tokens`, so the two adapters differ
//! exactly where their providers do.

pub mod hint;
pub mod models;
pub mod session;
pub mod task;

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::cli::{CliFlag, CliInvocation};
use review_runner::{
    InputTransport, ModelRunner, RenderedInput, ReviewerAdapter, ReviewerInputs, RunnerError,
    compose_model_prompt,
};
use review_store::Cas;

pub use session::ClaudeSessionStore;

pub struct ClaudeAdapter {
    prompt: String,
}

impl ClaudeAdapter {
    /// Build from a digest-verified package. The manifest must name `claude` (any path with
    /// that basename, so tests can point at a stub); its args become model flags; the prompt
    /// is the package's `reviewer.md` plus the shared result contract.
    pub fn from_package(package: &ResolvedReviewer) -> Result<ClaudeAdapter, String> {
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
        // Rendering spawns nothing, but a package whose arguments the Task adapter would refuse
        // is refused here too, so `af review render` never shows a package that cannot run.
        claude_model_flags(&package.runner)
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
        Ok(ClaudeAdapter { prompt })
    }

    /// Narrow this invocation's attention. A narrowing only — the package prompt still
    /// governs; this cannot grant anything the package did not.
    pub fn with_focus(mut self, focus: impl AsRef<str>) -> Self {
        review_runner::append_focus(&mut self.prompt, focus.as_ref());
        self
    }
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
    // The package's own words reach Claude Code only when they pass the static rules: no alias,
    // no misspelling of a known ID, no effort level Claude Code does not document (ADR-0148).
    // A package without --model leaves the model to Claude Code; its effort level is still
    // checked, and refused through the same hint.
    let checked = match (&model, &effort) {
        (Some(model), _) => models::check_static(models::Kind::Claude, model, effort.as_deref()),
        (None, Some(effort)) => models::check_effort(models::Kind::Claude, effort),
        (None, None) => Ok(()),
    };
    checked.map_err(|refusal| {
        refusal.hint(
            models::ProviderRef {
                id: None,
                kind: models::Kind::Claude,
            },
            model.as_deref().unwrap_or(""),
            effort.as_deref(),
        )
    })?;
    let mut flags = Vec::new();
    if let Some(model) = model {
        flags.extend(["--model".into(), model]);
    }
    if let Some(effort) = effort {
        flags.extend(["--effort".into(), effort]);
    }
    Ok(flags)
}

/// Every option a Claude Attempt's command passes, in the spelling it passes it: what an installed
/// `claude` must declare in `claude --help` before af dispatches to it (ADR-0146). A test holds
/// this list equal to what [`claude_command`] and the Task adapter emit, so it cannot drift from
/// the invocation. The isolation flags are required, never optional: a CLI without them is
/// refused, never run without them.
pub const CLAUDE_INVOCATION: CliInvocation = CliInvocation {
    program: "claude",
    help_args: &["--help"],
    flags: &[
        CliFlag::short("-p", "--print"),
        CliFlag::long("--output-format"),
        CliFlag::long("--model"),
        CliFlag::long("--effort"),
        CliFlag::long("--safe-mode"),
        CliFlag::long("--restricted"),
        CliFlag::long("--permission-mode"),
        CliFlag::long("--strict-mcp-config"),
        CliFlag::long("--tools"),
        CliFlag::long("--allowedTools"),
        CliFlag::long("--json-schema"),
    ],
    upgrade: "npm install -g @anthropic-ai/claude-code@latest",
};

/// The `claude -p` command one Attempt runs: the package model flags, then the adapter-owned
/// security flags.
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
    /// The package prompt, then this Attempt's labelled inputs — data the kernel resolved,
    /// rendered under an explicit heading rather than woven into the instructions.
    fn render_input(&self, inputs: &ReviewerInputs) -> Result<RenderedInput, RunnerError> {
        let (prompt, manifest) =
            compose_model_prompt(&self.prompt, inputs).map_err(RunnerError::Refused)?;
        Ok(RenderedInput {
            transport: InputTransport::Prompt,
            bytes: prompt.into_bytes(),
            manifest,
        })
    }
}

#[cfg(test)]
mod model_rule_tests {
    use super::*;

    fn flags(args: &[&str]) -> Result<Vec<String>, String> {
        claude_model_flags(&Command::new(
            "claude",
            args.iter().copied().map(Arg::literal).collect(),
        ))
    }

    /// A package that leaves the model to Claude Code still has its effort level checked, and its
    /// refusal is the one hint, naming the harness's default model and `af provider models`
    /// (ADR-0148).
    #[test]
    fn an_effort_level_without_a_model_is_refused_through_the_hint() {
        assert_eq!(
            flags(&["--effort", "xhigh"]).unwrap(),
            ["--effort", "xhigh"]
        );
        let refused = flags(&["--effort", "ultra"]).unwrap_err();
        for part in [
            "af refused the binding of the harness's default model (effort ultra) for a claude \
             Provider (model_not_accepted)",
            "the effort level `ultra` is not one Claude Code accepts: low, medium, high, xhigh, max",
            "`af provider models` (token-free)",
        ] {
            assert!(refused.contains(part), "{part}: {refused}");
        }
        let diagnostic = format!(
            "{}{refused}; transport: not started",
            hint::DIAGNOSTIC_PREFIX
        );
        assert_eq!(hint::find(&diagnostic), Some(refused.as_str()));
        let refused = flags(&["--effort", "/Users/alice"]).unwrap_err();
        assert!(
            refused.contains("its --effort value (12 bytes)"),
            "{refused}"
        );
        assert!(!refused.contains("alice"), "{refused}");
        assert_eq!(
            flags(&["--model", "claude-fable-5-1", "--effort", "max"]).unwrap(),
            ["--model", "claude-fable-5-1", "--effort", "max"]
        );
    }
}
