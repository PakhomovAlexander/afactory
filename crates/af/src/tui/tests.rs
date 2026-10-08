//! The browser over `fixtures/consumers/hub`: one golden render per pane at 100x30, the key
//! paths that reach them, and the command line's use of the CLI's own parser.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::*;
use crate::config::{self, MachineRoots};
use crate::providers::{ProviderInventory, ProviderLimit, ProviderStatus, UsageProbe, UsageState};

const SETTINGS: &str = include_str!("../../tests/fixtures/tui/settings-100x30.txt");
const PROVIDERS: &str = include_str!("../../tests/fixtures/tui/providers-100x30.txt");
const TASKS: &str = include_str!("../../tests/fixtures/tui/tasks-100x30.txt");
const TASK: &str = include_str!("../../tests/fixtures/tui/task-100x30.txt");
const WORKER: &str = include_str!("../../tests/fixtures/tui/worker-100x30.txt");

fn workspace() -> PathBuf {
    // Resolved at run time: a test binary a warm gate reuses was compiled in another sandbox.
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            // The source may be a read-only materialized tree (the kernel's own check run);
            // the copy is a fixture the tests edit.
            let permissions = std::os::unix::fs::PermissionsExt::from_mode(0o644);
            std::fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

fn git_out(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `fixtures/consumers/hub` as a committed repository at `root/hub`: the hub's own `.af/` over
/// the pinned Task packages it shares with `fixtures/task-runtime/pagination`.
pub(crate) fn hub_repo(root: &Path) -> PathBuf {
    let repo = root.join("hub");
    let pagination = workspace().join("fixtures/task-runtime/pagination");
    copy_tree(&pagination, &repo);
    copy_tree(&workspace().join("fixtures/consumers/hub"), &repo);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "hub"]);
    repo
}

/// The hub with two Tasks recorded, token-free, in the Task state `af task` resolves for it
/// under `root/state`: `pagination-cli`, whose command-Worker implementer writes the fix, then
/// `pagination-unfinished`, run after the implementer stopped writing it, so its check fails.
/// Returns the repository and that Task state directory.
pub(crate) fn hub_with_tasks(root: &Path) -> (PathBuf, PathBuf) {
    let repo = hub_repo(root);
    let xdg = root.join("state");
    let state = crate::task_execution::default_task_state(&xdg, &repo).unwrap();
    record_task(root, &repo, &state, "pagination-cli");
    let implementer = repo.join(".af/task-packages/fixture/implementer");
    let worker = "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'schema':'af.worker-reply/1',\
                  'outputs':{'report':[{'summary':'Left pagination as it was'}]}}))\n";
    std::fs::write(implementer.join("worker.py"), worker).unwrap();
    let catalog = repo.join(".af/task-catalog.toml");
    let text = std::fs::read_to_string(&catalog).unwrap();
    let mut value: toml::Value = toml::from_str(&text).unwrap();
    let digest = review_config::lock::package_digest("fixture/implementer", &implementer);
    value["packages"]["fixture/implementer"]["digest"] = toml::Value::String(digest.unwrap());
    std::fs::write(&catalog, toml::to_string(&value).unwrap()).unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "an implementer that leaves pagination unfinished",
        ],
    );
    record_task(root, &repo, &state, "pagination-unfinished");
    (repo, state)
}

/// Run the fixture's ticket under `task_id` the way `af task start --execute` runs it.
pub(crate) fn record_task(root: &Path, repo: &Path, state: &Path, task_id: &str) {
    let ticket = std::fs::read(repo.join("ticket.json")).unwrap();
    let mut task: serde_json::Value = serde_json::from_slice(&ticket).unwrap();
    task["task_id"] = serde_json::Value::String(task_id.to_owned());
    let file = root.join(format!("{task_id}.json"));
    std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
    crate::task_execution::run_silently(&file, repo, state).unwrap();
}

/// Lines two captures of one Task file share: the plan ID covers the deadline and TIME counts
/// down to it, so those lines keep only their label.
pub(crate) fn stable_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let content = line.trim();
        let volatile = content.starts_with("PLAN ")
            || content.starts_with("TIME ")
            || content.starts_with("--confirm-plan ");
        if volatile {
            lines.push(content.split(' ').next().unwrap_or_default().to_owned());
        } else {
            lines.push(line.trim_end().to_owned());
        }
    }
    lines
}

/// The hub's project scope with the machine layers pinned under `root`, which is also home:
/// nothing in a render depends on the machine running it.
pub(crate) fn hub_scope(root: &Path, repo: &Path) -> Scope {
    let roots = MachineRoots {
        system: root.join("etc/af"),
        user: root.join("config/af"),
        environment: Vec::new(),
    };
    let config = config::load_rooted(Some(repo), false, &roots).unwrap();
    Scope {
        kind: ScopeKind::Project,
        root: config.toplevel.clone().unwrap(),
        config,
        home: Some(root.to_path_buf()),
        registry: Some(root.join("config/af/providers.toml")),
        state: Some(root.join("state")),
    }
}

fn limit(name: &str, window: u64, used: u8) -> ProviderLimit {
    ProviderLimit {
        name: name.to_owned(),
        window_minutes: Some(window),
        used_percent: used,
        resets_at: None,
    }
}

/// A fixed inventory: no Provider CLI runs in a golden render.
fn inventory() -> ProviderInventory {
    let session = limit("5h window", 300, 41);
    let weekly = limit("weekly", 10_080, 68);
    let claude = ProviderStatus {
        id: "claude-main".to_owned(),
        kind: "claude".to_owned(),
        auth_context: "CLI default".to_owned(),
        source: "registry".to_owned(),
        status: "authenticated".to_owned(),
        auth_type: "oauth".to_owned(),
        subscription: "Max 20x".to_owned(),
        limits: vec![session, weekly],
        usage: UsageState::Available,
        detail: "weekly limit read from the local /usage screen".to_owned(),
    };
    let codex = ProviderStatus {
        id: "codex-ambient".to_owned(),
        kind: "codex".to_owned(),
        auth_context: "CLI default".to_owned(),
        source: "ambient CLI candidate; unstable local context label".to_owned(),
        status: "not authenticated".to_owned(),
        auth_type: "-".to_owned(),
        subscription: "-".to_owned(),
        limits: Vec::new(),
        usage: UsageState::NotApplicable,
        detail: "run `codex login` in this context".to_owned(),
    };
    ProviderInventory {
        providers: vec![claude, codex],
        registry: None,
        warning: None,
    }
}

fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    (temp, root)
}

fn hub_app(root: &Path) -> App {
    let repo = hub_repo(root);
    app_at(root, repo)
}

fn app_at(root: &Path, repo: PathBuf) -> App {
    let scope = hub_scope(root, &repo);
    let providers = ProvidersPane::with_inventory(inventory(), UsageProbe::Probe);
    let mut app = App::new(scope, Some(repo), Panes::new(providers));
    app.frame(100, 30);
    app
}

/// A terminal that records what the browser sends it and does to it, in order, and never runs a
/// child: `child` stands for what one would do to the disk, and `exit` for how it ends.
#[derive(Default)]
struct Recorder {
    sent: Vec<u8>,
    released: usize,
    /// `release`, `run ARGS`, `pause LINE` and `reenter`, as they happened.
    events: Vec<String>,
    runs: Vec<HandOff>,
    /// How a child ends; `exit 0` when unset.
    exit: Option<Exit>,
    /// A child that cannot be spawned, and why.
    unspawnable: Option<String>,
    /// A release that fails, and why.
    unreleasable: Option<String>,
    /// A re-entry that fails, and why: the terminal was left to the shell.
    unreenterable: Option<String>,
    child: Option<Box<dyn FnMut()>>,
}

impl Host for Recorder {
    fn release(&mut self) -> Result<(), String> {
        self.released += 1;
        self.events.push("release".to_owned());
        match &self.unreleasable {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn reenter(&mut self) -> Result<(), String> {
        self.events.push("reenter".to_owned());
        if let Some(error) = &self.unreenterable {
            return Err(error.clone());
        }
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.sent.extend_from_slice(bytes);
        Ok(())
    }

    fn run(&mut self, child: &HandOff) -> Result<Exit, String> {
        self.events.push(format!("run {}", child.args.join(" ")));
        self.runs.push(child.clone());
        if let Some(error) = &self.unspawnable {
            return Err(error.clone());
        }
        if let Some(child) = &mut self.child {
            child();
        }
        Ok(self.exit.unwrap_or(Exit::Code(0)))
    }

    fn pause(&mut self, line: &str) -> Result<(), String> {
        self.events.push(format!("pause {line}"));
        Ok(())
    }
}

fn press(app: &mut App, host: &mut Recorder, keys: &[u8]) {
    for key in keymap::decode(keys) {
        if let Some(effect) = app.key(key) {
            app.apply(effect, host);
        }
    }
}

fn status_line(app: &mut App) -> String {
    let frame = app.frame(100, 30).text();
    frame.lines().last().unwrap_or_default().to_owned()
}

#[test]
fn the_settings_pane_golden_at_100x30() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    assert_eq!(app.frame(100, 30).text(), SETTINGS);
}

#[test]
fn the_providers_pane_golden_at_100x30() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]j\r");
    assert_eq!(app.breadcrumb(), "providers/claude-main");
    assert_eq!(app.frame(100, 30).text(), PROVIDERS);
    // `y` copies the provider's id with OSC 52.
    press(&mut app, &mut host, b"y");
    assert_eq!(host.sent, b"\x1b]52;c;Y2xhdWRlLW1haW4=\x07");
}

