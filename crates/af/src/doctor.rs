//! `af doctor`: a token-free health check of this machine and the repository it runs in.
//!
//! Each check reads what the command that owns the thing reads (`af self status`, `af provider
//! status`, `af storage`, `af onboard`) and ends `ok`, `warn` or `fail` with the one command that
//! fixes it, exactly as a shell runs it, and a hint with whatever else the operator needs to know:
//! a placeholder's choices, a step to take first. Nothing here calls a model, logs in, installs or writes a file: the only processes it
//! starts are version checks, the Provider status probe and one anonymous HTTPS request to the
//! release source while update checks are on (ADR-0210). The charged end-to-end Provider check
//! stays `af provider doctor`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::selfmgmt;

/// The `--json` document's kind.
pub(crate) const SCHEMA: &str = "af/doctor@3";
/// The text output's first line.
pub(crate) const FIRST_LINE: &str =
    "token-free; the charged end-to-end Provider check is af provider doctor";
/// The oldest git af supports.
const GIT_FLOOR: (u32, u32) = (2, 30);
/// The largest file a Gate's tool scan reads.
const MAX_SCAN_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// One check's outcome: what was found and, when it is not `ok`, the fix: one command a shell
/// runs as given (commands joined by `&&` when it takes several), never prose. The hint carries
/// what a command cannot say: a placeholder's choices, a step to take first, what the command
/// does. A problem no single command fixes has no fix and says what to do in its hint.
#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub(crate) id: &'static str,
    pub(crate) status: Status,
    pub(crate) detail: String,
    pub(crate) fix: Option<String>,
    pub(crate) hint: Option<String>,
}

impl Check {
    fn ok(id: &'static str, detail: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
            hint: None,
        }
    }

    fn warn(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
            hint: None,
        }
    }

    fn fail(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
            hint: None,
        }
    }

    /// A problem no single command fixes: no fix, and the hint says what to do.
    fn advise(status: Status, id: &'static str, detail: impl Into<String>, hint: String) -> Check {
        Check {
            id,
            status,
            detail: detail.into(),
            fix: None,
            hint: Some(hint),
        }
    }

    /// The check with `hint` beside its fix.
    fn hint(mut self, hint: impl Into<String>) -> Check {
        self.hint = Some(hint.into());
        self
    }
}

/// `af doctor [--json] [--repo DIR]`: exit 0 when nothing failed, 1 when a check failed.
pub(crate) fn run(repo: &Path, json: bool) -> Result<i32, String> {
    if let Err(error) = std::fs::metadata(repo) {
        return Err(format!(
            "--repo {}: {error} — fix: name an existing checkout, or run af doctor inside one",
            repo.display()
        ));
    }
    let checks = checks(repo);
    let code = exit_code(&checks);
    if json {
        let document = serde_json::json!({
            "schema": SCHEMA,
            "token_free": true,
            "exit_code": code,
            "checks": checks,
        });
        println!("{document}");
    } else {
        print!("{}", render(&checks, crate::human::width().max(MIN_WIDTH)));
    }
    Ok(code)
}

fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|check| check.status == Status::Fail))
}

/// The narrowest the text form is laid out for: a narrower terminal gets 60-column lines.
const MIN_WIDTH: usize = 60;
/// Where a row's detail starts: past the status and the check's ID.
const DETAIL_INDENT: usize = 17;
/// Where a fix starts: past `fix: ` under the detail.
const FIX_INDENT: usize = 22;
/// Where a hint starts: past `hint: ` under the detail.
const HINT_INDENT: usize = 23;

/// The text form within `width` columns: the first line, one row per check, its fix and hint
/// under it, and a summary. A long detail or fix continues on indented lines; a path too long for a line
/// is broken after one of its `/`, never cut short; a command stays whole on its line, and one
/// too long for a line is broken between its arguments, as every other rendering of a command
/// is ([`crate::human::fit_command`]).
fn render(checks: &[Check], width: usize) -> String {
    let mut text = format!("{}\n", layout("", FIRST_LINE, width, 2, false));
    for check in checks {
        let prefix = format!("{:<5} {:<10} ", check.status.word(), check.id);
        text.push_str(&layout(&prefix, &check.detail, width, DETAIL_INDENT, false));
        text.push('\n');
        if let Some(fix) = &check.fix {
            let prefix = format!("{:<16} fix: ", "");
            text.push_str(&layout(&prefix, fix, width, FIX_INDENT, true));
            text.push('\n');
        }
        if let Some(hint) = &check.hint {
            let prefix = format!("{:<16} hint: ", "");
            text.push_str(&layout(&prefix, hint, width, HINT_INDENT, false));
            text.push('\n');
        }
    }
    let count = |status| checks.iter().filter(|c| c.status == status).count();
    let (warned, failed) = (count(Status::Warn), count(Status::Fail));
    text.push_str(&if failed == 0 {
        format!("nothing failed ({warned} warning(s))\n")
    } else {
        format!("{failed} check(s) failed, {warned} warning(s)\n")
    });
    text
}

/// The programs a command in a detail or a fix starts with.
const COMMANDS: &[&str] = &[
    "af",
    "brew",
    "curl",
    "sudo",
    "apk",
    "cargo",
    "xcode-select",
    "export",
    "fish_add_path",
    "rm",
    "gh",
];

/// How each shell's PATH line ([`path_fix`]) starts.
const PATH_LINES: &[&str] = &[
    "export PATH=",
    "fish_add_path ",
    "set paths = [",
    "$env:PATH = ",
];

/// One piece of a row's text: a word, which may be broken when it is a path too long for any
/// line, or a command, kept whole.
enum Unit {
    Word(String),
    Command(String),
    /// A shell comment to the end of the text: every line of it starts with `#`.
    Comment(Vec<String>),
    /// A command printed exactly as written, never broken: a PATH line.
    Line(String),
}

/// `text` as words and commands. A command starts at one of [`COMMANDS`] — at the start of a
/// fix (`leading`), or after a word ending in `:`, `,` or `;`, or `then`, `or`, `&&` — and runs
/// to a word ending in `,` or `;` (kept), or before `&&`, `||`, a word opening `(` or a shell
/// comment. A span between backticks is one word.
fn units(text: &str, leading: bool) -> Vec<Unit> {
    // A PATH line quotes a directory that may hold spaces, quotes or backquotes: it is one
    // command, kept exactly as written.
    if leading && PATH_LINES.iter().any(|line| text.starts_with(line)) {
        return vec![Unit::Line(text.to_owned())];
    }
    let mut words: Vec<String> = Vec::new();
    let mut quoted = false;
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        match words.last_mut() {
            Some(last) if quoted => {
                last.push(' ');
                last.push_str(word);
            }
            _ => words.push(word.to_string()),
        }
        quoted ^= word.matches('`').count() % 2 == 1;
    }
    let opens =
        |word: &str| word.ends_with([':', ',', ';']) || matches!(word, "then" | "or" | "&&" | "||");
    let mut units = Vec::new();
    let mut index = 0;
    while index < words.len() {
        if words[index].starts_with('#') && matches!(units.last(), Some(Unit::Command(_))) {
            units.push(Unit::Comment(words[index..].to_vec()));
            break;
        }
        let starts = COMMANDS.contains(&words[index].as_str())
            && if index == 0 {
                leading
            } else {
                opens(&words[index - 1])
            };
        if !starts {
            units.push(Unit::Word(words[index].clone()));
            index += 1;
            continue;
        }
        let mut command: Vec<&str> = Vec::new();
        while let Some(word) = words.get(index) {
            if !command.is_empty()
                && (matches!(word.as_str(), "&&" | "||")
                    || word.starts_with('(')
                    || word.starts_with('#'))
            {
                break;
            }
            command.push(word);
            index += 1;
            if word.ends_with([',', ';']) {
                break;
            }
        }
        units.push(Unit::Command(command.join(" ")));
    }
    units
}

