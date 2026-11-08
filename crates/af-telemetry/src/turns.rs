//! `af telemetry turns` and `tools` (design §10): one row per model turn or tool call a harness
//! stream showed (design §7.3), as `af/telemetry-turns@1` and `af/telemetry-tools@1`. Part of
//! the reader module (ADR-0153 clause 1): only `af telemetry`, `af storage` and the TUI link it.
//!
//! A turn's or a tool call's time is the observer's arrival times of the lines that opened and
//! closed it; its usage is the harness's own per-turn report, the `turn` series, never summed
//! into an Attempt's or a run's totals, which are the kernel's (design §9). A Claude message
//! printed as several lines ends its turn with the first; each further line's
//! `af.turn.continued` event is folded into the turn's row: its block and tool-call counts
//! added, its report (usage, stop reason, model) the latest, as one line of it all would be. Every row says
//! whether the stream it came from can be analysed whole: `complete` when the observer dropped
//! nothing, the tap saw no gap and judged every frame, and the tap ended after its release
//! decision; `incomplete` when one of those failed; `open` while the tap has not ended (its
//! process still runs, or its consumer stopped before it ended the tap).

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::index::Index;
use crate::stream::{HARNESS_SPAN, TOOL_SPAN, TURN_SPAN, event};
use crate::view::{Executions, IndexRead, Unreadable, executions, micros, ns_text, sql};

/// `af/telemetry-turns@1`.
pub const TURNS_SCHEMA: &str = "af/telemetry-turns@1";
/// `af/telemetry-tools@1`.
pub const TOOLS_SCHEMA: &str = "af/telemetry-tools@1";

/// The run a document describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRun {
    pub store_id: String,
    pub run_id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `prefix:k` or `partial`; never complete.
    pub history: String,
    pub missing_ordinals: Vec<u64>,
    /// Which executions the rows cover.
    pub retained: String,
}

/// The harness process a row came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    /// The `af.harness` span.
    pub span_id: String,
    pub segment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// `gen_ai.provider.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// `complete`, `incomplete` or `open` (module documentation).
    pub analysis: String,
    /// Chunks the observer dropped (`dropped_chunks`), when the span recorded them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_chunks: Option<u64>,
    /// Gaps in the observed streams (`gaps`), when the span recorded them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaps: Option<u64>,
    /// The release decision withheld the process's content (`af.stream.withheld`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld: Option<bool>,
}

/// A turn's per-turn usage as the harness reported it, exact decimal strings (ADR-0078); a
/// counter it did not report is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<String>,
    /// `complete`, `incomplete` or `unknown`; absent while the turn has no end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<String>,
}

/// One turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub harness: Harness,
    pub span_id: String,
    /// Its number in its harness's stream, from 1.
    pub ordinal: u64,
    pub start_unix_ns: String,
    /// From the run's first turn or tool call listed.
    pub start_offset_us: u64,
    /// `None` while the turn has no recorded end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    /// `ok`, `error`, `unset`, or `open` with no recorded end.
    pub status: String,
    /// `succeeded`, `failed` or `incomplete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The message's correlation id (`af.corr`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr: Option<String>,
    /// The bound model, when the harness reported it as the model that answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// From the closed enumeration of stop reasons, `other` for any other.
    pub finish_reasons: Vec<String>,
    pub usage: TurnUsage,
    pub text_blocks: u64,
    pub text_bytes: u64,
    pub thinking_blocks: u64,
    pub thinking_bytes: u64,
    pub tool_calls: u64,
}

/// `af/telemetry-turns@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turns {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: HarnessRun,
    /// `--attempt`, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    pub turns: Vec<Turn>,
    pub unreadable: Vec<Unreadable>,
}

/// One tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub harness: Harness,
    pub span_id: String,
    /// The turn that called it; `None` when the harness called it outside a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_span_id: Option<String>,
    /// `gen_ai.tool.name`: a name of the invocation's own tool set; `None` for another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The call's correlation id (`gen_ai.tool.call.id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr: Option<String>,
    pub start_unix_ns: String,
    pub start_offset_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub input_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// A command's exit code (Codex `command_execution`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// An item's status (Codex), from its closed enumeration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_status: Option<String>,
}

/// `af/telemetry-tools@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: HarnessRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    pub tools: Vec<Tool>,
    pub unreadable: Vec<Unreadable>,
}

struct Span {
    span_id: String,
    parent: Option<String>,
    segment_id: String,
    name: String,
    start: i64,
    end: Option<i64>,
    ended: bool,
    status: Option<String>,
    attributes: Map<String, Value>,
}

