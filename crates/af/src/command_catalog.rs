//! The command catalog `af help --json` prints: `af/command-catalog@1` (ADR-0247).
//!
//! The document is generated, never written by hand: every command, argument, default and
//! usage line comes from the clap definition in `cli.rs` as the binary parses it, and the one
//! thing clap cannot say, what a command may do, comes from [`EFFECTS`], one entry per runnable
//! command. A unit test fails when a runnable command has no entry, when an entry names a command
//! that does not exist, and when an entry's condition names a flag its command does not take.

use clap::{ArgAction, Command};
use serde_json::{Value, json};

pub(crate) const SCHEMA: &str = "af/command-catalog@1";

/// Whether a command may spend model tokens.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Tokens {
    /// It never calls a model.
    Free,
    /// It may dispatch model Workers that charge tokens against a budget; when the condition is
    /// given, only then.
    Charges(When),
}

/// State a command may write, beyond the housekeeping every command shares (ADR-0247).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// A Task Store or a review Campaign's Store: the durable record of a Task or a Campaign.
    Store,
    /// Files outside af's own state: in the repository, a worktree, a branch or a directory or
    /// file the command names.
    Files,
    /// af's machine-local state: the Provider and Project registries and Project Stores,
    /// configuration, installed releases, shell completions and man pages, telemetry and caches.
    Machine,
    /// Something off this machine: the gate branches and draft pull request of a Remote Check.
    Remote,
}

/// What a command may need from a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// A person completes a Provider's official login, at a private terminal or through the
    /// host's private channel.
    Login,
    /// A developer's signature over an exact decision payload, made outside af.
    Approval,
    /// An interactive terminal: the command opens a screen or an editor.
    Terminal,
}

/// The condition under which an effect applies: `None` always, else the flag as typed (`--apply`)
/// or a short phrase that names one.
pub(crate) type When = Option<&'static str>;

/// What one runnable command may do. An effect is what the command may do, never a promise
/// that it will: a refused command does none of them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Effects {
    pub(crate) tokens: Tokens,
    pub(crate) writes: &'static [(State, When)],
    pub(crate) human: &'static [(Need, When)],
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Files => "files",
            Self::Machine => "machine",
            Self::Remote => "remote",
        }
    }
}

impl Need {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Approval => "approval",
            Self::Terminal => "terminal",
        }
    }
}

use Need::{Approval, Login, Terminal};
use State::{Files, Machine, Remote, Store};

/// Reads only: no tokens, no state, no person.
const READS: Effects = Effects {
    tokens: Tokens::Free,
    writes: &[],
    human: &[],
};

const fn writes(writes: &'static [(State, When)]) -> Effects {
    Effects {
        tokens: Tokens::Free,
        writes,
        human: &[],
    }
}

/// A pipeline whose check node lists `remote_checks` pushes its gate branches (ADR-0140).
const REMOTE_CHECKS: When = Some("a pipeline with remote checks");

/// `af review run --file` (and bare `af review --file`) executes a Review Task, whose pipeline's
/// remote checks push gate branches and a draft gate pull request like `af task run`'s.
const REVIEW_TASK_REMOTE_CHECKS: When = Some("--file, a pipeline with remote checks");

/// `codex app-server` answers `model/list` against the Provider's `CODEX_HOME` and caches the
/// catalog in `$CODEX_HOME/models_cache.json` (ADR-0146).
const CODEX_MODELS: When = Some("a Codex Provider");

