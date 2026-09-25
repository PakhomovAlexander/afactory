//! Tasks: every Task this scope's Task state holds (docs/design/tui.md section 5.5), read by
//! the functions behind `af task list --json` (`task_execution::list_common`) and `af task
//! explain --json`, which is the `af task show --json` document with the plan and its graph.
//!
//! The bar groups the Tasks by state; one Task's main pane shows its identity, one row per
//! stage of its plan's graph order, its token and time totals, and its event history. Every
//! value is a field of those documents or of an artifact they name by ID: the node an
//! invocation ran and the goal of the revision. Nothing is written. While the opened Task runs,
//! a thread reads the Store again about once a second and the event loop collects the result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{Effect, Pane, Row};
use crate::task_execution;
use crate::tui::keymap::Key;
use crate::tui::paint::{self, Paint};
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// How often an opened running Task is read again.
const LIVE: Duration = Duration::from_secs(1);
/// The bar's columns, its separator excluded.
const BAR_INNER: usize = 27;
/// The hex digits of an artifact ID the pane prints; `Enter` and `y` use the whole ID.
const SHORT: usize = 8;
/// The columns a stage name takes in PROGRESS.
const STAGE: usize = 18;
/// The columns of the main pane beside the bar at 100 columns; the TASK line fits its goal
/// into them.
const MAIN: usize = 72;
/// The bar id of a Store that cannot be read.
const UNREADABLE: &str = "!unreadable";

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

    /// A settled stage will not run again in this plan.
    pub(crate) fn settled(self) -> bool {
        matches!(self, Mark::Ok | Mark::Failed | Mark::Skipped)
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
    /// The recorded wall of its Attempts (`attempt_walls`), when the document carries one.
    pub(crate) wall_ms: Option<u64>,
    /// The charge its settled Attempts recorded, when one settled.
    pub(crate) tokens: Option<u128>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    pub(crate) settled: usize,
    pub(crate) stages: usize,
}

