//! Where a Task file lives. A Task file is a request, not configuration: the kernel captures it
//! into the Store at plan time, so the copy on disk is disposable and belongs outside every
//! checkout. One that sits in the repository and git would track rides along in every later
//! Snapshot, which is how a consumer's pull request grew by a quarter of a million lines.
//!
//! Advisory only. The warning goes to stderr; exit codes, stdout and `--json` documents are
//! exactly what they were.

use std::path::Path;
use std::time::Duration;

use review_process::{ExitPolicy, run_supervised_with_policy};

/// The home the warning recommends: under the Store root, outside every repository.
const RECOMMENDED_HOME: &str = "$XDG_STATE_HOME/af/tasks/";

/// A bound on the advisory `git check-ignore`. Advice is never worth blocking a plan for.
const CHECK_IGNORE_TIMEOUT: Duration = Duration::from_secs(10);

/// Warn when the Task file resolves inside `repo` and `git check-ignore` does not ignore it.
/// Called once the bytes are in the Store, so the message can say so.
pub(super) fn warn_if_captured_from_the_repository(file: &Path, repo: &Path) {
    if let Some(warning) = advisory(file, repo) {
        eprintln!("warning: {warning}");
    }
}

fn advisory(file: &Path, repo: &Path) -> Option<String> {
    // Both sides are resolved, so a symlink into the checkout is judged by where it lands, and a
    // caller's `--repo .` is judged against the same root.
    let resolved = std::fs::canonicalize(file).ok()?;
    // `--repo` may name a subdirectory of the checkout; containment is judged against the
    // worktree's top level, which is also what `check-ignore` and the shown path use.
    let repo = toplevel(&std::fs::canonicalize(repo).ok()?)?;
    if !resolved.starts_with(&repo) || ignored(&repo, &resolved)? {
        return None;
    }
    let shown = resolved
        .strip_prefix(&repo)
        .unwrap_or(&resolved)
        .display()
        .to_string();
    let message = format!(
        "Task file `{shown}` is inside the repository and git does not ignore it. af captured it \
         into the Store, so the checkout needs no copy; keep Task files outside the repository, \
         for example under {RECOMMENDED_HOME}. This is advice, not a refusal."
    );
    Some(message)
}

/// The worktree top level that contains `directory`, canonicalized. `None` when git could not
/// answer — a missing binary, a directory that is no repository, a deadline.
fn toplevel(directory: &Path) -> Option<std::path::PathBuf> {
    let mut command = git_command(directory);
    command.args(["rev-parse", "--show-toplevel"]);
    let bounded = run_supervised_with_policy(
        &mut command,
        None,
        CHECK_IGNORE_TIMEOUT,
        ExitPolicy::KillProcessGroup,
    )
    .ok()?;
    if !bounded.status.success() {
        return None;
    }
    let text = String::from_utf8(bounded.stdout).ok()?;
    std::fs::canonicalize(text.trim_end_matches(['\n', '\r'])).ok()
}

/// A bounded, prompt-free git invocation in `directory` that still sees the user's own
/// configuration: an advisory check that ignored their excludes would contradict the
/// `git check-ignore` it names. Only prompting and lock taking are switched off.
fn git_command(directory: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command.env_clear();
    for name in ["PATH", "HOME", "XDG_CONFIG_HOME", "GIT_CONFIG_GLOBAL"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .current_dir(directory)
        .arg("--no-optional-locks");
    command
}

/// Whether git ignores `path`. `None` when git could not answer — a missing binary, a directory
/// that is no repository, a deadline. Advice that cannot be established is not given.
fn ignored(repo: &Path, path: &Path) -> Option<bool> {
    let mut command = git_command(repo);
    command.args(["check-ignore", "-q", "--"]).arg(path);
    let bounded = run_supervised_with_policy(
        &mut command,
        None,
        CHECK_IGNORE_TIMEOUT,
        ExitPolicy::KillProcessGroup,
    );
    // 0: the path is ignored. 1: it is not — which is also what a tracked path reports, and is
    // exactly the case worth warning about. Any other code is git declining to answer.
    match bounded.ok()?.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn only_a_task_file_the_repository_would_carry_is_advised_about() {
        let root = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(root.path()).unwrap().join("repo");
        std::fs::create_dir_all(repo.join("held")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "held/\n").unwrap();

        let inside = repo.join("ticket.json");
        std::fs::write(&inside, b"{}").unwrap();
        let warning = advisory(&inside, &repo).expect("no advice given");
        assert!(warning.contains("ticket.json"), "{warning}");
        assert!(warning.contains(RECOMMENDED_HOME), "{warning}");
        assert!(warning.contains("captured it"), "{warning}");

        let held = repo.join("held/ticket.json");
        std::fs::write(&held, b"{}").unwrap();
        // An excluded file, a file outside the repository, and a file that is not there.
        assert_eq!(advisory(&held, &repo), None);

        let outside = repo.parent().unwrap().join("ticket.json");
        std::fs::write(&outside, b"{}").unwrap();
        assert_eq!(advisory(&outside, &repo), None);
        assert_eq!(advisory(&repo.join("absent.json"), &repo), None);

        // `--repo` naming a subdirectory judges containment against the worktree top level.
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let from_sub = advisory(&inside, &sub).expect("no advice from a subdirectory");
        assert!(from_sub.contains("`ticket.json`"), "{from_sub}");
        assert_eq!(advisory(&held, &sub), None);
    }
}
