//! Harness streams (design §7.3): the tap the consumer runs over one observed harness process.
//!
//! The supervisor's drain offers every chunk it reads to a bounded queue (`review-process`'s
//! output observer); the consumer takes the chunks in order and hands them to a [`Tap`], which
//! frames each stream into lines incrementally and records what is **validated structure** as
//! the lines arrive, at `spans` and above:
//!
//! - the record kind from the closed vocabulary below, counts, byte sizes and arrival times (the
//!   drain's clock at the read, never the consumer's);
//! - for a Claude stream, `af.turn` per `assistant` message, ended as its line is read (a
//!   further line of the same message is `af.turn.continued` on it), and `af.tool` per
//!   `tool_use` block below it, closed by its `tool_result`; for a Codex stream, `af.turn` per turn and `af.tool` per
//!   `command_execution` or `mcp_tool_call` item, from `item.started` to `item.completed`; and
//!   the events of the other lines;
//! - correlation ids `af.corr`, the first 16 hex digits of `SHA-256(segment_id ":" original)`,
//!   in place of every identifier the harness supplies (session, thread, message, request,
//!   tool call, item);
//! - tool names that are in the invocation's own tool set, stop reasons and item statuses from
//!   their closed enumerations, and the bound model's id when the reported model equals it.
//!
//! Every untrusted string — text, thinking, tool arguments and results, agent messages,
//! reasoning summaries, error texts, unparsed lines, and the harness's own identifiers — is
//! never recorded live: at `spans` only its size is; at `content` its bytes are **pended** in a
//! buffer bounded by the Attempt's content allowance (`content_attempt_max_bytes`; overflow
//! drops the oldest pending content and counts it), and the identifiers are pended at every
//! level that records them.
//!
//! **Frames.** A frame longer than `content_max_blob_bytes` before its newline is discarded
//! whole through the next newline: only the event `af.stream.frame_withheld` with its size is
//! recorded, and no prefix of it is parsed, redacted or kept, because a line too long to be
//! judged whole cannot be judged at all.
//!
//! **Gaps.** A chunk whose offset is not the one expected — the observer dropped what lay
//! between — is a gap: the partial frame is discarded, `af.stream.gap` records the missing byte
//! range, and framing resumes after the first newline past the gap, so bytes on either side of
//! a drop are never joined into a line the harness did not write. A stream shorter than the
//! supervisor collected ends in a trailing gap. A stream with a gap is `incomplete` for turn and
//! tool analysis.
//!
//! **The release decision** ([`Release`]) is Telemetry's own, taken once the process has ended
//! over its complete streams and any output the harness left outside them (Codex's `-o` final
//! message), before any pending byte goes anywhere: when any of them carries
//! login-challenge material (`carries_auth_material`), when the adapter replaced the streams, or
//! when the native failure classification is an authentication one, every pending byte and
//! every pending identifier of the process is withheld (`af.stream.withheld`). Otherwise the
//! pending content passes the grant replacement and the credential rules on its assembled,
//! complete lines — so a grant split across two chunks is still replaced — and is handed on
//! ([`Released`]), and one `af.harness.ids` event lists each correlation id with its original,
//! each original having passed the credential rules; a list longer than one record holds
//! continues in the `af.harness.ids.part` events that follow it, as many as its
//! `af.harness.ids.parts` says. An identifier longer than 256 bytes, or past the 4096 distinct
//! ones a process pends, keeps its `af.corr` in live records but has no mapping:
//! `af.harness.ids.omitted` counts these and `af.harness.ids.analysis` is then `incomplete`.
//!
//! **Content** (package P7, [`crate::content`]). At `content`, after a release that withheld
//! nothing, the pending content becomes blobs, each named by one event on the turn or tool span
//! it came from, in the order it arrived: assistant text and Codex agent messages as
//! `gen_ai.content.output`, thinking blocks and reasoning items as `af.content.reasoning` (only
//! with `content_reasoning`), tool arguments and results as `af.content.tool_input` and
//! `af.content.tool_output` (only with `content_tool_io`; a switched-off kind is never even
//! pended). Each is judged from its original bytes by the grant replacement and the credential
//! rules, so its occurrence's source identity is the original's, and counted against the
//! Attempt's content allowance. A harness's error texts, its stderr and its unparsed lines stay
//! sizes only.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::blob::Media;
use crate::content::{self, Scope};
use crate::level::Level;
use crate::record::{Attrs, MAX_ARRAY_BYTES, MAX_ARRAY_ELEMENTS, SpanKind, SpanStatus};
use crate::redact::{self, Redactor};
use crate::segment::writer::Writer;
use crate::subscriber::SpanIdentity;

/// The harness a stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// `claude -p --output-format stream-json --verbose`.
    Claude,
    /// `codex exec --json`.
    Codex,
}

/// Which pipe a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

/// The span every record of the tap hangs below: the process's `af.harness`.
pub const HARNESS_SPAN: &str = "af.harness";
/// One model turn.
pub const TURN_SPAN: &str = "af.turn";
/// One tool call.
pub const TOOL_SPAN: &str = "af.tool";

/// The names of the events the tap records.
pub mod event {
    /// `system/init` (Claude) or `thread.started` (Codex), on `af.harness`.
    pub const INIT: &str = "af.harness.init";
    /// The Claude `result` line, on `af.harness`.
    pub const RESULT: &str = "af.harness.result";
    /// `rate_limit_event`, on `af.harness`.
    pub const RATE_LIMIT: &str = "af.harness.rate_limit";
    /// `permission_denied`, on `af.harness`.
    pub const PERMISSION_DENIED: &str = "af.harness.permission_denied";
    /// A Codex `error` line, on the open turn or `af.harness`.
    pub const ERROR: &str = "af.harness.error";
    /// `system/api_retry`, on the open turn.
    pub const API_RETRY: &str = "af.turn.api_retry";
    /// A further Claude `assistant` line of the message that ended a turn (the harness prints
    /// one line per content block), on that ended turn: the blocks, tool calls and report the
    /// line adds, which `af telemetry turns` folds into the turn.
    pub const CONTINUED: &str = "af.turn.continued";
    /// A Codex `reasoning` item, on the open turn.
    pub const REASONING: &str = "af.item.reasoning";
    /// A Codex `agent_message` item, on the open turn.
    pub const AGENT_MESSAGE: &str = "af.item.agent_message";
    /// A Codex `file_change` item, on the open turn.
    pub const FILE_CHANGE: &str = "af.item.file_change";
    /// A Codex `web_search` item, on the open turn.
    pub const WEB_SEARCH: &str = "af.item.web_search";
    /// A Codex `todo_list` (plan) item, on the open turn.
    pub const TODO_LIST: &str = "af.item.todo_list";
    /// A Codex item of another type, on the open turn.
    pub const OTHER_ITEM: &str = "af.item.other";
    /// The bytes the observer dropped, on `af.harness`.
    pub const GAP: &str = "af.stream.gap";
    /// A frame too long to judge, discarded whole, on `af.harness`.
    pub const FRAME_WITHHELD: &str = "af.stream.frame_withheld";
    /// After the release decision: the correlation ids with their originals (the first part of
    /// them) and the stream's counts, on `af.harness`. One per process.
    pub const IDS: &str = "af.harness.ids";
    /// After [`IDS`], on `af.harness`: each further part of the released originals, as many as
    /// its `af.harness.ids.parts` says, so every admitted mapping is kept whole within the record
    /// bounds.
    pub const IDS_PART: &str = "af.harness.ids.part";
}

