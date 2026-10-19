//! af's own `tracing` subscriber (design §7.1): span and event records become segment lines.
//!
//! At `off` no subscriber is installed, so a span site costs what `tracing` costs without one:
//! one relaxed atomic load of the global level filter, and nothing is formatted. With a
//! subscriber, concurrent work in one process shares the segment through per-thread span
//! stacks; a span started on a worker thread names its parent explicitly (`parent: &span`), as
//! the kernel hands parent ids to its threads.
//!
//! Field names decide how a value is written: identities, digests and enumerations are typed
//! fields that never pass the credential heuristic ([`typed_kind`]); exact counters are decimal
//! strings, dropped when they are not canonical; everything else is untrusted text. Four fields
//! steer the record instead of becoming attributes: `af.kind` (the span kind), `af.status` (the
//! end status), `af.event` (an event's name) and `message`.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::ThreadId;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes as SpanAttributes, Id, Record as SpanRecord};
use tracing_core::{Dispatch, Event, Interest, Metadata, Subscriber};

use crate::record::{Attr, Attrs, SpanKind, SpanStatus};
use crate::segment::writer::Writer;
use crate::trace;

/// Fields that steer the record.
const KIND_FIELD: &str = "af.kind";
const STATUS_FIELD: &str = "af.status";
const EVENT_FIELD: &str = "af.event";
const MESSAGE_FIELD: &str = "message";

/// How a field's value is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A kernel identity, digest or enumerated value: typed, exempt from the heuristic when it
    /// validates as one.
    Typed,
    /// An exact counter: a canonical decimal string or nothing.
    Decimal,
    /// Untrusted text.
    Text,
}

/// Fields af's schema types as enumerations.
const ENUMERATED: [&str; 16] = [
    "af.run.kind",
    "af.node.kind",
    "af.provider.kind",
    "af.native",
    "af.outcome",
    "af.failure.class",
    "af.check.status",
    "af.check.warm",
    "af.usage.observation",
    "af.usage.unknown_cause",
    "af.usage.source",
    "af.ledger.coverage",
    "af.history",
    "gen_ai.operation.name",
    "gen_ai.provider.name",
    "error.type",
];

/// How the field `name` is written: by af's schema, never by what the value looks like.
pub fn typed_kind(name: &str) -> FieldKind {
    let decimal = (name.starts_with("af.usage.") && name.ends_with("_tokens"))
        || name.starts_with("gen_ai.usage.")
        || name.starts_with("af.charge");
    if decimal || name == "af.store.sequence_at_start" {
        return FieldKind::Decimal;
    }
    let identity = name.ends_with("_id")
        || name.ends_with(".id")
        || name.ends_with("digest")
        || name.ends_with(".manifest")
        || name.ends_with(".fingerprint")
        || matches!(
            name,
            "af.snapshot"
                | "af.policy_rev"
                | "af.worker"
                | "af.provider"
                | "af.corr"
                | "af.phase"
                | "af.pipeline"
                | "af.campaign"
                | "gen_ai.request.model"
                | "gen_ai.response.model"
        );
    if identity || ENUMERATED.contains(&name) {
        FieldKind::Typed
    } else {
        FieldKind::Text
    }
}

fn attr(name: &str, text: String) -> Attr {
    match typed_kind(name) {
        FieldKind::Typed => Attr::Id(text),
        FieldKind::Decimal => Attr::Decimal(text),
        FieldKind::Text => Attr::Text(text),
    }
}

/// Collects one span's or event's fields.
#[derive(Default)]
struct Fields {
    attrs: Attrs,
    kind: Option<SpanKind>,
    status: Option<SpanStatus>,
    event: Option<String>,
    message: Option<String>,
}

