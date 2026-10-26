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
fn a_shipped_build_refuses_the_calibration_loop_control() {
    // Only a laboratory calibration build has the loop (ADR-0181); every other build refuses
    // its control, whatever its value, before doing anything, and never measures without it.
    for value in ["0", "1000", "not-a-count"] {
        let refused = crate::common::af()
            .arg("--version")
            .env("AF_SELF_OFFLINE", "1")
            .env("AF_PERF_CALIBRATION_ITERATIONS", value)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2), "{value}: {}", err(&refused));
        assert!(out(&refused).is_empty(), "{value}: {}", out(&refused));
        assert!(
            err(&refused).contains("AF_PERF_CALIBRATION_ITERATIONS is set, but this build has no perf-calibration feature"),
            "{value}: {}",
            err(&refused)
        );
    }
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

/// The one Task state model has its own topic, every new flag is documented in its command's
/// `--help` (ADR-0146: a command's `--help` is its contract), and the exit codes a script
/// branches on are listed (P5).
#[test]
fn task_states_topic_and_the_state_flags_describe_the_commands() {
    let topic = af(&["help", "task-states"]);
    assert!(topic.status.success(), "{}", err(&topic));
    let text = out(&topic);
    for cause in review_core::task::state::TaskCauseV1::ALL {
        assert!(
            text.contains(cause.as_str()),
            "{} missing:\n{text}",
            cause.as_str()
        );
    }
    for outcome in review_core::task::state::TaskOutcomeV1::ALL {
        assert!(text.contains(outcome.as_str()), "{text}");
    }
    for needle in [
        "deadline starts when you run",
        "next: af task run ID --retry-gate",
        "af task show ID --checks",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    let unknown = af(&["help", "nonsense"]);
    assert!(err(&unknown).contains("task-states"), "{}", err(&unknown));
    let codes = out(&af(&["help", "exit-codes"]));
    for needle in [
        "5    gate_failed",
        "6    provider_failed",
        "7    expired",
        "3    changes_requested",
    ] {
        assert!(codes.contains(needle), "{needle}:\n{codes}");
    }
    let json = out(&af(&["help", "json"]));
    for needle in [
        "af/task-inspection@13",
        "af/task-checks@2",
        "af/task-list@3",
        "af/task-list-entry@3",
        "af/task-report@2",
        "af/task-selection@3",
        "af/error@1",
        "recovery_exhausted",
        "planning_diagnostic.required_limits",
    ] {
        assert!(json.contains(needle), "{needle}:\n{json}");
    }
    for (command, flags) in [
        (
            &["task", "show", "--help"][..],
            &["--checks", "af/task-checks@2", "next"][..],
        ),
        (&["task", "plan", "--help"], &["--fit", "--replace"]),
    ] {
        let help = af(command);
        assert!(help.status.success(), "{}", err(&help));
        for flag in flags {
            assert!(
                out(&help).contains(flag),
                "{command:?} lacks {flag}:\n{}",
                out(&help)
            );
        }
    }
    let root = out(&af(&["--help"]));
    assert!(root.contains("task-states"), "{root}");
    assert!(root.contains("5 gate failed"), "{root}");
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
            "af/task-report@2",
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

/// `af task run --retry-gate` (ADR-0205) says what it runs again, on what, and when it refuses,
/// in `--help` and in `af help task run`, and conflicts with `--confirm-plan`.
#[test]
fn task_run_help_describes_the_retry_of_a_failed_gate() {
    for help in [af(&["task", "run", "--help"]), af(&["help", "task", "run"])] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--retry-gate",
            "Run the failed gate of a finished Task again on the same sealed candidate",
            "a new check Attempt in the same round under the recorded plan",
            "nothing re-planned or re-admitted",
            "the Task continues into review when the check passes",
            "Refused when the Task's last failure was not a gate",
            "af task run TASK_ID --retry-gate",
        ] {
            assert!(flat.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
    let both = af(&[
        "task",
        "run",
        "t",
        "--retry-gate",
        "--confirm-plan",
        "sha256:00",
    ]);
    assert_eq!(both.status.code(), Some(2));
    assert!(err(&both).contains("cannot be used with"), "{}", err(&both));
}

/// `af self status --refresh` and the bounded, offline-aware `af self update --check` say what
/// they do in `--help` and in `af help self …` (findings S1, S2, E7).
#[test]
fn self_status_and_update_help_describe_the_release_check() {
    for help in [
        af(&["self", "status", "--help"]),
        af(&["help", "self", "status"]),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let flat = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--refresh",
            "Run the release check now, before printing, instead of scheduling it when due",
            "last attempt <time> failed: <reason>",
            "af self update --rc",
            "status schedules the detached check",
            "af/self-status@2",
        ] {
            assert!(flat.contains(needle), "`{needle}` missing:\n{flat}");
        }
    }
    for help in [
        af(&["self", "update", "--help"]),
        af(&["help", "self", "update"]),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let flat = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "exit 10 when a newer release exists, 0 when none does, 1 with the reason",
            "before any `gh auth login` hint",
            "AF_SELF_OFFLINE=1 it asks nothing, prints `offline: check skipped` and exits 0",
            "5 s to connect and 30 s to list releases",
        ] {
            assert!(flat.contains(needle), "`{needle}` missing:\n{flat}");
        }
    }
}

/// `af task run` says how an interrupted run resumes and what `--verbose` adds (ADR-0209), in
/// `--help` and in `af help task run`.
#[test]
fn task_run_help_describes_the_resume_of_an_interrupted_run() {
    for help in [af(&["task", "run", "--help"]), af(&["help", "task", "run"])] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--verbose",
            "Say on stderr how the run keeps the machine awake: the power assertion it holds on \
             macOS, or why it holds none",
            "resumes with `af task run TASK_ID`",
            "It waits for a dead run's lease to expire (at most 60 s)",
            "charged only the usage its Provider reported",
            "without using one of the node's Attempts",
            "unless its slot declares `recovery_attempts`",
            "the first line says `cannot continue Task TASK_ID` with the count and the fix",
            "# needs: a Task whose run was interrupted (`interrupted by SIGINT`) af task run TASK_ID",
        ] {
            assert!(flat.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
}

/// `af task run --verbose` and `af task start --execute --verbose` print every line of the
/// storage sweep; without it a Task command prints one summary line (finding C14).
#[test]
fn task_run_and_start_help_describe_verbose_sweeps() {
    for (words, needle) in [
        (
            &["task", "run", "--help"][..],
            "Print every line of the storage sweep that ends the run, not only its one summary \
             line (`storage: freed 1.2 GiB (2 warm keys)`)",
        ),
        (
            &["help", "task", "run"][..],
            "Print every line of the storage sweep that ends the run",
        ),
        (
            &["task", "start", "--help"][..],
            "With --execute: print every line of the storage sweep that ends the run, not only \
             its one summary line",
        ),
    ] {
        let help = af(words);
        assert!(help.status.success(), "{}", err(&help));
        let flat = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("--verbose"), "{flat}");
        assert!(flat.contains(needle), "`{needle}` missing:\n{flat}");
    }
    let preview = af(&["task", "start", "--file", "t.json", "--verbose"]);
    assert_eq!(preview.status.code(), Some(2), "{}", err(&preview));
    assert!(err(&preview).contains("--execute"), "{}", err(&preview));
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

/// `af provider detect` says what it scans, what it never reads, what `--apply` writes, its ID
/// rule and its exit codes, in `--help`, in `af help provider detect` and in its man page; `af
/// help providers` explains the registry and the commands in the order of first use; and the
/// `add`, `setup` and `status` changes of ADR-0206 are in their own help (ADR-0146).
#[test]
fn provider_detect_help_and_the_providers_topic_describe_the_commands() {
    for help in [
        af(&["provider", "detect", "--help"]),
        af(&["help", "provider", "detect"]),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "Usage: af provider detect [OPTIONS]",
            "--dir <PATH>",
            "--id <ID>",
            "--kind <KIND>",
            "--all",
            "--apply",
            "--json",
            "--identity",
            "~/.local/bin, ~/.claude/local (claude), /opt/homebrew/bin, /usr/local/bin, ~/.npm-global/bin, ~/.volta/bin and ~/.bun/bin",
            "No shell rc file, `npm prefix` or `nvm` is evaluated",
            "env (CLAUDE_CONFIG_DIR, CODEX_HOME), default (~/.claude, ~/.codex), sibling",
            "registry (every auth_dir already registered) and explicit (--dir)",
            "Shell aliases or functions that set CLAUDE_CONFIG_DIR or CODEX_HOME are not read",
            ".claude.json, .credentials.json or settings.json",
            "auth.json or config.toml",
            "listed `empty` and not probed unless --all",
            "Nothing is written without --apply",
            "A registry conflict stops it at that row with exit 5; the rows before it stay registered",
            "~/.config/claude-work is claude-work, ~/.codex-gridgain is codex-gridgain",
            "*-ambient is never generated",
            "af/provider-detect@3",
            "its `install` names the install command and documentation of each kind whose CLI was not found",
            "`next_action` the scan to run again once one is installed",
            "the default ~/.claude may instead have a ~/.claude.json file beside it",
            "Exit codes: 0 the scan completed, 1 unclassified failure, 2 usage error, 4 a found CLI cannot start or is incompatible, or a login was left unregistered because its CLI is off PATH (the rows are still printed), 5 a registry conflict stopped --apply.",
            "never registers a login whose CLI is off PATH",
            "the row suggests the retry with a free ID",
            "A registry that cannot be read refuses --apply (exit 1)",
            "af provider detect --apply",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
    let provider = out(&af(&["provider", "--help"]));
    assert!(
        provider
            .lines()
            .any(|line| line.trim_start().starts_with("detect ")),
        "{provider}"
    );
    assert!(provider.contains("af help providers"), "{provider}");

    let topic = af(&["help", "providers"]);
    assert!(topic.status.success(), "{}", err(&topic));
    let text = out(&topic);
    let order: Vec<usize> = [
        "af provider detect ",
        "af provider detect --apply",
        "af provider setup ID",
        "af provider add ID",
        "af provider status",
        "af provider doctor",
    ]
    .iter()
    .map(|needle| {
        text.find(needle)
            .unwrap_or_else(|| panic!("`{needle}`:\n{text}"))
    })
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    for needle in [
        "~/.config/af/providers.toml",
        "never an email",
        "--identity",
    ] {
        assert!(text.contains(needle), "`{needle}` missing:\n{text}");
    }
    assert!(out(&af(&["help", "json"])).contains("af/provider-detect@3"));
    assert!(out(&af(&["help"])).contains("providers"));

    let add = out(&af(&["provider", "add", "--help"]))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "--unverified",
        "refused with exit 6",
        "prints `warning: not verified here`",
        "6 not authenticated",
    ] {
        assert!(add.contains(needle), "`{needle}` missing:\n{add}");
    }
    let setup = out(&af(&["provider", "setup", "--help"]))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        setup.contains("only --login creates a missing one"),
        "{setup}"
    );
    let status = out(&af(&["provider", "status", "--help"]))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "--verbose",
        "holds no login marker is not probed",
        "--auth-dir DIR --login",
    ] {
        assert!(status.contains(needle), "`{needle}` missing:\n{status}");
    }

    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    let page = std::fs::read_to_string(dir.path().join("af-provider-detect.1")).unwrap();
    assert!(page.contains("identity"), "{page}");
    assert!(dir.path().join("af-providers.7").is_file());

    let dynamic = crate::common::af()
        .env("COMPLETE", "fish")
        .env("AF_SELF_OFFLINE", "1")
        .args(["--", "af", "provider", ""])
        .output()
        .unwrap();
    assert!(out(&dynamic).contains("detect"), "{}", out(&dynamic));
}

