//! A Task's candidate outside the Store (ADR-0211): `af task materialize` makes a Git
//! worktree's tracked tree equal to a Snapshot the Task recorded — its writes, deletions and
//! executable bits — and `af task diff` prints the change between two of its Snapshots as `git
//! diff` would, touching no worktree.
//!
//! Neither commits, stages, pushes or contacts a remote. `materialize` refuses a worktree with
//! uncommitted changes unless `--force`, and a candidate the Task did not verify unless
//! `--allow-unverified`; `af task deliver` keeps its stricter contract.

use super::*;
use review_core::task::agent_loop::*;
use review_source_git::{Entry, decode_path, digest_bytes, encode_path};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

/// How long one git command of `materialize` or `diff` may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// One Snapshot the Task recorded, and what it is: its `source`, its result's `snapshot`, or a
/// node's output.
struct Recorded {
    snapshot_id: String,
    what: String,
}

/// Every Snapshot the Task recorded, the source first, then each node's output in plan order,
/// then the result's.
fn recorded(opened: &OpenedTask, result: Option<&TaskResultV1>) -> Vec<Recorded> {
    let task = &opened.task;
    let mut found: Vec<Recorded> = Vec::new();
    let mut push = |snapshot_id: Option<&String>, what: String| {
        if let Some(id) = snapshot_id
            && !found.iter().any(|known| &known.snapshot_id == id)
        {
            found.push(Recorded {
                snapshot_id: id.clone(),
                what,
            });
        }
    };
    push(
        task.revision
            .inputs
            .get("source")
            .and_then(|port| port.snapshot_id.as_ref()),
        "its source".into(),
    );
    if let Some(execution) = &task.execution {
        let order = &execution.graph.order;
        let mut nodes: Vec<(&String, &review_core::task::execution::TaskOutputV1)> = execution
            .outputs
            .iter()
            .map(|(node, (_, output))| (node, output))
            .collect();
        nodes.sort_by_key(|(node, _)| order.iter().position(|n| n == *node));
        for (node, output) in nodes {
            for (port, value) in &output.outputs {
                if value.artifact_type == SOURCE_TREE_V1 {
                    push(
                        value.snapshot_id.as_ref(),
                        format!("{}/{port}", runs::node_name(node)),
                    );
                }
            }
        }
    }
    if let Some(result) = result {
        push(
            result
                .outputs
                .get("snapshot")
                .and_then(|port| port.snapshot_id.as_ref()),
            "its result".into(),
        );
    }
    found
}

/// The Snapshot a command works on, and whether it is the Task's verified result: the one
/// `--snapshot` names, which must be one the Task recorded, or else the Task's candidate — its
/// result's Snapshot, or the last Snapshot a node of its plan sealed.
pub(super) struct Selected {
    pub(super) snapshot_id: String,
    pub(super) verified: bool,
    pub(super) source_id: Option<String>,
}

pub(super) fn select(opened: &OpenedTask, named: Option<&str>) -> Result<Selected, String> {
    let id = &opened.task.task_id;
    let (result, view) = opened.view()?;
    let all = recorded(opened, result.as_ref());
    let source_id = opened
        .task
        .revision
        .inputs
        .get("source")
        .and_then(|port| port.snapshot_id.clone());
    let result_snapshot = result
        .as_ref()
        .and_then(|result| result.outputs.get("snapshot"))
        .and_then(|port| port.snapshot_id.clone());
    let snapshot_id = match named {
        Some(named) => {
            if !all.iter().any(|known| known.snapshot_id == named) {
                let listed = all
                    .iter()
                    .map(|known| format!("{} ({})", known.snapshot_id, known.what))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "Task {id} recorded no Snapshot {}; it recorded {}",
                    preview::text(named),
                    if listed.is_empty() {
                        "none".into()
                    } else {
                        listed
                    }
                ));
            }
            named.to_owned()
        }
        None => match result_snapshot.clone().or_else(|| {
            all.iter()
                .rev()
                .find(|known| Some(&known.snapshot_id) != source_id.as_ref())
                .map(|known| known.snapshot_id.clone())
        }) {
            Some(candidate) => candidate,
            None => {
                return Err(format!(
                    "Task {id} has no candidate Snapshot yet: it is {}, and no node of its \
                     plan sealed one; next: {}",
                    view.state.label(),
                    view.state.next_action.as_deref().unwrap_or("none")
                ));
            }
        },
    };
    let verified = result.as_ref().is_some_and(|result| {
        result.acceptance == TaskAcceptanceV1::Satisfied
            && view.state.outcome == Some(review_core::task::state::TaskOutcomeV1::Verified)
    }) && result_snapshot.as_deref() == Some(snapshot_id.as_str());
    Ok(Selected {
        snapshot_id,
        verified,
        source_id,
    })
}

/// A git command with nothing inherited, no hooks, fsmonitor, external diff or remote
/// (`crate::task::isolated_git`), its home a scratch directory.
fn git(directory: &Path, home: &Path) -> std::process::Command {
    crate::task::isolated_git(directory, home)
}

