//! Sandboxes: where a reviewer or check may run, and what that costs.
//!
//! # The honest part first
//!
//! The provider implemented here is `trusted_local`: a materialized copy of a snapshot in a
//! temporary directory, with an optional read-only mode. **It is not security isolation.** A
//! process running as the same user can `chmod` its way out of read-only mode, read anything the
//! user can read, and open any socket. It buys two real things — the canonical checkout is not
//! reachable, and every mutation is captured — and it buys nothing else. The environment a check
//! runs with is not the sandbox's doing: `review_check::CheckRunner` clears it and rebuilds it
//! from an allowlist, and a container run starts from `--env-file /dev/null` plus the declared
//! variables.
//!
//! That distinction is enforced rather than documented. A [`Sandbox`] declares the
//! [`Isolation`] it actually provides, a pipeline declares the isolation it requires, and
//! [`admit`] refuses the pairing that does not satisfy it. The design's own risk register names
//! this failure — *"worktree mistaken for security sandbox"* — and the way to not make it is to
//! make the weaker provider unable to claim the stronger property.
//!
//! A [`ContainerProvider`] is also here, for hosts that have a usable runtime. Its detection is a
//! *probe*, not a lookup: on the machine this was written both `docker` and `podman` are
//! installed and neither daemon is reachable, so a provider that stopped at `which` would have
//! declared containment and delivered none.
//!
//! So the containment probes recorded in `docs/security/containment-probes.md` are only
//! **partly** discharged here. The probes for the canonical checkout, inherited credentials (by
//! the check runner's cleared environment) and argument injection are covered. The probes for a
//! host marker outside the sandbox and for undeclared network are *not*, and cannot be by a
//! provider of this kind. They close only when the container provider runs against a live daemon,
//! and that record says so rather than being quietly narrowed to what passes.

pub mod build_cache;
pub mod cache;
pub mod check_runtime;
pub mod container;
#[cfg(unix)]
pub mod removal;
pub mod seal;
pub mod seal_ignore;
pub mod stale;
pub mod storage;
pub mod task_build_cache;
pub mod toolchain;
pub mod workspace;

pub use build_cache::{
    CapturedBuildCache, MaterializedBuildCache, build_cache_environment, capture_build_cache,
    materialize_build_cache, prepare_build_cache_root,
};
pub use cache::{
    CacheEnvironment, CacheError, CacheErrorKind, CacheKind, CacheLimits, CacheMaterialization,
    CacheSnapshot, CacheSource, materialize_cache, materialize_cache_into_runtime,
    remove_materialized_caches,
};
pub use check_runtime::{CONTAINER_CHECK_RUNTIME, CheckRuntime};
pub use container::{Availability, ContainerProvider};
#[cfg(unix)]
pub use removal::{
    AbandonedClaim, ClaimSize, Identity, PartlyRemoved, REMOVAL_MAX_DEPTH, Stated, Unmeasured,
    abandoned_claims, born_at, born_of_descriptor, claim_name, claim_owner_at,
    deeper_than_removal_walk, finish_abandoned_claims, is_incomplete, open_anchor, open_beneath,
    partial, partly_removed, remove_beneath, remove_beneath_measured, remove_tree_at,
    remove_tree_measured_at,
};
pub use seal::{MutationSet, SealedSandbox};
pub use seal_ignore::{DEFAULT_SEAL_IGNORE, IgnoreRules, SEAL_IGNORE_FILE};
pub use stale::{
    SweepReport, attempt_directory, is_af_directory_name, remove_tree_nofollow, sandbox_root,
    set_sandbox_root, sweep_stale_sandboxes, sweep_stale_sandboxes_in, sweep_stale_tree_digests,
    sweep_stale_tree_digests_in,
};
pub use task_build_cache::{
    Ensured, KEY_LOCK, TaskBuildCacheKeyLock, TaskBuildCacheLock, Uninspectable, WARM_KINDS,
    WarmDirectory, clone_warm_kind, default_task_build_cache_root, directory_bytes, is_size_record,
    lock_task_build_cache, lock_task_build_cache_key, lock_task_build_cache_key_waiting,
    read_key_eviction, read_key_holder, record_key_eviction, recorded_key_size,
};
pub use workspace::{
    RecordedPreparation, WORKSPACE_LOCK, WorkspaceError, WorkspaceInUse, WorkspacePreparation,
    WorkspaceRoot, default_workspace_cache_root, hold_workspace_for_removal, prepare_workspace,
    workspace_id, workspace_in_use,
};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_source_git::{EntryKind, Manifest, fs_path, materialize};
use review_store::Cas;

