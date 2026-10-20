//! Blobs: identity of the retained bytes, the publication protocol against a paused sweep, a
//! concurrent reader and a crash at each step, occurrences that survive deduplication, and a
//! missing file reported, never fatal (design §6).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use review_telemetry::blob::collect::collect;
use review_telemetry::blob::reader::{self as blob_reader, Content};
use review_telemetry::blob::{self, Media};
use review_telemetry::index::{self, Index};
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{
    Attrs, BlobState, OCCURRENCE_PREFIX, Record, SealExit, SpanKind, SpanStatus,
};
use review_telemetry::redact::Redactor;
use review_telemetry::segment::reader;
use review_telemetry::segment::writer::{HookAction, PublishStep, Writer};

use crate::{options, sealed_records, writer};

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn telemetry_blob_identity_is_retained_bytes() {
    let source: String = (0..300)
        .map(|n| format!("line {n:03} of output\n"))
        .collect();
    let small = blob::prepare(source.as_bytes(), Media::Text, 1024, &Redactor::new());
    let large = blob::prepare(source.as_bytes(), Media::Text, 64 * 1024, &Redactor::new());
    // Another bound: other retained bytes, another identity; the source relates them.
    assert_ne!(small.digest, large.digest);
    assert_eq!(small.source_digest, large.source_digest);
    assert_eq!(small.original_bytes, large.original_bytes);
    assert!(small.truncated && !large.truncated);
    assert_eq!(
        small.digest,
        review_telemetry::trace::sha256_digest(&small.retained)
    );
    // Other redaction rules (here: a home prefix replaced) retain other bytes, so another
    // identity, never a wrong reuse.
    let with_home = format!("/home/fixture/work\n{source}");
    let plain = blob::prepare(
        with_home.as_bytes(),
        Media::Text,
        64 * 1024,
        &Redactor::new(),
    );
    let homed = blob::prepare(
        with_home.as_bytes(),
        Media::Text,
        64 * 1024,
        &Redactor::new().with_home("/home/fixture"),
    );
    assert_ne!(plain.digest, homed.digest);
    assert_eq!(plain.source_digest, homed.source_digest);
    // The same source, bound and rules: the same object, its file reused after a size check.
    let again = blob::prepare(source.as_bytes(), Media::Text, 1024, &Redactor::new());
    assert_eq!(again, small);
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let first = blob::stage(&layout, &small).unwrap();
    blob::place(&first).unwrap();
    let second = blob::stage(&layout, &again).unwrap();
    blob::place(&second).unwrap();
    assert!(!second.temp.exists());
    assert_eq!(
        blob_reader::read(&layout, &small.digest),
        Content::Bytes(small.retained.clone())
    );
}

fn published_events(layout: &Layout, segment_id: &str) -> Vec<Record> {
    reader::read_live(&layout.live_segment(segment_id))
        .unwrap()
        .records
}

