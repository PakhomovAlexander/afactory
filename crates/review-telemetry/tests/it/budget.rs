//! The telemetry kind of the Storage Budget (design §6 "Budget", ADR-0153 clause 8): the
//! physical union counted once, marginal bytes, eviction and every sweep's reclamation.

use std::path::Path;
use std::time::{Duration, SystemTime};

use review_telemetry::blob::Media;
use review_telemetry::budget::{self, Directory, State};
use review_telemetry::index::Index;
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::writer::Writer;

use crate::{options, writer};

/// The allocated bytes of `root` and everything beneath it.
fn allocated(root: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let own = std::fs::symlink_metadata(root).unwrap();
    let mut total = own.blocks() * 512;
    if !own.is_dir() {
        return total;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
        {
            let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
            total += metadata.blocks() * 512;
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// Every file beneath `root`, last modified `age` ago.
fn age(root: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .or_else(|_| std::fs::File::open(&path))
                    .unwrap()
                    .set_modified(when)
                    .unwrap();
            }
        }
    }
}

/// One sealed segment at `content` naming `shared` (identical bytes in every segment) and
/// `own` content; returns its segment id.
fn sealed_with(root: &Path, shared: &[u8], own: &[u8]) -> String {
    let writer = writer(root, Level::Content);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(
        &span,
        "gen_ai.content.input",
        Attrs::new(),
        shared,
        Media::Text,
    );
    writer.event_with_content(
        &span,
        "gen_ai.content.output",
        Attrs::new(),
        own,
        Media::Text,
    );
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap().unwrap().segment_id
}

fn shared_bytes() -> Vec<u8> {
    // Large enough that its blob takes blocks of its own.
    (0..20_000)
        .map(|line| format!("shared line {line} of the composed Worker input\n"))
        .collect::<String>()
        .into_bytes()
}

/// Every byte under the directory is counted once: a blob two segments share belongs to
/// neither segment's entry until one of them is gone, and an index larger than its segments is
/// in the `shared` entry; the entries add up to the physical total.
#[test]
fn telemetry_budget_counts_physical_bytes_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let shared = shared_bytes();
    let first = sealed_with(&root, &shared, b"first's own output\n");
    let second = sealed_with(&root, &shared, b"second's own output\n");
    // An index larger than its segments.
    let mut index = Index::open(&Layout::new(&root)).unwrap();
    index.ingest().unwrap();
    drop(index);

    let directory = Directory::measure(&root, false).unwrap().unwrap();
    assert_eq!(
        directory.total,
        allocated(&root),
        "the physical union, once"
    );
    let entries = directory.entries();
    assert_eq!(
        entries.iter().map(|entry| entry.bytes).sum::<u64>(),
        directory.total
    );
    let layout = Layout::new(&root);
    let index_bytes: u64 = layout
        .index_files()
        .iter()
        .filter_map(|path| std::fs::symlink_metadata(path).ok())
        .map(|metadata| std::os::unix::fs::MetadataExt::blocks(&metadata) * 512)
        .sum();
    let segment_bytes: u64 = [&first, &second]
        .iter()
        .map(|id| directory.segments[*id].own_bytes)
        .sum();
    assert!(
        index_bytes > segment_bytes,
        "{index_bytes} > {segment_bytes}"
    );
    let rest = entries
        .iter()
        .find(|entry| entry.state == "shared")
        .unwrap();
    assert!(rest.bytes >= index_bytes);
    // The shared blob is in neither segment's bytes; each one's own blob is in its own.
    let blob = directory
        .segments
        .get(&first)
        .unwrap()
        .refs
        .intersection(&directory.segments[&second].refs)
        .next()
        .cloned()
        .unwrap();
    let blob_bytes = directory.blobs[&blob];
    assert!(blob_bytes > 0);
    for id in [&first, &second] {
        let entry = entries
            .iter()
            .find(|entry| entry.segment_id.as_deref() == Some(id.as_str()))
            .unwrap();
        assert_eq!(entry.bytes, directory.marginal(id));
        assert!(entry.bytes < directory.segments[id].own_bytes + blob_bytes);
        assert_eq!(entry.state, "sealed");
        assert!(entry.in_use.is_none());
    }
}

/// An evicted segment frees exactly its marginal bytes, measured as allocated bytes: the first
/// of two segments sharing a blob frees its own files and its own blob; the second, the shared
/// blob with its own.
#[test]
fn telemetry_eviction_frees_exactly_the_marginal_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let shared = shared_bytes();
    let first = sealed_with(&root, &shared, b"first's own output\n");
    let second = sealed_with(&root, &shared, b"second's own output\n");
    age(&root, Duration::from_secs(2 * 3_600));
    // Once the hour that protects a just-sealed segment (its seal time) has passed.
    let now = SystemTime::now() + Duration::from_secs(2 * 3_600);
    for id in [&first, &second] {
        let directory = Directory::measure(&root, false).unwrap().unwrap();
        let marginal = directory.marginal(id);
        let before = allocated(&root);
        let target = directory.target(id).unwrap();
        assert_eq!(target.state, State::Sealed);
        assert!(matches!(
            budget::evict(&root, &target, now).unwrap(),
            budget::Evicted::Removed(_)
        ));
        budget::reclaim(&root, std::slice::from_ref(id), now).unwrap();
        assert_eq!(before - allocated(&root), marginal, "segment {id}");
    }
    assert!(
        Directory::measure(&root, false)
            .unwrap()
            .unwrap()
            .blobs
            .is_empty()
    );
}