/// What a provider genuinely enforces. Ordered: a stronger level satisfies a weaker requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Isolation {
    /// A directory. Filesystem conventions only — no boundary a determined process respects.
    None,
    /// A container or VM: filesystem, network and credentials are genuinely out of reach.
    Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Nothing may be written. Reviewers that only read get this.
    ReadOnly,
    /// The sandbox may be mutated freely — a TDD reviewer needs to edit and run tests. Every
    /// mutation is captured at seal time, and none of it can reach the source.
    EphemeralWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The weakest isolation this pipeline will accept.
    pub require: Isolation,
}

impl Policy {
    /// A pipeline that may auto-apply patches, or that reviews code it does not trust, must
    /// demand real isolation.
    pub fn safe() -> Policy {
        Policy {
            require: Isolation::Container,
        }
    }

    /// A pipeline reviewing its own trusted repository on a developer's machine.
    pub fn trusted_local() -> Policy {
        Policy {
            require: Isolation::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The provider offers less than the pipeline requires. Always fatal: a pipeline that
    /// silently downgraded would produce a verdict whose meaning nobody could state.
    InsufficientIsolation {
        required: Isolation,
        provided: Isolation,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::InsufficientIsolation { required, provided } => write!(
                f,
                "pipeline requires {required:?} isolation but the sandbox provides only \
                 {provided:?}; refusing rather than reviewing under a weaker boundary than declared"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Check a sandbox against a pipeline's requirement. Fails closed.
pub fn admit(policy: Policy, sandbox: &Sandbox) -> Result<(), PolicyError> {
    if sandbox.isolation < policy.require {
        return Err(PolicyError::InsufficientIsolation {
            required: policy.require,
            provided: sandbox.isolation,
        });
    }
    Ok(())
}

/// A materialized snapshot a node may run against.
///
/// `isolation` is deliberately private: it is a *claim* other code makes decisions on, and a
/// claim anyone could write would let a plain temp directory assert containment — the exact
/// forgery [`admit`] exists to refuse. Only a provider in this crate can set it.
pub struct Sandbox {
    root: PathBuf,
    isolation: Isolation,
    /// The manifest as materialized. Sealing diffs against this, so "what did the reviewer
    /// change" is computed rather than reported by the reviewer.
    baseline: Arc<Manifest>,
    /// Kept so the directory outlives the handle and is removed with it. An `Option` only so
    /// [`Sandbox::into_parts`] and [`Sandbox::preserve`] can move it out while the `Drop` below
    /// still runs.
    _dir: Option<tempfile::TempDir>,
    /// The base attached beside the tree, as attached: what sealing verifies the Worker left
    /// unchanged (ADR-0206).
    attached: std::sync::Mutex<Option<AttachedBase>>,
}

/// The base tree and the files beside it as [`Sandbox::attach_base`] wrote them. Their
/// read-only modes stop an accidental write, not an owner who restores write permission:
/// sealing compares both with this record and refuses a sandbox whose base changed.
pub(crate) struct AttachedBase {
    /// Every entry under `base/` as attached, by its path below it: its inode, size, mode and
    /// modification and change times. A write, a `chmod`, a rename or a removal changes one of
    /// them, and comparing them costs one `stat` per entry instead of hashing the tree again.
    pub(crate) stats: BTreeMap<PathBuf, EntryStat>,
    pub(crate) beside: Vec<(String, Vec<u8>)>,
    /// The template the base was cloned from, kept alive while this sandbox lives so another
    /// judge of the same change can clone it too.
    _template: Option<Arc<SandboxTemplate>>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A read-only sandbox left its directories at 0o555, and unlinking an entry needs
        // write on its parent — so `TempDir`'s own cleanup would fail silently and strand a
        // whole materialized tree in TMPDIR. Restore writability first, then let the TempDir
        // (dropped after this body) remove the tree, and the read-only `base/` beside it.
        match &self._dir {
            Some(dir) => restore_writable_dirs(dir.path()),
            None => restore_writable_dirs(&self.root),
        }
    }
}

/// Make every directory under `root` writable by its owner again, so a subsequent
/// `remove_dir_all` can unlink what is inside them. Best-effort: a failure here only means the
/// TempDir cleanup that follows will do no worse than before.
fn restore_writable_dirs(root: &Path) {
    let mut level = vec![root.to_path_buf()];
    while !level.is_empty() {
        let children = review_parallel::try_map_owned(level, |dir| {
            let _ = ensure_directory_mode(&dir, 0o700);
            let paths = std::fs::read_dir(&dir)
                .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
                .unwrap_or_default();
            review_parallel::try_map_owned(paths, |path| {
                let is_directory = std::fs::symlink_metadata(&path)
                    .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
                    .unwrap_or(false);
                Ok::<_, ()>(is_directory.then_some(path))
            })
            .map(|children| children.into_iter().flatten().collect::<Vec<_>>())
        })
        .expect("best-effort writable-directory walk is infallible");
        level = children.into_iter().flatten().collect();
    }
}

pub(crate) fn ensure_directory_mode(path: &Path, required: u32) -> std::io::Result<()> {
    use nix::fcntl::AT_FDCWD;
    use nix::sys::stat::{FchmodatFlags, Mode as NixMode, fchmodat};
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    if metadata.permissions().mode() & required != required {
        // `AT_SYMLINK_NOFOLLOW` closes the lstat/chmod race: if a reviewer replaces this
        // directory with a symlink, the replacement's target is never modified.
        let _ = fchmodat(
            AT_FDCWD,
            path,
            NixMode::from_bits_truncate(((metadata.permissions().mode() & 0o7777) | required) as _),
            FchmodatFlags::NoFollowSymlink,
        );
    }
    Ok(())
}

/// Recreate `src`'s tree at `dst`, copy-on-write cloning each regular file. Directories are
/// recreated (a clone is a fresh writable tree), symlinks are recreated as symlinks (they must
/// not be dereferenced), and regular files are reflinked — sharing blocks until one side
/// writes — with a plain copy where the filesystem does not support reflinks. Writable clones
/// preserve source permissions. Read-only clones strip write permission in the same file batch
/// and return the already-discovered directories for a child-before-parent chmod pass.
fn clone_tree(src: &Path, dst: &Path, mode: Mode) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dst)?;
    let mut level = vec![(src.to_path_buf(), dst.to_path_buf())];
    let mut directories = vec![dst.to_path_buf()];
    let mut pending_files = Vec::new();
    while !level.is_empty() {
        let (scanned, ()) = review_parallel::try_join(
            || review_parallel::try_map_owned(level, |pair| scan_clone_directory(pair, mode)),
            || clone_file_batch(pending_files),
        )?;
        let mut next_level = Vec::new();
        let mut next_files = Vec::new();
        for scan in scanned {
            for child in &scan.directories {
                directories.push(child.1.clone());
            }
            next_level.extend(scan.directories);
            next_files.extend(scan.files);
        }
        level = next_level;
        pending_files = next_files;
    }
    clone_file_batch(pending_files)?;
    Ok(directories)
}

type CloneFile = (PathBuf, PathBuf, bool, Option<u32>);

struct CloneScan {
    directories: Vec<(PathBuf, PathBuf)>,
    files: Vec<CloneFile>,
}

fn scan_clone_directory(
    (from_dir, to_dir): (PathBuf, PathBuf),
    mode: Mode,
) -> std::io::Result<CloneScan> {
    let paths = std::fs::read_dir(&from_dir)?
        .map(|entry| {
            entry.map(|entry| {
                let from = entry.path();
                let to = to_dir.join(entry.file_name());
                (from, to)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let classified =
        review_parallel::try_map_owned(paths, |(from, to)| classify_clone_entry(from, to, mode))?;
    let mut scan = CloneScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    for entry in classified {
        match entry {
            CloneEntry::Directory(from, to) => {
                std::fs::create_dir(&to)?;
                scan.directories.push((from, to));
            }
            CloneEntry::File(file) => scan.files.push(file),
        }
    }
    Ok(scan)
}

enum CloneEntry {
    Directory(PathBuf, PathBuf),
    File(CloneFile),
}

fn classify_clone_entry(from: PathBuf, to: PathBuf, mode: Mode) -> std::io::Result<CloneEntry> {
    let metadata = std::fs::symlink_metadata(&from)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Ok(CloneEntry::Directory(from, to));
    }
    let is_symlink = metadata.file_type().is_symlink();
    let read_only_mode = if mode == Mode::ReadOnly && !is_symlink {
        Some(read_only_mode_for(&metadata))
    } else {
        None
    };
    Ok(CloneEntry::File((from, to, is_symlink, read_only_mode)))
}

fn clone_file_batch(files: Vec<CloneFile>) -> std::io::Result<()> {
    review_parallel::try_for_each_owned(files, |(from, to, is_symlink, read_only_mode)| {
        if is_symlink {
            symlink_raw(&std::fs::read_link(&from)?, &to)?;
        } else {
            reflink_copy::reflink_or_copy(&from, &to)?;
            apply_one_file_permission(&to, read_only_mode)?;
        }
        Ok::<_, std::io::Error>(())
    })
}

fn apply_one_file_permission(path: &Path, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(mode) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)),
        None => Ok(()),
    }
}

fn read_only_mode_for(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o555
    } else {
        0o444
    }
}

fn apply_directories_read_only(directories: Vec<PathBuf>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for directory in directories.into_iter().rev() {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o555))?;
    }
    Ok(())
}

