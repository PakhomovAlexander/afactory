//! The `af/error@2` document a command prints on stdout under `--json` when it fails
//! (`af help json`, `schemas/error-v2.json`): the command, the error sentence and the exit code,
//! and for a refusal af classifies, a `cause` from a closed vocabulary, the one command to run
//! next and the facts behind the cause, so a script branches on fields rather than on prose.
//! Version 2 adds `interrupted`, a command SIGINT or SIGTERM stopped (ADR-0256).

use serde_json::{Map, Value, json};

/// The document's `schema`.
pub(crate) const ERROR_V2: &str = "af/error@2";

/// Why a classified refusal was refused: the `cause` of its document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    /// The command line was wrong (exit 2).
    Usage,
    /// A node of the Task used up its recovery allowance, so `af task run` cannot continue it.
    RecoveryExhausted,
    /// A refreshed source left the Task without a Pipeline that can run it now (exit 4).
    RefreshRefused,
    /// A field of the Task file holds a value the runtime refuses.
    TaskFileInvalid,
    /// A file the command names cannot be opened or read.
    InputUnreadable,
    TaskNotFound,
    NoTaskCatalog,
    NoAuthority,
    NotRepository,
    ProviderNotRegistered,
    UnknownReviewNode,
    PlanMismatch,
    DirtySource,
    ReviewSelectorsMissing,
    ReviewBindingMissing,
    /// SIGINT or SIGTERM stopped the command (exit 130 or 143).
    Interrupted,
}

impl Cause {
    /// Every cause, as `schemas/error-v2.json` lists them.
    pub(crate) const ALL: [Self; 16] = [
        Self::Usage,
        Self::RecoveryExhausted,
        Self::RefreshRefused,
        Self::TaskFileInvalid,
        Self::InputUnreadable,
        Self::TaskNotFound,
        Self::NoTaskCatalog,
        Self::NoAuthority,
        Self::NotRepository,
        Self::ProviderNotRegistered,
        Self::UnknownReviewNode,
        Self::PlanMismatch,
        Self::DirtySource,
        Self::ReviewSelectorsMissing,
        Self::ReviewBindingMissing,
        Self::Interrupted,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::RecoveryExhausted => "recovery_exhausted",
            Self::RefreshRefused => "refresh_refused",
            Self::TaskFileInvalid => "task_file_invalid",
            Self::InputUnreadable => "input_unreadable",
            Self::TaskNotFound => "task_not_found",
            Self::NoTaskCatalog => "no_task_catalog",
            Self::NoAuthority => "no_authority",
            Self::NotRepository => "not_repository",
            Self::ProviderNotRegistered => "provider_not_registered",
            Self::UnknownReviewNode => "unknown_review_node",
            Self::PlanMismatch => "plan_mismatch",
            Self::DirtySource => "dirty_source",
            Self::ReviewSelectorsMissing => "review_selectors_missing",
            Self::ReviewBindingMissing => "review_binding_missing",
            Self::Interrupted => "interrupted",
        }
    }

    pub(crate) fn explanation(self) -> &'static str {
        match self {
            Self::Usage => {
                "The command line has an invalid or missing argument. Read this command's --help."
            }
            Self::RecoveryExhausted => {
                "A Task node has no recovery Attempts left. Start a new Task."
            }
            Self::RefreshRefused => {
                "No Pipeline can run the refreshed Task source. Inspect the selection."
            }
            Self::TaskFileInvalid => {
                "A Task file field has a value af cannot accept. Correct that field."
            }
            Self::InputUnreadable => {
                "A named input file cannot be read. Check its path and permissions."
            }
            Self::TaskNotFound => {
                "The selected Task Store has no Task with that ID. List its Tasks."
            }
            Self::NoTaskCatalog => {
                "The selected committed Authority Snapshot has no `.af/task-catalog.toml`; review and commit a local catalog, then select that commit. af catalog init --repo .. --profile software --destination af-demo creates a starter."
            }
            Self::NoAuthority => {
                "The selected commit has no .af/ authority. Run af onboard --apply, review and commit its output, then select that commit."
            }
            Self::NotRepository => {
                "The selected path is not a Git repository. Choose a repository with a commit."
            }
            Self::ProviderNotRegistered => {
                "The Provider ID is absent from this machine's registry. List or register Providers."
            }
            Self::UnknownReviewNode => {
                "The named node is not a model Worker in this review pipeline. Inspect the plan's nodes."
            }
            Self::PlanMismatch => {
                "The --confirm-plan digest differs from the Task's captured plan. Inspect and confirm the current plan."
            }
            Self::DirtySource => {
                "The operation requires a clean source checkout. Inspect and resolve its changes."
            }
            Self::ReviewSelectorsMissing => {
                "Review needs explicit trusted policy and Subject selectors. Use the suggested command when authority is unambiguous."
            }
            Self::ReviewBindingMissing => {
                "A model Worker needs an explicit Provider binding. Use the suggested command only when each kind has exactly one enabled Provider."
            }
            Self::Interrupted => {
                "SIGINT or SIGTERM stopped the command. Run the next action to resume or continue the Task it was running."
            }
        }
    }
}