/// Every runnable command and what it may do, by the command as typed. A runnable command is a
/// visible one without subcommands, or a namespace that also runs on its own (`af`, `af review`,
/// `af storage`).
pub(crate) const EFFECTS: &[(&str, Effects)] = &[
    (
        "af",
        Effects {
            human: &[(Terminal, Some("without a command"))],
            ..READS
        },
    ),
    ("af provider detect", writes(&[(Machine, Some("--apply"))])),
    ("af provider disable", writes(&[(Machine, None)])),
    ("af provider enable", writes(&[(Machine, None)])),
    ("af provider status", READS),
    ("af provider models", writes(&[(Machine, CODEX_MODELS)])),
    (
        "af provider setup",
        Effects {
            human: &[(Login, Some("--login"))],
            ..writes(&[(Machine, None)])
        },
    ),
    ("af provider add", writes(&[(Machine, None)])),
    ("af provider remove", writes(&[(Machine, None)])),
    (
        "af provider auth begin",
        Effects {
            human: &[(Login, None)],
            ..writes(&[(Machine, None)])
        },
    ),
    ("af provider auth status", READS),
    ("af provider auth cancel", writes(&[(Machine, None)])),
    ("af provider recover", writes(&[(Machine, None)])),
    (
        "af provider doctor",
        Effects {
            tokens: Tokens::Charges(None),
            ..writes(&[(Store, None)])
        },
    ),
    (
        "af onboard",
        writes(&[
            (Files, Some("--apply")),
            (Files, Some("--refresh-lock")),
            (Machine, Some("--apply")),
        ]),
    ),
    (
        "af review",
        Effects {
            tokens: Tokens::Charges(None),
            ..writes(&[(Store, None), (Remote, REVIEW_TASK_REMOTE_CHECKS)])
        },
    ),
    (
        "af review run",
        Effects {
            tokens: Tokens::Charges(None),
            ..writes(&[(Store, None), (Remote, REVIEW_TASK_REMOTE_CHECKS)])
        },
    ),
    ("af review plan", writes(&[(Store, Some("--file"))])),
    ("af review render", READS),
    ("af review ledger", READS),
    ("af review show", READS),
    ("af review export", READS),
    ("af review report", READS),
    ("af review campaigns", READS),
    ("af review gc", writes(&[(Store, Some("--apply"))])),
    ("af review resolve", writes(&[(Store, None)])),
    ("af review attest-change", writes(&[(Store, None)])),
    ("af review verify-fix", writes(&[(Store, None)])),
    ("af review challenge-resolution", writes(&[(Store, None)])),
    ("af review policy-time advance", writes(&[(Store, None)])),
    ("af review group", writes(&[(Store, None)])),
    ("af review ungroup", writes(&[(Store, None)])),
    ("af review evidence add", writes(&[(Store, None)])),
    ("af review evidence satisfy", writes(&[(Store, None)])),
    ("af review demand waive", writes(&[(Store, None)])),
    (
        "af task start",
        Effects {
            tokens: Tokens::Charges(Some("--execute")),
            ..writes(&[
                (Store, None),
                (Remote, Some("--execute, a pipeline with remote checks")),
            ])
        },
    ),
    ("af task plan", writes(&[(Store, None)])),
    ("af task refresh", writes(&[(Store, None)])),
    ("af task decision-payload", writes(&[(Files, None)])),
    ("af task output", writes(&[(Files, Some("--output"))])),
    (
        "af task approve",
        Effects {
            human: &[(Approval, None)],
            ..writes(&[(Store, None)])
        },
    ),
    (
        "af task reject",
        Effects {
            human: &[(Approval, None)],
            ..writes(&[(Store, None)])
        },
    ),
    (
        "af task run",
        Effects {
            tokens: Tokens::Charges(None),
            ..writes(&[(Store, None), (Remote, REMOTE_CHECKS)])
        },
    ),
    ("af task wait", READS),
    ("af task status", READS),
    ("af task events", READS),
    ("af task cancel", writes(&[(Store, None)])),
    ("af task findings", READS),
    ("af task materialize", writes(&[(Files, None)])),
    ("af task diff", READS),
    ("af task explain", READS),
    ("af task export", writes(&[(Files, None)])),
    ("af task deliver", writes(&[(Files, None), (Store, None)])),
    ("af task observe-adoption", writes(&[(Store, None)])),
    ("af task list", READS),
    ("af task gc", writes(&[(Store, Some("--apply"))])),
    ("af task show", READS),
    ("af task report", READS),
    ("af self status", writes(&[(Machine, Some("--refresh"))])),
    (
        "af self update",
        writes(&[(Machine, Some("without --check"))]),
    ),
    (
        "af self channel",
        writes(&[(Machine, Some("with a channel"))]),
    ),
    ("af self rollback", writes(&[(Machine, None)])),
    ("af self install", writes(&[(Machine, None)])),
    ("af self remove", writes(&[(Machine, None)])),
    ("af self prune", writes(&[(Machine, None)])),
    (
        "af self setup-shell",
        writes(&[(Machine, Some("--write")), (Machine, Some("--remove"))]),
    ),
    (
        "af self uninstall",
        writes(&[(Machine, None), (Store, Some("--purge"))]),
    ),
    (
        "af self optimize",
        Effects {
            tokens: Tokens::Charges(Some("--execute")),
            ..writes(&[(Store, None)])
        },
    ),
    ("af doctor", READS),
    ("af help", READS),
    ("af schema show", READS),
    ("af project show", READS),
    ("af project workspaces", READS),
    ("af project survey", READS),
    ("af project refresh", writes(&[(Machine, None)])),
    ("af project history", READS),
    (
        "af project workspace new",
        writes(&[(Files, None), (Machine, None)]),
    ),
    ("af project workspace add", writes(&[(Machine, None)])),
    (
        "af project workspace remove",
        writes(&[(Machine, None), (Files, Some("--delete-worktree"))]),
    ),
    ("af project brief", READS),
    ("af setup agents", writes(&[(Files, Some("--write"))])),
    ("af catalog init", writes(&[(Files, None)])),
    ("af catalog test", READS),
    ("af catalog digest", READS),
    ("af catalog sync", writes(&[(Files, None)])),
    ("af config show", READS),
    (
        "af config edit",
        Effects {
            human: &[(Terminal, None)],
            ..writes(&[
                (Machine, Some("--layer user")),
                (Files, Some("--layer directory, project or local")),
            ])
        },
    ),
    ("af config paths", READS),
    ("af storage", READS),
    (
        "af storage prune",
        writes(&[(Machine, Some("--apply")), (Store, Some("--apply"))]),
    ),
    ("af telemetry status", READS),
    ("af telemetry reindex", writes(&[(Machine, None)])),
    ("af telemetry runs", READS),
    ("af telemetry show", READS),
    ("af telemetry turns", READS),
    ("af telemetry tools", READS),
    ("af telemetry context", READS),
    ("af telemetry reasoning", READS),
    ("af telemetry search", READS),
    ("af telemetry config", READS),
    ("af telemetry tail", READS),
    ("af telemetry ledger", READS),
    ("af telemetry gc", writes(&[(Machine, Some("--apply"))])),
    ("af completions", READS),
];

