//! `af telemetry runs`, `show` and `tail` (design §10), as documents: `af/telemetry-runs@1`,
//! `af/telemetry-show@1` and `af/telemetry-tail@1`. Part of the reader module (ADR-0153
//! clause 1): only `af telemetry`, `af storage` and the TUI link it.
//!
//! **Executions** (design §5). A run's trace holds one execution per process that worked on it:
//! its `af.run` span, named by `af.execution_id` (the segment id) and numbered by
//! `af.execution.ordinal`. Executions can overlap — a lease is taken over after it expires
//! without waiting for the fenced predecessor to exit (ADR-0089) — so a run's **active time** is
//! the union of its executions' observed `af.run` intervals, an overlap counted once; its
//! **execution time** (`af.execution.duration_sum`) is their sum; its **elapsed time** runs from
//! the first execution's start to the last one's end, idle gaps included. An interval without an
//! end (a segment never sealed by its writer) ends at its segment's last record time and is
//! marked `open`.
//!
//! **Retained history.** A reader cannot learn from retained segments how many executions a
//! run had, so it never says a run's history is complete: it is `prefix:k` when executions 1 to
//! k are retained with their seals and no ordinal between them is missing, and `partial`, with
//! the missing ordinals, when one is. Every total is labelled `retained executions 1–k` or
//! `partial`.
//!
//! **Open executions.** An execution is open while its segment is not sealed (a writer holds
//! it, or one that stopped left it unsealed) or its `af.run` has no recorded end. Open
//! executions are listed beside the sealed ones (`executions.open`), never hidden: the label
//! reads `retained executions 1–k` only when no listed execution is open, and otherwise names
//! them (`retained executions 1–k, open: m`, or `retained executions: none sealed, open: m`),
//! and `runs` says which executions its totals cover, each open one marked `open`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::index::{Index, Mode};
use crate::layout::{self, Confined, Layout};
use crate::record::{Record, SpanStatus};
use crate::segment::reader::{self, Position};

/// `af/telemetry-runs@1`.
pub const RUNS_SCHEMA: &str = "af/telemetry-runs@1";
/// `af/telemetry-show@1`.
pub const SHOW_SCHEMA: &str = "af/telemetry-show@1";
/// `af/telemetry-tail@1`, one line per span end or event.
pub const TAIL_SCHEMA: &str = "af/telemetry-tail@1";
/// The assembled configuration record `af telemetry config` prints (design §7.4, §10).
pub const CONFIG_SCHEMA: &str = crate::record::schema::CONFIG;

/// The attributes `show` prints beside each span; `--span` prints every one.
pub const KEY_ATTRIBUTES: [&str; 46] = [
    "af.run.kind",
    "af.task_id",
    "af.campaign",
    "af.round",
    "af.epoch",
    "af.execution.ordinal",
    "af.policy_rev",
    "af.pipeline",
    "af.pipeline.digest",
    "af.plan_digest",
    "af.node_count",
    "af.node_id",
    "af.node.kind",
    "af.attempt_id",
    "af.worker",
    "af.provider",
    "af.provider.kind",
    "af.outcome",
    "af.failure.class",
    "af.failure.reason",
    "error.type",
    "af.usage.chargeable_tokens",
    "af.usage.observation",
    "process.exit_code",
    "af.sweep.bytes_before",
    "af.sweep.bytes_after",
    "af.sweep.evicted",
    "af.snapshot",
    "af.snapshot.files",
    "af.cas.op",
    "af.warm.result",
    "af.gate.decision",
    "af.check.name",
    "af.check.status",
    "af.check.fingerprint",
    "af.check.warm",
    "af.context.estimated_tokens",
    "af.admission.wait_ms",
    "af.output.valid",
    "af.store.op",
    "af.git.op",
    "af.process.kind",
    "af.measurement.id",
    "af.rusage.user_ms",
    "af.rusage.system_ms",
    "af.rusage.max_rss_bytes",
];

/// How the index was read for a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexRead {
    /// `fresh` (every new byte ingested now) or `direct` (the segments were read directly).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unreadable {
    pub path: String,
    pub reason: String,
}

/// The index of `root`, every new segment byte ingested: the one way `runs` and `show` read.
/// `Err` when the directory or its index cannot be read (the reader fails closed).
pub fn open(root: &Path) -> Result<(Index, IndexRead, Vec<Unreadable>), String> {
    let layout = Layout::new(root);
    let root_dir = Confined::open_root(root)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotADirectory => error.to_string(),
            _ => format!("{}: {error}", root.display()),
        })?
        .ok_or_else(|| format!("no telemetry directory at {}", root.display()))?;
    let mut unreadable: Vec<Unreadable> = reader::other_versions(&root_dir)
        .into_iter()
        .map(|name| Unreadable {
            path: root_dir.join(&name).display().to_string(),
            reason: "written under another telemetry version; never read".into(),
        })
        .collect();
    let mut index = match root_dir
        .child_dir(layout::VERSION_DIR)
        .map_err(|error| format!("{}: {error}", layout.versioned().display()))?
    {
        Some(versioned) => Index::open_in(&layout, versioned)?,
        None => Index::direct_in(&layout, None, "nothing has been recorded here")?,
    };
    let (_, refused) = index.ingest()?;
    unreadable.extend(
        refused
            .into_iter()
            .map(|(path, reason)| Unreadable { path, reason }),
    );
    let read = IndexRead {
        state: match index.mode() {
            Mode::Shared => "fresh".into(),
            Mode::Direct => "direct".into(),
        },
        reason: index.note.clone(),
    };
    Ok((index, read, unreadable))
}

fn sql(error: rusqlite::Error) -> String {
    format!("telemetry index: {error}")
}

/// Unix nanoseconds as the decimal text documents carry (a JSON number loses precision).
fn ns_text(value: i64) -> String {
    value.to_string()
}

fn micros(nanos: i64) -> u64 {
    u64::try_from(nanos.max(0) / 1_000).unwrap_or(0)
}

// ------------------------------------------------------------------------------------------
// executions and history

/// One execution of a run, as its retained `af.run` span says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub execution_id: String,
    /// The `af.run` span.
    pub span_id: String,
    /// `af.execution.ordinal`: its number among the run's executions, from the Store's
    /// committed lease records; `None` when the span did not carry one.
    pub ordinal: Option<u64>,
    /// `af.store.sequence_at_start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_sequence_at_start: Option<String>,
    pub start_unix_ns: String,
    /// The `af.run` span's end, or, when it has none, its segment's last record time.
    pub end_unix_ns: String,
    /// The interval has no recorded end: it ends at its segment's last record time.
    pub open: bool,
    /// Its own `af.run` duration.
    pub wall_us: u64,
    /// The segment's state: `live`, `unsealed` or `sealed`.
    pub segment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl Execution {
    /// Its segment is not sealed, or its `af.run` has no recorded end.
    pub fn is_open(&self) -> bool {
        self.open || self.segment != "sealed"
    }

    fn interval(&self) -> (i64, i64) {
        let start = self.start_unix_ns.parse().unwrap_or(0);
        let end = self.end_unix_ns.parse().unwrap_or(start);
        (start, end.max(start))
    }
}

/// One open execution, listed beside the sealed ones: its segment is not sealed, or its
/// `af.run` has no recorded end (its interval ends at its segment's last record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenExecution {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    /// The segment's state: `live`, `unsealed`, or `sealed` (recovered with its `af.run`
    /// never ended).
    pub segment: String,
}

/// A run's retained executions: the ordinals of the sealed ones, the open ones beside them,
/// and every one in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Executions {
    /// How many executions are retained, sealed or open.
    pub retained: u64,
    /// The ordinals of the retained executions that are sealed and not open.
    pub sealed: Vec<u64>,
    /// Every open execution: never hidden behind the sealed prefix.
    pub open: Vec<OpenExecution>,
    /// Every retained execution, oldest first.
    pub list: Vec<Execution>,
}

impl Executions {
    fn of(list: Vec<Execution>) -> Self {
        let mut sealed: Vec<u64> = list
            .iter()
            .filter(|execution| !execution.is_open())
            .filter_map(|execution| execution.ordinal)
            .collect();
        sealed.sort_unstable();
        sealed.dedup();
        Self {
            retained: list.len() as u64,
            sealed,
            open: list
                .iter()
                .filter(|execution| execution.is_open())
                .map(|execution| OpenExecution {
                    execution_id: execution.execution_id.clone(),
                    ordinal: execution.ordinal,
                    segment: execution.segment.clone(),
                })
                .collect(),
            list,
        }
    }