#[test]
fn the_pipelines_pane_is_the_task_plan_tree_preview() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]jj");
    assert_eq!(app.breadcrumb(), "pipelines/fixture/implementation");
    press(&mut app, &mut host, b"\r");
    let deadline = Instant::now() + Duration::from_secs(600);
    while app.panes.pipelines.busy().is_some() {
        assert!(Instant::now() < deadline, "the preview plan never compiled");
        std::thread::sleep(Duration::from_millis(50));
        app.poll();
    }
    let shown: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    assert_eq!(
        shown[1], "PIPE  fixture/implementation@1.0.0  [configured]",
        "{shown:#?}"
    );
    // The same Task file, planned again the way `af task plan` plans it.
    let file = root.join("preview.json");
    let task = panes::pipelines::preview_task("fixture/implementation", "implement", 3);
    std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
    let again =
        crate::task_execution::plan_tree_preview_at(&file, &root.join("hub"), "HEAD").unwrap();
    let shown_text = shown.join("\n");
    assert_eq!(stable_lines(&shown_text), stable_lines(&again.text));
    // The frame paints those rows beside the bar, clipped to the main pane.
    let frame = app.frame(100, 30).text();
    for (line, row) in frame.lines().zip(&shown) {
        let clipped = &row[..row.len().min(100 - BAR_WIDTH)];
        let main = line.get(BAR_WIDTH..).unwrap_or("");
        assert_eq!(main, clipped.trim_end(), "{frame}");
    }
    // With the main pane focused, the status line names a Worker row's slot binding.
    let slot = (0..shown.len()).find(|row| app.pane().status(*row).is_some());
    let slot = slot.expect("a Worker row");
    press(&mut app, &mut host, b"\t");
    for _ in 0..slot {
        press(&mut app, &mut host, b"j");
    }
    let status = status_line(&mut app);
    assert!(status.contains(" -> command Worker"), "{status}");
    assert!(status.contains("fixture/"), "{status}");
    let binding = app.pane().status(slot).unwrap();
    // The working tree drifts from HEAD: the pane says so on its first row, still shows the
    // committed plan, and the Worker binding follows its row down by one.
    let file = root.join("hub/.af/task-packages/fixture/implementation/pipeline.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{text}# drifted\n")).unwrap();
    press(&mut app, &mut host, b"R");
    let deadline = Instant::now() + Duration::from_secs(600);
    while app.panes.pipelines.busy().is_some() {
        assert!(
            Instant::now() < deadline,
            "the preview plan never recompiled"
        );
        std::thread::sleep(Duration::from_millis(50));
        app.poll();
    }
    let drifted: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    assert!(
        drifted[0].starts_with("working tree differs from HEAD"),
        "{drifted:#?}"
    );
    assert_eq!(
        stable_lines(&drifted[1..].join("\n")),
        stable_lines(&shown.join("\n")),
        "the committed plan is unchanged"
    );
    assert_eq!(app.pane().status(slot), None);
    assert_eq!(app.pane().status(slot + 1), Some(binding.clone()));
    // At the 80-column minimum the binding is what the status line shows: the breadcrumb
    // yields before the binding is cut. (R put the cursor back on the first row.)
    for _ in 0..=slot {
        press(&mut app, &mut host, b"j");
    }
    let status = app.frame(80, 24).text();
    let last = status.lines().last().unwrap().trim_end().to_owned();
    assert!(last.ends_with(&binding), "{last}");
    assert!(!last.starts_with("NORMAL  pipelines/"), "{last}");
    // The preview is bound to the commit the entries were read from: after HEAD moves to a
    // commit whose package no longer matches its pin, that commit's preview is what the
    // moving `HEAD` would give, while the entries' commit still plans exactly as shown.
    let hub = root.join("hub");
    let first = String::from_utf8(git_out(&hub, &["rev-parse", "HEAD"])).unwrap();
    let pipeline = hub.join(".af/task-packages/fixture/implementation/pipeline.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    std::fs::write(&pipeline, format!("{text}# moved\n")).unwrap();
    git(&hub, &["add", "-A"]);
    git(&hub, &["commit", "-qm", "moved"]);
    let preview_file = root.join("preview.json");
    let at_first =
        crate::task_execution::plan_tree_preview_at(&preview_file, &hub, first.trim()).unwrap();
    assert_eq!(stable_lines(&at_first.text), stable_lines(&again.text));
    let at_head = crate::task_execution::plan_tree_preview_at(&preview_file, &hub, "HEAD");
    assert_ne!(
        at_head.map(|preview| stable_lines(&preview.text)),
        Ok(stable_lines(&again.text)),
        "HEAD moved to a commit that plans differently"
    );
}

#[test]
fn an_editor_hand_off_refreshes_the_opened_pane() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]j\r");
    assert_eq!(app.breadcrumb(), "pipelines/review");
    let rows: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    assert!(rows[0].starts_with("PIPE  review"), "{rows:#?}");
    // The child edited the file; coming back, the pane already says the tree differs.
    let file = root.join("hub/.af/pipelines/review.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{text}# edited\n")).unwrap();
    app.finish(Ok(()), "edited".to_owned());
    let rows: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    assert!(
        rows[0].starts_with("working tree differs from HEAD"),
        "{rows:#?}"
    );
    assert_eq!(
        app.breadcrumb(),
        "pipelines/review *",
        "the bar row carries the marker"
    );
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("review"), "{bar}");
}

#[test]
fn e_edits_the_highlighted_layer_file_exactly_and_a_refresh_names_a_broken_config() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    // Every editable, present layer row hands off its own path, not a layer name.
    let project = root.join("hub/.af/af.toml");
    let rows: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    let row = rows
        .iter()
        .position(|row| row.starts_with("project "))
        .expect("the project layer row");
    let effect = app.panes.settings.key(Key::Char('e'), row).unwrap();
    assert_eq!(effect, Some(Effect::OpenEditor(project.clone())));
    // `R` on the settings pane after the project layer stops parsing: the old values stay on
    // screen and the status line says the settings were not refreshed, and why.
    std::fs::write(&project, "this = is not [toml\n").unwrap();
    press(&mut app, &mut host, b"R");
    let status = status_line(&mut app);
    assert!(status.contains("settings not refreshed"), "{status}");
    assert!(!status.contains("read again"), "{status}");
}

#[test]
fn command_lines_go_through_the_cli_parser() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b":frobnicate\r");
    let status = status_line(&mut app);
    assert!(
        status.contains("unrecognized subcommand 'frobnicate'"),
        "{status}"
    );
    // `:e` spells `af config edit`, so a layer clap does not know is clap's refusal.
    press(&mut app, &mut host, b":e nowhere\r");
    let status = status_line(&mut app);
    assert!(status.contains("invalid value 'nowhere'"), "{status}");
    press(&mut app, &mut host, b":help layers\r");
    let first = app.main_rows()[0].text();
    assert!(first.starts_with("af help layers -- "), "{first}");
    press(&mut app, &mut host, b"q");
    assert!(app.help.is_none() && !app.quit, "q closes help first");
    press(&mut app, &mut host, b":sc\t");
    assert_eq!(app.prompt.text, "scope ");
    press(&mut app, &mut host, b"\x03");
    assert_eq!(app.mode, Mode::Normal);
    press(&mut app, &mut host, b":q\r");
    assert!(app.quit);
    assert_eq!(host.released, 0, "no command was handed the terminal");
}

#[test]
fn below_80x24_one_line_names_the_minimum() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let text = app.frame(79, 24).text();
    let refusal = "af needs at least 80x24; this terminal is 79x24";
    assert_eq!(text.lines().next(), Some(refusal));
    assert!(text.lines().skip(1).all(str::is_empty), "{text}");
    // At 80 columns the bar starts hidden, and <C-b> shows it.
    let text = app.frame(80, 24).text();
    assert!(text.starts_with("SETTINGS  project: hub\n"), "{text}");
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"\x02");
    let text = app.frame(80, 24).text();
    assert!(text.starts_with("~/hub"), "{text}");
}

