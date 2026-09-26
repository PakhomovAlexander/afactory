//! Workers: the Worker packages `HEAD` commits (docs/design/tui.md section 5.3). Reviewer
//! Workers are `.af/workers/*/reviewer.toml`; Task Worker packages are every `worker.toml` under
//! `.af/task-packages/`, `.af/packages/` and `.af/vendor/`, however deep. Like the Pipelines pane
//! it reads one resolved commit with git in a cleared environment, marks a working-tree file
//! that differs from it, and keeps the last good entries under a failed read.
//!
//! One Worker's main pane has three sections. IDENTITY is its declaration and the pin the
//! committed `af.lock` or `.af/task-catalog.toml` records for it. STATE counts the Attempts of
//! this scope's Task Stores whose invocation ran a slot the Task's recorded plan binds to this
//! Worker, read from the documents behind `af task explain --json`. PROMPT is the file the
//! kernel sends: `reviewer.md` for a reviewer Worker, `instructions.md` for a Task Worker
//! package.
//!
//! The pane reads nothing until its folder or one of its entries is first opened: STATE scans
//! every Task's records (section 8), so the browser does not pay for it on start.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use toml::Value;

use super::pipelines::{committed, declared, differs, git, head, pinned_paths};
use super::tasks::{self, Cache, Invoked, Target};
use super::{Pane, Row};
use crate::task_execution;
use crate::tui::paint::Paint;
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// The columns of a section rule: the main pane beside the bar at 100 columns.
const RULE: usize = 72;
const CATALOG: &str = ".af/task-catalog.toml";
const LOCK: &str = ".af/af.lock";

/// What a declaration makes: which lock pins it, and which file the kernel sends as its prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `.af/workers/<name>/reviewer.toml`, pinned in `af.lock`.
    Reviewer,
    /// A `worker.toml` package, pinned in `.af/task-catalog.toml`.
    Task,
}

impl Kind {
    /// The prompt file the kernel itself sends a Worker of this kind.
    pub(crate) fn prompt(self) -> &'static str {
        match self {
            Kind::Reviewer => "reviewer.md",
            Kind::Task => "instructions.md",
        }
    }

    fn lock(self) -> &'static str {
        match self {
            Kind::Reviewer => LOCK,
            Kind::Task => CATALOG,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Kind::Reviewer => "reviewer",
            Kind::Task => "Task",
        }
    }
}

/// The directories Worker packages are declared under, in bar order.
const SOURCES: [(&str, Kind); 4] = [
    (".af/workers/", Kind::Reviewer),
    (".af/task-packages/", Kind::Task),
    (".af/packages/", Kind::Task),
    (".af/vendor/", Kind::Task),
];

/// The source a committed path declares a Worker under, when it declares one.
fn source_of(path: &str) -> Option<(&'static str, Kind)> {
    for (source, kind) in SOURCES {
        let Some(rest) = path.strip_prefix(source) else {
            continue;
        };
        let declares = match kind {
            // One directory per reviewer, named by the Worker: the registry looks nowhere else.
            Kind::Reviewer => rest.split('/').count() == 2 && rest.ends_with("/reviewer.toml"),
            Kind::Task => rest.ends_with("/worker.toml"),
        };
        return declares.then_some((source, kind));
    }
    None
}

/// Where the committed lock places the Worker's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pin {
    /// At this package, with the version and digest it records.
    Here {
        version: Option<String>,
        digest: Option<String>,
    },
    /// At another package directory.
    Elsewhere(String),
    Unpinned,
}

/// One pin of a committed lock: the package directory, version and digest.
type Pinned = (String, Option<String>, Option<String>);

