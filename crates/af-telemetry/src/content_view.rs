//! `af telemetry context`, `reasoning` and `search` (design §9, §10; package P7): the content
//! level read back for the operator, as `af/telemetry-context@1`, `af/telemetry-reasoning@1` and
//! `af/telemetry-search@1`. Part of the reader module (ADR-0153 clause 1): only `af telemetry`,
//! `af storage` and the TUI link it, and no code that composes a prompt, a Warm Set, Worker
//! Notes or an optimization source reads it (clause 2) — a source test holds both.
//!
//! Every byte shown is a blob's retained bytes: redacted whole before any truncation when it was
//! written, read under `gc.lock` shared and verified against its digest. A content that was not
//! captured says why — the level it was recorded at, a switch that was off, a release decision
//! that withheld it, an allowance that dropped it, a blob file that is gone — and never fails
//! the query.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::blob::reader::{self, Content as Read};
use crate::content::event;
use crate::index::Index;
use crate::stream::{HARNESS_SPAN, event as stream_event};
use crate::turns::HarnessRun;
use crate::view::{Executions, IndexRead, Unreadable, executions, ns_text, sql};

/// `af/telemetry-context@1`.
pub const CONTEXT_SCHEMA: &str = "af/telemetry-context@1";
/// `af/telemetry-reasoning@1`.
pub const REASONING_SCHEMA: &str = "af/telemetry-reasoning@1";
/// `af/telemetry-search@1`.
pub const SEARCH_SCHEMA: &str = "af/telemetry-search@1";

/// The most matching blobs one search lists.
pub const SEARCH_LIMIT: usize = 50;
/// The most occurrences listed per matching blob.
pub const OCCURRENCE_LIMIT: usize = 20;
/// The longest excerpt a search shows, in bytes.
pub const EXCERPT_MAX_BYTES: usize = 320;

/// One content occurrence as a reader shows it: the event that names it, its occurrence and
/// object, and the retained bytes when they could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// The event that names the content.
    pub event: String,
    pub span_id: String,
    pub segment_id: String,
    /// The event's `seq`.
    pub seq: u64,
    pub time_unix_ns: String,
    /// `segment_id:seq` of the `blob_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<String>,
    /// `sha256:` over the retained bytes; absent when the content was dropped before it was
    /// digested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// `sha256:` over the original bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_bytes: Option<u64>,
    /// The retained bytes' length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_bytes: Option<u64>,
    /// Cut head and tail to `content_max_blob_bytes`.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// `stored` (the bytes are in `content`) or `unavailable` (`reason` says why).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The retained, redacted bytes, as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// `af/telemetry-context@1`: the composed Worker input of one Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: HarnessRun,
    /// The kernel Attempt id.
    pub attempt: String,
    /// `captured`, `not_captured` (below `content`), or `unavailable` (`reason` says why).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The `af.context.compose` span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_span_id: Option<String>,
    /// The level the composing execution recorded at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// `af.context.manifest`: the Worker context artifact the Attempt is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// `af.context.bytes`: the prompt's length as the kernel rendered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_bytes: Option<u64>,
    /// `af.context.estimated_tokens`, an exact decimal string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<String>,
    /// `af.context.layers`, in manifest order.
    pub layers: Vec<String>,
    /// The prompt bytes the Worker received (`gen_ai.content.input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Part>,
    /// The manifest they were composed by (`gen_ai.content.system`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<Part>,
    pub unreadable: Vec<Unreadable>,
}

/// One thinking block or reasoning summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thought {
    /// Its number in the Attempt's reasoning, from 1.
    pub ordinal: u64,
    /// `thinking` (Claude) or `reasoning` (Codex).
    pub kind: String,
    /// The `af.harness` span it came from.
    pub harness_span_id: String,
    /// The turn it belongs to, and that turn's ordinal in its stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_ordinal: Option<u64>,
    pub part: Part,
}

