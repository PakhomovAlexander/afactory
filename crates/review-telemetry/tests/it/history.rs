//! Executions and retained history (design §5): active time as a union, execution time as a
//! sum, open intervals, and `af.history` as `prefix:k` or `partial`, never complete.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use review_telemetry::index::Index;
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::segment::writer::Writer;
use review_telemetry::trace::Execution;
use review_telemetry::view;

use crate::options;

const SECOND: u64 = 1_000_000_000;
const BASE: u64 = 1_800_000_000 * SECOND;

/// A writer whose clock the test sets, with a fixed segment id.
fn clocked(root: &std::path::Path, segment: &str, clock: &Arc<AtomicU64>) -> Writer {
    let mut options = options(root, Level::Spans);
    let clock = clock.clone();
    options.clock = Some(Arc::new(move || u128::from(clock.load(Ordering::SeqCst))));
    options.segment_id = Some(segment.into());
    Writer::new(options).unwrap()
}

/// One execution: `af.command` and `af.run` of `run_id` with `ordinal`, started at `start`
/// seconds; ended at `end` seconds, or left unended when `end` is `None` (the writer abandoned
/// as a killed process would be, after flushing). Returns the run span's id.
fn execution(
    root: &std::path::Path,
    segment: &str,
    run_id: &str,
    ordinal: u64,
    start: u64,
    end: Option<u64>,
) {
    let clock = Arc::new(AtomicU64::new(BASE + start * SECOND));
    let writer = clocked(root, segment, &clock);
    let execution = Execution {
        store_id: "c".repeat(64),
        run_id: run_id.into(),
        execution_id: segment.into(),
        ordinal,
        store_sequence_at_start: ordinal * 10,
    };
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            Some(&execution.trace_id()),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            execution.attributes().enumerated("af.run.kind", "task"),
        )
        .unwrap();
    match end {
        Some(end) => {
            clock.store(BASE + end * SECOND, Ordering::SeqCst);
            writer.span_end(
                &run,
                SpanStatus::Ok,
                Attrs::new().text("af.outcome", "finished"),
                1,
            );
            writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
            writer.finish(SealExit::Ok).unwrap();
        }
        None => {
            clock.store(BASE + (start + 1) * SECOND, Ordering::SeqCst);
            writer.event(&run, "af.note", Attrs::new());
            writer.flush();
            writer.abandon();
        }
    }
}

fn runs(root: &std::path::Path) -> view::Runs {
    let (index, read, unreadable) = view::open(root).unwrap();
    view::runs(&index, root, read, unreadable, None, None).unwrap()
}

/// A paused writer loses its lease, its successor starts, and the original ends later: the two
/// executions overlap, active time counts the overlap once and execution time twice.
#[test]
fn telemetry_executions_overlap_after_takeover() {
    let temp = tempfile::tempdir().unwrap();
    // The original runs 0–100 s; its successor takes the expired lease at 40 s and ends at 70 s.
    execution(
        temp.path(),
        "1800000000000-1-0000000a",
        "task:run",
        1,
        0,
        Some(100),
    );
    execution(
        temp.path(),
        "1800000040000-2-0000000b",
        "task:run",
        2,
        40,
        Some(70),
    );
    let document = runs(temp.path());
    let [run] = document.runs.as_slice() else {
        panic!("{document:?}");
    };
    assert_eq!(run.executions.retained, 2);
    assert_eq!(run.active_us, 100_000_000, "the overlap counts once");
    assert_eq!(run.execution_us, 130_000_000, "af.execution.duration_sum");
    assert_eq!(run.elapsed_us, 100_000_000);
    assert_eq!(run.history, "prefix:2");
    assert_eq!(run.retained, "retained executions 1–2");
    assert!(!run.open);
}

/// An execution whose segment was never sealed ends at its segment's last record and is
/// marked open; recovery keeps it so.
#[test]
fn telemetry_open_interval_ends_at_the_last_record() {
    let temp = tempfile::tempdir().unwrap();
    execution(
        temp.path(),
        "1800000000000-1-0000000a",
        "task:run",
        1,
        0,
        Some(10),
    );
    execution(
        temp.path(),
        "1800000100000-2-0000000b",
        "task:run",
        2,
        100,
        None,
    );
    let document = runs(temp.path());
    let run = &document.runs[0];
    let second = &run.executions.list[1];
    assert!(second.open);
    assert_eq!(second.wall_us, SECOND / 1_000);
    assert_eq!(run.active_us, 11_000_000);
    assert_eq!(run.elapsed_us, 101_000_000);
    assert_eq!(run.coverage, "incomplete");
    // The live execution is not sealed: the prefix is the first one.
    assert_eq!(run.history, "prefix:1");
    review_telemetry::status::reindex(temp.path()).unwrap();
    let recovered = runs(temp.path());
    assert!(recovered.runs[0].executions.list[1].open);
    assert_eq!(recovered.runs[0].history, "prefix:2");
}

