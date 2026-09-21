//! Warm Workspaces (Worker warm layers, package P3): one stable template root per node per
//! Campaign, re-based to each new head by tree diff and verified against the head's Tree Digest.
//!
//! The root lives under the machine's cache directory and holds three things: the template tree
//! that per-Attempt sandboxes are cloned from, the manifest that tree was last verified to hold,
//! and a head marker naming the Snapshot and Tree Digest of that verification. The marker is
//! written last and removed first, so a preparation that ends early leaves a root the next
//! preparation refuses to trust: it falls back to a full materialization and records why. A
//! rebase never edits the trusted tree in place: it applies the diff to a copy-on-write clone,
//! scans the clone back into a manifest, compares that manifest's digest with the head's, and
//! only then swaps the clone in. Per-Attempt sandboxes remain fresh clones of the template, so
//! sibling isolation is exactly what it was with a temporary template.
//!
//! Warmth stays an artifact: the durable record names the root by an opaque identity and never
//! by a host path, nothing about the root is consulted without its marker, and the marker is
//! believed only when the Campaign log recorded the preparation that wrote it. Errors that
//! leave this module carry no host path; the detail stays with the operator.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use review_core::{WorkspaceBasisV1, WorkspaceFallbackReasonV1, is_workspace_id};
use review_source_git::{Manifest, apply_tree_diff, materialize, scan_tree};
use review_store::Cas;

use crate::{Mode, SandboxTemplate, clone_tree, restore_writable_dirs};

/// Why a workspace could not be prepared. The display text is fixed and path-free, so it may
/// travel into a failed node outcome or a Task diagnostic; the host path and the underlying
/// error stay in [`WorkspaceError::operator_detail`], for a terminal only.
#[derive(Debug)]
pub struct WorkspaceError {
    kind: WorkspaceErrorKind,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceErrorKind {
    /// The head manifest, its Snapshot ID or the recorded preparation is not usable.
    InvalidHead,
    /// The cache root cannot be located or is not an absolute directory.
    CacheRootUnavailable,
    /// The workspace identity is malformed, or the root exists but is not a real directory.
    RootUnavailable,
    /// The head could not be written from the CAS.
    MaterializationFailed,
    /// Another filesystem operation on the root failed.
    Io,
}

impl WorkspaceError {
    fn new(kind: WorkspaceErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// The detail with whatever path or system message it carries: for stderr, never for an
    /// event, an artifact or a report.
    pub fn operator_detail(&self) -> String {
        format!("{self}: {}", self.detail)
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            WorkspaceErrorKind::InvalidHead => "the Warm Workspace head is not usable",
            WorkspaceErrorKind::CacheRootUnavailable => {
                "the warm workspace cache root is unavailable"
            }
            WorkspaceErrorKind::RootUnavailable => "the warm workspace root is unavailable",
            WorkspaceErrorKind::MaterializationFailed => {
                "the Warm Workspace could not be materialized from the CAS"
            }
            WorkspaceErrorKind::Io => "a warm workspace filesystem operation failed",
        })
    }
}

impl std::error::Error for WorkspaceError {}

impl From<std::io::Error> for WorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::new(WorkspaceErrorKind::Io, error.to_string())
    }
}

/// The Campaign log's last `WorkspaceRebased@1` for a workspace: the head it was verified to
/// hold and the digest that verification produced. The root's marker is trusted only when it
/// claims exactly this digest; lineage comes from here, never from the marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPreparation {
    pub snapshot_id: String,
    pub verified_digest: String,
}

const TREE: &str = "tree";
const NEXT_TREE: &str = "tree.next";
const OLD_TREE: &str = "tree.old";
const MANIFEST: &str = "manifest.json";
const HEAD: &str = "head.json";
const HEAD_SCHEMA: &str = "af.warm-workspace/1";

/// The machine-local root every Warm Workspace lives under: `$XDG_CACHE_HOME/af/workspaces`,
/// or `~/.cache/af/workspaces` when the variable is unset. A relative value is refused.
pub fn default_workspace_cache_root() -> Result<PathBuf, WorkspaceError> {
    let unavailable =
        |detail: &str| WorkspaceError::new(WorkspaceErrorKind::CacheRootUnavailable, detail);
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        Some(cache) => PathBuf::from(cache),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    unavailable("neither XDG_CACHE_HOME nor HOME locates the warm workspace cache")
                })?;
            PathBuf::from(home).join(".cache")
        }
    };
    if !base.is_absolute() {
        return Err(unavailable(
            "the warm workspace cache root must be an absolute directory",
        ));
    }
    Ok(base.join("af").join("workspaces"))
}

