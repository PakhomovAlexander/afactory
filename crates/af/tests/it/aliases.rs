//! The words people type for a command that has another name: `af provider list` is `status`,
//! `af providers …` is `af provider …`, `af review tui` points at the browser, and a near miss
//! gets clap's "did you mean". `af task start` is documented as `plan` under another name.

use std::process::Output;

fn af(args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NO_COLOR", "1")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("AF_PROVIDERS_FILE")
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
fn provider_list_and_providers_are_the_provider_commands() {
    let status = af(&["provider", "status", "--json"]);
    assert!(status.status.success(), "{}", err(&status));
    for words in [
        ["provider", "list", "--json"],
        ["providers", "status", "--json"],
        ["providers", "list", "--json"],
    ] {
        let alias = af(&words);
        assert_eq!(alias.status.code(), status.status.code(), "{words:?}");
        assert_eq!(out(&alias), out(&status), "{words:?}");
    }
    let help = collapsed(&af(&["provider", "status", "--help"]));
    assert!(
        help.contains("`af provider list` is the same command."),
        "{help}"
    );
}

#[test]
fn review_tui_names_the_browser_as_a_usage_error() {
    let tui = af(&["review", "tui"]);
    assert_eq!(tui.status.code(), Some(2));
    assert!(out(&tui).is_empty());
    assert!(
        err(&tui)
            .contains("`af review tui` is not a command; the browser is `af` alone, at a terminal"),
        "{}",
        err(&tui)
    );
    assert!(err(&tui).contains("try: af"), "{}", err(&tui));
    // It is no command of the review help.
    assert!(!out(&af(&["review", "--help"])).contains("tui"));
}

#[test]
fn near_misses_get_a_pointer() {
    for (words, tip) in [
        (vec!["provider", "lst"], "'list'"),
        (vec!["providr", "status"], "'provider'"),
        (vec!["review", "tu"], "'tui'"),
        (vec!["task", "strt"], "'start'"),
    ] {
        let missed = af(&words);
        assert_eq!(missed.status.code(), Some(2), "{words:?}");
        assert!(err(&missed).contains("tip:"), "{words:?}: {}", err(&missed));
        assert!(err(&missed).contains(tip), "{words:?}: {}", err(&missed));
    }
}

/// Help text with its wrapping undone.
fn collapsed(output: &Output) -> String {
    out(output).split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn task_start_is_documented_as_plan_under_another_name() {
    let start = collapsed(&af(&["task", "start", "--help"]));
    assert!(
        start.contains("Without --execute this is `af task plan` under another name"),
        "{start}"
    );
    let plan = collapsed(&af(&["task", "plan", "--help"]));
    assert!(
        plan.contains("`af task start` without --execute does the same"),
        "{plan}"
    );
    let task = collapsed(&af(&["task", "--help"]));
    assert!(
        task.contains("as `plan` does; with --execute, also run it"),
        "{task}"
    );
}
