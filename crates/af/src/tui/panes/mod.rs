//! The main-pane renderers, one per node kind. Every pane projects data a CLI command already
//! prints, read by the function behind that command; a pane that would need a number no CLI
//! document carries does not show it.

use std::path::PathBuf;

use super::keymap::Key;
use super::paint::{Paint, Span, Tone};
use super::scope::Scope;
use super::tree::Item;

pub(crate) mod pipelines;
pub(crate) mod providers;
pub(crate) mod settings;
pub(crate) mod tasks;
pub(crate) mod workers;

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

    /// An error: `word` (`error`, `refused`, `warning`) as a `fail` chip, then the message in
    /// bold.
    pub(crate) fn error(word: &str, message: impl AsRef<str>) -> Row {
        Row {
            spans: vec![
                Span::chip(word, Tone::Fail),
                Span::new(" ", Paint::Plain),
                Span::new(message, Paint::Error),
            ],
        }
    }

    /// An error of several lines: the first behind the chip, the rest aligned under it.
    pub(crate) fn errors<S: AsRef<str>>(
        word: &str,
        lines: impl IntoIterator<Item = S>,
    ) -> Vec<Row> {
        let indent = " ".repeat(word.len() + 3);
        let mut rows = Vec::new();
        for line in lines {
            if rows.is_empty() {
                rows.push(Row::error(word, line));
                continue;
            }
            rows.push(Row {
                spans: vec![
                    Span::new(&indent, Paint::Plain),
                    Span::new(line, Paint::Error),
                ],
            });
        }
        rows
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
    /// Open the named Pipeline in the Pipelines pane, when that pane lists it.
    OpenPipeline(String),
    /// Open the `:` line holding this text, for the user to review; nothing is submitted.
    Prefill(String),
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

    /// The main pane shows another pane now; background reads for this one stop.
    fn close(&mut self) {}

    /// Whether a pane-local view, like an artifact opened from a row, covers the pane.
    fn nested(&self) -> bool {
        false
    }

    /// `q` or `Esc` on a pane-local view: close it, and return the row the cursor goes back
    /// to; `None` when no such view is open.
    fn back(&mut self) -> Option<usize> {
        None
    }

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

    /// After a command handed the terminal ran: read again what it may have changed, without
    /// anything `R` alone may start (a charged probe).
    fn reread(&mut self, scope: &Scope) -> Result<(), String> {
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
