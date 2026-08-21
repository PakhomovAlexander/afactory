//! Interactive configuration proposals and launch surface for `reviewctl`.
//!
//! The TUI never turns working-tree bytes into execution authority. Reviewer edits stay in
//! memory and `s` exports an explicit patch under review state. The operator applies, reviews,
//! and commits that patch, then starts a new campaign whose `--authority` names that commit.
//! `r` always delegates to the ordinary pinned-authority run path.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
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

use crate::{Options, providers};

const CONFIG_ROWS: usize = 3;
const RUN_ROWS: usize = 6;
const POLICY_ROWS: usize = 5;
const MAX_DIAGRAM_NODES: usize = 256;
const MAX_DIAGRAM_LINKS: usize = 2_048;
const MAX_DIAGRAM_CELLS: usize = 1_000_000;
const MAX_DIAGRAM_ROUTE_CELLS: usize = 4_000_000;

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
        if app.finish_provider_refresh() {
            terminal.draw(app)?;
        }
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
            Action::RefreshProviders => {
                app.start_provider_refresh();
            }
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

        draw_top_tabs(&mut self.stdout, app.tab, width)?;

        let footer = height - 3;
        if app.tab == TopTab::Providers {
            draw_provider_body(&mut self.stdout, app, width, footer)?;
        } else if app.tab == TopTab::Reviewers {
            draw_reviewer_body(&mut self.stdout, app, width, footer)?;
        } else {
            draw_pipeline_body(&mut self.stdout, app, width, footer)?;
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
                Paint::Mutation,
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
            draw_key_legend(&mut self.stdout, footer + 2, width, app.tab)?;
        }
        self.stdout.flush().map_err(|error| error.to_string())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

fn draw_reviewer_body(
    stdout: &mut Stdout,
    app: &App,
    width: u16,
    footer: u16,
) -> Result<(), String> {
    let content_x = 1;
    let content_width = width - 2;
    paint_spans(
        stdout,
        3,
        content_x,
        content_width,
        &[
            (
                "[REVIEWERS]",
                if app.pane == Pane::Reviewers {
                    Paint::Navigation
                } else {
                    Paint::Normal
                },
            ),
            ("  l>  ", Paint::Muted),
            (
                "[WORKTREE CONFIG PROPOSAL]",
                if app.pane == Pane::Configuration {
                    Paint::Navigation
                } else {
                    Paint::Normal
                },
            ),
            ("  l>  ", Paint::Muted),
            (
                "[PINNED-AUTHORITY RUN]",
                if app.pane == Pane::Run {
                    Paint::Navigation
                } else {
                    Paint::Normal
                },
            ),
        ],
    )?;

    let reviewer = &app.reviewers[app.selected_reviewer];
    match app.pane {
        Pane::Reviewers => {
            paint(
                stdout,
                4,
                content_x,
                content_width,
                "Select a reviewer; l opens its worktree configuration proposal",
                Paint::Muted,
            )?;
            let list_top = 6;
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
                    stdout,
                    list_top + row as u16,
                    content_x,
                    content_width,
                    &format!(
                        "{} {:<20} {:<8} {:<24} {}{}",
                        if selected { ">" } else { " " },
                        reviewer.name,
                        reviewer.backend,
                        reviewer.model,
                        reviewer.effort,
                        if reviewer.dirty { " *" } else { "" }
                    ),
                    if selected {
                        Paint::Navigation
                    } else if reviewer.dirty {
                        Paint::Mutation
                    } else {
                        Paint::Normal
                    },
                )?;
            }
        }
        Pane::Configuration => {
            paint(
                stdout,
                5,
                content_x,
                content_width,
                &format!(
                    "Reviewer {}{}",
                    reviewer.name,
                    if reviewer.dirty { " *" } else { "" }
                ),
                if reviewer.dirty {
                    Paint::Mutation
                } else {
                    Paint::Normal
                },
            )?;
            paint(
                stdout,
                6,
                content_x,
                content_width,
                "Values are worktree proposal inputs only; Run never executes them",
                Paint::Muted,
            )?;
            let config_values = [
                ("Backend (fixed)", reviewer.backend.as_str()),
                ("Model (worktree)", reviewer.model.as_str()),
                ("Effort (worktree)", reviewer.effort.as_str()),
            ];
            for (index, (label, value)) in config_values.iter().enumerate() {
                setting_row(
                    stdout,
                    8 + index as u16,
                    content_x,
                    content_width,
                    label,
                    value,
                    app.selected_config == index,
                )?;
            }
        }
        Pane::Run => {
            paint(
                stdout,
                5,
                content_x,
                content_width,
                "Run the selected campaign through committed, pinned configuration",
                Paint::Normal,
            )?;
            paint(
                stdout,
                6,
                content_x,
                content_width,
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
                    stdout,
                    8 + index as u16,
                    content_x,
                    content_width,
                    label,
                    value,
                    app.selected_run == index,
                )?;
            }
        }
    }
    Ok(())
}

