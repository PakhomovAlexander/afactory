//! The `af/error@1` document a command prints on stdout under `--json` when it fails
//! (`af help json`, `schemas/error-v1.json`): the command, the error sentence and the exit code,
//! and for a refusal af classifies, a `cause` from a closed vocabulary, the one command to run
//! next and the facts behind the cause, so a script branches on fields rather than on prose.

use serde_json::{Map, Value, json};

/// The document's `schema`.
pub(crate) const ERROR_V1: &str = "af/error@1";

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
}

impl Cause {
    /// Every cause, as `schemas/error-v1.json` lists them.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::Usage,
        Self::RecoveryExhausted,
        Self::RefreshRefused,
        Self::TaskFileInvalid,
        Self::InputUnreadable,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::RecoveryExhausted => "recovery_exhausted",
            Self::RefreshRefused => "refresh_refused",
            Self::TaskFileInvalid => "task_file_invalid",
            Self::InputUnreadable => "input_unreadable",
        }
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

/// `message`, with `detail` noted for the `af/error@1` document `main` prints when this error
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

/// One `af/error@1` document.
pub(crate) fn document(
    command: &str,
    error: &str,
    exit_code: i32,
    detail: Option<&Detail>,
) -> Value {
    let mut document = json!({
        "schema": ERROR_V1,
        "command": command,
        "error": error,
        "exit_code": exit_code,
    });
    if let Some(detail) = detail {
        document["cause"] = json!(detail.cause.as_str());
        document["next_action"] = json!(detail.next_action);
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
            serde_json::from_str(include_str!("../../../schemas/error-v1.json")).unwrap();
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
            json!({"schema": "af/error@1", "command": "af task", "error": "e", "exit_code": 1})
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
            assert_eq!(document["schema"], "af/error@1", "{argv:?}");
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