/// Name to pin, as a committed lock records Workers: the catalog's `[packages]` by their
/// `path`, and `af.lock`'s `[workers]` at `.af/workers/<name>`, where the reviewer registry
/// looks for them.
fn pins(kind: Kind, lock: &Value) -> BTreeMap<String, Pinned> {
    let (table, paths) = match kind {
        Kind::Task => (lock.get("packages"), pinned_paths(lock)),
        Kind::Reviewer => (lock.get("workers"), BTreeMap::new()),
    };
    let mut pins = BTreeMap::new();
    for (name, pin) in table.and_then(Value::as_table).into_iter().flatten() {
        let path = match kind {
            Kind::Task => match paths.get(name) {
                Some(path) => path.clone(),
                None => continue,
            },
            Kind::Reviewer => format!(".af/workers/{name}"),
        };
        let field = |key: &str| pin.get(key).and_then(Value::as_str).map(str::to_owned);
        pins.insert(name.clone(), (path, field("version"), field("digest")));
    }
    pins
}

fn pin_of(pins: &BTreeMap<String, Pinned>, name: Option<&str>, directory: &str) -> Pin {
    match name.and_then(|name| pins.get(name)) {
        Some((path, version, digest)) if Path::new(path) == Path::new(directory) => Pin::Here {
            version: version.clone(),
            digest: digest.clone(),
        },
        Some((path, _, _)) => Pin::Elsewhere(path.clone()),
        None => Pin::Unpinned,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The repository-relative path of the declaration: the bar id.
    pub(crate) id: String,
    source: &'static str,
    kind: Kind,
    /// The declared name, or the package directory under its source when none is declared.
    label: String,
    name: Option<String>,
    /// The package directory, repository-relative.
    directory: String,
    /// The declaration as committed at `HEAD`.
    declaration: String,
    /// The prompt as committed at `HEAD`; `None` when `HEAD` commits no such file.
    prompt: Option<String>,
    /// The files shown from `HEAD` whose working-tree copy differs from it.
    drifted: Vec<String>,
    pin: Pin,
}

impl Entry {
    fn prompt_path(&self) -> String {
        format!("{}/{}", self.directory, self.kind.prompt())
    }
}

/// What one read of `HEAD` found.
#[derive(Debug)]
struct Discovery {
    /// The commit every declaration was read from; empty for an unborn `HEAD`.
    commit: String,
    entries: Vec<Entry>,
}

/// Every Worker `HEAD` commits, by source in bar order and by path within one. An unborn
/// `HEAD` lists nothing; a read that fails is an error, never an empty list.
fn discover(root: &Path) -> Result<Discovery, String> {
    let commit = head(root)?;
    let mut found = Discovery {
        commit: commit.clone(),
        entries: Vec::new(),
    };
    if commit.is_empty() {
        return Ok(found);
    }
    let listing = git(root, &["ls-tree", "-r", "-z", "--name-only", &commit, "--"])?;
    let mut paths = BTreeSet::new();
    for path in listing.split(|byte| *byte == 0) {
        if let Ok(path) = std::str::from_utf8(path)
            && !path.is_empty()
        {
            paths.insert(path.to_owned());
        }
    }
    let lock = |file: &str| -> Result<Value, String> {
        if paths.contains(file) {
            Ok(declared(&committed(root, &commit, file)?))
        } else {
            Ok(Value::Table(toml::map::Map::new()))
        }
    };
    let catalog = pins(Kind::Task, &lock(CATALOG)?);
    let reviewers = pins(Kind::Reviewer, &lock(LOCK)?);
    for (source, kind) in SOURCES {
        for id in paths
            .iter()
            .filter(|path| source_of(path) == Some((source, kind)))
        {
            let declaration = committed(root, &commit, id)?;
            let value = declared(&declaration);
            let directory = id.rsplit_once('/').map_or("", |(dir, _)| dir).to_owned();
            let name = value.get("name").and_then(Value::as_str).map(str::to_owned);
            let fallback = directory.strip_prefix(source).unwrap_or(&directory);
            let prompt_path = format!("{directory}/{}", kind.prompt());
            // The prompt is shown as text, whatever its bytes: a stray byte is not a refusal.
            let prompt = if paths.contains(&prompt_path) {
                let bytes = git(root, &["show", &format!("{commit}:{prompt_path}")])?;
                Some(String::from_utf8_lossy(&bytes).into_owned())
            } else {
                None
            };
            let mut drifted = Vec::new();
            for file in [id.as_str(), prompt_path.as_str()] {
                if differs(root, &commit, file) {
                    drifted.push(file.to_owned());
                }
            }
            let pins = match kind {
                Kind::Reviewer => &reviewers,
                Kind::Task => &catalog,
            };
            found.entries.push(Entry {
                id: id.clone(),
                source,
                kind,
                label: name.clone().unwrap_or_else(|| fallback.to_owned()),
                pin: pin_of(pins, name.as_deref(), &directory),
                name,
                directory,
                declaration,
                prompt,
                drifted,
            });
        }
    }
    Ok(found)
}

/// What the Attempts of one Worker add up to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Reserved, and neither settled nor released.
    pub(crate) open: u64,
    pub(crate) ok: u64,
    pub(crate) failed: u64,
    /// Reservations released before dispatch: not Attempts, so neither charged nor timed.
    pub(crate) released: u64,
    /// The highest charge recorded for each Attempt, summed.
    pub(crate) tokens: u128,
    /// The recorded Attempt walls, summed.
    pub(crate) wall_ms: u64,
    /// The Attempts that recorded a wall.
    pub(crate) walls: u64,
}

