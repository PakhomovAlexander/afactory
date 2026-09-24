//! Bare `af`: help and exit 2 on a pipe, the browser on a terminal. The browser runs in a real
//! pseudo-terminal at 100x30 and is driven key by key; its Pipelines pane is compared line for
//! line with what `af task plan` and `af task explain --tree` print for the same Task file.

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

/// The screen the browser painted. Only what it writes is interpreted: cursor position and
/// erase; graphic rendition and private modes are ignored.
struct Screen {
    cells: Vec<Vec<char>>,
    row: usize,
    column: usize,
}

impl Screen {
    fn parse(bytes: &[u8]) -> Screen {
        let mut screen = Screen {
            cells: vec![vec![' '; COLS]; ROWS],
            row: 0,
            column: 0,
        };
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            match byte {
                0x1b => index = screen.escape(bytes, index),
                b'\r' => screen.column = 0,
                b'\n' => screen.row = (screen.row + 1).min(ROWS - 1),
                0x20..=0x7e => {
                    if screen.row < ROWS && screen.column < COLS {
                        screen.cells[screen.row][screen.column] = char::from(byte);
                    }
                    screen.column += 1;
                }
                _ => {}
            }
        }
        screen
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
                    b'J' if parameters == "2" => self.cells = vec![vec![' '; COLS]; ROWS],
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
    _master: Box<dyn MasterPty + Send>,
}

impl Browser {
    fn launch(cwd: &Path, home: &Path) -> Browser {
        let size = PtySize {
            rows: ROWS as u16,
            cols: COLS as u16,
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
            let screen = Screen::parse(&self.bytes);
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
