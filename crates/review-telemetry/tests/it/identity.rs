//! Trace and execution identities (design §5): a run is namespaced by its Store instance.

use review_telemetry::index::{self, Index};
use review_telemetry::layout::Layout;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
use review_telemetry::trace::{self, Execution};

use crate::writer;

#[test]
fn telemetry_trace_namespace_separates_stores() {
    let temp = tempfile::tempdir().unwrap();
    let telemetry = temp.path().join("telemetry");
    let store_a = temp.path().join("stores/a");
    let store_b = temp.path().join("stores/b");
    std::fs::create_dir_all(&store_a).unwrap();
    std::fs::create_dir_all(&store_b).unwrap();
    // `task_run_id` hashes the Task ID alone: the same run id in two Stores.
    let run_id = "taskrun-0f1e2d3c";
    let a = trace::store_id(&store_a).unwrap();
    let b = trace::store_id(&store_b).unwrap();
    assert_ne!(a, b);
    assert_eq!(a.len(), 64);
    // The canonical path: another spelling of the same directory is the same Store.
    assert_eq!(trace::store_id(&temp.path().join("stores/./a")).unwrap(), a);
    assert_ne!(trace::trace_id(&a, run_id), trace::trace_id(&b, run_id));

    // Three executions: one in Store B, and in Store A a first run and its resume.
    let mut executions = Vec::new();
    for (store, ordinal) in [(&a, 1), (&b, 1), (&a, 2)] {
        let writer = writer(&telemetry, Level::Spans);
        let span = writer
            .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
            .unwrap();
        let execution = Execution {
            store_id: store.clone(),
            run_id: run_id.into(),
            execution_id: writer.segment_id().unwrap(),
            ordinal,
            store_sequence_at_start: ordinal * 10,
        };
        let run = writer
            .span_start(
                Some(&execution.trace_id()),
                Some(&span),
                "af.run",
                SpanKind::Internal,
                execution.attributes(),
            )
            .unwrap();
        writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
        writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
        writer.finish(SealExit::Ok).unwrap();
        executions.push(execution);
    }
    assert_eq!(
        executions[0].trace_id(),
        executions[2].trace_id(),
        "a resume joins its trace"
    );
    assert_ne!(executions[0].trace_id(), executions[1].trace_id());
    assert_ne!(executions[0].execution_id, executions[2].execution_id);

    let layout = Layout::new(&telemetry);
    index::replace(&layout).unwrap();
    let index = Index::open(&layout).unwrap();
    assert_eq!(index.count("SELECT count(*) FROM runs").unwrap(), 2);
    assert_eq!(index.count("SELECT count(*) FROM executions").unwrap(), 3);
    assert_eq!(
        index
            .count(&format!(
                "SELECT count(*) FROM executions WHERE trace_id = '{}'",
                executions[0].trace_id()
            ))
            .unwrap(),
        2
    );
    assert_eq!(
        index
            .count("SELECT count(DISTINCT execution_id) FROM executions")
            .unwrap(),
        3
    );
}