/// What one harness process of the Attempt kept of its reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessContent {
    pub span_id: String,
    pub segment_id: String,
    /// The level its execution recorded at.
    pub level: String,
    /// `captured`, `not_captured`, `withheld` or `open` (`reason` says why).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `af/telemetry-reasoning@1`: one Attempt's thinking blocks and reasoning summaries in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reasoning {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: HarnessRun,
    pub attempt: String,
    /// `captured` when some harness process of the Attempt kept its reasoning, else the state
    /// of the first (`not_captured`, `withheld`, `open`), or `unavailable` with no harness.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub harnesses: Vec<HarnessContent>,
    pub thoughts: Vec<Thought>,
    pub unreadable: Vec<Unreadable>,
}

/// One occurrence of a matching blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub occurrence_id: String,
    /// The event that names it.
    pub event: String,
    pub time_unix_ns: String,
    pub span_id: String,
    /// The span names from the root down to the event's span.
    pub span_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The kernel Attempt it belongs to, when it is below an `af.attempt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

/// One blob whose text matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    pub digest: String,
    /// A bounded excerpt of the retained, redacted text around the match.
    pub excerpt: String,
    pub occurrences: Vec<Occurrence>,
    /// More occurrences than listed.
    pub more_occurrences: bool,
}

/// `af/telemetry-search@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    /// The text searched for, as one phrase.
    pub query: String,
    /// `--since`, as the oldest occurrence time listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_unix_ns: Option<String>,
    /// `--run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub matches: Vec<Match>,
    /// More matching blobs than listed.
    pub more: bool,
    pub unreadable: Vec<Unreadable>,
}

// ------------------------------------------------------------------------------------------
// reading

struct Span {
    span_id: String,
    parent: Option<String>,
    segment_id: String,
    name: String,
    attributes: Map<String, Value>,
}