#[test]
fn search_finds_bar_nodes_and_main_rows() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"zM/codex\r");
    assert_eq!(app.breadcrumb(), "providers/codex-ambient");
    press(&mut app, &mut host, b"\t/keep\r");
    let row = app.main_rows()[app.main.cursor].text();
    assert!(row.starts_with("keep_versions = 3"), "{row}");
    press(&mut app, &mut host, b"/nothing-here\r");
    let status = status_line(&mut app);
    assert!(
        status.contains("pattern not found: nothing-here"),
        "{status}"
    );
}

#[test]
fn r_and_gf_act_on_the_bar_selection_while_another_pane_is_open() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    // Settings stays open; the bar cursor moves to the review pipeline.
    press(&mut app, &mut host, b"]]]]]]j");
    assert_eq!(app.breadcrumb(), "pipelines/review");
    assert_eq!(app.opened, Opened::Root);
    let file = root.join("hub/.af/pipelines/review.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{text}# edited\n")).unwrap();
    let marked = |app: &App| {
        app.panes
            .pipelines
            .items()
            .iter()
            .any(|item| item.id == ".af/pipelines/review.toml" && item.muted)
    };
    assert!(!marked(&app), "the pane has not read again yet");
    // `gf` from the bar: the editor came back, and the Pipelines pane already marks the file.
    app.finish(Ok(()), "edited".to_owned());
    assert!(marked(&app));
    assert_eq!(app.opened, Opened::Root, "what was opened stays opened");
    // The bar row itself carries the marker, selected or not.
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("review *"), "{bar}");
    // `R` on the bar selection refreshes that pane, not the opened Settings.
    std::fs::write(&file, text).unwrap();
    press(&mut app, &mut host, b"R");
    assert!(!marked(&app));
    let bar = app.frame(100, 30).text();
    assert!(!bar.contains("review *"), "{bar}");
    let status = status_line(&mut app);
    assert!(!status.contains("settings read again"), "{status}");
}

#[test]
fn an_entry_head_no_longer_commits_falls_back_to_its_folder() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]j\r");
    assert_eq!(app.breadcrumb(), "pipelines/review");
    let hub = root.join("hub");
    git(&hub, &["rm", "-q", ".af/pipelines/review.toml"]);
    git(&hub, &["commit", "-qm", "drop the review pipeline"]);
    // Opening it again reads the new HEAD: the entry is gone, the folder is shown, the bar
    // no longer lists it.
    app.show(Opened::Item(
        Tab::Pipelines,
        ".af/pipelines/review.toml".to_owned(),
    ));
    assert_eq!(app.opened, Opened::Folder(Tab::Pipelines));
    let status = status_line(&mut app);
    assert!(status.contains("no longer listed"), "{status}");
    let bar = app.frame(100, 30).text();
    assert!(!bar.contains("      review"), "{bar}");
}

#[test]
fn a_repository_that_stops_being_one_reloads_as_the_user_scope() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]j\r");
    assert_eq!(app.breadcrumb(), "pipelines/review");
    // `.git` goes away under the open browser; `R` on the root reads the place again.
    std::fs::rename(root.join("hub/.git"), root.join("hub-git-moved")).unwrap();
    press(&mut app, &mut host, b"gg");
    press(&mut app, &mut host, b"R");
    assert_eq!(app.scope.kind, ScopeKind::User);
    let screen = app.frame(100, 30).text();
    assert!(screen.contains("SETTINGS  user: ~"), "{screen}");
    assert!(!screen.contains("(project)"), "{screen}");
    assert!(
        app.panes.pipelines.items().is_empty(),
        "no project pipelines remain listed"
    );
}

/// A frame with what differs between two recordings of one Task masked: artifact IDs (eight
/// hex digits), times of day and durations. The padding that right-aligns a duration folds to
/// two spaces, so no column after it depends on how long anything took.
fn masked(frame: &str) -> String {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let single = |word: &str| {
        let seconds = word.strip_suffix('s').and_then(|rest| rest.split_once('.'));
        word.strip_suffix("ms").is_some_and(digits)
            || seconds.is_some_and(|(whole, tenth)| digits(whole) && digits(tenth))
    };
    let pair = |word: &str, next: &str| {
        let unit = |word: &str, unit: char| word.strip_suffix(unit).is_some_and(digits);
        (unit(word, 'm') && next.len() == 3 && unit(next, 's'))
            || (unit(word, 'h') && next.len() == 3 && unit(next, 'm'))
    };
    let mut text = String::new();
    for line in frame.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        let mut kept: Vec<String> = Vec::new();
        let mut index = 0;
        while index < words.len() {
            let word = words[index];
            let next = words.get(index + 1).copied().unwrap_or_default();
            // What follows `elapsed` is a duration even where the pane's edge cuts it short.
            let numeric = |word: &str| word.starts_with(|c: char| c.is_ascii_digit());
            let elapsed = kept.last().is_some_and(|word| word == "elapsed") && numeric(word);
            let taken = if pair(word, next) {
                2
            } else if single(word) {
                1
            } else if elapsed {
                if numeric(next) { 2 } else { 1 }
            } else {
                0
            };
            if taken > 0 {
                while kept.last().is_some_and(String::is_empty) {
                    kept.pop();
                }
                kept.push(String::new());
                kept.push("<t>".to_owned());
                index += taken;
                continue;
            }
            let hex = word.len() == 8 && word.bytes().all(|b| b.is_ascii_hexdigit());
            let clock = word.len() == 9
                && word.ends_with('Z')
                && word.split(':').count() == 3
                && word[..8].bytes().all(|b| b.is_ascii_digit() || b == b':');
            kept.push(match (hex, clock) {
                (true, _) => "########".to_owned(),
                (_, true) => "hh:mm:ssZ".to_owned(),
                _ => word.to_owned(),
            });
            index += 1;
        }
        text.push_str(&kept.join(" "));
        text.push('\n');
    }
    text
}

#[test]
fn masking_folds_durations_and_hides_ids_and_times() {
    let line = "PLAN  96779029  configured  STATE done  started 15:32:48Z  elapsed 1m 02s\n";
    let expected = "PLAN  ########  configured  STATE done  started hh:mm:ssZ  elapsed  <t>\n";
    assert_eq!(masked(line), expected);
    // Cut short by the edge of the pane, it is still the duration.
    for cut in ["elapsed 987m\n", "elapsed 1m 0\n"] {
        assert_eq!(masked(cut), "elapsed  <t>\n", "{cut}");
    }
    let row = "  [ok]  check                 46ms        0 tok\n";
    let wider = "  [ok]  check               1.3s        0 tok\n";
    assert_eq!(masked(row), masked(wider));
    assert_eq!(masked(row), "  [ok]  check  <t>        0 tok\n");
}

