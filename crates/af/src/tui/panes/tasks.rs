//! Tasks: every Task this scope's Task state holds (docs/design/tui.md section 5.5), read by
//! the functions behind `af task list --json` (`task_execution::list_common`) and `af task
//! explain --json`, which is the `af task show --json` document with the plan and its graph.
//!
//! The bar groups the Tasks by state, and the folder's main pane opens on what needs a person,
//! grouped as `af task list --attention` groups the same entries; one Task's main pane shows its
//! identity, one row per stage of its plan's graph order, its token and time totals, and its
//! event history. Every value is a field of those documents or of an artifact they name by ID:
//! the node an invocation ran and the goal of the revision. Nothing is written. Every read of the
//! Stores runs on a thread: the one a load or `R` starts, and the one that reads the opened running
//! Task again about once a second. The event loop collects each Store as it is read, so the bar
//! fills Store by Store and the browser never waits on one (ADR-0240).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{Effect, Pane, Row};
use crate::task_execution;
use crate::tui::keymap::Key;
use crate::tui::paint::{self, Paint, Span, Tone};
use crate::tui::scope::{Scope, ScopeKind};
use crate::tui::tree::Item;

/// How often an opened running Task is read again.
const LIVE: Duration = Duration::from_secs(1);
/// The bar's columns, its separator excluded.
const BAR_INNER: usize = crate::tui::BAR_WIDTH - 1;
/// The hex digits of an artifact ID the pane prints; `Enter` and `y` use the whole ID.
const SHORT: usize = 8;
/// The columns a stage name takes in PROGRESS.
const STAGE: usize = 18;
/// The columns of the main pane beside the bar at 100 columns; the TASK line fits its goal
/// into them.
const MAIN: usize = 100 - crate::tui::BAR_WIDTH;
/// The bar id of a Store that cannot be read.
const UNREADABLE: &str = "!unreadable";
/// The bar id of the Stores an earlier af release wrote, listed as one entry.
const EARLIER: &str = "!earlier";

/// `text` in rows of at most `width` columns, each starting with `indent`: split at the last
/// space that fits, or mid-word for a word (a path) longer than a row.
pub(crate) fn bounded(text: &str, width: usize, indent: &str) -> Vec<String> {
    let room = width.saturating_sub(indent.chars().count()).max(1);
    let mut rows = Vec::new();
    let mut rest: Vec<char> = text.chars().collect();
    while rest.len() > room {
        let cut = rest[..=room]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|at| *at > 0)
            .unwrap_or(room);
        let line: String = rest[..cut].iter().collect();
        rows.push(format!("{indent}{}", line.trim_end()));
        rest.drain(..cut);
        while rest.first() == Some(&' ') {
            rest.remove(0);
        }
    }
    rows.push(format!("{indent}{}", rest.iter().collect::<String>()));
    rows
}

/// Whether this release refused the Store because another af release wrote it: such Stores
/// are listed together, once, by the releases that wrote them, rather than as one error each.
fn earlier(store: &Store) -> bool {
    store.release.is_some() && store.tasks.is_err()
}

/// Where a Task is: its phase, and for a finished Task the acceptance its result records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum State {
    Running,
    Awaiting,
    Done,
    Failed,
}

impl State {
    const ALL: [State; 4] = [State::Running, State::Awaiting, State::Done, State::Failed];

    pub(crate) fn word(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Awaiting => "awaiting approval",
            State::Done => "done",
            State::Failed => "failed",
        }
    }

    /// The state's tone: a Task awaiting approval needs a person, as a failed one does.
    pub(crate) fn tone(self) -> Tone {
        match self {
            State::Running => Tone::Active,
            State::Done => Tone::Ok,
            State::Awaiting | State::Failed => Tone::Fail,
        }
    }
}

/// A Task's state from the `phase` of its list entry, and for a finished Task from its
/// result's `acceptance`: a plan awaiting `af task run --confirm-plan` or a signed decision
/// awaits approval, any other unfinished phase is running, and a finished Task is done only
/// when its acceptance is satisfied.
pub(crate) fn state_of(phase: &Value, acceptance: Option<&str>) -> State {
    match phase["kind"].as_str() {
        Some("finished") if acceptance == Some("satisfied") => State::Done,
        Some("finished") => State::Failed,
        Some("ready") => State::Awaiting,
        Some("waiting") if phase["reason"] == "needs_plan_review" => State::Awaiting,
        _ => State::Running,
    }
}

/// How one stage of the plan stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Ok,
    Running,
    Failed,
    /// The run report suppressed it: a branch not selected, or an upstream that is missing.
    Skipped,
    NotReached,
}

impl Mark {
    fn text(self) -> &'static str {
        match self {
            Mark::Ok => "[ok]",
            Mark::Running => "[..]",
            Mark::Failed => "[!!]",
            Mark::Skipped | Mark::NotReached => "[  ]",
        }
    }
}

/// One node of the plan's graph order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stage {
    pub(crate) node: String,
    pub(crate) mark: Mark,
    /// Attempts reserved for the node.
    pub(crate) attempts: u64,
    /// The node's Attempt allowance in the compiled graph.
    pub(crate) max_attempts: Option<u64>,
    /// It will not run again in this plan: done, skipped, or failed with no Attempt left (or
    /// closed failed by the plan's report, or the Task finished). PROGRESS counts these.
    pub(crate) closed: bool,
    /// The recorded wall of its Attempts (`attempt_walls`), when the document carries one.
    pub(crate) wall_ms: Option<u64>,
    /// The charge its settled Attempts recorded, when one settled.
    pub(crate) tokens: Option<u128>,
    /// Its settled Attempts whose usage is unknown: no usage was reported, so each was charged
    /// zero (ADR-0143) and `tokens` is not their spend.
    pub(crate) unknown: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) settled: usize,
    pub(crate) stages: usize,
}

impl Progress {
    pub(crate) fn of(stages: &[Stage]) -> Progress {
        let settled = stages.iter().filter(|stage| stage.closed).count();
        Progress {
            settled,
            stages: stages.len(),
        }
    }

    pub(crate) fn percent(self) -> usize {
        (self.settled * 100).checked_div(self.stages).unwrap_or(0)
    }
}

/// What the records of one attempt-bearing node add up to.
#[derive(Default)]
struct Track {
    /// The last event sequence that recorded one of this node's records.
    last_sequence: u64,
    attempts: u64,
    open: bool,
    ok: bool,
    failed: bool,
    settled: u64,
    tokens: u128,
    unknown: u64,
}

