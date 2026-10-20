//! The index: a rebuild answers every query identically, and replacement under `index.lock`
//! never shows a reader a database replaced under it (design §6).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use review_telemetry::blob::Media;
use review_telemetry::index::{self, Index};
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, ConfigRow, ProblemWhat, SealExit, SpanKind, SpanStatus};
use review_telemetry::trace::Execution;

use crate::writer;

/// A directory with two sealed runs, a recovered crash and a live writer, blobs among them.
fn populated(root: &std::path::Path) -> review_telemetry::segment::writer::Writer {
    for (run, crash) in [("run-a", false), ("run-b", true)] {
        let writer = writer(root, Level::Content);
        let execution = Execution {
            store_id: "f".repeat(64),
            run_id: run.into(),
            execution_id: "x".into(),
            ordinal: 1,
            store_sequence_at_start: 7,
        };
        let command = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        let trace = execution.trace_id();
        let run_span = writer
            .span_start(
                Some(&trace),
                Some(&command),
                "af.run",
                SpanKind::Internal,
                execution.attributes().enumerated("af.run.kind", "task"),
            )
            .unwrap();
        writer.config(
            "x",
            vec![ConfigRow {
                key: "telemetry.level".into(),
                value: "\"content\"".into(),
                layer: "environment".into(),
                origin: None,
            }],
        );
        for node in 0..3 {
            let span = writer
                .span_start(
                    Some(&trace),
                    Some(&run_span),
                    "af.node",
                    SpanKind::Internal,
                    Attrs::new().id("af.node_id", format!("n{node}")),
                )
                .unwrap();
            writer.event_with_content(
                &span,
                "gen_ai.content.output",
                Attrs::new(),
                format!("node {node} said\n").as_bytes(),
                Media::Text,
            );
            writer.span_end(
                &span,
                SpanStatus::Ok,
                Attrs::new().decimal("af.usage.input_tokens", "12"),
                2,
            );
        }
        writer.problem(ProblemWhat::Redaction, "a value was withheld");
        if crash {
            writer.flush();
            writer.abandon();
            review_telemetry::segment::reader::recover_all(&Layout::new(root)).unwrap();
        } else {
            writer.span_end(&run_span, SpanStatus::Ok, Attrs::new(), 1);
            writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
            writer.finish(SealExit::Ok).unwrap();
        }
    }
    let live = writer(root, Level::Spans);
    let span = live
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    live.event(&span, "af.note", Attrs::new().text("af.text", "live"));
    live.flush();
    live
}

