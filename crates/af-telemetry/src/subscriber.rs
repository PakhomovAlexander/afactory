//! af's own `tracing` subscriber (design §7.1): span and event records become segment lines.
//!
//! At `off` no subscriber is installed, so a span site costs what `tracing` costs without one:
//! one relaxed atomic load of the global level filter, and nothing is formatted. With a
//! subscriber, concurrent work in one process shares the segment through per-thread span
//! stacks; a span started on a worker thread names its parent explicitly (`parent: &span`), as
//! the kernel hands parent ids to its threads.
//!
//! A field is untrusted text by default, whatever its name: a name says nothing about where its
//! value came from, so `request_id = "sk-proj-…"` passes the credential rules like any free text.
//! Only a value the kernel produced and validated, handed over as a [`Typed`] value
//! (`af.run_id = run.field()`), is written as a typed field that never passes the heuristic; the
//! subscriber recognises it by its type, never by the field's name or the value's shape. Integers
//! and booleans are typed by their Rust type; an integer under an exact counter's name is written
//! as a canonical decimal string (ADR-0078). Four fields steer the record instead of becoming
//! attributes: `af.kind` (the span kind), `af.status` (the end status), `af.event` (an event's
//! name) and `message`; the event name is free text too and the writer judges it whole.
//!
//! The process's first `af.run` span starts the consumer thread ([`Writer::run_started`]), which
//! then owns the segment until [`Writer::finish`] seals it or abandons it at the bound; an
//! `af.run` that ends ([`Writer::run_ended`]) leaves it running. At `metrics` only the spans
//! that level lists are recorded ([`METRICS_SPANS`]).

use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::ThreadId;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes as SpanAttributes, Id, Record as SpanRecord};
use tracing_core::{Dispatch, Event, Interest, Metadata, Subscriber};

use crate::level::Level;
use crate::record::{Attr, Attrs, SpanKind, SpanStatus};
use crate::redact;
use crate::segment::writer::Writer;
use crate::trace;

/// Fields that steer the record.
const KIND_FIELD: &str = "af.kind";
const STATUS_FIELD: &str = "af.status";
const EVENT_FIELD: &str = "af.event";
const MESSAGE_FIELD: &str = "message";

/// The span a process opens for the time it spends on one run; the first one starts the
/// consumer thread.
pub const RUN_SPAN: &str = "af.run";

/// The span of one Attempt: content below it counts against its allowance (design §8).
pub const ATTEMPT_SPAN: &str = "af.attempt";

/// The spans recorded at `metrics` (design §8); every other span and every event is recorded
/// from `spans` on.
pub const METRICS_SPANS: [&str; 8] = [
    "af.command",
    "af.run",
    "af.gate",
    "af.check",
    "af.node",
    "af.attempt",
    "af.harness",
    "af.sweep",
];

