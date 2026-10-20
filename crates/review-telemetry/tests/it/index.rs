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
        review_telemetry::layout::shared_now(&layout.gc_lock()).is_err(),
        "a linked lock file is refused"
    );
    assert!(!missing.exists(), "nothing created through the link");
}