/// `af project` is a namespace of its own with long help and examples, listed in `af --help`,
/// and every `af task` command that takes `--state` takes `--workspace` beside it (ADR-0158).
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
            "af project workspace new triage --branch agent/triage",
            "af project workspace remove triage --delete-worktree",
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

/// The Surveyor's commands (ADR-0159) are listed under `af project` with their documents, the
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

/// `af project brief` (ADR-0160) is listed under `af project` with the Brief, says what it prints
/// and records, and has its own manual page.
#[test]
fn project_brief_describes_the_exact_bytes_a_port_carries() {
    let project = out(&af(&["project", "--help"]));
    assert!(
        project
            .lines()
            .any(|line| line.trim_start().starts_with("brief ")),
        "{project}"
    );
    let project = project.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "af/ProjectBrief@1",
        "`[brief.<name>]` variant",
        "af/project-brief-preview@1",
        "af project brief --variant orient",
    ] {
        assert!(project.contains(needle), "`{needle}` missing:\n{project}");
    }
    let brief = out(&af(&["project", "brief", "--help"]));
    let brief = brief.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "--variant <NAME>",
        "--survey <ID>",
        "--json",
        "exactly the canonical bytes a plan would bind",
        "`orient` (tree, vocabulary, decisions, checks, hot_paths in 8192 bytes",
        "last-declared first",
        "`outcome: none` with its reason (`variant_none`, `no_survey`), exit 0",
        "registers nothing, records nothing and writes nothing inside the checkout",
    ] {
        assert!(brief.contains(needle), "`{needle}` missing:\n{brief}");
    }
    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    assert!(dir.path().join("af-project-brief.1").is_file());
}