#[test]
fn telemetry_blob_publication_survives_paused_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());

    // A publisher paused between the rename and the reference flush keeps the blob: the sweep
    // cannot take gc.lock while the publisher holds it shared.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Arc::new(Mutex::new(resume_rx));
    let mut paused = options(temp.path(), Level::Content);
    let reached = Mutex::new(reached_tx);
    paused.publish_hook = Some(Arc::new(move |step| {
        if step == PublishStep::Renamed {
            reached.lock().unwrap().send(()).unwrap();
            resume_rx.lock().unwrap().recv().unwrap();
        }
        HookAction::Continue
    }));
    let publisher = Writer::new(paused).unwrap();
    let span = publisher
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    publisher.flush();
    let segment_id = publisher.segment_id().unwrap();
    let thread_publisher = publisher.clone();
    let thread_span = span.clone();
    let publishing = std::thread::spawn(move || {
        thread_publisher.event_with_content(
            &thread_span,
            "gen_ai.content.input",
            Attrs::new(),
            b"the composed input\n",
            Media::Text,
        )
    });
    reached_rx.recv().unwrap();
    let prepared = blob::prepare(
        b"the composed input\n",
        Media::Text,
        options(temp.path(), Level::Content).content_max_blob_bytes,
        &Redactor::new(),
    );
    let file = layout.blob(&prepared.digest);
    assert!(file.is_file(), "renamed into place before any reference");
    assert!(
        !published_events(&layout, &segment_id)
            .iter()
            .any(|record| matches!(record, Record::BlobRef(_)))
    );
    let swept = collect(&layout, Duration::ZERO, SystemTime::now()).unwrap();
    assert!(swept.skipped.is_some(), "{swept:?}");
    assert!(file.is_file());
    resume_tx.send(()).unwrap();
    assert!(publishing.join().unwrap().is_some());
    // Now the flushed reference is a root, even to a sweep that ignores age.
    let swept = collect(&layout, Duration::ZERO, SystemTime::now()).unwrap();
    assert_eq!(swept.skipped, None);
    assert!(swept.removed_blobs.is_empty(), "{swept:?}");
    assert!(file.is_file());

    // A reader that reads a reference the moment it is flushed, while another thread
    // publishes, finds the file.
    let concurrent = writer(temp.path(), Level::Content);
    let concurrent_span = concurrent
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    concurrent.flush();
    let concurrent_id = concurrent.segment_id().unwrap();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_layout = layout.clone();
    let reader_done = done.clone();
    let reader_id = concurrent_id.clone();
    let reading = std::thread::spawn(move || {
        let mut checked = std::collections::BTreeSet::new();
        loop {
            let finished = reader_done.load(std::sync::atomic::Ordering::SeqCst);
            for record in published_events(&reader_layout, &reader_id) {
                if let Record::BlobRef(occurrence) = record
                    && checked.insert(occurrence.occurrence_id.clone())
                {
                    assert!(
                        matches!(
                            blob_reader::content(&reader_layout, &occurrence),
                            Content::Bytes(_)
                        ),
                        "{occurrence:?}"
                    );
                }
            }
            if finished {
                return checked.len();
            }
        }
    });
    for index in 0..40 {
        concurrent.event_with_content(
            &concurrent_span,
            "gen_ai.content.output",
            Attrs::new(),
            format!("output {index}\n").as_bytes(),
            Media::Text,
        );
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(reading.join().unwrap(), 40);
    concurrent.finish(SealExit::Ok).unwrap();
    publisher.finish(SealExit::Ok).unwrap();

    // A publisher crashed at each step, then a sweep and a reindex.
    for step in [
        PublishStep::Staged,
        PublishStep::Locked,
        PublishStep::Renamed,
        PublishStep::Flushed,
    ] {
        let mut crashing = options(temp.path(), Level::Content);
        crashing.publish_hook = Some(Arc::new(move |at| {
            if at == step {
                HookAction::Crash
            } else {
                HookAction::Continue
            }
        }));
        let crashed = Writer::new(crashing).unwrap();
        let span = crashed
            .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
            .unwrap();
        crashed.flush();
        let content = format!("crashed at {step:?}\n");
        assert_eq!(
            crashed.event_with_content(
                &span,
                "gen_ai.content.input",
                Attrs::new(),
                content.as_bytes(),
                Media::Text
            ),
            None
        );
        let digest =
            blob::prepare(content.as_bytes(), Media::Text, 16 << 20, &Redactor::new()).digest;
        // Within the hour nothing unreferenced goes; past it, orphans and temporaries do.
        collect(&layout, HOUR, SystemTime::now()).unwrap();
        let referenced = step == PublishStep::Flushed;
        let placed = matches!(step, PublishStep::Renamed | PublishStep::Flushed);
        assert_eq!(layout.blob(&digest).is_file(), placed, "{step:?}");
        let swept = collect(&layout, Duration::ZERO, SystemTime::now() + HOUR).unwrap();
        assert_eq!(
            layout.blob(&digest).is_file(),
            referenced,
            "{step:?}: {swept:?}"
        );
        assert!(
            reader::list(&layout).unwrap().temporary.is_empty(),
            "{step:?}"
        );
        let leftovers: Vec<_> = walk(&layout.blobs())
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "{step:?}: {leftovers:?}");
        index::replace(&layout).unwrap();
        let index = Index::open(&layout).unwrap();
        assert!(
            index.missing_blobs().unwrap().unwrap().is_empty(),
            "{step:?}"
        );
    }
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut all = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            if entry.path().is_dir() {
                all.extend(walk(&entry.path()));
            } else {
                all.push(entry.path());
            }
        }
    }
    all
}