/// A writer that crashed after flushing a blob reference leaves a `.jsonl` segment; with every
/// file older than an hour, a sweep before any reader opened the directory keeps the blob (the
/// reference in the flushed prefix is a root) and recovers the segment.
#[test]
fn telemetry_unsealed_references_are_roots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let writer = Writer::new(options(&root, Level::Content)).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(
        &span,
        "gen_ai.content.input",
        Attrs::new(),
        b"the composed input\n",
        Media::Text,
    );
    let segment = writer.segment_id().unwrap();
    writer.flush();
    // Crashed after the reference flush: no seal, the lock gone with the process.
    writer.abandon();
    age(&root, Duration::from_secs(2 * 3_600));
    let layout = Layout::new(&root);
    assert!(!layout.index().exists(), "no reader opened it");
    let blobs_before = Directory::measure(&root, false).unwrap().unwrap().blobs;
    assert_eq!(blobs_before.len(), 1);
    let reclaimed = budget::reclaim(&root, &[], SystemTime::now()).unwrap();
    assert_eq!(reclaimed.blob_bytes, 0, "{reclaimed:?}");
    let after = Directory::measure(&root, false).unwrap().unwrap();
    assert_eq!(
        after.blobs.keys().collect::<Vec<_>>(),
        blobs_before.keys().collect::<Vec<_>>()
    );
    for digest in blobs_before.keys() {
        assert!(layout.blob(digest).exists(), "{digest}");
    }
    assert_eq!(after.segments[&segment].state, State::Sealed, "recovered");
    assert!(!layout.live_segment(&segment).exists());
    let records =
        review_telemetry::segment::reader::read_sealed(&layout.sealed_segment(&segment)).unwrap();
    assert!(matches!(
        records.last(),
        Some(review_telemetry::record::Record::Seal(seal))
            if seal.exit == review_telemetry::record::SealExit::Recovered
    ));
}

/// Old corrupt segments, orphan temporaries and an index of free pages: the corrupt segments
/// are entries the sweep may evict, and every sweep reclaims the temporaries and vacuums the
/// index, each measured as allocated bytes.
#[test]
fn telemetry_reclaim_covers_temporaries_orphans_and_free_index_pages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let layout = Layout::new(&root);
    // An index with more than 16 MiB of free pages, a quarter of its file and more.
    {
        let mut index = Index::open(&layout).unwrap();
        index.ingest().unwrap();
        let connection = index.connection();
        connection
            .execute_batch("CREATE TABLE ballast(payload BLOB);")
            .unwrap();
        for _ in 0..24 {
            connection
                .execute("INSERT INTO ballast VALUES (zeroblob(1048576))", [])
                .unwrap();
        }
        connection.execute_batch("DROP TABLE ballast;").unwrap();
        let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    std::fs::create_dir_all(layout.segments()).unwrap();
    let temporary = layout.segments().join(".tmp-write-1-0a1b2c3d");
    std::fs::write(&temporary, vec![7_u8; 64 * 1024]).unwrap();
    std::fs::create_dir_all(layout.blobs().join("ab")).unwrap();
    let orphan = layout
        .blobs()
        .join("ab")
        .join(format!("{}.gz", "ab".repeat(32)));
    std::fs::write(&orphan, vec![1_u8; 32 * 1024]).unwrap();
    let corrupt = layout.corrupt_segment("1700000000000-1-0a1b2c3d");
    std::fs::write(&corrupt, vec![b'#'; 16 * 1024]).unwrap();
    age(&root, Duration::from_secs(48 * 3_600));
    let directory = Directory::measure(&root, false).unwrap().unwrap();
    let entry = directory
        .entries()
        .into_iter()
        .find(|entry| entry.state == "corrupt")
        .unwrap();
    assert!(entry.in_use.is_none());
    assert_eq!(entry.bytes, allocated(&corrupt));
    let temporary_bytes = allocated(&temporary);
    let orphan_bytes = allocated(&orphan);
    let index_before: u64 = layout
        .index_files()
        .iter()
        .filter(|path| path.exists())
        .map(|path| allocated(path))
        .sum();
    let reclaimed = budget::reclaim(&root, &[], SystemTime::now()).unwrap();
    assert_eq!(reclaimed.temporary_bytes, temporary_bytes);
    assert_eq!(reclaimed.blob_bytes, orphan_bytes);
    assert_eq!(reclaimed.index, "vacuumed");
    let index_after: u64 = layout
        .index_files()
        .iter()
        .filter(|path| path.exists())
        .map(|path| allocated(path))
        .sum();
    assert_eq!(reclaimed.index_bytes, index_before - index_after);
    assert!(reclaimed.index_bytes > 16 * 1024 * 1024, "{reclaimed:?}");
    assert!(!temporary.exists() && !orphan.exists());
    assert!(
        corrupt.exists(),
        "a corrupt segment goes by eviction, never by reclamation"
    );
}
