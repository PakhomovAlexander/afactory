//! The content level (design §1.3, §6 Blobs, §7.3–§7.5, §8; package P7): what `content` keeps
//! as blobs beside the structure every level records, and the allowances that bound it.
//!
//! At `content`, and only there, the writer stores as blobs, each named by one event:
//!
//! - the composed Worker input of `af.context.compose`: the prompt bytes the Worker receives
//!   ([`event::INPUT`], `gen_ai.content.input`) and the context manifest the kernel composed
//!   them by ([`event::SYSTEM`], `gen_ai.content.system`);
//! - from a harness stream, only after Telemetry's own release decision and never live (design
//!   §7.3): assistant `text` blocks and Codex `agent_message` items ([`event::OUTPUT`],
//!   `gen_ai.content.output`), `thinking` blocks and `reasoning` items when `content_reasoning`
//!   ([`event::REASONING`]), tool arguments and results when `content_tool_io`
//!   ([`event::TOOL_INPUT`], [`event::TOOL_OUTPUT`]);
//! - a check's stdout and stderr ([`event::CHECK_OUTPUT`]);
//! - a mirrored Store event's payload, on its `af.store.event`;
//! - an admitted Finding's body ([`event::FINDING_BODY`]);
//! - the configuration texts `af.config.text` names.
//!
//! `gen_ai.input.messages`, `gen_ai.output.messages` and `gen_ai.system_instructions` are never
//! attributes: content is only ever a blob an event names.
//!
//! **Allowances.** An Attempt's content is bounded by `content_attempt_max_bytes` and content
//! outside any Attempt by `content_process_max_bytes`, counted in retained bytes. Past either,
//! every further content of that scope is dropped: its event still names an occurrence, whose
//! state is `dropped`, the seal's `dropped` counts it, and one `budget` problem says so. Nothing
//! about the Worker changes: no process is terminated and nothing reaches the Store.
//!
//! **Boundary.** Nothing here is read by any code that composes a prompt, a Warm Set, Worker
//! Notes or an optimization source (ADR-0153 clause 2): the content is the operator's.

use crate::blob::Media;
use crate::record::Attrs;
use crate::subscriber;

/// The names of the events that carry content.
pub mod event {
    /// The prompt bytes of the composed Worker input, on `af.context.compose`.
    pub const INPUT: &str = "gen_ai.content.input";
    /// The context manifest the Worker input was composed by, on `af.context.compose`.
    pub const SYSTEM: &str = "gen_ai.content.system";
    /// An assistant `text` block or a Codex `agent_message` item, on its turn.
    pub const OUTPUT: &str = "gen_ai.content.output";
    /// A `thinking` block or a Codex `reasoning` item, on its turn.
    pub const REASONING: &str = "af.content.reasoning";
    /// A tool call's arguments, on its tool span.
    pub const TOOL_INPUT: &str = "af.content.tool_input";
    /// A tool call's result, on its tool span.
    pub const TOOL_OUTPUT: &str = "af.content.tool_output";
    /// One stream of a check's output (`af.stream`: `stdout` or `stderr`), on `af.check`.
    pub const CHECK_OUTPUT: &str = "af.check.output";
    /// An admitted Finding's body, keyed by `af.finding.id`.
    pub const FINDING_BODY: &str = "af.finding.body";
    /// A configuration text: the pipeline, the Task file or `.af/af.toml`.
    pub const CONFIG_TEXT: &str = "af.config.text";
}

/// Whose allowance a content is counted against.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The Attempt whose `af.attempt` span has this `span_id`: `content_attempt_max_bytes`.
    Attempt(String),
    /// Content outside any Attempt (check output, configuration texts, Store payloads, Finding
    /// bodies): `content_process_max_bytes`.
    Process,
}

/// The `budget` problem of an exhausted Attempt allowance.
pub const ATTEMPT_EXHAUSTED: &str = "content_attempt_max_bytes reached: further content of the \
                                     Attempt is dropped and counted; the Attempt continues";
/// The `budget` problem of an exhausted process allowance.
pub const PROCESS_EXHAUSTED: &str = "content_process_max_bytes reached: further content outside \
                                     an Attempt is dropped and counted; the run continues";

/// Whether this process records content now: a writer at `content` that has not failed.
pub fn recording() -> bool {
    subscriber::current_writer().is_some_and(|writer| writer.level().keeps_content())
}

/// At `content`, the event `name` on `span` naming `bytes` as a blob, counted against the
/// allowance of the Attempt `span` belongs to (its nearest `af.attempt`), or the process's
/// outside one. Below `content`, or for a span the subscriber did not keep, nothing at all.
/// Returns the event's `seq`.
pub fn on_span(
    span: &tracing::Span,
    name: &str,
    attrs: Attrs,
    bytes: &[u8],
    media: Media,
) -> Option<u64> {
    let writer = subscriber::current_writer()?;
    if !writer.level().keeps_content() {
        return None;
    }
    let recorded = subscriber::recorded(span)?;
    let scope = subscriber::content_scope(span);
    writer.content_event(&recorded.span_id, name, attrs, bytes, media, &scope)
}

/// [`on_span`], counted against the process's allowance whichever span it is on: content the
/// kernel produced outside an Attempt's work (check output, configuration texts).
pub fn on_span_outside_attempts(
    span: &tracing::Span,
    name: &str,
    attrs: Attrs,
    bytes: &[u8],
    media: Media,
) -> Option<u64> {
    let writer = subscriber::current_writer()?;
    if !writer.level().keeps_content() {
        return None;
    }
    let recorded = subscriber::recorded(span)?;
    writer.content_event(
        &recorded.span_id,
        name,
        attrs,
        bytes,
        media,
        &Scope::Process,
    )
}