/// The opaque identity of one node's workspace within one Campaign: the lowercase hex prefix
/// of a domain-separated digest over the Campaign run, its Campaign Manifest and the node. No
/// label and no host path takes part, so the identity can be recorded durably.
pub fn workspace_id(run_id: &str, campaign_manifest_id: &str, node: &str) -> String {
    let mut bytes = b"review.kernel/warm-workspace/v1\0".to_vec();
    for field in [run_id, campaign_manifest_id, node] {
        bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    let digest = review_store::canonical::blob_content_id(&bytes);
    digest
        .strip_prefix("sha256:")
        .unwrap_or(&digest)
        .chars()
        .take(review_core::WORKSPACE_ID_HEX_LEN)
        .collect()
}

/// One node's stable root below the cache root: `<cache_root>/<workspace_id>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    path: PathBuf,
}

impl WorkspaceRoot {
    pub fn new(cache_root: &Path, workspace_id: &str) -> Result<Self, WorkspaceError> {
        if !is_workspace_id(workspace_id) {
            return Err(WorkspaceError::new(
                WorkspaceErrorKind::RootUnavailable,
                format!("`{workspace_id}` is not a workspace identity"),
            ));
        }
        if !cache_root.is_absolute() {
            return Err(WorkspaceError::new(
                WorkspaceErrorKind::CacheRootUnavailable,
                "the warm workspace cache root must be an absolute directory",
            ));
        }
        Ok(Self {
            path: cache_root.join(workspace_id),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The template tree per-Attempt sandboxes are cloned from.
    pub fn tree(&self) -> PathBuf {
        self.path.join(TREE)
    }

    fn head_marker(&self) -> PathBuf {
        self.path.join(HEAD)
    }

    fn manifest_file(&self) -> PathBuf {
        self.path.join(MANIFEST)
    }
}

/// What preparing a node's workspace for a head did, and the template that resulted.
pub struct WorkspacePreparation {
    /// The verified template at the stable root; per-Attempt sandboxes clone it.
    pub template: SandboxTemplate,
    pub basis: WorkspaceBasisV1,
    /// Why the head was materialized in full, when it was.
    pub fallback: Option<WorkspaceFallbackReasonV1>,
    /// The head Snapshot the Campaign last recorded for this workspace, when it recorded one.
    /// Taken from the durable record the caller supplied, never from the root's marker.
    pub from_snapshot_id: Option<String>,
    /// The Tree Digest the resulting template holds: always the head's own.
    pub verified_digest: String,
    /// Entries written or removed: every entry for a full materialization, the diff's path set
    /// for a rebase, zero for a reused template.
    pub entries_touched: u64,
    /// Host-observed time the preparation took.
    pub preparation_ms: u64,
}

/// The previous verified state of a root, read from its marker and manifest. The marker's
/// Snapshot ID is checked for shape and otherwise ignored: lineage comes from the log.
struct VerifiedTemplate {
    content_digest: String,
    manifest: Manifest,
}

struct Outcome {
    basis: WorkspaceBasisV1,
    fallback: Option<WorkspaceFallbackReasonV1>,
    entries_touched: u64,
}

/// Bring the root's template to `head`. An unchanged head materializes nothing but is read back
/// and verified before it is served; a changed head re-bases the previous verified template on
/// a clone and verifies the result's digest; anything the root cannot prove falls back to a
/// full materialization with a recorded reason. The root's marker vouches for the tree only
/// when it agrees with `recorded`, the Campaign log's last preparation of this workspace: a
/// marker the log never recorded, whether left by a preparation that ended before its record
/// or written by hand, is not trusted and supplies no lineage.
pub fn prepare_workspace(
    root: &WorkspaceRoot,
    head: &Manifest,
    head_snapshot_id: &str,
    cas: &Cas,
    recorded: Option<&RecordedPreparation>,
) -> Result<WorkspacePreparation, WorkspaceError> {
    let invalid = |detail: &str| WorkspaceError::new(WorkspaceErrorKind::InvalidHead, detail);
    head.validate()
        .map_err(|error| invalid(&error.to_string()))?;
    if !review_core::is_digest(head_snapshot_id) {
        return Err(invalid("a Warm Workspace head needs a Snapshot ID"));
    }
    if recorded.is_some_and(|record| {
        !review_core::is_digest(&record.snapshot_id)
            || !review_core::is_digest(&record.verified_digest)
    }) {
        return Err(invalid("the recorded preparation is malformed"));
    }
    let clock = Instant::now();
    std::fs::create_dir_all(root.path())?;
    if std::fs::symlink_metadata(root.path())?
        .file_type()
        .is_symlink()
    {
        return Err(WorkspaceError::new(
            WorkspaceErrorKind::RootUnavailable,
            format!("warm workspace root {} is a symlink", root.path().display()),
        ));
    }
    // Leftovers of a preparation that ended between its steps carry no marker and no trust.
    remove_tree(&root.path().join(NEXT_TREE));
    remove_tree(&root.path().join(OLD_TREE));
    let head_digest = head.content_digest();
    let from_snapshot_id = recorded.map(|record| record.snapshot_id.clone());
    // Trust is by digest: the log's last record says which Tree Digest was verified at this
    // root, and the marker must claim exactly that. The Snapshot ID the marker carries is
    // informational; a reuse under a new Snapshot ID of the same tree rewrites nothing.
    let trusted = match read_verified(root) {
        Ok(None) => Err(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        Err(()) => Err(WorkspaceFallbackReasonV1::TemplateCorrupt),
        Ok(Some(marker)) => match recorded {
            Some(record) if record.verified_digest == marker.content_digest => Ok(marker),
            _ => Err(WorkspaceFallbackReasonV1::UnrecordedPreparation),
        },
    };
    let outcome = match trusted {
        Ok(previous) if previous.content_digest == head_digest => {
            // Nothing is written, but the tree is read back in full: the marker vouches for
            // what was verified when it was written, not for what the tree holds now.
            match scan_tree(root.tree(), head.path_encoding) {
                Ok(scanned) if scanned.content_digest() == head_digest => Outcome {
                    basis: WorkspaceBasisV1::Reused,
                    fallback: None,
                    entries_touched: 0,
                },
                _ => materialize_full(
                    root,
                    head,
                    head_snapshot_id,
                    &head_digest,
                    cas,
                    WorkspaceFallbackReasonV1::TemplateCorrupt,
                )?,
            }
        }
        Ok(previous) => match rebase(root, &previous.manifest, head, cas) {
            Ok(entries_touched) => {
                commit(root, head, head_snapshot_id, &head_digest)?;
                Outcome {
                    basis: WorkspaceBasisV1::Rebased,
                    fallback: None,
                    entries_touched,
                }
            }
            Err(reason) => {
                materialize_full(root, head, head_snapshot_id, &head_digest, cas, reason)?
            }
        },
        Err(reason) => materialize_full(root, head, head_snapshot_id, &head_digest, cas, reason)?,
    };
    Ok(WorkspacePreparation {
        template: SandboxTemplate::at_stable_root(head.clone(), root.tree()),
        basis: outcome.basis,
        fallback: outcome.fallback,
        from_snapshot_id,
        verified_digest: head_digest,
        entries_touched: outcome.entries_touched,
        preparation_ms: clock.elapsed().as_millis() as u64,
    })
}

/// Apply the diff to a copy-on-write clone of the trusted tree and verify the clone. On success
/// the clone waits at `tree.next` for [`commit`]; on any failure it is removed and the reason
/// returned, leaving the trusted tree untouched.
fn rebase(
    root: &WorkspaceRoot,
    previous: &Manifest,
    head: &Manifest,
    cas: &Cas,
) -> Result<u64, WorkspaceFallbackReasonV1> {
    let next = root.path().join(NEXT_TREE);
    let result = rebase_into(&next, root, previous, head, cas);
    if result.is_err() {
        remove_tree(&next);
    }
    result
}

fn rebase_into(
    next: &Path,
    root: &WorkspaceRoot,
    previous: &Manifest,
    head: &Manifest,
    cas: &Cas,
) -> Result<u64, WorkspaceFallbackReasonV1> {
    clone_tree(&root.tree(), next, Mode::EphemeralWrite)
        .map_err(|_| WorkspaceFallbackReasonV1::ApplyFailed)?;
    // The clone must hold exactly the previous manifest before any entry is unlinked or
    // written. A template that drifted under its marker, a directory replaced by a symlink
    // included, is caught here while nothing outside the clone has been touched; the apply
    // below then removes entries only through real directories.
    let cloned = scan_tree(next, previous.path_encoding)
        .map_err(|_| WorkspaceFallbackReasonV1::TemplateCorrupt)?;
    if cloned.content_digest() != previous.content_digest() {
        return Err(WorkspaceFallbackReasonV1::TemplateCorrupt);
    }
    let touched = apply_tree_diff(previous, head, cas, next)
        .map_err(|_| WorkspaceFallbackReasonV1::ApplyFailed)?;
    let scanned =
        scan_tree(next, head.path_encoding).map_err(|_| WorkspaceFallbackReasonV1::ApplyFailed)?;
    if scanned.content_digest() != head.content_digest() {
        return Err(WorkspaceFallbackReasonV1::DigestMismatch);
    }
    Ok(touched)
}

/// Materialize the head from the CAS into `tree.next` and commit it. Every object is verified
/// against its digest as it is written, so the result holds the head by construction.
fn materialize_full(
    root: &WorkspaceRoot,
    head: &Manifest,
    head_snapshot_id: &str,
    head_digest: &str,
    cas: &Cas,
    reason: WorkspaceFallbackReasonV1,
) -> Result<Outcome, WorkspaceError> {
    let next = root.path().join(NEXT_TREE);
    if let Err(error) = materialize(head, cas, &next) {
        remove_tree(&next);
        return Err(WorkspaceError::new(
            WorkspaceErrorKind::MaterializationFailed,
            error.to_string(),
        ));
    }
    commit(root, head, head_snapshot_id, head_digest)?;
    Ok(Outcome {
        basis: WorkspaceBasisV1::Full,
        fallback: Some(reason),
        entries_touched: head.entries.len() as u64,
    })
}

/// Swap the verified `tree.next` in and rewrite the manifest and marker. The marker is removed
/// before the swap and written after the manifest, so at no point does a marker vouch for a
/// tree that was not verified to hold its head.
fn commit(
    root: &WorkspaceRoot,
    head: &Manifest,
    head_snapshot_id: &str,
    head_digest: &str,
) -> Result<(), WorkspaceError> {
    let tree = root.tree();
    let next = root.path().join(NEXT_TREE);
    let old = root.path().join(OLD_TREE);
    remove_marker(root)?;
    if std::fs::symlink_metadata(&tree).is_ok() {
        std::fs::rename(&tree, &old)?;
    }
    std::fs::rename(&next, &tree)?;
    let manifest = serde_json::to_vec(head).map_err(std::io::Error::other)?;
    write_atomically(&root.manifest_file(), &manifest)?;
    let marker = serde_json::json!({
        "schema": HEAD_SCHEMA,
        "snapshot_id": head_snapshot_id,
        "content_digest": head_digest,
    });
    let marker = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
    write_atomically(&root.head_marker(), &marker)?;
    remove_tree(&old);
    Ok(())
}

fn remove_marker(root: &WorkspaceRoot) -> Result<(), WorkspaceError> {
    match std::fs::remove_file(root.head_marker()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// The root's previous verified state: `None` without a marker, `Err` when the marker, the
/// manifest and the tree do not agree. Either way the caller materializes in full; they differ
/// only in the reason recorded.
fn read_verified(root: &WorkspaceRoot) -> Result<Option<VerifiedTemplate>, ()> {
    let marker = match std::fs::read(root.head_marker()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let marker: serde_json::Value = serde_json::from_slice(&marker).map_err(|_| ())?;
    if marker.get("schema").and_then(serde_json::Value::as_str) != Some(HEAD_SCHEMA) {
        return Err(());
    }
    let field = |name: &str| {
        marker
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| review_core::is_digest(value))
            .map(str::to_owned)
            .ok_or(())
    };
    field("snapshot_id")?;
    let content_digest = field("content_digest")?;
    let manifest: Manifest = std::fs::read(root.manifest_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(())?;
    if manifest.validate().is_err() || manifest.content_digest() != content_digest {
        return Err(());
    }
    let tree = std::fs::symlink_metadata(root.tree()).map_err(|_| ())?;
    if tree.file_type().is_symlink() || !tree.is_dir() {
        return Err(());
    }
    Ok(Some(VerifiedTemplate {
        content_digest,
        manifest,
    }))
}

/// Best-effort removal of a tree the root no longer trusts or needs. Only a real directory is
/// walked; a symlink or a stray file where a tree should be is unlinked as it is, so nothing
/// it points at is ever chmod'ed or removed.
fn remove_tree(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        restore_writable_dirs(path);
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}