fn run_git(
    mut command: std::process::Command,
    input: Option<Vec<u8>>,
    what: &str,
) -> Result<Vec<u8>, String> {
    let output = review_process::run_supervised_with_policy(
        &mut command,
        input,
        GIT_TIMEOUT,
        review_process::ExitPolicy::KillProcessGroup,
    )
    .map_err(|error| format!("{what}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn admit_experiment_control(repo: &Path, home: &Path, revision: &str) -> Result<(), String> {
    let mut command = git(repo, home);
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    command.args(["ls-tree", "-r", "-l", "-z", "--full-tree", revision]);
    let capture = review_process::run_supervised_captured_bounded(
        &mut command,
        None,
        GIT_TIMEOUT,
        review_process::ExitPolicy::KillProcessGroup,
        None,
        32 * 1024 * 1024,
    );
    let output = capture.into_result().map_err(|e| e.to_string())?;
    if !output.status.success() || output.truncation.is_truncated() {
        return Err("Unsupported experiment control tree: bounded metadata unavailable".into());
    }
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    for entry in output.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        count += 1;
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or("Invalid Git tree metadata")?;
        let metadata = std::str::from_utf8(&entry[..tab]).map_err(|e| e.to_string())?;
        let fields: Vec<_> = metadata.split_ascii_whitespace().collect();
        if fields.len() != 4 || fields[1] != "blob" || !matches!(fields[0], "100644" | "100755") {
            return Err("Unsupported experiment control links or submodules".into());
        }
        let size: u64 = fields[3].parse().map_err(|_| "Invalid Git blob size")?;
        bytes = bytes
            .checked_add(size)
            .ok_or("Experiment control size overflow")?;
        if count > 32768 || bytes > 1024 * 1024 * 1024 {
            return Err("Experiment control exceeds the native source bound".into());
        }
    }
    Ok(())
}

fn into_text(path: &Path) -> String {
    preview::text(&path.display().to_string())
}

// ------------------------------------------------------------------------------------------
// af task materialize

/// The tracked files of the worktree at `root` as they are on disk now, with every other path
/// the candidate names that exists there: what the candidate replaces. Submodules are not
/// files af writes and are left out.
fn current_tree(
    root: &Path,
    home: &Path,
    candidate: &Manifest,
) -> Result<(Manifest, BTreeSet<String>), String> {
    let mut command = git(root, home);
    command.args(["ls-files", "-z", "-s"]);
    let listed = run_git(command, None, "git ls-files")?;
    let mut paths: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut tracked: BTreeSet<String> = BTreeSet::new();
    for record in listed.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        if record.starts_with(b"160000 ") {
            continue;
        }
        let path = record[tab + 1..].to_vec();
        tracked.insert(encode_path(&path));
        paths.insert(path);
    }
    for entry in &candidate.entries {
        paths.insert(decode_path(&entry.path));
    }
    let mut entries = Vec::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    for raw in paths {
        let path = root.join(OsStr::from_bytes(&raw));
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let (kind, content, size) = if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let bytes = target.as_os_str().as_bytes().to_vec();
            (EntryKind::Symlink, digest_bytes(&bytes), bytes.len() as u64)
        } else if metadata.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let kind = if metadata.permissions().mode() & 0o100 != 0 {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            let file = std::fs::File::open(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let (content, size) = review_source_git::digest_reader_with_buffer(file, &mut buffer)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            (kind, content, size)
        } else {
            // A directory where the candidate has a file: materialization refuses it below.
            continue;
        };
        entries.push(Entry {
            path: encode_path(&raw),
            kind,
            content,
            size,
        });
    }
    let manifest = Manifest::new(entries).map_err(|e| e.to_string())?;
    Ok((manifest, tracked))
}

/// What the candidate changes against the tree at hand: paths written (new, or a changed
/// content or kind), deleted, and changed only in their executable bit.
fn changes(
    from: &Manifest,
    to: &Manifest,
    tracked: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut written, mut deleted, mut mode) = (Vec::new(), Vec::new(), Vec::new());
    for entry in &to.entries {
        match from.get(&entry.path) {
            None => written.push(entry.path.clone()),
            Some(before) if before.content == entry.content && before.kind != entry.kind => {
                let regular = |kind| matches!(kind, EntryKind::File | EntryKind::Executable);
                if regular(before.kind) && regular(entry.kind) {
                    mode.push(entry.path.clone());
                } else {
                    written.push(entry.path.clone());
                }
            }
            Some(before) if before.content != entry.content || before.kind != entry.kind => {
                written.push(entry.path.clone());
            }
            Some(_) => {}
        }
    }
    for entry in &from.entries {
        if to.get(&entry.path).is_none() && tracked.contains(&entry.path) {
            deleted.push(entry.path.clone());
        }
    }
    (written, deleted, mode)
}

