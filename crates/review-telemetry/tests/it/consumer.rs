//! The consumer thread (design §6): the segment's sole owner while a run is open, its wake
//! conditions and visibility bound, and its bounded shutdown.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::reader;
use review_telemetry::segment::writer::{self, Writer};
use review_telemetry::trace::Execution;
use review_telemetry::view::{self, TailSelection};

use crate::options;

fn execution(segment: &str) -> Execution {
    Execution {
        store_id: "a".repeat(64),
        run_id: "run-1".into(),
        execution_id: segment.into(),
        ordinal: 1,
        store_sequence_at_start: 0,
    }
}

/// A deep kernel record made while the run's child is silent reaches `af telemetry tail`
/// within `flush_latency_ms` of its creation: the consumer wakes on the append itself, with no
/// timer and no buffer filling up, and writes it.
#[test]
fn telemetry_visibility_latency_holds() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    options.flush_latency_ms = 200;
    // A buffer no record fills: only the consumer's wake writes.
    options.flush_bytes = 16 * 1024 * 1024;
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.flush();
    let segment = writer.segment_id().unwrap();
    let run = writer
        .span_start(
            Some(&execution(&segment).trace_id()),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            execution(&segment).attributes(),
        )
        .unwrap();
    assert!(!writer.consumer_running());
    writer.run_started();
    assert!(writer.consumer_running(), "af.run starts the consumer");

    // `tail` follows the directory, noting when each line reached it.
    let (sender, received) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let root = temp.path().to_path_buf();
    let stopping = stop.clone();
    let tail = std::thread::spawn(move || {
        view::tail(
            &root,
            &TailSelection::default(),
            Duration::from_millis(5),
            &|| stopping.load(Ordering::Relaxed),
            &mut |line| {
                let _ = sender.send((Instant::now(), line.clone()));
                Ok(())
            },
        )
    });
    // A silent child: started, and left without a byte of output while the record is made.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Three spans deep, from a worker thread as the scheduler's nodes are.
    let deep_writer = writer.clone();
    let parent = run.clone();
    let created = std::thread::spawn(move || {
        let node = deep_writer
            .span_start(
                None,
                Some(&parent),
                "af.node",
                SpanKind::Internal,
                Attrs::new(),
            )
            .unwrap();
        let attempt = deep_writer
            .span_start(
                None,
                Some(&node),
                "af.attempt",
                SpanKind::Internal,
                Attrs::new(),
            )
            .unwrap();
        let created = Instant::now();
        deep_writer.event(&attempt, "af.deep", Attrs::new().int("af.depth", 3));
        (created, node, attempt)
    })
    .join()
    .unwrap();
    let (created, node, attempt) = created;
    let arrived = loop {
        let (at, line) = received
            .recv_timeout(Duration::from_secs(5))
            .expect("the deep record reaches tail");
        if line.name.as_deref() == Some("af.deep") {
            break at;
        }
    };
    let latency = arrived.duration_since(created);
    assert!(
        latency < Duration::from_millis(200),
        "visible after {latency:?}, beyond flush_latency_ms"
    );
    // On disk too, in the live segment's valid prefix.
    let live = reader::read_live(&writer.path().unwrap()).unwrap();
    assert!(live.records.iter().any(|record| matches!(
        record,
        review_telemetry::record::Record::Event(event) if event.name == "af.deep"
    )));
    let _ = child.kill();
    let _ = child.wait();

    writer.span_end(&attempt, SpanStatus::Ok, Attrs::new(), 3);
    writer.span_end(&node, SpanStatus::Ok, Attrs::new(), 2);
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    assert!(
        writer.consumer_running(),
        "the run's end leaves the segment to the consumer"
    );
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap().unwrap();
    // `tail` exits once the writer it followed sealed.
    let started = Instant::now();
    while !tail.is_finished() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::Relaxed);
    tail.join().unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "tail exits at the seal"
    );
}