/// What the tap is told about the process it observes.
#[derive(Debug, Clone)]
pub struct TapOptions {
    pub harness: Harness,
    /// The process's `af.harness` span.
    pub parent: SpanIdentity,
    /// `gen_ai.provider.name`: `anthropic` or `openai`.
    pub provider: &'static str,
    /// The model the binding requested.
    pub bound_model: Option<String>,
    /// The invocation's own tool set: only these names are recorded.
    pub tools: Vec<String>,
    /// `content_max_blob_bytes`: the longest frame that can be judged.
    pub frame_max_bytes: u64,
    /// `content_attempt_max_bytes`: the bound of the pending content.
    pub content_allowance_bytes: u64,
    /// The Provider grants the process received, replaced in released content.
    pub grants: Vec<(String, String)>,
    /// `content_reasoning`: thinking blocks and reasoning items are pended and published.
    pub content_reasoning: bool,
    /// `content_tool_io`: tool arguments and results are pended and published.
    pub content_tool_io: bool,
    /// Whose content allowance the released content counts against: the process's Attempt.
    pub scope: Scope,
}

/// Telemetry's own release decision over a process's complete output (design §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Release {
    /// Every pending byte of the process is withheld: no stream content is published.
    pub withheld: bool,
    /// No native content is published (package P9 pends it with the streams).
    pub native_withheld: bool,
    /// The bytes the supervisor collected from each stream, before any redaction: where each
    /// observed stream ends.
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

impl Release {
    /// Decide over the complete streams and the native content pending for the Attempt: withheld
    /// when any of them carries login-challenge material, when the adapter replaced the streams,
    /// or when the native failure classification is an authentication one. The decision changes
    /// nothing about the Attempt: it only says what Telemetry may publish.
    pub fn decide(
        stdout: &[u8],
        stderr: &[u8],
        native_pending: &[&[u8]],
        adapter_replaced: bool,
        authentication_failure: bool,
    ) -> Self {
        let withheld = adapter_replaced
            || authentication_failure
            || redact::carries_auth_material(stdout)
            || redact::carries_auth_material(stderr)
            || native_pending
                .iter()
                .any(|content| redact::carries_auth_material(content));
        Self {
            withheld,
            native_withheld: withheld,
            stdout_bytes: stdout.len() as u64,
            stderr_bytes: stderr.len() as u64,
        }
    }

    /// The streams' collected sizes, which the decision above measured after the supervisor's
    /// grant replacement: the observed raw sizes are what the gap protocol compares.
    pub fn with_sizes(mut self, stdout_bytes: u64, stderr_bytes: u64) -> Self {
        self.stdout_bytes = stdout_bytes;
        self.stderr_bytes = stderr_bytes;
        self
    }
}

/// One pending content: an untrusted string's bytes, waiting for the release decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// What it is: `text`, `thinking`, `tool_input`, `tool_output`, `agent_message`,
    /// `reasoning`, `error`, `line`.
    pub kind: &'static str,
    pub media: Media,
    pub bytes: Vec<u8>,
    /// The span it came from: its turn, its tool call, or the harness outside both.
    pub span_id: String,
}

/// The event a released content of `kind` is published as at `content`; `None` for the kinds
/// that stay sizes only (error texts, stderr and unparsed lines).
pub fn published_as(kind: &str) -> Option<&'static str> {
    match kind {
        "text" | "agent_message" => Some(content::event::OUTPUT),
        "thinking" | "reasoning" => Some(content::event::REASONING),
        "tool_input" => Some(content::event::TOOL_INPUT),
        "tool_output" => Some(content::event::TOOL_OUTPUT),
        _ => None,
    }
}

/// A content kind's closed spelling, for `af.content.kind`.
fn kind_member(kind: &str) -> &'static str {
    member(
        Some(kind),
        &[
            "text",
            "thinking",
            "tool_input",
            "tool_output",
            "agent_message",
            "reasoning",
        ],
    )
}

/// What a release hands on: the pending content, redacted, and the identifiers that passed the
/// credential rules. Empty when the decision withheld everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Released {
    pub withheld: bool,
    pub contents: Vec<Content>,
    /// `(af.corr, original)`, in the order they first appeared.
    pub ids: Vec<(String, String)>,
    /// Pending contents the allowance dropped, oldest first.
    pub dropped_contents: u64,
    /// Identifiers the credential rules withheld.
    pub withheld_ids: u64,
    /// Contents published as blob occurrences at `content`.
    pub published: u64,
}

/// The longest harness identifier pended for `af.harness.ids`: anything longer is no id.
const MAX_ID_BYTES: usize = 256;
/// The most identifiers pended per process.
const MAX_IDS: usize = 4096;
/// The most distinct identifiers past [`MAX_IDS`] or [`MAX_ID_BYTES`] counted one by one in
/// `af.harness.ids.omitted`; past it the count is a floor.
const MAX_OMITTED_IDS: usize = 65_536;

/// The released `(af.corr, original)` mappings as `corr original` texts, in order, cut into the
/// parts one record holds whole: at most [`MAX_ARRAY_ELEMENTS`] texts and [`MAX_ARRAY_BYTES`]
/// bytes each, so the record writer never cuts one.
fn id_parts(ids: &[(String, String)]) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut bytes = 0;
    for (corr, original) in ids {
        let text = format!("{corr} {original}");
        if parts.last().is_none_or(|part| {
            part.len() == MAX_ARRAY_ELEMENTS || bytes + text.len() > MAX_ARRAY_BYTES
        }) {
            parts.push(Vec::new());
            bytes = 0;
        }
        bytes += text.len();
        parts.last_mut().expect("a part").push(text);
    }
    parts
}

/// The correlation id of a harness identifier in `segment_id`: the first 16 hex digits of
/// `SHA-256(segment_id ":" original)`.
pub fn corr(segment_id: &str, original: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(segment_id.as_bytes());
    hasher.update(b":");
    hasher.update(original.as_bytes());
    crate::trace::hex(&hasher.finalize()[..8])
}

#[derive(Default)]
struct Framer {
    /// The offset the next chunk must start at.
    expected: u64,
    partial: Vec<u8>,
    /// The bytes of a frame over the bound so far, discarded whole through its newline.
    oversized: Option<u64>,
    /// After a gap: everything up to the first newline belongs to a frame begun in the gap.
    resync: bool,
    /// The arrival of the last chunk.
    last: u128,
}

#[derive(Default)]
struct Usage {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
    /// A counter the harness reported in a shape af cannot read.
    malformed: bool,
    reported: bool,
}

impl Usage {
    fn counter(value: &Value, key: &str, usage: &mut bool) -> Option<u64> {
        match value.get(key) {
            None | Some(Value::Null) => None,
            Some(number) => {
                let read = number.as_u64();
                *usage |= read.is_none();
                read
            }
        }
    }

    /// Claude's `usage`: input excludes cache reads and cache creation.
    fn claude(value: &Value) -> Self {
        let mut malformed = false;
        let usage = Self {
            input: Self::counter(value, "input_tokens", &mut malformed),
            output: Self::counter(value, "output_tokens", &mut malformed),
            cache_read: Self::counter(value, "cache_read_input_tokens", &mut malformed),
            cache_write: Self::counter(value, "cache_creation_input_tokens", &mut malformed),
            reasoning: None,
            malformed,
            reported: value.is_object(),
        };
        Self {
            malformed: usage.malformed || !value.is_object(),
            ..usage
        }
    }

    /// Codex's `usage`: input includes cached input, as the adapter accounts it.
    fn codex(value: &Value) -> Self {
        let mut malformed = false;
        let usage = Self {
            input: Self::counter(value, "input_tokens", &mut malformed),
            output: Self::counter(value, "output_tokens", &mut malformed),
            cache_read: Self::counter(value, "cached_input_tokens", &mut malformed),
            cache_write: Self::counter(value, "cache_write_input_tokens", &mut malformed),
            reasoning: Self::counter(value, "reasoning_output_tokens", &mut malformed),
            malformed,
            reported: value.is_object(),
        };
        Self {
            malformed: usage.malformed || !value.is_object(),
            ..usage
        }
    }

