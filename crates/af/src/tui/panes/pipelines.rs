//! Pipelines: `.af/pipelines/*.toml` and every `pipeline.toml` under `.af/task-packages/`, as
//! committed at `HEAD`. The plan preview compiles committed authority, so the bar names what
//! `HEAD` holds; a working-tree file that differs is marked, and `gf` still opens it.
//!
//! A package's main pane is, verbatim, the text `af task explain --tree` prints for a plan of
//! it that `af task plan` would capture: the same token-free compilation of the committed
//! authority, into a scratch Store that is discarded, on a thread the event loop polls. No
//! Store is written, nothing is admitted and no Worker runs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use toml::Value;

use super::{Effect, Pane, Row, SPINNER};
use crate::task_execution::{self, TreePreview};
use crate::tui::keymap::Key;
use crate::tui::paint::Paint;
use crate::tui::scope::Scope;
use crate::tui::tree::Item;
use crate::tui::tree::Tab;

/// The committed catalog that pins Pipeline and Worker packages by name.
pub(crate) const CATALOG: &str = ".af/task-catalog.toml";

/// The Task ID of every preview; it names no Task in any Store.
pub(crate) const PREVIEW_TASK_ID: &str = "pipeline-preview";

/// The Task file the pane plans for a package: its first accepted kind, the package selected
/// by name with no fallback, and limits wide enough for any bounded plan. `af task plan --file`
/// on this file prints the same tree.
pub(crate) fn preview_task(name: &str, kind: &str, max_attempts: u64) -> serde_json::Value {
    serde_json::json!({
        "schema": "af.task-file/1",
        "task_id": PREVIEW_TASK_ID,
        "kind": kind,
        "goal": format!("Preview the {name} Pipeline"),
        "pipeline": {"name": name, "fallback": "refuse"},
        "strategy": "preview",
        "facts": {},
        "limits": {
            "tokens": 1_000_000,
            "max_attempts": max_attempts,
            "wall_ms": 3_600_000,
            "verification": {
                "tokens": 100_000,
                "attempts": max_attempts.min(2),
                "wall_ms": 600_000
            }
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// A review Pipeline under `.af/pipelines/`: planned against a diff, not a Task.
    Review,
    /// A pipeline package, with the kind and Attempt bound its preview Task takes.
    Package {
        kind: String,
        max_attempts: u64,
        pin: Pin,
    },
}

/// Whether the committed catalog pins this file's package name at this file: only then does a
/// plan of the name plan this file, so only then is a preview compiled for it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pin {
    Here,
    /// The catalog pins the name at another package directory.
    Elsewhere(String),
    /// The catalog does not pin the name at all.
    Absent,
}

#[derive(Clone, Debug)]
struct Entry {
    /// The repository-relative path of the TOML file.
    id: String,
    label: String,
    /// The working-tree file, for `gf`.
    path: PathBuf,
    /// The file as committed at `HEAD`: what every preview and declaration is read from.
    declaration: String,
    /// The working-tree file differs from `HEAD` or is gone.
    modified: bool,
    source: Source,
}

type Compiled = Result<TreePreview, String>;

/// What one read of `HEAD` found.
#[derive(Debug)]
struct Discovery {
    /// The commit every declaration was read from; empty for an unborn `HEAD`.
    commit: String,
    entries: Vec<Entry>,
}

#[derive(Default)]
pub(crate) struct PipelinesPane {
    root: Option<PathBuf>,
    /// The commit the entries were read from, so a preview never compiles a different `HEAD`.
    commit: String,
    entries: Vec<Entry>,
    /// Why the last read of `HEAD` failed; the entries shown are the previous read's.
    error: Option<String>,
    selected: Option<String>,
    previews: BTreeMap<String, Compiled>,
    /// The entry being compiled, and the commit it was compiled at.
    job: Option<(String, String, Receiver<Compiled>)>,
    rows: Vec<Row>,
    /// The entry each row of the folder's listing names; empty while an entry is opened.
    named: BTreeMap<usize, String>,
    spinner: usize,
}

impl PipelinesPane {
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected.as_deref()?)
    }

    /// The bar id of the package the committed catalog pins under `name`: the Pipeline a Task
    /// selected by that name ran, not another file that declares the same name.
    /// The same, from a fresh read of `HEAD`: `p` names the package the catalog pins now.
    pub(crate) fn pinned_entry_now(&mut self, name: &str) -> Result<Option<String>, String> {
        self.ensure_current();
        match &self.error {
            // The entries are the last good read's; a jump from them could open a package the
            // catalog no longer pins.
            Some(error) => Err(format!("HEAD could not be read: {error}")),
            None => Ok(self.pinned_entry(name)),
        }
    }

    pub(crate) fn pinned_entry(&self, name: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|entry| {
                entry.label == name
                    && matches!(&entry.source, Source::Package { pin: Pin::Here, .. })
            })
            .map(|entry| entry.id.clone())
    }

    /// The pipeline file behind a bar entry, for `gf` on the bar.
    pub(crate) fn entry_file(&self, id: &str) -> Option<PathBuf> {
        self.entry(id).map(|entry| entry.path.clone())
    }

    /// Read `HEAD` again. On failure the previous entries of the same repository stay, marked
    /// by the error; nothing is compiled from them until a read succeeds.
    fn discover(&mut self) {
        self.previews.clear();
        self.job = None;
        let Some(root) = self.root.clone() else {
            self.entries.clear();
            self.commit.clear();
            self.error = None;
            return;
        };
        match discover(&root) {
            Ok(found) => {
                self.entries = found.entries;
                self.commit = found.commit;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// Everything read for one repository, dropped when the scope names another: entries of
    /// project A must never be listed, opened or edited under project B.
    fn forget(&mut self) {
        self.entries.clear();
        self.commit.clear();
        self.error = None;
        self.selected = None;
        self.previews.clear();
        self.job = None;
    }

    /// The rows above an entry's own: the failed-read notice and the working-tree notice.
    fn notice_rows(&self, entry: &Entry) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(error) = &self.error {
            rows.push(Row::error(
                "error",
                format!("HEAD could not be read; shown from the last successful read: {error}"),
            ));
        }
        if entry.modified {
            let note = "working tree differs from HEAD: shown as committed; gf opens the file";
            rows.push(Row::painted(note, Paint::Muted));
        }
        rows
    }

    /// `HEAD` may have moved since the entries were read; a preview compiles `HEAD`, so the
    /// entries are read again first.
    fn ensure_current(&mut self) {
        let Some(root) = &self.root else {
            return;
        };
        // A failed read is never trusted to still be current: once HEAD reads again, even at
        // the same commit, the entries are read again and the error cleared.
        if self.error.is_none() && head(root).is_ok_and(|commit| commit == self.commit) {
            return;
        }
        self.discover();
    }

    fn compile(&mut self) {
        self.ensure_current();
        if self.error.is_some() {
            // Stale entries are shown, never planned: a preview would compile a HEAD the
            // entries were not read from.
            return;
        }
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let Source::Package {
            kind,
            max_attempts,
            pin,
        } = &entry.source
        else {
            return;
        };
        let running = self.job.as_ref().is_some_and(|(id, _, _)| *id == entry.id);
        if running || self.previews.contains_key(&entry.id) {
            return;
        }
        let root = match &self.root {
            Some(root) => root.clone(),
            None => return,
        };
        // A plan selects a package by name through the committed catalog. A file the catalog
        // does not pin at this path would show another file's plan, or none: say so instead.
        let refused = match pin {
            Pin::Here => None,
            Pin::Elsewhere(path) => Some(format!(
                "not previewed: the committed catalog pins `{}` at {path}, not at this file",
                entry.label
            )),
            Pin::Absent => Some(format!(
                "not previewed: the committed catalog does not pin `{}`; the compiler would refuse it",
                entry.label
            )),
        };
        if let Some(refused) = refused {
            self.previews.insert(entry.id, Err(refused));
            return;
        }
        let task = preview_task(&entry.label, kind, *max_attempts);
        // The preview compiles the exact commit the entries were read from, not a `HEAD` that
        // may move before the thread gets to it.
        let commit = self.commit.clone();
        let at = commit.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(plan(&root, &task, &at));
        });
        self.job = Some((entry.id, commit, receiver));
    }

    fn rebuild(&mut self) {
        (self.rows, self.named) = match self.selected_entry() {
            Some(entry) => (self.entry_rows(entry), BTreeMap::new()),
            None => self.folder_rows(),
        };
    }

    fn entry_rows(&self, entry: &Entry) -> Vec<Row> {
        let heading = format!("PIPE  {} ({})", entry.label, entry.id);
        let mut rows = self.notice_rows(entry);
        match (&entry.source, self.previews.get(&entry.id)) {
            (Source::Review, _) => {
                let title = format!("{heading}  [review pipeline]");
                rows.push(Row::painted(title, Paint::Title));
                rows.push(Row::blank());
                let why = "A review Pipeline is planned against a diff selector the browser \
                           does not hold; its declaration, as committed:";
                rows.push(Row::painted(why, Paint::Muted));
                rows.push(Row::painted(
                    format!("  af review plan --pipeline {}", entry.id),
                    Paint::Muted,
                ));
                rows.push(Row::blank());
                rows.extend(entry.declaration.lines().map(Row::plain));
            }
            (Source::Package { .. }, Some(Ok(preview))) => {
                rows.extend(preview.text.lines().map(Row::plain));
            }
            (Source::Package { .. }, Some(Err(error))) => {
                // The compiler refused the preview Task: a package with required facts or
                // public inputs cannot be planned from an invented Task. Its public contract is
                // what the browser can show without inventing business inputs.
                rows.push(Row::painted(heading, Paint::Title));
                rows.push(Row::blank());
                rows.extend(Row::errors("refused", error.lines()));
                rows.push(Row::blank());
                rows.extend(contract_rows(&declared(&entry.declaration)));
            }
            (Source::Package { .. }, None) => {
                let spinner = SPINNER[self.spinner % SPINNER.len()];
                let what = "compiling a token-free plan, as af task plan does";
                rows.push(Row::painted(heading, Paint::Title));
                rows.push(Row::blank());
                rows.push(Row::painted(format!("{spinner} {what}"), Paint::Muted));
            }
        }
        rows
    }

    /// The folder's listing, and the entry each of its rows names.
    fn folder_rows(&self) -> (Vec<Row>, BTreeMap<usize, String>) {
        let mut named = BTreeMap::new();
        let mut rows = vec![Row::painted("PIPELINES", Paint::Title), Row::blank()];
        if let Some(error) = &self.error {
            let stale = if self.entries.is_empty() {
                "nothing is listed"
            } else {
                "the entries below are from the last successful read"
            };
            rows.push(Row::error(
                "error",
                format!("HEAD could not be read; {stale}: {error}"),
            ));
            rows.push(Row::blank());
        }
        if self.root.is_none() {
            rows.push(Row::plain(
                "Pipelines belong to a project; :cd into a repository.",
            ));
        } else if self.entries.is_empty() && self.error.is_none() {
            let none = "No Pipeline committed under .af/pipelines/ or .af/task-packages/.";
            rows.push(Row::plain(none));
        }
        for entry in &self.entries {
            let (label, id) = (&entry.label, &entry.id);
            let mark = if entry.modified { " *" } else { "" };
            named.insert(rows.len(), id.clone());
            rows.push(Row::plain(format!("{label:<28} {id}{mark}")));
        }
        (rows, named)
    }
}

