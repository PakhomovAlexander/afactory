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

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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
fn hub_scope(root: &Path, repo: &Path) -> Scope {
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
    let scope = hub_scope(root, &repo);
    let providers = ProvidersPane::with_inventory(inventory(), UsageProbe::Probe);
    let mut app = App::new(scope, Some(repo), Panes::new(providers));
    app.frame(100, 30);
    app
}

/// A terminal that records what the browser sends it and never runs a child.
#[derive(Default)]
struct Recorder {
    sent: Vec<u8>,
    released: usize,
}

impl Host for Recorder {
    fn release(&mut self) -> Result<(), String> {
        self.released += 1;
        Ok(())
    }

    fn reenter(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.sent.extend_from_slice(bytes);
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
    let again = crate::task_execution::plan_tree_preview(&file, &root.join("hub")).unwrap();
    let shown_text = shown.join("\n");
    assert_eq!(stable_lines(&shown_text), stable_lines(&again.text));
    // The frame paints those rows beside the bar, clipped to the main pane.
    let frame = app.frame(100, 30).text();
    for (line, row) in frame.lines().zip(&shown) {
        let clipped = &row[..row.len().min(72)];
        let main = line.get(28..).unwrap_or("");
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
    assert_eq!(app.pane().status(slot + 1), Some(binding));
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
