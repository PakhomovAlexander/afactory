//! The Antigravity adapter: a digest-pinned Worker package driving `agy`.
//!
//! Instructions and typed inputs are captured by the native Task Worker contract. The manifest's
//! runner args are the model flags (`--model <id> --effort <level>`), and both sit under the
//! lockfile's content digest — so "which Worker ran" is a verifiable claim, not a deployment
//! accident. [`task::AntigravityTaskAdapter`] runs that captured input.
//!
//! The invocation shape:
//! `agy --input-format stream-json --output-format stream-json --disable-slash-commands
//! <model-flags> [--json-schema <staging>/reply-schema.json] --sandbox --settings
//! <staging>/settings.json`, with one NDJSON user turn streamed on stdin
//! (`{"event":"user","message":{"content":"..."}}`). Streaming the prompt on stdin avoids OS
//! argv ceilings on large Change Sets. Events arrive as NDJSON on stdout: `init` carries the
//! effective model and conversation ID, `step_update` reports turn progress, and `result`
//! carries `status`, `structured_output`, `response`, `error`, and `usage`.
//!
//! [`ANTIGRAVITY_INVOCATION`] lists every option that command passes. af reads `agy --help`
//! before any paid dispatch and refuses a CLI that does not declare every one of them, instead of
//! dropping sandbox or settings isolation flags (ADR-0146).
//!
//! Security flags (`--sandbox` and `--settings <staging>/settings.json`) are adapter-owned and
//! derived strictly from [`af_runner::task::WorkerAccess`], following Antigravity's
//! `deny > ask > allow` precedence. They follow package model flags so packages and candidate
//! repositories cannot widen tool, shell, URL, or MCP authority (ADR-0042, ADR-0118).
//!
//! The package argument parser applies the static rules of [`af_runner::models`], the one
//! validation authority for model and effort spellings (ADR-0148): an unversioned model or an
//! effort level outside `low medium high xhigh max` is refused with a hint naming the rule and
//! `af provider models` before any process starts.
//!
//! **Token mapping, recorded:** `agy` reports `usage.input_tokens` excluding `cache_read_tokens`
//! (confirmed empirically against `agy 1.5.18` `stream-json` step and result totals). Chargeable
//! usage is therefore uncached `input_tokens` plus `output_tokens` plus separate
//! `thinking_tokens` when `total_tokens` includes them, with `cache_read_tokens` excluded. Missing
//! or malformed usage is recorded as unknown, never estimated (ADR-0143).

pub mod task;

use std::path::Path;

use af_contracts::{Arg, Command};
use af_runner::cli::{CliFlag, CliInvocation};
use af_runner::models;

