//! `af telemetry ledger` (design §7.5, §10): `af/telemetry-ledger@1`, the committed Store
//! events, Finding and Demand summaries and accounting snapshots of one run, in the order they
//! were recorded, with the coverage and history markers. Part of the reader module (ADR-0153
//! clause 1): it reads the telemetry index and never a Store, so it answers from a copied
//! telemetry directory as from the machine that recorded it.
//!
//! **What belongs to a run.** The records on the spans of its trace, and those on the root
//! `af.command` span of a segment that holds one of its executions (a commit before the
//! execution's `af.run` opened is recorded there) that name the run's Store.
//!
//! **Coverage.** `complete` only when every committed event of every Store log the run's
//! executions mirrored reached Telemetry, from the log's first sequence to the last one
//! mirrored, with every Finding and Demand summary: every retained execution's segment was
//! sealed by its writer with nothing dropped, its `af.run` says `af.ledger.coverage =
//! complete`, the history is a prefix with no execution open or missing, no `observer` problem
//! was recorded, and no sequence is missing. Otherwise `incomplete`, with every reason.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::accounting::{self, Accounting, EventRow, RUN_SPANS, integer, text};
use crate::index::Index;
use crate::view::{self, Executions, IndexRead, Unreadable};

/// `af/telemetry-ledger@1`.
pub const LEDGER_SCHEMA: &str = "af/telemetry-ledger@1";

/// The event mirroring one committed Store event.
pub const STORE_EVENT: &str = "af.store.event";
/// The lifecycle events of Findings and Demands.
pub const LIFECYCLE: [&str; 5] = [
    "af.finding.disposition",
    "af.demand.opened",
    "af.demand.evidence_satisfied",
    "af.demand.stale",
    "af.demand.waived",
];

fn sql(error: rusqlite::Error) -> String {
    format!("telemetry index: {error}")
}

/// The run the document describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRun {
    pub store_id: String,
    pub run_id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `prefix:k` or `partial`; never complete.
    pub history: String,
    pub missing_ordinals: Vec<u64>,
    /// What everything here covers: `retained executions 1–k`, naming the open ones, or
    /// `partial (…)`.
    pub retained: String,
    pub executions: Executions,
}

/// A range of sequences of one Store log no retained record mirrors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Missing {
    pub from: u64,
    pub to: u64,
}

/// One Store log the run's executions mirrored: a Task's log or a Campaign's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    /// The log's kernel run id (`task:…` for a Task, the Campaign's run id otherwise).
    pub run_id: String,
    /// Committed events mirrored.
    pub mirrored: u64,
    /// The highest sequence mirrored.
    pub last_sequence: u64,
    /// The sequences from 0 to `last_sequence` no retained record mirrors.
    pub missing: Vec<Missing>,
}

/// One recorded item, in record order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// `event` (a committed Store event), `finding` or `demand` (a summary record), or
    /// `lifecycle` (a disposition or Demand lifecycle event).
    pub entry: String,
    /// The execution (segment) that recorded it, and its `seq` there.
    pub execution_id: String,
    pub seq: u64,
    /// When an `event` or a `lifecycle` was recorded: unix nanoseconds, a decimal string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_unix_ns: Option<String>,
    /// The Store log, for an `event` or a `lifecycle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// The committed event's sequence in its log: an `event`'s own, a `lifecycle`'s cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// An `event`'s type, a `lifecycle`'s name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_bytes: Option<u64>,
    /// A `finding`'s or a `lifecycle`'s Finding key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_key: Option<String>,
    /// A `demand`'s or a `lifecycle`'s Demand id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demand_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    /// The status a `finding`, `demand` or `lifecycle` records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<String>,
}

impl Entry {
    fn new(entry: &str, row_segment: &str, seq: u64) -> Self {
        Self {
            entry: entry.into(),
            execution_id: row_segment.into(),
            seq,
            time_unix_ns: None,
            log: None,
            sequence: None,
            name: None,
            event_id: None,
            causation_id: None,
            artifact_refs: Vec::new(),
            payload_digest: None,
            payload_bytes: None,
            finding_key: None,
            demand_id: None,
            evidence_id: None,
            status: None,
            previous_status: None,
        }
    }
}

/// One Finding's change: a disposition event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disposition {
    pub status: String,
    pub previous_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

