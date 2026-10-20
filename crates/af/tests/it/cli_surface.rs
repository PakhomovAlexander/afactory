//! The CLI surface: scoped help, topics, completions, and the configuration ladder. Dispatch and
//! `af self` live in `self_managed.rs`.

use std::path::Path;
use std::process::Output;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: &str = env!("AF_TARGET");

fn af(args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn namespace_help_is_scoped_to_the_namespace() {
    let help = af(&["provider", "--help"]);
    assert!(help.status.success());
    let text = out(&help);
    assert!(text.contains("status"), "{text}");
    assert!(text.contains("models"), "{text}");
    assert!(text.contains("doctor"), "{text}");
    assert!(
        !text.contains("ledger"),
        "provider help leaked review commands: {text}"
    );
    assert!(!text.contains("onboard"), "{text}");

    let bare = af(&["provider"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(err(&bare).contains("Usage: af provider"), "{}", err(&bare));

    let root = af(&[]);
    assert_eq!(root.status.code(), Some(2));
    assert!(err(&root).contains("review"), "{}", err(&root));
}

#[test]
fn usage_errors_name_only_the_failing_command() {
    let bad = af(&["review", "ledger", "--campaign", "x", "--nope"]);
    assert_eq!(bad.status.code(), Some(2));
    let text = err(&bad);
    assert!(text.contains("--nope"), "{text}");
    assert!(text.contains("Usage: af review ledger"), "{text}");
    assert!(!text.contains("attest-change"), "{text}");
}

#[test]
fn help_topics_and_command_paths() {
    let layers = af(&["help", "layers"]);
    assert!(layers.status.success());
    assert!(out(&layers).contains("directory"), "{}", out(&layers));

    let ledger = af(&["help", "review", "ledger"]);
    assert!(ledger.status.success());
    assert!(out(&ledger).contains("--campaign"), "{}", out(&ledger));

    let unknown = af(&["help", "nonsense"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(err(&unknown).contains("topics:"), "{}", err(&unknown));
}

/// The rendered `af review run` help for `--timeout-secs`, with wrapping collapsed to single spaces.
fn review_run_timeout_help() -> Vec<String> {
    [
        af(&["review", "run", "--help"]),
        af(&["review", "run", "-h"]),
        af(&["help", "review", "run"]),
    ]
    .iter()
    .map(|help| {
        assert!(help.status.success(), "{}", err(help));
        let text = out(help).split_whitespace().collect::<Vec<_>>().join(" ");
        let start = text
            .find("--timeout-secs <N>")
            .expect("--timeout-secs is documented");
        let end = text[start..]
            .find("--git-timeout-secs")
            .map_or(text.len(), |offset| start + offset);
        text[start..end].to_owned()
    })
    .collect()
}

#[test]
fn review_run_timeout_is_per_reviewer_attempt_for_campaigns() {
    for flag in review_run_timeout_help() {
        assert!(
            flag.contains("Without --file (default routed run, --campaign or --pipeline): per reviewer Attempt (default 1800;"),
            "{flag}"
        );
        assert!(
            flag.contains("pinned in the Campaign manifest as reviewer_timeout_seconds"),
            "{flag}"
        );
        assert!(flag.contains("not a bound on the whole run"), "{flag}");
        assert!(
            !flag.contains("Wall-clock budget for the whole run"),
            "{flag}"
        );
    }
}

#[test]
fn review_run_timeout_caps_the_whole_task_for_task_files() {
    for flag in review_run_timeout_help() {
        assert!(
            flag.contains("With --file: a cap on the whole Task, the lower of this flag and the file's limits.wall_ms"),
            "{flag}"
        );
    }
}

#[test]
fn version_is_a_line_or_a_document() {
    let line = af(&["--version"]);
    assert_eq!(out(&line).trim(), format!("af {VERSION}"));
    let json: serde_json::Value =
        serde_json::from_slice(&af(&["--version", "--json"]).stdout).unwrap();
    assert_eq!(json["version"], VERSION);
    assert_eq!(json["target"], TARGET);
    assert!(json["commit"].is_string());
}

#[test]
fn completion_scripts_delegate_to_the_binary() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let script = af(&["completions", shell]);
        assert!(script.status.success(), "{shell}: {}", err(&script));
        assert!(
            out(&script).contains("COMPLETE"),
            "{shell}: {}",
            out(&script)
        );
    }
    let dynamic = crate::common::af()
        .env("COMPLETE", "fish")
        .env("AF_SELF_OFFLINE", "1")
        .args(["--", "af", "provider", ""])
        .output()
        .unwrap();
    assert!(dynamic.status.success(), "{}", err(&dynamic));
    let text = out(&dynamic);
    assert!(text.contains("status"), "{text}");
    assert!(text.contains("models"), "{text}");
    assert!(text.contains("doctor"), "{text}");
    assert!(!text.contains("ledger"), "{text}");
}

#[test]
fn man_pages_render_for_every_command_and_topic() {
    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    for page in [
        "af.1",
        "af-review.1",
        "af-review-run.1",
        "af-self.1",
        "af-layers.7",
    ] {
        assert!(dir.path().join(page).is_file(), "{page} missing");
    }
    let root = std::fs::read_to_string(dir.path().join("af.1")).unwrap();
    assert!(root.contains("review"));
}

/// `af task report` says what it prints, what it never does, and its selectors, in `--help`, in
/// `af help task report` and in its man page (ADR-0142).
#[test]
fn task_report_help_and_man_page_describe_the_command() {
    for help in [
        af(&["task", "report", "--help"]),
        af(&["help", "task", "report"]),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        for needle in [
            "Usage: af task report",
            "<TASK_ID>...",
            "<!-- af-task-report:v1 -->",
            "<!-- /af-task-report -->",
            "af/task-report@1",
            "Never:",
            "--repo <DIR>",
            "--state <DIR>",
            "--json",
            "af task report pagination-cli",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
    let task = out(&af(&["task", "--help"]));
    assert!(
        task.lines()
            .any(|line| line.trim_start().starts_with("report ")),
        "{task}"
    );
    let missing = af(&["task", "report"]);
    assert_eq!(missing.status.code(), Some(2), "a Task ID is required");

    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    let page = std::fs::read_to_string(dir.path().join("af-task-report.1")).unwrap();
    assert!(page.contains("af\\-task\\-report") || page.contains("af-task-report"));
    assert!(page.contains("TASK_ID"), "{page}");
    assert!(page.contains("Never"), "{page}");
}

/// `af provider models` says what it reads, what the list proves, its options and its exit codes,
/// in `--help`, in `af help provider models` and in its man page (ADR-0148).
#[test]
fn provider_models_help_and_man_page_describe_the_command() {
    for help in [
        af(&["provider", "models", "--help"]),
        af(&["help", "provider", "models"]),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "Usage: af provider models [OPTIONS] [ID]...",
            "--json",
            "--resolve <SPELLING[/EFFORT]>",
            "af/provider-models@1",
            "`list_models` control request",
            "`model/list` on a bare `codex app-server`",
            "`models_cache.json`",
            "af recognises these spellings for this Provider",
            "the charged `af provider doctor`",
            "reviewer.toml [runner] args, the worker.toml [runner] block and the bindings line",
            "Spends no model tokens and reads no credential.",
            "Exit codes: 0 every catalog listed, 4 a Provider CLI cannot start or is incompatible, 7 a catalog is unavailable",
            "af provider models claude-work --resolve 'Fable 5.1/xhigh'",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
    let provider = out(&af(&["provider", "--help"]));
    assert!(
        provider
            .lines()
            .any(|line| line.trim_start().starts_with("models ")),
        "{provider}"
    );
    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    let page = std::fs::read_to_string(dir.path().join("af-provider-models.1")).unwrap();
    assert!(page.contains("SPELLING"), "{page}");
    assert!(page.contains("list_models"), "{page}");
}

/// `af project` is a namespace of its own with long help and examples, listed in `af --help`,
/// and every `af task` command that takes `--state` takes `--workspace` beside it (ADR-0157).
#[test]
fn project_help_and_the_workspace_flag_describe_the_commands() {
    let root = out(&af(&["--help"]));
    assert!(
        root.lines()
            .any(|line| line.trim_start().starts_with("project ")),
        "{root}"
    );
    for help in [af(&["project", "--help"]), af(&["help", "project"])] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "Usage: af project <COMMAND>",
            "show",
            "workspaces",
            "workspace",
            "af/project-id@1\\0",
            "$XDG_STATE_HOME/af/project/<hex>/",
            "workspaces.toml",
            "git fetch --unshallow",
            "--workspace LABEL",
            "Never: writes inside a checkout, commits, pushes, or removes a Store.",
            "af/project@1",
            "af/error@1",
            "af project workspace new research --branch agent/research",
            "af project workspace remove research --delete-worktree",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
        assert!(!text.contains("ledger"), "{text}");
    }
    let workspace = out(&af(&["project", "workspace", "--help"]));
    for command in ["new", "add", "remove"] {
        assert!(
            workspace
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{command} "))),
            "{workspace}"
        );
    }
    let new = out(&af(&["project", "workspace", "new", "--help"]));
    let new = new.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "`git worktree add --no-checkout`",
        "no checkout, filter or hook of the repository runs on this machine",
        "Every check runs before anything is made",
        "inside `$XDG_STATE_HOME/af` is a usage error (exit 2)",
        "undone newest first",
    ] {
        assert!(new.contains(needle), "`{needle}` missing:\n{new}");
    }
    let bare = af(&["project"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(err(&bare).contains("Usage: af project"), "{}", err(&bare));
    for command in [
        &["task", "plan"][..],
        &["task", "start"],
        &["task", "run"],
        &["task", "show"],
        &["task", "list"],
        &["task", "explain"],
        &["task", "deliver"],
        &["task", "report"],
        &["task", "gc"],
    ] {
        let mut args = command.to_vec();
        args.push("--help");
        let help = out(&af(&args));
        assert!(help.contains("--workspace <LABEL>"), "{command:?}: {help}");
    }
    let both = af(&["task", "list", "--workspace", "w", "--state", "/tmp/state"]);
    assert_eq!(both.status.code(), Some(2));
    assert!(err(&both).contains("cannot be used with"), "{}", err(&both));

    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    for page in [
        "af-project.1",
        "af-project-show.1",
        "af-project-workspace-new.1",
    ] {
        assert!(dir.path().join(page).is_file(), "{page} missing");
    }
}

/// The Surveyor's commands (ADR-0158) are listed under `af project` with their documents, the
/// skip rule, the bound and the email rule, and each has its own manual page.
#[test]
fn project_survey_refresh_and_history_describe_the_surveyor() {
    let project = out(&af(&["project", "--help"]));
    for command in ["survey", "refresh", "history"] {
        assert!(
            project
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{command} "))),
            "{project}"
        );
    }
    let project = project.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "af/ProjectSurvey@2",
        "af/project-history@2",
        "`af onboard --apply` and on `af project refresh`",
        "never stores an author email",
        "survey key (head, refs, policy digest, Surveyor version) is unchanged",
        "af project refresh",
    ] {
        assert!(project.contains(needle), "`{needle}` missing:\n{project}");
    }
    let refresh = out(&af(&["project", "refresh", "--help"]));
    let refresh = refresh.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "unchanged survey key",
        "prints `unchanged: head …, refs …, policy …, surveyor …`",
        "--force",
        "`[survey] max_wall_ms`, checked at every item",
        "`max_survey_bytes`",
        "calls no Provider, runs no hook or project code and writes nothing inside the checkout",
        "first 16 hex digits of SHA-256 of the lowercased, trimmed email",
        "SurveyFailed@1",
        "AF_SURVEY__MAX_COMMITS=1000 af project refresh",
    ] {
        assert!(refresh.contains(needle), "`{needle}` missing:\n{refresh}");
    }
    let survey = out(&af(&["project", "survey", "--help"]));
    assert!(survey.contains("--json"), "{survey}");
    assert!(survey.contains("af project refresh"), "{survey}");
    let history = out(&af(&["project", "history", "--help"]));
    assert!(history.contains("af/project-history@2"), "{history}");

    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    for page in [
        "af-project-survey.1",
        "af-project-refresh.1",
        "af-project-history.1",
    ] {
        assert!(dir.path().join(page).is_file(), "{page} missing");
    }
}

/// `af telemetry`, its two commands and `af help telemetry` say what they print, what they never
/// do, their options and their exit codes, in `--help`, in `af help` and in the man pages
/// (ADR-0153, ADR-0146).
#[test]
fn telemetry_help_and_man_pages_describe_the_commands() {
    let namespace = out(&af(&["telemetry", "--help"]));
    for command in ["status ", "reindex "] {
        assert!(
            namespace
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "{namespace}"
        );
    }
    assert!(namespace.contains("ADR-0153"), "{namespace}");
    let bare = af(&["telemetry"]);
    assert_eq!(bare.status.code(), Some(2));
    for (command, schema) in [
        ("status", "af/telemetry-status@1"),
        ("reindex", "af/telemetry-reindex@1"),
    ] {
        for help in [
            af(&["telemetry", command, "--help"]),
            af(&["help", "telemetry", command]),
        ] {
            assert!(help.status.success(), "{}", err(&help));
            let text = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
            for needle in [
                &format!("Usage: af telemetry {command} [OPTIONS]"),
                "--json",
                "--dir <PATH>",
                schema,
                "Never:",
                "Exit codes: 0 success, 1",
                &format!("af telemetry {command} --json"),
            ] {
                assert!(text.contains(needle), "`{needle}` missing:\n{text}");
            }
        }
    }
    let topic = af(&["help", "telemetry"]);
    assert!(topic.status.success());
    let text = out(&topic);
    for needle in [
        "[telemetry]",
        "level = \"spans\"",
        "off | metrics | spans | content",
        "AF_TELEMETRY__<KEY>",
        "segment_max_bytes = \"256MiB\"",
        "sink_timeout_ms = 5000",
        "af telemetry status",
        "af telemetry reindex",
        "machine layers only",
    ] {
        assert!(text.contains(needle), "`{needle}` missing:\n{text}");
    }
    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    for page in [
        "af-telemetry.1",
        "af-telemetry-status.1",
        "af-telemetry-reindex.1",
        "af-telemetry.7",
    ] {
        assert!(dir.path().join(page).is_file(), "{page} missing");
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn the_configuration_ladder_merges_in_order_and_names_origins() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config = root.path().join("config");
    write(
        &config.join("af/config.toml"),
        "[self]\nauto_update = \"never\"\nchannel = \"stable\"\n",
    );
    write(
        &config.join("af/conf.d/10-extra.toml"),
        "[self]\nkeep_versions = 9\n",
    );
    let work = root.path().join("work");
    write(
        &work.join(".af/af.toml"),
        "[ui]\ncolor = \"never\"\n[defaults]\npipeline = \"shared\"\n",
    );
    let project = work.join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    // A `.git` the kernel accepts as a repository holds a HEAD.
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    write(
        &project.join(".af/af.toml"),
        "version = 1\n[defaults]\npipeline = \"review\"\n[self]\nsource = \"attacker/repo\"\n",
    );
    write(
        &project.join(".af/af.local.toml"),
        "[ui]\npager = \"less\"\n",
    );
    // Nothing below the toplevel is a layer.
    write(
        &project.join("sub/.af/af.toml"),
        "[ui]\ncolor = \"never-read\"\n",
    );

    let show = crate::common::af()
        .args(["config", "show", "--origin", "--repo"])
        .arg(project.join("sub"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("AF_SELF__CHECK_EVERY", "\"1h\"")
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(show.status.success(), "{}", err(&show));
    let text = out(&show);
    let line = |key: &str| {
        text.lines()
            .find(|line| line.starts_with(&format!("{key} = ")))
            .unwrap_or_else(|| panic!("{key} missing in\n{text}"))
            .to_string()
    };
    let check = |key: &str, value: &str, origin: &str| {
        let found = line(key);
        assert!(
            found.contains(value) && found.contains(origin),
            "{key}: expected {value} from {origin}, got `{found}` in\n{text}"
        );
    };
    check("self.auto_update", "\"never\"", "# user");
    check("self.keep_versions", "= 9", "# user");
    check("ui.color", "\"never\"", "# directory");
    check("defaults.pipeline", "\"review\"", "# project");
    check("ui.pager", "\"less\"", "# local");
    check("self.check_every", "\"1h\"", "# environment");
    check("self.install_pins", "true", "# built-in");
    check("self.source", "PakhomovAlexander", "# built-in");
    assert!(
        text.contains("# ignored: [self] in") && text.contains("proj/.af/af.toml (project layer)"),
        "{text}"
    );
    assert!(!text.contains("never-read"), "{text}");
    assert!(line("ui.color").contains("work/.af/af.toml:2"), "{text}");

    let json: serde_json::Value = serde_json::from_slice(
        &crate::common::af()
            .args(["config", "show", "--json", "--repo"])
            .arg(&project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(json["effective"]["self"]["keep_versions"], 9);
    assert_eq!(
        json["effective"]["self"]["source"],
        "PakhomovAlexander/afactory"
    );
    assert_eq!(json["origins"]["ui.color"]["layer"], "directory");
    assert_eq!(json["ignored"].as_array().unwrap().len(), 1);
    let layers: Vec<&str> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["layer"].as_str().unwrap())
        .collect();
    assert!(
        layers.starts_with(&["system", "user", "user"]),
        "{layers:?}"
    );
    assert!(
        layers.ends_with(&["directory", "project", "local"]),
        "{layers:?}"
    );
}

#[test]
fn json_errors_are_one_document_on_stdout() {
    let failed = crate::common::af()
        .args(["config", "show", "--json"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    let document: serde_json::Value = serde_json::from_slice(&failed.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", out(&failed)));
    assert_eq!(document["schema"], "af/error@1");
    assert!(
        document["error"].as_str().unwrap().contains("HOME"),
        "{document}"
    );
    assert_eq!(document["exit_code"], 1);
}
