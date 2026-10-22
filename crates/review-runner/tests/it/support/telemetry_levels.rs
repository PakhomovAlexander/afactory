//! A model adapter's Attempt run under each Telemetry level (design §7.3, §15 P6), for the runner
//! crates' test binaries: at `off` with no subscriber at all, at every other level inside an
//! `af.run` span of a writer at that level, so the adapter sees what a recording command gives
//! it. Shared like `native_cancellation.rs`.

use review_telemetry::level::Level;
use review_telemetry::record::{Record, SealExit};
use review_telemetry::segment::writer::{Writer, WriterOptions};
use review_telemetry::subscriber::{TelemetrySubscriber, Typed};

/// What the writer recorded of an Attempt: its records and the segment's text.
pub struct Recorded {
    pub records: Vec<Record>,
    pub text: String,
}

impl Recorded {
    /// The attributes of each ended span named `name`, its start's with its end's over them.
    pub fn ends(&self, name: &str) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let starts: Vec<(&str, &serde_json::Map<String, serde_json::Value>)> = self
            .records
            .iter()
            .filter_map(|record| match record {
                Record::SpanStart(start) if start.name == name => {
                    Some((start.span_id.as_str(), &start.attributes))
                }
                _ => None,
            })
            .collect();
        self.records
            .iter()
            .filter_map(|record| match record {
                Record::SpanEnd(end) => {
                    starts
                        .iter()
                        .find(|(id, _)| *id == end.span_id)
                        .map(|(_, start)| {
                            let mut attributes = (*start).clone();
                            attributes.extend(end.attributes.clone());
                            attributes
                        })
                }
                _ => None,
            })
            .collect()
    }

    /// The attributes of the events named `name`.
    pub fn events(&self, name: &str) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.records
            .iter()
            .filter_map(|record| match record {
                Record::Event(event) if event.name == name => Some(event.attributes.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Every level, `off` first.
pub const LEVELS: [Level; 4] = [Level::Off, Level::Metrics, Level::Spans, Level::Content];

/// Run `work` as a recording command at `level` would: nothing at `off`; otherwise inside an
/// `af.command` > `af.run` of a writer below `root`, whose records come back once the tap has
/// ended.
pub fn at_level<T>(
    level: Level,
    root: &std::path::Path,
    work: impl FnOnce() -> T,
) -> (T, Option<Recorded>) {
    if level == Level::Off {
        return (work(), None);
    }
    let mut options = WriterOptions::new(root, level);
    options.redactor = review_telemetry::redact::Redactor::new();
    options.flush_latency_ms = 50;
    let writer = Writer::new(options).unwrap();
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    let value = tracing::dispatcher::with_default(&dispatch, || {
        let store = Typed::identity("7".repeat(64)).unwrap();
        let run = Typed::identity(format!("task:sha256:{}", "3".repeat(64))).unwrap();
        let command = tracing::info_span!("af.command", process.command = "af");
        let run = tracing::info_span!(
            parent: &command,
            "af.run",
            af.store_id = store.field(),
            af.run_id = run.field(),
            af.run.kind = Typed::enumerated("task").field(),
        );
        let value = {
            let _command = command.enter();
            let _run = run.enter();
            work()
        };
        drop(run);
        drop(command);
        value
    });
    // A flush wakes the consumer, which drains every stream queue (ending each tap that has
    // its decision) before it writes.
    writer.flush();
    let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
    writer.finish(SealExit::Ok).unwrap();
    let records = text
        .lines()
        .map(|line| Record::parse_line(line.as_bytes()).unwrap())
        .collect();
    (value, Some(Recorded { records, text }))
}