impl Pane for PipelinesPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let root = scope.toplevel().map(Path::to_path_buf);
        if root != self.root {
            self.forget();
        }
        self.root = root;
        self.discover();
        self.compile();
        self.rebuild();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for entry in &self.entries {
            // The marker is in the label, so the selected row shows it too: paint alone is
            // overridden by the cursor. The id stays the committed path.
            let label = if entry.modified {
                format!("{} *", entry.label)
            } else {
                entry.label.clone()
            };
            items.push(Item {
                id: entry.id.clone(),
                label,
                muted: entry.modified || self.error.is_some(),
                tone: None,
                children: None,
            });
        }
        items
    }

    fn open(&mut self, item: Option<&str>) {
        self.selected = item.map(str::to_owned);
        self.compile();
        self.rebuild();
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// `Enter` on a Pipeline the folder lists opens it, as the bar does.
    fn key(&mut self, key: Key, row: usize) -> Result<Option<Effect>, String> {
        if key != Key::Enter {
            return Ok(None);
        }
        let id = self.named.get(&row).cloned();
        Ok(id.map(|id| Effect::OpenItem(Tab::Pipelines, id)))
    }

    fn legend(&self) -> &'static str {
        match self.selected {
            Some(_) => "j/k slot  R recompile  gf open TOML  y yank name  Tab bar  :cmd  q quit",
            None => "j/k move  Enter open  R re-read  Tab bar  :cmd  q quit",
        }
    }

    /// The Worker binding of the slot a highlighted plan row names.
    fn status(&self, row: usize) -> Option<String> {
        let entry = self.selected_entry()?;
        let preview = self.previews.get(&entry.id)?.as_ref().ok()?;
        // The preview's slot rows count from its first line; the notices sit above it.
        let line = row.checked_sub(self.notice_rows(entry).len())?;
        preview.slots.get(&line).cloned()
    }

    fn poll(&mut self) -> bool {
        let Some((id, commit, receiver)) = self.job.as_ref() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(compiled) => {
                // A result for a commit the pane no longer shows is dropped, not shown.
                if *commit == self.commit {
                    self.previews.insert(id.clone(), compiled);
                }
                self.job = None;
            }
            Err(TryRecvError::Empty) => self.spinner = self.spinner.wrapping_add(1),
            Err(TryRecvError::Disconnected) => {
                let stopped = Err("the plan compilation stopped".to_owned());
                self.previews.insert(id.clone(), stopped);
                self.job = None;
            }
        }
        self.rebuild();
        true
    }

    fn busy(&self) -> Option<char> {
        let spinner = SPINNER[self.spinner % SPINNER.len()];
        self.job.as_ref().map(|_| spinner)
    }

    fn file(&self, _row: usize) -> Option<PathBuf> {
        self.selected_entry().map(|entry| entry.path.clone())
    }

    fn yank(&self, _row: usize) -> Option<String> {
        self.selected_entry().map(|entry| entry.label.clone())
    }
}