/// `af telemetry`, its commands and `af help telemetry` say what they print, what they never
/// do, their options and their exit codes, in `--help`, in `af help` and in the man pages
/// (ADR-0153, ADR-0146).
#[test]
fn telemetry_help_and_man_pages_describe_the_commands() {
    let namespace = out(&af(&["telemetry", "--help"]));
    for command in [
        "status ", "reindex ", "runs ", "show ", "turns ", "tools ", "config ", "tail ", "ledger ",
        "gc ",
    ] {
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
    // The readers of P2 and P3 (design §10): what they print, what they never do, their
    // documents.
    for (command, usage, schema, options) in [
        (
            "runs",
            "Usage: af telemetry runs [OPTIONS]",
            "af/telemetry-runs@1",
            ["--since <D>", "--kind <K>", "--dir <PATH>"].as_slice(),
        ),
        (
            "show",
            "Usage: af telemetry show [OPTIONS] <RUN>",
            "af/telemetry-show@1",
            [
                "--store <ID>",
                "--attempt <ID>",
                "--span <ID>",
                "--depth <N>",
                "--dir <PATH>",
            ]
            .as_slice(),
        ),
        (
            "config",
            "Usage: af telemetry config [OPTIONS] <RUN>",
            "af/TelemetryConfig@2",
            ["--store <ID>", "--execution <ID>", "--dir <PATH>", "<env>"].as_slice(),
        ),
        (
            "tail",
            "Usage: af telemetry tail [OPTIONS] [RUN]",
            "af/telemetry-tail@1",
            ["--store <ID>", "--dir <PATH>"].as_slice(),
        ),
        (
            "gc",
            "Usage: af telemetry gc [OPTIONS]",
            "af/telemetry-gc@1",
            ["--apply"].as_slice(),
        ),
        // P4 (design §7.5, §10): the ledger view.
        (
            "ledger",
            "Usage: af telemetry ledger [OPTIONS] <RUN>",
            "af/telemetry-ledger@1",
            ["--store <ID>", "--dir <PATH>"].as_slice(),
        ),
        // P6 (design §7.3, §10): the turns and tool calls of the harness streams.
        (
            "turns",
            "Usage: af telemetry turns [OPTIONS] <RUN>",
            "af/telemetry-turns@1",
            [
                "--store <ID>",
                "--attempt <ID>",
                "--dir <PATH>",
                "release decision",
            ]
            .as_slice(),
        ),
        (
            "tools",
            "Usage: af telemetry tools [OPTIONS] <RUN>",
            "af/telemetry-tools@1",
            ["--store <ID>", "--attempt <ID>", "--dir <PATH>", "tool set"].as_slice(),
        ),
        // P7 (design §9, §10): the content level read back.
        (
            "context",
            "Usage: af telemetry context [OPTIONS] <RUN> <ATTEMPT>",
            "af/telemetry-context@1",
            [
                "--store <ID>",
                "--raw",
                "--dir <PATH>",
                "gen_ai.content.input",
                "not captured",
            ]
            .as_slice(),
        ),
        (
            "reasoning",
            "Usage: af telemetry reasoning [OPTIONS] <RUN> <ATTEMPT>",
            "af/telemetry-reasoning@1",
            [
                "--store <ID>",
                "--dir <PATH>",
                "content_reasoning",
                "release decision",
            ]
            .as_slice(),
        ),
        (
            "search",
            "Usage: af telemetry search [OPTIONS] <TEXT>",
            "af/telemetry-search@1",
            [
                "--since <D>",
                "--run <RUN>",
                "--store <ID>",
                "--dir <PATH>",
                "content_fts",
                "content_index = false",
            ]
            .as_slice(),
        ),
    ] {
        for help in [
            af(&["telemetry", command, "--help"]),
            af(&["help", "telemetry", command]),
        ] {
            assert!(help.status.success(), "{}", err(&help));
            let text = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
            for needle in [
                usage,
                "--json",
                schema,
                "Never:",
                "Exit codes: 0 success, 1",
            ]
            .into_iter()
            .chain(options.iter().copied())
            {
                assert!(text.contains(needle), "`{needle}` missing:\n{text}");
            }
            assert!(text.contains(&format!("af telemetry {command}")), "{text}");
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
        "af telemetry runs",
        "af telemetry show",
        "af telemetry config",
        "af telemetry tail",
        "af telemetry ledger",
        "af telemetry turns",
        "af telemetry tools",
        "af telemetry context",
        "af telemetry reasoning",
        "af telemetry search",
        "af telemetry gc",
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
        "af-telemetry-runs.1",
        "af-telemetry-show.1",
        "af-telemetry-config.1",
        "af-telemetry-tail.1",
        "af-telemetry-ledger.1",
        "af-telemetry-turns.1",
        "af-telemetry-tools.1",
        "af-telemetry-context.1",
        "af-telemetry-reasoning.1",
        "af-telemetry-search.1",
        "af-telemetry-gc.1",
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
    check("self.install_pins", "\"ask\"", "# built-in");
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

/// `[task] rust_toolchain_policy` comes from the configuration ladder, a project's `.af/af.toml`
/// over the user's, and `AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` over both; `af config show
/// --origin` names where it came from (finding C1).
#[test]
fn the_rust_toolchain_policy_shows_where_it_came_from() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config = root.path().join("config");
    write(
        &config.join("af/config.toml"),
        "[task]\nrust_toolchain_policy = \"~/.config/af/rust-toolchain.toml\"\n",
    );
    let project = root.path().join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let show = |env: &[(&str, &str)]| {
        let mut command = crate::common::af();
        command
            .args(["config", "show", "--origin", "--repo"])
            .arg(&project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env_remove("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE");
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", err(&output));
        out(&output)
            .lines()
            .find(|line| line.starts_with("task.rust_toolchain_policy = "))
            .unwrap_or_else(|| panic!("missing in\n{}", out(&output)))
            .to_string()
    };
    let user = show(&[]);
    assert!(
        user.contains("\"~/.config/af/rust-toolchain.toml\"") && user.contains("# user "),
        "{user}"
    );
    write(
        &project.join(".af/af.toml"),
        "[task]\nrust_toolchain_policy = \"/operator/project-toolchain.toml\"\n",
    );
    let from_project = show(&[]);
    assert!(
        from_project.contains("/operator/project-toolchain.toml")
            && from_project.contains("# project ")
            && from_project.contains(".af/af.toml:2"),
        "{from_project}"
    );
    let variable = show(&[(
        "AF_TASK_RUST_TOOLCHAIN_POLICY_FILE",
        "/operator/variable.toml",
    )]);
    assert!(
        variable.contains("/operator/variable.toml")
            && variable.contains("# environment AF_TASK_RUST_TOOLCHAIN_POLICY_FILE"),
        "{variable}"
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

/// The flags and names this release adds to the surface, each where its command documents it.
#[test]
fn the_surface_documents_verbose_storage_canonical_markdown_and_the_provider_list_alias() {
    let collapsed = |args: &[&str]| {
        let help = af(args);
        assert!(help.status.success(), "{args:?}: {}", err(&help));
        out(&help).split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let storage = collapsed(&["storage", "--help"]);
    assert!(
        storage.contains("--verbose Also name every path that could not be measured and every Store left alone, and why"),
        "{storage}"
    );
    let prune = collapsed(&["storage", "prune", "--help"]);
    assert!(prune.contains("--verbose"), "{prune}");
    let output = collapsed(&["task", "output", "--help"]);
    for format in ["json", "markdown", "canonical-markdown", "human-markdown"] {
        assert!(
            output.contains(&format!(" {format} ")),
            "{format}: {output}"
        );
    }
    assert!(
        output.contains("An unknown --port is refused with the ports the Task's result has."),
        "{output}"
    );
    let provider = collapsed(&["provider", "status", "--help"]);
    assert!(
        provider.contains("`af provider list` is the same command."),
        "{provider}"
    );
    let doctor = collapsed(&["provider", "doctor", "--help"]);
    assert!(doctor.contains("--campaign <NAME>"), "{doctor}");
    assert!(!doctor.contains("--file"), "{doctor}");
    let machine = collapsed(&["doctor", "--help"]);
    assert!(
        machine.contains(
            "The rows fit 100 columns, or the terminal's width when narrower (at least 60)"
        ),
        "{machine}"
    );
    // GA second fix item 4: a fix is a command, what it cannot say is its hint.
    assert!(
        machine.contains(
            "--json prints one `af/doctor@2` document, each check with its `fix` and `hint`."
        ),
        "{machine}"
    );
    assert!(
        machine.contains("A problem's `fix:` is the one command that fixes it, as a shell runs it"),
        "{machine}"
    );
    // The pointer for `af review tui` is no command of the namespace.
    let review = collapsed(&["review", "--help"]);
    assert!(!review.contains(" tui "), "{review}");
}

/// Bare `af` without a terminal keeps its usage on stderr and exit 2, after two first-run lines
/// (ADR-0210).
#[test]
fn bare_af_without_a_terminal_prints_the_first_run_hints() {
    let bare = crate::common::af()
        .env("AF_SELF_OFFLINE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(2));
    assert!(out(&bare).is_empty(), "{}", out(&bare));
    let text = err(&bare);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..2],
        [
            "af: no command given, and the browser opens only when stdout is a terminal",
            "first run? af provider detect · af onboard · af help start",
        ],
        "{text}"
    );
    assert!(text.contains("Usage: af"), "{text}");
}

/// `af --help` opens with what af is and the four steps, lists the commands once in the order a
/// first run needs them, and every topic it names resolves; `af help` lists every topic once
/// with the registry's description (ADR-0146, ADR-0210).
#[test]
fn root_help_has_one_command_list_and_every_named_topic_resolves() {
    let help = out(&af(&["--help"]));
    assert!(
        help.starts_with("af runs model and command Workers in sandboxes"),
        "{help}"
    );
    assert!(
        help.contains("Start here (`af help start` explains each step):"),
        "{help}"
    );
    for step in [
        "1. af provider detect",
        "2. af onboard",
        "3. git add .af && git commit",
        "4. af review plan --policy-rev HEAD --base HEAD --uncommitted",
        "af doctor",
    ] {
        assert!(help.contains(step), "`{step}` missing:\n{help}");
    }
    assert!(
        !help.contains("Namespaces:"),
        "a second command list:\n{help}"
    );
    let commands: Vec<&str> = help
        .split("Commands, in the order a first run needs them:\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        commands[..7],
        [
            "provider", "onboard", "review", "task", "self", "doctor", "help"
        ],
        "{help}"
    );
    let mut unique = commands.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), commands.len(), "{help}");
    let named: Vec<String> = help
        .split("`af help ")
        .skip(1)
        .filter_map(|rest| rest.split('`').next())
        .map(str::to_owned)
        .collect();
    assert!(named.contains(&"start".to_owned()) && named.contains(&"concepts".to_owned()));
    for topic in &named {
        let shown = af(&["help", topic]);
        assert!(shown.status.success(), "af help {topic}: {}", err(&shown));
        assert!(
            out(&shown).starts_with(&format!("af help {topic} — ")),
            "{}",
            out(&shown)
        );
    }

    let listed = out(&af(&["help"]));
    let topics = listed
        .split("Topics (`af help TOPIC`):\n")
        .nth(1)
        .unwrap_or_else(|| panic!("{listed}"));
    let rows: Vec<(&str, &str)> = topics
        .lines()
        .take_while(|line| line.starts_with("  "))
        .map(|line| {
            let line = line.trim_start();
            let (name, about) = line.split_once(' ').unwrap();
            (name, about.trim_start())
        })
        .collect();
    let names: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "start",
            "concepts",
            "config",
            "layers",
            "environment",
            "exit-codes",
            "json",
            "providers",
            "task-states",
            "task-file",
            "agents",
            "self",
            "storage",
            "telemetry"
        ]
    );
    for (name, about) in rows {
        // The same description `af help TOPIC` heads its page with: one source.
        let page = out(&af(&["help", name]));
        assert_eq!(
            page.lines().next(),
            Some(format!("af help {name} — {about}").as_str())
        );
    }
    let concepts = out(&af(&["help", "concepts"]));
    for word in [
        "Task",
        "Worker",
        "Attempt",
        "Snapshot",
        "Round",
        "Campaign",
        "Store",
        "Provider",
        "Gate",
        "Pipeline",
        "Candidate",
    ] {
        assert!(
            concepts
                .lines()
                .any(|line| line.starts_with(&format!("{word} "))),
            "{word} missing:\n{concepts}"
        );
    }
}

/// The agent loop's verbs (ADR-0211) say what they print, refuse and never do in `--help`, `af
/// help agents` is `docs/agents.md` byte for byte, and every `af task` command the page names
/// is a command whose `--help` documents every flag the page gives it.
#[test]
fn the_agent_loop_verbs_and_the_agents_topic_describe_the_commands() {
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (command, needles) in [
        (
            &["task", "findings", "--help"][..],
            &[
                "--round <N>",
                "af/task-findings@1",
                "severity, confidence, title, location, the quoted evidence",
                "evaluator's verdict",
                "Never:",
                "af task findings TASK_ID",
            ][..],
        ),
        (
            &["task", "materialize", "--help"],
            &[
                "--into <DIR>",
                "--snapshot <SHA>",
                "--allow-unverified",
                "--force",
                "unverified candidate: review it before merging",
                "af/task-materialized@1",
                "Never: commits",
            ],
        ),
        (
            &["task", "diff", "--help"],
            &[
                "--snapshot <SHA>",
                "--base <SHA>",
                "--stat",
                "--name-only",
                "af/task-diff@1",
                "Never: touches a worktree",
            ],
        ),
        (
            &["task", "cancel", "--help"],
            &[
                "only that recorded pid is signalled",
                "another host is refused with its pid and host",
                "abandoned/cancelled/host_cancelled",
                "af/task-cancelled@1",
            ],
        ),
        (
            &["task", "wait", "--help"],
            &["--timeout <S>", "exits 124", "af/task-status@1"],
        ),
        (
            &["task", "status", "--help"],
            &["af/task-status@1", "without waiting"],
        ),
        (
            &["task", "run", "--help"],
            &[
                "--detach",
                "--quiet",
                "af/task-detached@2",
                "not admitted within 2 minutes exits 4",
                "every 5 minutes a heartbeat",
                "runs/<TASK_ID>-<N>.log",
                "stderr's last line is always the Task's state",
            ],
        ),
        (
            &["task", "report", "--help"],
            &[
                "--pr-body",
                "--template <FILE>",
                "--base <REF>",
                "cannot print its block, nothing is printed and the command exits 1",
                "<!-- af-pr-lines:v1 -->",
                "gh pr create --body-file",
                "scripts/check-pr-report.py accepts it",
            ],
        ),
        (
            &["task", "deliver", "--help"],
            &[
                "kind `verification`",
                "the delivery records both Tasks",
                "af task materialize TASK_ID --into DIR --allow-unverified",
            ],
        ),
    ] {
        let help = af(command);
        assert!(help.status.success(), "{command:?}: {}", err(&help));
        let text = flat(&out(&help));
        for needle in needles {
            assert!(
                text.contains(&flat(needle)),
                "{command:?} lacks `{needle}`:\n{}",
                out(&help)
            );
        }
    }
    let task = out(&af(&["task", "--help"]));
    for verb in [
        "findings",
        "materialize",
        "diff",
        "cancel",
        "wait",
        "status",
    ] {
        assert!(
            task.lines()
                .any(|line| line.trim_start().starts_with(&format!("{verb} "))),
            "{verb}: {task}"
        );
    }
    for (args, code) in [
        (&["task", "diff", "t", "--stat", "--json"][..], 2),
        (&["task", "diff", "t", "--stat", "--name-only"], 2),
        (&["task", "report", "t", "--template", "x.md"], 2),
        (&["task", "report", "t", "--base", "origin/main"], 2),
        (&["task", "findings", "t", "--round", "0"], 2),
        (&["task", "wait", "t", "--timeout", "0"], 2),
        (&["task", "materialize", "t"], 2),
    ] {
        assert_eq!(af(args).status.code(), Some(code), "{args:?}");
    }

    // `af help agents` is the page, and the page names only commands and flags af has.
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let page = std::fs::read_to_string(workspace.join("docs/agents.md")).unwrap();
    let topic = af(&["help", "agents"]);
    assert!(topic.status.success(), "{}", err(&topic));
    assert!(out(&topic).contains(page.trim_end()), "{}", out(&topic));
    let listed = af(&["help", "nonsense"]);
    assert!(err(&listed).contains("agents"), "{}", err(&listed));
    assert!(out(&af(&["--help"])).contains("agents"));
    let mut named = 0;
    for span in page.split('`').skip(1).step_by(2) {
        let words: Vec<&str> = span.split_whitespace().collect();
        if words.first() != Some(&"af") {
            continue;
        }
        let help = match (words.get(1), words.get(2)) {
            // The namespace itself, named in prose, and a line `af task run` prints.
            (Some(&"task"), None) => continue,
            (Some(&"task"), Some(verb)) if verb.ends_with(':') => continue,
            (Some(&"task"), Some(verb)) => af(&["task", verb, "--help"]),
            (Some(&"help"), Some(topic)) => af(&["help", topic]),
            _ => panic!("docs/agents.md names `{span}`, which is not an af task command"),
        };
        assert!(help.status.success(), "`{span}`: {}", err(&help));
        for flag in words.iter().filter(|word| word.starts_with("--")) {
            assert!(
                out(&help).contains(flag),
                "`{span}`: {flag} is not in its help:\n{}",
                out(&help)
            );
        }
        named += 1;
    }
    assert!(named >= 15, "only {named} commands named");
    // GA final fix item 5: step 6 teaches the continuation command, which carries the findings
    // and the verdict, and the hand-made source binding only as the form that does not.
    let step = page
        .split("## 6. Run another round")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap();
    assert!(
        step.contains("`af task run NEW_ID --from TASK_ID [--round N] --json`"),
        "{step}"
    );
    assert!(
        step.contains("findings") && step.contains("evaluator's verdict"),
        "{step}"
    );
    assert!(
        step.contains("carries no findings and no verdict"),
        "{step}"
    );
    let continuations: Vec<&str> = step
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("af task run "))
        .collect();
    assert!(continuations.len() >= 2, "{step}");
    let store = tempfile::tempdir().unwrap();
    for span in continuations {
        // Each parses, its placeholders filled: a Task that does not exist is no usage error.
        let line = span.replace("[--round N]", "--round 1");
        let mut args: Vec<&str> = line.split_whitespace().skip(1).collect();
        args.extend(["--state", store.path().to_str().unwrap()]);
        let output = af(&args);
        assert_ne!(output.status.code(), Some(2), "`{span}`: {}", err(&output));
    }
    let json = out(&af(&["help", "json"]));
    for document in [
        "af/task-findings@1",
        "af/task-diff@1",
        "af/task-materialized@1",
        "af/task-status@1",
        "af/task-detached@2",
        "af/task-cancelled@1",
    ] {
        assert!(json.contains(document), "{document}:\n{json}");
        assert!(page.contains(document), "docs/agents.md lacks {document}");
    }
    assert!(out(&af(&["help", "exit-codes"])).contains("124"));
}

/// `af task run NEW --from TASK` (ADR-0212) says what it plans and from what in `--help` and in
/// `af help task run`; `--round`, `--goal` and `--bindings` need `--from`, which conflicts with
/// `--confirm-plan` and `--detach`; and `af help task-file` describes defaults, context and
/// continuation.
#[test]
fn task_run_help_and_the_task_file_topic_describe_continuation() {
    for help in [af(&["task", "run", "--help"]), af(&["help", "task", "run"])] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--from <TASK>",
            "--round <N>",
            "--goal <FILE>",
            "continues the finished Task TASK from its reviewed candidate",
            "the implementer's `findings` input",
            "af task run NEW_ID --from TASK_ID",
        ] {
            assert!(flat.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
    for args in [
        &["task", "run", "t", "--round", "1"][..],
        &["task", "run", "t", "--goal", "g.md"][..],
        &[
            "task",
            "run",
            "t",
            "--from",
            "s",
            "--confirm-plan",
            "sha256:00",
        ][..],
        &["task", "run", "t", "--from", "s", "--round", "17"][..],
        &["task", "run", "t", "--from", "s", "--execute", "--detach"][..],
    ] {
        let refused = af(args);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{args:?}: {}",
            err(&refused)
        );
    }
    let topic = af(&["help", "task-file"]);
    assert!(topic.status.success(), "{}", err(&topic));
    let text = out(&topic);
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "Only `task_id`, `kind` and `goal` are required",
        "[task.defaults]",
        ".af/rules.md",
        "af/TaskReviewFindings@1",
        "af task run NEW --from TASK [--round N] [--goal FILE]",
    ] {
        assert!(flat.contains(needle), "`{needle}` missing:\n{text}");
    }
}

