//! A harness's streams for Telemetry (design §7.3): the supervisor's output observer feeding
//! Telemetry's tap.
//!
//! `review-process` links no telemetry (ADR-0026), so this crate, which receives the
//! supervisor's results, joins the two: [`Observation::start`] makes a bounded observer queue
//! (`[telemetry] observer_queue_bytes`) whose drain offers wake Telemetry's consumer thread, and
//! attaches it to the writer as a stream queue the consumer drains into a
//! [`review_telemetry::stream::Tap`]. The supervisor returns as soon as its drain ends; the
//! consumer finishes the process's queued chunks afterwards, and only once it has every chunk
//! and this process's release decision does it end the tap, which at `content` publishes the
//! released content as blobs counted against the Attempt's content allowance.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use review_process::{ObservedOutput, ObservedStream, OutputObserver};
use review_telemetry::segment::writer::{StreamQueue, Writer};
use review_telemetry::stream::{Harness, Release, Stream, Tap, TapOptions};

/// What a harness's stdout is, for the tap: which harness writes it and the invocation's own
/// tool set, the only tool names Telemetry records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessStream {
    pub harness: Harness,
    pub tools: Vec<String>,
}

impl HarnessStream {
    /// `claude -p --output-format stream-json --verbose` with `--tools` granting `tools`
    /// (comma-separated, as the adapter passes them).
    pub fn claude(tools: &str) -> Self {
        Self {
            harness: Harness::Claude,
            tools: tools.split(',').map(str::to_string).collect(),
        }
    }

    /// `codex exec --json`: its tool calls are the `command_execution` and `mcp_tool_call`
    /// items.
    pub fn codex() -> Self {
        Self {
            harness: Harness::Codex,
            tools: vec!["command_execution".into(), "mcp_tool_call".into()],
        }
    }
}

/// Whether Telemetry records harness streams in this process now: a writer at `spans` or
/// `content` that has not failed. What a harness is asked to report may follow it (design §7.3
/// lists every such flag); nothing it is asked to do does.
pub fn records_streams() -> bool {
    review_telemetry::subscriber::current_writer()
        .is_some_and(|writer| Tap::records(writer.level()))
}

/// Split `bytes` into its lines, each with its newline when it has one.
pub fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|byte| *byte == b'\n')
}

/// The `type` of a line that is one JSON object naming a string `type` (a stream message);
/// `None` for any other line.
pub fn message_type(line: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Message<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<std::borrow::Cow<'a, str>>,
    }
    let trimmed = line.trim_ascii();
    if !trimmed.starts_with(b"{") {
        return None;
    }
    serde_json::from_slice::<Message<'_>>(trimmed)
        .ok()
        .and_then(|message| message.kind)
        .map(std::borrow::Cow::into_owned)
}

/// The most chunks one drain takes before the consumer writes and looks again.
const CHUNKS_PER_DRAIN: usize = 256;

/// The consumer's end: the observer queue, the tap, and the release decision once taken.
struct Queue {
    output: ObservedOutput,
    tap: Mutex<Tap>,
    release: Mutex<Option<Release>>,
    done: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl StreamQueue for Queue {
    fn drain(&self, writer: &Writer) -> bool {
        let mut tap = lock(&self.tap);
        if tap.finished() {
            return true;
        }
        // Read before taking: every chunk queued before the close is taken below.
        let closed = self.output.closed();
        for _ in 0..CHUNKS_PER_DRAIN {
            let Some(chunk) = self.output.try_take() else {
                break;
            };
            let stream = match chunk.stream {
                ObservedStream::Stdout => Stream::Stdout,
                ObservedStream::Stderr => Stream::Stderr,
            };
            let arrival = writer.unix_ns_at(chunk.arrival);
            tap.chunk(writer, stream, chunk.byte_offset, &chunk.bytes, arrival);
        }
        if self.output.has_queued() {
            return false;
        }
        if closed && let Some(release) = *lock(&self.release) {
            // At `content` the tap publishes the released content as blobs itself.
            let _released = tap.finish(writer, &release);
            // The observer's drops of this process, in the segment's seal too.
            writer.count_dropped_chunks(self.output.dropped_chunks());
            self.done.store(true, Ordering::Release);
        }
        true
    }

    fn discard(&self) -> u64 {
        let mut left = 0;
        while self.output.try_take().is_some() {
            left += 1;
        }
        left + self.output.dropped_chunks()
    }

    fn has_pending(&self) -> bool {
        !self.done.load(Ordering::Acquire)
            && (self.output.has_queued() || (self.output.closed() && lock(&self.release).is_some()))
    }

    fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// One harness process observed for Telemetry.
pub(crate) struct Observation {
    writer: Writer,
    observer: OutputObserver,
    queue: Arc<Queue>,
}

impl Observation {
    /// Observe the process of the `af.harness` span `span` when Telemetry records harness
    /// streams (`spans` and above) and kept the span: `None` otherwise, and the process runs
    /// as it would without Telemetry.
    pub(crate) fn start(
        span: &tracing::Span,
        stream: &HarnessStream,
        provider: &'static str,
        bound_model: Option<String>,
        grants: Vec<(String, String)>,
    ) -> Option<Self> {
        let writer = review_telemetry::subscriber::current_writer()?;
        if !Tap::records(writer.level()) {
            return None;
        }
        let parent = review_telemetry::subscriber::span_identity(span)?;
        let options = writer.options();
        let tap = Tap::new(
            &writer,
            TapOptions {
                harness: stream.harness,
                parent,
                provider,
                bound_model,
                tools: stream.tools.clone(),
                frame_max_bytes: options.content_max_blob_bytes,
                content_allowance_bytes: options.content_attempt_max_bytes,
                grants,
                content_reasoning: options.content_reasoning,
                content_tool_io: options.content_tool_io,
                scope: review_telemetry::subscriber::content_scope(span),
            },
        );
        let (observer, output) =
            review_process::observer(options.observer_queue_bytes, writer.consumer_thread());
        let queue = Arc::new(Queue {
            output,
            tap: Mutex::new(tap),
            release: Mutex::new(None),
            done: AtomicBool::new(false),
        });
        writer.attach_stream(Arc::clone(&queue) as Arc<dyn StreamQueue>);
        Some(Self {
            writer,
            observer,
            queue,
        })
    }

    pub(crate) fn observer(&self) -> &OutputObserver {
        &self.observer
    }

    /// The supervisor returned: nothing more is offered.
    pub(crate) fn close(&self) {
        self.observer.close();
    }

    /// The release decision for this process: the consumer ends the tap once it has drained
    /// every queued chunk. Never waits on the consumer.
    pub(crate) fn release(&self, release: Release) {
        *lock(&self.queue.release) = Some(release);
        self.writer.stream_chunk();
    }
}
