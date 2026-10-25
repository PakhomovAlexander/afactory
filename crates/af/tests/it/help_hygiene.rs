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
        [
            "repo",
            "pipeline",
            "state",
            "campaign",
            "policy-rev",
            "base",
            "candidate",
            "uncommitted",
            "focus",
            "heavy",
            "restart-round",
            "provider",
            "provider-admission-tokens",
            "provider-admission-wall-ms",
            "timeout-secs",
            "git-timeout-secs",
            "json",
        ],
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
fn review_help_groups_its_commands_by_lifecycle() {
    let help = out(&af(&["review", "--help"]));
    assert_eq!(
        between(
            &help,
            "Plan, token-free and without Campaign state:",
            "  gc                    Reclaim Campaign state: list what would go; remove it only with --apply"
        ),
        [
            "Plan, token-free and without Campaign state:",
            "  plan                  Show what a review would run, token-free, without Campaign state",
            "  render                Compose one Worker's exact input, token-free, without Campaign state",
            "",
            "Run:",
            "  run                   Execute the pipeline and fold results into the ledger",
            "",
            "Inspect a Campaign:",
            "  ledger                List the ledger: findings, dispositions, and open demands",
            "  show                  Show one ledger entry by key",
            "  report                Render the Campaign report",
            "  export                Export a verified patch proposal, by id or by the finding it fixes",
            "",
            "Work its findings and demands:",
            "  resolve               Resolve a finding without a fix: rejected, or tracked as wontfix",
            "  attest-change         Attest that a region changed in response to a finding",
            "  verify-fix            Record a verification of an attested fix",
            "  challenge-resolution  Challenge a non-fixed resolution",
            "  group                 Group one finding under another",
            "  ungroup               Undo a grouping",
            "  evidence              Attach or apply evidence to a demand",
            "  demand                Waive a demand",
            "  policy-time           Advance the Campaign's policy clock",
            "",
            "Campaigns and maintenance:",
            "  campaigns             List Campaigns under the state root",
            "  gc                    Reclaim Campaign state: list what would go; remove it only with --apply",
        ]
    );
    assert!(
        help.contains("Run options (`af review [OPTIONS]` is `af review run [OPTIONS]`): see `af review run --help`."),
        "{help}"
    );
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
            "  4. af review plan --policy-rev HEAD --base HEAD --uncommitted   see what a review would run",
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
        help.trim_end()
            .ends_with("explicitly requests convergence review, and repeat that explicit mode when resuming it."),
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

    // The plan's admission wall in seconds, its inputs in units; the document keeps both raw.
    let plan = out(&in_repo(&[
        "review",
        "plan",
        "--policy-rev",
        "HEAD",
        "--base",
        "HEAD~1",
        "--candidate",
        "HEAD",
    ]));
    assert!(
        plan.contains("provider admission  32768 tokens, 45.0 s per capability"),
        "{plan}"
    );
    assert!(
        plan.lines()
            .filter(|line| line.starts_with("input    "))
            .all(|line| line.contains(" KiB (about ") && !line.contains(" bytes")),
        "{plan}"
    );
    let document: serde_json::Value = serde_json::from_slice(
        &in_repo(&[
            "review",
            "plan",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD~1",
            "--candidate",
            "HEAD",
            "--json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(document["provider_admission"]["wall_ms"], 45000);
    assert!(document["subject"]["patch_bytes"].as_u64().unwrap() > 0);
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
    // `af/task-list-entry@3` -> `task-list-entry-v3.json`.
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
