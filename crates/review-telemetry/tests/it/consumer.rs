//! The consumer thread (design §6): the segment's sole owner while a run is open, its wake
//! conditions and visibility bound, and its bounded shutdown.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::reader;
use review_telemetry::segment::writer::Writer;
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
    assert!(writer.run_ended(), "the consumer stops within the bound");
    assert!(
        !writer.consumer_running(),
        "the run's end stops the consumer"
    );
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

/// A consumer blocked in a write is abandoned at the run's end within the 2 s bound: no other
/// thread writes, seals or replaces the segment, which stays `.jsonl` with what was flushed.
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
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    let stopping = Instant::now();
    assert!(!writer.run_ended(), "a blocked consumer is abandoned");
    let waited = stopping.elapsed();
    assert!(
        waited >= Duration::from_millis(1_900) && waited < Duration::from_secs(4),
        "{waited:?}"
    );
    assert!(writer.abandoned());
    // Nothing else seals or writes it.
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
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