impl Tally {
    pub(crate) fn attempts(&self) -> u64 {
        self.open + self.ok + self.failed
    }

    fn add(&mut self, other: &Tally) {
        self.open += other.open;
        self.ok += other.ok;
        self.failed += other.failed;
        self.released += other.released;
        self.tokens = self.tokens.saturating_add(other.tokens);
        self.wall_ms = self.wall_ms.saturating_add(other.wall_ms);
        self.walls += other.walls;
    }
}

/// One reservation of a Worker's slot, as its records leave it.
struct Reservation {
    worker: String,
    released: bool,
    /// `Some(succeeded)` once settled.
    settled: Option<bool>,
    charged: u128,
}

/// The Worker a node of a compiled graph runs: the package the graph binds to the node's one
/// slot. A node that names no slot, or several (a Provider admission, an optimization
/// experiment), runs no single Worker.
pub(crate) fn worker_of(graph: &Json, node: &str) -> Option<String> {
    let operator = &graph["nodes"][node]["operator"];
    let slot = match operator["kind"].as_str()? {
        "primitive" => operator["operator"]["slot"].as_str(),
        "review_domain" => operator["operation"]["slot"].as_str(),
        _ => None,
    }?;
    graph["slots"][slot]["worker"].as_str().map(str::to_owned)
}