fn draw_provider_body(
    stdout: &mut Stdout,
    app: &App,
    width: u16,
    footer: u16,
) -> Result<(), String> {
    let content_x = 1;
    let content_width = width - 2;
    paint(
        stdout,
        3,
        content_x,
        content_width,
        "PROVIDERS | IDs label local auth contexts; accounts are not verified; Campaign authority is untouched",
        Paint::Normal,
    )?;
    if let Some(warning) = &app.providers.warning {
        paint(stdout, 4, content_x, content_width, warning, Paint::Error)?;
    } else {
        let registry = app
            .providers
            .registry
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(HOME unavailable)".to_string());
        paint(
            stdout,
            4,
            content_x,
            content_width,
            &format!("Registry {registry}"),
            Paint::Muted,
        )?;
    }
    paint(
        stdout,
        5,
        content_x,
        content_width,
        "  ID                     KIND      STATUS              AUTH TYPE",
        Paint::Muted,
    )?;
    let detail_top = footer - 6;
    let list_top = 6;
    let capacity = usize::from(detail_top.saturating_sub(list_top));
    if app.providers.providers.is_empty() {
        paint(
            stdout,
            list_top,
            content_x,
            content_width,
            "No supported provider CLI is installed and no provider registry entries were loaded",
            Paint::Error,
        )?;
        return Ok(());
    }
    let start = app
        .selected_provider
        .saturating_sub(capacity.saturating_sub(1));
    for (row, (index, provider)) in app
        .providers
        .providers
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .enumerate()
    {
        let selected = index == app.selected_provider;
        paint(
            stdout,
            list_top + row as u16,
            content_x,
            content_width,
            &format!(
                "{} {:<22} {:<9} {:<19} {}",
                if selected { ">" } else { " " },
                provider.id,
                provider.kind,
                provider.status,
                provider.auth_type
            ),
            if selected {
                Paint::Navigation
            } else {
                Paint::Normal
            },
        )?;
    }
    let provider = &app.providers.providers[app.selected_provider];
    paint(
        stdout,
        detail_top,
        content_x,
        content_width,
        &format!("SELECTED {} [{}]", provider.id, provider.kind),
        Paint::Navigation,
    )?;
    paint(
        stdout,
        detail_top + 1,
        content_x,
        content_width,
        &format!("COMMAND      {}", provider.command),
        Paint::Muted,
    )?;
    paint(
        stdout,
        detail_top + 2,
        content_x,
        content_width,
        &format!("AUTH CONTEXT {}", provider.auth_context),
        Paint::Muted,
    )?;
    paint(
        stdout,
        detail_top + 3,
        content_x,
        content_width,
        &format!("SOURCE       {}", provider.source),
        Paint::Muted,
    )?;
    paint(
        stdout,
        detail_top + 4,
        content_x,
        content_width,
        &format!(
            "DETAIL       {}",
            if provider.detail.is_empty() {
                "-"
            } else {
                &provider.detail
            }
        ),
        Paint::Muted,
    )?;
    Ok(())
}

fn draw_pipeline_body(
    stdout: &mut Stdout,
    app: &App,
    width: u16,
    footer: u16,
) -> Result<(), String> {
    let content_x = 1;
    let content_width = width - 2;
    if app.tab == TopTab::PipelineGraph {
        paint(
            stdout,
            3,
            content_x,
            content_width,
            &format!(
                " WORKTREE ASCII DAG{}",
                if app.pipeline_dirty { " *" } else { "" }
            ),
            if app.pipeline_dirty {
                Paint::Mutation
            } else {
                Paint::Normal
            },
        )?;
        let detail_top = footer - 4;
        let graph_top = 5;
        let graph_height = usize::from(detail_top.saturating_sub(graph_top));
        match &app.diagram {
            Ok(diagram) => {
                let viewport =
                    diagram.viewport(app.selected_node, usize::from(content_width), graph_height);
                paint(
                    stdout,
                    4,
                    content_x,
                    content_width,
                    &format!(
                        "cyan * selected | --> data | ..> gate control | viewport follows selection{}",
                        viewport.clipping_label()
                    ),
                    Paint::Muted,
                )?;
                for (row, line) in viewport.lines.iter().enumerate() {
                    paint_graph_row(
                        stdout,
                        graph_top + row as u16,
                        content_x,
                        content_width,
                        line,
                        viewport.selection.filter(|selection| selection.0 == row),
                    )?;
                }
            }
            Err(error) => paint(
                stdout,
                4,
                content_x,
                content_width,
                &format!("ASCII graph unavailable: {error}"),
                Paint::Error,
            )?,
        }

        let node = &app.pipeline_view.nodes[app.selected_node];
        let incoming = &app.graph.incoming[app.selected_node];
        let outgoing = &app.graph.outgoing[app.selected_node];
        paint(
            stdout,
            detail_top,
            content_x,
            content_width,
            &format!(
                "NODE {} [{}] package={}",
                node.id,
                node.kind,
                node.package.as_deref().unwrap_or("-")
            ),
            Paint::Navigation,
        )?;
        paint(
            stdout,
            detail_top + 1,
            content_x,
            content_width,
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
            detail_top + 2,
            content_x,
            content_width,
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
        return Ok(());
    }

    paint(
        stdout,
        3,
        content_x,
        content_width,
        &format!(
            " WORKTREE POLICY PROPOSAL{}",
            if app.pipeline_dirty { " *" } else { "" }
        ),
        if app.pipeline_dirty {
            Paint::Mutation
        } else {
            Paint::Normal
        },
    )?;
    paint(
        stdout,
        4,
        content_x,
        content_width,
        "Values below are worktree proposal inputs only; Run never executes them",
        Paint::Muted,
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
            6 + index as u16,
            content_x,
            content_width,
            label,
            value,
            app.selected_policy == index,
        )?;
    }
    Ok(())
}