/// Write the preview Task file into a scratch directory and plan it the `af task plan` way,
/// with the given commit as its authority.
fn plan(root: &Path, task: &serde_json::Value, commit: &str) -> Compiled {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let file = scratch.path().join("pipeline-preview.json");
    let bytes = serde_json::to_vec_pretty(task).map_err(|error| error.to_string())?;
    std::fs::write(&file, bytes).map_err(|error| error.to_string())?;
    task_execution::plan_tree_preview_at(&file, root, commit)
}

/// The commit `HEAD` names, or an empty string for an unborn `HEAD`: a symbolic `HEAD` whose
/// branch does not exist yet. A `HEAD` that names a missing commit, a detached `HEAD` that does
/// not resolve, no repository and no git are all errors, never an empty list.
pub(crate) fn head(root: &Path) -> Result<String, String> {
    match git(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
        Ok(bytes) => return Ok(String::from_utf8_lossy(&bytes).trim().to_owned()),
        // `--quiet` makes an unresolvable HEAD exit 1 with nothing on stderr; anything else
        // (no repository, an unreadable object store, no git) says why.
        Err(error) if error.is_empty() => {}
        Err(error) => return Err(error),
    }
    let branch = match git(root, &["symbolic-ref", "--quiet", "HEAD"]) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).trim().to_owned(),
        Err(error) if error.is_empty() => {
            return Err("HEAD is detached and names no commit".to_owned());
        }
        Err(error) => return Err(error),
    };
    match git(root, &["show-ref", "--verify", "--quiet", &branch]) {
        Err(error) if error.is_empty() => Ok(String::new()),
        Ok(_) => Err(format!("HEAD ({branch}) names a missing commit")),
        Err(error) => Err(error),
    }
}

/// Review Pipelines first, then pipeline packages, each sorted by path, all as one commit of
/// `HEAD` holds them. An unborn `HEAD` lists nothing; a read that fails is an error, never an
/// empty list.
fn discover(root: &Path) -> Result<Discovery, String> {
    let commit = head(root)?;
    let mut found = Discovery {
        commit: commit.clone(),
        entries: Vec::new(),
    };
    if commit.is_empty() {
        return Ok(found);
    }
    // Every pipeline and the catalog live under `.af/`; one listing, one read and one drift
    // check serve them all, however many there are.
    let objects = listed(root, &commit, ".af")?;
    let mut reviews = Vec::new();
    let mut packages = Vec::new();
    for path in objects.keys() {
        if let Some(rest) = path.strip_prefix(".af/pipelines/")
            && !rest.contains('/')
            && rest.ends_with(".toml")
        {
            reviews.push(path.clone());
        } else if path.starts_with(".af/task-packages/") && path.ends_with("/pipeline.toml") {
            packages.push(path.clone());
        }
    }
    let mut wanted: Vec<&str> = reviews
        .iter()
        .chain(&packages)
        .map(String::as_str)
        .collect();
    wanted.push(CATALOG);
    let ids: Vec<&str> = wanted
        .iter()
        .filter_map(|path| objects.get(*path).map(String::as_str))
        .collect();
    let shown: Vec<&str> = reviews
        .iter()
        .chain(&packages)
        .map(String::as_str)
        .collect();
    let (read, modified) = std::thread::scope(|scope| {
        let modified = scope.spawn(|| differing(root, &commit, &shown));
        (blobs(root, &ids), modified.join())
    });
    let read = read?;
    let modified = modified.map_err(|_| "checking the working tree panicked")?;
    let text = |path: &str| -> Result<Option<String>, String> {
        let Some(bytes) = objects.get(path).and_then(|id| read.get(id)) else {
            return Ok(None);
        };
        String::from_utf8(bytes.clone())
            .map(Some)
            .map_err(|error| format!("{path}: {error}"))
    };
    let committed = |path: &str| -> Result<String, String> {
        text(path)?.ok_or_else(|| format!("{path}: not a file at HEAD"))
    };
    let pinned = match text(CATALOG)? {
        Some(catalog) => pinned_paths(&declared(&catalog)),
        None => BTreeMap::new(),
    };
    for id in reviews {
        let declaration = committed(&id)?;
        let stem = Path::new(&id).file_stem().unwrap_or_default();
        found.entries.push(Entry {
            label: stem.to_string_lossy().into_owned(),
            path: root.join(&id),
            modified: modified.contains(&id),
            declaration,
            id,
            source: Source::Review,
        });
    }
    for id in packages {
        let declaration = committed(&id)?;
        let value = declared(&declaration);
        let directory = Path::new(&id).parent().unwrap_or(Path::new(""));
        let fallback = relative(Path::new(".af/task-packages"), directory);
        let name = value.get("name").and_then(Value::as_str);
        let accepts = value.get("accepts");
        let kinds = accepts.and_then(|accepts| accepts.get("kinds"));
        let first = kinds.and_then(|kinds| kinds.get(0));
        let kind = first.and_then(Value::as_str);
        let attempts = value.get("max_attempts").and_then(Value::as_integer);
        let max_attempts = attempts.and_then(|n| u64::try_from(n).ok()).unwrap_or(3);
        let label = name.map_or(fallback, str::to_owned);
        let pin = match pinned.get(&label) {
            Some(path) if Path::new(path) == directory => Pin::Here,
            Some(path) => Pin::Elsewhere(path.clone()),
            None => Pin::Absent,
        };
        found.entries.push(Entry {
            label,
            path: root.join(&id),
            modified: modified.contains(&id),
            declaration,
            id,
            source: Source::Package {
                kind: kind.unwrap_or("implement").to_owned(),
                max_attempts: max_attempts.max(1),
                pin,
            },
        });
    }
    Ok(found)
}

