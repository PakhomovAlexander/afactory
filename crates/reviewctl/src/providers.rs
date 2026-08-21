//! Machine-local provider inventory for the TUI.
//!
//! Providers are display-only in this iteration. The registry names auth directories, never
//! credentials, arbitrary commands, arguments, or environment variables. Status is obtained from
//! the two fixed adapter CLIs with bounded output and wall time. The accepted response shapes are
//! pinned by fixtures captured from Claude Code 2.1.238 and codex-cli 0.149.0 on 2026-08-21.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_PROVIDERS: usize = 32;
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_CONCURRENT_PROBES: usize = 4;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ProviderInventory {
    pub providers: Vec<ProviderStatus>,
    pub registry: Option<PathBuf>,
    pub warning: Option<String>,
}

pub struct ProviderStatus {
    pub id: String,
    pub kind: String,
    pub command: String,
    pub auth_context: String,
    pub source: String,
    pub status: String,
    pub auth_type: String,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProviderKind {
    Claude,
    Codex,
}

impl ProviderKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(format!("unsupported provider kind `{value}`")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn command(self) -> &'static str {
        self.name()
    }
}

#[derive(Clone)]
struct ProviderSpec {
    id: String,
    kind: ProviderKind,
    auth_dir: Option<PathBuf>,
    explicit_selector: bool,
    source: String,
}

pub fn discover() -> ProviderInventory {
    let (specs, registry, warning) = load_specs();
    ProviderInventory {
        providers: specs.iter().map(unprobed_status).collect(),
        registry,
        warning,
    }
}

pub fn discover_with_cancel(cancelled: &AtomicBool) -> ProviderInventory {
    let (specs, registry, warning) = load_specs();
    let mut providers = Vec::with_capacity(specs.len());
    for chunk in specs.chunks(MAX_CONCURRENT_PROBES) {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .cloned()
                .map(|spec| {
                    let fallback = spec.clone();
                    (
                        fallback,
                        scope.spawn(move || probe_provider(spec, cancelled)),
                    )
                })
                .collect();
            providers.extend(handles.into_iter().map(|(spec, handle)| {
                handle
                    .join()
                    .unwrap_or_else(|_| unavailable_status(&spec, "provider status probe panicked"))
            }));
        });
    }
    if cancelled.load(Ordering::Acquire) {
        providers.extend(specs[providers.len()..].iter().map(unprobed_status));
    }
    ProviderInventory {
        providers,
        registry,
        warning,
    }
}

fn load_specs() -> (Vec<ProviderSpec>, Option<PathBuf>, Option<String>) {
    let (registry, path_warning) = match registry_path() {
        Ok(path) => (path, None),
        Err(error) => (None, Some(error)),
    };
    let (mut specs, load_warning) = match registry.as_deref() {
        Some(path) => match read_registry(path) {
            Ok(Some(text)) => match parse_registry(&text, path) {
                Ok(specs) => (specs, None),
                Err(_) => (
                    Vec::new(),
                    Some(format!(
                        "provider registry {} is invalid; expected version 1 and [[providers]] entries with id, kind, and auth_dir",
                        path.display()
                    )),
                ),
            },
            Ok(None) => (Vec::new(), None),
            Err(error) => (Vec::new(), Some(error)),
        },
        _ => (Vec::new(), None),
    };
    let warning = path_warning.or(load_warning);

    for default in implicit_defaults() {
        let duplicate_context = specs.iter().any(|spec| same_context(spec, &default));
        if !duplicate_context && !specs.iter().any(|spec| spec.id == default.id) {
            specs.push(default);
        }
    }
    specs.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));

    (specs, registry, warning)
}

fn registry_path() -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("REVIEWCTL_PROVIDERS_FILE") {
        if path.is_empty() {
            return Err("REVIEWCTL_PROVIDERS_FILE is empty".to_string());
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("REVIEWCTL_PROVIDERS_FILE must be absolute".to_string());
        }
        return Ok(Some(path));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        if !config.is_empty() {
            let config = PathBuf::from(config);
            if !config.is_absolute() {
                return Err("XDG_CONFIG_HOME must be absolute".to_string());
            }
            return Ok(Some(config.join("reviewctl/providers.toml")));
        }
    }
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/reviewctl/providers.toml"));
    if path.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err("HOME must be absolute to locate the provider registry".to_string());
    }
    Ok(path)
}