/// A Finding: its latest summary, with every disposition after it was admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingSummary {
    pub key: String,
    pub severity: String,
    pub effective_severity: String,
    pub status: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub title: String,
    pub report_artifact_id: String,
    pub node_id: String,
    pub worker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    pub dispositions: Vec<Disposition>,
}

/// One step of a Demand's lifecycle, keyed by the Demand id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    /// `opened`, `evidence_satisfied`, `stale` or `waived`.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

/// A Demand: its latest summary and its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemandSummary {
    pub demand_id: String,
    pub kind: String,
    pub required: bool,
    pub subject_id: String,
    pub snapshot_id: String,
    pub status: String,
    pub lifecycle: Vec<Lifecycle>,
}

/// `af/telemetry-ledger@1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub schema: String,
    pub directory: String,
    pub index: IndexRead,
    pub run: LedgerRun,
    /// `complete` or `incomplete`.
    pub coverage: String,
    /// Why it is incomplete; empty when complete.
    pub coverage_reasons: Vec<String>,
    pub logs: Vec<Log>,
    /// Every committed event, summary and lifecycle event, in record order.
    pub entries: Vec<Entry>,
    pub findings: Vec<FindingSummary>,
    pub demands: Vec<DemandSummary>,
    /// Each Attempt's charge snapshot and wall observations, and the run's totals.
    pub accounting: Accounting,
    pub unreadable: Vec<Unreadable>,
}

/// A summary record of the run, in record order.
struct SummaryRow {
    segment_id: String,
    seq: u64,
    record: Map<String, Value>,
}

