//! What the Storage Budget measures on disk: an entry's allocated bytes, its newest write, and
//! the free bytes of the volume that holds it.
//!
//! An entry's size is its allocated bytes (`st_blocks * 512`), so a copy-on-write clone or a
//! sparse file counts what it holds, not its apparent length; a file reached through several
//! hard links counts once. The walk never follows a link: a link counts as itself.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::SystemTime;

/// Directories deeper than this are not walked; an entry af keeps is nowhere near it.
const MAX_DEPTH: u32 = 256;

/// Allocated bytes below `path`, `path` included, without following a link. A missing path
/// holds nothing; an entry that cannot be read is an error, so an unmeasurable entry is never
/// counted as empty.
pub fn allocated_bytes(path: &Path) -> std::io::Result<u64> {
    let mut seen = BTreeSet::new();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => walk(path, &metadata, 0, &mut seen),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn allocated(metadata: &std::fs::Metadata, seen: &mut BTreeSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if metadata.nlink() > 1 && !metadata.is_dir() && !seen.insert((metadata.dev(), metadata.ino()))
    {
        return 0;
    }
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(metadata: &std::fs::Metadata, _: &mut BTreeSet<(u64, u64)>) -> u64 {
    metadata.len()
}

fn walk(
    path: &Path,
    metadata: &std::fs::Metadata,
    depth: u32,
    seen: &mut BTreeSet<(u64, u64)>,
) -> std::io::Result<u64> {
    let mut total = allocated(metadata, seen);
    if !metadata.is_dir() {
        return Ok(total);
    }
    if depth > MAX_DEPTH {
        return Err(std::io::Error::other(format!(
            "{} is nested deeper than {MAX_DEPTH} directories",
            path.display()
        )));
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
        total = total.saturating_add(walk(&child, &metadata, depth + 1, seen)?);
    }
    Ok(total)
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
    fn the_free_bytes_of_a_real_volume_are_known() {
        let root = tempfile::tempdir().unwrap();
        assert!(free_bytes(root.path()).unwrap() > 0);
    }
}