/// The list and individual explanations share the exact cause table used in documents.
pub(crate) fn explain(cause: Option<&str>) -> Result<String, String> {
    match cause {
        None => {
            let mut lines = Vec::new();
            for cause in Cause::ALL {
                let prefix = format!("  {:<26} ", cause.as_str());
                let mut line = prefix.clone();
                for word in cause.explanation().split_whitespace() {
                    if line.len() + word.len() + usize::from(!line.ends_with(' ')) > 100 {
                        lines.push(line.trim_end().to_owned());
                        line = " ".repeat(prefix.len());
                    }
                    if !line.ends_with(' ') {
                        line.push(' ');
                    }
                    line.push_str(word);
                }
                lines.push(line);
            }
            Ok(lines.join("\n"))
        }
        Some(name) => Cause::ALL
            .iter()
            .find(|cause| cause.as_str() == name)
            .map(|cause| format!("{}: {}", cause.as_str(), cause.explanation()))
            .ok_or_else(|| format!("unknown error cause {name}; run af help errors")),
    }
}

/// What a classified refusal carries besides its sentence.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Detail {
    pub(crate) cause: Cause,
    /// One exact `af` command, with the Store selectors the command was given.
    pub(crate) next_action: Option<String>,
    /// The facts behind the cause, by name (`node`, `count`, `allowance`, ...).
    pub(crate) details: Map<String, Value>,
}

impl Detail {
    pub(crate) fn new(cause: Cause) -> Self {
        Self {
            cause,
            next_action: None,
            details: Map::new(),
        }
    }

    pub(crate) fn next(mut self, command: impl Into<String>) -> Self {
        self.next_action = Some(command.into());
        self
    }

    pub(crate) fn with(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.details.insert(name.to_owned(), value.into());
        self
    }
}

/// The detail of the refusal this process is failing with, beside the sentence it was noted
/// for: a command's errors travel to `main` as sentences, and the document is printed there.
static NOTED: std::sync::Mutex<Option<(String, Detail)>> = std::sync::Mutex::new(None);

/// `message`, with `detail` noted for the `af/error@2` document `main` prints when this error
/// ends the command.
pub(crate) fn typed(message: String, detail: Detail) -> String {
    *NOTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((message.clone(), detail));
    message
}

/// The detail noted for the error `main` reports, once: only when `error` still carries the
/// sentence it was noted for, so a detail never describes another error.
pub(crate) fn take(error: &str) -> Option<Detail> {
    let noted = NOTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()?;
    error.contains(&noted.0).then_some(noted.1)
}

/// The longest `next_action` `schemas/error-v2.json` admits, in characters.
pub(crate) const MAX_NEXT_ACTION: usize = 4096;

/// Whether `command` can be a document's `next_action`, a line a person pastes: an `af`
/// command of at most [`MAX_NEXT_ACTION`] characters with no control character (a tab or a
/// newline in a Store selector, say), as `schemas/error-v2.json` requires.
pub(crate) fn pasteable(command: &str) -> bool {
    command.len() > "af ".len()
        && command.starts_with("af ")
        && command.chars().count() <= MAX_NEXT_ACTION
        && !command.chars().any(char::is_control)
}