    fn attrs(&self, harness: Harness, mut attrs: Attrs) -> Attrs {
        if !self.reported {
            return attrs.enumerated("af.usage.observation", "unknown");
        }
        for (key, value) in [
            ("af.usage.input_tokens", self.input),
            ("af.usage.output_tokens", self.output),
            ("af.usage.cache_read_tokens", self.cache_read),
            ("af.usage.cache_write_tokens", self.cache_write),
            ("af.usage.reasoning_tokens", self.reasoning),
        ] {
            if let Some(value) = value {
                attrs = attrs.decimal(key, value.to_string());
            }
        }
        // The conventions' input includes cached tokens; the exact split stays in `af.usage.*`.
        let input = match harness {
            Harness::Claude => self.input.map(|input| {
                u128::from(input)
                    + u128::from(self.cache_read.unwrap_or(0))
                    + u128::from(self.cache_write.unwrap_or(0))
            }),
            Harness::Codex => self.input.map(u128::from),
        };
        if let Some(input) = input {
            attrs = attrs.decimal("gen_ai.usage.input_tokens", input.to_string());
        }
        if let Some(output) = self.output {
            attrs = attrs.decimal("gen_ai.usage.output_tokens", output.to_string());
        }
        let complete = !self.malformed && self.input.is_some() && self.output.is_some();
        attrs.enumerated(
            "af.usage.observation",
            if complete { "complete" } else { "incomplete" },
        )
    }
}

/// An open turn.
struct Turn {
    span_id: Option<String>,
    /// The arrival of the last line of its message (Claude), or of the line that closes it.
    last: u128,
    /// The correlation id of its message (Claude); `None` until one arrives.
    message: Option<String>,
    usage: Usage,
    stop_reason: Option<&'static str>,
    model_bound: Option<bool>,
    text_blocks: i64,
    text_bytes: i64,
    thinking_blocks: i64,
    thinking_bytes: i64,
    tool_calls: i64,
    timestamp: Option<String>,
}

/// An open tool call.
struct Tool {
    span_id: Option<String>,
}

/// A Claude turn its assistant message ended.
struct Answered {
    span_id: Option<String>,
    /// The correlation id of its message.
    message: Option<String>,
}

/// What one Claude `assistant` line reports: its message's report and its content blocks.
#[derive(Default)]
struct Blocks {
    usage: Option<Usage>,
    stop_reason: Option<&'static str>,
    model_bound: Option<bool>,
    timestamp: Option<String>,
    text_blocks: i64,
    text_bytes: i64,
    thinking_blocks: i64,
    thinking_bytes: i64,
    /// `(call key, name, input bytes)` of each `tool_use` block, with its input.
    calls: Vec<(String, Option<String>, i64, Option<Value>)>,
}

/// The tap over one harness process's two streams.
pub struct Tap {
    options: TapOptions,
    segment_id: String,
    content: bool,
    framers: [Framer; 2],
    turn: Option<Turn>,
    /// The turn the last Claude `assistant` line ended, until the tool results go back to the
    /// model: its tool calls hang below it, and a further line of its message continues it.
    answered: Option<Answered>,
    turns: u64,
    tools: BTreeMap<String, Tool>,
    tool_calls: u64,
    /// `(af.corr, original)`, in the order they first appeared.
    ids: Vec<(String, String)>,
    known_ids: BTreeSet<String>,
    /// The correlation ids placed in live records whose original no bound let the tap pend, so
    /// `af.harness.ids` can never resolve them: counted, never kept.
    omitted_ids: BTreeSet<String>,
    pending: VecDeque<Content>,
    pending_bytes: u64,
    dropped_contents: u64,
    lines: u64,
    unparsed_lines: u64,
    unparsed_bytes: u64,
    stderr_lines: u64,
    stderr_bytes: u64,
    frames_withheld: u64,
    gaps: u64,
    /// The arrival of the last chunk of either stream.
    last: u128,
    finished: bool,
}

/// A static spelling of a closed enumeration's member; `other` for anything else.
fn member(value: Option<&str>, members: &[&'static str]) -> &'static str {
    value
        .and_then(|value| members.iter().find(|member| **member == value).copied())
        .unwrap_or("other")
}

const STOP_REASONS: [&str; 7] = [
    "end_turn",
    "max_tokens",
    "stop_sequence",
    "tool_use",
    "pause_turn",
    "refusal",
    "model_context_window_exceeded",
];

const ITEM_STATUSES: [&str; 4] = ["in_progress", "completed", "failed", "declined"];

/// A string value's length in bytes, or of the JSON it is when not a string.
fn size_of(value: Option<&Value>) -> i64 {
    match value {
        None | Some(Value::Null) => 0,
        Some(Value::String(text)) => text.len() as i64,
        Some(value) => serde_json::to_vec(value).map_or(0, |bytes| bytes.len() as i64),
    }
}

/// A harness version, kept only in the grammar of one: digits, dots and at most a short
/// alphanumeric suffix.
fn version(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let valid = (1..=32).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    valid.then(|| text.to_string())
}

/// A message's own timestamp, kept only in the RFC 3339 grammar.
fn timestamp(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let valid = (10..=40).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | '.' | 'Z' | '+'));
    valid.then(|| text.to_string())
}

impl Tap {
    pub fn new(writer: &Writer, options: TapOptions) -> Self {
        Self {
            segment_id: writer.segment_id().unwrap_or_default(),
            content: writer.level().keeps_content(),
            options,
            framers: [Framer::default(), Framer::default()],
            turn: None,
            answered: None,
            turns: 0,
            tools: BTreeMap::new(),
            tool_calls: 0,
            ids: Vec::new(),
            known_ids: BTreeSet::new(),
            omitted_ids: BTreeSet::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            dropped_contents: 0,
            lines: 0,
            unparsed_lines: 0,
            unparsed_bytes: 0,
            stderr_lines: 0,
            stderr_bytes: 0,
            frames_withheld: 0,
            gaps: 0,
            last: 0,
            finished: false,
        }
    }

    /// Whether `level` records a harness stream at all: `spans` and above.
    pub fn records(level: Level) -> bool {
        matches!(level, Level::Spans | Level::Content)
    }

    /// Whether [`Self::finish`] ran.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// The gaps seen so far.
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    fn harness_span(&self) -> &str {
        &self.options.parent.span.span_id
    }

    fn trace(&self) -> Option<&str> {
        self.options.parent.span.trace_id.as_deref()
    }

    /// The run identity every record of the tap names first, as a span of the subscriber would.
    fn identity(&self) -> Attrs {
        Attrs(self.options.parent.identity.clone())
    }

    /// The key of a tool call: its identifier's correlation id, or, for a harness that gave
    /// none, one the tap numbers.
    fn call_key(&mut self, original: &str) -> String {
        if original.is_empty() {
            format!("unidentified:{}", self.tool_calls)
        } else {
            self.corr(original)
        }
    }

    /// The correlation id of a harness identifier; the original is pended for `af.harness.ids`.
    /// One too long to be an id, or past the most a process pends, has no mapping: it is
    /// counted in `af.harness.ids.omitted`, and the process's identifier analysis is incomplete.
    fn corr(&mut self, original: &str) -> String {
        let corr = corr(&self.segment_id, original);
        if self.known_ids.contains(&corr) {
            return corr;
        }
        if original.len() <= MAX_ID_BYTES && self.ids.len() < MAX_IDS {
            self.known_ids.insert(corr.clone());
            self.ids.push((corr.clone(), original.to_string()));
        } else if self.omitted_ids.len() < MAX_OMITTED_IDS {
            self.omitted_ids.insert(corr.clone());
        }
        corr
    }