/// Per Worker name, the Attempts one Task's `af task explain --json` document records. A
/// reservation's node comes from its invocation (`invoked`), and its Worker from that node's
/// slot in the compiled graph of the plan the invocation ran under: the document's own graph
/// for its current plan, `graph` for an earlier one. Never from a name.
///
/// The accounting is the Tasks pane's: an Attempt's charge is the highest its settlement or a
/// usage observation records, and a released reservation is no Attempt.
pub(crate) fn tally(
    document: &Json,
    invoked: &mut dyn FnMut(&str) -> Result<Invoked, String>,
    graph: &mut dyn FnMut(&str) -> Result<Json, String>,
) -> Result<BTreeMap<String, Tally>, String> {
    let current = document["plan_id"].as_str();
    let mut graphs: BTreeMap<String, Json> = BTreeMap::new();
    let mut reservations: BTreeMap<String, Reservation> = BTreeMap::new();
    for entry in tasks::array(&document["execution_records"]) {
        let record = &entry["record"];
        let Some(attempt) = record["attempt_id"].as_str() else {
            continue;
        };
        match record["kind"].as_str() {
            Some("reserved") => {
                let invoked = invoked(tasks::text(&record["invocation_id"])?)?;
                let plan = invoked.plan_id.as_deref().or(current);
                let plan = plan.ok_or("a reservation names no plan")?;
                if !graphs.contains_key(plan) {
                    let compiled = if Some(plan) == current {
                        document["graph"].clone()
                    } else {
                        graph(plan)?
                    };
                    graphs.insert(plan.to_owned(), compiled);
                }
                if let Some(worker) = worker_of(&graphs[plan], &invoked.node) {
                    let reservation = Reservation {
                        worker,
                        released: false,
                        settled: None,
                        charged: 0,
                    };
                    reservations.insert(attempt.to_owned(), reservation);
                }
            }
            Some("released") => {
                if let Some(reservation) = reservations.get_mut(attempt) {
                    reservation.released = true;
                }
            }
            Some(kind @ ("usage_observed" | "settled")) => {
                let Some(reservation) = reservations.get_mut(attempt) else {
                    continue;
                };
                let charged = tasks::text(&record["charged_tokens"])?;
                let charged: u128 = charged.parse().map_err(|_| "a charge is not decimal")?;
                reservation.charged = reservation.charged.max(charged);
                if kind == "settled" {
                    reservation.settled = Some(record["result"]["kind"] == "succeeded");
                }
            }
            _ => {}
        }
    }
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for reservation in reservations.values() {
        let tally = tallies.entry(reservation.worker.clone()).or_default();
        if reservation.released {
            tally.released += 1;
            continue;
        }
        match reservation.settled {
            None => tally.open += 1,
            Some(true) => tally.ok += 1,
            Some(false) => tally.failed += 1,
        }
        tally.tokens = tally.tokens.saturating_add(reservation.charged);
    }
    let mut walled = BTreeSet::new();
    for wall in tasks::array(&document["attempt_walls"]) {
        let Some(attempt) = wall["attempt_id"].as_str() else {
            continue;
        };
        let reservation = reservations.get(attempt).filter(|r| !r.released);
        if let (Some(reservation), Some(ms)) = (reservation, wall["elapsed_ms"].as_u64()) {
            let tally = tallies.entry(reservation.worker.clone()).or_default();
            tally.wall_ms = tally.wall_ms.saturating_add(ms);
            if walled.insert(attempt) {
                tally.walls += 1;
            }
        }
    }
    Ok(tallies)
}

/// The compiled graph of a recorded plan, through the plan's `compiled_graph_id`.
fn compiled_graph(dir: &Path, plan: &str) -> Result<Json, String> {
    let plan = task_execution::recorded_artifact(dir, plan)?;
    let graph = tasks::text(&plan["payload"]["compiled_graph_id"])?;
    Ok(task_execution::recorded_artifact(dir, graph)?["payload"].clone())
}

fn task_tally(
    dir: &Path,
    task_id: &str,
    cache: &mut Cache,
) -> Result<BTreeMap<String, Tally>, String> {
    let document = task_execution::inspection_document(dir, task_id, true)?
        .ok_or_else(|| format!("not in {}", dir.display()))?;
    let invoked = &mut |id: &str| tasks::node_of(dir, id, cache);
    tally(&document, invoked, &mut |plan| compiled_graph(dir, plan))
}

/// One Task state directory, and every Worker's Attempts in it or why it cannot be read.
struct Store {
    shown: String,
    tallies: Result<BTreeMap<String, Tally>, String>,
}

