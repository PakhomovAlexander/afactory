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

/// Design §3.1, ADR-0153: storage housekeeping reads structural metadata only. A sealed
/// segment whose span and event records are deliberately unparsable, while its header, its seal
/// and its manifest are valid, is measured as sealed with its seal time and its manifest's
/// references, with no problem — the inventory never decodes a record between the header and
/// the seal — while the full record reader refuses it.
#[test]
fn telemetry_housekeeping_never_parses_spans() {
    use std::io::Write as _;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let id = sealed_with(&root, b"a shared input\n", b"its own output\n");
    let layout = Layout::new(&root);
    let path = layout.sealed_segment(&id);
    let text = String::from_utf8(crate::sealed_bytes(&path)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let seal: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(seal["t"], "seal");
    let ended_ms = seal["ended_unix_ns"]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap()
        / 1_000_000;
    // Every record between the header and the seal becomes a line no reader can parse, of the
    // same kinds it was.
    let mut broken = vec![lines[0].to_string()];
    let mut spans = 0;
    for line in &lines[1..lines.len() - 1] {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let kind = value["t"].as_str().unwrap().to_string();
        spans += usize::from(kind.starts_with("span") || kind == "event");
        broken.push(format!(
            r#"{{"seq":{},"t":"{kind}","attributes":{{"unterminated"#,
            value["seq"]
        ));
    }
    assert!(spans >= 3, "{text}");
    broken.push(lines.last().unwrap().to_string());
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(format!("{}\n", broken.join("\n")).as_bytes())
        .unwrap();
    std::fs::write(&path, encoder.finish().unwrap()).unwrap();
    let manifest = std::fs::read_to_string(layout.refs(&id)).unwrap();
    assert_eq!(manifest.lines().count(), 2, "the manifest names both blobs");

    let directory = Directory::measure(&root, false).unwrap().unwrap();
    assert!(directory.problems.is_empty(), "{:?}", directory.problems);
    let segment = &directory.segments[&id];
    assert_eq!(segment.state, State::Sealed);
    assert_eq!(
        u128::from(segment.last_use_unix_ms),
        ended_ms,
        "the seal time"
    );
    assert_eq!(
        segment.refs,
        manifest.lines().map(str::to_string).collect(),
        "the manifest's references"
    );
    assert!(directory.target(&id).is_some(), "evictable as sealed");
    assert!(
        review_telemetry::segment::reader::read_sealed(&path).is_err(),
        "the full record reader refuses it"
    );

    // A live segment, its writer holding the lock: every span and event payload becomes
    // unparsable behind a valid envelope, while its header, blob and blob_ref records stay
    // valid. The inventory measures it with its references, the sweep keeps its blobs as roots,
    // and only the full reader refuses it.
    let writer = writer(&root, Level::Content);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(
        &span,
        "gen_ai.content.input",
        Attrs::new(),
        b"a live input\n",
        Media::Text,
    );
    writer.event(&span, "af.note", Attrs::new().text("af.text", "a note"));
    writer.flush();
    let live_id = writer.segment_id().unwrap();
    let live = layout.live_segment(&live_id);
    let text = std::fs::read_to_string(&live).unwrap();
    let mut referenced = std::collections::BTreeSet::new();
    let mut spans = 0;
    let rewritten: String = text
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let kind = value["t"].as_str().unwrap();
            if kind == "blob_ref" {
                referenced.insert(value["digest"].as_str().unwrap().to_string());
            }
            if kind.starts_with("span") || kind == "event" {
                spans += 1;
                format!(
                    r#"{{"t":"{kind}","seq":{},"schema":"{}","attributes":{{"unterminated"#,
                    value["seq"],
                    value["schema"].as_str().unwrap()
                ) + "\n"
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    assert!(spans >= 3 && referenced.len() == 1, "{text}");
    std::fs::write(&live, &rewritten).unwrap();
    age(&root, Duration::from_secs(2 * 3_600));

    for recover in [false, true] {
        let directory = Directory::measure(&root, recover).unwrap().unwrap();
        assert!(directory.problems.is_empty(), "{:?}", directory.problems);
        let segment = &directory.segments[&live_id];
        assert_eq!(segment.state, State::Live);
        assert_eq!(
            segment.refs, referenced,
            "its blob_ref records, read structurally"
        );
        assert!(directory.target(&live_id).is_none(), "in use");
    }
    let reclaimed = budget::reclaim(&root, &[], SystemTime::now()).unwrap();
    assert!(reclaimed.notes.is_empty(), "{reclaimed:?}");
    for digest in &referenced {
        assert!(layout.blob(digest).exists(), "a root: {digest}");
    }
    assert_eq!(
        std::fs::read_to_string(&live).unwrap(),
        rewritten,
        "untouched"
    );
    let structure = review_telemetry::segment::reader::structure(rewritten.as_bytes());
    assert_eq!(structure.kept_bytes, rewritten.len(), "{structure:?}");
    let full = review_telemetry::segment::reader::read_live(&live).unwrap();
    assert!(
        full.invalid.is_some() && full.interior_corruption,
        "the full record reader refuses it: {full:?}"
    );
    assert!(
        review_telemetry::segment::reader::referenced(&full.records).is_empty(),
        "the full reader never reaches its references"
    );
    drop(writer);
}

/// ADR-0144, design §6: an eviction entry is removed whole or left whole. A deterministic hook
/// between the first step of a sealed segment's staged removal (its compressed copy) and the
/// second (its manifest) replaces the manifest: the compressed copy is back in place, the
/// replacement stays, no staging file is left, and the entry is reported changed, never
/// removed. Without a change both files go.
#[test]
fn telemetry_eviction_keeps_an_entry_whole_when_the_manifest_changes_midway() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let layout = Layout::new(&root);
    let kept = sealed_with(&root, b"kept's input\n", b"kept's own output\n");
    let gone = sealed_with(&root, b"gone's input\n", b"gone's own output\n");
    age(&root, Duration::from_secs(2 * 3_600));
    let now = SystemTime::now() + Duration::from_secs(2 * 3_600);
    let directory = Directory::measure(&root, false).unwrap().unwrap();
    let names = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(layout.segments())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let sealed = layout.sealed_segment(&kept);
    let compressed = std::fs::read(&sealed).unwrap();
    let manifest = layout.refs(&kept);
    let target = directory.target(&kept).unwrap();
    let reached = std::cell::Cell::new(false);
    let evicted = budget::evict_with(&root, &target, now, &|| {
        // Midway: the compressed copy is staged, the manifest not yet.
        assert!(
            !sealed.exists(),
            "the first step staged the compressed copy"
        );
        let staged = root.join("replacement");
        std::fs::write(&staged, b"not the manifest the inventory found\n").unwrap();
        std::fs::rename(&staged, &manifest).unwrap();
        reached.set(true);
    })
    .unwrap();
    assert!(reached.get(), "the hook ran between the two steps");
    let budget::Evicted::Changed(reason) = evicted else {
        panic!("reported removed: {evicted:?}");
    };
    assert!(reason.contains("manifest was replaced"), "{reason}");
    assert_eq!(
        std::fs::read(&sealed).unwrap(),
        compressed,
        "the compressed copy is in place"
    );
    review_telemetry::segment::reader::read_sealed(&sealed).unwrap();
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        b"not the manifest the inventory found\n"
    );
    assert!(
        names().iter().all(|name| !name.starts_with('.')),
        "no staging file is left: {:?}",
        names()
    );
    let after = Directory::measure(&root, false).unwrap().unwrap();
    assert_eq!(after.segments[&kept].state, State::Sealed);

    // Unchanged, the whole entry goes.
    let target = directory.target(&gone).unwrap();
    assert!(matches!(
        budget::evict(&root, &target, now).unwrap(),
        budget::Evicted::Removed(_)
    ));
    assert!(!layout.sealed_segment(&gone).exists() && !layout.refs(&gone).exists());
    assert_eq!(
        names(),
        [format!("{kept}.jsonl.gz"), format!("{kept}.refs")],
        "only the changed entry is left"
    );

    // A file appears under the compressed copy's name once that copy is staged: before the
    // staged files are unlinked both original names are checked, so the entry is reported
    // changed, never removed; its manifest is restored, the replacement stays, and the staged
    // copy that cannot go back under its taken name is left for the sweep's temporaries.
    let returned = sealed_with(&root, b"returned's input\n", b"returned's own output\n");
    age(&root, Duration::from_secs(2 * 3_600));
    let directory = Directory::measure(&root, false).unwrap().unwrap();
    let target = directory.target(&returned).unwrap();
    let sealed = layout.sealed_segment(&returned);
    let manifest = layout.refs(&returned);
    let manifest_bytes = std::fs::read(&manifest).unwrap();
    let evicted = budget::evict_with(&root, &target, now, &|| {
        assert!(!sealed.exists(), "the compressed copy is staged");
        std::fs::write(&sealed, b"a replacement under the compressed name").unwrap();
    })
    .unwrap();
    let budget::Evicted::Changed(reason) = evicted else {
        panic!("reported removed: {evicted:?}");
    };
    assert!(reason.contains("reappeared"), "{reason}");
    assert!(reason.contains("could not be restored"), "{reason}");
    assert_eq!(
        std::fs::read(&sealed).unwrap(),
        b"a replacement under the compressed name"
    );
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        manifest_bytes,
        "the manifest is back under its name"
    );
    let staged: Vec<String> = names()
        .into_iter()
        .filter(|name| name.starts_with('.'))
        .collect();
    assert_eq!(
        staged.len(),
        1,
        "the staged copy is a temporary: {:?}",
        names()
    );
}
