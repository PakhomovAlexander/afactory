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
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use review_core::{ChangeSetV1, PathRenameV1};
use review_store::Cas;

use crate::manifest::{Manifest, decode_path, encode_path};

#[derive(Debug)]
pub enum GitError {
    Spawn(std::io::Error),
    Io(std::io::Error),
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
    Cas(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Spawn(e) => write!(f, "git could not be started: {e}"),
            GitError::Io(e) => write!(f, "preparing isolated git execution: {e}"),
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
            GitError::Cas(detail) => write!(f, "reading a synthetic tree artifact: {detail}"),
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

/// The kernel-owned policy whose output M2.4 records with each Change Set.
pub const TREE_DIFF_POLICY_VERSION: &str =
    "review.kernel/git-tree-diff@1;binary=git-deflate-level-6";

/// A tree object id admitted by [`Repo::resolve_tree`] or the kernel's synthetic-tree builder.
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
    pub git_version: String,
    pub diff_policy: String,
    output: Vec<u8>,
    patch_start: usize,
}

impl TreeDiff {
    pub fn patch(&self) -> &[u8] {
        &self.output[self.patch_start..]
    }

    pub fn change_set(
        &self,
        base_snapshot_id: impl Into<String>,
        head_snapshot_id: impl Into<String>,
    ) -> Result<ChangeSetV1, String> {
        let mut paths = Vec::new();
        let mut renames = Vec::new();
        for change in &self.changes {
            paths.extend(change.old_path.as_deref().map(encode_path));
            paths.extend(change.new_path.as_deref().map(encode_path));
            if let TreeChangeKind::Renamed { similarity } = &change.kind
                && let (Some(old_path), Some(new_path)) =
                    (change.old_path.as_deref(), change.new_path.as_deref())
            {
                renames.push(PathRenameV1 {
                    old_path: encode_path(old_path),
                    new_path: encode_path(new_path),
                    similarity: *similarity,
                });
            }
        }
        ChangeSetV1::new(
            base_snapshot_id,
            head_snapshot_id,
            paths,
            renames,
            self.patch(),
            &self.git_version,
            &self.diff_policy,
        )
    }
}

enum DiffHead<'a> {
    Resolved(&'a TreeId),
    Synthetic(&'a Manifest, &'a Cas),
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

    /// Every inherited environment variable git is given. The list is exhaustive by
    /// construction: the environment is cleared first, so a variable absent here cannot reach
    /// git no matter what launched the kernel. The typed tree-diff path additionally supplies a
    /// kernel-resolved `GIT_OBJECT_DIRECTORY`; it is authority, not inherited environment.
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
            "core.quotePath=true",
            "-c",
            "core.bigFileThreshold=512m",
            "-c",
            "core.compression=6",
            "-c",
            "core.loosecompression=6",
            "-c",
            "diff.external=",
            "-c",
            "diff.suppressBlankEmpty=false",
            "-c",
            "diff.renameLimit=1000",
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
    /// complete argv is built from constants and opaque [`TreeId`] values. It runs in a
    /// kernel-owned bare administrative directory whose only candidate input is the resolved
    /// object database. Keeping it separate from [`Self::run_raw`] prevents admitting
    /// caller-controlled `git diff` forms globally.
    fn run_tree_diff_unchecked<S: AsRef<OsStr>>(
        &self,
        git_dir: &Path,
        object_dir: &Path,
        alternate_object_dir: Option<&Path>,
        args: &[S],
    ) -> Result<(Output, String), GitError> {
        let mut version_cmd = self.command();
        version_cmd
            .current_dir(&self.home)
            .args(["version", "--build-options"]);
        let version_output = version_cmd.output().map_err(GitError::Spawn)?;
        if !version_output.status.success() {
            return Err(GitError::Failed {
                args: vec!["version".to_string(), "--build-options".to_string()],
                stderr: String::from_utf8_lossy(&version_output.stderr).into_owned(),
            });
        }
        let git_version = String::from_utf8(version_output.stdout)
            .map_err(|_| GitError::NotUtf8)?
            .trim()
            .to_string();

        let mut cmd = self.command();
        cmd.current_dir(&self.home)
            .arg("--git-dir")
            .arg(git_dir)
            .env("GIT_OBJECT_DIRECTORY", object_dir)
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        if let Some(alternate) = alternate_object_dir {
            let joined = std::env::join_paths([alternate]).map_err(|error| {
                GitError::MalformedTreeDiff {
                    detail: format!("invalid alternate object directory: {error}"),
                }
            })?;
            cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", joined);
        }
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
        Ok((output, git_version))
    }

    fn run_isolated_with_input(
        &self,
        git_dir: &Path,
        object_dir: &Path,
        alternate_object_dir: Option<&Path>,
        args: &[&str],
        input: &[u8],
    ) -> Result<Vec<u8>, GitError> {
        let mut cmd = self.command();
        cmd.current_dir(&self.home)
            .arg("--git-dir")
            .arg(git_dir)
            .env("GIT_OBJECT_DIRECTORY", object_dir)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(alternate) = alternate_object_dir {
            let joined = std::env::join_paths([alternate]).map_err(|error| {
                GitError::MalformedTreeDiff {
                    detail: format!("invalid alternate object directory: {error}"),
                }
            })?;
            cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", joined);
        }
        let mut child = cmd.spawn().map_err(GitError::Spawn)?;
        child
            .stdin
            .take()
            .ok_or_else(|| GitError::MalformedTreeDiff {
                detail: "isolated Git command has no stdin".to_string(),
            })?
            .write_all(input)
            .map_err(GitError::Io)?;
        let output = child.wait_with_output().map_err(GitError::Io)?;
        if !output.status.success() {
            return Err(GitError::Failed {
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(output.stdout)
    }

    fn prepare_tree_diff_repository(&self) -> Result<(tempfile::TempDir, PathBuf), GitError> {
        let object_dir = PathBuf::from(self.line(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ])?);
        let object_format = self.line(&["rev-parse", "--show-object-format=storage"])?;
        if !matches!(object_format.as_str(), "sha1" | "sha256") {
            return Err(GitError::MalformedTreeDiff {
                detail: format!("unsupported Git object format {object_format:?}"),
            });
        }

        let administration = tempfile::Builder::new()
            .prefix("review-kernel-tree-diff-")
            .tempdir()
            .map_err(GitError::Io)?;
        let workdir = self.workdir.canonicalize().map_err(GitError::Io)?;
        let admin_root = administration.path().canonicalize().map_err(GitError::Io)?;
        if admin_root.starts_with(workdir) {
            return Err(GitError::MalformedTreeDiff {
                detail: "temporary tree-diff administration is inside the candidate checkout"
                    .to_string(),
            });
        }
        let git_dir = administration.path().join("repo.git");
        fs::create_dir(&git_dir).map_err(GitError::Io)?;
        fs::create_dir(git_dir.join("objects")).map_err(GitError::Io)?;
        fs::create_dir(git_dir.join("refs")).map_err(GitError::Io)?;
        fs::create_dir(git_dir.join("refs/heads")).map_err(GitError::Io)?;
        fs::create_dir(git_dir.join("info")).map_err(GitError::Io)?;
        write_new(
            &git_dir.join("HEAD"),
            b"ref: refs/heads/review-kernel-unused\n",
        )?;
        let config = if object_format == "sha256" {
            "[core]\n\trepositoryformatversion = 1\n\tbare = true\n[extensions]\n\tobjectformat = sha256\n"
        } else {
            "[core]\n\trepositoryformatversion = 0\n\tbare = true\n"
        };
        write_new(&git_dir.join("config"), config.as_bytes())?;
        write_new(&git_dir.join("info/attributes"), b"")?;
        Ok((administration, object_dir))
    }

    fn write_synthetic_tree(
        &self,
        git_dir: &Path,
        object_dir: &Path,
        alternate_object_dir: Option<&Path>,
        manifest: &Manifest,
        cas: &Cas,
    ) -> Result<TreeId, GitError> {
        let mut cmd = self.command();
        cmd.current_dir(&self.home)
            .arg("--git-dir")
            .arg(git_dir)
            .env("GIT_OBJECT_DIRECTORY", object_dir)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(["fast-import", "--quiet", "--force"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(alternate) = alternate_object_dir {
            let joined = std::env::join_paths([alternate]).map_err(|error| {
                GitError::MalformedTreeDiff {
                    detail: format!("invalid alternate object directory: {error}"),
                }
            })?;
            cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", joined);
        }
        let mut child = cmd.spawn().map_err(GitError::Spawn)?;
        {
            let mut input = child
                .stdin
                .take()
                .ok_or_else(|| GitError::MalformedTreeDiff {
                    detail: "isolated fast-import has no stdin".to_string(),
                })?;
            input
                .write_all(
                    b"feature done\ncommit refs/heads/review-kernel-synthetic\ncommitter Review Kernel <review-kernel@invalid> 0 +0000\ndata 0\ndeleteall\n",
                )
                .map_err(GitError::Io)?;
            for entry in &manifest.entries {
                let path = decode_path(&entry.path);
                if path.is_empty()
                    || path.contains(&0)
                    || path.split(|byte| *byte == b'/').any(|component| {
                        component.is_empty() || component == b"." || component == b".."
                    })
                {
                    return Err(GitError::MalformedTreeDiff {
                        detail: format!("synthetic manifest has invalid path {:?}", entry.path),
                    });
                }
                let bytes = cas
                    .get(&entry.content)
                    .map_err(|error| GitError::Cas(error.to_string()))?;
                if bytes.len() as u64 != entry.size {
                    return Err(GitError::MalformedTreeDiff {
                        detail: format!("synthetic manifest size disagrees at {:?}", entry.path),
                    });
                }
                writeln!(
                    input,
                    "M {} inline {}",
                    entry.kind.mode(),
                    quote_fast_import_path(&path)
                )
                .map_err(GitError::Io)?;
                writeln!(input, "data {}", bytes.len()).map_err(GitError::Io)?;
                input.write_all(&bytes).map_err(GitError::Io)?;
                input.write_all(b"\n").map_err(GitError::Io)?;
            }
            input.write_all(b"done\n").map_err(GitError::Io)?;
        }
        let output = child.wait_with_output().map_err(GitError::Io)?;
        if !output.status.success() {
            return Err(GitError::Failed {
                args: vec!["fast-import".into(), "--quiet".into(), "--force".into()],
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let output = self.run_isolated_with_input(
            git_dir,
            object_dir,
            alternate_object_dir,
            &[
                "rev-parse",
                "--verify",
                "refs/heads/review-kernel-synthetic^{tree}",
            ],
            b"",
        )?;
        Ok(TreeId(parse_object_id(&output, "synthetic rev-parse")?))
    }

    fn tree_diff_with_head(
        &self,
        base: &TreeId,
        head: DiffHead<'_>,
    ) -> Result<(TreeId, TreeDiff), GitError> {
        let (administration, candidate_objects) = self.prepare_tree_diff_repository()?;
        let git_dir = administration.path().join("repo.git");
        let local_objects = git_dir.join("objects");
        let (head, object_dir, alternate) = match head {
            DiffHead::Resolved(head) => (head.clone(), candidate_objects.as_path(), None),
            DiffHead::Synthetic(manifest, cas) => (
                self.write_synthetic_tree(
                    &git_dir,
                    &local_objects,
                    Some(&candidate_objects),
                    manifest,
                    cas,
                )?,
                local_objects.as_path(),
                Some(candidate_objects.as_path()),
            ),
        };
        let (output, git_version) = self.run_tree_diff_unchecked(
            &git_dir,
            object_dir,
            alternate,
            &[
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
                "-l1000",
                "-O/dev/null",
                base.as_str(),
                head.as_str(),
                "--",
            ],
        )?;
        Ok((head, parse_tree_diff(output.stdout, git_version)?))
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

    /// Indexed gitlinks cannot be represented by the current synthetic-worktree manifest.
    pub fn indexed_gitlinks(&self) -> Result<Vec<String>, GitError> {
        let mut paths = Vec::new();
        for record in split_nul(&self.bytes(&["ls-files", "-s", "-z"])? ) {
            let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                continue;
            };
            let metadata =
                std::str::from_utf8(&record[..tab]).map_err(|_| GitError::NotUtf8)?;
            if metadata.split_ascii_whitespace().next() == Some("160000") {
                paths.push(encode_path(&record[tab + 1..]));
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Compare two resolved trees without exposing Git's worktree-capable diff interface.
    pub fn tree_diff(&self, base: &TreeId, head: &TreeId) -> Result<TreeDiff, GitError> {
        self.tree_diff_with_head(base, DiffHead::Resolved(head))
            .map(|(_, diff)| diff)
    }

    /// Build a content-matching tree in kernel-owned storage and compare it to a resolved Base.
    pub fn tree_diff_synthetic_head(
        &self,
        base: &TreeId,
        manifest: &Manifest,
        cas: &Cas,
    ) -> Result<(TreeId, TreeDiff), GitError> {
        self.tree_diff_with_head(base, DiffHead::Synthetic(manifest, cas))
    }

    /// Derive the tree identity of a revalidated worktree without writing into the repository.
    pub fn synthetic_tree(&self, manifest: &Manifest, cas: &Cas) -> Result<TreeId, GitError> {
        let (administration, _) = self.prepare_tree_diff_repository()?;
        let git_dir = administration.path().join("repo.git");
        self.write_synthetic_tree(&git_dir, &git_dir.join("objects"), None, manifest, cas)
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

fn quote_fast_import_path(path: &[u8]) -> String {
    let mut quoted = String::from("\"");
    for byte in path {
        match byte {
            b'"' | b'\\' => {
                quoted.push('\\');
                quoted.push(*byte as char);
            }
            b' '..=b'~' => quoted.push(*byte as char),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn parse_object_id(output: &[u8], operation: &str) -> Result<String, GitError> {
    let value = std::str::from_utf8(output)
        .map_err(|_| GitError::NotUtf8)?
        .trim();
    if !matches!(value.len(), 40 | 64)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(GitError::MalformedTreeDiff {
            detail: format!("{operation} returned an invalid object id {value:?}"),
        });
    }
    Ok(value.to_string())
}

fn parse_tree_diff(output: Vec<u8>, git_version: String) -> Result<TreeDiff, GitError> {
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
            || !fields[0][1..]
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'7'))
            || !fields[1].bytes().all(|byte| matches!(byte, b'0'..=b'7'))
            || !is_full_object_id(fields[2])
            || !is_full_object_id(fields[3])
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
        if first_path.is_empty() {
            return Err(GitError::MalformedTreeDiff {
                detail: "raw path was empty".to_string(),
            });
        }
        rest = after_first;
        let (old_path, new_path) = match kind {
            TreeChangeKind::Added => (None, Some(first_path.to_vec())),
            TreeChangeKind::Deleted => (Some(first_path.to_vec()), None),
            TreeChangeKind::Copied { .. } | TreeChangeKind::Renamed { .. } => {
                let (second_path, after_second) = take_nul(rest, "second path")?;
                if second_path.is_empty() {
                    return Err(GitError::MalformedTreeDiff {
                        detail: "second raw path was empty".to_string(),
                    });
                }
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

    let mut patch_start = output.len() - rest.len();
    if changes.is_empty() {
        if !rest.is_empty() {
            return Err(GitError::MalformedTreeDiff {
                detail: "patch section appeared without raw changes".to_string(),
            });
        }
    } else {
        let Some((&separator, patch)) = rest.split_first() else {
            return Err(GitError::MalformedTreeDiff {
                detail: "raw changes had no patch section".to_string(),
            });
        };
        if separator != 0 || !patch.starts_with(b"diff --git ") {
            return Err(GitError::MalformedTreeDiff {
                detail: "patch section did not follow the NUL separator with a git diff header"
                    .to_string(),
            });
        }
        let actual_headers: Vec<&[u8]> = patch
            .split(|byte| *byte == b'\n')
            .filter(|line| line.starts_with(b"diff --git "))
            .collect();
        let expected_headers = expected_patch_headers(&changes);
        if actual_headers.len() != expected_headers.len()
            || actual_headers
                .iter()
                .zip(&expected_headers)
                .any(|(actual, expected)| *actual != expected.as_slice())
        {
            return Err(GitError::MalformedTreeDiff {
                detail: format!(
                    "raw changes disagree with patch headers ({} expected, {} present)",
                    expected_headers.len(),
                    actual_headers.len()
                ),
            });
        }
        patch_start += 1;
    }

    Ok(TreeDiff {
        changes,
        git_version,
        diff_policy: TREE_DIFF_POLICY_VERSION.to_string(),
        output,
        patch_start,
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

fn is_full_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn expected_patch_headers(changes: &[TreeChange]) -> Vec<Vec<u8>> {
    let mut headers = Vec::new();
    for change in changes {
        let old_path = change
            .old_path
            .as_deref()
            .or(change.new_path.as_deref())
            .expect("validated raw change has a path");
        let new_path = change
            .new_path
            .as_deref()
            .or(change.old_path.as_deref())
            .expect("validated raw change has a path");
        let mut header = b"diff --git ".to_vec();
        header.extend_from_slice(&quote_patch_path(b"a/", old_path));
        header.push(b' ');
        header.extend_from_slice(&quote_patch_path(b"b/", new_path));
        let copies = if matches!(change.kind, TreeChangeKind::TypeChanged) {
            2
        } else {
            1
        };
        for _ in 0..copies {
            headers.push(header.clone());
        }
    }
    headers
}

fn quote_patch_path(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    let bytes: Vec<u8> = prefix.iter().chain(path).copied().collect();
    let quoted = bytes
        .iter()
        .any(|byte| !matches!(byte, b' '..=b'~') || matches!(byte, b'"' | b'\\'));
    if !quoted {
        return bytes;
    }
    let mut output = Vec::with_capacity(bytes.len() + 2);
    output.push(b'"');
    for byte in bytes {
        match byte {
            7 => output.extend_from_slice(b"\\a"),
            8 => output.extend_from_slice(b"\\b"),
            b'\t' => output.extend_from_slice(b"\\t"),
            b'\n' => output.extend_from_slice(b"\\n"),
            11 => output.extend_from_slice(b"\\v"),
            12 => output.extend_from_slice(b"\\f"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'"' => output.extend_from_slice(b"\\\""),
            b'\\' => output.extend_from_slice(b"\\\\"),
            b' '..=b'~' => output.push(byte),
            _ => output.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    output.push(b'"');
    output
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), GitError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(GitError::Io)?;
    file.write_all(bytes).map_err(GitError::Io)
}

/// Split `-z` output into records without allocating a String — paths need not be UTF-8.
pub fn split_nul(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_tree_diff;

    const OID: &str = "0123456789012345678901234567890123456789";

    fn raw(status: &str, paths: &[&str]) -> Vec<u8> {
        let mut bytes = format!(":100644 100644 {OID} {OID} {status}").into_bytes();
        bytes.push(0);
        for path in paths {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn malformed_combined_output_fails_closed() {
        let mut raw_only = raw("M", &["file"]);
        let mut malformed_mode = format!(":10064x 100644 {OID} {OID} M\0file\0").into_bytes();
        malformed_mode.extend_from_slice(b"\0diff --git a/file b/file\n");
        let cases = [
            b"diff --git a/file b/file\n".to_vec(),
            raw_only.clone(),
            malformed_mode,
            raw("M", &[""]),
            raw("R100", &["old"]),
            b"unexpected\0bytes".to_vec(),
            {
                let mut mismatch = raw("M", &["safe"]);
                mismatch.extend_from_slice(b"\0diff --git a/hidden b/hidden\n");
                mismatch
            },
        ];
        for bytes in cases {
            assert!(
                parse_tree_diff(bytes, "git version test".to_string()).is_err(),
                "malformed output was admitted"
            );
        }
        raw_only.push(0);
        raw_only.extend_from_slice(b"diff --git a/file b/file\n");
        let parsed = parse_tree_diff(raw_only, "git version test".to_string()).unwrap();
        assert_eq!(parsed.diff_policy, super::TREE_DIFF_POLICY_VERSION);
    }
}