/// Package name to package directory, as the committed catalog's `[packages]` table pins them.
pub(crate) fn pinned_paths(catalog: &Value) -> BTreeMap<String, String> {
    let mut pinned = BTreeMap::new();
    if let Some(packages) = catalog.get("packages").and_then(Value::as_table) {
        for (name, pin) in packages {
            if let Some(path) = pin.get("path").and_then(Value::as_str) {
                let path = path.trim_end_matches('/');
                pinned.insert(name.clone(), path.to_owned());
            }
        }
    }
    pinned
}

/// The public contract a pipeline package declares: what it accepts, takes and produces.
fn contract_rows(value: &Value) -> Vec<Row> {
    let mut rows = vec![Row::painted(
        "CONTRACT  declared by the package",
        Paint::Title,
    )];
    let accepts = value.get("accepts");
    let kinds: Vec<&str> = accepts
        .and_then(|accepts| accepts.get("kinds"))
        .and_then(Value::as_array)
        .map(|kinds| kinds.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    rows.push(Row::plain(format!("accepts   kinds {}", kinds.join(", "))));
    if let Some(facts) = accepts
        .and_then(|accepts| accepts.get("required_facts"))
        .and_then(Value::as_table)
        && !facts.is_empty()
    {
        let names: Vec<&str> = facts.keys().map(String::as_str).collect();
        rows.push(Row::plain(format!(
            "          required facts {}",
            names.join(", ")
        )));
    }
    for (port, label) in [("inputs", "IN"), ("outputs", "OUT")] {
        let Some(ports) = value
            .get("contract")
            .and_then(|contract| contract.get(port))
            .and_then(Value::as_table)
        else {
            continue;
        };
        for (name, declared) in ports {
            let artifact = declared
                .get("artifact_type")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let optional = declared
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let cardinality = declared
                .get("cardinality")
                .and_then(Value::as_str)
                .unwrap_or("one");
            let note = if optional { "  optional" } else { "" };
            rows.push(Row::plain(format!(
                "{label:<4}      {name}: {artifact} ({cardinality}){note}"
            )));
        }
    }
    rows
}

/// Every blob `commit` holds under `under`, by path, with its object id, from one
/// `git ls-tree -r -z`: NUL-separated, so a path may hold any byte, a newline included.
pub(crate) fn listed(
    root: &Path,
    commit: &str,
    under: &str,
) -> Result<BTreeMap<String, String>, String> {
    let listing = git(root, &["ls-tree", "-r", "-z", commit, "--", under])?;
    Ok(entries(&listing))
}

/// The blobs of a `git ls-tree -z` listing, path to object id.
fn entries(listing: &[u8]) -> BTreeMap<String, String> {
    blob_entries(listing)
        .into_iter()
        .map(|(path, (_, object))| (path, object))
        .collect()
}

/// The blobs of a `git ls-tree -z` listing, path to mode and object id.
fn blob_entries(listing: &[u8]) -> BTreeMap<String, (String, String)> {
    let mut found = BTreeMap::new();
    for entry in listing.split(|byte| *byte == 0) {
        // `<mode> SP <type> SP <object> TAB <path>`
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let (meta, path) = (&entry[..tab], &entry[tab + 1..]);
        let meta = String::from_utf8_lossy(meta);
        let mut fields = meta.split(' ');
        let (mode, kind, object) = (fields.next(), fields.next(), fields.next());
        if let (Some(mode), Some("blob"), Some(object), Ok(path)) =
            (mode, kind, object, std::str::from_utf8(path))
        {
            found.insert(path.to_owned(), (mode.to_owned(), object.to_owned()));
        }
    }
    found
}

/// The bytes of every blob in `objects` (object ids, as `listed` gives them), read by one
/// `git cat-file --batch`: one process however many files, where `committed` spawns one per
/// file. Asking by object id keeps path bytes out of the request stream.
pub(crate) fn blobs(root: &Path, objects: &[&str]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut found = BTreeMap::new();
    if objects.is_empty() {
        return Ok(found);
    }
    let mut child = git_command(root, &["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("running git: {error}"))?;
    let mut requests = Vec::new();
    for object in objects {
        if !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("not an object id: {object}"));
        }
        requests.extend_from_slice(format!("{object}\n").as_bytes());
    }
    // Written from another thread, so a large answer never blocks on a full request pipe.
    let mut stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(&requests));
    let output = review_process::reap::wait_with_output(child)
        .map_err(|error| format!("running git: {error}"))?;
    let written = writer
        .join()
        .map_err(|_| "writing to git cat-file panicked")?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    written.map_err(|error| format!("writing to git cat-file: {error}"))?;
    let mut rest = output.stdout.as_slice();
    for object in objects {
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("git cat-file answered short")?;
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        // Every id came from the commit's own tree: one the object store cannot give back is
        // a failed read, never a file the commit lacks.
        if header.ends_with(" missing") {
            return Err(format!("git cannot read committed object {object}"));
        }
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| format!("git cat-file: {header}"))?;
        if rest.len() < size + 1 {
            return Err("git cat-file answered short".to_owned());
        }
        if header.split(' ').nth(1) != Some("blob") {
            return Err(format!("committed object {object} is not a file: {header}"));
        }
        found.insert((*object).to_owned(), rest[..size].to_vec());
        rest = &rest[size + 1..];
    }
    Ok(found)
}