#[test]
fn telemetry_blob_occurrences_survive_dedup() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let sources = [
        b"token=first-secret\nthe same output\n".as_slice(),
        b"token=other-secret\nthe same output\n".as_slice(),
    ];
    let mut sealed = Vec::new();
    for _segment in 0..2 {
        let writer = writer(temp.path(), Level::Content);
        let span = writer
            .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
            .unwrap();
        for source in sources {
            writer.event_with_content(
                &span,
                "gen_ai.content.output",
                Attrs::new(),
                source,
                Media::Text,
            );
        }
        sealed.push(writer.finish(SealExit::Ok).unwrap().unwrap());
    }
    let records = sealed_records(&sealed[0].path);
    let objects: Vec<_> = records
        .iter()
        .filter(|record| matches!(record, Record::Blob(_)))
        .collect();
    let occurrences: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(objects.len(), 1, "one object per digest per segment");
    assert_eq!(occurrences.len(), 2);
    assert_eq!(occurrences[0].digest, occurrences[1].digest);
    assert_ne!(occurrences[0].source_digest, occurrences[1].source_digest);
    assert!(occurrences.iter().all(|o| o.state == BlobState::Stored));
    // Each event names its own occurrence.
    let named: Vec<String> = records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) => event.blob.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        occurrences
            .iter()
            .map(|o| o.occurrence_id.clone())
            .collect::<Vec<_>>()
    );
    index::replace(&layout).unwrap();
    let index = Index::open(&layout).unwrap();
    assert_eq!(index.count("SELECT count(*) FROM blobs").unwrap(), 1);
    assert_eq!(index.count("SELECT count(*) FROM blob_refs").unwrap(), 4);
    assert_eq!(
        index
            .count("SELECT count(DISTINCT source_digest) FROM blob_refs")
            .unwrap(),
        2
    );
}

#[test]
fn telemetry_missing_blob_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Content);
    let span = writer
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(
        &span,
        "gen_ai.content.input",
        Attrs::new(),
        b"kept\n",
        Media::Text,
    );
    writer.event_with_content(
        &span,
        "gen_ai.content.output",
        Attrs::new(),
        b"deleted\n",
        Media::Text,
    );
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let occurrences: Vec<_> = sealed_records(&sealed.path)
        .into_iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence),
            _ => None,
        })
        .collect();
    let gone = occurrences[1].clone();
    std::fs::remove_file(layout.blob(gone.digest.as_deref().unwrap())).unwrap();

    let (status, _) = review_telemetry::status::status(&review_telemetry::status::StatusInput {
        root: temp.path().to_path_buf(),
        level: Level::Content,
        max_bytes: 4 << 30,
        registry: None,
        accounted: Vec::new(),
        accounted_unknown: None,
    })
    .unwrap();
    assert_eq!(status.missing_blobs, 1);
    let index = Index::open(&layout).unwrap();
    assert_eq!(
        index.missing_blobs().unwrap().unwrap(),
        [(gone.occurrence_id.clone(), gone.digest.clone().unwrap())]
    );
    let occurrence = index.occurrence(&gone.occurrence_id).unwrap().unwrap();
    assert_eq!(
        blob_reader::content(&layout, &occurrence),
        Content::Unavailable(blob_reader::MISSING.into())
    );
    assert_eq!(blob_reader::MISSING, "content unavailable: missing blob");
    assert!(matches!(
        blob_reader::content(&layout, &occurrences[0]),
        Content::Bytes(_)
    ));
    // Every query still answers.
    assert_eq!(index.dump().unwrap()["blob_refs"].len(), 2);
}

/// When the sweep holds `gc.lock` the publisher never waits: the blob is dropped and counted,
/// its occurrence says `dropped`, and no file and no object record is left.
#[test]
fn a_blob_is_dropped_while_a_sweep_holds_the_lock() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Content);
    let span = writer
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    let sweep = review_telemetry::layout::exclusive_within(&layout.gc_lock(), HOUR)
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    assert!(
        writer
            .event_with_content(
                &span,
                "gen_ai.content.input",
                Attrs::new(),
                b"held\n",
                Media::Text
            )
            .is_some()
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the publisher waited"
    );
    drop(sweep);
    assert_eq!(writer.dropped(), 1);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let records = sealed_records(&sealed.path);
    assert!(
        !records
            .iter()
            .any(|record| matches!(record, Record::Blob(_)))
    );
    let occurrence = records
        .iter()
        .find_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(occurrence.state, BlobState::Dropped);
    assert_eq!(
        blob_reader::content(&layout, &occurrence),
        Content::Unavailable(blob_reader::DROPPED.into())
    );
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("no seal");
    };
    assert_eq!(seal.dropped, Some(1));
    assert!(walk(&layout.blobs()).is_empty(), "no file left behind");
    assert_eq!(std::fs::read_to_string(&sealed.refs).unwrap(), "");
}