#[test]
fn the_tasks_pane_golden_at_100x30_and_its_verbs() {
    let (_temp, root) = temp_root();
    let (repo, state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]]]\r");
    assert_eq!(app.breadcrumb(), "tasks/");
    // The Store's directory is named after the repository's temporary path.
    let opaque = state.file_name().unwrap().to_string_lossy().into_owned();
    let frame = app.frame(100, 30).text().replace(&opaque, "<repository>");
    assert_eq!(masked(&frame), TASKS);
    press(&mut app, &mut host, b"jj");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    // `y` on the bar copies the Task id, not the row's progress text.
    press(&mut app, &mut host, b"y");
    assert_eq!(host.sent, b"\x1b]52;c;cGFnaW5hdGlvbi1jbGk=\x07");
    press(&mut app, &mut host, b"\r");
    assert_eq!(masked(&app.frame(100, 30).text()), TASK);
    // With the main pane focused, the legend names the pane's verbs.
    press(&mut app, &mut host, b"\t");
    let status = status_line(&mut app);
    assert!(status.ends_with("Enter artifact  p pipeline  y yank id  R re-read  Tab bar"));
    // Enter on a HISTORY row shows its artifact; `q` and `Esc` come back to that row.
    let shown = crate::task_execution::inspection_document(&state, "pagination-cli", false);
    let shown = shown.unwrap().unwrap();
    let plan_id = shown["plan_id"].as_str().unwrap().to_owned();
    press(&mut app, &mut host, b"/plan_admitted\r");
    let row = app.main.cursor;
    for back in [&b"q"[..], b"\x1b"] {
        press(&mut app, &mut host, b"\r");
        assert_eq!(app.main_rows()[0].text(), format!("ARTIFACT  {plan_id}"));
        let json: Vec<String> = app.main_rows().iter().map(Row::text).collect();
        let typed = "  \"type\": \"af/ExecutionPlan@1\"".to_owned();
        assert!(json.contains(&typed), "{json:#?}");
        assert_eq!(app.main.cursor, 0);
        let status = status_line(&mut app);
        assert!(status.ends_with("q/Esc back to the Task"), "{status}");
        press(&mut app, &mut host, back);
        assert!(!app.quit);
        assert!(
            app.main_rows()[0]
                .text()
                .starts_with("TASK  pagination-cli")
        );
        assert_eq!(app.main.cursor, row);
    }
    press(&mut app, &mut host, b"gg\r");
    let status = status_line(&mut app);
    assert!(
        status.contains("Enter opens the artifact of a HISTORY row"),
        "{status}"
    );
    // `y` in the main pane copies the Task id too.
    host.sent.clear();
    press(&mut app, &mut host, b"y");
    assert_eq!(host.sent, b"\x1b]52;c;cGFnaW5hdGlvbi1jbGk=\x07");
    // `R` reads the Store again at once and keeps the Task open.
    press(&mut app, &mut host, b"R");
    assert!(
        app.main_rows()[0]
            .text()
            .starts_with("TASK  pagination-cli")
    );
    // At the 80x24 minimum the bar hides and the pane starts at the first column.
    let small = app.frame(80, 24).text();
    assert!(
        small.starts_with("TASK  pagination-cli  implement: "),
        "{small}"
    );
    assert!(small.contains("\nPROGRESS  6 / 6 stages\n"), "{small}");
    // `p` opens the Task's pipeline where the Pipelines pane lists it.
    press(&mut app, &mut host, b"p");
    assert_eq!(app.breadcrumb(), "pipelines/fixture/implementation");
    let package = ".af/task-packages/fixture/implementation/pipeline.toml";
    assert_eq!(app.opened, Opened::Item(Tab::Pipelines, package.to_owned()));
    // A pipeline the pane does not list is named on the status line, and nothing moves.
    app.apply(
        Effect::OpenPipeline("elsewhere/pipeline".to_owned()),
        &mut host,
    );
    let status = status_line(&mut app);
    assert!(status.contains("pipeline elsewhere/pipeline is not listed under pipelines/"));
    assert_eq!(app.opened, Opened::Item(Tab::Pipelines, package.to_owned()));
}

/// The first screen row holding `needle`, and the column it starts at.
fn find(frame: &Frame, needle: &str) -> (usize, usize) {
    let text = frame.text();
    let found = text
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find(needle).map(|column| (row, column)));
    found.unwrap_or_else(|| panic!("{needle:?} is not on the screen:\n{text}"))
}

/// The paints of the cells `needle` covers where it first shows.
fn paints_of(frame: &Frame, needle: &str) -> Vec<Paint> {
    let (row, column) = find(frame, needle);
    (column..column + needle.len())
        .map(|column| frame.paint_at(row, column))
        .collect()
}

#[test]
fn states_are_chips_in_the_bar_the_task_header_and_its_stages() {
    let (_temp, root) = temp_root();
    let (repo, _state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    let ok = Paint::Chip(paint::Tone::Ok);
    press(&mut app, &mut host, b"]]]]]]]]jj");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    // The bar row ends in its percentage as a chip, which keeps its fill on the cursor row.
    let frame = app.frame(100, 30);
    let (row, column) = find(&frame, "  100% |");
    // A space of the cursor row, then ` 100% ` as the chip, ending at the separator.
    assert_eq!(column + 7, BAR_WIDTH - 1, "{}", frame.text());
    assert_eq!(frame.paint_at(row, column), Paint::Cursor);
    assert!((column + 1..column + 7).all(|at| frame.paint_at(row, at) == ok));
    press(&mut app, &mut host, b"\r");
    let frame = app.frame(100, 30);
    // The state word takes a space either side, and the text is the golden's.
    assert_eq!(paints_of(&frame, "STATE"), vec![Paint::Plain; 5]);
    assert_eq!(paints_of(&frame, " done "), vec![ok; 6]);
    assert_eq!(paints_of(&frame, "[ok]  inputs")[..4], vec![ok; 4]);
    assert_eq!(
        paints_of(&frame, "[ok]  inputs")[4..],
        vec![Paint::Plain; 8]
    );
    assert_eq!(masked(&frame.text()), TASK);
}

#[test]
fn bar_chips_are_one_width_for_right_aligned_progress() {
    for (label, field) in [
        ("gg-51945-c~  100%", "100%"),
        ("gg-51945-c~   63%", " 63%"),
        ("gg-51945-c~    0%", "  0%"),
    ] {
        let text = format!("        {label}");
        let at = chip_start(&text).unwrap();
        assert_eq!(&text[at..], field, "{label}");
        // The chip's opening space is the second of the two before the field.
        assert_eq!(&text[at - 3..at], "~  ", "{label}");
    }
    assert_eq!(chip_start("a b"), Some(2));
    assert_eq!(chip_start("no-space"), None);
}

#[test]
fn a_provider_status_is_a_chip_and_a_muted_candidate_stays_muted() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]\r");
    let frame = app.frame(100, 30);
    let ok = Paint::Chip(paint::Tone::Ok);
    assert_eq!(paints_of(&frame, " authenticated "), vec![ok; 15]);
    let (row, column) = find(&frame, "not authenticated");
    assert!((column..column + 17).all(|at| frame.paint_at(row, at) == Paint::Muted));
}

#[test]
fn the_status_line_turns_pink_while_it_carries_an_error() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let frame = app.frame(100, 30);
    assert!((0..100).all(|at| frame.paint_at(29, at) == Paint::Status));
    app.say_error("cannot read the scope");
    let frame = app.frame(100, 30);
    assert!(frame.text().ends_with("cannot read the scope\n"));
    assert!((0..100).all(|at| frame.paint_at(29, at) == Paint::Alert));
    app.say("read again");
    let frame = app.frame(100, 30);
    assert!((0..100).all(|at| frame.paint_at(29, at) == Paint::Status));
}

#[test]
fn the_help_banner_draws_the_pink_worker_with_its_eyes() {
    let rows = banner_rows();
    let text: Vec<String> = rows.iter().map(Row::text).collect();
    // The drawing is brand/ascii.txt's, cell for cell.
    assert_eq!(text[2], "  #  ###  #      agent pipelines made fast");
    let paints: Vec<Paint> = rows[2]
        .spans
        .iter()
        .flat_map(|span| std::iter::repeat_n(span.paint, span.text.len()))
        .collect();
    let (body, eye) = (
        Paint::Pixel(paint::Pixel::Pink),
        Paint::Pixel(paint::Pixel::Eye),
    );
    assert_eq!(
        paints[2..11],
        [body, eye, eye, body, body, body, eye, eye, body]
    );
    assert!(paints[11..].iter().all(|paint| *paint == Paint::Plain));
}

#[test]
fn a_store_this_binary_cannot_read_is_an_error_row_naming_it() {
    let (_temp, root) = temp_root();
    let repo = hub_repo(&root);
    let state = crate::task_execution::default_task_state(&root.join("state"), &repo).unwrap();
    std::fs::create_dir_all(state.join("cas/objects")).unwrap();
    std::fs::write(
        state.join("events.sqlite"),
        "records another release wrote\n",
    )
    .unwrap();
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]]]\r");
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    let shown = state.strip_prefix(&root).unwrap().display();
    // The Store's location, then its cause on a row of its own.
    let at = rows.iter().position(|row| *row == format!("~/{shown}:"));
    let at = at.unwrap_or_else(|| panic!("{rows:#?}"));
    let cause = " error  this Store cannot be read: ";
    assert!(
        rows[at + 1].starts_with(cause) && rows[at + 1].len() > cause.len(),
        "{rows:#?}"
    );
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("      ! Store unreadable"), "{bar}");
    // The row opens the same refusal, never an empty list.
    press(&mut app, &mut host, b"j\r");
    // The breadcrumb names the entry as the bar does, never by its internal id.
    assert_eq!(app.breadcrumb(), "tasks/! Store unreadable");
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    assert!(rows.iter().any(|row| row.starts_with(cause)), "{rows:#?}");
}