/// `prefix` and then `text` within `width` columns, continuation lines indented `indent`. Words
/// wrap at spaces; a word too long for a line of its own — a path — is broken after one of its
/// `/` (by characters only when it has none), so nothing is cut short; a command stays whole on
/// one line, and one too long for a line is broken between its arguments by
/// [`crate::human::fit_command`]. `leading` says whether `text` may start with a command.
fn layout(prefix: &str, text: &str, width: usize, indent: usize, leading: bool) -> String {
    let count = |text: &str| text.chars().count();
    let pad = " ".repeat(indent);
    let mut lines: Vec<String> = Vec::new();
    let mut line = prefix.to_string();
    // Whether `line` holds nothing yet past its prefix or indent.
    let mut fresh = true;
    let add = |line: &mut String, fresh: &mut bool, piece: &str| {
        if !*fresh {
            line.push(' ');
        }
        line.push_str(piece);
        *fresh = false;
    };
    for unit in units(text, leading) {
        match unit {
            Unit::Command(command) => {
                let space = usize::from(!fresh);
                if count(&line) + space + count(&command) <= width {
                    add(&mut line, &mut fresh, &command);
                    continue;
                }
                if !fresh {
                    lines.push(std::mem::replace(&mut line, pad.clone()));
                }
                // Broken between its arguments where it starts; when an argument is still too
                // long there, on a line of its own nearer the margin.
                let fits = |fitted: &str| fitted.lines().all(|line| count(line) <= width);
                let mut fitted = crate::human::fit_command(&format!("{line}{command}"), width);
                if !fits(&fitted) {
                    let margin = crate::human::fit_command(&format!("  {command}"), width);
                    if fits(&margin) {
                        // The line it would have started on, unless that holds only its indent.
                        if !line.trim().is_empty() {
                            lines.push(std::mem::take(&mut line));
                        }
                        fitted = margin;
                    }
                }
                let mut parts: Vec<String> = fitted.lines().map(str::to_string).collect();
                line = parts.pop().unwrap_or_default();
                lines.extend(parts);
                fresh = false;
            }
            Unit::Line(command) => {
                if !fresh && count(&line) + 1 + count(&command) > width {
                    lines.push(std::mem::replace(&mut line, pad.clone()));
                    fresh = true;
                }
                add(&mut line, &mut fresh, &command);
            }
            Unit::Comment(words) => {
                let comment = words.join(" ");
                if count(&line) + 1 + count(&comment) > width {
                    lines.push(std::mem::replace(&mut line, pad.clone()));
                    fresh = true;
                }
                for word in words {
                    if !fresh && count(&line) + 1 + count(&word) > width {
                        lines.push(std::mem::replace(&mut line, format!("{pad}#")));
                    }
                    add(&mut line, &mut fresh, &word);
                }
            }
            Unit::Word(word) => {
                let mut rest: Vec<char> = word.chars().collect();
                loop {
                    let room = width.saturating_sub(count(&line) + usize::from(!fresh));
                    if rest.len() <= room {
                        add(&mut line, &mut fresh, &rest.iter().collect::<String>());
                        break;
                    }
                    // A word that does not fit here starts the next line; only one too long
                    // for any line is broken there.
                    if !fresh {
                        lines.push(std::mem::replace(&mut line, pad.clone()));
                        fresh = true;
                        continue;
                    }
                    let cut = rest[..room.min(rest.len())]
                        .iter()
                        .rposition(|c| *c == '/')
                        .map(|at| at + 1);
                    let cut = cut.unwrap_or(room.max(1));
                    add(
                        &mut line,
                        &mut fresh,
                        &rest[..cut].iter().collect::<String>(),
                    );
                    lines.push(std::mem::replace(&mut line, pad.clone()));
                    fresh = true;
                    rest.drain(..cut);
                }
            }
        }
    }
    lines.push(line);
    lines
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn checks(repo: &Path) -> Vec<Check> {
    let installation = selfmgmt::installation();
    let registry = crate::providers::read_registry_entries();
    // The machine's own configuration, which `af self` and `af storage` read before anything
    // else: when it does not load, no fix that runs one of them can clear its check.
    let machine = crate::config::load_machine_naming()
        .err()
        .map(|refusal| Problem::of_refusal(&refusal));
    let toplevel = crate::config::git_toplevel(repo);
    let authority = toplevel
        .as_deref()
        .filter(|toplevel| toplevel.join(".af").exists())
        .map(crate::onboard::authority_facts);
    let mut checks = vec![
        agent_harness(),
        agent_sandbox(),
        agent_network(),
        binary(&installation),
        path(&installation),
        shell(),
        update(machine.as_ref()),
        tools(
            toplevel.as_deref(),
            authority.as_ref().and_then(|a| a.as_ref().ok()),
        ),
        providers(&registry),
        storage(&installation, machine.as_ref()),
    ];
    match &toplevel {
        Some(toplevel) => checks.push(project(toplevel, authority.as_ref(), &registry)),
        // Asked about a directory by name: say it is no repository rather than skip it.
        None if repo != Path::new(".") => checks.push(
            Check::warn(
                "project",
                format!("--repo {} is not inside a git repository", repo.display()),
                "af doctor --repo DIR",
            )
            .hint("DIR: a git checkout; or run af doctor inside one"),
        ),
        None => {}
    }
    let registry_problem = registry.warning.as_ref().map(|warning| {
        registry
            .path
            .as_deref()
            .map_or_else(|| Problem::said(warning), |path| Problem::at(path, warning))
    });
    let blockers: Vec<(Reads, &Problem)> = [
        machine
            .as_ref()
            .map(|problem| (Reads::MachineConfig, problem)),
        registry_problem
            .as_ref()
            .map(|problem| (Reads::Registry, problem)),
    ]
    .into_iter()
    .flatten()
    .collect();
    withhold(&mut checks, &blockers);
    checks
}

fn agent_harness() -> Check {
    let context = crate::providers::compatibility::AgentEnvironment::detect();
    Check::ok(
        "harness",
        format!(
            "driving agent harness: {}",
            context.harness.unwrap_or("none detected")
        ),
    )
}

fn agent_sandbox() -> Check {
    let context = crate::providers::compatibility::AgentEnvironment::detect();
    Check::ok(
        "sandbox",
        if context.sandboxed {
            "agent sandbox: detected"
        } else {
            "agent sandbox: not detected"
        },
    )
}

fn agent_network() -> Check {
    let context = crate::providers::compatibility::AgentEnvironment::detect();
    Check::ok(
        "network",
        match context.network_allowed {
            Some(true) => "agent sandbox network: allowed",
            Some(false) => "agent sandbox network: blocked",
            None => "agent sandbox network: unknown",
        },
    )
}

// ------------------------------------------------------------------------------------------
// files a fix reads

/// A file that does not load: where (the file, and the line a TOML parse error names) and what
/// is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    file: Option<PathBuf>,
    line: Option<usize>,
    message: String,
}

impl Problem {
    /// A problem with no file to name.
    fn said(message: &str) -> Problem {
        Problem {
            file: None,
            line: None,
            message: first_line(message),
        }
    }

    /// `file`'s problem: its TOML parse error, with the line, when it does not parse, else
    /// `otherwise` (it parses, but is not what its reader expects).
    fn at(file: &Path, otherwise: &str) -> Problem {
        match toml_problem(file) {
            Some((line, message)) => Problem {
                file: Some(file.to_path_buf()),
                line: Some(line),
                message,
            },
            None => Problem {
                file: Some(file.to_path_buf()),
                line: None,
                message: first_line(otherwise),
            },
        }
    }

    /// A configuration refusal: the file it names, read again for the line.
    fn of_refusal(refusal: &crate::config::Refusal) -> Problem {
        let Some(range) = refusal.path.clone() else {
            return Problem::said(&refusal.text);
        };
        let file = PathBuf::from(&refusal.text[range.clone()]);
        let after = refusal.text[range.end..].trim_start_matches(':').trim();
        Problem::at(&file, after)
    }

    /// Where, as a person looks for it: `FILE line N`, `FILE` or nothing.
    fn place(&self) -> Option<String> {
        let file = self.file.as_ref()?.display();
        Some(match self.line {
            Some(line) => format!("{file} line {line}"),
            None => file.to_string(),
        })
    }