fn symlink_raw(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

/// A snapshot materialized once, to be cloned per sandbox.
///
/// Materialization prepares paths serially, then verifies each distinct CAS object once and
/// writes independent content groups through the process-wide executor. A template is built
/// exactly once; every sandbox is then a bounded-parallel copy-on-write clone of it, which shares
/// blocks instead of re-reading and re-writing the tree while keeping writes isolated.
pub struct SandboxTemplate {
    manifest: Arc<Manifest>,
    root: PathBuf,
    /// Present for a temporary template, removed with the handle. A Warm Workspace template
    /// lives at a stable root the handle does not own, so nothing is removed with it.
    _dir: Option<tempfile::TempDir>,
}

impl SandboxTemplate {
    pub fn materialize(manifest: &Manifest, cas: &Cas) -> Result<SandboxTemplate, std::io::Error> {
        let dir = stale::tempdir()?;
        let root = dir.path().join("tree");
        materialize(manifest, cas, &root).map_err(std::io::Error::other)?;
        Ok(SandboxTemplate {
            manifest: Arc::new(manifest.clone()),
            root,
            _dir: Some(dir),
        })
    }

    /// Where the template's tree is, read-only to every clone of it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The tree the template holds.
    pub fn manifest(&self) -> &Manifest {
        self.manifest.as_ref()
    }

    /// A template at a stable root this handle does not own. Only the workspace module builds
    /// one, after it verified that `root` holds exactly `manifest`.
    pub(crate) fn at_stable_root(manifest: Manifest, root: PathBuf) -> SandboxTemplate {
        SandboxTemplate {
            manifest: Arc::new(manifest),
            root,
            _dir: None,
        }
    }
}

impl Sandbox {
    /// Materialize a snapshot into a fresh temporary directory.
    ///
    /// Deliberately a copy, never the checkout: the strongest property this provider has is that
    /// a check writing to `../../src/main.rs` corrupts a temporary directory nobody will read
    /// again, instead of the working tree under review.
    pub fn materialize(
        manifest: &Manifest,
        cas: &Cas,
        mode: Mode,
    ) -> Result<Sandbox, std::io::Error> {
        let dir = stale::tempdir()?;
        let root = dir.path().join("tree");
        materialize(manifest, cas, &root).map_err(std::io::Error::other)?;

        let sandbox = Sandbox {
            root,
            isolation: Isolation::None,
            baseline: Arc::new(manifest.clone()),
            _dir: Some(dir),
            attached: std::sync::Mutex::new(None),
        };
        if mode == Mode::ReadOnly {
            sandbox.apply_read_only()?;
        }
        Ok(sandbox)
    }

    /// A copy-on-write clone of a materialized template — the fast path. Writes are isolated:
    /// COW gives each clone its own copy of any block it changes.
    pub fn from_template(
        template: &SandboxTemplate,
        mode: Mode,
    ) -> Result<Sandbox, std::io::Error> {
        Self::from_template_with_isolation(template, mode, Isolation::None)
    }

    /// Provider-only constructor. Isolation is not caller-settable: only code in this crate that
    /// owns and probes a provider may attach the provider's verified claim to a sandbox.
    fn from_template_with_isolation(
        template: &SandboxTemplate,
        mode: Mode,
        isolation: Isolation,
    ) -> Result<Sandbox, std::io::Error> {
        let dir = stale::tempdir()?;
        let root = dir.path().join("tree");
        let cloned_directories = clone_tree(&template.root, &root, mode)?;

        let sandbox = Sandbox {
            root,
            isolation,
            baseline: Arc::clone(&template.manifest),
            _dir: Some(dir),
            attached: std::sync::Mutex::new(None),
        };
        if mode == Mode::ReadOnly {
            apply_directories_read_only(cloned_directories)?;
        }
        Ok(sandbox)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What this sandbox genuinely enforces — readable by anyone, settable by no one.
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }

    pub fn baseline(&self) -> &Manifest {
        self.baseline.as_ref()
    }

    /// Make this check's regular source files newer than a build left in a shared Cargo target.
    /// Call after materialization and acquiring any warm target lock, before dispatch.
    /// `after` is the newest mtime observed in the held target, or absent for a cold target.
    /// Stored times must beat it without being in the future. Changes only the private tree,
    /// never the CAS or its manifest; coarse-clock retries share the caller's budget.
    pub fn refresh_source_mtimes(
        &self,
        after: Option<std::time::SystemTime>,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> std::io::Result<usize> {
        refresh_source_mtimes(&self.root, &self.baseline, after, cancellation, budget)
    }

    /// [`Self::refresh_source_mtimes`], except that a file for which `kept` names a time gets
    /// exactly that time back (ADR-0201): the stamp a warm layer's sources record holds for a
    /// group whose content it built. Every other regular file gets one current stamp, newer
    /// than `after`, which the result returns for the record.
    pub fn refresh_source_mtimes_keyed(
        &self,
        after: Option<std::time::SystemTime>,
        kept: &dyn Fn(&str) -> Option<std::time::SystemTime>,
        cancellation: Option<&AtomicBool>,
        budget: Duration,
    ) -> std::io::Result<Refreshed> {
        refresh_source_mtimes_observed(
            &self.root,
            &self.baseline,
            after,
            kept,
            cancellation,
            budget,
            &mut |_| {},
        )
    }

    fn apply_read_only(&self) -> std::io::Result<()> {
        apply_tree_read_only(&self.root)
    }

    /// Materialize `manifest` read-only as `base/`, the sibling of this sandbox's `tree/`, and
    /// write each of `beside` read-only next to both (ADR-0206). A Worker that judges a change
    /// reads the tree before it at `../base` and the change's paths in `../diff-subject.json`;
    /// neither is under the root, so neither is sealed back or counted as an edit. Only a
    /// sandbox that owns its temporary directory has a sibling to write.
    pub fn attach_base(
        &self,
        manifest: &Manifest,
        cas: &Cas,
        beside: &[(&str, &[u8])],
    ) -> std::io::Result<PathBuf> {
        self.attach(manifest, beside, None, |base| {
            materialize(manifest, cas, base).map_err(std::io::Error::other)
        })
    }

    /// [`Self::attach_base`] from a materialized `template` of the base: a copy-on-write clone
    /// of it, so a second judge of the same change does not build the base tree again. The
    /// sandbox keeps the template alive until it is sealed or dropped.
    pub fn attach_base_template(
        &self,
        template: &Arc<SandboxTemplate>,
        beside: &[(&str, &[u8])],
    ) -> std::io::Result<PathBuf> {
        self.attach(
            template.manifest.as_ref(),
            beside,
            Some(Arc::clone(template)),
            |base| clone_tree(&template.root, base, Mode::ReadOnly).map(|_| ()),
        )
    }

    fn attach(
        &self,
        manifest: &Manifest,
        beside: &[(&str, &[u8])],
        template: Option<Arc<SandboxTemplate>>,
        place: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> std::io::Result<PathBuf> {
        let dir = self
            ._dir
            .as_ref()
            .ok_or_else(|| std::io::Error::other("a sandbox at a stable root has no sibling"))?;
        if beside.iter().any(|(name, _)| {
            name.is_empty() || name.contains('/') || [".", "..", "base", "tree"].contains(name)
        }) {
            return Err(std::io::Error::other(
                "a file beside the tree is one plain name",
            ));
        }
        let mut attached = self
            .attached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if attached.is_some() {
            return Err(std::io::Error::other("the sandbox already has its base"));
        }
        let base = dir.path().join("base");
        place(&base)?;
        apply_tree_read_only(&base)?;
        for (name, bytes) in beside {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes)?;
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o444))?;
        }
        let _ = manifest;
        *attached = Some(AttachedBase {
            stats: entry_stats(&base)?,
            beside: beside
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect(),
            _template: template,
        });
        Ok(base)
    }
}