fn read_registry(path: &Path) -> Result<Option<String>, String> {
    let resolved = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot resolve provider registry {}: {error}",
                path.display()
            ));
        }
    };
    let file = open_registry(&resolved)
        .map_err(|error| format!("cannot read provider registry {}: {error}", path.display()))?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "cannot inspect provider registry {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "provider registry {} must resolve to a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(format!(
            "provider registry {} exceeds {MAX_REGISTRY_BYTES} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read provider registry {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(format!(
            "provider registry {} changed while reading or exceeds {MAX_REGISTRY_BYTES} bytes",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("provider registry {} is not UTF-8", path.display()))
}

#[cfg(unix)]
fn open_registry(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_registry(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

fn implicit_defaults() -> Vec<ProviderSpec> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let mut defaults = Vec::new();
    if resolve_program("claude").is_some() {
        defaults.push(ProviderSpec {
            id: "claude-ambient".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            explicit_selector: std::env::var_os("CLAUDE_CONFIG_DIR").is_some(),
            source: "ambient CLI candidate; unstable local context label".to_string(),
        });
    }
    if resolve_program("codex").is_some() {
        defaults.push(ProviderSpec {
            id: "codex-ambient".to_string(),
            kind: ProviderKind::Codex,
            auth_dir: canonical_if_present(home.map(|home| home.join(".codex"))),
            explicit_selector: false,
            source: "ambient CLI candidate; unstable local context label".to_string(),
        });
    }
    defaults
}

fn canonical_if_present(path: Option<PathBuf>) -> Option<PathBuf> {
    path.map(|path| fs::canonicalize(&path).unwrap_or(path))
}

fn same_context(left: &ProviderSpec, right: &ProviderSpec) -> bool {
    left.kind == right.kind
        && match (&left.auth_dir, &right.auth_dir) {
            (Some(left), Some(right)) => context_path(left) == context_path(right),
            (None, None) => true,
            _ => false,
        }
}

fn context_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn parse_registry(text: &str, path: &Path) -> Result<Vec<ProviderSpec>, String> {
    let document: toml::Value = text
        .parse()
        .map_err(|error| format!("provider registry {}: {error}", path.display()))?;
    let root = document
        .as_table()
        .ok_or_else(|| format!("provider registry {} is not a table", path.display()))?;
    reject_unknown(
        root.keys().map(String::as_str),
        &["version", "providers"],
        "registry",
    )?;
    if root.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(format!(
            "provider registry {} must declare version = 1",
            path.display()
        ));
    }
    let entries = match root.get("providers") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                format!(
                    "provider registry {} `providers` must be an array of tables",
                    path.display()
                )
            })?
            .as_slice(),
        None => &[],
    };
    if entries.len() > MAX_PROVIDERS {
        return Err(format!(
            "provider registry {} has {} entries; the limit is {MAX_PROVIDERS}",
            path.display(),
            entries.len()
        ));
    }

    let mut ids = BTreeSet::new();
    let mut contexts = BTreeSet::new();
    let mut specs = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let table = entry.as_table().ok_or_else(|| {
            format!(
                "provider registry {} entry {} is not a table",
                path.display(),
                index + 1
            )
        })?;
        reject_unknown(
            table.keys().map(String::as_str),
            &["id", "kind", "auth_dir"],
            "provider entry",
        )?;
        let id = table
            .get("id")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("provider entry {} has no string `id`", index + 1))?;
        safe_id(id)?;
        if matches!(id, "claude-ambient" | "codex-ambient") {
            return Err(format!(
                "provider id `{id}` is reserved for ambient discovery"
            ));
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("provider id `{id}` is duplicated"));
        }
        let kind = table
            .get("kind")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("provider `{id}` has no string `kind`"))
            .and_then(ProviderKind::parse)?;
        let auth_dir = table
            .get("auth_dir")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("provider `{id}` has no string `auth_dir`"))?;
        let auth_dir = PathBuf::from(auth_dir);
        if !auth_dir.is_absolute()
            || auth_dir
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return Err(format!(
                "provider `{id}` auth_dir must be an absolute path without `..`"
            ));
        }
        let auth_dir = fs::canonicalize(&auth_dir).map_err(|error| {
            format!(
                "provider `{id}` auth_dir {} cannot be resolved: {error}",
                auth_dir.display()
            )
        })?;
        if !auth_dir.is_dir() {
            return Err(format!(
                "provider `{id}` auth_dir {} is not a directory",
                auth_dir.display()
            ));
        }
        let context = (kind, auth_dir.clone());
        if !contexts.insert(context) {
            return Err(format!(
                "provider `{id}` duplicates another {} auth context",
                kind.name()
            ));
        }
        specs.push(ProviderSpec {
            id: id.to_string(),
            kind,
            auth_dir: Some(auth_dir),
            explicit_selector: true,
            source: path.display().to_string(),
        });
    }
    Ok(specs)
}