    /// What to do: correct the file by hand, then run `recheck`.
    fn hint(&self, recheck: &str) -> String {
        match self.place() {
            Some(place) => format!(
                "correct {place} by hand ({}), then recheck: {recheck}",
                self.message
            ),
            None => format!("{}; correct it, then recheck: {recheck}", self.message),
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// The line and message of `file`'s TOML parse error; `None` when it parses or cannot be read.
/// Like onboarding's own authority reader, it never follows a symbolic link and never reads a
/// file larger than `MAX_SCAN_BYTES`: a repository's `.af/` cannot make doctor read, and print a
/// line of, a file outside it.
fn toml_problem(file: &Path) -> Option<(usize, String)> {
    let metadata = std::fs::symlink_metadata(file).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SCAN_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let error = toml::from_str::<toml::Table>(&text).err()?;
    let start = error.span().map_or(0, |span| span.start.min(text.len()));
    let line = text.as_bytes()[..start]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1;
    Some((line, first_line(error.message())))
}

/// What an `af` command reads before it does anything, of what a check can find unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reads {
    /// The system and user configuration: `af self` (apart from `setup-shell`), `af storage`,
    /// `af config`.
    MachineConfig,
    /// The Provider registry: `af provider setup`, `add`, `remove`, `detect --apply`.
    Registry,
}

fn reads(command: &[String]) -> Vec<Reads> {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["af", "self", "setup-shell", ..] => Vec::new(),
        ["af", "self" | "storage" | "config", ..] => vec![Reads::MachineConfig],
        ["af", "provider", "setup" | "add" | "remove", ..] => vec![Reads::Registry],
        ["af", "provider", "detect", rest @ ..] if rest.contains(&"--apply") => {
            vec![Reads::Registry]
        }
        _ => Vec::new(),
    }
}

/// A fix is offered only when running it can clear its check: one that runs a command a
/// `blockers` file stops before it starts would fail with that file's error, so it moves to the
/// hint, after what to correct first.
fn withhold(checks: &mut [Check], blockers: &[(Reads, &Problem)]) {
    for check in checks.iter_mut() {
        let Some(fix) = check.fix.clone() else {
            continue;
        };
        let Ok(words) = shell_words::split(&fix) else {
            continue;
        };
        let stops = words
            .split(|word| word == "&&")
            .flat_map(reads)
            .find_map(|read| {
                blockers
                    .iter()
                    .find(|(blocked, _)| *blocked == read)
                    .map(|(_, problem)| *problem)
            });
        let Some(problem) = stops else {
            continue;
        };
        let first = match problem.place() {
            Some(place) => format!("first correct {place} by hand ({})", problem.message),
            None => format!("first correct this: {}", problem.message),
        };
        let mut hint = format!("{first}, which stops this fix; then run: {fix}");
        if let Some(old) = &check.hint {
            hint.push_str("; ");
            hint.push_str(old);
        }
        check.fix = None;
        check.hint = Some(hint);
    }
}

// ------------------------------------------------------------------------------------------
// binary · path · shell · update

fn binary(installation: &Result<selfmgmt::Installation, String>) -> Check {
    let version = selfmgmt::VERSION;
    let installation = match installation {
        Ok(installation) => installation,
        Err(error) => {
            return Check::fail(
                "binary",
                format!("af {version}: where af installs cannot be named: {error}"),
                SET_HOME,
            )
            .hint(format!(
                "{HOME_HINT}, or set XDG_DATA_HOME and XDG_BIN_HOME"
            ));
        }
    };
    let running = installation.running.as_deref().map_or_else(
        || "an unknown path".to_string(),
        |path| path.display().to_string(),
    );
    let bin = installation.bin.display();
    let default = match (&installation.default, installation.default_status) {
        (Some(default), _) => format!("default {bin} -> af {default}"),
        (None, status) => format!("default {bin} ({status})"),
    };
    match &installation.receipt {
        None => Check::warn(
            "binary",
            format!(
                "af {version} at {running} has no install receipt at {} (a source build): it cannot write a byte-bound pin or update itself; {default}",
                installation.receipt_path.display()
            ),
            format!("af self install {version}"),
        ),
        Some(receipt)
            if receipt.verified_by.contains("minisign")
                || receipt.verified_by.contains("signature") =>
        {
            Check::ok(
                "binary",
                format!(
                    "af {version} at {running}, verified by {}; {default}",
                    receipt.verified_by
                ),
            )
        }
        Some(receipt) => {
            let detail = format!(
                "af {version} at {running} was verified by {} only, not by the release signature; {default}",
                receipt.verified_by
            );
            let reinstall = format!("af self remove {version} && af self install {version}");
            let why = "reinstalls the release, which minisign then verifies";
            match install_command("minisign") {
                Ok(install) => {
                    Check::warn("binary", detail, format!("{install} && {reinstall}")).hint(why)
                }
                Err(advice) => Check::warn("binary", detail, reinstall)
                    .hint(format!("{advice} first; that {why}")),
            }
        }
    }
}

fn path(installation: &Result<selfmgmt::Installation, String>) -> Check {
    let Ok(installation) = installation else {
        return Check::warn(
            "path",
            "the default link's directory cannot be named",
            SET_HOME,
        )
        .hint(format!("{HOME_HINT}, or set XDG_BIN_HOME"));
    };
    let bin = &installation.bin;
    let Some(dir) = bin.parent() else {
        return Check::warn(
            "path",
            format!("{} has no directory", bin.display()),
            "export XDG_BIN_HOME=DIR",
        )
        .hint("DIR: the absolute directory af links its default `af` into");
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let shell = std::env::var("SHELL").ok();
    if !dirs.iter().any(|entry| same_dir(entry, dir)) {
        let (fix, hint) = path_fix(dir, shell.as_deref(), false);
        let detail = format!(
            "{} is not on PATH, so `af` there is not found",
            dir.display()
        );
        return Check {
            fix,
            hint,
            ..Check::advise(Status::Warn, "path", detail, String::new())
        };
    }
    let first = dirs
        .iter()
        .map(|entry| entry.join("af"))
        .find(|candidate| is_executable(candidate));
    match first {
        Some(found) if same_file(&found, bin) => Check::ok(
            "path",
            format!("`af` on PATH is the default link {}", bin.display()),
        ),
        Some(found) => {
            let (fix, hint) = path_fix(dir, shell.as_deref(), true);
            let before = format!(
                "it puts {} before {} on PATH",
                dir.display(),
                found.parent().unwrap_or(Path::new("/")).display()
            );
            let hint = match (&fix, hint) {
                (Some(_), Some(hint)) => format!("{before}; {hint}; or remove {}", found.display()),
                (Some(_), None) => format!("{before}; or remove {}", found.display()),
                (None, hint) => format!(
                    "{}; or remove {}",
                    hint.unwrap_or_default(),
                    found.display()
                ),
            };
            let detail = format!(
                "`af` on PATH is {}, not the default link {}",
                found.display(),
                bin.display()
            );
            Check {
                fix,
                ..Check::advise(Status::Warn, "path", detail, hint)
            }
        }
        None => Check::warn(
            "path",
            format!("{} is on PATH but holds no af", dir.display()),
            format!("af self install {}", selfmgmt::VERSION),
        ),
    }
}

/// The fix a check that needs HOME gives, and its hint's start.
const SET_HOME: &str = "export HOME=DIR";
const HOME_HINT: &str = "DIR: your home directory, as an absolute path";

/// The command that puts `dir` first on PATH for the shell `$SHELL` names (`front`: also when it
/// is on PATH already, behind another `af`), with `dir` quoted the way that shell reads a
/// literal, so nothing in it is expanded when the line is pasted; and the hint, where the line
/// belongs for a later shell. A shell af writes no line for, or a directory no PATH line can
/// hold, has no fix, and the hint names the directory and what to do. `af self setup-shell
/// --write` is never the fix: it writes completions and man pages and leaves PATH alone.
pub(crate) fn path_fix(
    dir: &Path,
    shell: Option<&str>,
    front: bool,
) -> (Option<String>, Option<String>) {
    let name = shell
        .and_then(|shell| Path::new(shell).file_name())
        .and_then(|name| name.to_str());
    let shown = dir.display();
    let Some(literal) = dir.to_str().filter(|dir| path_literal(dir)) else {
        return (
            None,
            Some(format!(
                "af prints no PATH line for {shown}: a `:` separates PATH entries, and a control \
                 character cannot be shown legibly on one line; set XDG_BIN_HOME to a directory \
                 without either, then run af self install {}",
                selfmgmt::VERSION
            )),
        );
    };
    let line = |file: &str| Some(format!("add that line to {file}, then open a new shell"));
    match (name, name.and_then(selfmgmt::shell_named)) {
        (_, Some(crate::cli::Shell::Fish)) => {
            let flag = if front { "--move " } else { "" };
            (
                Some(format!("fish_add_path {flag}{}", fish_quote(literal))),
                None,
            )
        }
        (_, Some(crate::cli::Shell::Zsh)) => (Some(sh_export(literal)), line("~/.zshrc")),
        (_, Some(crate::cli::Shell::Bash)) => (Some(sh_export(literal)), line("~/.bashrc")),
        (_, Some(crate::cli::Shell::Elvish)) => (
            Some(format!("set paths = [{} $@paths]", elvish_quote(literal))),
            line("~/.config/elvish/rc.elv"),
        ),
        (_, Some(crate::cli::Shell::Powershell)) => (
            Some(format!(
                "$env:PATH = {} + [IO.Path]::PathSeparator + $env:PATH",
                pwsh_quote(literal)
            )),
            line("the file $PROFILE names"),
        ),
        (Some("sh"), None) => (Some(sh_export(literal)), line("~/.profile")),
        (other, None) => (
            None,
            Some(format!(
                "put {shown} first on PATH in your shell's startup file, then open a new \
                 shell; af writes that line for bash, zsh, sh, fish, elvish and pwsh, and \
                 $SHELL is {}",
                other.unwrap_or("unset")
            )),
        ),
    }
}

/// Whether af writes a PATH line for `dir`: no `:`, which separates entries, so no PATH entry
/// holds one, and no control character, which af declines to put in a line it prints to paste.
fn path_literal(dir: &str) -> bool {
    !dir.contains(':') && !dir.chars().any(char::is_control)
}

/// `export PATH='DIR':"$PATH"`, DIR single-quoted, so sh, bash and zsh expand nothing in it.
fn sh_export(dir: &str) -> String {
    format!("export PATH='{}':\"$PATH\"", dir.replace('\'', r"'\''"))
}

/// A fish literal: inside fish's single quotes only `\\` and `\'` are escapes.
fn fish_quote(dir: &str) -> String {
    format!("'{}'", dir.replace('\\', r"\\").replace('\'', r"\'"))
}

/// An elvish literal: inside elvish's single quotes only `''` is special, one `'`.
fn elvish_quote(dir: &str) -> String {
    format!("'{}'", dir.replace('\'', "''"))
}

/// A PowerShell literal: inside single quotes every single quote, the typographic ones
/// PowerShell also reads as one included, is written twice.
fn pwsh_quote(dir: &str) -> String {
    let mut quoted = String::from("'");
    for c in dir.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

fn shell() -> Check {
    let name = std::env::var("SHELL").ok();
    let shell = name
        .as_deref()
        .and_then(|shell| Path::new(shell).file_name())
        .and_then(|name| name.to_str())
        .and_then(selfmgmt::shell_named);
    let Some(shell) = shell else {
        return Check::warn(
            "shell",
            format!(
                "$SHELL ({}) is not one of bash, zsh, fish, elvish, powershell",
                name.as_deref().unwrap_or("unset")
            ),
            "af self setup-shell --shell SHELL --write",
        )
        .hint("SHELL: the shell you use, one of bash, zsh, fish, elvish, powershell");
    };
    let (completion, man) = match (selfmgmt::completion_target(shell), selfmgmt::man_dir()) {
        (Ok((completion, _)), Ok(man)) => (completion, man),
        (Err(error), _) | (_, Err(error)) => {
            return Check::warn(
                "shell",
                format!(
                    "where {} completions go cannot be named: {error}",
                    shell.name()
                ),
                SET_HOME,
            )
            .hint(HOME_HINT);
        }
    };
    let mut missing = Vec::new();
    if !completion.is_file() {
        missing.push(format!(
            "no {} completions at {}",
            shell.name(),
            completion.display()
        ));
    }
    if !man.join("af.1").is_file() {
        missing.push(format!("no man pages in {}", man.display()));
    }
    if missing.is_empty() {
        Check::ok(
            "shell",
            format!(
                "{} completions at {}; man pages in {}",
                shell.name(),
                completion.display(),
                man.display()
            ),
        )
    } else {
        Check::warn(
            "shell",
            missing.join("; "),
            format!("af self setup-shell --shell {} --write", shell.name()),
        )
    }
}

fn update(machine: Option<&Problem>) -> Check {
    let facts = match selfmgmt::update_facts() {
        Ok(facts) => facts,
        Err(error) => {
            return Check::advise(
                Status::Warn,
                "update",
                format!("the [self] policy cannot be read: {error}"),
                unreadable_policy("self", machine),
            );
        }
    };
    let checked = match facts.checked_at_epoch {
        Some(at) => format!(
            "last checked {} ago",
            age(facts.now_epoch.saturating_sub(at))
        ),
        None => "never checked".to_string(),
    };
    let policy = &facts.policy;
    let auto = match policy.auto_update {
        crate::config::AutoUpdate::Notify => "notify",
        crate::config::AutoUpdate::Always => "always",
        crate::config::AutoUpdate::Never => "never",
    };
    let channel = match policy.channel {
        crate::config::Channel::Stable => "stable",
        crate::config::Channel::Rc => "rc",
    };
    let policy_text = format!(
        "auto_update={auto} channel={channel} source={}",
        policy.source
    );
    let newer = facts.latest.as_deref().filter(|latest| {
        match (
            semver::Version::parse(latest),
            semver::Version::parse(selfmgmt::VERSION),
        ) {
            (Ok(latest), Ok(running)) => latest > running,
            _ => false,
        }
    });
    // A stalled check and a failed one are said the way `af self status` says them, with the
    // same fix.
    if let Some((said, fix)) = &facts.stalled {
        return Check::warn("update", format!("{said}; {checked}; {policy_text}"), fix);
    }
    if let Some((when, error)) = &facts.failed {
        return Check::warn(
            "update",
            format!("last attempt {when} failed: {error}; {checked}; {policy_text}"),
            "af self status --refresh",
        )
        .hint("it runs the release check again now");
    }
    if let Some(latest) = newer {
        return Check::warn(
            "update",
            format!(
                "af {latest} is available (running {}); {checked}; {policy_text}",
                selfmgmt::VERSION
            ),
            "af self update",
        );
    }
    if let Some(off) = &facts.off {
        return Check::ok(
            "update",
            format!("no release check runs ({off}); {checked}; {policy_text}"),
        );
    }
    match selfmgmt::release_source_reachability(policy) {
        Ok(reach) => Check::ok("update", format!("{checked}; {policy_text}; {reach}")),
        Err(why) => {
            let detail = format!("{checked}; {policy_text}; {why}");
            if why.contains("gh auth login") {
                Check::warn("update", detail, "gh auth login")
                    .hint("with an account that can read the release repository")
            } else {
                Check::warn("update", detail, "af self update --check")
                    .hint("check the network first")
            }
        }
    }
}

/// What to do when `[table]` does not read. A configuration file that does not load stops
/// every command that reads it, `af config show` among them, so it is named, with its line, to
/// correct by hand; a value that loads but is wrong is found by `af config show --origin`.
fn unreadable_policy(table: &str, machine: Option<&Problem>) -> String {
    match machine {
        Some(problem) => problem.hint("af doctor"),
        None => format!(
            "af config show --origin names the layer that sets [{table}]; correct it there, \
             then recheck: af doctor"
        ),
    }
}

fn age(seconds: u64) -> String {
    match seconds {
        0..=119 => format!("{seconds}s"),
        120..=7199 => format!("{}m", seconds / 60),
        7200..=172_799 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

// ------------------------------------------------------------------------------------------
// tools

/// One program a check needs, whether its absence fails or warns, and why it is needed.
struct Need {
    program: &'static str,
    fatal: bool,
    why: &'static str,
}

/// The fixes several problems of one check need: the commands, run in order and joined by `&&`,
/// each once, and the hints, joined by `; `.
#[derive(Default)]
struct Fixes {
    commands: Vec<String>,
    hints: Vec<String>,
}

impl Fixes {
    fn command(&mut self, command: impl Into<String>) {
        let command = command.into();
        if !self.commands.contains(&command) {
            self.commands.push(command);
        }
    }

    fn hint(&mut self, hint: impl Into<String>) {
        self.hints.push(hint.into());
    }

    /// The install command for `program`, or its advice as a hint when none is known.
    fn install(&mut self, program: &str) {
        match install_command(program) {
            Ok(command) => self.command(command),
            Err(advice) => self.hint(advice),
        }
    }

    fn check(self, status: Status, id: &'static str, detail: String) -> Check {
        let hint = (!self.hints.is_empty()).then(|| self.hints.join("; "));
        match (self.commands.is_empty(), hint) {
            (false, hint) => Check {
                id,
                status,
                detail,
                fix: Some(self.commands.join(" && ")),
                hint,
            },
            (true, Some(hint)) => Check::advise(status, id, detail, hint),
            (true, None) => Check {
                id,
                status,
                detail,
                fix: None,
                hint: None,
            },
        }
    }
}

fn tools(toplevel: Option<&Path>, authority: Option<&crate::onboard::AuthorityFacts>) -> Check {
    let mut found = Vec::new();
    // Each problem: whether it fails the check, what it is, and the program to install.
    let mut problems: Vec<(bool, String, &'static str)> = Vec::new();
    match git_version() {
        Some((major, minor)) if (major, minor) >= GIT_FLOOR => {
            found.push(format!("git {major}.{minor}"));
        }
        Some((major, minor)) => problems.push((
            true,
            format!(
                "git {major}.{minor} is older than {}.{}",
                GIT_FLOOR.0, GIT_FLOOR.1
            ),
            "git",
        )),
        None => problems.push((true, "git is not installed".into(), "git")),
    }
    let mut needs = vec![
        Need {
            program: "curl",
            fatal: false,
            why: "releases are fetched with it",
        },
        Need {
            program: "minisign",
            fatal: false,
            why: "install.sh verifies the release signature with it",
        },
    ];
    if !on_path("curl") {
        needs.push(Need {
            program: "gh",
            fatal: false,
            why: "without curl, releases come through gh",
        });
    }
    if let (Some(toplevel), Some(authority)) = (toplevel, authority) {
        needs.extend(gate_needs(toplevel, authority));
    }
    for need in needs {
        if need.program == "docker|podman" {
            if on_path("docker") || on_path("podman") {
                found.push("a container runtime".into());
            } else {
                problems.push((
                    need.fatal,
                    format!("no container runtime (docker or podman): {}", need.why),
                    "docker",
                ));
            }
        } else if on_path(need.program) {
            found.push(need.program.to_string());
        } else {
            problems.push((
                need.fatal,
                format!("{} is not installed: {}", need.program, need.why),
                need.program,
            ));
        }
    }
    if problems.is_empty() {
        return Check::ok("tools", found.join(", "));
    }
    let fatal = problems.iter().any(|(fatal, ..)| *fatal);
    let mut details: Vec<String> = problems
        .iter()
        .map(|(_, detail, ..)| detail.clone())
        .collect();
    if !found.is_empty() {
        details.push(format!("found {}", found.join(", ")));
    }
    let mut fixes = Fixes::default();
    for (_, _, program) in &problems {
        fixes.install(program);
    }
    let status = if fatal { Status::Fail } else { Status::Warn };
    fixes.check(status, "tools", details.join("; "))
}

/// What the repository's Gate runs, read from its commands and the files they name: make, a C
/// compiler and cargo for a Rust build, cargo-nextest, npx, python3, and a container runtime when
/// the Gate runs in one. A missing one fails: the Gate cannot pass without it.
fn gate_needs(toplevel: &Path, authority: &crate::onboard::AuthorityFacts) -> Vec<Need> {
    let mut text = String::new();
    let mut make = false;
    for (_, program, args) in &authority.gates {
        text.push_str(program);
        text.push(' ');
        text.push_str(&args.join(" "));
        text.push('\n');
        make |= matches!(program.as_str(), "make" | "gmake");
        if program.contains('/') {
            text.push_str(&bounded_text(&toplevel.join(program)));
        }
    }
    if make {
        for makefile in ["GNUmakefile", "Makefile", "makefile"] {
            text.push_str(&bounded_text(&toplevel.join(makefile)));
        }
    }
    let mut needs = Vec::new();
    let mut need = |program, why| {
        needs.push(Need {
            program,
            fatal: true,
            why,
        });
    };
    if make {
        need("make", "the Gate runs make");
    }
    if text.contains("cargo") {
        need("cargo", "the Gate builds Rust");
        need("cc", "the Gate builds Rust, which links with cc");
    }
    if text.contains("nextest") {
        need("cargo-nextest", "the Gate runs cargo nextest");
    }
    if text.contains("npx") {
        need("npx", "the Gate runs npx");
    }
    if text.contains("python3") {
        need("python3", "the Gate runs python3");
    }
    if authority.container {
        need("docker|podman", "the Gate runs in a container");
    }
    needs
}

fn bounded_text(path: &Path) -> String {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = file.take(MAX_SCAN_BYTES).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `git --version` as (major, minor); `None` when git is missing or says something else.
fn git_version() -> Option<(u32, u32)> {
    let output = review_process::reap::output(
        std::process::Command::new("git")
            .arg("--version")
            .stdin(std::process::Stdio::null()),
    )
    .ok()?;
    parse_git_version(&String::from_utf8_lossy(&output.stdout))
}

/// `git version 2.39.5 (Apple Git-154)` -> (2, 39).
pub(crate) fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let version = text.trim().strip_prefix("git version ")?;
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn on_path(program: &str) -> bool {
    crate::providers::resolve_program(program).is_some()
}

/// The one command that installs `program` with this machine's package manager, or, when no
/// package manager af knows is on PATH, what to do instead.
fn install_command(program: &str) -> Result<String, String> {
    let manager = ["brew", "apt-get", "dnf", "pacman", "apk"]
        .into_iter()
        .find(|manager| on_path(manager));
    let package = match (program, manager) {
        ("cargo-nextest", _) => return Ok("cargo install cargo-nextest --locked".into()),
        // rustup's own installer, as https://rustup.rs prints it.
        ("cargo", _) => {
            return Ok("curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh".into());
        }
        ("cc", Some("brew")) => return Ok("xcode-select --install".into()),
        ("cc" | "make", Some("apt-get")) => "build-essential",
        ("cc", _) => "gcc",
        ("npx", Some("brew")) => "node",
        ("npx", Some("apt-get" | "dnf")) => "npm",
        ("npx", _) => "nodejs",
        ("python3", Some("brew")) => "python",
        ("docker", Some("brew")) => return Ok("brew install --cask docker".into()),
        ("docker", _) => "podman",
        (other, _) => other,
    };
    match manager {
        Some("brew") => Ok(format!("brew install {package}")),
        Some("apt-get") => Ok(format!("sudo apt install {package}")),
        Some("dnf") => Ok(format!("sudo dnf install {package}")),
        Some("pacman") => Ok(format!("sudo pacman -S {package}")),
        Some("apk") => Ok(format!("apk add {package}")),
        _ => Err(format!("install {package} with your package manager")),
    }
}

// ------------------------------------------------------------------------------------------
// providers · storage · project

fn providers(registry: &crate::providers::RegistryRead) -> Check {
    if let Some(warning) = &registry.warning {
        // Every command that reads the registry stops on it, and `af provider status` only says
        // it again: no command clears it, so the hint names the file and the line.
        let problem = registry
            .path
            .as_deref()
            .map_or_else(|| Problem::said(warning), |path| Problem::at(path, warning));
        let hint = match problem.place() {
            Some(place) => format!(
                "correct {place} by hand ({}), or move the file aside to start an empty \
                 registry, then recheck: af doctor",
                problem.message
            ),
            None => problem.hint("af doctor"),
        };
        return Check::advise(
            Status::Fail,
            "providers",
            format!("the Provider registry cannot be read: {warning}"),
            hint,
        );
    }
    // The one status probe doctor runs, the logged-out ambient contexts the status table leaves
    // out kept; a denied probe is never left out, and establishes neither compatibility nor
    // login (ADR-0249), so it is said before anything is suggested.
    let statuses = crate::providers::probed_statuses_with_ambient();
    if let Some(denied) = statuses
        .iter()
        .find(|status| status.status == "probe denied")
    {
        return Check::advise(
            Status::Warn,
            "providers",
            format!(
                "provider {}: host denied a capability probe; compatibility and login could not be established",
                denied.id
            ),
            "retry outside the sandbox or with host-approved permissions; then run af doctor"
                .into(),
        );
    }
    // The same names-only scan `af provider status` and `af onboard` list (ADR-0206). It reads
    // no marker and probes nothing: a directory it names is not known to hold a login.
    if registry.registered.is_empty() {
        let found: Vec<String> = registry
            .unregistered
            .iter()
            .map(|dir| crate::providers::detect::display_path(dir))
            .collect();
        let found_text = if found.is_empty() {
            String::new()
        } else {
            format!(
                "; found but not registered (not probed): {}",
                found.join(", ")
            )
        };
        let kind = registry
            .candidates
            .first()
            .map_or("claude", |candidate| candidate.kind);
        let registry_text = registry
            .path
            .as_deref()
            .map(|path| format!(" in {}", path.display()))
            .unwrap_or_default();
        return Check::warn(
            "providers",
            format!("no Provider registered{registry_text}{found_text}"),
            "af provider detect --apply",
        )
        .hint(format!(
            "it registers the logins it finds and none that is logged out; or, at your \
             terminal: af provider setup {kind}-main --kind {kind} --login"
        ));
    }
    let (ambient, statuses): (Vec<_>, Vec<_>) = statuses
        .iter()
        .partition(|status| crate::providers::is_ambient_candidate(status));
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for entry in &registry.registered {
        let status = statuses
            .iter()
            .find(|status| status.id == entry.id)
            .map_or(NOT_PROBED, |status| status.status.as_str());
        let line = format!("{} ({}, {status})", entry.id, entry.kind);
        if status == "authenticated" {
            good.push(line);
        } else {
            bad.push((entry, status, line));
        }
    }
    let mut detail = good
        .iter()
        .chain(bad.iter().map(|(_, _, line)| line))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    detail.push_str(&found_unregistered(&registry.unregistered, &ambient));
    match bad.first() {
        Some((entry, status, _)) => registered_problem(entry, status, detail),
        // Another login on this machine is the operator's to register or not: a registered
        // Provider that works needs nothing more, so nothing found beside it is a warning.
        None => Check::ok("providers", detail),
    }
}

/// The status a registered Provider has when the probe returned no row for it.
const NOT_PROBED: &str = "not probed";

/// The directories the names-only scan found that no registry entry names, and what the one
/// status probe doctor already ran learned about each: only an ambient context
/// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME` or a CLI's default directory) is ever asked, and nothing is
/// probed for this line. Each is optional; the commands are what an operator who wants one runs.
fn found_unregistered(found: &[PathBuf], ambient: &[&crate::providers::ProviderStatus]) -> String {
    if found.is_empty() {
        return String::new();
    }
    let (mut logged_in, mut logged_out, mut unprobed) = (Vec::new(), Vec::new(), Vec::new());
    for dir in found {
        let shown = crate::providers::detect::display_path(dir);
        let answer = ambient
            .iter()
            .find(|status| Path::new(&status.auth_context) == dir.as_path())
            .map(|status| status.status.as_str());
        match answer {
            Some("authenticated") => logged_in.push(shown),
            Some("not authenticated") => logged_out.push(shown),
            _ => unprobed.push(shown),
        }
    }
    let mut text = String::from("; optional, found but not registered:");
    for (dirs, state) in [
        (&logged_in, "logged in"),
        (&logged_out, "logged out"),
        (&unprobed, "not probed"),
    ] {
        if !dirs.is_empty() {
            text.push_str(&format!(" {} ({state}),", dirs.join(", ")));
        }
    }
    text.pop();
    if !logged_in.is_empty() {
        text.push_str("; to register the logged-in ones: af provider detect --apply");
    } else if !unprobed.is_empty() {
        // Doctor read no marker, so it cannot say which of them detect will ask about.
        text.push_str(UNPROBED_GUIDANCE);
    }
    text
}

/// What `af provider detect` does with a directory doctor did not probe (ADR-0206): it asks a
/// CLI only about a safe directory holding a login marker, `--all` also about a safe empty one,
/// and never about an unsafe or missing one.
const UNPROBED_GUIDANCE: &str = "; to inspect the not-probed ones: af provider detect, which asks \
     a CLI only about a safe directory holding a login marker; to also ask about a safe empty \
     one: af provider detect --all; an unsafe or missing one is never asked";

/// A registered Provider that is not authenticated, by what its status probe said: a login af can
/// see is missing, its CLI cannot be used (no login fixes that), or af disabled it for a while.
fn registered_problem(entry: &crate::providers::Declared, status: &str, detail: String) -> Check {
    let id = shell_words::quote(&entry.id);
    let kind = entry.kind;
    let setup = format!("af provider setup {id} --kind {kind} --login");
    match status {
        "not authenticated" => Check::warn("providers", detail, setup)
            .hint("at your terminal, where it logs in; af provider status shows why first"),
        NOT_PROBED => Check::warn("providers", detail, setup).hint(
            "its directory holds no login yet, so its CLI was not asked; at your terminal, where \
             it logs in",
        ),
        "disabled" => Check::warn("providers", detail, format!("af provider enable {id}"))
            .hint("af set it aside after a Provider failure; this makes it selectable again"),
        "installation failed" => Check::advise(
            Status::Warn,
            "providers",
            detail,
            format!(
                "the {kind} CLI is not on PATH or does not start, so no login fixes it: install \
                 or repair it, then recheck: af provider status"
            ),
        ),
        "incompatible" => Check::advise(
            Status::Warn,
            "providers",
            detail,
            format!(
                "the {kind} CLI does not accept af's invocation, so no login fixes it: upgrade \
                 it, then recheck: af provider status"
            ),
        ),
        _ => Check::advise(
            Status::Warn,
            "providers",
            detail,
            format!(
                "its status probe gave no answer about the login ({status}): af provider status \
                 names the cause"
            ),
        ),
    }
}

fn storage(
    installation: &Result<selfmgmt::Installation, String>,
    machine: Option<&Problem>,
) -> Check {
    let policy = match crate::storage::policy() {
        Ok(policy) => policy,
        Err(error) => {
            return Check::advise(
                Status::Fail,
                "storage",
                format!("[storage] cannot be read: {error}"),
                unreadable_policy("storage", machine),
            );
        }
    };
    let total = crate::storage::Roots::current()
        .map(|roots| crate::storage::inventory(&roots, crate::storage::Detail::Stores).total());
    let free = crate::storage::volumes_of(&policy)
        .into_iter()
        .filter_map(|volume| volume.free.ok().map(|free| (free, volume.path)))
        .min_by_key(|(free, _)| *free);
    let human = crate::storage::human_bytes;
    let mut detail = match &total {
        Ok(total) => format!(
            "af keeps {} of a {} budget",
            human(*total),
            human(policy.max_bytes)
        ),
        Err(error) => format!("what af keeps cannot be measured: {error}"),
    };
    if let Some((free, path)) = &free {
        detail.push_str(&format!(
            "; {} free at {} (floor {})",
            human(*free),
            path.display(),
            human(policy.min_free_bytes)
        ));
    }
    if let Some((free, path)) = &free
        && *free < policy.min_free_bytes
    {
        return Check::fail("storage", detail, "af storage prune --apply").hint(format!(
            "it reclaims what af keeps; free disk at {} must stay above {}",
            path.display(),
            human(policy.min_free_bytes)
        ));
    }
    if let Ok(total) = total
        && total > policy.max_bytes
    {
        return Check::warn("storage", detail, "af storage prune --apply");
    }
    let abandoned = installation
        .as_ref()
        .map(|installation| installation.abandoned_stagings.clone())
        .unwrap_or_default();
    if let Some(first) = abandoned.first() {
        detail.push_str(&format!(
            "; {} abandoned install staging(s), such as {}",
            abandoned.len(),
            first.display()
        ));
        return Check::warn("storage", detail, "af self status")
            .hint("any af self command recovers them");
    }
    let roots = crate::storage::Roots::current()
        .map(|roots| vec![roots.warm, roots.workspaces])
        .unwrap_or_default()
        .into_iter()
        .chain([crate::storage::sandbox_root(&policy)])
        .collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        let indexed = spotlight_indexed(&roots, Path::new(MDUTIL), &|path| {
            crate::storage::measure_volume(path)
                .mount
                .map_or_else(|| path.to_path_buf(), |(mount, _)| mount)
        });
        if !indexed.is_empty() {
            return spotlight_warning(detail, &indexed);
        }
    }
    Check::ok("storage", detail)
}

/// macOS's Spotlight tool, asked only for a root's indexing status.
const MDUTIL: &str = "/usr/bin/mdutil";

/// How long `mdutil -s` may take for one root before it counts as unknown, not indexed.
const MDUTIL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Where Spotlight never indexes: the per-user temporary directories below `/var/folders`.
const UNINDEXED: [&str; 2] = ["/var/folders", "/private/var/folders"];

/// The roots af's builds and sandboxes write to that Spotlight indexes: `mdutil -s` says
/// indexing is enabled on the volume holding one (`volume_of` names it: `mdutil` answers for
/// volumes only), and no component of its path is named `*.noindex`, which Spotlight skips, nor
/// is it below `/var/folders`. Every build writes thousands of files there, and indexing them,
/// with Gatekeeper's scan of each new binary, costs cores on every gate. A root that does not
/// exist yet is asked about at its nearest existing ancestor; one `mdutil` cannot answer for is
/// left out.
fn spotlight_indexed(
    roots: &[PathBuf],
    mdutil: &Path,
    volume_of: &dyn Fn(&Path) -> PathBuf,
) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter(|root| !UNINDEXED.iter().any(|dir| root.starts_with(dir)))
        .cloned()
        .collect();
    indexed_among(&roots, mdutil, volume_of)
}

/// [`spotlight_indexed`] over roots none of which is below a directory Spotlight never indexes.
fn indexed_among(
    roots: &[PathBuf],
    mdutil: &Path,
    volume_of: &dyn Fn(&Path) -> PathBuf,
) -> Vec<PathBuf> {
    let mut indexed = Vec::new();
    for root in roots {
        let excluded = root.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name.to_ascii_lowercase().ends_with(".noindex"))
        });
        if excluded || indexed.contains(root) {
            continue;
        }
        let Some(existing) = root.ancestors().find(|path| path.exists()) else {
            continue;
        };
        if mdutil_says_indexed(mdutil, &volume_of(existing)) {
            indexed.push(root.clone());
        }
    }
    indexed
}

/// Whether `mdutil -s PATH` reports indexing enabled, within [`MDUTIL_TIMEOUT`], supervised and
/// reaped by `review-process` (ADR-0026).
fn mdutil_says_indexed(mdutil: &Path, path: &Path) -> bool {
    let mut command = std::process::Command::new(mdutil);
    command.arg("-s").arg(path);
    review_process::run_supervised(&mut command, None, MDUTIL_TIMEOUT)
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("Indexing enabled"))
}

/// The storage row when Spotlight indexes af's build and sandbox roots: a warning naming them,
/// whose fix excludes them.
fn spotlight_warning(mut detail: String, indexed: &[PathBuf]) -> Check {
    let named: Vec<String> = indexed
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    detail.push_str(&format!(
        "; Spotlight indexes {}, which every build and sandbox writes to",
        named.join(", ")
    ));
    Check::advise(
        Status::Warn,
        "storage",
        detail,
        "add them in System Settings > Spotlight > Search Privacy, or point [storage] \
         build_cache_root and sandbox_root at directories named *.noindex (docs/README.md, \
         macOS)"
            .into(),
    )
}

fn project(
    toplevel: &Path,
    authority: Option<&Result<crate::onboard::AuthorityFacts, String>>,
    registry: &crate::providers::RegistryRead,
) -> Check {
    let repo = toplevel.display();
    let onboard = format!(
        "af onboard --repo {}",
        shell_words::quote(&repo.to_string())
    );
    let Some(authority) = authority else {
        return Check::warn(
            "project",
            format!("{repo} has no .af/ review authority"),
            onboard,
        )
        .hint("a token-free preview of the .af/ it would write; nothing is written");
    };
    let authority = match authority {
        Ok(authority) => authority,
        // `af onboard` checks an existing `.af/` exactly as this check did, so it would stop on
        // the same error: there is no fix, and the hint names the file and the line.
        Err(error) => {
            let recheck = format!("af doctor --repo {}", shell_words::quote(&repo.to_string()));
            let hint = match authority_problem(toplevel) {
                Some(problem) => problem.hint(&recheck),
                None if error.contains("af.lock") || error.contains("--refresh-lock") => format!(
                    "correct .af/ by hand as the error says, then recheck: {recheck}; a lock \
                     that no longer matches files you changed on purpose is re-pinned by: \
                     {onboard} --refresh-lock"
                ),
                None => format!("correct .af/ by hand as the error says, then recheck: {recheck}"),
            };
            return Check::advise(
                Status::Fail,
                "project",
                format!("{repo}/.af does not validate: {}", first_line(error)),
                hint,
            );
        }
    };
    let mut parts = vec![format!("{repo}/.af validates")];
    // A failure's fix comes before a warning's.
    let (mut fails, mut warns) = (Fixes::default(), Fixes::default());
    match selfmgmt::pin_here(toplevel) {
        Ok(None) => parts.push("the lock pins no af release".into()),
        Ok(Some(pin)) => {
            parts.push(format!("{} pins af {}", pin.lock.display(), pin.version));
            if !pin.bound {
                warns.command(format!("{onboard} --refresh-lock --af {}", pin.version));
                warns.hint("the lock binds no bytes for this target; --refresh-lock binds them");
            }
            if !pin.installed {
                warns.command(format!("af self install {}", pin.version));
                warns.hint("the pinned release is not installed");
            } else if pin.matches == Some(false) {
                fails.command(format!(
                    "af self remove {0} && af self install {0}",
                    pin.version
                ));
                fails.hint("the installed bytes are not the ones the lock pins");
            }
        }
        Err(why) => fails.hint(format!(
            "fix {} by hand ({why})",
            toplevel.join(".af/af.lock").display()
        )),
    }
    for (name, program, _) in &authority.gates {
        let resolvable = if program.contains('/') {
            toplevel.join(program).is_file()
        } else {
            on_path(program)
        };
        if resolvable {
            parts.push(format!("Gate {name} runs {program}"));
        } else {
            fails.install(program);
            fails.hint(format!("Gate {name} runs {program}, which is not found"));
        }
    }
    for (node, runner) in &authority.workers {
        let kind = match runner.as_str() {
            "claude" => "claude",
            "codex" => "codex",
            _ => continue,
        };
        match registry.ids_of(kind).first() {
            Some(id) => parts.push(format!("{node} can bind {id}")),
            None => {
                warns.command("af provider detect --apply");
                warns.hint(format!(
                    "{node} needs a {kind} Provider; or, at your terminal: af provider setup \
                     {kind}-main --kind {kind} --login"
                ));
            }
        }
    }
    let detail = parts.join("; ");
    let failed = !fails.commands.is_empty() || !fails.hints.is_empty();
    let warned = !warns.commands.is_empty() || !warns.hints.is_empty();
    for command in warns.commands {
        fails.command(command);
    }
    fails.hints.extend(warns.hints);
    if failed {
        fails.check(Status::Fail, "project", detail)
    } else if warned {
        fails.check(Status::Warn, "project", detail)
    } else {
        Check::ok("project", detail)
    }
}

/// The first `.af/` file `af onboard` reads that is not TOML, in the order it reads them: the
/// project file, the lock, the pipelines, the Worker packages.
fn authority_problem(toplevel: &Path) -> Option<Problem> {
    let af = toplevel.join(".af");
    let listed = |dir: PathBuf| -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        found.sort();
        found
    };
    let workers = listed(af.join("workers"))
        .into_iter()
        .filter(|dir| dir.is_dir())
        .flat_map(listed);
    [af.join("af.toml"), af.join("af.lock")]
        .into_iter()
        .chain(listed(af.join("pipelines")))
        .chain(workers)
        .filter(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
                && (path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                    || path.file_name().is_some_and(|name| name == "af.lock"))
        })
        .find_map(|path| {
            toml_problem(&path).map(|(line, message)| Problem {
                file: Some(path),
                line: Some(line),
                message,
            })
        })
}