/// The stages of a Task's plan in graph order, from an `af task explain --json` document.
/// `node_of` names the node and the plan of an invocation or output artifact the records name.
/// Only records of the document's current plan count: a source refresh or a Review
/// continuation installs a new plan whose nodes may reuse an earlier plan's names.
pub(crate) fn stages(
    document: &Value,
    node_of: &mut dyn FnMut(&str) -> Result<Invoked, String>,
) -> Result<Vec<Stage>, String> {
    let finished = document["phase"]["kind"] == "finished";
    let current = document["plan_id"].as_str();
    let mut tracks: BTreeMap<String, Track> = BTreeMap::new();
    // The current plan's Attempts, by id, and the node each ran.
    let mut attempts: BTreeMap<String, String> = BTreeMap::new();
    // The highest charge recorded for each Attempt: a usage observation may exceed the charge
    // its settlement names, and the Store keeps the higher one. An Attempt with an entry here
    // reported its usage, even 0 tokens.
    let mut observed: BTreeMap<String, u128> = BTreeMap::new();
    // Each settled Attempt's node and the charge its settlement names; summed after the scan,
    // since a usage observation may arrive after the settlement it raises.
    let mut settled_charges: BTreeMap<String, (String, u128, bool)> = BTreeMap::new();
    // The event sequence that recorded each execution record, to tell a report's outcome from
    // records written after it.
    let mut sequence_of: BTreeMap<&str, u64> = BTreeMap::new();
    for event in array(&document["history"]) {
        let change = &event["transition"]["change"];
        if let (Some(id), Some(sequence)) =
            (change["record_id"].as_str(), event["sequence"].as_u64())
        {
            sequence_of.insert(id, sequence);
        }
    }
    for entry in array(&document["execution_records"]) {
        let record = &entry["record"];
        let attempt = record["attempt_id"].as_str();
        let invoked = match (record["kind"].as_str(), attempt) {
            (Some("invocation" | "reserved"), _) => node_of(text(&record["invocation_id"])?)?,
            // A published output with no Attempt, and a dynamic Scatter's completion of its
            // parent, both name the output of the node's own invocation.
            (Some("published"), None) | (Some("owned_children_completed"), _) => {
                node_of(text(&record["output_id"])?)?
            }
            (Some(_), Some(attempt)) => match attempts.get(attempt) {
                Some(node) => Invoked {
                    node: node.clone(),
                    plan_id: None,
                },
                None => continue,
            },
            _ => continue,
        };
        if invoked.plan_id.is_some() && invoked.plan_id.as_deref() != current {
            continue;
        }
        let node = invoked.node;
        let track = tracks.entry(node.clone()).or_default();
        if let Some(sequence) = entry["artifact_id"]
            .as_str()
            .and_then(|id| sequence_of.get(id))
        {
            track.last_sequence = track.last_sequence.max(*sequence);
        }
        match record["kind"].as_str() {
            Some("reserved") => {
                if let Some(attempt) = attempt {
                    attempts.insert(attempt.to_owned(), node);
                }
                track.attempts += 1;
                track.open = true;
            }
            // A reservation released before dispatch consumed no Attempt.
            Some("released") => {
                track.open = false;
                track.attempts = track.attempts.saturating_sub(1);
            }
            Some("usage_observed") => {
                if let Some(attempt) = attempt {
                    let charged = text(&record["charged_tokens"])?;
                    let charged: u128 = charged.parse().map_err(|_| "a charge is not decimal")?;
                    let seen = observed.entry(attempt.to_owned()).or_default();
                    *seen = (*seen).max(charged);
                }
            }
            Some("settled") => {
                track.open = false;
                track.settled += 1;
                let charged = text(&record["charged_tokens"])?;
                let charged: u128 = charged.parse().map_err(|_| "a charge is not decimal")?;
                if let Some(attempt) = attempt {
                    let unknown = record["unknown_usage"].is_object();
                    settled_charges.insert(attempt.to_owned(), (node.clone(), charged, unknown));
                }
                let succeeded = record["result"]["kind"] == "succeeded";
                // The latest settlement decides: a newer failure after an earlier success is a
                // failure, and a newer success after a failure is a success.
                track.ok = succeeded;
                track.failed = !succeeded;
            }
            Some("published" | "owned_children_completed") => {
                track.ok = true;
                track.failed = false;
            }
            _ => {}
        }
    }
    for (attempt, (node, charged, unknown)) in &settled_charges {
        let seen = observed.get(attempt).copied();
        let charged = (*charged).max(seen.unwrap_or(0));
        let track = tracks.entry(node.clone()).or_default();
        track.tokens = track.tokens.saturating_add(charged);
        // A usage observation makes its usage known, even one that reported 0 tokens.
        track.unknown += u64::from(*unknown && seen.is_none());
    }
    // The last whole-Round report of the current plan: every compiled node with its outcome,
    // including a failure before any Attempt and a suppressed branch.
    let mut outcomes: BTreeMap<&str, &str> = BTreeMap::new();
    // Why the report suppressed a node, where it did.
    let mut suppressed: BTreeMap<&str, &str> = BTreeMap::new();
    let current = document["plan_id"].as_str();
    let reports = array(&document["run_reports"]).iter().rev();
    let last = reports
        .map(|entry| &entry["report"])
        .find(|report| report["plan_id"].as_str() == current && report["phase_id"].is_null());
    let through = last.and_then(|report| report["through_sequence"].as_u64());
    for node in last.map(|report| array(&report["nodes"])).unwrap_or(&[]) {
        if let (Some(name), Some(kind)) = (node["node"].as_str(), node["outcome"]["kind"].as_str())
        {
            // A report speaks for a node only until a newer record of that node exists: a
            // retry that ran after it is what the stage shows.
            let newer = tracks
                .get(name)
                .is_some_and(|track| through.is_some_and(|through| track.last_sequence > through));
            if !newer {
                outcomes.insert(name, kind);
                if let Some(reason) = node["outcome"]["reason"].as_str() {
                    suppressed.insert(name, reason);
                }
            }
        }
    }
    let mut walls: BTreeMap<&str, u64> = BTreeMap::new();
    for wall in array(&document["attempt_walls"]) {
        // A wall of an earlier plan's Attempt is not this plan's stage time.
        let ours = wall["attempt_id"]
            .as_str()
            .is_some_and(|attempt| attempts.contains_key(attempt));
        if !ours {
            continue;
        }
        if let (Some(node), Some(ms)) = (wall["node_id"].as_str(), wall["elapsed_ms"].as_u64()) {
            let total = walls.entry(node).or_default();
            *total = total.saturating_add(ms);
        }
    }
    let empty = Track::default();
    let mut stages = Vec::new();
    for node in array(&document["graph"]["order"]) {
        let node = text(node)?;
        let track = tracks.get(node).unwrap_or(&empty);
        let outcome = outcomes.get(node).copied();
        // A report not superseded by newer records decides the stage over older records.
        let mark = if !finished && track.open {
            Mark::Running
        } else if outcome == Some("failed") {
            Mark::Failed
        } else if outcome == Some("suppressed") {
            Mark::Skipped
        } else if track.ok || outcome == Some("completed") {
            Mark::Ok
        } else if track.failed {
            Mark::Failed
        } else {
            Mark::NotReached
        };
        let allowance = &document["graph"]["allowances"][node]["max_attempts"];
        let exhausted = allowance.as_u64().is_some_and(|max| track.attempts >= max);
        let closed = match mark {
            Mark::Ok => true,
            // A branch not selected will not run; a node suppressed for a missing upstream runs
            // when the unfinished Task recovers its upstream, so it is not settled yet.
            Mark::Skipped => finished || suppressed.get(node) != Some(&"upstream_missing"),
            // A failed report of an unfinished Task does not close a stage the plan may run
            // again; only a finished Task or a spent allowance does.
            Mark::Failed => finished || exhausted,
            Mark::Running | Mark::NotReached => false,
        };
        stages.push(Stage {
            node: node.to_owned(),
            mark,
            closed,
            attempts: track.attempts,
            max_attempts: allowance.as_u64(),
            wall_ms: walls.get(node).copied(),
            tokens: (track.settled > 0).then_some(track.tokens),
            unknown: track.unknown,
        });
    }
    Ok(stages)
}

pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

pub(crate) fn text(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "a recorded field is not text".to_owned())
}

/// A token component totalled over `attempt_walls`: only when the walls cover every settled
/// Attempt and each carries the component, since a partial sum is not the Task's total.
fn component(document: &Value, key: &str) -> Option<u128> {
    let records = array(&document["execution_records"]);
    let settled = records
        .iter()
        .filter(|entry| entry["record"]["kind"] == "settled")
        .count();
    let walls = array(&document["attempt_walls"]);
    if walls.is_empty() || walls.len() != settled {
        return None;
    }
    let mut total: u128 = 0;
    for wall in walls {
        let value: u128 = wall["usage"][key].as_str()?.parse().ok()?;
        total = total.saturating_add(value);
    }
    Some(total)
}

/// The recorded runtime spans of one kind, summed; `None` when no span of the kind exists.
fn spans(document: &Value, kind: &str) -> Option<u64> {
    let mut total = None;
    for observation in array(&document["runtime_observations"]) {
        for span in array(&observation["record"]["spans"]) {
            if span["kind"] == kind
                && let Some(ms) = span["elapsed_ms"].as_u64()
            {
                total = Some(total.unwrap_or(0_u64).saturating_add(ms));
            }
        }
    }
    total
}

/// When the Task opened, and when it finished or else recorded its last event. A finished
/// Task still records delivery, adoption and lease events later; they are not its run time.
pub(crate) fn span_of(document: &Value) -> Option<(u64, u64)> {
    let history = array(&document["history"]);
    let at = |event: &Value| event["transition"]["now_unix_ms"].as_u64();
    let first = at(history.first()?)?;
    // A refreshed issue Task finishes again; its time ends at the result it shows now.
    let finished = history
        .iter()
        .rev()
        .find(|event| event["transition"]["change"]["kind"] == "finished");
    let end = at(finished.or(history.last())?)?;
    Some((first, end))
}

/// A duration as the pane prints it.
pub(crate) fn duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{}.{}s", ms / 1_000, ms % 1_000 / 100)
    } else if ms < 3_600_000 {
        format!("{}m {:02}s", ms / 60_000, ms / 1_000 % 60)
    } else {
        format!("{}h {:02}m", ms / 3_600_000, ms / 60_000 % 60)
    }
}

/// A Unix time in milliseconds as the UTC time of day.
pub(crate) fn clock(unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000 % 86_400;
    let (hours, minutes) = (seconds / 3_600, seconds / 60 % 60);
    format!("{hours:02}:{minutes:02}:{:02}Z", seconds % 60)
}

/// The first hex digits of an artifact ID, or `-`.
pub(crate) fn short(id: Option<&str>) -> String {
    match id {
        Some(id) => {
            let hex = id.strip_prefix("sha256:").unwrap_or(id);
            hex.chars().take(SHORT).collect()
        }
        None => "-".to_owned(),
    }
}

/// The TASK line in the main pane's width. The goal keeps at least 16 columns: the id is
/// cut first (it is also on the bar and in HISTORY), then the kind.
pub(crate) fn task_title(task_id: &str, kind: &str, goal: &str) -> String {
    const GOAL: usize = 16;
    let (mut task_id, mut kind) = (paint::ascii(task_id), paint::ascii(kind));
    // "TASK  " + id + "  " + kind + ": \"" + goal + "\""
    let frame = 6 + 2 + 3 + 1;
    let over = |id: &str, kind: &str| (frame + id.len() + kind.len() + GOAL).saturating_sub(MAIN);
    let cut = over(&task_id, &kind);
    if cut > 0 {
        task_id = clip(&task_id, task_id.len().saturating_sub(cut).max(8));
    }
    let cut = over(&task_id, &kind);
    if cut > 0 {
        kind = clip(&kind, kind.len().saturating_sub(cut).max(4));
    }
    let room = MAIN.saturating_sub(frame + task_id.len() + kind.len());
    let goal = clip(goal, room);
    format!("TASK  {task_id}  {kind}: \"{goal}\"")
}

/// A path in at most `width` columns, keeping its end, a cut start marked with `..`.
fn clip_left(path: &str, width: usize) -> String {
    let path = paint::ascii(path);
    if path.len() <= width {
        return path;
    }
    let keep = width.saturating_sub(2);
    format!("..{}", &path[path.len() - keep..])
}

/// `text` in at most `width` columns, a cut marked with `~`.
fn clip(text: &str, width: usize) -> String {
    let text = paint::ascii(text);
    if text.len() <= width {
        return text;
    }
    match width {
        0 => String::new(),
        _ => format!("{}~", &text[..width - 1]),
    }
}

/// A bar row, `task-id  outcome  progress%`, fitted to `width` columns with the progress
/// right-aligned in four, so every row's progress chip stands in one column. The Task id tells
/// the rows apart, so the outcome yields first: it is cut, and below three columns left out;
/// only then is the id cut.
pub(crate) fn fit(task_id: &str, outcome: &str, percent: &str, width: usize) -> String {
    let percent = format!("{percent:>4}");
    let room = width.saturating_sub(percent.len() + 2);
    let full = format!("{task_id}  {outcome}");
    let left = if full.len() <= room {
        full
    } else if room >= task_id.len() + 2 + 3 {
        format!("{task_id}  {}", clip(outcome, room - task_id.len() - 2))
    } else {
        clip(task_id, room.max(1))
    };
    format!("{left:<room$}  {percent}")
}

