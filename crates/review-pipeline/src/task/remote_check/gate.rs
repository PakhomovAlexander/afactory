//! The kernel's private gate repository and its supervised `git`/`gh` calls (ADR-0139 §3.4).
//!
//! Gate commits are built here from Snapshot manifests in a temporary repository the kernel
//! owns, never the operator's checkout, and every tree is read back and compared with its
//! manifest before it is used. Every subprocess goes through the shared supervision with its
//! own wall inside the remote phase's remaining time and the shared kill path on cancellation.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_process::{ExitPolicy, SupervisedError};
use review_source_git::{EntryKind, Manifest};
use review_store::Cas;

use super::Redactor;

/// The fixed identity of every gate commit.
pub(super) const GATE_IDENTITY: &str = "af <af@localhost> 0 +0000";
const LOCAL_CALL: Duration = Duration::from_secs(600);
const NETWORK_CALL: Duration = Duration::from_secs(600);
const MAX_CHAIN: usize = 512;

/// Environment variables that would point a `git` call at another repository, index or object
/// store than the one the kernel names.
const REPOSITORY_LOCATORS: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_CONFIG_PARAMETERS",
];

/// How a supervised call ended when it did not succeed.
#[derive(Debug)]
pub(super) enum ToolError {
    /// The program is not on PATH.
    Missing(&'static str),
    /// It exited unsuccessfully; the redacted, bounded diagnostic when there was one.
    Failed(Option<String>),
    /// The remote phase's time ran out.
    TimedOut,
    Cancelled,
}

impl ToolError {
    /// The text a kernel error carries: never a raw diagnostic, which is already redacted.
    pub(super) fn describe(&self, what: &str) -> String {
        match self {
            Self::Missing(program) => format!("{what}: `{program}` is not on PATH"),
            Self::Failed(Some(diagnostic)) => format!("{what} failed: {diagnostic}"),
            Self::Failed(None) => format!("{what} failed"),
            Self::TimedOut => format!("{what} did not finish within the remote phase"),
            Self::Cancelled => format!("{what} was cancelled"),
        }
    }
}

/// Supervised `git` and `gh` invocations inside one remote phase.
pub(super) struct Tools<'a> {
    pub path: Option<&'a OsStr>,
    pub deadline: Instant,
    pub cancellation: Option<&'a AtomicBool>,
    pub redactor: &'a Redactor,
}

impl Tools<'_> {
    pub(super) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancellation
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    /// Run `program` with `args`. `local` calls touch only the private repository and run
    /// without the operator's Git configuration, so nothing in it can change a gate commit;
    /// remote calls keep the operator's ambient configuration and authentication.
    pub(super) fn run<S: AsRef<OsStr>>(
        &self,
        program: &'static str,
        args: &[S],
        input: Option<Vec<u8>>,
        local: bool,
        cwd: &Path,
        environment: &[(&str, &OsStr)],
    ) -> Result<Vec<u8>, ToolError> {
        if self.cancelled() {
            return Err(ToolError::Cancelled);
        }
        let cap = if local { LOCAL_CALL } else { NETWORK_CALL };
        let timeout = self.remaining().min(cap);
        if timeout.is_zero() {
            return Err(ToolError::TimedOut);
        }
        let mut command = std::process::Command::new(program);
        command.args(args).current_dir(cwd);
        for name in REPOSITORY_LOCATORS {
            command.env_remove(name);
        }
        if let Some(path) = self.path {
            command.env("PATH", path);
        }
        for (name, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
            ("GH_PAGER", "cat"),
            ("GIT_PAGER", "cat"),
            ("NO_COLOR", "1"),
            ("CLICOLOR", "0"),
            ("LC_ALL", "C"),
        ] {
            command.env(name, value);
        }
        if local {
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
        }
        for (name, value) in environment {
            command.env(name, value);
        }
        let captured = match self.cancellation {
            Some(flag) => review_process::run_supervised_captured_cancellable_with_policy(
                &mut command,
                input,
                timeout,
                ExitPolicy::KillProcessGroup,
                flag,
            ),
            None => review_process::run_supervised_captured_with_policy(
                &mut command,
                input,
                timeout,
                ExitPolicy::KillProcessGroup,
            ),
        };
        match captured.status {
            Ok(status) if status.success() => Ok(captured.stdout),
            Ok(_) => {
                let mut raw = captured.stderr;
                if !captured.stdout.is_empty() {
                    raw.push(b'\n');
                    raw.extend_from_slice(&captured.stdout);
                }
                Err(ToolError::Failed(self.redactor.diagnostic(&raw)))
            }
            Err(SupervisedError::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(ToolError::Missing(program))
            }
            Err(SupervisedError::TimedOut { .. }) => Err(ToolError::TimedOut),
            Err(SupervisedError::Cancelled) => Err(ToolError::Cancelled),
            Err(error) => Err(ToolError::Failed(
                self.redactor.diagnostic(error.to_string().as_bytes()),
            )),
        }
    }
}

