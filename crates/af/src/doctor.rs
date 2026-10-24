//! `af doctor`: a token-free health check of this machine and the repository it runs in.
//!
//! Each check reads what the command that owns the thing reads (`af self status`, `af provider
//! status`, `af storage`, `af onboard`) and ends `ok`, `warn` or `fail` with the one command that
//! fixes it, exactly as a shell runs it, and a hint with whatever else the operator needs to know:
//! a placeholder's choices, a step to take first. Nothing here calls a model, logs in, installs or writes a file: the only processes it
//! starts are version checks, the Provider status probe and one anonymous HTTPS request to the
//! release source while update checks are on (ADR-0204). The charged end-to-end Provider check
//! stays `af provider doctor`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::selfmgmt;

/// The `--json` document's kind.
pub(crate) const SCHEMA: &str = "af/doctor@2";
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

/// One piece of a row's text: a word, which may be broken when it is a path too long for any
/// line, or a command, kept whole.
enum Unit {
    Word(String),
    Command(String),
    /// A shell comment to the end of the text: every line of it starts with `#`.
    Comment(Vec<String>),
}

/// `text` as words and commands. A command starts at one of [`COMMANDS`] — at the start of a
/// fix (`leading`), or after a word ending in `:`, `,` or `;`, or `then`, `or`, `&&` — and runs
/// to a word ending in `,` or `;` (kept), or before `&&`, `||`, a word opening `(` or a shell
/// comment. A span between backticks is one word.
fn units(text: &str, leading: bool) -> Vec<Unit> {
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
                        lines.push(std::mem::take(&mut line));
                        fitted = margin;
                    }
                }
                let mut parts: Vec<String> = fitted.lines().map(str::to_string).collect();
                line = parts.pop().unwrap_or_default();
                lines.extend(parts);
                fresh = false;
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
    let toplevel = crate::config::git_toplevel(repo);
    let authority = toplevel
        .as_deref()
        .filter(|toplevel| toplevel.join(".af").exists())
        .map(crate::onboard::authority_facts);
    let mut checks = vec![
        binary(&installation),
        path(&installation),
        shell(),
        update(),
        tools(
            toplevel.as_deref(),
            authority.as_ref().and_then(|a| a.as_ref().ok()),
        ),
        providers(&registry),
        storage(&installation),
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
    checks
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
        let check = Check::warn(
            "path",
            format!(
                "{} is not on PATH, so `af` there is not found",
                dir.display()
            ),
            fix,
        );
        return match hint {
            Some(hint) => check.hint(hint),
            None => check,
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
            let hint = match hint {
                Some(hint) => format!("{before}; {hint}; or remove {}", found.display()),
                None => format!("{before}; or remove {}", found.display()),
            };
            Check::warn(
                "path",
                format!(
                    "`af` on PATH is {}, not the default link {}",
                    found.display(),
                    bin.display()
                ),
                fix,
            )
            .hint(hint)
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
/// is on PATH already, behind another `af`), and where that line belongs for a later shell.
pub(crate) fn path_fix(dir: &Path, shell: Option<&str>, front: bool) -> (String, Option<String>) {
    let dir = dir.display().to_string();
    let name = shell
        .and_then(|shell| Path::new(shell).file_name())
        .and_then(|name| name.to_str());
    let export = format!("export PATH=\"{dir}:$PATH\"");
    match name {
        Some("fish") if front => (
            format!("fish_add_path --move {}", shell_words::quote(&dir)),
            None,
        ),
        Some("fish") => (format!("fish_add_path {}", shell_words::quote(&dir)), None),
        Some("zsh") => (
            export,
            Some("add that line to ~/.zshrc, then open a new shell".into()),
        ),
        Some("bash") => (
            export,
            Some("add that line to ~/.bashrc, then open a new shell".into()),
        ),
        _ => (
            export,
            Some("add that line to your shell's startup file, then open a new shell".into()),
        ),
    }
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

fn update() -> Check {
    let facts = match selfmgmt::update_facts() {
        Ok(facts) => facts,
        Err(error) => {
            return Check::warn(
                "update",
                format!("the [self] policy cannot be read: {error}"),
                "af config show --origin",
            )
            .hint("it names the layer that sets [self]");
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
        return Check::fail(
            "providers",
            format!("the Provider registry cannot be read: {warning}"),
            "af provider status",
        )
        .hint("it names the problem; fix or move the registry file");
    }
    // The same names-only scan `af provider status` and `af onboard` list (ADR-0200).
    let found: Vec<String> = registry
        .unregistered
        .iter()
        .map(|dir| crate::providers::detect::display_path(dir))
        .collect();
    let found_text = if found.is_empty() {
        String::new()
    } else {
        format!("; found but not registered: {}", found.join(", "))
    };
    if registry.registered.is_empty() {
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
            "it registers the logins it finds; or, at your terminal: af provider setup \
             {kind}-main --kind {kind} --login"
        ));
    }
    let statuses = crate::providers::probed_statuses();
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for entry in &registry.registered {
        let status = statuses
            .iter()
            .find(|status| status.id == entry.id)
            .map_or("not probed", |status| status.status.as_str());
        let line = format!("{} ({}, {status})", entry.id, entry.kind);
        if status == "authenticated" {
            good.push(line);
        } else {
            bad.push((entry, line));
        }
    }
    let mut detail = good
        .iter()
        .chain(bad.iter().map(|(_, line)| line))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    detail.push_str(&found_text);
    match bad.first() {
        Some((entry, _)) => Check::warn(
            "providers",
            detail,
            format!(
                "af provider setup {} --kind {} --login",
                shell_words::quote(&entry.id),
                entry.kind
            ),
        )
        .hint("at your terminal, where it logs in; af provider status shows why first"),
        None if !found.is_empty() => Check::warn("providers", detail, "af provider detect --apply")
            .hint("it registers what it finds"),
        None => Check::ok("providers", detail),
    }
}

fn storage(installation: &Result<selfmgmt::Installation, String>) -> Check {
    let policy = match crate::storage::policy() {
        Ok(policy) => policy,
        Err(error) => {
            return Check::fail(
                "storage",
                format!("[storage] cannot be read: {error}"),
                "af config show --origin",
            )
            .hint("it names the layer that sets [storage]");
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
        Err(error) => {
            return Check::fail(
                "project",
                format!("{repo}/.af does not validate: {error}"),
                onboard,
            )
            .hint("it names the file and the fix");
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
    #[test]
    fn the_path_fix_is_the_line_for_the_users_shell() {
        let dir = Path::new("/home/o/.local/bin");
        assert_eq!(
            path_fix(dir, Some("/usr/bin/fish"), false),
            ("fish_add_path /home/o/.local/bin".into(), None)
        );
        assert_eq!(
            path_fix(dir, Some("/usr/bin/fish"), true).0,
            "fish_add_path --move /home/o/.local/bin"
        );
        let export = "export PATH=\"/home/o/.local/bin:$PATH\"";
        for (shell, file) in [
            (Some("/bin/zsh"), "~/.zshrc"),
            (Some("/bin/bash"), "~/.bashrc"),
            (None, "your shell's startup file"),
        ] {
            let (fix, hint) = path_fix(dir, shell, false);
            assert_eq!(fix, export);
            assert_eq!(
                hint.unwrap(),
                format!("add that line to {file}, then open a new shell")
            );
        }
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
        let fixes = [
            providers(&registry).fix.unwrap(),
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