/// A usage error under `--json` is the one `af/error@1` document `af help json` promises, with
/// clap's message and exit 2, beside clap's own lines on stderr; without `--json` stdout stays
/// empty, and `--help` and `--version` under `--json` are no errors.
#[test]
fn usage_errors_under_json_print_one_error_document() {
    // Parsing usage must not inherit the checkout's release pin, which legitimately trails
    // a prerelease source build until publication.
    let dir = tempfile::tempdir().unwrap();
    let af = |args: &[&str]| {
        crate::common::af()
            .current_dir(dir.path())
            .args(args)
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };
    for args in [
        &[
            "task",
            "start",
            "--file",
            "ticket.json",
            "--timeout-secs",
            "0",
        ][..],
        &["provider", "detect", "--kind", "gemini"],
        &["review", "plan", "--unknown"],
        &["provider", "doctor", "--unknown"],
        &["task", "findings", "missing", "--round", "0"],
        &["task", "wait", "missing", "--timeout", "0"],
        &["task", "run", "new", "--from", "first", "--round", "17"],
    ] {
        let plain = af(args);
        assert_eq!(plain.status.code(), Some(2), "{args:?}: {}", err(&plain));
        assert!(out(&plain).is_empty(), "{args:?}: {}", out(&plain));
        assert!(
            err(&plain).starts_with("error: "),
            "{args:?}: {}",
            err(&plain)
        );
        let json = af(&[args, &["--json"]].concat());
        assert_eq!(json.status.code(), Some(2), "{args:?}: {}", err(&json));
        let document: serde_json::Value = serde_json::from_slice(&json.stdout)
            .unwrap_or_else(|_| panic!("{args:?}: not one document: {}", out(&json)));
        crate::schemas::valid(&crate::schemas::validator("error-v1.json"), &document);
        assert_eq!(document["schema"], "af/error@1", "{args:?}");
        assert_eq!(document["exit_code"], 2, "{args:?}");
        assert_eq!(document["cause"], "usage", "{args:?}");
        let message = document["error"].as_str().unwrap();
        let first = err(&json).lines().next().unwrap().to_owned();
        assert_eq!(
            Some(message.lines().next().unwrap()),
            first.strip_prefix("error: "),
            "{args:?}: clap's message"
        );
        let command = document["command"].as_str().unwrap();
        assert_eq!(command, format!("af {} {}", args[0], args[1]), "{args:?}");
        assert_eq!(document["next_action"], format!("{command} --help"));
    }
    for args in [
        &["task", "plan", "--help", "--json"][..],
        &["--version", "--json"],
    ] {
        let shown = af(args);
        assert!(shown.status.success(), "{args:?}: {}", err(&shown));
        assert!(
            !out(&shown).contains("af/error@1"),
            "{args:?}: {}",
            out(&shown)
        );
    }
}