#[test]
fn only_an_opened_running_task_is_read_again_about_once_a_second() {
    let (_temp, root) = temp_root();
    let (repo, _state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]]]jj\r");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    let second = Duration::from_millis(1_100);
    std::thread::sleep(second);
    app.poll();
    assert_eq!(
        app.panes.tasks.reads(),
        0,
        "a finished Task is not read again"
    );
    // The same Task as its phase read while it ran: the next poll reads the Store on a
    // thread, and the one after it collects what the thread read.
    let running = serde_json::json!({"kind": "running"});
    *app.panes.tasks.detail_document().unwrap() = {
        let mut document = app.panes.tasks.detail_document().unwrap().clone();
        document["phase"] = running.clone();
        document
    };
    app.poll();
    assert_eq!(app.panes.tasks.reads(), 1);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !app.panes.tasks.poll() {
        assert!(Instant::now() < deadline, "the live read never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    // It found the Task finished, so nothing is read again.
    std::thread::sleep(second);
    app.poll();
    assert_eq!(app.panes.tasks.reads(), 1);
    assert!(app.main_rows()[1].text().contains("STATE done"));
    // A Task awaiting approval is read again too: the CLI may start it at any moment.
    app.panes.tasks.detail_document().unwrap()["phase"] = serde_json::json!({"kind": "ready"});
    std::thread::sleep(second);
    app.poll();
    assert_eq!(app.panes.tasks.reads(), 2, "an awaiting Task is live");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !app.panes.tasks.poll() {
        assert!(Instant::now() < deadline, "the live read never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(app.main_rows()[1].text().contains("STATE done"));
    // A running Task whose pane is not opened is not read either.
    app.panes.tasks.detail_document().unwrap()["phase"] = running;
    press(&mut app, &mut host, b"gg]]\r");
    assert_eq!(app.opened, Opened::Folder(Tab::Providers));
    std::thread::sleep(second);
    app.poll();
    assert_eq!(app.panes.tasks.reads(), 2);
}

#[test]
fn a_store_refused_on_a_live_read_closes_its_opened_task() {
    let (_temp, root) = temp_root();
    let (repo, state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]]]jj\r");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    // An artifact is open from the Task's HISTORY when the Store turns unreadable.
    press(&mut app, &mut host, b"\t");
    let history = app
        .main_rows()
        .iter()
        .position(|row| row.text().starts_with("HISTORY"))
        .unwrap();
    for _ in 0..=history {
        press(&mut app, &mut host, b"j");
    }
    press(&mut app, &mut host, b"\r");
    let top = app.main_rows()[0].text();
    assert!(top.starts_with("ARTIFACT"), "{top}");
    // The directory stays searchable (the Task still inspects by path) but cannot be listed.
    let search_only = std::os::unix::fs::PermissionsExt::from_mode(0o111);
    std::fs::set_permissions(&state, search_only).unwrap();
    press(&mut app, &mut host, b"R");
    let listable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&state, listable).unwrap();
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    assert!(
        !rows
            .iter()
            .any(|row| row.starts_with("TASK  pagination-cli")),
        "{rows:#?}"
    );
    assert!(
        !rows[0].starts_with("ARTIFACT"),
        "the opened artifact goes too: {rows:#?}"
    );
}

#[test]
fn the_user_scope_reads_a_symlinked_task_state_directory() {
    let (_temp, root) = temp_root();
    let (_repo, state) = hub_with_tasks(&root);
    let xdg = root.join("xdg-state");
    let local = crate::task_execution::local_task_states(&xdg);
    std::fs::create_dir_all(&local).unwrap();
    std::os::unix::fs::symlink(&state, local.join("0123456789abcdef")).unwrap();
    std::os::unix::fs::symlink(root.join("gone"), local.join("fedcba9876543210")).unwrap();
    let found = crate::tui::panes::tasks::user_targets(&xdg).unwrap();
    let names: Vec<&str> = found.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["0123456789abcdef", "fedcba9876543210"]);
    // The link to nothing is listed so the pane can refuse it, never shown as no Tasks.
    let gone = crate::tui::panes::tasks::refusal_of(&found[1].0);
    assert!(gone.is_some_and(|why| why.contains("link to nothing")));
}

#[test]
fn a_history_artifact_the_store_cannot_give_back_refuses_the_store() {
    let (_temp, root) = temp_root();
    let (repo, state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]]]jj\r");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    press(&mut app, &mut host, b"\t");
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    let history = rows
        .iter()
        .position(|row| row.starts_with("HISTORY"))
        .unwrap();
    // The artifact of the first HISTORY row is removed before Enter opens it.
    let show = crate::task_execution::inspection_document(&state, "pagination-cli", false)
        .unwrap()
        .unwrap();
    let first =
        crate::tui::panes::tasks::change_artifact(&show["history"][0]["transition"]["change"])
            .unwrap()
            .trim_start_matches("sha256:")
            .to_owned();
    std::fs::remove_file(
        state
            .join("cas/objects")
            .join(&first[..2])
            .join(&first[2..]),
    )
    .unwrap();
    for _ in 0..=history {
        press(&mut app, &mut host, b"j");
    }
    press(&mut app, &mut host, b"\r");
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    assert!(
        !rows
            .iter()
            .any(|row| row.starts_with("TASK  pagination-cli")),
        "{rows:#?}"
    );
    assert!(!rows[0].starts_with("ARTIFACT"), "{rows:#?}");
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("! Store unreadable"), "{bar}");
}