/// A Store the Tasks pane would refuse is refused here with the same words; so is one with a
/// Task whose records cannot be read, named with the Task.
fn read_store(target: &Target) -> Store {
    let dir = &target.dir;
    let listed = match tasks::not_a_store(dir) {
        Some(refusal) => Err(refusal),
        None => task_execution::list_common(dir),
    };
    let tallies = listed.and_then(|entries| {
        let mut cache = Cache::default();
        let mut total: BTreeMap<String, Tally> = BTreeMap::new();
        for entry in entries {
            let task_id = tasks::text(&entry["task_id"])?;
            let counted = task_tally(dir, task_id, &mut cache)
                .map_err(|error| format!("Task {task_id}: {error}"))?;
            for (worker, tally) in counted {
                total.entry(worker).or_default().add(&tally);
            }
        }
        Ok(total)
    });
    Store {
        shown: target.shown.clone(),
        tallies,
    }
}

#[derive(Default)]
pub(crate) struct WorkersPane {
    title: String,
    root: Option<PathBuf>,
    /// The pane was opened in this scope: from then on every load reads.
    wanted: bool,
    /// The commit the entries were read from.
    commit: String,
    entries: Vec<Entry>,
    /// Why the last read of `HEAD` failed; the entries shown are the previous read's.
    error: Option<String>,
    /// The Task state directories the Tasks pane reads for this scope.
    targets: Vec<Target>,
    /// Why the scope names no Task state.
    no_state: Option<String>,
    stores: Vec<Store>,
    selected: Option<String>,
    rows: Vec<Row>,
    /// The first row of the opened Worker's PROMPT section.
    prompt_row: usize,
}

