//! `af project workspace remove --delete-worktree`, bound to what it inspected (ADR-0158 clause 5).
//!
//! Names prove nothing; a record does. Before a removal moves or deletes anything it writes a
//! machine-local removal record beside `workspaces.toml`, `removals/<workspace id>.toml` in the
//! Project Store: the registered checkout path, the identity (device, inode and birth time) of
//! the checkout and of its administrative entry as inspected, the container it creates for each
//! with that container's identity, and the step each reached. The birth time tells a directory
//! from a later one at a reused inode (ext4 hands one out at once); a file system that reports
//! none refuses the removal at inspection, nothing changed. The record is rewritten at every step,
//! so a rerun finishes an interrupted removal from it and acts only on directories whose identity
//! it holds. Once every directory is done the record stays, complete, until the Workspace's
//! `WorkspaceRemoved@1` is appended and its paths dropped; only then is it deleted
//! (`ProjectStore::settle_removal`), so no crash leaves a registered Workspace whose removal holds
//! no identities, and a replacement at its path is never inspected afresh. A directory that is
//! merely named like a container is somebody else's: reported and left alone.
//!
//! Each directory is claimed before it is judged or deleted: this removal creates a container
//! beside it exclusively (`mkdir`, mode 0700, a fresh random name), records the container's
//! identity, opens it without following a link, and only when `fstat` of that descriptor equals
//! the recorded identity renames the directory into it relative to that descriptor, never over a
//! name that is taken. The directory is then found there by its identity; the checkout is judged
//! there, its index through delivery's cached-index comparison and its files by a
//! descriptor-based, no-follow traversal against the committed capture; and only the claimed
//! directory is deleted, through descriptors, before the container goes with `rmdir`. A
//! container holding anything else keeps it and is reported with the first such path; a
//! replacement at the registered path is never touched and is reported.
//!
//! A recorded step proves nothing by itself: a rerun re-establishes its precondition from the
//! file system and the record together before it acts. Before a live directory (the checkout
//! before its deletion is recorded done, an entry before its deletion is recorded) is claimed or
//! deleted, the rerun checks the administrative entry now (`Removal::entry_proves`): it is opened
//! without following a link and must be the directory whose identity the record holds, or it is a
//! replacement and is never read; only then must its `gitdir` still name the checkout and may it
//! hold no `locked`, or nothing of that directory is touched. An entry whose deletion is recorded
//! (`deleting`) is found by its identity in its container and its deletion resumes without its
//! `gitdir`, which that deletion may have removed; one found in neither its container nor its
//! path has been deleted, and with one recorded `deleted` it advances to done once its recorded
//! locations are verified (`claim_directory`, `remove_container`). A failed check never
//! deletes the record (`Stop::Precondition`): the next run honours the identities it holds
//! instead of inspecting afresh. A directory that is neither in its container nor gone is found
//! by its identity at its path and claimed again from inspection, and an administrative entry is
//! claimed or deleted only while what it holds now still makes it this checkout's
//! (`entry_unproven`).

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use review_sandbox::removal::{Identity, open_anchor};
use review_source_git::{Capture, EntryKind, Manifest, Snapshot};
use review_store::Cas;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};

use super::Git;

/// The version of a removal record. Version 2 is the first whose identities carry a birth time;
/// version 1 is refused (ADR-0145).
const VERSION: u32 = 2;

/// The infix of a container's name: `<name>.af-removing-<pid>-<16 random hex digits>`. Only
/// people read it; a rerun knows a container by the record, never by its name.
pub(super) const REMOVING: &str = ".af-removing-";

/// How many fresh container names a claim tries before it gives up.
const CONTAINER_TRIES: usize = 16;

/// How deep a judged or deleted tree may nest.
const MAX_DEPTH: usize = 128;

/// A debug build's fixture may stop a removal right after it reached a step, as a crash would.
/// Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_STOP: &str = "AF_TEST_PROJECT_REMOVAL_STOP";

/// A debug build's fixture may name a file to which a removal appends, one line each, the entry
/// check and every action it takes on a directory, in order. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_TRACE: &str = "AF_TEST_PROJECT_REMOVAL_TRACE";

/// How far the removal of one directory got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Step {
    /// Inspected: nothing of it moved, and it has no container.
    Inspected,
    /// Its container is named; it may have been made, but its identity is not recorded.
    Container,
    /// Its container is made and its identity recorded; the directory may be inside it.
    Contained,
    /// It is in its container and judged; its deletion may have begun.
    Deleting,
    /// It is deleted; its container may remain.
    Deleted,
    /// Its container is gone too.
    Done,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Inspected => "inspected",
            Step::Container => "container",
            Step::Contained => "contained",
            Step::Deleting => "deleting",
            Step::Deleted => "deleted",
            Step::Done => "done",
        }
    }
}

/// One directory a removal claims and deletes: the checkout or an administrative entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Claim {
    /// Where it was inspected.
    pub(super) path: PathBuf,
    /// `<device>:<inode>:<birth time in nanoseconds>` as inspected, decimal.
    pub(super) identity: String,
    pub(super) step: Step,
    /// The container this removal named for it, beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) container: Option<PathBuf>,
    /// `<device>:<inode>:<birth time in nanoseconds>` of that container when it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) container_identity: Option<String>,
}

/// The removal record of one Workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u32,
    pub(super) workspace_id: String,
    /// The registered checkout.
    pub(super) path: PathBuf,
    /// The checkout, absent when it was already gone at inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkout: Option<Claim>,
    /// Its administrative entries: the one its `.git` names, or, once the checkout was gone, each
    /// whose `gitdir` named it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) entries: Vec<Claim>,
}

/// The record's spelling of a born identity, `<device>:<inode>:<birth time in nanoseconds>`. A
/// removal records only an identity that carries its birth time ([`born_at`]).
pub(super) fn encode(identity: Identity) -> String {
    format!(
        "{}:{}:{}",
        identity.device,
        identity.inode,
        identity.born.unwrap_or(0)
    )
}

pub(super) fn decode(text: &str) -> Option<Identity> {
    let mut parts = text.split(':');
    let (device, inode, born) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    (parts.next().is_none() && digits(device) && digits(inode) && digits(born)).then_some(())?;
    Some(Identity {
        device: device.parse().ok()?,
        inode: inode.parse().ok()?,
        born: Some(born.parse().ok()?).filter(|&born| born > 0),
    })
    .filter(|identity| identity.born.is_some())
}

/// Where the Project Store at `store` keeps the removal record of `workspace_id`.
pub(super) fn location(store: &Path, workspace_id: &str) -> PathBuf {
    store.join("removals").join(format!("{workspace_id}.toml"))
}

