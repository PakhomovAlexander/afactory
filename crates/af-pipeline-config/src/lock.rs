//! Release pins and bounded, digest-verified current Task package reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum LockError {
    Parse(String),
    InvalidName {
        name: String,
    },
    /// A version that does not name exactly one release.
    Floating {
        name: String,
        version: String,
    },
    MalformedDigest {
        name: String,
        digest: String,
    },
    /// A symlink can point outside the package, so its target would be content the digest
    /// silently depends on. Refused rather than followed or skipped.
    Symlink {
        name: String,
        path: PathBuf,
    },
    UnsupportedFileType {
        name: String,
        path: PathBuf,
    },
    UnsupportedPath {
        name: String,
        path: PathBuf,
    },
    /// The package holds more files or bytes, or one file of it more bytes, than the walk was
    /// bounded to ([`PackageBounds`]); refused before the excess is read.
    TooLarge {
        name: String,
        root: PathBuf,
        /// The package-relative file whose turn crossed a bound.
        at: String,
        files: usize,
        bytes: usize,
        file_bytes: usize,
    },
    Io {
        path: PathBuf,
        error: String,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Parse(e) => write!(f, "lockfile: {e}"),
            LockError::InvalidName { name } => {
                write!(f, "package name `{name}` is not a safe registry component")
            }
            LockError::Floating { name, version } => write!(
                f,
                "package `{name}` pins version `{version}`, which does not name exactly one \
                 release; a pin must be `major.minor.patch` — a run never resolves `latest`"
            ),
            LockError::MalformedDigest { name, digest } => write!(
                f,
                "package `{name}` pins digest `{digest}`, which is not `sha256:` plus 64 hex \
                 digits; a malformed digest pins nothing"
            ),
            LockError::Symlink { name, path } => write!(
                f,
                "package `{name}` contains a symlink at {}; a package is regular files only",
                path.display()
            ),
            LockError::UnsupportedFileType { name, path } => write!(
                f,
                "package `{name}` contains a non-regular file at {}; a package is regular files only",
                path.display()
            ),
            LockError::UnsupportedPath { name, path } => write!(
                f,
                "package `{name}` contains a path that is not valid UTF-8 at {}; refusing a lossy digest name",
                path.display()
            ),
            LockError::TooLarge {
                name,
                root,
                at,
                files,
                bytes,
                file_bytes,
            } => write!(
                f,
                "package `{name}` at {} is refused at `{at}`: a package is at most {files} files \
                 and {bytes} bytes, a package file is at most {file_bytes} bytes",
                root.display()
            ),
            LockError::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}

/// Retained lock metadata beside the independently byte-bound release pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub version: String,
    pub digest: String,
}

/// The `af` release a lock pins, and the bytes that release has: one archive digest per target,
/// copied from the release's signed `SHA256SUMS` when the lock was written.
///
/// The version alone would pin a *name*; the digests pin the *bytes*. Dispatch installs a pinned
/// release on demand only when the archive it downloads matches the digest recorded here for the
/// running machine's target, so a re-uploaded or substituted asset never runs under a lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AfPin {
    pub version: String,
    /// `<target triple>` → `sha256:<hex>` of `af-v<version>-<target>.tar.gz`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub digests: BTreeMap<String, String>,
}

impl AfPin {
    pub fn version_only(version: &str) -> AfPin {
        AfPin {
            version: version.to_string(),
            digests: BTreeMap::new(),
        }
    }

    pub fn digest_for(&self, target: &str) -> Option<&str> {
        self.digests.get(target).map(String::as_str)
    }
}

/// The lockfile itself — `.af/af.lock`, TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    /// The `af` release that wrote this lock (`af onboard --apply` or `--refresh-lock`) and its
    /// per-target archive digests. A newer `af` proceeds and notes the
    /// difference; an older `af` refuses, because it cannot know what the newer release meant by
    /// these pins; inside the project, any `af` on `PATH` dispatches to the pinned release.
    /// Absent on locks written by a build that has no install receipt (a source build pins
    /// nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub af: Option<AfPin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workers: BTreeMap<String, Pin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pipelines: BTreeMap<String, Pin>,
}

