//! `af doctor`: a token-free health check of this machine and the repository it runs in.
//!
//! Each check reads what the command that owns the thing reads (`af self status`, `af provider
//! status`, `af storage`, `af onboard`) and ends `ok`, `warn` or `fail` with the one command that
//! fixes it. Nothing here calls a model, logs in, installs or writes a file: the only processes it
//! starts are version checks, the Provider status probe and one anonymous HTTPS request to the
//! release source while update checks are on (ADR-0201). The charged end-to-end Provider check
//! stays `af provider doctor`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::selfmgmt;

/// The `--json` document's kind.
pub(crate) const SCHEMA: &str = "af/doctor@1";
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

/// One check's outcome: what was found, and the one-line fix when it is not `ok`.
#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub(crate) id: &'static str,
    pub(crate) status: Status,
    pub(crate) detail: String,
    pub(crate) fix: Option<String>,
}

impl Check {
    fn ok(id: &'static str, detail: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(id: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            id,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
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
        print!("{}", render(&checks));
    }
    Ok(code)
}

fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|check| check.status == Status::Fail))
}

/// The text form: the first line, one line per check, its fix under it, and a summary.
fn render(checks: &[Check]) -> String {
    let mut text = format!("{FIRST_LINE}\n");
    for check in checks {
        text.push_str(&format!(
            "{:<5} {:<10} {}\n",
            check.status.word(),
            check.id,
            check.detail
        ));
        if let Some(fix) = &check.fix {
            text.push_str(&format!("{:<16} fix: {fix}\n", ""));
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
        None if repo != Path::new(".") => checks.push(Check::warn(
            "project",
            format!("--repo {} is not inside a git repository", repo.display()),
            "pass --repo a git checkout, or run af doctor inside one",
        )),
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
                "set HOME (or XDG_DATA_HOME and XDG_BIN_HOME) to an absolute path",
            );
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
        Some(receipt) => Check::warn(
            "binary",
            format!(
                "af {version} at {running} was verified by {} only, not by the release signature; {default}",
                receipt.verified_by
            ),
            format!("install minisign, then af self remove {version} && af self install {version}"),
        ),
    }
}

fn path(installation: &Result<selfmgmt::Installation, String>) -> Check {
    let Ok(installation) = installation else {
        return Check::warn(
            "path",
            "the default link's directory cannot be named",
            "set HOME (or XDG_BIN_HOME) to an absolute path",
        );
    };
    let bin = &installation.bin;
    let Some(dir) = bin.parent() else {
        return Check::warn(
            "path",
            format!("{} has no directory", bin.display()),
            "set XDG_BIN_HOME to an absolute directory",
        );
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if !dirs.iter().any(|entry| same_dir(entry, dir)) {
        return Check::warn(
            "path",
            format!(
                "{} is not on PATH, so `af` there is not found",
                dir.display()
            ),
            path_fix(dir, std::env::var("SHELL").ok().as_deref()),
        );
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
        Some(found) => Check::warn(
            "path",
            format!(
                "`af` on PATH is {}, not the default link {}",
                found.display(),
                bin.display()
            ),
            format!(
                "put {} before {} on PATH, or remove {}",
                dir.display(),
                found.parent().unwrap_or(Path::new("/")).display(),
                found.display()
            ),
        ),
        None => Check::warn(
            "path",
            format!("{} is on PATH but holds no af", dir.display()),
            format!("af self install {}", selfmgmt::VERSION),
        ),
    }
}

/// The line that puts `dir` on PATH for the shell `$SHELL` names.
pub(crate) fn path_fix(dir: &Path, shell: Option<&str>) -> String {
    let dir = dir.display();
    let name = shell
        .and_then(|shell| Path::new(shell).file_name())
        .and_then(|name| name.to_str());
    match name {
        Some("fish") => format!("fish_add_path {dir}"),
        Some("zsh") => {
            format!("export PATH=\"{dir}:$PATH\"   # in ~/.zshrc, then open a new shell")
        }
        Some("bash") => {
            format!("export PATH=\"{dir}:$PATH\"   # in ~/.bashrc, then open a new shell")
        }
        _ => format!("add {dir} to PATH in your shell's startup file"),
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
            "af self setup-shell --shell bash|zsh|fish|elvish|powershell --write",
        );
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
                "set HOME to an absolute path",
            );
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
        Check::warn("shell", missing.join("; "), "af self setup-shell --write")
    }
}