impl Fields {
    fn text(&mut self, field: &Field, text: String) {
        match field.name() {
            KIND_FIELD => {
                self.kind = match text.as_str() {
                    "process" => Some(SpanKind::Process),
                    "client" => Some(SpanKind::Client),
                    _ => Some(SpanKind::Internal),
                }
            }
            STATUS_FIELD => {
                self.status = match text.as_str() {
                    "ok" => Some(SpanStatus::Ok),
                    "error" => Some(SpanStatus::Error),
                    _ => Some(SpanStatus::Unset),
                }
            }
            EVENT_FIELD => self.event = Some(text),
            MESSAGE_FIELD => self.message = Some(text),
            name => self.attrs.0.push((name.to_string(), attr(name, text))),
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.text(field, format!("{value:?}"));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        match typed_kind(field.name()) {
            FieldKind::Decimal => self.text(field, value.to_string()),
            _ => self
                .attrs
                .0
                .push((field.name().to_string(), Attr::Int(value))),
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match (typed_kind(field.name()), i64::try_from(value)) {
            (FieldKind::Decimal, _) | (_, Err(_)) => {
                self.attrs
                    .0
                    .push((field.name().to_string(), Attr::Decimal(value.to_string())));
            }
            (_, Ok(value)) => self
                .attrs
                .0
                .push((field.name().to_string(), Attr::Int(value))),
        }
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.attrs
            .0
            .push((field.name().to_string(), Attr::Decimal(value.to_string())));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        match u128::try_from(value) {
            Ok(value) => self.record_u128(field, value),
            Err(_) => self.text(field, value.to_string()),
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attrs
            .0
            .push((field.name().to_string(), Attr::Float(value)));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.attrs
            .0
            .push((field.name().to_string(), Attr::Bool(value)));
    }
}

struct SpanState {
    metadata: &'static Metadata<'static>,
    /// `None` when the writer did not keep the start (the segment limit).
    span_id: Option<String>,
    trace_id: Option<String>,
    depth: usize,
    end: Attrs,
    status: Option<SpanStatus>,
    references: usize,
}

/// The subscriber: one per process, writing through one [`Writer`].
pub struct TelemetrySubscriber {
    writer: Writer,
    next: AtomicU64,
    spans: Mutex<HashMap<u64, SpanState>>,
    stacks: Mutex<HashMap<ThreadId, Vec<u64>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TelemetrySubscriber {
    pub fn new(writer: Writer) -> Self {
        Self {
            writer,
            next: AtomicU64::new(1),
            spans: Mutex::new(HashMap::new()),
            stacks: Mutex::new(HashMap::new()),
        }
    }

    pub fn writer(&self) -> &Writer {
        &self.writer
    }

    fn current(&self) -> Option<u64> {
        lock(&self.stacks)
            .get(&std::thread::current().id())
            .and_then(|stack| stack.last().copied())
    }

    fn parent_of(&self, explicit: Option<&Id>, contextual: bool) -> Option<u64> {
        match explicit {
            Some(id) => Some(id.into_u64()),
            None if contextual => self.current(),
            None => None,
        }
    }
}

/// Install `writer`'s subscriber as the process's global default. Called only at a level that
/// records; at `off` nothing is installed.
pub fn install(writer: Writer) -> Result<(), String> {
    tracing_core::dispatcher::set_global_default(Dispatch::new(TelemetrySubscriber::new(writer)))
        .map_err(|error| error.to_string())
}

impl Subscriber for TelemetrySubscriber {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::always()
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        !self.writer.failed()
    }

    fn new_span(&self, span: &SpanAttributes<'_>) -> Id {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut fields = Fields::default();
        span.record(&mut fields);
        let parent = self.parent_of(span.parent(), span.is_contextual());
        let (parent_span, parent_trace, depth) = parent
            .and_then(|parent| {
                lock(&self.spans).get(&parent).map(|state| {
                    (
                        state.span_id.clone(),
                        state.trace_id.clone(),
                        state.depth + 1,
                    )
                })
            })
            .unwrap_or((None, None, 0));
        let text = |key: &str| {
            fields.attrs.0.iter().find_map(|(name, value)| match value {
                Attr::Id(text) | Attr::Text(text) if name == key => Some(text.clone()),
                _ => None,
            })
        };
        let trace_id = match (text("af.store_id"), text("af.run_id")) {
            (Some(store), Some(run)) => Some(trace::trace_id(&store, &run)),
            _ => parent_trace,
        };
        let span_id = self.writer.span_start(
            trace_id.as_deref(),
            parent_span.as_deref(),
            span.metadata().name(),
            fields.kind.unwrap_or(SpanKind::Internal),
            fields.attrs,
        );
        lock(&self.spans).insert(
            id,
            SpanState {
                metadata: span.metadata(),
                span_id,
                trace_id,
                depth,
                end: Attrs::new(),
                status: fields.status,
                references: 1,
            },
        );
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &SpanRecord<'_>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        if let Some(state) = lock(&self.spans).get_mut(&span.into_u64()) {
            state.end.0.extend(fields.attrs.0);
            if fields.status.is_some() {
                state.status = fields.status;
            }
        }
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(parent) = self.parent_of(event.parent(), event.is_contextual()) else {
            return;
        };
        let Some(span_id) = lock(&self.spans)
            .get(&parent)
            .and_then(|state| state.span_id.clone())
        else {
            return;
        };
        let name = fields
            .event
            .or(fields.message)
            .unwrap_or_else(|| event.metadata().name().to_string());
        self.writer.event(&span_id, &name, fields.attrs);
    }

    fn enter(&self, span: &Id) {
        lock(&self.stacks)
            .entry(std::thread::current().id())
            .or_default()
            .push(span.into_u64());
    }

    fn exit(&self, span: &Id) {
        let mut stacks = lock(&self.stacks);
        let thread = std::thread::current().id();
        if let Some(stack) = stacks.get_mut(&thread) {
            if let Some(at) = stack.iter().rposition(|id| *id == span.into_u64()) {
                stack.remove(at);
            }
            if stack.is_empty() {
                stacks.remove(&thread);
            }
        }
    }

    fn clone_span(&self, span: &Id) -> Id {
        if let Some(state) = lock(&self.spans).get_mut(&span.into_u64()) {
            state.references += 1;
        }
        span.clone()
    }

    fn try_close(&self, span: Id) -> bool {
        let closed = {
            let mut spans = lock(&self.spans);
            let Some(state) = spans.get_mut(&span.into_u64()) else {
                return false;
            };
            state.references = state.references.saturating_sub(1);
            if state.references > 0 {
                return false;
            }
            spans.remove(&span.into_u64())
        };
        if let Some(state) = closed
            && let Some(span_id) = state.span_id
        {
            self.writer.span_end(
                &span_id,
                state.status.unwrap_or(SpanStatus::Unset),
                state.end,
                state.depth,
            );
        }
        true
    }

    fn current_span(&self) -> tracing_core::span::Current {
        let Some(id) = self.current() else {
            return tracing_core::span::Current::none();
        };
        match lock(&self.spans).get(&id) {
            Some(state) => tracing_core::span::Current::new(Id::from_u64(id), state.metadata),
            None => tracing_core::span::Current::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_decide_their_kind() {
        assert_eq!(typed_kind("af.run_id"), FieldKind::Typed);
        assert_eq!(typed_kind("af.context.manifest"), FieldKind::Typed);
        assert_eq!(typed_kind("af.pipeline.digest"), FieldKind::Typed);
        assert_eq!(typed_kind("af.failure.class"), FieldKind::Typed);
        assert_eq!(typed_kind("af.usage.input_tokens"), FieldKind::Decimal);
        assert_eq!(typed_kind("af.failure.reason"), FieldKind::Text);
        assert_eq!(typed_kind("process.command"), FieldKind::Text);
    }
}