// ------------------------------------------------------------------------------------------
// files

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn same_dir(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (std::fs::canonicalize(left), std::fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

/// Whether `found` on PATH is the default link itself: the same path, or the same file it names.
fn same_file(found: &Path, link: &Path) -> bool {
    found == link
        || matches!(
            (std::fs::canonicalize(found), std::fs::canonicalize(link)),
            (Ok(found), Ok(link)) if found == link
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed `.af/` file is named, but a symbolic link (here to a malformed file outside
    /// `.af/`) and a file above the scan bound are never read.
    #[test]
    fn the_authority_scan_reads_no_link_and_no_oversized_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.toml");
        std::fs::write(&outside, "secret = [\n").unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".af/pipelines/a-link.toml")).unwrap();
        let big = "# padding\n".repeat(usize::try_from(MAX_SCAN_BYTES).unwrap() / 10 + 1);
        std::fs::write(repo.join(".af/pipelines/b-big.toml"), big + "broken = [\n").unwrap();
        assert!(super::toml_problem(&repo.join(".af/pipelines/a-link.toml")).is_none());
        assert!(super::toml_problem(&repo.join(".af/pipelines/b-big.toml")).is_none());
        assert!(super::authority_problem(&repo).is_none());
        std::fs::write(
            repo.join(".af/pipelines/c-small.toml"),
            "x = 1\nbroken = [\n",
        )
        .unwrap();
        let problem = super::authority_problem(&repo).expect("the small malformed file is named");
        assert_eq!(
            problem.file.as_deref(),
            Some(repo.join(".af/pipelines/c-small.toml").as_path())
        );
        assert_eq!(problem.line, Some(2));
    }

    /// C3 item 29: a root `mdutil -s` reports indexed is named, one named `*.noindex` is never
    /// asked about, and a tool that cannot answer reports nothing.
    #[test]
    fn spotlight_indexed_roots_are_named_with_their_fix() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let asked = directory.path().join("asked");
        let mdutil = directory.path().join("mdutil");
        let script = r#"#!/bin/sh
echo "$2" >> "$ASKED"
case "$2" in
  */off*) printf '%s:\n\tIndexing disabled.\n' "$2" ;;
  *) printf '%s:\n\tIndexing enabled.\n' "$2" ;;