#[test]
fn telemetry_index_rebuild_is_identical() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let live = populated(temp.path());
    // Mixed media: the text bytes `"violet"` and a JSON string of the same source bytes retain
    // identical bytes, one object with two occurrences of different media types, the first
    // ingested before the second is written.
    let mixed = writer(temp.path(), Level::Content);
    let mixed_span = mixed
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    mixed.event_with_content(
        &mixed_span,
        "af.text",
        Attrs::new(),
        b"\"violet\"",
        Media::Text,
    );
    mixed.flush();
    let mut incremental = Index::open(&layout).unwrap();
    incremental.ingest().unwrap();
    // The live segments grow; a second ingest reads only their new tails.
    let span = review_telemetry::trace::span_id(&live.segment_id().unwrap(), 2);
    live.event(&span, "af.note", Attrs::new().text("af.text", "later"));
    live.flush();
    mixed.event_with_content(
        &mixed_span,
        "af.json",
        Attrs::new(),
        b"\"violet\"",
        Media::Json,
    );
    mixed.flush();
    let (second, refused) = incremental.ingest().unwrap();
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(second.records, 3);
    let before = incremental.dump().unwrap();
    drop(incremental);
    assert_eq!(before["runs"].len(), 2);
    assert_eq!(before["executions"].len(), 2);
    assert!(before["spans"].iter().all(|row| row.len() == 14));
    // The digest-keyed row holds only what is intrinsic to the bytes; the media type is each
    // occurrence's.
    let violet = review_telemetry::trace::sha256_digest(b"\"violet\"");
    let objects: Vec<_> = before["blobs"]
        .iter()
        .filter(|row| row[0] == format!("'{violet}'"))
        .collect();
    assert_eq!(objects.len(), 1, "{:?}", before["blobs"]);
    assert_eq!(
        objects[0].len(),
        4,
        "digest, bytes, stored_bytes, codec: {objects:?}"
    );
    assert_eq!(objects[0][3], format!("'{}'", index::BLOB_CODEC));
    let index_now = Index::open(&layout).unwrap();
    let media: Vec<String> = index_now
        .connection()
        .prepare("SELECT media_type FROM blob_refs WHERE digest = ?1 ORDER BY occurrence_id")
        .unwrap()
        .query_map([&violet], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        media,
        [Media::Text.media_type(), Media::Json.media_type()],
        "two occurrences of one object keep their own media types"
    );
    drop(index_now);

    index::replace(&layout).unwrap();
    let rebuilt = Index::open(&layout).unwrap().dump().unwrap();
    assert_eq!(rebuilt, before);
    // Reading the segments directly answers the same.
    let mut direct = Index::direct(&layout, "test").unwrap();
    direct.ingest().unwrap();
    assert_eq!(direct.dump().unwrap(), before);
    live.finish(SealExit::Ok).unwrap();
    mixed.finish(SealExit::Ok).unwrap();
    // Sealed, reindexed: the occurrences still answer with their own media types.
    index::replace(&layout).unwrap();
    let sealed = Index::open(&layout).unwrap().dump().unwrap();
    let media_of = |dump: &std::collections::BTreeMap<String, Vec<Vec<String>>>| {
        let columns = dump["blob_refs"][0].len();
        dump["blob_refs"]
            .iter()
            .filter(|row| row[3] == format!("'{violet}'"))
            .map(|row| row[columns - 2].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(media_of(&sealed), media_of(&before));
    assert_eq!(media_of(&sealed).len(), 2);
}

#[test]
fn telemetry_index_replacement_under_lock() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let live = populated(temp.path());
    live.finish(SealExit::Ok).unwrap();
    let mut baseline = Index::open(&layout).unwrap();
    baseline.ingest().unwrap();
    let expected = baseline.dump().unwrap();
    drop(baseline);

    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let layout = layout.clone();
            let expected = expected.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut reads = 0;
                while !stop.load(Ordering::SeqCst) || reads < 5 {
                    let mut index = Index::open(&layout).unwrap();
                    index.ingest().unwrap();
                    assert_eq!(index.dump().unwrap(), expected);
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    let mut replaced = 0;
    for _ in 0..8 {
        if index::replace(&layout).is_ok() {
            replaced += 1;
        }
    }
    stop.store(true, Ordering::SeqCst);
    for reader in readers {
        assert!(reader.join().unwrap() >= 5);
    }
    assert!(replaced > 0, "no replacement took the lock");
    // The replaced index answers as before, and its files are one database, WAL and memory.
    let index = Index::open(&layout).unwrap();
    assert_eq!(index.dump().unwrap(), expected);
}

/// The index and the lock files are never opened through a link: a linked `index.sqlite` is
/// refused and what it points to stays untouched; `reindex` removes the link itself and
/// recreates the database in its place; a linked `gc.lock` is never created or locked.
#[test]
fn telemetry_index_never_opens_through_a_link() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let layout = Layout::new(&root);
    let live = populated(&root);
    live.finish(SealExit::Ok).unwrap();
    let outside = temp.path().join("outside.sqlite");
    std::fs::write(&outside, b"not this index").unwrap();
    std::os::unix::fs::symlink(&outside, layout.index()).unwrap();
    let refusal = Index::open(&layout).err().expect("refused");
    assert!(refusal.contains("not a regular file"), "{refusal}");
    index::replace(&layout).unwrap();
    assert!(
        std::fs::symlink_metadata(layout.index()).is_ok_and(|metadata| metadata.is_file()),
        "recreated in place of the link"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"not this index");
    let mut index = Index::open(&layout).unwrap();
    index.ingest().unwrap();
    assert_eq!(index.dump().unwrap()["runs"].len(), 2);
    drop(index);

    let missing = temp.path().join("nowhere.lock");
    std::fs::remove_file(layout.gc_lock()).ok();
    std::os::unix::fs::symlink(&missing, layout.gc_lock()).unwrap();
    assert!(
        review_telemetry::layout::lock_in(
            &layout.open_versioned().unwrap(),
            review_telemetry::layout::GC_LOCK,
            std::time::Duration::ZERO,
            nix::fcntl::FlockArg::LockSharedNonblock,
        )
        .is_err(),
        "a linked lock file is refused"
    );
    assert!(!missing.exists(), "nothing created through the link");
}

/// Every file beneath `directory` with its bytes, walked without following a link (a link is
/// listed with its target's name).
fn snapshot(directory: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let name = path.strip_prefix(directory).unwrap().display().to_string();
            if metadata.is_dir() {
                files.insert(format!("{name}/"), Vec::new());
                pending.push(path);
            } else if metadata.is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                files.insert(name, target.display().to_string().into_bytes());
            } else {
                files.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Put the original `v1/` back where `swap` moved it from.
fn restore(versioned: &std::path::Path, moved: &std::path::Path) {
    if std::fs::symlink_metadata(moved).is_err() {
        return;
    }
    match std::fs::symlink_metadata(versioned) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(versioned).unwrap(),
        Ok(_) => std::fs::remove_file(versioned).unwrap(),
        Err(_) => {}
    }
    std::fs::rename(moved, versioned).unwrap();
}