/// A stage's node without the root call and `nodes.` segments: `root.nodes.check` is `check`.
fn stage_name(node: &str) -> String {
    let node = node.strip_prefix("root.").unwrap_or(node);
    // A qualified path alternates the structural `nodes` marker with node ids
    // (`nodes.review.nodes.checks`); only a marker is dropped, so a node whose id is itself
    // `nodes` keeps its name.
    let mut parts = Vec::new();
    let mut marker = true;
    for part in node.split('.') {
        if marker && part == "nodes" {
            marker = false;
            continue;
        }
        parts.push(part);
        marker = true;
    }
    parts.join(".")
}

/// What stays true between reads of one Store: the node of an invocation or output artifact
/// never changes, and neither does a finished Task. Both are keyed by the Store's directory as
/// well, so one Store's reads never stand in for another's, even for a shared artifact id.
#[derive(Clone, Default)]
pub(crate) struct Cache {
    nodes: BTreeMap<(PathBuf, String), Invoked>,
    finished: BTreeMap<(PathBuf, String), Summary>,
}

/// The node an invocation ran and the plan it ran under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invoked {
    pub(crate) node: String,
    /// `None` only where a record names no invocation of its own.
    pub(crate) plan_id: Option<String>,
}

/// The node and plan of an invocation artifact, or of the invocation an output answers.
pub(crate) fn node_of(dir: &Path, id: &str, cache: &mut Cache) -> Result<Invoked, String> {
    let key = (dir.to_path_buf(), id.to_owned());
    if let Some(invoked) = cache.nodes.get(&key) {
        return Ok(invoked.clone());
    }
    let envelope = task_execution::recorded_artifact(dir, id)?;
    let payload = &envelope["payload"];
    let invoked = match (payload["node"].as_str(), payload["invocation_id"].as_str()) {
        (Some(node), _) => Invoked {
            node: node.to_owned(),
            plan_id: payload["plan_id"].as_str().map(str::to_owned),
        },
        (None, Some(invocation)) => node_of(dir, invocation, cache)?,
        (None, None) => return Err(format!("artifact {id} names no node")),
    };
    cache.nodes.insert(key, invoked.clone());
    Ok(invoked)
}

/// What the bar needs of one Task beyond its list entry.
#[derive(Clone, Debug)]
struct Summary {
    /// The phase the inspection read: the bar groups by it, so a Task that finished between
    /// the list read and its inspection is not left under the list read's phase.
    phase: Value,
    /// The outcome and charge from that same read, so a row never pairs a new group with an
    /// old outcome.
    outcome: Option<String>,
    chargeable: Option<String>,
    /// Attempts whose usage is unknown (ADR-0143), from that same read.
    unknown: u64,
    acceptance: Option<String>,
    /// The Task's current plan, which `r` confirms.
    plan_id: Option<String>,
    started: Option<u64>,
    progress: Progress,
}

fn summary(dir: &Path, task_id: &str, cache: &mut Cache) -> Result<Summary, String> {
    let document = task_execution::inspection_document(dir, task_id, true)?
        .ok_or_else(|| format!("Task {task_id} is not in {}", dir.display()))?;
    let stages = stages(&document, &mut |id| node_of(dir, id, cache))?;
    Ok(summary_of(&document, &stages))
}