/// Every file under `blobs/`, with its size.
fn blob_files(layout: &Layout) -> Vec<(std::path::PathBuf, u64)> {
    let mut files: Vec<_> = walk(&layout.blobs())
        .into_iter()
        .map(|path| {
            let bytes = std::fs::metadata(&path).unwrap().len();
            (path, bytes)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn telemetry_exhausted_segment_creates_no_blob() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let mut options = options(temp.path(), Level::Content);
    options.segment_max_bytes = 128 * 1024;
    options.content_max_blob_bytes = 64 * 1024;
    let writer = Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    // Unique content until the limit refuses a batch: the refused batch is a content batch
    // that would have crossed the limit, and it leaves no file behind.
    let mut round = 0;
    while writer.dropped() == 0 {
        round += 1;
        writer.event_with_content(
            &span,
            "gen_ai.content.output",
            Attrs::new(),
            format!("unique output {round}\n").repeat(20).as_bytes(),
            Media::Text,
        );
        assert!(round < 10_000, "the limit never bound");
    }
    let at_exhaustion = blob_files(&layout);
    assert!(!at_exhaustion.is_empty());
    // A sustained producer of unique content after exhaustion adds no bytes under blobs/:
    // neither event content nor an attribute over its bound becomes a file.
    for index in 0..500 {
        assert_eq!(
            writer.event_with_content(
                &span,
                "gen_ai.content.output",
                Attrs::new(),
                format!("late output {index}\n").repeat(20).as_bytes(),
                Media::Text,
            ),
            None
        );
        writer.event(
            &span,
            "af.note",
            Attrs::new().text("af.text", format!("late attribute {index} ").repeat(100)),
        );
    }
    assert_eq!(blob_files(&layout), at_exhaustion);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    // Every file under blobs/ is one the sealed segment references: the batch the limit
    // refused placed nothing, and no temporary file is left.
    let referenced: std::collections::BTreeSet<String> = std::fs::read_to_string(&sealed.refs)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let on_disk: std::collections::BTreeSet<String> = at_exhaustion
        .iter()
        .map(|(path, _)| {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            format!("sha256:{}", name.strip_suffix(".gz").unwrap())
        })
        .collect();
    assert_eq!(on_disk, referenced);
    let records = sealed_records(&sealed.path);
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("no seal");
    };
    assert!(seal.dropped >= Some(1000), "{:?}", seal.dropped);
}

#[test]
fn telemetry_concurrent_same_digest_publishes_one_object() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    // Both publishers hold gc.lock and have staged their file before either reaches the
    // segment: the race on the segment's known digests is as close as it gets.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut options = options(temp.path(), Level::Content);
    let hook_barrier = barrier.clone();
    options.publish_hook = Some(Arc::new(move |step| {
        if step == PublishStep::Locked {
            hook_barrier.wait();
        }
        HookAction::Continue
    }));
    let writer = Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    const ROUNDS: usize = 25;
    for round in 0..ROUNDS {
        let content = format!("the same output of round {round}\n");
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let writer = writer.clone();
                let span = span.clone();
                let content = content.clone();
                std::thread::spawn(move || {
                    writer.event_with_content(
                        &span,
                        "gen_ai.content.output",
                        Attrs::new(),
                        content.as_bytes(),
                        Media::Text,
                    )
                })
            })
            .collect();
        for thread in threads {
            assert!(thread.join().unwrap().is_some());
        }
    }
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let records = sealed_records(&sealed.path);
    let mut objects = std::collections::BTreeMap::<String, usize>::new();
    for record in &records {
        if let Record::Blob(object) = record {
            *objects.entry(object.digest.clone()).or_default() += 1;
        }
    }
    assert_eq!(objects.len(), ROUNDS);
    assert!(
        objects.values().all(|count| *count == 1),
        "one blob object per digest: {objects:?}"
    );
    let occurrences: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(occurrences.len(), 2 * ROUNDS);
    assert!(occurrences.iter().all(|o| o.state == BlobState::Stored));
    let Some(Record::Seal(seal)) = records.last() else {
        panic!("no seal");
    };
    assert_eq!(seal.blobs, ROUNDS as u64, "the seal counts one per digest");
    assert_eq!(seal.dropped, Some(0));
    index::replace(&layout).unwrap();
    let index = Index::open(&layout).unwrap();
    assert_eq!(
        index.count("SELECT count(*) FROM blobs").unwrap(),
        ROUNDS as u64
    );
    assert_eq!(
        index.count("SELECT count(*) FROM blob_refs").unwrap(),
        2 * ROUNDS as u64
    );
}

