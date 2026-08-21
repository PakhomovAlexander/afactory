//! Interactive configuration proposals and launch surface for `reviewctl`.
//!
//! The TUI never turns working-tree bytes into execution authority. Reviewer edits stay in
//! memory and `s` exports an explicit patch under review state. The operator applies, reviews,
//! and commits that patch, then starts a new campaign whose `--authority` names that commit.
//! `r` always delegates to the ordinary pinned-authority run path.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, size,
};
use crossterm::{execute, queue};
use review_config::lock::{
    Lockfile, PackageManifest, Registry, reviewer_runner_settings, update_reviewer_runner_settings,
};
use review_config::pipeline_edit::{
    PipelineSetting, PipelineView, add_reviewer, pipeline_view, rebind_reviewer, remove_reviewer,
    update_pipeline_setting, validate_pipeline, validate_pipeline_structure,
};

use crate::Options;

const CONFIG_ROWS: usize = 3;
const RUN_ROWS: usize = 6;
const POLICY_ROWS: usize = 5;

pub fn launch(options: Options) -> Result<(), String> {
    let mut app = App::load(options)?;
    let mut terminal = TerminalSession::new()?;
    let result = event_loop(&mut terminal, &mut app);
    terminal.leave()?;
    result
}

fn event_loop(terminal: &mut TerminalSession, app: &mut App) -> Result<(), String> {
    terminal.draw(app)?;
    loop {
        if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
            continue;
        }
        let key = match event::read().map_err(|error| error.to_string())? {
            Event::Resize(_, _) => {
                terminal.draw(app)?;
                continue;
            }
            Event::Key(key) => key,
            _ => continue,
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        match app.handle_key(key) {
            Action::Continue => {}
            Action::Quit => return Ok(()),
            Action::Export => match app.export_configuration() {
                Ok((reviewers, pipeline, path)) => app.success(format!(
                    "Exported pipeline={} and {reviewers} reviewer change(s) to {}; apply and commit before a new campaign",
                    yes_no(pipeline),
                    path.display()
                )),
                Err(error) => app.failure(error),
            },
            Action::Reload => match App::load(app.options.clone()) {
                Ok(reloaded) => {
                    *app = reloaded;
                    app.success(
                        "Reloaded worktree configuration; use a new campaign and committed authority after applying a proposal",
                    );
                }
                Err(error) => app.failure(format!("Reload refused: {error}")),
            },
            Action::Run => {
                terminal.leave()?;
                println!(
                    "reviewctl tui: terminal released; starting the ordinary pinned-authority run path\n"
                );
                let result = crate::run(&app.options);
                match &result {
                    Ok(verdict) => println!("\nreviewctl tui: run completed: {verdict:?}"),
                    Err(error) => println!("\nreviewctl tui: run failed: {error}"),
                }
                print!("Press Enter to return to the TUI...");
                io::stdout().flush().map_err(|error| error.to_string())?;
                let mut input = String::new();
                io::stdin()
                    .read_line(&mut input)
                    .map_err(|error| error.to_string())?;
                terminal.enter()?;
                match result {
                    Ok(verdict) => app.success(format!("Run completed: {verdict:?}")),
                    Err(error) => app.failure(format!("Run failed: {error}")),
                }
            }
        }
        terminal.draw(app)?;
    }
}

struct TerminalSession {
    stdout: Stdout,
    active: bool,
}

impl TerminalSession {
    fn new() -> Result<Self, String> {
        let mut session = Self {
            stdout: io::stdout(),
            active: false,
        };
        session.enter()?;
        Ok(session)
    }

    fn enter(&mut self) -> Result<(), String> {
        if self.active {
            return Ok(());
        }
        enable_raw_mode().map_err(|error| error.to_string())?;
        if let Err(error) = execute!(
            self.stdout,
            EnterAlternateScreen,
            Hide,
            Clear(ClearType::All)
        ) {
            let _ = disable_raw_mode();
            return Err(error.to_string());
        }
        self.active = true;
        Ok(())
    }

    fn leave(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        disable_raw_mode().map_err(|error| error.to_string())?;
        execute!(self.stdout, Show, LeaveAlternateScreen, ResetColor)
            .map_err(|error| error.to_string())?;
        self.active = false;
        Ok(())
    }