/// A bar row's summary from one inspection read and the stages computed from it.
fn summary_of(document: &Value, stages: &[Stage]) -> Summary {
    Summary {
        phase: document["phase"].clone(),
        outcome: document["result"]["domain_conclusion"]
            .as_str()
            .map(str::to_owned),
        chargeable: match &document["chargeable_tokens"] {
            Value::String(n) => Some(n.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        },
        unknown: document["unknown_usage_attempts"].as_u64().unwrap_or(0),
        acceptance: document["result"]["acceptance"].as_str().map(str::to_owned),
        plan_id: document["plan_id"].as_str().map(str::to_owned),
        started: span_of(document).map(|(first, _)| first),
        progress: Progress::of(stages),
    }
}

/// One Task as `af task list` lists it.
struct Listed {
    task_id: String,
    /// The `af/task-list-entry@4` document.
    entry: Value,
    summary: Result<Summary, String>,
}

impl Listed {
    fn state(&self) -> State {
        match &self.summary {
            Ok(summary) => state_of(&summary.phase, summary.acceptance.as_deref()),
            Err(_) => state_of(&self.entry["phase"], None),
        }
    }

    fn outcome(&self) -> &str {
        if let Ok(summary) = &self.summary {
            // A Task without a result lists as `incomplete`, as `af task list` prints it.
            return summary.outcome.as_deref().unwrap_or("incomplete");
        }
        // Without its inspection, the outcome the list entry's state names.
        self.entry["state"]["outcome"]
            .as_str()
            .unwrap_or("incomplete")
    }

    fn percent(&self) -> String {
        match &self.summary {
            Ok(summary) => format!("{}%", summary.progress.percent()),
            Err(_) => "?%".to_owned(),
        }
    }
}

/// The `:` line `r` or `D` prefills for a Task, or why `D` refuses it; `None` for another key.
/// `r` confirms the plan the pane shows. `D` stops after `--confirm`: delivery asks for the Task
/// id again as its explicit confirmation (ADR-0031), so the user types it (ADR-0123).
/// A Store a command reaches only by naming it adds `--state DIR` after the Task id.
pub(crate) fn prefill(
    key: Key,
    task_id: &str,
    plan_id: Option<&str>,
    state: State,
    store: Option<&Path>,
) -> Option<Result<String, String>> {
    let mut id = shell_words::quote(task_id).into_owned();
    if let Some(store) = store {
        let store = store.display().to_string();
        id = format!("{id} --state {}", shell_words::quote(&store));
    }
    let line = match (key, state) {
        (Key::Char('r'), _) => match plan_id {
            Some(plan) => {
                let plan = shell_words::quote(plan);
                Ok(format!("task run {id} --confirm-plan {plan}"))
            }
            None => Err(format!("Task {task_id} records no plan to confirm")),
        },
        (Key::Char('D'), State::Done) => {
            let branch = shell_words::quote(&format!("af/{task_id}")).into_owned();
            let worktree = shell_words::quote(&format!("../{task_id}")).into_owned();
            Ok(format!(
                "task deliver {id} --branch {branch} --worktree {worktree} --confirm "
            ))
        }
        (Key::Char('D'), unverified) => {
            let why = match unverified {
                State::Running => "is still running",
                State::Awaiting => "is awaiting approval",
                State::Done | State::Failed => "finished without satisfying its acceptance",
            };
            Err(format!(
                "only a verified Task can be delivered: {task_id} {why}"
            ))
        }
        _ => return None,
    };
    Some(line)
}

/// One Task state directory, the group the bar lists it under, and what reading it found.
struct Store {
    dir: PathBuf,
    /// The directory as the pane prints it, `~` for the home directory.
    shown: String,
    /// The bar group: the user scope's opaque repository name, or an explicit Store's path.
    repo: Option<String>,
    origin: Origin,
    tasks: Result<Vec<Listed>, String>,
    /// Who wrote the Store, when its event log refused itself as another release's: set where
    /// the Store is read, from that read's own error and the Store's writer markers, never from
    /// text a path could carry.
    release: Option<review_core::store_writer::StoreProvenance>,
}

/// An existing directory with entries but no `events.sqlite` holds something the running
/// binary does not read as a Task Store; an absent or empty one holds no Tasks yet.
/// Why the pane refuses a Task state directory before reading it, if it does.
#[cfg(test)]
pub(crate) fn refusal_of(dir: &Path) -> Option<String> {
    not_a_store(dir)
}

/// A directory that exists but cannot be listed is refused too: it may hold anything.
pub(crate) fn not_a_store(dir: &Path) -> Option<String> {
    let unreadable = |error: std::io::Error| Some(format!("{}: {error}", dir.display()));
    if std::fs::symlink_metadata(dir).is_ok() && std::fs::metadata(dir).is_err() {
        return Some(format!("{} is a link to nothing", dir.display()));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => return unreadable(error),
    };
    let mut held = 0;
    for entry in entries {
        if let Err(error) = entry {
            return unreadable(error);
        }
        held += 1;
    }
    let store = dir.join("events.sqlite");
    (held > 0 && std::fs::symlink_metadata(&store).is_err())
        .then(|| format!("holds {held} entries but no events.sqlite"))
}

/// A scope's own Store (no `repo`) or a repository's default Store, read.
#[cfg(test)]
fn read_store(dir: &Path, shown: &str, repo: Option<String>, cache: &mut Cache) -> Store {
    let origin = match repo {
        Some(_) => Origin::Repository,
        None => Origin::Scope,
    };
    let target = Target {
        dir: dir.to_path_buf(),
        shown: shown.to_owned(),
        repo,
        origin,
    };
    read_target(&target, cache)
}

fn read_target(target: &Target, cache: &mut Cache) -> Store {
    let (dir, shown, repo) = (&target.dir, &target.shown, target.repo.clone());
    let (tasks, release) = match not_a_store(dir) {
        Some(refusal) => (Err(refusal), None),
        None => {
            let listed = task_execution::list_common(dir);
            let release = listed
                .as_ref()
                .err()
                .and_then(|error| crate::store_release::foreign(dir, error));
            (listed, release)
        }
    };
    let tasks = tasks.and_then(|entries| {
        let mut tasks = Vec::new();
        for entry in entries {
            // A collected Task (ADR-0135) has no artifacts left to inspect; `af task list`
            // and `af task show` report it, the browser shows the Tasks it can open.
            if entry.get("collected").is_some() {
                continue;
            }
            let task_id = text(&entry["task_id"])?.to_owned();
            let result = entry["phase"]["result_id"].as_str().map(str::to_owned);
            let key = result.as_ref().map(|id| (dir.to_path_buf(), id.clone()));
            let cached = key.as_ref().and_then(|key| cache.finished.get(key));
            let summary = match cached {
                Some(summary) => Ok(summary.clone()),
                // A Task the Store lists but cannot inspect is the Store's refusal, named with
                // the Task: it is never grouped by a summary it does not have.
                None => Ok(summary(dir, &task_id, cache)
                    .map_err(|error| format!("Task {task_id}: {error}"))?),
            };
            if let (Some(key), Ok(summary)) = (key, &summary) {
                cache.finished.insert(key, summary.clone());
            }
            tasks.push(Listed {
                task_id,
                entry,
                summary,
            });
        }
        // Newest first, by the time each Task opened.
        tasks.sort_by(|a, b| {
            let started = |task: &Listed| task.summary.as_ref().ok().and_then(|s| s.started);
            started(b)
                .cmp(&started(a))
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        Ok(tasks)
    });
    Store {
        dir: dir.to_path_buf(),
        shown: shown.to_owned(),
        repo,
        origin: target.origin,
        tasks,
        release,
    }
}

/// One opened Task.
struct Detail {
    dir: PathBuf,
    task_id: String,
    /// The `af task explain --json` document: `af task show --json` with `plan` and `graph`.
    document: Value,
    kind: String,
    goal: String,
    stages: Vec<Stage>,
}

impl Detail {
    fn read(dir: &Path, task_id: &str, cache: &mut Cache) -> Result<Detail, String> {
        let document = task_execution::inspection_document(dir, task_id, true)?
            .ok_or_else(|| format!("Task {task_id} is no longer in {}", dir.display()))?;
        let revision = text(&document["revision_id"])?;
        let revision = task_execution::recorded_artifact(dir, revision)?;
        let stages = stages(&document, &mut |id| node_of(dir, id, cache))?;
        Ok(Detail {
            dir: dir.to_path_buf(),
            task_id: task_id.to_owned(),
            kind: text(&revision["payload"]["kind"])?.to_owned(),
            goal: text(&revision["payload"]["goal"])?.to_owned(),
            document,
            stages,
        })
    }

    fn state(&self) -> State {
        let acceptance = self.document["result"]["acceptance"].as_str();
        state_of(&self.document["phase"], acceptance)
    }

    /// The Pipeline the plan's root call runs.
    fn pipeline(&self) -> Option<&str> {
        self.document["graph"]["calls"]["root"]["pipeline"].as_str()
    }

    /// The pane's rows, and the artifact behind each HISTORY row.
    fn rows(&self, now_ms: u64) -> (Vec<Row>, BTreeMap<usize, String>) {
        let document = &self.document;
        let state = self.state();
        let mut rows = Vec::new();
        let title = task_title(&self.task_id, &self.kind, &self.goal);
        rows.push(Row::painted(title, Paint::Title));
        let plan = &document["plan"];
        let origin = if plan.is_null() {
            "-"
        } else if !plan["preparation"].is_null() {
            "planner preparation"
        } else if !array(&plan["generated_origins"]).is_empty() {
            "generated"
        } else {
            "configured"
        };
        let lead = format!(
            "PLAN  {}  {origin}  STATE",
            short(document["plan_id"].as_str())
        );
        // The chip takes the spaces either side of the state word.
        let mut line = String::new();
        let span = span_of(document);
        let elapsed = span.map(|(first, last)| match state {
            State::Done | State::Failed => last.saturating_sub(first),
            State::Running | State::Awaiting => now_ms.saturating_sub(first),
        });
        if let (Some((first, _)), Some(elapsed)) = (span, elapsed) {
            line.push_str(&format!(" started {}", clock(first)));
            line.push_str(&format!("  elapsed {}", duration(elapsed)));
        }
        rows.push(Row {
            spans: vec![
                Span::new(lead, Paint::Plain),
                Span::chip(state.word(), state.tone()),
                Span::new(line, Paint::Plain),
            ],
        });
        rows.push(Row::plain(format!(
            "SNAP  source {}  derived {}  policy {}",
            short(plan["inputs"]["source"]["snapshot_id"].as_str()),
            short(document["result"]["outputs"]["snapshot"]["snapshot_id"].as_str()),
            short(plan["authority"]["policy_id"].as_str()),
        )));
        rows.push(Row::blank());
        let progress = Progress::of(&self.stages);
        let (settled, count) = (progress.settled, progress.stages);
        rows.push(Row::painted(
            format!("PROGRESS  {settled} / {count} stages"),
            Paint::Title,
        ));
        for stage in &self.stages {
            rows.push(stage_row(stage));
        }
        rows.push(Row::blank());
        let tokens = |key: &str| component(document, key).map_or("-".to_owned(), |n| n.to_string());
        let chargeable = document["chargeable_tokens"].as_str().unwrap_or("-");
        let chargeable = format!(
            "{chargeable}{}",
            unknown_suffix(document["unknown_usage_attempts"].as_u64().unwrap_or(0))
        );
        rows.push(Row::plain(format!(
            "TOKENS  chargeable {chargeable}  input {}  output {}  cache read {}  reasoning {}",
            tokens("input_tokens"),
            tokens("output_tokens"),
            tokens("cache_read_tokens"),
            tokens("reasoning_tokens"),
        )));
        let spent = |kind: &str| spans(document, kind).map_or("-".to_owned(), duration);
        rows.push(Row::plain(format!(
            "TIME  wall {}  checks {}  dependency prep {}",
            elapsed.map_or("-".to_owned(), duration),
            spent("check"),
            spent("dependency_preparation"),
        )));
        rows.push(Row::blank());
        rows.push(Row::painted("HISTORY  (af task show)", Paint::Title));
        let mut kinds: BTreeMap<&str, &str> = BTreeMap::new();
        for entry in array(&document["execution_records"]) {
            if let (Some(id), Some(kind)) = (
                entry["artifact_id"].as_str(),
                entry["record"]["kind"].as_str(),
            ) {
                kinds.insert(id, kind);
            }
        }
        let mut artifacts = BTreeMap::new();
        for event in array(&document["history"]) {
            let change = &event["transition"]["change"];
            let mut what = change["kind"].as_str().unwrap_or("-").to_owned();
            let id = change_artifact(change);
            if let Some(kind) = id.and_then(|id| kinds.get(id)) {
                what = format!("{what} {kind}");
            }
            let sequence = event["sequence"]
                .as_u64()
                .map_or("-".into(), |n| n.to_string());
            if let Some(id) = id {
                artifacts.insert(rows.len(), id.to_owned());
            }
            rows.push(Row::plain(format!(
                "  {sequence:>4}  {what:<34}  {}",
                short(id)
            )));
        }
        (rows, artifacts)
    }
}

/// The recorded artifact a Task change is about, by its kind: a revocation, not the decision
/// it revokes; a report, not an event id. A change about no artifact names none.
pub(crate) fn change_artifact(change: &Value) -> Option<&str> {
    let field = match change["kind"].as_str()? {
        "recording_resumed" | "review_integration_finished" | "run_reported" => "report_id",
        "review_integration_selected" => "phase_id",
        "review_continued" => "handoff_id",
        "adoption_observation_recorded" => "observation_id",
        "opened" | "source_refreshed" => "revision_id",
        "plan_proposed" | "plan_admitted" | "planning_completed" => "plan_id",
        "plan_decided" => "decision_id",
        "approval_revoked" => "revocation_id",
        "finished" => "result_id",
        "execution_recorded" | "delivery_recorded" => "record_id",
        _ => return None,
    };
    change[field]
        .as_str()
        .filter(|id| review_core::is_digest(id))
}

/// ` (+N unknown)` after a charged total when N Attempts' usage is unknown, so the total never
/// reads as their spend (ADR-0143); empty when every usage is known.
pub(crate) fn unknown_suffix(unknown: u64) -> String {
    if unknown == 0 {
        String::new()
    } else {
        format!(" (+{unknown} unknown)")
    }
}

fn stage_row(stage: &Stage) -> Row {
    let name = clip(&stage_name(&stage.node), STAGE);
    let wall = stage.wall_ms.map_or(String::new(), duration);
    let tokens = stage.tokens.map_or(String::new(), |n| {
        format!("{n} tok{}", unknown_suffix(stage.unknown))
    });
    let bound = stage
        .max_attempts
        .map_or("-".to_owned(), |max| max.to_string());
    let attempt = format!("attempt {}/{bound}", stage.attempts);
    let note = match stage.mark {
        Mark::Running => format!("running, {attempt}"),
        Mark::Failed if stage.attempts > 0 => format!("failed, {attempt}"),
        Mark::Failed => "failed".to_owned(),
        Mark::Skipped => "skipped".to_owned(),
        Mark::Ok | Mark::NotReached => String::new(),
    };
    let line = format!("  {name:<STAGE$} {wall:>7} {tokens:>12}  {note}");
    let (mark, paint) = match stage.mark {
        Mark::Ok => (Paint::Chip(Tone::Ok), Paint::Plain),
        Mark::Running => (Paint::Chip(Tone::Active), Paint::Plain),
        Mark::Failed => (Paint::Chip(Tone::Fail), Paint::Plain),
        Mark::NotReached | Mark::Skipped => (Paint::Muted, Paint::Muted),
    };
    Row {
        spans: vec![
            Span::new("  ", paint),
            Span::new(stage.mark.text(), mark),
            Span::new(line.trim_end(), paint),
        ],
    }
}

/// `r` or `D` for a Task of `store`. A Store a command reaches only by naming it gets `--state
/// DIR`. The user scope lists every repository's default Store, but a handed-off command runs
/// from the home directory and names a Task only by its id, and the pane knows its repository
/// only by the opaque state-directory name: no line it could prefill would reach that Task, so
/// it says where to press the key instead.
fn verb(
    store: &Store,
    key: Key,
    task_id: &str,
    plan: Option<&str>,
    state: State,
) -> Option<Result<String, String>> {
    // Asked first: whatever the Task's plan or state, from here no line reaches it.
    if store.origin == Origin::Repository
        && let Key::Char(pressed @ ('r' | 'D')) = key
    {
        return Some(Err(format!(
            "{task_id} is listed from the user scope, which cannot name its repository: \
             :cd into that repository and press {pressed} there"
        )));
    }
    let named = store.origin.explicit().then_some(store.dir.as_path());
    prefill(key, task_id, plan, state, named)
}

/// One message of a read: a Store as soon as it is read, then the end of the read.
enum Read {
    /// The user scope's targets, discovered on the read's thread: every repository's Store
    /// under the state root, then the explicit targets that are no repository's. The Stores
    /// after this message are theirs, in this order. An error names the state root that cannot
    /// be listed; the explicit targets are read all the same.
    Targets(Result<Vec<Target>, String>),
    /// One target's Store, read; with the opened Task's detail when this is its Store, read
    /// from the same Store state as its row, so the bar and the main pane never disagree
    /// about it.
    Store {
        store: Box<Store>,
        detail: Option<Result<Detail, String>>,
    },
    /// Every target was read; the cache the read filled.
    Done(Cache),
}

/// Read every Store, and the opened Task again, on the calling thread: each Store goes through
/// `sender` the moment it is read, the cache last. In the user scope the repositories' Stores
/// are discovered first, here, never by the load (ADR-0240); `explicit` are the targets the
/// load resolved. A receiver gone away ends the read early, since nobody waits for the rest.
fn read(
    discover: Option<Discovery>,
    explicit: Vec<Target>,
    opened: Option<(PathBuf, String)>,
    mut cache: Cache,
    sender: &mpsc::Sender<Read>,
) {
    let targets = match discover {
        None => explicit,
        Some(discovery) => match discovery.targets() {
            Ok(mut found) => {
                let explicit: Vec<Target> = explicit
                    .into_iter()
                    .filter(|target| !found.iter().any(|repo| same_store(&repo.dir, &target.dir)))
                    .collect();
                found.extend(explicit);
                if sender.send(Read::Targets(Ok(found.clone()))).is_err() {
                    return;
                }
                found
            }
            Err(error) => {
                if sender.send(Read::Targets(Err(error))).is_err() {
                    return;
                }
                explicit
            }
        },
    };
    for target in &targets {
        let mut store = read_target(target, &mut cache);
        let detail = match &opened {
            Some((dir, task_id)) if *dir == store.dir => {
                let detail = Detail::read(dir, task_id, &mut cache);
                if let Err(error) = &detail {
                    refuse(std::slice::from_mut(&mut store), dir, task_id, error);
                }
                Some(detail)
            }
            _ => None,
        };
        let read = Read::Store {
            store: Box::new(store),
            detail,
        };
        if sender.send(read).is_err() {
            return;
        }
    }
    let _ = sender.send(Read::Done(cache));
}

/// A Task a Store lists but cannot be inspected refuses the whole Store: no other Task of it
/// is shown as if the Store were readable.
fn refuse(stores: &mut [Store], dir: &Path, task_id: &str, error: &str) {
    for store in stores.iter_mut().filter(|store| store.dir == dir) {
        store.tasks = Err(format!("Task {task_id}: {error}"));
        store.release = None;
    }
}

/// Where a Task Store the pane reads comes from, which decides how a command reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Origin {
    /// The project scope's own Store: a command run in the scope's root resolves it.
    Scope,
    /// A repository's default Store the user scope lists by its opaque name: no command run
    /// from the home directory names it.
    Repository,
    /// A `--state DIR` Store the Store registry (`stores.toml`) records.
    Registered,
    /// A Store `:state DIR` listed for this session.
    Session,
}

impl Origin {
    /// Whether a command reaches the Store only with `--state DIR`.
    fn explicit(self) -> bool {
        matches!(self, Origin::Registered | Origin::Session)
    }
}

/// One Task state directory a scope reads.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Target {
    pub(crate) dir: PathBuf,
    pub(crate) shown: String,
    /// The group the bar lists it under: the user scope's opaque repository name, or an
    /// explicit Store's path.
    repo: Option<String>,
    origin: Origin,
}

/// The Task state directories a scope resolves on the calling thread, or why it names none:
/// the project scope's own Store, the directory `af task` resolves for the repository without
/// `--state`. The user scope reads every repository's Store, a walk of the state root that the
/// read makes on its thread (`Discovery`), never a load: the user scope resolves none here.
pub(crate) fn targets(scope: &Scope) -> Result<Vec<Target>, String> {
    let Some(root) = &scope.state else {
        return Err("no Task state: neither XDG_STATE_HOME nor HOME is set".to_owned());
    };
    let Some(toplevel) = scope.toplevel() else {
        return Ok(Vec::new());
    };
    let repo = std::fs::canonicalize(toplevel)
        .map_err(|error| format!("{}: {error}", toplevel.display()))?;
    let dir = task_execution::default_task_state(root, &repo)?;
    let shown = scope.abbreviate(&dir);
    Ok(vec![Target {
        dir,
        shown,
        repo: None,
        origin: Origin::Scope,
    }])
}

/// The user scope's state root, whose repositories' Stores the read discovers on its thread:
/// a directory walk, with a `metadata` call behind every symlink, that no load makes.
#[derive(Clone)]
struct Discovery {
    root: PathBuf,
    /// The directory paths are abbreviated against, as the scope shows them.
    home: Option<PathBuf>,
}

impl Discovery {
    /// `None` in a project scope, whose one target `targets` resolves.
    fn of(scope: &Scope) -> Option<Discovery> {
        if scope.toplevel().is_some() {
            return None;
        }
        scope.state.clone().map(|root| Discovery {
            root,
            home: scope.home.clone(),
        })
    }

    /// Every repository's Store under the root, as the user scope lists them.
    fn targets(&self) -> Result<Vec<Target>, String> {
        let mut targets = Vec::new();
        for (dir, name) in user_targets(&self.root)? {
            let shown = crate::tui::scope::abbreviate(self.home.as_deref(), &dir);
            targets.push(Target {
                dir,
                shown,
                repo: Some(name),
                origin: Origin::Repository,
            });
        }
        Ok(targets)
    }
}

/// The Task Stores beyond the scope's own: every `--state DIR` Store the Store registry records
/// (ADR-0144), then the ones `:state DIR` added, each once and never one `listed` already holds.
/// The second value says why the registry added none, when it cannot be read.
fn explicit_targets(
    scope: &Scope,
    session: &[PathBuf],
    listed: &[Target],
) -> (Vec<Target>, Option<String>) {
    let mut warning = None;
    let mut found: Vec<(PathBuf, Origin)> = Vec::new();
    if let Some(state) = &scope.state {
        let registry = crate::storage::registry::location(state);
        match crate::storage::registry::read(&registry) {
            Ok(entries) => {
                for entry in entries {
                    if entry.kind == crate::storage::registry::StoreKind::Task {
                        found.push((entry.path, Origin::Registered));
                    }
                }
            }
            Err(error) => {
                warning = Some(format!(
                    "the Store registry cannot be read, so its Task Stores are not listed: {error}"
                ));
            }
        }
    }
    found.extend(session.iter().map(|dir| (dir.clone(), Origin::Session)));
    let mut targets: Vec<Target> = Vec::new();
    for (dir, origin) in found {
        let known = listed
            .iter()
            .chain(targets.iter())
            .any(|t| same_store(&t.dir, &dir));
        if known {
            continue;
        }
        let shown = scope.abbreviate(&dir);
        targets.push(Target {
            repo: Some(shown.clone()),
            dir,
            shown,
            origin,
        });
    }
    (targets, warning)
}

/// Whether two Store paths name one Store: the same path, or two that both resolve to one
/// directory. Two paths that cannot be resolved (each missing or unreadable) are two Stores,
/// each listed with its own refusal.
fn same_store(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Every repository's Task state directory under the XDG state root, sorted, with its opaque
/// name. A symlink to a Store is read through it, as `af task` reads the path; a link to
/// nothing is still listed, so the pane shows its refusal rather than skipping it.
pub(crate) fn user_targets(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let local = task_execution::local_task_states(root);
    let entries = match std::fs::read_dir(&local) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", local.display())),
    };
    let mut targets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", local.display()))?;
        let kind = entry.file_type().ok();
        let is_dir = kind.is_some_and(|kind| kind.is_dir())
            || (kind.is_some_and(|kind| kind.is_symlink())
                && std::fs::metadata(entry.path()).map_or(true, |meta| meta.is_dir()));
        if is_dir {
            let name = entry.file_name().to_string_lossy().into_owned();
            targets.push((entry.path(), name));
        }
    }
    targets.sort();
    Ok(targets)
}