/// Exactly one release: `major.minor.patch`, digits only.
fn exact_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn well_formed_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn validate_pin(name: &str, pin: &Pin) -> Result<(), LockError> {
    if !exact_version(&pin.version) {
        return Err(LockError::Floating {
            name: name.to_string(),
            version: pin.version.clone(),
        });
    }
    if !well_formed_digest(&pin.digest) {
        return Err(LockError::MalformedDigest {
            name: name.to_string(),
            digest: pin.digest.clone(),
        });
    }
    Ok(())
}

/// The most bytes af reads from one authority file: a policy, a lock, one file of a Worker
/// package.
pub const MAX_AUTHORITY_FILE_BYTES: u64 = 1024 * 1024;
/// The most bytes one Worker package on disk may hold in all.
pub const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024;
/// The most files one Worker package on disk may hold.
pub const MAX_PACKAGE_FILES: usize = 256;

/// Whether the package root itself may be reached through a symlink. [`package_digest`] follows
/// one; a directory an author names must be the package itself; a registry reads exactly the
/// directory, by device and inode, that it located.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootLink {
    Follow,
    Refuse,
    Located((u64, u64)),
}

/// The most regular files and bytes, in total and in one file, one walk may read. Each is
/// enforced before the bytes that would exceed it are read, never after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageBounds {
    pub files: usize,
    pub bytes: usize,
    /// The most bytes of any one file.
    pub file_bytes: usize,
}

/// The bounds of a current Worker package af reads as authority: its files and its
/// digest ([`package_digest`]).
pub const AUTHORITY_PACKAGE_BOUNDS: PackageBounds = PackageBounds {
    files: MAX_PACKAGE_FILES,
    bytes: MAX_PACKAGE_BYTES as usize,
    file_bytes: MAX_AUTHORITY_FILE_BYTES as usize,
};

/// Read every regular file in the package into memory, keyed by `/`-separated relative path.
///
/// One read serves both the digest and the manifest parse, so what was verified is what is
/// interpreted — a second read from disk could race with a writer. Every file is read without
/// following a link and within [`MAX_AUTHORITY_FILE_BYTES`]; the package within
/// [`MAX_PACKAGE_FILES`] files and [`MAX_PACKAGE_BYTES`] bytes.
fn collect(name: &str, root: &Path) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    collect_no_follow(
        name,
        root,
        RootLink::Follow,
        Some(AUTHORITY_PACKAGE_BOUNDS),
        &mut |_| {},
    )
}