impl Progress {
    pub(crate) fn of(stages: &[Stage]) -> Progress {
        let settled = stages.iter().filter(|stage| stage.mark.settled()).count();
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
    attempts: u64,
    open: bool,
    ok: bool,
    failed: bool,
    settled: u64,
    tokens: u128,
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
        match record["kind"].as_str() {
            Some("reserved") => {
                if let Some(attempt) = attempt {
                    attempts.insert(attempt.to_owned(), node);
                }
                track.attempts += 1;
                track.open = true;
            }
            Some("released") => track.open = false,
            Some("settled") => {
                track.open = false;
                track.settled += 1;
                let charged = text(&record["charged_tokens"])?;
                let charged: u128 = charged.parse().map_err(|_| "a charge is not decimal")?;
                track.tokens = track.tokens.saturating_add(charged);
                let succeeded = record["result"]["kind"] == "succeeded";
                track.ok |= succeeded;
                track.failed = !succeeded;
            }
            Some("published" | "owned_children_completed") => {
                track.ok = true;
                track.failed = false;
            }
            _ => {}
        }
    }
    // The last whole-Round report of the current plan: every compiled node with its outcome,
    // including a failure before any Attempt and a suppressed branch.
    let mut outcomes: BTreeMap<&str, &str> = BTreeMap::new();
    let current = document["plan_id"].as_str();
    let reports = array(&document["run_reports"]).iter().rev();
    let last = reports
        .map(|entry| &entry["report"])
        .find(|report| report["plan_id"].as_str() == current && report["phase_id"].is_null());
    for node in last.map(|report| array(&report["nodes"])).unwrap_or(&[]) {
        if let (Some(name), Some(kind)) = (node["node"].as_str(), node["outcome"]["kind"].as_str())
        {
            outcomes.insert(name, kind);
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
        let mark = if !finished && track.open {
            Mark::Running
        } else if outcome == Some("failed") {
            Mark::Failed
        } else if track.ok || outcome == Some("completed") {
            Mark::Ok
        } else if track.failed {
            Mark::Failed
        } else if outcome == Some("suppressed") {
            Mark::Skipped
        } else {
            Mark::NotReached
        };
        let allowance = &document["graph"]["allowances"][node]["max_attempts"];
        stages.push(Stage {
            node: node.to_owned(),
            mark,
            attempts: track.attempts,
            max_attempts: allowance.as_u64(),
            wall_ms: walls.get(node).copied(),
            tokens: (track.settled > 0).then_some(track.tokens),
        });
    }
    Ok(stages)
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn text(value: &Value) -> Result<&str, String> {
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
    let finished = history
        .iter()
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

/// A bar row, `task-id  outcome  progress%`, fitted to `width` columns. The Task id tells
/// the rows apart, so the outcome yields first: it is cut, and below three columns left out;
/// only then is the id cut.
pub(crate) fn fit(task_id: &str, outcome: &str, percent: &str, width: usize) -> String {
    let full = format!("{task_id}  {outcome}  {percent}");
    if full.len() <= width {
        return full;
    }
    let room = width.saturating_sub(task_id.len() + 4 + percent.len());
    if room >= 3 {
        return format!("{task_id}  {}  {percent}", clip(outcome, room));
    }
    let id = clip(task_id, width.saturating_sub(2 + percent.len()).max(1));
    format!("{id}  {percent}")
}

/// A stage's node without the root call and `nodes.` segments: `root.nodes.check` is `check`.
fn stage_name(node: &str) -> String {
    let node = node.strip_prefix("root.").unwrap_or(node);
    let parts: Vec<&str> = node.split('.').filter(|part| *part != "nodes").collect();
    parts.join(".")
}

/// What stays true between reads: the node of an invocation or output artifact never
/// changes, and neither does a finished Task.
#[derive(Clone, Default)]
struct Cache {
    nodes: BTreeMap<String, Invoked>,
    finished: BTreeMap<String, Summary>,
}

/// The node an invocation ran and the plan it ran under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invoked {
    pub(crate) node: String,
    /// `None` only where a record names no invocation of its own.
    pub(crate) plan_id: Option<String>,
}

/// The node and plan of an invocation artifact, or of the invocation an output answers.
fn node_of(dir: &Path, id: &str, cache: &mut Cache) -> Result<Invoked, String> {
    if let Some(invoked) = cache.nodes.get(id) {
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
    cache.nodes.insert(id.to_owned(), invoked.clone());
    Ok(invoked)
}

/// What the bar needs of one Task beyond its list entry.
#[derive(Clone, Debug)]
struct Summary {
    acceptance: Option<String>,
    started: Option<u64>,
    progress: Progress,
}

fn summary(dir: &Path, task_id: &str, cache: &mut Cache) -> Result<Summary, String> {
    let document = task_execution::inspection_document(dir, task_id, true)?
        .ok_or_else(|| format!("Task {task_id} is not in {}", dir.display()))?;
    let stages = stages(&document, &mut |id| node_of(dir, id, cache))?;
    Ok(Summary {
        acceptance: document["result"]["acceptance"].as_str().map(str::to_owned),
        started: span_of(&document).map(|(first, _)| first),
        progress: Progress::of(&stages),
    })
}

/// One Task as `af task list` lists it.
struct Listed {
    task_id: String,
    /// The `af/task-list-entry@2` document.
    entry: Value,
    summary: Result<Summary, String>,
}

impl Listed {
    fn state(&self) -> State {
        let acceptance = self.summary.as_ref().ok();
        let acceptance = acceptance.and_then(|summary| summary.acceptance.as_deref());
        state_of(&self.entry["phase"], acceptance)
    }

    fn outcome(&self) -> &str {
        // `af task list` prints a Task without a result the same way.
        self.entry["outcome"].as_str().unwrap_or("incomplete")
    }

    fn percent(&self) -> String {
        match &self.summary {
            Ok(summary) => format!("{}%", summary.progress.percent()),
            Err(_) => "?%".to_owned(),
        }
    }
}

/// One Task state directory, the repository group the user scope lists it under, and what
/// reading it found.
struct Store {
    dir: PathBuf,
    /// The directory as the pane prints it, `~` for the home directory.
    shown: String,
    repo: Option<String>,
    tasks: Result<Vec<Listed>, String>,
}

/// An existing directory with entries but no `events.sqlite` holds something the running
/// binary does not read as a Task Store; an absent or empty one holds no Tasks yet.
fn not_a_store(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let held = entries.filter_map(Result::ok).count();
    let store = dir.join("events.sqlite");
    (held > 0 && std::fs::symlink_metadata(&store).is_err())
        .then(|| format!("holds {held} entries but no events.sqlite"))
}

fn read_store(dir: &Path, shown: &str, repo: Option<String>, cache: &mut Cache) -> Store {
    let tasks = match not_a_store(dir) {
        Some(refusal) => Err(refusal),
        None => task_execution::list_common(dir),
    };
    let tasks = tasks.and_then(|entries| {
        let mut tasks = Vec::new();
        for entry in entries {
            let task_id = text(&entry["task_id"])?.to_owned();
            let result = entry["phase"]["result_id"].as_str().map(str::to_owned);
            let cached = result.as_ref().and_then(|id| cache.finished.get(id));
            let summary = match cached {
                Some(summary) => Ok(summary.clone()),
                // A Task the Store lists but cannot inspect is the Store's refusal, named with
                // the Task: it is never grouped by a summary it does not have.
                None => Ok(summary(dir, &task_id, cache)
                    .map_err(|error| format!("Task {task_id}: {error}"))?),
            };
            if let (Some(id), Ok(summary)) = (result, &summary) {
                cache.finished.insert(id, summary.clone());
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
        tasks,
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
        let (task_id, kind) = (paint::ascii(&self.task_id), paint::ascii(&self.kind));
        let room = MAIN
            .saturating_sub(6 + task_id.len() + 2 + kind.len() + 4)
            .max(16);
        let goal = clip(&self.goal, room);
        let title = format!("TASK  {task_id}  {kind}: \"{goal}\"");
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
        let mut line = format!(
            "PLAN  {}  {origin}  STATE {}",
            short(document["plan_id"].as_str()),
            state.word()
        );
        let span = span_of(document);
        let elapsed = span.map(|(first, last)| match state {
            State::Done | State::Failed => last.saturating_sub(first),
            State::Running | State::Awaiting => now_ms.saturating_sub(first),
        });
        if let (Some((first, _)), Some(elapsed)) = (span, elapsed) {
            line.push_str(&format!("  started {}", clock(first)));
            line.push_str(&format!("  elapsed {}", duration(elapsed)));
        }
        rows.push(Row::plain(line));
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

fn stage_row(stage: &Stage) -> Row {
    let name = clip(&stage_name(&stage.node), STAGE);
    let wall = stage.wall_ms.map_or(String::new(), duration);
    let tokens = stage.tokens.map_or(String::new(), |n| format!("{n} tok"));
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
    let line = format!(
        "  {}  {name:<STAGE$} {wall:>7} {tokens:>12}  {note}",
        stage.mark.text()
    );
    let paint = match stage.mark {
        Mark::Failed => Paint::Error,
        Mark::NotReached | Mark::Skipped => Paint::Muted,
        Mark::Ok | Mark::Running => Paint::Plain,
    };
    Row::painted(line.trim_end(), paint)
}

/// What one read of the scope's Task state found.
struct Reread {
    stores: Vec<Store>,
    detail: Option<Result<Detail, String>>,
    cache: Cache,
}

/// Read every Store, and the opened Task again. It runs on the key loop for a load or `R`, and
/// on a thread for a running Task's live read.
fn read(targets: &[Target], opened: Option<(PathBuf, String)>, mut cache: Cache) -> Reread {
    let mut stores = Vec::new();
    for target in targets {
        let repo = target.repo.clone();
        stores.push(read_store(&target.dir, &target.shown, repo, &mut cache));
    }
    let detail = opened.map(|(dir, task_id)| {
        let detail = Detail::read(&dir, &task_id, &mut cache);
        if let Err(error) = &detail {
            refuse(&mut stores, &dir, &task_id, error);
        }
        detail
    });
    Reread {
        stores,
        detail,
        cache,
    }
}

/// A Task a Store lists but cannot be inspected refuses the whole Store: no other Task of it
/// is shown as if the Store were readable.
fn refuse(stores: &mut [Store], dir: &Path, task_id: &str, error: &str) {
    for store in stores.iter_mut().filter(|store| store.dir == dir) {
        store.tasks = Err(format!("Task {task_id}: {error}"));
    }
}

/// One Task state directory a scope reads.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    dir: PathBuf,
    shown: String,
    /// The repository group the user scope lists it under: the directory's opaque name.
    repo: Option<String>,
}

/// The Task state directories a scope reads, or why the scope names none: the project scope
/// reads the directory `af task` resolves for the repository without `--state`, and the user
/// scope every repository's.
fn targets(scope: &Scope) -> Result<Vec<Target>, String> {
    let Some(root) = &scope.state else {
        return Err("no Task state: neither XDG_STATE_HOME nor HOME is set".to_owned());
    };
    if let Some(toplevel) = scope.toplevel() {
        let repo = std::fs::canonicalize(toplevel)
            .map_err(|error| format!("{}: {error}", toplevel.display()))?;
        let dir = task_execution::default_task_state(root, &repo)?;
        let shown = scope.abbreviate(&dir);
        return Ok(vec![Target {
            dir,
            shown,
            repo: None,
        }]);
    }
    let local = task_execution::local_task_states(root);
    let entries = match std::fs::read_dir(&local) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", local.display())),
    };
    let mut targets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", local.display()))?;
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir {
            let name = entry.file_name().to_string_lossy().into_owned();
            let dir = entry.path();
            let shown = scope.abbreviate(&dir);
            targets.push(Target {
                dir,
                shown,
                repo: Some(name),
            });
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
    receiver: Receiver<Reread>,
    /// The Task the read was started for; a read for another is dropped.
    for_task: Option<String>,
}

#[derive(Default)]
pub(crate) struct TasksPane {
    /// The folder pane's title.
    title: String,
    targets: Vec<Target>,
    /// Why the scope names no Task state.
    error: Option<String>,
    stores: Vec<Store>,
    cache: Cache,
    /// The bar id of the opened Task.
    selected: Option<String>,
    detail: Option<Result<Detail, String>>,
    artifact: Option<Opened>,
    /// The main pane shows this pane.
    opened: bool,
    job: Option<Job>,
    next: Option<Instant>,
    /// Live reads started, for tests.
    #[cfg(test)]
    reads: usize,
    rows: Vec<Row>,
    history: BTreeMap<usize, String>,
}

impl TasksPane {
    /// The bar id of a Task, a group or an unreadable Store.
    fn id(store: &Store, name: &str) -> String {
        match &store.repo {
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

    /// A load or `R`: every listed Task is inspected again. Only the live read of a running
    /// Task reuses finished summaries; an explicit read never trusts them, so a Task that can
    /// no longer be inspected refuses its Store.
    fn read_now(&mut self) {
        self.job = None;
        self.cache.finished.clear();
        let reread = read(&self.targets, self.opened_task(), self.cache.clone());
        self.apply(reread);
    }

    fn apply(&mut self, reread: Reread) {
        self.stores = reread.stores;
        self.cache = reread.cache;
        match reread.detail {
            // The Store was refused with the cause; the folder shows it, not the Task.
            Some(Err(_)) => {
                self.detail = None;
                self.selected = None;
            }
            Some(detail) => self.detail = Some(detail),
            None => {}
        }
        self.rebuild();
    }

    /// Whether the opened Task is running, so the pane reads it again.
    fn live(&self) -> bool {
        let running = matches!(&self.detail, Some(Ok(detail)) if detail.state() == State::Running);
        self.opened && running
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
                Row::painted(format!("cannot be read: {error}"), Paint::Error),
            ],
            _ => self.folder_rows(),
        };
    }

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted(&self.title, Paint::Title), Row::blank()];
        if let Some(error) = &self.error {
            rows.push(Row::painted(error, Paint::Error));
        } else if self.stores.is_empty() {
            rows.push(Row::plain("No Task state is recorded here yet."));
        }
        for store in &self.stores {
            let dir = &store.shown;
            match &store.tasks {
                Err(error) => {
                    let refusal = format!("{dir}: this Store cannot be read: {error}");
                    rows.push(Row::painted(refusal, Paint::Error));
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
}

fn folder_row(task: &Listed) -> Row {
    let tokens = task.entry["chargeable_tokens"].as_str().unwrap_or("-");
    let line = format!(
        "    {:<24} {:<18} {:>4}  {tokens} tok",
        task.task_id,
        task.outcome(),
        task.percent()
    );
    match &task.summary {
        Ok(_) => Row::plain(line),
        Err(error) => Row::painted(format!("{line}  cannot be read: {error}"), Paint::Error),
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
                children: None,
            }];
        }
    };
    let width = BAR_INNER.saturating_sub(2 * (depth + 1) + 2);
    let mut groups = Vec::new();
    for state in State::ALL {
        let mut children = Vec::new();
        for task in tasks.iter().filter(|task| task.state() == state) {
            children.push(Item {
                id: TasksPane::id(store, &task.task_id),
                label: fit(&task.task_id, task.outcome(), &task.percent(), width),
                muted: task.summary.is_err(),
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
            children: Some(children),
        });
    }
    groups
}

impl Pane for TasksPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let (targets, error) = match targets(scope) {
            Ok(targets) => (targets, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        if targets != self.targets {
            // Another scope: nothing read for the old one is listed, opened or yanked.
            self.stores.clear();
            self.selected = None;
            self.detail = None;
            self.artifact = None;
            self.cache = Cache::default();
        }
        self.targets = targets;
        self.error = error;
        self.title = format!("TASKS  {}: {}", scope.word(), scope.name());
        self.read_now();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for store in &self.stores {
            match &store.repo {
                None => items.extend(store_items(store, 2)),
                Some(repo) => {
                    let children = store_items(store, 3);
                    let count = store.tasks.as_ref().map_or(0, Vec::len);
                    items.push(Item {
                        id: format!("{repo}/"),
                        label: format!("{repo}/ ({count})"),
                        muted: store.tasks.is_err(),
                        children: Some(children),
                    });
                }
            }
        }
        items
    }

    fn open(&mut self, item: Option<&str>) {
        self.opened = true;
        self.artifact = None;
        self.detail = None;
        self.selected = item.filter(|id| self.task(id).is_some()).map(str::to_owned);
        if let Some((dir, task_id)) = self.opened_task() {
            match Detail::read(&dir, &task_id, &mut self.cache) {
                Ok(detail) => self.detail = Some(Ok(detail)),
                Err(error) => {
                    refuse(&mut self.stores, &dir, &task_id, &error);
                    self.selected = None;
                }
            }
        }
        self.next = Some(Instant::now() + LIVE);
        self.rebuild();
    }

    fn close(&mut self) {
        self.opened = false;
        self.job = None;
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
                let value = task_execution::recorded_artifact(&detail.dir, id)
                    .map_err(|error| format!("{id}: {error}"))?;
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
            _ => Ok(None),
        }
    }

    fn legend(&self) -> &'static str {
        match (&self.artifact, &self.detail) {
            (Some(_), _) => "j/k scroll  y yank Task id  q/Esc back to the Task",
            (None, Some(Ok(_))) => "Enter artifact  p pipeline  y yank id  R re-read  Tab bar",
            _ => "j/k move  R re-read  Tab bar  :cmd  q quit",
        }
    }

    fn refresh(&mut self, scope: &Scope) -> Result<(), String> {
        self.load(scope)
    }

    fn poll(&mut self) -> bool {
        if let Some(job) = &self.job {
            return match job.receiver.try_recv() {
                Ok(reread) => {
                    let current = job.for_task == self.selected;
                    self.job = None;
                    if current {
                        self.apply(reread);
                    }
                    current
                }
                Err(TryRecvError::Empty) => false,
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    false
                }
            };
        }
        if !self.live() {
            return false;
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
        let (targets, opened) = (self.targets.clone(), self.opened_task());
        let cache = self.cache.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(read(&targets, opened, cache));
        });
        self.job = Some(Job {
            receiver,
            for_task: self.selected.clone(),
        });
        false
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