/// Which of `paths` differ between the working tree and `commit` in bytes, mode or existence.
/// A git that cannot answer marks every path, as `differs` would each.
///
/// Never `git diff` against the working tree, and never `git hash-object`: both hash working-tree
/// files through the clean filter a checkout's own `.gitattributes` and `.git/config` select, and
/// a worktree diff refreshes the index through its fsmonitor. The committed side is the blob id
/// `ls-tree` lists (no filter, no blob read), the working-tree side is hashed here as git hashes
/// a blob, straight from the filesystem through one bounded buffer, and the two are compared by
/// id, as a dirty capture would see them. Neither a whole blob nor a whole working-tree file is
/// ever held. Index flags (`skip-worktree`, `assume-unchanged`) hide nothing from that comparison.
pub(crate) fn differing(root: &Path, commit: &str, paths: &[&str]) -> BTreeSet<String> {
    if paths.is_empty() {
        return BTreeSet::new();
    }
    let everything = || paths.iter().map(|path| (*path).to_owned()).collect();
    let Ok(committed) = listed_paths(root, commit, paths) else {
        return everything();
    };
    let mut differ = BTreeSet::new();
    for path in paths {
        let same = match committed.get(*path) {
            Some((mode, id)) => holds(&root.join(path), mode, id),
            // Not a file at `commit`: the same only while the working tree has nothing there.
            None => std::fs::symlink_metadata(root.join(path)).is_err(),
        };
        if !same {
            differ.insert((*path).to_owned());
        }
    }
    differ
}

/// Whether the working-tree entry at `full` is the blob `id` with `mode`: the same kind (a link,
/// a file, an executable as a dirty capture classifies it) and the same blob id, the file hashed
/// through [`blob_id`] and never read whole.
fn holds(full: &Path, mode: &str, id: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(full) else {
        return false;
    };
    match review_source_git::EntryKind::from_mode(mode) {
        Some(review_source_git::EntryKind::Symlink) if meta.file_type().is_symlink() => {
            std::fs::read_link(full).is_ok_and(|target| {
                let target = target.as_os_str().as_bytes();
                blob_id(id, target.len() as u64, &mut &target[..]).as_deref() == Some(id)
            })
        }
        Some(kind) if meta.is_file() => {
            let executable = meta.permissions().mode() & 0o111 != 0;
            executable == (kind == review_source_git::EntryKind::Executable)
                && std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                    .open(full)
                    .is_ok_and(|mut file| blob_id(id, meta.len(), &mut file).as_deref() == Some(id))
        }
        _ => false,
    }
}

/// The bytes of a working-tree file [`blob_id`] holds at once, whatever the file's size.
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// The git blob id of exactly `len` bytes from `reader`, hashed as git hashes a blob — the
/// header `blob <len>\0`, then the bytes, streamed through one [`HASH_BUFFER_BYTES`] buffer —
/// with the object format `like` (an id of the same repository) names by its length: 40 hex
/// digits SHA-1, 64 SHA-256. `None` when the reader gives other than `len` bytes (the file
/// changed while it was read), fails, or `like` is neither.
fn blob_id(like: &str, len: u64, reader: &mut impl std::io::Read) -> Option<String> {
    use sha2::Digest;
    enum Hasher {
        Sha1(Box<Sha1>),
        Sha256(Box<sha2::Sha256>),
    }
    let mut hasher = match like.len() {
        40 => Hasher::Sha1(Box::default()),
        64 => Hasher::Sha256(Box::default()),
        _ => return None,
    };
    let mut update = |bytes: &[u8]| match &mut hasher {
        Hasher::Sha1(hasher) => hasher.update(bytes),
        Hasher::Sha256(hasher) => hasher.update(bytes),
    };
    update(format!("blob {len}\0").as_bytes());
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut read = 0_u64;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        read = read.saturating_add(count as u64);
        if read > len {
            return None;
        }
        update(&buffer[..count]);
    }
    if read != len {
        return None;
    }
    Some(match hasher {
        Hasher::Sha1(hasher) => review_core::hex::encode(&hasher.finish()),
        Hasher::Sha256(hasher) => review_core::hex::encode(&hasher.finalize()),
    })
}

/// SHA-1 (FIPS 180-4), for the blob ids of a SHA-1 repository only: [`blob_id`] compares a
/// working-tree file with the id `ls-tree` lists, without running `git hash-object`.
struct Sha1 {
    state: [u32; 5],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }
}

