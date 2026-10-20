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