impl WorkersPane {
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected.as_deref()?)
    }

    /// The file behind a bar entry, for `gf` on the bar: its prompt when `HEAD` commits one,
    /// otherwise its declaration.
    pub(crate) fn entry_file(&self, id: &str) -> Option<PathBuf> {
        let (root, entry) = (self.root.as_ref()?, self.entry(id)?);
        Some(match entry.prompt {
            Some(_) => root.join(entry.prompt_path()),
            None => root.join(&entry.id),
        })
    }

    /// Everything read for one repository, dropped when the scope names another.
    fn forget(&mut self) {
        self.wanted = false;
        self.commit.clear();
        self.entries.clear();
        self.error = None;
        self.stores.clear();
        self.selected = None;
    }

    /// Read `HEAD` and the Stores. On a failed read of `HEAD` the previous entries of the same
    /// repository stay, marked by the error.
    fn read(&mut self) {
        let Some(root) = self.root.clone() else {
            // The user scope lists no Worker: Workers belong to a project.
            self.commit.clear();
            self.entries.clear();
            self.error = None;
            self.stores.clear();
            return;
        };
        match discover(&root) {
            Ok(found) => {
                self.commit = found.commit;
                self.entries = found.entries;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.stores = self.targets.iter().map(read_store).collect();
    }

    /// `HEAD` may have moved since the entries were read; opening one reads again then.
    fn ensure_current(&mut self) {
        let Some(root) = &self.root else {
            return;
        };
        if self.error.is_none() && head(root).is_ok_and(|commit| commit == self.commit) {
            return;
        }
        self.read();
    }

    fn rebuild(&mut self) {
        let (rows, prompt_row) = match self.selected_entry() {
            Some(entry) => self.entry_rows(entry),
            None => (self.folder_rows(), usize::MAX),
        };
        self.rows = rows;
        self.prompt_row = prompt_row;
    }

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted(&self.title, Paint::Title), Row::blank()];
        if let Some(error) = &self.error {
            let stale = if self.entries.is_empty() {
                "nothing is listed"
            } else {
                "the entries below are from the last successful read"
            };
            let error = format!("HEAD could not be read; {stale}: {error}");
            rows.push(Row::painted(error, Paint::Error));
            rows.push(Row::blank());
        }
        if self.root.is_none() {
            let project = "Workers belong to a project; :cd into a repository.";
            rows.push(Row::plain(project));
            return rows;
        }
        if self.entries.is_empty() && self.error.is_none() {
            let none = "No Worker committed under .af/workers/, .af/task-packages/, \
                        .af/packages/ or .af/vendor/.";
            rows.push(Row::plain(none));
        }
        for (source, _) in SOURCES {
            let listed: Vec<&Entry> = self.entries.iter().filter(|e| e.source == source).collect();
            if listed.is_empty() {
                continue;
            }
            rows.push(Row::painted(source, Paint::Title));
            for entry in listed {
                let mark = if entry.drifted.is_empty() { "" } else { " *" };
                let (label, directory) = (&entry.label, &entry.directory);
                rows.push(Row::plain(format!("  {label:<26} {directory}{mark}")));
            }
        }
        rows.extend(self.store_refusals());
        rows
    }

    /// Why the scope's Task state, or one of its Stores, cannot be read.
    fn store_refusals(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(error) = &self.no_state {
            rows.push(Row::painted(error, Paint::Error));
        }
        for store in &self.stores {
            if let Err(error) = &store.tallies {
                let refusal = format!("{}: this Store cannot be read: {error}", store.shown);
                rows.push(Row::painted(refusal, Paint::Error));
            }
        }
        rows
    }

    /// The opened Worker's rows, and the first row of its PROMPT section.
    fn entry_rows(&self, entry: &Entry) -> (Vec<Row>, usize) {
        let mut rows = Vec::new();
        if let Some(error) = &self.error {
            let error =
                format!("HEAD could not be read; shown from the last successful read: {error}");
            rows.push(Row::painted(error, Paint::Error));
        }
        if !entry.drifted.is_empty() {
            let files: Vec<&str> = entry
                .drifted
                .iter()
                .map(|file| file.rsplit('/').next().unwrap_or(file))
                .collect();
            let note = format!(
                "working tree differs from HEAD in {}: shown as committed; gf opens the file",
                files.join(", ")
            );
            rows.push(Row::painted(note, Paint::Muted));
        }
        rows.push(Row::painted(
            format!("WORKER  {}", entry.label),
            Paint::Title,
        ));
        rows.push(Row::blank());
        let file = entry.id.rsplit('/').next().unwrap_or(&entry.id);
        rows.push(Row::painted(
            format!("IDENTITY  {file} at HEAD"),
            Paint::Title,
        ));
        rows.extend(identity_rows(entry));
        rows.push(rule());
        let state = "STATE  Attempts in this scope's Task Stores";
        rows.push(Row::painted(state, Paint::Title));
        rows.extend(self.state_rows(entry));
        rows.push(rule());
        let prompt_row = rows.len();
        let prompt = entry.kind.prompt();
        rows.push(Row::painted(
            format!("PROMPT  {prompt} at HEAD"),
            Paint::Title,
        ));
        match &entry.prompt {
            Some(text) => rows.extend(text.lines().map(Row::plain)),
            None => {
                let kind = entry.kind.word();
                let none = format!("This package commits no {prompt}.");
                let only =
                    format!("The kernel sends a {kind} Worker that file; nothing else is guessed.");
                rows.push(Row::painted(none, Paint::Muted));
                rows.push(Row::painted(only, Paint::Muted));
            }
        }
        (rows, prompt_row)
    }

    fn state_rows(&self, entry: &Entry) -> Vec<Row> {
        let mut rows = self.store_refusals();
        let name = match (&entry.pin, &entry.name) {
            (Pin::Here { .. }, Some(name)) => name,
            _ => {
                let lock = entry.kind.lock();
                let unbound = "No plan binds this package: a plan binds a Worker by its name,";
                let pins = format!("and {lock} pins no name at this package.");
                rows.push(Row::painted(unbound, Paint::Muted));
                rows.push(Row::painted(pins, Paint::Muted));
                return rows;
            }
        };
        let mut total = Tally::default();
        for store in &self.stores {
            if let Ok(tallies) = &store.tallies
                && let Some(tally) = tallies.get(name)
            {
                total.add(tally);
            }
        }
        if total == Tally::default() {
            let none = "No Attempt in this scope's Task Stores ran a slot bound to this Worker.";
            rows.push(Row::plain(none));
            return rows;
        }
        rows.extend(tally_rows(&total));
        rows
    }
}

