//! The Codex adapter: a digest-pinned Worker package driving `codex exec`.
//!
//! Everything the model sees comes from the package: `reviewer.md` is the prompt, the
//! manifest's runner args are the model flags, and both sit under the lockfile's content
//! digest — so "which Worker ran" is a verifiable claim, not a deployment accident.
//! [`CodexAdapter`] renders a package's input for `af review render`; [`task::CodexTaskAdapter`]
//! runs it.
//!
//! The invocation shape: `codex exec --ephemeral --skip-git-repo-check --json -C <sandbox> -s
//! <mode> -o <staging>/last-message [--output-schema <staging>/reply-schema.json] <flags> -`, with
//! the prompt streamed on stdin; the schema file is the typed reply's strict projection, written
//! only for a typed request whose contract has one (ADR-0196). Events arrive
//! as JSONL on stdout; the `-o` file holds the final answer; `turn.completed` events carry token
//! usage. `--ephemeral` keeps session files off the host, `-C` roots the model in the kernel's
//! sandbox, and `-s` is `workspace-write` when the Worker may write its own copy — and nothing
//! else — and `read-only` otherwise. A source-writing Worker and a reviewer declaring
//! `execute-checks` both write their copy; only the first has it captured as a candidate.
//!
//! [`CODEX_INVOCATION`] lists every option that command passes. af reads `codex exec --help`
//! before any paid dispatch and refuses a CLI that does not declare one of them, instead of
//! dropping the sandbox mode or any other flag; a Worker that still exits on its own argument
//! parser is classified
//! [`UnsupportedInvocation`](review_runner::native_failure::NativeFailureKind::UnsupportedInvocation),
//! with its exit status and a bounded stderr excerpt in the diagnostic (ADR-0146).
//!
//! The package argument parser applies the static rules of `review_runner_claude::models`, the
//! one validation authority for model and effort spellings (ADR-0148): a misspelled or
//! unversioned model, or an effort level outside `low medium high xhigh max ultra`, is refused
//! with a hint naming the rule and `af provider models` before any process starts.
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
use review_runner::cli::{CliFlag, CliInvocation};
use review_runner::{
    InputTransport, ModelRunner, RenderedInput, ReviewerAdapter, ReviewerInputs, RunnerError,
    compose_model_prompt,
};
use review_runner_claude::models;
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
    // The package's own words reach Codex only when they pass the static rules: no misspelling
    // of a known ID, no unversioned model, no effort level outside Codex's (ADR-0148).
    let level = effort.as_deref().map(|value| {
        value
            .strip_prefix("model_reasoning_effort=")
            .expect("the option was matched by this prefix")
            .trim_matches(['\'', '"'])
    });
    // A package without a model leaves it to Codex; its effort level is still checked, and
    // refused through the same hint.
    let checked = match (&model, level) {
        (Some(model), _) => models::check_static(models::Kind::Codex, model, level),
        (None, Some(level)) => models::check_effort(models::Kind::Codex, level),
        (None, None) => Ok(()),
    };
    checked.map_err(|refusal| {
        refusal.hint(
            models::ProviderRef {
                id: None,
                kind: models::Kind::Codex,
            },
            model.as_deref().unwrap_or(""),
            level,
        )
    })?;
    let mut flags = Vec::new();
    if let Some(model) = model {
        flags.extend(["--model".into(), model]);
    }
    if let Some(effort) = effort {
        flags.extend(["-c".into(), effort]);
    }
    Ok(flags)
}

/// Every option a Codex Attempt's `codex exec` passes, in the spelling it passes it: what an
/// installed `codex` must declare in `codex exec --help` before af dispatches to it (ADR-0146). A
/// test holds this list equal to what [`codex_command`] emits, so it cannot drift from the
/// invocation. `-s` carries the sandbox mode, the isolation boundary: a CLI without it is
/// refused, never run without it.
pub const CODEX_INVOCATION: CliInvocation = CliInvocation {
    program: "codex",
    help_args: &["exec", "--help"],
    flags: &[
        CliFlag::long("--ephemeral"),
        CliFlag::long("--skip-git-repo-check"),
        CliFlag::long("--json"),
        CliFlag::short("-C", "--cd"),
        CliFlag::short("-s", "--sandbox"),
        CliFlag::short("-o", "--output-last-message"),
        CliFlag::long("--output-schema"),
        CliFlag::long("--model"),
        CliFlag::short("-c", "--config"),
    ],
    upgrade: "npm install -g @openai/codex@latest",
};

