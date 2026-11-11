//! An Attempt's accounting as Telemetry retained it (design §7.5, §9), two series never merged.
//! Part of the reader module (ADR-0153 clause 1).
//!
//! - **The charge series.** After every committed Task event that changed an Attempt's
//!   accounting, the kernel recorded the event `af.usage.state`: the Attempt's charge and
//!   counters as the Task projection held them after that commit, with `af.usage.sequence`, the
//!   committed event's own sequence. An Attempt's figures are its snapshot with the highest
//!   sequence — the kernel's own merged state, never one this reader merges — and run totals
//!   are sums of those snapshots. A reindex keeps the same records, so it gives the same answer.
//! - **The wall series.** Every committed `attempt_wall` sidecar write recorded `af.usage.wall`
//!   with the merged row and `af.usage.wall.sequence`, the record's own `seq`, which orders the
//!   writes of one execution and nothing else: executions overlap and the sidecar is written
//!   without a lease, so a predecessor's write can land after its successor's, and no sequence,
//!   start time or ordinal says which is later. The reader shows each execution's latest
//!   observation, labelled by execution, with the order across executions `unknown`, and never
//!   takes a maximum, a sum or a replacement of them; nothing of this series enters a charge.

use std::collections::BTreeMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::index::{Index, sum_decimals};

/// The charge series' event.
pub const USAGE_STATE: &str = "af.usage.state";
/// The wall series' event.
pub const USAGE_WALL: &str = "af.usage.wall";

fn sql(error: rusqlite::Error) -> String {
    format!("telemetry index: {error}")
}

/// The spans whose records belong to the run with trace `trace_id`: every span of its trace,
/// and the root `af.command` span of every segment that holds one of its executions (a commit
/// before the execution's `af.run` opened is recorded there).
pub(crate) const RUN_SPANS: &str = "SELECT span_id FROM spans WHERE trace_id = ?1
     UNION SELECT span_id FROM spans
       WHERE name = 'af.command' AND trace_id IS NULL AND parent_span_id IS NULL
         AND segment_id IN (SELECT s.segment_id FROM executions x
                            JOIN spans s ON s.span_id = x.span_id WHERE x.trace_id = ?1)";

/// One event of the run's spans, in record order: executions by their segment's start, then
/// `seq`.
pub(crate) struct EventRow {
    pub segment_id: String,
    pub seq: u64,
    pub time_unix_ns: i64,
    pub name: String,
    pub attributes: Map<String, Value>,
}

/// Every event named one of `names` on the run's spans, in record order.
pub(crate) fn run_events(
    index: &Index,
    trace_id: &str,
    names: &[&str],
) -> Result<Vec<EventRow>, String> {
    let placeholders = (0..names.len())
        .map(|at| format!("?{}", at + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT e.segment_id, e.seq, e.time_unix_ns, e.name, e.attributes
         FROM events e JOIN segments g ON g.segment_id = e.segment_id
         WHERE e.name IN ({placeholders}) AND e.span_id IN ({RUN_SPANS})
         ORDER BY g.started_unix_ns, e.segment_id, e.seq"
    );
    let mut statement = index.connection().prepare(&query).map_err(sql)?;
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&trace_id];
    for name in names {
        values.push(name);
    }
    let rows = statement
        .query_map(values.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(segment_id, seq, time_unix_ns, name, attributes)| EventRow {
                segment_id,
                seq: u64::try_from(seq).unwrap_or(0),
                time_unix_ns,
                name,
                attributes: match serde_json::from_str(&attributes) {
                    Ok(Value::Object(map)) => map,
                    _ => Map::new(),
                },
            },
        )
        .collect())
}

