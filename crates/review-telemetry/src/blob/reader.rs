//! Reading blob files. A blob file is opened only while `gc.lock` is held shared, so no sweep
//! removes a file under the read; when the lock is not had within [`layout::LOCK_WAIT`] (a
//! collection holds it) the occurrence is reported as `content unavailable: collection in
//! progress` and the file is never opened. A reference whose file is nevertheless missing (a
//! hand-deleted file, a copied directory) is reported for that occurrence as `content
//! unavailable: missing blob` and never fails the query.

use std::io::Read as _;

use flate2::read::GzDecoder;

use crate::layout::{self, Layout};
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

/// Whether the blob file of `digest` exists.
pub fn exists(layout: &Layout, digest: &str) -> bool {
    layout.blob(digest).is_file()
}

/// The retained bytes of `digest`, verified against it, read under `gc.lock` shared. Without
/// the lock the file is not opened at all.
pub fn read(layout: &Layout, digest: &str) -> Content {
    let _shared = match layout::shared_within(&layout.gc_lock(), layout::LOCK_WAIT) {
        Ok(Some(held)) => held,
        Ok(None) => return Content::Unavailable(COLLECTING.into()),
        Err(error) => {
            return Content::Unavailable(format!("content unavailable: gc.lock: {error}"));
        }
    };
    let path = layout.blob(digest);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Content::Unavailable(MISSING.into());
        }
        Err(error) => return Content::Unavailable(format!("content unavailable: {error}")),
    };
    let mut bytes = Vec::new();
    if GzDecoder::new(file).read_to_end(&mut bytes).is_err() || sha256_digest(&bytes) != digest {
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