struct AsciiGraph {
    cells: Vec<Vec<char>>,
    positions: Vec<(usize, usize)>,
    label_widths: Vec<usize>,
}

struct GraphViewport {
    lines: Vec<String>,
    selection: Option<(usize, usize, usize)>,
    clipped_left: bool,
    clipped_right: bool,
    clipped_up: bool,
    clipped_down: bool,
}

impl GraphViewport {
    fn clipping_label(&self) -> String {
        let mut directions = Vec::new();
        if self.clipped_left {
            directions.push("left");
        }
        if self.clipped_right {
            directions.push("right");
        }
        if self.clipped_up {
            directions.push("up");
        }
        if self.clipped_down {
            directions.push("down");
        }
        if directions.is_empty() {
            String::new()
        } else {
            format!(" | clipped: {}", directions.join(" "))
        }
    }
}

impl AsciiGraph {
    fn new(view: &PipelineView, graph: &PipelineGraph) -> Result<Self, String> {
        if view.nodes.is_empty() || graph.levels.is_empty() {
            return Ok(Self {
                cells: vec![Vec::new()],
                positions: Vec::new(),
                label_widths: Vec::new(),
            });
        }
        if view.nodes.len() > MAX_DIAGRAM_NODES {
            return Err(format!(
                "{} nodes exceed the TUI limit of {MAX_DIAGRAM_NODES}; edit this pipeline in TOML",
                view.nodes.len()
            ));
        }
        if graph.links.len() > MAX_DIAGRAM_LINKS {
            return Err(format!(
                "{} links exceed the TUI limit of {MAX_DIAGRAM_LINKS}; edit this pipeline in TOML",
                graph.links.len()
            ));
        }

        let labels: Vec<String> = view
            .nodes
            .iter()
            .map(|node| format!(" [{}]", display_ascii(&node.id)))
            .collect();
        let mut node_levels = vec![0_usize; view.nodes.len()];
        for (level, nodes) in graph.levels.iter().enumerate() {
            for &node in nodes {
                node_levels[node] = level;
            }
        }
        let column_widths: Vec<usize> = graph
            .levels
            .iter()
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|&node| labels[node].len())
                    .max()
                    .unwrap_or(1)
            })
            .collect();
        let mut long_links_by_level = vec![0_usize; graph.levels.len()];
        for link in &graph.links {
            if node_levels[link.to] > node_levels[link.from].saturating_add(1) {
                long_links_by_level[node_levels[link.from]] += 1;
            }
        }
        let mut column_x = vec![0_usize; graph.levels.len()];
        for level in 1..graph.levels.len() {
            let channel_width = graph.levels[level - 1]
                .len()
                .checked_add(long_links_by_level[level - 1])
                .and_then(|width| width.checked_add(3))
                .ok_or("ASCII graph width overflow")?
                .max(5);
            column_x[level] = column_x[level - 1]
                .checked_add(column_widths[level - 1])
                .and_then(|x| x.checked_add(channel_width))
                .ok_or("ASCII graph width overflow")?;
        }

        let node_rows = graph
            .levels
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .checked_mul(2)
            .and_then(|rows| rows.checked_sub(1))
            .ok_or("ASCII graph height overflow")?
            .max(1);
        let mut positions = vec![(0_usize, 0_usize); view.nodes.len()];
        for (level, nodes) in graph.levels.iter().enumerate() {
            for (row, &node) in nodes.iter().enumerate() {
                let y = if nodes.len() == 1 {
                    (node_rows - 1) / 2
                } else {
                    row * (node_rows - 1) / (nodes.len() - 1)
                };
                positions[node] = (column_x[level], y);
            }
        }
        let canvas_width = column_x
            .last()
            .zip(column_widths.last())
            .and_then(|(x, width)| x.checked_add(*width))
            .ok_or("ASCII graph width overflow")?
            .max(1);
        let long_links: usize = long_links_by_level.iter().sum();
        let canvas_height = if long_links == 0 {
            node_rows
        } else {
            node_rows
                .checked_add(long_links)
                .and_then(|height| height.checked_add(1))
                .ok_or("ASCII graph height overflow")?
        };
        let cells = canvas_width
            .checked_mul(canvas_height)
            .ok_or("ASCII graph area overflow")?;
        if cells > MAX_DIAGRAM_CELLS {
            return Err(format!(
                "diagram needs {cells} cells, above the TUI limit of {MAX_DIAGRAM_CELLS}; edit this pipeline in TOML"
            ));
        }
        let mut cells = vec![vec![' '; canvas_width]; canvas_height];
        let mut arrows = Vec::new();
        let mut long_link = 0;
        let mut long_rank_by_level = vec![0_usize; graph.levels.len()];
        let mut route_cells = 0_usize;

        for link in &graph.links {
            let from_level = node_levels[link.from];
            let to_level = node_levels[link.to];
            let (from_x, from_y) = positions[link.from];
            let (to_x, to_y) = positions[link.to];
            let from_end = from_x + labels[link.from].len();
            let arrow_x = to_x.saturating_sub(1);
            let horizontal = if link.control { '.' } else { '-' };
            let vertical = if link.control { ':' } else { '|' };
            if to_level == from_level.saturating_add(1) {
                let source_rank = graph.levels[from_level]
                    .iter()
                    .position(|node| *node == link.from)
                    .unwrap_or(0);
                let channel = column_x[from_level] + column_widths[from_level] + 1 + source_rank;
                route_cells = add_route_work(
                    route_cells,
                    from_end.abs_diff(channel) + from_y.abs_diff(to_y) + channel.abs_diff(arrow_x),
                )?;
                draw_horizontal(&mut cells, from_end, channel, from_y, horizontal);
                if from_y != to_y {
                    draw_vertical(&mut cells, channel, from_y, to_y, vertical);
                }
                draw_horizontal(
                    &mut cells,
                    channel,
                    arrow_x.saturating_sub(1),
                    to_y,
                    horizontal,
                );
            } else {
                let lane_y = node_rows + 1 + long_link;
                long_link += 1;
                let rank = long_rank_by_level[from_level];
                long_rank_by_level[from_level] += 1;
                let left_channel = column_x[from_level]
                    + column_widths[from_level]
                    + 1
                    + graph.levels[from_level].len()
                    + rank;
                let right_channel = arrow_x.saturating_sub(1);
                route_cells = add_route_work(
                    route_cells,
                    from_end.abs_diff(left_channel)
                        + from_y.abs_diff(lane_y)
                        + left_channel.abs_diff(right_channel)
                        + lane_y.abs_diff(to_y),
                )?;
                draw_horizontal(&mut cells, from_end, left_channel, from_y, horizontal);
                draw_vertical(&mut cells, left_channel, from_y, lane_y, vertical);
                draw_horizontal(&mut cells, left_channel, right_channel, lane_y, horizontal);
                draw_vertical(&mut cells, right_channel, lane_y, to_y, vertical);
            }
            arrows.push((arrow_x, to_y));
        }

        for (x, y) in arrows {
            if let Some(cell) = cells.get_mut(y).and_then(|row| row.get_mut(x)) {
                *cell = '>';
            }
        }
        for (index, label) in labels.iter().enumerate() {
            let (x, y) = positions[index];
            for (offset, character) in label.chars().enumerate() {
                if let Some(cell) = cells.get_mut(y).and_then(|row| row.get_mut(x + offset)) {
                    *cell = character;
                }
            }
        }
        let label_widths = labels.iter().map(String::len).collect();
        Ok(Self {
            cells,
            positions,
            label_widths,
        })
    }

    fn viewport(&self, selected: usize, width: usize, height: usize) -> GraphViewport {
        if width == 0 || height == 0 || self.cells.is_empty() || self.cells[0].is_empty() {
            return GraphViewport {
                lines: Vec::new(),
                selection: None,
                clipped_left: false,
                clipped_right: false,
                clipped_up: false,
                clipped_down: false,
            };
        }
        let selected = selected.min(self.positions.len().saturating_sub(1));
        let (selected_x, selected_y) = self.positions[selected];
        let canvas_height = self.cells.len();
        let canvas_width = self.cells[0].len();
        let visible_width = width.min(canvas_width);
        let visible_height = height.min(canvas_height);
        let anchor_x = selected_x + self.label_widths[selected] / 2;
        let x = anchor_x
            .saturating_sub(visible_width / 2)
            .min(canvas_width - visible_width);
        let y = selected_y
            .saturating_sub(visible_height / 2)
            .min(canvas_height - visible_height);
        let mut lines: Vec<String> = (0..visible_height)
            .map(|row| {
                let mut visible = self.cells[y + row][x..x + visible_width].to_vec();
                if y + row == selected_y && (x..x + visible_width).contains(&selected_x) {
                    visible[selected_x - x] = '*';
                }
                visible.into_iter().collect()
            })
            .collect();
        let label_end = selected_x + self.label_widths[selected];
        let visible_start = selected_x.max(x);
        let visible_end = label_end.min(x + visible_width);
        let selection = (selected_y >= y && selected_y < y + visible_height)
            .then_some((
                selected_y - y,
                visible_start - x,
                visible_end.saturating_sub(visible_start),
            ))
            .filter(|selection| selection.2 > 0);
        for line in &mut lines {
            while line.ends_with(' ') {
                line.pop();
            }
        }
        GraphViewport {
            lines,
            selection,
            clipped_left: x > 0,
            clipped_right: x + visible_width < canvas_width,
            clipped_up: y > 0,
            clipped_down: y + visible_height < canvas_height,
        }
    }
}

