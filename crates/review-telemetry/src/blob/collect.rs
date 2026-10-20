//! The collector side of the publication protocol: it takes `gc.lock` exclusively, waiting at
//! most two seconds, and skips blob collection this time if it cannot. A blob is a **root**,
//! never removed, while a sealed segment's `.refs` manifest names it or a `blob_ref` record in
//! the valid prefix of any retained `.jsonl` segment references it, locked or not. Before it
//! collects anything it recovers every unsealed segment whose lock it can take and reads the
//! flushed prefix of every segment it cannot lock, in that order. It reads only that structural
//! metadata (ADR-0153 clause 1).
//!
//! The storage sweep (package P2) runs this after its own decisions; fixtures run it directly.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::layout::{self, Confined, Layout};
use crate::segment::reader;

/// What one collection removed, or why it collected nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    pub removed_blobs: Vec<PathBuf>,
    pub removed_temporary: Vec<PathBuf>,
    /// Why blob collection was skipped (the lock was busy).
    pub skipped: Option<String>,
}

/// Every digest a retained segment references: sealed manifests, and the valid prefix of every
/// `.jsonl` segment, locked or not. Recovers the unsealed segments it can lock first.
pub fn roots(layout: &Layout) -> Result<BTreeSet<String>, String> {
    reader::recover_all(layout)?;
    let listing = reader::list(layout)?;
    let mut roots = BTreeSet::new();
    for id in &listing.sealed {
        roots.extend(reader::manifest_digests(layout, id)?);
    }
    for id in listing.live.iter().chain(&listing.unsealed) {
        let prefix = reader::read_live(&layout.live_segment(id))?;
        roots.extend(reader::referenced(&prefix.records));
    }
    Ok(roots)
}

/// Remove unreferenced blobs and temporary files older than `min_age` (one hour in the sweep),
/// under `gc.lock` held exclusively.
///
/// Collection never leaves the telemetry root. It acts only on paths that match the layout's
/// grammar exactly: in `segments/` a regular file with an atomic write's temporary name; in
/// `blobs/` a prefix directory named by two lowercase hex digits that is a real directory (a
/// link or any other entry is never entered) and in it a regular file named by 64 lowercase hex
/// digits starting with the prefix plus `.gz`, or a publisher's temporary name. Every judgement
/// and removal is made relative to the opened directory without following a link, so a prefix
/// linked to a directory outside the root removes nothing there, and a stray-named or linked
/// file inside is never removed (`status` reports it as unrecognized).
pub fn collect(layout: &Layout, min_age: Duration, now: SystemTime) -> Result<Collected, String> {
    let mut collected = Collected::default();
    if !layout.has_versioned() {
        return Ok(collected);
    }
    let Some(_exclusive) = layout::exclusive_within(&layout.gc_lock(), layout::LOCK_WAIT)
        .map_err(|error| format!("{}: {error}", layout.gc_lock().display()))?
    else {
        collected.skipped = Some("gc.lock is held by a publisher or reader".into());
        return Ok(collected);
    };
    let roots = roots(layout)?;
    let failed =
        |path: PathBuf| move |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(mut segments) =
        Confined::versioned(layout, "segments").map_err(failed(layout.segments()))?
    {
        for name in segments.names().map_err(failed(layout.segments()))? {
            if layout::is_temp_name(&name, layout::WRITE_TEMP_LABEL)
                && segments.file_older_than(&name, min_age, now)
                && segments.remove_file(&name).is_ok()
            {
                collected.removed_temporary.push(segments.join(&name));
            }
        }
    }
    let Some(mut blobs) = Confined::versioned(layout, "blobs").map_err(failed(layout.blobs()))?
    else {
        return Ok(collected);
    };
    for prefix_name in blobs.names().map_err(failed(layout.blobs()))? {
        if !layout::is_blob_prefix(&prefix_name) {
            continue;
        }
        let Ok(Some(mut prefix)) = blobs.child_dir(&prefix_name) else {
            continue;
        };
        let Ok(names) = prefix.names() else {
            continue;
        };
        for name in names {
            if layout::is_temp_name(&name, layout::BLOB_TEMP_LABEL) {
                if prefix.file_older_than(&name, min_age, now) && prefix.remove_file(&name).is_ok()
                {
                    collected.removed_temporary.push(prefix.join(&name));
                }
                continue;
            }
            let Some(digest) = layout::blob_object_digest(&prefix_name, &name) else {
                continue;
            };
            if roots.contains(&digest) || !prefix.file_older_than(&name, min_age, now) {
                continue;
            }
            if prefix.remove_file(&name).is_ok() {
                collected.removed_blobs.push(prefix.join(&name));
            }
        }
    }
    collected.removed_blobs.sort();
    collected.removed_temporary.sort();
    Ok(collected)
}
