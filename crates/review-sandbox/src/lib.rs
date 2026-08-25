//! Sandboxes: where a reviewer or check may run, and what that costs.
//!
//! # The honest part first
//!
//! The provider implemented here is `trusted_local`: a materialized copy of a snapshot in a
//! temporary directory, with an optional read-only mode. **It is not security isolation.** A
//! process running as the same user can `chmod` its way out of read-only mode, read anything the
//! user can read, and open any socket. It buys three real things — the canonical checkout is not
//! reachable, the environment is rebuilt from an allowlist, and every mutation is captured — and
//! it buys nothing else.
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
//! So `fixtures/adversarial/malicious-check.md` is only **partly** discharged here. Its probes
//! for the canonical checkout, inherited credentials and argument injection are covered. Its
//! probes for a host marker outside the sandbox and for undeclared network are *not*, and cannot
//! be by a provider of this kind. They close only when the container provider runs against a
//! live daemon, and the case says so rather than being quietly narrowed to what passes.

pub mod container;
pub mod seal;

pub use self::SandboxTemplate as Template;
pub use container::{Availability, ContainerProvider};
pub use seal::{MutationSet, SealedSandbox};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use review_source_git::{Manifest, materialize};
use review_store::Cas;

/// What a provider genuinely enforces. Ordered: a stronger level satisfies a weaker requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Isolation {
    /// A directory. Filesystem conventions only — no boundary a determined process respects.
    None,
    /// A separate process tree with a rebuilt environment and no inherited descriptors.
    Process,
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
    mode: Mode,
    isolation: Isolation,
    /// The manifest as materialized. Sealing diffs against this, so "what did the reviewer
    /// change" is computed rather than reported by the reviewer.
    baseline: Arc<Manifest>,
    /// Kept so the directory outlives the handle and is removed with it. An `Option` only so
    /// [`Sandbox::into_parts`] can move it out while the `Drop` below still runs.
    _dir: Option<tempfile::TempDir>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A read-only sandbox left its directories at 0o555, and unlinking an entry needs
        // write on its parent — so `TempDir`'s own cleanup would fail silently and strand a
        // whole materialized tree in TMPDIR. Restore writability first, then let the TempDir
        // (dropped after this body) remove the tree.
        restore_writable_dirs(&self.root);
    }
}

/// Make every directory under `root` writable by its owner again, so a subsequent
/// `remove_dir_all` can unlink what is inside them. Best-effort: a failure here only means the
/// TempDir cleanup that follows will do no worse than before.
#[cfg(unix)]
fn restore_writable_dirs(root: &Path) {
    let mut level = vec![root.to_path_buf()];
    while !level.is_empty() {
        let children = review_parallel::try_map_owned(level, |dir| {
            let _ = ensure_directory_mode(&dir, 0o700);
            let mut children = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry
                        .file_type()
                        .map(|kind| kind.is_dir() && !kind.is_symlink())
                        .unwrap_or(false)
                    {
                        children.push(entry.path());
                    }
                }
            }
            Ok::<_, ()>(children)
        })
        .expect("best-effort writable-directory walk is infallible");
        level = children.into_iter().flatten().collect();
    }
}

#[cfg(not(unix))]
fn restore_writable_dirs(_root: &Path) {}

#[cfg(unix)]
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

#[cfg(not(unix))]
pub(crate) fn ensure_directory_mode(_path: &Path, _required: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
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
    let mut scan = CloneScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    for entry in std::fs::read_dir(&from_dir)? {
        let entry = entry?;
        let from = entry.path();
        let to = to_dir.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            std::fs::create_dir(&to)?;
            scan.directories.push((from, to));
        } else {
            let is_symlink = file_type.is_symlink();
            let read_only_mode = if mode == Mode::ReadOnly && !is_symlink {
                Some(read_only_mode_for(&entry.metadata()?))
            } else {
                None
            };
            scan.files.push((from, to, is_symlink, read_only_mode));
        }
    }
    Ok(scan)
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

#[cfg(unix)]
fn apply_one_file_permission(path: &Path, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(mode) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn apply_one_file_permission(_path: &Path, _mode: Option<u32>) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn read_only_mode_for(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o555
    } else {
        0o444
    }
}