/// What `af task show --json` records for one Worker across the Tasks of a Store, derived here
/// without the pane: each reservation's invocation names its node and plan, the plan names its
/// compiled graph, and the graph binds the node's slot to a Worker. Returns the STATE rows the
/// pane must show.
/// STATE as derived straight from each Task's show document: the Attempts whose plan bound the
/// node's slot to `worker` at `digest`, the digest the committed catalog pins.
fn derived_state(state: &Path, task_ids: &[&str], worker: &str, digest: &str) -> Vec<String> {
    let artifact = |id: &str| crate::task_execution::recorded_artifact(state, id).unwrap();
    let (mut open, mut ok, mut failed, mut released, mut tokens) = (0, 0, 0, 0, 0_u128);
    for task_id in task_ids {
        let show = crate::task_execution::inspection_document(state, task_id, false);
        let show = show.unwrap().unwrap();
        let records: Vec<&serde_json::Value> = show["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| &entry["record"])
            .collect();
        for reserved in records.iter().filter(|record| record["kind"] == "reserved") {
            let invocation = artifact(reserved["invocation_id"].as_str().unwrap());
            let node = invocation["payload"]["node"].as_str().unwrap();
            let plan = artifact(invocation["payload"]["plan_id"].as_str().unwrap());
            let graph = artifact(plan["payload"]["compiled_graph_id"].as_str().unwrap());
            let graph = &graph["payload"];
            let Some(slot) = graph["nodes"][node]["operator"]["operator"]["slot"].as_str() else {
                continue;
            };
            let bound = &plan["payload"]["bindings"][slot]["package_digest"];
            if graph["slots"][slot]["worker"] != worker || bound != digest {
                continue;
            }
            let attempt = &reserved["attempt_id"];
            let own: Vec<&&serde_json::Value> = records
                .iter()
                .filter(|record| record["attempt_id"] == *attempt)
                .collect();
            if own.iter().any(|record| record["kind"] == "released") {
                released += 1;
                continue;
            }
            match own.iter().find(|record| record["kind"] == "settled") {
                None => open += 1,
                Some(settled) if settled["result"]["kind"] == "succeeded" => ok += 1,
                Some(_) => failed += 1,
            }
            let charges = own
                .iter()
                .filter_map(|record| record["charged_tokens"].as_str());
            tokens += charges
                .map(|n| n.parse::<u128>().unwrap())
                .max()
                .unwrap_or(0);
        }
    }
    vec![
        format!(
            "attempts  reserved {open}  settled ok {ok}  settled failed {failed}  released {released}"
        ),
        format!("tokens    charged {tokens}"),
    ]
}

#[test]
fn the_workers_pane_golden_at_100x30_and_its_state_is_af_task_shows() {
    let (_temp, root) = temp_root();
    let (repo, state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo.clone());
    let mut host = Recorder::default();
    // Nothing is read until the folder opens, so no other pane's bar changes.
    let folders: Vec<String> = app.tree.rows().iter().map(|row| row.text()).collect();
    let at = folders
        .iter()
        .position(|row| row == "  v workers/")
        .unwrap();
    assert_eq!(folders[at + 1], "  v pipelines/", "{folders:#?}");
    press(&mut app, &mut host, b"]]]]\r");
    assert_eq!(app.breadcrumb(), "workers/");
    let listed: Vec<String> = app
        .panes
        .workers
        .items()
        .into_iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(listed, ["fixture/evaluator", "fixture/implementer"]);
    press(&mut app, &mut host, b"jj");
    assert_eq!(app.breadcrumb(), "workers/fixture/implementer");
    press(&mut app, &mut host, b"\r");
    let frame = app.frame(100, 30).text();
    assert_eq!(masked(&frame), WORKER, "{frame}");
    // Every STATE number is what the Store's `af task show --json` documents record.
    let tasks = ["pagination-cli", "pagination-unfinished"];
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    let catalog: toml::Value = toml::from_str(&catalog).unwrap();
    let pinned = |worker: &str| {
        catalog["packages"][worker]["digest"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (worker, down) in [
        ("fixture/implementer", &b""[..]),
        ("fixture/evaluator", b"k\r"),
    ] {
        press(&mut app, &mut host, down);
        assert_eq!(app.breadcrumb(), format!("workers/{worker}"));
        let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
        let at = rows
            .iter()
            .position(|row| row.starts_with("attempts  "))
            .unwrap();
        assert_eq!(
            rows[at..at + 2],
            derived_state(&state, &tasks, worker, &pinned(worker)),
            "{rows:#?}"
        );
    }
    // `y` copies the Worker's name; `gf` opens its declaration, since it commits no prompt.
    press(&mut app, &mut host, b"\ty");
    assert_eq!(host.sent, b"\x1b]52;c;Zml4dHVyZS9ldmFsdWF0b3I=\x07");
    let declaration = repo.join(".af/task-packages/fixture/evaluator/worker.toml");
    assert_eq!(app.pane().file(app.main.cursor), Some(declaration.clone()));
    assert_eq!(app.node_file(), Some(declaration.clone()));
    // A drifted Worker's bar label carries the marker; `y` on the bar copies only its name.
    let text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{text}# edited\n")).unwrap();
    press(&mut app, &mut host, b"\t\r");
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("fixture/evaluator *"), "{bar}");
    host.sent.clear();
    press(&mut app, &mut host, b"y");
    assert_eq!(host.sent, b"\x1b]52;c;Zml4dHVyZS9ldmFsdWF0b3I=\x07");
    std::fs::write(&declaration, text).unwrap();
    press(&mut app, &mut host, b"\r\t");
    // At the 80x24 minimum the bar hides and the pane starts at the first column.
    let small = app.frame(80, 24).text();
    assert!(small.starts_with("WORKER  fixture/evaluator\n"), "{small}");
    assert!(small.lines().all(|line| line.len() <= 80), "{small}");
}

/// The `:` lines the browser owns keep their behaviour; a line without a subcommand is refused;
/// any other command line runs as a child of this executable in the scope's root.
#[test]
fn command_lines_route_to_the_browser_a_refusal_or_a_hand_off() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    for (line, said) in [
        (
            &b":--repo .\r"[..],
            "af --repo . would open a second browser",
        ),
        (b":cd nowhere\r", "nowhere: not a directory"),
        (
            b":scope elsewhere\r",
            "the scope is user or project, not elsewhere",
        ),
        (b":frobnicate\r", "unrecognized subcommand 'frobnicate'"),
    ] {
        press(&mut app, &mut host, line);
        let status = status_line(&mut app);
        assert!(status.contains(said), "{status}");
    }
    press(&mut app, &mut host, b":help task\r");
    assert!(
        app.main_rows()
            .iter()
            .any(|row| row.text().contains("af task"))
    );
    press(&mut app, &mut host, b"q");
    assert!(host.events.is_empty(), "{:?}", host.events);

    press(&mut app, &mut host, b":task show pagination-cli\r");
    let line = "af task show pagination-cli";
    assert_eq!(
        host.events,
        [
            "release".to_owned(),
            "run task show pagination-cli".to_owned(),
            format!("pause {line}: exit 0 -- Enter returns to the browser"),
            "reenter".to_owned(),
        ]
    );
    let child = &host.runs[0];
    assert_eq!(child.program, std::env::current_exe().unwrap());
    assert_eq!(child.args, ["task", "show", "pagination-cli"]);
    assert_eq!(child.dir, root.join("hub"), "the repository toplevel");
    assert!(
        child
            .env
            .iter()
            .any(|(name, _)| name == "AF_DISPATCHED_FROM"),
        "{:?}",
        child.env
    );
    assert_eq!(app.message, Some((format!("{line}: exit 0"), false)));

    // A command that fails, or that a signal ends, is an error on the status line.
    host.events.clear();
    host.exit = Some(Exit::Code(2));
    press(&mut app, &mut host, b":task show nothing\r");
    let failed = Some(("af task show nothing: exit 2".to_owned(), true));
    assert_eq!(app.message, failed);
    host.exit = Some(Exit::Signal(2));
    press(&mut app, &mut host, b":task show nothing\r");
    let killed = "af task show nothing: killed by SIGINT";
    assert_eq!(app.message, Some((killed.to_owned(), true)));
    // `pause` itself starts the line fresh, whatever the command left.
    let pause = format!("pause {killed} -- Enter returns to the browser");
    assert!(host.events.contains(&pause), "{:?}", host.events);

    // A child that cannot be spawned: the error, and the terminal re-entered, without a wait.
    host.events.clear();
    host.unspawnable = Some("no such file".to_owned());
    press(&mut app, &mut host, b":task list\r");
    assert_eq!(host.events, ["release", "run task list", "reenter"]);
    let error = Some(("af task list: no such file".to_owned(), true));
    assert_eq!(app.message, error);
    assert_eq!(app.mode, Mode::Normal);

    press(&mut app, &mut host, b":q\r");
    assert!(app.quit);
}

/// A refusal naming a long path keeps its reason on the status line at the 80-column minimum:
/// the path is shortened from the left, its final component kept; a short path is unchanged.
#[test]
fn a_long_path_on_the_status_line_keeps_the_reason_visible() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    let long = root
        .join("a-directory-with-a-rather-long-name/".repeat(4))
        .join("nowhere");
    press(
        &mut app,
        &mut host,
        format!(":cd {}\r", long.display()).as_bytes(),
    );
    let full = format!("{}: not a directory", long.display());
    assert_eq!(app.message, Some((full.clone(), true)));
    assert!(full.len() > 80, "{full}");
    let frame = app.frame(80, 24).text();
    let status = frame.lines().last().unwrap();
    assert!(status.len() <= 80, "{status}");
    let shortened = ".../a-directory-with-a-rather-long-name/nowhere: not a directory";
    assert!(status.ends_with(shortened), "{status}");

    press(&mut app, &mut host, b":cd /nowhere\r");
    assert_eq!(
        app.message,
        Some(("/nowhere: not a directory".to_owned(), true))
    );
    let frame = app.frame(80, 24).text();
    let status = frame.lines().last().unwrap();
    assert!(status.len() <= 80, "{status}");
    assert!(status.ends_with("  /nowhere: not a directory"), "{status}");
    assert!(!status.contains("..."), "{status}");
}

/// Released before the child, re-entered after the wait; then the panes the child may have
/// changed are read again, keeping what is opened and the bar's selection.
#[test]
fn a_hand_off_releases_first_reenters_after_and_reads_the_panes_again() {
    let (_temp, root) = temp_root();
    let repo = hub_repo(&root);
    let mut app = app_at(&root, repo.clone());
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]j\r");
    assert_eq!(app.breadcrumb(), "pipelines/review");
    press(&mut app, &mut host, b"gg");
    assert!(app.panes.tasks.task_ids().is_empty());
    let recorded = |app: &App| {
        let rows = app.tree.rows();
        rows.iter().any(|row| row.id == "pagination-cli")
    };
    assert!(!recorded(&app));
    // The child records a Task and edits the opened pipeline.
    let state = crate::task_execution::default_task_state(&root.join("state"), &repo).unwrap();
    let (at, hub) = (root.clone(), repo.clone());
    host.child = Some(Box::new(move || {
        record_task(&at, &hub, &state, "pagination-cli");
        let file = hub.join(".af/pipelines/review.toml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}# edited\n")).unwrap();
    }));
    press(
        &mut app,
        &mut host,
        b":task start --file ticket.json --execute\r",
    );
    assert_eq!(host.events[0], "release");
    assert!(host.events[1].starts_with("run task start"));
    assert!(host.events[2].starts_with("pause "));
    assert_eq!(host.events[3], "reenter");
    // The Tasks pane was never opened, and lists the Task now.
    assert_eq!(app.panes.tasks.task_ids(), ["pagination-cli"]);
    assert!(recorded(&app));
    // The opened pipeline reads again and still shows; the bar keeps the root selected.
    let package = ".af/pipelines/review.toml".to_owned();
    assert_eq!(app.opened, Opened::Item(Tab::Pipelines, package));
    let rows: Vec<String> = app.pane().rows().iter().map(Row::text).collect();
    assert!(
        rows[0].starts_with("working tree differs from HEAD"),
        "{rows:#?}"
    );
    assert_eq!(app.breadcrumb(), "hub");
    let status = status_line(&mut app);
    assert!(
        status.ends_with("af task start --file ticket.json --execute: exit 0"),
        "{status}"
    );
}

