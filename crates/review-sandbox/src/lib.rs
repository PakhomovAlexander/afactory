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
    baseline: Manifest,
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
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_type()
                .map(|t| t.is_dir() && !t.is_symlink())
                .unwrap_or(false)
            {
                stack.push(entry.path());
            }
        }
    }
}

#[cfg(not(unix))]
fn restore_writable_dirs(_root: &Path) {}

/// Recreate `src`'s tree at `dst`, copy-on-write cloning each regular file. Directories are
/// recreated (a clone is a fresh writable tree), symlinks are recreated as symlinks (they must
/// not be dereferenced), and regular files are reflinked — sharing blocks until one side
/// writes — with a plain copy where the filesystem does not support reflinks. Both preserve
/// the source permissions, so the exec bit survives.
fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from_dir, to_dir)) = stack.pop() {
        for entry in std::fs::read_dir(&from_dir)? {
            let entry = entry?;
            let from = entry.path();
            let to = to_dir.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                std::fs::create_dir(&to)?;
                stack.push((from, to));
            } else if file_type.is_symlink() {
                symlink_raw(&std::fs::read_link(&from)?, &to)?;
            } else {
                // COW clone, or a plain copy where reflinks are unavailable — either way the
                // content and permissions are those of the template.
                reflink_copy::reflink_or_copy(&from, &to)?;
            }
        }
    }
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
/// Materializing walks the manifest and, per entry, does a CAS read (open + full SHA-256
/// verification) + write + chmod — syscall-bound, and paid once for the gate and once per
/// reviewer attempt when each sandbox re-materialized from scratch. A template materializes
/// exactly once; every sandbox is then a copy-on-write clone of it, which shares blocks
/// instead of re-reading and re-writing the tree, with writes still fully isolated per clone.
pub struct SandboxTemplate {
    manifest: Manifest,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

impl SandboxTemplate {
    pub fn materialize(manifest: &Manifest, cas: &Cas) -> Result<SandboxTemplate, std::io::Error> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("tree");
        materialize(manifest, cas, &root).map_err(std::io::Error::other)?;
        Ok(SandboxTemplate {
            manifest: manifest.clone(),
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
            baseline: manifest.clone(),
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
        clone_tree(&template.root, &root)?;

        let sandbox = Sandbox {
            root,
            mode,
            isolation: Isolation::None,
            baseline: template.manifest.clone(),
            _dir: Some(dir),
        };
        if mode == Mode::ReadOnly {
            sandbox.apply_read_only()?;
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
        &self.baseline
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
        use std::os::unix::fs::PermissionsExt;
        // Files first, then directories: a read-only directory cannot have its contents chmod'd.
        let mut dirs = vec![self.root.clone()];
        let mut seen_dirs = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                let meta = std::fs::symlink_metadata(&path)?;
                if meta.is_dir() {
                    dirs.push(path);
                } else if !meta.file_type().is_symlink() {
                    // Strip write, keep execute. Flattening to 0o444 was a real bug the hub's
                    // own tree caught on the first live run: every script lost its exec bit,
                    // so seal reported the whole executable population as mutated and the
                    // gate's verify check saw a tree full of non-executable hooks.
                    use std::os::unix::fs::PermissionsExt;
                    let executable = meta.permissions().mode() & 0o111 != 0;
                    let mode = if executable { 0o555 } else { 0o444 };
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
                }
            }
            seen_dirs.push(dir);
        }
        for dir in seen_dirs.into_iter().rev() {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))?;
        }
        Ok(())
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

    pub(crate) fn into_parts(mut self) -> (PathBuf, Manifest, Mode, tempfile::TempDir) {
        // Restore writability here, while the real root is still known: the TempDir moves to the
        // SealedSandbox, so its later cleanup must find directories it can empty. The residual
        // `self` (emptied below) then drops as a no-op.
        restore_writable_dirs(&self.root);
        let dir = self._dir.take().expect("sandbox owns its dir until sealed");
        let root = std::mem::take(&mut self.root);
        let baseline = std::mem::take(&mut self.baseline);
        (root, baseline, self.mode, dir)
    }
}
