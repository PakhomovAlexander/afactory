//! The help pages a person reads first, held as they print: `af help exit-codes` aligned from
//! its first row, `af provider doctor --help` with its own flags only, `af self --help` leading
//! with `status` and `update`, `af review --help` grouped by where a Campaign is in its life,
//! `af onboard --help` written for a person with the agent notes last, and figures in units in
//! human text, the raw figure only under `--json`.

use std::process::Output;

fn af(args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("NO_COLOR", "1")
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The lines of `text` from the one equal to `first` through the one equal to `last`.
fn between<'a>(text: &'a str, first: &str, last: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| *line == first)
        .unwrap_or_else(|| panic!("{first:?} in\n{text}"));
    let end = lines[start..]
        .iter()
        .position(|line| *line == last)
        .unwrap_or_else(|| panic!("{last:?} in\n{text}"));
    lines[start..=start + end].to_vec()
}

#[test]
fn exit_codes_and_environment_are_aligned_from_their_first_row() {
    let codes = out(&af(&["help", "exit-codes"]));
    assert_eq!(
        codes.lines().take(4).collect::<Vec<_>>(),
        [
            "af help exit-codes — What every exit code means",
            "",
            "  0    pass — the command succeeded; for `review run`, the Round passed",
            "  1    error — af could not do what was asked; stderr names the entity, the knob, and the fix",
        ]
    );
    let environment = out(&af(&["help", "environment"]));
    assert!(
        environment
            .lines()
            .nth(2)
            .unwrap()
            .starts_with("  AF_<TABLE>__<KEY>       override one configuration value"),
        "{environment}"
    );
}

#[test]
fn provider_doctor_shows_only_its_own_flags() {
    let help = out(&af(&["provider", "doctor", "--help"]));
    let flags: Vec<&str> = help
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("--"))
        .map(|rest| rest.split([' ', '<']).next().unwrap())
        .collect();
    assert_eq!(
        flags,
        ["confirm-plan", "repo", "state", "workspace", "json"],
        "{help}"
    );
}

#[test]
fn self_help_leads_with_status_and_update() {
    let help = out(&af(&["self", "--help"]));
    assert!(
        help.starts_with(
            "See, install, update, roll back, and remove af itself, or analyze project economics.\n\nStart with `af self status`"
        ),
        "{help}"
    );
    let commands: Vec<&str> = between(&help, "Commands:", "")
        .into_iter()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        commands,
        [
            "status",
            "update",
            "channel",
            "rollback",
            "install",
            "remove",
            "prune",
            "setup-shell",
            "uninstall",
            "optimize"
        ]
    );
}

#[test]
fn review_help_describes_the_one_native_task_lifecycle() {
    let help = out(&af(&["review", "--help"]));
    let commands: Vec<_> = between(&help, "Commands:", "")
        .into_iter()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        commands,
        [
            "run",
            "plan",
            "inspect",
            "show",
            "ledger",
            "report",
            "resume",
            "continue",
            "export",
            "decision-context",
            "decide"
        ]
    );
    assert!(help.contains("Campaign selectors and Review Pipeline configuration are retired"));
    assert!(help.contains("common native Task lifecycle"));
    assert!(help.contains("sealed proposed repair independently"));
}

#[test]
fn onboard_help_is_written_for_a_person_with_agent_notes_last() {
    let help = out(&af(&["onboard", "--help"]));
    assert_eq!(
        help.lines().take(9).collect::<Vec<_>>(),
        [
            "Set up a repository for af review: write the `.af/` policy a review runs under, then commit it. af",
            "reads review policy only from a commit, so nothing you have not reviewed and committed ever governs",
            "a review. Four steps:",
            "",
            "  1. af onboard          preview what would be written; token-free, writes nothing",
            "  2. af onboard --apply  write `.af/`, only where none exists; it never overwrites one",
            "  3. git add .af && git commit -m \"af review policy\"",
            "  4. af review plan --file ../review.json --base HEAD --uncommitted   see what a review would run",
            "",
        ]
    );
    let notes = help
        .find("Notes for agents and automation:")
        .expect("agent notes");
    for earlier in ["Usage: af onboard", "Options:", "Examples:"] {
        assert!(
            help.find(earlier).unwrap() < notes,
            "{earlier} after the notes"
        );
    }
    assert!(
        help.contains("the retired --heavy Campaign flag is refused."),
        "{help}"
    );
    assert!(
        help.trim_end().ends_with(
            "the recorded Task with its exact plan ID; its limits and accounting remain captured."
        ),
        "{help}"
    );
}