/// The walk behind [`collect`]. Every directory and file below the root is opened relative to
/// its parent's descriptor with `O_NOFOLLOW`, and the opened object must be the very one that
/// was classified, so an entry swapped for a symlink (or anything else) between the check and
/// the open is refused instead of read: nothing outside the root is ever reached. `before_open`
/// runs between the two for every entry and before the root is opened; only the race regressions
/// give it a body. `bounds`, when given, is checked against each file's size before it is read,
/// and no read takes more than what is left of it, so a file that grows meanwhile is refused too.
fn collect_no_follow(
    name: &str,
    root: &Path,
    root_link: RootLink,
    bounds: Option<PackageBounds>,
    before_open: &mut dyn FnMut(&Path),
) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    use nix::errno::Errno;
    use nix::fcntl::{AtFlags, OFlag, open, openat};
    use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat};
    use std::ffi::OsString;
    use std::io::Read;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::rc::Rc;

    let io = |path: &Path, error: std::io::Error| LockError::Io {
        path: path.to_path_buf(),
        error: error.to_string(),
    };
    // A no-follow open of a symlink fails with `ELOOP`, or with `ENOTDIR` when a directory was
    // asked for; `is_symlink` tells the latter apart from an entry that is simply no directory.
    let opened = |path: &Path, error: Errno, is_symlink: &dyn Fn() -> bool| match error {
        Errno::ELOOP => LockError::Symlink {
            name: name.to_string(),
            path: path.to_path_buf(),
        },
        Errno::ENOTDIR if is_symlink() => LockError::Symlink {
            name: name.to_string(),
            path: path.to_path_buf(),
        },
        other => io(path, other.into()),
    };
    let kind = |stat: &FileStat| SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
    let same = |a: &FileStat, b: &FileStat| a.st_dev == b.st_dev && a.st_ino == b.st_ino;
    let replaced = |path: &Path| LockError::Io {
        path: path.to_path_buf(),
        error: "was replaced while the package was read".into(),
    };
    let directory_flags =
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    // `O_NONBLOCK` keeps a FIFO swapped in for a file from hanging the open.
    let file_flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;

    let too_large = |bounds: PackageBounds, at: &str| LockError::TooLarge {
        name: name.to_string(),
        root: root.to_path_buf(),
        at: at.to_string(),
        files: bounds.files,
        bytes: bounds.bytes,
        file_bytes: bounds.file_bytes,
    };

    before_open(root);
    // A root that must not be a symlink is opened as its last component, relative to its parent,
    // never through the spelling given: `O_NOFOLLOW` binds only the final component, and in
    // `link/` or `link/.` that is no longer the link. Trailing separators and `.` components are
    // dropped lexically first; a root ending in `..` or naming `/` or `.` has no component that
    // could be a symlink and is opened as given.
    let lexical = root_final_component(root);
    let as_given = |flags: OFlag| {
        open(root, flags, Mode::empty()).map_err(|e| {
            opened(root, e, &|| {
                std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink())
            })
        })
    };
    let root_fd = match (root_link, lexical) {
        (RootLink::Follow, _) => as_given(directory_flags & !OFlag::O_NOFOLLOW)?,
        (RootLink::Refuse | RootLink::Located(_), None) => as_given(directory_flags)?,
        (RootLink::Refuse | RootLink::Located(_), Some((parent, last))) => {
            let parent = open(
                &parent,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io(root, e.into()))?;
            openat(
                parent.as_fd(),
                last.as_os_str(),
                directory_flags,
                Mode::empty(),
            )
            .map_err(|e| {
                opened(root, e, &|| {
                    fstatat(
                        parent.as_fd(),
                        last.as_os_str(),
                        AtFlags::AT_SYMLINK_NOFOLLOW,
                    )
                    .is_ok_and(|stat| kind(&stat) == SFlag::S_IFLNK)
                })
            })?
        }
    };
    if let RootLink::Located(located) = root_link {
        use std::os::unix::fs::MetadataExt;
        let opened = std::fs::File::from(root_fd.try_clone().map_err(|e| io(root, e))?)
            .metadata()
            .map_err(|e| io(root, e))?;
        if (opened.dev(), opened.ino()) != located {
            return Err(replaced(root));
        }
    }
    let mut files = BTreeMap::new();
    let mut total: usize = 0;
    // A directory is opened only when its turn comes, relative to its parent's descriptor,
    // which the pending entry keeps open with the identity the directory was classified with.
    // The root has no parent: it is open already.
    type Parent = Option<(Rc<OwnedFd>, FileStat)>;
    let mut pending: Vec<(Parent, Vec<OsString>)> = vec![(None, Vec::new())];
    let mut root_fd = Some(root_fd);

    while let Some((parent, relative)) = pending.pop() {
        let at = relative.iter().fold(root.to_path_buf(), |at, c| at.join(c));
        let directory = match parent {
            None => root_fd.take().expect("the root is walked once"),
            Some((parent, checked)) => {
                let entry = relative.last().expect("a child has a name");
                let fd = openat(
                    parent.as_fd(),
                    entry.as_os_str(),
                    directory_flags,
                    Mode::empty(),
                )
                .map_err(|e| {
                    opened(&at, e, &|| {
                        fstatat(
                            parent.as_fd(),
                            entry.as_os_str(),
                            AtFlags::AT_SYMLINK_NOFOLLOW,
                        )
                        .is_ok_and(|stat| kind(&stat) == SFlag::S_IFLNK)
                    })
                })?;
                let stat = fstat(&fd).map_err(|e| io(&at, e.into()))?;
                if !same(&stat, &checked) {
                    return Err(replaced(&at));
                }
                fd
            }
        };
        let directory = Rc::new(directory);
        let listing = directory.try_clone().map_err(|e| io(&at, e))?;
        let mut listing = nix::dir::Dir::from_fd(listing).map_err(|e| io(&at, e.into()))?;
        let mut names = Vec::new();
        for entry in listing.iter() {
            let entry = entry.map_err(|e| io(&at, e.into()))?;
            let bytes = entry.file_name().to_bytes();
            if bytes != b"." && bytes != b".." {
                names.push(OsString::from(std::ffi::OsStr::from_bytes(bytes)));
            }
        }
        drop(listing);

        for entry in names {
            let path = at.join(&entry);
            let checked = fstatat(
                directory.as_fd(),
                entry.as_os_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|e| io(&path, e.into()))?;
            let mut child = relative.clone();
            child.push(entry.clone());
            match kind(&checked) {
                SFlag::S_IFLNK => {
                    return Err(LockError::Symlink {
                        name: name.to_string(),
                        path,
                    });
                }
                SFlag::S_IFDIR => {
                    before_open(&path);
                    pending.push((Some((Rc::clone(&directory), checked)), child));
                }
                SFlag::S_IFREG => {
                    let mut components = Vec::with_capacity(child.len());
                    for component in &child {
                        components.push(component.to_str().ok_or_else(|| {
                            LockError::UnsupportedPath {
                                name: name.to_string(),
                                path: path.clone(),
                            }
                        })?);
                    }
                    let relative = components.join("/");
                    if let Some(bounds) = bounds
                        && files.len() >= bounds.files
                    {
                        return Err(too_large(bounds, &relative));
                    }
                    before_open(&path);
                    let fd = openat(
                        directory.as_fd(),
                        entry.as_os_str(),
                        file_flags,
                        Mode::empty(),
                    )
                    .map_err(|e| opened(&path, e, &|| false))?;
                    let stat = fstat(&fd).map_err(|e| io(&path, e.into()))?;
                    if kind(&stat) != SFlag::S_IFREG {
                        return Err(LockError::UnsupportedFileType {
                            name: name.to_string(),
                            path,
                        });
                    }
                    if !same(&stat, &checked) {
                        return Err(replaced(&path));
                    }
                    let mut bytes = Vec::new();
                    let file = std::fs::File::from(fd);
                    match bounds {
                        None => {
                            (&file).read_to_end(&mut bytes).map_err(|e| io(&path, e))?;
                        }
                        Some(bounds) => {
                            let left = (bounds.bytes - total).min(bounds.file_bytes);
                            // The size is checked before anything is read, and the read takes at
                            // most one byte past what is left, enough to tell growth from fit.
                            if usize::try_from(stat.st_size).map_or(true, |size| size > left) {
                                return Err(too_large(bounds, &relative));
                            }
                            let limit = u64::try_from(left).unwrap_or(u64::MAX).saturating_add(1);
                            (&file)
                                .take(limit)
                                .read_to_end(&mut bytes)
                                .map_err(|e| io(&path, e))?;
                            if bytes.len() > left {
                                return Err(too_large(bounds, &relative));
                            }
                            total += bytes.len();
                        }
                    }
                    files.insert(relative, bytes);
                }
                _ => {
                    return Err(LockError::UnsupportedFileType {
                        name: name.to_string(),
                        path,
                    });
                }
            }
        }
    }
    Ok(files)
}