/// A live segment of `run-1` in the Store `store`: its command, its `af.run` and one ended
/// `af.authority.load`, flushed. Returns the writer, the command and run span ids and the
/// segment id.
fn live_run_in(root: &std::path::Path, store: &str) -> (Writer, String, String, String) {
    let writer = Writer::new(options(root, Level::Spans)).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.flush();
    let segment = writer.segment_id().unwrap();
    let execution = Execution {
        store_id: store.into(),
        ..execution(&segment)
    };
    let trace = execution.trace_id();
    let run = writer
        .span_start(
            Some(&trace),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            execution.attributes(),
        )
        .unwrap();
    let load = writer
        .span_start(
            Some(&trace),
            Some(&run),
            "af.authority.load",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.span_end(&load, SpanStatus::Ok, Attrs::new(), 2);
    writer.flush();
    (writer, command, run, segment)
}

/// `tail RUN` follows one Store for its whole lifetime. With no Store known when it begins, the
/// first `af.run` of the run binds it, and a second Store starting the same run id ends the
/// tail as ambiguous, naming both Stores, before any record of the second is emitted; with the
/// Store named (`--store`) the other Store's run is passed over and the tail ends at the seal.
#[test]
fn telemetry_tail_binds_the_store_of_the_run_it_follows() {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    for named in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (a, a_command, a_run, a_segment) = live_run_in(temp.path(), &first);
        let (sender, received) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let root = temp.path().to_path_buf();
        let stopping = stop.clone();
        let selection = TailSelection {
            run_id: Some("run-1".into()),
            store_id: named.then(|| first.clone()),
            store_named: named,
            sealed_baseline: None,
        };
        let tail = std::thread::spawn(move || {
            view::tail(
                &root,
                &selection,
                Duration::from_millis(5),
                &|| stopping.load(Ordering::Relaxed),
                &mut |line| {
                    let _ = sender.send(line.clone());
                    Ok(())
                },
            )
        });
        let line = received
            .recv_timeout(Duration::from_secs(10))
            .expect("tail prints the first Store's record");
        assert_eq!(line.segment_id, a_segment);
        let (b, b_command, b_run, _) = live_run_in(temp.path(), &second);
        if named {
            a.span_end(&a_run, SpanStatus::Ok, Attrs::new(), 1);
            a.span_end(&a_command, SpanStatus::Ok, Attrs::new(), 0);
            a.finish(SealExit::Ok).unwrap().unwrap();
        }
        let started = Instant::now();
        while !tail.is_finished() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        let tailed = tail.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "tail ended");
        let lines: Vec<_> = std::iter::once(line).chain(received.try_iter()).collect();
        assert!(
            lines.iter().all(|line| line.segment_id == a_segment),
            "only the bound Store's records: {lines:?}"
        );
        if named {
            assert_eq!(tailed, view::Tailed::Done);
            assert!(lines.iter().any(|line| line.span == "af.run"), "{lines:?}");
        } else {
            assert_eq!(
                tailed,
                view::Tailed::Ambiguous(vec![first.clone(), second.clone()])
            );
            a.span_end(&a_run, SpanStatus::Ok, Attrs::new(), 1);
            a.span_end(&a_command, SpanStatus::Ok, Attrs::new(), 0);
            a.finish(SealExit::Ok).unwrap().unwrap();
        }
        b.span_end(&b_run, SpanStatus::Ok, Attrs::new(), 1);
        b.span_end(&b_command, SpanStatus::Ok, Attrs::new(), 0);
        b.finish(SealExit::Ok).unwrap().unwrap();
    }
}

/// `tail` follows `root` with `selection` on a thread, every line on the channel; `stop` ends it.
fn following(
    root: &std::path::Path,
    selection: TailSelection,
) -> (
    std::thread::JoinHandle<Result<view::Tailed, String>>,
    mpsc::Receiver<view::TailLine>,
    Arc<AtomicBool>,
) {
    let (sender, received) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (root, stopping) = (root.to_path_buf(), stop.clone());
    let tail = std::thread::spawn(move || {
        view::tail(
            &root,
            &selection,
            Duration::from_millis(5),
            &|| stopping.load(Ordering::Relaxed),
            &mut |line| {
                let _ = sender.send(line.clone());
                Ok(())
            },
        )
    });
    (tail, received, stop)
}