/// What [`AttachedBase`] keeps of one entry: inode, device, size, mode, and the modification
/// and change times in nanoseconds.
pub(crate) type EntryStat = (u64, u64, u64, u32, i128, i128);

/// [`EntryStat`] of every entry under `root`, `root` itself as the empty path, links not
/// followed.
pub(crate) fn entry_stats(root: &Path) -> std::io::Result<BTreeMap<PathBuf, EntryStat>> {
    use std::os::unix::fs::MetadataExt;
    let stat = |metadata: &std::fs::Metadata| -> EntryStat {
        (
            metadata.ino(),
            metadata.dev(),
            metadata.size(),
            metadata.mode(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        )
    };
    let mut stats = BTreeMap::new();
    stats.insert(PathBuf::new(), stat(&std::fs::symlink_metadata(root)?));
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            stats.insert(path, stat(&metadata));
        }
    }
    Ok(stats)
}

/// Make every file under `root` read-only, then every directory, `root` included.
fn apply_tree_read_only(root: &Path) -> std::io::Result<()> {
    {
        // Files first, then directories: a read-only directory cannot have its contents chmod'd.
        let mut level = vec![root.to_path_buf()];
        let mut seen_dirs = vec![root.to_path_buf()];
        let mut pending_files = Vec::new();
        while !level.is_empty() {
            let (scanned, ()) = review_parallel::try_join(
                || review_parallel::try_map_owned(level, scan_permission_directory),
                || apply_file_permissions(pending_files),
            )?;
            let mut next_level = Vec::new();
            let mut next_files = Vec::new();
            for scan in scanned {
                seen_dirs.extend(scan.directories.iter().cloned());
                next_level.extend(scan.directories);
                next_files.extend(scan.files);
            }
            level = next_level;
            pending_files = next_files;
        }
        apply_file_permissions(pending_files)?;
        apply_directories_read_only(seen_dirs)
    }
}