/// `af task materialize ID [--snapshot SHA] --into DIR [--allow-unverified] [--force]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize(
    id: &str,
    snapshot: Option<&str>,
    into: &Path,
    allow_unverified: bool,
    force: bool,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let selected = select(&opened, snapshot)?;
    let shown = into_text(into);
    if !selected.verified && !allow_unverified {
        let (_, view) = opened.view()?;
        return Err(format!(
            "Task {id}'s Snapshot {} is not its verified result (the Task is {}), so it was not \
             written into {shown}; review it with `af task diff {id}` or `af task findings \
             {id}`, then write it anyway with `af task materialize {id} --into {shown} \
             --allow-unverified`",
            selected.snapshot_id,
            view.state.label()
        ));
    }
    let root = std::fs::canonicalize(into).map_err(|error| {
        format!(
            "--into {shown}: {error}; name an existing Git worktree, for example one `git \
             worktree add {shown}` made"
        )
    })?;
    if root.starts_with(&opened.state) || opened.state.starts_with(&root) {
        return Err(format!(
            "--into {shown} overlaps the Task Store {}; name a Git worktree outside it",
            opened.state.display()
        ));
    }
    let home = tempfile::tempdir().map_err(|e| format!("creating a scratch directory: {e}"))?;
    let mut top = git(&root, home.path());
    top.args(["rev-parse", "--show-toplevel"]);
    let top =
        run_git(top, None, &format!("--into {shown} is not a Git worktree")).map_err(|error| {
            format!(
                "{error}; name the top of a Git worktree, for example one `git worktree add \
                 {shown}` made"
            )
        })?;
    let top = std::fs::canonicalize(String::from_utf8_lossy(&top).trim())
        .map_err(|e| format!("--into {shown}: {e}"))?;
    if top != root {
        return Err(format!(
            "--into {shown} is inside the Git worktree {}, not its top; name {} itself",
            top.display(),
            top.display()
        ));
    }
    // What counts as uncommitted is what the person's own `git status` says: their global
    // ignore rules apply, as `af task deliver` classifies ignored paths (hooks, fsmonitor and
    // external diffs stay off).
    let mut status = git(&root, home.path());
    for name in ["HOME", "XDG_CONFIG_HOME", "GIT_CONFIG_GLOBAL"] {
        match std::env::var_os(name) {
            Some(value) => status.env(name, value),
            None => status.env_remove(name),
        };
    }
    status.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let status = run_git(status, None, "git status")?;
    let dirty: Vec<String> = status
        .split(|byte| *byte == 0)
        .filter(|record| record.len() > 3)
        .map(|record| String::from_utf8_lossy(record).into_owned())
        .collect();
    if !dirty.is_empty() && !force {
        let shown_paths = dirty
            .iter()
            .take(5)
            .map(|line| preview::text(line))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if dirty.len() > 5 {
            format!(" and {} more", dirty.len() - 5)
        } else {
            String::new()
        };
        return Err(crate::error_document::typed(
            format!(
                "--into {shown} has uncommitted changes ({shown_paths}{more}), which materializing \
             Task {id} would overwrite; commit or stash them (`git -C {shown} status`), or pass \
             --force to overwrite them"
            ),
            crate::error_document::Detail::new(crate::error_document::Cause::DirtySource)
                .with("repo", shown.clone()),
        ));
    }
    let (_, candidate) =
        review_source_git::task::read_snapshot(&opened.cas, &selected.snapshot_id)?;
    let (from, tracked) = current_tree(&root, home.path(), &candidate)?;
    let (written, deleted, mode) = changes(&from, &candidate, &tracked);
    // The tree the worktree ends at: every candidate entry, every tracked path the candidate
    // drops removed. Untracked files the candidate does not name are left as they are.
    let keep: Vec<Entry> = from
        .entries
        .iter()
        .filter(|entry| !tracked.contains(&entry.path) && candidate.get(&entry.path).is_none())
        .cloned()
        .collect();
    let mut target_entries = candidate.entries.clone();
    target_entries.extend(keep);
    let target = Manifest::new(target_entries).map_err(|e| e.to_string())?;
    review_source_git::apply_tree_diff(&from, &target, &opened.cas, &root).map_err(|error| {
        format!(
            "writing Task {id}'s Snapshot into {shown} stopped: {error}; what was written stays, \
             `git -C {shown} status` shows it"
        )
    })?;
    // Read back what the candidate names: the worktree holds it exactly.
    let (after, _) = current_tree(&root, home.path(), &candidate)?;
    for entry in &candidate.entries {
        if after.get(&entry.path) != Some(entry) {
            return Err(format!(
                "{shown}/{} does not hold what Task {id}'s Snapshot names after it was written; \
                 `git -C {shown} status` shows the worktree",
                preview::text(&entry.path)
            ));
        }
    }
    let next = vec![
        format!("git -C {shown} status"),
        format!("af task report {id}"),
    ];
    let document = TaskMaterializedV1 {
        schema: TASK_MATERIALIZED_V1.into(),
        task_id: id.into(),
        snapshot_id: selected.snapshot_id,
        verified: selected.verified,
        into: root.display().to_string(),
        written,
        deleted,
        mode,
        next,
    };
    document.validate()?;
    if !document.verified {
        eprintln!("unverified candidate: review it before merging");
    }
    if inspect.json {
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "snapshot {} ({} candidate of Task {id})",
            document.snapshot_id,
            if document.verified {
                "the verified"
            } else {
                "an unverified"
            }
        );
        println!("into     {}", into_text(Path::new(&document.into)));
        println!(
            "written {}, deleted {}, mode {}",
            document.written.len(),
            document.deleted.len(),
            document.mode.len()
        );
        if !document.verified {
            println!("unverified candidate: review it before merging");
        }
        println!("next: {}", document.next.join("; "));
    }
    Ok(0)
}

