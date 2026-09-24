//! The main-pane renderers, one per node kind. Every pane projects data a CLI command already
//! prints, read by the function behind that command; a pane that would need a number no CLI
//! document carries does not show it.

use std::path::PathBuf;

use super::keymap::Key;
use super::paint::{Paint, Span};
use super::scope::Scope;
use super::tree::Item;

pub(crate) mod pipelines;
pub(crate) mod providers;
pub(crate) mod settings;

/// One main-pane row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) spans: Vec<Span>,
}

impl Row {
    pub(crate) fn plain(text: impl AsRef<str>) -> Row {
        Row::painted(text, Paint::Plain)
    }

    pub(crate) fn painted(text: impl AsRef<str>, paint: Paint) -> Row {
        Row {
            spans: vec![Span::new(text, paint)],
        }
    }

    pub(crate) fn blank() -> Row {
        Row::default()
    }

    pub(crate) fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// The closed set of things a pane or a key asks the event loop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Quit,
    /// Release the screen, run `$EDITOR` on the file, re-enter.
    OpenEditor(PathBuf),
    /// An `af` command line, parsed by the CLI's own clap definition.
    RunCommand(Vec<String>),
    /// Copy the text to the terminal's clipboard with OSC 52.
    Yank(String),
    Refresh,
}

pub(crate) trait Pane {
    /// A pure read of disk or the Store for this scope; nothing is written.
    fn load(&mut self, scope: &Scope) -> Result<(), String>;

    /// The bar entries under this pane's folder.
    fn items(&self) -> Vec<Item> {
        Vec::new()
    }

    /// Show one entry, or the folder itself for `None`.
    fn open(&mut self, _item: Option<&str>) {}

    /// What the main pane paints.
    fn rows(&self) -> &[Row];

    /// A pane-local verb, given the main-pane row under the cursor. `Err` is shown on the
    /// status line.
    fn key(&mut self, _key: Key, _row: usize) -> Result<Option<Effect>, String> {
        Ok(None)
    }

    /// The key legend while the main pane has focus.
    fn legend(&self) -> &'static str;

    /// What the status line says about the row under the cursor, when anything.
    fn status(&self, _row: usize) -> Option<String> {
        None
    }

    /// `R`: read everything again.
    fn refresh(&mut self, scope: &Scope) -> Result<(), String> {
        self.load(scope)
    }

    /// Collect finished background work; `true` when the rows changed.
    fn poll(&mut self) -> bool {
        false
    }

    /// A spinner frame while background work runs.
    fn busy(&self) -> Option<char> {
        None
    }

    /// `<C-c>`: stop background work; `true` when some was running.
    fn cancel(&mut self) -> bool {
        false
    }

    /// The file behind the row under the cursor, for `gf`.
    fn file(&self, _row: usize) -> Option<PathBuf> {
        None
    }

    /// The id `y` copies from the main pane.
    fn yank(&self, _row: usize) -> Option<String> {
        None
    }
}

/// The spinner frames background work shows, in ASCII.
pub(crate) const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

/// A folder whose pane arrives in a later package of docs/design/tui.md section 7.
pub(crate) struct Placeholder {
    rows: Vec<Row>,
}

impl Placeholder {
    pub(crate) fn new(title: &str, step: u8) -> Placeholder {
        Placeholder {
            rows: vec![
                Row::painted(title, Paint::Title),
                Row::blank(),
                Row::plain("This pane arrives in a later package"),
                Row::plain(format!("(docs/design/tui.md section 7, step {step}).")),
            ],
        }
    }
}

impl Pane for Placeholder {
    fn load(&mut self, _scope: &Scope) -> Result<(), String> {
        Ok(())
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn legend(&self) -> &'static str {
        "j/k move  Tab bar  :cmd  q quit"
    }
}