/// The removal record of `workspace_id`, `None` when there is none.
pub(super) fn read(store: &Path, workspace_id: &str) -> Result<Option<Record>, String> {
    let file = location(store, workspace_id);
    match std::fs::read_to_string(&file) {
        Ok(text) => parse(&text, workspace_id).map(Some).map_err(|error| {
            format!(
                "the removal record {}: {error}; af wrote it, so delete it only once what it \
                 names is cleared by hand",
                file.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

/// Parse and check a removal record of `workspace_id`.
pub(super) fn parse(text: &str, workspace_id: &str) -> Result<Record, String> {
    // The version is read before any other field: a record another release wrote is refused
    // as what it is, never decoded as this one.
    #[derive(Deserialize)]
    struct Version {
        version: Option<u32>,
    }
    if let Ok(Version {
        version: Some(version),
    }) = toml::from_str::<Version>(text)
        && version != VERSION
    {
        return Err(format!(
            "version {version} is not {VERSION}{}",
            if version == 1 {
                ", a superseded version of the removal record another af release wrote"
            } else {
                ""
            }
        ));
    }
    let record: Record = toml::from_str(text).map_err(|error| error.to_string())?;
    if record.version != VERSION {
        return Err(format!("version {} is not {VERSION}", record.version));
    }
    if record.workspace_id != workspace_id {
        return Err(format!("it names Workspace {}", record.workspace_id));
    }
    if !record.path.is_absolute() {
        return Err("it names a relative path".into());
    }
    for claim in record.checkout.iter().chain(&record.entries) {
        check(claim)?;
    }
    Ok(record)
}

fn check(claim: &Claim) -> Result<(), String> {
    let shown = claim.path.display();
    if !claim.path.is_absolute()
        || claim.path.parent().is_none()
        || claim.path.file_name().is_none()
    {
        return Err(format!("{shown} is no absolute path of a directory"));
    }
    if decode(&claim.identity).is_none() {
        return Err(format!(
            "the identity of {shown} is no `<device>:<inode>:<birth time>`"
        ));
    }
    let needs_container = !matches!(claim.step, Step::Inspected | Step::Done);
    let needs_identity = !matches!(claim.step, Step::Inspected | Step::Container | Step::Done);
    match &claim.container {
        Some(container) => {
            if container.parent() != claim.path.parent() || container.file_name().is_none() {
                return Err(format!(
                    "the container {} is not beside {shown}",
                    container.display()
                ));
            }
        }
        None if needs_container => {
            return Err(format!(
                "{shown} is {} without a container",
                claim.step.name()
            ));
        }
        None => {}
    }
    match &claim.container_identity {
        Some(identity) if decode(identity).is_none() => Err(format!(
            "the identity of the container of {shown} is no `<device>:<inode>:<birth time>`"
        )),
        None if needs_identity => Err(format!(
            "{shown} is {} without its container's identity",
            claim.step.name()
        )),
        _ => Ok(()),
    }
}

/// Replace the record whole through a temporary file and a rename.
pub(super) fn write(store: &Path, record: &Record) -> Result<(), String> {
    let file = location(store, &record.workspace_id);
    let directory = file.parent().ok_or("a removal record has no directory")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let text = toml::to_string(record).map_err(|error| error.to_string())?;
    let mut staged =
        tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    staged
        .write_all(
            format!(
                "# An unfinished `af project workspace remove --delete-worktree`; the next run of \
                 it finishes from here (ADR-0158).\n{text}"
            )
            .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    staged
        .persist(&file)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

/// Whether `record` is a completed removal: every directory it names is done, so nothing of it
/// is left to claim or delete and only the Workspace's unregistration and the record's own
/// deletion remain.
pub(super) fn complete(record: &Record) -> bool {
    record
        .checkout
        .iter()
        .chain(&record.entries)
        .all(|claim| claim.step == Step::Done)
}

/// Delete the record of `workspace_id`, and `removals/` once it is empty: only once the
/// Workspace's unregistration is durable (`ProjectStore::settle_removal`), or when a fresh
/// removal refused with nothing moved.
pub(super) fn delete(store: &Path, workspace_id: &str) -> Result<(), String> {
    let file = location(store, workspace_id);
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("deleting {}: {error}", file.display())),
    }
    if let Some(directory) = file.parent() {
        // Only an empty one goes; another Workspace's record keeps it.
        let _ = std::fs::remove_dir(directory);
    }
    Ok(())
}

/// The standard library's widening of a `stat`'s device and inode: the identity a walk compares
/// a name with the descriptor it opens there, never one a removal records.
#[allow(clippy::unnecessary_cast)]
fn identity_of(stat: &rustix::fs::Stat) -> Identity {
    Identity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        born: None,
    }
}

/// The born identity of `name` in `parent` (its device, inode and birth time), without following
/// a link; `None` when nothing is there. A file system that reports no birth time is the error,
/// one sentence naming it: nothing could tell the directory from a later one at its inode.
fn identity_at(parent: &OwnedFd, name: &OsStr) -> std::io::Result<Option<Identity>> {
    review_sandbox::born_at(parent, name)?
        .map(|stated| stated.complete(&name.to_string_lossy(), parent))
        .transpose()
}

/// The born identity of the directory `descriptor` holds open, refused as [`identity_at`]
/// refuses one.
fn identity_held(descriptor: &OwnedFd) -> std::io::Result<Identity> {
    review_sandbox::born_of_descriptor(descriptor)?
        .complete("a directory this removal opened", descriptor)
}

/// The directory `name` of `parent`, opened without following a link.
fn open_directory(parent: &OwnedFd, name: &OsStr) -> std::io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// The names in `directory`, sorted, without `.` and `..`.
fn names(directory: &OwnedFd) -> std::io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(name).to_os_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Rename `from` of `from_directory` to `to` of `to_directory`, never over a name that is taken.
fn rename_free(
    from_directory: &OwnedFd,
    from: &OsStr,
    to_directory: &OwnedFd,
    to: &OsStr,
) -> std::io::Result<()> {
    Ok(rustix::fs::renameat_with(
        from_directory,
        from,
        to_directory,
        to,
        RenameFlags::NOREPLACE,
    )?)
}

/// Delete the directory `name` of `parent`, which must be `identity`, and everything below it,
/// through descriptors: a link is unlinked as the entry it is, never followed, and nothing's
/// permissions are changed, so what may not be deleted stays and is the error.
#[cfg(test)]
pub(super) fn remove_tree(
    parent: &OwnedFd,
    name: &OsStr,
    identity: Identity,
) -> std::io::Result<()> {
    remove_tree_with(parent, name, identity, &mut |_| {})
}

/// [`remove_tree`], `unlinked` told the name of every entry the walk unlinks at the top of the
/// directory, after it went: where a debug build's fixture stops a deletion part way.
fn remove_tree_with(
    parent: &OwnedFd,
    name: &OsStr,
    identity: Identity,
    unlinked: &mut dyn FnMut(&OsStr),
) -> std::io::Result<()> {
    let directory = open_directory(parent, name)?;
    if identity_held(&directory)? != identity {
        return Err(std::io::Error::other(
            "it is not the directory this removal claimed",
        ));
    }
    empty(&directory, 0, unlinked)?;
    drop(directory);
    // POSIX has no unlink by descriptor: the name is checked once more right before it goes, and
    // `rmdir` removes only an empty directory.
    if identity_at(parent, name)? != Some(identity) {
        return Err(std::io::Error::other(
            "it changed while it was being removed",
        ));
    }
    Ok(rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?)
}

fn empty(
    directory: &OwnedFd,
    depth: usize,
    unlinked: &mut dyn FnMut(&OsStr),
) -> std::io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(std::io::Error::other(format!(
            "it nests deeper than {MAX_DEPTH} directories"
        )));
    }
    for name in names(directory)? {
        match open_directory(directory, &name) {
            Ok(child) => {
                empty(&child, depth + 1, &mut |_| {})?;
                drop(child);
                rustix::fs::unlinkat(directory, &name, AtFlags::REMOVEDIR)?;
            }
            // Not a directory, a link to one included: unlinked as what it is.
            Err(error)
                if matches!(
                    rustix::io::Errno::from_io_error(&error),
                    Some(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP)
                ) =>
            {
                rustix::fs::unlinkat(directory, &name, AtFlags::empty())?;
            }
            Err(error) => return Err(error),
        }
        if depth == 0 {
            unlinked(&name);
        }
    }
    Ok(())
}

/// The comparison of a claimed checkout with its commit: every path it holds, found through
/// descriptors without following a link, against the committed capture's manifest.
struct Comparison<'a> {
    manifest: &'a Manifest,
    seen: Vec<bool>,
    first: Option<String>,
    buffer: Vec<u8>,
}

impl Comparison<'_> {
    fn differs(&mut self, path: String) {
        if self
            .first
            .as_ref()
            .is_none_or(|first| path.as_bytes() < first.as_bytes())
        {
            self.first = Some(path);
        }
    }

    /// Compare one file or link of `kind` and `size` at the raw path `relative`; `content`
    /// digests it, and runs only when the manifest holds the path in that kind and size.
    fn compare(
        &mut self,
        relative: &[u8],
        kind: EntryKind,
        size: u64,
        content: impl FnOnce(&mut Vec<u8>) -> std::io::Result<String>,
    ) -> std::io::Result<()> {
        let path = review_source_git::encode_path(relative);
        let found = self
            .manifest
            .entries
            .binary_search_by(|entry| entry.path.as_bytes().cmp(path.as_bytes()));
        let Ok(index) = found else {
            self.differs(path);
            return Ok(());
        };
        self.seen[index] = true;
        let want = &self.manifest.entries[index];
        let (want_kind, want_size, want_content) = (want.kind, want.size, want.content.clone());
        if want_kind != kind || want_size != size || content(&mut self.buffer)? != want_content {
            self.differs(path);
        }
        Ok(())
    }

    fn walk(&mut self, directory: &OwnedFd, prefix: &[u8], depth: usize) -> std::io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(std::io::Error::other(format!(
                "it nests deeper than {MAX_DEPTH} directories"
            )));
        }
        for name in names(directory)? {
            let stat = rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
            let mut relative = prefix.to_vec();
            relative.extend_from_slice(name.as_bytes());
            let kind = FileType::from_raw_mode(stat.st_mode);
            // Git's own `.git` file at the top is no file of the worktree.
            if prefix.is_empty() && name == ".git" && kind == FileType::RegularFile {
                continue;
            }
            #[allow(clippy::unnecessary_cast)]
            let size = stat.st_size as u64;
            match kind {
                FileType::Directory => {
                    let child = open_directory(directory, &name)?;
                    if identity_of(&rustix::fs::fstat(&child)?) != identity_of(&stat) {
                        return Err(std::io::Error::other(format!(
                            "`{}` changed while it was read",
                            review_source_git::encode_path(&relative)
                        )));
                    }
                    relative.push(b'/');
                    self.walk(&child, &relative, depth + 1)?;
                }
                FileType::Symlink => {
                    let target = rustix::fs::readlinkat(directory, &name, Vec::new())?;
                    let target = target.as_bytes();
                    let content = review_source_git::digest_bytes(target);
                    self.compare(&relative, EntryKind::Symlink, target.len() as u64, |_| {
                        Ok(content)
                    })?;
                }
                FileType::RegularFile => {
                    #[allow(clippy::unnecessary_cast)]
                    let executable = (stat.st_mode as u32) & 0o111 != 0;
                    let kind = if executable {
                        EntryKind::Executable
                    } else {
                        EntryKind::File
                    };
                    let expected = identity_of(&stat);
                    self.compare(&relative, kind, size, |buffer| {
                        let file = rustix::fs::openat(
                            directory,
                            &name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )?;
                        if identity_of(&rustix::fs::fstat(&file)?) != expected {
                            return Err(std::io::Error::other(format!(
                                "`{}` changed while it was read",
                                review_source_git::encode_path(&relative)
                            )));
                        }
                        Ok(review_source_git::digest_reader_with_buffer(
                            std::fs::File::from(file),
                            buffer.as_mut_slice(),
                        )?
                        .0)
                    })?;
                }
                // A fifo, a socket or a device is never a file of a commit.
                _ => self.differs(review_source_git::encode_path(&relative)),
            }
        }
        Ok(())
    }
}

