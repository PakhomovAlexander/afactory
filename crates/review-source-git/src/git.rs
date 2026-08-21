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
//! 3. The generic runner admits only plumbing that does not transform content. The one exception
//!    is [`Repo::tree_diff`]: it supplies two opaque, resolved tree ids and every option itself,
//!    so neither a caller nor repository configuration can select a worktree diff.

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
    /// Git's machine-readable tree-diff output violated the format requested by this adapter.
    MalformedTreeDiff {
        detail: String,
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
            GitError::MalformedTreeDiff { detail } => {
                write!(f, "git produced a malformed tree diff: {detail}")
            }
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

/// A tree object id admitted by [`Repo::resolve_tree`].
///
/// The inner value is deliberately private: callers can select a revision, but cannot smuggle a
/// flag, pathspec, or worktree operand into [`Repo::tree_diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeId(String);

impl TreeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Git's classification of one changed path record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeChangeKind {
    Added,
    Copied { similarity: u8 },
    Deleted,
    Modified,
    Renamed { similarity: u8 },
    TypeChanged,
    Unmerged,
    Unknown,
    BrokenPair,
}

/// One parsed `--raw -z` record. Paths remain bytes because Git permits non-UTF-8 names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    pub kind: TreeChangeKind,
    pub old_path: Option<Vec<u8>>,
    pub new_path: Option<Vec<u8>>,
}

/// The configuration-neutral result of comparing two resolved Git trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeDiff {
    pub changes: Vec<TreeChange>,
    pub patch: Vec<u8>,
}

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

    /// Run the one command that intentionally remains outside [`SAFE_SUBCOMMANDS`].
    ///
    /// This bypass is private and has exactly one call site, in [`Self::tree_diff`], where the
    /// complete argv is built from constants and opaque [`TreeId`] values. Keeping it separate
    /// from [`Self::run_raw`] prevents admitting caller-controlled `git diff` forms globally.
    fn run_tree_diff_unchecked<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Output, GitError> {
        let mut cmd = self.command();
        cmd.args(args);
        let output = cmd.output().map_err(GitError::Spawn)?;
        if !output.status.success() {
            return Err(GitError::Failed {
                args: args
                    .iter()
                    .map(|arg| arg.as_ref().to_string_lossy().into_owned())
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
        self.line(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ])
    }

    /// Resolve a human-facing revision selector to an opaque tree object id.
    pub fn resolve_tree(&self, rev: &str) -> Result<TreeId, GitError> {
        let commit = self.rev_parse(rev)?;
        let tree = self.line(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ])?;
        if !matches!(tree.len(), 40 | 64) || !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GitError::MalformedTreeDiff {
                detail: format!("resolved tree id is not a full object id: {tree:?}"),
            });
        }
        Ok(TreeId(tree))
    }

    /// Compare two resolved trees without exposing Git's worktree-capable diff interface.
    pub fn tree_diff(&self, base: &TreeId, head: &TreeId) -> Result<TreeDiff, GitError> {
        let output = self.run_tree_diff_unchecked(&[
            "diff",
            "--patch-with-raw",
            "-z",
            "--no-abbrev",
            "--full-index",
            "--binary",
            "--diff-algorithm=myers",
            "--no-indent-heuristic",
            "--find-renames=50%",
            "--unified=3",
            "--inter-hunk-context=0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--line-prefix=",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--submodule=short",
            "--ignore-submodules=none",
            "-O/dev/null",
            base.as_str(),
            head.as_str(),
            "--",
        ])?;
        parse_tree_diff(output.stdout)
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

    /// The repository's own identity, independent of clone path: its sorted root commit set.
    ///
    /// A remote URL would be wrong here — the same content served from two remotes is one
    /// repository, and a URL is also attacker-controlled configuration.
    pub fn repository_id(&self) -> Result<String, GitError> {
        if let Some(id) = self.repository_id.get() {
            return Ok(id.clone());
        }
        let roots = self.text(&["rev-list", "--max-parents=0", "HEAD"])?;
        let mut roots: Vec<&str> = roots
            .lines()
            .map(str::trim)
            .filter(|root| !root.is_empty())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            return Err(GitError::Failed {
                args: vec!["rev-list".into(), "--max-parents=0".into(), "HEAD".into()],
                stderr: "git rev-list returned no repository roots".into(),
            });
        }
        let id = roots.join(",");
        Ok(self.repository_id.get_or_init(|| id).clone())
    }
}