fn update() -> Check {
    let facts = match selfmgmt::update_facts() {
        Ok(facts) => facts,
        Err(error) => {
            return Check::warn(
                "update",
                format!("the [self] policy cannot be read: {error}"),
                "af config show --origin names the layer that sets it",
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
            "af self status --refresh (runs the release check again now)",
        );
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
        Err(why) => Check::warn(
            "update",
            format!("{checked}; {policy_text}; {why}"),
            if why.contains("gh auth login") {
                "gh auth login (an account that can read the release repository)"
            } else {
                "check the network, then af self update --check"
            },
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

fn tools(toplevel: Option<&Path>, authority: Option<&crate::onboard::AuthorityFacts>) -> Check {
    let mut found = Vec::new();
    let mut problems: Vec<(bool, String, String)> = Vec::new();
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
            install_line("git"),
        )),
        None => problems.push((true, "git is not installed".into(), install_line("git"))),
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
                    install_line("docker"),
                ));
            }
        } else if on_path(need.program) {
            found.push(need.program.to_string());
        } else {
            problems.push((
                need.fatal,
                format!("{} is not installed: {}", need.program, need.why),
                install_line(need.program),
            ));
        }
    }
    if problems.is_empty() {
        return Check::ok("tools", found.join(", "));
    }
    let fatal = problems.iter().any(|(fatal, _, _)| *fatal);
    let mut details: Vec<String> = problems
        .iter()
        .map(|(_, detail, _)| detail.clone())
        .collect();
    if !found.is_empty() {
        details.push(format!("found {}", found.join(", ")));
    }
    let detail = details.join("; ");
    let fix = problems
        .iter()
        .map(|(_, _, fix)| fix.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if fatal {
        Check::fail("tools", detail, fix)
    } else {
        Check::warn("tools", detail, fix)
    }
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

/// The one line that installs `program` with this machine's package manager.
fn install_line(program: &str) -> String {
    let manager = ["brew", "apt-get", "dnf", "pacman", "apk"]
        .into_iter()
        .find(|manager| on_path(manager));
    let package = match (program, manager) {
        ("cargo-nextest", _) => return "cargo install cargo-nextest --locked".into(),
        ("cargo", _) => return "install Rust from https://rustup.rs".into(),
        ("cc", Some("brew")) => return "xcode-select --install".into(),
        ("cc" | "make", Some("apt-get")) => "build-essential",
        ("cc", _) => "gcc",
        ("npx", Some("brew")) => "node",
        ("npx", Some("apt-get" | "dnf")) => "npm",
        ("npx", _) => "nodejs",
        ("python3", Some("brew")) => "python",
        ("docker", Some("brew")) => return "brew install --cask docker".into(),
        ("docker", _) => "podman",
        (other, _) => other,
    };
    match manager {
        Some("brew") => format!("brew install {package}"),
        Some("apt-get") => format!("sudo apt install {package}"),
        Some("dnf") => format!("sudo dnf install {package}"),
        Some("pacman") => format!("sudo pacman -S {package}"),
        Some("apk") => format!("apk add {package}"),
        _ => format!("install {package} with your package manager"),
    }
}

// ------------------------------------------------------------------------------------------
// providers · storage · project

fn providers(registry: &crate::providers::RegistryRead) -> Check {
    if let Some(warning) = &registry.warning {
        return Check::fail(
            "providers",
            format!("the Provider registry cannot be read: {warning}"),
            "af provider status names the problem; fix or move the registry file",
        );
    }
    // The same names-only scan `af provider status` and `af onboard` list (ADR-0197).
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
            format!(
                "af provider detect --apply, or at your terminal: af provider setup {kind}-main --kind {kind} --login"
            ),
        );
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
                "af provider status shows why; at your terminal: af provider setup {} --kind {} --login",
                entry.id, entry.kind
            ),
        ),
        None if !found.is_empty() => Check::warn(
            "providers",
            detail,
            "af provider detect --apply registers what it finds",
        ),
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
                "af config show --origin names the layer that sets it",
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
        return Check::fail(
            "storage",
            detail,
            format!(
                "free disk at {} above {}: af storage prune --apply reclaims what af keeps",
                path.display(),
                human(policy.min_free_bytes)
            ),
        );
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
        return Check::warn(
            "storage",
            detail,
            "af self status (any af self command recovers them)",
        );
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
    Check::warn(
        "storage",
        detail,
        "add them in System Settings > Spotlight > Search Privacy, or point [storage] \
         build_cache_root and sandbox_root at directories named *.noindex (docs/README.md, \
         macOS)",
    )
}

