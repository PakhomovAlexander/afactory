//! `af` with no subcommand: a full-screen, vim-shaped browser over what af knows about the place
//! it was started in (docs/design/tui.md, ADR-0119).
//!
//! It is read-first. Every pane is a projection of data a CLI command already prints, read by
//! the function behind that command, and every mutation goes through the CLI's own path: a
//! command line parsed by the CLI's clap definition, or `$EDITOR` on a declared file. Nothing
//! here turns working-tree bytes into execution authority.

use std::path::{Path, PathBuf};

use clap::{CommandFactory as _, Parser as _};

use crate::cli;

mod keymap;
mod paint;
pub(crate) mod panes;
mod scope;
mod term;
mod tree;

use keymap::{Action, Key, KeyMap, Prompt, PromptEvent};
use paint::{Frame, Paint, Span};
use panes::pipelines::PipelinesPane;
use panes::providers::ProvidersPane;
use panes::settings::SettingsPane;
use panes::{Effect, Pane, Placeholder, Row};
use scope::{Scope, ScopeKind};
use tree::{NodeKind, Tab, Tree};

/// Columns the bar takes, its `|` separator included.
const BAR_WIDTH: usize = 28;
/// Below this width the bar starts hidden; `<C-b>` still shows it.
const BAR_MIN_WIDTH: usize = 90;
const MIN_WIDTH: usize = 80;
const MIN_HEIGHT: usize = 24;
const BAR_LEGEND: &str = "j/k move  Enter open  zo/zc fold  / search  :cmd  q quit";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Every pane: the root's settings, then the four folders.
const PANES: [Option<Tab>; 5] = [
    None,
    Some(Tab::Providers),
    Some(Tab::Workers),
    Some(Tab::Pipelines),
    Some(Tab::Tasks),
];

const KEYS: &str = "\
KEYS  (docs/design/tui.md section 4)

j k                row down / up
h l                bar: close / open a folder, or go to parent / first child
                   main pane: scroll sideways
