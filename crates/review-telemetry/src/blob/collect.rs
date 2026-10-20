//! The collector side of the publication protocol: it takes `gc.lock` exclusively, waiting at
//! most two seconds, and skips blob collection this time if it cannot. A blob is a **root**,
//! never removed, while a sealed segment's `.refs` manifest names it or a `blob_ref` record in
//! the valid prefix of any retained `.jsonl` segment references it, locked or not. Before it
//! collects anything it recovers every unsealed segment whose lock it can take and reads the
//! flushed prefix of every segment it cannot lock, in that order. It reads only that structural
//! metadata (ADR-0153 clause 1).
//!
//! The telemetry root is opened once; the lock, the segments and the blobs are reached through
//! that descriptor ([`collect_in`]), never through a pathname again.
//!
//! The storage sweep (package P2) runs this after its own decisions; fixtures run it directly.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use nix::fcntl::FlockArg;

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
    match layout.open_versioned() {
        Some(versioned) => roots_in(&versioned),
        None => Ok(BTreeSet::new()),
    }
}

/// [`roots`] through the held `v1/` `versioned`.
pub fn roots_in(versioned: &Confined) -> Result<BTreeSet<String>, String> {
    let failed =
        |error: std::io::Error| format!("{}: {error}", versioned.join("segments").display());
    let mut roots = BTreeSet::new();
    let Some(segments) = versioned.child_dir("segments").map_err(failed)? else {
        return Ok(roots);
    };
    reader::recover_all_in(&segments)?;
    let listing = reader::list_in(&segments)?;
    for id in &listing.sealed {
        roots.extend(reader::manifest_digests_in(&segments, id)?);
    }
    for id in listing.live.iter().chain(&listing.unsealed) {
        let bytes = reader::read_live_in(&segments, id).map_err(|error| {
            format!(
                "{}: {error}",
                segments.join(&format!("{id}{}", layout::LIVE)).display()
            )
        })?;
        roots.extend(reader::referenced(&reader::scan(&bytes).records));
    }
    Ok(roots)
}

/// Remove unreferenced blobs and temporary files older than `min_age` (one hour in the sweep),
/// under `gc.lock` held exclusively. The root is opened once ([`collect_in`]).
pub fn collect(layout: &Layout, min_age: Duration, now: SystemTime) -> Result<Collected, String> {
    match layout.open_versioned() {
        Some(versioned) => collect_in(&versioned, min_age, now),
        None => Ok(Collected::default()),
    }
}

/// [`collect`] through the held `v1/` `versioned`.
///
/// Collection never leaves the telemetry root. It acts only on paths that match the layout's
/// grammar exactly: in `segments/` a regular file with an atomic write's temporary name; in
/// `blobs/` a prefix directory named by two lowercase hex digits that is a real directory (a
/// link or any other entry is never entered) and in it a regular file named by 64 lowercase hex
/// digits starting with the prefix plus `.gz`, or a publisher's temporary name. Every judgement
/// and removal is made relative to the opened directory without following a link, so a prefix
/// linked to a directory outside the root removes nothing there, and a stray-named or linked
/// file inside is never removed (`status` reports it as unrecognized).
pub fn collect_in(
    versioned: &Confined,
    min_age: Duration,
    now: SystemTime,
) -> Result<Collected, String> {
    let mut collected = Collected::default();
    let Some(_exclusive) = layout::lock_in(
        versioned,
        layout::GC_LOCK,
        layout::LOCK_WAIT,
        FlockArg::LockExclusiveNonblock,
    )
    .map_err(|error| format!("{}: {error}", versioned.join(layout::GC_LOCK).display()))?
    else {
        collected.skipped = Some("gc.lock is held by a publisher or reader".into());
        return Ok(collected);
    };
    let roots = roots_in(versioned)?;
    let failed =
        |path: PathBuf| move |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(segments) = versioned
        .child_dir("segments")
        .map_err(failed(versioned.join("segments")))?
    {
        for name in segments.names().map_err(failed(segments.path().into()))? {
            if layout::is_temp_name(&name, layout::WRITE_TEMP_LABEL)
                && segments.file_older_than(&name, min_age, now)
                && segments.remove_file(&name).is_ok()
            {
                collected.removed_temporary.push(segments.join(&name));
            }
        }
    }
    let Some(blobs) = versioned
        .child_dir("blobs")
        .map_err(failed(versioned.join("blobs")))?
    else {
        return Ok(collected);
    };
    for prefix_name in blobs.names().map_err(failed(blobs.path().into()))? {
        if !layout::is_blob_prefix(&prefix_name) {
            continue;
        }
        let Ok(Some(prefix)) = blobs.child_dir(&prefix_name) else {
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