/// The directory holding `root` and the name of `root` in it, once trailing separators and `.`
/// components are dropped; `None` when what is left ends in `..` or is `/` or `.`, none of which
/// can be a symlink. Nothing is resolved: `link/.` yields `link` itself, never its target.
fn root_final_component(root: &Path) -> Option<(PathBuf, std::ffi::OsString)> {
    let lexical: PathBuf = root.components().collect();
    let last = lexical.file_name()?.to_os_string();
    let parent = match lexical.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Some((parent, last))
}

pub fn package_digest_from_files(files: &BTreeMap<String, Vec<u8>>) -> String {
    let listed: BTreeMap<&str, String> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), af_store::canonical::blob_content_id(bytes)))
        .collect();
    // Stable package digest domain, shared by current Task catalogs; unrelated to a manifest reader.
    let payload = serde_json::json!({ "kind": "reviewer-package@1", "files": listed });
    af_store::canonical::content_id(&payload).expect("digest map is admissible JSON")
}

/// The content digest of a package on disk — what a pin records.
pub fn package_digest(name: &str, root: &Path) -> Result<String, LockError> {
    Ok(package_digest_from_files(&collect(name, root)?))
}

/// Every regular file of the package directory `root`, keyed as [`package_digest_from_files`]
/// hashes them: the same walk [`package_digest`] makes, except that `root` itself must not be a
/// symlink and the walk stops at `bounds`. A symlink, a non-regular file, a path that is not
/// UTF-8, an entry replaced while it is read or a package past `bounds` is refused, never
/// skipped, followed or truncated.
pub fn package_files(
    name: &str,
    root: &Path,
    bounds: PackageBounds,
) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    collect_no_follow(name, root, RootLink::Refuse, Some(bounds), &mut |_| {})
}

