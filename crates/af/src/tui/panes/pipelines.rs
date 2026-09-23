//! Pipelines: `.af/pipelines/*.toml` and every `pipeline.toml` under `.af/task-packages/`.
//!
//! A package's main pane is, verbatim, the text `af task explain --tree` prints for a plan of
//! it that `af task plan` would capture: the same token-free compilation of the committed
//! authority, into a scratch Store that is discarded, on a thread the event loop polls. No
//! Store is written, nothing is admitted and no Worker runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use toml::Value;

use super::{Pane, Row, SPINNER};
use crate::task_execution::{self, TreePreview};
use crate::tui::paint::Paint;
use crate::tui::scope::Scope;
use crate::tui::tree::Item;

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

#[derive(Clone, Debug)]
enum Source {
    /// A review Pipeline under `.af/pipelines/`: planned against a diff, not a Task.
    Review,
    /// A pipeline package, with the kind and Attempt bound its preview Task takes.
    Package { kind: String, max_attempts: u64 },
}

#[derive(Clone, Debug)]
struct Entry {
    /// The repository-relative path of the TOML file.
    id: String,
    label: String,
    path: PathBuf,
    source: Source,
}

type Compiled = Result<TreePreview, String>;

#[derive(Default)]
pub(crate) struct PipelinesPane {
    root: Option<PathBuf>,
    entries: Vec<Entry>,
    selected: Option<String>,
    previews: BTreeMap<String, Compiled>,
    job: Option<(String, Receiver<Compiled>)>,
    rows: Vec<Row>,
    spinner: usize,
}

impl PipelinesPane {
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected.as_deref()?)
    }

    /// The pipeline file behind a bar entry, for `gf` on the bar.
    pub(crate) fn entry_file(&self, id: &str) -> Option<PathBuf> {
        self.entry(id).map(|entry| entry.path.clone())
    }

    fn compile(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let Source::Package { kind, max_attempts } = &entry.source else {
            return;
        };
        let running = self.job.as_ref().is_some_and(|(id, _)| *id == entry.id);
        if running || self.previews.contains_key(&entry.id) {
            return;
        }
        let root = match &self.root {
            Some(root) => root.clone(),
            None => return,
        };
        let task = preview_task(&entry.label, kind, *max_attempts);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(plan(&root, &task));
        });
        self.job = Some((entry.id, receiver));
    }

    fn rebuild(&mut self) {
        self.rows = match self.selected_entry() {
            Some(entry) => self.entry_rows(entry),
            None => self.folder_rows(),
        };
    }

    fn entry_rows(&self, entry: &Entry) -> Vec<Row> {
        let heading = format!("PIPE  {} ({})", entry.label, entry.id);
        let mut rows = Vec::new();
        match (&entry.source, self.previews.get(&entry.id)) {
            (Source::Review, _) => {
                let title = format!("{heading}  [review pipeline]");
                rows.push(Row::painted(title, Paint::Title));
                rows.push(Row::blank());
                let why = "A review Pipeline is planned against a diff, not a Task:";
                rows.push(Row::plain(why));
                rows.push(Row::plain(format!(
                    "  af review plan --pipeline {}",
                    entry.id
                )));
            }
            (Source::Package { .. }, Some(Ok(preview))) => {
                rows.extend(preview.text.lines().map(Row::plain));
            }
            (Source::Package { .. }, Some(Err(error))) => {
                rows.push(Row::painted(heading, Paint::Title));
                rows.push(Row::blank());
                rows.extend(error.lines().map(|line| Row::painted(line, Paint::Error)));
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

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted("PIPELINES", Paint::Title), Row::blank()];
        if self.root.is_none() {
            rows.push(Row::plain(
                "Pipelines belong to a project; :cd into a repository.",
            ));
        } else if self.entries.is_empty() {
            let none = "No Pipeline under .af/pipelines/ or .af/task-packages/.";
            rows.push(Row::plain(none));
        }
        for entry in &self.entries {
            let (label, id) = (&entry.label, &entry.id);
            rows.push(Row::plain(format!("{label:<28} {id}")));
        }
        rows
    }
}

impl Pane for PipelinesPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        self.root = scope.toplevel().map(Path::to_path_buf);
        self.entries = match &self.root {
            Some(root) => discover(root),
            None => Vec::new(),
        };
        self.previews.clear();
        self.job = None;
        self.compile();
        self.rebuild();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for entry in &self.entries {
            items.push(Item {
                id: entry.id.clone(),
                label: entry.label.clone(),
                muted: false,
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

    fn legend(&self) -> &'static str {
        "j/k slot  R recompile  gf open TOML  y yank name  Tab bar  :cmd  q quit"
    }

    /// The Worker binding of the slot a highlighted plan row names.
    fn status(&self, row: usize) -> Option<String> {
        let entry = self.selected_entry()?;
        let preview = self.previews.get(&entry.id)?.as_ref().ok()?;
        preview.slots.get(&row).cloned()
    }

    fn poll(&mut self) -> bool {
        let Some((id, receiver)) = self.job.as_ref() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(compiled) => {
                self.previews.insert(id.clone(), compiled);
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

/// Write the preview Task file into a scratch directory and plan it the `af task plan` way.
fn plan(root: &Path, task: &serde_json::Value) -> Compiled {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let file = scratch.path().join("pipeline-preview.json");
    let bytes = serde_json::to_vec_pretty(task).map_err(|error| error.to_string())?;
    std::fs::write(&file, bytes).map_err(|error| error.to_string())?;
    task_execution::plan_tree_preview(&file, root)
}

/// Review Pipelines first, then pipeline packages, each sorted by path.
fn discover(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    for path in toml_files(&root.join(".af/pipelines")) {
        let stem = path.file_stem().unwrap_or_default();
        entries.push(Entry {
            id: relative(root, &path),
            label: stem.to_string_lossy().into_owned(),
            path,
            source: Source::Review,
        });
    }
    let packages = root.join(".af/task-packages");
    let mut found = Vec::new();
    find_packages(&packages, 0, &mut found);
    found.sort();
    for path in found {
        let value = declared(&path);
        let directory = path.parent().unwrap_or(root);
        let fallback = relative(&packages, directory);
        let name = value.get("name").and_then(Value::as_str);
        let accepts = value.get("accepts");
        let kinds = accepts.and_then(|accepts| accepts.get("kinds"));
        let first = kinds.and_then(|kinds| kinds.get(0));
        let kind = first.and_then(Value::as_str);
        let attempts = value.get("max_attempts").and_then(Value::as_integer);
        let max_attempts = attempts.and_then(|n| u64::try_from(n).ok()).unwrap_or(3);
        entries.push(Entry {
            id: relative(root, &path),
            label: name.map_or(fallback, str::to_owned),
            path,
            source: Source::Package {
                kind: kind.unwrap_or("implement").to_owned(),
                max_attempts: max_attempts.max(1),
            },
        });
    }
    entries
}

/// A pipeline file read leniently: one that does not parse still lists, and its preview reports
/// the compiler's own refusal.
fn declared(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    toml::from_str(&text).unwrap_or_else(|_| Value::Table(toml::map::Map::new()))
}

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    paths.sort();
    paths
}

/// Every `pipeline.toml` below `dir`, a few levels deep, without following symlinks.
fn find_packages(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let pipeline = path.join("pipeline.toml");
        if pipeline.is_file() {
            found.push(pipeline);
        } else if depth < 4 {
            find_packages(&path, depth + 1, found);
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.display().to_string()
}