fn add_route_work(current: usize, added: usize) -> Result<usize, String> {
    let total = current
        .checked_add(added)
        .ok_or("ASCII graph routing work overflow")?;
    if total > MAX_DIAGRAM_ROUTE_CELLS {
        return Err(format!(
            "diagram routing exceeds the TUI limit of {MAX_DIAGRAM_ROUTE_CELLS} cells; edit this pipeline in TOML"
        ));
    }
    Ok(total)
}

fn draw_horizontal(cells: &mut [Vec<char>], from: usize, to: usize, y: usize, character: char) {
    let (start, end) = if from <= to { (from, to) } else { (to, from) };
    for x in start..=end {
        merge_edge(cells, x, y, character);
    }
}

fn draw_vertical(cells: &mut [Vec<char>], x: usize, from: usize, to: usize, character: char) {
    let (start, end) = if from <= to { (from, to) } else { (to, from) };
    for y in start..=end {
        merge_edge(cells, x, y, character);
    }
}

fn merge_edge(cells: &mut [Vec<char>], x: usize, y: usize, character: char) {
    let Some(cell) = cells.get_mut(y).and_then(|row| row.get_mut(x)) else {
        return;
    };
    *cell = match *cell {
        ' ' => character,
        existing if existing == character => existing,
        _ => '+',
    };
}

