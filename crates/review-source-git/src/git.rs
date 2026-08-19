//! Running git with nothing of the host's opinion in it.
//!
//! Capture happens before any sandbox exists, against a repository whose contents — including
//! its `.gitattributes`, its `.gitmodules`, and anything it can talk `.git/config` into — are
//! the very thing under review. So the rule is: **the only inputs are content and the command
//! line**. Configuration cannot select an executable, and it cannot change what a snapshot
//! digests to.
//!
//! That is enforced three ways, and the hostile-configuration test proves each:
//!
//! 1. The environment is cleared and rebuilt from a fixed allowlist. `GIT_EXTERNAL_DIFF`,
//!    `GIT_ATTR_NOSYSTEM`, credential helpers and proxies cannot survive that.
//! 2. Every invocation carries `-c` overrides that neutralize hooks, fsmonitor, filters,
//!    external diff, CRLF translation and the attributes file.
//! 3. Only plumbing that does not transform content is ever called — `ls-tree`, `cat-file`,
//!    `ls-files`, `rev-parse`. Worktree bytes are read from the filesystem directly and hashed
//!    here, never handed to git, so a clean filter has nothing to act on.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug)]
pub enum GitError {
    Spawn(std::io::Error),
    /// git exited non-zero. stderr is carried because a capture failure must be diagnosable.
    Failed {
        args: Vec<String>,
        stderr: String,
    },
    NotUtf8,
    /// A subcommand outside [`SAFE_SUBCOMMANDS`] was attempted.
    UnsafeSubcommand {
        subcommand: String,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Spawn(e) => write!(f, "git could not be started: {e}"),
            GitError::Failed { args, stderr } => {
                write!(f, "git {} failed: {}", args.join(" "), stderr.trim())
            }
            GitError::NotUtf8 => write!(f, "git produced output that is not UTF-8"),
            GitError::UnsafeSubcommand { subcommand } => write!(
                f,
                "refusing to run `git {subcommand}` during capture: it can apply candidate-controlled filters"
            ),
        }
    }
}

impl std::error::Error for GitError {}

/// The only git subcommands capture may invoke.
///
/// This is a real boundary, not documentation. `status` and `diff` compare the worktree to the
/// index, which means they *hash worktree files* — and hashing runs the `clean` filter that the
/// candidate's own `.gitattributes` selected. A single `git status` therefore executes
/// attacker-chosen code with the operator's privileges, before any sandbox exists.
///
/// There is no configuration that disables an in-tree filter driver by name (the name is
/// attacker-chosen), so the defence is to never call a command that applies one. Enforced in
/// [`Repo::run_raw`] so a future edit cannot reintroduce the hole by reaching for the obvious
/// command.
pub const SAFE_SUBCOMMANDS: &[&str] = &["ls-tree", "cat-file", "ls-files", "rev-parse", "rev-list"];

/// A repository we may only read.
pub struct Repo {
    workdir: PathBuf,
    /// A private, empty HOME so a global config cannot be discovered even by accident.
    home: PathBuf,
    /// The root-commit walk is O(history) and its answer never changes for an open `Repo`,
    /// so it is paid once, not once per capture.
    repository_id: std::sync::OnceLock<String>,
}

impl Repo {
    /// `home` must be a directory this process controls and git may read; it is deliberately
    /// empty, so `GIT_CONFIG_GLOBAL` has nothing to find even if a future git ignores the
    /// `/dev/null` setting below.
    pub fn open(workdir: impl AsRef<Path>, home: impl AsRef<Path>) -> Self {
        Self {
            workdir: workdir.as_ref().to_path_buf(),
            home: home.as_ref().to_path_buf(),
            repository_id: std::sync::OnceLock::new(),
        }
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Every environment variable git is given. The list is exhaustive by construction: the
    /// environment is cleared first, so a variable absent here cannot reach git no matter what
    /// launched the kernel. `GIT_EXTERNAL_DIFF`, `GIT_CONFIG_COUNT`, credential and proxy
    /// settings are all inert for that reason rather than by being individually blocked.
    pub const ENV_ALLOWLIST: &'static [&'static str] = &[
        "PATH",
        "HOME",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_ATTR_NOSYSTEM",
        "GIT_TERMINAL_PROMPT",
        "GIT_OPTIONAL_LOCKS",
        "LC_ALL",
        "TZ",
    ];

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        // Nothing inherited. Not "most things filtered" — nothing.
        cmd.env_clear();
        for (key, value) in self.environment() {
            cmd.env(key, value);
        }