/// The `codex exec` command of one Attempt. `schema` is the file holding the typed reply's
/// strict output schema ([`review_runner::task::reply_schema::strict`]), when the Worker declares
/// a typed reply with a strict projection: Codex then refuses a structurally wrong final message
/// itself, before it is charged as a typed-reply failure (ADR-0196).
fn codex_command(
    program: &str,
    model_flags: &[String],
    sandbox_root: &Path,
    mode: &str,
    output: &Path,
    schema: Option<&Path>,
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
    if let Some(schema) = schema {
        args.extend([
            Arg::literal("--output-schema"),
            Arg::literal(schema.display().to_string()),
        ]);
    }
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

#[cfg(test)]
mod invocation_tests {
    use super::*;
    use review_runner::task::WorkerAccess;
    use std::collections::BTreeSet;

    /// [`CODEX_INVOCATION`] is the single source of truth for what `codex exec --help` must
    /// declare: every option any Attempt's command can emit is in it, and nothing else is.
    #[test]
    fn the_required_flag_list_is_exactly_what_the_command_builder_emits() {
        let model_flags = codex_model_flags(&Command::new(
            "codex",
            [
                "--model",
                "gpt-fixture-1",
                "-c",
                "model_reasoning_effort=\"high\"",
            ]
            .map(Arg::literal)
            .into(),
        ))
        .unwrap();
        let mut emitted = BTreeSet::new();
        for access in [
            WorkerAccess::ReadOnly,
            WorkerAccess::ExecuteChecks,
            WorkerAccess::WriteSource,
            WorkerAccess::WriteSourceWithShell,
        ] {
            for schema in [None, Some(Path::new("/staging/reply-schema.json"))] {
                let command = codex_command(
                    "codex",
                    &model_flags,
                    Path::new("/sandbox"),
                    task::task_sandbox_mode(access),
                    Path::new("/staging/last-message"),
                    schema,
                );
                let argv = command.resolve().unwrap();
                assert_eq!(argv[0], "exec", "the help checked is the subcommand's");
                assert_eq!(
                    argv.last().map(String::as_str),
                    Some("-"),
                    "prompt on stdin"
                );
                for value in &argv[1..argv.len() - 1] {
                    if value.starts_with('-') {
                        assert!(
                            CODEX_INVOCATION.declares(value),
                            "`{value}` is emitted but not required of the installed CLI"
                        );
                        emitted.insert(value.clone());
                    }
                }
            }
        }
        let required: BTreeSet<String> = CODEX_INVOCATION
            .flags
            .iter()
            .map(|flag| flag.spelling.to_string())
            .collect();
        assert_eq!(emitted, required, "a required flag is never emitted");
        assert_eq!(CODEX_INVOCATION.help_args, ["exec", "--help"]);
        assert!(
            CODEX_INVOCATION.declares("-s"),
            "the sandbox mode is required"
        );
    }
}

#[cfg(test)]
mod model_rule_tests {
    use super::*;

    fn flags(model: &str, effort: &str) -> Result<Vec<String>, String> {
        codex_model_flags(&Command::new(
            "codex",
            ["--model", model, "-c", effort]
                .into_iter()
                .map(Arg::literal)
                .collect(),
        ))
    }

    /// The package parser applies the one validation authority's static rules (ADR-0148).
    #[test]
    fn the_package_parser_accepts_codex_levels_and_refuses_what_codex_does_not_accept() {
        for effort in [
            "model_reasoning_effort=\"ultra\"",
            "model_reasoning_effort=high",
        ] {
            assert!(flags("gpt-6-sol", effort).is_ok(), "{effort}");
        }
        let refused = flags("gpt-6-sol", "model_reasoning_effort=\"minimal\"").unwrap_err();
        for part in [
            "for a codex Provider (model_not_accepted)",
            "the effort level `minimal` is not one Codex accepts: low, medium, high, xhigh, max, ultra",
            "`af provider models` (token-free)",
        ] {
            assert!(refused.contains(part), "{part}: {refused}");
        }
        let refused = flags("gpt", "model_reasoning_effort=\"high\"").unwrap_err();
        assert!(
            refused.contains("Task Model binding requires an explicit versioned model ID"),
            "{refused}"
        );
        let refused = flags("/Users/alice/m-1", "model_reasoning_effort=\"high\"").unwrap_err();
        assert!(
            refused.contains("its --model value (16 bytes)"),
            "{refused}"
        );
        assert!(!refused.contains("alice"), "{refused}");
    }

    /// A package that leaves the model to Codex still has its effort level checked, and its
    /// refusal is the same hint, naming the harness's default model and `af provider models`.
    #[test]
    fn an_effort_level_without_a_model_is_refused_through_the_hint() {
        let only = |effort: &str| {
            codex_model_flags(&Command::new(
                "codex",
                ["-c", effort].into_iter().map(Arg::literal).collect(),
            ))
        };
        assert_eq!(
            only("model_reasoning_effort=\"ultra\"").unwrap(),
            ["-c", "model_reasoning_effort=\"ultra\""]
        );
        let refused = only("model_reasoning_effort=\"minimal\"").unwrap_err();
        for part in [
            "af refused the binding of the harness's default model (effort minimal) for a codex \
             Provider (model_not_accepted)",
            "the effort level `minimal` is not one Codex accepts: low, medium, high, xhigh, max, ultra",
            "`af provider models` (token-free)",
        ] {
            assert!(refused.contains(part), "{part}: {refused}");
        }
        let refused = only("model_reasoning_effort=\"a b\"").unwrap_err();
        assert!(
            refused.contains("its --effort value (3 bytes)"),
            "{refused}"
        );
        assert!(!refused.contains("a b"), "{refused}");
    }
}