/// The command tree as the binary parses it, global arguments propagated.
fn tree() -> Command {
    use clap::CommandFactory as _;
    let mut root = crate::cli::Af::command();
    root.build();
    root
}

/// The visible subcommands of `command`.
fn visible(command: &Command) -> impl Iterator<Item = &Command> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
}

/// The arguments of `command` a person can type: not hidden, not `--help`.
fn arguments(command: &Command) -> impl Iterator<Item = &clap::Arg> {
    command.get_arguments().filter(|argument| {
        !argument.is_hide_set()
            && !matches!(
                argument.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            )
    })
}

/// A visible command without subcommands, or a namespace that takes arguments of its own and
/// does not require a subcommand.
fn runnable(command: &Command) -> bool {
    visible(command).next().is_none()
        || (!command.is_subcommand_required_set() && arguments(command).next().is_some())
}

/// Every visible command below and including `command`, depth first in definition order, each
/// with its words after `af`.
fn walk<'a>(command: &'a Command, path: Vec<String>, into: &mut Vec<(Vec<String>, &'a Command)>) {
    into.push((path.clone(), command));
    for subcommand in visible(command) {
        let mut child = path.clone();
        child.push(subcommand.get_name().to_owned());
        walk(subcommand, child, into);
    }
}

fn typed(path: &[String]) -> String {
    std::iter::once("af")
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Everything wrong between a command tree and an effects table: a runnable command without an
/// entry, an entry that names no runnable command or is listed twice, and a condition naming a
/// flag its command does not take.
#[cfg(test)]
fn problems(root: &Command, table: &[(&str, Effects)]) -> Vec<String> {
    let mut commands = Vec::new();
    walk(root, Vec::new(), &mut commands);
    let mut problems = Vec::new();
    for (path, command) in &commands {
        let name = typed(path);
        if runnable(command) && !table.iter().any(|(entry, _)| *entry == name) {
            problems.push(format!("{name}: runnable, without an effects entry"));
        }
    }
    for (index, (entry, effects)) in table.iter().enumerate() {
        if table[..index].iter().any(|(earlier, _)| earlier == entry) {
            problems.push(format!("{entry}: listed twice"));
        }
        let Some((_, command)) = commands
            .iter()
            .find(|(path, command)| typed(path) == *entry && runnable(command))
        else {
            problems.push(format!("{entry}: an effects entry for no runnable command"));
            continue;
        };
        let longs: Vec<String> = arguments(command)
            .filter_map(|argument| argument.get_long().map(|long| format!("--{long}")))
            .collect();
        let charges = match effects.tokens {
            Tokens::Charges(when) => vec![when],
            Tokens::Free => Vec::new(),
        };
        let conditions = charges
            .into_iter()
            .chain(effects.writes.iter().map(|(_, when)| *when))
            .chain(effects.human.iter().map(|(_, when)| *when))
            .flatten();
        for condition in conditions {
            for flag in condition
                .split([' ', ','])
                .filter(|word| word.starts_with("--"))
            {
                if !longs.iter().any(|long| long == flag) {
                    problems.push(format!("{entry}: `{condition}` names {flag}, not its flag"));
                }
            }
        }
    }
    problems
}

fn when(condition: When) -> Value {
    condition.map_or(Value::Null, |condition| json!(condition))
}

fn effects_json(effects: &Effects) -> Value {
    let (tokens, charges_when) = match effects.tokens {
        Tokens::Free => ("free", Value::Null),
        Tokens::Charges(condition) => ("charges", when(condition)),
    };
    json!({
        "tokens": tokens,
        "charges_when": charges_when,
        "writes": effects.writes.iter().map(|(state, condition)| json!({
            "state": state.as_str(),
            "when": when(*condition),
        })).collect::<Vec<_>>(),
        "human": effects.human.iter().map(|(need, condition)| json!({
            "need": need.as_str(),
            "when": when(*condition),
        })).collect::<Vec<_>>(),
    })
}

fn text(styled: Option<&clap::builder::StyledStr>) -> Value {
    styled.map_or(Value::Null, |styled| json!(styled.to_string().trim()))
}

fn argument_json(argument: &clap::Arg) -> Value {
    // A flag has no value: clap's own placeholder and `false` default for it are not shown.
    let takes_value = argument.get_action().takes_values();
    let value_names: Vec<String> = match argument.get_value_names() {
        _ if !takes_value => Vec::new(),
        Some(names) => names.iter().map(ToString::to_string).collect(),
        None => vec![argument.get_id().as_str().to_uppercase()],
    };
    let default: Vec<String> = if takes_value {
        argument
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    } else {
        Vec::new()
    };
    let possible: Vec<String> = argument
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect();
    json!({
        "name": argument.get_id().as_str(),
        "long": argument.get_long().map(|long| format!("--{long}")),
        "short": argument.get_short().map(|short| format!("-{short}")),
        "positional": argument.is_positional(),
        "value_names": value_names,
        "takes_value": takes_value,
        "repeatable": matches!(argument.get_action(), ArgAction::Append | ArgAction::Count),
        "required": argument.is_required_set(),
        "global": argument.is_global_set(),
        "default": default,
        "possible_values": possible,
        "env": argument.get_env().map(|env| env.to_string_lossy().into_owned()),
        "heading": argument.get_help_heading(),
        "help": text(argument.get_help().or_else(|| argument.get_long_help())),
    })
}

fn command_json(path: &[String], command: &Command) -> Value {
    let name = typed(path);
    let usage = command.clone().render_usage().to_string();
    let usage: Vec<&str> = usage
        .trim()
        .trim_start_matches("Usage:")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let effects = runnable(command).then(|| {
        let (_, effects) = EFFECTS
            .iter()
            .find(|(entry, _)| *entry == name)
            .unwrap_or_else(|| panic!("{name} has no effects entry (command_catalog tests)"));
        effects_json(effects)
    });
    json!({
        "command": name,
        "path": path,
        "about": text(command.get_about()),
        "aliases": command.get_visible_aliases().collect::<Vec<_>>(),
        "usage": usage,
        "runnable": runnable(command),
        "subcommands": visible(command).map(Command::get_name).collect::<Vec<_>>(),
        "arguments": arguments(command).map(argument_json).collect::<Vec<_>>(),
        "effects": effects,
    })
}

/// `af help [COMMAND...] --json`: the catalog of the command `words` names, as typed (an alias
/// is the command it names), and every visible command below it; the whole tree without words.
/// A word that names no visible command is refused with the sentence `af help` prints.
pub(crate) fn document(words: &[String]) -> Result<Value, String> {
    let root = tree();
    let mut command = &root;
    let mut path = Vec::new();
    for word in words {
        match visible(command).find(|subcommand| {
            subcommand.get_name() == word || subcommand.get_all_aliases().any(|alias| alias == word)
        }) {
            Some(subcommand) => {
                path.push(subcommand.get_name().to_owned());
                command = subcommand;
            }
            None => {
                return Err(format!(
                    "no command {}; run af help --json for every command",
                    crate::terminal_text(&typed(words))
                ));
            }
        }
    }
    let mut commands = Vec::new();
    walk(command, path.clone(), &mut commands);
    Ok(json!({
        "schema": SCHEMA,
        "af_version": env!("CARGO_PKG_VERSION"),
        "scope": typed(&path),
        "commands": commands
            .iter()
            .map(|(path, command)| command_json(path, command))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runnable_command_has_one_effects_entry_and_every_entry_names_one() {
        assert_eq!(problems(&tree(), EFFECTS), Vec::<String>::new());
    }

    /// The check above is not vacuous: a command missing from the table, an entry for a command
    /// that does not exist and a condition with a flag the command does not take each fail it.
    #[test]
    fn a_missing_entry_an_unknown_command_and_an_unknown_flag_are_problems() {
        let without: Vec<(&str, Effects)> = EFFECTS
            .iter()
            .copied()
            .filter(|(entry, _)| *entry != "af task run")
            .collect();
        assert_eq!(
            problems(&tree(), &without),
            ["af task run: runnable, without an effects entry"]
        );
        let mut extra = EFFECTS.to_vec();
        extra.push(("af task nope", READS));
        extra.push(("af task", READS));
        extra.push(("af doctor", writes(&[(Files, Some("--apply"))])));
        assert_eq!(
            problems(&tree(), &extra),
            [
                "af task nope: an effects entry for no runnable command",
                "af task: an effects entry for no runnable command",
                "af doctor: listed twice",
                "af doctor: `--apply` names --apply, not its flag",
            ]
        );
    }

    #[test]
    fn the_catalog_is_generated_from_the_command_tree() {
        let document = document(&[]).unwrap();
        assert_eq!(document["schema"], SCHEMA);
        assert_eq!(document["scope"], "af");
        let commands = document["commands"].as_array().unwrap();
        let mut walked = Vec::new();
        let root = tree();
        walk(&root, Vec::new(), &mut walked);
        assert_eq!(commands.len(), walked.len());
        let runnable = commands
            .iter()
            .filter(|command| command["runnable"] == true)
            .count();
        assert_eq!(runnable, EFFECTS.len());
        for command in commands {
            assert_eq!(
                command["runnable"] == true,
                !command["effects"].is_null(),
                "{command}"
            );
        }
        let run = commands
            .iter()
            .find(|command| command["command"] == "af task run")
            .unwrap();
        assert_eq!(run["path"], json!(["task", "run"]));
        assert_eq!(run["effects"]["tokens"], "charges");
        let confirm = run["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|argument| argument["long"] == "--confirm-plan")
            .unwrap();
        assert_eq!(confirm["takes_value"], true);
        assert_eq!(confirm["value_names"], json!(["PLAN_ID"]));
        let hidden = commands
            .iter()
            .any(|command| command["command"] == "af self man");
        assert!(!hidden, "a hidden command is not in the catalog");
    }

    /// The `writes` of one command as the catalog prints them: `(state, when)` pairs.
    fn printed_writes(command: &str) -> Vec<(String, Value)> {
        let document = document(&[]).unwrap();
        let entry = document["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["command"] == command)
            .unwrap_or_else(|| panic!("{command} is in the catalog"))
            .clone();
        entry["effects"]["writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|write| {
                (
                    write["state"].as_str().unwrap().to_owned(),
                    write["when"].clone(),
                )
            })
            .collect()
    }

    /// For a Codex Provider, `af provider models` starts `codex app-server`, which caches the
    /// catalog in the Provider's `$CODEX_HOME/models_cache.json`: machine-local state.
    #[test]
    fn provider_models_writes_machine_state_for_a_codex_provider() {
        assert_eq!(
            printed_writes("af provider models"),
            [("machine".to_owned(), json!("a Codex Provider"))]
        );
    }

    /// `af review plan --file` opens a Task and records its proposed plan; without `--file` the
    /// plan is token-free and stateless.
    #[test]
    fn review_plan_writes_the_store_only_with_file() {
        assert_eq!(
            printed_writes("af review plan"),
            [("store".to_owned(), json!("--file"))]
        );
    }

    /// `af review run --file`, and bare `af review --file`, executes a Review Task whose pipeline's
    /// remote checks may push gate branches and a draft gate pull request (ADR-0140).
    #[test]
    fn review_run_with_file_and_remote_checks_writes_remote_state() {
        for command in ["af review run", "af review"] {
            assert_eq!(
                printed_writes(command),
                [
                    ("store".to_owned(), Value::Null),
                    (
                        "remote".to_owned(),
                        json!("--file, a pipeline with remote checks")
                    ),
                ],
                "{command}"
            );
        }
    }

    /// Delivery writes the delivered files and persists its preparation and terminal delivery
    /// records in the Task Store (ADR-0031).
    #[test]
    fn task_deliver_writes_files_and_the_store() {
        assert_eq!(
            printed_writes("af task deliver"),
            [
                ("files".to_owned(), Value::Null),
                ("store".to_owned(), Value::Null),
            ]
        );
    }
}