#[test]
fn telemetry_blob_read_requires_shared_lock() {
    let temp = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let writer = writer(temp.path(), Level::Content);
    let span = writer
        .span_start(None, None, "af.attempt", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(
        &span,
        "gen_ai.content.input",
        Attrs::new(),
        b"read only under the lock\n",
        Media::Text,
    );
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let occurrence = sealed_records(&sealed.path)
        .into_iter()
        .find_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence),
            _ => None,
        })
        .unwrap();
    let digest = occurrence.digest.clone().unwrap();
    assert!(layout.blob(&digest).is_file());
    // A collection holds gc.lock: the reader waits its bound, then reports the occurrence
    // without opening the file, although the file is there and readable.
    let collection = review_telemetry::layout::exclusive_within(&layout.gc_lock(), HOUR)
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        blob_reader::content(&layout, &occurrence),
        Content::Unavailable(blob_reader::COLLECTING.into())
    );
    assert!(started.elapsed() >= review_telemetry::layout::LOCK_WAIT);
    assert_eq!(
        blob_reader::read(&layout, &digest),
        Content::Unavailable(blob_reader::COLLECTING.into())
    );
    assert_eq!(
        blob_reader::COLLECTING,
        "content unavailable: collection in progress"
    );
    drop(collection);
    assert_eq!(
        blob_reader::content(&layout, &occurrence),
        Content::Bytes(b"read only under the lock\n".to_vec())
    );
}

#[test]
fn telemetry_content_level_array_values_become_blobs() {
    let long = vec!["w".repeat(2048), "short".to_string()];
    let many: Vec<String> = (0..40).map(|n| format!("item {n}")).collect();
    let small = vec!["a".to_string(), "b".to_string()];
    for level in [Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let writer = writer(temp.path(), level);
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        writer.event(
            &span,
            "af.arrays",
            Attrs::new()
                .texts("af.long", long.clone())
                .texts("af.many", many.clone())
                .texts("af.small", small.clone()),
        );
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
        let records = sealed_records(&sealed.path);
        let lines = crate::sealed_lines(&sealed.path);
        let event = lines
            .iter()
            .find(|line| line["t"] == "event" && line["name"] == "af.arrays")
            .unwrap();
        let attributes = &event["attributes"];
        assert_eq!(
            attributes["af.small"],
            serde_json::json!(small),
            "{level:?}"
        );
        let occurrences: Vec<_> = records
            .iter()
            .filter_map(|record| match record {
                Record::BlobRef(occurrence) => Some(occurrence.clone()),
                _ => None,
            })
            .collect();
        if level == Level::Content {
            let layout = Layout::new(temp.path());
            for (key, expected) in [("af.long", &long), ("af.many", &many)] {
                let named = attributes[key].as_str().unwrap();
                let id = named.strip_prefix(OCCURRENCE_PREFIX).unwrap();
                let occurrence = occurrences
                    .iter()
                    .find(|occurrence| occurrence.occurrence_id == id)
                    .unwrap_or_else(|| panic!("{key}: no occurrence {id}"));
                assert_eq!(occurrence.state, BlobState::Stored);
                assert!(!occurrence.truncated);
                // The reader returns the whole array through the occurrence.
                let Content::Bytes(bytes) = blob_reader::content(&layout, occurrence) else {
                    panic!("{key}: content unavailable");
                };
                let array: Vec<String> = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(&array, expected, "{key}");
            }
            assert_eq!(occurrences.len(), 2);
            assert!(attributes.get("af.truncated_fields").is_none());
        } else {
            let cut = attributes["af.long"].as_array().unwrap();
            assert_eq!(cut.len(), 2);
            let first = cut[0].as_str().unwrap();
            assert!(first.len() <= 1024 && first.ends_with('…'), "{first}");
            let cut = attributes["af.many"].as_array().unwrap();
            assert_eq!(cut.len(), 32);
            assert_eq!(cut[31], "…");
            assert_eq!(attributes["af.truncated_fields"], 2);
            assert!(occurrences.is_empty());
        }
    }
}