/// One `af/error@2` document. A `next_action` no pasted line can carry ([`pasteable`]) is
/// null, whichever writer composed it (ADR-0256).
pub(crate) fn document(
    command: &str,
    error: &str,
    exit_code: i32,
    detail: Option<&Detail>,
) -> Value {
    let mut document = json!({
        "schema": ERROR_V2,
        "command": command,
        "error": error,
        "exit_code": exit_code,
    });
    if let Some(detail) = detail {
        document["cause"] = json!(detail.cause.as_str());
        document["next_action"] = json!(
            detail
                .next_action
                .as_deref()
                .filter(|command| pasteable(command))
        );
        if !detail.details.is_empty() {
            document["details"] = Value::Object(detail.details.clone());
        }
    }
    document
}

/// The usage error clap found in `argv` as its document, when `argv` asks for `--json`: clap's
/// own message, its exit code (2) and `cause: usage`. `None` for `--help` and `--version`,
/// which are not errors, and for a command line without `--json`, whose output is clap's alone.
pub(crate) fn usage(argv: &[String], error: &clap::Error) -> Option<Value> {
    use clap::error::ErrorKind;
    if !argv.iter().skip(1).any(|word| word == "--json")
        || matches!(
            error.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        )
    {
        return None;
    }
    let command = command_path(argv);
    let message = if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        format!("`{command}` needs a subcommand; `{command} --help` lists them")
    } else {
        let rendered = error.render().to_string();
        let rendered = rendered.trim();
        rendered
            .strip_prefix("error: ")
            .unwrap_or(rendered)
            .to_owned()
    };
    let detail = Detail::new(Cause::Usage).next(format!("{command} --help"));
    Some(document(
        &command,
        &message,
        error.exit_code(),
        Some(&detail),
    ))
}