/// `af task --help` leads a reader to continuing a reviewed Task, and `af task report --help`
/// counts a lease that recorded a gate retry as active time, as the report does.
#[test]
fn task_help_names_continuation_and_report_help_counts_gate_retries() {
    let task = out(&af(&["task", "--help"]));
    let run = task
        .lines()
        .find(|line| line.trim_start().starts_with("run "))
        .unwrap_or_else(|| panic!("{task}"));
    assert!(
        run.contains("continue a reviewed Task with --from"),
        "{run}"
    );
    assert!(run.chars().count() <= 100, "{run}");
    let report = out(&af(&["task", "report", "--help"]));
    let flat = report.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "Active time is the sum of the Task's runs, the writer leases that started an \
             Attempt or recorded a gate retry (`af task run --retry-gate`)"
        ),
        "{report}"
    );
}

/// GA third fix item 4: `af task report --help` describes the whole lines added block the
/// validator accepts — the title, the Directory/Added/Share header, its separator and at least
/// one directory row, or `**Lines added: none**` alone — not only an end marker and a title,
/// within 100 columns.
#[test]
fn task_report_help_describes_the_whole_lines_added_block() {
    let report = out(&af(&["task", "report", "--help"]));
    for line in report.lines() {
        assert!(line.chars().count() <= 100, "`{line}`\n{report}");
    }
    let flat = report.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "so does a block scripts/check-pr-report.py would refuse, naming what is missing. A \
             whole block is the begin marker, then the title `**Lines added: <figure>**`, the \
             header `| Directory | Added | Share |`, its separator and at least one directory \
             row, each directory once, or `**Lines added: none**` alone, then the end marker."
        ),
        "{report}"
    );
    assert!(!flat.contains("title first inside"), "{report}");
}

