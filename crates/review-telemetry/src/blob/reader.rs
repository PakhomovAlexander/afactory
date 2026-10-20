//! Reading blob files. A blob file is opened only while `gc.lock` is held shared, so no sweep
//! removes a file under the read; when the lock is not had within [`layout::LOCK_WAIT`] (a
//! collection holds it) the occurrence is reported as `content unavailable: collection in
//! progress` and the file is never opened. A reference whose file is nevertheless missing (a
//! hand-deleted file, a copied directory) is reported for that occurrence as `content
//! unavailable: missing blob` and never fails the query. The root is opened once per read; the
//! lock and every directory beneath it are reached relative to that descriptor without following
//! a link, so a linked prefix directory reads as a missing blob.

use std::io::Read as _;

use flate2::read::GzDecoder;
use nix::fcntl::FlockArg;

use crate::layout::{self, Confined, EntryKind, Layout};
use crate::record::{BlobRef, BlobState};
use crate::trace::sha256_digest;

/// What an occurrence's content is, for a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Bytes(Vec<u8>),
    /// Why there are no bytes: `content unavailable: missing blob`, `… dropped`, `… corrupt`,
    /// `… collection in progress`.
    Unavailable(String),
}

/// The reason a reader gives for an occurrence whose blob file is gone.
pub const MISSING: &str = "content unavailable: missing blob";
/// The reason for an occurrence the writer dropped.
pub const DROPPED: &str = "content unavailable: dropped";
/// The reason for an occurrence read while a collection holds `gc.lock`.
pub const COLLECTING: &str = "content unavailable: collection in progress";

/// The prefix directory `blobs/<aa>/` of `digest` in the opened `blobs/` and the object's file
/// name in it, the prefix directory opened without following a link: a linked prefix directory
/// is no blob directory, and its objects are missing.
fn object_in(blobs: &Confined, digest: &str) -> std::io::Result<Option<(Confined, String)>> {
    let Some(hex) = digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && crate::redact::is_lower_hex(hex))
    else {
        return Ok(None);
    };
    Ok(blobs
        .child_dir(&hex[..2])?
        .map(|prefix| (prefix, format!("{hex}.gz"))))
}

/// Whether the blob file of `digest` exists in the opened `blobs/` directory: a regular file in
/// a real prefix directory, judged relative to it without following a link.
pub fn exists_in(blobs: &Confined, digest: &str) -> bool {
    object_in(blobs, digest)
        .ok()
        .flatten()
        .is_some_and(|(prefix, name)| prefix.kind(&name) == Some(EntryKind::File))
}

/// Whether the blob file of `digest` exists beneath the root: the root is opened once, and
/// everything beneath it judged relative to it ([`exists_in`]).
pub fn exists(layout: &Layout, digest: &str) -> bool {
    layout
        .open_versioned()
        .and_then(|versioned| versioned.child_dir("blobs").ok().flatten())
        .is_some_and(|blobs| exists_in(&blobs, digest))
}

/// The retained bytes of `digest`, verified against it, read under `gc.lock` shared; the root
/// is opened once and the lock and the file are reached through it ([`read_in`]).
pub fn read(layout: &Layout, digest: &str) -> Content {
    match layout.open_versioned() {
        Some(versioned) => read_in(&versioned, digest),
        None => Content::Unavailable(MISSING.into()),
    }
}

/// [`read`] through the held `v1/` `versioned`: `gc.lock` is taken and the blob file opened
/// relative to it, never through a pathname. Without the lock the file is not opened at all.
pub fn read_in(versioned: &Confined, digest: &str) -> Content {
    let _shared = match layout::lock_in(
        versioned,
        layout::GC_LOCK,
        layout::LOCK_WAIT,
        FlockArg::LockSharedNonblock,
    ) {
        Ok(Some(held)) => held,
        Ok(None) => return Content::Unavailable(COLLECTING.into()),
        Err(error) => {
            return Content::Unavailable(format!("content unavailable: gc.lock: {error}"));
        }
    };
    // Never through a link: a blob file is read only where the layout put it.
    let opened = versioned
        .child_dir("blobs")
        .and_then(|blobs| match blobs {
            Some(blobs) => object_in(&blobs, digest),
            None => Ok(None),
        })
        .and_then(|found| match found {
            Some((prefix, name)) => prefix.open_file(&name),
            None => Err(std::io::ErrorKind::NotFound.into()),
        });
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Content::Unavailable(MISSING.into());
        }
        Err(error) => return Content::Unavailable(format!("content unavailable: {error}")),
    };
    // Decompressed only up to the longest blob this release retains: a larger stream is
    // corrupt, refused before it is held.
    let mut bytes = Vec::new();
    let limit = crate::level::CONTENT_MAX_BLOB_BYTES_BOUND;
    if GzDecoder::new(file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > limit
        || sha256_digest(&bytes) != digest
    {
        return Content::Unavailable("content unavailable: corrupt blob".into());
    }
    Content::Bytes(bytes)
}

/// The content of one occurrence.
pub fn content(layout: &Layout, occurrence: &BlobRef) -> Content {
    match (&occurrence.state, &occurrence.digest) {
        (BlobState::Stored, Some(digest)) => read(layout, digest),
        _ => Content::Unavailable(DROPPED.into()),
    }
}