/// [`package_files`] for a root the caller already checked: the directory opened must still be
/// the one with `located`'s device and inode, so a root replaced after the check, by a symlink or
/// by another directory, is refused rather than read. `before_open` runs as in
/// [`collect_no_follow`]; only the race regressions give it a body.
pub(crate) fn located_package_files(
    name: &str,
    root: &Path,
    located: (u64, u64),
    bounds: PackageBounds,
    before_open: &mut dyn FnMut(&Path),
) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    collect_no_follow(
        name,
        root,
        RootLink::Located(located),
        Some(bounds),
        before_open,
    )
}

/// Only the `af` pin, read leniently: dispatch must find the pin in any lock a *newer* release
/// wrote, so it ignores every field it does not know instead of refusing the whole file.
pub fn pinned_af(text: &str) -> Option<AfPin> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let af = value.get("af")?.as_table()?;
    let version = af.get("version")?.as_str()?.to_string();
    let digests = af
        .get("digests")
        .and_then(toml::Value::as_table)
        .map(|digests| {
            digests
                .iter()
                .filter_map(|(target, digest)| {
                    digest
                        .as_str()
                        .map(|digest| (target.clone(), digest.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(AfPin { version, digests })
}

fn validate_af_pin(pin: &AfPin) -> Result<(), LockError> {
    if semver::Version::parse(&pin.version).is_err() {
        return Err(LockError::Parse(format!(
            "af pin `{}` is not a semantic version",
            pin.version
        )));
    }
    for (target, digest) in &pin.digests {
        if target.is_empty()
            || !target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(LockError::Parse(format!(
                "af pin digest target `{target}` is not a target triple"
            )));
        }
        if !well_formed_digest(digest) {
            return Err(LockError::MalformedDigest {
                name: format!("af ({target})"),
                digest: digest.clone(),
            });
        }
    }
    Ok(())
}

impl Lockfile {
    pub fn empty() -> Lockfile {
        Lockfile {
            version: 1,
            af: None,
            workers: BTreeMap::new(),
            pipelines: BTreeMap::new(),
        }
    }

    /// The pinned `af` version, from the lock's `[af]` table.
    pub fn af_version(&self) -> Option<&str> {
        self.af.as_ref().map(|pin| pin.version.as_str())
    }

    /// Parse and validate. A floating version or malformed digest is refused *here*, so a bad
    /// pin cannot sit latent in a file that parses.
    pub fn from_toml(text: &str) -> Result<Lockfile, LockError> {
        let lockfile: Lockfile = toml::from_str(text)
            .map_err(|e| LockError::Parse(crate::excerpt::bounded(&e.to_string())))?;
        if lockfile.version != 1 {
            return Err(LockError::Parse(format!(
                "unsupported lockfile version {}; this kernel understands version 1",
                lockfile.version
            )));
        }
        if let Some(pin) = &lockfile.af {
            validate_af_pin(pin)?;
        }
        for pins in [&lockfile.workers, &lockfile.pipelines] {
            for (name, pin) in pins {
                validate_pin(name, pin)?;
            }
        }
        Ok(lockfile)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("a lockfile serializes")
    }
}

#[cfg(test)]
mod tests;