fn parse_tree_diff(output: Vec<u8>) -> Result<TreeDiff, GitError> {
    let mut rest = output.as_slice();
    let mut changes = Vec::new();

    while rest.first() == Some(&b':') {
        let (header, after_header) = take_nul(rest, "raw header")?;
        rest = after_header;
        let header = std::str::from_utf8(header).map_err(|_| GitError::MalformedTreeDiff {
            detail: "raw header was not ASCII".to_string(),
        })?;
        let fields: Vec<&str> = header.split_ascii_whitespace().collect();
        if fields.len() != 5
            || !fields[0].starts_with(':')
            || fields[0].len() != 7
            || fields[1].len() != 6
        {
            return Err(GitError::MalformedTreeDiff {
                detail: format!("unexpected raw header {header:?}"),
            });
        }

        let status = fields[4];
        let code =
            status
                .as_bytes()
                .first()
                .copied()
                .ok_or_else(|| GitError::MalformedTreeDiff {
                    detail: "raw status was empty".to_string(),
                })?;
        let kind = match code {
            b'A' => TreeChangeKind::Added,
            b'C' => TreeChangeKind::Copied {
                similarity: parse_similarity(status)?,
            },
            b'D' => TreeChangeKind::Deleted,
            b'M' => TreeChangeKind::Modified,
            b'R' => TreeChangeKind::Renamed {
                similarity: parse_similarity(status)?,
            },
            b'T' => TreeChangeKind::TypeChanged,
            b'U' => TreeChangeKind::Unmerged,
            b'X' => TreeChangeKind::Unknown,
            b'B' => TreeChangeKind::BrokenPair,
            _ => {
                return Err(GitError::MalformedTreeDiff {
                    detail: format!("unknown raw status {status:?}"),
                });
            }
        };
        if !matches!(code, b'C' | b'R') && status.len() != 1 {
            return Err(GitError::MalformedTreeDiff {
                detail: format!("unexpected score on raw status {status:?}"),
            });
        }

        let (first_path, after_first) = take_nul(rest, "first path")?;
        rest = after_first;
        let (old_path, new_path) = match kind {
            TreeChangeKind::Added => (None, Some(first_path.to_vec())),
            TreeChangeKind::Deleted => (Some(first_path.to_vec()), None),
            TreeChangeKind::Copied { .. } | TreeChangeKind::Renamed { .. } => {
                let (second_path, after_second) = take_nul(rest, "second path")?;
                rest = after_second;
                (Some(first_path.to_vec()), Some(second_path.to_vec()))
            }
            _ => (Some(first_path.to_vec()), Some(first_path.to_vec())),
        };
        changes.push(TreeChange {
            kind,
            old_path,
            new_path,
        });
    }

    Ok(TreeDiff {
        changes,
        patch: rest.to_vec(),
    })
}

fn take_nul<'a>(bytes: &'a [u8], field: &str) -> Result<(&'a [u8], &'a [u8]), GitError> {
    let end =
        bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| GitError::MalformedTreeDiff {
                detail: format!("unterminated {field}"),
            })?;
    Ok((&bytes[..end], &bytes[end + 1..]))
}

fn parse_similarity(status: &str) -> Result<u8, GitError> {
    let score = status[1..]
        .parse::<u8>()
        .map_err(|_| GitError::MalformedTreeDiff {
            detail: format!("invalid similarity score in {status:?}"),
        })?;
    if score > 100 {
        return Err(GitError::MalformedTreeDiff {
            detail: format!("similarity score exceeds 100 in {status:?}"),
        });
    }
    Ok(score)
}

/// Split `-z` output into records without allocating a String — paths need not be UTF-8.
pub fn split_nul(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .collect()
}
