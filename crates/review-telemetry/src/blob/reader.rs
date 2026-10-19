//! Reading blob files. A reader holds `gc.lock` shared for the read, so no sweep removes a file
//! under it; a reference whose file is nevertheless missing (a hand-deleted file, a copied
//! directory) is reported for that occurrence as `content unavailable: missing blob` and never
//! fails the query.

use std::io::Read as _;

use flate2::read::GzDecoder;

use crate::layout::{self, Layout};
use crate::record::{BlobRef, BlobState};
use crate::trace::sha256_digest;

/// What an occurrence's content is, for a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Bytes(Vec<u8>),
    /// Why there are no bytes: `content unavailable: missing blob`, `… dropped`, `… corrupt`.
    Unavailable(String),
}

/// The reason a reader gives for an occurrence whose blob file is gone.
pub const MISSING: &str = "content unavailable: missing blob";
/// The reason for an occurrence the writer dropped.
pub const DROPPED: &str = "content unavailable: dropped";

/// Whether the blob file of `digest` exists.
pub fn exists(layout: &Layout, digest: &str) -> bool {
    layout.blob(digest).is_file()
}

/// The retained bytes of `digest`, verified against it, read under `gc.lock` shared.
pub fn read(layout: &Layout, digest: &str) -> Content {
    let _shared = layout::shared_within(&layout.gc_lock(), layout::LOCK_WAIT)
        .ok()
        .flatten();
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
