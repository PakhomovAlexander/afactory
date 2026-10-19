//! The collector side of the publication protocol: it takes `gc.lock` exclusively, waiting at
//! most two seconds, and skips blob collection this time if it cannot. A blob is a **root**,
//! never removed, while a sealed segment's `.refs` manifest names it or a `blob_ref` record in
//! the valid prefix of any retained `.jsonl` segment references it, locked or not. Before it
//! collects anything it recovers every unsealed segment whose lock it can take and reads the
//! flushed prefix of every segment it cannot lock, in that order. It reads only that structural
//! metadata (ADR-0151 clause 1).
//!
//! The storage sweep (package P2) runs this after its own decisions; fixtures run it directly.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::layout::{self, Layout};
use crate::segment::reader;

/// What one collection removed, or why it collected nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    pub removed_blobs: Vec<PathBuf>,
    pub removed_temporary: Vec<PathBuf>,
    /// Why blob collection was skipped (the lock was busy).
    pub skipped: Option<String>,
}

fn older_than(path: &Path, age: Duration, now: SystemTime) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|since| since >= age))
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
pub fn collect(layout: &Layout, min_age: Duration, now: SystemTime) -> Result<Collected, String> {
    let mut collected = Collected::default();
    if !layout.versioned().is_dir() {
        return Ok(collected);
    }
    let Some(_exclusive) = layout::exclusive_within(&layout.gc_lock(), layout::LOCK_WAIT)
        .map_err(|error| format!("{}: {error}", layout.gc_lock().display()))?
    else {
        collected.skipped = Some("gc.lock is held by a publisher or reader".into());
        return Ok(collected);
    };
    let roots = roots(layout)?;
    for temporary in reader::list(layout)?.temporary {
        if older_than(&temporary, min_age, now) && std::fs::remove_file(&temporary).is_ok() {
            collected.removed_temporary.push(temporary);
        }
    }
    let Ok(prefixes) = std::fs::read_dir(layout.blobs()) else {
        return Ok(collected);
    };
    for prefix in prefixes.filter_map(Result::ok) {
        let Ok(files) = std::fs::read_dir(prefix.path()) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let path = file.path();
            let name = file.file_name().to_string_lossy().to_string();
            if name.starts_with(layout::TEMP_PREFIX) {
                if older_than(&path, min_age, now) && std::fs::remove_file(&path).is_ok() {
                    collected.removed_temporary.push(path);
                }
                continue;
            }
            let Some(hex) = name.strip_suffix(".gz") else {
                continue;
            };
            if roots.contains(&format!("sha256:{hex}")) || !older_than(&path, min_age, now) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                collected.removed_blobs.push(path);
            }
        }
    }
    collected.removed_blobs.sort();
    collected.removed_temporary.sort();
    Ok(collected)
}