fn status_input(root: &std::path::Path) -> review_telemetry::status::StatusInput {
    review_telemetry::status::StatusInput {
        root: root.to_path_buf(),
        level: Level::Spans,
        max_bytes: 4 << 30,
        registry: None,
        accounted: Vec::new(),
        accounted_unknown: None,
    }
}

/// The index is opened only through the `v1/` descriptor the command holds, never through a
/// checked pathname: `v1/` is renamed and replaced, by a link to an outside directory or by
/// another real directory, between the checks of the held directory and the open of the
/// database. `status` and `reindex` then open and create nothing outside the original `v1/`,
/// neither the database nor its WAL, shared-memory or lock files: SQLite is given the path that
/// names the held directory itself (its inode path on macOS, its descriptor's own entry on
/// Linux), so the database is opened, and created, in the original `v1/` and the command
/// succeeds.
#[test]
fn telemetry_index_open_stays_within_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let layout = Layout::new(&root);
    let live = populated(&root);
    live.finish(SealExit::Ok).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let versioned = layout.versioned();
    let moved = root.join("v1.moved");
    let input = status_input(&root);
    for replacement in ["link", "directory"] {
        for operation in ["status", "reindex"] {
            // The original `v1/` in place, with no index yet: the open has to create one.
            restore(&versioned, &moved);
            for file in layout.index_files() {
                std::fs::remove_file(file).ok();
            }
            let swap = {
                let (versioned, moved, outside) =
                    (versioned.clone(), moved.clone(), outside.clone());
                let mut swapped = false;
                move || {
                    if !swapped {
                        swapped = true;
                        std::fs::rename(&versioned, &moved).unwrap();
                        if replacement == "link" {
                            std::os::unix::fs::symlink(&outside, &versioned).unwrap();
                        } else {
                            std::fs::create_dir(&versioned).unwrap();
                        }
                    }
                }
            };
            let result = index::with_connect_hook(swap, || match operation {
                "status" => review_telemetry::status::status(&input).map(|(status, _)| status.runs),
                _ => review_telemetry::status::reindex(&root).map(|document| document.segments),
            });
            let case = format!("{operation} after a {replacement} replaced v1/");
            let replaced = std::fs::symlink_metadata(&versioned).unwrap();
            assert_eq!(
                (replaced.is_symlink(), replaced.is_dir()),
                (replacement == "link", replacement == "directory"),
                "{case}: the hook swapped v1/"
            );
            assert!(snapshot(&outside).is_empty(), "{case}: nothing outside");
            if replacement == "directory" {
                assert!(
                    snapshot(&versioned).is_empty(),
                    "{case}: nothing in the replacement: {:?}",
                    snapshot(&versioned).keys().collect::<Vec<_>>()
                );
            }
            let count = result.unwrap_or_else(|refusal| panic!("{case}: {refusal}"));
            assert!(count > 0, "{case}: {count}");
            assert!(
                std::fs::symlink_metadata(moved.join("index.sqlite"))
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0),
                "{case}: the index is in the original v1/"
            );
        }
    }
    restore(&versioned, &moved);
}

/// Direct mode keeps the held descriptor: `index.lock` is held past the two-second wait, and
/// while `status` waits for it the telemetry root is renamed and replaced by a link to another
/// telemetry directory. `status` then reads the segments directly through the `v1/` it opened:
/// it counts and ingests the original segments only, and nothing of the link target.
#[test]
fn telemetry_direct_mode_reads_the_held_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let moved = temp.path().join("telemetry.moved");
    let other = temp.path().join("other");
    let layout = Layout::new(&root);
    // The original: one sealed run. The other directory: two sealed runs and a live tail.
    let original = writer(&root, Level::Spans);
    let span = original
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    original.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    original.finish(SealExit::Ok).unwrap();
    let other_live = populated(&other);
    other_live.finish(SealExit::Ok).unwrap();
    let before = snapshot(&other);

    let replacing = review_telemetry::layout::lock_in(
        &layout.open_versioned().unwrap(),
        review_telemetry::layout::INDEX_LOCK,
        std::time::Duration::ZERO,
        nix::fcntl::FlockArg::LockExclusiveNonblock,
    )
    .unwrap()
    .expect("the test holds index.lock");
    let input = status_input(&root);
    let reading = std::thread::spawn(move || review_telemetry::status::status(&input));
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::fs::rename(&root, &moved).unwrap();
    std::os::unix::fs::symlink(&other, &root).unwrap();
    let (status, note) = reading.join().unwrap().unwrap();
    drop(replacing);

    assert_eq!(status.index.state, "direct", "{status:?}");
    assert!(note.is_some_and(|note| note.contains("directly")));
    assert_eq!(status.segments.sealed, 1, "{:?}", status.segments);
    assert_eq!(status.index.ingested_segments, 1, "{:?}", status.index);
    assert_eq!(status.runs, 0, "the original has no run; the other has two");
    assert!(status.unreadable.is_empty(), "{:?}", status.unreadable);
    assert_eq!(snapshot(&other), before, "the link target is never touched");
}