#[test]
fn telemetry_array_blob_occurrences_keep_source_identity() {
    // Two over-bound arrays that differ only in a credential: both redact to one document.
    let long = "w".repeat(2048);
    let first = vec![long.clone(), "API_KEY=hunter2".to_string()];
    let second = vec![long.clone(), "API_KEY=swordfish".to_string()];
    let temp = tempfile::tempdir().unwrap();
    let writer = writer(temp.path(), Level::Content);
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(
        &span,
        "af.arrays",
        Attrs::new()
            .texts("af.first", first.clone())
            .texts("af.second", second.clone()),
    );
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let records = sealed_records(&sealed.path);
    let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
    assert!(!text.contains("hunter2") && !text.contains("swordfish"));
    let objects: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::Blob(object) => Some(object.clone()),
            _ => None,
        })
        .collect();
    let occurrences: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .collect();
    // One retained object, two occurrences naming it.
    assert_eq!(objects.len(), 1, "{objects:?}");
    assert_eq!(occurrences.len(), 2, "{occurrences:?}");
    let lines = crate::sealed_lines(&sealed.path);
    let event = lines
        .iter()
        .find(|line| line["t"] == "event" && line["name"] == "af.arrays")
        .unwrap();
    let layout = Layout::new(temp.path());
    for (key, original) in [("af.first", &first), ("af.second", &second)] {
        let id = event["attributes"][key]
            .as_str()
            .unwrap()
            .strip_prefix(OCCURRENCE_PREFIX)
            .unwrap();
        let occurrence = occurrences
            .iter()
            .find(|occurrence| occurrence.occurrence_id == id)
            .unwrap_or_else(|| panic!("{key}: no occurrence {id}"));
        assert_eq!(occurrence.state, BlobState::Stored);
        assert_eq!(
            occurrence.digest.as_deref(),
            Some(objects[0].digest.as_str())
        );
        // The source identity is the original serialized array, before any redaction.
        let source = serde_json::to_vec(original).unwrap();
        assert_eq!(
            occurrence.source_digest,
            review_telemetry::trace::sha256_digest(&source),
            "{key}"
        );
        assert_eq!(occurrence.original_bytes, source.len() as u64, "{key}");
        // Only the redacted bytes are retained.
        let Content::Bytes(bytes) = blob_reader::content(&layout, occurrence) else {
            panic!("{key}: content unavailable");
        };
        let array: Vec<String> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            array,
            vec![long.clone(), review_telemetry::redact::WITHHELD.to_string()],
            "{key}"
        );
    }
    assert_ne!(occurrences[0].source_digest, occurrences[1].source_digest);
}