impl Span {
    fn attribute(&self, key: &str) -> Option<&Value> {
        self.attributes.get(key)
    }

    fn text(&self, key: &str) -> Option<String> {
        self.attribute(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn count(&self, key: &str) -> u64 {
        self.attribute(key).and_then(Value::as_u64).unwrap_or(0)
    }

    fn shown_status(&self) -> String {
        match (&self.status, self.end) {
            (Some(status), Some(_)) if self.ended => status.clone(),
            _ => "open".into(),
        }
    }

    fn duration(&self) -> Option<u64> {
        self.end
            .filter(|_| self.ended)
            .map(|end| micros(end - self.start))
    }
}

fn object(text: Option<String>) -> Map<String, Value> {
    match text.and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// The spans of `trace_id` named `names`, start and end attributes merged, the end's winning.
fn spans(index: &Index, trace_id: &str, names: &[&str]) -> Result<Vec<Span>, String> {
    let mut statement = index
        .connection()
        .prepare(
            "SELECT span_id, parent_span_id, segment_id, seq, name, start_unix_ns, end_unix_ns,
                    ended, status, attributes, end_attributes
             FROM spans WHERE trace_id = ?1 ORDER BY start_unix_ns, segment_id, seq",
        )
        .map_err(sql)?;
    let rows = statement
        .query_map(params![trace_id], |row| {
            let mut attributes = object(row.get(9)?);
            attributes.extend(object(row.get(10)?));
            Ok(Span {
                span_id: row.get(0)?,
                parent: row.get(1)?,
                segment_id: row.get(2)?,
                name: row.get(4)?,
                start: row.get(5)?,
                end: row.get(6)?,
                ended: row.get::<_, i64>(7)? != 0,
                status: row.get(8)?,
                attributes,
            })
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(rows
        .into_iter()
        .filter(|span| names.contains(&span.name.as_str()))
        .collect())
}

/// The attributes of the one `af.harness.ids` event of `span_id`, when the tap ended.
fn ids_event(index: &Index, span_id: &str) -> Result<Option<Map<String, Value>>, String> {
    let mut statement = index
        .connection()
        .prepare("SELECT attributes FROM events WHERE span_id = ?1 AND name = ?2 ORDER BY seq")
        .map_err(sql)?;
    let found: Vec<String> = statement
        .query_map(params![span_id, event::IDS], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    Ok(found.into_iter().next().map(|text| object(Some(text))))
}

/// The `af.turn.continued` events of the spans of `trace_id`, by span, in record order.
fn continuations(
    index: &Index,
    trace_id: &str,
) -> Result<BTreeMap<String, Vec<Map<String, Value>>>, String> {
    let mut statement = index
        .connection()
        .prepare(
            "SELECT events.span_id, events.attributes FROM events
             JOIN spans ON spans.span_id = events.span_id
             WHERE spans.trace_id = ?1 AND events.name = ?2
             ORDER BY events.segment_id, events.seq",
        )
        .map_err(sql)?;
    let rows = statement
        .query_map(params![trace_id, event::CONTINUED], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let mut found: BTreeMap<String, Vec<Map<String, Value>>> = BTreeMap::new();
    for (span_id, attributes) in rows {
        found
            .entry(span_id)
            .or_default()
            .push(object(Some(attributes)));
    }
    Ok(found)
}

/// Fold one `af.turn.continued` event into its turn's attributes: the counts added, the
/// report the event carries replacing the turn's.
fn fold_continued(attributes: &mut Map<String, Value>, continued: &Map<String, Value>) {
    for key in [
        "af.turn.text_blocks",
        "af.turn.text_bytes",
        "af.turn.thinking_blocks",
        "af.turn.thinking_bytes",
        "af.turn.tool_calls",
    ] {
        let sum = [attributes.get(key), continued.get(key)]
            .into_iter()
            .map(|value| value.and_then(Value::as_u64).unwrap_or(0))
            .fold(0u64, u64::saturating_add);
        attributes.insert(key.to_string(), sum.into());
    }
    let replaced = |prefixes: &[&str], attributes: &mut Map<String, Value>| {
        attributes.retain(|key, _| !prefixes.iter().any(|prefix| key.starts_with(prefix)));
        for (key, value) in continued {
            if prefixes.iter().any(|prefix| key.starts_with(prefix)) {
                attributes.insert(key.clone(), value.clone());
            }
        }
    };
    if continued.contains_key("af.usage.observation") {
        replaced(&["af.usage.", "gen_ai.usage."], attributes);
    }
    if continued.contains_key("gen_ai.response.model")
        || continued.contains_key("af.turn.model_other")
    {
        replaced(
            &["gen_ai.response.model", "af.turn.model_other"],
            attributes,
        );
    }
    for key in ["gen_ai.response.finish_reasons", "af.turn.timestamp"] {
        if let Some(value) = continued.get(key) {
            attributes.insert(key.to_string(), value.clone());
        }
    }
}

/// What both documents read: the run, its harnesses with their Attempts, and the turn and tool
/// spans below them.
struct Read {
    run: HarnessRun,
    harnesses: BTreeMap<String, Harness>,
    turns: Vec<Span>,
    tools: Vec<Span>,
    first: i64,
}

fn read(
    index: &Index,
    run: (&str, &str, &str, Option<String>),
    attempt: Option<&str>,
) -> Result<Read, String> {
    let (store_id, run_id, trace_id, kind) = run;
    let retained = Executions::of(executions(index, trace_id)?).history();
    let all = spans(
        index,
        trace_id,
        &["af.attempt", HARNESS_SPAN, TURN_SPAN, TOOL_SPAN],
    )?;
    let attempts: BTreeMap<&str, &Span> = all
        .iter()
        .filter(|span| span.name == "af.attempt")
        .map(|span| (span.span_id.as_str(), span))
        .collect();
    let mut harnesses = BTreeMap::new();
    for span in all.iter().filter(|span| span.name == HARNESS_SPAN) {
        let owner = span
            .parent
            .as_deref()
            .and_then(|parent| attempts.get(parent));
        let ids = ids_event(index, &span.span_id)?;
        let dropped = span.attribute("dropped_chunks").and_then(Value::as_u64);
        let gaps = span.attribute("gaps").and_then(Value::as_u64);
        let analysis = match &ids {
            None => "open",
            Some(ids)
                if ids.get("af.stream.analysis").and_then(Value::as_str) == Some("complete")
                    && dropped.unwrap_or(0) == 0
                    && gaps.unwrap_or(0) == 0 =>
            {
                "complete"
            }
            Some(_) => "incomplete",
        };
        harnesses.insert(
            span.span_id.clone(),
            Harness {
                span_id: span.span_id.clone(),
                segment_id: span.segment_id.clone(),
                attempt_id: owner.and_then(|owner| owner.text("af.attempt_id")),
                node_id: owner.and_then(|owner| owner.text("af.node_id")),
                provider: span.text("gen_ai.provider.name"),
                analysis: analysis.into(),
                dropped_chunks: dropped,
                gaps,
                withheld: span
                    .attribute("af.stream.withheld")
                    .and_then(Value::as_bool),
            },
        );
    }
    if let Some(attempt) = attempt
        && !attempts
            .values()
            .any(|span| span.text("af.attempt_id").as_deref() == Some(attempt))
    {
        return Err(format!("no Attempt {attempt} in run {run_id}"));
    }
    // A turn hangs below its harness; a tool below its turn or, outside one, its harness.
    let parents: BTreeMap<String, String> = all
        .iter()
        .filter_map(|span| {
            span.parent
                .clone()
                .map(|parent| (span.span_id.clone(), parent))
        })
        .collect();
    let harness_of = |span: &Span| -> Option<String> {
        let mut cursor = span.parent.as_deref();
        while let Some(id) = cursor {
            if harnesses.contains_key(id) {
                return Some(id.to_string());
            }
            cursor = parents.get(id).map(String::as_str);
        }
        None
    };
    let selected = |span: &Span| {
        harness_of(span).is_some_and(|harness| {
            attempt.is_none_or(|attempt| harnesses[&harness].attempt_id.as_deref() == Some(attempt))
        })
    };
    let mut continued = continuations(index, trace_id)?;
    let mut turns = Vec::new();
    let mut tools = Vec::new();
    for mut span in all {
        match span.name.as_str() {
            TURN_SPAN if selected(&span) => {
                for event in continued.remove(&span.span_id).unwrap_or_default() {
                    fold_continued(&mut span.attributes, &event);
                }
                turns.push(span);
            }
            TOOL_SPAN if selected(&span) => tools.push(span),
            _ => {}
        }
    }
    let first = turns
        .iter()
        .chain(tools.iter())
        .map(|span| span.start)
        .min()
        .unwrap_or(0);
    Ok(Read {
        run: HarnessRun {
            store_id: store_id.to_string(),
            run_id: run_id.to_string(),
            trace_id: trace_id.to_string(),
            kind,
            history: retained.history,
            missing_ordinals: retained.missing_ordinals,
            retained: retained.label,
        },
        harnesses,
        turns,
        tools,
        first,
    })
}

impl Read {
    fn harness_of(&self, span: &Span, by_id: &BTreeMap<String, Option<String>>) -> Harness {
        let mut cursor = span.parent.clone();
        while let Some(id) = cursor {
            if let Some(harness) = self.harnesses.get(&id) {
                return harness.clone();
            }
            cursor = by_id.get(&id).cloned().flatten();
        }
        unreachable!("a selected span has a harness")
    }
}

/// `af telemetry turns`: one row per turn of the run's harness streams, in start order;
/// `attempt` keeps one Attempt's.
pub fn turns(
    index: &Index,
    directory: &Path,
    read_state: IndexRead,
    unreadable: Vec<Unreadable>,
    run: (&str, &str, &str, Option<String>),
    attempt: Option<&str>,
) -> Result<Turns, String> {
    let read = read(index, run, attempt)?;
    let by_id = read
        .turns
        .iter()
        .chain(read.tools.iter())
        .map(|span| (span.span_id.clone(), span.parent.clone()))
        .collect();
    let turns = read
        .turns
        .iter()
        .map(|span| {
            let decimal = |key: &str| span.text(key);
            Turn {
                harness: read.harness_of(span, &by_id),
                span_id: span.span_id.clone(),
                ordinal: span.count("af.turn.ordinal"),
                start_unix_ns: ns_text(span.start),
                start_offset_us: micros(span.start - read.first),
                duration_us: span.duration(),
                status: span.shown_status(),
                outcome: span.text("af.outcome"),
                corr: span.text("af.corr"),
                model: span.text("gen_ai.response.model"),
                finish_reasons: span
                    .attribute("gen_ai.response.finish_reasons")
                    .and_then(Value::as_array)
                    .map(|reasons| {
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                usage: TurnUsage {
                    input_tokens: decimal("af.usage.input_tokens"),
                    output_tokens: decimal("af.usage.output_tokens"),
                    cache_read_tokens: decimal("af.usage.cache_read_tokens"),
                    cache_write_tokens: decimal("af.usage.cache_write_tokens"),
                    reasoning_tokens: decimal("af.usage.reasoning_tokens"),
                    observation: span.text("af.usage.observation"),
                },
                text_blocks: span.count("af.turn.text_blocks"),
                text_bytes: span.count("af.turn.text_bytes"),
                thinking_blocks: span.count("af.turn.thinking_blocks"),
                thinking_bytes: span.count("af.turn.thinking_bytes"),
                tool_calls: span.count("af.turn.tool_calls"),
            }
        })
        .collect();
    Ok(Turns {
        schema: TURNS_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read_state,
        run: read.run,
        attempt: attempt.map(str::to_string),
        turns,
        unreadable,
    })
}

/// `af telemetry tools`: one row per tool call of the run's harness streams, in start order;
/// `attempt` keeps one Attempt's.
pub fn tools(
    index: &Index,
    directory: &Path,
    read_state: IndexRead,
    unreadable: Vec<Unreadable>,
    run: (&str, &str, &str, Option<String>),
    attempt: Option<&str>,
) -> Result<Tools, String> {
    let read = read(index, run, attempt)?;
    let by_id: BTreeMap<String, Option<String>> = read
        .turns
        .iter()
        .chain(read.tools.iter())
        .map(|span| (span.span_id.clone(), span.parent.clone()))
        .collect();
    let turn_ids: std::collections::BTreeSet<&str> = read
        .turns
        .iter()
        .map(|span| span.span_id.as_str())
        .collect();
    let tools = read
        .tools
        .iter()
        .map(|span| Tool {
            harness: read.harness_of(span, &by_id),
            span_id: span.span_id.clone(),
            turn_span_id: span
                .parent
                .clone()
                .filter(|parent| turn_ids.contains(parent.as_str())),
            name: span.text("gen_ai.tool.name"),
            corr: span.text("gen_ai.tool.call.id"),
            start_unix_ns: ns_text(span.start),
            start_offset_us: micros(span.start - read.first),
            duration_us: span.duration(),
            status: span.shown_status(),
            outcome: span.text("af.outcome"),
            input_bytes: span.count("af.tool.input_bytes"),
            output_bytes: span
                .attribute("af.tool.output_bytes")
                .and_then(Value::as_u64),
            exit_code: span.attribute("af.tool.exit_code").and_then(Value::as_i64),
            item_status: span.text("af.tool.status"),
        })
        .collect();
    Ok(Tools {
        schema: TOOLS_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read_state,
        run: read.run,
        attempt: attempt.map(str::to_string),
        tools,
        unreadable,
    })
}