impl Span {
    fn text(&self, key: &str) -> Option<String> {
        self.attributes
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

fn object(text: Option<String>) -> Map<String, Value> {
    match text.and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Every span of `trace_id`, start and end attributes merged, the end's winning.
fn spans(index: &Index, trace_id: &str) -> Result<Vec<Span>, String> {
    let mut statement = index
        .connection()
        .prepare(
            "SELECT span_id, parent_span_id, segment_id, name, attributes, end_attributes
             FROM spans WHERE trace_id = ?1 ORDER BY start_unix_ns, segment_id, seq",
        )
        .map_err(sql)?;
    statement
        .query_map(params![trace_id], |row| {
            let mut attributes = object(row.get(4)?);
            attributes.extend(object(row.get(5)?));
            Ok(Span {
                span_id: row.get(0)?,
                parent: row.get(1)?,
                segment_id: row.get(2)?,
                name: row.get(3)?,
                attributes,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

/// The level a segment was recorded at.
fn level(index: &Index, segment_id: &str) -> Result<String, String> {
    index
        .connection()
        .query_row(
            "SELECT level FROM segments WHERE segment_id = ?1",
            params![segment_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)
        .map(|level| level.unwrap_or_else(|| "unknown".into()))
}

/// One event row.
struct EventRow {
    segment_id: String,
    seq: u64,
    span_id: String,
    time: i64,
    name: String,
    attributes: Map<String, Value>,
    blob: Option<String>,
}

/// The events named `names` on the spans `span_ids`, in segment and `seq` order.
fn events_on(index: &Index, span_ids: &[&str], names: &[&str]) -> Result<Vec<EventRow>, String> {
    let mut rows = Vec::new();
    let mut statement = index
        .connection()
        .prepare(
            "SELECT segment_id, seq, span_id, time_unix_ns, name, attributes, blob FROM events
             WHERE span_id = ?1 ORDER BY segment_id, seq",
        )
        .map_err(sql)?;
    for span_id in span_ids {
        let found = statement
            .query_map(params![span_id], |row| {
                Ok(EventRow {
                    segment_id: row.get(0)?,
                    seq: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    span_id: row.get(2)?,
                    time: row.get(3)?,
                    name: row.get(4)?,
                    attributes: object(row.get(5)?),
                    blob: row.get(6)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        rows.extend(
            found
                .into_iter()
                .filter(|row| names.contains(&row.name.as_str())),
        );
    }
    rows.sort_by(|a, b| (a.segment_id.as_str(), a.seq).cmp(&(b.segment_id.as_str(), b.seq)));
    Ok(rows)
}

/// The content an event names, read from its blob file under `gc.lock` shared.
fn part(index: &Index, row: &EventRow) -> Result<Part, String> {
    let mut part = Part {
        event: row.name.clone(),
        span_id: row.span_id.clone(),
        segment_id: row.segment_id.clone(),
        seq: row.seq,
        time_unix_ns: ns_text(row.time),
        occurrence_id: row.blob.clone(),
        digest: None,
        source_digest: None,
        original_bytes: None,
        retained_bytes: None,
        truncated: false,
        media_type: None,
        state: "unavailable".into(),
        reason: None,
        content: None,
    };
    let Some(occurrence) = row.blob.as_deref() else {
        part.reason = Some("content unavailable: the event names no occurrence".into());
        return Ok(part);
    };
    let Some(reference) = index.occurrence(occurrence)? else {
        part.reason = Some("content unavailable: its occurrence record is not retained".into());
        return Ok(part);
    };
    part.digest = reference.digest.clone();
    part.source_digest = Some(reference.source_digest.clone());
    part.original_bytes = Some(reference.original_bytes);
    part.truncated = reference.truncated;
    part.media_type = Some(reference.media_type.clone());
    let read = match index.versioned() {
        Some(versioned) => match (&reference.state, &reference.digest) {
            (crate::record::BlobState::Stored, Some(digest)) => reader::read_in(versioned, digest),
            _ => Read::Unavailable(reader::DROPPED.into()),
        },
        None => Read::Unavailable(reader::MISSING.into()),
    };
    match read {
        Read::Bytes(bytes) => {
            part.retained_bytes = Some(bytes.len() as u64);
            part.state = "stored".into();
            part.content = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        Read::Unavailable(reason) => part.reason = Some(reason),
    }
    Ok(part)
}

/// The run a document describes.
fn run_of(index: &Index, run: (&str, &str, &str, Option<String>)) -> Result<HarnessRun, String> {
    let (store_id, run_id, trace_id, kind) = run;
    let retained = Executions::of(executions(index, trace_id)?).history();
    Ok(HarnessRun {
        store_id: store_id.to_string(),
        run_id: run_id.to_string(),
        trace_id: trace_id.to_string(),
        kind,
        history: retained.history,
        missing_ordinals: retained.missing_ordinals,
        retained: retained.label,
    })
}

/// The `af.attempt` spans of `attempt` in the run, or why there is none.
fn attempt_spans<'a>(
    all: &'a [Span],
    run_id: &str,
    attempt: &str,
) -> Result<Vec<&'a Span>, String> {
    let found: Vec<&Span> = all
        .iter()
        .filter(|span| {
            span.name == "af.attempt" && span.text("af.attempt_id").as_deref() == Some(attempt)
        })
        .collect();
    if found.is_empty() {
        return Err(format!("no Attempt {attempt} in run {run_id}"));
    }
    Ok(found)
}

/// The spans below any of `roots` (the roots excluded), by parent.
fn below<'a>(all: &'a [Span], roots: &[&str]) -> Vec<&'a Span> {
    let mut children: BTreeMap<&str, Vec<&Span>> = BTreeMap::new();
    for span in all {
        if let Some(parent) = span.parent.as_deref() {
            children.entry(parent).or_default().push(span);
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<&str> = roots.to_vec();
    while let Some(id) = stack.pop() {
        for child in children.get(id).into_iter().flatten() {
            out.push(*child);
            stack.push(&child.span_id);
        }
    }
    out
}

/// Why a span recorded at `level` kept no content, when that is why.
fn below_content(level: &str) -> Option<String> {
    (level != "content").then(|| {
        format!(
            "content was not captured: this execution recorded at level {level}; only \
             [telemetry] level = \"content\" keeps it"
        )
    })
}

/// `af telemetry context RUN ATTEMPT`: the composed Worker input of one Attempt — the prompt
/// bytes the Worker received and the manifest they were composed by, as the content level kept
/// them; below `content`, why there is nothing.
pub fn context(
    index: &Index,
    directory: &Path,
    read_state: IndexRead,
    unreadable: Vec<Unreadable>,
    run: (&str, &str, &str, Option<String>),
    attempt: &str,
) -> Result<Context, String> {
    let all = spans(index, run.2)?;
    let attempts = attempt_spans(&all, run.1, attempt)?;
    let roots: Vec<&str> = attempts.iter().map(|span| span.span_id.as_str()).collect();
    let compose = below(&all, &roots)
        .into_iter()
        .rfind(|span| span.name == "af.context.compose");
    let mut document = Context {
        schema: CONTEXT_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read_state,
        run: run_of(index, run)?,
        attempt: attempt.to_string(),
        state: "unavailable".into(),
        reason: None,
        compose_span_id: None,
        level: None,
        manifest: None,
        rendered_bytes: None,
        estimated_tokens: None,
        layers: Vec::new(),
        input: None,
        system: None,
        unreadable,
    };
    let Some(compose) = compose else {
        document.reason = Some(
            "no composed Worker input was recorded for this Attempt (no af.context.compose)".into(),
        );
        return Ok(document);
    };
    let level = level(index, &compose.segment_id)?;
    document.compose_span_id = Some(compose.span_id.clone());
    document.manifest = compose.text("af.context.manifest");
    document.rendered_bytes = compose
        .attributes
        .get("af.context.bytes")
        .and_then(Value::as_u64);
    document.estimated_tokens =
        compose
            .attributes
            .get("af.context.estimated_tokens")
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            });
    document.layers = compose
        .text("af.context.layers")
        .map(|layers| {
            layers
                .split(',')
                .filter(|layer| !layer.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    document.level = Some(level.clone());
    if let Some(reason) = below_content(&level) {
        document.state = "not_captured".into();
        document.reason = Some(reason);
        return Ok(document);
    }
    let rows = events_on(
        index,
        &[compose.span_id.as_str()],
        &[event::INPUT, event::SYSTEM],
    )?;
    for row in &rows {
        let part = part(index, row)?;
        if row.name == event::INPUT {
            document.input = Some(part);
        } else {
            document.system = Some(part);
        }
    }
    match &document.input {
        Some(input) if input.state == "stored" => document.state = "captured".into(),
        Some(input) => {
            document.reason.clone_from(&input.reason);
        }
        None => {
            document.reason = Some(
                "content unavailable: the composed input was not recorded (the segment was \
                 exhausted, or the writer failed)"
                    .into(),
            );
        }
    }
    Ok(document)
}

/// `af telemetry reasoning RUN ATTEMPT`: the thinking blocks and reasoning summaries of one
/// Attempt's harness processes, in the order they arrived.
pub fn reasoning(
    index: &Index,
    directory: &Path,
    read_state: IndexRead,
    unreadable: Vec<Unreadable>,
    run: (&str, &str, &str, Option<String>),
    attempt: &str,
) -> Result<Reasoning, String> {
    let all = spans(index, run.2)?;
    let attempts = attempt_spans(&all, run.1, attempt)?;
    let roots: Vec<&str> = attempts.iter().map(|span| span.span_id.as_str()).collect();
    let beneath = below(&all, &roots);
    let harnesses: Vec<&Span> = beneath
        .iter()
        .copied()
        .filter(|span| span.name == HARNESS_SPAN)
        .collect();
    let mut document = Reasoning {
        schema: REASONING_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read_state,
        run: run_of(index, run)?,
        attempt: attempt.to_string(),
        state: "unavailable".into(),
        reason: None,
        harnesses: Vec::new(),
        thoughts: Vec::new(),
        unreadable,
    };
    if harnesses.is_empty() {
        document.reason = Some("no harness process was recorded for this Attempt".into());
        return Ok(document);
    }
    let turns: BTreeMap<&str, &Span> = beneath
        .iter()
        .copied()
        .filter(|span| span.name == crate::stream::TURN_SPAN)
        .map(|span| (span.span_id.as_str(), span))
        .collect();
    for harness in harnesses {
        let level = level(index, &harness.segment_id)?;
        let ids = events_on(index, &[harness.span_id.as_str()], &[stream_event::IDS])?;
        let (state, reason) = if let Some(reason) = below_content(&level) {
            ("not_captured", Some(reason))
        } else {
            match ids.first() {
                None => (
                    "open",
                    Some(
                        "the harness stream has not been released: its process still runs, or \
                         its tap never ended"
                            .to_string(),
                    ),
                ),
                Some(ids)
                    if ids.attributes.get("af.stream.withheld") == Some(&Value::Bool(true)) =>
                {
                    (
                        "withheld",
                        Some(
                            "Telemetry's release decision withheld this process's content \
                         (login-challenge material, or the adapter replaced its streams)"
                                .to_string(),
                        ),
                    )
                }
                Some(ids)
                    if ids.attributes.get("af.content.reasoning") == Some(&Value::Bool(false)) =>
                {
                    (
                        "not_captured",
                        Some(
                            "content was not captured: [telemetry] content_reasoning = false"
                                .to_string(),
                        ),
                    )
                }
                Some(_) => ("captured", None),
            }
        };
        if state == "captured" {
            let spans: Vec<&str> = std::iter::once(harness.span_id.as_str())
                .chain(
                    below(&all, &[harness.span_id.as_str()])
                        .into_iter()
                        .map(|span| span.span_id.as_str()),
                )
                .collect();
            for row in events_on(index, &spans, &[event::REASONING])? {
                let turn = turns.get(row.span_id.as_str());
                let kind = row
                    .attributes
                    .get("af.content.kind")
                    .and_then(Value::as_str)
                    .unwrap_or("thinking")
                    .to_string();
                document.thoughts.push(Thought {
                    ordinal: document.thoughts.len() as u64 + 1,
                    kind,
                    harness_span_id: harness.span_id.clone(),
                    turn_span_id: turn.map(|turn| turn.span_id.clone()),
                    turn_ordinal: turn.and_then(|turn| {
                        turn.attributes
                            .get("af.turn.ordinal")
                            .and_then(Value::as_u64)
                    }),
                    part: part(index, &row)?,
                });
            }
        }
        document.harnesses.push(HarnessContent {
            span_id: harness.span_id.clone(),
            segment_id: harness.segment_id.clone(),
            level,
            state: state.into(),
            reason,
        });
    }
    if let Some(harness) = document
        .harnesses
        .iter()
        .find(|harness| harness.state == "captured")
        .or_else(|| document.harnesses.first())
    {
        document.state.clone_from(&harness.state);
        document.reason.clone_from(&harness.reason);
    }
    Ok(document)
}

/// The FTS5 query of `text`: one phrase, its double quotes doubled, so no operator in the text
/// is ever interpreted.
fn phrase(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// `text` cut to at most `max` bytes at a character boundary, with `…` when cut.
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut at = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    format!("{}…", &text[..at])
}

/// The occurrences a search selects: stored, at or after `?2` when given, in the run of trace
/// `?3` when given. Over `blob_refs r`, `events e` and `spans s`.
const SELECTED: &str = "r.state = 'stored'
     AND (?2 IS NULL OR e.time_unix_ns >= ?2)
     AND (?3 IS NULL OR s.trace_id = ?3)";

/// `af telemetry search 'text'`: the blobs whose retained text holds `text` as a phrase, each
/// with a bounded excerpt and its occurrences with their span paths; `since_unix_ns` keeps the
/// occurrences at or after it, `run` (`run_id`, `trace_id`) the run's. Reads `content_fts`,
/// which ingest keeps when `[telemetry] content_index` is on.
///
/// Bounded whatever the corpus: the matching blobs are read in pages, in digest order, with the
/// filters applied in the query, and no more than [`SEARCH_LIMIT`] are kept, one more only to say
/// `more`; each blob's occurrences likewise, [`OCCURRENCE_LIMIT`] and one. Every excerpt is
/// returned only after its blob was read under `gc.lock` shared and verified against its digest,
/// as the content readers read it: a blob that is missing or corrupt is listed in `unreadable`
/// with the reader's reason, never shown, and leaves `content_fts` (its `content_index` row says
/// `unavailable`, as an ingest that found it so would), so no later search returns its text; a
/// blob a collection holds is listed and left for the next search.
pub fn search(
    index: &Index,
    directory: &Path,
    read_state: IndexRead,
    unreadable: Vec<Unreadable>,
    text: &str,
    since_unix_ns: Option<i64>,
    run: Option<(&str, &str)>,
) -> Result<Search, String> {
    let mut document = Search {
        schema: SEARCH_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read_state,
        query: text.to_string(),
        since_unix_ns: since_unix_ns.map(ns_text),
        run_id: run.map(|(run_id, _)| run_id.to_string()),
        matches: Vec::new(),
        more: false,
        unreadable,
    };
    if text.trim().is_empty() {
        return Ok(document);
    }
    let query = phrase(text);
    let trace = run.map(|(_, trace)| trace);
    let connection = index.connection();
    let mut page = connection
        .prepare(&format!(
            "SELECT content_fts.rowid, content_fts.digest FROM content_fts
             WHERE content_fts MATCH ?1 AND content_fts.digest > ?4
               AND EXISTS (SELECT 1 FROM blob_refs r
                   JOIN events e ON e.segment_id = r.segment_id AND e.seq = r.event_seq
                   LEFT JOIN spans s ON s.span_id = r.span_id
                   WHERE r.digest = content_fts.digest AND {SELECTED})
             ORDER BY content_fts.digest LIMIT ?5"
        ))
        .map_err(sql)?;
    let mut excerpt = connection
        .prepare(
            "SELECT snippet(content_fts, 1, '', '', '…', 24) FROM content_fts
             WHERE content_fts MATCH ?1 AND rowid = ?2",
        )
        .map_err(sql)?;
    let mut occurrences_of = connection
        .prepare(&format!(
            "SELECT r.occurrence_id, e.name, e.time_unix_ns, r.span_id, s.trace_id
             FROM blob_refs r
             JOIN events e ON e.segment_id = r.segment_id AND e.seq = r.event_seq
             LEFT JOIN spans s ON s.span_id = r.span_id
             WHERE r.digest = ?1 AND {SELECTED}
             ORDER BY e.time_unix_ns, r.occurrence_id LIMIT ?4"
        ))
        .map_err(sql)?;
    let mut spans = SpanIndex::default();
    let layout = crate::layout::Layout::new(directory);
    let page_size = i64::try_from(SEARCH_LIMIT + 1).unwrap_or(i64::MAX);
    let mut after = String::new();
    'pages: loop {
        let candidates: Vec<(i64, String)> = page
            .query_map(
                params![query, since_unix_ns, trace, after, page_size],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let last_page = candidates.len() < SEARCH_LIMIT + 1;
        for (rowid, digest) in candidates {
            after.clone_from(&digest);
            // The excerpt is the blob's text: shown only once the blob reads and verifies.
            let read = match index.versioned() {
                Some(versioned) => reader::read_in(versioned, &digest),
                None => Read::Unavailable(reader::MISSING.into()),
            };
            if let Read::Unavailable(reason) = read {
                if reason != reader::COLLECTING {
                    forget_text(index, rowid, &digest)?;
                }
                document.unreadable.push(Unreadable {
                    path: layout.blob(&digest).display().to_string(),
                    reason,
                });
                continue;
            }
            if document.matches.len() == SEARCH_LIMIT {
                document.more = true;
                break 'pages;
            }
            let found: String = excerpt
                .query_row(params![query, rowid], |row| row.get(0))
                .map_err(sql)?;
            let mut rows: Vec<(String, String, i64, String, Option<String>)> = occurrences_of
                .query_map(
                    params![
                        digest,
                        since_unix_ns,
                        trace,
                        i64::try_from(OCCURRENCE_LIMIT + 1).unwrap_or(i64::MAX)
                    ],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?;
            let more_occurrences = rows.len() > OCCURRENCE_LIMIT;
            rows.truncate(OCCURRENCE_LIMIT);
            let mut occurrences = Vec::with_capacity(rows.len());
            for (occurrence_id, event, time, span_id, trace) in rows {
                let (store_id, run_id) = match trace.as_deref() {
                    Some(trace) => spans.run(index, trace)?.unzip(),
                    None => (None, None),
                };
                occurrences.push(Occurrence {
                    occurrence_id,
                    event,
                    time_unix_ns: ns_text(time),
                    span_path: spans.path(index, &span_id)?,
                    attempt_id: spans.attempt(index, &span_id)?,
                    span_id,
                    store_id,
                    run_id,
                });
            }
            document.matches.push(Match {
                digest,
                excerpt: bounded(&found, EXCERPT_MAX_BYTES),
                occurrences,
                more_occurrences,
            });
        }
        if last_page {
            break;
        }
    }
    Ok(document)
}

/// Take the text of a blob that is missing or corrupt out of the search index, in step with what
/// an ingest that found it so records: no `content_fts` row, and `content_index` saying
/// `unavailable`, so it is not read again until it is referenced anew.
fn forget_text(index: &Index, rowid: i64, digest: &str) -> Result<(), String> {
    let transaction = index.connection().unchecked_transaction().map_err(sql)?;
    transaction
        .execute("DELETE FROM content_fts WHERE rowid = ?1", params![rowid])
        .map_err(sql)?;
    transaction
        .execute(
            "INSERT OR REPLACE INTO content_index(digest, state) VALUES (?1, 'unavailable')",
            params![digest],
        )
        .map_err(sql)?;
    transaction.commit().map_err(sql)
}

/// A span's name, its parent and the Attempt id an `af.attempt` names.
type SpanNames = (String, Option<String>, Option<String>);

/// What a search names an occurrence by, looked up span by span as occurrences are listed and
/// kept for the search: each span's name, parent and Attempt id, and each run by trace. Never
/// more than the listed occurrences' paths.
#[derive(Default)]
struct SpanIndex {
    spans: BTreeMap<String, Option<SpanNames>>,
    runs: BTreeMap<String, Option<(String, String)>>,
}

impl SpanIndex {
    /// The name, parent and Attempt id of `span_id`, when the index holds it.
    fn span(&mut self, index: &Index, span_id: &str) -> Result<Option<SpanNames>, String> {
        if let Some(known) = self.spans.get(span_id) {
            return Ok(known.clone());
        }
        let found = index
            .connection()
            .query_row(
                "SELECT name, parent_span_id,
                        CASE WHEN name = 'af.attempt' THEN attributes ELSE '{}' END
                 FROM spans WHERE span_id = ?1",
                params![span_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(sql)?
            .map(|(name, parent, attributes)| {
                let attempt = (name == "af.attempt")
                    .then(|| object(Some(attributes)).get("af.attempt_id").cloned())
                    .flatten()
                    .and_then(|value| value.as_str().map(str::to_string));
                (name, parent, attempt)
            });
        self.spans.insert(span_id.to_string(), found.clone());
        Ok(found)
    }

    /// The Store and run of `trace`.
    fn run(&mut self, index: &Index, trace: &str) -> Result<Option<(String, String)>, String> {
        if let Some(known) = self.runs.get(trace) {
            return Ok(known.clone());
        }
        let found = index
            .connection()
            .query_row(
                "SELECT store_id, run_id FROM runs WHERE trace_id = ?1",
                params![trace],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        self.runs.insert(trace.to_string(), found.clone());
        Ok(found)
    }

    /// The names from the root down to `span_id` (at most 64 deep).
    fn path(&mut self, index: &Index, span_id: &str) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        let mut cursor = Some(span_id.to_string());
        while let Some(id) = cursor {
            let Some((name, parent, _)) = self.span(index, &id)? else {
                break;
            };
            path.push(name);
            if path.len() == 64 {
                break;
            }
            cursor = parent;
        }
        path.reverse();
        Ok(path)
    }

    /// The Attempt id of the nearest `af.attempt` at or above `span_id`.
    fn attempt(&mut self, index: &Index, span_id: &str) -> Result<Option<String>, String> {
        let mut cursor = Some(span_id.to_string());
        let mut depth = 0;
        while let Some(id) = cursor {
            let Some((_, parent, attempt)) = self.span(index, &id)? else {
                return Ok(None);
            };
            if attempt.is_some() {
                return Ok(attempt);
            }
            depth += 1;
            if depth == 64 {
                return Ok(None);
            }
            cursor = parent;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_phrase_never_reaches_the_query_language() {
        assert_eq!(phrase("a \"b\" OR c*"), "\"a \"\"b\"\" OR c*\"");
    }

    #[test]
    fn excerpts_are_bounded_at_a_character_boundary() {
        let text = "é".repeat(400);
        let cut = bounded(&text, EXCERPT_MAX_BYTES);
        assert!(cut.len() <= EXCERPT_MAX_BYTES, "{}", cut.len());
        assert!(cut.ends_with('…'));
        assert_eq!(bounded("short", EXCERPT_MAX_BYTES), "short");
    }
}