/// Whether `metadata` is recorded at `level`: at `metrics` a span only when that level lists it;
/// an event whenever its site emits one (the sites that emit at `metrics` are the accounting
/// ones).
pub fn recorded_at(level: Level, metadata: &Metadata<'_>) -> bool {
    match level {
        Level::Off => false,
        Level::Metrics => !metadata.is_span() || METRICS_SPANS.contains(&metadata.name()),
        Level::Spans | Level::Content => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TypedValue {
    Identity(String),
    Decimal(String),
    Enumerated(&'static str),
}

/// A value the kernel produced and validated, for a `tracing` field: a digest or an identity (a
/// run, Attempt, node, segment or span id), an exact counter, or an enumerated value. It is the
/// one way a field earns the typed exemption of design §8; the constructors validate, so a value
/// that is not one of these kinds never becomes one.
///
/// ```text
/// let run = Typed::identity(run_id).expect("a kernel run id");
/// tracing::info_span!("af.run", af.run_id = run.field());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed(TypedValue);

impl Typed {
    /// A digest or identity that has the identity shape ([`redact::is_identity`]). It is kept
    /// as given only in an identity position and when it is exactly a kernel identity
    /// ([`crate::record::is_identity_position`], [`redact::is_kernel_identity`]); otherwise it
    /// is judged as untrusted text.
    pub fn identity(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        redact::is_identity(&value).then_some(Self(TypedValue::Identity(value)))
    }

    /// An exact counter as a canonical decimal string ([`redact::is_canonical_decimal`]).
    pub fn decimal(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        redact::is_canonical_decimal(&value).then_some(Self(TypedValue::Decimal(value)))
    }

    /// A value from one of af's closed enumerations.
    pub fn enumerated(value: &'static str) -> Self {
        Self(TypedValue::Enumerated(value))
    }

    /// The value as a `tracing` field value. `tracing`'s `Value` is sealed, so the value
    /// travels as an error object the subscriber recognises by its type.
    pub fn field(&self) -> &(dyn std::error::Error + 'static) {
        self
    }

    fn attr(&self) -> Attr {
        match &self.0 {
            TypedValue::Identity(value) => Attr::Id(value.clone()),
            TypedValue::Decimal(value) => Attr::Decimal(value.clone()),
            TypedValue::Enumerated(value) => Attr::Enum(value),
        }
    }

    fn text(&self) -> &str {
        match &self.0 {
            TypedValue::Identity(value) | TypedValue::Decimal(value) => value,
            TypedValue::Enumerated(value) => value,
        }
    }
}

impl Display for Typed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.text())
    }
}

impl std::error::Error for Typed {}

/// Whether an integer field is an exact counter, written as a canonical decimal string. This
/// decides only the spelling of a number, never an exemption for text.
pub fn decimal_counter(name: &str) -> bool {
    crate::record::is_exact_counter(name)
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
            name => self.attrs.0.push((name.to_string(), Attr::Text(text))),
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

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        match value.downcast_ref::<Typed>() {
            Some(typed)
                if !matches!(
                    field.name(),
                    KIND_FIELD | STATUS_FIELD | EVENT_FIELD | MESSAGE_FIELD
                ) =>
            {
                self.attrs.0.push((field.name().to_string(), typed.attr()));
            }
            Some(typed) => self.text(field, typed.to_string()),
            None => self.text(field, value.to_string()),
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        match u64::try_from(value) {
            Ok(value) if decimal_counter(field.name()) => self.record_u64(field, value),
            _ => self
                .attrs
                .0
                .push((field.name().to_string(), Attr::Int(value))),
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match i64::try_from(value) {
            Ok(value) if !decimal_counter(field.name()) => self
                .attrs
                .0
                .push((field.name().to_string(), Attr::Int(value))),
            _ => self
                .attrs
                .0
                .push((field.name().to_string(), Attr::Decimal(value.to_string()))),
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

/// The attributes every span of a run names (design §5): a span that does not name one itself
/// takes its parent's, so a phase a crate below the runtime records (a check, a git operation,
/// a Store append) names the Store, run, kind and execution it worked for without knowing them.
pub const RUN_IDENTITY: [&str; 4] = ["af.store_id", "af.run_id", "af.run.kind", "af.execution_id"];

struct SpanState {
    metadata: &'static Metadata<'static>,
    /// `None` when the writer did not keep the start (the segment limit).
    span_id: Option<String>,
    trace_id: Option<String>,
    /// The [`RUN_IDENTITY`] attributes the span named or took from its parent.
    identity: Vec<(String, Attr)>,
    depth: usize,
    /// The `span_id` of the nearest `af.attempt` at or above the span: whose content allowance
    /// content on it counts against (design §8).
    attempt: Option<String>,
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

    fn recorded(&self, id: u64) -> Option<RecordedSpan> {
        lock(&self.spans).get(&id).and_then(|state| {
            state.span_id.clone().map(|span_id| RecordedSpan {
                span_id,
                trace_id: state.trace_id.clone(),
            })
        })
    }

    fn identity_of(&self, id: u64) -> Option<SpanIdentity> {
        lock(&self.spans).get(&id).and_then(|state| {
            state.span_id.clone().map(|span_id| SpanIdentity {
                span: RecordedSpan {
                    span_id,
                    trace_id: state.trace_id.clone(),
                },
                identity: state.identity.clone(),
                depth: state.depth,
            })
        })
    }

    fn parent_of(&self, explicit: Option<&Id>, contextual: bool) -> Option<u64> {
        match explicit {
            Some(id) => Some(id.into_u64()),
            None if contextual => self.current(),
            None => None,
        }
    }
}

/// A span the subscriber recorded: its `span_id` in the segment and the run's trace, when the
/// span or an ancestor named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSpan {
    pub span_id: String,
    pub trace_id: Option<String>,
}

/// The record of `span`, when the subscriber it was created under is this crate's and kept its
/// start; `None` for a disabled span. Records written beside the span (an event on it, a
/// `finding` naming it) use this id.
pub fn recorded(span: &tracing::Span) -> Option<RecordedSpan> {
    span.with_subscriber(|(id, dispatch)| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .and_then(|subscriber| subscriber.recorded(id.into_u64()))
    })
    .flatten()
}

/// A recorded span with the run identity it named or took from its parent ([`RUN_IDENTITY`]) and
/// its depth (the root is 0): what a record written beside the subscriber — a harness stream's
/// `af.turn` and `af.tool` spans — names to belong to the same run below it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanIdentity {
    pub span: RecordedSpan,
    pub identity: Vec<(String, Attr)>,
    pub depth: usize,
}

/// Whose content allowance content on `span` counts against: the Attempt of its nearest
/// `af.attempt` (that span itself included), else the process's.
pub fn content_scope(span: &tracing::Span) -> crate::content::Scope {
    span.with_subscriber(|(id, dispatch)| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .and_then(|subscriber| {
                lock(&subscriber.spans)
                    .get(&id.into_u64())
                    .and_then(|state| state.attempt.clone())
            })
    })
    .flatten()
    .map_or(
        crate::content::Scope::Process,
        crate::content::Scope::Attempt,
    )
}