fn reject_unknown<'a>(
    actual: impl Iterator<Item = &'a str>,
    allowed: &[&str],
    label: &str,
) -> Result<(), String> {
    if let Some(field) = actual.into_iter().find(|field| !allowed.contains(field)) {
        return Err(format!("{label} contains unknown field `{field}`"));
    }
    Ok(())
}

fn safe_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        })
    {
        return Err(format!("provider id `{id}` is unsafe"));
    }
    Ok(())
}

fn probe_provider(spec: ProviderSpec, cancelled: &AtomicBool) -> ProviderStatus {
    if spec
        .auth_dir
        .as_ref()
        .is_some_and(|path| spec.explicit_selector && !path.is_absolute())
    {
        return unavailable_status(&spec, "provider auth selector must be absolute");
    }
    let Some(program) = resolve_program(spec.kind.command()) else {
        return unavailable_status(&spec, &format!("{} is not on PATH", spec.kind.command()));
    };
    let output = match run_probe(&program, &spec, cancelled) {
        Ok(output) => output,
        Err(error) => return unavailable_status(&spec, &error),
    };
    let (status, auth_type, detail) = match spec.kind {
        ProviderKind::Claude => parse_claude_status(output.status.success(), &output.stdout),
        ProviderKind::Codex => parse_codex_status(output.status.success(), &output.stdout),
    };
    ProviderStatus {
        id: spec.id,
        kind: spec.kind.name().to_string(),
        command: program.display().to_string(),
        auth_context: spec
            .auth_dir
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "CLI default".to_string()),
        source: spec.source,
        status,
        auth_type,
        detail,
    }
}

fn unprobed_status(spec: &ProviderSpec) -> ProviderStatus {
    ProviderStatus {
        id: spec.id.clone(),
        kind: spec.kind.name().to_string(),
        command: resolve_program(spec.kind.command())
            .unwrap_or_else(|| PathBuf::from(spec.kind.command()))
            .display()
            .to_string(),
        auth_context: spec
            .auth_dir
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "CLI default".to_string()),
        source: spec.source.clone(),
        status: "not probed".to_string(),
        auth_type: "-".to_string(),
        detail: "Open PROVIDERS or press R to refresh status".to_string(),
    }
}

fn unavailable_status(spec: &ProviderSpec, detail: &str) -> ProviderStatus {
    ProviderStatus {
        id: spec.id.clone(),
        kind: spec.kind.name().to_string(),
        command: spec.kind.command().to_string(),
        auth_context: spec
            .auth_dir
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "CLI default".to_string()),
        source: spec.source.clone(),
        status: "unavailable".to_string(),
        auth_type: "-".to_string(),
        detail: detail.to_string(),
    }
}

struct ProbeOutput {
    status: ExitStatus,
    stdout: String,
}