/// The collector acts only on paths of the blob grammar, judged and removed relative to the
/// opened directory: a prefix linked to a directory outside the root removes nothing there, and
/// a stray-named or linked file inside is never removed; `status` reports them as unrecognized.
#[test]
fn telemetry_collection_never_leaves_the_root() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let layout = Layout::new(&root);
    let blobs = layout.blobs();
    std::fs::create_dir_all(blobs.join("ab")).unwrap();
    std::fs::create_dir_all(layout.segments()).unwrap();

    // Outside the root: an old blob-named file, a temporary-named file and a plain target.
    let outside_blob = outside.join(format!("cd{}.gz", "0".repeat(62)));
    let outside_temp = outside.join(".tmp-blob-1-0a1b2c3d");
    let target = outside.join("target");
    for path in [&outside_blob, &outside_temp, &target] {
        std::fs::write(path, b"outside").unwrap();
    }
    // A prefix linked outside, a stray name, a linked blob file and linked temporaries inside.
    symlink(&outside, blobs.join("cd")).unwrap();
    let stray = blobs.join("ab").join("stray.gz.bak");
    std::fs::write(&stray, b"stray").unwrap();
    let linked_blob = blobs.join("ab").join(format!("ab{}.gz", "1".repeat(62)));
    symlink(&target, &linked_blob).unwrap();
    let linked_temp = blobs.join("ab").join(".tmp-blob-2-0a1b2c3d");
    symlink(&target, &linked_temp).unwrap();
    let linked_write = layout.segments().join(".tmp-write-3-0a1b2c3d");
    symlink(&target, &linked_write).unwrap();
    let linked_segment = layout.segments().join("1700000000000-4-0a1b2c3d.jsonl");
    symlink(&target, &linked_segment).unwrap();
    // A genuine unreferenced blob and a genuine temporary, which the sweep does remove.
    let genuine = blobs.join("ab").join(format!("ab{}.gz", "2".repeat(62)));
    std::fs::write(&genuine, blob::compress(b"unreferenced")).unwrap();
    let genuine_temp = blobs.join("ab").join(".tmp-blob-5-0a1b2c3d");
    std::fs::write(&genuine_temp, b"partial").unwrap();

    let swept = collect(&layout, HOUR, SystemTime::now() + 2 * HOUR).unwrap();
    assert_eq!(swept.skipped, None);
    assert_eq!(swept.removed_blobs, [genuine.clone()]);
    assert_eq!(swept.removed_temporary, [genuine_temp.clone()]);
    for path in [&outside_blob, &outside_temp, &target, &stray] {
        assert!(!std::fs::read(path).unwrap().is_empty(), "{path:?}");
    }
    for link in [
        blobs.join("cd"),
        linked_blob.clone(),
        linked_temp.clone(),
        linked_write.clone(),
        linked_segment.clone(),
    ] {
        assert!(
            std::fs::symlink_metadata(&link).is_ok_and(|metadata| metadata.is_symlink()),
            "{link:?} was removed or replaced"
        );
    }
    assert_eq!(std::fs::read(&target).unwrap(), b"outside");

    let (status, _) = review_telemetry::status::status(&review_telemetry::status::StatusInput {
        root: root.clone(),
        level: Level::Content,
        max_bytes: 4 << 30,
        registry: None,
        accounted: Vec::new(),
        accounted_unknown: None,
    })
    .unwrap();
    assert_eq!(status.segments.unsealed + status.segments.live, 0);
    let unrecognized = status
        .bytes
        .entries
        .iter()
        .find(|entry| entry.kind == "unrecognized")
        .expect("the stray and linked paths are unrecognized");
    let names = unrecognized.names.clone().unwrap();
    for path in [
        &blobs.join("cd"),
        &stray,
        &linked_blob,
        &linked_temp,
        &linked_write,
        &linked_segment,
    ] {
        let relative = path.strip_prefix(&root).unwrap().display().to_string();
        assert!(names.contains(&relative), "{relative} not in {names:?}");
    }
    assert_eq!(unrecognized.count, Some(6));
}

/// An over-bound text attribute reaches the blob as the original bytes: the occurrence's
/// `source_digest` and `original_bytes` describe the attribute as the writer gave it, even when
/// the redactor replaced a home or grant path and the result is still over the bound, while only
/// the redacted bytes are retained and displayed.
#[test]
fn telemetry_text_blob_occurrences_keep_source_identity() {
    let long = "w".repeat(2048);
    let home = format!("/home/violet/src {long}");
    let tilde = format!("~/src {long}");
    let grant = format!("/srv/codex-grant/auth {long}");
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Content);
    options.redactor = Redactor::new()
        .with_home("/home/violet")
        .with_grant("CODEX_HOME", "/srv/codex-grant");
    let writer = Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event(
        &span,
        "af.texts",
        Attrs::new()
            .text("af.home", home.clone())
            .text("af.tilde", tilde.clone())
            .text("af.grant", grant.clone()),
    );
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
    assert!(!text.contains("/home/violet") && !text.contains("/srv/codex-grant"));
    let records = sealed_records(&sealed.path);
    let occurrences: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .collect();
    let objects = records
        .iter()
        .filter(|record| matches!(record, Record::Blob(_)))
        .count();
    // `af.home` and `af.tilde` retain the same bytes: one object, two occurrences.
    assert_eq!((objects, occurrences.len()), (2, 3), "{occurrences:?}");
    let lines = crate::sealed_lines(&sealed.path);
    let event = lines
        .iter()
        .find(|line| line["t"] == "event" && line["name"] == "af.texts")
        .unwrap();
    let layout = Layout::new(temp.path());
    let mut digests = Vec::new();
    for (key, original, retained) in [
        ("af.home", &home, &tilde),
        ("af.tilde", &tilde, &tilde),
        (
            "af.grant",
            &grant,
            &format!("<grant:CODEX_HOME>/auth {long}"),
        ),
    ] {
        let id = event["attributes"][key]
            .as_str()
            .unwrap()
            .strip_prefix(OCCURRENCE_PREFIX)
            .unwrap();
        let occurrence = occurrences
            .iter()
            .find(|occurrence| occurrence.occurrence_id == id)
            .unwrap_or_else(|| panic!("{key}: no occurrence {id}"));
        assert_eq!(occurrence.state, BlobState::Stored, "{key}");
        assert_eq!(
            occurrence.source_digest,
            review_telemetry::trace::sha256_digest(original.as_bytes()),
            "{key}"
        );
        assert_eq!(occurrence.original_bytes, original.len() as u64, "{key}");
        assert_eq!(occurrence.media_type, Media::Text.media_type(), "{key}");
        let Content::Bytes(bytes) = blob_reader::content(&layout, occurrence) else {
            panic!("{key}: content unavailable");
        };
        assert_eq!(String::from_utf8(bytes).unwrap(), *retained, "{key}");
        digests.push(occurrence.digest.clone().unwrap());
    }
    assert_eq!(digests[0], digests[1]);
    assert_ne!(digests[0], digests[2]);
    assert_ne!(occurrences[0].source_digest, occurrences[1].source_digest);
}