/// The [`SpanIdentity`] of `span`, when this crate's subscriber kept its start.
pub fn span_identity(span: &tracing::Span) -> Option<SpanIdentity> {
    span.with_subscriber(|(id, dispatch)| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .and_then(|subscriber| subscriber.identity_of(id.into_u64()))
    })
    .flatten()
}

/// The record of the current thread's innermost entered span under the current dispatcher.
pub fn current_recorded() -> Option<RecordedSpan> {
    tracing_core::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .and_then(|subscriber| subscriber.current().and_then(|id| subscriber.recorded(id)))
    })
}

/// The writer of the subscriber installed as the process's global default.
static INSTALLED: std::sync::OnceLock<Writer> = std::sync::OnceLock::new();

/// Install `writer`'s subscriber as the process's global default. Called only at a level that
/// records; at `off` nothing is installed.
pub fn install(writer: Writer) -> Result<(), String> {
    tracing_core::dispatcher::set_global_default(Dispatch::new(TelemetrySubscriber::new(
        writer.clone(),
    )))
    .map_err(|error| error.to_string())?;
    let _ = INSTALLED.set(writer);
    Ok(())
}

/// The writer the current dispatcher records through: the scoped default a fixture set, or the
/// process's global one; `None` at `off`. The records that are not spans or events (`node`) are
/// written through it.
pub fn current_writer() -> Option<Writer> {
    tracing_core::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .map(|subscriber| subscriber.writer().clone())
    })
    .or_else(|| INSTALLED.get().cloned())
    .filter(|writer| !writer.failed())
}