pub(crate) fn antigravity_model_flags(runner: &Command) -> Result<Vec<String>, String> {
    let values = runner.resolve().map_err(|error| error.to_string())?;
    let mut model = None;
    let mut effort = None;
    let mut index = 0;
    while index < values.len() {
        let option = &values[index];
        let value = values
            .get(index + 1)
            .ok_or_else(|| format!("Antigravity model option `{option}` has no value"))?;
        let slot = match option.as_str() {
            "--model" => &mut model,
            "--effort" => &mut effort,
            _ => {
                return Err(format!(
                    "unsupported Antigravity package argument `{option}`; packages may set only one --model and one --effort"
                ));
            }
        };
        if slot.replace(value.clone()).is_some() {
            return Err(format!("duplicate Antigravity package option `{option}`"));
        }
        index += 2;
    }
    let checked = match (&model, &effort) {
        (Some(model), _) => {
            models::check_static(models::Kind::Antigravity, model, effort.as_deref())
        }
        (None, Some(effort)) => models::check_effort(models::Kind::Antigravity, effort),
        (None, None) => Ok(()),
    };
    checked.map_err(|refusal| {
        refusal.hint(
            models::ProviderRef {
                id: None,
                kind: models::Kind::Antigravity,
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

/// Every option an Antigravity Attempt's command passes, in the spelling it passes it: what an
/// installed `agy` must declare in `agy --help` before af dispatches to it (ADR-0146). A unit
/// test holds this list equal to what [`antigravity_command`] emits, so it cannot drift from the
/// invocation. `--sandbox`, `--settings`, and `--disable-slash-commands` are required isolation
/// flags: a CLI without them is refused, never run without them.
pub const ANTIGRAVITY_INVOCATION: CliInvocation = CliInvocation {
    program: "agy",
    help_args: &["--help"],
    flags: &[
        CliFlag::long("--input-format"),
        CliFlag::long("--output-format"),
        CliFlag::long("--disable-slash-commands"),
        CliFlag::long("--model"),
        CliFlag::long("--effort"),
        CliFlag::long("--json-schema"),
        CliFlag::long("--sandbox"),
        CliFlag::long("--settings"),
    ],
    upgrade: "agy update",
};

/// The `agy` command of one Attempt. `schema` is the file holding the reply's strict output
/// schema ([`af_runner::task::reply_schema::ReplyContract::strict`]) when the Worker declares a
/// typed reply with a strict projection. Security flags (`--sandbox` and `--settings`) are
/// adapter-owned and deliberately follow package model flags.
pub(crate) fn antigravity_command(
    program: &str,
    model_flags: &[String],
    settings_file: &Path,
    schema_file: Option<&Path>,
) -> Command {
    let mut args = vec![
        Arg::literal("--input-format"),
        Arg::literal("stream-json"),
        Arg::literal("--output-format"),
        Arg::literal("stream-json"),
        Arg::literal("--disable-slash-commands"),
    ];
    args.extend(model_flags.iter().map(Arg::literal));
    if let Some(schema) = schema_file {
        args.extend([
            Arg::literal("--json-schema"),
            Arg::literal(schema.display().to_string()),
        ]);
    }
    args.extend([
        Arg::literal("--sandbox"),
        Arg::literal("--settings"),
        Arg::literal(settings_file.display().to_string()),
    ]);
    Command::new(program, args)
}

#[cfg(test)]
mod invocation_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// [`ANTIGRAVITY_INVOCATION`] is the single source of truth for what `agy --help` must
    /// declare: every option any Attempt's command can emit is in it, and nothing else is.
    #[test]
    fn the_required_flag_list_is_exactly_what_the_command_builder_emits() {
        let model_flags = antigravity_model_flags(&Command::new(
            "agy",
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        ))
        .unwrap();
        let mut emitted = BTreeSet::new();
        for schema in [None, Some(Path::new("/staging/reply-schema.json"))] {
            let command = antigravity_command(
                "agy",
                &model_flags,
                Path::new("/staging/settings.json"),
                schema,
            );
            let argv = command.resolve().unwrap();
            for value in &argv {
                if value.starts_with('-') {
                    assert!(
                        ANTIGRAVITY_INVOCATION.declares(value),
                        "`{value}` is emitted but not required of the installed CLI"
                    );
                    emitted.insert(value.clone());
                }
            }
        }
        let required: BTreeSet<String> = ANTIGRAVITY_INVOCATION
            .flags
            .iter()
            .map(|flag| flag.spelling.to_string())
            .collect();
        assert_eq!(emitted, required, "a required flag is never emitted");
        assert_eq!(ANTIGRAVITY_INVOCATION.help_args, ["--help"]);
        assert!(ANTIGRAVITY_INVOCATION.declares("--sandbox"));
        assert!(ANTIGRAVITY_INVOCATION.declares("--settings"));
        assert!(ANTIGRAVITY_INVOCATION.declares("--disable-slash-commands"));
    }

    #[test]
    fn model_flags_enforce_static_rules_and_reject_extra_options() {
        let ok = antigravity_model_flags(&Command::new(
            "agy",
            ["--model", "gemini-4-neon-high", "--effort", "xhigh"]
                .map(Arg::literal)
                .into(),
        ))
        .unwrap();
        assert_eq!(ok, ["--model", "gemini-4-neon-high", "--effort", "xhigh"]);

        let bad_effort = antigravity_model_flags(&Command::new(
            "agy",
            ["--model", "gemini-4-neon-high", "--effort", "ultra"]
                .map(Arg::literal)
                .into(),
        ))
        .unwrap_err();
        assert!(
            bad_effort.contains("Antigravity accepts: low, medium, high, xhigh, max"),
            "{bad_effort}"
        );

        let bad_arg = antigravity_model_flags(&Command::new(
            "agy",
            ["--dangerously-skip-permissions", "true"]
                .map(Arg::literal)
                .into(),
        ))
        .unwrap_err();
        assert!(
            bad_arg.contains("unsupported Antigravity package argument"),
            "{bad_arg}"
        );
    }
}
