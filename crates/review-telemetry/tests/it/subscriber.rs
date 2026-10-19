//! The `tracing` subscriber: spans and events become segment records, with parents from the
//! per-thread span stack or handed over explicitly, and typed fields kept typed (design §7.1).

use review_telemetry::level::Level;
use review_telemetry::record::{Record, SealExit, SpanKind, SpanStatus};
use review_telemetry::subscriber::TelemetrySubscriber;

use crate::{sealed_records, writer};

#[test]
fn spans_and_events_become_records_with_their_parents() {
    let temp = tempfile::tempdir().unwrap();
    let writer = writer(temp.path(), Level::Spans);
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    let store = "e".repeat(64);
    tracing::dispatcher::with_default(&dispatch, || {
        let command = tracing::info_span!("af.command");
        let _command = command.enter();
        let run = tracing::info_span!(
            "af.run",
            af.store_id = store.as_str(),
            af.run_id = "run-1",
            af.status = tracing::field::Empty,
            af.outcome = tracing::field::Empty
        );
        let entered = run.enter();
        tracing::info!(
            af.event = "af.store.event",
            af.store.event.sequence = 3_u64,
            "ignored"
        );
        // A worker thread names its parent explicitly.
        let parent = run.clone();
        let worker = dispatch.clone();
        std::thread::spawn(move || {
            tracing::dispatcher::with_default(&worker, || {
                let node = tracing::info_span!(parent: &parent, "af.node", af.kind = "process");
                let _node = node.enter();
                tracing::info!(af.event = "af.note", af.text = "API_KEY=sk-FIXTURE");
            });
        })
        .join()
        .unwrap();
        run.record("af.status", "error");
        run.record("af.outcome", "failed");
        drop(entered);
        drop(run);
    });
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let records = sealed_records(&sealed.path);
    let starts: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::SpanStart(start) => Some(start.clone()),
            _ => None,
        })
        .collect();
    let names: Vec<&str> = starts.iter().map(|start| start.name.as_str()).collect();
    assert_eq!(names, ["af.command", "af.run", "af.node"]);
    let trace = review_telemetry::trace::trace_id(&store, "run-1");
    assert_eq!(starts[0].trace_id, None);
    assert_eq!(
        starts[1].parent_span_id.as_deref(),
        Some(starts[0].span_id.as_str())
    );
    assert_eq!(starts[1].trace_id.as_deref(), Some(trace.as_str()));
    assert_eq!(
        starts[2].parent_span_id.as_deref(),
        Some(starts[1].span_id.as_str())
    );
    assert_eq!(starts[2].trace_id.as_deref(), Some(trace.as_str()));
    assert_eq!(starts[2].kind, SpanKind::Process);
    assert_eq!(starts[1].attributes["af.run_id"], "run-1");
    let events: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) => Some(event.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(events[0].name, "af.store.event");
    assert_eq!(events[0].span_id, starts[1].span_id);
    assert_eq!(events[0].attributes["af.store.event.sequence"], 3);
    assert_eq!(events[1].name, "af.note");
    assert_eq!(events[1].span_id, starts[2].span_id);
    assert_eq!(
        events[1].attributes["af.text"],
        review_telemetry::redact::WITHHELD
    );
    let run_end = records
        .iter()
        .find_map(|record| match record {
            Record::SpanEnd(end) if end.span_id == starts[1].span_id => Some(end.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(run_end.status, SpanStatus::Error);
    assert_eq!(run_end.attributes["af.outcome"], "failed");
    // The command span ended last (it was entered first and closed when the guard dropped).
    assert!(
        records.iter().any(
            |record| matches!(record, Record::SpanEnd(end) if end.span_id == starts[0].span_id)
        )
    );
}