/// Issue #358: `af review campaigns --json` is `--format json`, the same `af/review-campaigns@1`
/// document byte for byte; text stays the default; and `--json` beside an explicit `--format`
/// is a usage error under `--json`'s own contract, one `af/error@1` document on stdout.
#[test]
fn review_campaigns_json_is_the_format_json_document() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("campaigns");
    let root = root.to_str().unwrap();
    let listing = |root: &str, extra: &[&str]| {
        let mut args = vec!["review", "campaigns", "--state-root", root];
        args.extend_from_slice(extra);
        crate::common::af()
            .current_dir(dir.path())
            .args(&args)
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };
    let campaigns = |extra: &[&str]| listing(root, extra);
    let shorthand = campaigns(&["--json"]);
    let format = campaigns(&["--format", "json"]);
    assert_eq!(shorthand.status.code(), Some(0), "{}", err(&shorthand));
    assert_eq!(format.status.code(), Some(0), "{}", err(&format));
    assert_eq!(out(&shorthand), out(&format));
    let document: serde_json::Value = serde_json::from_slice(&shorthand.stdout).unwrap();
    assert_eq!(
        document,
        serde_json::json!({"schema": "af/review-campaigns@1", "campaigns": [], "problems": []})
    );
    assert_eq!(out(&campaigns(&["--json", "--sizes"])), out(&format));

    // The text listing wraps at the terminal width, and a root wider than it (a long `TMPDIR`)
    // takes a line of its own: read the words, not the layout, for this root and a long one.
    let long = dir.path().join("x".repeat(120)).join("campaigns");
    for root in [root, long.to_str().unwrap()] {
        let listed = listing(root, &[]);
        assert_eq!(listed.status.code(), Some(0), "{}", err(&listed));
        let text = out(&listed);
        assert_eq!(text, out(&listing(root, &["--format", "text"])));
        let words: Vec<&str> = text
            .split_whitespace()
            .filter(|word| *word != "\\")
            .collect();
        assert_eq!(words[..3], ["No", "Campaigns", "under"], "{text}");
        assert!(
            words.iter().any(|word| word.trim_end_matches(';') == root),
            "{text}"
        );
        let document: serde_json::Value =
            serde_json::from_slice(&listing(root, &["--json"]).stdout).unwrap();
        assert_eq!(document["campaigns"], serde_json::json!([]), "{document}");
    }

    for extra in [
        ["--json", "--format", "text"],
        ["--format", "json", "--json"],
    ] {
        let refused = campaigns(&extra);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{extra:?}: {}",
            err(&refused)
        );
        let error: serde_json::Value = serde_json::from_slice(&refused.stdout)
            .unwrap_or_else(|_| panic!("{extra:?}: not one document: {}", out(&refused)));
        crate::schemas::valid(&crate::schemas::validator("error-v1.json"), &error);
        assert_eq!(error["cause"], "usage", "{error}");
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("cannot be used with"),
            "{error}"
        );
        assert_eq!(error["next_action"], "af review campaigns --help");
    }

    let help = out(&af(&["review", "campaigns", "--help"]));
    assert!(help.contains("--json"), "{help}");
    assert!(help.contains("af review campaigns --json"), "{help}");
}