/// The first path, in manifest order, at which the directory `name` of `container`, which must
/// be `identity`, differs from `manifest` in content, mode or presence; `None` when they equal.
/// Every directory and file is reached through descriptors without following a link, and a file
/// is digested only when the manifest holds its path in its kind and size.
pub(super) fn first_difference_at(
    container: &OwnedFd,
    name: &OsStr,
    identity: Identity,
    manifest: &Manifest,
) -> std::io::Result<Option<String>> {
    let top = open_directory(container, name)?;
    if identity_held(&top)? != identity {
        return Err(std::io::Error::other(
            "it is not the directory this removal claimed",
        ));
    }
    let mut comparison = Comparison {
        manifest,
        seen: vec![false; manifest.entries.len()],
        first: None,
        buffer: vec![0; 64 * 1024],
    };
    comparison.walk(&top, b"", 0)?;
    let missing: Vec<String> = manifest
        .entries
        .iter()
        .zip(&comparison.seen)
        .filter(|(_, seen)| !**seen)
        .map(|(entry, _)| entry.path.clone())
        .collect();
    for path in missing {
        comparison.differs(path);
    }
    Ok(comparison.first)
}

/// A name for a container of `name`: `<name>.af-removing-<pid>-<16 random hex digits>`.
pub(super) fn container_name(name: &OsStr) -> OsString {
    use std::hash::{BuildHasher, Hasher};
    // 64 bits from the operating system's random source, through the standard library's
    // per-process hash keys.
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let mut container = name.to_os_string();
    container.push(format!("{REMOVING}{}-{random:016x}", std::process::id()));
    container
}

/// The `gitdir` of the administrative entry `entry`, `None` when it has none. A `gitdir` that
/// cannot be read may name the checkout, so it is an error naming that file, never "no entry".
pub(super) fn read_gitdir(entry: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = entry.join("gitdir");
    match std::fs::read(&file) {
        Ok(gitdir) => named_path(gitdir, &file).map(Some),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!(
            "{} could not be read ({error}), so its entry may name the checkout",
            file.display()
        )),
    }
}

/// Whether the `gitdir` of the administrative entry `entry` names `own`, the `.git` of a canonical
/// checkout path that may be gone: literally, or once the directory above the worktree, which
/// outlives it, is resolved (a relative `gitdir` against the entry, as git reads it).
pub(super) fn names_checkout(entry: &Path, gitdir: &[u8], own: &Path) -> bool {
    let named = entry.join(Path::new(OsStr::from_bytes(gitdir)));
    if named == own {
        return true;
    }
    let (Some(dot_git), Some(worktree)) = (named.file_name(), named.parent()) else {
        return false;
    };
    let (Some(name), Some(above)) = (worktree.file_name(), worktree.parent()) else {
        return false;
    };
    std::fs::canonicalize(above).is_ok_and(|above| above.join(name).join(dot_git) == own)
}

/// The administrative entry of the linked worktree at `path` (canonical): the directory under
/// `<common dir>/worktrees/` that its `.git` file names and whose `gitdir` names `<path>/.git`.
fn own_entry(git: &Git, path: &Path) -> Result<PathBuf, String> {
    let not_linked = |why: String| {
        format!(
            "{} is not a linked worktree of this repository whose administrative entry names it \
             ({why})",
            path.display()
        )
    };
    let dot_git = path.join(".git");
    let text = match std::fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.is_file() => std::fs::read(&dot_git)
            .map_err(|error| format!("reading {}: {error}", dot_git.display()))?,
        _ => return Err(not_linked("its .git is no file".into())),
    };
    let named = text
        .strip_prefix(b"gitdir: ")
        .map(|rest| rest.strip_suffix(b"\n").unwrap_or(rest))
        .ok_or_else(|| not_linked("its .git file names no gitdir".into()))?;
    let resolve = |base: &Path, bytes: &[u8]| {
        let named = Path::new(OsStr::from_bytes(bytes));
        std::fs::canonicalize(base.join(named)).ok()
    };
    let administrative = std::fs::canonicalize(git.common_dir()?.join("worktrees"))
        .map_err(|error| not_linked(format!("the repository has no worktrees/: {error}")))?;
    let entry = resolve(path, named)
        .filter(|entry| entry.parent() == Some(administrative.as_path()))
        .ok_or_else(|| not_linked("its .git file names no entry under worktrees/".into()))?;
    let gitdir = std::fs::read(entry.join("gitdir"))
        .map_err(|error| not_linked(format!("{}: {error}", entry.join("gitdir").display())))?;
    let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(&gitdir);
    if resolve(&entry, gitdir).as_deref() != Some(dot_git.as_path()) {
        return Err(not_linked(format!(
            "the gitdir of {} names another path",
            entry.display()
        )));
    }
    Ok(entry)
}

/// The born identity of the directory at `path`, without following a link; refused, nothing
/// changed, on a file system that reports no birth time.
fn directory_identity(path: &Path) -> Result<Identity, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspecting {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    Identity::born_of(path).map_err(|error| format!("inspecting {}: {error}", path.display()))
}

/// The born identity of what `at` names, without following a link; `None` when nothing is
/// there or it cannot be stated with its birth time.
fn identity_of_path(at: &Path) -> Option<Identity> {
    Identity::born_of(at).ok()
}

fn locked(entry: &Path) -> bool {
    std::fs::symlink_metadata(entry.join("locked")).is_ok()
}

/// The administrative entry at `at`, opened without following a link, while `fstat` of that
/// descriptor equals `identity`, the one a removal recorded at inspection; else why not. Another
/// directory, a link or a file at its path is a replacement, never read.
fn open_entry(at: &Path, identity: Identity) -> Result<OwnedFd, String> {
    let shown = at.display();
    let replaced = || {
        format!(
            "its administrative entry was replaced since inspection: {shown} is not the directory \
             this removal inspected, so it is left alone"
        )
    };
    let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
        return Err(format!("{shown} is no administrative entry"));
    };
    let unopened = |error: std::io::Error| {
        format!(
            "its administrative entry {shown} could not be opened ({error}), so it is not known \
             to name this checkout"
        )
    };
    let parent = open_anchor(parent).map_err(unopened)?;
    let entry = match open_directory(&parent, name) {
        Ok(entry) => entry,
        Err(error) => {
            return Err(match rustix::io::Errno::from_io_error(&error) {
                Some(rustix::io::Errno::NOENT) => format!(
                    "its administrative entry {shown} is gone since inspection, so it is not \
                     known to name this checkout"
                ),
                Some(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => replaced(),
                _ => unopened(error),
            });
        }
    };
    if identity_held(&entry).map_err(unopened)? != identity {
        return Err(replaced());
    }
    Ok(entry)
}

/// The `gitdir` of the opened administrative entry `entry`, shown as `at`, read through its
/// descriptor without following a link; `None` when it has none. As `read_gitdir`.
fn gitdir_in(entry: &OwnedFd, at: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read as _;
    let file = at.join("gitdir");
    let unreadable = |error: std::io::Error| {
        format!(
            "{} could not be read ({error}), so its entry may name the checkout",
            file.display()
        )
    };
    let opened = match rustix::fs::openat(
        entry,
        "gitdir",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(opened) => opened,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => return Ok(None),
        Err(errno) => return Err(unreadable(errno.into())),
    };
    let mut gitdir = Vec::new();
    std::fs::File::from(opened)
        .read_to_end(&mut gitdir)
        .map_err(unreadable)?;
    named_path(gitdir, &file).map(Some)
}

/// The path the bytes of the `gitdir` file `file` name, without its final newline.
fn named_path(mut gitdir: Vec<u8>, file: &Path) -> Result<Vec<u8>, String> {
    if gitdir.last() == Some(&b'\n') {
        gitdir.pop();
    }
    if gitdir.is_empty() || gitdir.contains(&0) {
        return Err(format!(
            "{} holds no path, so its entry may name the checkout",
            file.display()
        ));
    }
    Ok(gitdir)
}

/// Whether the opened administrative entry `entry`, shown as `at`, holds a `locked`; a `locked`
/// that cannot be looked at may be there, so it is the reason.
fn locked_in(entry: &OwnedFd, at: &Path) -> Result<bool, String> {
    match rustix::fs::statat(entry, "locked", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(errno) => Err(format!(
            "{} could not be looked at ({}), so its entry may be locked",
            at.join("locked").display(),
            std::io::Error::from(errno)
        )),
    }
}