/// Design §6 and §10: a seal record read from the live `.jsonl` completes nothing. The writer's
/// seal line is flushed to the live file and its publication held by the hook: `tail` has read
/// the seal and has not exited. The publication completes: `tail` exits, with every record.
#[test]
fn telemetry_tail_waits_for_the_published_seal() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    let (entered, gate, step) = (held.clone(), release.clone(), steps.clone());
    options.write_hook = Some(Arc::new(move |_| {
        // The first publication hook runs before the manifest's rename, after the seal line.
        if writer::phase() == writer::Phase::Publish && step.fetch_add(1, Ordering::SeqCst) == 0 {
            entered.store(true, Ordering::SeqCst);
            while !gate.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }));
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.run_started();
    writer.event(&run, "af.note", Attrs::new());
    writer.flush();
    let id = writer.segment_id().unwrap();
    let (tail, received, stop) = following(temp.path(), TailSelection::default());
    let first = received
        .recv_timeout(Duration::from_secs(10))
        .expect("tail follows the live segment");
    assert_eq!(first.name.as_deref(), Some("af.note"));

    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let sealing = writer.clone();
    let finished = std::thread::spawn(move || sealing.finish(SealExit::Ok));
    let waiting = Instant::now();
    while !held.load(Ordering::SeqCst) {
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "publication held"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let live = reader::read_live(&writer.path().unwrap()).unwrap();
    assert!(live.sealed(), "the seal line is in the live file");
    // Many looks of tail, well inside the 2 s publication deadline.
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !tail.is_finished(),
        "tail does not exit on a seal record while the publication is held"
    );
    let layout = review_telemetry::layout::Layout::new(temp.path());
    assert!(!layout.sealed_segment(&id).exists());

    release.store(true, Ordering::SeqCst);
    finished.join().unwrap().unwrap().unwrap();
    let started = Instant::now();
    while !tail.is_finished() && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    assert!(tail.is_finished(), "tail exits once the seal is published");
    assert_eq!(tail.join().unwrap().unwrap(), view::Tailed::Done);
    let lines: Vec<_> = std::iter::once(first).chain(received.try_iter()).collect();
    let ended: Vec<&str> = lines
        .iter()
        .filter(|line| line.record == "span_end")
        .map(|line| line.span.as_str())
        .collect();
    assert_eq!(ended, ["af.run", "af.command"], "every record: {lines:?}");
    assert!(lines.iter().all(|line| line.segment_id == id));
}

/// Design §10: a second Store that seals during tail's startup is still detected. Barrier
/// controlled: the run's lookup ([`view::tail_start`]) knows Store A alone; at the barrier
/// Store B starts the same run id and seals its segment; only then does `tail` list the
/// directory for the first time. B's sealed segment is absent from the lookup's baseline, so it
/// is inspected: the tail is refused as ambiguous, naming both Stores, with A's records only.
#[test]
fn telemetry_tail_refuses_a_second_store_sealed_during_startup() {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let temp = tempfile::tempdir().unwrap();
    let (a, a_command, a_run, a_segment) = live_run_in(temp.path(), &first);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (sender, received) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (root, stopping, gate) = (temp.path().to_path_buf(), stop.clone(), barrier.clone());
    let tail = std::thread::spawn(move || {
        let selection = match view::tail_start(&root, Some("run-1"), None)? {
            view::TailStart::Follow(selection) => selection,
            other => return Err(format!("{other:?}")),
        };
        // The lookup is done; B is created and sealed before the first listing.
        gate.wait();
        gate.wait();
        view::tail(
            &root,
            &selection,
            Duration::from_millis(5),
            &|| stopping.load(Ordering::Relaxed),
            &mut |line| {
                let _ = sender.send(line.clone());
                Ok(())
            },
        )
    });
    barrier.wait();
    let (b, b_command, b_run, b_segment) = live_run_in(temp.path(), &second);
    b.span_end(&b_run, SpanStatus::Ok, Attrs::new(), 1);
    b.span_end(&b_command, SpanStatus::Ok, Attrs::new(), 0);
    b.finish(SealExit::Ok).unwrap().unwrap();
    let layout = review_telemetry::layout::Layout::new(temp.path());
    assert!(layout.sealed_segment(&b_segment).exists(), "B is sealed");
    barrier.wait();
    let started = Instant::now();
    while !tail.is_finished() && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    let tailed = tail.join().unwrap().unwrap();
    assert_eq!(
        tailed,
        view::Tailed::Ambiguous(vec![first.clone(), second.clone()])
    );
    let lines: Vec<_> = received.try_iter().collect();
    assert!(!lines.is_empty(), "A's pending record is printed");
    assert!(
        lines.iter().all(|line| line.segment_id == a_segment),
        "nothing of B: {lines:?}"
    );
    a.span_end(&a_run, SpanStatus::Ok, Attrs::new(), 1);
    a.span_end(&a_command, SpanStatus::Ok, Attrs::new(), 0);
    a.finish(SealExit::Ok).unwrap().unwrap();
}