/// The bound is judged before anything is parsed: a well-formed and a malformed JSON body, both
/// above `content_max_blob_bytes`, become the same withheld occurrence.
#[test]
fn telemetry_oversized_content_is_withheld_before_parsing() {
    let max = 1024;
    let well_formed = serde_json::to_vec(&serde_json::json!({
        "items": (0..400).map(|n| format!("item {n}")).collect::<Vec<_>>(),
    }))
    .unwrap();
    let mut malformed = b"{\"items\": [".to_vec();
    malformed.extend(std::iter::repeat_n(b'x', 4 * max));
    assert!(serde_json::from_slice::<serde_json::Value>(&malformed).is_err());
    assert!(well_formed.len() > max && malformed.len() > max);
    let redactor = Redactor::new();
    let withheld = serde_json::to_vec(review_telemetry::redact::WITHHELD).unwrap();
    let prepared: Vec<_> = [&well_formed, &malformed]
        .into_iter()
        .map(|source| blob::prepare(source, Media::Json, max as u64, &redactor))
        .collect();
    for (prepared, source) in prepared.iter().zip([&well_formed, &malformed]) {
        assert_eq!(prepared.retained, withheld);
        assert!(prepared.withheld_whole && !prepared.truncated);
        assert_eq!(prepared.media_type, Media::Json.media_type());
        assert_eq!(prepared.original_bytes, source.len() as u64);
        assert_eq!(
            prepared.source_digest,
            review_telemetry::trace::sha256_digest(source)
        );
    }
    assert_eq!(prepared[0].digest, prepared[1].digest);

    // Through the writer: both occurrences are stored, name one object and read as withheld.
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Content);
    options.content_max_blob_bytes = max as u64;
    let writer = Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    for source in [&well_formed, &malformed] {
        writer.event_with_content(&span, "af.body", Attrs::new(), source, Media::Json);
    }
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let occurrences: Vec<_> = sealed_records(&sealed.path)
        .into_iter()
        .filter_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence),
            _ => None,
        })
        .collect();
    assert_eq!(occurrences.len(), 2);
    let layout = Layout::new(temp.path());
    for occurrence in &occurrences {
        assert_eq!(
            occurrence.digest.as_deref(),
            Some(prepared[0].digest.as_str())
        );
        assert_eq!(occurrence.media_type, Media::Json.media_type());
        assert!(!occurrence.truncated);
        assert_eq!(
            blob_reader::content(&layout, occurrence),
            Content::Bytes(withheld.clone())
        );
    }
}

/// The writer never writes through a link either: a blob whose prefix directory is linked
/// outside the root is dropped with one problem, and nothing is created where the link points.
#[test]
fn telemetry_writer_never_writes_through_a_link() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let options = options(&root, Level::Content);
    let source = b"content through a link\n";
    let prepared = blob::prepare(
        source,
        Media::Text,
        options.content_max_blob_bytes,
        &options.redactor,
    );
    let layout = Layout::new(&root);
    let prefix = layout.blob(&prepared.digest);
    std::fs::create_dir_all(prefix.parent().unwrap().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, prefix.parent().unwrap()).unwrap();
    let writer = Writer::new(options).unwrap();
    let span = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.event_with_content(&span, "af.body", Attrs::new(), source, Media::Text);
    writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing outside"
    );
    let records = sealed_records(&sealed.path);
    let occurrence = records
        .iter()
        .find_map(|record| match record {
            Record::BlobRef(occurrence) => Some(occurrence.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(occurrence.state, BlobState::Dropped);
    let problems: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::Problem(problem) => Some(problem.reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
}