fn project(
    toplevel: &Path,
    authority: Option<&Result<crate::onboard::AuthorityFacts, String>>,
    registry: &crate::providers::RegistryRead,
) -> Check {
    let repo = toplevel.display();
    let Some(authority) = authority else {
        return Check::warn(
            "project",
            format!("{repo} has no .af/ review authority"),
            format!(
                "af onboard --repo {} (a token-free preview)",
                shell_words::quote(&repo.to_string())
            ),
        );
    };
    let authority = match authority {
        Ok(authority) => authority,
        Err(error) => {
            return Check::fail(
                "project",
                format!("{repo}/.af does not validate: {error}"),
                "af onboard (it names the file and the fix)",
            );
        }
    };
    let mut parts = vec![format!("{repo}/.af validates")];
    let mut warns: Vec<String> = Vec::new();
    let mut fails: Vec<String> = Vec::new();
    match selfmgmt::pin_here(toplevel) {
        Ok(None) => parts.push("the lock pins no af release".into()),
        Ok(Some(pin)) => {
            parts.push(format!("{} pins af {}", pin.lock.display(), pin.version));
            if !pin.bound {
                warns.push(format!(
                    "af onboard --refresh-lock --af {} (the lock binds no bytes for this target)",
                    pin.version
                ));
            }
            if !pin.installed {
                warns.push(format!(
                    "af self install {} (the pinned release is not installed)",
                    pin.version
                ));
            } else if pin.matches == Some(false) {
                fails.push(format!(
                    "af self remove {0} && af self install {0} (the installed bytes are not the ones the lock pins)",
                    pin.version
                ));
            }
        }
        Err(why) => fails.push(format!(
            "fix {} ({why})",
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
            fails.push(format!(
                "{} (Gate {name} runs {program}, which is not found)",
                install_line(program)
            ));
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
            None => warns.push(format!(
                "af provider detect --apply, or af provider setup {kind}-main --kind {kind} --login ({node} needs a {kind} Provider)"
            )),
        }
    }
    let detail = parts.join("; ");
    if !fails.is_empty() {
        let fix = fails
            .into_iter()
            .chain(warns)
            .collect::<Vec<_>>()
            .join("; ");
        Check::fail("project", detail, fix)
    } else if !warns.is_empty() {
        Check::warn("project", detail, warns.join("; "))
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
        assert!(check.fix.unwrap().contains("Search Privacy"));
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

    #[test]
    fn the_path_fix_is_the_line_for_the_users_shell() {
        let dir = Path::new("/home/o/.local/bin");
        assert_eq!(
            path_fix(dir, Some("/usr/bin/fish")),
            "fish_add_path /home/o/.local/bin"
        );
        assert!(path_fix(dir, Some("/bin/zsh")).contains("~/.zshrc"));
        assert!(
            path_fix(dir, Some("/bin/bash"))
                .starts_with("export PATH=\"/home/o/.local/bin:$PATH\"")
        );
        assert_eq!(
            path_fix(dir, None),
            "add /home/o/.local/bin to PATH in your shell's startup file"
        );
    }

    #[test]
    fn only_a_failed_check_fails_the_command_and_the_text_starts_with_the_promise() {
        let checks = vec![
            Check::ok("binary", "af"),
            Check::warn("path", "off PATH", "fish_add_path /x"),
        ];
        assert_eq!(exit_code(&checks), 0);
        let text = render(&checks);
        assert_eq!(text.lines().next(), Some(FIRST_LINE));
        assert!(text.contains("fix: fish_add_path /x"), "{text}");
        assert!(text.ends_with("nothing failed (1 warning(s))\n"), "{text}");
        let failed = vec![Check::fail(
            "tools",
            "git is not installed",
            "brew install git",
        )];
        assert_eq!(exit_code(&failed), 1);
        assert!(render(&failed).contains("1 check(s) failed"));
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