/// A consumer blocked in a write is abandoned at the seal within the 2 s bound: ending the run
/// waits for nothing, and no other thread writes, seals or replaces the segment, which stays
/// `.jsonl` with what was flushed.
#[test]
fn telemetry_consumer_shutdown_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    let writes = Arc::new(AtomicU64::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let (seen, gate) = (writes.clone(), release.clone());
    options.write_hook = Some(Arc::new(move |ordinal| {
        seen.store(ordinal, Ordering::SeqCst);
        // Every write after the first blocks, as on a stalled volume, until the test ends.
        while ordinal > 1 && !gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }));
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.flush();
    let path = writer.path().unwrap();
    let flushed = std::fs::metadata(&path).unwrap().len();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.run_started();
    writer.event(&run, "af.note", Attrs::new());
    let started = Instant::now();
    while writes.load(Ordering::SeqCst) < 2 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let ending = Instant::now();
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    assert!(
        ending.elapsed() < Duration::from_millis(500),
        "ending the run and the command waits for nothing"
    );
    let sealing = Instant::now();
    assert!(
        writer.finish(SealExit::Ok).is_err(),
        "a blocked consumer is abandoned"
    );
    let waited = sealing.elapsed();
    assert!(
        waited >= Duration::from_millis(1_900) && waited < Duration::from_secs(4),
        "{waited:?}"
    );
    assert!(writer.abandoned());
    // Nothing else seals or writes it.
    assert!(writer.finish(SealExit::Ok).is_err());
    assert!(path.exists(), "left unsealed");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), flushed);
    assert!(
        writer
            .problems()
            .iter()
            .any(|(_, reason)| reason.contains("did not stop within 2 s")),
        "{:?}",
        writer.problems()
    );
    release.store(true, Ordering::SeqCst);
}