/// A commit as `git cat-file commit` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committer: String,
    /// Any header beyond tree, parent, author and committer (a signature, an encoding).
    pub extra_headers: bool,
    pub message: String,
}

/// One verified commit of a head branch: its identity, its tree and the Snapshot its message
/// names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChainCommit {
    pub id: String,
    pub tree: String,
    pub snapshot: String,
}

/// Which gate commit a message names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateRole {
    Base,
    Head,
}

/// The parseable message of a gate commit. The base names the source Snapshot, the head the
/// candidate Snapshot; both name the Task and its owner, so a commit identifies who made it.
pub(super) fn gate_message(role: GateRole, task_id: &str, owner: &str, snapshot: &str) -> String {
    let role = match role {
        GateRole::Base => "base",
        GateRole::Head => "head",
    };
    format!(
        "af gate {role}: {task_id}\n\nMade by af from a Task Snapshot to run declared checks; not \
         for review or merge.\n\nAf-Gate: {role}\nAf-Task: {task_id}\nAf-Task-Owner: {owner}\n\
         Af-Snapshot: {snapshot}\n"
    )
}

/// Parse a gate commit message back: `(role, task, owner, snapshot)` when it is exactly the
/// message [`gate_message`] writes for those values.
pub(super) fn parse_gate_message(message: &str) -> Option<(GateRole, String, String, String)> {
    let mut role = None;
    let mut task = None;
    let mut owner = None;
    let mut snapshot = None;
    for line in message.lines() {
        if let Some(value) = line.strip_prefix("Af-Gate: ") {
            role = match value {
                "base" => Some(GateRole::Base),
                "head" => Some(GateRole::Head),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("Af-Task: ") {
            task = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("Af-Task-Owner: ") {
            owner = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("Af-Snapshot: ") {
            snapshot = Some(value.to_owned());
        }
    }
    let (role, task, owner, snapshot) = (role?, task?, owner?, snapshot?);
    (review_core::is_digest(&snapshot) && gate_message(role, &task, &owner, &snapshot) == message)
        .then_some((role, task, owner, snapshot))
}

/// Whether `id` is a valid single Git ref component (`git check-ref-format` rules).
pub(super) fn is_ref_component(id: &str) -> bool {
    !id.is_empty()
        && id != "@"
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.ends_with(".lock")
        && !id.contains("..")
        && !id.contains("@{")
        && !id.bytes().any(|b| {
            b.is_ascii_control()
                || matches!(
                    b,
                    b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\' | b'/'
                )
        })
}

/// The only refspec a gate push may carry: a commit to one of this Task's two branches,
/// without `+`. Anything else is a kernel error, so no other ref can ever be written.
pub(super) fn push_refspec(commit: &str, reference: &str, task_id: &str) -> Result<String, String> {
    let prefix = format!("refs/heads/af-gate/{task_id}/");
    let commit_ok = (commit.len() == 40 || commit.len() == 64)
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !commit_ok
        || !is_ref_component(task_id)
        || !matches!(reference.strip_prefix(&prefix), Some("base" | "head"))
    {
        return Err("a gate push may write only this Task's af-gate branches".into());
    }
    Ok(format!("{commit}:{reference}"))
}

fn is_hex_object(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The private repository of one remote phase. Dropped with its temporary directory.
pub(super) struct GateRepository {
    _directory: tempfile::TempDir,
    root: PathBuf,
    git_dir: PathBuf,
    fetched: std::cell::Cell<u32>,
}

impl GateRepository {
    /// Initialize a bare repository with no template (so no hooks) inside `directory`.
    pub(super) fn init(tools: &Tools<'_>, directory: tempfile::TempDir) -> Result<Self, ToolError> {
        let root = directory.path().to_path_buf();
        let git_dir = root.join("gate.git");
        tools.run(
            "git",
            &[
                OsStr::new("init"),
                OsStr::new("--bare"),
                OsStr::new("--quiet"),
                OsStr::new("--template="),
                OsStr::new("--object-format=sha1"),
                git_dir.as_os_str(),
            ],
            None,
            true,
            &root,
            &[],
        )?;
        Ok(Self {
            _directory: directory,
            root,
            git_dir,
            fetched: std::cell::Cell::new(0),
        })
    }

    /// The temporary directory holding the repository; `gh` runs here.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    fn git<S: AsRef<OsStr>>(
        &self,
        tools: &Tools<'_>,
        args: &[S],
        input: Option<Vec<u8>>,
        local: bool,
        environment: &[(&str, &OsStr)],
    ) -> Result<Vec<u8>, ToolError> {
        let mut full: Vec<OsString> = vec![format!("--git-dir={}", self.git_dir.display()).into()];
        full.extend(args.iter().map(|arg| arg.as_ref().to_os_string()));
        tools.run("git", &full, input, local, &self.root, environment)
    }

    fn text(
        &self,
        tools: &Tools<'_>,
        args: &[&str],
        input: Option<Vec<u8>>,
        environment: &[(&str, &OsStr)],
    ) -> Result<String, ToolError> {
        let out = self.git(tools, args, input, true, environment)?;
        String::from_utf8(out)
            .map(|text| text.trim().to_owned())
            .map_err(|_| ToolError::Failed(Some("git printed non-UTF-8 output".into())))
    }

    /// Write each manifest as a tree, read every tree back with `git ls-tree -r` and compare
    /// every path, mode and blob identity with its manifest. A mismatch is a kernel error.
    pub(super) fn write_trees(
        &self,
        tools: &Tools<'_>,
        cas: &Cas,
        manifests: &[&Manifest],
    ) -> Result<Vec<String>, Result<ToolError, String>> {
        // Each call writes its own scratch refs: the trees of a second call do not descend from
        // those of the first, and `git fast-import` will not move a ref sideways.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, Ordering::Relaxed);
        let mut marks: BTreeMap<&str, usize> = BTreeMap::new();
        let mut stream = Vec::new();
        for manifest in manifests {
            for entry in &manifest.entries {
                if marks.contains_key(entry.content.as_str()) {
                    continue;
                }
                let mark = marks.len() + 1;
                let bytes = cas
                    .get(&entry.content)
                    .map_err(|error| Err(format!("reading a Snapshot blob: {error}")))?;
                stream.extend_from_slice(
                    format!("blob\nmark :{mark}\ndata {}\n", bytes.len()).as_bytes(),
                );
                stream.extend_from_slice(&bytes);
                stream.push(b'\n');
                marks.insert(&entry.content, mark);
            }
        }
        for (index, manifest) in manifests.iter().enumerate() {
            stream.extend_from_slice(
                format!(
                    "commit refs/af/scratch/{call}-{index}\ncommitter {GATE_IDENTITY}\ndata 0\n"
                )
                .as_bytes(),
            );
            for entry in &manifest.entries {
                stream.extend_from_slice(
                    format!(
                        "M {} :{} ",
                        entry.kind.mode(),
                        marks[entry.content.as_str()]
                    )
                    .as_bytes(),
                );
                quote_path(&review_core::decode_path(&entry.path), &mut stream);
                stream.push(b'\n');
            }
            stream.push(b'\n');
        }
        stream.extend_from_slice(b"done\n");
        let marks_file = self.root.join("marks");
        self.git(
            tools,
            &[
                OsString::from("fast-import"),
                OsString::from("--quiet"),
                OsString::from("--done"),
                OsString::from(format!("--export-marks={}", marks_file.display())),
            ],
            Some(stream),
            true,
            &[],
        )
        .map_err(Ok)?;
        let exported = std::fs::read_to_string(&marks_file)
            .map_err(|error| Err(format!("reading the gate repository's marks: {error}")))?;
        let mut blobs = BTreeMap::new();
        for line in exported.lines() {
            let (mark, object) = line
                .strip_prefix(':')
                .and_then(|line| line.split_once(' '))
                .ok_or_else(|| Err("the gate repository wrote malformed marks".to_string()))?;
            blobs.insert(mark.parse::<usize>().unwrap_or_default(), object.to_owned());
        }
        let mut trees = Vec::new();
        for (index, manifest) in manifests.iter().enumerate() {
            let tree = self
                .text(
                    tools,
                    &[
                        "rev-parse",
                        &format!("refs/af/scratch/{call}-{index}^{{tree}}"),
                    ],
                    None,
                    &[],
                )
                .map_err(Ok)?;
            let listed = self
                .git(
                    tools,
                    &["ls-tree", "-r", "-z", "--full-tree", &tree],
                    None,
                    true,
                    &[],
                )
                .map_err(Ok)?;
            let mut actual = Vec::new();
            for record in listed.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                let tab = record
                    .iter()
                    .position(|b| *b == b'\t')
                    .ok_or_else(|| Err("git ls-tree printed a malformed record".to_string()))?;
                let header = std::str::from_utf8(&record[..tab])
                    .map_err(|_| Err("git ls-tree printed a malformed record".to_string()))?;
                let fields: Vec<&str> = header.split(' ').collect();
                let [mode, kind, object] = fields.as_slice() else {
                    return Err(Err("git ls-tree printed a malformed record".into()));
                };
                actual.push((
                    record[tab + 1..].to_vec(),
                    mode.to_string(),
                    kind.to_string(),
                    object.to_string(),
                ));
            }
            let mut expected = Vec::new();
            for entry in &manifest.entries {
                let object = blobs
                    .get(&marks[entry.content.as_str()])
                    .ok_or_else(|| Err("the gate repository lost a blob mark".to_string()))?;
                expected.push((
                    review_core::decode_path(&entry.path),
                    entry.kind.mode().to_string(),
                    "blob".to_string(),
                    object.clone(),
                ));
            }
            actual.sort();
            expected.sort();
            if actual != expected || !is_hex_object(&tree) {
                return Err(Err(
                    "the gate repository's tree differs from its Snapshot manifest".into(),
                ));
            }
            trees.push(tree);
        }
        // Every kind this repository writes is one the manifest names.
        debug_assert!(manifests.iter().all(|m| m.entries.iter().all(|e| matches!(
            e.kind,
            EntryKind::File | EntryKind::Executable | EntryKind::Symlink
        ))));
        Ok(trees)
    }

    /// A gate commit with the fixed identity and `message`, verified after it is written.
    pub(super) fn commit(
        &self,
        tools: &Tools<'_>,
        tree: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, Result<ToolError, String>> {
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        let identity = [
            ("GIT_AUTHOR_NAME", OsStr::new("af")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("af@localhost")),
            ("GIT_AUTHOR_DATE", OsStr::new("@0 +0000")),
            ("GIT_COMMITTER_NAME", OsStr::new("af")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("af@localhost")),
            ("GIT_COMMITTER_DATE", OsStr::new("@0 +0000")),
        ];
        let commit = self
            .text(tools, &args, Some(message.as_bytes().to_vec()), &identity)
            .map_err(Ok)?;
        let written = self.read_commit(tools, &commit).map_err(Ok)?;
        if written
            != (RawCommit {
                tree: tree.to_owned(),
                parents: parent.into_iter().map(str::to_owned).collect(),
                author: GATE_IDENTITY.into(),
                committer: GATE_IDENTITY.into(),
                extra_headers: false,
                message: message.to_owned(),
            })
        {
            return Err(Err(
                "the gate repository wrote another commit than asked".into()
            ));
        }
        Ok(commit)
    }

    pub(super) fn read_commit(
        &self,
        tools: &Tools<'_>,
        commit: &str,
    ) -> Result<RawCommit, ToolError> {
        if !is_hex_object(commit) {
            return Err(ToolError::Failed(Some("not a commit identity".into())));
        }
        let raw = self.git(tools, &["cat-file", "commit", commit], None, true, &[])?;
        let text = String::from_utf8(raw)
            .map_err(|_| ToolError::Failed(Some("commit is not UTF-8".into())))?;
        let (headers, message) = text
            .split_once("\n\n")
            .ok_or_else(|| ToolError::Failed(Some("malformed commit".into())))?;
        let mut parsed = RawCommit {
            tree: String::new(),
            parents: Vec::new(),
            author: String::new(),
            committer: String::new(),
            extra_headers: false,
            message: message.to_owned(),
        };
        for line in headers.lines() {
            if let Some(value) = line.strip_prefix("tree ") {
                parsed.tree = value.to_owned();
            } else if let Some(value) = line.strip_prefix("parent ") {
                parsed.parents.push(value.to_owned());
            } else if let Some(value) = line.strip_prefix("author ") {
                parsed.author = value.to_owned();
            } else if let Some(value) = line.strip_prefix("committer ") {
                parsed.committer = value.to_owned();
            } else {
                parsed.extra_headers = true;
            }
        }
        Ok(parsed)
    }

    /// Fetch one remote ref into a fresh private ref and return the commit it named, or `None`
    /// when the remote has no such ref.
    pub(super) fn fetch(
        &self,
        tools: &Tools<'_>,
        url: &str,
        reference: &str,
    ) -> Result<Option<String>, ToolError> {
        let index = self.fetched.get() + 1;
        self.fetched.set(index);
        let local = format!("refs/af/fetched/{index}");
        let refspec = format!("{reference}:{local}");
        match self.git(
            tools,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--no-recurse-submodules",
                "--quiet",
                "--",
                url,
                &refspec,
            ],
            None,
            false,
            &[],
        ) {
            Ok(_) => {}
            Err(ToolError::Failed(diagnostic))
                if diagnostic
                    .as_deref()
                    .is_some_and(|d| d.contains("couldn't find remote ref")) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        let commit = self.text(
            tools,
            &["rev-parse", "--verify", &format!("{local}^{{commit}}")],
            None,
            &[],
        )?;
        Ok(Some(commit))
    }

    /// The commits the remote's `references` name, exactly by name.
    pub(super) fn ls_remote(
        &self,
        tools: &Tools<'_>,
        url: &str,
        references: &[&str],
    ) -> Result<BTreeMap<String, String>, ToolError> {
        let mut args = vec!["ls-remote", "--", url];
        args.extend(references);
        let out = tools.run("git", &args, None, false, &self.root, &[])?;
        let text = String::from_utf8_lossy(&out);
        let mut found = BTreeMap::new();
        for line in text.lines() {
            if let Some((commit, name)) = line.split_once('\t')
                && references.contains(&name)
                && is_hex_object(commit)
            {
                found.insert(name.to_owned(), commit.to_owned());
            }
        }
        Ok(found)
    }

    /// One atomic, plain push of gate refspecs built by [`push_refspec`]: never `--force`, never
    /// a `+` refspec, and hooks of the operator's configuration do not run.
    pub(super) fn push(
        &self,
        tools: &Tools<'_>,
        url: &str,
        refspecs: &[String],
    ) -> Result<(), ToolError> {
        let args = push_arguments(url, refspecs);
        self.git(tools, &args, None, false, &[]).map(|_| ())
    }

    /// Walk `tip` down to `base`: every commit has exactly one parent, the fixed identity and a
    /// head gate message of this Task and owner, and the walk ends at exactly `base`. Returns
    /// the head commits, tip first, each with the Snapshot its message names; the caller binds
    /// every tree to that Snapshot.
    pub(super) fn verify_chain(
        &self,
        tools: &Tools<'_>,
        tip: &str,
        base: &str,
        task_id: &str,
        owner: &str,
    ) -> Result<Result<Vec<ChainCommit>, String>, ToolError> {
        let mut current = tip.to_owned();
        let mut chain = Vec::new();
        for _ in 0..MAX_CHAIN {
            if current == base {
                return Ok(Err(
                    "the head branch names the base commit itself, not a commit on it".into(),
                ));
            }
            let commit = self.read_commit(tools, &current)?;
            let [parent] = commit.parents.as_slice() else {
                return Ok(Err(format!(
                    "commit {current} on the head branch has {} parents, not one",
                    commit.parents.len()
                )));
            };
            if commit.author != GATE_IDENTITY
                || commit.committer != GATE_IDENTITY
                || commit.extra_headers
            {
                return Ok(Err(format!(
                    "commit {current} on the head branch was not made by af"
                )));
            }
            let snapshot = match parse_gate_message(&commit.message) {
                Some((GateRole::Head, task, found, snapshot))
                    if task == task_id && found == owner =>
                {
                    snapshot
                }
                _ => {
                    return Ok(Err(format!(
                        "commit {current} on the head branch has a message this Task did not write"
                    )));
                }
            };
            let parent = parent.clone();
            chain.push(ChainCommit {
                id: current.clone(),
                tree: commit.tree,
                snapshot,
            });
            if parent == base {
                return Ok(Ok(chain));
            }
            current = parent;
        }
        Ok(Err(format!(
            "the head branch is more than {MAX_CHAIN} commits deep without reaching this Task's \
             base"
        )))
    }
}

/// The exact argument vector of a gate push. Kept apart so a test can pin it.
pub(super) fn push_arguments(url: &str, refspecs: &[String]) -> Vec<String> {
    let mut args = vec![
        "push".to_string(),
        "--atomic".to_string(),
        "--porcelain".to_string(),
        "--no-verify".to_string(),
        "--".to_string(),
        url.to_string(),
    ];
    args.extend(refspecs.iter().cloned());
    args
}

/// C-style quote a path for `git fast-import`, so any byte sequence a manifest can hold is
/// written exactly.
fn quote_path(path: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for &byte in path {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x20..=0x7e => out.push(byte),
            _ => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_messages_parse_back_exactly() {
        let snapshot = format!("sha256:{}", "a".repeat(64));
        let owner = format!("sha256:{}", "b".repeat(64));
        for role in [GateRole::Base, GateRole::Head] {
            let message = gate_message(role, "task-1", &owner, &snapshot);
            assert_eq!(
                parse_gate_message(&message),
                Some((role, "task-1".into(), owner.clone(), snapshot.clone()))
            );
            assert_eq!(parse_gate_message(&format!("{message}extra\n")), None);
            assert_eq!(
                parse_gate_message(&message.replace("Made by", "made by")),
                None
            );
        }
        assert_eq!(parse_gate_message("fix the build\n"), None);
    }

    #[test]
    fn ref_components_follow_git() {
        for valid in ["task-1", "RC1_remote", "a"] {
            assert!(is_ref_component(valid), "{valid}");
        }
        for invalid in [
            "", ".x", "x.", "a..b", "x.lock", "a/b", "a b", "a:b", "@", "a@{b", "a~1",
        ] {
            assert!(!is_ref_component(invalid), "{invalid}");
        }
    }

    #[test]
    fn a_gate_push_never_forces_and_writes_only_this_tasks_branches() {
        let commit = "a".repeat(40);
        let base = push_refspec(&commit, "refs/heads/af-gate/t/base", "t").unwrap();
        let head = push_refspec(&commit, "refs/heads/af-gate/t/head", "t").unwrap();
        for refused in [
            ("refs/heads/main", "t"),
            ("refs/heads/af-gate/other/base", "t"),
            ("refs/heads/af-gate/t/base/x", "t"),
            ("refs/pull/1/merge", "t"),
            ("refs/heads/af-gate/t/base", "a..b"),
        ] {
            assert!(
                push_refspec(&commit, refused.0, refused.1).is_err(),
                "{refused:?}"
            );
        }
        assert!(push_refspec("+aaaa", "refs/heads/af-gate/t/base", "t").is_err());
        let args = push_arguments("/srv/gate.git", &[base, head]);
        assert!(args.iter().all(|arg| !arg.starts_with('+')
            && arg != "--force"
            && arg != "-f"
            && !arg.starts_with("--force")
            && arg != "--mirror"
            && arg != "--delete"));
        assert_eq!(
            args[..5],
            ["push", "--atomic", "--porcelain", "--no-verify", "--"]
        );
    }

    #[test]
    fn paths_are_quoted_byte_exactly() {
        let mut out = Vec::new();
        quote_path(b"a \"b\"\\c\nd\xff", &mut out);
        assert_eq!(out, b"\"a \\\"b\\\"\\\\c\\nd\\377\"");
    }
}