/// The STATE counts, tokens and wall of a Worker's Attempts.
pub(crate) fn tally_rows(tally: &Tally) -> Vec<Row> {
    let attempts = format!(
        "attempts  reserved {}  settled ok {}  settled failed {}  released {}",
        tally.open, tally.ok, tally.failed, tally.released
    );
    let wall = match tally.walls {
        0 => "wall      -  (no Attempt recorded a wall)".to_owned(),
        walls => format!(
            "wall      {}  ({walls} of {} Attempts recorded a wall)",
            tasks::duration(tally.wall_ms),
            tally.attempts()
        ),
    };
    vec![
        Row::plain(attempts),
        Row::plain(format!("tokens    charged {}", tally.tokens)),
        Row::plain(wall),
    ]
}

fn rule() -> Row {
    Row::painted("-".repeat(RULE), Paint::Muted)
}

/// A declared field as text, `-` when the declaration does not carry it.
fn field(value: &Value, path: &[&str]) -> String {
    let mut value = Some(value);
    for key in path {
        value = value.and_then(|value| value.get(key));
    }
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Integer(number)) => number.to_string(),
        _ => "-".to_owned(),
    }
}

/// A declared list of words, joined; `-` when the declaration does not carry it.
fn words(value: &Value, path: &[&str]) -> String {
    let mut value = Some(value);
    for key in path {
        value = value.and_then(|value| value.get(key));
    }
    let listed: Vec<&str> = value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if listed.is_empty() {
        "-".to_owned()
    } else {
        listed.join(", ")
    }
}

/// The runner a declaration names: its kind; its program, or the provider kind of a model
/// runner; its model and effort; and its args as declared. A reviewer declares its model and
/// effort in its args, and they are read the way the kernel reads them.
fn runner_rows(entry: &Entry, value: &Value) -> Vec<Row> {
    let runner = value
        .get("runner")
        .cloned()
        .unwrap_or(Value::Boolean(false));
    let command = runner.get("command").unwrap_or(&runner);
    let program = command.get("program").and_then(Value::as_str);
    let provider = runner.get("provider_kind").and_then(Value::as_str);
    let (mut model, mut effort) = (field(&runner, &["model"]), field(&runner, &["effort"]));
    if entry.kind == Kind::Reviewer
        && let Ok(manifest) = toml::from_str(&entry.declaration)
        && let Ok(settings) = review_config::lock::reviewer_runner_settings_from_manifest(&manifest)
    {
        (model, effort) = (settings.model, settings.effort);
    }
    let what = match (program, provider) {
        (Some(program), _) => format!("program {program}"),
        (None, Some(provider)) => format!("provider {provider}"),
        (None, None) => "program -".to_owned(),
    };
    let args: Vec<&str> = command
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.get("value").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let args = if args.is_empty() {
        "-".to_owned()
    } else {
        shell_words::join(args)
    };
    let kind = field(&runner, &["kind"]);
    vec![
        Row::plain(format!(
            "runner    kind {kind}  {what}  model {model}  effort {effort}"
        )),
        Row::plain(format!("args      {args}")),
    ]
}

fn pin_text(entry: &Entry) -> String {
    let lock = entry.kind.lock();
    let name = entry.name.as_deref().unwrap_or("-");
    match &entry.pin {
        Pin::Here { version, digest } => {
            let version = version.as_deref().unwrap_or("-");
            let digest = digest
                .as_deref()
                .map_or("-".to_owned(), |d| tasks::short(Some(d)));
            format!("{lock}  {version}  {digest}")
        }
        Pin::Elsewhere(path) => format!("pinned elsewhere: {lock} pins {name} at {path}"),
        Pin::Unpinned => "unpinned".to_owned(),
    }
}

