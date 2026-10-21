//! The `tracing` subscriber: spans and events become segment records, with parents from the
//! per-thread span stack or handed over explicitly, and typed fields kept typed (design §7.1).

use review_telemetry::level::Level;
use review_telemetry::record::{Record, SealExit, SpanKind, SpanStatus};
use review_telemetry::subscriber::{TelemetrySubscriber, Typed};

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

#[test]
fn telemetry_identifier_fields_are_untrusted_by_default() {
    const DIGEST: &str = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const TOKEN: &str = "sk-proj-Abc123";
    let temp = tempfile::tempdir().unwrap();
    let writer = writer(temp.path(), Level::Spans);
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    // What the kernel produced and validated, handed over by origin.
    let run = Typed::identity("run-7").unwrap();
    let manifest = Typed::identity(DIGEST).unwrap();
    let tokens = Typed::decimal("18446744073709551616").unwrap();
    let outcome = Typed::enumerated("failed");
    let node = Typed::identity("node-3").unwrap();
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!(
            "af.run",
            af.run_id = run.field(),
            af.context.manifest = manifest.field(),
            af.usage.input_tokens = tokens.field(),
            af.outcome = outcome.field(),
            // Names that look like identities, digests or enumerations earn nothing.
            request_id = TOKEN,
            af.attempt_id = "ghp_FIXTURE1234",
            upstream.id = TOKEN,
            af.pipeline.digest = "0123456789abcdef0123456789abcdef",
            af.failure.class = TOKEN
        );
        let _entered = span.enter();
        tracing::info!(
            af.event = "af.note",
            request_id = TOKEN,
            af.node_id = node.field()
        );
    });
    let sealed = writer.finish(SealExit::Ok).unwrap().unwrap();
    let records = sealed_records(&sealed.path);
    let start = records
        .iter()
        .find_map(|record| match record {
            Record::SpanStart(start) => Some(start.clone()),
            _ => None,
        })
        .unwrap();
    let attributes = &start.attributes;
    for withheld in [
        "request_id",
        "af.attempt_id",
        "upstream.id",
        "af.pipeline.digest",
        "af.failure.class",
    ] {
        assert_eq!(
            attributes[withheld],
            review_telemetry::redact::WITHHELD,
            "{withheld}"
        );
    }
    assert_eq!(attributes["af.redacted"], 5);
    assert_eq!(attributes["af.run_id"], "run-7");
    assert_eq!(attributes["af.context.manifest"], DIGEST);
    assert_eq!(attributes["af.usage.input_tokens"], "18446744073709551616");
    assert_eq!(attributes["af.outcome"], "failed");
    let event = records
        .iter()
        .find_map(|record| match record {
            Record::Event(event) => Some(event.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        event.attributes["request_id"],
        review_telemetry::redact::WITHHELD
    );
    assert_eq!(event.attributes["af.node_id"], "node-3");
    let text = String::from_utf8(crate::sealed_bytes(&sealed.path)).unwrap();
    for planted in ["sk-proj", "ghp_"] {
        assert!(!text.contains(planted), "`{planted}` survived in {text}");
    }
    // A value that does not validate never becomes typed.
    assert!(Typed::identity(TOKEN.replace('-', " ")).is_none());
}