impl Sandbox {
    /// Seal the sandbox and capture what changed.
    ///
    /// Consumes the handle on purpose. The design requires a sandbox to be terminated and frozen
    /// *before* its output is captured, and a seal that could be followed by more writes would
    /// describe a state that no longer exists — the same torn-read problem the dirty capture
    /// solves, one layer up.
    pub fn seal(self) -> Result<SealedSandbox, std::io::Error> {
        seal::seal(self)
    }

    /// Release the directory without removing it, and return the tree's path for the operator.
    ///
    /// A provider calls this when a container's cleanup was not confirmed: a daemon-owned
    /// process may still hold the tree as a writable bind, and deleting under it is worse than
    /// a leftover. The directory is marked beside its tree, where no bind can see the marker,
    /// so the stale-sandbox sweep of a later `af` process keeps it too (see [`stale`]).
    pub fn preserve(mut self) -> PathBuf {
        let root = std::mem::take(&mut self.root);
        if let Some(dir) = self._dir.take() {
            let _ = stale::mark_preserved(dir.path());
            let _ = dir.keep();
        }
        // The emptied handle drops as a no-op: no tree to make writable, no `TempDir` to remove.
        root
    }

    /// The base attached beside the tree, taken out of the handle for sealing.
    pub(crate) fn take_attached(&self) -> Option<AttachedBase> {
        self.attached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub(crate) fn into_parts(mut self) -> (PathBuf, Arc<Manifest>, tempfile::TempDir) {
        // Seal restores traversal permissions as it scans. The residual `self` (emptied below)
        // then drops as a no-op.
        let dir = self._dir.take().expect("sandbox owns its dir until sealed");
        let root = std::mem::take(&mut self.root);
        let baseline = std::mem::take(&mut self.baseline);
        (root, baseline, dir)
    }
}

/// What [`Sandbox::refresh_source_mtimes_keyed`] did: how many regular files it stamped now,
/// how many it gave back a kept time, and the one current stamp it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refreshed {
    pub stamped: usize,
    pub kept: usize,
    pub stamp: std::time::SystemTime,
}