        cmd.current_dir(&self.workdir);
        cmd.args([
            "--no-optional-locks",
            // A hostile repository cannot run code at any point in the capture path.
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "diff.external=",
            // No transport may be attempted; a submodule URL cannot become a fetch.
            "-c",
            "protocol.allow=never",
            "-c",
            "credential.helper=",
        ]);
        cmd
    }

    /// The exact environment [`Self::ENV_ALLOWLIST`] resolves to for this repository.
    pub fn environment(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        vec![
            ("PATH", std::env::var_os("PATH").unwrap_or_default()),
            ("HOME", self.home.clone().into_os_string()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_ATTR_NOSYSTEM", "1".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_OPTIONAL_LOCKS", "0".into()),
            // Deterministic collation and message text, so output parsing cannot drift by locale.
            ("LC_ALL", "C".into()),
            ("TZ", "UTC".into()),
        ]
    }

    fn run_raw<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Output, GitError> {
        let subcommand = args
            .first()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .unwrap_or_default();
        if !SAFE_SUBCOMMANDS.contains(&subcommand.as_str()) {
            return Err(GitError::UnsafeSubcommand { subcommand });
        }
        let mut cmd = self.command();
        cmd.args(args);
        let output = cmd.output().map_err(GitError::Spawn)?;
        if !output.status.success() {
            return Err(GitError::Failed {
                args: args
                    .iter()
                    .map(|a| a.as_ref().to_string_lossy().into_owned())
                    .collect(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(output)
    }

    /// Raw stdout bytes — required for `-z` output, whose fields may not be UTF-8.
    pub fn bytes<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, GitError> {
        Ok(self.run_raw(args)?.stdout)
    }

    pub fn text<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String, GitError> {
        String::from_utf8(self.run_raw(args)?.stdout).map_err(|_| GitError::NotUtf8)
    }

    pub fn line<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String, GitError> {
        Ok(self.text(args)?.trim().to_string())
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        self.line(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
    }

    /// A sanitized git invocation the caller will stream, rather than capture whole.
    ///
    /// `cat-file --batch` is a conversation — oids in, objects out — so it cannot go through
    /// [`Self::run_raw`], which reads all of stdout before returning. This is the one path that
    /// does not; it is built from exactly the same cleared environment and `-c` overrides as
    /// every other call ([`Self::command`]), and the subcommand is checked against
    /// [`SAFE_SUBCOMMANDS`] here so the allowlist is not bypassed by taking this door.
    pub(crate) fn streaming(&self, args: &[&str]) -> Command {
        let subcommand = args.first().copied().unwrap_or_default();
        assert!(
            SAFE_SUBCOMMANDS.contains(&subcommand),
            "streaming refuses `git {subcommand}`: not in the capture allowlist"
        );
        let mut cmd = self.command();
        cmd.args(args);
        cmd
    }

    /// The repository's own identity, independent of clone path: the first commit's ID.
    ///
    /// A remote URL would be wrong here — the same content served from two remotes is one
    /// repository, and a URL is also attacker-controlled configuration.
    pub fn repository_id(&self) -> Result<String, GitError> {
        if let Some(id) = self.repository_id.get() {
            return Ok(id.clone());
        }
        let roots = self.text(&["rev-list", "--max-parents=0", "HEAD"])?;
        let id = roots.lines().next().unwrap_or_default().trim().to_string();
        Ok(self.repository_id.get_or_init(|| id).clone())
    }
}

/// Split `-z` output into records without allocating a String — paths need not be UTF-8.
pub fn split_nul(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .collect()
}