/// The command a command line names, as far as its words are subcommands: `af task start`.
fn command_path(argv: &[String]) -> String {
    use clap::CommandFactory as _;
    let mut command = crate::cli::Af::command();
    let mut path = vec!["af".to_owned()];
    for word in argv.iter().skip(1) {
        if word.starts_with('-') {
            continue;
        }
        let Some(subcommand) = command.find_subcommand(word).cloned() else {
            break;
        };
        path.push(subcommand.get_name().to_owned());
        command = subcommand;
    }
    path.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory as _, Parser as _};

    fn schema_causes() -> Vec<String> {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schemas/error-v2.json")).unwrap();
        schema["properties"]["cause"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cause| cause.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn the_schema_lists_every_cause() {
        assert_eq!(
            schema_causes(),
            Cause::ALL.map(|cause| cause.as_str().to_owned())
        );
    }

    /// #331 item 10, every writer: a `next_action` no pasted line carries (a Store selector
    /// with a tab or a newline, or a line past 4096 characters, which `--state ./././...`
    /// reaches) is null in the document, which then validates against `error-v2.json`, as the
    /// interrupted Task branch's `af task run ID --state DIR` does; one that fits is kept.
    #[test]
    fn a_next_action_no_line_can_carry_is_null_and_the_document_validates() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schemas/error-v2.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let interrupted = |next: &str| {
            let mut detail = Detail::new(Cause::Interrupted)
                .with("signal", "SIGINT")
                .with("task", "interrupted-ux");
            detail.next_action = Some(next.to_owned());
            document("af task", "interrupted by SIGINT", 130, Some(&detail))
        };
        let long = format!(
            "af task run interrupted-ux --state {}../state",
            "./".repeat(2100)
        );
        for next in [
            "af task run interrupted-ux --state '../state\twith-tab'",
            "af task run interrupted-ux --state '../state\nwith-newline'",
            "af task run interrupted-ux --state '../state\u{7f}'",
            long.as_str(),
            "af ",
            "rm -rf x",
        ] {
            let document = interrupted(next);
            assert_eq!(document["next_action"], Value::Null, "{next:?}");
            assert!(validator.is_valid(&document), "{document}");
        }
        let at_limit = format!("af task run t --state {}", "x".repeat(4096 - 22));
        assert_eq!(at_limit.chars().count(), MAX_NEXT_ACTION);
        for next in ["af task run interrupted-ux --state '../a b'", &at_limit] {
            let document = interrupted(next);
            assert_eq!(document["next_action"], json!(next));
            assert!(validator.is_valid(&document), "{document}");
        }
        let past = format!("{at_limit}x");
        assert_eq!(interrupted(&past)["next_action"], Value::Null);
        assert!(!validator.is_valid(&json!({
            "schema": "af/error@2", "command": "af task", "error": "e", "exit_code": 130,
            "cause": "interrupted", "next_action": past,
            "details": {"signal": "SIGINT", "task": null}
        })));
    }

    #[test]
    fn every_cause_has_the_same_explanation_in_the_list_and_topic() {
        let list = explain(None).unwrap();
        let words = list.split_whitespace().collect::<Vec<_>>().join(" ");
        for cause in Cause::ALL {
            let one = explain(Some(cause.as_str())).unwrap();
            assert!(one.contains(cause.explanation()), "{one}");
            assert!(words.contains(cause.explanation()), "{list}");
        }
    }

    #[test]
    fn a_noted_detail_belongs_only_to_its_own_error() {
        let detail = Detail::new(Cause::RecoveryExhausted)
            .next("af task start --file TASK_FILE")
            .with("node", "root.nodes.implement")
            .with("count", 2)
            .with("allowance", 1);
        let message = typed("cannot continue Task t: ...".into(), detail.clone());
        assert_eq!(take(&format!("--from: {message}")), Some(detail.clone()));
        assert_eq!(take(&message), None, "taken once");
        typed(message, detail);
        assert_eq!(take("another error"), None);
        let document = document("af task", "e", 1, None);
        assert_eq!(
            document,
            json!({"schema": "af/error@2", "command": "af task", "error": "e", "exit_code": 1})
        );
    }

    /// Every leaf command that takes `--json`, given a flag it does not know: one document,
    /// exit 2, clap's message. `--help` and `--version` stay what they are.
    #[test]
    fn every_json_command_reports_a_usage_error_as_one_document() {
        fn leaves(command: &clap::Command, path: Vec<String>, out: &mut Vec<Vec<String>>) {
            if command.get_arguments().any(|arg| arg.get_id() == "json") {
                out.push(path.clone());
            }
            for subcommand in command.get_subcommands() {
                let mut child = path.clone();
                child.push(subcommand.get_name().to_owned());
                leaves(subcommand, child, out);
            }
        }
        let mut paths = Vec::new();
        leaves(&crate::cli::Af::command(), Vec::new(), &mut paths);
        assert!(paths.len() > 40, "{paths:?}");
        for path in paths {
            let argv: Vec<String> = ["af"]
                .into_iter()
                .map(str::to_owned)
                .chain(path.iter().cloned())
                .chain(["--json".to_owned(), "--no-such-flag".to_owned()])
                .collect();
            let error = crate::cli::Af::try_parse_from(&argv).unwrap_err();
            let document = usage(&argv, &error).unwrap_or_else(|| panic!("{argv:?}: {error}"));
            assert_eq!(document["schema"], "af/error@2", "{argv:?}");
            assert_eq!(document["exit_code"], 2, "{argv:?}");
            assert_eq!(document["cause"], "usage", "{argv:?}");
            assert_eq!(
                document["command"],
                format!("af {}", path.join(" ")).trim_end(),
                "{argv:?}"
            );
            let message = document["error"].as_str().unwrap();
            assert!(!message.starts_with("error:"), "{message}");
            assert!(
                error
                    .render()
                    .to_string()
                    .contains(message.lines().next().unwrap()),
                "{argv:?}: {message}"
            );
            let mut without = argv.clone();
            without.retain(|word| word != "--json");
            let error = crate::cli::Af::try_parse_from(&without).unwrap_err();
            assert!(usage(&without, &error).is_none(), "{without:?}");
            let mut asked = argv[..argv.len() - 1].to_vec();
            asked.push("--help".to_owned());
            let shown = crate::cli::Af::try_parse_from(&asked).unwrap_err();
            assert!(usage(&asked, &shown).is_none(), "{asked:?}");
        }
        let argv: Vec<String> = ["af", "--version", "--json"].map(str::to_owned).to_vec();
        if let Err(error) = crate::cli::Af::try_parse_from(&argv) {
            assert!(usage(&argv, &error).is_none());
        }
    }
}