    /// The history of these executions.
    fn history(&self) -> History {
        history(
            &self
                .list
                .iter()
                .map(|execution| {
                    (
                        execution.ordinal,
                        execution.segment == "sealed",
                        execution.is_open(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    }
}

/// One execution a run's totals cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Covered {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    /// Its figures run to its segment's last record: it is still open.
    pub open: bool,
}

/// A run's retained history (design §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    /// `prefix:k` or `partial`; never complete.
    pub history: String,
    /// The ordinals below the highest retained one that no retained execution carries.
    pub missing_ordinals: Vec<u64>,
    /// How every total over the run is labelled: `retained executions 1–k` when no execution
    /// is open, `retained executions 1–k, open: m…` (or `retained executions: none sealed,
    /// open: m…`) when some are, or `partial` with the missing ordinals (and the open ones).
    pub label: String,
}

fn joined(ordinals: impl IntoIterator<Item = u64>) -> String {
    ordinals
        .into_iter()
        .map(|ordinal| ordinal.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The retained history of executions with these ordinals, each with whether its segment is
/// sealed and whether it is open.
pub fn history(executions: &[(Option<u64>, bool, bool)]) -> History {
    let present: BTreeMap<u64, bool> = executions
        .iter()
        .filter_map(|(ordinal, sealed, _)| ordinal.map(|ordinal| (ordinal, *sealed)))
        .fold(BTreeMap::new(), |mut present, (ordinal, sealed)| {
            let entry = present.entry(ordinal).or_insert(sealed);
            *entry |= sealed;
            present
        });
    let open: BTreeSet<u64> = executions
        .iter()
        .filter(|(_, _, open)| *open)
        .filter_map(|(ordinal, _, _)| *ordinal)
        .collect();
    let open_unnumbered = executions
        .iter()
        .any(|(ordinal, _, open)| *open && ordinal.is_none());
    // The open executions, named after whatever the label says of the rest.
    let open_note = if open.is_empty() && !open_unnumbered {
        String::new()
    } else {
        let mut named = joined(open.iter().copied());
        if open_unnumbered {
            if !named.is_empty() {
                named.push_str(", ");
            }
            named.push_str("one without an ordinal");
        }
        format!(", open: {named}")
    };
    let unnumbered = executions.iter().any(|(ordinal, _, _)| ordinal.is_none());
    let highest = present.keys().next_back().copied().unwrap_or(0);
    let missing: Vec<u64> = (1..highest)
        .filter(|ordinal| !present.contains_key(ordinal))
        .collect();
    if !missing.is_empty() || unnumbered || present.is_empty() {
        let label = if missing.is_empty() {
            "partial (an execution carries no ordinal)".to_string()
        } else {
            format!("partial (missing executions {})", joined(missing.clone()))
        };
        return History {
            history: "partial".into(),
            missing_ordinals: missing,
            label: format!("{label}{open_note}"),
        };
    }
    let prefix = (1..=highest)
        .take_while(|ordinal| present.get(ordinal).copied().unwrap_or(false))
        .last()
        .unwrap_or(0);
    // Sealed, closed executions beyond the prefix (after an open one) are named too.
    let beyond: Vec<u64> = executions
        .iter()
        .filter(|(_, sealed, open)| *sealed && !*open)
        .filter_map(|(ordinal, _, _)| *ordinal)
        .filter(|ordinal| *ordinal > prefix)
        .collect::<BTreeSet<u64>>()
        .into_iter()
        .collect();
    let mut label = match prefix {
        0 => "retained executions: none sealed".to_string(),
        k => format!("retained executions 1–{k}"),
    };
    if !beyond.is_empty() {
        label.push_str(", ");
        label.push_str(&joined(beyond));
    }
    label.push_str(&open_note);
    History {
        history: format!("prefix:{prefix}"),
        missing_ordinals: Vec::new(),
        label,
    }
}

/// Active, execution and elapsed time of these executions, in microseconds: the union of the
/// intervals (an overlap counted once), their sum, and first start to last end.
pub fn times(executions: &[Execution]) -> (u64, u64, u64) {
    let mut intervals: Vec<(i64, i64)> = executions.iter().map(Execution::interval).collect();
    intervals.sort();
    let sum: i64 = intervals.iter().map(|(start, end)| end - start).sum();
    let mut active = 0_i64;
    let mut reach = i64::MIN;
    for (start, end) in &intervals {
        let from = (*start).max(reach);
        if *end > from {
            active += end - from;
        }
        reach = reach.max(*end);
    }
    let elapsed = match (
        intervals.first(),
        intervals.iter().map(|(_, end)| *end).max(),
    ) {
        (Some((first, _)), Some(last)) => last - first,
        _ => 0,
    };
    (micros(active), micros(sum), micros(elapsed))
}

/// The last record time of a segment, from the index.
fn last_record_time(index: &Index, segment_id: &str) -> Result<Option<i64>, String> {
    index
        .connection()
        .query_row(
            "SELECT max(t) FROM (
                 SELECT max(start_unix_ns) AS t FROM spans WHERE segment_id = ?1
                 UNION ALL SELECT max(end_unix_ns) FROM spans WHERE segment_id = ?1
                 UNION ALL SELECT max(time_unix_ns) FROM events WHERE segment_id = ?1
                 UNION ALL SELECT max(time_unix_ns) FROM problems WHERE segment_id = ?1
                 UNION ALL SELECT ended_unix_ns FROM segments WHERE segment_id = ?1
                 UNION ALL SELECT started_unix_ns FROM segments WHERE segment_id = ?1)",
            params![segment_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(sql)
}

/// The segment states a listing gives, by segment id.
fn segment_states(index: &Index) -> BTreeMap<String, String> {
    let mut states = BTreeMap::new();
    let Some(versioned) = index.versioned() else {
        return states;
    };
    let Ok(Some(segments)) = versioned.child_dir("segments") else {
        return states;
    };
    if let Ok(listing) = reader::list_in(&segments) {
        for id in listing.live {
            states.insert(id, "live".to_string());
        }
        for id in listing.unsealed {
            states.insert(id, "unsealed".to_string());
        }
        for id in listing.sealed {
            states.insert(id, "sealed".to_string());
        }
    }
    states
}

/// One `af.run` span of an execution, as the index holds it.
struct ExecutionRow {
    execution_id: String,
    span_id: String,
    ordinal: Option<i64>,
    sequence: Option<String>,
    segment_id: String,
    start: i64,
    end: Option<i64>,
    outcome: Option<String>,
}

/// Every retained execution of the trace `trace_id`, oldest first.
pub fn executions(index: &Index, trace_id: &str) -> Result<Vec<Execution>, String> {
    executions_in(index, trace_id, &segment_states(index))
}

/// [`executions`] with the segments' states already listed.
fn executions_in(
    index: &Index,
    trace_id: &str,
    states: &BTreeMap<String, String>,
) -> Result<Vec<Execution>, String> {
    let rows: Vec<ExecutionRow> = {
        let mut statement = index
            .connection()
            .prepare(
                "SELECT e.execution_id, e.span_id, e.ordinal, e.store_sequence_at_start,
                        s.segment_id, s.start_unix_ns, s.end_unix_ns,
                        json_extract(s.end_attributes, '$.\"af.outcome\"')
                 FROM executions e JOIN spans s ON s.span_id = e.span_id
                 WHERE e.trace_id = ?1 ORDER BY s.start_unix_ns, e.span_id",
            )
            .map_err(sql)?;
        statement
            .query_map(params![trace_id], |row| {
                Ok(ExecutionRow {
                    execution_id: row.get(0)?,
                    span_id: row.get(1)?,
                    ordinal: row.get(2)?,
                    sequence: row.get(3)?,
                    segment_id: row.get(4)?,
                    start: row.get(5)?,
                    end: row.get(6)?,
                    outcome: row.get(7)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    let mut out = Vec::new();
    for ExecutionRow {
        execution_id,
        span_id,
        ordinal,
        sequence,
        segment_id,
        start,
        end,
        outcome,
    } in rows
    {
        let (end, open) = match end {
            Some(end) => (end, false),
            None => (last_record_time(index, &segment_id)?.unwrap_or(start), true),
        };
        let end = end.max(start);
        out.push(Execution {
            execution_id,
            span_id,
            ordinal: ordinal.and_then(|ordinal| u64::try_from(ordinal).ok()),
            store_sequence_at_start: sequence,
            start_unix_ns: ns_text(start),
            end_unix_ns: ns_text(end),
            open,
            wall_us: micros(end - start),
            segment: states
                .get(&segment_id)
                .cloned()
                .unwrap_or_else(|| "sealed".into()),
            outcome,
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// runs

/// The exact usage counters a run's Attempts carried at their span ends, summed exactly; a
/// counter some Attempt with a charge did not deliver is absent, never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
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
}

/// One run in `af/telemetry-runs@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub store_id: String,
    pub run_id: String,
    pub trace_id: String,
    /// `task`, `review`, `optimize` or `measure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    /// Its retained executions: the sealed ones' ordinals, the open ones beside them, and
    /// every one in full.
    pub executions: Executions,
    pub first_start_unix_ns: String,
    pub last_end_unix_ns: String,
    /// Union of the executions' intervals.
    pub active_us: u64,
    /// `af.execution.duration_sum`: the sum of the same intervals.
    pub execution_us: u64,
    /// First start to last end.
    pub elapsed_us: u64,
    /// Some retained execution is open: its interval runs to its segment's last record.
    pub open: bool,
    /// The latest execution's `af.outcome`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The Attempts' exact usage counters, summed.
    pub tokens: Tokens,
    /// The Attempts' charges (`af.usage.chargeable_tokens`), summed exactly; absent when no
    /// Attempt recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge: Option<String>,
    /// Attempts whose span ended with a charge.
    pub attempts: u64,
    /// Spans that ended with status `error`.
    pub failures: u64,
    /// `complete` when every retained execution's segment was sealed by its writer with
    /// nothing dropped; `incomplete` otherwise (live, unsealed, recovered, or records dropped).
    pub coverage: String,
    /// `prefix:k` or `partial`.
    pub history: String,
    pub missing_ordinals: Vec<u64>,
    /// `retained executions 1–k` (with `, open: m…` when some are open), or `partial (…)`:
    /// what every total here covers.
    pub retained: String,
    /// The executions every total here — active, execution and elapsed time, tokens, charge,
    /// Attempts and failures — covers: each retained one, `open` when it is.
    pub totals_cover: Vec<Covered>,
}

/// `af/telemetry-runs@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runs {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    /// `--since`, as the oldest last activity listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_unix_ns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub runs: Vec<Run>,
    pub unreadable: Vec<Unreadable>,
}

/// The retained runs of the index, newest last activity first.
pub fn runs(
    index: &Index,
    directory: &Path,
    read: IndexRead,
    unreadable: Vec<Unreadable>,
    since_unix_ns: Option<i64>,
    kind: Option<&str>,
) -> Result<Runs, String> {
    let traces: Vec<(String, String, String, Option<String>)> = {
        let mut statement = index
            .connection()
            .prepare("SELECT trace_id, store_id, run_id, kind FROM runs")
            .map_err(sql)?;
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    let states = segment_states(index);
    let mut out = Vec::new();
    for (trace_id, store_id, run_id, run_kind) in traces {
        if kind.is_some_and(|kind| run_kind.as_deref() != Some(kind)) {
            continue;
        }
        let run = run(index, &states, &trace_id, store_id, run_id, run_kind)?;
        if let Some(since) = since_unix_ns
            && run.last_end_unix_ns.parse::<i64>().unwrap_or(0) < since
        {
            continue;
        }
        out.push(run);
    }
    out.sort_by(|a, b| {
        b.last_end_unix_ns
            .parse::<i64>()
            .unwrap_or(0)
            .cmp(&a.last_end_unix_ns.parse::<i64>().unwrap_or(0))
            .then_with(|| a.run_id.cmp(&b.run_id))
            .then_with(|| a.store_id.cmp(&b.store_id))
    });
    Ok(Runs {
        schema: RUNS_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read,
        since_unix_ns: since_unix_ns.map(ns_text),
        kind: kind.map(str::to_string),
        runs: out,
        unreadable,
    })
}

/// A text attribute of the latest `af.run` start of the trace that carries it.
fn run_attribute(index: &Index, trace_id: &str, key: &str) -> Result<Option<String>, String> {
    index
        .connection()
        .query_row(
            &format!(
                "SELECT CAST(json_extract(attributes, '$.\"{key}\"') AS TEXT) FROM spans
                 WHERE trace_id = ?1 AND name = 'af.run'
                   AND json_extract(attributes, '$.\"{key}\"') IS NOT NULL
                 ORDER BY start_unix_ns DESC LIMIT 1"
            ),
            params![trace_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(sql)
        .map(Option::flatten)
}

/// The latest of `executions`: the one with the greatest committed ordinal (the last listed of
/// equals); `None` when none carries an ordinal.
fn latest(executions: &[Execution]) -> Option<&Execution> {
    executions
        .iter()
        .filter(|execution| execution.ordinal.is_some())
        .max_by_key(|execution| execution.ordinal)
}

fn run(
    index: &Index,
    states: &BTreeMap<String, String>,
    trace_id: &str,
    store_id: String,
    run_id: String,
    kind: Option<String>,
) -> Result<Run, String> {
    let executions = executions_in(index, trace_id, states)?;
    let (active_us, execution_us, elapsed_us) = times(&executions);
    let executions = Executions::of(executions);
    let retained = executions.history();
    let first_start = executions
        .list
        .iter()
        .map(|execution| execution.interval().0)
        .min()
        .unwrap_or(0);
    let last_end = executions
        .list
        .iter()
        .map(|execution| execution.interval().1)
        .max()
        .unwrap_or(0);
    // The latest execution's own outcome and nothing else: the latest is the one with the
    // greatest committed ordinal, never the latest start (the clocks of two machines, or a
    // takeover, can run them opposite); one without an ordinal is listed but never latest. A
    // live latest execution whose `af.run` has no end attributes yet has none, never an earlier
    // execution's, and with no ordinal at all there is no outcome.
    let outcome = latest(&executions.list).and_then(|execution| execution.outcome.clone());
    let failures: i64 = index
        .connection()
        .query_row(
            "SELECT count(*) FROM spans WHERE trace_id = ?1 AND status = 'error'",
            params![trace_id],
            |row| row.get(0),
        )
        .map_err(sql)?;
    let failures = u64::try_from(failures).unwrap_or(0);
    let (tokens, charge, attempts) = usage(index, trace_id)?;
    let coverage = coverage(index, &executions.list)?;
    let totals_cover = executions
        .list
        .iter()
        .map(|execution| Covered {
            execution_id: execution.execution_id.clone(),
            ordinal: execution.ordinal,
            open: execution.is_open(),
        })
        .collect();
    Ok(Run {
        task_id: run_attribute(index, trace_id, "af.task_id")?,
        campaign: run_attribute(index, trace_id, "af.campaign")?,
        store_id,
        run_id,
        trace_id: trace_id.to_string(),
        kind,
        first_start_unix_ns: ns_text(first_start),
        last_end_unix_ns: ns_text(last_end),
        active_us,
        execution_us,
        elapsed_us,
        open: !executions.open.is_empty(),
        outcome,
        tokens,
        charge,
        attempts,
        failures,
        coverage,
        history: retained.history,
        missing_ordinals: retained.missing_ordinals,
        retained: retained.label,
        executions,
        totals_cover,
    })
}

/// Whether every retained execution's segment was sealed by its writer with nothing dropped.
fn coverage(index: &Index, executions: &[Execution]) -> Result<String, String> {
    for execution in executions {
        if execution.segment != "sealed" {
            return Ok("incomplete".into());
        }
        let complete: Option<(Option<String>, Option<i64>, Option<i64>)> = index
            .connection()
            .query_row(
                "SELECT exit, dropped, discarded_bytes FROM segments WHERE segment_id = ?1",
                params![execution.execution_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        match complete {
            Some((Some(exit), Some(0), Some(0))) if exit != "recovered" => {}
            _ => return Ok("incomplete".into()),
        }
    }
    Ok("complete".into())
}

/// The run's Attempts' exact counters and charges as their spans ended, each Attempt once (its
/// latest span end).
fn usage(index: &Index, trace_id: &str) -> Result<(Tokens, Option<String>, u64), String> {
    let rows: Vec<(Option<String>, String)> = {
        let mut statement = index
            .connection()
            .prepare(
                "SELECT json_extract(attributes, '$.\"af.attempt_id\"'), end_attributes
                 FROM spans WHERE trace_id = ?1 AND name = 'af.attempt' AND ended = 1
                   AND end_attributes IS NOT NULL
                 ORDER BY end_unix_ns, span_id",
            )
            .map_err(sql)?;
        statement
            .query_map(params![trace_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    let mut latest: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (index_key, (attempt, attributes)) in rows.into_iter().enumerate() {
        let Ok(Value::Object(attributes)) = serde_json::from_str::<Value>(&attributes) else {
            continue;
        };
        latest.insert(
            attempt.unwrap_or_else(|| format!("#{index_key}")),
            attributes,
        );
    }
    let charged: Vec<&Map<String, Value>> = latest
        .values()
        .filter(|attributes| attributes.contains_key("af.usage.chargeable_tokens"))
        .collect();
    let sum = |key: &str| -> Option<String> {
        if charged.is_empty() {
            return None;
        }
        let values: Option<Vec<&str>> = charged
            .iter()
            .map(|attributes| attributes.get(key).and_then(Value::as_str))
            .collect();
        crate::index::sum_decimals(values?)
    };
    let tokens = Tokens {
        input_tokens: sum("af.usage.input_tokens"),
        output_tokens: sum("af.usage.output_tokens"),
        cache_read_tokens: sum("af.usage.cache_read_tokens"),
        cache_write_tokens: sum("af.usage.cache_write_tokens"),
        reasoning_tokens: sum("af.usage.reasoning_tokens"),
    };
    Ok((
        tokens,
        sum("af.usage.chargeable_tokens"),
        charged.len() as u64,
    ))
}

// ------------------------------------------------------------------------------------------
// resolving a RUN argument

/// What a `RUN` argument names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Run {
        store_id: String,
        trace_id: String,
        kind: Option<String>,
    },
    Unknown,
    /// Two Stores share the run id: each store id, for `--store ID`.
    Ambiguous(Vec<String>),
}

/// Resolve a kernel run id, within `store` when given.
pub fn resolve(index: &Index, run_id: &str, store: Option<&str>) -> Result<Resolved, String> {
    let candidates: Vec<(String, String, Option<String>)> = {
        let mut statement = index
            .connection()
            .prepare(
                "SELECT store_id, trace_id, kind FROM runs WHERE run_id = ?1 ORDER BY store_id",
            )
            .map_err(sql)?;
        statement
            .query_map(params![run_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(store_id, _, _)| store.is_none_or(|store| store_id == store))
        .collect();
    Ok(match candidates.as_slice() {
        [] => Resolved::Unknown,
        [(store_id, trace_id, kind)] => Resolved::Run {
            store_id: store_id.clone(),
            trace_id: trace_id.clone(),
            kind: kind.clone(),
        },
        several => Resolved::Ambiguous(several.iter().map(|(store, _, _)| store.clone()).collect()),
    })
}

// ------------------------------------------------------------------------------------------
// show

/// One span of the tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShownSpan {
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub name: String,
    pub depth: u64,
    pub segment_id: String,
    /// From the earliest span shown.
    pub start_offset_us: u64,
    /// `None` while the span has no recorded end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_us: Option<u64>,
    /// Total less the union of its ended children, clipped to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_us: Option<u64>,
    /// `ok`, `error`, `unset`, or `open` when it has no recorded end.
    pub status: String,
    /// [`KEY_ATTRIBUTES`] from the start and the end, the end's winning; with `--span`, every
    /// attribute.
    pub attributes: Map<String, Value>,
}

/// One event of the span `--span` names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShownEvent {
    pub seq: u64,
    pub name: String,
    pub time_unix_ns: String,
    pub offset_us: u64,
    pub attributes: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// The run `show` describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShownRun {
    pub store_id: String,
    pub run_id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub history: String,
    pub missing_ordinals: Vec<u64>,
    /// The label of [`History`]: `retained executions 1–k`, naming the open executions when
    /// some are, or `partial (…)`.
    pub retained: String,
    /// Its retained executions, the open ones listed beside the sealed ones.
    pub executions: Executions,
    pub first_start_unix_ns: String,
}

/// `af/telemetry-show@1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Show {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: ShownRun,
    /// The tree in depth-first order, children by start time.
    pub spans: Vec<ShownSpan>,
    /// With `--span`: that span's events in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<ShownEvent>>,
    pub unreadable: Vec<Unreadable>,
}

/// What `show` selects.
#[derive(Debug, Clone, Default)]
pub struct ShowSelection {
    /// Only the subtree of the `af.attempt` span with this `af.attempt_id`, with its ancestors.
    pub attempt: Option<String>,
    /// Only this span, with every attribute and its events.
    pub span: Option<String>,
    /// Spans at most this deep (the root is 0).
    pub depth: Option<u64>,
}

struct Row {
    span_id: String,
    parent: Option<String>,
    segment_id: String,
    name: String,
    start: i64,
    end: Option<i64>,
    ended: bool,
    status: Option<String>,
    attributes: Map<String, Value>,
    end_attributes: Map<String, Value>,
    self_ns: Option<i64>,
}

fn object(text: Option<String>) -> Map<String, Value> {
    match text.and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn span_rows(index: &Index, query: &str, key: &str) -> Result<Vec<Row>, String> {
    let mut statement = index.connection().prepare(query).map_err(sql)?;
    statement
        .query_map(params![key], |row| {
            Ok(Row {
                span_id: row.get(0)?,
                parent: row.get(1)?,
                segment_id: row.get(3)?,
                name: row.get(4)?,
                start: row.get(5)?,
                end: row.get(6)?,
                ended: row.get::<_, i64>(7)? != 0,
                status: row.get(8)?,
                attributes: object(row.get(9)?),
                end_attributes: object(row.get(10)?),
                self_ns: row.get(11)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

const SPAN_COLUMNS: &str = "span_id, parent_span_id, trace_id, segment_id, name, start_unix_ns,
     end_unix_ns, ended, status, attributes, end_attributes, self_ns";

/// `af telemetry show`: the span tree of the run `trace_id` resolved to, from each execution's
/// `af.command` down.
pub fn show(
    index: &Index,
    directory: &Path,
    read: IndexRead,
    unreadable: Vec<Unreadable>,
    run: (&str, &str, &str, Option<String>),
    selection: &ShowSelection,
) -> Result<Show, String> {
    let (store_id, run_id, trace_id, kind) = run;
    let executions = Executions::of(executions(index, trace_id)?);
    let retained = executions.history();
    let mut rows: BTreeMap<String, Row> = span_rows(
        index,
        &format!("SELECT {SPAN_COLUMNS} FROM spans WHERE trace_id = ?1"),
        trace_id,
    )?
    .into_iter()
    .map(|row| (row.span_id.clone(), row))
    .collect();
    // Each execution's `af.command` and its own spans outside any trace (`af.sweep`).
    let roots: BTreeSet<String> = rows
        .values()
        .filter(|row| row.name == "af.run")
        .filter_map(|row| row.parent.clone())
        .collect();
    for root in &roots {
        for row in span_rows(
            index,
            &format!("SELECT {SPAN_COLUMNS} FROM spans WHERE span_id = ?1"),
            root,
        )?
        .into_iter()
        .chain(span_rows(
            index,
            &format!(
                "SELECT {SPAN_COLUMNS} FROM spans WHERE parent_span_id = ?1 AND trace_id IS NULL"
            ),
            root,
        )?) {
            rows.entry(row.span_id.clone()).or_insert(row);
        }
    }
    let first = rows.values().map(|row| row.start).min().unwrap_or(0);
    let mut children: BTreeMap<Option<String>, Vec<(i64, String)>> = BTreeMap::new();
    for row in rows.values() {
        let parent = row
            .parent
            .clone()
            .filter(|parent| rows.contains_key(parent));
        children
            .entry(parent)
            .or_default()
            .push((row.start, row.span_id.clone()));
    }
    for list in children.values_mut() {
        list.sort();
    }
    let mut order: Vec<(String, u64)> = Vec::new();
    let mut stack: Vec<(String, u64)> = children
        .get(&None)
        .map(|roots| roots.iter().rev().map(|(_, id)| (id.clone(), 0)).collect())
        .unwrap_or_default();
    while let Some((id, depth)) = stack.pop() {
        order.push((id.clone(), depth));
        if let Some(list) = children.get(&Some(id)) {
            for (_, child) in list.iter().rev() {
                stack.push((child.clone(), depth + 1));
            }
        }
    }
    // `--attempt`: the Attempt's subtree with its ancestors.
    if let Some(attempt) = &selection.attempt {
        let target = rows
            .values()
            .filter(|row| {
                row.name == "af.attempt"
                    && row.attributes.get("af.attempt_id").and_then(Value::as_str)
                        == Some(attempt.as_str())
            })
            .map(|row| row.span_id.clone())
            .next()
            .ok_or_else(|| format!("no Attempt {attempt} in run {run_id}"))?;
        let mut keep: BTreeSet<String> = BTreeSet::new();
        let mut cursor = Some(target.clone());
        while let Some(id) = cursor {
            cursor = rows.get(&id).and_then(|row| row.parent.clone());
            keep.insert(id);
        }
        let mut pending = vec![target];
        while let Some(id) = pending.pop() {
            for (_, child) in children.get(&Some(id)).into_iter().flatten() {
                keep.insert(child.clone());
                pending.push(child.clone());
            }
        }
        order.retain(|(id, _)| keep.contains(id));
    }
    if let Some(depth) = selection.depth {
        order.retain(|(_, at)| *at <= depth);
    }
    let mut events = None;
    if let Some(span) = &selection.span {
        if !rows.contains_key(span) {
            return Err(format!("no span {span} in run {run_id}"));
        }
        order.retain(|(id, _)| id == span);
        let mut statement = index
            .connection()
            .prepare(
                "SELECT seq, name, time_unix_ns, attributes, blob FROM events
                 WHERE span_id = ?1 ORDER BY time_unix_ns, seq",
            )
            .map_err(sql)?;
        let listed: Vec<ShownEvent> = statement
            .query_map(params![span], |row| {
                let time: i64 = row.get(2)?;
                Ok(ShownEvent {
                    seq: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                    name: row.get(1)?,
                    time_unix_ns: ns_text(time),
                    offset_us: micros(time - first),
                    attributes: object(row.get(3)?),
                    blob: row.get(4)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        events = Some(listed);
    }
    let every = selection.span.is_some();
    let spans = order
        .into_iter()
        .map(|(id, depth)| {
            let row = &rows[&id];
            let mut attributes = Map::new();
            for (key, value) in row.attributes.iter().chain(row.end_attributes.iter()) {
                if every || KEY_ATTRIBUTES.contains(&key.as_str()) {
                    attributes.insert(key.clone(), value.clone());
                }
            }
            let total = row.end.map(|end| micros(end - row.start));
            ShownSpan {
                span_id: row.span_id.clone(),
                parent_span_id: row.parent.clone(),
                name: row.name.clone(),
                depth,
                segment_id: row.segment_id.clone(),
                start_offset_us: micros(row.start - first),
                total_us: total,
                self_us: row.self_ns.filter(|_| total.is_some()).map(micros),
                status: match (&row.status, row.end) {
                    (Some(status), Some(_)) if row.ended => status.clone(),
                    _ => "open".into(),
                },
                attributes,
            }
        })
        .collect();
    Ok(Show {
        schema: SHOW_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read,
        run: ShownRun {
            store_id: store_id.to_string(),
            run_id: run_id.to_string(),
            trace_id: trace_id.to_string(),
            kind,
            history: retained.history,
            missing_ordinals: retained.missing_ordinals,
            retained: retained.label,
            executions,
            first_start_unix_ns: ns_text(first),
        },
        spans,
        events,
        unreadable,
    })
}

// ------------------------------------------------------------------------------------------
// tail

/// One line of `af telemetry tail`: a span end or an event, as it is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailLine {
    pub schema: String,
    pub segment_id: String,
    pub seq: u64,
    /// `span_end` or `event`.
    pub record: String,
    pub span_id: String,
    /// The span's name; for an event, the span it is on.
    pub span: String,
    /// The event's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub time_unix_ns: String,
    /// A span end's duration, when the span started in the lines followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub attributes: Map<String, Value>,
}

/// What `tail` follows.
#[derive(Debug, Clone, Default)]
pub struct TailSelection {
    /// Only this run's records (its `af.run_id`, in the one Store `store_id` binds).
    pub run_id: Option<String>,
    /// The Store the run is followed in for the whole tail: the one `--store` names, or the one
    /// the index knew the run in when following began. `None` binds the Store of the first
    /// `af.run` of the run that arrives.
    pub store_id: Option<String>,
    /// `store_id` was named with `--store`: another Store's run of this id is passed over.
    /// Otherwise a second Store recording the run ends the tail as [`Tailed::Ambiguous`] before
    /// any of its records is emitted.
    pub store_named: bool,
    /// The sealed segments listed before the index lookup that resolved the run
    /// ([`tail_start`]): the index read every one of them. With a run selected, every other
    /// sealed segment is inspected for it, those of the first listing included. `None` takes
    /// the first listing's sealed segments as the baseline.
    pub sealed_baseline: Option<BTreeSet<String>>,
    /// The run had no live or unsealed execution when it was resolved: every segment holding
    /// one of its executions, each in [`Self::sealed_baseline`]. Nobody is writing it, so
    /// [`tail`] reads these sealed files on its first look, prints their records as the follow
    /// would have, and finishes then, unless a live segment carries the run by that look.
    pub sealed_run: Option<BTreeSet<String>>,
}

/// How `tail` starts: the selection to follow, or why there is nothing to follow.
#[derive(Debug, Clone)]
pub enum TailStart {
    Follow(TailSelection),
    /// No Store records the run (in the Store named, when one is): nothing is followed.
    Unknown,
    /// Two Stores record the run and none is named: both store ids, for `--store ID`.
    Ambiguous(Vec<String>),
}

/// Resolve what `tail` follows before it follows anything (design §10). Without a run, every
/// live segment. With one, the sealed segments are listed first (the baseline carried into
/// [`tail`]), then the index ingests every new segment byte and resolves the run, within
/// `store` when given: an unknown run or one two Stores share without `store` starts no follow
/// loop. A segment sealed after the baseline was listed is not in it, so `tail` inspects it.
/// A run that cannot be resolved while a segment is unreadable (interior corruption, a sealed
/// file that cannot be read) is never called unknown: the rows it may hold were dropped, so
/// `tail` fails closed naming that segment and its reason.
pub fn tail_start(
    root: &Path,
    run_id: Option<&str>,
    store: Option<&str>,
) -> Result<TailStart, String> {
    let Some(run_id) = run_id else {
        return Ok(TailStart::Follow(TailSelection {
            run_id: None,
            store_id: store.map(str::to_string),
            store_named: store.is_some(),
            sealed_baseline: None,
            sealed_run: None,
        }));
    };
    if !root.is_dir() {
        return Ok(TailStart::Unknown);
    }
    let baseline: BTreeSet<String> = reader::list(&Layout::new(root))?
        .sealed
        .into_iter()
        .collect();
    let (index, _, unreadable) = open(root)?;
    // Index ingestion forgets every `.corrupt` segment's rows, so the index alone cannot tell
    // whether one held the run: the listing names them, taken after the ingest.
    let layout = Layout::new(root);
    let segments = Confined::versioned(&layout, "segments")
        .map_err(|error| format!("{}: {error}", layout.segments().display()))?;
    let corrupt = match &segments {
        Some(segments) => reader::list_in(segments)?.corrupt,
        None => Vec::new(),
    };
    Ok(match resolve(&index, run_id, store)? {
        Resolved::Run {
            store_id, trace_id, ..
        } => {
            // A `.corrupt` segment that could hold an execution of the run fails the tail: it
            // would finish over the readable executions while omitting that one.
            if let Some(segments) = &segments {
                for id in &corrupt {
                    if let Some(why) = corrupt_could_hold(segments, id, run_id, store)? {
                        return Err(format!(
                            "{}: an unreadable segment (interior corruption; {why}); never \
                             read, so run {run_id} cannot be followed whole",
                            segments.join(&format!("{id}{}", layout::CORRUPT)).display()
                        ));
                    }
                }
            }
            // Every execution in a segment sealed before the lookup: nobody writes the run.
            let executed = execution_segments(&index, &trace_id)?;
            let sealed_run =
                (!executed.is_empty() && executed.is_subset(&baseline)).then_some(executed);
            TailStart::Follow(TailSelection {
                run_id: Some(run_id.to_string()),
                store_id: Some(store_id),
                store_named: store.is_some(),
                sealed_baseline: Some(baseline),
                sealed_run,
            })
        }
        Resolved::Unknown => {
            // Only a segment can hold the run's rows; another version's directory never could.
            let segment = unreadable.iter().find(|unreadable| {
                Path::new(&unreadable.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(layout::parse_segment_name)
                    .is_some()
            });
            if let Some(segment) = segment {
                return Err(format!(
                    "{}: an unreadable segment ({}); never read, so run {run_id} cannot be \
                     resolved",
                    segment.path, segment.reason
                ));
            }
            // A `.corrupt` segment may hold the run's only execution: never called unknown.
            if let (Some(segments), Some(id)) = (&segments, corrupt.first()) {
                return Err(format!(
                    "{}: an unreadable segment (interior corruption; renamed {id}{}); never \
                     read, so run {run_id} cannot be resolved",
                    segments.join(&format!("{id}{}", layout::CORRUPT)).display(),
                    layout::CORRUPT
                ));
            }
            TailStart::Unknown
        }
        Resolved::Ambiguous(stores) => TailStart::Ambiguous(stores),
    })
}

/// Why the `.corrupt` segment `segment_id` could hold an execution of `run_id` (within `store`
/// when one is named), or `None` when it cannot. Its records are never taken as telemetry; each
/// line is judged alone, whatever the corruption around it: a valid `af.run` start of the run
/// holds one, and so does a line that is no record but names the run id, since it may be that
/// `af.run` damaged. Without `store` an `af.run` of the run in any Store counts: it could make
/// the run ambiguous. A file that cannot be read (past the segment bound included) could hold
/// one; a file gone since the listing holds nothing any more.
fn corrupt_could_hold(
    segments: &Confined,
    segment_id: &str,
    run_id: &str,
    store: Option<&str>,
) -> Result<Option<String>, String> {
    let bytes = match reader::read_corrupt_in(segments, segment_id) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Ok(Some(format!("it cannot be judged: {error}"))),
    };
    let needle = run_id.as_bytes();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        match Record::parse_line(line) {
            Ok(Record::SpanStart(start)) if start.name == "af.run" => {
                let text = |key: &str| start.attributes.get(key).and_then(Value::as_str);
                if text("af.run_id") == Some(run_id)
                    && store.is_none_or(|store| text("af.store_id") == Some(store))
                {
                    return Ok(Some(format!("it holds an `af.run` of run {run_id}")));
                }
            }
            Ok(_) => {}
            Err(_) => {
                if !needle.is_empty() && line.windows(needle.len()).any(|window| window == needle) {
                    return Ok(Some(format!("an invalid line in it names run {run_id}")));
                }
            }
        }
    }
    Ok(None)
}

/// The segments holding the `af.run` spans of the trace `trace_id`'s executions.
fn execution_segments(index: &Index, trace_id: &str) -> Result<BTreeSet<String>, String> {
    let mut statement = index
        .connection()
        .prepare(
            "SELECT DISTINCT s.segment_id FROM executions e JOIN spans s ON s.span_id = e.span_id
             WHERE e.trace_id = ?1",
        )
        .map_err(sql)?;
    statement
        .query_map(params![trace_id], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

/// How `tail` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tailed {
    /// Every followed segment that carried the selection was sealed, or `stop` said so.
    Done,
    /// A second Store recorded the selected run id while it was followed, without `--store`:
    /// both Store ids, in order. Nothing of the second Store was emitted.
    Ambiguous(Vec<String>),
}

/// The Store a followed run is bound to, and the second one that made it ambiguous.
struct Binding {
    store_id: Option<String>,
    conflict: Option<String>,
}

/// One followed segment.
#[derive(Default)]
struct Followed {
    offset: usize,
    position: Option<Position>,
    /// Span id → (name, trace, start).
    spans: BTreeMap<String, (String, Option<String>, i64)>,
    /// The traces of the selected run seen in this segment.
    run_traces: BTreeSet<String>,
    carries: bool,
    sealed: bool,
}

impl Followed {
    /// Turn new records into lines.
    fn take(
        &mut self,
        segment_id: &str,
        records: Vec<Record>,
        selection: &TailSelection,
        binding: &mut Binding,
        out: &mut Vec<TailLine>,
    ) {
        for record in records {
            self.position = Some(Position {
                seq: record.seq(),
                sealed: matches!(record, Record::Seal(_)),
            });
            match record {
                Record::SpanStart(start) => {
                    let start_ns = start.start_unix_ns.parse().unwrap_or(0);
                    if start.name == "af.run" {
                        let text = |key: &str| {
                            start
                                .attributes
                                .get(key)
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        };
                        let matches = match &selection.run_id {
                            None => true,
                            Some(run) if text("af.run_id").as_deref() == Some(run.as_str()) => {
                                let store = text("af.store_id").unwrap_or_default();
                                match &binding.store_id {
                                    None => {
                                        binding.store_id = Some(store);
                                        true
                                    }
                                    Some(bound) if *bound == store => true,
                                    Some(_) if selection.store_named => false,
                                    Some(_) => {
                                        // Refused once every followed segment's pending lines of
                                        // the bound Store are drained; its traces are never
                                        // selected, so none of its records is emitted.
                                        binding.conflict.get_or_insert(store);
                                        false
                                    }
                                }
                            }
                            Some(_) => false,
                        };
                        if matches && let Some(trace) = &start.trace_id {
                            self.run_traces.insert(trace.clone());
                        }
                    }
                    self.spans
                        .insert(start.span_id, (start.name, start.trace_id, start_ns));
                }
                Record::SpanEnd(end) => {
                    let (name, trace, start) = self
                        .spans
                        .get(&end.span_id)
                        .cloned()
                        .unwrap_or_else(|| ("".into(), None, 0));
                    if !self.selected(&trace, selection) {
                        continue;
                    }
                    self.carries = true;
                    let time = end.end_unix_ns.clone().unwrap_or_default();
                    let at: i64 = time.parse().unwrap_or(0);
                    out.push(TailLine {
                        schema: TAIL_SCHEMA.into(),
                        segment_id: segment_id.to_string(),
                        seq: end.seq,
                        record: "span_end".into(),
                        span_id: end.span_id,
                        span: name,
                        name: None,
                        trace_id: trace,
                        duration_us: (start > 0 && at > 0).then(|| micros(at - start)),
                        time_unix_ns: time,
                        status: Some(
                            match end.status {
                                SpanStatus::Ok => "ok",
                                SpanStatus::Error => "error",
                                SpanStatus::Unset => "unset",
                            }
                            .into(),
                        ),
                        attributes: end.attributes,
                    });
                }
                Record::Event(event) => {
                    let (span, trace, _) = self
                        .spans
                        .get(&event.span_id)
                        .cloned()
                        .unwrap_or_else(|| ("".into(), None, 0));
                    if !self.selected(&trace, selection) {
                        continue;
                    }
                    self.carries = true;
                    out.push(TailLine {
                        schema: TAIL_SCHEMA.into(),
                        segment_id: segment_id.to_string(),
                        seq: event.seq,
                        record: "event".into(),
                        span_id: event.span_id,
                        span,
                        name: Some(event.name),
                        trace_id: trace,
                        time_unix_ns: event.time_unix_ns,
                        duration_us: None,
                        status: None,
                        attributes: event.attributes,
                    });
                }
                // A seal read from the live `.jsonl` completes nothing: the segment is sealed
                // once its published file validates, or once it is recovered.
                _ => {}
            }
        }
    }

    fn selected(&self, trace: &Option<String>, selection: &TailSelection) -> bool {
        match &selection.run_id {
            None => true,
            Some(_) => trace
                .as_ref()
                .is_some_and(|trace| self.run_traces.contains(trace)),
        }
    }
}

/// `af telemetry tail`: follows the live segments of `root` (every one, or those that carry the
/// selected run) and hands every span end and event to `emit` as it is written, from the start
/// of each segment; returns once at least one followed segment carried what was selected and
/// every such segment is sealed: its published sealed file read and valid, or it was
/// recovered. A seal record read from a live `.jsonl` completes nothing while the writer still
/// compresses or publishes. A run whose every execution was sealed before it was resolved
/// ([`TailSelection::sealed_run`]) finishes after the first look, its sealed records printed.
/// An unlocked `.jsonl` (a writer that crashed, or abandoned its publication) is recovered
/// before it is followed, as the reader able to lock it (design §6), and its recovered sealed
/// file read. Without a live segment it waits for one, looking every `interval`. `stop` ends it
/// early (a fixture's timeout). A followed segment whose sealed file cannot be read, or whose
/// live bytes hold interior corruption, fails it, naming the file and emitting nothing of that
/// read (the reader fails closed); so does a followed segment that is gone without a sealed
/// file (another reader's recovery renamed it `.corrupt`, which is named): only a validated
/// sealed file or a recovery this tail completed seals it. The selected run stays in one Store
/// for the whole tail (see [`TailSelection::store_id`]); a second Store recording it ends the
/// tail as [`Tailed::Ambiguous`] once every pending line of the bound Store in every followed
/// segment is emitted, whatever order the segments sort in. With a run selected, every sealed
/// segment outside [`TailSelection::sealed_baseline`] is inspected for it, those of the first
/// listing included (one created and sealed between the run's lookup and the first look, or
/// between two looks): a second Store there is refused the same way.
pub fn tail(
    root: &Path,
    selection: &TailSelection,
    interval: Duration,
    stop: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(&TailLine) -> std::io::Result<()>,
) -> Result<Tailed, String> {
    let mut followed: BTreeMap<String, Followed> = BTreeMap::new();
    let mut binding = Binding {
        store_id: selection.store_id.clone(),
        conflict: None,
    };
    let mut root_dir: Option<Confined> = None;
    let mut segments: Option<Confined> = None;
    // The sealed segments the index read before following began (the baseline carried from the
    // run's lookup, else the first listing's); every other sealed segment is new.
    let mut preexisting: Option<BTreeSet<String>> = selection.sealed_baseline.clone();
    // A run nobody writes: its sealed segments, read on the first look.
    let mut sealed_run = selection.sealed_run.clone();
    let mut first = true;
    loop {
        if segments.is_none() {
            if root_dir.is_none() {
                root_dir = Confined::open_root(root)
                    .map_err(|error| format!("{}: {error}", root.display()))?;
            }
            if let Some(root_dir) = &root_dir {
                // Absent until the first writer creates it; a directory that cannot be opened is
                // an error, never waited on.
                let failed = |path: PathBuf| {
                    move |error: std::io::Error| format!("{}: {error}", path.display())
                };
                segments = match root_dir
                    .child_dir(layout::VERSION_DIR)
                    .map_err(failed(root_dir.join(layout::VERSION_DIR)))?
                {
                    Some(versioned) => versioned
                        .child_dir("segments")
                        .map_err(failed(versioned.join("segments")))?,
                    None => None,
                };
            }
        }
        if let Some(directory) = &segments {
            let listing = reader::list_in(directory)?;
            for id in listing.live.iter().chain(&listing.unsealed) {
                followed.entry(id.clone()).or_default();
            }
            for id in sealed_run.take().into_iter().flatten() {
                followed.entry(id).or_default();
            }
            let preexisting = preexisting.get_or_insert_with(|| {
                if first {
                    listing.sealed.iter().cloned().collect()
                } else {
                    BTreeSet::new()
                }
            });
            if selection.run_id.is_some() {
                for id in listing
                    .sealed
                    .iter()
                    .filter(|id| !preexisting.contains(*id))
                {
                    followed.entry(id.clone()).or_default();
                }
            }
            let mut lines = Vec::new();
            for (id, state) in followed.iter_mut().filter(|(_, state)| !state.sealed) {
                let live_path = || directory.join(&format!("{id}{}", layout::LIVE));
                let mut published = listing.sealed.contains(id);
                if listing.unsealed.contains(id) {
                    // Nobody holds it: its writer crashed or abandoned its publication, and its
                    // prefix never changes again. Recovered here, then read sealed.
                    match reader::recover_in(directory, id)? {
                        // Locked since the listing (by a reader recovering it): followed live.
                        // Gone since: what replaced it is found by listing again below.
                        reader::Recovery::Live | reader::Recovery::AlreadySealed => {}
                        reader::Recovery::Recovered { .. } => published = true,
                        reader::Recovery::Corrupt { .. } => {
                            return Err(format!(
                                "{}: an unreadable segment (interior corruption); renamed {id}{}, \
                                 never read",
                                live_path().display(),
                                layout::CORRUPT
                            ));
                        }
                        reader::Recovery::Empty => {
                            state.sealed = true;
                            continue;
                        }
                    }
                }
                if !published && (listing.live.contains(id) || listing.unsealed.contains(id)) {
                    if state.position.is_some_and(|position| position.sealed) {
                        // The writer's seal is read; it is still compressing or publishing.
                        // Nothing may follow a seal, so the live bytes are not read again.
                        continue;
                    }
                    match reader::read_live_in(directory, id) {
                        Ok(bytes) => {
                            let offset = state.offset.min(bytes.len());
                            let prefix = reader::scan_from(&bytes, offset, state.position);
                            if prefix.interior_corruption {
                                // An invalid line before a valid one: nothing of this read is
                                // emitted and the offset is never read again.
                                return Err(format!(
                                    "{}: an unreadable segment (interior corruption after byte \
                                     {}{}); never read",
                                    live_path().display(),
                                    prefix.kept_bytes,
                                    prefix
                                        .invalid
                                        .as_deref()
                                        .map(|reason| format!(": {reason}"))
                                        .unwrap_or_default()
                                ));
                            }
                            state.offset = prefix.kept_bytes;
                            state.take(id, prefix.records, selection, &mut binding, &mut lines);
                            continue;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("{}: {error}", live_path().display()));
                        }
                    }
                }
                // Sealed (or recovered) since: the rest is in the sealed file. A sealed file that
                // cannot be read is the reader's failure, with its path: the segment is never
                // taken as complete.
                let now = if published {
                    None
                } else {
                    Some(reader::list_in(directory)?)
                };
                if published || now.as_ref().is_some_and(|now| now.sealed.contains(id)) {
                    let (records, _) = reader::read_sealed_in(directory, id)?;
                    let rest: Vec<Record> = records
                        .into_iter()
                        .filter(|record| match state.position {
                            None => true,
                            Some(Position { seq, sealed: false }) => record.seq() > seq,
                            // After the writer's seal: a recovery that superseded it ended the
                            // spans it left open from that `seq` on.
                            Some(Position { seq, sealed: true }) => {
                                record.seq() >= seq && !matches!(record, Record::Seal(_))
                            }
                        })
                        .collect();
                    state.take(id, rest, selection, &mut binding, &mut lines);
                    state.sealed = true;
                } else if let Some(now) = now
                    && !now.live.contains(id)
                    && !now.unsealed.contains(id)
                {
                    // Gone without a sealed file or a recovery of this tail's: never a seal.
                    // Another reader's recovery renamed it `.corrupt`, or it was removed.
                    if now.corrupt.contains(id) {
                        return Err(format!(
                            "{}: an unreadable segment (interior corruption); another reader's \
                             recovery renamed it while it was followed, never read as sealed",
                            directory
                                .join(&format!("{id}{}", layout::CORRUPT))
                                .display()
                        ));
                    }
                    return Err(format!(
                        "{}: gone while followed, without a sealed file; never taken as sealed",
                        live_path().display()
                    ));
                }
            }
            lines.sort_by(|a, b| {
                a.time_unix_ns
                    .parse::<i64>()
                    .unwrap_or(0)
                    .cmp(&b.time_unix_ns.parse::<i64>().unwrap_or(0))
                    .then_with(|| a.segment_id.cmp(&b.segment_id))
                    .then_with(|| a.seq.cmp(&b.seq))
            });
            for line in &lines {
                emit(line).map_err(|error| format!("writing the tail: {error}"))?;
            }
            if let (Some(bound), Some(second)) = (&binding.store_id, &binding.conflict) {
                let mut stores = vec![bound.clone(), second.clone()];
                stores.sort();
                return Ok(Tailed::Ambiguous(stores));
            }
            let carrying: Vec<&Followed> =
                followed.values().filter(|state| state.carries).collect();
            // A run nobody wrote when it was resolved is done once its sealed records are
            // printed, unless a live segment carries it by now.
            let nobody_writes = first && selection.sealed_run.is_some();
            if (nobody_writes || !carrying.is_empty()) && carrying.iter().all(|state| state.sealed)
            {
                return Ok(Tailed::Done);
            }
        } else if first && selection.sealed_run.is_some() {
            // The run's sealed segments went with their directory: never taken as sealed.
            return Err(format!(
                "{}: gone while followed, without the sealed segments of run {}",
                root.join(layout::VERSION_DIR).join("segments").display(),
                selection.run_id.as_deref().unwrap_or_default()
            ));
        }
        if stop() {
            return Ok(Tailed::Done);
        }
        first = false;
        std::thread::sleep(interval);
    }
}

/// The run a `tail RUN` argument names, from the segments' own records when the index does not
/// know it yet: the store ids whose `af.run` spans carry this run id.
pub fn stores_of(index: &Index, run_id: &str) -> Result<Vec<String>, String> {
    let mut statement = index
        .connection()
        .prepare("SELECT DISTINCT store_id FROM runs WHERE run_id = ?1 ORDER BY store_id")
        .map_err(sql)?;
    statement
        .query_map(params![run_id], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

/// A duration such as `14d`, `2h`, `30m` or `45s`.
pub fn parse_since(text: &str) -> Option<Duration> {
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let number: u64 = number.parse().ok()?;
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    Some(Duration::from_secs(number.checked_mul(seconds)?))
}

/// The telemetry root of a `--dir`, absolute.
pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
    }
}

// ------------------------------------------------------------------------------------------
// config

/// One text an execution's configuration names (`af.config.text`): its kind, digest and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigText {
    /// `pipeline`, `task_file` or `af_toml`.
    pub kind: String,
    pub digest: Option<String>,
    pub bytes: Option<u64>,
}

/// `af/TelemetryConfig@1` as `af telemetry config` prints it: an execution's configuration
/// record assembled from its `config` parts, `incomplete` when one is missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRecord {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub store_id: String,
    pub run_id: String,
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    /// `complete` (every part from 1 to the expected count, the last one marked complete),
    /// `incomplete` (a part is missing) or `absent` (the execution recorded none: a level
    /// below `spans`).
    pub state: String,
    /// The parts the record was written as, when any part is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<u32>,
    /// The parts that are not retained.
    pub missing_parts: Vec<u32>,
    /// Every retained row, in the record's order.
    pub rows: Vec<crate::record::ConfigRow>,
    /// The texts the execution named at `content`.
    pub texts: Vec<ConfigText>,
}

/// The configuration record of one execution of the run `trace_id`: `execution`, or the latest
/// (the greatest committed ordinal, else the latest start). `Err` names an unknown execution.
pub fn config(
    index: &Index,
    directory: &Path,
    read: IndexRead,
    run: (&str, &str, &str),
    execution: Option<&str>,
) -> Result<Result<ConfigRecord, String>, String> {
    let (store_id, run_id, trace_id) = run;
    let executions = executions(index, trace_id)?;
    let chosen = match execution {
        Some(id) => executions
            .iter()
            .find(|execution| execution.execution_id == id),
        None => latest(&executions).or(executions.last()),
    };
    let Some(chosen) = chosen else {
        return Ok(Err(match execution {
            Some(id) => format!("run {run_id} has no execution {id}"),
            None => format!("run {run_id} has no retained execution"),
        }));
    };
    let parts: Vec<(u32, u32, bool, String)> = {
        let mut statement = index
            .connection()
            .prepare(
                "SELECT part, parts, complete, rows FROM configs WHERE execution_id = ?1
                 ORDER BY part, segment_id, seq",
            )
            .map_err(sql)?;
        statement
            .query_map(params![chosen.execution_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get(3)?,
                ))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    let expected = parts.iter().map(|(_, parts, _, _)| *parts).max();
    let mut rows = Vec::new();
    let mut found = BTreeSet::new();
    for (part, _, _, text) in &parts {
        if !found.insert(*part) {
            continue;
        }
        let part_rows: Vec<crate::record::ConfigRow> =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        rows.extend(part_rows);
    }
    let missing: Vec<u32> = (1..=expected.unwrap_or(0))
        .filter(|part| !found.contains(part))
        .collect();
    let closed = parts
        .iter()
        .any(|(part, count, complete, _)| *complete && Some(*part) == expected && part == count);
    let state = match expected {
        None => "absent",
        Some(_) if missing.is_empty() && closed => "complete",
        Some(_) => "incomplete",
    };
    let texts = {
        let mut statement = index
            .connection()
            .prepare(
                "SELECT json_extract(attributes, '$.\"af.config.text.kind\"'),
                        json_extract(attributes, '$.\"af.config.text.digest\"'),
                        json_extract(attributes, '$.\"af.config.text.bytes\"')
                 FROM events WHERE span_id = ?1 AND name = 'af.config.text'
                 ORDER BY segment_id, seq",
            )
            .map_err(sql)?;
        statement
            .query_map(params![chosen.span_id], |row| {
                Ok(ConfigText {
                    kind: row
                        .get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| "unknown".into()),
                    digest: row.get(1)?,
                    bytes: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|bytes| u64::try_from(bytes).ok()),
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?
    };
    Ok(Ok(ConfigRecord {
        schema: CONFIG_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read,
        store_id: store_id.to_string(),
        run_id: run_id.to_string(),
        execution_id: chosen.execution_id.clone(),
        ordinal: chosen.ordinal,
        state: state.into(),
        parts: expected,
        missing_parts: missing,
        rows,
        texts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execution(start: i64, end: i64, ordinal: u64) -> Execution {
        Execution {
            execution_id: format!("e{ordinal}"),
            span_id: format!("s{ordinal}"),
            ordinal: Some(ordinal),
            store_sequence_at_start: None,
            start_unix_ns: start.to_string(),
            end_unix_ns: end.to_string(),
            open: false,
            wall_us: micros(end - start),
            segment: "sealed".into(),
            outcome: None,
        }
    }

    #[test]
    fn overlapping_executions_count_once_in_active_time_and_twice_in_execution_time() {
        // 0–10 s and 5–15 s: active 15 s, execution 20 s, elapsed 15 s.
        let executions = [
            execution(0, 10_000_000_000, 1),
            execution(5_000_000_000, 15_000_000_000, 2),
        ];
        assert_eq!(times(&executions), (15_000_000, 20_000_000, 15_000_000));
        // An idle gap: active and execution time leave it out, elapsed time keeps it.
        let gapped = [
            execution(0, 1_000_000_000, 1),
            execution(5_000_000_000, 6_000_000_000, 2),
        ];
        assert_eq!(times(&gapped), (2_000_000, 2_000_000, 6_000_000));
    }

    #[test]
    fn history_is_a_prefix_or_partial_and_never_complete() {
        assert_eq!(
            history(&[(Some(1), true, false), (Some(2), true, false)]).history,
            "prefix:2"
        );
        assert_eq!(
            history(&[(Some(1), true, false), (Some(2), true, false)]).label,
            "retained executions 1–2"
        );
        let partial = history(&[(Some(2), true, false), (Some(4), true, false)]);
        assert_eq!(partial.history, "partial");
        assert_eq!(partial.missing_ordinals, vec![1, 3]);
        assert_eq!(partial.label, "partial (missing executions 1, 3)");
        // A live execution after the prefix keeps the prefix, and is named open.
        let live = history(&[(Some(1), true, false), (Some(2), false, true)]);
        assert_eq!(live.history, "prefix:1");
        assert_eq!(live.label, "retained executions 1–1, open: 2");
        assert_eq!(history(&[(None, true, false)]).history, "partial");
        assert_eq!(parse_since("14d"), Some(Duration::from_secs(14 * 86_400)));
    }

    #[test]
    fn open_executions_are_named_never_hidden_behind_the_prefix() {
        // `prefix:0`: the open execution is listed, not hidden.
        let none = history(&[(Some(1), false, true)]);
        assert_eq!(none.history, "prefix:0");
        assert_eq!(none.label, "retained executions: none sealed, open: 1");
        // A sealed execution after an open one is named beside the prefix.
        let overlap = history(&[
            (Some(1), true, false),
            (Some(2), false, true),
            (Some(3), true, false),
        ]);
        assert_eq!(overlap.history, "prefix:1");
        assert_eq!(overlap.label, "retained executions 1–1, 3, open: 2");
        // Recovered with its run never ended: sealed, yet open.
        let recovered = history(&[(Some(1), true, false), (Some(2), true, true)]);
        assert_eq!(recovered.history, "prefix:2");
        assert_eq!(recovered.label, "retained executions 1–2, open: 2");
        let partial = history(&[(Some(2), true, false), (Some(3), false, true)]);
        assert_eq!(partial.label, "partial (missing executions 1), open: 3");
        assert_eq!(parse_since("x"), None);
    }
}