    fn draw(&mut self, app: &App) -> Result<(), String> {
        let (width, height) = size().map_err(|error| error.to_string())?;
        queue!(self.stdout, MoveTo(0, 0), Clear(ClearType::All))
            .map_err(|error| error.to_string())?;
        if width < 72 || height < 22 {
            paint(
                &mut self.stdout,
                0,
                0,
                width,
                "reviewctl tui needs at least 72x22",
                Paint::Error,
            )?;
            self.stdout.flush().map_err(|error| error.to_string())?;
            return Ok(());
        }

        paint(
            &mut self.stdout,
            0,
            0,
            width,
            " REVIEWCTL / INTERACTIVE REVIEW ",
            Paint::Header,
        )?;
        paint(
            &mut self.stdout,
            1,
            0,
            width,
            &format!(
                " repo {}  |  pipeline {}",
                app.options.repo.display(),
                app.options.pipeline.display()
            ),
            Paint::Muted,
        )?;

        paint(
            &mut self.stdout,
            2,
            1,
            14,
            if app.tab == TopTab::Pipelines {
                "[ PIPELINES ]"
            } else {
                "  PIPELINES  "
            },
            if app.tab == TopTab::Pipelines {
                Paint::Focus
            } else {
                Paint::Normal
            },
        )?;
        paint(
            &mut self.stdout,
            2,
            16,
            14,
            if app.tab == TopTab::Reviewers {
                "[ REVIEWERS ]"
            } else {
                "  REVIEWERS  "
            },
            if app.tab == TopTab::Reviewers {
                Paint::Focus
            } else {
                Paint::Normal
            },
        )?;

        let footer = height - 3;
        let left_width = (width / 3).clamp(24, 40);
        if app.tab == TopTab::Reviewers {
            for y in 3..footer {
                paint(&mut self.stdout, y, left_width, 1, "|", Paint::Muted)?;
            }
            paint(
                &mut self.stdout,
                3,
                1,
                left_width - 2,
                if app.pane == Pane::Reviewers {
                    "[ REVIEWERS ]"
                } else {
                    "  REVIEWERS  "
                },
                if app.pane == Pane::Reviewers {
                    Paint::Focus
                } else {
                    Paint::Normal
                },
            )?;
            let list_top = 5;
            let capacity = usize::from(footer.saturating_sub(list_top));
            let start = app
                .selected_reviewer
                .saturating_sub(capacity.saturating_sub(1));
            for (row, (index, reviewer)) in app
                .reviewers
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .enumerate()
            {
                let selected = index == app.selected_reviewer;
                paint(
                    &mut self.stdout,
                    list_top + row as u16,
                    1,
                    left_width - 2,
                    &format!(
                        "{} {}{}",
                        if selected { ">" } else { " " },
                        reviewer.name,
                        if reviewer.dirty { " *" } else { "" }
                    ),
                    if selected {
                        Paint::Selected
                    } else {
                        Paint::Normal
                    },
                )?;
            }

            let right_x = left_width + 2;
            let right_width = width - right_x - 1;
            let reviewer = &app.reviewers[app.selected_reviewer];
            paint(
                &mut self.stdout,
                3,
                right_x,
                right_width,
                if app.pane == Pane::Configuration {
                    "[ WORKTREE CONFIG PROPOSAL ]"
                } else {
                    "  WORKTREE CONFIG PROPOSAL  "
                },
                if app.pane == Pane::Configuration {
                    Paint::Focus
                } else {
                    Paint::Normal
                },
            )?;
            paint(
                &mut self.stdout,
                4,
                right_x,
                right_width,
                "Values below are worktree proposal inputs only; Run never executes them",
                Paint::Muted,
            )?;
            let config_values = [
                ("Backend (fixed)", reviewer.backend.as_str()),
                ("Model (worktree)", reviewer.model.as_str()),
                ("Effort (worktree)", reviewer.effort.as_str()),
            ];
            for (index, (label, value)) in config_values.iter().enumerate() {
                setting_row(
                    &mut self.stdout,
                    6 + index as u16,
                    right_x,
                    right_width,
                    label,
                    value,
                    app.pane == Pane::Configuration && app.selected_config == index,
                )?;
            }
            paint(
                &mut self.stdout,
                10,
                right_x,
                right_width,
                if app.pane == Pane::Run {
                    "[ PINNED-AUTHORITY RUN ]"
                } else {
                    "  PINNED-AUTHORITY RUN  "
                },
                if app.pane == Pane::Run {
                    Paint::Focus
                } else {
                    Paint::Normal
                },
            )?;
            paint(
                &mut self.stdout,
                11,
                right_x,
                right_width,
                "A resumed campaign keeps its original packages and ignores --authority",
                Paint::Muted,
            )?;
            let state = app
                .options
                .state
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(campaign default)".to_string());
            let run_values = [
                ("Campaign", optional(&app.options.campaign)),
                ("Authority", optional(&app.options.authority)),
                ("Focus", optional(&app.options.focus)),
                ("State", state),
                ("Uncommitted", yes_no(app.options.uncommitted).to_string()),
                (
                    "Restart round",
                    yes_no(app.options.restart_round).to_string(),
                ),
            ];
            for (index, (label, value)) in run_values.iter().enumerate() {
                setting_row(
                    &mut self.stdout,
                    12 + index as u16,
                    right_x,
                    right_width,
                    label,
                    value,
                    app.pane == Pane::Run && app.selected_run == index,
                )?;
            }
        } else {
            draw_pipeline_body(&mut self.stdout, app, width, footer, left_width)?;
        }

        paint(
            &mut self.stdout,
            footer,
            0,
            width,
            &"-".repeat(usize::from(width)),
            Paint::Muted,
        )?;
        if let Some(editor) = &app.editor {
            paint(
                &mut self.stdout,
                footer + 1,
                0,
                width,
                &format!(" EDIT {}: {}_", editor.label, editor.value),
                Paint::Focus,
            )?;
            paint(
                &mut self.stdout,
                footer + 2,
                0,
                width,
                " Enter apply | Esc cancel | Ctrl-U clear | Ctrl-W delete word",
                Paint::Muted,
            )?;
        } else {
            paint(
                &mut self.stdout,
                footer + 1,
                0,
                width,
                &format!(" {}", app.message),
                if app.message_is_error {
                    Paint::Error
                } else {
                    Paint::Success
                },
            )?;
            paint(
                &mut self.stdout,
                footer + 2,
                0,
                width,
                if app.tab == TopTab::Pipelines {
                    " j/k g/G move | h/l panes | Tab or H/L tabs | Enter edit | a add | d remove | s export | R reload | q"
                } else {
                    " j/k g/G C-u/C-d move | h/l panes | Tab or H/L tabs | Enter edit | s export | R reload | r run | q"
                },
                Paint::Muted,
            )?;
        }
        self.stdout.flush().map_err(|error| error.to_string())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

fn draw_pipeline_body(
    stdout: &mut Stdout,
    app: &App,
    width: u16,
    footer: u16,
    left_width: u16,
) -> Result<(), String> {
    for y in 3..footer {
        paint(stdout, y, left_width, 1, "|", Paint::Muted)?;
    }
    paint(
        stdout,
        3,
        1,
        left_width - 2,
        if app.pipeline_pane == PipelinePane::Nodes {
            "[ NODES ]"
        } else {
            "  NODES  "
        },
        if app.pipeline_pane == PipelinePane::Nodes {
            Paint::Focus
        } else {
            Paint::Normal
        },
    )?;
    let list_top = 5;
    let capacity = usize::from(footer.saturating_sub(list_top));
    let start = app.selected_node.saturating_sub(capacity.saturating_sub(1));
    for (row, (index, node)) in app
        .pipeline_view
        .nodes
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .enumerate()
    {
        let selected = index == app.selected_node;
        paint(
            stdout,
            list_top + row as u16,
            1,
            left_width - 2,
            &format!(
                "{} {:<14} {}",
                if selected { ">" } else { " " },
                node.id,
                node.kind
            ),
            if selected {
                Paint::Selected
            } else {
                Paint::Normal
            },
        )?;
    }

    let right_x = left_width + 2;
    let right_width = width - right_x - 1;
    paint(
        stdout,
        3,
        right_x,
        right_width,
        &format!(
            " WORKTREE GRAPH PROPOSAL{}",
            if app.pipeline_dirty { " *" } else { "" }
        ),
        Paint::Normal,
    )?;
    paint(
        stdout,
        4,
        right_x,
        right_width,
        "Run executes pinned authority, not this draft",
        Paint::Muted,
    )?;
    let visible_levels = if app.graph.levels.len() > 4 {
        3
    } else {
        app.graph.levels.len()
    };
    for (row, level) in app.graph.levels.iter().take(visible_levels).enumerate() {
        paint(
            stdout,
            5 + row as u16,
            right_x,
            right_width,
            &format!("L{row}  {}", level.join("  ||  ")),
            Paint::Muted,
        )?;
    }
    if app.graph.levels.len() > 4 {
        paint(
            stdout,
            8,
            right_x,
            right_width,
            &format!("... +{} more levels", app.graph.levels.len() - 3),
            Paint::Muted,
        )?;
    }

    let node = &app.pipeline_view.nodes[app.selected_node];
    let incoming = &app.graph.incoming[app.selected_node];
    let outgoing = &app.graph.outgoing[app.selected_node];
    paint(
        stdout,
        9,
        right_x,
        right_width,
        &format!(
            "NODE {} [{}] package={}",
            node.id,
            node.kind,
            node.package.as_deref().unwrap_or("-")
        ),
        Paint::Selected,
    )?;
    paint(
        stdout,
        10,
        right_x,
        right_width,
        &format!(
            "IN   {}",
            if incoming.is_empty() {
                "-"
            } else {
                incoming.as_str()
            }
        ),
        Paint::Muted,
    )?;
    paint(
        stdout,
        11,
        right_x,
        right_width,
        &format!(
            "OUT  {}",
            if outgoing.is_empty() {
                "-"
            } else {
                outgoing.as_str()
            }
        ),
        Paint::Muted,
    )?;

    paint(
        stdout,
        12,
        right_x,
        right_width,
        if app.pipeline_pane == PipelinePane::Policy {
            "[ WORKTREE POLICY PROPOSAL ]"
        } else {
            "  WORKTREE POLICY PROPOSAL  "
        },
        if app.pipeline_pane == PipelinePane::Policy {
            Paint::Focus
        } else {
            Paint::Normal
        },
    )?;
    let policy = &app.pipeline_view.policy;
    let values = [
        (
            "Attempt (worktree)",
            policy
                .attempt_budget
                .map(|value| value.to_string())
                .unwrap_or_else(|| "uncapped".into()),
        ),
        (
            "Run (worktree)",
            policy
                .run_budget
                .map(|value| value.to_string())
                .unwrap_or_else(|| "uncapped".into()),
        ),
        ("Clean (worktree)", policy.clean_rounds.to_string()),
        ("Max (worktree)", policy.max_rounds.to_string()),
        ("Gate (worktree)", policy.gate.clone()),
    ];
    for (index, (label, value)) in values.iter().enumerate() {
        setting_row(
            stdout,
            13 + index as u16,
            right_x,
            right_width,
            label,
            value,
            app.pipeline_pane == PipelinePane::Policy && app.selected_policy == index,
        )?;
    }
    Ok(())
}

struct PipelineGraph {
    levels: Vec<Vec<String>>,
    incoming: Vec<String>,
    outgoing: Vec<String>,
}

impl PipelineGraph {
    fn new(view: &PipelineView) -> Self {
        let indexes: BTreeMap<&str, usize> = view
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect();
        let mut indegree = vec![0_usize; view.nodes.len()];
        let mut adjacency = vec![Vec::new(); view.nodes.len()];
        let mut incoming = vec![Vec::new(); view.nodes.len()];
        let mut outgoing = vec![Vec::new(); view.nodes.len()];
        for edge in &view.edges {
            let (Some(&from), Some(&to)) = (
                indexes.get(edge.from_node.as_str()),
                indexes.get(edge.to_node.as_str()),
            ) else {
                continue;
            };
            indegree[to] += 1;
            adjacency[from].push(to);
            incoming[to].push(format!("{}.{}", edge.from_node, edge.from_port));
            outgoing[from].push(format!("{}.{}", edge.to_node, edge.to_port));
        }
        let mut queue: VecDeque<usize> = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| (*degree == 0).then_some(index))
            .collect();
        let mut node_levels = vec![0_usize; view.nodes.len()];
        while let Some(from) = queue.pop_front() {
            for &to in &adjacency[from] {
                node_levels[to] = node_levels[to].max(node_levels[from].saturating_add(1));
                indegree[to] -= 1;
                if indegree[to] == 0 {
                    queue.push_back(to);
                }
            }
        }
        let mut levels = vec![Vec::new(); node_levels.iter().copied().max().unwrap_or(0) + 1];
        for (node, level) in view.nodes.iter().zip(node_levels) {
            levels[level].push(format!("[{}]", node.id));
        }
        Self {
            levels,
            incoming: incoming
                .into_iter()
                .map(|items| items.join(" | "))
                .collect(),
            outgoing: outgoing
                .into_iter()
                .map(|items| items.join(" | "))
                .collect(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TopTab {
    Pipelines,
    Reviewers,
}

impl TopTab {
    fn other(self) -> Self {
        match self {
            Self::Pipelines => Self::Reviewers,
            Self::Reviewers => Self::Pipelines,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PipelinePane {
    Nodes,
    Policy,
}

impl PipelinePane {
    fn other(self) -> Self {
        match self {
            Self::Nodes => Self::Policy,
            Self::Policy => Self::Nodes,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Reviewers,
    Configuration,
    Run,
}

impl Pane {
    fn next(self) -> Self {
        match self {
            Self::Reviewers => Self::Configuration,
            Self::Configuration => Self::Run,
            Self::Run => Self::Reviewers,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Reviewers => Self::Run,
            Self::Configuration => Self::Reviewers,
            Self::Run => Self::Configuration,
        }
    }
}

enum Action {
    Continue,
    Export,
    Reload,
    Run,
    Quit,
}

#[derive(Clone, Copy)]
enum EditTarget {
    Model,
    Effort,
    ReviewerPackage,
    AddReviewer,
    AttemptBudget,
    RunBudget,
    CleanRounds,
    MaxRounds,
    FindingGate,
    Campaign,
    Authority,
    Focus,
    State,
}

struct Editor {
    target: EditTarget,
    label: &'static str,
    value: String,
}

#[derive(Clone)]
struct ReviewerConfig {
    name: String,
    path: PathBuf,
    original: String,
    original_digest: String,
    backend: String,
    model: String,
    effort: String,
    dirty: bool,
}

struct App {
    options: Options,
    repository: PathBuf,
    review_root: PathBuf,
    pipeline_path: PathBuf,
    pipeline_original: String,
    pipeline_text: String,
    pipeline_view: PipelineView,
    graph: PipelineGraph,
    pipeline_dirty: bool,
    reviewers_root: PathBuf,
    lock_path: PathBuf,
    lock_original: String,
    reviewers: Vec<ReviewerConfig>,
    tab: TopTab,
    pipeline_pane: PipelinePane,
    selected_node: usize,
    selected_policy: usize,
    selected_reviewer: usize,
    selected_config: usize,
    selected_run: usize,
    pane: Pane,
    editor: Option<Editor>,
    message: String,
    message_is_error: bool,
    confirm_quit: bool,
    confirm_reload: bool,
    confirm_remove: Option<String>,
    exported_path: Option<PathBuf>,
    export_sequence: u64,
}

impl App {
    fn load(options: Options) -> Result<Self, String> {
        let repository = fs::canonicalize(&options.repo)
            .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
        let requested_pipeline = if options.pipeline.is_absolute() {
            options.pipeline.clone()
        } else {
            repository.join(&options.pipeline)
        };
        refuse_symlink(&requested_pipeline, "pipeline")?;
        let pipeline = fs::canonicalize(&requested_pipeline).map_err(|error| {
            format!("opening pipeline {}: {error}", requested_pipeline.display())
        })?;
        if !pipeline.starts_with(&repository) {
            return Err("the TUI pipeline must be inside --repo".to_string());
        }
        let relative_pipeline = pipeline
            .strip_prefix(&repository)
            .map_err(|_| "the TUI pipeline must be inside --repo".to_string())?
            .to_str()
            .ok_or_else(|| "the pipeline path must be UTF-8".to_string())?;
        let review_root = repository.join(crate::authority::review_dir(relative_pipeline)?);
        let reviewers_root = review_root.join("reviewers");
        let lock_path = review_root.join("review.lock");
        refuse_symlink(&reviewers_root, "reviewer registry")?;
        refuse_symlink(&lock_path, "review lock")?;

        let pipeline_text = fs::read_to_string(&pipeline)
            .map_err(|error| format!("reading {}: {error}", pipeline.display()))?;
        let pipeline_view = pipeline_view(&pipeline_text)
            .map_err(|error| format!("parsing {}: {error}", pipeline.display()))?;
        let names = selected_package_names(&pipeline_view)?;

        let lock_original = fs::read_to_string(&lock_path)
            .map_err(|error| format!("reading {}: {error}", lock_path.display()))?;
        let lock = Lockfile::from_toml(&lock_original).map_err(|error| error.to_string())?;
        let mut reviewers = Vec::with_capacity(names.len());
        for name in names {
            reviewers.push(load_reviewer_config(&name, &reviewers_root, &lock)?);
        }
        let registry = Registry::new([reviewers_root.clone()]);
        validate_pipeline(&pipeline_text, &lock, &registry)?;
        let graph = PipelineGraph::new(&pipeline_view);

        Ok(Self {
            options,
            repository,
            review_root,
            pipeline_path: pipeline,
            pipeline_original: pipeline_text.clone(),
            pipeline_text,
            pipeline_view,
            graph,
            pipeline_dirty: false,
            reviewers_root,
            lock_path,
            lock_original,
            reviewers,
            tab: TopTab::Pipelines,
            pipeline_pane: PipelinePane::Nodes,
            selected_node: 0,
            selected_policy: 0,
            selected_reviewer: 0,
            selected_config: 0,
            selected_run: 0,
            pane: Pane::Reviewers,
            editor: None,
            message: "Ready; Run uses pinned authority, while edits export as a separate patch"
                .to_string(),
            message_is_error: false,
            confirm_quit: false,
            confirm_reload: false,
            confirm_remove: None,
            exported_path: None,
            export_sequence: 0,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if self.editor.is_some() {
            self.handle_editor_key(key);
            return Action::Continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => self.move_active_selection(-5),
                KeyCode::Char('d') => self.move_active_selection(5),
                KeyCode::Char('c') => return Action::Quit,
                _ => {}
            }
            return Action::Continue;
        }
        if !matches!(key.code, KeyCode::Char('q')) {
            self.confirm_quit = false;
        }
        if !matches!(key.code, KeyCode::Char('R')) {
            self.confirm_reload = false;
        }
        if !matches!(key.code, KeyCode::Char('d')) {
            self.confirm_remove = None;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('H') | KeyCode::Char('L') => {
                self.tab = self.tab.other();
            }
            KeyCode::Char('s') => return Action::Export,
            KeyCode::Char('R') => {
                if self.has_pending_configuration() && !self.confirm_reload {
                    self.confirm_reload = true;
                    self.failure("Reload discards the pending draft; press R again to confirm");
                } else {
                    return Action::Reload;
                }
            }
            KeyCode::Char('r') => {
                if self.has_pending_configuration() {
                    let message = if self.exported_path.is_some() {
                        "Run refused: the exported proposal is not authority; apply, commit, and relaunch a new campaign"
                    } else {
                        "Run refused: export, apply, review, commit, then relaunch a new campaign with that authority"
                    };
                    self.failure(message);
                } else {
                    return Action::Run;
                }
            }
            KeyCode::Char('q') => {
                if self.has_pending_configuration() && !self.confirm_quit {
                    self.confirm_quit = true;
                    let message = self
                        .exported_path
                        .as_ref()
                        .map(|path| {
                            format!(
                                "Proposal exported to {}; q again to discard the in-memory draft",
                                path.display()
                            )
                        })
                        .unwrap_or_else(|| {
                            "Unexported in-memory changes; press q again to discard them".into()
                        });
                    self.failure(message);
                } else {
                    return Action::Quit;
                }
            }
            _ => match self.tab {
                TopTab::Pipelines => self.handle_pipeline_key(key),
                TopTab::Reviewers => self.handle_reviewer_key(key),
            },
        }
        Action::Continue
    }

    fn handle_reviewer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_reviewer_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_reviewer_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.reviewer_selection_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.reviewer_selection_edge(true),
            KeyCode::Char('h') | KeyCode::Left => self.pane = self.pane.previous(),
            KeyCode::Char('l') | KeyCode::Right => self.pane = self.pane.next(),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_reviewer_selection(),
            _ => {}
        }
    }

    fn handle_pipeline_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_pipeline_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_pipeline_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.pipeline_selection_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.pipeline_selection_edge(true),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Char('l') | KeyCode::Right => {
                self.pipeline_pane = self.pipeline_pane.other();
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_pipeline_selection(),
            KeyCode::Char('a') => {
                self.begin_edit(EditTarget::AddReviewer, "reviewer package", String::new());
            }
            KeyCode::Char('d') => self.remove_selected_reviewer(),
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.success("Edit cancelled");
                return;
            }
            KeyCode::Enter => {
                if let Err(error) = self.apply_edit(editor.target, editor.value) {
                    self.failure(error);
                }
                return;
            }
            KeyCode::Backspace => {
                editor.value.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                editor.value.clear();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                while editor.value.ends_with(char::is_whitespace) {
                    editor.value.pop();
                }
                while editor
                    .value
                    .chars()
                    .last()
                    .is_some_and(|character| !character.is_whitespace())
                {
                    editor.value.pop();
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor.value.push(character);
            }
            _ => {}
        }
        self.editor = Some(editor);
    }

    fn move_active_selection(&mut self, amount: isize) {
        match self.tab {
            TopTab::Pipelines => self.move_pipeline_selection(amount),
            TopTab::Reviewers => self.move_reviewer_selection(amount),
        }
    }

    fn move_reviewer_selection(&mut self, amount: isize) {
        let (selected, count) = match self.pane {
            Pane::Reviewers => (&mut self.selected_reviewer, self.reviewers.len()),
            Pane::Configuration => (&mut self.selected_config, CONFIG_ROWS),
            Pane::Run => (&mut self.selected_run, RUN_ROWS),
        };
        *selected = selected
            .saturating_add_signed(amount)
            .min(count.saturating_sub(1));
    }

    fn reviewer_selection_edge(&mut self, end: bool) {
        let (selected, count) = match self.pane {
            Pane::Reviewers => (&mut self.selected_reviewer, self.reviewers.len()),
            Pane::Configuration => (&mut self.selected_config, CONFIG_ROWS),
            Pane::Run => (&mut self.selected_run, RUN_ROWS),
        };
        *selected = if end { count.saturating_sub(1) } else { 0 };
    }

    fn activate_reviewer_selection(&mut self) {
        match self.pane {
            Pane::Reviewers => self.pane = Pane::Configuration,
            Pane::Configuration if self.selected_config == 0 => {
                self.failure("Backend is package-owned; change it in a reviewed package update")
            }
            Pane::Configuration if self.selected_config == 1 => {
                let value = self.reviewers[self.selected_reviewer].model.clone();
                self.begin_edit(EditTarget::Model, "model", value);
            }
            Pane::Configuration => {
                let value = self.reviewers[self.selected_reviewer].effort.clone();
                self.begin_edit(EditTarget::Effort, "effort", value);
            }
            Pane::Run => match self.selected_run {
                0 => self.begin_edit(
                    EditTarget::Campaign,
                    "campaign",
                    self.options.campaign.clone().unwrap_or_default(),
                ),
                1 => self.begin_edit(
                    EditTarget::Authority,
                    "authority",
                    self.options.authority.clone().unwrap_or_default(),
                ),
                2 => self.begin_edit(
                    EditTarget::Focus,
                    "focus",
                    self.options.focus.clone().unwrap_or_default(),
                ),
                3 => self.begin_edit(
                    EditTarget::State,
                    "state",
                    self.options
                        .state
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                ),
                4 => self.options.uncommitted = !self.options.uncommitted,
                _ => self.options.restart_round = !self.options.restart_round,
            },
        }
    }

    fn move_pipeline_selection(&mut self, amount: isize) {
        let (selected, count) = match self.pipeline_pane {
            PipelinePane::Nodes => (&mut self.selected_node, self.pipeline_view.nodes.len()),
            PipelinePane::Policy => (&mut self.selected_policy, POLICY_ROWS),
        };
        *selected = selected
            .saturating_add_signed(amount)
            .min(count.saturating_sub(1));
    }

    fn pipeline_selection_edge(&mut self, end: bool) {
        let (selected, count) = match self.pipeline_pane {
            PipelinePane::Nodes => (&mut self.selected_node, self.pipeline_view.nodes.len()),
            PipelinePane::Policy => (&mut self.selected_policy, POLICY_ROWS),
        };
        *selected = if end { count.saturating_sub(1) } else { 0 };
    }

    fn activate_pipeline_selection(&mut self) {
        match self.pipeline_pane {
            PipelinePane::Nodes => {
                let node = &self.pipeline_view.nodes[self.selected_node];
                let Some(package) = &node.package else {
                    self.failure(
                        "Only package-backed reviewer nodes are editable; typed infrastructure nodes remain TOML-owned",
                    );
                    return;
                };
                self.begin_edit(
                    EditTarget::ReviewerPackage,
                    "reviewer package",
                    package.clone(),
                );
            }
            PipelinePane::Policy => {
                let policy = &self.pipeline_view.policy;
                let (target, label, value) = match self.selected_policy {
                    0 => (
                        EditTarget::AttemptBudget,
                        "attempt tokens",
                        policy
                            .attempt_budget
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    ),
                    1 => (
                        EditTarget::RunBudget,
                        "run tokens",
                        policy
                            .run_budget
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    ),
                    2 => (
                        EditTarget::CleanRounds,
                        "clean rounds",
                        policy.clean_rounds.to_string(),
                    ),
                    3 => (
                        EditTarget::MaxRounds,
                        "max rounds",
                        policy.max_rounds.to_string(),
                    ),
                    _ => (EditTarget::FindingGate, "finding gate", policy.gate.clone()),
                };
                self.begin_edit(target, label, value);
            }
        }
    }

    fn remove_selected_reviewer(&mut self) {
        let node = &self.pipeline_view.nodes[self.selected_node];
        if node.kind != "reviewer" {
            self.failure("Only reviewer nodes have canonical removable wiring");
            return;
        }
        let id = node.id.clone();
        if self.confirm_remove.as_deref() != Some(&id) {
            self.confirm_remove = Some(id.clone());
            self.failure(format!(
                "Press d again to remove reviewer node `{id}` and its edges"
            ));
            return;
        }
        self.confirm_remove = None;
        match remove_reviewer(&self.pipeline_text, &id)
            .and_then(|candidate| self.accept_pipeline(candidate))
        {
            Ok(()) => self.success(format!("Removed reviewer `{id}` in memory; s exports it")),
            Err(error) => self.failure(error),
        }
    }

    fn begin_edit(&mut self, target: EditTarget, label: &'static str, value: String) {
        self.editor = Some(Editor {
            target,
            label,
            value,
        });
    }

    fn apply_edit(&mut self, target: EditTarget, value: String) -> Result<(), String> {
        let value = value.trim().to_string();
        let mut success =
            "Value updated in memory; s exports a patch, r never uses uncommitted config"
                .to_string();
        match target {
            EditTarget::Model => {
                let reviewer = &mut self.reviewers[self.selected_reviewer];
                let rendered =
                    update_reviewer_runner_settings(&reviewer.original, &value, &reviewer.effort)
                        .map_err(|error| error.to_string())?;
                reviewer.model = value;
                reviewer.dirty = rendered != reviewer.original;
                self.exported_path = None;
            }
            EditTarget::Effort => {
                let reviewer = &mut self.reviewers[self.selected_reviewer];
                let rendered =
                    update_reviewer_runner_settings(&reviewer.original, &reviewer.model, &value)
                        .map_err(|error| error.to_string())?;
                reviewer.effort = value;
                reviewer.dirty = rendered != reviewer.original;
                self.exported_path = None;
            }
            EditTarget::ReviewerPackage => {
                let node_id = self.pipeline_view.nodes[self.selected_node].id.clone();
                let candidate = rebind_reviewer(&self.pipeline_text, &node_id, &value)?;
                self.accept_pipeline(candidate)?;
            }
            EditTarget::AddReviewer => {
                let template = self
                    .pipeline_view
                    .nodes
                    .iter()
                    .find(|node| node.kind == "reviewer" && node.package.is_some())
                    .map(|node| node.id.clone())
                    .ok_or("membership editing requires a package-backed reviewer template")?;
                let candidate = add_reviewer(&self.pipeline_text, &value)?;
                self.accept_pipeline(candidate)?;
                self.selected_node = self.pipeline_view.nodes.len().saturating_sub(1);
                success = format!(
                    "Added `{value}` by cloning reviewer `{template}` wiring; s exports the patch"
                );
            }
            EditTarget::AttemptBudget => {
                self.edit_pipeline_setting(PipelineSetting::AttemptBudget, &value)?;
            }
            EditTarget::RunBudget => {
                self.edit_pipeline_setting(PipelineSetting::RunBudget, &value)?;
            }
            EditTarget::CleanRounds => {
                self.edit_pipeline_setting(PipelineSetting::CleanRounds, &value)?;
            }
            EditTarget::MaxRounds => {
                self.edit_pipeline_setting(PipelineSetting::MaxRounds, &value)?;
            }
            EditTarget::FindingGate => {
                self.edit_pipeline_setting(PipelineSetting::Gate, &value)?;
            }
            EditTarget::Campaign => self.options.campaign = nonempty(value),
            EditTarget::Authority => self.options.authority = nonempty(value),
            EditTarget::Focus => self.options.focus = nonempty(value),
            EditTarget::State => self.options.state = nonempty(value).map(PathBuf::from),
        }
        self.success(success);
        Ok(())
    }

    fn edit_pipeline_setting(
        &mut self,
        setting: PipelineSetting,
        value: &str,
    ) -> Result<(), String> {
        let candidate = update_pipeline_setting(&self.pipeline_text, setting, value)?;
        self.accept_pipeline(candidate)
    }

    fn accept_pipeline(&mut self, candidate: String) -> Result<(), String> {
        let lock = Lockfile::from_toml(&self.lock_original).map_err(|error| error.to_string())?;
        let registry = Registry::new([self.reviewers_root.clone()]);
        validate_pipeline_structure(&candidate)?;
        let view = pipeline_view(&candidate)?;
        let names = selected_package_names(&view)?;
        for reviewer in &self.reviewers {
            if reviewer.dirty && !names.contains(&reviewer.name) {
                return Err(format!(
                    "reviewer `{}` has an unexported draft; revert it before removing its pipeline binding",
                    reviewer.name
                ));
            }
        }
        let mut reviewers = Vec::with_capacity(names.len());
        for name in names {
            if let Some(existing) = self.reviewers.iter().find(|reviewer| reviewer.name == name) {
                reviewers.push(existing.clone());
            } else {
                lock.resolve_for_subject(&name, &registry, view.subject)
                    .map_err(|error| error.to_string())?;
                reviewers.push(load_reviewer_config(&name, &self.reviewers_root, &lock)?);
            }
        }
        let graph = PipelineGraph::new(&view);
        self.pipeline_text = candidate;
        self.pipeline_view = view;
        self.graph = graph;
        self.reviewers = reviewers;
        self.pipeline_dirty = self.pipeline_text != self.pipeline_original;
        self.selected_node = self
            .selected_node
            .min(self.pipeline_view.nodes.len().saturating_sub(1));
        self.selected_reviewer = self
            .selected_reviewer
            .min(self.reviewers.len().saturating_sub(1));
        self.exported_path = None;
        Ok(())
    }

    fn has_pending_configuration(&self) -> bool {
        self.pipeline_dirty || self.reviewers.iter().any(|reviewer| reviewer.dirty)
    }

    fn export_configuration(&mut self) -> Result<(usize, bool, PathBuf), String> {
        let current_pipeline = fs::read_to_string(&self.pipeline_path)
            .map_err(|error| format!("reading {}: {error}", self.pipeline_path.display()))?;
        if current_pipeline != self.pipeline_original {
            return Err(format!(
                "{} changed while the TUI was open; restart before exporting",
                self.pipeline_path.display()
            ));
        }
        let current_lock = fs::read_to_string(&self.lock_path)
            .map_err(|error| format!("reading {}: {error}", self.lock_path.display()))?;
        if current_lock != self.lock_original {
            return Err(format!(
                "{} changed while the TUI was open; restart before exporting",
                self.lock_path.display()
            ));
        }
        let dirty: Vec<&ReviewerConfig> = self
            .reviewers
            .iter()
            .filter(|reviewer| reviewer.dirty)
            .collect();
        if dirty.is_empty() && !self.pipeline_dirty {
            return Err("No pipeline or reviewer configuration changes to export".to_string());
        }

        let registry = Registry::new([self.reviewers_root.clone()]);
        let mut lock = Lockfile::from_toml(&current_lock).map_err(|error| error.to_string())?;
        validate_pipeline(&self.pipeline_text, &lock, &registry)?;
        let mut files = Vec::with_capacity(dirty.len() + 2);
        if self.pipeline_dirty {
            files.push((
                self.pipeline_path.clone(),
                self.pipeline_original.clone(),
                self.pipeline_text.clone(),
            ));
        }
        for reviewer in &dirty {
            let current = fs::read_to_string(&reviewer.path)
                .map_err(|error| format!("reading {}: {error}", reviewer.path.display()))?;
            if current != reviewer.original {
                return Err(format!(
                    "{} changed while the TUI was open; restart before exporting",
                    reviewer.path.display()
                ));
            }
            let rendered = update_reviewer_runner_settings(
                &reviewer.original,
                &reviewer.model,
                &reviewer.effort,
            )
            .map_err(|error| error.to_string())?;
            let pin = Lockfile::pin_with_replacement(
                &reviewer.name,
                &registry,
                &reviewer.original_digest,
                "reviewer.toml",
                rendered.as_bytes().to_vec(),
            )
            .map_err(|error| error.to_string())?;
            lock.reviewers.insert(reviewer.name.clone(), pin);
            files.push((reviewer.path.clone(), reviewer.original.clone(), rendered));
        }
        if !dirty.is_empty() {
            let rendered_lock = lock.to_toml();
            files.push((
                self.lock_path.clone(),
                self.lock_original.clone(),
                rendered_lock,
            ));
        }

        let mut patch = String::new();
        for (path, before, after) in files {
            let relative = path.strip_prefix(&self.repository).map_err(|_| {
                format!("configuration path {} left the repository", path.display())
            })?;
            append_full_file_patch(&mut patch, relative, &before, &after)?;
        }
        let count = dirty.len();
        let directory = prepare_proposal_directory(
            &self.repository,
            &self.review_root,
            &self.options.resolved_state_dir()?,
        )?;
        let path = publish_patch(&directory, patch.as_bytes(), self.export_sequence)?;
        self.export_sequence = self.export_sequence.wrapping_add(1);
        self.exported_path = Some(path.clone());
        Ok((count, self.pipeline_dirty, path))
    }

    fn success(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.message_is_error = false;
    }

    fn failure(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.message_is_error = true;
    }
}

fn append_full_file_patch(
    patch: &mut String,
    path: &Path,
    before: &str,
    after: &str,
) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("patch path {} is not UTF-8", path.display()))?;
    if path.is_empty()
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "patch path `{path}` contains characters the proposal exporter does not encode"
        ));
    }
    let before_lines = patch_lines(before);
    let after_lines = patch_lines(after);
    let before_range = patch_range(before_lines.len());
    let after_range = patch_range(after_lines.len());
    patch.push_str(&format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{before_range} +{after_range} @@\n"
    ));
    append_patch_side(patch, '-', before_lines);
    append_patch_side(patch, '+', after_lines);
    Ok(())
}

fn patch_lines(text: &str) -> Vec<(&str, bool)> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n')
        .map(|line| {
            line.strip_suffix('\n')
                .map(|content| (content, true))
                .unwrap_or((line, false))
        })
        .collect()
}

fn patch_range(lines: usize) -> String {
    if lines == 0 {
        "0,0".to_string()
    } else {
        format!("1,{lines}")
    }
}

fn append_patch_side(patch: &mut String, prefix: char, lines: Vec<(&str, bool)>) {
    for (line, terminated) in lines {
        patch.push(prefix);
        patch.push_str(line);
        patch.push('\n');
        if !terminated {
            patch.push_str("\\ No newline at end of file\n");
        }
    }
}

fn prepare_proposal_directory(
    repository: &Path,
    review_root: &Path,
    state: &Path,
) -> Result<PathBuf, String> {
    let directory = proposal_directory(repository, review_root, state)?;
    create_directories_durable(&directory)?;
    let canonical = fs::canonicalize(&directory)
        .map_err(|error| format!("opening {}: {error}", directory.display()))?;
    if canonical.starts_with(repository) {
        let allowed = crate::resolve_filesystem_path(&review_root.join("runs"))?;
        if !canonical.starts_with(allowed) {
            return Err(
                "configuration proposals may not overlap captured repository content".into(),
            );
        }
    }
    Ok(canonical)
}

fn create_directories_durable(path: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    let mut missing = Vec::new();
    while !current.exists() {
        missing.push(current.clone());
        if !current.pop() {
            return Err(format!("path {} has no existing ancestor", path.display()));
        }
    }
    for directory in missing.into_iter().rev() {
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("creating {}: {error}", directory.display())),
        }
        sync_directory(&directory)?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn proposal_directory(
    repository: &Path,
    review_root: &Path,
    state: &Path,
) -> Result<PathBuf, String> {
    if !state.is_absolute() {
        return Err(format!(
            "resolved state {} is not absolute",
            state.display()
        ));
    }
    let directory = state.join("config-proposals");
    let allowed = crate::resolve_filesystem_path(&review_root.join("runs"))?;
    if directory.starts_with(repository) && !directory.starts_with(allowed) {
        return Err(format!(
            "proposal directory {} overlaps captured repository content; use state outside --repo or below {}",
            directory.display(),
            review_root.join("runs").display()
        ));
    }
    Ok(directory)
}

fn publish_patch(directory: &Path, bytes: &[u8], sequence: u64) -> Result<PathBuf, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    for collision in 0..100_u32 {
        let stem = format!(
            "reviewers-{timestamp}-{}-{sequence}-{collision}",
            std::process::id()
        );
        let path = directory.join(format!("{stem}.patch"));
        let temporary = directory.join(format!(".{stem}.tmp"));
        let mut output = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("creating {}: {error}", temporary.display())),
        };
        let written = output.write_all(bytes).and_then(|()| output.sync_all());
        drop(output);
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(format!("writing {}: {error}", temporary.display()));
        }
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
                continue;
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(format!("publishing {}: {error}", path.display()));
            }
        }
        fs::remove_file(&temporary)
            .map_err(|error| format!("removing {}: {error}", temporary.display()))?;
        sync_directory(directory)?;
        return Ok(path);
    }
    Err("could not allocate a unique configuration proposal filename".into())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), String> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("syncing {}: {error}", directory.display()))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), String> {
    Ok(())
}

