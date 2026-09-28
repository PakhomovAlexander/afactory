//! Bare `af`: help and exit 2 on a pipe, the browser on a terminal. The browser runs in a real
//! pseudo-terminal at 100x30 and is driven key by key; its Pipelines pane is compared line for
//! line with what `af task plan` and `af task explain --tree` print for the same Task file, and
//! its Tasks and Workers panes with what `af task show --json` records.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const AF: &str = env!("CARGO_BIN_EXE_af");
const ROWS: usize = 30;
const COLS: usize = 100;

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

/// `fixtures/consumers/hub` committed at `root/hub`, over the pinned packages of
/// `fixtures/task-runtime/pagination` (see the hub's README).
fn hub(root: &Path) -> PathBuf {
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

fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    (temp, root)
}

/// The machine a test's `af` sees: its own home and XDG directories, no update checks.
fn environment(home: &Path) -> Vec<(&'static str, std::ffi::OsString)> {
    let mut environment = vec![
        ("HOME", home.as_os_str().to_owned()),
        ("XDG_CONFIG_HOME", home.join("config").into_os_string()),
        ("XDG_STATE_HOME", home.join("state").into_os_string()),
        ("XDG_DATA_HOME", home.join("data").into_os_string()),
        ("XDG_CACHE_HOME", home.join("cache").into_os_string()),
        ("AF_SELF_OFFLINE", "1".into()),
        ("NO_COLOR", "1".into()),
        ("TERM", "xterm-256color".into()),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        environment.push(("PATH", path));
    }
    environment
}

fn af(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(AF);
    command.current_dir(cwd).env_clear().args(args);
    for (name, value) in environment(home) {
        command.env(name, value);
    }
    command.output().unwrap()
}

/// The Task file the Pipelines pane plans for `fixture/implementation`; it is the one
/// `tui::panes::pipelines::preview_task` writes, and must stay equal to it.
fn preview_task() -> serde_json::Value {
    serde_json::json!({
        "schema": "af.task-file/1",
        "task_id": "pipeline-preview",
        "kind": "implement",
        "goal": "Preview the fixture/implementation Pipeline",
        "pipeline": {"name": "fixture/implementation", "fallback": "refuse"},
        "strategy": "preview",
        "facts": {},
        "limits": {
            "tokens": 1_000_000,
            "max_attempts": 3,
            "wall_ms": 3_600_000,
            "verification": {"tokens": 100_000, "attempts": 2, "wall_ms": 600_000}
        }
    })
}

/// Lines two captures of one Task file share: the plan ID covers the deadline and TIME counts
/// down to it, so those lines keep only their label.
fn stable_lines(text: &str) -> Vec<String> {
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

/// The screen the browser painted. Only what it writes is interpreted: cursor position, erase,
/// the alternate screen, autowrap, and a line feed that scrolls at the bottom; graphic rendition
/// and other private modes are ignored. A command the browser hands the terminal to writes on
/// the main screen, as it would on a real terminal.
struct Screen {
    cells: Vec<Vec<char>>,
    rows: usize,
    columns: usize,
    row: usize,
    column: usize,
    wrap: bool,
    /// The main screen and its cursor while the alternate screen shows.
    main: Option<(Vec<Vec<char>>, usize, usize)>,
}

impl Screen {
    fn parse(bytes: &[u8], rows: usize, columns: usize) -> Screen {
        let mut screen = Screen {
            cells: vec![vec![' '; columns]; rows],
            rows,
            columns,
            row: 0,
            column: 0,
            wrap: true,
            main: None,
        };
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            match byte {
                0x1b => index = screen.escape(bytes, index),
                b'\r' => screen.column = 0,
                b'\n' => screen.line_feed(),
                0x20..=0x7e => {
                    if screen.column >= screen.columns && screen.wrap {
                        screen.column = 0;
                        screen.line_feed();
                    }
                    if screen.row < screen.rows && screen.column < screen.columns {
                        screen.cells[screen.row][screen.column] = char::from(byte);
                    }
                    screen.column += 1;
                }
                _ => {}
            }
        }
        screen
    }

    fn line_feed(&mut self) {
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.cells.remove(0);
            self.cells.push(vec![' '; self.columns]);
        }
    }

    /// `CSI ? 1049 h` and `l`: the alternate screen, entered cleared and left for the main one
    /// as it was; `CSI ? 7 h` and `l`: autowrap.
    fn private_mode(&mut self, parameters: &str, set: bool) {
        match (parameters, set) {
            ("?7", _) => self.wrap = set,
            ("?1049", true) => {
                let blank = vec![vec![' '; self.columns]; self.rows];
                let main = std::mem::replace(&mut self.cells, blank);
                self.main = Some((main, self.row, self.column));
            }
            ("?1049", false) => {
                if let Some((main, row, column)) = self.main.take() {
                    (self.cells, self.row, self.column) = (main, row, column);
                }
            }
            _ => {}
        }
    }

    /// One escape sequence after its `ESC`; returns the index after it.
    fn escape(&mut self, bytes: &[u8], start: usize) -> usize {
        match bytes.get(start) {
            Some(b'[') => {
                let mut end = start + 1;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let Some(last) = bytes.get(end) else {
                    return bytes.len();
                };
                let parameters = String::from_utf8_lossy(&bytes[start + 1..end]).into_owned();
                match last {
                    b'H' => {
                        let mut numbers = parameters.split(';');
                        let row = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                        let column = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                        self.row = usize::max(row, 1) - 1;
                        self.column = usize::max(column, 1) - 1;
                    }
                    b'J' if parameters == "2" => {
                        self.cells = vec![vec![' '; self.columns]; self.rows];
                    }
                    b'h' | b'l' => self.private_mode(&parameters, *last == b'h'),
                    _ => {}
                }
                end + 1
            }
            Some(b']') => {
                let mut end = start + 1;
                while end < bytes.len() && bytes[end] != 0x07 {
                    end += 1;
                }
                end + 1
            }
            Some(_) => start + 1,
            None => start,
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for cells in &self.cells {
            let line: String = cells.iter().collect();
            lines.push(line.trim_end().to_owned());
        }
        lines
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }
}

