//! Isolated Git construction shared by Project Workspaces and Task delivery.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command as ProcessCommand;

/// The git command delivery runs, in `directory` with the private `home`: nothing inherited but
/// `PATH`, no user or system configuration, attributes file, hooks, fsmonitor, sparse checkout,
/// external diff, transport or credential helper. `af project workspace new` runs the same
/// command (ADR-0158).
pub fn isolated_git(directory: &Path, home: &Path) -> ProcessCommand {
    let mut command = ProcessCommand::new("git");
    command.env_clear();
    command
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .current_dir(directory)
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.sparseCheckout=false",
            "-c",
            "core.sparseCheckoutCone=false",
            "-c",
            "diff.external=",
            "-c",
            "protocol.allow=never",
            "-c",
            "credential.helper=",
        ]);
    command
}

pub fn ensure_empty_linked_worktree(worktree: &Path) -> Result<(), String> {
    if !worktree.join(".git").is_file() {
        return Err("Git did not create a linked-worktree administration file".into());
    }
    for entry in std::fs::read_dir(worktree).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name() != OsStr::new(".git") {
            return Err(format!(
                "new no-checkout worktree unexpectedly contains `{}`",
                entry.file_name().to_string_lossy()
            ));
        }
    }
    Ok(())
}