/// Design §6: a consumer that missed the deadline never publishes a sealed segment. Its write
/// of the compressed copy is held until `finish` has given up at the 2 s bound, then released:
/// the compressed bytes were staged, the deadline is found passed, and the staged files are
/// removed. The live `.jsonl` stays, no `.jsonl.gz` and no `.refs` appear, and a later reader
/// recovers the prefix — every record the consumer wrote, its seal superseded by the recovered
/// one.
#[test]
fn telemetry_released_terminal_write_cannot_seal_after_the_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    let release = Arc::new(AtomicBool::new(false));
    let returned = Arc::new(AtomicBool::new(false));
    let (gate, done) = (release.clone(), returned.clone());
    options.write_hook = Some(Arc::new(move |_| {
        if writer::phase() == writer::Phase::Compressed {
            while !gate.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            done.store(true, Ordering::SeqCst);
        }
        Ok(())
    }));
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.run_started();
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let id = writer.segment_id().unwrap();
    let layout = review_telemetry::layout::Layout::new(temp.path());
    let started = Instant::now();
    assert_eq!(
        writer.finish(SealExit::Ok).unwrap_err(),
        writer::ABANDONED,
        "finish gives up at the bound"
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    // The held write goes through after the deadline.
    release.store(true, Ordering::SeqCst);
    let names = || -> Vec<String> {
        std::fs::read_dir(layout.segments())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let waiting = Instant::now();
    while !(returned.load(Ordering::SeqCst) && names().len() == 1) {
        assert!(
            waiting.elapsed() < Duration::from_secs(10),
            "the released consumer removes what it staged: {:?}",
            names()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Long enough for a consumer that would publish to have done so.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(names(), [format!("{id}.jsonl")], "only the live segment");
    assert!(!layout.sealed_segment(&id).exists());
    assert!(!layout.refs(&id).exists());
    // The process ends: its lock goes with the last handle.
    drop(writer);
    let recovering = Instant::now();
    let recovery = loop {
        match reader::recover(&layout, &id).unwrap() {
            reader::Recovery::Live => {
                assert!(recovering.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(10));
            }
            other => break other,
        }
    };
    assert!(
        matches!(
            recovery,
            reader::Recovery::Recovered {
                discarded_bytes: 0,
                ..
            }
        ),
        "{recovery:?}"
    );
    let records = reader::read_sealed(&layout.sealed_segment(&id)).unwrap();
    let ended: Vec<&str> = records
        .iter()
        .filter_map(|record| match record {
            review_telemetry::record::Record::SpanEnd(end) if end.end_unix_ns.is_some() => {
                Some(end.span_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended, [run.as_str(), command.as_str()], "the whole prefix");
    let Some(review_telemetry::record::Record::Seal(seal)) = records.last() else {
        panic!("recovered with a seal");
    };
    assert_eq!(seal.exit, SealExit::Recovered);
}

/// Design §6: the deadline governs the whole publication, with no exemption for one already
/// under way, and once it has passed nothing of the seal is ever visible under its final name.
/// The publication's hooks all run before its first step, and the deadline check is the last
/// thing before each rename and runs again after it. The hook of the compressed copy's rename
/// is held until `finish` has given up at the 2 s bound, then released, and so is the unlink's.
/// A watcher looks at the directory from the moment `finish` returns until the consumer is done:
/// a `.jsonl.gz` or a `.refs` under its final name at any moment after the deadline fails the
/// test. The consumer finds the deadline passed, removes what it staged and leaves the live
/// segment, whose complete prefix a later reader recovers.
#[test]
fn telemetry_stalled_rename_cannot_seal_after_the_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = options(temp.path(), Level::Spans);
    // One gate per publication step: the manifest's rename (1), the compressed copy's (2) and
    // the live file's unlink (3); each step's hook waits until its gate opens.
    let steps = Arc::new(AtomicU64::new(0));
    let opened = Arc::new(AtomicU64::new(1));
    let (step, gate) = (steps.clone(), opened.clone());
    options.write_hook = Some(Arc::new(move |_| {
        if writer::phase() == writer::Phase::Publish {
            let this = step.fetch_add(1, Ordering::SeqCst) + 1;
            while gate.load(Ordering::SeqCst) < this {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }));
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.run_started();
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let id = writer.segment_id().unwrap();
    let layout = review_telemetry::layout::Layout::new(temp.path());
    let started = Instant::now();
    assert_eq!(
        writer.finish(SealExit::Ok).unwrap_err(),
        writer::ABANDONED,
        "finish returns at the deadline and reports the segment unsealed"
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(writer.abandoned());
    // From here on, past the deadline, a watcher looks for the seal under its final names.
    let done = Arc::new(AtomicBool::new(false));
    let (watching, sealed, refs) = (done.clone(), layout.sealed_segment(&id), layout.refs(&id));
    let watcher = std::thread::spawn(move || {
        let mut seen = Vec::new();
        loop {
            let finished = watching.load(Ordering::SeqCst);
            for path in [&sealed, &refs] {
                if path.exists() {
                    seen.push(path.display().to_string());
                }
            }
            if finished {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    assert_eq!(
        steps.load(Ordering::SeqCst),
        2,
        "held in the compressed rename's hook"
    );
    let unsealed = |what: &str| {
        let listing = reader::list(&layout).unwrap();
        assert_eq!(listing.live, [id.clone()], "{what}: {listing:?}");
        assert!(listing.sealed.is_empty(), "{what}: {listing:?}");
        assert!(listing.refs.is_empty(), "{what}: {listing:?}");
        assert!(listing.stale.is_empty(), "{what}: {listing:?}");
    };
    assert!(layout.live_segment(&id).exists());
    unsealed("the publication held before its first step");

    // The held hooks go through after the deadline; the next check finds it passed.
    opened.store(2, Ordering::SeqCst);
    let waiting = Instant::now();
    while steps.load(Ordering::SeqCst) < 3 {
        assert!(waiting.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    unsealed("the unlink's hook held");
    opened.store(3, Ordering::SeqCst);
    let names = || -> Vec<String> {
        std::fs::read_dir(layout.segments())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let waiting = Instant::now();
    while names().len() != 1 {
        assert!(
            waiting.elapsed() < Duration::from_secs(10),
            "the released consumer removes what it staged: {:?}",
            names()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Long enough for a consumer that would publish to have done so.
    std::thread::sleep(Duration::from_millis(200));
    done.store(true, Ordering::SeqCst);
    let seen = watcher.join().unwrap();
    assert!(
        seen.is_empty(),
        "visible under a final name after the deadline: {seen:?}"
    );
    assert_eq!(names(), [format!("{id}.jsonl")], "only the live segment");
    drop(writer);
    let recovering = Instant::now();
    let recovery = loop {
        match reader::recover(&layout, &id).unwrap() {
            reader::Recovery::Live => {
                assert!(recovering.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(10));
            }
            other => break other,
        }
    };
    assert!(
        matches!(
            recovery,
            reader::Recovery::Recovered {
                discarded_bytes: 0,
                unended_spans: 0,
                ..
            }
        ),
        "{recovery:?}"
    );
    let records = reader::read_sealed(&layout.sealed_segment(&id)).unwrap();
    let ended: Vec<&str> = records
        .iter()
        .filter_map(|record| match record {
            review_telemetry::record::Record::SpanEnd(end) if end.end_unix_ns.is_some() => {
                Some(end.span_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ended,
        [run.as_str(), command.as_str()],
        "the complete prefix"
    );
    let Some(review_telemetry::record::Record::Seal(seal)) = records.last() else {
        panic!("recovered with a seal");
    };
    assert_eq!(
        seal.exit,
        SealExit::Recovered,
        "the writer's seal superseded"
    );
}

/// Design §6: a seal is reported only when the live `.jsonl` itself was unlinked. The live name
/// is replaced by another file just before the publication's last step: the step finds the name
/// no longer holding the writer's file, the staged seal is withdrawn — no `.jsonl.gz` and no
/// `.refs` remain — the writer never says sealed, and `finish` reports the segment unsealed, as
/// the listing does: the replacement is the `.jsonl` under the id, beside nothing sealed.
#[test]
fn telemetry_replaced_live_name_withdraws_the_seal() {
    let temp = tempfile::tempdir().unwrap();
    let layout = review_telemetry::layout::Layout::new(temp.path());
    let mut options = options(temp.path(), Level::Spans);
    let steps = Arc::new(AtomicU64::new(0));
    let (step, segments) = (steps.clone(), layout.segments());
    options.write_hook = Some(Arc::new(move |_| {
        if writer::phase() == writer::Phase::Publish && step.fetch_add(1, Ordering::SeqCst) + 1 == 3
        {
            // The unlink's hook: another file takes the live name.
            let live = std::fs::read_dir(&segments)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
                .unwrap();
            let replacement = segments.join("replacement");
            std::fs::write(&replacement, b"").unwrap();
            std::fs::rename(&replacement, &live).unwrap();
        }
        Ok(())
    }));
    let writer = Writer::new(options).unwrap();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            None,
            Some(&command),
            "af.run",
            SpanKind::Internal,
            Attrs::new(),
        )
        .unwrap();
    writer.run_started();
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.run_ended();
    writer.begin_terminal();
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    let id = writer.segment_id().unwrap();
    let error = writer.finish(SealExit::Ok).unwrap_err();
    assert_eq!(error, writer::REPLACED, "reported unsealed");
    assert_eq!(steps.load(Ordering::SeqCst), 3, "every step's hook ran");
    assert!(writer.failed(), "never sealed");
    assert!(
        writer
            .problems()
            .iter()
            .any(|(_, reason)| reason == writer::REPLACED),
        "{:?}",
        writer.problems()
    );
    assert!(
        !layout.sealed_segment(&id).exists(),
        "the seal is withdrawn"
    );
    assert!(!layout.refs(&id).exists(), "the manifest is withdrawn");
    let mut names: Vec<String> = std::fs::read_dir(layout.segments())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [format!("{id}.jsonl")], "only the replacement");
    drop(writer);
    let listing = reader::list(&layout).unwrap();
    assert_eq!(listing.unsealed, [id.clone()], "{listing:?}");
    assert!(
        listing.sealed.is_empty() && listing.refs.is_empty() && listing.stale.is_empty(),
        "{listing:?}"
    );
}