/// Blob inspection and verification go through the held `v1/`: after the command opened the
/// root, it is renamed and replaced by a link to a directory holding a corrupt file under the
/// first referenced digest's name and none under the others'. `status` reports the original
/// blobs present (through the link every other one would be missing), `reindex` verifies them
/// and finds none corrupt (through the link the first would be), and neither reads, locks or
/// creates anything in the link target.
#[test]
fn telemetry_blob_checks_stay_within_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let moved = temp.path().join("telemetry.moved");
    let outside = temp.path().join("outside");
    let layout = Layout::new(&root);
    let live = populated(&root);
    live.finish(SealExit::Ok).unwrap();
    // The first stored reference's file planted corrupt in the link target, the others absent.
    index::replace(&layout).unwrap();
    let digests: Vec<String> = Index::open(&layout)
        .unwrap()
        .connection()
        .prepare("SELECT DISTINCT digest FROM blob_refs WHERE state = 'stored'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(digests.len() > 1, "{digests:?}");
    let planted = Layout::new(&outside).blob(&digests[0]);
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(&planted, b"not the blob").unwrap();
    let before = snapshot(&outside);

    for operation in ["status", "reindex"] {
        if std::fs::symlink_metadata(&moved).is_ok() {
            std::fs::remove_file(&root).unwrap();
            std::fs::rename(&moved, &root).unwrap();
        }
        let swap = {
            let (root, moved, outside) = (root.clone(), moved.clone(), outside.clone());
            let mut swapped = false;
            move || {
                if !swapped {
                    swapped = true;
                    std::fs::rename(&root, &moved).unwrap();
                    std::os::unix::fs::symlink(&outside, &root).unwrap();
                }
            }
        };
        let input = status_input(&root);
        index::with_connect_hook(swap, || match operation {
            "status" => {
                let (status, _) = review_telemetry::status::status(&input).unwrap();
                assert_eq!(status.blob_verification.as_str(), "verified");
                assert_eq!(status.missing_blobs, 0, "{operation}");
            }
            _ => {
                let document = review_telemetry::status::reindex(&root).unwrap();
                assert_eq!(document.blob_verification.as_str(), "verified");
                assert_eq!(document.missing_blobs, 0, "{operation}");
                assert!(
                    document.corrupt_blobs.is_empty(),
                    "{operation}: {:?}",
                    document.corrupt_blobs
                );
            }
        });
        assert!(
            std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_symlink()),
            "{operation}: the hook swapped the root"
        );
        assert_eq!(
            snapshot(&outside),
            before,
            "{operation}: the link target untouched"
        );
    }
}

/// Recovery goes through the held `v1/`: the root is renamed and replaced by a link to another
/// telemetry directory holding an unsealed segment after `reindex` opened it and before it
/// recovers anything. `reindex` recovers the original's unsealed segment in the original root,
/// and recovers, seals, writes and locks nothing in the link target.
#[test]
fn telemetry_reindex_recovery_stays_within_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let moved = temp.path().join("telemetry.moved");
    let outside = temp.path().join("outside");
    let unsealed = |directory: &std::path::Path| {
        let crashed = writer(directory, Level::Spans);
        crashed
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        crashed.flush();
        crashed.abandon();
        crashed.segment_id().unwrap()
    };
    let original = unsealed(&root);
    let foreign = unsealed(&outside);
    let before = snapshot(&outside);
    let swap = {
        let (root, moved, outside) = (root.clone(), moved.clone(), outside.clone());
        move || {
            if std::fs::symlink_metadata(&moved).is_err() {
                std::fs::rename(&root, &moved).unwrap();
                std::os::unix::fs::symlink(&outside, &root).unwrap();
            }
        }
    };
    let document = review_telemetry::status::with_reindex_hook(swap, || {
        review_telemetry::status::reindex(&root)
    })
    .unwrap();
    assert!(
        std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_symlink()),
        "the hook swapped the root"
    );
    let recovered: Vec<&str> = document
        .recovered
        .iter()
        .map(|recovered| recovered.segment_id.as_str())
        .collect();
    assert_eq!(recovered, [original.as_str()], "{document:?}");
    assert_eq!(document.segments, 1);
    let held = Layout::new(&moved);
    assert!(held.sealed_segment(&original).is_file());
    assert!(!held.live_segment(&original).exists());
    assert_eq!(snapshot(&outside), before, "the link target untouched");
    assert!(Layout::new(&outside).live_segment(&foreign).is_file());
}