/// The latest execution removed before a reindex: `prefix:1`, never complete. A copy of the
/// directory without it, read as another directory, says the same.
#[test]
fn telemetry_history_prefix_only() {
    let temp = tempfile::tempdir().unwrap();
    execution(
        temp.path(),
        "1800000000000-1-0000000a",
        "task:run",
        1,
        0,
        Some(10),
    );
    execution(
        temp.path(),
        "1800000100000-2-0000000b",
        "task:run",
        2,
        100,
        Some(110),
    );
    assert_eq!(runs(temp.path()).runs[0].history, "prefix:2");
    let copy = tempfile::tempdir().unwrap();
    let layout = Layout::new(temp.path());
    let latest = layout.sealed_segment("1800000100000-2-0000000b");
    std::fs::remove_file(&latest).unwrap();
    std::fs::remove_file(layout.refs("1800000100000-2-0000000b")).unwrap();
    review_telemetry::status::reindex(temp.path()).unwrap();
    let document = runs(temp.path());
    assert_eq!(document.runs[0].history, "prefix:1");
    assert_eq!(document.runs[0].retained, "retained executions 1–1");
    assert!(document.runs[0].missing_ordinals.is_empty());
    // A directory copied elsewhere and read on its own answers the same.
    let copied = Layout::new(copy.path());
    std::fs::create_dir_all(copied.segments()).unwrap();
    for name in [
        "1800000000000-1-0000000a.jsonl.gz",
        "1800000000000-1-0000000a.refs",
    ] {
        std::fs::copy(layout.segments().join(name), copied.segments().join(name)).unwrap();
    }
    let read = runs(copy.path());
    assert_eq!(read.runs[0].history, "prefix:1");
    assert_ne!(read.runs[0].history, "complete");
}

/// The first execution evicted: `partial`, naming the missing ordinal.
#[test]
fn telemetry_history_partial_after_eviction() {
    let temp = tempfile::tempdir().unwrap();
    execution(
        temp.path(),
        "1800000000000-1-0000000a",
        "task:run",
        1,
        0,
        Some(10),
    );
    execution(
        temp.path(),
        "1800000100000-2-0000000b",
        "task:run",
        2,
        100,
        Some(110),
    );
    let mut index = Index::open(&Layout::new(temp.path())).unwrap();
    index.ingest().unwrap();
    drop(index);
    let directory = review_telemetry::budget::Directory::measure(temp.path(), false)
        .unwrap()
        .unwrap();
    // Evicted once the hour that protects a just-sealed segment has passed: its last use is
    // its seal time, on the writer's clock.
    let sealed = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(BASE + 10 * SECOND);
    let later =
        sealed.max(std::time::SystemTime::now()) + std::time::Duration::from_secs(2 * 3_600);
    let evicted = review_telemetry::budget::evict(
        temp.path(),
        &directory.target("1800000000000-1-0000000a").unwrap(),
        later,
    )
    .unwrap();
    assert!(matches!(
        evicted,
        review_telemetry::budget::Evicted::Removed(_)
    ));
    let document = runs(temp.path());
    let run = &document.runs[0];
    assert_eq!(run.history, "partial");
    assert_eq!(run.missing_ordinals, vec![1]);
    assert_eq!(run.retained, "partial (missing executions 1)");
    assert_eq!(run.executions.retained, 1);
}