/// An artifact opened from a HISTORY row.
struct Opened {
    rows: Vec<Row>,
    /// The HISTORY row `q` returns to.
    row: usize,
}

struct Job {
    receiver: Receiver<Read>,
    /// The Task the read was started for: its detail is dropped when another Task is opened
    /// meanwhile; the Stores are listed either way.
    for_task: Option<String>,
    /// A load or `R` started it: its Stores are wanted whether or not the pane shows, so
    /// closing the pane or opening a Task keeps it. A live reread is dropped by both.
    explicit: bool,
    /// The opening the read was started under: a detail read under an earlier opening of
    /// the Task is dropped, and the Task inspected again when its Store answers.
    opening: u64,
}

#[derive(Default)]
pub(crate) struct TasksPane {
    /// The folder pane's title.
    title: String,
    /// The scope loaded last, by what makes it another one: another scope lists nothing of
    /// the old; the same scope with other targets keeps its Stores until the read answers.
    place: Option<(ScopeKind, PathBuf)>,
    /// The targets the load resolved: the scope's own Store, the registered Stores and the
    /// session's. What a read starts from; in the user scope, what it adds the discovered
    /// repositories' Stores to, so a Store discovered once is never a target of the load's.
    loaded: Vec<Target>,
    /// The targets as the last read answered them (`loaded`, and the discovered Stores in
    /// the user scope): the order the Stores stand in, and what `Done` prunes them by.
    targets: Vec<Target>,
    /// The user scope's state root, whose repositories' Stores the read discovers.
    discover: Option<Discovery>,
    /// Why the scope names no Task state.
    error: Option<String>,
    /// Why the Store registry added no Store.
    warning: Option<String>,
    /// The Stores `:state DIR` added; they stay listed while the browser runs.
    session: Vec<PathBuf>,
    stores: Vec<Store>,
    cache: Cache,
    /// The bar id of the opened Task.
    selected: Option<String>,
    detail: Option<Result<Detail, String>>,
    artifact: Option<Opened>,
    /// The main pane shows this pane.
    opened: bool,
    job: Option<Job>,
    /// An explicit read `read_now` recorded, which the next `poll` starts.
    pending: bool,
    /// How many times a Task was opened: a read's detail is taken only from a read started
    /// under the opening that shows now.
    opening: u64,
    next: Option<Instant>,
    /// The spinner frame the bar shows while a read runs.
    spinner: usize,
    /// Live reads started, for tests.
    #[cfg(test)]
    reads: usize,
    /// Reads started on a thread, for tests: a load records one and starts none.
    #[cfg(test)]
    started: usize,
    rows: Vec<Row>,
    history: BTreeMap<usize, String>,
}

