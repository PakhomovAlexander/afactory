//! The Codex adapter: a digest-pinned Worker package driving `codex exec`.
//!
//! Everything the model sees comes from the package: `reviewer.md` is the prompt, the
//! manifest's runner args are the model flags, and both sit under the lockfile's content
//! digest — so "which Worker ran" is a verifiable claim, not a deployment accident.
//! [`CodexAdapter`] renders a package's input for `af review render`; [`task::CodexTaskAdapter`]
//! runs it.
//!
//! The invocation shape: `codex exec --ephemeral --skip-git-repo-check --json -C <sandbox> -s
//! <mode> -o <staging>/last-message <flags> -`, with the prompt streamed on stdin. Events arrive
//! as JSONL on stdout; the `-o` file holds the final answer; `turn.completed` events carry token
//! usage. `--ephemeral` keeps session files off the host, `-C` roots the model in the kernel's
//! sandbox, and `-s` is `workspace-write` when the Worker may write its own copy — and nothing
//! else — and `read-only` otherwise. A source-writing Worker and a reviewer declaring
//! `execute-checks` both write their copy; only the first has it captured as a candidate.
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
    InputTransport, ModelRunner, RenderedInput, ReviewerAdapter, ReviewerInputs, RunnerError,
    compose_model_prompt,
};
use review_store::Cas;

pub struct CodexAdapter {
    prompt: String,
}

impl CodexAdapter {
    /// Build from a digest-verified package. The manifest must name `codex` (any path with
    /// that basename, so tests can point at a stub) and may set only the model flags a Task
    /// Worker accepts; the prompt is the package's `reviewer.md` plus the result contract.
    pub fn from_package(package: &ResolvedReviewer) -> Result<CodexAdapter, String> {
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
        codex_model_flags(&package.runner)
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
        Ok(CodexAdapter { prompt })
    }

    /// Narrow this invocation's attention. A narrowing only — the package prompt still
    /// governs; this cannot grant anything the package did not.
    pub fn with_focus(mut self, focus: impl AsRef<str>) -> Self {
        review_runner::append_focus(&mut self.prompt, focus.as_ref());
        self
    }
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
    output: &Path,
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
        Arg::literal("-o"),
        Arg::literal(output.display().to_string()),
    ];
    args.extend(model_flags.iter().map(Arg::literal));
    args.push(Arg::literal("-"));
    Command::new(program, args)
}

impl ReviewerAdapter for CodexAdapter {
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