/// One `af.run` of `run_id` in its own segment, its identity attributes `attributes`, started
/// at `start` seconds and ended at `end` with `outcome` and sealed; or, with `end = None`, left
/// running with its records flushed: the returned writer holds the segment live.
fn run_of(
    root: &std::path::Path,
    segment: &str,
    attributes: Attrs,
    trace_id: &str,
    start: u64,
    end: Option<(u64, &str)>,
) -> Option<Writer> {
    let clock = Arc::new(AtomicU64::new(BASE + start * SECOND));
    let writer = clocked(root, segment, &clock);
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let run = writer
        .span_start(
            Some(trace_id),
            Some(&command),
            "af.run",
            SpanKind::Internal,
            attributes.enumerated("af.run.kind", "task"),
        )
        .unwrap();
    let Some((end, outcome)) = end else {
        clock.store(BASE + (start + 1) * SECOND, Ordering::SeqCst);
        writer.event(&run, "af.note", Attrs::new());
        writer.flush();
        return Some(writer);
    };
    clock.store(BASE + end * SECOND, Ordering::SeqCst);
    writer.span_end(
        &run,
        SpanStatus::Ok,
        Attrs::new().text("af.outcome", outcome),
        1,
    );
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap();
    None
}

/// Design §5: the latest execution is the one with the greatest committed ordinal, never the
/// latest start. Ordinal 1 started later and sealed `finished`; ordinal 2, live, started earlier
/// (another machine's clock): the run has no outcome, never ordinal 1's. An execution without
/// an ordinal is listed but never latest, so a run none of whose executions carries one has no
/// outcome. Identical after a reindex.
#[test]
fn telemetry_latest_execution_is_by_ordinal_not_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let identity = |run_id: &str, segment: &str, ordinal: Option<u64>| {
        let execution = Execution {
            store_id: "c".repeat(64),
            run_id: run_id.into(),
            execution_id: segment.into(),
            ordinal: ordinal.unwrap_or(0),
            store_sequence_at_start: 0,
        };
        let attributes = match ordinal {
            Some(_) => execution.attributes(),
            None => Attrs::new()
                .id("af.store_id", execution.store_id.clone())
                .id("af.run_id", execution.run_id.clone())
                .id("af.execution_id", execution.execution_id.clone()),
        };
        (attributes, execution.trace_id())
    };
    let first = "1800000100000-1-0000000a";
    let second = "1800000050000-2-0000000b";
    let unnumbered = "1800000300000-3-0000000c";
    let (attributes, trace) = identity("task:run", first, Some(1));
    run_of(
        temp.path(),
        first,
        attributes,
        &trace,
        100,
        Some((200, "finished")),
    );
    let (attributes, trace) = identity("task:run", second, Some(2));
    let live = run_of(temp.path(), second, attributes, &trace, 50, None).unwrap();
    // Started last of all and sealed with an outcome, but without an ordinal: never latest.
    let (attributes, trace) = identity("task:run", unnumbered, None);
    run_of(
        temp.path(),
        unnumbered,
        attributes,
        &trace,
        300,
        Some((400, "failed")),
    );
    // A run none of whose executions carries an ordinal.
    let alone = "1800000500000-4-0000000d";
    let (attributes, trace) = identity("task:alone", alone, None);
    run_of(
        temp.path(),
        alone,
        attributes,
        &trace,
        500,
        Some((600, "finished")),
    );

    let read = || {
        let document = runs(temp.path());
        let run = |run_id: &str| {
            document
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .unwrap_or_else(|| panic!("{run_id}: {document:?}"))
                .clone()
        };
        (run("task:run"), run("task:alone"))
    };
    let before = read();
    let (run, alone_run) = &before;
    assert_eq!(run.executions.retained, 3, "{run:?}");
    let by_segment = |segment: &str| {
        run.executions
            .list
            .iter()
            .find(|execution| execution.execution_id == segment)
            .unwrap()
            .clone()
    };
    assert_eq!(by_segment(first).ordinal, Some(1));
    assert_eq!(by_segment(first).outcome.as_deref(), Some("finished"));
    assert_eq!(by_segment(second).ordinal, Some(2));
    assert_eq!(by_segment(second).segment, "live");
    assert!(by_segment(second).outcome.is_none());
    assert_eq!(
        by_segment(unnumbered).ordinal,
        None,
        "listed without an ordinal"
    );
    assert_eq!(by_segment(unnumbered).outcome.as_deref(), Some("failed"));
    assert!(
        run.executions.list.first().unwrap().execution_id == second,
        "ordinal 2 started first: {run:?}"
    );
    assert_eq!(
        run.outcome, None,
        "the live ordinal 2 is latest and has no outcome"
    );
    assert_eq!(alone_run.executions.retained, 1);
    assert_eq!(alone_run.outcome, None, "no execution carries an ordinal");

    review_telemetry::status::reindex(temp.path()).unwrap();
    assert_eq!(read(), before, "identical after a reindex");
    drop(live);
}