#[cfg(unix)]
fn run_probe(
    program: &Path,
    spec: &ProviderSpec,
    cancelled: &AtomicBool,
) -> Result<ProbeOutput, String> {
    let mut command = Command::new(program);
    match spec.kind {
        ProviderKind::Claude => {
            command.args(["auth", "status", "--json"]);
        }
        ProviderKind::Codex => {
            command.args(["login", "status"]);
        }
    }
    command
        .env_clear()
        .current_dir(Path::new("/"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    command.env("PATH", sanitized_path());
    if let Some(home) = std::env::var_os("HOME").filter(|value| Path::new(value).is_absolute()) {
        command.env("HOME", home);
    }
    if let Some(user) = std::env::var_os("USER") {
        command.env("USER", user);
    }
    if spec.explicit_selector
        && let Some(auth_dir) = &spec.auth_dir
    {
        command.env(
            match spec.kind {
                ProviderKind::Claude => "CLAUDE_CONFIG_DIR",
                ProviderKind::Codex => "CODEX_HOME",
            },
            auth_dir,
        );
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start provider status probe: {error}"))?;
    let mut stdout = child.stdout.take().expect("provider stdout was piped");
    if let Err(error) = set_nonblocking(&stdout) {
        stop_probe(&mut child);
        return Err(error);
    }
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT.min(4096));
    let mut exceeded = false;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Err(error) = drain_available(&mut stdout, &mut captured, &mut exceeded) {
            stop_probe(&mut child);
            return Err(error);
        }
        if exceeded {
            stop_probe(&mut child);
            return Err(format!(
                "provider status output exceeds {MAX_PROBE_OUTPUT} bytes"
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                stop_probe(&mut child);
                return Err(format!("provider status probe failed: {error}"));
            }
        }
        if cancelled.load(Ordering::Acquire) {
            stop_probe(&mut child);
            return Err("provider status refresh cancelled".to_string());
        }
        if Instant::now() >= deadline {
            stop_probe(&mut child);
            return Err(format!(
                "provider status probe timed out after {} seconds",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    terminate_probe_group(child.id());
    while drain_available(&mut stdout, &mut captured, &mut exceeded)? && !exceeded {}
    if exceeded {
        return Err(format!(
            "provider status output exceeds {MAX_PROBE_OUTPUT} bytes"
        ));
    }
    let stdout = String::from_utf8(captured)
        .map_err(|_| "provider status output is not UTF-8".to_string())?;
    Ok(ProbeOutput { status, stdout })
}

#[cfg(unix)]
fn stop_probe(child: &mut Child) {
    terminate_probe_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn run_probe(
    _program: &Path,
    _spec: &ProviderSpec,
    _cancelled: &AtomicBool,
) -> Result<ProbeOutput, String> {
    Err("provider probes require Unix process-group isolation".to_string())
}

#[cfg(unix)]
fn terminate_probe_group(process_group: u32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(process_group as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[cfg(unix)]
fn set_nonblocking(stdout: &impl std::os::fd::AsFd) -> Result<(), String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    fcntl(stdout, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
        .map(|_| ())
        .map_err(|error| format!("cannot make provider output nonblocking: {error}"))
}

fn drain_available(
    stdout: &mut impl Read,
    captured: &mut Vec<u8>,
    exceeded: &mut bool,
) -> Result<bool, String> {
    let mut chunk = [0_u8; 8192];
    match stdout.read(&mut chunk) {
        Ok(0) => Ok(false),
        Ok(count) => {
            let remaining = MAX_PROBE_OUTPUT.saturating_sub(captured.len());
            captured.extend_from_slice(&chunk[..count.min(remaining)]);
            *exceeded |= count > remaining;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(format!("cannot read provider status output: {error}")),
    }
}

fn sanitized_path() -> std::ffi::OsString {
    let directories = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|path| path.is_absolute())
        .filter_map(|path| fs::canonicalize(path).ok())
        .filter(|path| path.is_dir());
    std::env::join_paths(directories).unwrap_or_default()
}

fn parse_claude_status(success: bool, stdout: &str) -> (String, String, String) {
    if !success {
        return (
            "unavailable".to_string(),
            "-".to_string(),
            "Claude auth status exited unsuccessfully".to_string(),
        );
    }
    let parsed: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(value) => value,
        Err(error) => {
            return (
                "unknown".to_string(),
                "-".to_string(),
                format!("unrecognized Claude auth status shape: {error}"),
            );
        }
    };
    match parsed.get("loggedIn").and_then(serde_json::Value::as_bool) {
        Some(true) => {}
        Some(false) => {
            return (
                "not authenticated".to_string(),
                "-".to_string(),
                String::new(),
            );
        }
        None => {
            return (
                "unknown".to_string(),
                "-".to_string(),
                "unrecognized Claude auth status shape: `loggedIn` is not boolean".to_string(),
            );
        }
    }
    let method = normalize_claude_method(
        parsed
            .get("authMethod")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
    );
    let provider = normalize_claude_provider(
        parsed
            .get("apiProvider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
    );
    (
        "authenticated".to_string(),
        format!("{method} / {provider}"),
        String::new(),
    )
}

fn parse_codex_status(success: bool, stdout: &str) -> (String, String, String) {
    if !success {
        return (
            "unavailable".to_string(),
            "-".to_string(),
            "Codex login status exited unsuccessfully".to_string(),
        );
    }
    if let Some(auth_type) = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Logged in using "))
    {
        return (
            "authenticated".to_string(),
            normalize_codex_auth(auth_type).to_string(),
            String::new(),
        );
    }
    let status = if stdout.to_ascii_lowercase().contains("not logged in") {
        "not authenticated"
    } else {
        "unknown"
    };
    (status.to_string(), "-".to_string(), String::new())
}

fn normalize_claude_method(value: &str) -> &'static str {
    match value {
        "api_key" => "api_key",
        "claude.ai" => "claude.ai",
        "oauth" => "oauth",
        _ => "other",
    }
}

fn normalize_claude_provider(value: &str) -> &'static str {
    match value {
        "firstParty" => "firstParty",
        "bedrock" => "bedrock",
        "vertex" => "vertex",
        _ => "other",
    }
}

fn normalize_codex_auth(value: &str) -> &'static str {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "chatgpt" {
        "ChatGPT"
    } else if normalized.contains("api key") {
        "API key"
    } else {
        "other"
    }
}

fn resolve_program(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .filter_map(|candidate| fs::canonicalize(candidate).ok())
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    return metadata.permissions().mode() & 0o111 != 0;
    #[cfg(not(unix))]
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_allows_multiple_accounts_of_one_kind() {
        let registry = r#"version = 1
[[providers]]
id = "claude-work"
kind = "claude"
auth_dir = "/profiles/claude-work"
[[providers]]
id = "claude-personal"
kind = "claude"
auth_dir = "/profiles/claude-personal"
"#;
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("claude-work");
        let personal = root.path().join("claude-personal");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&personal).unwrap();
        let registry = registry
            .replace("/profiles/claude-work", work.to_str().unwrap())
            .replace("/profiles/claude-personal", personal.to_str().unwrap());
        let providers = parse_registry(&registry, Path::new("/registry.toml")).unwrap();
        assert_eq!(providers.len(), 2);
        assert!(
            providers
                .iter()
                .all(|provider| provider.kind == ProviderKind::Claude)
        );
    }

    #[test]
    fn registry_rejects_duplicate_ids_and_contexts() {
        let duplicate_id = r#"version = 1
[[providers]]
id = "work"
kind = "claude"
auth_dir = "/profiles/one"
[[providers]]
id = "work"
kind = "codex"
auth_dir = "/profiles/two"
"#;
        assert!(parse_registry(duplicate_id, Path::new("/registry.toml")).is_err());
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir_all(&shared).unwrap();
        let duplicate_context = format!(
            r#"version = 1
[[providers]]
id = "one"
kind = "codex"
auth_dir = "{}"
[[providers]]
id = "two"
kind = "codex"
auth_dir = "{}"
"#,
            shared.display(),
            shared.display()
        );
        assert!(parse_registry(&duplicate_context, Path::new("/registry.toml")).is_err());
    }

    #[test]
    fn provider_auth_types_are_parsed_without_credentials() {
        // Captured from `claude auth status --json` on 2026-08-21.
        let (status, auth_type, detail) = parse_claude_status(
            true,
            include_str!("../tests/fixtures/providers/claude-2.1.238-authenticated.json"),
        );
        assert_eq!(status, "authenticated");
        assert_eq!(auth_type, "api_key / firstParty");
        assert!(detail.is_empty());
        // Captured from `codex login status` on 2026-08-21.
        let (status, auth_type, _) = parse_codex_status(
            true,
            include_str!("../tests/fixtures/providers/codex-0.149.0-chatgpt.txt"),
        );
        assert_eq!(status, "authenticated");
        assert_eq!(auth_type, "ChatGPT");
    }

    #[test]
    fn captured_logged_out_shapes_are_distinct_from_contract_drift() {
        let (status, _, _) = parse_claude_status(
            true,
            include_str!("../tests/fixtures/providers/claude-2.1.238-logged-out.json"),
        );
        assert_eq!(status, "not authenticated");
        let (status, _, _) = parse_codex_status(
            true,
            include_str!("../tests/fixtures/providers/codex-0.149.0-logged-out.txt"),
        );
        assert_eq!(status, "not authenticated");
        assert_eq!(parse_claude_status(true, "{}").0, "unknown");
        assert_eq!(parse_codex_status(true, "changed output").0, "unknown");
    }

    #[test]
    fn identifiers_and_output_boundaries_are_enforced() {
        assert!(safe_id("claude-work").is_ok());
        assert!(safe_id("../work").is_err());

        let mut input = std::io::Cursor::new(vec![b'x'; MAX_PROBE_OUTPUT + 1]);
        let mut captured = Vec::new();
        let mut exceeded = false;
        while drain_available(&mut input, &mut captured, &mut exceeded).unwrap() && !exceeded {}
        assert_eq!(captured.len(), MAX_PROBE_OUTPUT);
        assert!(exceeded);
    }
}