fn refresh_source_mtimes(
    root: &Path,
    manifest: &Manifest,
    after: Option<std::time::SystemTime>,
    cancellation: Option<&AtomicBool>,
    budget: Duration,
) -> std::io::Result<usize> {
    refresh_source_mtimes_observed(
        root,
        manifest,
        after,
        &|_| None,
        cancellation,
        budget,
        &mut |_| {},
    )
    .map(|refreshed| refreshed.stamped)
}

fn refresh_source_mtimes_observed(
    root: &Path,
    manifest: &Manifest,
    after: Option<std::time::SystemTime>,
    kept: &dyn Fn(&str) -> Option<std::time::SystemTime>,
    cancellation: Option<&AtomicBool>,
    budget: Duration,
    stamped: &mut dyn FnMut(&Path),
) -> std::io::Result<Refreshed> {
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode as FileMode;
    use std::fs::{File, FileTimes};
    use std::time::SystemTime;

    let started = Instant::now();
    check_refresh_budget(cancellation, started, budget)?;
    let root = open(
        root,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
        FileMode::empty(),
    )?;
    'refresh: loop {
        let modified = SystemTime::now();
        let times = FileTimes::new().set_modified(modified);
        let mut latest = modified;
        let mut touched = 0;
        let mut restored = 0;
        for entry in &manifest.entries {
            check_refresh_budget(cancellation, started, budget)?;
            if !matches!(entry.kind, EntryKind::File | EntryKind::Executable) {
                continue;
            }
            // Anchor traversal in the private root and refuse replacements of any component,
            // not just a final symlink. Keep the opened file bound while changing its metadata.
            let path = fs_path(&entry.path);
            let parent = open_beneath(&root, path.parent().unwrap_or_else(|| Path::new("")))?;
            let file = File::from(openat(
                &parent,
                path.file_name()
                    .ok_or_else(|| std::io::Error::other("source has no filename"))?,
                OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
                FileMode::empty(),
            )?);
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::other(
                    "materialized source is not a regular file",
                ));
            }
            check_refresh_budget(cancellation, started, budget)?;
            if let Some(time) = kept(&entry.path) {
                // A group whose content the layer built gets that build's stamp back: older
                // than the artifacts built from it, so Cargo keeps them, and never newer than now.
                file.set_times(FileTimes::new().set_modified(time))?;
                restored += 1;
                continue;
            }
            file.set_times(times)?;
            let stored = file.metadata()?.modified()?;
            if after.is_some_and(|after| stored <= after) {
                // A coarse filesystem may round the current time back to the target's tick.
                // Retry the whole tree with one later current stamp, never a future lead.
                pause_refresh(cancellation, started, budget)?;
                continue 'refresh;
            }
            latest = latest.max(stored);
            stamped(&path);
            touched += 1;
        }
        // Also covers a filesystem rounding up, or a wall-clock rollback during traversal.
        while SystemTime::now() < latest {
            pause_refresh(cancellation, started, budget)?;
        }
        check_refresh_budget(cancellation, started, budget)?;
        return Ok(Refreshed {
            stamped: touched,
            kept: restored,
            stamp: modified,
        });
    }
}