    /// The span content arriving now belongs to: the open turn, else the harness.
    fn content_span(&self) -> String {
        self.turn
            .as_ref()
            .and_then(|turn| turn.span_id.clone())
            .unwrap_or_else(|| self.harness_span().to_string())
    }

    /// The span of the open tool call `corr`, else the content span.
    fn tool_span(&self, corr: &str) -> String {
        self.tools
            .get(corr)
            .and_then(|tool| tool.span_id.clone())
            .unwrap_or_else(|| self.content_span())
    }

    /// Whether content of `kind` is kept at all: thinking and reasoning only with
    /// `content_reasoning`, tool arguments and results only with `content_tool_io`.
    fn keeps(&self, kind: &str) -> bool {
        match kind {
            "thinking" | "reasoning" => self.options.content_reasoning,
            "tool_input" | "tool_output" => self.options.content_tool_io,
            _ => true,
        }
    }

    /// Pend an untrusted string's bytes at `content`, as content of the current span; at
    /// every other level only its size is ever recorded.
    fn pend(&mut self, kind: &'static str, media: Media, bytes: &[u8]) {
        let span = self.content_span();
        self.pend_on(kind, media, bytes, span);
    }

    /// Pend content of `span`. The oldest pending content goes first when the allowance is
    /// exhausted; a kind its switch turned off is never pended.
    fn pend_on(&mut self, kind: &'static str, media: Media, bytes: &[u8], span: String) {
        if !self.content || bytes.is_empty() || !self.keeps(kind) {
            return;
        }
        let size = bytes.len() as u64;
        if size > self.options.content_allowance_bytes {
            self.dropped_contents += 1;
            return;
        }
        while self.pending_bytes + size > self.options.content_allowance_bytes {
            let Some(oldest) = self.pending.pop_front() else {
                break;
            };
            self.pending_bytes -= oldest.bytes.len() as u64;
            self.dropped_contents += 1;
        }
        self.pending_bytes += size;
        self.pending.push_back(Content {
            kind,
            media,
            bytes: bytes.to_vec(),
            span_id: span,
        });
    }

    fn pend_value(&mut self, kind: &'static str, value: Option<&Value>) {
        let span = self.content_span();
        self.pend_value_on(kind, value, span);
    }

    fn pend_value_on(&mut self, kind: &'static str, value: Option<&Value>, span: String) {
        if !self.content || !self.keeps(kind) {
            return;
        }
        match value {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => {
                let bytes = text.as_bytes().to_vec();
                self.pend_on(kind, Media::Text, &bytes, span);
            }
            Some(value) => {
                if let Ok(bytes) = serde_json::to_vec(value) {
                    self.pend_on(kind, Media::Json, &bytes, span);
                }
            }
        }
    }

    // --------------------------------------------------------------------------------------
    // framing

    /// One chunk the observer queued: `offset` its position in `stream`, `arrival` the unix
    /// nanoseconds of the drain's read.
    pub fn chunk(
        &mut self,
        writer: &Writer,
        stream: Stream,
        offset: u64,
        bytes: &[u8],
        arrival: u128,
    ) {
        if self.finished {
            return;
        }
        self.last = self.last.max(arrival);
        let max = self.options.frame_max_bytes;
        let index = stream.index();
        let expected = self.framers[index].expected;
        if offset != expected {
            self.gap(writer, stream, expected, offset, arrival);
            let framer = &mut self.framers[index];
            framer.partial = Vec::new();
            framer.oversized = None;
            framer.resync = true;
        }
        let framer = &mut self.framers[index];
        framer.expected = offset + bytes.len() as u64;
        framer.last = arrival;
        let mut rest = bytes;
        while !rest.is_empty() {
            let framer = &mut self.framers[index];
            let Some(at) = rest.iter().position(|byte| *byte == b'\n') else {
                if framer.resync {
                } else if let Some(size) = &mut framer.oversized {
                    *size += rest.len() as u64;
                } else if (framer.partial.len() + rest.len()) as u64 > max {
                    // Over the bound before its newline: nothing of it is kept from here on.
                    framer.oversized = Some((framer.partial.len() + rest.len()) as u64);
                    framer.partial = Vec::new();
                } else {
                    framer.partial.extend_from_slice(rest);
                }
                break;
            };
            let piece = &rest[..at];
            rest = &rest[at + 1..];
            if framer.resync {
                framer.resync = false;
                continue;
            }
            if let Some(size) = framer.oversized.take() {
                let size = size + piece.len() as u64 + 1;
                self.frame_withheld(writer, stream, size, arrival);
                continue;
            }
            if (framer.partial.len() + piece.len()) as u64 > max {
                let size = (framer.partial.len() + piece.len()) as u64 + 1;
                framer.partial = Vec::new();
                self.frame_withheld(writer, stream, size, arrival);
                continue;
            }
            let line = if framer.partial.is_empty() {
                piece.to_vec()
            } else {
                let mut line = std::mem::take(&mut framer.partial);
                line.extend_from_slice(piece);
                line
            };
            self.line(writer, stream, &line, arrival);
        }
    }

    fn gap(&mut self, writer: &Writer, stream: Stream, from: u64, to: u64, time: u128) {
        self.gaps += 1;
        let attrs = Attrs::new()
            .enumerated("af.stream", stream.as_str())
            .int(
                "af.stream.gap.from",
                i64::try_from(from).unwrap_or(i64::MAX),
            )
            .int("af.stream.gap.to", i64::try_from(to).unwrap_or(i64::MAX))
            .int(
                "af.stream.gap.bytes",
                i64::try_from(to.saturating_sub(from)).unwrap_or(i64::MAX),
            );
        let span = self.harness_span().to_string();
        writer.stream_event(&span, event::GAP, attrs, time);
    }

    fn frame_withheld(&mut self, writer: &Writer, stream: Stream, size: u64, time: u128) {
        self.frames_withheld += 1;
        let attrs = Attrs::new().enumerated("af.stream", stream.as_str()).int(
            "af.stream.frame.bytes",
            i64::try_from(size).unwrap_or(i64::MAX),
        );
        let span = self.harness_span().to_string();
        writer.stream_event(&span, event::FRAME_WITHHELD, attrs, time);
    }

    /// One complete line, without its newline.
    fn line(&mut self, writer: &Writer, stream: Stream, line: &[u8], arrival: u128) {
        self.lines += 1;
        if stream == Stream::Stderr {
            // A harness's stderr is untrusted text: counted and sized, its bytes pended.
            self.stderr_lines += 1;
            self.stderr_bytes += line.len() as u64;
            self.pend("line", Media::Text, line);
            return;
        }
        let Some(value) = serde_json::from_slice::<Value>(line)
            .ok()
            .filter(|value| value.get("type").is_some_and(Value::is_string))
        else {
            self.unparsed_lines += 1;
            self.unparsed_bytes += line.len() as u64;
            self.pend("line", Media::Text, line);
            return;
        };
        match self.options.harness {
            Harness::Claude => self.claude(writer, &value, arrival),
            Harness::Codex => self.codex(writer, &value, arrival),
        }
    }

    // --------------------------------------------------------------------------------------
    // turns and tools