// ------------------------------------------------------------------------------------------
// af task diff

/// A scratch Git object database holding exactly the entries two Snapshots differ in, as two
/// trees: what `git diff-tree` compares as `git diff` compares a worktree with its base.
struct Scratch {
    directory: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Result<Self, String> {
        let directory =
            tempfile::tempdir().map_err(|e| format!("creating a scratch directory: {e}"))?;
        Self::initialize(directory)
    }

    fn initialize(directory: tempfile::TempDir) -> Result<Self, String> {
        let scratch = Self { directory };
        let mut init = scratch.git(None);
        init.args(["init", "-q", "--bare", "objects.git"]);
        run_git(init, None, "git init")?;
        Ok(scratch)
    }

    fn git(&self, index: Option<&Path>) -> std::process::Command {
        let root = self.directory.path();
        let mut command = git(root, root);
        if root.join("objects.git").is_dir() {
            command.arg("--git-dir").arg(root.join("objects.git"));
        }
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command
    }

    /// The blob IDs of `entries`' content, written into the scratch database.
    fn blobs(&self, cas: &Cas, entries: &[&Entry]) -> Result<Vec<String>, String> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let staged = self.directory.path().join("blobs");
        std::fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
        let mut list = Vec::new();
        for (n, entry) in entries.iter().enumerate() {
            let path = staged.join(n.to_string());
            let bytes = cas.get(&entry.content).map_err(|e| e.to_string())?;
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
            list.extend_from_slice(path.as_os_str().as_bytes());
            list.push(b'\n');
        }
        let mut command = self.git(None);
        command.args(["hash-object", "-w", "--no-filters", "--stdin-paths"]);
        let out = run_git(command, Some(list), "git hash-object")?;
        let ids: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_owned)
            .collect();
        if ids.len() != entries.len() {
            return Err("git hash-object returned another number of blobs".into());
        }
        Ok(ids)
    }

    /// The tree of `entries` with their blob IDs.
    fn tree(&self, name: &str, entries: &[&Entry], blobs: &[String]) -> Result<String, String> {
        let index = self.directory.path().join(name);
        let mut input = Vec::new();
        for (entry, blob) in entries.iter().zip(blobs) {
            input.extend_from_slice(format!("{} {blob}\t", entry.kind.mode()).as_bytes());
            input.extend_from_slice(&decode_path(&entry.path));
            input.push(0);
        }
        let mut update = self.git(Some(&index));
        update.args(["update-index", "-z", "--index-info"]);
        run_git(update, Some(input), "git update-index")?;
        let mut write = self.git(Some(&index));
        write.arg("write-tree");
        let tree = run_git(write, None, "git write-tree")?;
        Ok(String::from_utf8_lossy(&tree).trim().to_owned())
    }

    fn diff(&self, base: &str, head: &str, flags: &[&str]) -> Result<Vec<u8>, String> {
        let mut command = self.git(None);
        command
            .args([
                "diff-tree",
                "-r",
                "-M",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
            ])
            .args(flags)
            .args([base, head]);
        run_git(command, None, "git diff-tree")
    }

    fn commit(&self, tree: &str, parent: Option<&str>, snapshot: &str) -> Result<String, String> {
        let mut command = self.git(None);
        command.args(["commit-tree", tree]);
        if let Some(parent) = parent {
            command.args(["-p", parent]);
        }
        for key in ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"] {
            command.env(key, "2000-01-01T00:00:00+0000");
        }
        for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            command.env(key, "AF private experiment source");
        }
        for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
            command.env(key, "experiment@af.invalid");
        }
        let out = run_git(
            command,
            Some(format!("Snapshot {snapshot}\n").into_bytes()),
            "private commit-tree",
        )?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }
}