fn identity_rows(entry: &Entry) -> Vec<Row> {
    let value = declared(&entry.declaration);
    let mut rows = vec![
        Row::plain(format!("name      {}", field(&value, &["name"]))),
        Row::plain(format!("version   {}", field(&value, &["version"]))),
        Row::plain(format!("schema    {}", field(&value, &["schema"]))),
        Row::plain(format!("path      {}", entry.directory)),
        Row::plain(format!("pin       {}", pin_text(entry))),
    ];
    rows.extend(runner_rows(entry, &value));
    let tokens = field(&value, &["signature", "attempt", "tokens"]);
    let wall = field(&value, &["signature", "attempt", "wall_ms"]);
    rows.push(Row::plain(format!(
        "attempt   tokens {tokens}  wall_ms {wall}"
    )));
    let effects = words(&value, &["signature", "effects"]);
    rows.push(Row::plain(format!("effects   {effects}")));
    let roles = words(&value, &["signature", "roles"]);
    rows.push(Row::plain(format!("roles     {roles}")));
    rows
}

impl Pane for WorkersPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let root = scope.toplevel().map(Path::to_path_buf);
        if root != self.root {
            self.forget();
        }
        self.root = root;
        self.title = format!("WORKERS  {}: {}", scope.word(), scope.name());
        (self.targets, self.no_state) = match tasks::targets(scope) {
            Ok(targets) => (targets, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        if self.wanted {
            self.read();
        }
        self.rebuild();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let item = |entry: &Entry| Item {
            id: entry.id.clone(),
            // The marker is in the label, so the selected row shows it too.
            label: if entry.drifted.is_empty() {
                entry.label.clone()
            } else {
                format!("{} *", entry.label)
            },
            muted: !entry.drifted.is_empty() || self.error.is_some(),
            children: None,
        };
        let mut groups = Vec::new();
        for (source, _) in SOURCES {
            let listed: Vec<Item> = self
                .entries
                .iter()
                .filter(|entry| entry.source == source)
                .map(item)
                .collect();
            if !listed.is_empty() {
                groups.push((source, listed));
            }
        }
        if groups.len() < 2 {
            return groups.into_iter().flat_map(|(_, items)| items).collect();
        }
        groups
            .into_iter()
            .map(|(source, children)| {
                let folder = source.trim_start_matches(".af/");
                Item {
                    id: source.to_owned(),
                    label: format!("{folder} ({})", children.len()),
                    muted: false,
                    children: Some(children),
                }
            })
            .collect()
    }

    fn open(&mut self, item: Option<&str>) {
        if self.wanted {
            self.ensure_current();
        } else {
            self.wanted = true;
            self.read();
        }
        self.selected = item
            .filter(|id| self.entry(id).is_some())
            .map(str::to_owned);
        self.rebuild();
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn legend(&self) -> &'static str {
        match self.selected {
            Some(_) => "j/k scroll  gf open file  y yank name  R re-read  Tab bar",
            None => "j/k move  R re-read  Tab bar  :cmd  q quit",
        }
    }

    /// `R` reads, opened before or not; a load for another scope first forgets the old one.
    fn refresh(&mut self, scope: &Scope) -> Result<(), String> {
        self.load(scope)?;
        if !self.wanted {
            self.wanted = true;
            self.read();
            self.rebuild();
        }
        Ok(())
    }

    /// The prompt's working-tree file below the PROMPT rule when `HEAD` commits one; the
    /// declaration otherwise.
    fn file(&self, row: usize) -> Option<PathBuf> {
        let (root, entry) = (self.root.as_ref()?, self.selected_entry()?);
        Some(match entry.prompt {
            Some(_) if row >= self.prompt_row => root.join(entry.prompt_path()),
            _ => root.join(&entry.id),
        })
    }

    fn yank(&self, _row: usize) -> Option<String> {
        self.selected_entry().map(|entry| entry.label.clone())
    }
}

#[cfg(test)]
mod tests;