fn refuse_symlink(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("opening {label} {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("{label} {} is a symlink", path.display()));
    }
    Ok(())
}

fn safe_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        })
}

fn selected_package_names(view: &PipelineView) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for node in &view.nodes {
        if node.kind != "reviewer" {
            continue;
        }
        let Some(name) = &node.package else {
            continue;
        };
        if !safe_package_name(name) {
            return Err(format!("pipeline reviewer package name `{name}` is unsafe"));
        }
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    if names.is_empty() {
        return Err("pipeline selects no packaged reviewers to configure".to_string());
    }
    Ok(names)
}

fn load_reviewer_config(
    name: &str,
    reviewers_root: &Path,
    lock: &Lockfile,
) -> Result<ReviewerConfig, String> {
    let package = reviewers_root.join(name);
    refuse_symlink(&package, "reviewer package")?;
    let path = package.join("reviewer.toml");
    refuse_symlink(&path, "reviewer manifest")?;
    let original = fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let manifest: PackageManifest = toml::from_str(&original)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    let settings = reviewer_runner_settings(&original)
        .map_err(|error| format!("reviewer `{name}` is not TUI-configurable: {error}"))?;
    let pin = lock
        .reviewers
        .get(name)
        .ok_or_else(|| format!("reviewer `{name}` is absent from review.lock"))?;
    if pin.version != manifest.version {
        return Err(format!(
            "reviewer `{name}` does not match review.lock; repair authority before opening the TUI"
        ));
    }
    Ok(ReviewerConfig {
        name: name.to_string(),
        path,
        original,
        original_digest: pin.digest.clone(),
        backend: settings.backend.to_string(),
        model: settings.model,
        effort: settings.effort,
        dirty: false,
    })
}