/// What the administrative entry now at `at`, inspected at `recorded` as `identity`, holds that
/// keeps a removal of the checkout whose `.git` is `own` from claiming or deleting it; `None`
/// when nothing does. Read from the file system every time, whatever step the record holds: the
/// directory at `at`, opened without following a link, must be `identity`, or it is a
/// replacement and nothing of it is read; only then are its `gitdir` and `locked` read, through
/// that descriptor. Its `gitdir` must name `own` as git reads it from `recorded`, and it may not
/// be locked. A missing `gitdir` proves nothing, so it keeps the entry, unless the entry's
/// deletion has begun (`deleting`): that deletion removes its files in name order, `gitdir`
/// among the first, so then only a `gitdir` still there must name `own`. A `gitdir` that cannot
/// be read may name the checkout: it keeps the entry too, with the read error.
pub(super) fn entry_unproven(
    at: &Path,
    identity: Identity,
    recorded: &Path,
    own: &Path,
    deleting: bool,
) -> Option<String> {
    let shown = at.display();
    let entry = match open_entry(at, identity) {
        Ok(entry) => entry,
        Err(reason) => return Some(reason),
    };
    match gitdir_in(&entry, at) {
        Err(error) => return Some(error),
        Ok(None) if !deleting => {
            return Some(format!(
                "its administrative entry {shown} is kept: its gitdir is missing, so it is not \
                 known to name this checkout"
            ));
        }
        Ok(Some(gitdir)) if !names_checkout(recorded, &gitdir, own) => {
            return Some(format!(
                "its administrative entry {shown} is kept: its gitdir names {} now, not this \
                 worktree",
                String::from_utf8_lossy(&gitdir)
            ));
        }
        Ok(_) => {}
    }
    match locked_in(&entry, at) {
        Ok(false) => return None,
        Ok(true) => {}
        Err(error) => return Some(error),
    }
    Some(if at == recorded {
        format!(
            "its administrative entry {shown} is locked, so it is kept; unlock it with `git \
             worktree unlock` first"
        )
    } else {
        format!(
            "its administrative entry {shown} is locked, so it is kept; delete {} once nothing \
             holds the lock",
            at.join("locked").display()
        )
    })
}

fn inspected(path: &Path, identity: Identity) -> Claim {
    Claim {
        path: path.to_path_buf(),
        identity: encode(identity),
        step: Step::Inspected,
        container: None,
        container_identity: None,
    }
}

/// What a dirty or otherwise refused worktree is told: nothing changed, and the ways on.
fn refused(reason: &str) -> String {
    format!(
        "{reason}; nothing was changed: commit or discard the worktree's changes, or remove the \
         Workspace without --delete-worktree"
    )
}

/// What a finished `--delete-worktree` says: its line, then what it left alone.
pub(super) struct Removed {
    pub(super) line: String,
    pub(super) notes: Vec<String>,
}

/// Delete the linked worktree at `path`, the checkout of Workspace `workspace_id`, and its own
/// administrative entry, or finish the removal the record of an earlier run holds. Never `git
/// worktree remove`, whose cleanliness check runs the repository's clean filters (ADR-0001). The
/// error says what remains, or that nothing was changed.
pub(super) fn delete_worktree(
    git: &Git,
    store: &Path,
    workspace_id: &str,
    path: &Path,
) -> Result<Removed, String> {
    let common = git.common_dir()?.join("worktrees");
    let administrative = std::fs::canonicalize(&common).unwrap_or(common);
    let (record, fresh) = match read(store, workspace_id)? {
        Some(record) if record.path == path => (record, false),
        Some(record) => {
            return Err(format!(
                "the removal record {} names the checkout {}, not {}; nothing was changed",
                location(store, workspace_id).display(),
                record.path.display(),
                path.display()
            ));
        }
        None => match inspect(git, workspace_id, path, &administrative)? {
            Some(record) => (record, true),
            None => {
                return Ok(Removed {
                    line: format!(
                        "the worktree {} is already gone; its branch is kept",
                        path.display()
                    ),
                    notes: unrecorded(None, path, &administrative),
                });
            }
        },
    };
    let gone_before = record
        .checkout
        .as_ref()
        .is_none_or(|checkout| matches!(checkout.step, Step::Deleted | Step::Done));
    let removal = Removal {
        store,
        notes: unrecorded(Some(&record), path, &administrative),
        record,
        fresh,
        gone_before,
        changed: false,
    };
    if fresh {
        removal
            .save()
            .map_err(|error| format!("recording the removal: {error}; nothing was changed"))?;
    }
    removal.run(git)
}

/// Inspect the checkout at `path` for a fresh removal: its identity and its own administrative
/// entry's, or, when it is already gone, each entry at its own path whose `gitdir` names it.
/// `None` when it is gone with no such entry. Reads only.
fn inspect(
    git: &Git,
    workspace_id: &str,
    path: &Path,
    administrative: &Path,
) -> Result<Option<Record>, String> {
    let record = |checkout, entries| Record {
        version: VERSION,
        workspace_id: workspace_id.to_string(),
        path: path.to_path_buf(),
        checkout,
        entries,
    };
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let gone = |error: String| {
                format!(
                    "the worktree {} is already gone, but {error}; fix it and run the command \
                     again; nothing was changed",
                    path.display()
                )
            };
            let own = path.join(".git");
            let mut entries = Vec::new();
            for name in super::commands::administrative_names(administrative).map_err(gone)? {
                let entry = administrative.join(name);
                if read_gitdir(&entry)
                    .map_err(gone)?
                    .is_some_and(|gitdir| names_checkout(&entry, &gitdir, &own))
                {
                    if locked(&entry) {
                        return Err(gone(format!(
                            "its administrative entry {} is locked; unlock it with `git worktree \
                             unlock` first",
                            entry.display()
                        )));
                    }
                    entries.push(inspected(&entry, directory_identity(&entry).map_err(gone)?));
                }
            }
            Ok((!entries.is_empty()).then(|| record(None, entries)))
        }
        Err(error) => Err(format!("inspecting {}: {error}", path.display())),
        Ok(_) => {
            let entry = own_entry(git, path).map_err(|error| refused(&error))?;
            if locked(&entry) {
                return Err(format!(
                    "the worktree {} is locked; unlock it with `git worktree unlock` first",
                    path.display()
                ));
            }
            let checkout = directory_identity(path).map_err(|error| refused(&error))?;
            let entry_identity = directory_identity(&entry).map_err(|error| refused(&error))?;
            Ok(Some(record(
                Some(inspected(path, checkout)),
                vec![inspected(&entry, entry_identity)],
            )))
        }
    }
}

/// The directories beside the checkout and its entries named like a removal's container that no
/// record names: somebody else's, reported and left alone.
fn unrecorded(record: Option<&Record>, path: &Path, administrative: &Path) -> Vec<String> {
    let claims: Vec<&Claim> = record
        .map(|record| record.checkout.iter().chain(&record.entries).collect())
        .unwrap_or_default();
    let recorded: BTreeSet<&Path> = claims
        .iter()
        .filter_map(|claim| claim.container.as_deref())
        .collect();
    let mut places: BTreeSet<(PathBuf, OsString)> = BTreeSet::new();
    let mut place = |at: &Path| {
        if let (Some(parent), Some(name)) = (at.parent(), at.file_name()) {
            places.insert((parent.to_path_buf(), name.to_os_string()));
        }
    };
    place(path);
    place(&administrative.join(path.file_name().unwrap_or_default()));
    for claim in &claims {
        place(&claim.path);
    }
    let mut notes = Vec::new();
    for (directory, name) in places {
        let mut prefix = name.as_bytes().to_vec();
        prefix.extend_from_slice(REMOVING.as_bytes());
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut found: Vec<PathBuf> = listing
            .flatten()
            .map(|entry| entry.path())
            .filter(|found| {
                found
                    .file_name()
                    .is_some_and(|found| found.as_bytes().starts_with(&prefix))
                    && !recorded.contains(found.as_path())
            })
            .collect();
        found.sort();
        notes.extend(found.into_iter().map(|found| {
            format!(
                "left {}: it is named like a removal's container, but no removal record names \
                 it, so it is not this removal's",
                found.display()
            )
        }));
    }
    notes
}

/// Which directory of the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Checkout,
    Entry(usize),
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Checkout => "checkout",
            Which::Entry(_) => "entry",
        }
    }
}

/// A directory this removal holds in its container: found there by its identity.
struct Held {
    parent: OwnedFd,
    container: OwnedFd,
    container_path: PathBuf,
    name: OsString,
    identity: Identity,
}

/// Where a claim stands.
enum Claimed {
    Held(Held),
    /// It is deleted already.
    Deleted,
    /// It is neither at its path nor in its container: gone without this removal.
    Vanished,
    /// Another directory is at its path and it is not in its container: never claimed.
    Replaced,
}

/// Why a claim stopped.
enum Fail {
    /// It stays at its own path: its container could not be made, or the move into it failed;
    /// `container` is the empty container left behind when it could not be removed either.
    Stays {
        error: std::io::Error,
        container: Option<PathBuf>,
    },
    /// Its deletion in its container failed part way.
    Remains { at: PathBuf, error: std::io::Error },
    /// Its container holds `first`, which this removal did not put there.
    Foreign { container: PathBuf, first: PathBuf },
    /// Its container is not the one this removal made.
    ContainerReplaced { container: PathBuf },
    /// What it holds now does not prove it is this removal's to claim or delete: it stays where
    /// it is and the step stays where it was, for the reason given.
    Kept(String),
    /// It is in its container, moved there and not yet judged or deleted, and another directory
    /// appeared at its original `path` since: a replacement that stops the removal. It stays in
    /// its container, at `kept`, and nothing is deleted.
    Appeared { path: PathBuf, kept: PathBuf },
    /// Anything else, whole.
    Other(String),
}

