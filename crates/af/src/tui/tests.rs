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
fn record_task(root: &Path, repo: &Path, state: &Path, task_id: &str) {
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
    let again =
        crate::task_execution::plan_tree_preview_at(&file, &root.join("hub"), "HEAD").unwrap();
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
            let taken = if pair(word, next) {
                2
            } else if single(word) {
                1
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
    let refusal = format!("~/{shown}: this Store cannot be read: ");
    let row = rows.iter().find(|row| row.starts_with(&refusal));
    assert!(
        row.is_some_and(|row| row.len() > refusal.len()),
        "{rows:#?}"
    );
    let bar = app.frame(100, 30).text();
    assert!(bar.contains("      ! Store unreadable"), "{bar}");
    // The row opens the same refusal, never an empty list.
    press(&mut app, &mut host, b"j\r");
    assert_eq!(app.breadcrumb(), "tasks/!unreadable");
    let rows: Vec<String> = app.main_rows().iter().map(Row::text).collect();
    assert!(
        rows.iter().any(|row| row.starts_with(&refusal)),
        "{rows:#?}"
    );
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
    // A running Task whose pane is not opened is not read either.
    app.panes.tasks.detail_document().unwrap()["phase"] = running;
    press(&mut app, &mut host, b"gg]]\r");
    assert_eq!(app.opened, Opened::Folder(Tab::Providers));
    std::thread::sleep(second);
    app.poll();
    assert_eq!(app.panes.tasks.reads(), 1);
}