gg G               first / last row
<C-d> <C-u>        half page down / up
<C-f> <C-b>        page down / up; <C-b> on the bar hides it, and shows it
zo zc za           open / close / toggle the folder under the cursor
zR zM              open / close every folder
/ ? n N            search the focused region forward / backward; again
Tab <C-w>l <C-w>h  move focus between the bar and the main pane
]] [[              next / previous folder
Enter o            open the selected node in the main pane
y                  copy the selected id to the clipboard (OSC 52)
gf                 open the file behind the node in $EDITOR
R                  read the pane again; providers: probe quota windows
q ZZ :q            quit; <C-c> first cancels a prompt or a running probe

:e user|directory|project|local   af config edit --layer ...
:help [TOPIC|COMMAND]             af help ...
:cd DIR                           browse another directory
:scope user|project               switch the scope
Any other : line is parsed as an af command line.";

/// What an effect needs from the terminal: handing it to a child and taking it back, and
/// writing one control sequence.
pub(crate) trait Host {
    fn release(&mut self) -> Result<(), String>;
    fn reenter(&mut self) -> Result<(), String>;
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Whether bare `af` opens the browser: only when stdout is a terminal.
pub(crate) fn wanted() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// `af` with no subcommand on a terminal: resolve the scope and browse until `q`.
pub(crate) fn launch(repo: Option<&Path>) -> Result<i32, String> {
    let scope = match repo {
        Some(dir) => {
            let scope = Scope::resolve(Some(dir))?;
            if scope.kind != ScopeKind::Project {
                let dir = dir.display();
                return Err(format!("--repo {dir}: not inside a git repository"));
            }
            scope
        }
        None => Scope::resolve(None)?,
    };
    let start = match repo {
        Some(dir) => Some(dir.to_path_buf()),
        None => std::env::current_dir().ok(),
    };
    let panes = Panes::new(ProvidersPane::discovering());
    let mut app = App::new(scope, start, panes);
    let mut session = term::Session::open()?;
    let outcome = run(&mut app, &mut session);
    session.close();
    outcome.map(|()| 0)
}

/// The event loop: paint what changed, read keys for at most a tenth of a second, collect
/// background work, again.
fn run(app: &mut App, session: &mut term::Session) -> Result<(), String> {
    let color = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let mut shown = Vec::new();
    let mut size = (0, 0);
    let mut buffer = [0_u8; 512];
    let mut decoder = keymap::Decoder::default();
    loop {
        let now = session.size();
        if session.take_dirty() || now != size {
            size = now;
            shown.clear();
            session.send(b"\x1b[2J")?;
        }
        let frame = app.frame(size.0, size.1);
        session.paint(&frame, &mut shown, color)?;
        if app.quit {
            return Ok(());
        }
        let count = session.read(&mut buffer)?;
        let keys = if count == 0 {
            decoder.flush()
        } else {
            decoder.feed(&buffer[..count])
        };
        for key in keys {
            if let Some(effect) = app.key(key) {
                app.apply(effect, session);
            }
            if app.quit {
                break;
            }
        }
        app.poll();
    }
}

/// The panes, one per node kind.
pub(crate) struct Panes {
    settings: SettingsPane,
    providers: ProvidersPane,
    pipelines: PipelinesPane,
    workers: Placeholder,
    tasks: Placeholder,
}

impl Panes {
    pub(crate) fn new(providers: ProvidersPane) -> Panes {
        Panes {
            settings: SettingsPane::default(),
            providers,
            pipelines: PipelinesPane::default(),
            workers: Placeholder::new("WORKERS", 4),
            tasks: Placeholder::new("TASKS", 3),
        }
    }

    /// The root's settings for `None`, or a folder's pane.
    fn get(&self, tab: Option<Tab>) -> &dyn Pane {
        match tab {
            None => &self.settings,
            Some(Tab::Providers) => &self.providers,
            Some(Tab::Workers) => &self.workers,
            Some(Tab::Pipelines) => &self.pipelines,
            Some(Tab::Tasks) => &self.tasks,
        }
    }

    fn get_mut(&mut self, tab: Option<Tab>) -> &mut dyn Pane {
        match tab {
            None => &mut self.settings,
            Some(Tab::Providers) => &mut self.providers,
            Some(Tab::Workers) => &mut self.workers,
            Some(Tab::Pipelines) => &mut self.pipelines,
            Some(Tab::Tasks) => &mut self.tasks,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Bar,
    Main,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Command,
    Search { forward: bool },
}

/// Whether the bar shows: by the terminal's width until `<C-b>` decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bar {
    Auto,
    Shown,
    Hidden,
}

/// What the main pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Opened {
    Root,
    Folder(Tab),
    Item(Tab, String),
}

/// The main pane's cursor row, first visible row, and first visible column.
#[derive(Clone, Copy, Debug, Default)]
struct View {
    cursor: usize,
    top: usize,
    left: usize,
}

pub(crate) struct App {
    scope: Scope,
    /// Where `:scope project` returns: `--repo`, the directory `af` started in, or the last
    /// project `:cd` entered.
    start: Option<PathBuf>,
    panes: Panes,
    tree: Tree,
    opened: Opened,
    focus: Focus,
    bar: Bar,
    mode: Mode,
    keymap: KeyMap,
    prompt: Prompt,
    /// The last search and whether it ran forward.
    search: Option<(String, bool)>,
    /// A status-line message and whether it is an error.
    message: Option<(String, bool)>,
    /// `:help` rows, shown in place of the main pane.
    help: Option<Vec<Row>>,
    main: View,
    bar_top: usize,
    size: (usize, usize),
    quit: bool,
}

impl App {
    pub(crate) fn new(scope: Scope, start: Option<PathBuf>, panes: Panes) -> App {
        let tree = Tree::new(&root_label(&scope));
        let mut app = App {
            scope,
            start,
            panes,
            tree,
            opened: Opened::Root,
            focus: Focus::Bar,
            bar: Bar::Auto,
            mode: Mode::Normal,
            keymap: KeyMap::default(),
            prompt: Prompt::default(),
            search: None,
            message: None,
            help: None,
            main: View::default(),
            bar_top: 0,
            size: (BAR_MIN_WIDTH, MIN_HEIGHT),
            quit: false,
        };
        app.load();
        app
    }

    /// Read every pane for the scope and put what they found in the bar.
    fn load(&mut self) {
        for tab in PANES {
            let loaded = self.panes.get_mut(tab).load(&self.scope);
            if let Err(error) = loaded {
                self.say_error(error);
            }
        }
        self.sync();
        let opened = self.opened.clone();
        self.show(opened);
    }

    fn sync(&mut self) {
        for tab in Tab::ALL {
            let items = self.panes.get(Some(tab)).items();
            self.tree.set_items(tab, items);
        }
    }

    /// Collect background work; the bar follows what it found.
    pub(crate) fn poll(&mut self) {
        let mut changed = false;
        for tab in PANES {
            changed |= self.panes.get_mut(tab).poll();
        }
        if changed {
            self.sync();
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    fn say_error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    fn opened_tab(&self) -> Option<Tab> {
        match &self.opened {
            Opened::Root => None,
            Opened::Folder(tab) | Opened::Item(tab, _) => Some(*tab),
        }
    }

    fn pane(&self) -> &dyn Pane {
        self.panes.get(self.opened_tab())
    }

    fn main_rows(&self) -> &[Row] {
        match &self.help {
            Some(rows) => rows,
            None => self.pane().rows(),
        }
    }

    fn open_selected(&mut self) {
        let row = self.tree.selected();
        let opened = match row.kind {
            NodeKind::Root => Opened::Root,
            NodeKind::Folder(tab) => Opened::Folder(tab),
            NodeKind::Item(tab) => Opened::Item(tab, row.id),
        };
        self.show(opened);
    }

    fn show(&mut self, opened: Opened) {
        match &opened {
            Opened::Root => {}
            Opened::Folder(tab) => self.panes.get_mut(Some(*tab)).open(None),
            Opened::Item(tab, id) => self.panes.get_mut(Some(*tab)).open(Some(id.as_str())),
        }
        self.opened = opened;
        self.main = View::default();
        self.help = None;
    }

    /// The selected node, as the status line names it.
    pub(crate) fn breadcrumb(&self) -> String {
        let row = self.tree.selected();
        match row.kind {
            NodeKind::Root => self.scope.name(),
            NodeKind::Folder(tab) => format!("{}/", tab.name()),
            NodeKind::Item(tab) => format!("{}/{}", tab.name(), row.label),
        }
    }

    // ------------------------------------------------------------------------------------
    // keys

    /// One key press, and the effect it asks of the event loop, if any.
    pub(crate) fn key(&mut self, key: Key) -> Option<Effect> {
        self.settle_focus();
        match self.mode {
            Mode::Command => return self.command_key(key),
            Mode::Search { forward } => {
                self.search_key(key, forward);
                return None;
            }
            Mode::Normal => {}
        }
        let action = self.keymap.feed(key)?;
        self.message = None;
        if self.help.is_some() {
            match action {
                Action::Quit | Action::Cancel => self.help = None,
                Action::Pane(_) => {}
                _ => {
                    self.focus = Focus::Main;
                    return self.action(action);
                }
            }
            return None;
        }
        self.action(action)
    }

    fn action(&mut self, action: Action) -> Option<Effect> {
        let page = isize::try_from(self.size.1.saturating_sub(1)).unwrap_or(isize::MAX);
        match action {
            Action::Down => self.step(1),
            Action::Up => self.step(-1),
            Action::HalfDown => self.step(page / 2),
            Action::HalfUp => self.step(-page / 2),
            Action::PageDown => self.step(page),
            Action::PageUp if self.focus == Focus::Bar || !self.bar_visible() => self.toggle_bar(),
            Action::PageUp => self.step(-page),
            Action::Top => self.jump(false),
            Action::Bottom => self.jump(true),
            Action::Left => self.left(),
            Action::Right => self.right(),
            Action::FoldOpen => self.tree.fold_open(),
            Action::FoldClose => self.tree.fold_close(),
            Action::FoldToggle => self.tree.fold_toggle(),
            Action::FoldOpenAll => self.tree.fold_all(false),
            Action::FoldCloseAll => self.tree.fold_all(true),
            Action::SearchForward => self.begin(Mode::Search { forward: true }),
            Action::SearchBackward => self.begin(Mode::Search { forward: false }),
            Action::SearchNext => self.repeat_search(true),
            Action::SearchPrevious => self.repeat_search(false),
            Action::FocusNext => self.focus_next(),
            Action::FocusMain => self.focus_on(Focus::Main),
            Action::FocusBar => self.focus_on(Focus::Bar),
            Action::NextFolder => self.tree.next_folder(),
            Action::PreviousFolder => self.tree.previous_folder(),
            Action::Open if self.focus == Focus::Bar => self.open_selected(),
            Action::Open => {}
            Action::Yank => return self.yank(),
            Action::EditFile => return self.edit_file(),
            Action::Refresh => return Some(Effect::Refresh),
            Action::Quit => return Some(Effect::Quit),
            Action::Command => self.begin(Mode::Command),
            Action::Cancel => self.cancel(),
            Action::Pane(key) => return self.pane_key(key),
        }
        None
    }

    fn step(&mut self, delta: isize) {
        match self.focus {
            Focus::Bar => self.tree.move_by(delta),
            Focus::Main => {
                let last = self.main_rows().len().saturating_sub(1);
                self.main.cursor = self.main.cursor.saturating_add_signed(delta).min(last);
            }
        }
    }

    fn jump(&mut self, end: bool) {
        match (self.focus, end) {
            (Focus::Bar, false) => self.tree.top(),
            (Focus::Bar, true) => self.tree.bottom(),
            (Focus::Main, false) => self.main.cursor = 0,
            (Focus::Main, true) => self.main.cursor = self.main_rows().len().saturating_sub(1),
        }
    }

    fn left(&mut self) {
        match self.focus {
            Focus::Bar => self.tree.left(),
            Focus::Main => self.main.left = self.main.left.saturating_sub(8),
        }
    }

    fn right(&mut self) {
        match self.focus {
            Focus::Bar => {
                if self.tree.right() {
                    self.open_selected();
                }
            }
            Focus::Main => {
                let widest = self.main_rows().iter().map(|row| row.text().len()).max();
                let limit = widest.unwrap_or(0).saturating_sub(1);
                self.main.left = (self.main.left + 8).min(limit);
            }
        }
    }

    fn bar_visible(&self) -> bool {
        match self.bar {
            Bar::Auto => self.size.0 >= BAR_MIN_WIDTH,
            Bar::Shown => true,
            Bar::Hidden => false,
        }
    }

    /// The bar cannot keep focus while it is hidden.
    fn settle_focus(&mut self) {
        if !self.bar_visible() {
            self.focus = Focus::Main;
        }
    }

    fn toggle_bar(&mut self) {
        if self.bar_visible() {
            self.bar = Bar::Hidden;
            self.focus = Focus::Main;
        } else {
            self.bar = Bar::Shown;
            self.focus = Focus::Bar;
        }
    }

    fn focus_on(&mut self, focus: Focus) {
        if focus == Focus::Bar && !self.bar_visible() {
            self.bar = Bar::Shown;
        }
        self.focus = focus;
    }

    fn focus_next(&mut self) {
        let next = match self.focus {
            Focus::Bar => Focus::Main,
            Focus::Main => Focus::Bar,
        };
        self.focus_on(next);
    }

    /// `<C-c>` and `Esc`: stop a running probe; otherwise clear the message.
    fn cancel(&mut self) {
        let mut cancelled = false;
        for tab in PANES {
            cancelled |= self.panes.get_mut(tab).cancel();
        }
        if cancelled {
            self.say("cancelled");
        }
    }

    fn yank(&mut self) -> Option<Effect> {
        let text = match self.focus {
            Focus::Main => self.pane().yank(self.main.cursor),
            Focus::Bar => self.node_id(),
        };
        if text.is_none() {
            self.say_error("nothing to yank here");
        }
        text.map(Effect::Yank)
    }

    fn node_id(&self) -> Option<String> {
        let row = self.tree.selected();
        match row.kind {
            NodeKind::Root => Some(self.scope.root.display().to_string()),
            NodeKind::Folder(_) => None,
            NodeKind::Item(_) => Some(row.label),
        }
    }

    fn edit_file(&mut self) -> Option<Effect> {
        let file = match self.focus {
            Focus::Main => self.pane().file(self.main.cursor),
            Focus::Bar => self.node_file(),
        };
        if file.is_none() {
            self.say_error("no file behind this node");
        }
        file.map(Effect::OpenEditor)
    }

    fn node_file(&self) -> Option<PathBuf> {
        let row = self.tree.selected();
        match row.kind {
            NodeKind::Root => self.panes.settings.file(0),
            NodeKind::Item(Tab::Pipelines) => self.panes.pipelines.entry_file(&row.id),
            NodeKind::Item(Tab::Providers) => self.panes.providers.file(0),
            NodeKind::Folder(_) | NodeKind::Item(_) => None,
        }
    }

    fn pane_key(&mut self, key: Key) -> Option<Effect> {
        let (tab, row) = (self.opened_tab(), self.main.cursor);
        let outcome = self.panes.get_mut(tab).key(key, row);
        match outcome {
            Ok(effect) => effect,
            Err(error) => {
                self.say_error(error);
                None
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // prompts

    fn begin(&mut self, mode: Mode) {
        self.mode = mode;
        self.prompt = Prompt::default();
    }

    fn command_key(&mut self, key: Key) -> Option<Effect> {
        match self.prompt.key(key) {
            PromptEvent::Edited => None,
            PromptEvent::Complete => {
                self.complete();
                None
            }
            PromptEvent::Cancel => {
                self.mode = Mode::Normal;
                None
            }
            PromptEvent::Submit(line) => {
                self.mode = Mode::Normal;
                self.command(&line)
            }
        }
    }

    fn search_key(&mut self, key: Key, forward: bool) {
        match self.prompt.key(key) {
            PromptEvent::Submit(query) => {
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    self.search = Some((query, forward));
                }
                self.repeat_search(true);
            }
            PromptEvent::Cancel => self.mode = Mode::Normal,
            PromptEvent::Edited | PromptEvent::Complete => {}
        }
    }

    /// `n` (`same`) and `N`: the last search again, in its own direction or against it.
    fn repeat_search(&mut self, same: bool) {
        let Some((query, forward)) = self.search.clone() else {
            self.say_error("no previous search");
            return;
        };
        let forward = forward == same;
        let found = match self.focus {
            Focus::Bar => self.tree.search(&query, forward),
            Focus::Main => self.search_main(&query, forward),
        };
        if !found {
            self.say_error(format!("pattern not found: {query}"));
        }
    }

    fn search_main(&mut self, query: &str, forward: bool) -> bool {
        let query = query.to_ascii_lowercase();
        let mut texts = Vec::new();
        for row in self.main_rows() {
            texts.push(row.text().to_ascii_lowercase());
        }
        let count = texts.len();
        for step in 1..=count {
            let index = if forward {
                (self.main.cursor + step) % count
            } else {
                (self.main.cursor + count - step) % count
            };
            if texts[index].contains(&query) {
                self.main.cursor = index;
                return true;
            }
        }
        false
    }

    /// A `:` line. `q`, `cd` and `scope` belong to the browser; `e` and `help` spell
    /// `af config edit` and `af help`; any other line is an `af` command line.
    fn command(&mut self, line: &str) -> Option<Effect> {
        let words = match shell_words::split(line) {
            Ok(words) => words,
            Err(error) => {
                self.say_error(format!(":{line}: {error}"));
                return None;
            }
        };
        let (verb, rest) = words.split_first()?;
        match (verb.as_str(), rest) {
            ("q" | "quit", []) => Some(Effect::Quit),
            ("cd", [dir]) => {
                self.cd(dir);
                None
            }
            ("cd", _) => {
                self.say_error("usage: :cd DIR");
                None
            }
            ("scope", [which]) => {
                self.rescope(which);
                None
            }
            ("scope", _) => {
                self.say_error("usage: :scope user|project");
                None
            }
            ("e" | "edit", _) => Some(Effect::RunCommand(self.edit_words(rest))),
            ("help", _) => {
                let mut help = vec!["help".to_owned()];
                help.extend(rest.iter().cloned());
                Some(Effect::RunCommand(help))
            }
            _ => Some(Effect::RunCommand(words.clone())),
        }
    }

    /// `:e [LAYER]` as `af config edit`, for this scope's repository.
    fn edit_words(&self, rest: &[String]) -> Vec<String> {
        let mut words = vec!["config".to_owned(), "edit".to_owned()];
        if !rest.is_empty() {
            words.push("--layer".to_owned());
            words.extend(rest.iter().cloned());
        }
        if let Some(toplevel) = self.scope.toplevel() {
            words.push("--repo".to_owned());
            words.push(toplevel.display().to_string());
        }
        words
    }

    /// `<Tab>` on the `:` line: complete the verb, or the argument of `:e` and `:scope`.
    fn complete(&mut self) {
        let text = self.prompt.text.clone();
        let (head, word) = match text.rsplit_once(' ') {
            Some((head, word)) => (head.trim(), word),
            None => ("", text.as_str()),
        };
        let choices: &[&str] = match head {
            "" => &["cd", "e", "help", "q", "scope"],
            "e" | "edit" => &["user", "directory", "project", "local"],
            "scope" => &["user", "project"],
            _ => &[],
        };
        let mut matches = Vec::new();
        for choice in choices {
            if choice.starts_with(word) {
                matches.push(*choice);
            }
        }
        match matches.as_slice() {
            [] => {}
            [only] => {
                let completed = if head.is_empty() {
                    format!("{only} ")
                } else {
                    format!("{head} {only}")
                };
                self.prompt.set(&completed);
            }
            many => self.say(many.join("  ")),
        }
    }

    fn cd(&mut self, dir: &str) {
        let path = match (dir.strip_prefix('~'), &self.scope.home) {
            (Some(rest), Some(home)) => home.join(rest.trim_start_matches('/')),
            _ => self.scope.root.join(dir),
        };
        if !path.is_dir() {
            self.say_error(format!("{}: not a directory", path.display()));
            return;
        }
        match Scope::resolve(Some(path.as_path())) {
            Ok(scope) => self.enter_scope(scope),
            Err(error) => self.say_error(error),
        }
    }

    fn rescope(&mut self, which: &str) {
        let scope = match which {
            "user" => Scope::user(),
            "project" => self.project_scope(),
            other => Err(format!("the scope is user or project, not {other}")),
        };
        match scope {
            Ok(scope) => self.enter_scope(scope),
            Err(error) => self.say_error(error),
        }
    }

    fn project_scope(&self) -> Result<Scope, String> {
        let start = self.start.clone();
        let start = start.ok_or("no repository to return to; :cd DIR into one")?;
        let scope = Scope::resolve(Some(start.as_path()))?;
        let start = start.display();
        match scope.kind {
            ScopeKind::Project => Ok(scope),
            ScopeKind::User => Err(format!("{start} is not inside a git repository")),
        }
    }

    fn enter_scope(&mut self, scope: Scope) {
        if scope.kind == ScopeKind::Project {
            self.start = Some(scope.root.clone());
        }
        self.scope = scope;
        self.tree.set_root(&root_label(&self.scope));
        self.opened = Opened::Root;
        self.load();
        self.tree.top();
        self.focus_on(Focus::Bar);
        let word = self.scope.word();
        let root = self.scope.abbreviate(&self.scope.root);
        self.say(format!("{word} scope: {root}"));
    }

    // ------------------------------------------------------------------------------------
    // effects

    /// Carry out an effect, handing the terminal to a child when one needs it.
    pub(crate) fn apply(&mut self, effect: Effect, host: &mut dyn Host) {
        match effect {
            Effect::Quit => self.quit = true,
            Effect::Refresh => self.refresh(),
            Effect::Yank(text) => {
                let sequence = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
                match host.send(sequence.as_bytes()) {
                    Ok(()) => self.say(format!("yanked {text}")),
                    Err(error) => self.say_error(error),
                }
            }
            Effect::OpenEditor(path) => {
                let outcome = handed_off(host, || crate::config::open_in_editor(&path));
                let shown = self.scope.display(&path);
                self.finish(outcome, format!("edited {shown}"));
            }
            Effect::RunCommand(words) => self.run_command(&words, host),
        }
    }

    /// A command line parsed by the CLI's own clap definition, so the browser never grows a
    /// second grammar. This package runs `af config edit`, with the terminal handed over, and
    /// `af help`; any other command is named, not run.
    fn run_command(&mut self, words: &[String], host: &mut dyn Host) {
        let argv = std::iter::once("af").chain(words.iter().map(String::as_str));
        let parsed = match cli::Af::try_parse_from(argv) {
            Ok(parsed) => parsed,
            Err(error) => {
                let rendered = error.render().to_string();
                self.say_error(first_line(&rendered));
                return;
            }
        };
        match handoff(parsed.command) {
            Handoff::Edit(layer, repo) => {
                let outcome = handed_off(host, || crate::config::edit(layer, repo.as_deref()));
                self.finish(outcome, "configuration edited".to_owned());
            }
            Handoff::Help(topic) => {
                self.help = Some(help_rows(&topic));
                self.main = View::default();
            }
            Handoff::Other => {
                let line = words.join(" ");
                let later = "the browser hands commands off in a later package";
                self.say_error(format!("af {line} runs from the shell; {later}"));
            }
        }
    }

    /// After a hand-off: read the scope again, since the child may have changed what the
    /// panes show, and say how it went.
    fn finish(&mut self, outcome: Result<(), String>, done: String) {
        // A configuration that no longer loads is the news, whatever the child did: the
        // settings on screen are the old ones, and saying "edited" would call them current.
        let reloaded = self.reload();
        // The child may have changed what the opened pane shows (an edited pipeline now
        // differs from HEAD): that pane reads again too, keeping what is opened.
        if let Some(tab) = self.opened_tab() {
            if let Err(error) = self.panes.get_mut(Some(tab)).refresh(&self.scope) {
                self.say_error(error);
            }
            self.sync();
            let opened = self.opened.clone();
            self.show(opened);
        }
        match (reloaded, outcome) {
            (Err(stale), _) => self.say_error(stale),
            (Ok(()), Ok(())) => self.say(done),
            (Ok(()), Err(error)) => self.say_error(error),
        }
    }

    /// Read the scope and the settings pane again. On failure the scope and the pane keep
    /// what they showed, and the error names why they are stale.
    fn reload(&mut self) -> Result<(), String> {
        self.scope = self.scope.reload()?;
        self.panes.settings.load(&self.scope)
    }

    /// `R`: the opened pane reads everything again.
    fn refresh(&mut self) {
        let tab = self.opened_tab();
        if tab.is_none() {
            match self.reload() {
                Ok(()) => self.say("settings read again"),
                Err(error) => self.say_error(format!("settings not refreshed: {error}")),
            }
            return;
        }
        let refreshed = self.panes.get_mut(tab).refresh(&self.scope);
        if let Err(error) = refreshed {
            self.say_error(error);
        }
        self.sync();
        let opened = self.opened.clone();
        self.show(opened);
    }

    // ------------------------------------------------------------------------------------
    // painting

    /// The screen at `width` x `height`: the bar, the main pane and the status line, or one
    /// line naming the minimum size.
    pub(crate) fn frame(&mut self, width: usize, height: usize) -> Frame {
        self.size = (width, height);
        self.settle_focus();
        let mut frame = Frame::new(width, height);
        if width < MIN_WIDTH || height < MIN_HEIGHT {
            let line = [Span::new(refusal(width, height), Paint::Error)];
            frame.paint_spans(0, 0, width, &line, Paint::Plain);
            return frame;
        }
        let body = height - 1;
        let left = if self.bar_visible() {
            self.paint_bar(&mut frame, body);
            BAR_WIDTH
        } else {
            0
        };
        self.paint_main(&mut frame, left, width - left, body);
        let fill = match self.mode {
            Mode::Normal => Paint::Status,
            Mode::Command | Mode::Search { .. } => Paint::Plain,
        };
        let status = self.status_spans(width);
        frame.paint_spans(body, 0, width, &status, fill);
        frame
    }

    fn paint_bar(&mut self, frame: &mut Frame, body: usize) {
        let inner = BAR_WIDTH - 1;
        let header = Span::new(self.scope.abbreviate(&self.scope.root), Paint::Muted);
        frame.paint_spans(0, 0, inner, &[header], Paint::Plain);
        let rows = self.tree.rows();
        let cursor = self.tree.cursor();
        let visible = body - 1;
        if cursor < self.bar_top {
            self.bar_top = cursor;
        } else if cursor >= self.bar_top + visible {
            self.bar_top = cursor + 1 - visible;
        }
        for (index, row) in rows.iter().enumerate().skip(self.bar_top).take(visible) {
            let mut text = row.text();
            if let NodeKind::Folder(tab) = row.kind
                && let Some(spinner) = self.panes.get(Some(tab)).busy()
            {
                text.push(' ');
                text.push(spinner);
            }
            let paint = match (index == cursor, self.focus) {
                (true, Focus::Bar) => Paint::Cursor,
                (true, Focus::Main) => Paint::Marked,
                (false, _) if row.muted => Paint::Muted,
                (false, _) => Paint::Plain,
            };
            let fill = if index == cursor { paint } else { Paint::Plain };
            let line = index - self.bar_top + 1;
            frame.paint_spans(line, 0, inner, &[Span::new(text, paint)], fill);
        }
        let separator = [Span::new("|", Paint::Muted)];
        for line in 0..body {
            frame.paint_spans(line, inner, 1, &separator, Paint::Muted);
        }
    }

    fn paint_main(&mut self, frame: &mut Frame, left: usize, width: usize, body: usize) {
        let count = self.main_rows().len();
        let view = &mut self.main;
        view.cursor = view.cursor.min(count.saturating_sub(1));
        if view.cursor < view.top {
            view.top = view.cursor;
        } else if view.cursor >= view.top + body {
            view.top = view.cursor + 1 - body;
        }
        let view = self.main;
        let focused = self.focus == Focus::Main;
        let rows = self.main_rows();
        for (index, row) in rows.iter().enumerate().skip(view.top).take(body) {
            let mut spans = paint::scrolled(&row.spans, view.left);
            let line = index - view.top;
            if focused && index == view.cursor {
                for span in &mut spans {
                    span.paint = Paint::Cursor;
                }
                frame.paint_spans(line, left, width, &spans, Paint::Cursor);
            } else {
                frame.paint_spans(line, left, width, &spans, Paint::Plain);
            }
        }
    }

    fn status_spans(&self, width: usize) -> Vec<Span> {
        match self.mode {
            Mode::Command => return self.prompt_spans(':'),
            Mode::Search { forward: true } => return self.prompt_spans('/'),
            Mode::Search { forward: false } => return self.prompt_spans('?'),
            Mode::Normal => {}
        }
        let word = match self.help {
            Some(_) => "HELP",
            None => "NORMAL",
        };
        let mut left = format!("{word}  {}", self.breadcrumb());
        if let Some(pending) = self.keymap.pending() {
            left.push_str("  ");
            left.push_str(pending);
        }
        let (right, paint) = match &self.message {
            Some((text, true)) => (text.clone(), Paint::Error),
            Some((text, false)) => (text.clone(), Paint::Status),
            None => (self.legend(), Paint::Status),
        };
        // The message, binding or legend on the right is what the line is for. At a narrow
        // width the breadcrumb shrinks to the mode word, then goes, before the right is cut.
        let fits = |left: &str| left.len() + 2 + right.len() <= width;
        if !fits(&left) {
            left = word.to_owned();
        }
        if !fits(&left) {
            return vec![Span::new(right, paint)];
        }
        let gap = width.saturating_sub(left.len() + right.len()).max(2);
        vec![
            Span::new(left, Paint::Status),
            Span::new(" ".repeat(gap), Paint::Status),
            Span::new(right, paint),
        ]
    }

    /// The `:` or `/` line, with the cursor on its character.
    fn prompt_spans(&self, lead: char) -> Vec<Span> {
        let text = &self.prompt.text;
        let (before, after) = text.split_at(self.prompt.cursor.min(text.len()));
        let mut rest = after.chars();
        let under = rest.next().unwrap_or(' ');
        vec![
            Span::new(format!("{lead}{before}"), Paint::Plain),
            Span::new(under.to_string(), Paint::Cursor),
            Span::new(rest.as_str(), Paint::Plain),
        ]
    }

    /// The key legend, or what the pane says about the row under the main cursor.
    fn legend(&self) -> String {
        if self.help.is_some() {
            return "j/k scroll  q close help".to_owned();
        }
        match self.focus {
            Focus::Bar => BAR_LEGEND.to_owned(),
            Focus::Main => {
                let pane = self.pane();
                let status = pane.status(self.main.cursor);
                status.unwrap_or_else(|| pane.legend().to_owned())
            }
        }
    }
}

/// The part of a parsed command line the browser runs itself.
enum Handoff {
    Edit(cli::LayerArg, Option<PathBuf>),
    Help(Vec<String>),
    Other,
}

fn handoff(command: Option<cli::Command>) -> Handoff {
    match command {
        Some(cli::Command::Config {
            command: cli::ConfigCommand::Edit { layer, repo },
        }) => Handoff::Edit(layer, repo),
        Some(cli::Command::Help { words }) => Handoff::Help(words),
        _ => Handoff::Other,
    }
}

/// Hand the terminal to a child, and take it back whatever the child did.
fn handed_off(
    host: &mut dyn Host,
    child: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    host.release()?;
    let outcome = child();
    host.reenter()?;
    outcome
}

fn root_label(scope: &Scope) -> String {
    format!("{}  ({})", scope.name(), scope.word())
}

fn refusal(width: usize, height: usize) -> String {
    format!("af needs at least {MIN_WIDTH}x{MIN_HEIGHT}; this terminal is {width}x{height}")
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty());
    line.unwrap_or_default().trim().to_owned()
}

/// `:help` rows: the key reference, a topic as `af help TOPIC` prints it, or a command's long
/// help as `af help COMMAND...` prints it.
fn help_rows(words: &[String]) -> Vec<Row> {
    let text = match words {
        [] => KEYS.to_owned(),
        [word] if crate::topics::find(word).is_some() => topic_help(word),
        path => command_help(path),
    };
    let mut rows = Vec::new();
    for line in text.lines() {
        rows.push(Row::plain(typographic(line)));
    }
    rows
}

fn topic_help(word: &str) -> String {
    match crate::topics::find(word) {
        Some((topic, about, text)) => {
            let body = crate::topics::body(topic, text);
            format!("af help {topic} -- {about}\n\n{body}")
        }
        None => String::new(),
    }
}

fn command_help(path: &[String]) -> String {
    let mut root = cli::Af::command();
    let mut current = &mut root;
    for word in path {
        match current.find_subcommand_mut(word) {
            Some(sub) => current = sub,
            None => return format!("`{}` is neither a topic nor a command", path.join(" ")),
        }
    }
    let name = current.get_name().to_owned();
    let bin = format!("af {}", path.join(" "));
    current
        .clone()
        .name(name)
        .bin_name(bin)
        .render_long_help()
        .to_string()
}

/// Help text uses a few typographic marks; the terminal gets their ASCII spelling.
fn typographic(line: &str) -> String {
    let line = line.replace('\u{2014}', "--");
    let line = line.replace('\u{2026}', "...");
    line.replace('\u{b7}', "-")
}

/// Standard base64, for the OSC 52 clipboard sequence.
fn base64(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |index: usize| u32::from(chunk.get(index).copied().unwrap_or(0));
        let group = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 63;
                encoded.push(char::from(BASE64[sextet as usize]));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
pub(crate) mod tests;