impl Fail {
    /// The failure of one claim, `noun` and `path` naming the directory.
    fn describe(&self, noun: &str, path: &Path) -> String {
        match self {
            Fail::Stays { error, container } => {
                let mut described = format!("{noun} {} remains: {error}", path.display());
                if let Some(container) = container {
                    described.push_str(&format!(
                        "; so does the empty directory {} made to receive it",
                        container.display()
                    ));
                }
                described
            }
            Fail::Remains { at, error } => format!("{noun} remains at {}: {error}", at.display()),
            Fail::Foreign { container, first } => format!(
                "the directory {}, which this removal made to receive {noun}, holds {}, which \
                 this removal did not put there, so it is kept with what it holds; remove that \
                 once nothing needs it",
                container.display(),
                first.display()
            ),
            Fail::ContainerReplaced { container } => format!(
                "the directory {}, which this removal made to receive {noun}, was replaced \
                 since, so it is kept and nothing more is moved",
                container.display()
            ),
            Fail::Appeared { path, kept } => format!(
                "{noun} is kept at {}, where this removal moved it: a new directory appeared at \
                 {} since this removal moved {noun}",
                kept.display(),
                path.display()
            ),
            Fail::Kept(message) | Fail::Other(message) => message.clone(),
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(error: std::io::Error) -> Self {
        Fail::Other(error.to_string())
    }
}

/// Why a removal stops short of done, which decides whether its record may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The worktree as it is refuses the removal: its changes, its commit or a move the
    /// operating system refused. While nothing moved the record goes; `advice` adds the ways on.
    Refused { advice: bool },
    /// A precondition of what the record holds failed: the entry check, or the identity or state
    /// a step requires. The record stays exactly as the stop found it, so the next run honours
    /// the identities it holds instead of inspecting afresh.
    Precondition,
}

struct Removal<'a> {
    store: &'a Path,
    record: Record,
    /// This run inspected afresh and wrote the record itself: no earlier run recorded anything,
    /// so a failed check refuses what this run inspected rather than what a record holds.
    fresh: bool,
    /// The checkout was gone before this run.
    gone_before: bool,
    /// This run moved or deleted something.
    changed: bool,
    notes: Vec<String>,
}

