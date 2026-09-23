//! Pipelines: `.af/pipelines/*.toml` and every `pipeline.toml` under `.af/task-packages/`, as
//! committed at `HEAD`. The plan preview compiles committed authority, so the bar names what
//! `HEAD` holds; a working-tree file that differs is marked, and `gf` still opens it.
//!
//! A package's main pane is, verbatim, the text `af task explain --tree` prints for a plan of
//! it that `af task plan` would capture: the same token-free compilation of the committed
//! authority, into a scratch Store that is discarded, on a thread the event loop polls. No
//! Store is written, nothing is admitted and no Worker runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    /// The working-tree file, for `gf`.
    path: PathBuf,
    /// The file as committed at `HEAD`: what every preview and declaration is read from.
    declaration: String,
    /// The working-tree file differs from `HEAD` or is gone.
    modified: bool,
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
        if entry.modified {
            let note = "working tree differs from HEAD: shown as committed; gf opens the file";
            rows.push(Row::painted(note, Paint::Muted));
        }
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
                rows.extend(error.lines().map(|line| Row::painted(line, Paint::Error)));
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

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted("PIPELINES", Paint::Title), Row::blank()];
        if self.root.is_none() {
            rows.push(Row::plain(
                "Pipelines belong to a project; :cd into a repository.",
            ));
        } else if self.entries.is_empty() {
            let none = "No Pipeline committed under .af/pipelines/ or .af/task-packages/.";
            rows.push(Row::plain(none));
        }
        for entry in &self.entries {
            let (label, id) = (&entry.label, &entry.id);
            let mark = if entry.modified { " *" } else { "" };
            rows.push(Row::plain(format!("{label:<28} {id}{mark}")));
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
                muted: entry.modified,
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

/// Review Pipelines first, then pipeline packages, each sorted by path, all as `HEAD` commits
/// them. A repository without a `HEAD` lists nothing.
fn discover(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Ok(listing) = git(root, &["ls-tree", "-r", "-z", "--name-only", "HEAD", "--"]) else {
        return entries;
    };
    let mut reviews = Vec::new();
    let mut packages = Vec::new();
    for path in listing.split(|byte| *byte == 0) {
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        if let Some(rest) = path.strip_prefix(".af/pipelines/")
            && !rest.contains('/')
            && rest.ends_with(".toml")
        {
            reviews.push(path.to_owned());
        } else if path.starts_with(".af/task-packages/") && path.ends_with("/pipeline.toml") {
            packages.push(path.to_owned());
        }
    }
    reviews.sort();
    packages.sort();
    for id in reviews {
        let Ok(declaration) = committed(root, &id) else {
            continue;
        };
        let stem = Path::new(&id).file_stem().unwrap_or_default();
        entries.push(Entry {
            label: stem.to_string_lossy().into_owned(),
            path: root.join(&id),
            modified: differs(root, &id, &declaration),
            declaration,
            id,
            source: Source::Review,
        });
    }
    for id in packages {
        let Ok(declaration) = committed(root, &id) else {
            continue;
        };
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
        entries.push(Entry {
            label: name.map_or(fallback, str::to_owned),
            path: root.join(&id),
            modified: differs(root, &id, &declaration),
            declaration,
            id,
            source: Source::Package {
                kind: kind.unwrap_or("implement").to_owned(),
                max_attempts: max_attempts.max(1),
            },
        });
    }
    entries
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

/// The bytes `HEAD` holds at a repository-relative path.
fn committed(root: &Path, path: &str) -> Result<String, String> {
    let bytes = git(root, &["show", &format!("HEAD:{path}")])?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// Whether the working-tree file differs from the committed text, or is gone.
fn differs(root: &Path, path: &str, committed: &str) -> bool {
    std::fs::read(root.join(path)).ok().as_deref() != Some(committed.as_bytes())
}

/// One git command against the repository, with the user's global configuration kept out.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .map_err(|error| format!("running git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// A pipeline declaration read leniently: one that does not parse still lists, and its preview
/// reports the compiler's own refusal.
fn declared(text: &str) -> Value {
    toml::from_str(text).unwrap_or_else(|_| Value::Table(toml::map::Map::new()))
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_all(root: &Path) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "pipelines"],
        ] {
            git(root, &args).unwrap();
        }
    }

    #[test]
    fn every_committed_pipeline_package_is_found_however_nested() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let packages = root.join(".af/task-packages");
        // A package nested inside another, and one seven directories deep.
        for dir in ["a", "a/b", "d1/d2/d3/d4/d5/d6/d7"] {
            let dir = packages.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let text = format!("name = \"{name}\"\n[accepts]\nkinds = [\"implement\"]\n");
            std::fs::write(dir.join("pipeline.toml"), text).unwrap();
        }
        std::fs::create_dir_all(root.join(".af/pipelines")).unwrap();
        std::fs::write(root.join(".af/pipelines/review.toml"), "version = 1\n").unwrap();
        // Nothing is listed before a commit exists.
        assert!(discover(root).is_empty());
        commit_all(root);
        let ids: Vec<(String, String, bool)> = discover(root)
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
        let package = root.join(".af/task-packages/p");
        std::fs::create_dir_all(&package).unwrap();
        let committed = "name = \"fixture/p\"\n[accepts]\nkinds = [\"implement\"]\n";
        std::fs::write(package.join("pipeline.toml"), committed).unwrap();
        commit_all(root);
        // A rename in the working tree changes neither the identity nor the declaration; an
        // untracked package beside it is not listed at all.
        let dirty = committed.replace("fixture/p", "dirty/p");
        std::fs::write(package.join("pipeline.toml"), &dirty).unwrap();
        std::fs::create_dir_all(root.join(".af/task-packages/shadow")).unwrap();
        std::fs::write(
            root.join(".af/task-packages/shadow/pipeline.toml"),
            committed,
        )
        .unwrap();
        let entries = discover(root);
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