impl TasksPane {
    /// The bar id of a Task, a group or an unreadable Store.
    fn id(store: &Store, name: &str) -> String {
        TasksPane::id_of(&store.repo, name)
    }

    fn id_of(repo: &Option<String>, name: &str) -> String {
        match repo {
            Some(repo) => format!("{repo}/{name}"),
            None => name.to_owned(),
        }
    }

    /// The Store and Task a bar id names.
    fn task(&self, id: &str) -> Option<(&Store, &Listed)> {
        for store in &self.stores {
            for task in store.tasks.iter().flatten() {
                if TasksPane::id(store, &task.task_id) == id {
                    return Some((store, task));
                }
            }
        }
        None
    }

    /// The Task id behind a bar id, for `y` on the bar.
    pub(crate) fn task_id(&self, id: &str) -> Option<String> {
        self.task(id).map(|(_, task)| task.task_id.clone())
    }

    /// `r` or `D` on the bar's Task `id`: the line to prefill, or why not; `None` when `id`
    /// is no Task or the key is neither.
    pub(crate) fn bar_verb(&self, id: &str, key: Key) -> Option<Result<String, String>> {
        let (store, task) = self.task(id)?;
        let summary = task.summary.as_ref().ok();
        let plan = summary.and_then(|summary| summary.plan_id.as_deref());
        verb(store, key, &task.task_id, plan, task.state())
    }

    /// The Store holding the Task opened from `dir`.
    fn store_at(&self, dir: &Path) -> Option<&Store> {
        self.stores.iter().find(|store| store.dir == dir)
    }

    /// `:state DIR`: list the Store `dir` for the rest of the session, and read every Store
    /// again; `false` when the pane already lists it.
    pub(crate) fn add_session_store(&mut self, dir: PathBuf, scope: &Scope) -> bool {
        let same = |a: &Path| a == dir || std::fs::canonicalize(a).ok().as_deref() == Some(&dir);
        if self.session.iter().any(|known| same(known))
            || self.targets.iter().any(|target| same(&target.dir))
        {
            return false;
        }
        self.session.push(dir);
        let _ = self.load(scope);
        true
    }

    /// Every Task id this scope lists that a handed-off command can resolve, once each and
    /// sorted: what the `:` line completes a Task ID argument from. That is the project
    /// scope's own Store; a Task of another Store needs `--state DIR` or another repository.
    pub(crate) fn task_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for store in self.stores.iter().filter(|s| s.origin == Origin::Scope) {
            for task in store.tasks.iter().flatten() {
                ids.push(task.task_id.clone());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// Live reads started so far.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.reads
    }

    /// The opened Task's state, as a test drives a running one.
    #[cfg(test)]
    pub(crate) fn detail_document(&mut self) -> Option<&mut Value> {
        match &mut self.detail {
            Some(Ok(detail)) => Some(&mut detail.document),
            _ => None,
        }
    }

    fn opened_task(&self) -> Option<(PathBuf, String)> {
        let id = self.selected.as_deref()?;
        let (store, task) = self.task(id)?;
        Some((store.dir.clone(), task.task_id.clone()))
    }

    /// A load or `R`: every listed Task is inspected again, on a thread, each Store listed as
    /// it is read. The read is recorded here and started at the next `poll`, which the event
    /// loop reaches after the frame that follows: no Store is read before that frame, the
    /// browser's first after a load (ADR-0240). A read in flight is dropped. Only the live
    /// read of a running Task reuses finished summaries; an explicit read never trusts them,
    /// so a Task that can no longer be inspected refuses its Store.
    fn read_now(&mut self) {
        self.cache.finished.clear();
        self.cache.nodes.clear();
        self.job = None;
        self.pending = true;
    }

    /// A Store refused on the key loop, for the opened Task: it cannot be inspected, or an
    /// artifact of its HISTORY cannot be given back. The folder shows the refusal, not the
    /// Task. A read in flight may still deliver this Store as it stood before the refusal and
    /// list the Task again over it: the read starts over, so whatever lands was read after the
    /// refusal and repeats it.
    fn refuse_opened(&mut self, dir: &Path, task_id: &str, error: &str) {
        refuse(&mut self.stores, dir, task_id, error);
        self.selected = None;
        self.detail = None;
        self.artifact = None;
        if self.pending || self.job.is_some() {
            self.read_now();
        }
    }

    /// Start a read of every target on a thread; a read in flight is dropped, its thread ends
    /// at its next send. Only `poll` starts a read, the explicit one `read_now` recorded or a
    /// live one, so no read begins before the frame painted last: the splash waits for the
    /// scope only, and no Store is read before the browser's first frame (ADR-0240). Nothing
    /// interrupts the read: it holds no key, and `q` quits without waiting for it.
    fn start(&mut self, explicit: bool) {
        self.pending = false;
        #[cfg(test)]
        {
            self.started += 1;
        }
        let (discover, targets) = (self.discover.clone(), self.loaded.clone());
        let (opened, cache) = (self.opened_task(), self.cache.clone());
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || read(discover, targets, opened, cache, &sender));
        self.job = Some(Job {
            receiver,
            for_task: self.selected.clone(),
            explicit,
            opening: self.opening,
        });
    }

    /// Whether the read in flight, if any, was started by a load or `R`, or one is recorded.
    fn explicit_read(&self) -> bool {
        self.pending || self.job.as_ref().is_some_and(|job| job.explicit)
    }

    /// One message of the read in flight: the detail is for the Task the read was started
    /// for, and dropped when another was opened meanwhile; a Store is listed either way.
    fn collect(&mut self, read: Read) {
        let current = self
            .job
            .as_ref()
            .is_some_and(|job| job.for_task == self.selected && job.opening == self.opening);
        let read = match read {
            Read::Store {
                store,
                detail: Some(_),
            } if !current => Read::Store {
                store,
                detail: None,
            },
            other => other,
        };
        self.apply(read);
    }