impl Sha1 {
    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        while !bytes.is_empty() {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == 64 {
                self.compress();
                self.filled = 0;
            }
        }
    }

    fn finish(mut self) -> [u8; 20] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut digest = [0; 20];
        for (out, word) in digest.chunks_exact_mut(4).zip(self.state) {
            out.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self) {
        let mut schedule = [0_u32; 80];
        for (word, bytes) in schedule.iter_mut().zip(self.block.chunks_exact(4)) {
            *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for at in 16..80 {
            schedule[at] =
                (schedule[at - 3] ^ schedule[at - 8] ^ schedule[at - 14] ^ schedule[at - 16])
                    .rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (round, word) in schedule.iter().enumerate() {
            let (mixed, constant) = match round {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(mixed)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d, e]) {
            *state = state.wrapping_add(value);
        }
    }
}

/// The mode and blob id `commit` holds at exactly `paths`.
fn listed_paths(
    root: &Path,
    commit: &str,
    paths: &[&str],
) -> Result<BTreeMap<String, (String, String)>, ()> {
    let mut args = vec!["--literal-pathspecs", "ls-tree", "-z", commit, "--"];
    args.extend_from_slice(paths);
    let listing = git(root, &args).map_err(|_| ())?;
    Ok(blob_entries(&listing))
}

/// The HOME every browser git runs with: no `.gitconfig` can ever exist below `/dev/null`.
const GIT_HOME: &str = "/dev/null";

/// One git command against exactly this repository, isolated as capture isolates it
/// ([`review_source_git::git::isolated_command`]): the environment cleared, so an inherited
/// `GIT_DIR` or `GIT_WORK_TREE` cannot point it at another repository, no global or system
/// configuration, and every capture override ahead of the subcommand, so the checkout's own
/// `.git/config` cannot run a hook, an fsmonitor or a filter.
fn git_command(root: &Path, args: &[&str]) -> Command {
    #[cfg(test)]
    spawned::note(root);
    let mut command = review_source_git::git::isolated_command(root, Path::new(GIT_HOME));
    command.args(args);
    command
}

/// The command's stdout, or its stderr trimmed (empty when it said nothing).
pub(crate) fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = review_process::reap::output(&mut git_command(root, args))
        .map_err(|error| format!("running git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// A pipeline declaration read leniently: one that does not parse still lists, and its preview
/// reports the compiler's own refusal.
pub(crate) fn declared(text: &str) -> Value {
    toml::from_str(text).unwrap_or_else(|_| Value::Table(toml::map::Map::new()))
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.display().to_string()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::config;

    fn commit_all(root: &Path, message: &str) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["add", "-A"],
            vec!["commit", "-qm", message],
        ] {
            git(root, &args).unwrap();
        }
    }

    fn package(root: &Path, dir: &str, name: &str) {
        let dir = root.join(".af/task-packages").join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!("name = \"{name}\"\n[accepts]\nkinds = [\"implement\"]\n");
        std::fs::write(dir.join("pipeline.toml"), text).unwrap();
    }

    fn catalog(root: &Path, pins: &[(&str, &str)]) {
        let mut text = String::from("version = 1\n");
        for (name, dir) in pins {
            text.push_str(&format!(
                "[packages.\"{name}\"]\nversion = \"1.0.0\"\npath = \".af/task-packages/{dir}\"\n"
            ));
        }
        std::fs::create_dir_all(root.join(".af")).unwrap();
        std::fs::write(root.join(".af/task-catalog.toml"), text).unwrap();
    }

    fn scope(root: &Path) -> Scope {
        Scope::project(root.to_path_buf(), config::load(Some(root)).unwrap())
    }

    /// `Enter` on a listed Pipeline's row is the effect that opens it; a title row and an
    /// opened entry's rows name none.
    #[test]
    fn enter_on_a_listed_pipeline_row_opens_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "p", "p-pipeline");
        catalog(root, &[("p-pipeline", "p")]);
        commit_all(root, "p");
        let mut pane = PipelinesPane::default();
        pane.load(&scope(root)).unwrap();
        let id = pane.items()[0].id.clone();
        let row = pane
            .rows()
            .iter()
            .position(|row| row.text().starts_with("p-pipeline "))
            .unwrap_or_else(|| panic!("{:#?}", pane.rows()));
        assert_eq!(
            pane.key(Key::Enter, row),
            Ok(Some(Effect::OpenItem(Tab::Pipelines, id.clone())))
        );
        assert_eq!(pane.key(Key::Enter, 0), Ok(None));
        pane.open(Some(&id));
        assert_eq!(pane.key(Key::Enter, row), Ok(None));
    }

    /// Every open reads again, so the read must cost the same for 2 pipelines as for 30.
    #[test]
    fn a_read_spawns_as_many_git_processes_for_thirty_pipelines_as_for_two() {
        let spawns = |count: usize| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            for n in 0..count {
                package(&root, &format!("p{n}"), &format!("fixture/p{n}"));
            }
            commit_all(&root, "pipelines");
            let before = spawned::count(&root);
            let found = discover(&root).unwrap();
            assert_eq!(found.entries.len(), count);
            spawned::count(&root) - before
        };
        let (few, many) = (spawns(2), spawns(30));
        assert_eq!(few, many, "git processes for 2 pipelines, then for 30");
        assert!(many <= 6, "{many} git processes for one read");
    }

    #[test]
    fn git_runs_with_a_cleared_environment_and_every_capture_override() {
        let command = git_command(Path::new("/"), &["rev-parse", "HEAD"]);
        assert_eq!(command.get_program(), "git");
        let set: Vec<String> = command
            .get_envs()
            .map(|(key, value)| format!("{}={}", key.to_string_lossy(), value.is_some()))
            .collect();
        assert_eq!(
            set,
            [
                "GIT_ATTR_NOSYSTEM=true",
                "GIT_CONFIG_GLOBAL=true",
                "GIT_CONFIG_NOSYSTEM=true",
                "GIT_OPTIONAL_LOCKS=true",
                "GIT_TERMINAL_PROMPT=true",
                "HOME=true",
                "LC_ALL=true",
                "PATH=true",
                "TZ=true",
            ]
        );
        // `env_clear` leaves nothing inherited: an outer GIT_DIR cannot reach the child.
        assert!(command.get_envs().all(|(key, _)| key != "GIT_DIR"));
        let home = command.get_envs().find(|(key, _)| *key == "HOME");
        assert_eq!(
            home.and_then(|(_, value)| value),
            Some(OsStr::new(GIT_HOME))
        );
        // The checkout's own configuration cannot select a hook, an fsmonitor or a filter: every
        // capture override precedes the subcommand.
        let args: Vec<&OsStr> = command.get_args().collect();
        let overrides = review_source_git::git::CAPTURE_OVERRIDES;
        assert_eq!(
            args[..overrides.len()],
            *overrides.iter().map(OsStr::new).collect::<Vec<_>>()
        );
        assert!(overrides.contains(&"core.fsmonitor=false"));
        assert!(overrides.contains(&"core.hooksPath=/dev/null"));
        assert_eq!(
            args[overrides.len()..],
            [OsStr::new("rev-parse"), OsStr::new("HEAD")]
        );
    }

    /// No browser read runs a command that hashes working-tree files through a filter.
    #[test]
    fn the_browser_never_runs_a_worktree_diff_or_hash_object() {
        let source = include_str!("pipelines.rs");
        let production = &source[..source.find("#[cfg(test)]\nmod tests").unwrap()];
        for refused in ["\"diff\"", "\"hash-object\"", "\"status\""] {
            assert!(!production.contains(refused), "{refused} in pipelines.rs");
        }
        let workers = include_str!("workers.rs");
        for refused in ["\"diff\"", "\"hash-object\"", "\"status\""] {
            assert!(!workers.contains(refused), "{refused} in workers.rs");
        }
    }

    #[test]
    fn a_failed_read_of_head_is_an_error_and_an_unborn_head_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let error = discover(&plain).unwrap_err();
        assert!(error.contains("not a git repository"), "{error}");
        let unborn = temp.path().join("unborn");
        std::fs::create_dir_all(&unborn).unwrap();
        git(&unborn, &["init", "-q", "-b", "main"]).unwrap();
        let found = discover(&unborn).unwrap();
        assert!(found.commit.is_empty());
        assert!(found.entries.is_empty());
        // A branch that names a commit the repository does not have is not unborn: an error.
        let broken = temp.path().join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        package(&broken, "p", "fixture/p");
        commit_all(&broken, "package");
        let head_ref = String::from_utf8(git(&broken, &["symbolic-ref", "HEAD"]).unwrap()).unwrap();
        std::fs::write(
            broken.join(".git").join(head_ref.trim()),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        let error = discover(&broken).unwrap_err();
        assert!(
            error.contains("missing commit")
                || error.contains("bad ref")
                || error.contains("bad object"),
            "{error}"
        );
        // The pane keeps the last good entries and names the error on a failed refresh.
        let mut pane = PipelinesPane {
            root: Some(plain.clone()),
            entries: vec![Entry {
                id: "x".into(),
                label: "x".into(),
                path: plain.join("x"),
                declaration: String::new(),
                modified: false,
                source: Source::Review,
            }],
            ..PipelinesPane::default()
        };
        pane.discover();
        pane.rebuild();
        assert_eq!(pane.entries.len(), 1);
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows[2].starts_with(" error  HEAD could not be read; the entries below"),
            "{rows:#?}"
        );
        assert!(pane.items()[0].muted);
        // An opened stale entry says so too, above its own rows, and compiles nothing.
        pane.open(Some("x"));
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows[0]
                .starts_with(" error  HEAD could not be read; shown from the last successful read"),
            "{rows:#?}"
        );
        assert!(pane.busy().is_none());
        // Another repository is another scope: nothing of the old one survives the change.
        let other = temp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        pane.load(&scope(&other)).unwrap();
        assert!(pane.entries.is_empty());
        assert!(pane.selected.is_none());
        assert!(pane.error.is_some());
    }

    /// The checkout's `.git/config` is as untrusted as its tree: an fsmonitor and a clean filter
    /// it configures, with a committed `* filter=pwn`, never run while the bar reads `HEAD` and
    /// marks a changed working tree.
    #[test]
    fn a_hostile_checkout_runs_nothing_while_drift_is_read() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        package(&root, "p", "fixture/p");
        std::fs::write(root.join(".gitattributes"), "* filter=pwn\n").unwrap();
        commit_all(&root, "package");
        let marker = temp.path().join("MARKER");
        let script = temp.path().join("pwn.sh");
        let body = format!("#!/bin/sh\necho ran >> '{}'\ncat\n", marker.display());
        std::fs::write(&script, body).unwrap();
        let permissions = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let mut config = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".git/config"))
            .unwrap();
        let script = script.display();
        write!(
            config,
            "[core]\n\tfsmonitor = {script}\n\thooksPath = {}\n[filter \"pwn\"]\n\tclean = {script}\n\tsmudge = {script}\n\trequired = true\n",
            temp.path().display()
        )
        .unwrap();
        drop(config);
        let file = root.join(".af/task-packages/p/pipeline.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}# edited\n")).unwrap();

        let found = discover(&root).unwrap();
        assert!(found.entries[0].modified, "the edit is still marked");
        let paths = [".af/task-packages/p/pipeline.toml", ".gitattributes"];
        let differ = differing(&root, &found.commit, &paths);
        assert_eq!(differ.len(), 1);
        assert!(
            !marker.exists(),
            "checkout-configured code ran: {}",
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    #[test]
    fn a_mode_only_change_marks_the_working_tree_as_different() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "p", "fixture/p");
        commit_all(root, "package");
        let found = discover(root).unwrap();
        assert!(!found.entries[0].modified);
        let file = root.join(".af/task-packages/p/pipeline.toml");
        let permissions = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&file, permissions).unwrap();
        assert!(
            discover(root).unwrap().entries[0].modified,
            "the executable bit differs"
        );
    }

    /// The drift check compares blob ids only: a working-tree file several times the hash
    /// buffer is hashed through it as git hashes a blob, the same bytes are the same, and one
    /// changed byte at its end, at an equal length, differs.
    #[test]
    fn a_file_larger_than_the_hash_buffer_is_compared_by_blob_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "p", "fixture/p");
        let large = ".af/task-packages/p/large.bin";
        let mut bytes: Vec<u8> = (0..3 * HASH_BUFFER_BYTES + 17)
            .map(|at| (at % 251) as u8)
            .collect();
        std::fs::write(root.join(large), &bytes).unwrap();
        commit_all(root, "package");
        let found = discover(root).unwrap();
        let paths = [large, ".af/task-packages/p/pipeline.toml"];
        assert!(differing(root, &found.commit, &paths).is_empty());
        let listed = listed_paths(root, &found.commit, &[large]).unwrap();
        let mut file = std::fs::File::open(root.join(large)).unwrap();
        assert_eq!(
            blob_id(&listed[large].1, bytes.len() as u64, &mut file).as_deref(),
            Some(listed[large].1.as_str()),
            "git's own blob id"
        );
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(root.join(large), &bytes).unwrap();
        assert_eq!(
            differing(root, &found.commit, &paths),
            BTreeSet::from([large.to_owned()])
        );
    }

    #[test]
    fn a_blob_id_is_gits_for_either_object_format() {
        let sha1 = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        assert_eq!(blob_id(sha1, 0, &mut &b""[..]).as_deref(), Some(sha1));
        let hello = "ce013625030ba8dba906f756967f9e9ca394464a";
        assert_eq!(
            blob_id(sha1, 6, &mut &b"hello\n"[..]).as_deref(),
            Some(hello)
        );
        let sha256 = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";
        assert_eq!(blob_id(sha256, 0, &mut &b""[..]).as_deref(), Some(sha256));
        // A reader that gives other than the stated length names no blob.
        assert_eq!(blob_id(sha1, 5, &mut &b"hello\n"[..]), None);
        assert_eq!(blob_id(sha1, 7, &mut &b"hello\n"[..]), None);
        assert_eq!(blob_id("abc", 0, &mut &b""[..]), None);
        let mut abc = Sha1::default();
        abc.update(b"abc");
        assert_eq!(
            review_core::hex::encode(&abc.finish()),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let mut long = Sha1::default();
        long.update(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(
            review_core::hex::encode(&long.finish()),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn every_committed_pipeline_package_is_found_however_nested() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for (dir, name) in [("a", "a"), ("a/b", "b"), ("d1/d2/d3/d4/d5/d6/d7", "d7")] {
            package(root, dir, name);
        }
        std::fs::create_dir_all(root.join(".af/pipelines")).unwrap();
        std::fs::write(root.join(".af/pipelines/review.toml"), "version = 1\n").unwrap();
        commit_all(root, "pipelines");
        let found = discover(root).unwrap();
        assert_eq!(found.commit.len(), 40);
        let ids: Vec<(String, String, bool)> = found
            .entries
            .into_iter()
            .map(|entry| (entry.id, entry.label, entry.modified))
            .collect();
        assert_eq!(
            ids,
            [
                (".af/pipelines/review.toml".into(), "review".into(), false),
                (
                    ".af/task-packages/a/b/pipeline.toml".into(),
                    "b".into(),
                    false
                ),
                (
                    ".af/task-packages/a/pipeline.toml".into(),
                    "a".into(),
                    false
                ),
                (
                    ".af/task-packages/d1/d2/d3/d4/d5/d6/d7/pipeline.toml".into(),
                    "d7".into(),
                    false
                ),
            ]
        );
    }

    #[test]
    fn the_bar_names_what_head_commits_and_marks_a_changed_working_tree() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "p", "fixture/p");
        commit_all(root, "package");
        let committed =
            std::fs::read_to_string(root.join(".af/task-packages/p/pipeline.toml")).unwrap();
        // A rename in the working tree changes neither the identity nor the declaration; an
        // untracked package beside it is not listed at all.
        let dirty = committed.replace("fixture/p", "dirty/p");
        std::fs::write(root.join(".af/task-packages/p/pipeline.toml"), &dirty).unwrap();
        package(root, "shadow", "fixture/p");
        let entries = discover(root).unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "fixture/p");
        assert_eq!(entries[0].declaration, committed);
        assert!(entries[0].modified);
        assert_eq!(
            entries[0].path,
            root.join(".af/task-packages/p/pipeline.toml")
        );
    }

    #[test]
    fn only_the_file_the_catalog_pins_is_previewed_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "pinned", "fixture/p");
        package(root, "other", "fixture/p");
        package(root, "loose", "fixture/loose");
        catalog(root, &[("fixture/p", "pinned")]);
        commit_all(root, "packages");
        let entries = discover(root).unwrap().entries;
        let pin = |dir: &str| {
            let id = format!(".af/task-packages/{dir}/pipeline.toml");
            match &entries.iter().find(|entry| entry.id == id).unwrap().source {
                Source::Package { pin, .. } => pin.clone(),
                Source::Review => unreachable!(),
            }
        };
        assert_eq!(pin("pinned"), Pin::Here);
        assert_eq!(
            pin("other"),
            Pin::Elsewhere(".af/task-packages/pinned".into())
        );
        assert_eq!(pin("loose"), Pin::Absent);
        // `p` from a Task opens the pinned file, never a shadow of the same name.
        let pane = PipelinesPane {
            entries: entries.clone(),
            ..PipelinesPane::default()
        };
        assert_eq!(
            pane.pinned_entry("fixture/p").as_deref(),
            Some(".af/task-packages/pinned/pipeline.toml")
        );
        assert_eq!(pane.pinned_entry("fixture/loose"), None);
        // Opening the shadow row refuses, with the reason and the file's own contract, rather
        // than compiling the pinned file's plan under this row.
        let mut pane = PipelinesPane::default();
        pane.load(&scope(root)).unwrap();
        pane.open(Some(".af/task-packages/other/pipeline.toml"));
        let rows: Vec<String> = pane.rows().iter().map(Row::text).collect();
        assert!(
            rows[2].contains("pins `fixture/p` at .af/task-packages/pinned"),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("CONTRACT")),
            "{rows:#?}"
        );
        assert!(pane.busy().is_none(), "nothing is compiled for a shadow");
        // HEAD moves: the catalog now pins the name at the other file. `p` follows it.
        let mut pane = PipelinesPane::default();
        pane.load(&scope(root)).unwrap();
        catalog(root, &[("fixture/p", "other")]);
        git(root, &["add", "-A"]).unwrap();
        git(root, &["commit", "-qm", "repin"]).unwrap();
        assert_eq!(
            pane.pinned_entry_now("fixture/p").unwrap().as_deref(),
            Some(".af/task-packages/other/pipeline.toml")
        );
        // HEAD becomes unreadable: the jump is refused, never taken from the stale entries.
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/nowhere\n").unwrap();
        std::fs::write(
            root.join(".git/refs/heads/nowhere"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert!(pane.pinned_entry_now("fixture/p").is_err());
        // HEAD reads again at the same commit: the jump works again.
        let good = git(root, &["rev-parse", "main"]).unwrap();
        let good = String::from_utf8(good).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(!good.trim().is_empty());
        assert_eq!(
            pane.pinned_entry_now("fixture/p").unwrap().as_deref(),
            Some(".af/task-packages/other/pipeline.toml")
        );
    }

    #[test]
    fn a_preview_reads_the_entries_again_when_head_moved() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        package(root, "p", "fixture/p");
        catalog(root, &[("fixture/p", "p")]);
        commit_all(root, "first");
        let mut pane = PipelinesPane::default();
        pane.load(&scope(root)).unwrap();
        let first = pane.commit.clone();
        assert_eq!(first.len(), 40);
        // HEAD moves while the browser is open: the declaration and the mark follow it.
        let file = root.join(".af/task-packages/p/pipeline.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}# second\n")).unwrap();
        git(root, &["add", "-A"]).unwrap();
        git(root, &["commit", "-qm", "second"]).unwrap();
        assert_eq!(
            pane.commit, first,
            "nothing is read until a preview needs HEAD"
        );
        pane.open(Some(".af/task-packages/p/pipeline.toml"));
        assert_ne!(pane.commit, first);
        assert!(!pane.entries[0].modified);
        assert!(pane.entries[0].declaration.ends_with("# second\n"));
    }

    #[test]
    fn a_refused_preview_still_shows_the_declared_contract() {
        let value: Value = toml::from_str(
            "[accepts]\nkinds = [\"implement\"]\n[accepts.required_facts.standard]\n\
             kind = \"boolean\"\n[contract.inputs.source]\nartifact_type = \"af/SourceTree@1\"\n\
             cardinality = \"one\"\noptional = false\n[contract.outputs.snapshot]\n\
             artifact_type = \"af/SourceTree@1\"\ncardinality = \"one\"\noptional = true\n",
        )
        .unwrap();
        let rows: Vec<String> = contract_rows(&value).iter().map(Row::text).collect();
        assert_eq!(rows[1], "accepts   kinds implement");
        assert_eq!(rows[2], "          required facts standard");
        assert_eq!(rows[3], "IN        source: af/SourceTree@1 (one)");
        assert_eq!(
            rows[4],
            "OUT       snapshot: af/SourceTree@1 (one)  optional"
        );
    }
}

/// How many git processes the browser started against each repository: tests read it to prove a
/// read's cost does not grow with what it lists. Each test works in its own repository, so
/// tests running side by side never count each other.
#[cfg(test)]
pub(crate) mod spawned {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static SPAWNED: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

    pub(super) fn note(root: &Path) {
        let mut spawned = SPAWNED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *spawned.entry(root.to_path_buf()).or_default() += 1;
    }

    /// The git processes started against `root` so far.
    pub(crate) fn count(root: &Path) -> usize {
        let spawned = SPAWNED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        spawned.get(root).copied().unwrap_or(0)
    }
}
