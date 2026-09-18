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
//! by a host path, and nothing about the root is consulted without its marker.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use review_core::{WorkspaceBasisV1, WorkspaceFallbackReasonV1, is_workspace_id};
use review_source_git::{Manifest, apply_tree_diff, materialize, scan_tree};
use review_store::Cas;

use crate::{Mode, SandboxTemplate, clone_tree, restore_writable_dirs};

const TREE: &str = "tree";
const NEXT_TREE: &str = "tree.next";
const OLD_TREE: &str = "tree.old";
const MANIFEST: &str = "manifest.json";
const HEAD: &str = "head.json";
const HEAD_SCHEMA: &str = "af.warm-workspace/1";

/// The machine-local root every Warm Workspace lives under: `$XDG_CACHE_HOME/af/workspaces`,
/// or `~/.cache/af/workspaces` when the variable is unset. A relative value is refused.
pub fn default_workspace_cache_root() -> Result<PathBuf, std::io::Error> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        Some(cache) => PathBuf::from(cache),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    std::io::Error::other(
                        "neither XDG_CACHE_HOME nor HOME locates the warm workspace cache",
                    )
                })?;
            PathBuf::from(home).join(".cache")
        }
    };
    if !base.is_absolute() {
        return Err(std::io::Error::other(
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
    id: String,
    path: PathBuf,
}

impl WorkspaceRoot {
    pub fn new(cache_root: &Path, workspace_id: &str) -> Result<Self, std::io::Error> {
        if !is_workspace_id(workspace_id) {
            return Err(std::io::Error::other(format!(
                "`{workspace_id}` is not a workspace identity"
            )));
        }
        if !cache_root.is_absolute() {
            return Err(std::io::Error::other(
                "the warm workspace cache root must be an absolute directory",
            ));
        }
        Ok(Self {
            id: workspace_id.to_string(),
            path: cache_root.join(workspace_id),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
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
    /// The head Snapshot the previous verified template held, when there was one.
    pub from_snapshot_id: Option<String>,
    /// The Tree Digest the resulting template holds: always the head's own.
    pub verified_digest: String,
    /// Entries written or removed: every entry for a full materialization, the diff's path set
    /// for a rebase, zero for a reused template.
    pub entries_touched: u64,
    /// Host-observed time the preparation took.
    pub preparation_ms: u64,
}

/// The previous verified state of a root, read from its marker and manifest.
struct VerifiedTemplate {
    snapshot_id: String,
    content_digest: String,
    manifest: Manifest,
}

struct Outcome {
    basis: WorkspaceBasisV1,
    fallback: Option<WorkspaceFallbackReasonV1>,
    entries_touched: u64,
}

/// Bring the root's template to `head`. An unchanged head materializes nothing; a changed head
/// re-bases the previous verified template and verifies the result's digest; anything the root
/// cannot prove falls back to a full materialization with a recorded reason. The returned
/// template is trusted only because the root's marker says the tree was verified to hold the
/// head, and the marker is rewritten only after that verification.
pub fn prepare_workspace(
    root: &WorkspaceRoot,
    head: &Manifest,
    head_snapshot_id: &str,
    cas: &Cas,
) -> Result<WorkspacePreparation, std::io::Error> {
    head.validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if !review_core::is_digest(head_snapshot_id) {
        return Err(std::io::Error::other(
            "a Warm Workspace head needs a Snapshot ID",
        ));
    }
    let clock = Instant::now();
    std::fs::create_dir_all(root.path())?;
    if std::fs::symlink_metadata(root.path())?
        .file_type()
        .is_symlink()
    {
        return Err(std::io::Error::other(format!(
            "warm workspace root {} is a symlink",
            root.path().display()
        )));
    }
    // Leftovers of a preparation that ended between its steps carry no marker and no trust.
    remove_tree(&root.path().join(NEXT_TREE));
    remove_tree(&root.path().join(OLD_TREE));
    let head_digest = head.content_digest();
    let (previous, missing) = match read_verified(root) {
        Ok(previous) => (previous, WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        Err(()) => (None, WorkspaceFallbackReasonV1::TemplateCorrupt),
    };
    let from_snapshot_id = previous.as_ref().map(|found| found.snapshot_id.clone());
    let outcome = match previous {
        Some(previous) if previous.content_digest == head_digest => Outcome {
            basis: WorkspaceBasisV1::Reused,
            fallback: None,
            entries_touched: 0,
        },
        Some(previous) => match rebase(root, &previous.manifest, head, cas) {
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
        None => materialize_full(root, head, head_snapshot_id, &head_digest, cas, missing)?,
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
) -> Result<Outcome, std::io::Error> {
    let next = root.path().join(NEXT_TREE);
    if let Err(error) = materialize(head, cas, &next) {
        remove_tree(&next);
        return Err(std::io::Error::other(error.to_string()));
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
) -> Result<(), std::io::Error> {
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

fn remove_marker(root: &WorkspaceRoot) -> Result<(), std::io::Error> {
    match std::fs::remove_file(root.head_marker()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
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
    let snapshot_id = field("snapshot_id")?;
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
        snapshot_id,
        content_digest,
        manifest,
    }))
}

/// Best-effort removal of a tree the root no longer trusts or needs.
fn remove_tree(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok() {
        restore_writable_dirs(path);
        let _ = std::fs::remove_dir_all(path);
    }
}
