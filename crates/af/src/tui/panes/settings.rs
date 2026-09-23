//! Settings for the scope root: the layer table `af config paths` prints and the effective
//! configuration `af config show --origin` prints, both from the scope's one `config::load`.
//! `e` edits a layer through `af config edit`, parsed by the CLI's own definition.

use std::path::{Path, PathBuf};

use super::{Effect, Pane, Row};
use crate::cli::LayerArg;
use crate::config::{self, Origin};
use crate::tui::keymap::Key;
use crate::tui::paint::{Paint, Span};
use crate::tui::scope::{Scope, ScopeKind};

/// A layer, its file (none for the built-in defaults), and the state column.
type LayerEntry = (&'static str, Option<PathBuf>, &'static str);

/// One row of the layer table.
struct Layer {
    row: usize,
    name: &'static str,
    path: Option<PathBuf>,
    edit: Option<LayerArg>,
}

#[derive(Default)]
pub(crate) struct SettingsPane {
    rows: Vec<Row>,
    layers: Vec<Layer>,
    /// The layer `e` edits when the cursor is not on a layer row: `af config edit`'s own
    /// default in the project scope, the user layer in the user scope.
    default: Option<LayerArg>,
    default_row: Option<usize>,
    /// `--repo` for `af config edit` in the project scope.
    repo: Option<PathBuf>,
}

impl Pane for SettingsPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let default = match scope.kind {
            ScopeKind::User => LayerArg::User,
            ScopeKind::Project => LayerArg::Project,
        };
        self.default = Some(default);
        self.default_row = None;
        self.repo = scope.toplevel().map(Path::to_path_buf);
        self.layers.clear();
        let title = format!("SETTINGS  {}: {}", scope.word(), scope.name());
        self.rows = vec![Row::painted(title, Paint::Title)];
        let header = table_row("layer", "file", "state");
        self.rows.push(Row::painted(header, Paint::Title));
        for (name, path, state) in layer_files(scope) {
            let edit = editable(name);
            let file = match (&path, name) {
                (Some(path), _) => scope.display(path),
                (None, "built-in") => "(binary)".to_owned(),
                (None, _) => "(unknown)".to_owned(),
            };
            let mut text = table_row(name, &file, state);
            if edit == Some(default) && self.default_row.is_none() {
                text.push_str("  <- edit with e");
                self.default_row = Some(self.rows.len());
            }
            let row = self.rows.len();
            self.layers.push(Layer {
                row,
                name,
                path,
                edit,
            });
            self.rows.push(Row::plain(text));
        }
        self.rows.push(Row::blank());
        self.rows.extend(effective(scope));
        Ok(())
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn key(&mut self, key: Key, row: usize) -> Result<Option<Effect>, String> {
        if key != Key::Char('e') {
            return Ok(None);
        }
        let layer = layer_name(self.target(row)?);
        let mut words = Vec::from(["config", "edit", "--layer", layer].map(str::to_owned));
        if let Some(repo) = &self.repo {
            words.push("--repo".to_owned());
            words.push(repo.display().to_string());
        }
        Ok(Some(Effect::RunCommand(words)))
    }

    fn legend(&self) -> &'static str {
        "j/k move  e edit layer  gf open file  Tab bar  :cmd  q quit"
    }

    fn status(&self, row: usize) -> Option<String> {
        let layer = self.layer(row)?;
        let name = layer.name;
        Some(match layer.edit {
            Some(_) => format!("e edits the {name} layer"),
            None => format!("the {name} layer is read-only here"),
        })
    }

    fn file(&self, row: usize) -> Option<PathBuf> {
        let row = if self.layer(row).is_some() {
            row
        } else {
            self.default_row?
        };
        let path = self.layer(row)?.path.clone()?;
        path.is_file().then_some(path)
    }

    fn yank(&self, row: usize) -> Option<String> {
        let path = self.layer(row)?.path.as_ref()?;
        Some(path.display().to_string())
    }
}