/// Encode exact accepted Snapshots as private Git objects for the existing native laboratory.
/// No worktree, ref in the source repository, remote or publication is touched.
pub(crate) fn experiment_source(
    request: &crate::scenario::source::SourceRequest,
    repo: &Path,
    state: &Path,
    into: &Path,
) -> Result<crate::scenario::source::SourceReceipt, String> {
    use crate::scenario::source::SourceReceipt;
    request.validate()?;
    let receipt = &request.receipt;
    let opened = open_task(&receipt.task_id, repo, Some(state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
    {
        return Err("Experiment source requires the exact accepted native Task result".into());
    }
    let selected = select(&opened, None)?;
    let baseline_snapshot_id = selected
        .source_id
        .as_ref()
        .ok_or("Task has no original source Snapshot")?;
    if !selected.verified
        || selected.snapshot_id != request.candidate_snapshot_id
        || request
            .baseline_snapshot_id
            .as_ref()
            .is_some_and(|id| id != baseline_snapshot_id)
    {
        return Err(
            "Experiment source differs from the Task's original source or verified candidate"
                .into(),
        );
    }
    let candidate_port = receipt
        .result
        .outputs
        .get("snapshot")
        .ok_or("No candidate source output")?;
    let source_port = opened
        .task
        .revision
        .inputs
        .get("source")
        .ok_or("No original source input")?;
    for (port, named) in [
        (candidate_port, request.candidate_snapshot_id.as_str()),
        (source_port, baseline_snapshot_id.as_str()),
    ] {
        let [artifact_id] = port.artifact_ids.as_slice() else {
            return Err("Experiment source requires one exact SourceTree artifact".into());
        };
        if port.artifact_type != SOURCE_TREE_V1
            || port.cardinality != PortCardinality::One
            || input_bindings::source_tree_snapshot(&opened.cas, artifact_id)? != named
        {
            return Err("Experiment source artifact differs from its Snapshot metadata".into());
        }
    }
    let (base, old) = review_source_git::task::read_snapshot(&opened.cas, baseline_snapshot_id)?;
    let (head, new) =
        review_source_git::task::read_snapshot(&opened.cas, &request.candidate_snapshot_id)?;
    let base_origin = review_source_git::task::read_origin(&opened.cas, &base.origin_id)?;
    let head_origin = review_source_git::task::read_origin(&opened.cas, &head.origin_id)?;
    if base_origin.repository_id() != head_origin.repository_id()
        || base_origin.repository_id().len() > 4096
    {
        return Err("Experiment Snapshots belong to different repositories".into());
    }
    for manifest in [&old, &new] {
        if manifest.entries.len() > 32768
            || manifest.entries.iter().any(|e| {
                !matches!(
                    e.kind,
                    review_source_git::EntryKind::File | review_source_git::EntryKind::Executable
                )
            })
            || manifest
                .entries
                .iter()
                .map(|e| u128::from(e.size))
                .sum::<u128>()
                > 1024 * 1024 * 1024
        {
            return Err("Native experiment source does not support this Snapshot's size, links or submodules".into());
        }
    }
    let parent = into
        .parent()
        .ok_or("Experiment capture needs an existing parent")?;
    let parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    {
        use std::os::unix::fs::MetadataExt;
        let info = std::fs::metadata(&parent).map_err(|e| e.to_string())?;
        if !info.is_dir()
            || info.uid() != nix::unistd::geteuid().as_raw()
            || info.mode() & 0o022 != 0
        {
            return Err(
                "Experiment capture parent must be owned and unwritable by other accounts".into(),
            );
        }
    }
    let target = parent.join(into.file_name().ok_or("Experiment capture needs a leaf")?);
    crate::scenario::source::isolated_destination(&target)?;
    let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let state = std::fs::canonicalize(state).map_err(|e| e.to_string())?;
    if target.starts_with(&repo)
        || repo.starts_with(&target)
        || target.starts_with(&state)
        || state.starts_with(&target)
    {
        return Err(
            "Experiment capture must be isolated from the repository and Task Store".into(),
        );
    }
    // Reuse the native Snapshot path alphabet and raw, filter-free Git materialization.
    let scratch = Scratch::initialize(tempfile::tempdir_in(&parent).map_err(|e| e.to_string())?)?;
    let native_repo = review_source_git::Repo::open(&repo, scratch.directory.path());
    if native_repo.repository_id().map_err(|e| e.to_string())? != base_origin.repository_id() {
        return Err("Experiment Task belongs to a different repository".into());
    }
    if !target.exists() {
        if native_repo
            .rev_parse(&request.trusted_ref)
            .map_err(|e| e.to_string())?
            != request.control_sha
        {
            return Err(
                "Install the exact reviewed main control before capturing a new experiment".into(),
            );
        }
        admit_experiment_control(&repo, scratch.directory.path(), &request.control_sha)?;
    }
    let old_entries: Vec<_> = old.entries.iter().collect();
    let new_entries: Vec<_> = new.entries.iter().collect();
    let old_blobs = scratch.blobs(&opened.cas, &old_entries)?;
    let new_blobs = scratch.blobs(&opened.cas, &new_entries)?;
    let old_tree = scratch.tree("base.index", &old_entries, &old_blobs)?;
    let new_tree = scratch.tree("head.index", &new_entries, &new_blobs)?;
    let baseline_sha = scratch.commit(&old_tree, None, baseline_snapshot_id)?;
    let candidate_sha = scratch.commit(
        &new_tree,
        Some(&baseline_sha),
        &request.candidate_snapshot_id,
    )?;
    let expected = SourceReceipt {
        schema: "af/scenario-experiment-source@1".into(),
        request_digest: request.digest()?,
        repository_id: base_origin.repository_id().into(),
        baseline_snapshot_id: baseline_snapshot_id.clone(),
        candidate_snapshot_id: request.candidate_snapshot_id.clone(),
        baseline_content_digest: base.content_digest,
        candidate_content_digest: head.content_digest,
        baseline_origin_id: base.origin_id,
        candidate_origin_id: head.origin_id,
        baseline_sha,
        candidate_sha,
        repository: target.join("repository.git"),
    };
    match std::fs::create_dir(&target) {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            // Only local object reads. The source's config/hooks/credentials are never copied.
            let mut fetch = scratch.git(None);
            fetch.env("GIT_NO_REPLACE_OBJECTS", "1");
            fetch
                .args([
                    "-c",
                    "protocol.file.allow=always",
                    "fetch",
                    "--quiet",
                    "--depth=1",
                    "--no-tags",
                    "--no-write-fetch-head",
                ])
                .arg(&repo)
                .arg(format!(
                    "{}:refs/heads/af-experiment-authority",
                    request.control_sha
                ))
                .arg(format!("{}:{}", request.control_sha, request.trusted_ref));
            run_git(fetch, None, "retaining installed control authority")?;
            for (name, sha) in [
                ("baseline", &expected.baseline_sha),
                ("candidate", &expected.candidate_sha),
            ] {
                let mut pin = scratch.git(None);
                pin.args([
                    "update-ref",
                    &format!("refs/heads/af-experiment-{name}"),
                    sha,
                ]);
                run_git(pin, None, "pinning private experiment source")?;
            }
            std::fs::rename(
                scratch.directory.path().join("objects.git"),
                &expected.repository,
            )
            .map_err(|e| e.to_string())?;
            let mut marker = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target.join("source.json"))
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            marker
                .write_all(&serde_json::to_vec(&expected).map_err(|e| e.to_string())?)
                .and_then(|()| marker.sync_all())
                .map_err(|e| e.to_string())?;
            std::fs::File::open(&target)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            use std::os::unix::fs::MetadataExt;
            let info = std::fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
            if !info.is_dir()
                || info.uid() != nix::unistd::geteuid().as_raw()
                || info.mode() & 0o077 != 0
            {
                return Err("Experiment capture is not a private owned directory".into());
            }
            let held = crate::scenario::source::read_receipt(&target.join("source.json"))?;
            if held != expected {
                return Err("Existing experiment capture has different authority or source".into());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    for (sha, tree) in [
        (&expected.baseline_sha, &old_tree),
        (&expected.candidate_sha, &new_tree),
    ] {
        let mut check = git(&expected.repository, scratch.directory.path());
        check.env("GIT_NO_REPLACE_OBJECTS", "1");
        check.args(["rev-parse", &format!("{sha}^{{tree}}")]);
        if String::from_utf8_lossy(&run_git(check, None, "verifying private source tree")?).trim()
            != tree
        {
            return Err("Private experiment Git objects differ from the native Snapshot".into());
        }
    }
    let mut fsck = git(&expected.repository, scratch.directory.path());
    fsck.env("GIT_NO_REPLACE_OBJECTS", "1");
    fsck.args(["fsck", "--strict", "--no-reflogs", "--no-dangling"]);
    run_git(fsck, None, "verifying private experiment objects")?;
    Ok(expected)
}

/// The two trees of the entries `base` and `head` differ in.
fn trees(cas: &Cas, base: &Manifest, head: &Manifest) -> Result<(Scratch, String, String), String> {
    let differs = |entry: &Entry, other: &Manifest| other.get(&entry.path) != Some(entry);
    let old: Vec<&Entry> = base.entries.iter().filter(|e| differs(e, head)).collect();
    let new: Vec<&Entry> = head.entries.iter().filter(|e| differs(e, base)).collect();
    let scratch = Scratch::new()?;
    let old_blobs = scratch.blobs(cas, &old)?;
    let new_blobs = scratch.blobs(cas, &new)?;
    let base_tree = scratch.tree("base.index", &old, &old_blobs)?;
    let head_tree = scratch.tree("head.index", &new, &new_blobs)?;
    Ok((scratch, base_tree, head_tree))
}

fn files(scratch: &Scratch, base: &str, head: &str) -> Result<Vec<TaskDiffFileV1>, String> {
    let names = scratch.diff(base, head, &["--name-status", "-z"])?;
    let mut fields = names.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let letter = status.first().copied().unwrap_or(b'M');
        let path = |field: Option<&[u8]>| field.map(encode_path).unwrap_or_default();
        let (status, old_path, path) = match letter {
            b'R' => {
                let old = path(fields.next());
                (TaskDiffStatusV1::Renamed, Some(old), path(fields.next()))
            }
            b'A' => (TaskDiffStatusV1::Added, None, path(fields.next())),
            b'D' => (TaskDiffStatusV1::Deleted, None, path(fields.next())),
            b'T' => (TaskDiffStatusV1::TypeChanged, None, path(fields.next())),
            _ => (TaskDiffStatusV1::Modified, None, path(fields.next())),
        };
        files.push(TaskDiffFileV1 {
            path,
            status,
            old_path,
            added: None,
            deleted: None,
        });
    }
    let numbers = scratch.diff(base, head, &["--numstat", "-z"])?;
    let mut fields = numbers.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let text = String::from_utf8_lossy(field).into_owned();
        let mut parts = text.splitn(3, '\t');
        let (added, deleted, rest) = (parts.next(), parts.next(), parts.next().unwrap_or(""));
        // A rename leaves the path empty here and names both paths in the next two fields.
        let path = if rest.is_empty() {
            let _old = fields.next();
            fields.next().map(encode_path).unwrap_or_default()
        } else {
            encode_path(rest.as_bytes())
        };
        if let Some(file) = files.iter_mut().find(|file| file.path == path) {
            file.added = added.and_then(|n| n.parse().ok());
            file.deleted = deleted.and_then(|n| n.parse().ok());
            if file.added.is_none() || file.deleted.is_none() {
                (file.added, file.deleted) = (None, None);
            }
        }
    }
    Ok(files)
}

/// `af task diff ID [--snapshot SHA] [--base SHA] [--stat | --name-only]`: from the Task's
/// source (or `--base`) to its candidate (or `--snapshot`), both Snapshots the Task recorded.
pub(crate) fn diff(
    id: &str,
    snapshot: Option<&str>,
    base: Option<&str>,
    stat: bool,
    name_only: bool,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let head = select(&opened, snapshot)?;
    let base_id = match base {
        Some(named) => select(&opened, Some(named))?.snapshot_id,
        None => head.source_id.clone().ok_or_else(|| {
            format!("Task {id} has no source Snapshot to diff against; name one with --base SHA")
        })?,
    };
    let (_, base_manifest) = review_source_git::task::read_snapshot(&opened.cas, &base_id)?;
    let (_, head_manifest) =
        review_source_git::task::read_snapshot(&opened.cas, &head.snapshot_id)?;
    let (scratch, base_tree, head_tree) = trees(&opened.cas, &base_manifest, &head_manifest)?;
    if inspect.json {
        let patch = scratch.diff(&base_tree, &head_tree, &["-p"])?;
        let document = TaskDiffV1 {
            schema: TASK_DIFF_V1.into(),
            task_id: id.into(),
            base_snapshot_id: base_id,
            snapshot_id: head.snapshot_id,
            files: files(&scratch, &base_tree, &head_tree)?,
            patch: String::from_utf8_lossy(&patch).into_owned(),
        };
        document.validate()?;
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    let flag = if stat {
        "--stat"
    } else if name_only {
        "--name-only"
    } else {
        "-p"
    };
    let out = scratch.diff(&base_tree, &head_tree, &[flag])?;
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&out)
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("writing the diff: {e}"))?;
    if out.is_empty() {
        eprintln!(
            "af task diff: Snapshot {} equals {base_id}: no change",
            head.snapshot_id
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: EntryKind, content: &[u8]) -> Entry {
        Entry {
            path: path.into(),
            kind,
            content: digest_bytes(content),
            size: content.len() as u64,
        }
    }

    #[test]
    fn a_candidate_writes_deletes_and_changes_modes_against_the_tracked_tree() {
        let from = Manifest::new(vec![
            entry("kept", EntryKind::File, b"a"),
            entry("changed", EntryKind::File, b"a"),
            entry("gone", EntryKind::File, b"a"),
            entry("tool", EntryKind::File, b"#!"),
            entry("untracked", EntryKind::File, b"u"),
        ])
        .unwrap();
        let to = Manifest::new(vec![
            entry("kept", EntryKind::File, b"a"),
            entry("changed", EntryKind::File, b"b"),
            entry("new", EntryKind::File, b"n"),
            entry("tool", EntryKind::Executable, b"#!"),
        ])
        .unwrap();
        let tracked: BTreeSet<String> = ["kept", "changed", "gone", "tool"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let (written, deleted, mode) = changes(&from, &to, &tracked);
        assert_eq!(written, ["changed", "new"]);
        assert_eq!(deleted, ["gone"], "an untracked file is never deleted");
        assert_eq!(mode, ["tool"]);
    }
}

/// Scenario lifecycle staging uses the exact native accepted candidate, never a Worker's
/// filesystem claim. This is an isolated object export; only the separately installed adapter
/// can create a source-repository branch/worktree or publish it under signed authority.
pub(crate) fn implementation_source(
    request: &crate::scenario::implementation::SourceRequest,
    repo: &Path,
    state: &Path,
    into: &Path,
) -> Result<crate::scenario::implementation::SourceReceipt, String> {
    use crate::scenario::implementation::SourceReceipt;
    request.validate()?;
    let receipt = &request.receipt;
    let opened = open_task(&receipt.task_id, repo, Some(state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision.kind != "implement"
        || opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
    {
        return Err(
            "Implementation staging requires the exact accepted implementation Task".into(),
        );
    }
    super::scenario_template::implementation_plan(&opened)?;
    let selected = select(&opened, None)?;
    if !selected.verified || selected.snapshot_id != request.candidate_snapshot_id {
        return Err("Implementation staging requires the accepted candidate Snapshot".into());
    }
    let source_id = selected
        .source_id
        .ok_or("Implementation has no original source")?;
    if let Some(expected) = &request.predecessor_snapshot_id {
        super::scenario_template::experiment_source_matches(&opened.cas, &source_id, expected)?;
    }
    let candidate_port = receipt
        .result
        .outputs
        .get("snapshot")
        .ok_or("Implementation lacks its candidate output")?;
    let source_port = opened
        .task
        .revision
        .inputs
        .get("source")
        .ok_or("Implementation lacks its original source input")?;
    for (port, snapshot) in [
        (candidate_port, selected.snapshot_id.as_str()),
        (source_port, source_id.as_str()),
    ] {
        let [artifact_id] = port.artifact_ids.as_slice() else {
            return Err("Implementation staging requires one exact SourceTree artifact".into());
        };
        if port.artifact_type != SOURCE_TREE_V1
            || port.cardinality != PortCardinality::One
            || input_bindings::source_tree_snapshot(&opened.cas, artifact_id)? != snapshot
        {
            return Err("Implementation source artifact differs from its Snapshot metadata".into());
        }
    }
    let (base, old) = review_source_git::task::read_snapshot(&opened.cas, &source_id)?;
    let (head, new) = review_source_git::task::read_snapshot(&opened.cas, &selected.snapshot_id)?;
    let origin = review_source_git::task::read_origin(&opened.cas, &base.origin_id)?;
    let head_origin = review_source_git::task::read_origin(&opened.cas, &head.origin_id)?;
    if head_origin.repository_id() != origin.repository_id() {
        return Err("Implementation candidate belongs to a different repository".into());
    }
    if new.entries.len() > 32768
        || new.entries.iter().any(|e| e.path.len() > 4096)
        || new.entries.iter().map(|e| u128::from(e.size)).sum::<u128>() > 64 * 1024 * 1024
    {
        return Err("Implementation staging exceeds supported Snapshot size".into());
    }
    let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let state = std::fs::canonicalize(state).map_err(|e| e.to_string())?;
    let parent = into
        .parent()
        .ok_or("Staging needs an existing private parent")?;
    let parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let info = std::fs::metadata(&parent).map_err(|e| e.to_string())?;
    if info.uid() != nix::unistd::geteuid().as_raw() || info.mode() & 0o077 != 0 {
        return Err("Implementation staging parent must be owner-only".into());
    }
    let target = parent.join(into.file_name().ok_or("Staging needs a leaf")?);
    crate::scenario::source::isolated_destination(&target)?;
    if target.starts_with(&repo)
        || repo.starts_with(&target)
        || target.starts_with(&state)
        || state.starts_with(&target)
    {
        return Err("Implementation staging overlaps repository or Task Store".into());
    }
    let scratch = Scratch::initialize(tempfile::tempdir_in(&parent).map_err(|e| e.to_string())?)?;
    let native = review_source_git::Repo::open(&repo, scratch.directory.path());
    if native.repository_id().map_err(|e| e.to_string())? != origin.repository_id() {
        return Err("Implementation belongs to a different repository".into());
    }
    let committed = Capture::new(&native, &opened.cas)
        .committed(&request.source_commit)
        .map_err(|e| e.to_string())?;
    if committed.content_digest != old.content_digest() {
        return Err("Implementation source bytes differ from the installed commit".into());
    }
    let entries: Vec<_> = new.entries.iter().collect();
    let blobs = scratch.blobs(&opened.cas, &entries)?;
    let tree = scratch.tree("candidate.index", &entries, &blobs)?;
    let transport = scratch.commit(&tree, None, &selected.snapshot_id)?;
    let expected = SourceReceipt {
        schema: "af/scenario-impl-source@1".into(),
        request_digest: request.digest()?,
        repository_id: origin.repository_id().into(),
        source_snapshot: source_id,
        candidate_snapshot: selected.snapshot_id,
        predecessor_snapshot: request.predecessor_snapshot_id.clone(),
        source_content_digest: base.content_digest,
        source_commit: request.source_commit.clone(),
        candidate_tree: tree.clone(),
        transport_commit: transport.clone(),
        repository: target.join("repository.git"),
    };
    // Publish only a complete capture. A process crash cannot expose an incomplete target
    // and a concurrent or foreign leaf is never replaced, even when it is an empty directory.
    let stage = scratch.directory.path().join("capture");
    std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let mut pin = scratch.git(None);
    pin.args(["update-ref", "refs/heads/af-impl-candidate", &transport]);
    run_git(pin, None, "pinning implementation staging objects")?;
    std::fs::rename(
        scratch.directory.path().join("objects.git"),
        stage.join("repository.git"),
    )
    .map_err(|e| e.to_string())?;
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join("source.json"))
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    marker
        .write_all(&serde_json::to_vec(&expected).map_err(|e| e.to_string())?)
        .and_then(|()| marker.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::File::open(&stage)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    match rustix::fs::renameat_with(
        rustix::fs::CWD,
        &stage,
        rustix::fs::CWD,
        &target,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            std::fs::File::open(&parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Err(rustix::io::Errno::EXIST) => {
            let info = std::fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
            if !info.is_dir()
                || info.uid() != nix::unistd::geteuid().as_raw()
                || info.mode() & 0o077 != 0
            {
                return Err("Implementation capture is not an owned private directory".into());
            }
            if SourceReceipt::read(&target.join("source.json"))? != expected {
                return Err("Existing implementation capture has different authority".into());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    let mut check = git(&expected.repository, scratch.directory.path());
    check
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(["rev-parse", &format!("{transport}^{{tree}}")]);
    if String::from_utf8_lossy(&run_git(
        check,
        None,
        "verifying implementation staging tree",
    )?)
    .trim()
        != tree
    {
        return Err("Private staging tree differs from accepted Snapshot".into());
    }
    let mut fsck = git(&expected.repository, scratch.directory.path());
    fsck.env("GIT_NO_REPLACE_OBJECTS", "1").args([
        "fsck",
        "--strict",
        "--no-reflogs",
        "--no-dangling",
    ]);
    run_git(fsck, None, "verifying implementation staging objects")?;
    Ok(expected)
}
