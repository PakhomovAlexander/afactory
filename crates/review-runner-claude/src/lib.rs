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
//! **Auth is explicit grants, discovered by bisection against the real CLI.** Keychain auth
//! needs `USER` (the keychain account) and the real `HOME` (the keychain search path). An
//! operator-selected profile additionally needs `CLAUDE_CONFIG_DIR`. Never synthesize a config
//! directory: current Claude treats an explicit `$HOME/.claude` differently from its unset
//! default and therefore selects the wrong credential profile. Ambient API keys are not
//! forwarded because they can silently override the selected subscription identity. Granting
//! the real `HOME` is a deliberate loosening relative to the codex adapter; every grant's value
//! is redacted from everything stored.
//!
//! **Sessions are the kernel's, not the provider's.** When a node's warm policy asks for the
//! session layer, an Attempt's command carries the kernel-derived `--session-id`, and, when a
//! Session Snapshot was carried, `--resume <source> --fork-session` so the captured transcript is
//! read and never mutated ([`ClaudeAdapter::attempt_command`]). Both flags precede the package's
//! model flags and are pinned by test like the security flags. Without them — every other
//! adapter, Codex included — no session is assigned, captured, or resumed. The Task host does not
//! run the session layer yet, so its Attempts carry neither flag. See [`session`].
//!
//! **Token mapping, recorded:** cost is uncached input plus cache creation plus output as the CLI
//! reports them — cache reads excluded. An agentic Worker re-reads its context
//! through the cache on every turn; counting those would spend the whole attempt cap on
//! bookkeeping. Codex reports cached reads inside `input_tokens`, so the two adapters differ
//! exactly where their providers do.

pub mod session;
pub mod task;

use std::path::Path;
use std::time::Duration;

use review_core::{Arg, Command};
use review_runner::ResolvedReviewer;
use review_runner::{
    InputTransport, ModelRunner, RenderedInput, ReviewerAdapter, ReviewerInputs, RunnerError,
    SessionLayer, compose_model_prompt,
};
use review_store::Cas;

pub use session::ClaudeSessionStore;

pub struct ClaudeAdapter {
    program: String,
    model_flags: Vec<String>,
    prompt: String,
    /// The operator's harness session store, derived from the explicit auth grants. Absent
    /// until one is supplied: without `HOME` there is no directory to address, and the adapter
    /// refuses the session layer rather than guessing one.
    session_store: Option<ClaudeSessionStore>,
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
            session_store: None,
        })
    }

    /// Host the session layer in `store`: the directory the operator's auth grants point the
    /// harness at ([`ClaudeSessionStore::from_grants`]). Built by the caller from those same
    /// grants rather than read from the environment, so the adapter can only ever capture and
    /// delete inside what the operator granted.
    pub fn with_session_store(mut self, store: ClaudeSessionStore) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Narrow this invocation's attention. A narrowing only — the package prompt still
    /// governs; this cannot grant anything the package did not.
    pub fn with_focus(mut self, focus: impl AsRef<str>) -> Self {
        review_runner::append_focus(&mut self.prompt, focus.as_ref());
        self
    }

    /// The `claude -p` command one Attempt runs for `inputs`: the session the kernel assigned
    /// (and the forked resume of a carried Session Snapshot) first, then the package model
    /// flags, then the adapter-owned security flags. Kept as the base for running the session
    /// layer on the Task host (ADR-0110).
    pub fn attempt_command(&self, inputs: &ReviewerInputs) -> Command {
        let session = inputs.session_id.as_deref().map(|session_id| SessionArgs {
            session_id,
            resume_from: inputs
                .session_resume
                .as_ref()
                .map(|resume| resume.session_id.as_str()),
        });
        claude_command(&self.program, &self.model_flags, session)
    }
}

/// The session arguments one Attempt runs under: the kernel's own identity for the session this
/// Attempt will write, and, when a Session Snapshot was carried, the source session to resume
/// with a fork so the captured transcript is read and never mutated.
struct SessionArgs<'a> {
    session_id: &'a str,
    resume_from: Option<&'a str>,
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

fn claude_command(
    program: &str,
    model_flags: &[String],
    session: Option<SessionArgs<'_>>,
) -> Command {
    let mut args = vec![
        Arg::literal("-p"),
        Arg::literal("--output-format"),
        Arg::literal("json"),
    ];
    // The session identity is the kernel's, assigned from the Attempt ID before the process
    // starts, so the transcript this invocation writes is the kernel's to capture and delete
    // rather than the provider's to keep. A resume always forks: the captured transcript is
    // read, and everything this Attempt says lands in its own session.
    if let Some(session) = &session {
        args.extend([
            Arg::literal("--session-id"),
            Arg::literal(session.session_id),
        ]);
        if let Some(resume_from) = session.resume_from {
            args.extend([
                Arg::literal("--resume"),
                Arg::literal(resume_from),
                Arg::literal("--fork-session"),
            ]);
        }
    }
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
    fn session_layer(&self) -> Option<&dyn SessionLayer> {
        self.session_store
            .as_ref()
            .map(|store| store as &dyn SessionLayer)
    }

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