/// Whether the span current on this thread works for a run: it names, or took from its parent,
/// an `af.run_id` (design §5). A phase of §7.1 that belongs to a run — a Store operation, a git
/// process, a capture — is recorded only then, so the same phase before a run opens, or on a
/// thread no span was handed to, never becomes a span of no run. `false` at `off`.
pub fn in_run() -> bool {
    tracing_core::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<TelemetrySubscriber>()
            .is_some_and(TelemetrySubscriber::current_in_run)
    })
}

impl TelemetrySubscriber {
    fn current_in_run(&self) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        lock(&self.spans)
            .get(&current)
            .is_some_and(|state| state.identity.iter().any(|(key, _)| key == "af.run_id"))
    }
}

/// The process's `af.command` span, which every `af.run` names as its parent whichever thread
/// opens it.
static COMMAND: Mutex<Option<tracing::Span>> = Mutex::new(None);

/// Remember the process's `af.command` span.
pub fn set_command_span(span: &tracing::Span) {
    *lock(&COMMAND) = Some(span.clone());
}

/// Forget the process's `af.command` span, before it ends.
pub fn take_command_span() -> Option<tracing::Span> {
    lock(&COMMAND).take()
}

/// The process's `af.command` span, when one is open.
pub fn command_span() -> Option<tracing::Span> {
    lock(&COMMAND).clone()
}

/// The process's `af.command` span's id, when one is open and recorded.
pub fn command_span_id() -> Option<Id> {
    lock(&COMMAND).as_ref().and_then(tracing::Span::id)
}

impl Subscriber for TelemetrySubscriber {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if recorded_at(self.writer.level(), metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        recorded_at(self.writer.level(), metadata) && !self.writer.failed()
    }

    fn new_span(&self, span: &SpanAttributes<'_>) -> Id {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut fields = Fields::default();
        span.record(&mut fields);
        let parent = self.parent_of(span.parent(), span.is_contextual());
        let (parent_span, parent_trace, parent_identity, depth, parent_attempt) = parent
            .and_then(|parent| {
                lock(&self.spans).get(&parent).map(|state| {
                    (
                        state.span_id.clone(),
                        state.trace_id.clone(),
                        state.identity.clone(),
                        state.depth + 1,
                        state.attempt.clone(),
                    )
                })
            })
            .unwrap_or((None, None, Vec::new(), 0, None));
        // A run identity the span does not name is its parent's, written first so the record
        // bound never drops it before the span's own attributes.
        let inherited: Vec<(String, Attr)> = parent_identity
            .into_iter()
            .filter(|(key, _)| !fields.attrs.0.iter().any(|(name, _)| name == key))
            .collect();
        fields.attrs.0.splice(0..0, inherited);
        let identity: Vec<(String, Attr)> = RUN_IDENTITY
            .iter()
            .filter_map(|key| fields.attrs.0.iter().find(|(name, _)| name == key).cloned())
            .collect();
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
        if span.metadata().name() == RUN_SPAN && span_id.is_some() {
            self.writer.run_started();
        }
        let attempt = if span.metadata().name() == ATTEMPT_SPAN {
            span_id.clone()
        } else {
            parent_attempt
        };
        lock(&self.spans).insert(
            id,
            SpanState {
                metadata: span.metadata(),
                span_id,
                trace_id,
                identity,
                depth,
                attempt,
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
            if state.metadata.name() == RUN_SPAN {
                self.writer.run_ended();
            }
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
    fn typed_values_validate_and_names_only_spell_numbers() {
        assert!(Typed::identity("run-1").is_some());
        assert!(Typed::identity(format!("sha256:{}", "ab".repeat(32))).is_some());
        assert!(Typed::identity("has space").is_none());
        assert!(Typed::decimal("18446744073709551616").is_some());
        assert!(Typed::decimal("01").is_none());
        assert_eq!(Typed::enumerated("failed").to_string(), "failed");
        assert!(decimal_counter("af.usage.input_tokens"));
        assert!(decimal_counter("gen_ai.usage.output_tokens"));
        assert!(!decimal_counter("af.run_id"));
        assert!(!decimal_counter("af.failure.reason"));
    }
}