#[cfg(not(unix))]
fn read_only_mode_for(_metadata: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn apply_directories_read_only(directories: Vec<PathBuf>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for directory in directories.into_iter().rev() {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o555))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_directories_read_only(_directories: Vec<PathBuf>) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink_raw(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(not(unix))]
fn symlink_raw(target: &Path, at: &Path) -> std::io::Result<()> {
    std::fs::write(at, target.to_string_lossy().as_bytes())
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
    _dir: tempfile::TempDir,
}

impl SandboxTemplate {
    pub fn materialize(manifest: &Manifest, cas: &Cas) -> Result<SandboxTemplate, std::io::Error> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("tree");
        materialize(manifest, cas, &root).map_err(std::io::Error::other)?;
        Ok(SandboxTemplate {
            manifest: Arc::new(manifest.clone()),
            root,
            _dir: dir,
        })
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
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("tree");
        materialize(manifest, cas, &root).map_err(std::io::Error::other)?;

        let sandbox = Sandbox {
            root,
            mode,
            isolation: Isolation::None,
            baseline: Arc::new(manifest.clone()),
            _dir: Some(dir),
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
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("tree");
        let cloned_directories = clone_tree(&template.root, &root, mode)?;

        let sandbox = Sandbox {
            root,
            mode,
            isolation: Isolation::None,
            baseline: Arc::clone(&template.manifest),
            _dir: Some(dir),
        };
        if mode == Mode::ReadOnly {
            apply_directories_read_only(cloned_directories)?;
        }
        Ok(sandbox)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// What this sandbox genuinely enforces — readable by anyone, settable by no one.
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }

    pub fn baseline(&self) -> &Manifest {
        self.baseline.as_ref()
    }

    /// The environment a node runs with: rebuilt from an allowlist, never inherited.
    ///
    /// This is the credential probe from the malicious-check case. It holds because the
    /// environment is *cleared* — a token in the kernel's own environment cannot leak into a
    /// check by being forgotten in a denylist.
    pub fn environment(&self) -> Vec<(&'static str, String)> {
        vec![
            ("PATH", std::env::var("PATH").unwrap_or_default()),
            ("HOME", self.root.to_string_lossy().into_owned()),
            ("LC_ALL", "C".to_string()),
            ("TZ", "UTC".to_string()),
        ]
    }

    #[cfg(unix)]
    fn apply_read_only(&self) -> std::io::Result<()> {
        // Files first, then directories: a read-only directory cannot have its contents chmod'd.
        let mut level = vec![self.root.clone()];
        let mut seen_dirs = vec![self.root.clone()];
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

    #[cfg(not(unix))]
    fn apply_read_only(&self) -> std::io::Result<()> {
        Ok(())
    }

    /// Seal the sandbox and capture what changed.
    ///
    /// Consumes the handle on purpose. The design requires a sandbox to be terminated and frozen
    /// *before* its output is captured, and a seal that could be followed by more writes would
    /// describe a state that no longer exists — the same torn-read problem the dirty capture
    /// solves, one layer up.
    pub fn seal(self) -> Result<SealedSandbox, std::io::Error> {
        seal::seal(self)
    }

    pub(crate) fn into_parts(mut self) -> (PathBuf, Arc<Manifest>, Mode, tempfile::TempDir) {
        // Seal restores traversal permissions as it scans. The residual `self` (emptied below)
        // then drops as a no-op.
        let dir = self._dir.take().expect("sandbox owns its dir until sealed");
        let root = std::mem::take(&mut self.root);
        let baseline = std::mem::take(&mut self.baseline);
        (root, baseline, self.mode, dir)
    }
}

struct PermissionScan {
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, u32)>,
}

fn scan_permission_directory(directory: PathBuf) -> std::io::Result<PermissionScan> {
    let mut scan = PermissionScan {
        directories: Vec::new(),
        files: Vec::new(),
    };
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            scan.directories.push(path);
        } else if !metadata.file_type().is_symlink() {
            scan.files.push((path, read_only_mode_for(&metadata)));
        }
    }
    Ok(scan)
}

#[cfg(unix)]
fn apply_file_permissions(files: Vec<(PathBuf, u32)>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    review_parallel::try_for_each_owned(files, |(path, mode)| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    })
}