/// The Workers pane reads nothing until first opened (ADR-0122). Once it has read, a hand-off
/// re-reads it whether it is opened or not; until then it stays unread, and its first open
/// reads fresh.
#[test]
fn a_hand_off_rereads_a_workers_pane_that_has_read_even_when_not_opened() {
    let (_temp, root) = temp_root();
    let repo = hub_repo(&root);
    let mut app = app_at(&root, repo.clone());
    let mut host = Recorder::default();
    let declaration = repo.join(".af/task-packages/fixture/implementer/worker.toml");
    let edit = move || {
        let text = std::fs::read_to_string(&declaration).unwrap();
        std::fs::write(&declaration, format!("{text}# edited\n")).unwrap();
    };
    // Never opened: a hand-off leaves it unread.
    host.child = Some(Box::new(edit.clone()));
    press(&mut app, &mut host, b":task list\r");
    assert!(app.panes.workers.items().is_empty(), "the scan stays lazy");
    // Opened once, then another pane opened in its place.
    press(&mut app, &mut host, b"]]]]\r");
    assert_eq!(app.breadcrumb(), "workers/");
    let label = |app: &App| -> Vec<String> {
        app.panes
            .workers
            .items()
            .into_iter()
            .map(|item| item.label)
            .collect()
    };
    assert_eq!(label(&app), ["fixture/evaluator", "fixture/implementer *"]);
    press(&mut app, &mut host, b"gg\r");
    assert_eq!(app.opened, Opened::Root);
    // The child restores the declaration; the unopened Workers pane follows.
    host.child = Some(Box::new(move || git(&repo, &["checkout", "--", "."])));
    press(&mut app, &mut host, b":task list\r");
    assert_eq!(app.opened, Opened::Root);
    assert_eq!(label(&app), ["fixture/evaluator", "fixture/implementer"]);
}

/// The user scope lists every repository's Tasks, but a handed-off command runs from home and
/// names a Task only by id: `r` and `D` say where to press them, and completion offers no id.
#[test]
fn a_user_scope_task_is_neither_prefilled_nor_completed() {
    let (_temp, root) = temp_root();
    let (_repo, _state) = hub_with_tasks(&root);
    let mut scope = Scope::user().unwrap();
    scope.home = Some(root.clone());
    scope.state = Some(root.join("state"));
    let providers = ProvidersPane::with_inventory(inventory(), UsageProbe::Probe);
    let mut app = App::new(scope, None, Panes::new(providers));
    app.frame(100, 30);
    fn leaves(items: Vec<crate::tui::tree::Item>, found: &mut Vec<String>) {
        for item in items {
            match item.children {
                Some(children) => leaves(children, found),
                None => found.push(item.id),
            }
        }
    }
    let mut ids = Vec::new();
    leaves(app.panes.tasks.items(), &mut ids);
    // Verified or not, the guidance comes first: no line from here would reach the Task.
    for task in ["/pagination-cli", "/pagination-unfinished"] {
        let id = ids.iter().find(|id| id.ends_with(task)).unwrap();
        for key in ['r', 'D'] {
            let refused = app.panes.tasks.bar_verb(id, Key::Char(key)).unwrap();
            let why = refused.unwrap_err();
            assert!(
                why.contains("user scope") && why.contains(&format!("press {key} there")),
                "{why}"
            );
        }
    }
    assert!(app.panes.tasks.task_ids().is_empty());
}

/// A release that fails may have left the screen half released: the browser takes it back
/// before it reports the failure, and runs nothing.
#[test]
fn a_failed_release_reenters_before_it_is_reported() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder {
        unreleasable: Some("restoring the terminal: Input/output error".to_owned()),
        ..Recorder::default()
    };
    press(&mut app, &mut host, b":task list\r");
    assert_eq!(host.events, ["release", "reenter"]);
    assert!(host.runs.is_empty());
    let (message, error) = app.message.clone().unwrap();
    assert!(error, "{message}");
    assert_eq!(
        message,
        "af task list: restoring the terminal: Input/output error"
    );
}

/// An editor is handed the terminal as a command is, through `Host::run` in its own process
/// group, so `<C-c>` reaches the editor, never the browser; there is no Enter wait after it.
#[test]
fn an_editor_runs_in_its_own_group_like_a_command() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    app.editor = Some("vim -n".to_owned());
    let mut host = Recorder::default();
    press(&mut app, &mut host, b"]]]]]]jgf");
    let file = root.join("hub/.af/pipelines/review.toml");
    assert_eq!(host.runs.len(), 1);
    assert_eq!(host.runs[0].program, PathBuf::from("vim"));
    assert_eq!(
        host.runs[0].args,
        ["-n".to_owned(), file.display().to_string()]
    );
    assert_eq!(host.events.len(), 3, "{:?}", host.events);
    assert_eq!(host.events[0], "release");
    assert!(host.events[1].starts_with("run -n "));
    assert_eq!(host.events[2], "reenter");
    // An editor that fails says so, and the browser is back all the same.
    host.exit = Some(Exit::Code(1));
    press(&mut app, &mut host, b"gf");
    assert_eq!(host.events.last().map(String::as_str), Some("reenter"));
    let (message, error) = app.message.clone().unwrap();
    assert!(error && message.contains("vim exit 1"), "{message}");
    // No `$EDITOR`: nothing is released, and the fix is named.
    app.editor = None;
    let before = host.events.len();
    press(&mut app, &mut host, b"gf");
    assert_eq!(host.events.len(), before);
    let (message, _) = app.message.clone().unwrap();
    assert!(message.contains("EDITOR is not set"), "{message}");
}

/// A terminal the browser cannot take back after a command: it cannot paint, so it ends, and
/// says why once the terminal is the shell's again. Nothing is read or painted after it.
#[test]
fn a_terminal_that_cannot_be_retaken_ends_the_browser() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder {
        unreenterable: Some("the terminal was left to the shell".to_owned()),
        ..Recorder::default()
    };
    press(&mut app, &mut host, b":task list\r");
    assert!(app.quit);
    assert_eq!(
        app.fatal.as_deref(),
        Some("af task list: the terminal was left to the shell")
    );
}

/// A typed `:config edit` runs where the browser is: after `:cd` into another repository, the
/// project layer is that repository's, not the one `af` started in; a relative `--repo` is
/// from there too.
#[test]
fn a_typed_config_edit_follows_the_browsers_scope() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    app.editor = Some("true".to_owned());
    let other = root.join("other");
    std::fs::create_dir_all(other.join("nested")).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    let mut host = Recorder::default();
    press(
        &mut app,
        &mut host,
        format!(":cd {}\r", other.display()).as_bytes(),
    );
    assert_eq!(app.scope.root, other);
    press(&mut app, &mut host, b":config edit --layer project\r");
    let edited = other.join(".af/af.toml").display().to_string();
    assert_eq!(host.runs.last().unwrap().args.last(), Some(&edited));
    press(
        &mut app,
        &mut host,
        b":config edit --layer local --repo nested\r",
    );
    let local = other.join(".af/af.local.toml").display().to_string();
    assert_eq!(host.runs.last().unwrap().args.last(), Some(&local));
}

/// `<Tab>` completes subcommand names at every level from the clap definition itself.
#[test]
fn completion_comes_from_the_clap_definition() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    let names = |path: &[&str]| -> Vec<String> {
        let mut command = cli::Af::command();
        for word in path {
            command = command.find_subcommand(word).unwrap().clone();
        }
        let visible = command.get_subcommands().filter(|sub| !sub.is_hide_set());
        visible.map(|sub| sub.get_name().to_owned()).collect()
    };
    // The browser's verbs, then every top-level command.
    press(&mut app, &mut host, b":\t");
    let mut all = vec!["cd", "e", "help", "q", "scope"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for name in names(&[]) {
        if !all.contains(&name) {
            all.push(name);
        }
    }
    assert_eq!(app.message, Some((all.join("  "), false)));
    press(&mut app, &mut host, b"\x1b:ta\t");
    assert_eq!(app.prompt.text, "task ");
    press(&mut app, &mut host, b"\t");
    assert_eq!(app.message, Some((names(&["task"]).join("  "), false)));
    press(&mut app, &mut host, b"sh\t");
    assert_eq!(app.prompt.text, "task show ");
    for (typed, completed) in [
        (&b":provider st\t"[..], "provider status "),
        (b":help ta\t", "help task "),
        (b":sc\t", "scope "),
        (b":scope u\t", "scope user "),
        (b":e lo\t", "e local "),
    ] {
        press(&mut app, &mut host, b"\x1b");
        press(&mut app, &mut host, typed);
        assert_eq!(app.prompt.text, completed);
    }
    // Past a flag, or a word that is no subcommand, nothing is guessed.
    press(&mut app, &mut host, b"\x1b:task show --json \t");
    assert_eq!(app.prompt.text, "task show --json ");
    press(&mut app, &mut host, b"\x1b:frob \t");
    assert_eq!(app.prompt.text, "frob ");
    assert!(host.events.is_empty());
}