pub(crate) fn text(attributes: &Map<String, Value>, key: &str) -> Option<String> {
    attributes
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(crate) fn integer(attributes: &Map<String, Value>, key: &str) -> Option<u64> {
    attributes.get(key).and_then(Value::as_u64)
}

/// Whether an event belongs to the Task log `run_id` of the Store `store_id`.
fn of_run(attributes: &Map<String, Value>, store_id: &str, run_id: &str) -> bool {
    text(attributes, "af.store_id").as_deref() == Some(store_id)
        && text(attributes, "af.run_id").as_deref() == Some(run_id)
}

/// Exact token counters, decimal strings; a counter the source did not deliver is absent,
/// never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chargeable_tokens: Option<String>,
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

impl Counters {
    fn read(attributes: &Map<String, Value>, prefix: &str) -> Self {
        let counter = |name: &str| text(attributes, &format!("{prefix}.{name}"));
        Self {
            chargeable_tokens: counter("chargeable_tokens"),
            input_tokens: counter("input_tokens"),
            output_tokens: counter("output_tokens"),
            cache_read_tokens: counter("cache_read_tokens"),
            cache_write_tokens: counter("cache_write_tokens"),
            reasoning_tokens: counter("reasoning_tokens"),
        }
    }
}

/// The separately identified native observation the kernel kept beside an Attempt's charge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Native {
    /// `complete` when it completes the charge, `incomplete` otherwise.
    pub observation: String,
    pub counters: Counters,
}

impl Native {
    fn read(attributes: &Map<String, Value>) -> Option<Self> {
        text(attributes, "af.usage.native.observation").map(|observation| Self {
            observation,
            counters: Counters::read(attributes, "af.usage.native"),
        })
    }
}

/// An Attempt's charge snapshot: the kernel's accounting after one committed transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charge {
    /// `af.usage.sequence`: the committed event's own sequence in the Task's log.
    pub sequence: u64,
    /// `af.usage.source`: `usage_observed`, `settled` or `released`.
    pub source: String,
    /// The execution (segment) that recorded it.
    pub execution_id: String,
    /// The charge and counters as the Task projection held them; the charge is
    /// `af.usage.chargeable_tokens`.
    pub counters: Counters,
    /// `af.usage.observation`: `complete`, `incomplete` or `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<String>,
    /// `af.usage.unknown_cause`, when the observation is `unknown` (ADR-0143).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

/// One execution's latest sidecar observation of an Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wall {
    /// The execution (segment) whose write it is.
    pub execution_id: String,
    /// `af.usage.wall.sequence`: the write's `seq` in that execution's segment; it orders that
    /// execution's writes and nothing else.
    pub wall_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// The merged sidecar row's usage: never an Attempt's charge.
    pub counters: Counters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

/// One Attempt: its charge snapshot with the highest sequence, and beside it the latest wall
/// observation of each execution that wrote one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// `None` when no charge snapshot of the Attempt is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge: Option<Charge>,
    /// One per execution that wrote the sidecar, in record order.
    pub walls: Vec<Wall>,
    /// The order of `walls` across executions: `unknown` when more than one execution wrote,
    /// `one execution` when one did, `none` when none did.
    pub wall_order: String,
}

/// The run's totals: sums of the Attempts' charge snapshots, exact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    /// Attempts with a retained charge snapshot: the ones the sums cover.
    pub attempts: u64,
    /// Of those, the ones whose usage is `unknown`, charged zero.
    pub unknown_usage_attempts: u64,
    /// Each counter summed over the Attempts; absent when one Attempt did not deliver it.
    pub counters: Counters,
}

/// The accounting of one run: every Attempt of the Task log `run_id` with a retained charge
/// snapshot or wall observation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounting {
    pub attempts: Vec<Attempt>,
    pub totals: Totals,
}

