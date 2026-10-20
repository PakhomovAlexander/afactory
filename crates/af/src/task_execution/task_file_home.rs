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

/// A bounded, prompt-free git invocation in `directory`, isolated as capture isolates it
/// ([`review_source_git::git::isolated_command`]): a checkout's `.git/config` is as untrusted as
/// its tree, so no fsmonitor, hook or filter it configures runs for advice, and no global or
/// system configuration is read. The one piece of the user's own setup it keeps is their global
/// excludes file at git's default place, so the advice still agrees with the
/// `git check-ignore` it names.
fn git_command(directory: &Path) -> std::process::Command {
    let mut command = review_source_git::git::isolated_command(directory, Path::new("/dev/null"));
    let excludes = global_excludes(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    if let Some(excludes) = excludes {
        let mut setting = std::ffi::OsString::from("core.excludesFile=");
        setting.push(excludes);
        command.arg("-c").arg(setting);
    }
    command
}

/// Git's default global excludes file, `$XDG_CONFIG_HOME/git/ignore` or else
/// `$HOME/.config/git/ignore`, when it is a file. Patterns only: it can name nothing to run.
fn global_excludes(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let base = match xdg_config_home.filter(|value| !value.is_empty()) {
        Some(xdg) => std::path::PathBuf::from(xdg),
        None => std::path::PathBuf::from(home.filter(|value| !value.is_empty())?).join(".config"),
    };
    let file = base.join("git/ignore");
    file.is_file().then_some(file)
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

    /// A repository's own `.git/config` cannot run its fsmonitor (or a hook) through the
    /// advisory `check-ignore`, which reads the index.
    #[test]
    fn the_advice_never_runs_a_repository_configured_fsmonitor() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("tracked.txt"), b"tracked\n").unwrap();
        git(&repo, &["add", "-A"]);
        let marker = root.join("MARKER");
        let script = root.join("fsmonitor.sh");
        let body = format!("#!/bin/sh\necho ran >> '{}'\n", marker.display());
        std::fs::write(&script, body).unwrap();
        let permissions = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        git(
            &repo,
            &["config", "core.fsmonitor", script.to_str().unwrap()],
        );
        git(&repo, &["config", "core.hooksPath", root.to_str().unwrap()]);

        let inside = repo.join("ticket.json");
        std::fs::write(&inside, b"{}").unwrap();
        let warning = advisory(&inside, &repo).expect("no advice given");
        assert!(warning.contains("ticket.json"), "{warning}");
        assert!(
            !marker.exists(),
            "the repository's fsmonitor ran: {}",
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    #[test]
    fn the_global_excludes_file_is_found_where_git_looks_by_default() {
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg");
        let home = root.path().join("home");
        std::fs::create_dir_all(xdg.join("git")).unwrap();
        std::fs::create_dir_all(home.join(".config/git")).unwrap();
        assert_eq!(global_excludes(Some(xdg.clone().into()), None), None);
        std::fs::write(xdg.join("git/ignore"), "*.json\n").unwrap();
        std::fs::write(home.join(".config/git/ignore"), "*.json\n").unwrap();
        assert_eq!(
            global_excludes(Some(xdg.clone().into()), Some(home.clone().into())),
            Some(xdg.join("git/ignore"))
        );
        assert_eq!(
            global_excludes(Some("".into()), Some(home.clone().into())),
            Some(home.join(".config/git/ignore"))
        );
        assert_eq!(global_excludes(None, None), None);
    }
}