/// `d` on a registered Provider, from the bar and from its opened pane, prefills the `:` line
/// with `provider remove ID` and never submits it. An ambient candidate has no registry entry,
/// and a row that is no Provider has nothing to remove.
#[test]
fn the_providers_pane_prefills_remove_for_a_registered_provider() {
    let (_temp, root) = temp_root();
    let mut app = hub_app(&root);
    let mut host = Recorder::default();
    let line = "provider remove claude-main";
    press(&mut app, &mut host, b"]]j");
    assert_eq!(app.breadcrumb(), "providers/claude-main");
    press(&mut app, &mut host, b"d");
    assert_eq!(app.mode, Mode::Command);
    assert_eq!(app.prompt.text, line);
    assert_eq!(app.prompt.cursor, line.len());
    press(&mut app, &mut host, b"\x1b");
    // Opened, with the main pane focused: the same line, and the legend names the verb.
    press(&mut app, &mut host, b"\r\t");
    let status = status_line(&mut app);
    assert!(status.contains("d remove"), "{status}");
    press(&mut app, &mut host, b"d");
    assert_eq!(app.prompt.text, line);
    press(&mut app, &mut host, b"\x1b");
    assert!(host.events.is_empty(), "nothing was submitted");
    // Enter on the prefilled line is what runs it, as typed.
    press(&mut app, &mut host, b"d\r");
    assert_eq!(host.runs[0].args, ["provider", "remove", "claude-main"]);

    // An ambient candidate is discovered, not registered: no line is offered, from the bar or
    // from its opened pane.
    let ambient = "codex-ambient is discovered, not registered: there is no registry entry to \
                   remove";
    press(&mut app, &mut host, b"\tj");
    assert_eq!(app.breadcrumb(), "providers/codex-ambient");
    for keys in [&b"d"[..], b"\r\td"] {
        press(&mut app, &mut host, keys);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.message, Some((ambient.to_owned(), true)));
    }

    // The folder opened, and the bar on a row that is no Provider while one stays opened:
    // `d` acts on nothing.
    let nothing = "d removes a Provider: select one in the bar, or open it";
    press(&mut app, &mut host, b"\tk\rgg");
    assert!(matches!(app.opened, Opened::Item(Tab::Providers, _)));
    assert_eq!(app.focus, Focus::Bar);
    press(&mut app, &mut host, b"d");
    assert_eq!(app.message, Some((nothing.to_owned(), true)));
    press(&mut app, &mut host, b"]]\r\t");
    assert_eq!(app.opened, Opened::Folder(Tab::Providers));
    press(&mut app, &mut host, b"d");
    assert_eq!(app.message, Some((nothing.to_owned(), true)));
    assert_eq!(host.runs.len(), 1, "only the submitted line ran");
}

/// `r` and `D` from the bar and from the opened Task prefill the `:` line and never submit it;
/// `D` refuses a Task that is not verified. A Task ID completes from the Tasks pane.
#[test]
fn the_tasks_pane_prefills_run_and_deliver_and_completes_task_ids() {
    let (_temp, root) = temp_root();
    let (repo, state) = hub_with_tasks(&root);
    let mut app = app_at(&root, repo);
    let mut host = Recorder::default();
    let plan = |task_id: &str| {
        let shown = crate::task_execution::inspection_document(&state, task_id, false);
        shown.unwrap().unwrap()["plan_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let run = format!(
        "task run pagination-cli --confirm-plan {}",
        plan("pagination-cli")
    );
    let deliver = "task deliver pagination-cli --branch af/pagination-cli --worktree ../pagination-cli --confirm ";
    press(&mut app, &mut host, b"]]]]]]]]jj");
    assert_eq!(app.breadcrumb(), "tasks/pagination-cli");
    for (key, line) in [(&b"r"[..], run.as_str()), (b"D", deliver)] {
        press(&mut app, &mut host, key);
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.prompt.text, line);
        assert_eq!(app.prompt.cursor, line.len());
        press(&mut app, &mut host, b"\x1b");
    }
    // Opened, with the main pane focused: the same lines, and the legend names the verbs.
    press(&mut app, &mut host, b"\r\t");
    let status = status_line(&mut app);
    assert!(status.contains("r run  D deliver"), "{status}");
    for (key, line) in [(&b"r"[..], run.as_str()), (b"D", deliver)] {
        press(&mut app, &mut host, key);
        assert_eq!(app.prompt.text, line);
        press(&mut app, &mut host, b"\x1b");
    }
    assert!(host.events.is_empty(), "nothing was submitted");
    // Enter on the prefilled line is what runs it, as typed.
    press(&mut app, &mut host, b"r\r");
    let words: Vec<String> = shell_words::split(&run).unwrap();
    assert_eq!(host.runs[0].args, words);

    // A Task that failed its acceptance is not verified: `D` says why, `r` still prefills.
    press(&mut app, &mut host, b"\t");
    assert!(
        app.tree
            .select(NodeKind::Item(Tab::Tasks), "pagination-unfinished")
    );
    press(&mut app, &mut host, b"D");
    assert_eq!(app.mode, Mode::Normal);
    let refused = "only a verified Task can be delivered: pagination-unfinished finished without \
                   satisfying its acceptance";
    assert_eq!(app.message, Some((refused.to_owned(), true)));
    press(&mut app, &mut host, b"r");
    let rerun = format!(
        "task run pagination-unfinished --confirm-plan {}",
        plan("pagination-unfinished")
    );
    assert_eq!(app.prompt.text, rerun);
    press(&mut app, &mut host, b"\x1b");

    // Task IDs complete from what the pane lists for this scope.
    press(&mut app, &mut host, b":task show pag\t");
    let both = "pagination-cli  pagination-unfinished".to_owned();
    assert_eq!(app.message, Some((both, false)));
    press(&mut app, &mut host, b"ination-c\t");
    assert_eq!(app.prompt.text, "task show pagination-cli ");
    for verb in ["run", "explain", "deliver"] {
        press(&mut app, &mut host, b"\x1b");
        press(
            &mut app,
            &mut host,
            format!(":task {verb} pagination-u\t").as_bytes(),
        );
        assert_eq!(
            app.prompt.text,
            format!("task {verb} pagination-unfinished ")
        );
    }
    // Options the command declares may come before the ID, with their values.
    for typed in [
        "task show --json pagination-u",
        "task show --repo . pagination-u",
        "task show --repo=. pagination-u",
        "task show --repo 'a repo with spaces' pagination-u",
        "task show -- pagination-u",
    ] {
        press(&mut app, &mut host, b"\x1b");
        press(&mut app, &mut host, format!(":{typed}\t").as_bytes());
        let (head, _) = typed.rsplit_once(' ').unwrap();
        assert_eq!(app.prompt.text, format!("{head} pagination-unfinished "));
    }
    // An option waiting for its value, or an ID already given, completes no Task ID.
    for typed in [
        "task show --repo pagination-u",
        "task show pagination-cli pagination-u",
        "task show --not-a-real-option=value pagination-u",
        "task show --json=yes pagination-u",
        "task show --repo --json pagination-u",
        "task show -- pagination-cli pagination-u",
    ] {
        press(&mut app, &mut host, b"\x1b");
        press(&mut app, &mut host, format!(":{typed}\t").as_bytes());
        assert_eq!(app.prompt.text, typed);
    }
    press(&mut app, &mut host, b"\x1b");

    // With the bar on a row that is no Task, `r` and `D` act on nothing, not on the Task
    // opened in the main pane.
    assert!(matches!(app.opened, Opened::Item(Tab::Tasks, _)));
    assert_eq!(app.focus, Focus::Bar);
    press(&mut app, &mut host, b"gg");
    for key in [&b"r"[..], b"D"] {
        press(&mut app, &mut host, key);
        assert_eq!(app.mode, Mode::Normal);
        let (message, error) = app.message.clone().unwrap();
        assert!(
            error && message.starts_with("r and D act on a Task"),
            "{message}"
        );
    }
}

#[test]
fn bare_help_opens_with_the_worker_and_the_tagline() {
    let text: Vec<String> = help_rows(&[]).iter().map(Row::text).collect();
    assert_eq!(text[0], "     ###");
    assert!(
        text[1].ends_with(&format!("af {}", env!("CARGO_PKG_VERSION"))),
        "{}",
        text[1]
    );
    assert!(
        text[2].ends_with("agent pipelines made fast"),
        "{}",
        text[2]
    );
    assert_eq!(text[5], " ###########");
    assert_eq!(text[6], "");
    assert!(text[7].starts_with("KEYS"), "{}", text[7]);
    assert!(
        text[..7].iter().all(|row| row.len() <= 52),
        "the banner fits the pane at 80 columns"
    );
    let topic: Vec<String> = help_rows(&["layers".to_owned()])
        .iter()
        .map(Row::text)
        .collect();
    assert!(topic[0].starts_with("af help layers -- "), "{}", topic[0]);
}