/// The attributes of each Attempt's charge snapshot with the highest `af.usage.sequence`, by
/// Attempt id: what `runs` sums.
pub(crate) fn charge_attributes(
    index: &Index,
    trace_id: &str,
    store_id: &str,
    run_id: &str,
) -> Result<BTreeMap<String, Map<String, Value>>, String> {
    let mut latest: BTreeMap<String, (u64, Map<String, Value>)> = BTreeMap::new();
    for row in run_events(index, trace_id, &[USAGE_STATE])? {
        if !of_run(&row.attributes, store_id, run_id) {
            continue;
        }
        let (Some(attempt), Some(sequence)) = (
            text(&row.attributes, "af.attempt_id"),
            integer(&row.attributes, "af.usage.sequence"),
        ) else {
            continue;
        };
        // The highest committed sequence; of two records of one commit, the first retained.
        if latest
            .get(&attempt)
            .is_none_or(|(kept, _)| sequence > *kept)
        {
            latest.insert(attempt, (sequence, row.attributes));
        }
    }
    Ok(latest
        .into_iter()
        .map(|(attempt, (_, attributes))| (attempt, attributes))
        .collect())
}

/// The accounting of the Task log `run_id` of the Store `store_id`, from the run with trace
/// `trace_id`.
pub fn accounting(
    index: &Index,
    trace_id: &str,
    store_id: &str,
    run_id: &str,
) -> Result<Accounting, String> {
    let mut attempts: BTreeMap<String, Attempt> = BTreeMap::new();
    let mut charges: BTreeMap<String, (u64, Charge)> = BTreeMap::new();
    // Each execution's latest write of each Attempt, by (Attempt, execution).
    let mut walls: BTreeMap<(String, String), Wall> = BTreeMap::new();
    let mut wall_order: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in run_events(index, trace_id, &[USAGE_STATE, USAGE_WALL])? {
        if !of_run(&row.attributes, store_id, run_id) {
            continue;
        }
        let Some(attempt_id) = text(&row.attributes, "af.attempt_id") else {
            continue;
        };
        let attempt = attempts
            .entry(attempt_id.clone())
            .or_insert_with(|| Attempt {
                attempt_id: attempt_id.clone(),
                node_id: None,
                charge: None,
                walls: Vec::new(),
                wall_order: "none".into(),
            });
        if attempt.node_id.is_none() {
            attempt.node_id = text(&row.attributes, "af.node_id");
        }
        if row.name == USAGE_STATE {
            let Some(sequence) = integer(&row.attributes, "af.usage.sequence") else {
                continue;
            };
            if charges
                .get(&attempt_id)
                .is_some_and(|(kept, _)| sequence <= *kept)
            {
                continue;
            }
            charges.insert(
                attempt_id,
                (
                    sequence,
                    Charge {
                        sequence,
                        source: text(&row.attributes, "af.usage.source").unwrap_or_default(),
                        execution_id: row.segment_id.clone(),
                        counters: Counters::read(&row.attributes, "af.usage"),
                        observation: text(&row.attributes, "af.usage.observation"),
                        unknown_cause: text(&row.attributes, "af.usage.unknown_cause"),
                        native: Native::read(&row.attributes),
                    },
                ),
            );
        } else {
            let wall_sequence =
                integer(&row.attributes, "af.usage.wall.sequence").unwrap_or(row.seq);
            let key = (attempt_id.clone(), row.segment_id.clone());
            // Within one execution the write with the greater `seq` is the later one.
            if walls
                .get(&key)
                .is_some_and(|kept| kept.wall_sequence >= wall_sequence)
            {
                continue;
            }
            let order = wall_order.entry(attempt_id).or_default();
            if !order.contains(&row.segment_id) {
                order.push(row.segment_id.clone());
            }
            walls.insert(
                key,
                Wall {
                    execution_id: row.segment_id.clone(),
                    wall_sequence,
                    started_unix_ms: integer(&row.attributes, "af.usage.wall.started_unix_ms"),
                    elapsed_ms: integer(&row.attributes, "af.usage.wall.elapsed_ms"),
                    counters: Counters::read(&row.attributes, "af.usage"),
                    native: Native::read(&row.attributes),
                },
            );
        }
    }
    for (attempt_id, attempt) in &mut attempts {
        attempt.charge = charges.remove(attempt_id).map(|(_, charge)| charge);
        let executions = wall_order.remove(attempt_id).unwrap_or_default();
        attempt.wall_order = match executions.len() {
            0 => "none",
            1 => "one execution",
            _ => "unknown",
        }
        .into();
        attempt.walls = executions
            .into_iter()
            .filter_map(|execution| walls.remove(&(attempt_id.clone(), execution)))
            .collect();
    }
    let attempts: Vec<Attempt> = attempts.into_values().collect();
    let totals = totals(&attempts);
    Ok(Accounting { attempts, totals })
}