#[test]
fn human_text_prints_figures_in_units_and_json_keeps_them_raw() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let done = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}: {done:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Repo\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    let in_repo = |args: &[&str]| {
        crate::common::af()
            .args(args)
            .current_dir(&repo)
            .env("NO_COLOR", "1")
            .env("TZ", "UTC")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("AF_PROVIDERS_FILE", root.path().join("providers.toml"))
            .output()
            .unwrap()
    };
    let onboard = in_repo(&[
        "onboard",
        "--runner",
        "codex",
        "--gate",
        "check=true",
        "--apply",
    ]);
    assert!(onboard.status.success(), "{onboard:?}");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "policy"]);
    let auth = root.path().join("codex-auth");
    std::fs::create_dir_all(&auth).unwrap();
    let added = in_repo(&[
        "provider",
        "add",
        "codex-main",
        "--kind",
        "codex",
        "--auth-dir",
        auth.to_str().unwrap(),
        "--unverified",
    ]);
    assert!(added.status.success(), "{added:?}");

    // A Store's use is local time with its zone, to the minute; its size in units.
    let listed = out(&in_repo(&["storage"]));
    let project = listed
        .lines()
        .find(|line| line.starts_with("Project Stores"))
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(
        project.contains(" KiB") && project.contains(" UTC"),
        "{project}"
    );
    assert!(
        !project
            .split_whitespace()
            .any(|word| word.contains('T') && word.ends_with('Z')),
        "no RFC 3339 UTC stamp in human text: {project}"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&in_repo(&["storage", "--json"]).stdout).unwrap();
    let kind = document["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "project_store")
        .unwrap_or_else(|| panic!("{document}"));
    assert!(kind["newest_use_unix_ms"].as_u64().unwrap() > 0, "{kind}");
    assert!(kind["bytes"].as_u64().unwrap() > 0, "{kind}");

    // Native Review uses the same figure rendering as Task planning, with raw exact limits
    // in JSON. This fixture runs no Provider and creates no Attempt while planning.
    let native = root.path().join("native");
    let (repository, state) = crate::task_cli::fixture_named(&native, "review");
    let mut command = crate::common::af();
    let planned = command
        .current_dir(&repository)
        .args([
            "review",
            "plan",
            "--file",
            "review.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(planned.status.success(), "{planned:?}");
    let document: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    let deadline = document["plan"]["limits"]["deadline_unix_ms"]
        .as_u64()
        .unwrap();
    let opened = document["history"][0]["transition"]["now_unix_ms"]
        .as_u64()
        .unwrap();
    assert_eq!(
        document["clock"]["wall_ms"].as_u64().unwrap(),
        deadline - opened
    );
    assert_eq!(document["attempts"], 0);
    assert_eq!(
        document["graph"]["allowances"]["root.nodes.check"]["wall_ms_per_attempt"],
        5_000
    );
    let inspected = crate::common::af()
        .current_dir(&repository)
        .args(["review", "inspect", "review-cli", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    let text = out(&inspected);
    assert!(text.contains("Attempt wall 5 s"), "{text}");
}

/// Every help page — `af <path> --help` for every command and `af help <topic>` for every
/// topic — as a terminal 100 columns wide renders it: no line is wider. clap wraps what it
/// lays out; a topic's text and an example are held to the width as written.
#[test]
fn no_help_line_is_wider_than_100_columns() {
    let mut wide = Vec::new();
    let pages = crate::help_examples::pages();
    assert!(pages.len() > 100, "only {} pages", pages.len());
    for (page, text) in pages {
        for line in text.lines() {
            let width = line.chars().count();
            if width > 100 {
                wide.push(format!("{page}: {width} columns: {line}"));
            }
        }
    }
    assert!(wide.is_empty(), "{}", wide.join("\n"));
}

/// `af help json` names exactly the Task documents whose schema carries the Task's `state`,
/// and the documents it lists are the versions `schemas/` publishes.
#[test]
fn json_help_names_the_documents_that_carry_a_state() {
    let help = out(&af(&["help", "json"]));
    let schemas = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("schemas");
    // `af/task-list-entry@4` -> `task-list-entry-v4.json`.
    let file = |document: &str| {
        let (kind, version) = document.trim_start_matches("af/").split_once('@').unwrap();
        schemas.join(format!("{kind}-v{version}.json"))
    };
    let documents = |text: &str| -> Vec<String> {
        text.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':')
            .filter(|word| word.starts_with("af/task-") && word.contains('@'))
            .map(|word| word.trim_end_matches(['.', ')']).to_owned())
            .collect()
    };
    let section = help
        .split("The Task documents, each named by its `schema`:")
        .nth(1)
        .unwrap_or_else(|| panic!("{help}"));
    let (table, rest) = section.trim_start_matches('\n').split_once("\n\n").unwrap();
    let listed = documents(table);
    assert!(listed.len() >= 10, "{listed:?}");
    let with_state = documents(
        rest.split_once("carry the Task's `state`")
            .unwrap_or_else(|| panic!("{rest}"))
            .0,
    );
    for document in &listed {
        let path = file(document);
        let schema = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{document}: {}: {error}", path.display()));
        let has_state = schema.contains("\"state\"");
        assert_eq!(
            with_state.contains(document),
            has_state,
            "{document}: its schema {} a `state`; the help {} it\n{help}",
            if has_state { "has" } else { "has no" },
            if with_state.contains(document) {
                "names"
            } else {
                "omits"
            }
        );
    }
}

#[test]
fn task_default_help_uses_the_installed_operator_selection() {
    for topic in ["config", "task-file"] {
        let help = out(&af(&["help", topic]));
        assert!(help.contains("bugfix/default-codex"), "{help}");
        assert!(!help.contains("kernel/implementation-reviewed"), "{help}");
        if topic == "task-file" {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let catalog: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("fixtures/issue-workflows/selections.json")).unwrap(),
            )
            .unwrap();
            for selection in catalog["selections"].as_object().unwrap().keys() {
                assert!(help.contains(selection), "missing {selection}: {help}");
            }
            assert!(
                help.contains(catalog["verification"].as_str().unwrap()),
                "{help}"
            );
        }
    }
}