    /// Wait for the read in flight, if any, and list everything it finds: a test's event loop.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        if self.pending {
            self.start(true);
        }
        loop {
            let received = match &self.job {
                Some(job) => job.receiver.recv(),
                None => return,
            };
            match received {
                Ok(read) => self.collect(read),
                Err(_) => self.job = None,
            }
        }
    }

    /// How many reads were started on a thread: a load records one and starts none.
    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.started
    }

    /// A read in flight whose messages a test sends by hand, for the Task opened now.
    #[cfg(test)]
    fn fed(&mut self) -> mpsc::Sender<Read> {
        let (sender, receiver) = mpsc::channel();
        self.pending = false;
        self.job = Some(Job {
            receiver,
            for_task: self.selected.clone(),
            explicit: true,
            opening: self.opening,
        });
        sender
    }

    /// A target's Store, read: over what the last read found for it, the Stores in target
    /// order. A Store listed for a target this read no longer names stays, last, until the
    /// read is done (`Done` drops it): a changed target list never unlists a Task before its
    /// own Store is read, and an opened one stays opened meanwhile.
    fn place(&mut self, store: Store) {
        match self.stores.iter().position(|s| s.dir == store.dir) {
            Some(listed) => self.stores[listed] = store,
            None => self.stores.push(store),
        }
        self.order();
    }

    /// The Stores in the order of their targets; one of no target last.
    fn order(&mut self) {
        let targets = &self.targets;
        let order = |dir: &Path| {
            targets
                .iter()
                .position(|target| target.dir == dir)
                .unwrap_or(usize::MAX)
        };
        self.stores.sort_by_key(|store| order(&store.dir));
    }

    /// The opened Task's bar row, rebuilt from the same read as its detail, so the bar and the
    /// main pane never disagree about its state, outcome, progress or charge.
    fn sync_selected_row(&mut self) {
        let (Some(id), Some(Ok(detail))) = (self.selected.clone(), &self.detail) else {
            return;
        };
        let summary = summary_of(&detail.document, &detail.stages);
        for store in &mut self.stores {
            let named = |task: &&mut Listed| TasksPane::id_of(&store.repo, &task.task_id) == id;
            if let Ok(tasks) = &mut store.tasks
                && let Some(task) = tasks.iter_mut().find(named)
            {
                task.summary = Ok(summary.clone());
            }
        }
    }

    fn apply(&mut self, read: Read) {
        match read {
            // The targets as this read answers them: the Stores retained from the last read
            // follow their order at once, before the read answers for any of them.
            Read::Targets(Ok(targets)) => {
                self.targets = targets;
                self.order();
                self.error = None;
            }
            Read::Targets(Err(error)) => {
                // The walk failed: this read answers the load's targets alone, so the Stores
                // discovered before go when it is done, and the folder shows why.
                self.targets = self.loaded.clone();
                self.order();
                self.error = Some(error);
            }
            Read::Store { store, detail } => {
                let mut store = *store;
                let detail = match detail {
                    None => self.detail_of_opened(&mut store),
                    detail => detail,
                };
                self.place(store);
                match detail {
                    // The Store was refused with the cause; the folder shows it, not the Task.
                    Some(Err(_)) => {
                        self.detail = None;
                        self.selected = None;
                        self.artifact = None;
                    }
                    Some(detail) => self.detail = Some(detail),
                    None => {}
                }
            }
            Read::Done(cache) => {
                // Every target answered: a Store of a target this read did not name goes.
                let targets = &self.targets;
                self.stores
                    .retain(|store| targets.iter().any(|target| target.dir == store.dir));
                self.order();
                self.cache = cache;
                self.job = None;
            }
        }
        // A Store refused as a whole takes its opened Task with it, even when that one Task
        // could still be inspected by path: the folder shows the refusal instead.
        if let Some(id) = self.selected.clone()
            && self.task(&id).is_none()
        {
            self.selected = None;
            self.detail = None;
            // An artifact opened from its HISTORY goes too, so the refusal is what shows.
            self.artifact = None;
        }
        self.sync_selected_row();
        self.rebuild();
    }

    /// The opened Task's detail, read here when its Store answers a read that carries none
    /// for it (a read started before the Task was opened, for another Task or for none):
    /// the detail the Task was opened with must not stand over the Store's newer summary.
    /// One Task's inspection, as `open` does it; the Store refuses when it fails, as it does
    /// on the reader's thread. `None` for any other Store.
    fn detail_of_opened(&mut self, store: &mut Store) -> Option<Result<Detail, String>> {
        let (dir, task_id) = self.opened_task()?;
        if dir != store.dir {
            return None;
        }
        let detail = Detail::read(&dir, &task_id, &mut self.cache);
        if let Err(error) = &detail {
            refuse(std::slice::from_mut(store), &dir, &task_id, error);
        }
        Some(detail)
    }

    /// Whether the opened Task is unfinished, so the pane reads it again: a Task awaiting
    /// approval may start running at any moment from the CLI.
    fn live(&self) -> bool {
        let unfinished = matches!(&self.detail,
            Some(Ok(detail)) if matches!(detail.state(), State::Running | State::Awaiting));
        self.opened && unfinished
    }

    fn rebuild(&mut self) {
        self.history.clear();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let now = u64::try_from(now).unwrap_or(u64::MAX);
        self.rows = match (&self.selected, &self.detail) {
            (Some(_), Some(Ok(detail))) => {
                let (rows, history) = detail.rows(now);
                self.history = history;
                rows
            }
            (Some(id), Some(Err(error))) => vec![
                Row::painted(format!("TASK  {id}"), Paint::Title),
                Row::blank(),
                Row::error("error", format!("cannot be read: {error}")),
            ],
            _ => self.folder_rows(),
        };
    }

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted(&self.title, Paint::Title), Row::blank()];
        if let Some(error) = &self.error {
            rows.push(Row::error("error", error));
        } else if self.stores.is_empty() {
            rows.push(Row::plain("No Task state is recorded here yet."));
        }
        if let Some(warning) = &self.warning {
            rows.extend(Row::errors("warning", bounded(warning, MAIN - 10, "")));
            rows.push(Row::blank());
        }
        rows.extend(self.attention_rows());
        let earlier_stores: Vec<&Store> = self.stores.iter().filter(|s| earlier(s)).collect();
        if let Some(first) = earlier_stores.first() {
            let count = earlier_stores.len();
            let parent = first
                .shown
                .rsplit_once('/')
                .map_or(first.shown.as_str(), |(parent, _)| parent);
            // One row for all of them, by the releases that wrote them, with the command that
            // runs them; short lines, as the main pane is MAIN columns beside the bar.
            let summary = crate::store_release::summary(
                earlier_stores
                    .iter()
                    .filter_map(|store| store.release.as_ref()),
                "af",
            )
            .unwrap_or_default();
            for line in summary.lines() {
                // A line of the summary keeps its indent on each row it wraps to.
                let text = line.trim_start();
                let indent = &line[..line.len() - text.len()];
                for row in bounded(text, MAIN, indent) {
                    rows.push(Row::painted(row, Paint::Muted));
                }
            }
            let (stores, they) = if count == 1 {
                ("Task Store", "It is")
            } else {
                ("Task Stores", "They are")
            };
            rows.push(Row::painted(format!("{they} {stores} in"), Paint::Muted));
            for line in bounded(parent, MAIN, "  ") {
                rows.push(Row::painted(line, Paint::Muted));
            }
            rows.push(Row::blank());
        }
        for store in self.stores.iter().filter(|s| !earlier(s)) {
            // Each Store under its path, and how an explicit one came to be listed.
            let dir = &match store.origin {
                Origin::Scope | Origin::Repository => store.shown.clone(),
                Origin::Registered => format!("{}  (--state, in stores.toml)", store.shown),
                Origin::Session => format!("{}  (:state, this session)", store.shown),
            };
            match &store.tasks {
                Err(error) => {
                    // The location, then the cause on rows of its own: a long state path must
                    // never push the cause out of the pane.
                    for line in bounded(&format!("{dir}:"), MAIN, "") {
                        rows.push(Row::painted(line, Paint::Error));
                    }
                    let cause = format!("this Store cannot be read: {error}");
                    let chip = "error";
                    let lines = bounded(&cause, MAIN - (chip.len() + 3), "");
                    rows.extend(Row::errors(chip, lines));
                }
                Ok(tasks) if tasks.is_empty() => {
                    rows.push(Row::plain(format!("{dir}: no Tasks")));
                }
                Ok(tasks) => {
                    rows.push(Row::plain(dir));
                    for state in State::ALL {
                        let listed: Vec<&Listed> =
                            tasks.iter().filter(|task| task.state() == state).collect();
                        if listed.is_empty() {
                            continue;
                        }
                        rows.push(Row::painted(format!("  {}/", state.word()), Paint::Title));
                        for task in listed {
                            rows.push(folder_row(task));
                        }
                    }
                }
            }
            rows.push(Row::blank());
        }
        rows
    }

    /// The attention view at the top of the folder, as `af task list --attention` groups the
    /// same list entries: each group that waits for a person, its Tasks by bar id and state
    /// with the next command each names, then the running Tasks and how many need nothing.
    /// Read from each entry's `state` alone; empty when no readable Store lists a Task.
    fn attention_rows(&self) -> Vec<Row> {
        use review_core::task::state::TaskAttentionV1;
        type Entry<'a> = (&'a Store, &'a Listed, review_core::task::state::TaskStateV1);
        let mut groups: Vec<(TaskAttentionV1, Vec<Entry>)> = TaskAttentionV1::ALL
            .into_iter()
            .map(|group| (group, Vec::new()))
            .collect();
        for store in self.stores.iter().filter(|store| !earlier(store)) {
            for task in store.tasks.iter().flatten() {
                // An entry whose state does not read is left out; its folder row still lists it.
                let Ok(state) = task_execution::attention::state_of(&task.entry) else {
                    continue;
                };
                let group = TaskAttentionV1::of(&state);
                if let Some((_, tasks)) = groups.iter_mut().find(|(each, _)| *each == group) {
                    tasks.push((store, task, state));
                }
            }
        }
        if groups.iter().all(|(_, tasks)| tasks.is_empty()) {
            return Vec::new();
        }
        let mut rows = vec![Row::painted(
            "NEEDS YOU  (af task list --attention)",
            Paint::Title,
        )];
        let mut unreachable = false;
        for (group, tasks) in groups
            .iter()
            .filter(|(g, t)| g.needs_you() && !t.is_empty())
        {
            rows.push(Row::painted(
                format!("  {} ({})", group.title(), tasks.len()),
                Paint::Title,
            ));
            for (store, task, state) in tasks {
                rows.push(Row::plain(format!(
                    "    {}  {}",
                    TasksPane::id(store, &task.task_id),
                    state.label()
                )));
                let next = state.next_action.as_deref().unwrap_or("none");
                unreachable |= store.origin == Origin::Repository;
                let line =
                    crate::human::fit_command(&format!("      next: {}", reach(store, next)), MAIN);
                rows.extend(line.lines().map(Row::plain));
            }
        }
        if rows.len() == 1 {
            rows.push(Row::plain("  No Task needs you now."));
        }
        for (group, tasks) in groups
            .iter()
            .filter(|(g, t)| !g.needs_you() && !t.is_empty())
        {
            let line = match group {
                TaskAttentionV1::Running => {
                    let ids: Vec<String> = tasks
                        .iter()
                        .map(|(store, task, _)| TasksPane::id(store, &task.task_id))
                        .collect();
                    format!("  {} ({}): {}", group.title(), tasks.len(), ids.join(", "))
                }
                _ => format!("  {} ({})", group.title(), tasks.len()),
            };
            for line in bounded(&line, MAIN, "") {
                rows.push(Row::painted(line, Paint::Muted));
            }
        }
        if unreachable {
            let why = "Run a next command in its Task's repository: the user scope cannot name it.";
            for line in bounded(why, MAIN, "  ") {
                rows.push(Row::painted(line, Paint::Muted));
            }
        }
        rows.push(Row::blank());
        rows
    }
}