impl SettingsPane {
    fn layer(&self, row: usize) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.row == row)
    }

    /// The layer `e` edits: the one under the cursor, or the default off the table.
    fn target(&self, row: usize) -> Result<LayerArg, String> {
        let default = self.default;
        let Some(layer) = self.layer(row) else {
            return default.ok_or_else(|| "no layer to edit here".to_owned());
        };
        let (name, edit) = (layer.name, layer.edit);
        edit.ok_or_else(|| format!("the {name} layer is not edited with af config edit"))
    }
}

fn table_row(layer: &str, file: &str, state: &str) -> String {
    format!("{layer:<10} {file:<36} {state}")
}

fn state(present: bool) -> &'static str {
    if present { "present" } else { "absent" }
}

/// The layers in ladder order: every file `af config paths` lists, except that absent
/// directory layers fold into the nearest one — the file `af config edit --layer directory`
/// would create. The user scope adds the Provider registry, its other machine-owned file.
fn layer_files(scope: &Scope) -> Vec<LayerEntry> {
    let files = &scope.config.files;
    let mut present = false;
    let mut nearest = None;
    for (index, file) in files.iter().enumerate() {
        if file.layer == "directory" {
            present |= file.present;
            nearest = Some(index);
        }
    }
    let mut layers: Vec<LayerEntry> = vec![("built-in", None, "-")];
    for (index, file) in files.iter().enumerate() {
        let directory = file.layer == "directory";
        let folded = directory && !file.present && (present || Some(index) != nearest);
        if !folded {
            layers.push((file.layer, Some(file.path.clone()), state(file.present)));
        }
    }
    if scope.kind == ScopeKind::User {
        let registry = scope.registry.clone();
        let present = registry.as_deref().is_some_and(Path::is_file);
        layers.push(("providers", registry, state(present)));
    }
    layers
}

/// The effective configuration, one value per row with the layer and line it came from, grouped
/// under its table. Keys sort, top-level ones first, so the order never depends on how a TOML
/// map iterates.
fn effective(scope: &Scope) -> Vec<Row> {
    let mut leaves = Vec::new();
    config::flatten(&scope.config.effective, "", &mut leaves);
    leaves.sort_by_key(|(key, _)| (key.contains('.'), key.clone()));
    let mut entries = Vec::new();
    let mut width = 0;
    for (key, value) in &leaves {
        let (table, name) = match key.split_once('.') {
            Some((table, name)) => (Some(table), name),
            None => (None, key.as_str()),
        };
        let left = format!("{name} = {value}");
        width = width.max(left.len());
        let note = origin_note(scope, scope.config.origins.get(key));
        entries.push((table, left, note));
    }
    let mut rows = Vec::new();
    let mut current = None;
    for (table, left, note) in entries {
        if let Some(name) = table
            && table != current
        {
            rows.push(Row::painted(format!("[{name}]"), Paint::Title));
        }
        current = table;
        let spans = vec![
            Span::new(format!("{left:<width$}   "), Paint::Plain),
            Span::new(note, Paint::Muted),
        ];
        rows.push(Row { spans });
    }
    rows
}

/// Where one value came from, as `af config show --origin` annotates it.
fn origin_note(scope: &Scope, origin: Option<&Origin>) -> String {
    let Some(origin) = origin else {
        return "?".to_owned();
    };
    match (&origin.path, origin.line) {
        (Some(path), Some(line)) => format!("{} {}:{line}", origin.layer, scope.display(path)),
        (Some(path), None) => format!("{} {}", origin.layer, scope.display(path)),
        (None, _) => origin.layer.to_owned(),
    }
}

fn editable(layer: &str) -> Option<LayerArg> {
    match layer {
        "user" => Some(LayerArg::User),
        "directory" => Some(LayerArg::Directory),
        "project" => Some(LayerArg::Project),
        "local" => Some(LayerArg::Local),
        _ => None,
    }
}

fn layer_name(layer: LayerArg) -> &'static str {
    match layer {
        LayerArg::User => "user",
        LayerArg::Directory => "directory",
        LayerArg::Project => "project",
        LayerArg::Local => "local",
    }
}