impl Removal<'_> {
    fn path(&self) -> &Path {
        &self.record.path
    }

    fn claim(&self, which: Which) -> &Claim {
        match which {
            Which::Checkout => self.record.checkout.as_ref().expect("a recorded checkout"),
            Which::Entry(index) => &self.record.entries[index],
        }
    }

    fn claim_mut(&mut self, which: Which) -> &mut Claim {
        match which {
            Which::Checkout => self.record.checkout.as_mut().expect("a recorded checkout"),
            Which::Entry(index) => &mut self.record.entries[index],
        }
    }

    fn save(&self) -> Result<(), String> {
        write(self.store, &self.record)
    }

    /// Record that `which` reached `step`, with its container when it has one.
    fn reach(
        &mut self,
        which: Which,
        step: Step,
        container: Option<(PathBuf, Option<Identity>)>,
    ) -> Result<(), Fail> {
        let claim = self.claim_mut(which);
        claim.step = step;
        match container {
            Some((path, identity)) => {
                claim.container = Some(path);
                claim.container_identity = identity.map(encode);
            }
            None if matches!(step, Step::Inspected | Step::Done) => {
                claim.container = None;
                claim.container_identity = None;
            }
            None => {}
        }
        self.save().map_err(|error| {
            Fail::Other(format!(
                "recording the removal in {}: {error}",
                location(self.store, &self.record.workspace_id).display()
            ))
        })?;
        stop_at(which, step.name());
        Ok(())
    }

    /// Whether every directory of the record is still only inspected.
    fn nothing_moved(&self) -> bool {
        !self.changed
            && self
                .record
                .checkout
                .iter()
                .chain(&self.record.entries)
                .all(|claim| matches!(claim.step, Step::Inspected))
    }

    /// How a failed check stops: on a rerun it is a precondition of the record an earlier run
    /// wrote; on a fresh run it refuses what this run itself just inspected and recorded.
    fn failed_check(&self) -> Stop {
        if self.fresh {
            Stop::Refused { advice: false }
        } else {
            Stop::Precondition
        }
    }

    /// The message of a stop that `reason` explains. The record is deleted here only when the
    /// worktree refused the removal (`Stop::Refused`) while nothing moved, and the message then
    /// says nothing was changed, with the ways on when its state is the reason (`advice`). A
    /// failed precondition never deletes it: it stays as it is for the rerun, and only the
    /// unregistration that follows a completed removal deletes it after.
    fn abandon(&mut self, reason: &str, stop: Stop) -> String {
        let file = location(self.store, &self.record.workspace_id);
        match stop {
            Stop::Refused { advice } if self.nothing_moved() => {
                match delete(self.store, &self.record.workspace_id) {
                    Ok(()) if advice => refused(reason),
                    Ok(()) => format!("{reason}; nothing was changed"),
                    Err(error) => format!("{reason}; nothing was moved, but {error}"),
                }
            }
            Stop::Precondition if self.nothing_moved() => format!(
                "{reason}; nothing was changed: the removal stays recorded in {}, and the same \
                 command finishes it from there once that is resolved",
                file.display()
            ),
            Stop::Refused { .. } | Stop::Precondition => format!(
                "{reason}; the removal is recorded in {}, and the same command finishes it once \
                 that is resolved",
                file.display()
            ),
        }
    }

    fn with_notes(&self, message: String) -> String {
        std::iter::once(message)
            .chain(self.notes.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run(mut self, git: &Git) -> Result<Removed, String> {
        if self
            .record
            .checkout
            .as_ref()
            .is_some_and(|checkout| checkout.step != Step::Done)
        {
            self.checkout(git).map_err(|error| self.with_notes(error))?;
        }
        let mut removed = Vec::new();
        for index in 0..self.record.entries.len() {
            if let Some(at) = self.entry(index).map_err(|error| self.with_notes(error))? {
                removed.push(at);
            }
        }
        // The record stays, complete, until the Workspace's unregistration is durable: it is the
        // caller's to delete after that (`ProjectStore::settle_removal`), never before.
        debug_assert!(complete(&self.record));
        let path = self.path().display();
        let line = if !self.gone_before {
            format!("removed the worktree {path}; its branch is kept")
        } else if removed.is_empty() {
            format!("the worktree {path} is already gone; its branch is kept")
        } else {
            format!(
                "the worktree {path} was already gone; removed its administrative entry {}; its \
                 branch is kept",
                removed
                    .iter()
                    .map(|at| at.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Ok(Removed {
            line,
            notes: self.notes,
        })
    }

    /// Claim the checkout, judge it in its container, delete it, then the container.
    fn checkout(&mut self, git: &Git) -> Result<(), String> {
        let entry = self
            .record
            .entries
            .first()
            .map(|entry| entry.path.clone())
            .ok_or("the removal record names no administrative entry of the checkout")?;
        let path = self.path().to_path_buf();
        // Before any step touches the live checkout, wherever it is: a failed check leaves it
        // there. Once its deletion is recorded, nothing of it is left to claim, judge or delete:
        // its rerun removes only its container, which its recorded identity proves.
        if self.claim(Which::Checkout).step < Step::Deleted
            && let Some(reason) = self.entry_proves(Which::Checkout)
        {
            return Err(self.checkout_kept(&reason));
        }
        // The commit and its tree, read through the administrative entry, never the checkout.
        let snapshot = if matches!(
            self.claim(Which::Checkout).step,
            Step::Inspected | Step::Container | Step::Contained
        ) {
            trace(Which::Checkout, "capture");
            match capture(git, &entry) {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    let reason = format!("capturing the HEAD of {}: {error}", path.display());
                    return Err(self.abandon(&reason, Stop::Refused { advice: true }));
                }
            }
        } else {
            None
        };
        match self.claim_directory(Which::Checkout) {
            Ok(Claimed::Held(held)) => {
                if self.claim(Which::Checkout).step == Step::Contained {
                    // Its entry is read again before it is judged: a run that moved it may be
                    // an earlier one.
                    if let Some(reason) = self.unproven(Which::Checkout, &path, false) {
                        let stop = self.failed_check();
                        return Err(self.restore(&held, &reason, stop));
                    }
                    // Captured before the claim, unless the checkout came back out of its
                    // container since an earlier run judged it.
                    trace(Which::Checkout, "judge");
                    let verdict = match &snapshot {
                        Some(snapshot) => judge(git, &entry, &held, snapshot, &path),
                        None => capture(git, &entry)
                            .map_err(|error| {
                                format!("capturing the HEAD of {}: {error}", path.display())
                            })
                            .and_then(|snapshot| judge(git, &entry, &held, &snapshot, &path)),
                    };
                    if let Some(reason) = verdict.unwrap_or_else(Some) {
                        return Err(self.restore(&held, &reason, Stop::Refused { advice: true }));
                    }
                    self.reach(Which::Checkout, Step::Deleting, None)
                        .map_err(|fail| self.checkout_failure(&fail, &entry))?;
                }
                self.delete_held(Which::Checkout, &held)
                    .map_err(|fail| self.checkout_failure(&fail, &entry))?;
            }
            Ok(Claimed::Deleted) => {}
            Ok(Claimed::Vanished) => self.gone_before = true,
            Ok(Claimed::Replaced) => {
                // Neither in its container nor at its path: its empty container goes, and the
                // claim is only inspected, never done.
                let _ = self.remove_container(Which::Checkout, Step::Inspected);
                let stop = self.failed_check();
                return Err(self.abandon(
                    &format!(
                        "the worktree {} was replaced since this removal inspected it: the \
                         directory there now is another one, so it is kept",
                        path.display()
                    ),
                    stop,
                ));
            }
            Err(fail) => return Err(self.checkout_failure(&fail, &entry)),
        }
        self.remove_container(Which::Checkout, Step::Done)
            .map_err(|fail| self.checkout_failure(&fail, &entry))?;
        if std::fs::symlink_metadata(&path).is_ok() {
            self.notes.push(format!(
                "kept {}: it appeared there after this removal claimed the worktree, so it is \
                 not the worktree",
                path.display()
            ));
        }
        Ok(())
    }

    fn checkout_failure(&mut self, fail: &Fail, entry: &Path) -> String {
        let path = self.path().display().to_string();
        match fail {
            Fail::Stays { error, container } => {
                let mut reason = format!(
                    "moving the worktree {path} into a directory beside it failed: {error}"
                );
                if let Some(container) = container {
                    reason.push_str(&format!(
                        "; the empty directory {} made to receive it remains, so delete it once \
                         nothing uses it",
                        container.display()
                    ));
                }
                self.abandon(&reason, Stop::Refused { advice: false })
            }
            Fail::Remains { at, error } => format!(
                "removing the worktree {path}: {error}; what is left of it is at {}, its \
                 administrative entry {} remains, and the Workspace stays registered; fix it and \
                 run the command again",
                at.display(),
                entry.display()
            ),
            // It stays where it is, and the record holds the step it had.
            Fail::Kept(reason) => {
                let stop = self.failed_check();
                self.abandon(reason, stop)
            }
            // It stays in its container; nothing is deleted and the record stays as it is.
            Fail::Appeared { path, kept } => format!(
                "the worktree {} is kept at {}, where this removal moved it, and the Workspace \
                 stays registered: a new directory appeared at {} since this removal moved the \
                 recorded worktree; clear it or move the recorded worktree back, then run the \
                 command again",
                path.display(),
                kept.display(),
                path.display()
            ),
            Fail::Foreign { .. } | Fail::ContainerReplaced { .. } | Fail::Other(_) => {
                let reason = fail.describe("the worktree", self.path());
                let deleted = self.claim(Which::Checkout).step >= Step::Deleted;
                if !deleted && self.nothing_moved() {
                    // A container that is not as this removal made it fails the step's own
                    // precondition; anything else is the operating system's refusal.
                    let stop = match fail {
                        Fail::Other(_) => Stop::Refused { advice: false },
                        _ => self.failed_check(),
                    };
                    return self.abandon(&reason, stop);
                }
                let prefix = if deleted {
                    format!("removed the worktree {path}; ")
                } else {
                    String::new()
                };
                format!(
                    "{prefix}{reason}; its administrative entry {} remains, and the Workspace \
                     stays registered; fix it and run the command again",
                    entry.display()
                )
            }
        }
    }

    /// Put a refused checkout back at its path, never over one that is taken, and say why, with
    /// the ways on when its changes are the reason (`advice`).
    fn restore(&mut self, held: &Held, reason: &str, stop: Stop) -> String {
        trace(Which::Checkout, "restore");
        match rename_free(&held.container, &held.name, &held.parent, &held.name) {
            Ok(()) => {
                self.changed = false;
                let left = self
                    .remove_container(Which::Checkout, Step::Inspected)
                    .err();
                let mut message = self.abandon(reason, stop);
                if let Some(left) = left {
                    message.push('\n');
                    message.push_str(&left.describe("the worktree", self.path()));
                }
                message
            }
            Err(error) => format!(
                "{reason}; it is kept at {}, because it could not be moved back to {}: {error}; \
                 move it back once nothing is in the way, or run the command again once it is \
                 clean",
                held.container_path.join(&held.name).display(),
                self.path().display()
            ),
        }
    }

    /// Delete the administrative entry `index` once its checkout is gone. Where it was deleted
    /// from, `None` when nothing of it was left to delete.
    fn entry(&mut self, index: usize) -> Result<Option<PathBuf>, String> {
        let which = Which::Entry(index);
        if self.claim(which).step == Step::Done {
            return Ok(None);
        }
        // Before any step touches the entry, as its step and where it is now require: a failed
        // check leaves it there.
        if let Some(reason) = self.entry_proves(which) {
            return Err(self.entry_refusal(&reason));
        }
        let claim = self.claim(which).clone();
        let identity = decode(&claim.identity).ok_or("a removal record without identity")?;
        let at_path = identity_of_path(&claim.path);
        // Where it is found decides only where it is reported deleted from; whether it may be
        // claimed or deleted is read from it wherever it is (`claim_directory`).
        let in_place = at_path == Some(identity);
        let shown = match (&claim.container, in_place, claim.step) {
            (Some(container), false, Step::Contained | Step::Deleting) => {
                container.join(claim.path.file_name().unwrap_or_default())
            }
            _ => claim.path.clone(),
        };
        match self.claim_directory(which) {
            Ok(Claimed::Held(held)) => {
                if self.claim(which).step == Step::Contained {
                    self.reach(which, Step::Deleting, None)
                        .map_err(|fail| self.entry_failure(&fail, &claim.path))?;
                }
                self.delete_held(which, &held)
                    .map_err(|fail| self.entry_failure(&fail, &claim.path))?;
            }
            Ok(Claimed::Deleted) => {}
            Ok(Claimed::Vanished) => return self.settle(which).map(|()| None),
            Ok(Claimed::Replaced) => {
                self.notes.push(format!(
                    "kept {}: it is not the administrative entry this removal inspected",
                    claim.path.display()
                ));
                return self.settle(which).map(|()| None);
            }
            // Left where it is, and the Workspace stays registered until an operator clears it.
            Err(Fail::Kept(reason)) => return Err(self.entry_refusal(&reason)),
            // Left in its container, nothing deleted, the record as it is.
            Err(Fail::Appeared { path, kept }) => {
                return Err(format!(
                    "{}, but its administrative entry is kept at {}, where this removal moved \
                     it, and the Workspace stays registered: a new administrative entry \
                     appeared at {} since this removal moved the recorded one; clear it or move \
                     the recorded entry back, then run the command again",
                    self.prefix(),
                    kept.display(),
                    path.display()
                ));
            }
            Err(fail) => return Err(self.entry_failure(&fail, &claim.path)),
        }
        self.remove_container(which, Step::Done)
            .map_err(|fail| self.entry_failure(&fail, &claim.path))?;
        Ok(Some(shown))
    }

    /// Finish `which` without deleting it: its empty container goes, and it is done.
    fn settle(&mut self, which: Which) -> Result<(), String> {
        let entry = self.claim(which).path.clone();
        self.remove_container(which, Step::Done)
            .map_err(|fail| self.entry_failure(&fail, &entry))
    }

    fn prefix(&self) -> String {
        if self.gone_before {
            format!("the worktree {} is already gone", self.path().display())
        } else {
            format!("removed the worktree {}", self.path().display())
        }
    }

    fn entry_refusal(&self, reason: &str) -> String {
        if self.gone_before && !self.changed {
            format!(
                "{}, but {reason}; the Workspace stays registered; fix it and run the command \
                 again; nothing was changed",
                self.prefix()
            )
        } else {
            format!(
                "{}, but {reason}; the Workspace stays registered; fix it and run the command \
                 again",
                self.prefix()
            )
        }
    }

    /// What keeps `which`, now at `at`, from being claimed or deleted, read from the file system
    /// whatever step the record holds; `None` when nothing does. An entry is read where it is
    /// (`entry_unproven`); the checkout is claimed only while its entry, at its own path then,
    /// still names it and is not locked.
    fn unproven(&self, which: Which, at: &Path, deleting: bool) -> Option<String> {
        let own = self.path().join(".git");
        match which {
            Which::Entry(index) => {
                let entry = &self.record.entries[index];
                let Some(identity) = decode(&entry.identity) else {
                    return Some("a removal record without identity".into());
                };
                entry_unproven(at, identity, &entry.path, &own, deleting)
            }
            Which::Checkout => {
                let entry = self.record.entries.first()?;
                let Some(identity) = decode(&entry.identity) else {
                    return Some("a removal record without identity".into());
                };
                entry_unproven(&entry.path, identity, &entry.path, &own, false).map(|reason| {
                    format!(
                        "the worktree {} is not claimed, because {reason}",
                        self.path().display()
                    )
                })
            }
        }
    }

    /// The entry check a rerun makes before anything touches `which`, as its recorded step
    /// requires: the administrative entry, opened now without following a link, must be the one
    /// the record holds by identity (device, inode and birth time), and only then is it read: its
    /// `gitdir` must name the checkout and it may hold no `locked`; the reason it does not,
    /// `None` when it does. The live checkout's entry (`Removal::checkout` asks only before the
    /// checkout's deletion is recorded) is opened at its own path, where it stays until the
    /// checkout is done, and another directory there is a replacement. An entry is read where its
    /// recorded identity is found now, at its path or in its container. Live (before its deletion
    /// is recorded), its `gitdir` must be there, and one found in neither place while another
    /// directory holds its path is a replacement. Once its deletion is recorded (`deleting`) and
    /// it is in its container, that deletion resumes without the `gitdir` it may have removed;
    /// found in neither place, its deletion finished, and whatever its path holds now is not it,
    /// which its claim keeps and reports. Recorded `deleted`, nothing of it is left to read: its
    /// claim verifies the recorded locations and advances it to done.
    fn entry_proves(&self, which: Which) -> Option<String> {
        let checked = |at: &Path, identity, entry: &Claim, deleting| {
            trace(which, &format!("check-{}", self.claim(which).step.name()));
            entry_unproven(
                at,
                identity,
                &entry.path,
                &self.path().join(".git"),
                deleting,
            )
        };
        let entry = match which {
            Which::Checkout => self.record.entries.first(),
            Which::Entry(index) => self.record.entries.get(index),
        };
        let Some(entry) = entry else {
            trace(which, &format!("check-{}", self.claim(which).step.name()));
            return Some("the removal record names no administrative entry of the checkout".into());
        };
        let Some(identity) = decode(&entry.identity) else {
            return Some("a removal record without identity".into());
        };
        match which {
            Which::Checkout => checked(&entry.path, identity, entry, false),
            Which::Entry(_) => match (entry.step, found(entry)) {
                (Step::Deleted | Step::Done, _) => None,
                (Step::Deleting, Some(at)) if at != entry.path => {
                    checked(&at, identity, entry, true)
                }
                (_, Some(at)) => checked(&at, identity, entry, false),
                (Step::Deleting, None) => None,
                (_, None) if std::fs::symlink_metadata(&entry.path).is_ok() => {
                    checked(&entry.path, identity, entry, false)
                }
                (_, None) => None,
            },
        }
    }

    /// What a checkout whose entry failed the check is told: it is left where it was found, in
    /// its container by its claim name when a run moved it there.
    fn checkout_kept(&mut self, reason: &str) -> String {
        let claim = self.claim(Which::Checkout).clone();
        if claim.step >= Step::Deleted {
            return self.entry_refusal(reason);
        }
        let path = self.path().display().to_string();
        let message = match found(&claim) {
            Some(at) if at != claim.path => format!(
                "the worktree {path} is kept at {}, where this removal moved it, because \
                 {reason}; the Workspace stays registered; move it back to {path} to keep it",
                at.display()
            ),
            _ => format!("the worktree {path} is not claimed, because {reason}"),
        };
        let stop = self.failed_check();
        self.abandon(&message, stop)
    }

    fn entry_failure(&self, fail: &Fail, entry: &Path) -> String {
        format!(
            "{}; {}; fix it and run the command again",
            self.prefix(),
            fail.describe("its administrative entry", entry)
        )
    }

    /// Bring `which` into its container: through the steps its record holds, each recorded
    /// before the next is taken.
    fn claim_directory(&mut self, which: Which) -> Result<Claimed, Fail> {
        trace(which, "claim");
        let claim = self.claim(which).clone();
        let identity = decode(&claim.identity)
            .ok_or_else(|| Fail::Other("a removal record without identity".into()))?;
        let (Some(parent_path), Some(name)) = (claim.path.parent(), claim.path.file_name()) else {
            return Err(Fail::Other(format!(
                "{} is no directory to claim",
                claim.path.display()
            )));
        };
        let parent_path = parent_path.to_path_buf();
        let name = name.to_os_string();
        let parent = open_anchor(&parent_path)?;
        let mut tries = 0;
        let mut moved_now = false;
        loop {
            let claim = self.claim(which).clone();
            match claim.step {
                Step::Deleted | Step::Done => return Ok(Claimed::Deleted),
                Step::Inspected => {
                    match identity_at(&parent, &name)? {
                        None => return Ok(Claimed::Vanished),
                        Some(found) if found != identity => return Ok(Claimed::Replaced),
                        Some(_) => {}
                    }
                    // Read again whatever an earlier run found: what no longer proves it is this
                    // removal's stays where it is, and no container is named for it.
                    if let Some(reason) = self.unproven(which, &claim.path, false) {
                        return Err(Fail::Kept(reason));
                    }
                    // Named before it is made, so no container of this removal goes unrecorded.
                    let container = container_name(&name);
                    let container_path = parent_path.join(&container);
                    self.reach(which, Step::Container, Some((container_path.clone(), None)))?;
                    match rustix::fs::mkdirat(&parent, &container, Mode::RWXU) {
                        Ok(()) => {}
                        Err(rustix::io::Errno::EXIST) if tries < CONTAINER_TRIES => {
                            tries += 1;
                            self.reach(which, Step::Inspected, None)?;
                            continue;
                        }
                        Err(errno) => {
                            self.reach(which, Step::Inspected, None)?;
                            return Err(Fail::Stays {
                                error: errno.into(),
                                container: None,
                            });
                        }
                    }
                    let made = identity_at(&parent, &container)?.ok_or_else(|| {
                        Fail::Other(format!(
                            "{} vanished as soon as it was made",
                            container_path.display()
                        ))
                    })?;
                    self.reach(which, Step::Contained, Some((container_path, Some(made))))?;
                }
                Step::Container => {
                    // Named, perhaps made, but never identified: nothing proves a directory
                    // there is this removal's.
                    if let Some(container) = &claim.container
                        && std::fs::symlink_metadata(container).is_ok()
                    {
                        self.notes.push(format!(
                            "left {}: an interrupted removal named it before it made it, and \
                             nothing proves it is that removal's",
                            container.display()
                        ));
                    }
                    self.reach(which, Step::Inspected, None)?;
                }
                Step::Contained | Step::Deleting => {
                    let container_path = claim.container.clone().expect("checked when read");
                    let expected = claim
                        .container_identity
                        .as_deref()
                        .and_then(decode)
                        .expect("checked when read");
                    let container_name = container_path
                        .file_name()
                        .expect("checked when read")
                        .to_os_string();
                    let container = match open_directory(&parent, &container_name) {
                        Ok(container) => container,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            // Its container is gone: what is at its path decides, never the step.
                            match identity_at(&parent, &name)? {
                                Some(found) if found == identity => {
                                    // Back at its path: claimed again from inspection, and the
                                    // checkout judged again.
                                    self.reach(which, Step::Inspected, None)?;
                                    continue;
                                }
                                Some(_) => return Ok(Claimed::Replaced),
                                None if claim.step == Step::Deleting => {
                                    return Ok(Claimed::Deleted);
                                }
                                None => return Ok(Claimed::Vanished),
                            }
                        }
                        Err(error) => {
                            return Err(Fail::Other(format!(
                                "opening {}: {error}",
                                container_path.display()
                            )));
                        }
                    };
                    if identity_held(&container)? != expected {
                        if claim.step == Step::Contained
                            && identity_at(&parent, &name)? == Some(identity)
                        {
                            // Never used: the directory stays where it is.
                            self.reach(which, Step::Inspected, None)?;
                        }
                        return Err(Fail::ContainerReplaced {
                            container: container_path,
                        });
                    }
                    let at_path = identity_at(&parent, &name)?;
                    if claim.step == Step::Contained && at_path == Some(identity) && !moved_now {
                        if let Some(reason) = self.unproven(which, &claim.path, false) {
                            return Err(Fail::Kept(reason));
                        }
                        // Made empty by this removal: anything in it now is somebody else's,
                        // and the directory stays where it is.
                        if let Some(first) = names(&container)?.into_iter().next() {
                            self.reach(which, Step::Inspected, None)?;
                            return Err(Fail::Foreign {
                                first: container_path.join(first),
                                container: container_path,
                            });
                        }
                        if let Err(error) = claim_into(&parent, &name, &container) {
                            // Empty and made by this removal: removed while it is still that.
                            let left = self
                                .remove_container(which, Step::Inspected)
                                .err()
                                .map(|_| container_path);
                            return Err(Fail::Stays {
                                error,
                                container: left,
                            });
                        }
                        moved_now = true;
                        self.changed = true;
                        stop_at(which, "moved");
                    }
                    match identity_at(&container, &name)? {
                        Some(found) if found == identity => {
                            // Moved and not yet judged or deleted: its original path must still
                            // be free. A directory there now, after an earlier run's move or
                            // this one's, is a replacement (a new administrative entry, or a
                            // new checkout) that stops the removal: this one stays in its
                            // container and nothing is deleted.
                            if claim.step == Step::Contained {
                                let now = if moved_now {
                                    identity_at(&parent, &name)?
                                } else {
                                    at_path
                                };
                                if now.is_some() {
                                    return Err(Fail::Appeared {
                                        path: claim.path.clone(),
                                        kept: container_path.join(&name),
                                    });
                                }
                            }
                            // An entry found there is read there before it is deleted; the
                            // checkout is judged there (`checkout`).
                            if matches!(which, Which::Entry(_))
                                && let Some(reason) = self.unproven(
                                    which,
                                    &container_path.join(&name),
                                    claim.step == Step::Deleting,
                                )
                            {
                                return Err(Fail::Kept(reason));
                            }
                            return Ok(Claimed::Held(Held {
                                parent,
                                container,
                                container_path,
                                name,
                                identity,
                            }));
                        }
                        Some(_) if moved_now => {
                            // Another directory took the path between the check and the move:
                            // it goes back, never over a name that is taken again.
                            let moved = container_path.join(&name);
                            return match rename_free(&container, &name, &parent, &name) {
                                Ok(()) => {
                                    // Put back as it was: this move changed nothing.
                                    self.changed = false;
                                    self.notes.push(format!(
                                        "kept {}: another directory took the worktree's place \
                                         while this removal claimed it, and it was put back",
                                        claim.path.display()
                                    ));
                                    Ok(Claimed::Replaced)
                                }
                                Err(error) => Err(Fail::Other(format!(
                                    "another directory took the place of {} while this removal \
                                     claimed it; it could not be put back ({error}) and is kept \
                                     at {}",
                                    claim.path.display(),
                                    moved.display()
                                ))),
                            };
                        }
                        _ => {}
                    }
                    if at_path == Some(identity) && !moved_now {
                        // Moved back out of its container since: claimed and judged again.
                        self.reach(which, Step::Contained, None)?;
                        continue;
                    }
                    // Neither in its container nor at its path: another directory there is a
                    // replacement, kept; only when nothing holds its identity is it deleted.
                    return Ok(match at_path {
                        Some(found) if found != identity => Claimed::Replaced,
                        _ if claim.step == Step::Deleting => Claimed::Deleted,
                        _ => Claimed::Vanished,
                    });
                }
            }
        }
    }

    /// Delete what `which` holds in its container, then record that. A debug build's fixture
    /// may stop as a crash would part way, once an entry's `gitdir` is unlinked
    /// (`<which>-partial`), and once the directory is unlinked, before that is recorded
    /// (`<which>-unlinked`).
    fn delete_held(&mut self, which: Which, held: &Held) -> Result<(), Fail> {
        trace(which, "delete");
        self.changed = true;
        remove_tree_with(&held.container, &held.name, held.identity, &mut |name| {
            if name == "gitdir" {
                stop_at(which, "partial");
            }
        })
        .map_err(|error| Fail::Remains {
            at: held.container_path.join(&held.name),
            error,
        })?;
        stop_at(which, "unlinked");
        self.reach(which, Step::Deleted, None)
    }

    /// Remove the container of `which` with `rmdir`, only while it is still the one this removal
    /// made and empty, and record `which` at `after`: done, or only inspected when it was never
    /// deleted. Anything in it is somebody else's: kept, and the first such path is the failure.
    fn remove_container(&mut self, which: Which, after: Step) -> Result<(), Fail> {
        trace(which, "rmdir");
        let claim = self.claim(which).clone();
        let (Some(container_path), Some(expected)) = (
            claim.container.clone(),
            claim.container_identity.as_deref().and_then(decode),
        ) else {
            return self.reach(which, after, None);
        };
        let (Some(parent_path), Some(container_name)) =
            (container_path.parent(), container_path.file_name())
        else {
            return Err(Fail::Other(format!(
                "{} is no container",
                container_path.display()
            )));
        };
        let parent = open_anchor(parent_path)?;
        match identity_at(&parent, container_name)? {
            None => return self.reach(which, after, None),
            Some(found) if found != expected => {
                self.notes.push(format!(
                    "left {}: it is not the directory this removal made there",
                    container_path.display()
                ));
                return self.reach(which, after, None);
            }
            Some(_) => {}
        }
        let foreign = |container: &OwnedFd| -> Result<Option<PathBuf>, Fail> {
            Ok(names(container)?
                .into_iter()
                .next()
                .map(|first| container_path.join(first)))
        };
        let container = open_directory(&parent, container_name)?;
        if identity_held(&container)? != expected {
            return Err(Fail::ContainerReplaced {
                container: container_path,
            });
        }
        if let Some(first) = foreign(&container)? {
            return Err(Fail::Foreign {
                container: container_path,
                first,
            });
        }
        match rustix::fs::unlinkat(&parent, container_name, AtFlags::REMOVEDIR) {
            // A debug build's fixture may stop here as a crash would: the container gone, its
            // removal not yet recorded (`<which>-rmdir`).
            Ok(()) if after == Step::Done => stop_at(which, "rmdir"),
            Ok(()) => {}
            Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => {
                let first = foreign(&container)?.unwrap_or_else(|| container_path.clone());
                return Err(Fail::Foreign {
                    container: container_path,
                    first,
                });
            }
            Err(errno) => {
                return Err(Fail::Other(format!(
                    "removing the empty directory {}: {}",
                    container_path.display(),
                    std::io::Error::from(errno)
                )));
            }
        }
        self.reach(which, after, None)
    }
}

/// Where the directory `claim` holds is now by its recorded identity, without following a link:
/// its own path, or its name in its recorded container; `None` when it is in neither.
fn found(claim: &Claim) -> Option<PathBuf> {
    let identity = decode(&claim.identity)?;
    let inside = claim
        .container
        .as_ref()
        .zip(claim.path.file_name())
        .map(|(container, name)| container.join(name));
    std::iter::once(claim.path.clone())
        .chain(inside)
        .find(|at| identity_of_path(at) == Some(identity))
}

/// Move the directory `name` of `parent` into `container` under the same name, never over a
/// name that is taken; on a file system without that guarantee, the container this removal just
/// made and holds open is the only place it can land.
fn claim_into(parent: &OwnedFd, name: &OsStr, container: &OwnedFd) -> std::io::Result<()> {
    match rename_free(parent, name, container, name) {
        Err(error)
            if matches!(
                rustix::io::Errno::from_io_error(&error),
                Some(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS)
            ) =>
        {
            Ok(rustix::fs::renameat(parent, name, container, name)?)
        }
        other => other,
    }
}

/// The committed capture of the `HEAD` of the worktree whose administrative entry is `entry`,
/// read through that entry as git's directory, never through the worktree.
fn capture(git: &Git, entry: &Path) -> Result<Snapshot, String> {
    let scratch = tempfile::Builder::new()
        .prefix("af-workspace-remove-")
        .tempdir()
        .map_err(|error| format!("creating a scratch directory: {error}"))?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    Capture::new(&git.at(entry).repo(), &cas)
        .committed("HEAD")
        .map_err(|error| error.to_string())
}

/// Why the claimed worktree may not be deleted, `None` when its index and its files both equal
/// its commit: the index through delivery's filter-free cached-index comparison, the files
/// through descriptors against the committed capture.
fn judge(
    git: &Git,
    entry: &Path,
    held: &Held,
    snapshot: &Snapshot,
    path: &Path,
) -> Result<Option<String>, String> {
    let commit = snapshot.source_revision.as_deref().unwrap_or("HEAD");
    // A staged addition, modification or deletion, or an unmerged entry, is a change too.
    let staged = super::commands::staged_paths(git, Some(entry), commit)?;
    if let Some(staged) = staged.first() {
        return Ok(Some(format!(
            "the index of the worktree {} differs from its commit {commit} at `{}`",
            path.display(),
            staged.display()
        )));
    }
    let differing = first_difference_at(
        &held.container,
        &held.name,
        held.identity,
        &snapshot.manifest,
    )
    .map_err(|error| format!("reading the worktree {}: {error}", path.display()))?;
    Ok(differing.map(|differing| {
        format!(
            "the worktree {} differs from its commit {commit} at `{}`",
            path.display(),
            review_source_git::fs_path(&differing).display()
        )
    }))
}

/// Append `<which>-<event>` to the file a debug build's fixture names.
fn trace(which: Which, event: &str) {
    #[cfg(debug_assertions)]
    if let Some(file) = std::env::var_os(TEST_TRACE)
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
    {
        let _ = writeln!(file, "{}-{event}", which.name());
    }
    #[cfg(not(debug_assertions))]
    let _ = (which, event);
}

/// Stop here, as a crash would, when a debug build's fixture asks for `<which>-<point>`.
fn stop_at(which: Which, point: &str) {
    stop_after(&format!("{}-{point}", which.name()));
}

/// Stop here, as a crash would, when a debug build's fixture asks for `point`: a step of a
/// directory, or `unregistered`, after the Workspace's unregistration and before its completed
/// record is deleted.
pub(super) fn stop_after(point: &str) {
    #[cfg(debug_assertions)]
    if std::env::var(TEST_STOP).is_ok_and(|stop| stop == point) {
        eprintln!("af: stopped after {point} ({TEST_STOP})");
        std::process::exit(86);
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
}
