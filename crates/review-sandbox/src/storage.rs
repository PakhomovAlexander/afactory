//! What the Storage Budget measures on disk: an entry's allocated bytes, its newest write, and
//! the free bytes of the volume that holds it.
//!
//! An entry's size is the allocated bytes (`st_blocks * 512`) of the regular files below it, so a
//! copy-on-write clone or a sparse file counts what it holds, not its apparent length, and a file
//! reached through several hard links counts once. Directories and symbolic links count nothing:
//! a file system that charges a directory its blocks (ext4, 4096 bytes each) and one that does
//! not (APFS) give every total the same number, and a removal's bytes are those of the regular
//! files it deleted (ADR-0158 clause 13). The walk never follows a link.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::SystemTime;

/// Directories deeper than this are not walked; an entry af keeps is nowhere near it.
const MAX_DEPTH: u32 = 256;

/// Allocated bytes of the regular files below `path`, `path` included, without following a link.
/// A missing path holds nothing; an entry that cannot be read is an error, so an unmeasurable entry is never
/// counted as empty.
pub fn allocated_bytes(path: &Path) -> std::io::Result<u64> {
    allocated_bytes_and_depth(path).map(|(bytes, _)| bytes)
}

/// [`allocated_bytes`] and, from the same walk, how deep directories nest below `path`: the
/// level of the deepest directory, `path` itself at level 0 (a file or an empty directory is 0).
/// What a removal walk cannot reach is known before it starts
/// ([`crate::removal::deeper_than_removal_walk`]).
pub fn allocated_bytes_and_depth(path: &Path) -> std::io::Result<(u64, u32)> {
    let mut seen = BTreeSet::new();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => walk(path, &metadata, 0, &mut seen),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
        Err(error) => Err(error),
    }
}

/// What one entry counts: a regular file its allocated bytes, once however many hard links reach
/// it; a directory, a link or anything else nothing.
#[cfg(unix)]
fn allocated(metadata: &std::fs::Metadata, seen: &mut BTreeSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if !metadata.file_type().is_file()
        || (metadata.nlink() > 1 && !seen.insert((metadata.dev(), metadata.ino())))
    {
        return 0;
    }
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(metadata: &std::fs::Metadata, _: &mut BTreeSet<(u64, u64)>) -> u64 {
    if metadata.is_file() {
        metadata.len()
    } else {
        0
    }
}

/// The error [`allocated_bytes_and_depth`] returns for a tree nested deeper than its walk
/// reaches: told apart from every other failure by [`is_nested_too_deep`].
#[derive(Debug)]
struct NestedTooDeep(String);

impl std::fmt::Display for NestedTooDeep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NestedTooDeep {}

/// Whether `error`, from [`allocated_bytes_and_depth`], says the tree is nested deeper than the
/// measurement walk reaches, rather than that something in it could not be read.
pub fn is_nested_too_deep(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<NestedTooDeep>().is_some())
}

/// The allocated bytes of the regular files below `path` and the level of its deepest
/// directory, `path` at `depth`.
fn walk(
    path: &Path,
    metadata: &std::fs::Metadata,
    depth: u32,
    seen: &mut BTreeSet<(u64, u64)>,
) -> std::io::Result<(u64, u32)> {
    let mut total = allocated(metadata, seen);
    if !metadata.is_dir() {
        return Ok((total, 0));
    }
    let mut deepest = depth;
    if depth > MAX_DEPTH {
        return Err(std::io::Error::other(NestedTooDeep(format!(
            "{} is nested deeper than {MAX_DEPTH} directories",
            path.display()
        ))));
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let metadata = match std::fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            // Removed between the listing and the stat: it holds nothing now.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let (bytes, below) = walk(&child, &metadata, depth + 1, seen)?;
        total = total.saturating_add(bytes);
        deepest = deepest.max(below);
    }
    Ok((total, deepest))
}

/// The newest modification time below `path`, `path` included, without following a link.
/// `None` when nothing can be read.
pub fn newest_write(path: &Path) -> Option<SystemTime> {
    fn visit(path: &Path, depth: u32, newest: &mut Option<SystemTime>) {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if let Ok(modified) = metadata.modified() {
            *newest = Some(newest.map_or(modified, |known| known.max(modified)));
        }
        if !metadata.is_dir() || depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            visit(&entry.path(), depth + 1, newest);
        }
    }
    let mut newest = None;
    visit(path, 0, &mut newest);
    newest
}

/// The bytes an unprivileged process may still write on the volume that holds `path`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> std::io::Result<u64> {
    let stat = nix::sys::statvfs::statvfs(path).map_err(std::io::Error::from)?;
    #[allow(clippy::useless_conversion)]
    let available = u64::from(stat.blocks_available());
    #[allow(clippy::useless_conversion)]
    let fragment = u64::from(stat.fragment_size());
    Ok(available.saturating_mul(fragment))
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_bytes_count_a_hard_link_once_and_never_follow_a_link() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big"), vec![7_u8; 256 * 1024]).unwrap();
        let entry = root.path().join("entry");
        std::fs::create_dir(&entry).unwrap();
        std::fs::write(entry.join("data"), vec![1_u8; 64 * 1024]).unwrap();
        let alone = allocated_bytes(&entry).unwrap();
        assert!(alone >= 64 * 1024, "{alone}");
        std::fs::hard_link(entry.join("data"), entry.join("again")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), entry.join("link")).unwrap();
        let linked = allocated_bytes(&entry).unwrap();
        assert!(linked < alone + 64 * 1024, "{alone} then {linked}");
        assert_eq!(allocated_bytes(&root.path().join("absent")).unwrap(), 0);
    }

    /// Only regular files are counted: an empty directory, a tree of them and a link hold
    /// nothing, whatever blocks the file system charges a directory, and a file below them holds
    /// exactly its own blocks.
    #[test]
    fn only_regular_files_are_counted() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join("entry");
        std::fs::create_dir_all(entry.join("a/b/c")).unwrap();
        assert_eq!(
            allocated_bytes(&entry).unwrap(),
            0,
            "directories hold nothing"
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", entry.join("link")).unwrap();
        assert_eq!(allocated_bytes(&entry).unwrap(), 0, "nor does a link");
        std::fs::write(entry.join("a/b/c/file"), b"seven b").unwrap();
        let file = std::fs::metadata(entry.join("a/b/c/file"))
            .unwrap()
            .blocks()
            * 512;
        assert_eq!(allocated_bytes(&entry).unwrap(), file);
        assert_eq!(allocated_bytes(&entry.join("a/b/c/file")).unwrap(), file);
    }

    #[test]
    fn the_newest_write_is_found_below_the_entry() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/file"), b"x").unwrap();
        let newest = newest_write(root.path()).unwrap();
        let file = std::fs::metadata(root.path().join("a/b/file"))
            .unwrap()
            .modified()
            .unwrap();
        assert!(newest >= file);
        assert!(newest_write(&root.path().join("absent")).is_none());
    }

    #[test]
    fn a_tree_deeper_than_the_walk_is_told_apart_from_an_unreadable_one() {
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().join("deep");
        for _ in 0..=MAX_DEPTH + 1 {
            deep = deep.join("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        let error = allocated_bytes_and_depth(&root.path().join("deep")).unwrap_err();
        assert!(is_nested_too_deep(&error), "{error}");
        assert!(!is_nested_too_deep(&std::io::Error::other(
            "permission denied"
        )));
    }

    #[test]
    fn the_free_bytes_of_a_real_volume_are_known() {
        let root = tempfile::tempdir().unwrap();
        assert!(free_bytes(root.path()).unwrap() > 0);
    }
}