/// `next` as a command that reaches `store`: an `af task` command for a Store a command reaches
/// only by naming it gets `--state DIR`, as `r` and `D` prefill it.
fn reach(store: &Store, next: &str) -> String {
    if store.origin.explicit() && next.starts_with("af task ") && !next.contains(" --state ") {
        let dir = store.dir.display().to_string();
        format!("{next} --state {}", shell_words::quote(&dir))
    } else {
        next.to_owned()
    }
}

fn folder_row(task: &Listed) -> Row {
    let listed = task.entry["chargeable_tokens"].as_str();
    let inspected = task
        .summary
        .as_ref()
        .ok()
        .and_then(|summary| summary.chargeable.as_deref());
    let tokens = inspected.or(listed).unwrap_or("-");
    let unknown = match &task.summary {
        Ok(summary) => summary.unknown,
        Err(_) => task.entry["unknown_usage_attempts"].as_u64().unwrap_or(0),
    };
    let unknown = unknown_suffix(unknown);
    let line = format!(
        "    {:<24} {:<18} {:>4}  {tokens} tok{unknown}",
        task.task_id,
        task.outcome(),
        task.percent()
    );
    match &task.summary {
        Ok(_) => Row::plain(line),
        Err(error) => Row {
            spans: vec![
                Span::new(format!("{line} "), Paint::Plain),
                Span::chip("error", Tone::Fail),
                Span::new(" ", Paint::Plain),
                Span::new(format!("cannot be read: {error}"), Paint::Error),
            ],
        },
    }
}

/// The bar entries of one Store: its state groups, or one row naming it unreadable.
fn store_items(store: &Store, depth: usize) -> Vec<Item> {
    let tasks = match &store.tasks {
        Ok(tasks) => tasks,
        Err(_) => {
            return vec![Item {
                id: TasksPane::id(store, UNREADABLE),
                label: "! Store unreadable".to_owned(),
                muted: true,
                tone: None,
                children: None,
            }];
        }
    };
    // The indent and the fold glyph, and a last column for the progress chip's closing space.
    let width = BAR_INNER.saturating_sub(2 * (depth + 1) + 2 + 1);
    let mut groups = Vec::new();
    for state in State::ALL {
        let mut children = Vec::new();
        for task in tasks.iter().filter(|task| task.state() == state) {
            let muted = task.summary.is_err();
            children.push(Item {
                id: TasksPane::id(store, &task.task_id),
                label: fit(&task.task_id, task.outcome(), &task.percent(), width),
                muted,
                tone: (!muted).then(|| state.tone()),
                children: None,
            });
        }
        if children.is_empty() {
            continue;
        }
        let folder = format!("{}/", state.word());
        groups.push(Item {
            id: TasksPane::id(store, &folder),
            label: format!("{folder} ({})", children.len()),
            muted: false,
            tone: None,
            children: Some(children),
        });
    }
    groups
}

impl Pane for TasksPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let (mut targets, error) = match targets(scope) {
            Ok(targets) => (targets, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let (explicit, warning) = explicit_targets(scope, &self.session, &targets);
        targets.extend(explicit);
        self.warning = warning;
        let place = (scope.kind, scope.root.clone());
        if self.place.as_ref() != Some(&place) {
            // Another scope: nothing read for the old one is listed, opened or yanked. The
            // same scope with other targets (a Store registered meanwhile, `:state DIR`) keeps
            // every Store listed until the read answers for it, so an opened Task is never
            // reported gone because its Store is not read yet.
            self.stores.clear();
            self.selected = None;
            self.detail = None;
            self.artifact = None;
            self.cache = Cache::default();
        }
        self.place = Some(place);
        self.discover = Discovery::of(scope);
        self.loaded = targets.clone();
        self.targets = targets;
        // The Stores retained from the last read follow the targets as they stand now, before
        // the read answers for any of them.
        self.order();
        self.error = error;
        self.title = format!("TASKS  {}: {}", scope.word(), scope.name());
        self.read_now();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let count = self.stores.iter().filter(|store| earlier(store)).count();
        for store in self.stores.iter().filter(|store| !earlier(store)) {
            match &store.repo {
                None => items.extend(store_items(store, 2)),
                Some(repo) => {
                    let children = store_items(store, 3);
                    let count = store.tasks.as_ref().map_or(0, Vec::len);
                    let count = format!(" ({count})");
                    // An explicit Store's path keeps its end: the indent, the fold glyph and
                    // the count take the rest of the bar.
                    let room = BAR_INNER.saturating_sub(2 * 2 + 2 + count.len() + 1);
                    let name = clip_left(repo, room);
                    items.push(Item {
                        id: format!("{repo}/"),
                        label: format!("{name}/{count}"),
                        muted: store.tasks.is_err(),
                        tone: None,
                        children: Some(children),
                    });
                }
            }
        }
        if count > 0 {
            let stores = if count == 1 { "Store" } else { "Stores" };
            items.push(Item {
                id: EARLIER.to_owned(),
                label: format!("! {count} old {stores}"),
                muted: true,
                tone: None,
                children: None,
            });
        }
        items
    }

    fn open(&mut self, item: Option<&str>) {
        if item != self.selected.as_deref() && !self.explicit_read() {
            // A slow live read of the previously opened Task must not delay the new one's; a
            // load's read lists every Store and goes on.
            self.job = None;
        }
        // Every opening counts: a read's detail from an earlier one never stands over this
        // one's inspection.
        self.opening = self.opening.wrapping_add(1);
        self.opened = true;
        self.artifact = None;
        self.detail = None;
        self.selected = item.filter(|id| self.task(id).is_some()).map(str::to_owned);
        if let Some((dir, task_id)) = self.opened_task() {
            match Detail::read(&dir, &task_id, &mut self.cache) {
                Ok(detail) => {
                    self.detail = Some(Ok(detail));
                    self.sync_selected_row();
                }
                Err(error) => self.refuse_opened(&dir, &task_id, &error),
            }
        }
        self.next = Some(Instant::now() + LIVE);
        self.rebuild();
    }

    fn close(&mut self) {
        self.opened = false;
        if !self.explicit_read() {
            self.job = None;
        }
    }

    fn rows(&self) -> &[Row] {
        match &self.artifact {
            Some(opened) => &opened.rows,
            None => &self.rows,
        }
    }

    fn key(&mut self, key: Key, row: usize) -> Result<Option<Effect>, String> {
        let detail = match &self.detail {
            Some(Ok(detail)) if self.artifact.is_none() => detail,
            _ => return Ok(None),
        };
        match key {
            Key::Enter => {
                let Some(id) = self.history.get(&row) else {
                    return Err("Enter opens the artifact of a HISTORY row".to_owned());
                };
                let value = match task_execution::recorded_artifact(&detail.dir, id) {
                    Ok(value) => value,
                    Err(error) => {
                        // An artifact the Store records but cannot give back refuses the Store,
                        // as any other failed read of it does.
                        let error = format!("{id}: {error}");
                        let (dir, task_id) = (detail.dir.clone(), detail.task_id.clone());
                        self.refuse_opened(&dir, &task_id, &error);
                        self.rebuild();
                        return Err(error);
                    }
                };
                let pretty = serde_json::to_string_pretty(&value)
                    .map_err(|error| format!("{id}: {error}"))?;
                let mut rows = vec![
                    Row::painted(format!("ARTIFACT  {id}"), Paint::Title),
                    Row::blank(),
                ];
                rows.extend(pretty.lines().map(Row::plain));
                self.artifact = Some(Opened { rows, row });
                Ok(None)
            }
            Key::Char('p') => match detail.pipeline() {
                Some(pipeline) => Ok(Some(Effect::OpenPipeline(pipeline.to_owned()))),
                None => Err(format!("Task {} records no plan", detail.task_id)),
            },
            Key::Char('r' | 'D') => {
                let plan = detail.document["plan_id"].as_str();
                let Some(store) = self.store_at(&detail.dir) else {
                    return Ok(None);
                };
                match verb(store, key, &detail.task_id, plan, detail.state()) {
                    Some(line) => line.map(|line| Some(Effect::Prefill(line))),
                    None => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn legend(&self) -> &'static str {
        match (&self.artifact, &self.detail) {
            (Some(_), _) => "j/k scroll  y yank Task id  q/Esc back to the Task",
            (None, Some(Ok(_))) => {
                "r run  D deliver  Enter artifact  p pipeline  y yank id  R re-read  Tab bar"
            }
            _ => "j/k move  R re-read  Tab bar  :cmd  q quit",
        }
    }

    fn refresh(&mut self, scope: &Scope) -> Result<(), String> {
        self.load(scope)
    }

    fn poll(&mut self) -> bool {
        // The read a load or `R` recorded starts here, after the frame painted last.
        if self.pending {
            self.start(true);
        }
        // Every Store the read in flight has delivered, then the end of the read.
        let mut changed = false;
        while let Some(job) = &self.job {
            match job.receiver.try_recv() {
                Ok(read) => {
                    self.collect(read);
                    changed = true;
                }
                Err(TryRecvError::Empty) => {
                    self.spinner = self.spinner.wrapping_add(1);
                    return changed;
                }
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    return changed;
                }
            }
        }
        if changed || !self.live() {
            return changed;
        }
        let now = Instant::now();
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + LIVE);
        #[cfg(test)]
        {
            self.reads += 1;
        }
        self.start(false);
        false
    }

    fn busy(&self) -> Option<char> {
        let spinner = super::SPINNER[self.spinner % super::SPINNER.len()];
        (self.pending || self.job.is_some()).then_some(spinner)
    }

    fn nested(&self) -> bool {
        self.artifact.is_some()
    }

    fn back(&mut self) -> Option<usize> {
        self.artifact.take().map(|opened| opened.row)
    }

    fn yank(&self, _row: usize) -> Option<String> {
        match &self.detail {
            Some(Ok(detail)) => Some(detail.task_id.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