fn optional(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "(none)".to_string())
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn setting_row(
    stdout: &mut Stdout,
    y: u16,
    x: u16,
    width: u16,
    label: &str,
    value: &str,
    selected: bool,
) -> Result<(), String> {
    paint(
        stdout,
        y,
        x,
        width,
        &format!("{} {label:<17} {value}", if selected { ">" } else { " " }),
        if selected {
            Paint::Selected
        } else {
            Paint::Normal
        },
    )
}

#[derive(Clone, Copy)]
enum Paint {
    Normal,
    Muted,
    Header,
    Focus,
    Selected,
    Success,
    Error,
}

fn paint(
    stdout: &mut Stdout,
    y: u16,
    x: u16,
    width: u16,
    text: &str,
    paint: Paint,
) -> Result<(), String> {
    let width = usize::from(width);
    let text_width = text.chars().count();
    let mut visible = if text_width > width {
        if width <= 3 {
            ".".repeat(width)
        } else {
            text.chars().take(width - 3).collect::<String>() + "..."
        }
    } else {
        text.to_string()
    };
    visible.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(visible.chars().count()),
    ));
    let (foreground, background, bold) = match paint {
        Paint::Normal => (Color::White, Color::Reset, false),
        Paint::Muted => (Color::DarkGrey, Color::Reset, false),
        Paint::Header => (Color::Black, Color::Cyan, true),
        Paint::Focus => (Color::Yellow, Color::Reset, true),
        Paint::Selected => (Color::Black, Color::DarkYellow, true),
        Paint::Success => (Color::Green, Color::Reset, false),
        Paint::Error => (Color::Red, Color::Reset, true),
    };
    queue!(
        stdout,
        MoveTo(x, y),
        SetForegroundColor(foreground),
        SetBackgroundColor(background),
        SetAttribute(if bold {
            Attribute::Bold
        } else {
            Attribute::NormalIntensity
        }),
        Print(visible),
        SetAttribute(Attribute::Reset),
        ResetColor
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{append_full_file_patch, proposal_directory};

    #[test]
    fn configuration_proposal_is_a_full_file_patch() {
        let mut patch = String::new();
        append_full_file_patch(
            &mut patch,
            std::path::Path::new(".review/review.lock"),
            "version = 1\n",
            "version = 2\n",
        )
        .unwrap();
        assert!(patch.contains("diff --git a/.review/review.lock b/.review/review.lock"));
        assert!(patch.contains("-version = 1"));
        assert!(patch.contains("+version = 2"));
    }

    #[test]
    fn configuration_patch_preserves_missing_terminal_newlines_and_empty_files() {
        let mut patch = String::new();
        append_full_file_patch(
            &mut patch,
            std::path::Path::new(".review/review.lock"),
            "version = 1",
            "version = 2",
        )
        .unwrap();
        assert_eq!(patch.matches("\\ No newline at end of file").count(), 2);

        let mut empty = String::new();
        append_full_file_patch(
            &mut empty,
            std::path::Path::new(".review/review.lock"),
            "",
            "version = 1\n",
        )
        .unwrap();
        assert!(empty.contains("@@ -0,0 +1,1 @@"));
    }

    #[test]
    fn proposal_state_cannot_alias_reviewer_packages() {
        let repository = std::path::Path::new("/repo");
        let review_root = repository.join(".review");
        assert!(
            proposal_directory(
                repository,
                &review_root,
                std::path::Path::new("/repo/.review/reviewers/architecture"),
            )
            .is_err()
        );
        assert!(
            proposal_directory(
                repository,
                &review_root,
                std::path::Path::new("/repo/.review/runs/campaign"),
            )
            .is_ok()
        );
    }

    #[test]
    fn proposal_patch_refuses_unencoded_paths() {
        let mut patch = String::new();
        assert!(
            append_full_file_patch(
                &mut patch,
                std::path::Path::new(".review/pipelines/bad name.toml"),
                "before\n",
                "after\n",
            )
            .is_err()
        );
        assert!(patch.is_empty());
    }
}