/// `af` with no arguments on the slave side of a pseudo-terminal.
struct Browser {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    rows: usize,
    columns: usize,
    _master: Box<dyn MasterPty + Send>,
}

impl Browser {
    fn launch(cwd: &Path, home: &Path) -> Browser {
        Browser::launch_sized(cwd, home, ROWS, COLS)
    }

    /// The browser on a terminal of `rows` x `columns`.
    fn launch_sized(cwd: &Path, home: &Path, rows: usize, columns: usize) -> Browser {
        Browser::launch_with(cwd, home, rows, columns, &[])
    }

    /// A browser whose environment also carries `extra`.
    fn launch_with(
        cwd: &Path,
        home: &Path,
        rows: usize,
        columns: usize,
        extra: &[(&str, &str)],
    ) -> Browser {
        let size = PtySize {
            rows: rows as u16,
            cols: columns as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size).unwrap();
        let mut command = CommandBuilder::new(AF);
        command.env_clear();
        command.cwd(cwd);
        for (name, value) in environment(home) {
            command.env(name, value);
        }
        for (name, value) in extra {
            command.env(name, value);
        }
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut chunk) {
                if count == 0 || sender.send(chunk[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Browser {
            child,
            writer,
            output,
            bytes: Vec::new(),
            rows,
            columns,
            _master: pair.master,
        }
    }

    /// The screen once `ready` holds for it; a timeout shows the last screen.
    fn wait_for(&mut self, what: &str, ready: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            while let Ok(chunk) = self.output.try_recv() {
                self.bytes.extend(chunk);
            }
            let screen = Screen::parse(&self.bytes, self.rows, self.columns);
            if ready(&screen) {
                return screen;
            }
            let shown = screen.text();
            assert!(Instant::now() < deadline, "no {what} on:\n{shown}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Type keys; each batch gets its own read by the browser.
    fn keys(&mut self, keys: &[u8]) {
        self.writer.write_all(keys).unwrap();
        self.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    }

    fn exit_code(mut self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("the browser did not exit");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn bar_rows(screen: &Screen) -> Vec<String> {
    let mut rows = Vec::new();
    for line in screen.lines() {
        let bar: String = line.chars().take(27).collect();
        rows.push(bar.trim_end().to_owned());
    }
    rows
}

fn shows(screen: &Screen, prefix: &str) -> bool {
    screen.lines().iter().any(|line| line.starts_with(prefix))
}

/// The plan tree is painted: its first and its last line are on the screen.
fn tree_painted(screen: &Screen) -> bool {
    let first = shows(screen, "TASK  pipeline-preview");
    first && shows(screen, "Details: af task explain")
}

#[test]
fn bare_af_on_a_pipe_prints_help_and_exits_2() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let output = af(&root, &home, &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "help goes to stderr");
    let help = String::from_utf8_lossy(&output.stderr);
    assert!(help.contains("Usage: af"), "{help}");
    assert!(help.contains("review"), "{help}");
}

#[test]
fn bare_af_in_a_repository_opens_the_project_scope() {
    let (_temp, root) = temp_root();
    let repo = hub(&root);
    let mut browser = Browser::launch(&repo, &root.join("home"));
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    let screen = browser.wait_for("project settings", ready);
    let bar = bar_rows(&screen);
    let folders = [
        "  v providers/",
        "  v workers/",
        "  v pipelines/",
        "  v tasks/",
    ];
    for folder in folders {
        // A folder whose pane is still discovering carries a spinner after its name.
        let listed = bar.iter().any(|row| row.starts_with(folder));
        assert!(listed, "{}", screen.text());
    }
    assert!(screen.lines()[ROWS - 1].starts_with("NORMAL  hub"));
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

#[test]
fn bare_af_outside_a_repository_opens_the_user_scope() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let plain = home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut browser = Browser::launch(&plain, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  user: ~");
    let screen = browser.wait_for("user settings", ready);
    assert!(screen.text().contains("providers"), "{}", screen.text());
    browser.keys(b"q");
    assert_eq!(browser.exit_code(), 0);
}

#[test]
fn the_pipelines_pane_prints_what_task_plan_and_explain_tree_print() {
    let (_temp, root) = temp_root();
    let repo = hub(&root);
    let home = root.join("home");
    let task = root.join("preview.json");
    std::fs::write(&task, serde_json::to_vec_pretty(&preview_task()).unwrap()).unwrap();
    let state = root.join("state");
    let file = task.to_str().unwrap();
    let state = state.to_str().unwrap();
    let args = ["task", "plan", "--file", file, "--state", state];
    let planned = af(&repo, &home, &args);
    let stderr = String::from_utf8_lossy(&planned.stderr);
    assert!(planned.status.success(), "{stderr}");
    let args = [
        "task",
        "explain",
        "pipeline-preview",
        "--tree",
        "--state",
        state,
    ];
    let explained = af(&repo, &home, &args);
    assert!(explained.status.success());
    let expected = stable_lines(&String::from_utf8(explained.stdout).unwrap());

    let mut browser = Browser::launch(&repo, &home);
    let listed = |screen: &Screen| shows(screen, "  v pipelines/");
    browser.wait_for("the bar", listed);
    // ]] three times reaches pipelines/; its entries are review, then the package.
    browser.keys(b"]]]]]]");
    browser.keys(b"jj");
    browser.keys(b"\r");
    // <C-b> on the bar hides it, so the main pane is as wide as the CLI's preview.
    browser.keys(b"\x02");
    let screen = browser.wait_for("the plan tree", tree_painted);
    let lines = screen.lines();
    let shown = stable_lines(&lines[..expected.len()].join("\n"));
    assert_eq!(shown, expected, "{}", screen.text());
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// A time of day and a duration, spelled the way the Tasks pane spells them.
fn clock(unix_ms: u64) -> String {
    let seconds = unix_ms / 1_000 % 86_400;
    let (hours, minutes) = (seconds / 3_600, seconds / 60 % 60);
    format!("{hours:02}:{minutes:02}:{:02}Z", seconds % 60)
}

fn duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{}.{}s", ms / 1_000, ms % 1_000 / 100)
    } else if ms < 3_600_000 {
        format!("{}m {:02}s", ms / 60_000, ms / 1_000 % 60)
    } else {
        format!("{}h {:02}m", ms / 3_600_000, ms / 60_000 % 60)
    }
}

fn short(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(8).collect()
}

/// The hub with the pagination ticket run to its end by `af task start --execute`, into the
/// Task state `af task` resolves for the hub under the test's `XDG_STATE_HOME`.
fn hub_with_a_task(root: &Path, home: &Path) -> PathBuf {
    let repo = hub(root);
    let ticket = root.join("ticket.json");
    std::fs::copy(repo.join("ticket.json"), &ticket).unwrap();
    let file = ticket.to_str().unwrap();
    let args = ["task", "start", "--execute", "--file", file, "--json"];
    let started = af(&repo, home, &args);
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(started.status.success(), "{stderr}");
    repo
}

/// Every number the Tasks pane shows for a Task equals the same field of the document
/// `af task show --json` prints for it.
#[test]
fn the_tasks_pane_shows_the_numbers_af_task_show_json_records() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub_with_a_task(&root, &home);
    let shown = af(&repo, &home, &["task", "show", "pagination-cli", "--json"]);
    assert!(shown.status.success());
    let show: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let mut browser = Browser::launch(&repo, &home);
    browser.wait_for("the bar", |screen| shows(screen, "  v tasks/"));
    // ]] four times reaches tasks/; its first group is done/, and in it the Task.
    browser.keys(b"]]]]]]]]");
    browser.keys(b"jj");
    browser.keys(b"\r");
    let opened = |screen: &Screen| {
        let lines = screen.lines();
        lines
            .iter()
            .any(|line| line.get(28..) == Some("HISTORY  (af task show)"))
    };
    let screen = browser.wait_for("the Task", opened);
    let main: Vec<String> = screen
        .lines()
        .iter()
        .map(|line| line.get(28..).unwrap_or("").to_owned())
        .collect();
    let text = screen.text();
    let line = |prefix: &str| {
        let found = main.iter().find(|line| line.starts_with(prefix));
        found
            .unwrap_or_else(|| panic!("no {prefix} line on:\n{text}"))
            .clone()
    };

    let history = show["history"].as_array().unwrap();
    let time = |event: &serde_json::Value| event["transition"]["now_unix_ms"].as_u64().unwrap();
    // A finished Task's elapsed time ends at its `finished` transition; later lease events
    // are history, not run time.
    let finished = history
        .iter()
        .find(|event| event["transition"]["change"]["kind"] == "finished")
        .unwrap();
    let (first, last) = (time(&history[0]), time(finished));
    let plan = short(show["plan_id"].as_str().unwrap());
    let elapsed = duration(last - first);
    let expected = format!(
        "PLAN  {plan}  configured  STATE done  started {}  elapsed {elapsed}",
        clock(first)
    );
    assert_eq!(line("PLAN "), expected);

    let report = &show["run_reports"].as_array().unwrap().last().unwrap()["report"];
    let nodes = report["nodes"].as_array().unwrap();
    let settled = nodes
        .iter()
        .filter(|node| {
            let kind = node["outcome"]["kind"].as_str();
            matches!(kind, Some("completed" | "failed" | "suppressed"))
        })
        .count();
    let expected = format!("PROGRESS  {settled} / {} stages", nodes.len());
    assert_eq!(line("PROGRESS "), expected);
    let percent = settled * 100 / nodes.len();
    let bar = bar_rows(&screen);
    let row = bar
        .iter()
        .find(|row| row.trim_start().starts_with("pagination"));
    assert!(row.unwrap().ends_with(&format!("  {percent}%")), "{text}");

    // The check's wall is its recorded Attempt wall, its tokens the charge it settled with.
    let walls = show["attempt_walls"].as_array().unwrap();
    let wall = walls
        .iter()
        .find(|wall| wall["node_id"] == "root.nodes.check")
        .unwrap();
    let records = show["execution_records"].as_array().unwrap();
    let settled_check = records
        .iter()
        .find(|entry| {
            entry["record"]["kind"] == "settled"
                && entry["record"]["attempt_id"] == wall["attempt_id"]
        })
        .unwrap();
    let charged = settled_check["record"]["charged_tokens"].as_str().unwrap();
    let check = line("  [ok]  check ");
    let wall_ms = wall["elapsed_ms"].as_u64().unwrap();
    let words: Vec<&str> = check.split_whitespace().collect();
    assert_eq!(
        words[2..],
        [duration(wall_ms).as_str(), charged, "tok"],
        "{check}"
    );

    // Chargeable is the document's; the components are `-`, because the Attempt walls that
    // carry them cover only some of the settled Attempts.
    let settled_attempts = records
        .iter()
        .filter(|entry| entry["record"]["kind"] == "settled")
        .count();
    assert!(walls.len() < settled_attempts);
    let chargeable = show["chargeable_tokens"].as_str().unwrap();
    let expected =
        format!("TOKENS  chargeable {chargeable}  input -  output -  cache read -  reasoning -");
    assert_eq!(line("TOKENS "), expected);

    let mut checks = 0;
    for observation in show["runtime_observations"].as_array().unwrap() {
        for span in observation["record"]["spans"].as_array().unwrap() {
            if span["kind"] == "check" {
                checks += span["elapsed_ms"].as_u64().unwrap();
            }
        }
    }
    let expected = format!(
        "TIME  wall {elapsed}  checks {}  dependency prep -",
        duration(checks)
    );
    assert_eq!(line("TIME "), expected);

    // Each HISTORY row is one event: its sequence, and the artifact its change names.
    let start = main
        .iter()
        .position(|line| line == "HISTORY  (af task show)")
        .unwrap();
    let rows = &main[start + 1..ROWS - 1];
    assert!(rows.len() > 10, "{text}");
    for (row, event) in rows.iter().zip(history) {
        let words: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(words[0], event["sequence"].as_u64().unwrap().to_string());
        let change = event["transition"]["change"].as_object().unwrap();
        assert_eq!(words[1], change["kind"].as_str().unwrap());
        let id = change
            .values()
            .filter_map(serde_json::Value::as_str)
            .find(|value| value.starts_with("sha256:"));
        assert_eq!(*words.last().unwrap(), id.map_or("-".to_owned(), short));
    }
    browser.keys(b"q");
    assert_eq!(browser.exit_code(), 0);
}

/// Outside a repository the bar lists every repository's Task state, one level each.
#[test]
fn the_user_scope_groups_tasks_by_repository() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    hub_with_a_task(&root, &home);
    let local = home.join("state/af/task/local");
    let mut entries = std::fs::read_dir(&local).unwrap();
    let repository = entries.next().unwrap().unwrap().file_name();
    let repository = repository.to_string_lossy().into_owned();
    let plain = home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut browser = Browser::launch(&plain, &home);
    let grouped = format!("    v {repository}/ (1)");
    let listed = |screen: &Screen| bar_rows(screen).contains(&grouped);
    let screen = browser.wait_for("the repository group", listed);
    let bar = bar_rows(&screen);
    let at = bar.iter().position(|row| *row == grouped).unwrap();
    assert_eq!(bar[at + 1], "      v done/ (1)", "{}", screen.text());
    assert_eq!(
        bar[at + 2],
        "          pagination~  100%",
        "{}",
        screen.text()
    );
    browser.keys(b"q");
    assert_eq!(browser.exit_code(), 0);
}

/// The Tasks pane at the 80x24 minimum on a real pseudo-terminal: the bar is hidden below 90
/// columns, the Task's rows fit the main pane, and the status line names the pane.
#[test]
fn the_tasks_pane_at_80x24_on_a_pseudo_terminal() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub_with_a_task(&root, &home);
    let mut browser = Browser::launch_sized(&repo, &home, 24, 80);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("the project settings at 80x24", ready);
    // The bar starts hidden at 80 columns; <C-b> shows it, and ]] four times reaches tasks/.
    browser.keys(b"\x02");
    browser.keys(b"]]]]]]]]");
    browser.keys(b"jj");
    browser.keys(b"\r");
    let opened = |screen: &Screen| screen.text().contains("TASK  pagination-cli");
    let screen = browser.wait_for("the Task at 80x24", opened);
    let lines = screen.lines();
    assert_eq!(lines.len(), 24);
    assert!(
        lines.iter().all(|line| line.len() <= 80),
        "{}",
        screen.text()
    );
    assert!(screen.text().contains("PROGRESS"), "{}", screen.text());
    assert!(lines[23].starts_with("NORMAL"), "{}", screen.text());
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// One recorded artifact, read from the Task state's content-addressed objects.
fn recorded(state: &Path, id: &str) -> serde_json::Value {
    let hex = id.trim_start_matches("sha256:");
    let path = state.join("cas/objects").join(&hex[..2]).join(&hex[2..]);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The STATE counts of `worker` as the `af task show --json` document of `show` records them:
/// each reservation's invocation names its node and plan, the plan its compiled graph, and the
/// graph the Worker its slot binds.
fn attempts_of(show: &serde_json::Value, state: &Path, worker: &str) -> String {
    let records: Vec<&serde_json::Value> = show["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["record"])
        .collect();
    let (mut open, mut ok, mut failed, mut released) = (0, 0, 0, 0);
    for reserved in records.iter().filter(|record| record["kind"] == "reserved") {
        let invocation = recorded(state, reserved["invocation_id"].as_str().unwrap());
        let node = invocation["payload"]["node"].as_str().unwrap();
        let plan = recorded(state, invocation["payload"]["plan_id"].as_str().unwrap());
        let graph = recorded(
            state,
            plan["payload"]["compiled_graph_id"].as_str().unwrap(),
        );
        let graph = &graph["payload"];
        let slot = &graph["nodes"][node]["operator"]["operator"]["slot"];
        let Some(slot) = slot.as_str() else {
            continue;
        };
        if graph["slots"][slot]["worker"] != worker {
            continue;
        }
        let own = |kind: &str| {
            let attempt = &reserved["attempt_id"];
            let found = records
                .iter()
                .find(|record| record["attempt_id"] == *attempt && record["kind"] == kind);
            found.copied()
        };
        match (own("released"), own("settled")) {
            (Some(_), _) => released += 1,
            (None, None) => open += 1,
            (None, Some(settled)) if settled["result"]["kind"] == "succeeded" => ok += 1,
            (None, Some(_)) => failed += 1,
        }
    }
    format!(
        "attempts  reserved {open}  settled ok {ok}  settled failed {failed}  released {released}"
    )
}

/// The Workers pane on a real pseudo-terminal at 100x30 and at the 80x24 minimum: the folder
/// lists the hub's Task Worker packages once opened, and one Worker's STATE counts are those
/// `af task show --json` records for the Task that ran it.
#[test]
fn the_workers_pane_shows_the_attempts_af_task_show_json_records() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub_with_a_task(&root, &home);
    let shown = af(&repo, &home, &["task", "show", "pagination-cli", "--json"]);
    assert!(shown.status.success());
    let show: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let local = home.join("state/af/task/local");
    let mut stores = std::fs::read_dir(&local).unwrap();
    let state = stores.next().unwrap().unwrap().path();
    let expected = attempts_of(&show, &state, "fixture/implementer");
    assert_eq!(
        expected,
        "attempts  reserved 0  settled ok 1  settled failed 0  released 0"
    );
    for (rows, columns) in [(ROWS, COLS), (24, 80)] {
        let mut browser = Browser::launch_sized(&repo, &home, rows, columns);
        let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
        browser.wait_for("the project settings", ready);
        let narrow = columns < 90;
        if narrow {
            // The bar starts hidden below 90 columns; <C-b> shows it.
            browser.keys(b"\x02");
        }
        // ]] twice reaches workers/; opening it lists the Worker packages HEAD commits.
        browser.keys(b"]]]]");
        browser.keys(b"\r");
        let listed = |screen: &Screen| {
            let bar = bar_rows(screen);
            bar.contains(&"      fixture/evaluator".to_owned())
                && bar.contains(&"      fixture/implementer".to_owned())
        };
        browser.wait_for("the Workers in the bar", listed);
        browser.keys(b"jj");
        browser.keys(b"\r");
        if narrow {
            // <C-b> hides the bar again, so the pane starts at the first column.
            browser.keys(b"\x02");
        }
        let left = if narrow { 0 } else { 28 };
        let main = |screen: &Screen| -> Vec<String> {
            let lines = screen.lines();
            let main = lines.iter().map(|line| line.get(left..).unwrap_or(""));
            main.map(str::to_owned).collect()
        };
        let opened = |screen: &Screen| {
            let main = main(screen);
            main.first()
                .is_some_and(|line| line == "WORKER  fixture/implementer")
                && main.iter().any(|line| line.starts_with("attempts  "))
        };
        let screen = browser.wait_for("the Worker's pane", opened);
        let text = screen.text();
        let lines = screen.lines();
        assert_eq!(lines.len(), rows);
        assert!(lines.iter().all(|line| line.len() <= columns), "{text}");
        let main = main(&screen);
        let line = |prefix: &str| {
            let found = main.iter().find(|line| line.starts_with(prefix));
            found
                .unwrap_or_else(|| panic!("no {prefix} line on:\n{text}"))
                .clone()
        };
        assert_eq!(line("IDENTITY "), "IDENTITY  worker.toml at HEAD");
        assert_eq!(line("name "), "name      fixture/implementer");
        assert_eq!(line("roles "), "roles     implement");
        assert_eq!(line("attempts "), expected, "{text}");
        assert_eq!(
            line("tokens "),
            format!(
                "tokens    charged {}",
                show["chargeable_tokens"].as_str().unwrap()
            )
        );
        assert!(lines[rows - 1].starts_with("NORMAL"), "{text}");
        browser.keys(b":q\r");
        assert_eq!(browser.exit_code(), 0);
    }
}

/// The line the released screen shows after a handed-off command ends.
fn exit_line(line: &str, ended: &str) -> String {
    format!("af {line}: {ended} -- Enter returns to the browser")
}

/// Type `:LINE` in the browser and wait for the released screen's exit line; return the screen
/// and the row the exit line is on.
fn hand_off(browser: &mut Browser, line: &str, ended: &str) -> (Screen, usize) {
    browser.keys(format!(":{line}\r").as_bytes());
    let shown = exit_line(line, ended);
    let ended = |screen: &Screen| screen.lines().contains(&shown);
    let screen = browser.wait_for("the exit line", ended);
    let at = screen.lines().iter().position(|row| *row == shown).unwrap();
    (screen, at)
}

/// Enter on the released screen: the browser again, at 100x30, its status line naming the
/// command's exit.
fn back_in_the_browser(browser: &mut Browser, status: &str) -> Screen {
    browser.keys(b"\r");
    let back = |screen: &Screen| {
        let lines = screen.lines();
        lines[ROWS - 1].starts_with("NORMAL  hub") && lines[ROWS - 1].ends_with(status)
    };
    let screen = browser.wait_for("the browser again", back);
    let text = screen.text();
    assert!(text.contains("SETTINGS  project: hub"), "{text}");
    assert!(
        bar_rows(&screen).contains(&"  v tasks/".to_owned()),
        "{text}"
    );
    screen
}

/// `:task show ID` in bare `af` prints on the released screen exactly what `af task show ID`
/// prints from the shell, then the exit line; Enter is the browser again.
#[test]
fn a_command_line_hands_the_terminal_to_af_task_show_and_enter_returns() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub_with_a_task(&root, &home);
    let shown = af(&repo, &home, &["task", "show", "pagination-cli"]);
    assert!(shown.status.success());
    let expected = String::from_utf8(shown.stdout).unwrap();
    let expected: Vec<&str> = expected.lines().collect();
    assert!(!expected.is_empty());
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    let line = "task show pagination-cli";
    let (screen, at) = hand_off(&mut browser, line, "exit 0");
    let lines = screen.lines();
    assert!(at >= expected.len(), "{}", screen.text());
    assert_eq!(
        lines[at - expected.len()..at],
        expected,
        "{}",
        screen.text()
    );
    // Nothing of the browser's frame is left on the released screen.
    assert!(!screen.text().contains("SETTINGS"), "{}", screen.text());
    back_in_the_browser(&mut browser, &format!("af {line}: exit 0"));
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// A command that fails shows its own error and its non-zero exit, and the browser stays.
#[test]
fn a_failing_command_line_shows_its_non_zero_exit() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    let failed = af(&repo, &home, &["task", "show", "nothing-here"]);
    let code = failed.status.code().unwrap();
    assert_ne!(code, 0);
    let error = String::from_utf8(failed.stderr).unwrap();
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    let line = "task show nothing-here";
    let ended = format!("exit {code}");
    let (screen, at) = hand_off(&mut browser, line, &ended);
    let lines = screen.lines();
    let expected: Vec<&str> = error.lines().collect();
    assert_eq!(
        lines[at - expected.len()..at],
        expected,
        "{}",
        screen.text()
    );
    back_in_the_browser(&mut browser, &format!("af {line}: {ended}"));
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// The handed-off command owns the terminal's foreground: `<C-c>` ends it, and the browser,
/// which the signal never reaches, comes back on Enter.
#[test]
fn ctrl_c_stops_the_handed_off_command_not_the_browser() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    // The command Worker sleeps, so `af task start --execute` is still running at `<C-c>`.
    let implementer = repo.join(".af/task-packages/fixture/implementer");
    std::fs::write(
        implementer.join("worker.py"),
        "import time\ntime.sleep(30)\n",
    )
    .unwrap();
    let digest = review_config::lock::package_digest("fixture/implementer", &implementer);
    let catalog = repo.join(".af/task-catalog.toml");
    let mut value: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog).unwrap()).unwrap();
    value["packages"]["fixture/implementer"]["digest"] = toml::Value::String(digest.unwrap());
    std::fs::write(&catalog, toml::to_string(&value).unwrap()).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "a Worker that sleeps"]);
    let ticket = root.join("ticket.json");
    std::fs::copy(repo.join("ticket.json"), &ticket).unwrap();
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    // The command runs in the repository's toplevel, so the ticket beside it is `../`.
    let line = "task start --file ../ticket.json --execute".to_owned();
    browser.keys(format!(":{line}\r").as_bytes());
    let released = |screen: &Screen| !screen.text().contains("SETTINGS");
    browser.wait_for("the released screen", released);
    std::thread::sleep(Duration::from_secs(2));
    browser.keys(b"\x03");
    let shown = exit_line(&line, "killed by SIGINT");
    let ended = |screen: &Screen| screen.lines().contains(&shown);
    browser.wait_for("the exit line", ended);
    back_in_the_browser(&mut browser, &format!("af {line}: killed by SIGINT"));
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// A command that ends at once shows its own exit, every time. The hand-off also covers the
/// race where such a command ends before its process group takes the terminal's foreground
/// (the group is gone, and its exit is reported, not a failed hand-off); `af` starts too
/// slowly for this test to reach that race on demand, so this guards the ordinary path.
#[test]
fn a_command_that_ends_at_once_still_shows_its_exit() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    let line = "--version";
    for _ in 0..8 {
        hand_off(&mut browser, line, "exit 0");
        back_in_the_browser(&mut browser, &format!("af {line}: exit 0"));
    }
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// Keys that arrive in the same read as a command line were typed before the command ran:
/// they are dropped, never replayed in the browser the user returns to. Here the `q` after
/// `:task list` would otherwise quit the browser.
#[test]
fn keys_read_with_a_command_line_are_not_replayed_after_it() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    let line = "task list";
    browser.keys(format!(":{line}\rq").as_bytes());
    let shown = exit_line(line, "exit 0");
    browser.wait_for("the exit line", |screen| screen.lines().contains(&shown));
    back_in_the_browser(&mut browser, &format!("af {line}: exit 0"));
    // Still running: a key now is the browser's.
    browser.keys(b"]]");
    browser.wait_for("the bar moved", |screen| {
        screen.lines()[ROWS - 1].starts_with("NORMAL  providers/")
    });
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// An editor runs in its own process group, as a command does: `<C-c>` ends the editor, and the
/// browser comes back and says so, where it once died with it.
#[test]
fn ctrl_c_in_an_editor_stops_the_editor_not_the_browser() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    // An "editor" that waits: `sh -c 'sleep 30' editor FILE`.
    let editor = "/bin/sh -c 'sleep 30' editor";
    let mut browser = Browser::launch_with(&repo, &home, ROWS, COLS, &[("EDITOR", editor)]);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    // The bar's first pipeline, and `gf` on it.
    browser.keys(b"]]]]]]j");
    browser.wait_for("a pipeline selected", |screen| {
        screen.lines()[ROWS - 1].starts_with("NORMAL  pipelines/")
    });
    browser.keys(b"gf");
    browser.wait_for("the released screen", |screen| {
        !screen.text().contains("SETTINGS")
    });
    std::thread::sleep(Duration::from_secs(1));
    browser.keys(b"\x03");
    let back = |screen: &Screen| {
        let last = &screen.lines()[ROWS - 1];
        last.starts_with("NORMAL  pipelines/") && last.contains("killed by SIGINT")
    };
    browser.wait_for("the browser again, naming the editor's end", back);
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}

/// Whatever a command's output left the cursor on, the exit line starts a line of its own, and
/// a command that ended its output with a newline gets no blank line before it.
#[test]
fn the_exit_line_starts_its_own_line() {
    let (_temp, root) = temp_root();
    let home = root.join("home");
    let repo = hub(&root);
    let mut browser = Browser::launch(&repo, &home);
    let ready = |screen: &Screen| screen.text().contains("SETTINGS  project: hub");
    browser.wait_for("project settings", ready);
    // `af --version` ends its output with a newline: the exit line follows it directly.
    let line = "--version";
    let (screen, at) = hand_off(&mut browser, line, "exit 0");
    let lines = screen.lines();
    assert!(lines[at - 1].starts_with("af "), "{lines:#?}");
    back_in_the_browser(&mut browser, &format!("af {line}: exit 0"));
    browser.keys(b":q\r");
    assert_eq!(browser.exit_code(), 0);
}