fn pause_refresh(
    cancellation: Option<&AtomicBool>,
    started: Instant,
    budget: Duration,
) -> std::io::Result<()> {
    check_refresh_budget(cancellation, started, budget)?;
    std::thread::sleep(Duration::from_millis(10).min(budget.saturating_sub(started.elapsed())));
    check_refresh_budget(cancellation, started, budget)
}

fn check_refresh_budget(
    cancellation: Option<&AtomicBool>,
    started: Instant,
    budget: Duration,
) -> std::io::Result<()> {
    if cancellation.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "source freshness refresh cancelled",
        ));
    }
    if started.elapsed() >= budget {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "source freshness refresh exceeded check deadline",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod source_mtime_tests {
    use super::*;
    use review_source_git::Entry;

    #[test]
    fn a_freshness_floor_ahead_of_the_clock_cannot_escape_the_budget_or_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.rs");
        std::fs::write(&path, b"source").unwrap();
        let manifest = Manifest::new(vec![Entry {
            path: "source.rs".into(),
            kind: EntryKind::File,
            content: review_source_git::digest_bytes(b"source"),
            size: 6,
        }])
        .unwrap();
        let after = std::time::SystemTime::now() + Duration::from_secs(86_400);
        let started = Instant::now();
        let error = refresh_source_mtimes(
            root.path(),
            &manifest,
            Some(after),
            None,
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                cancelled.store(true, Ordering::Release);
            });
            let error = refresh_source_mtimes(
                root.path(),
                &manifest,
                Some(after),
                Some(&cancelled),
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        });
        assert!(
            std::fs::metadata(path).unwrap().modified().unwrap() <= std::time::SystemTime::now()
        );
    }

    #[test]
    fn refreshing_replaced_source_components_refuses_symlinks_without_touching_their_targets() {
        for path in ["source.rs", "nested/source.rs"] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("tree");
            let outside = temporary.path().join("outside");
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(&outside).unwrap();
            let target = outside.join("source.rs");
            std::fs::write(&target, b"untouched").unwrap();
            let before = std::fs::metadata(&target).unwrap().modified().unwrap();
            if path.contains('/') {
                std::os::unix::fs::symlink(&outside, root.join("nested")).unwrap();
            } else {
                std::os::unix::fs::symlink(&target, root.join(path)).unwrap();
            }
            let manifest = Manifest::new(vec![Entry {
                path: path.into(),
                kind: EntryKind::File,
                content: review_source_git::digest_bytes(b"source"),
                size: 6,
            }])
            .unwrap();

            assert!(
                refresh_source_mtimes(&root, &manifest, None, None, Duration::from_secs(1))
                    .is_err()
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
            assert_eq!(
                std::fs::metadata(target).unwrap().modified().unwrap(),
                before
            );
        }
    }

    #[test]
    fn cancellation_inside_a_flat_manifest_stops_before_the_next_stamp_and_after_the_last() {
        for cancel_after in [1, 3] {
            let temporary = tempfile::tempdir().unwrap();
            let old = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
            let entries = (0..3)
                .map(|index| {
                    let path = format!("source-{index}.rs");
                    let file = std::fs::File::create(temporary.path().join(&path)).unwrap();
                    file.set_times(std::fs::FileTimes::new().set_modified(old))
                        .unwrap();
                    Entry {
                        path,
                        kind: EntryKind::File,
                        content: review_source_git::digest_bytes(b""),
                        size: 0,
                    }
                })
                .collect();
            let manifest = Manifest::new(entries).unwrap();
            let cancelled = AtomicBool::new(false);
            let mut count = 0;
            let error = refresh_source_mtimes_observed(
                temporary.path(),
                &manifest,
                None,
                &|_| None,
                Some(&cancelled),
                Duration::from_secs(60),
                &mut |_| {
                    count += 1;
                    if count == cancel_after {
                        cancelled.store(true, Ordering::Release);
                    }
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
            assert_eq!(count, cancel_after);
            for (index, entry) in manifest.entries.iter().enumerate() {
                let modified = std::fs::metadata(temporary.path().join(&entry.path))
                    .unwrap()
                    .modified()
                    .unwrap();
                assert_eq!(modified == old, index >= cancel_after);
            }
        }
    }

    #[test]
    fn refresh_refuses_cancellation_and_expired_budget_before_touching_a_file() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("source.rs");
        std::fs::write(&file, b"source").unwrap();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let manifest = Manifest::new(vec![Entry {
            path: "source.rs".into(),
            kind: EntryKind::File,
            content: review_source_git::digest_bytes(b"source"),
            size: 6,
        }])
        .unwrap();
        let cancelled = AtomicBool::new(true);
        let error = refresh_source_mtimes(
            temporary.path(),
            &manifest,
            None,
            Some(&cancelled),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        let error = refresh_source_mtimes(temporary.path(), &manifest, None, None, Duration::ZERO)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(std::fs::metadata(file).unwrap().modified().unwrap(), before);
    }
}

struct PermissionScan {
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, u32)>,
}

fn scan_permission_directory(directory: PathBuf) -> std::io::Result<PermissionScan> {
    let paths = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let classified = review_parallel::try_map_owned(paths, |path| {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            Ok::<_, std::io::Error>((Some(path), None))
        } else if !metadata.file_type().is_symlink() {
            let mode = read_only_mode_for(&metadata);
            Ok((None, Some((path, mode))))
        } else {
            Ok((None, None))
        }
    })?;
    let mut scan = PermissionScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    for (directory, file) in classified {
        scan.directories.extend(directory);
        scan.files.extend(file);
    }
    Ok(scan)
}

fn apply_file_permissions(files: Vec<(PathBuf, u32)>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    review_parallel::try_for_each_owned(files, |(path, mode)| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    })
}

#[cfg(test)]
mod directory_mode_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn directory_mode_repair_never_follows_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&target, &link).unwrap();

        ensure_directory_mode(&link, 0o1000).unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn directory_mode_repair_adds_only_the_requested_bits() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();

        ensure_directory_mode(&target, 0o500).unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o500
        );
    }
}