esac
"#;
        std::fs::write(
            &mdutil,
            script.replace("\"$ASKED\"", &format!("'{}'", asked.display())),
        )
        .unwrap();
        std::fs::set_permissions(&mdutil, std::fs::Permissions::from_mode(0o755)).unwrap();
        let on = directory.path().join("on/cache");
        let off = directory.path().join("off/cache");
        let skipped = directory.path().join("builds.noindex/cache");
        for root in [&on, &off, &skipped] {
            std::fs::create_dir_all(root).unwrap();
        }
        let absent = directory.path().join("on/not-yet/sandboxes");
        let roots = [on.clone(), off.clone(), skipped.clone(), absent.clone()];
        let itself = |path: &Path| path.to_path_buf();
        // `$TMPDIR` is below `/var/folders`, which Spotlight never indexes: the roots here are
        // judged as if they were elsewhere.
        assert_eq!(
            indexed_among(&roots, &mdutil, &itself),
            [on.clone(), absent]
        );
        let asked = std::fs::read_to_string(&asked).unwrap();
        assert!(!asked.contains(".noindex"), "{asked}");
        assert!(
            asked.contains("/on/not-yet\n") || asked.contains("/on\n"),
            "{asked}"
        );
        // A tool that is missing answers nothing.
        assert!(indexed_among(&roots, &directory.path().join("missing"), &itself).is_empty());
        // The per-user temporary directories are never indexed, so never asked about.
        let temporary = [PathBuf::from("/private/var/folders/xy/T/af-sandboxes")];
        assert!(spotlight_indexed(&temporary, &mdutil, &itself).is_empty());
        let check = spotlight_warning("af keeps 1 B".into(), &[on.clone()]);
        assert_eq!(check.status, Status::Warn);
        assert!(
            check
                .detail
                .contains(&format!("Spotlight indexes {}", on.display()))
        );
        // No one command excludes them: the check has no fix, and its hint says what to do.
        assert_eq!(check.fix, None);
        assert!(check.hint.unwrap().contains("Search Privacy"));
    }

    #[test]
    fn git_versions_parse_with_vendor_suffixes() {
        assert_eq!(
            parse_git_version("git version 2.39.5 (Apple Git-154)\n"),
            Some((2, 39))
        );
        assert_eq!(parse_git_version("git version 2.30.0"), Some((2, 30)));
        assert_eq!(
            parse_git_version("git version 2.45.windows.1"),
            Some((2, 45))
        );
        assert_eq!(parse_git_version("hub version 2.14.2"), None);
        assert!(parse_git_version("git version 2.29.2").unwrap() < GIT_FLOOR);
    }

    /// The fix is the command alone; where the line belongs is the hint (GA second fix item 4).
    /// Each shell gets its own line (GA third fix item 2), and a shell af writes no line for
    /// gets no fix and a hint that names the directory.
    #[test]
    fn the_path_fix_is_the_line_for_the_users_shell() {
        let dir = Path::new("/home/o/.local/bin");
        assert_eq!(
            path_fix(dir, Some("/usr/bin/fish"), false),
            (Some("fish_add_path '/home/o/.local/bin'".into()), None)
        );
        assert_eq!(
            path_fix(dir, Some("/usr/bin/fish"), true).0.unwrap(),
            "fish_add_path --move '/home/o/.local/bin'"
        );
        let export = "export PATH='/home/o/.local/bin':\"$PATH\"";
        for (shell, file) in [
            ("/bin/zsh", "~/.zshrc"),
            ("/bin/bash", "~/.bashrc"),
            ("/bin/sh", "~/.profile"),
        ] {
            let (fix, hint) = path_fix(dir, Some(shell), false);
            assert_eq!(fix.as_deref(), Some(export));
            assert_eq!(
                hint.unwrap(),
                format!("add that line to {file}, then open a new shell")
            );
        }
        let (fix, hint) = path_fix(dir, Some("/usr/local/bin/elvish"), false);
        assert_eq!(fix.unwrap(), "set paths = ['/home/o/.local/bin' $@paths]");
        assert!(hint.unwrap().contains("rc.elv"));
        let (fix, hint) = path_fix(dir, Some("/usr/local/bin/pwsh"), true);
        assert_eq!(
            fix.unwrap(),
            "$env:PATH = '/home/o/.local/bin' + [IO.Path]::PathSeparator + $env:PATH"
        );
        assert!(hint.unwrap().contains("$PROFILE"));
        for shell in [None, Some("/bin/tcsh"), Some("/usr/bin/nu")] {
            let (fix, hint) = path_fix(dir, shell, false);
            assert_eq!(fix, None, "{shell:?}");
            let hint = hint.unwrap();
            assert!(
                hint.contains("put /home/o/.local/bin first on PATH"),
                "{hint}"
            );
            let named = shell.map_or("unset", |shell| shell.rsplit('/').next().unwrap());
            assert!(hint.ends_with(&format!("$SHELL is {named}")), "{hint}");
        }
        // No PATH entry holds a `:`, and no pasted line carries a newline.
        for odd in ["/home/o/a:b/bin", "/home/o/a\nb/bin"] {
            let (fix, hint) = path_fix(Path::new(odd), Some("/bin/zsh"), false);
            assert_eq!(fix, None);
            assert!(hint.unwrap().contains("XDG_BIN_HOME"));
        }
    }

    /// Every character a shell might expand, in one directory name.
    const HOSTILE: &str = "a $TAG \"b' c`id`d$(touch x)e\u{2019}";

    /// A real directory with [`HOSTILE`] in its name, inside a fresh temporary directory that
    /// also serves as the shell's working directory, so a `$(touch x)` that ran leaves `x`.
    fn hostile_dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(HOSTILE).join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        (root, dir)
    }

    /// GA third fix item 2: the sh, bash and zsh line parses in `/bin/sh -n`, and run in sh (and
    /// in bash and zsh, which are on PATH here or skipped) puts the literal directory first on
    /// PATH, expanding nothing in it.
    #[test]
    fn the_posix_path_line_keeps_a_hostile_directory_literal() {
        let (root, dir) = hostile_dir();
        let literal = dir.to_str().unwrap();
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            let (fix, _) = path_fix(&dir, Some(shell), false);
            let fix = fix.unwrap();
            assert!(fix.starts_with("export PATH='"), "{fix}");
            let parsed = std::process::Command::new("/bin/sh")
                .args(["-n", "-c", &fix])
                .output()
                .unwrap();
            assert!(parsed.status.success(), "{fix}: {parsed:?}");
            for runner in ["sh", "bash", "zsh"] {
                if which(runner).is_none() {
                    eprintln!("skipped: {runner} is not on PATH");
                    continue;
                }
                let script = format!("{fix}; printf %s \"$PATH\"");
                let printed = run_in(&root, runner, &["-c", &script]);
                let first = printed.split(':').next().unwrap();
                assert_eq!(first, literal, "{runner}: {fix}");
                assert_eq!(printed, format!("{literal}:/usr/bin:/bin"), "{runner}");
            }
            assert!(!root.path().join("x").exists(), "{fix} ran $(touch x)");
            // The text form prints the line exactly as the JSON carries it, at any width.
            for width in [100, 60] {
                let text = render(&[Check::warn("path", "off PATH", fix.clone())], width);
                assert!(
                    text.lines()
                        .any(|line| line.trim_start() == format!("fix: {fix}")),
                    "{width}\n{text}"
                );
            }
        }
    }

    /// GA third fix item 2: the fish, elvish and pwsh lines, run in their own shell when it is on
    /// PATH, put the literal directory first; each is skipped with a message when it is not.
    #[test]
    fn the_fish_elvish_and_pwsh_lines_keep_a_hostile_directory_literal() {
        let (root, dir) = hostile_dir();
        let literal = dir.to_str().unwrap();
        let cases = [
            ("fish", "printf %s\\n $PATH[1]"),
            ("elvish", "echo $paths[0]"),
            ("pwsh", "($env:PATH -split [IO.Path]::PathSeparator)[0]"),
        ];
        for (shell, print) in cases {
            let (fix, _) = path_fix(&dir, Some(&format!("/usr/bin/{shell}")), false);
            let fix = fix.unwrap();
            let Some(binary) = which(shell) else {
                eprintln!("skipped: {shell} is not on PATH, so `{fix}` was not run");
                continue;
            };
            let script = format!("{fix}\n{print}\n");
            let args: Vec<&str> = match shell {
                "pwsh" => vec!["-NoProfile", "-NonInteractive", "-Command", &script],
                // fish copies `fish_user_paths` into PATH from its own share config, so it
                // starts with config, kept off the user's by the private XDG directories.
                "fish" => vec!["-c", &script],
                _ => vec!["-norc", "-c", &script],
            };
            let printed = run_in(&root, binary.to_str().unwrap(), &args);
            assert_eq!(printed.trim_end_matches('\n'), literal, "{shell}: {fix}");
            assert!(!root.path().join("x").exists(), "{fix} ran $(touch x)");
        }
    }

    fn which(program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }

    /// `program args` in `root` with a private HOME and XDG directories (fish saves
    /// `fish_add_path` in its universal variables there), `TAG` set and PATH `/usr/bin:/bin`;
    /// its stdout.
    fn run_in(root: &tempfile::TempDir, program: &str, args: &[&str]) -> String {
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let program = which(program).unwrap_or_else(|| PathBuf::from(program));
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(root.path())
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("TMPDIR", root.path())
            .env("TAG", "EXPANDED")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn only_a_failed_check_fails_the_command_and_the_text_starts_with_the_promise() {
        let checks = vec![
            Check::ok("binary", "af"),
            Check::warn("path", "off PATH", "fish_add_path /x"),
        ];
        assert_eq!(exit_code(&checks), 0);
        let text = render(&checks, 100);
        assert_eq!(text.lines().next(), Some(FIRST_LINE));
        assert!(text.contains("fix: fish_add_path /x"), "{text}");
        assert!(text.ends_with("nothing failed (1 warning(s))\n"), "{text}");
        let failed = vec![Check::fail(
            "tools",
            "git is not installed",
            "brew install git",
        )];
        assert_eq!(exit_code(&failed), 1);
        assert!(render(&failed, 100).contains("1 check(s) failed"));
    }

    /// GA final fix item 7: every row of the text form fits the width, at 100, 80 and 60
    /// columns, with long paths: a path is broken after a `/` and never cut short, and each fix
    /// command stays whole on its line or is broken between its arguments.
    #[test]
    fn rows_fit_the_width_and_keep_paths_and_fix_commands_whole() {
        let home = format!(
            "/private/var/folders/f9/{}/T/af-check-12345-AbCdEf/tmp/.tmpAbCdEf/home",
            "y".repeat(32)
        );
        let bin = format!("{home}/.local/bin");
        let checks = vec![
            Check::warn(
                "binary",
                format!(
                    "af 0.1.0 at {home}/target/debug/af has no install receipt at \
                     {home}/.local/share/af/receipt.toml (a source build): it cannot write a \
                     byte-bound pin or update itself; default {bin}/af (missing)"
                ),
                "af self install 0.1.0",
            ),
            Check::warn(
                "path",
                format!("{bin} is not on PATH, so `af` there is not found"),
                format!("export PATH=\"{bin}:$PATH\""),
            )
            .hint("add that line to your shell's startup file, then open a new shell"),
            Check::warn(
                "shell",
                format!(
                    "no bash completions at {home}/.local/share/bash-completion/completions/af; no man pages in {home}/.local/share/man/man1"
                ),
                "af self setup-shell --shell SHELL --write",
            )
            .hint("SHELL: the shell you use, one of bash, zsh, fish, elvish, powershell"),
            Check::warn(
                "providers",
                "no Provider registered",
                "af provider detect --apply",
            )
            .hint(
                "it registers the logins it finds; or, at your terminal: af provider setup \
                 claude-main --kind claude --login",
            ),
            Check::warn(
                "path",
                "off PATH",
                "export PATH=\"$HOME/.local/bin:$PATH\"",
            )
            .hint("add that line to ~/.zshrc, then open a new shell"),
        ];
        let squeeze = |text: &str| text.split_whitespace().collect::<String>();
        // One shell word too long for any line, printed whole on its own line.
        let export = format!("fix: export PATH=\"{bin}:$PATH\"");
        for width in [100, 80, 60] {
            let text = render(&checks, width);
            for line in text.lines().filter(|line| line.trim_start() != export) {
                assert!(line.chars().count() <= width, "{width}: `{line}`\n{text}");
            }
            assert!(
                text.lines().any(|line| line.trim_start() == export),
                "{width}\n{text}"
            );
            // Nothing is cut short: every detail, fix and hint is there, path characters in order.
            let flat = squeeze(&text.replace(" \\\n", " "));
            for check in &checks {
                assert!(
                    flat.contains(&squeeze(&check.detail)),
                    "{width}: {}\n{text}",
                    check.detail
                );
                let fix = check.fix.as_deref().unwrap();
                assert!(
                    flat.contains(&squeeze(fix)) || width < 100,
                    "{width}: {fix}\n{text}"
                );
                if let Some(hint) = &check.hint {
                    assert!(flat.contains(&squeeze(hint)), "{width}: {hint}\n{text}");
                }
            }
            // The af commands are whole on one line, or broken between arguments with ` \`.
            for command in [
                "fix: af self install 0.1.0",
                "fix: af provider detect --apply",
                "fix: af self setup-shell --shell SHELL --write",
            ] {
                // At 60 columns the last one is broken between its arguments.
                if DETAIL_INDENT + command.len() > width {
                    continue;
                }
                assert!(
                    text.lines().any(|line| line.contains(command)),
                    "{width}: {command}\n{text}"
                );
            }
            let setup = text
                .lines()
                .skip_while(|line| !line.contains("af provider setup"))
                .take_while(|line| !line.starts_with("warn "))
                .collect::<Vec<_>>()
                .join("\n");
            let words: Vec<&str> = setup
                .split_whitespace()
                .filter(|word| *word != "\\")
                .skip_while(|word| *word != "af")
                .collect();
            assert_eq!(
                words,
                [
                    "af",
                    "provider",
                    "setup",
                    "claude-main",
                    "--kind",
                    "claude",
                    "--login"
                ],
                "{width}\n{text}"
            );
            assert!(
                text.contains("export PATH=\"$HOME/.local/bin:$PATH\""),
                "{width}\n{text}"
            );
        }
        // At 100 columns what fits a line is never broken, and a path too long for one is
        // broken only after a `/`.
        let text = render(&checks, 100);
        assert!(
            text.contains(
                "fix: af provider detect --apply\n                 hint: it registers the logins \
                 it finds; or, at your terminal:\n                       af provider setup \
                 claude-main --kind claude --login\n"
            ),
            "{text}"
        );
        assert!(
            text.lines()
                .filter(|line| line.contains("/private/var/"))
                .all(|line| line.ends_with('/') || line.contains(".local/bin")),
            "{text}"
        );
        let narrow = render(&checks, 80);
        assert!(
            narrow.contains(
                "fix: export PATH=\"$HOME/.local/bin:$PATH\"\n                 hint: add that \
                 line to ~/.zshrc, then open a new shell\n"
            ),
            "the command whole on its line, its hint under it:\n{narrow}"
        );
        // Every line of a comment that must be broken is a comment.
        assert_eq!(
            layout(
                "fix: ",
                "export A=1   # in ~/.zshrc, then open a new shell",
                24,
                5,
                true
            ),
            "fix: export A=1\n     # in ~/.zshrc, then\n     # open a new shell"
        );
    }

    fn ambient(dir: &str, status: &str) -> crate::providers::ProviderStatus {
        crate::providers::ProviderStatus {
            id: "codex-ambient".into(),
            kind: "codex".into(),
            auth_context: dir.into(),
            source: "ambient CLI candidate; unstable local context label".into(),
            status: status.into(),
            auth_type: "-".into(),
            subscription: "-".into(),
            limits: Vec::new(),
            usage: crate::providers::UsageState::NotRequested,
            detail: String::new(),
            disabled_until: None,
        }
    }

    /// Issue #358: a directory the names-only scan found is called logged in or logged out only
    /// when the status probe doctor ran answered for it, and `not probed` otherwise; only a
    /// logged-in one is named with the `--apply` that registers it, so the line never promises a
    /// registration that detect leaves undone.
    #[test]
    fn found_directories_say_only_what_the_probe_already_learned() {
        assert_eq!(found_unregistered(&[], &[]), "");
        let found = [
            PathBuf::from("/w/codex-home"),
            PathBuf::from("/w/codex-spare"),
            PathBuf::from("/w/codex-team"),
        ];
        let logged_out = ambient("/w/codex-home", "not authenticated");
        let text = found_unregistered(&found, &[&logged_out]);
        assert_eq!(
            text,
            "; optional, found but not registered: /w/codex-home (logged out), /w/codex-spare, \
             /w/codex-team (not probed); to inspect the not-probed ones: af provider detect, \
             which asks a CLI only about a safe directory holding a login marker; to also ask \
             about a safe empty one: af provider detect --all; an unsafe or missing one is \
             never asked"
        );
        assert!(!text.contains("--apply"), "{text}");
        // Detect is an inspection, not a promise that every directory's CLI is asked.
        assert!(!text.contains("ask their CLI"), "{text}");

        let logged_in = ambient("/w/codex-home", "authenticated");
        assert_eq!(
            found_unregistered(&found[..1], &[&logged_in]),
            "; optional, found but not registered: /w/codex-home (logged in); to register the \
             logged-in ones: af provider detect --apply"
        );
        // An answer that is not a login state is no claim about one.
        let unprobed = ambient("/w/codex-home", "unavailable");
        assert_eq!(
            found_unregistered(&found[..1], &[&unprobed]),
            format!(
                "; optional, found but not registered: /w/codex-home (not probed){UNPROBED_GUIDANCE}"
            )
        );
        // Only logged-out ones: nothing to run.
        assert_eq!(
            found_unregistered(&found[..1], &[&logged_out]),
            "; optional, found but not registered: /w/codex-home (logged out)"
        );

        // In a row, the command stays whole on its line at every width.
        let check = Check::ok(
            "providers",
            format!("claude-main (claude, authenticated), codex-main (codex, authenticated){text}"),
        );
        for width in [100, 80, 60] {
            let rendered = render(std::slice::from_ref(&check), width);
            for command in ["af provider detect,", "af provider detect --all;"] {
                assert!(
                    rendered.lines().any(|line| line.contains(command)),
                    "{width}: {command}\n{rendered}"
                );
            }
            for line in rendered.lines() {
                assert!(
                    line.chars().count() <= width,
                    "{width}: `{line}`\n{rendered}"
                );
            }
        }
    }

    /// GA second fix item 4: the `af` fixes of the states an integration fixture cannot reach
    /// (a pinned release, a receipt verified by digest only, a Provider whose login lapsed, a
    /// newer release, a full disk) parse with the CLI's own parser, each command of a fix.
    #[test]
    fn every_af_fix_parses_with_the_cli() {
        use clap::Parser as _;
        let registry = crate::providers::RegistryRead {
            path: None,
            registered: Vec::new(),
            warning: None,
            unregistered: Vec::new(),
            candidates: Vec::new(),
        };
        let entry = crate::providers::Declared {
            id: "claude-main".into(),
            kind: "claude",
            auth_dir: None,
        };
        let registered = [
            "not authenticated",
            NOT_PROBED,
            "disabled",
            "installation failed",
            "incompatible",
            "unavailable",
        ]
        .map(|status| registered_problem(&entry, status, "claude-main".into()).fix);
        let fixes = [
            providers(&registry).fix.unwrap(),
            registered[0].clone().unwrap(),
            registered[1].clone().unwrap(),
            registered[2].clone().unwrap(),
            "af self remove 0.1.0 && af self install 0.1.0".into(),
            "af onboard --repo '/w x' --refresh-lock --af 0.1.0".into(),
            "af provider setup claude-main --kind claude --login".into(),
            "af self update".into(),
            "af self update --check".into(),
            "af self status --refresh".into(),
            "af self status".into(),
            "af storage prune --apply".into(),
            "af config show --origin".into(),
            "af provider status".into(),
            "af self setup-shell --shell powershell --write".into(),
        ];
        // No login fixes a CLI af cannot start or use: those rows offer no command.
        assert!(
            registered[3..].iter().all(Option::is_none),
            "{registered:?}"
        );
        for fix in fixes {
            let words = shell_words::split(&fix).unwrap();
            for command in words.split(|word| word == "&&") {
                assert_eq!(command[0], "af", "{fix}");
                if let Err(error) = crate::cli::Af::try_parse_from(command) {
                    panic!("{fix}: {error}");
                }
            }
        }
    }

    /// GA third fix item 3: a file that does not parse is named with the line its TOML error
    /// names, and one that parses but is not what its reader expects keeps the reader's words.
    #[test]
    fn a_problem_names_the_file_and_the_parse_errors_line() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config.toml");
        std::fs::write(&file, "[self]\nchannel = \"rc\"\n\nx = [\n").unwrap();
        let problem = Problem::at(&file, "unused");
        assert_eq!(problem.line, Some(4), "{problem:?}");
        assert!(problem.message.contains("unclosed array"), "{problem:?}");
        assert_eq!(
            problem.hint("af doctor"),
            format!(
                "correct {} line 4 by hand ({}), then recheck: af doctor",
                file.display(),
                problem.message
            )
        );
        // A configuration refusal names the file; the line is read from the file itself.
        let refusal = crate::config::Refusal::naming("", &file, ": unclosed array");
        assert_eq!(Problem::of_refusal(&refusal), problem);
        // TOML that parses keeps what its reader said, on one line, and names no line.
        std::fs::write(&file, "version = 2\n").unwrap();
        let schema = Problem::at(&file, "expected version 1\n  and more");
        assert_eq!(schema.line, None);
        assert_eq!(schema.message, "expected version 1");
        assert_eq!(
            schema.place().unwrap(),
            file.display().to_string(),
            "{schema:?}"
        );
        assert_eq!(
            Problem::said("HOME is not set").hint("af doctor"),
            "HOME is not set; correct it, then recheck: af doctor"
        );
    }

    /// GA third fix item 3: the `.af/` file a project check names is the first one `af onboard`
    /// reads that is not TOML: the project file, the lock, a pipeline, a Worker package.
    #[test]
    fn the_af_file_that_does_not_parse_is_named_in_onboards_order() {
        let root = tempfile::tempdir().unwrap();
        let af = root.path().join(".af");
        for dir in ["pipelines", "workers/correctness"] {
            std::fs::create_dir_all(af.join(dir)).unwrap();
        }
        let files = [
            "af.toml",
            "af.lock",
            "pipelines/review.toml",
            "workers/correctness/reviewer.toml",
        ];
        for file in files {
            std::fs::write(af.join(file), "version = 1\n").unwrap();
        }
        std::fs::write(af.join("README.md"), "x = [\n").unwrap();
        assert_eq!(authority_problem(root.path()), None, "only TOML is read");
        for file in files.iter().rev() {
            std::fs::write(af.join(file), "version = 1\nx = [\n").unwrap();
            let problem = authority_problem(root.path()).unwrap();
            assert_eq!(problem.file.as_deref(), Some(af.join(file).as_path()));
            assert_eq!(problem.line, Some(2), "{problem:?}");
        }
    }

    /// GA third fix item 3: a fix that runs a command an unreadable file stops would fail with
    /// that file's error, so it is withheld, per check kind: the binary's and the project's
    /// `af self` commands and the storage and update checks' own commands stop on the machine
    /// configuration, and the project's `af provider detect --apply` on the Provider registry.
    /// A command neither file stops keeps its fix.
    #[test]
    fn a_fix_that_an_unreadable_file_stops_is_withheld() {
        let config = Problem {
            file: Some(PathBuf::from("/h/.config/af/config.toml")),
            line: Some(2),
            message: "unclosed array".into(),
        };
        let registry = Problem {
            file: Some(PathBuf::from("/h/.config/af/providers.toml")),
            line: None,
            message: "expected version 1".into(),
        };
        let fixes = || {
            vec![
                Check::warn("binary", "no receipt", "af self install 0.1.0"),
                Check::warn(
                    "binary",
                    "digest only",
                    "brew install minisign && af self remove 0.1.0 && af self install 0.1.0",
                )
                .hint("reinstalls the release"),
                Check::warn("update", "failed", "af self status --refresh"),
                Check::warn("update", "newer", "af self update"),
                Check::warn("storage", "over budget", "af storage prune --apply"),
                Check::warn("storage", "staging", "af self status"),
                Check::warn("project", "no Provider", "af provider detect --apply"),
                Check::warn(
                    "providers",
                    "lapsed",
                    "af provider setup claude-main --kind claude --login",
                ),
                Check::warn(
                    "shell",
                    "no completions",
                    "af self setup-shell --shell zsh --write",
                ),
                Check::warn(
                    "project",
                    "unbound",
                    "af onboard --repo /r --refresh-lock --af 0.1.0",
                ),
                Check::warn("path", "off PATH", "export PATH='/h/.local/bin':\"$PATH\""),
                Check::warn("tools", "missing", "brew install curl"),
            ]
        };
        let ids = |checks: &[Check]| -> Vec<(&'static str, bool)> {
            checks
                .iter()
                .map(|check| (check.id, check.fix.is_some()))
                .collect()
        };
        let mut checks = fixes();
        withhold(&mut checks, &[(Reads::MachineConfig, &config)]);
        assert_eq!(
            ids(&checks),
            [
                ("binary", false),
                ("binary", false),
                ("update", false),
                ("update", false),
                ("storage", false),
                ("storage", false),
                ("project", true),
                ("providers", true),
                ("shell", true),
                ("project", true),
                ("path", true),
                ("tools", true),
            ]
        );
        assert_eq!(
            checks[1].hint.as_deref().unwrap(),
            "first correct /h/.config/af/config.toml line 2 by hand (unclosed array), which stops \
             this fix; then run: brew install minisign && af self remove 0.1.0 && af self install \
             0.1.0; reinstalls the release"
        );
        let mut checks = fixes();
        withhold(&mut checks, &[(Reads::Registry, &registry)]);
        let withheld: Vec<&str> = checks
            .iter()
            .filter(|check| check.fix.is_none())
            .map(|check| check.id)
            .collect();
        assert_eq!(withheld, ["project", "providers"]);
        assert_eq!(
            checks[6].hint.as_deref().unwrap(),
            "first correct /h/.config/af/providers.toml by hand (expected version 1), which stops \
             this fix; then run: af provider detect --apply"
        );
        // Nothing unreadable: every fix stays.
        let mut checks = fixes();
        withhold(&mut checks, &[]);
        assert!(checks.iter().all(|check| check.fix.is_some()));
        // `af provider detect` without --apply reads no registry it could be stopped by.
        let scan = ["af", "provider", "detect"].map(String::from);
        assert!(reads(&scan).is_empty());
    }

    #[test]
    fn a_gate_names_the_tools_it_needs() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Makefile"),
            "check:\n\tcargo nextest run\n\tnpx --yes markdownlint-cli2 README.md\n\tpython3 scripts/x.py\n",
        )
        .unwrap();
        let authority = crate::onboard::AuthorityFacts {
            gates: vec![("check".into(), "make".into(), vec!["check".into()])],
            container: true,
            workers: Vec::new(),
        };
        let needs: Vec<&str> = gate_needs(root.path(), &authority)
            .iter()
            .map(|need| need.program)
            .collect();
        assert_eq!(
            needs,
            [
                "make",
                "cargo",
                "cc",
                "cargo-nextest",
                "npx",
                "python3",
                "docker|podman"
            ]
        );
    }
}