#[derive(Clone, Copy)]
struct GraphLink {
    from: usize,
    to: usize,
    control: bool,
}

struct PipelineGraph {
    levels: Vec<Vec<usize>>,
    links: Vec<GraphLink>,
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
        let mut incoming = vec![Vec::new(); view.nodes.len()];
        let mut outgoing = vec![Vec::new(); view.nodes.len()];
        let mut dependencies = BTreeSet::new();
        let mut control_dependencies = BTreeSet::new();
        for edge in &view.edges {
            let (Some(&from), Some(&to)) = (
                indexes.get(edge.from_node.as_str()),
                indexes.get(edge.to_node.as_str()),
            ) else {
                continue;
            };
            dependencies.insert((from, to));
            let mapping = format!(
                "{}.{} -> {}.{}",
                edge.from_node, edge.from_port, edge.to_node, edge.to_port
            );
            incoming[to].push(mapping.clone());
            outgoing[from].push(mapping);
        }
        for (to, node) in view.nodes.iter().enumerate() {
            let Some(gate_id) = node.gated_by.as_deref() else {
                continue;
            };
            let Some(&from) = indexes.get(gate_id) else {
                continue;
            };
            dependencies.insert((from, to));
            control_dependencies.insert((from, to));
            let mapping = format!("CONTROL {gate_id} -> {}", node.id);
            incoming[to].push(mapping.clone());
            outgoing[from].push(mapping);
        }
        let links = dependencies
            .iter()
            .map(|&(from, to)| GraphLink {
                from,
                to,
                control: control_dependencies.contains(&(from, to)),
            })
            .collect();
        let mut indegree = vec![0_usize; view.nodes.len()];
        let mut adjacency = vec![Vec::new(); view.nodes.len()];
        for &(from, to) in &dependencies {
            indegree[to] += 1;
            adjacency[from].push(to);
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
        for (node, level) in node_levels.into_iter().enumerate() {
            levels[level].push(node);
        }
        Self {
            levels,
            links,
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
    PipelineGraph,
    PipelinePolicy,
    Reviewers,
    Providers,
}

impl TopTab {
    fn next(self) -> Self {
        match self {
            Self::PipelineGraph => Self::PipelinePolicy,
            Self::PipelinePolicy => Self::Reviewers,
            Self::Reviewers => Self::Providers,
            Self::Providers => Self::PipelineGraph,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::PipelineGraph => Self::Providers,
            Self::PipelinePolicy => Self::PipelineGraph,
            Self::Reviewers => Self::PipelinePolicy,
            Self::Providers => Self::Reviewers,
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
    RefreshProviders,
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
    diagram: Result<AsciiGraph, String>,
    pipeline_dirty: bool,
    reviewers_root: PathBuf,
    lock_path: PathBuf,
    lock_original: String,
    reviewers: Vec<ReviewerConfig>,
    providers: providers::ProviderInventory,
    tab: TopTab,
    selected_node: usize,
    selected_policy: usize,
    selected_reviewer: usize,
    selected_provider: usize,
    selected_config: usize,
    selected_run: usize,
    provider_refresh: Option<ProviderRefresh>,
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

struct ProviderRefresh {
    receiver: Receiver<providers::ProviderInventory>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut refresh) = self.provider_refresh.take() {
            refresh
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(handle) = refresh.handle.take() {
                let _ = handle.join();
            }
        }
    }
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
        let diagram = AsciiGraph::new(&pipeline_view, &graph);
        let providers = providers::discover();

        Ok(Self {
            options,
            repository,
            review_root,
            pipeline_path: pipeline,
            pipeline_original: pipeline_text.clone(),
            pipeline_text,
            pipeline_view,
            graph,
            diagram,
            pipeline_dirty: false,
            reviewers_root,
            lock_path,
            lock_original,
            reviewers,
            providers,
            tab: TopTab::PipelineGraph,
            selected_node: 0,
            selected_policy: 0,
            selected_reviewer: 0,
            selected_provider: 0,
            selected_config: 0,
            selected_run: 0,
            provider_refresh: None,
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
            KeyCode::Tab => {
                self.tab = self.tab.next();
                if self.provider_refresh_needed() {
                    return Action::RefreshProviders;
                }
            }
            KeyCode::BackTab => {
                self.tab = self.tab.previous();
                if self.provider_refresh_needed() {
                    return Action::RefreshProviders;
                }
            }
            KeyCode::Char('s') => return Action::Export,
            KeyCode::Char('R') => {
                if self.tab == TopTab::Providers {
                    return Action::RefreshProviders;
                }
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
                TopTab::PipelineGraph | TopTab::PipelinePolicy => self.handle_pipeline_key(key),
                TopTab::Reviewers => self.handle_reviewer_key(key),
                TopTab::Providers => self.handle_provider_key(key),
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

    fn handle_provider_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_provider_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_provider_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.provider_selection_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.provider_selection_edge(true),
            _ => {}
        }
    }

    fn start_provider_refresh(&mut self) {
        if self.provider_refresh.is_some() {
            self.failure("Provider refresh is already running");
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancelled = std::sync::Arc::clone(&cancelled);
        let handle = std::thread::spawn(move || {
            let inventory = providers::discover_with_cancel(&worker_cancelled);
            let _ = sender.send(inventory);
        });
        self.provider_refresh = Some(ProviderRefresh {
            receiver,
            cancelled,
            handle: Some(handle),
        });
        self.success("Refreshing provider status in the background");
    }

    fn provider_refresh_needed(&self) -> bool {
        self.tab == TopTab::Providers
            && self.provider_refresh.is_none()
            && self
                .providers
                .providers
                .iter()
                .any(|provider| provider.status == "not probed")
    }

    fn finish_provider_refresh(&mut self) -> bool {
        let result = match self
            .provider_refresh
            .as_ref()
            .map(|refresh| refresh.receiver.try_recv())
        {
            Some(Ok(inventory)) => Some(Ok(inventory)),
            Some(Err(TryRecvError::Disconnected)) => Some(Err(())),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        if result.is_none() {
            return false;
        }
        let mut refresh = self
            .provider_refresh
            .take()
            .expect("completed provider refresh exists");
        if let Some(handle) = refresh.handle.take() {
            let _ = handle.join();
        }
        match result {
            Some(Ok(inventory)) => {
                self.providers = inventory;
                self.selected_provider = self
                    .selected_provider
                    .min(self.providers.providers.len().saturating_sub(1));
                self.success("Provider status refreshed");
                true
            }
            Some(Err(())) => {
                self.failure("Provider refresh worker stopped without a result");
                true
            }
            None => unreachable!("empty refresh result returned above"),
        }
    }

    fn handle_pipeline_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.tab == TopTab::PipelineGraph {
                    self.move_graph_selection(0, 1);
                } else {
                    self.move_pipeline_selection(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.tab == TopTab::PipelineGraph {
                    self.move_graph_selection(0, -1);
                } else {
                    self.move_pipeline_selection(-1);
                }
            }
            KeyCode::Char('g') | KeyCode::Home => self.pipeline_selection_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.pipeline_selection_edge(true),
            KeyCode::Char('h') | KeyCode::Left if self.tab == TopTab::PipelineGraph => {
                self.move_graph_selection(-1, 0);
            }
            KeyCode::Char('l') | KeyCode::Right if self.tab == TopTab::PipelineGraph => {
                self.move_graph_selection(1, 0);
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_pipeline_selection(),
            KeyCode::Char('a') if self.tab == TopTab::PipelineGraph => {
                self.begin_edit(EditTarget::AddReviewer, "reviewer package", String::new());
            }
            KeyCode::Char('d') if self.tab == TopTab::PipelineGraph => {
                self.remove_selected_reviewer();
            }
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
            TopTab::PipelineGraph | TopTab::PipelinePolicy => {
                self.move_pipeline_selection(amount);
            }
            TopTab::Reviewers => self.move_reviewer_selection(amount),
            TopTab::Providers => self.move_provider_selection(amount),
        }
    }

    fn move_provider_selection(&mut self, amount: isize) {
        self.selected_provider = self
            .selected_provider
            .saturating_add_signed(amount)
            .min(self.providers.providers.len().saturating_sub(1));
    }

    fn provider_selection_edge(&mut self, end: bool) {
        self.selected_provider = if end {
            self.providers.providers.len().saturating_sub(1)
        } else {
            0
        };
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
        let (selected, count) = match self.tab {
            TopTab::PipelineGraph => (&mut self.selected_node, self.pipeline_view.nodes.len()),
            TopTab::PipelinePolicy => (&mut self.selected_policy, POLICY_ROWS),
            TopTab::Reviewers => return,
            TopTab::Providers => return,
        };
        *selected = selected
            .saturating_add_signed(amount)
            .min(count.saturating_sub(1));
    }

    fn pipeline_selection_edge(&mut self, end: bool) {
        match self.tab {
            TopTab::PipelineGraph => {
                let selected = if end {
                    self.graph.levels.last().and_then(|level| level.last())
                } else {
                    self.graph.levels.first().and_then(|level| level.first())
                };
                if let Some(&selected) = selected {
                    self.selected_node = selected;
                }
            }
            TopTab::PipelinePolicy => {
                self.selected_policy = if end { POLICY_ROWS - 1 } else { 0 };
            }
            TopTab::Reviewers => {}
            TopTab::Providers => {}
        }
    }

    fn move_graph_selection(&mut self, horizontal: isize, vertical: isize) {
        let Some((level, row)) = self
            .graph
            .levels
            .iter()
            .enumerate()
            .find_map(|(level, nodes)| {
                nodes
                    .iter()
                    .position(|node| *node == self.selected_node)
                    .map(|row| (level, row))
            })
        else {
            return;
        };
        let target_level = level
            .saturating_add_signed(horizontal)
            .min(self.graph.levels.len().saturating_sub(1));
        let target_nodes = &self.graph.levels[target_level];
        let target_row = row
            .saturating_add_signed(vertical)
            .min(target_nodes.len().saturating_sub(1));
        if let Some(&node) = target_nodes.get(target_row) {
            self.selected_node = node;
        }
    }

    fn activate_pipeline_selection(&mut self) {
        match self.tab {
            TopTab::PipelineGraph => {
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
            TopTab::PipelinePolicy => {
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
            TopTab::Reviewers => {}
            TopTab::Providers => {}
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
        let diagram = AsciiGraph::new(&view, &graph);
        self.pipeline_text = candidate;
        self.pipeline_view = view;
        self.graph = graph;
        self.diagram = diagram;
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

fn draw_top_tabs(stdout: &mut Stdout, active: TopTab, width: u16) -> Result<(), String> {
    paint_spans(
        stdout,
        2,
        1,
        width - 2,
        &[
            (
                "[ PIPELINE GRAPH ]",
                if active == TopTab::PipelineGraph {
                    Paint::Tab
                } else {
                    Paint::Normal
                },
            ),
            (" ", Paint::Normal),
            (
                "[ PIPELINE POLICY ]",
                if active == TopTab::PipelinePolicy {
                    Paint::Tab
                } else {
                    Paint::Normal
                },
            ),
            (" ", Paint::Normal),
            (
                "[ REVIEWERS ]",
                if active == TopTab::Reviewers {
                    Paint::Tab
                } else {
                    Paint::Normal
                },
            ),
            (" ", Paint::Normal),
            (
                "[ PROVIDERS ]",
                if active == TopTab::Providers {
                    Paint::Tab
                } else {
                    Paint::Normal
                },
            ),
        ],
    )
}

fn draw_key_legend(stdout: &mut Stdout, y: u16, width: u16, tab: TopTab) -> Result<(), String> {
    let graph = [
        (" ", Paint::Muted),
        ("h/j/k/l g/G C-u/C-d", Paint::Navigation),
        (" move | ", Paint::Muted),
        ("Tab/S-Tab", Paint::Tab),
        (" tabs | ", Paint::Muted),
        ("Enter a d s R r", Paint::Mutation),
        (" change/run | q quit", Paint::Muted),
    ];
    let policy = [
        (" ", Paint::Muted),
        ("j/k g/G C-u/C-d", Paint::Navigation),
        (" move | ", Paint::Muted),
        ("Tab/S-Tab", Paint::Tab),
        (" tabs | ", Paint::Muted),
        ("Enter s R r", Paint::Mutation),
        (" change/run | q quit", Paint::Muted),
    ];
    let reviewers = [
        (" ", Paint::Muted),
        ("j/k g/G C-u/C-d h/l", Paint::Navigation),
        (" move | ", Paint::Muted),
        ("Tab/S-Tab", Paint::Tab),
        (" tabs | ", Paint::Muted),
        ("Enter s R r", Paint::Mutation),
        (" change/run | q quit", Paint::Muted),
    ];
    let providers = [
        (" ", Paint::Muted),
        ("j/k g/G C-u/C-d", Paint::Navigation),
        (" select | ", Paint::Muted),
        ("Tab/S-Tab", Paint::Tab),
        (" tabs | R reload/probe | q quit", Paint::Muted),
    ];
    paint_spans(
        stdout,
        y,
        0,
        width,
        match tab {
            TopTab::PipelineGraph => &graph,
            TopTab::PipelinePolicy => &policy,
            TopTab::Reviewers => &reviewers,
            TopTab::Providers => &providers,
        },
    )
}

fn paint_graph_row(
    stdout: &mut Stdout,
    y: u16,
    x: u16,
    width: u16,
    line: &str,
    selection: Option<(usize, usize, usize)>,
) -> Result<(), String> {
    let Some((_, start, length)) = selection else {
        return paint(stdout, y, x, width, line, Paint::Muted);
    };
    let start = start.min(line.len());
    let end = start.saturating_add(length).min(line.len());
    paint_spans(
        stdout,
        y,
        x,
        width,
        &[
            (&line[..start], Paint::Muted),
            (&line[start..end], Paint::Navigation),
            (&line[end..], Paint::Muted),
        ],
    )
}

fn paint_spans(
    stdout: &mut Stdout,
    y: u16,
    x: u16,
    width: u16,
    spans: &[(&str, Paint)],
) -> Result<(), String> {
    let mut offset = 0_usize;
    let total = usize::from(width);
    for (text, style) in spans {
        if offset >= total {
            break;
        }
        let text = display_ascii(text);
        let visible = text.len().min(total - offset);
        if visible == 0 {
            continue;
        }
        paint(
            stdout,
            y,
            x + offset as u16,
            visible as u16,
            &text[..visible],
            *style,
        )?;
        offset += visible;
    }
    if offset < total {
        paint(
            stdout,
            y,
            x + offset as u16,
            (total - offset) as u16,
            "",
            Paint::Muted,
        )?;
    }
    Ok(())
}

fn display_ascii(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if (' '..='~').contains(&character) {
                character.to_string().chars().collect::<Vec<_>>()
            } else {
                character.escape_default().collect()
            }
        })
        .collect()
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
            Paint::Navigation
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
    Tab,
    Navigation,
    Mutation,
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
    let text = display_ascii(text);
    let text_width = text.chars().count();
    let mut visible = if text_width > width {
        if width <= 3 {
            ".".repeat(width)
        } else {
            text.chars().take(width - 3).collect::<String>() + "..."
        }
    } else {
        text
    };
    visible.extend(std::iter::repeat_n(
        ' ',
        width.saturating_sub(visible.chars().count()),
    ));
    let (foreground, background, bold) = match paint {
        Paint::Normal => (Color::White, Color::Reset, false),
        Paint::Muted => (Color::DarkGrey, Color::Reset, false),
        Paint::Header => (Color::Black, Color::Cyan, true),
        Paint::Tab => (Color::Black, Color::Yellow, true),
        Paint::Navigation => (Color::Cyan, Color::Reset, true),
        Paint::Mutation => (Color::Green, Color::Reset, true),
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
    use super::{
        AsciiGraph, PipelineGraph, append_full_file_patch, display_ascii, proposal_directory,
    };
    use review_config::pipeline_edit::pipeline_view;

    const ASCII_PIPELINE: &str = r#"version = 1

[[nodes]]
id = "gate"
kind = "gate"
inputs = []
outputs = []

[[nodes]]
id = "source"
kind = "generation"
inputs = []
outputs = []

[[nodes]]
id = "middle"
kind = "reviewer"
inputs = []
outputs = []
gated_by = "gate"
package = "architecture"

[[nodes]]
id = "branch"
kind = "reviewer"
inputs = []
outputs = []
package = "contracts"

[[nodes]]
id = "sink"
kind = "gather"
inputs = []
outputs = []

[[nodes]]
id = "final"
kind = "ledger"
inputs = []
outputs = []

[[edges]]
from = { node = "source", port = "a" }
to = { node = "middle", port = "a" }

[[edges]]
from = { node = "source", port = "b" }
to = { node = "middle", port = "b" }

[[edges]]
from = { node = "source", port = "branch" }
to = { node = "branch", port = "input" }

[[edges]]
from = { node = "middle", port = "result" }
to = { node = "sink", port = "middle" }

[[edges]]
from = { node = "branch", port = "result" }
to = { node = "sink", port = "branch" }

[[edges]]
from = { node = "source", port = "skip" }
to = { node = "final", port = "skip" }

[[edges]]
from = { node = "sink", port = "reports" }
to = { node = "final", port = "reports" }

[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
"#;

    #[test]
    fn ascii_graph_routes_and_follows_the_selected_node() {
        let view = pipeline_view(ASCII_PIPELINE).unwrap();
        let graph = PipelineGraph::new(&view);
        let middle = view
            .nodes
            .iter()
            .position(|node| node.id == "middle")
            .unwrap();
        let diagram = AsciiGraph::new(&view, &graph).unwrap();
        let rendered = diagram.viewport(middle, 200, 200).lines.join("\n");
        assert!(rendered.is_ascii());
        assert!(rendered.contains("*[middle]"));
        assert!(rendered.contains("[sink]"));
        assert!(rendered.contains('-'));
        assert!(rendered.contains('|'));
        assert!(rendered.contains('.'));
        assert!(rendered.contains('>'));
        assert!(graph.incoming[middle].contains("source.a -> middle.a"));
        assert!(graph.incoming[middle].contains("source.b -> middle.b"));
        assert!(graph.incoming[middle].contains("CONTROL gate -> middle"));

        let final_node = view
            .nodes
            .iter()
            .position(|node| node.id == "final")
            .unwrap();
        let clipped = diagram.viewport(final_node, 24, 5);
        assert!(clipped.lines.iter().any(|line| line.contains("*[final]")));
        assert!(clipped.clipped_left);
    }

    #[test]
    fn terminal_data_is_printable_ascii() {
        let escaped = display_ascii("node\u{1b}\n雪");
        assert!(escaped.bytes().all(|byte| (b' '..=b'~').contains(&byte)));
        assert!(escaped.contains("\\u{1b}"));
        assert!(escaped.contains("\\n"));
        assert!(escaped.contains("\\u{96ea}"));
    }

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
