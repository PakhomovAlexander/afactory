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

use super::pipelines::{CATALOG, blobs, declared, differing, head, listed, pinned_paths};
use super::tasks::{self, Cache, Invoked, Target};
use super::{Pane, Row};
use crate::task_execution;
use crate::tui::paint::{Paint, Span, Tone};
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

/// The columns of a section rule: the main pane beside the bar at 100 columns.
const RULE: usize = 100 - crate::tui::BAR_WIDTH;
const LOCK: &str = ".af/af.lock";

/// What a declaration makes: which lock pins it, and which file the kernel sends as its prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
            Kind::Task => rest == "worker.toml" || rest.ends_with("/worker.toml"),
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

/// For each shown Worker (its declaration, its prompt, and whether `commit` holds that prompt),
/// the files whose working-tree copy differs from `commit`, from one `git diff` for them all.
/// `git diff` ignores an untracked file, so a prompt `commit` lacks differs when the working
/// tree has one, a link included. A comparison git cannot make counts as a difference.
fn drift(root: &Path, commit: &str, shown: &[(&str, &str, bool)]) -> Vec<Vec<String>> {
    let mut tracked = Vec::new();
    for (declaration, prompt, committed) in shown {
        tracked.push(*declaration);
        if *committed {
            tracked.push(*prompt);
        }
    }
    let differ = differing(root, commit, &tracked);
    shown
        .iter()
        .map(|(declaration, prompt, committed)| {
            let mut drifted = Vec::new();
            if differ.contains(*declaration) {
                drifted.push((*declaration).to_owned());
            }
            let prompt_drifts = if *committed {
                differ.contains(*prompt)
            } else {
                std::fs::symlink_metadata(root.join(prompt)).is_ok()
            };
            if prompt_drifts {
                drifted.push((*prompt).to_owned());
            }
            drifted
        })
        .collect()
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
    // Every Worker source, the locks and the prompts live under `.af/`: nothing else is listed.
    let objects = listed(root, &commit, ".af")?;
    let paths: BTreeSet<String> = objects.keys().cloned().collect();
    // Every declaration, in bar order, with the prompt the kernel would send beside it.
    let mut declared_at = Vec::new();
    for (source, kind) in SOURCES {
        for id in paths
            .iter()
            .filter(|path| source_of(path) == Some((source, kind)))
        {
            let directory = id.rsplit_once('/').map_or("", |(dir, _)| dir).to_owned();
            let prompt_path = format!("{directory}/{}", kind.prompt());
            declared_at.push((source, kind, id.clone(), directory, prompt_path));
        }
    }
    // One read of every committed file the pane shows, and one drift check of them all: an
    // open reads again each time, so it must not cost a git process per Worker.
    let mut wanted: Vec<&str> = vec![CATALOG, LOCK];
    for (_, _, id, _, prompt_path) in &declared_at {
        wanted.push(id);
        wanted.push(prompt_path);
    }
    let wanted: Vec<&str> = wanted
        .into_iter()
        .filter_map(|path| objects.get(path).map(String::as_str))
        .collect();
    let shown: Vec<(&str, &str, bool)> = declared_at
        .iter()
        .map(|(_, _, id, _, prompt)| (id.as_str(), prompt.as_str(), paths.contains(prompt)))
        .collect();
    // The read and the drift check are independent processes: run them side by side.
    let (blobs, drifts) = std::thread::scope(|scope| {
        let drifts = scope.spawn(|| drift(root, &commit, &shown));
        let blobs = blobs(root, &wanted);
        (blobs, drifts.join())
    });
    // By path, as the rest of discovery asks.
    let read = blobs?;
    let blobs: BTreeMap<&str, &Vec<u8>> = objects
        .iter()
        .filter_map(|(path, object)| read.get(object).map(|bytes| (path.as_str(), bytes)))
        .collect();
    let mut drifts = drifts
        .map_err(|_| "checking the working tree panicked")?
        .into_iter();
    let text = |path: &str| -> Result<Option<String>, String> {
        match blobs.get(path) {
            Some(bytes) => String::from_utf8((*bytes).clone())
                .map(Some)
                .map_err(|error| format!("{path}: {error}")),
            None => Ok(None),
        }
    };
    let lock = |file: &str| -> Result<Value, String> {
        let committed = text(file)?;
        Ok(committed.map_or(Value::Table(toml::map::Map::new()), |text| declared(&text)))
    };
    let catalog = pins(Kind::Task, &lock(CATALOG)?);
    let reviewers = pins(Kind::Reviewer, &lock(LOCK)?);
    for (source, kind, id, directory, prompt_path) in declared_at {
        let declaration = text(&id)?.ok_or_else(|| format!("{id}: not a file at HEAD"))?;
        let value = declared(&declaration);
        let name = value.get("name").and_then(Value::as_str).map(str::to_owned);
        let fallback = directory.strip_prefix(source).unwrap_or(&directory);
        // The prompt is shown as text, whatever its bytes: a stray byte is not a refusal.
        let prompt = blobs
            .get(prompt_path.as_str())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        let pins = match kind {
            Kind::Reviewer => &reviewers,
            Kind::Task => &catalog,
        };
        found.entries.push(Entry {
            id,
            source,
            kind,
            label: name.clone().unwrap_or_else(|| fallback.to_owned()),
            pin: pin_of(pins, name.as_deref(), &directory),
            name,
            directory,
            declaration,
            prompt,
            drifted: drifts.next().unwrap_or_default(),
        });
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

/// A Worker package as a plan binds it: the kind of package, its name, and the package digest
/// the plan bound (`None` when the plan records none). A reviewer and a Task package may share
/// a name, and one name may be bound at several digests; each is its own Worker.
pub(crate) type Worker = (Kind, String, Option<String>);

/// One reservation of a Worker's slot, as its records leave it.
struct Reservation {
    worker: Worker,
    released: bool,
    /// `Some(succeeded)` once settled.
    settled: Option<bool>,
    charged: u128,
}

/// A recorded plan as the pane reads it: `{"graph": <compiled graph>, "bindings": <the plan's
/// effective Worker bindings by slot>}`.
fn recorded_plan(graph: &Json, bindings: &Json) -> Json {
    serde_json::json!({"graph": graph, "bindings": bindings})
}

/// The artifact type a captured Review binds a reviewer slot to, in place of the package.
const REVIEW_DEPENDENCY: &str = "af/LegacyReviewDependency@1";

/// A plan's bindings with each `package_digest` as the committed pin would record it. A Task
/// binds a slot to the package itself, so its digest is the pin's. A captured Review binds a
/// reviewer slot to a Review dependency whose own digest covers Campaign and pipeline data; the
/// reviewer package it ran is its `original_package_digest` (none for a slot no reviewer
/// package fills). `package` reads a recorded artifact.
fn pinned_bindings(
    bindings: &Json,
    package: &mut dyn FnMut(&str) -> Result<Json, String>,
) -> Result<Json, String> {
    let mut pinned = bindings.clone();
    let Some(slots) = pinned.as_object_mut() else {
        return Ok(pinned);
    };
    for binding in slots.values_mut() {
        let Some(id) = binding["package_artifact_id"].as_str() else {
            continue;
        };
        let artifact = package(id)?;
        if artifact["type"] == REVIEW_DEPENDENCY {
            binding["package_digest"] = artifact["payload"]["original_package_digest"].clone();
        }
    }
    Ok(pinned)
}

/// The Worker a node of a recorded plan runs: the package the plan binds to the node's one
/// slot. A node that names no slot, or several (a Provider admission, an optimization
/// experiment), runs no single Worker.
pub(crate) fn worker_of(plan: &Json, node: &str) -> Option<Worker> {
    operator_worker(plan, &plan["graph"]["nodes"][node]["operator"])
}

/// The Worker a compiled operator runs: a reviewer package for a Review-domain operation, a
/// Task Worker package for a primitive, at the digest the plan's binding of its slot records.
fn operator_worker(plan: &Json, operator: &Json) -> Option<Worker> {
    let (kind, slot) = match operator["kind"].as_str()? {
        "primitive" => (Kind::Task, operator["operator"]["slot"].as_str()?),
        "review_domain" => (Kind::Reviewer, operator["operation"]["slot"].as_str()?),
        _ => return None,
    };
    let worker = plan["graph"]["slots"][slot]["worker"].as_str()?;
    let digest = plan["bindings"][slot]["package_digest"].as_str();
    Some((kind, worker.to_owned(), digest.map(str::to_owned)))
}

/// Each owned child invocation a Task registered, to the invocation of the node that owns it.
fn owners(document: &Json) -> Result<BTreeMap<&str, &str>, String> {
    let mut owners = BTreeMap::new();
    for set in tasks::array(&document["owned_child_sets"]) {
        let parent = tasks::text(&set["record"]["parent_invocation_id"])?;
        for child in tasks::array(&set["record"]["children"]) {
            owners.insert(tasks::text(&child["invocation_id"])?, parent);
        }
    }
    Ok(owners)
}

/// Per Worker, the Attempts one Task's `af task explain --json` document records. A
/// reservation's node comes from its invocation (`invoked`), and its Worker from that node's
/// slot in the plan the invocation ran under: the document's own graph and bindings for its
/// current plan, `recorded` (see `recorded_plan`) for an earlier one. Never from a name. An owned child (a
/// Review shard such as `parent.slice1`) is no node of the graph: it runs the operator the
/// graph's `owned_children` records for the node whose invocation registered it.
///
/// The accounting is the Tasks pane's: an Attempt's charge is the highest its settlement or a
/// usage observation records, and a released reservation is no Attempt.
pub(crate) fn tally(
    document: &Json,
    invoked: &mut dyn FnMut(&str) -> Result<Invoked, String>,
    recorded: &mut dyn FnMut(&str) -> Result<Json, String>,
    package: &mut dyn FnMut(&str) -> Result<Json, String>,
) -> Result<BTreeMap<Worker, Tally>, String> {
    let current = document["plan_id"].as_str();
    let owners = owners(document)?;
    let mut graphs: BTreeMap<String, Json> = BTreeMap::new();
    let mut reservations: BTreeMap<String, Reservation> = BTreeMap::new();
    for entry in tasks::array(&document["execution_records"]) {
        let record = &entry["record"];
        let Some(attempt) = record["attempt_id"].as_str() else {
            continue;
        };
        match record["kind"].as_str() {
            Some("reserved") => {
                let invocation = tasks::text(&record["invocation_id"])?;
                let run = invoked(invocation)?;
                let plan = run.plan_id.as_deref().or(current);
                let plan = plan.ok_or("a reservation names no plan")?;
                if !graphs.contains_key(plan) {
                    let compiled = if Some(plan) == current {
                        let bindings = pinned_bindings(&document["plan"]["bindings"], package)?;
                        recorded_plan(&document["graph"], &bindings)
                    } else {
                        recorded(plan)?
                    };
                    graphs.insert(plan.to_owned(), compiled);
                }
                let compiled = &graphs[plan];
                let worker = match owners.get(invocation) {
                    Some(parent) if compiled["graph"]["nodes"].get(&run.node).is_none() => {
                        let owner = invoked(parent)?;
                        let template = &compiled["graph"]["owned_children"][&owner.node];
                        operator_worker(compiled, &template["operator"])
                    }
                    _ => worker_of(compiled, &run.node),
                };
                if let Some(worker) = worker {
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
    let mut tallies: BTreeMap<Worker, Tally> = BTreeMap::new();
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

/// A recorded plan's bindings, and its compiled graph through the plan's `compiled_graph_id`.
fn plan_of(dir: &Path, plan: &str) -> Result<Json, String> {
    let plan = task_execution::recorded_artifact(dir, plan)?;
    let graph = tasks::text(&plan["payload"]["compiled_graph_id"])?;
    let graph = &task_execution::recorded_artifact(dir, graph)?["payload"];
    let package = &mut |id: &str| task_execution::recorded_artifact(dir, id);
    let bindings = pinned_bindings(&plan["payload"]["bindings"], package)?;
    Ok(recorded_plan(graph, &bindings))
}

fn task_tally(
    dir: &Path,
    task_id: &str,
    cache: &mut Cache,
) -> Result<BTreeMap<Worker, Tally>, String> {
    let document = task_execution::inspection_document(dir, task_id, true)?
        .ok_or_else(|| format!("not in {}", dir.display()))?;
    let invoked = &mut |id: &str| tasks::node_of(dir, id, cache);
    let package = &mut |id: &str| task_execution::recorded_artifact(dir, id);
    tally(&document, invoked, &mut |plan| plan_of(dir, plan), package)
}

/// One Task state directory, and every Worker's Attempts in it or why it cannot be read.
struct Store {
    shown: String,
    tallies: Result<BTreeMap<Worker, Tally>, String>,
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
        let mut total: BTreeMap<Worker, Tally> = BTreeMap::new();
        for entry in entries {
            // A collected Task (ADR-0135) has no recorded Attempts left to tally.
            if entry.get("collected").is_some() {
                continue;
            }
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

    /// The declared name behind a bar entry, without the drift marker its label carries; none
    /// when the declaration names none (its label is then only its directory).
    pub(crate) fn entry_name(&self, id: &str) -> Option<String> {
        self.entry(id)?.name.clone()
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
            Err(error) => {
                // The kept entries still show the working tree's drift from their commit.
                let prompts: Vec<String> = self.entries.iter().map(Entry::prompt_path).collect();
                let shown: Vec<(&str, &str, bool)> = self
                    .entries
                    .iter()
                    .zip(&prompts)
                    .map(|(entry, prompt)| {
                        (entry.id.as_str(), prompt.as_str(), entry.prompt.is_some())
                    })
                    .collect();
                let drifts = drift(&root, &self.commit, &shown);
                for (entry, drifted) in self.entries.iter_mut().zip(drifts) {
                    entry.drifted = drifted;
                }
                self.error = Some(error);
            }
        }
        self.stores = self.targets.iter().map(read_store).collect();
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
            rows.push(Row::error("error", error));
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
            rows.push(Row::error("error", error));
        }
        for store in &self.stores {
            if let Err(error) = &store.tallies {
                let refusal = format!("{}: this Store cannot be read: {error}", store.shown);
                rows.push(Row::error("error", refusal));
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
            rows.push(Row::error("error", error));
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
        let (name, digest) = match (&entry.pin, &entry.name) {
            (Pin::Here { digest, .. }, Some(name)) => (name, digest),
            _ => {
                let lock = entry.kind.lock();
                let unbound = "No plan binds this package: a plan binds a Worker by its name,";
                let pins = format!("and {lock} pins no name at this package.");
                rows.push(Row::painted(unbound, Paint::Muted));
                rows.push(Row::painted(pins, Paint::Muted));
                return rows;
            }
        };
        // A plan binds the package at an exact digest: only Attempts of the digest the committed
        // pin records are this package's. Other digests of the name are counted apart.
        let (mut total, mut other) = (Tally::default(), Tally::default());
        for store in &self.stores {
            let Ok(tallies) = &store.tallies else {
                continue;
            };
            for ((kind, bound, at), tally) in tallies {
                if *kind != entry.kind || bound != name {
                    continue;
                }
                if at == digest {
                    total.add(tally);
                } else {
                    other.add(tally);
                }
            }
        }
        let others = (other.attempts() + other.released > 0).then(|| {
            let count = other.attempts();
            let other = format!("other     {count} Attempts ran this name at an unpinned digest");
            Row::painted(other, Paint::Muted)
        });
        if total == Tally::default() {
            let none = "No Attempt in this scope's Task Stores ran a slot bound to this Worker.";
            rows.push(Row::plain(none));
            rows.extend(others);
            return rows;
        }
        rows.extend(tally_rows(&total));
        rows.extend(others);
        rows
    }
}

/// The STATE counts, tokens and wall of a Worker's Attempts. A count above zero of reserved,
/// settled ok or settled failed Attempts is a chip in its tone.
pub(crate) fn tally_rows(tally: &Tally) -> Vec<Row> {
    let count = |word: &str, count: u64, tone| match count {
        0 => Span::new(format!(" {word} 0 "), Paint::Plain),
        count => Span::chip(&format!("{word} {count}"), tone),
    };
    let attempts = Row {
        spans: vec![
            Span::new("attempts ", Paint::Plain),
            count("reserved", tally.open, Tone::Active),
            count("settled ok", tally.ok, Tone::Ok),
            count("settled failed", tally.failed, Tone::Fail),
            Span::new(format!(" released {}", tally.released), Paint::Plain),
        ],
    };
    let wall = match tally.walls {
        0 => "wall      -  (no Attempt recorded a wall)".to_owned(),
        walls => format!(
            "wall      {}  ({walls} of {} Attempts recorded a wall)",
            tasks::duration(tally.wall_ms),
            tally.attempts()
        ),
    };
    vec![
        attempts,
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
            tone: None,
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
                    tone: None,
                    children: Some(children),
                }
            })
            .collect()
    }

    /// Every open reads again: `HEAD`, the working tree's drift and the Stores all move under
    /// an open browser.
    fn open(&mut self, item: Option<&str>) {
        self.wanted = true;
        self.read();
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

    /// The prompt's working-tree file at and below the PROMPT rule, committed or not: that is
    /// the file the kernel would send. The declaration above it.
    fn file(&self, row: usize) -> Option<PathBuf> {
        let (root, entry) = (self.root.as_ref()?, self.selected_entry()?);
        Some(if row >= self.prompt_row {
            root.join(entry.prompt_path())
        } else {
            root.join(&entry.id)
        })
    }

    fn yank(&self, _row: usize) -> Option<String> {
        self.selected_entry()?.name.clone()
    }
}

#[cfg(test)]
mod tests;