    fn open_turn(&mut self, writer: &Writer, start: u128) {
        self.turns += 1;
        let attrs = self
            .identity()
            .enumerated("gen_ai.operation.name", "chat")
            .enumerated("gen_ai.provider.name", self.options.provider)
            .enumerated("af.series", "turn")
            .int(
                "af.turn.ordinal",
                i64::try_from(self.turns).unwrap_or(i64::MAX),
            );
        let attrs = match &self.options.bound_model {
            Some(model) => attrs.text("gen_ai.request.model", model.clone()),
            None => attrs,
        };
        let span_id = writer.stream_span_start(
            self.trace(),
            Some(self.harness_span()),
            TURN_SPAN,
            SpanKind::Client,
            attrs,
            start,
        );
        self.turn = Some(Turn {
            span_id,
            last: start,
            message: None,
            usage: Usage::default(),
            stop_reason: None,
            model_bound: None,
            text_blocks: 0,
            text_bytes: 0,
            thinking_blocks: 0,
            thinking_bytes: 0,
            tool_calls: 0,
            timestamp: None,
        });
    }

    /// End the open turn at `end`: `outcome` `succeeded`, `failed` (with the error text's size)
    /// or `incomplete`.
    fn close_turn(&mut self, writer: &Writer, end: u128, outcome: &'static str, error: i64) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        let Some(span_id) = &turn.span_id else {
            return;
        };
        let mut attrs = Attrs::new();
        if let Some(message) = &turn.message {
            attrs = attrs.id("af.corr", message.clone());
        }
        attrs = turn.usage.attrs(self.options.harness, attrs);
        if let Some(reason) = turn.stop_reason {
            attrs = attrs.texts("gen_ai.response.finish_reasons", vec![reason.to_string()]);
        }
        match (turn.model_bound, &self.options.bound_model) {
            (Some(true), Some(model)) => {
                attrs = attrs.text("gen_ai.response.model", model.clone());
            }
            (Some(false), _) => attrs = attrs.boolean("af.turn.model_other", true),
            _ => {}
        }
        if let Some(timestamp) = &turn.timestamp {
            attrs = attrs.text("af.turn.timestamp", timestamp.clone());
        }
        attrs = attrs
            .int("af.turn.text_blocks", turn.text_blocks)
            .int("af.turn.text_bytes", turn.text_bytes)
            .int("af.turn.thinking_blocks", turn.thinking_blocks)
            .int("af.turn.thinking_bytes", turn.thinking_bytes)
            .int("af.turn.tool_calls", turn.tool_calls)
            .enumerated("af.outcome", outcome);
        let status = match outcome {
            "succeeded" => SpanStatus::Ok,
            "failed" => {
                attrs = attrs
                    .enumerated("af.failure.class", "failed")
                    .enumerated("error.type", "failed")
                    .int("af.error.bytes", error);
                SpanStatus::Error
            }
            _ => SpanStatus::Unset,
        };
        writer.stream_span_end(span_id, status, attrs, end);
    }

    /// Open the tool call `corr` below `parent`, its turn or the harness.
    fn open_tool(
        &mut self,
        writer: &Writer,
        corr: String,
        name: Option<&str>,
        input: i64,
        at: u128,
        parent: String,
    ) {
        self.tool_calls += 1;
        let mut attrs = self
            .identity()
            .enumerated("gen_ai.operation.name", "execute_tool")
            .enumerated("gen_ai.tool.type", "function");
        if !corr.starts_with("unidentified:") {
            attrs = attrs
                .id("af.corr", corr.clone())
                .id("gen_ai.tool.call.id", corr.clone());
        }
        match name.filter(|name| self.options.tools.iter().any(|tool| tool == name)) {
            Some(name) => attrs = attrs.text("gen_ai.tool.name", name.to_string()),
            None => attrs = attrs.boolean("af.tool.unlisted", true),
        }
        attrs = attrs.int("af.tool.input_bytes", input);
        let span_id = writer.stream_span_start(
            self.trace(),
            Some(&parent),
            TOOL_SPAN,
            SpanKind::Internal,
            attrs,
            at,
        );
        self.tools.insert(corr, Tool { span_id });
    }

    /// End the tool call `corr`: `outcome` `succeeded`, `failed` or `incomplete`.
    fn close_tool(
        &mut self,
        writer: &Writer,
        corr: &str,
        end: u128,
        attrs: Attrs,
        outcome: &'static str,
    ) {
        let Some(tool) = self.tools.remove(corr) else {
            return;
        };
        let Some(span_id) = tool.span_id else {
            return;
        };
        let mut attrs = attrs.enumerated("af.outcome", outcome);
        let status = match outcome {
            "succeeded" => SpanStatus::Ok,
            "failed" => {
                attrs = attrs
                    .enumerated("af.failure.class", "failed")
                    .enumerated("error.type", "failed");
                SpanStatus::Error
            }
            _ => SpanStatus::Unset,
        };
        writer.stream_span_end(&span_id, status, attrs, end);
    }

    /// An event on the open turn, or on `af.harness` when none is open.
    fn on_turn(&self, writer: &Writer, name: &str, attrs: Attrs, time: u128) {
        let span = self
            .turn
            .as_ref()
            .and_then(|turn| turn.span_id.clone())
            .unwrap_or_else(|| self.harness_span().to_string());
        writer.stream_event(&span, name, attrs, time);
    }

    fn on_harness(&self, writer: &Writer, name: &str, attrs: Attrs, time: u128) {
        let span = self.harness_span().to_string();
        writer.stream_event(&span, name, attrs, time);
    }

    // --------------------------------------------------------------------------------------
    // Claude

    fn claude(&mut self, writer: &Writer, value: &Value, arrival: u128) {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        let subtype = value.get("subtype").and_then(Value::as_str);
        match (kind, subtype) {
            ("system", Some("init")) => {
                let session = value.get("session_id").and_then(Value::as_str);
                let mut attrs = Attrs::new().enumerated("af.stream.kind", "system/init");
                if let Some(session) = session {
                    let corr = self.corr(session);
                    attrs = attrs
                        .id("af.corr", corr.clone())
                        .text("gen_ai.conversation.id", corr);
                }
                let model = value.get("model").and_then(Value::as_str);
                attrs = self.model_attrs(attrs, model, "gen_ai.response.model");
                let tools: Vec<&str> = value
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|tools| tools.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let (listed, other): (Vec<&str>, Vec<&str>) = tools
                    .iter()
                    .partition(|name| self.options.tools.iter().any(|tool| tool == *name));
                attrs = attrs
                    .texts(
                        "af.harness.tools",
                        listed.iter().map(|name| (*name).to_string()).collect(),
                    )
                    .int("af.harness.tools.other", other.len() as i64)
                    .int(
                        "af.harness.mcp_servers",
                        value
                            .get("mcp_servers")
                            .and_then(Value::as_array)
                            .map_or(0, |servers| servers.len() as i64),
                    );
                if let Some(version) = version(value.get("claude_code_version")) {
                    attrs = attrs.text("af.harness.version", version);
                }
                self.on_harness(writer, event::INIT, attrs, arrival);
                if self.turn.is_none() {
                    self.open_turn(writer, arrival);
                }
            }
            ("system", Some("api_retry")) => {
                let int = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(-1);
                let attrs = Attrs::new()
                    .enumerated("af.stream.kind", "system/api_retry")
                    .int("af.retry.attempt", int("attempt"))
                    .int("af.retry.max_retries", int("max_retries"))
                    .int("af.retry.delay_ms", int("retry_delay_ms"))
                    .int("af.retry.error_status", int("error_status"))
                    .int("af.error.bytes", size_of(value.get("error")));
                self.pend_value("error", value.get("error"));
                self.on_turn(writer, event::API_RETRY, attrs, arrival);
            }
            ("system", Some("permission_denied")) | ("permission_denied", _) => {
                self.permission_denied(writer, value, arrival);
            }
            ("assistant", _) => self.assistant(writer, value, arrival),
            ("user", _) => self.user(writer, value, arrival),
            ("result", _) => {
                self.answered = None;
                let error = value.get("is_error").and_then(Value::as_bool);
                if self
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.message.is_some())
                {
                    let last = self.turn.as_ref().map_or(arrival, |turn| turn.last);
                    self.close_turn(writer, last, "succeeded", 0);
                } else {
                    let outcome = if error == Some(true) {
                        "failed"
                    } else {
                        "incomplete"
                    };
                    self.close_turn(writer, arrival, outcome, 0);
                }
                let mut attrs = Attrs::new()
                    .enumerated("af.stream.kind", "result")
                    .enumerated(
                        "af.result.subtype",
                        member(
                            subtype,
                            &[
                                "success",
                                "error_max_turns",
                                "error_during_execution",
                                "error_max_budget_usd",
                                "error_max_structured_output_retries",
                            ],
                        ),
                    );
                if let Some(error) = error {
                    attrs = attrs.boolean("af.result.is_error", error);
                }
                for (key, name) in [
                    ("num_turns", "af.result.num_turns"),
                    ("duration_ms", "af.result.duration_ms"),
                    ("duration_api_ms", "af.result.duration_api_ms"),
                ] {
                    if let Some(number) = value.get(key).and_then(Value::as_i64) {
                        attrs = attrs.int(name, number);
                    }
                }
                if let Some(reason) = value.get("stop_reason").and_then(Value::as_str) {
                    attrs = attrs.texts(
                        "gen_ai.response.finish_reasons",
                        vec![member(Some(reason), &STOP_REASONS).to_string()],
                    );
                }
                if let Some(session) = value.get("session_id").and_then(Value::as_str) {
                    let corr = self.corr(session);
                    attrs = attrs.id("af.corr", corr);
                }
                // The result repeats the reply the assistant's text blocks already carry: its
                // size and structure only, never pended as another output (design §7.3, P7).
                attrs = attrs.int("af.result.bytes", size_of(value.get("result")));
                self.on_harness(writer, event::RESULT, attrs, arrival);
            }
            ("rate_limit_event", _) => {
                let info = value.get("rate_limit_info").unwrap_or(&Value::Null);
                let mut attrs = Attrs::new()
                    .enumerated("af.stream.kind", "rate_limit_event")
                    .enumerated(
                        "af.rate_limit.status",
                        member(
                            info.get("status").and_then(Value::as_str),
                            &["allowed", "allowed_warning", "rejected"],
                        ),
                    )
                    .enumerated(
                        "af.rate_limit.type",
                        member(
                            info.get("rateLimitType").and_then(Value::as_str),
                            &[
                                "five_hour",
                                "seven_day",
                                "seven_day_opus",
                                "seven_day_sonnet",
                                "overage",
                            ],
                        ),
                    );
                if let Some(resets) = info.get("resetsAt").and_then(Value::as_i64) {
                    attrs = attrs.int("af.rate_limit.resets_at", resets);
                }
                self.on_harness(writer, event::RATE_LIMIT, attrs, arrival);
            }
            _ => {
                self.unparsed_lines += 1;
            }
        }
    }

    fn model_attrs(&self, attrs: Attrs, model: Option<&str>, key: &str) -> Attrs {
        match (model, &self.options.bound_model) {
            (Some(model), Some(bound)) if model == bound => attrs.text(key, bound.clone()),
            (Some(_), _) => attrs.boolean("af.model.other", true),
            (None, _) => attrs,
        }
    }

    fn permission_denied(&mut self, writer: &Writer, value: &Value, arrival: u128) {
        let mut attrs = Attrs::new().enumerated("af.stream.kind", "permission_denied");
        let name = value.get("tool_name").and_then(Value::as_str);
        match name.filter(|name| self.options.tools.iter().any(|tool| tool == name)) {
            Some(name) => attrs = attrs.text("gen_ai.tool.name", name.to_string()),
            None => attrs = attrs.boolean("af.tool.unlisted", true),
        }
        if let Some(id) = value.get("tool_use_id").and_then(Value::as_str) {
            let corr = self.corr(id);
            attrs = attrs.id("af.corr", corr);
        }
        self.on_harness(writer, event::PERMISSION_DENIED, attrs, arrival);
    }

    /// One Claude `assistant` line: its message ends the open turn (design §7.3) when the line
    /// is processed, with its usage, stop reason and blocks, and opens one `af.tool` per
    /// `tool_use` block below that ended turn. The harness prints a message as one line per
    /// content block, so a further line of the message that ended the last turn, before the
    /// tool results go back, continues that turn: its blocks and tool calls are recorded on
    /// it as one `af.turn.continued` event.
    fn assistant(&mut self, writer: &Writer, value: &Value, arrival: u128) {
        let message = value.get("message").unwrap_or(&Value::Null);
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .map(|id| self.corr(id));
        let continued = self.turn.is_none()
            && self
                .answered
                .as_ref()
                .is_some_and(|answered| answered.message == id);
        if !continued && self.turn.is_none() {
            self.open_turn(writer, arrival);
        }
        // The span this line's content belongs to: the turn it ends or continues.
        let span = if continued {
            self.answered
                .as_ref()
                .and_then(|answered| answered.span_id.clone())
                .unwrap_or_else(|| self.harness_span().to_string())
        } else {
            self.content_span()
        };
        let blocks = self.blocks(value, message, span.clone());
        if continued {
            self.continue_turn(writer, id, &blocks, &span, arrival);
        } else {
            let turn = self.turn.as_mut().expect("a turn is open");
            turn.message = id.clone();
            turn.last = arrival;
            if let Some(usage) = blocks.usage {
                turn.usage = usage;
            }
            turn.stop_reason = blocks.stop_reason.or(turn.stop_reason);
            turn.model_bound = blocks.model_bound.or(turn.model_bound);
            turn.timestamp = blocks.timestamp.or(turn.timestamp.take());
            turn.text_blocks += blocks.text_blocks;
            turn.text_bytes += blocks.text_bytes;
            turn.thinking_blocks += blocks.thinking_blocks;
            turn.thinking_bytes += blocks.thinking_bytes;
            turn.tool_calls += blocks.calls.len() as i64;
            let span_id = turn.span_id.clone();
            self.close_turn(writer, arrival, "succeeded", 0);
            self.answered = Some(Answered {
                span_id,
                message: id,
            });
        }
        for (corr, name, size, input) in blocks.calls {
            self.open_tool(
                writer,
                corr.clone(),
                name.as_deref(),
                size,
                arrival,
                span.clone(),
            );
            // The arguments are the tool call's content, pended once its span is open.
            let tool = self.tool_span(&corr);
            self.pend_value_on("tool_input", input.as_ref(), tool);
        }
    }

    /// Read one `assistant` line's report and blocks, pending its text and thinking on `span`.
    fn blocks(&mut self, value: &Value, message: &Value, span: String) -> Blocks {
        let mut blocks = Blocks {
            usage: message.get("usage").map(Usage::claude),
            stop_reason: message
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(|reason| member(Some(reason), &STOP_REASONS)),
            model_bound: message
                .get("model")
                .and_then(Value::as_str)
                .map(|model| self.options.bound_model.as_deref() == Some(model)),
            timestamp: timestamp(value.get("timestamp")),
            ..Blocks::default()
        };
        for block in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    blocks.text_blocks += 1;
                    blocks.text_bytes += size_of(block.get("text"));
                    self.pend_value_on("text", block.get("text"), span.clone());
                }
                Some("thinking") | Some("redacted_thinking") => {
                    blocks.thinking_blocks += 1;
                    blocks.thinking_bytes += size_of(block.get("thinking"));
                    self.pend_value_on("thinking", block.get("thinking"), span.clone());
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    let corr = self.call_key(id);
                    let name = block.get("name").and_then(Value::as_str);
                    blocks.calls.push((
                        corr,
                        name.map(str::to_string),
                        size_of(block.get("input")),
                        block.get("input").cloned(),
                    ));
                }
                _ => {}
            }
        }
        blocks
    }

    /// The `af.turn.continued` event of a further line of the message that ended the turn
    /// `span`: what the line adds, in the turn's own attribute names.
    fn continue_turn(
        &self,
        writer: &Writer,
        message: Option<String>,
        blocks: &Blocks,
        span: &str,
        arrival: u128,
    ) {
        let mut attrs = Attrs::new();
        if let Some(message) = message {
            attrs = attrs.id("af.corr", message);
        }
        if let Some(usage) = &blocks.usage {
            attrs = usage.attrs(self.options.harness, attrs);
        }
        if let Some(reason) = blocks.stop_reason {
            attrs = attrs.texts("gen_ai.response.finish_reasons", vec![reason.to_string()]);
        }
        match (blocks.model_bound, &self.options.bound_model) {
            (Some(true), Some(model)) => {
                attrs = attrs.text("gen_ai.response.model", model.clone());
            }
            (Some(false), _) => attrs = attrs.boolean("af.turn.model_other", true),
            _ => {}
        }
        if let Some(timestamp) = &blocks.timestamp {
            attrs = attrs.text("af.turn.timestamp", timestamp.clone());
        }
        attrs = attrs
            .int("af.turn.text_blocks", blocks.text_blocks)
            .int("af.turn.text_bytes", blocks.text_bytes)
            .int("af.turn.thinking_blocks", blocks.thinking_blocks)
            .int("af.turn.thinking_bytes", blocks.thinking_bytes)
            .int("af.turn.tool_calls", blocks.calls.len() as i64);
        writer.stream_event(span, event::CONTINUED, attrs, arrival);
    }

    fn user(&mut self, writer: &Writer, value: &Value, arrival: u128) {
        let message = value.get("message").unwrap_or(&Value::Null);
        for block in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let corr = self.corr(id);
            let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
            let attrs = Attrs::new().int("af.tool.output_bytes", size_of(block.get("content")));
            let span = self.tool_span(&corr);
            self.pend_value_on("tool_output", block.get("content"), span);
            self.close_tool(
                writer,
                &corr,
                arrival,
                attrs,
                if failed { "failed" } else { "succeeded" },
            );
        }
        // The tool results go back to the model: the next request, and its turn, begin now.
        self.answered = None;
        if self.turn.is_none() {
            self.open_turn(writer, arrival);
        }
    }

    // --------------------------------------------------------------------------------------
    // Codex

    fn codex(&mut self, writer: &Writer, value: &Value, arrival: u128) {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "thread.started" => {
                let mut attrs = Attrs::new().enumerated("af.stream.kind", "thread.started");
                if let Some(thread) = value.get("thread_id").and_then(Value::as_str) {
                    let corr = self.corr(thread);
                    attrs = attrs
                        .id("af.corr", corr.clone())
                        .text("gen_ai.conversation.id", corr);
                }
                self.on_harness(writer, event::INIT, attrs, arrival);
            }
            "turn.started" => {
                if self.turn.is_some() {
                    self.close_turn(writer, arrival, "incomplete", 0);
                }
                self.open_turn(writer, arrival);
            }
            "turn.completed" => {
                if self.turn.is_none() {
                    self.open_turn(writer, arrival);
                }
                if let Some(turn) = &mut self.turn {
                    turn.usage = Usage::codex(value.get("usage").unwrap_or(&Value::Null));
                    turn.last = arrival;
                }
                self.close_turn(writer, arrival, "succeeded", 0);
            }
            "turn.failed" => {
                if self.turn.is_none() {
                    self.open_turn(writer, arrival);
                }
                let error = value.get("error").and_then(|error| error.get("message"));
                let size = size_of(error);
                self.pend_value("error", error);
                self.close_turn(writer, arrival, "failed", size);
            }
            "error" => {
                let message = value.get("message");
                let attrs = Attrs::new()
                    .enumerated("af.stream.kind", "error")
                    .int("af.error.bytes", size_of(message));
                self.pend_value("error", message);
                self.on_turn(writer, event::ERROR, attrs, arrival);
            }
            "item.started" | "item.updated" | "item.completed" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                self.codex_item(writer, kind, item, arrival);
            }
            _ => {
                self.unparsed_lines += 1;
            }
        }
    }

    fn codex_item(&mut self, writer: &Writer, kind: &str, item: &Value, arrival: u128) {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let corr = self.call_key(id);
        let status = member(item.get("status").and_then(Value::as_str), &ITEM_STATUSES);
        let tool = match item_type {
            "command_execution" => Some(("command", "aggregated_output")),
            "mcp_tool_call" => Some(("arguments", "result")),
            _ => None,
        };
        if let Some((input, output)) = tool {
            if !self.tools.contains_key(&corr) && kind != "item.updated" {
                let size = size_of(item.get(input));
                if let Some(turn) = &mut self.turn {
                    turn.tool_calls += 1;
                }
                let parent = self.content_span();
                self.open_tool(writer, corr.clone(), Some(item_type), size, arrival, parent);
                let span = self.tool_span(&corr);
                self.pend_value_on("tool_input", item.get(input), span);
            }
            if kind != "item.completed" {
                return;
            }
            let mut attrs = Attrs::new().enumerated("af.tool.status", status);
            let produced = item.get(output).or_else(|| item.get("error"));
            attrs = attrs.int("af.tool.output_bytes", size_of(produced));
            let span = self.tool_span(&corr);
            self.pend_value_on("tool_output", produced, span);
            let exit = item.get("exit_code").and_then(Value::as_i64);
            if let Some(exit) = exit {
                attrs = attrs.int("af.tool.exit_code", exit);
            }
            let outcome = match status {
                "completed" if exit.is_none_or(|exit| exit == 0) => "succeeded",
                "failed" | "declined" | "completed" => "failed",
                _ => "incomplete",
            };
            self.close_tool(writer, &corr, arrival, attrs, outcome);
            return;
        }
        if kind != "item.completed" {
            return;
        }
        let mut attrs = Attrs::new().enumerated("af.stream.kind", "item.completed");
        if !id.is_empty() {
            attrs = attrs.id("af.corr", corr);
        }
        let (name, attrs) = match item_type {
            "reasoning" => {
                let size = size_of(item.get("text"));
                self.pend_value("reasoning", item.get("text"));
                (event::REASONING, attrs.int("af.item.text_bytes", size))
            }
            "agent_message" => {
                let size = size_of(item.get("text"));
                self.pend_value("agent_message", item.get("text"));
                (event::AGENT_MESSAGE, attrs.int("af.item.text_bytes", size))
            }
            "file_change" => (
                event::FILE_CHANGE,
                attrs
                    .int(
                        "af.item.changes",
                        item.get("changes")
                            .and_then(Value::as_array)
                            .map_or(0, |changes| changes.len() as i64),
                    )
                    .enumerated("af.item.status", status),
            ),
            "web_search" => {
                let size = size_of(item.get("query"));
                (event::WEB_SEARCH, attrs.int("af.item.query_bytes", size))
            }
            "todo_list" => (
                event::TODO_LIST,
                attrs.int(
                    "af.item.entries",
                    item.get("items")
                        .and_then(Value::as_array)
                        .map_or(0, |items| items.len() as i64),
                ),
            ),
            _ => (event::OTHER_ITEM, attrs),
        };
        self.on_turn(writer, name, attrs, arrival);
    }

    // --------------------------------------------------------------------------------------
    // the end of the streams and the release

    /// The process ended and every chunk the observer queued was handed over: frame what is
    /// left (a final line without its newline, a trailing gap), end every open turn and tool
    /// as `incomplete`, then apply `release`, Telemetry's own decision over the complete
    /// streams, publish the released content as blobs at `content`, and record
    /// `af.harness.ids`. What a release hands on is returned; nothing is published when the
    /// decision withheld it.
    pub fn finish(&mut self, writer: &Writer, release: &Release) -> Released {
        if self.finished {
            return Released::default();
        }
        for (stream, total) in [
            (Stream::Stdout, release.stdout_bytes),
            (Stream::Stderr, release.stderr_bytes),
        ] {
            let index = stream.index();
            let expected = self.framers[index].expected;
            let last = self.framers[index].last.max(self.last);
            if expected < total {
                self.gap(writer, stream, expected, total, last);
                self.framers[index].partial = Vec::new();
                self.framers[index].oversized = None;
                continue;
            }
            if let Some(size) = self.framers[index].oversized.take() {
                self.frame_withheld(writer, stream, size, last);
                continue;
            }
            let framer = &mut self.framers[index];
            if framer.resync || framer.partial.is_empty() {
                continue;
            }
            let line = std::mem::take(&mut framer.partial);
            self.line(writer, stream, &line, last);
        }
        let end = self.last;
        let open: Vec<String> = self.tools.keys().cloned().collect();
        for corr in open {
            self.close_tool(writer, &corr, end, Attrs::new(), "incomplete");
        }
        if let Some(turn) = &self.turn {
            if turn.message.is_some() {
                let last = turn.last;
                self.close_turn(writer, last, "succeeded", 0);
            } else {
                self.close_turn(writer, end, "incomplete", 0);
            }
        }
        self.finished = true;
        // Every identifier admitted to the pending buffer, counted before the release drains it.
        let admitted = self.ids.len();
        let released = self.release(writer, release);
        if released.dropped_contents > 0 {
            // The pending buffer is the Attempt's content allowance too: past it, the one
            // problem says so, and `af.content.dropped` below counts what went.
            writer.problem(
                crate::record::ProblemWhat::Budget,
                content::ATTEMPT_EXHAUSTED,
            );
        }
        let mut attrs = Attrs::new()
            .boolean("af.stream.withheld", release.withheld)
            .boolean("af.native.withheld", release.native_withheld)
            .int("af.harness.ids.count", admitted as i64)
            .int("af.harness.ids.withheld", released.withheld_ids as i64)
            .int("af.harness.ids.omitted", self.omitted_ids.len() as i64)
            .enumerated(
                "af.harness.ids.analysis",
                if self.omitted_ids.is_empty() {
                    "complete"
                } else {
                    "incomplete"
                },
            )
            .int("af.stream.lines", self.lines as i64)
            .int("af.stream.unparsed_lines", self.unparsed_lines as i64)
            .int("af.stream.unparsed_bytes", self.unparsed_bytes as i64)
            .int("af.stream.stderr_lines", self.stderr_lines as i64)
            .int("af.stream.stderr_bytes", self.stderr_bytes as i64)
            .int("af.stream.frames_withheld", self.frames_withheld as i64)
            .int("af.stream.gaps", self.gaps as i64)
            .int("af.stream.turns", self.turns as i64)
            .int("af.stream.tool_calls", self.tool_calls as i64)
            .int(
                "af.content.pending_bytes",
                released
                    .contents
                    .iter()
                    .map(|content| content.bytes.len() as i64)
                    .sum(),
            )
            .int("af.content.dropped", released.dropped_contents as i64)
            .int("af.content.published", released.published as i64)
            .enumerated(
                "af.stream.analysis",
                if self.gaps == 0 && self.frames_withheld == 0 {
                    "complete"
                } else {
                    "incomplete"
                },
            );
        if self.content {
            // The switches the content was kept under, for a reader that finds none.
            attrs = attrs
                .boolean("af.content.reasoning", self.options.content_reasoning)
                .boolean("af.content.tool_io", self.options.content_tool_io);
        }
        let mut parts = Vec::new();
        if !released.withheld {
            parts = id_parts(&released.ids);
            let first = if parts.is_empty() {
                Vec::new()
            } else {
                parts.remove(0)
            };
            attrs = attrs
                .texts("af.harness.ids", first)
                .int("af.harness.ids.parts", parts.len() as i64);
        }
        self.on_harness(writer, event::IDS, attrs, end);
        for (ordinal, part) in parts.into_iter().enumerate() {
            let attrs = Attrs::new()
                .int("af.harness.ids.part", ordinal as i64 + 1)
                .texts("af.harness.ids", part);
            self.on_harness(writer, event::IDS_PART, attrs, end);
        }
        released
    }

    fn release(&mut self, writer: &Writer, release: &Release) -> Released {
        let ids = std::mem::take(&mut self.ids);
        let pending = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        if release.withheld {
            return Released {
                withheld: true,
                contents: Vec::new(),
                ids: Vec::new(),
                dropped_contents: self.dropped_contents,
                withheld_ids: ids.len() as u64,
                published: 0,
            };
        }
        let mut redactor: Redactor = writer.options().redactor.clone();
        for (name, value) in &self.options.grants {
            redactor = redactor.with_grant(name, value);
        }
        let mut withheld_ids = 0;
        let ids = ids
            .into_iter()
            .filter_map(|(corr, original)| {
                let (judged, withheld) = redactor.value(&original);
                // Only an original the rules keep whole, unchanged by a replacement, is listed.
                if withheld || judged != original || original.contains(char::is_whitespace) {
                    withheld_ids += 1;
                    None
                } else {
                    Some((corr, original))
                }
            })
            .collect();
        let mut published = 0;
        let contents = pending
            .into_iter()
            .map(|content| {
                // Each content is progress under the seal (design §6): judged for its blob,
                // published, then judged for the records it is handed on with.
                writer.renew_shutdown();
                // At `content` the original becomes a blob, judged by the same rules as the
                // bytes handed on below, so its source identity is the original's.
                if self.content
                    && let Some(name) = published_as(content.kind)
                {
                    let attrs = Attrs::new()
                        .enumerated("af.content.kind", kind_member(content.kind))
                        .int("af.content.ordinal", published + 1);
                    if writer
                        .content_event_judged(
                            &content.span_id,
                            name,
                            attrs,
                            &content.bytes,
                            content.media,
                            &self.options.scope,
                            &redactor,
                        )
                        .is_some()
                    {
                        published += 1;
                    }
                }
                let bytes = match content.media {
                    Media::Json | Media::Kernel(_) => {
                        match serde_json::from_slice::<Value>(&content.bytes) {
                            Ok(mut document) => {
                                redactor.json(&mut document);
                                serde_json::to_vec(&document).unwrap_or_default()
                            }
                            Err(_) => redactor
                                .text(&String::from_utf8_lossy(&content.bytes))
                                .0
                                .into_bytes(),
                        }
                    }
                    Media::Text => redactor
                        .text(&String::from_utf8_lossy(&content.bytes))
                        .0
                        .into_bytes(),
                };
                writer.renew_shutdown();
                Content { bytes, ..content }
            })
            .collect();
        Released {
            withheld: false,
            contents,
            ids,
            dropped_contents: self.dropped_contents,
            withheld_ids,
            published: u64::try_from(published).unwrap_or(0),
        }
    }
}
