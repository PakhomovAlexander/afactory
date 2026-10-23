//! A thing af cannot find is said in one shape, whichever command looked for it:
//! `<Thing> <name> not found in <where>; <what exists or the fix>`. A Task file that cannot be
//! opened says so with its path and the system's own words.

use std::path::Path;
use std::process::{Command, Output};

fn af(repo: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .current_dir(repo)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NO_COLOR", "1")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("AF_PROVIDERS_FILE")
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let done = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

/// The text after the command's own prefix (`af task: `, `error: `) on the first line.
fn refusal(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let first = stderr.lines().next().unwrap_or_default().to_owned();
    first
        .split_once(": ")
        .map_or(first.clone(), |(_, rest)| rest.to_owned())
}

#[test]
fn every_not_found_refusal_takes_one_shape() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# Repo\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let onboard = af(
        &repo,
        &[
            "onboard",
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ],
    );
    assert!(onboard.status.success(), "{onboard:?}");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "policy"]);
    let state = root.path().canonicalize().unwrap().join("state");
    let state_arg = state.to_str().unwrap();

    // Each command, and how its refusal begins and what it offers.
    let cases: Vec<(Vec<&str>, &str, &str)> = vec![
        (
            vec!["task", "show", "nope", "--state", state_arg],
            "Task nope not found in ",
            "; af task list --state ",
        ),
        (
            vec!["task", "report", "nope", "--state", state_arg],
            "Task nope not found in ",
            "lists its Tasks",
        ),
        (
            vec!["task", "explain", "nope", "--state", state_arg],
            "Task Store not found in ",
            "; af task plan --file FILE creates it",
        ),
        (
            vec!["task", "run", "nope", "--execute", "--state", state_arg],
            "Task Store not found in ",
            "; af task plan --file FILE creates it",
        ),
        (
            vec![
                "task", "output", "nope", "--port", "report", "--output", "x.md", "--state",
                state_arg,
            ],
            "Task Store not found in ",
            "; af task plan --file FILE creates it",
        ),
        (
            vec![
                "task",
                "gc",
                "--older-than",
                "1",
                "--keep",
                "1",
                "--state",
                state_arg,
            ],
            "Task Store not found in ",
            "; nothing to collect",
        ),
        (
            vec!["review", "ledger", "--campaign", "nope"],
            "Campaign nope not found in ",
            "; af review campaigns lists the Campaigns, and af review run --campaign nope starts it",
        ),
        (
            vec!["provider", "remove", "nope"],
            "Provider `nope` not found in the Provider registry ",
            "af provider setup ID --kind KIND registers a Provider",
        ),
        (
            vec!["provider", "models", "nope"],
            "Provider nope not found in this machine's Provider registry",
            "; af provider status lists the Providers af knows",
        ),
        (
            vec![
                "review",
                "plan",
                "--policy-rev",
                "HEAD",
                "--base",
                "HEAD",
                "--uncommitted",
                "--provider",
                "nosuchnode=x",
            ],
            "Node nosuchnode not found in .af/pipelines/review.toml",
            "; model Workers: architecture, correctness",
        ),
        (
            vec!["help", "nope"],
            "Topic or command nope not found in af help",
            "; topics: config, layers",
        ),
    ];
    for (args, starts, offers) in &cases {
        let output = af(&repo, args);
        assert!(!output.status.success(), "af {args:?} succeeded");
        let said = refusal(&output);
        assert!(said.starts_with(starts), "af {args:?}: {said}");
        assert!(said.contains(offers), "af {args:?}: {said}");
    }

    // A Task file that cannot be opened: its path and the system's words, before anything else.
    let output = af(&repo, &["task", "plan", "--file", "missing.json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        refusal(&output),
        "cannot open Task file missing.json: No such file or directory (os error 2)"
    );
}