fn summaries(index: &Index, trace_id: &str, table: &str) -> Result<Vec<SummaryRow>, String> {
    let mut statement = index
        .connection()
        .prepare(&format!(
            "SELECT r.segment_id, r.seq, r.record
             FROM {table} r JOIN segments g ON g.segment_id = r.segment_id
             WHERE r.span_id IN ({RUN_SPANS})
             ORDER BY g.started_unix_ns, r.segment_id, r.seq"
        ))
        .map_err(sql)?;
    let rows = statement
        .query_map(params![trace_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(rows
        .into_iter()
        .map(|(segment_id, seq, record)| SummaryRow {
            segment_id,
            seq: u64::try_from(seq).unwrap_or(0),
            record: match serde_json::from_str(&record) {
                Ok(Value::Object(map)) => map,
                _ => Map::new(),
            },
        })
        .collect())
}

/// The order key of a record: its segment's start, its segment, its `seq`.
fn segment_starts(index: &Index) -> Result<BTreeMap<String, i64>, String> {
    let mut statement = index
        .connection()
        .prepare("SELECT segment_id, started_unix_ns FROM segments")
        .map_err(sql)?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql)?
        .collect::<Result<BTreeMap<String, i64>, _>>()
        .map_err(sql)?;
    Ok(rows)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn field(record: &Map<String, Value>, key: &str) -> String {
    text(record, key).unwrap_or_default()
}

/// The ledger of the run `(store_id, run_id, trace_id, kind)`.
pub fn ledger(
    index: &Index,
    directory: &Path,
    read: IndexRead,
    unreadable: Vec<Unreadable>,
    (store_id, run_id, trace_id, kind): (&str, &str, &str, Option<String>),
) -> Result<Ledger, String> {
    let executions = view::executions(index, trace_id)?;
    let capture = view::coverage(index, &executions)?;
    let executions = Executions::of(executions);
    let history = executions.history();
    let mut reasons = Vec::new();
    if executions.list.is_empty() {
        reasons.push("no execution of the run is retained".to_string());
    }
    if capture != "complete" {
        reasons.push(
            "a retained execution's segment is live, unsealed, recovered or dropped records"
                .to_string(),
        );
    }
    if history.history == "partial" {
        reasons.push(format!("the history is {}", history.label));
    }
    if !executions.open.is_empty() {
        reasons.push("an execution is open".to_string());
    }
    for execution in &executions.list {
        let coverage: Option<String> = index
            .connection()
            .query_row(
                "SELECT json_extract(end_attributes, '$.\"af.ledger.coverage\"') FROM spans
                 WHERE span_id = ?1",
                params![execution.span_id],
                |row| row.get(0),
            )
            .map_err(sql)?;
        match coverage.as_deref() {
            Some("complete") => {}
            Some(other) => reasons.push(format!(
                "execution {} recorded af.ledger.coverage = {other}",
                execution.execution_id
            )),
            None => reasons.push(format!(
                "execution {} recorded no af.ledger.coverage (the mirror records from `spans` on)",
                execution.execution_id
            )),
        }
    }
    let observer_problems: i64 = index
        .connection()
        .query_row(
            "SELECT count(*) FROM problems WHERE what = 'observer' AND segment_id IN
                 (SELECT s.segment_id FROM executions x JOIN spans s ON s.span_id = x.span_id
                  WHERE x.trace_id = ?1)",
            params![trace_id],
            |row| row.get(0),
        )
        .map_err(sql)?;
    if observer_problems > 0 {
        reasons.push(format!(
            "{observer_problems} Store mirror problem(s) were recorded"
        ));
    }

    let starts = segment_starts(index)?;
    let order = |segment: &str, seq: u64| {
        (
            starts.get(segment).copied().unwrap_or(0),
            segment.to_string(),
            seq,
        )
    };
    let mut entries: Vec<((i64, String, u64), Entry)> = Vec::new();
    let mut logs: BTreeMap<String, BTreeMap<u64, String>> = BTreeMap::new();
    let mut dispositions: BTreeMap<String, Vec<Disposition>> = BTreeMap::new();
    let mut lifecycles: BTreeMap<String, Vec<Lifecycle>> = BTreeMap::new();
    // A lifecycle event of one committed event, recorded by two executions (the second folded
    // the log from its start), is listed once.
    let mut lifecycle_seen: std::collections::BTreeSet<(String, String, String, Option<u64>)> =
        std::collections::BTreeSet::new();
    let mut names = vec![STORE_EVENT];
    names.extend(LIFECYCLE);
    for EventRow {
        segment_id,
        seq,
        time_unix_ns,
        name,
        attributes,
    } in accounting::run_events(index, trace_id, &names)?
    {
        if text(&attributes, "af.store_id").as_deref() != Some(store_id) {
            continue;
        }
        let log = text(&attributes, "af.run_id");
        let sequence = integer(&attributes, "af.store.event.sequence");
        let mut entry = Entry::new(
            if name == STORE_EVENT {
                "event"
            } else {
                "lifecycle"
            },
            &segment_id,
            seq,
        );
        entry.time_unix_ns = Some(time_unix_ns.to_string());
        entry.log = log.clone();
        entry.sequence = sequence;
        if name == STORE_EVENT {
            let (Some(log), Some(sequence)) = (&log, sequence) else {
                continue;
            };
            let event_id = text(&attributes, "af.store.event.id");
            let seen = logs.entry(log.clone()).or_default();
            // A commit is mirrored by the one execution that made it; a repeat is the same
            // event recorded twice and is listed once.
            if seen.contains_key(&sequence) {
                continue;
            }
            seen.insert(sequence, event_id.clone().unwrap_or_default());
            entry.name = text(&attributes, "af.store.event.type");
            entry.event_id = event_id;
            entry.causation_id = text(&attributes, "af.store.event.causation_id");
            entry.artifact_refs = strings(attributes.get("artifact_refs"));
            entry.payload_digest = text(&attributes, "af.store.event.payload_digest");
            entry.payload_bytes = integer(&attributes, "af.store.event.payload_bytes");
        } else {
            entry.name = Some(name.clone());
            entry.finding_key = text(&attributes, "af.finding.id");
            entry.demand_id = text(&attributes, "af.demand.id");
            entry.evidence_id = text(&attributes, "af.evidence.id");
            entry.status = text(&attributes, "af.finding.status")
                .or_else(|| text(&attributes, "af.demand.status"));
            entry.previous_status = text(&attributes, "af.finding.previous_status");
            if !lifecycle_seen.insert((
                log.clone().unwrap_or_default(),
                name.clone(),
                entry
                    .finding_key
                    .clone()
                    .or_else(|| entry.demand_id.clone())
                    .unwrap_or_default(),
                sequence,
            )) {
                continue;
            }
            if let (Some(key), Some(status), Some(previous)) =
                (&entry.finding_key, &entry.status, &entry.previous_status)
            {
                dispositions
                    .entry(key.clone())
                    .or_default()
                    .push(Disposition {
                        status: status.clone(),
                        previous_status: previous.clone(),
                        sequence,
                    });
            }
            if let Some(demand) = &entry.demand_id {
                lifecycles
                    .entry(demand.clone())
                    .or_default()
                    .push(Lifecycle {
                        event: name.strip_prefix("af.demand.").unwrap_or(&name).to_string(),
                        evidence_id: entry.evidence_id.clone(),
                        sequence,
                    });
            }
        }
        entries.push((order(&segment_id, seq), entry));
    }

    let mut findings: BTreeMap<String, FindingSummary> = BTreeMap::new();
    let mut finding_order: Vec<String> = Vec::new();
    for SummaryRow {
        segment_id,
        seq,
        record,
    } in summaries(index, trace_id, "findings")?
    {
        let key = field(&record, "key");
        let mut entry = Entry::new("finding", &segment_id, seq);
        entry.finding_key = Some(key.clone());
        entry.status = text(&record, "status");
        entries.push((order(&segment_id, seq), entry));
        if !finding_order.contains(&key) {
            finding_order.push(key.clone());
        }
        findings.insert(
            key.clone(),
            FindingSummary {
                key,
                severity: field(&record, "severity"),
                effective_severity: field(&record, "effective_severity"),
                status: field(&record, "status"),
                scope: field(&record, "scope"),
                file: text(&record, "file"),
                line: integer(&record, "line"),
                title: field(&record, "title"),
                report_artifact_id: field(&record, "report_artifact_id"),
                node_id: field(&record, "node_id"),
                worker: field(&record, "worker"),
                model: text(&record, "model"),
                round: integer(&record, "round"),
                epoch: integer(&record, "epoch"),
                dispositions: Vec::new(),
            },
        );
    }
    let findings: Vec<FindingSummary> = finding_order
        .into_iter()
        .filter_map(|key| {
            findings.remove(&key).map(|mut finding| {
                finding.dispositions = dispositions.remove(&key).unwrap_or_default();
                finding
            })
        })
        .collect();

    let mut demands: BTreeMap<String, DemandSummary> = BTreeMap::new();
    let mut demand_order: Vec<String> = Vec::new();
    for SummaryRow {
        segment_id,
        seq,
        record,
    } in summaries(index, trace_id, "demands")?
    {
        let demand_id = field(&record, "demand_id");
        let mut entry = Entry::new("demand", &segment_id, seq);
        entry.demand_id = Some(demand_id.clone());
        entry.status = text(&record, "status");
        entries.push((order(&segment_id, seq), entry));
        if !demand_order.contains(&demand_id) {
            demand_order.push(demand_id.clone());
        }
        demands.insert(
            demand_id.clone(),
            DemandSummary {
                demand_id,
                kind: field(&record, "kind"),
                required: record
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                subject_id: field(&record, "subject_id"),
                snapshot_id: field(&record, "snapshot_id"),
                status: field(&record, "status"),
                lifecycle: Vec::new(),
            },
        );
    }
    let demands: Vec<DemandSummary> = demand_order
        .into_iter()
        .filter_map(|id| {
            demands.remove(&id).map(|mut demand| {
                demand.lifecycle = lifecycles.remove(&id).unwrap_or_default();
                demand
            })
        })
        .collect();

    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    let entries: Vec<Entry> = entries.into_iter().map(|(_, entry)| entry).collect();
    let logs: Vec<Log> = logs
        .into_iter()
        .map(|(run_id, sequences)| {
            let last_sequence = sequences.keys().next_back().copied().unwrap_or(0);
            let mut missing = Vec::new();
            let mut next = 0u64;
            for sequence in sequences.keys() {
                if *sequence > next {
                    missing.push(Missing {
                        from: next,
                        to: sequence - 1,
                    });
                }
                next = sequence + 1;
            }
            Log {
                run_id,
                mirrored: sequences.len() as u64,
                last_sequence,
                missing,
            }
        })
        .collect();
    for log in &logs {
        for missing in &log.missing {
            reasons.push(format!(
                "log {} sequences {}–{} were not mirrored",
                log.run_id, missing.from, missing.to
            ));
        }
    }
    let accounting = accounting::accounting(index, trace_id, store_id, run_id)?;
    Ok(Ledger {
        schema: LEDGER_SCHEMA.into(),
        directory: directory.display().to_string(),
        index: read,
        run: LedgerRun {
            store_id: store_id.to_string(),
            run_id: run_id.to_string(),
            trace_id: trace_id.to_string(),
            kind,
            history: history.history,
            missing_ordinals: history.missing_ordinals,
            retained: history.label,
            executions,
        },
        coverage: if reasons.is_empty() {
            "complete".into()
        } else {
            "incomplete".into()
        },
        coverage_reasons: reasons,
        logs,
        entries,
        findings,
        demands,
        accounting,
        unreadable,
    })
}