/// Sums of the Attempts' charge snapshots, each counter exact; a counter one charged Attempt
/// did not deliver is absent from the sum.
pub fn totals(attempts: &[Attempt]) -> Totals {
    let charged: Vec<&Charge> = attempts
        .iter()
        .filter_map(|attempt| attempt.charge.as_ref())
        .collect();
    let sum = |field: fn(&Counters) -> &Option<String>| -> Option<String> {
        if charged.is_empty() {
            return None;
        }
        let values: Option<Vec<&str>> = charged
            .iter()
            .map(|charge| field(&charge.counters).as_deref())
            .collect();
        sum_decimals(values?)
    };
    Totals {
        attempts: charged.len() as u64,
        unknown_usage_attempts: charged
            .iter()
            .filter(|charge| charge.observation.as_deref() == Some("unknown"))
            .count() as u64,
        counters: Counters {
            chargeable_tokens: sum(|counters| &counters.chargeable_tokens),
            input_tokens: sum(|counters| &counters.input_tokens),
            output_tokens: sum(|counters| &counters.output_tokens),
            cache_read_tokens: sum(|counters| &counters.cache_read_tokens),
            cache_write_tokens: sum(|counters| &counters.cache_write_tokens),
            reasoning_tokens: sum(|counters| &counters.reasoning_tokens),
        },
    }
}

/// How many `af.usage.state` and `af.usage.wall` events the run's spans hold (fixtures).
pub fn series_counts(index: &Index, trace_id: &str) -> Result<(u64, u64), String> {
    let count = |name: &str| -> Result<u64, String> {
        index
            .connection()
            .query_row(
                &format!(
                    "SELECT count(*) FROM events WHERE name = ?2 AND span_id IN ({RUN_SPANS})"
                ),
                params![trace_id, name],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| u64::try_from(count).unwrap_or(0))
            .map_err(sql)
    };
    Ok((count(USAGE_STATE)?, count(USAGE_WALL)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charge(sequence: u64, counters: Counters, observation: &str) -> Attempt {
        Attempt {
            attempt_id: format!("a{sequence}"),
            node_id: None,
            charge: Some(Charge {
                sequence,
                source: "settled".into(),
                execution_id: "1-1-0a1b2c3d".into(),
                counters,
                observation: Some(observation.into()),
                unknown_cause: None,
                native: None,
            }),
            walls: Vec::new(),
            wall_order: "none".into(),
        }
    }

    #[test]
    fn totals_are_exact_sums_and_a_missing_counter_is_absent() {
        let above_u64 = "18446744073709551616"; // 2^64
        let attempts = [
            charge(
                1,
                Counters {
                    chargeable_tokens: Some(above_u64.into()),
                    input_tokens: Some("9007199254740993".into()), // 2^53 + 1
                    ..Counters::default()
                },
                "complete",
            ),
            charge(
                2,
                Counters {
                    chargeable_tokens: Some(above_u64.into()),
                    ..Counters::default()
                },
                "unknown",
            ),
        ];
        let totals = totals(&attempts);
        assert_eq!(
            totals.counters.chargeable_tokens.as_deref(),
            Some("36893488147419103232")
        );
        assert_eq!(totals.counters.input_tokens, None);
        assert_eq!(totals.attempts, 2);
        assert_eq!(totals.unknown_usage_attempts, 1);
    }
}
