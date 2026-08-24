//! Machine-local provider inventory for the TUI.
//!
//! Providers are display-only in this iteration. The registry names auth directories, never
//! credentials, arbitrary commands, arguments, or environment variables. Status is obtained from
//! the two fixed adapter CLIs with bounded output and wall time. Codex exposes its plan and quota
//! windows through the official local app-server protocol. Claude has no headless usage-status
//! command, so its fixed local `/usage` screen is opened in a bounded pseudo-terminal and only the
//! weekly percentages are parsed. The probe neither reads credentials nor starts a billable
//! model session. Accepted response shapes are pinned by fixtures.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_PROVIDERS: usize = 32;
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_CONCURRENT_PROBES: usize = 4;
const MAX_PROVIDER_LIMITS: usize = 16;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CLAUDE_USAGE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

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
    pub subscription: String,
    pub limits: Vec<ProviderLimit>,
    pub detail: String,
}

#[derive(Clone)]
pub struct ProviderLimit {
    pub name: String,
    pub used_percent: u8,
    pub resets_at: Option<u64>,
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

pub fn print_status() {
    let cancelled = AtomicBool::new(false);
    let inventory = discover_with_cancel(&cancelled);
    if let Some(warning) = inventory.warning {
        eprintln!("warning: {warning}");
    }
    if inventory.providers.is_empty() {
        println!("No supported provider CLI is installed and no provider registry entries exist");
        return;
    }
    println!(
        "{:<24} {:<8} {:<19} {:<18} SUBSCRIPTION",
        "ID", "KIND", "STATUS", "AUTH"
    );
    for provider in inventory.providers {
        println!(
            "{:<24} {:<8} {:<19} {:<18} {}",
            provider.id, provider.kind, provider.status, provider.auth_type, provider.subscription
        );
        for limit in &provider.limits {
            println!("  limit  {}", format_limit(limit));
        }
        if !provider.detail.is_empty() {
            println!("  note   {}", provider.detail);
        }
    }
}

pub fn format_limit(limit: &ProviderLimit) -> String {
    let reset = limit
        .resets_at
        .map(format_reset)
        .unwrap_or_else(|| "reset unavailable".to_string());
    format!(
        "{}: {}% used, {}% left, {reset}",
        limit.name,
        limit.used_percent,
        100_u8.saturating_sub(limit.used_percent)
    )
}

fn format_reset(resets_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let remaining = resets_at.saturating_sub(now);
    if remaining == 0 {
        "reset due".to_string()
    } else if remaining >= 24 * 60 * 60 {
        format!(
            "resets in {}d {}h",
            remaining / (24 * 60 * 60),
            remaining % (24 * 60 * 60) / (60 * 60)
        )
    } else if remaining >= 60 * 60 {
        format!(
            "resets in {}h {}m",
            remaining / (60 * 60),
            remaining % (60 * 60) / 60
        )
    } else {
        format!("resets in {}m", remaining.div_ceil(60))
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
            return Ok(Some(config.join("afactory/providers.toml")));
        }
    }
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/afactory/providers.toml"));
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
        let configured_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
        defaults.push(ProviderSpec {
            id: "codex-ambient".to_string(),
            kind: ProviderKind::Codex,
            auth_dir: canonical_if_present(
                configured_home
                    .clone()
                    .or_else(|| home.map(|home| home.join(".codex"))),
            ),
            explicit_selector: configured_home.is_some(),
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
    let probe_path = sanitized_path();
    let output = match run_probe(&program, &spec, &probe_path, cancelled) {
        Ok(output) => output,
        Err(error) => return unavailable_status(&spec, &error),
    };
    let (status, auth_type, mut detail) = match spec.kind {
        ProviderKind::Claude => parse_claude_status(output.status.success(), &output.stdout),
        ProviderKind::Codex => parse_codex_status(output.status.success(), &output.stdout),
    };
    let mut subscription = match spec.kind {
        ProviderKind::Claude => claude_subscription(&output.stdout),
        ProviderKind::Codex if auth_type == "API key" => "API billing".to_string(),
        ProviderKind::Codex if auth_type == "ChatGPT" => "ChatGPT (plan unavailable)".to_string(),
        ProviderKind::Codex => "-".to_string(),
    };
    let mut limits = Vec::new();
    if spec.kind == ProviderKind::Claude
        && status == "authenticated"
        && claude_subscription_usage_supported(&output.stdout)
    {
        match probe_claude_weekly_limits(&program, &spec, &probe_path, cancelled) {
            Ok(claude_limits) => limits.extend(claude_limits),
            Err(error) => {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!("weekly limit unavailable: {error}"));
            }
        }
    }
    if spec.kind == ProviderKind::Codex && status == "authenticated" && auth_type == "ChatGPT" {
        match probe_codex_subscription(&program, &spec, &probe_path, cancelled) {
            Ok(snapshot) => {
                subscription = snapshot.subscription;
                limits = snapshot.limits;
                if let Some(warning) = snapshot.warning {
                    if !detail.is_empty() {
                        detail.push_str("; ");
                    }
                    detail.push_str(&format!("subscription status partial: {warning}"));
                }
            }
            Err(error) => {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!("subscription status unavailable: {error}"));
            }
        }
    }
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
        subscription,
        limits,
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
        subscription: "-".to_string(),
        limits: Vec::new(),
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
        subscription: "-".to_string(),
        limits: Vec::new(),
        detail: detail.to_string(),
    }
}

struct ProbeOutput {
    status: ExitStatus,
    stdout: String,
}

struct SubscriptionSnapshot {
    subscription: String,
    limits: Vec<ProviderLimit>,
    warning: Option<String>,
}

struct ParsedClaudeUsage {
    limits: Vec<ProviderLimit>,
    complete: bool,
}

fn claude_subscription_usage_supported(auth_status: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(auth_status) else {
        return false;
    };
    parsed.get("loggedIn").and_then(serde_json::Value::as_bool) == Some(true)
        && matches!(
            parsed.get("authMethod").and_then(serde_json::Value::as_str),
            Some("claude.ai" | "oauth")
        )
        && parsed
            .get("apiProvider")
            .and_then(serde_json::Value::as_str)
            == Some("firstParty")
}

#[cfg(unix)]
fn probe_claude_weekly_limits(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<Vec<ProviderLimit>, String> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::sync::mpsc::{RecvTimeoutError, channel};

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 50,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("cannot create Claude usage terminal: {error}"))?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("cannot read Claude usage terminal: {error}"))?;
    let mut command = CommandBuilder::new(program.as_os_str());
    command.args(["--setting-sources", "user", "/usage"]);
    command.env_clear();
    command.cwd("/");
    command.env("PATH", probe_path);
    if let Some(home) = std::env::var_os("HOME").filter(|value| Path::new(value).is_absolute()) {
        command.env("HOME", home);
    }
    if let Some(user) = std::env::var_os("USER") {
        command.env("USER", user);
    }
    if spec.explicit_selector
        && let Some(auth_dir) = &spec.auth_dir
    {
        command.env("CLAUDE_CONFIG_DIR", auth_dir);
    }
    command.env("TERM", "xterm-256color");
    command.env("LC_ALL", "C");
    command.env("TZ", "UTC");
    // The fixed local command has no model turn or tools. Marking this narrow subprocess as
    // sandboxed avoids mutating Claude's trust registry just to inspect account usage.
    command.env("CLAUDE_CODE_SANDBOXED", "1");
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("cannot start Claude usage probe: {error}"))?;
    let process_group = child.process_id();
    drop(pair.slave);

    let (sender, receiver) = channel();
    let reader_thread = thread::spawn(move || {
        loop {
            let mut chunk = vec![0_u8; 8192];
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    chunk.truncate(count);
                    if sender.send(Ok(chunk)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + CLAUDE_USAGE_PROBE_TIMEOUT;
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT.min(8192));
    let mut dirty = false;
    let result = loop {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(chunk)) => {
                let remaining = MAX_PROBE_OUTPUT.saturating_sub(captured.len());
                captured.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if chunk.len() > remaining {
                    break Err(format!(
                        "Claude usage output exceeds {MAX_PROBE_OUTPUT} bytes"
                    ));
                }
                dirty = true;
            }
            Ok(Err(error)) => {
                break Err(format!("cannot read Claude usage terminal: {error}"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err("Claude usage terminal closed without a weekly limit".to_string());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if dirty {
            if let Ok(usage) = parse_claude_weekly_limits(&captured)
                && usage.complete
            {
                break Ok(usage.limits);
            }
            dirty = false;
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                break Err("Claude usage screen exited without a weekly limit".to_string());
            }
            Ok(None) => {}
            Err(error) => break Err(format!("Claude usage probe failed: {error}")),
        }
        if cancelled.load(Ordering::Acquire) {
            break Err("provider status refresh cancelled".to_string());
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "Claude usage probe timed out after {} seconds",
                CLAUDE_USAGE_PROBE_TIMEOUT.as_secs()
            ));
        }
    };
    if let Some(process_group) = process_group {
        terminate_probe_group(process_group);
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader_thread.join();
    result
}

#[cfg(not(unix))]
fn probe_claude_weekly_limits(
    _program: &Path,
    _spec: &ProviderSpec,
    _probe_path: &std::ffi::OsStr,
    _cancelled: &AtomicBool,
) -> Result<Vec<ProviderLimit>, String> {
    Err("Claude usage probes require a Unix pseudo-terminal".to_string())
}

fn parse_claude_weekly_limits(output: &[u8]) -> Result<ParsedClaudeUsage, String> {
    // Ink uses cursor-position controls instead of literal spaces in recent releases. Compacting
    // whitespace after removing terminal controls gives old and new screen renderers one stable
    // text shape without attempting to emulate a terminal.
    let screen: String = strip_terminal_controls(output)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let sections = [
        ("Currentweek(allmodels)", "Claude all models 1w"),
        ("Currentweek(Fable)", "Claude Fable 1w"),
        ("Currentweek(Sonnetonly)", "Claude Sonnet 1w"),
        ("Currentweek(Opusonly)", "Claude Opus 1w"),
    ];
    let mut limits = Vec::with_capacity(sections.len());
    for (section, name) in sections {
        if let Some(used_percent) = percent_used_after(&screen, section) {
            limits.push(ProviderLimit {
                name: name.to_string(),
                used_percent,
                // Claude renders a localized wall-clock string rather than an epoch. Do not
                // guess a timestamp; the percentage is the stable compatibility surface.
                resets_at: None,
            });
        }
    }
    if limits
        .first()
        .is_none_or(|limit| limit.name != "Claude all models 1w")
    {
        return Err("unrecognized Claude usage screen".to_string());
    }
    let after_weekly_limit = screen
        .rsplit_once("Currentweek(allmodels)")
        .map(|(_, after)| after)
        .unwrap_or_default();
    Ok(ParsedClaudeUsage {
        limits,
        complete: after_weekly_limit.contains("Usagecredits")
            || after_weekly_limit.contains("Extrausage")
            || after_weekly_limit.contains("Esctocancel"),
    })
}

fn percent_used_after(text: &str, section: &str) -> Option<u8> {
    let section = text.rsplit_once(section)?.1;
    let percent_sign = section.match_indices('%').find_map(|(index, _)| {
        section[index + 1..]
            .trim_start()
            .starts_with("used")
            .then_some(index)
    })?;
    let before_marker = &section[..percent_sign];
    let digits_reversed: String = before_marker
        .chars()
        .rev()
        .skip_while(|character| character.is_whitespace())
        .take_while(|character| character.is_ascii_digit())
        .collect();
    let digits: String = digits_reversed.chars().rev().collect();
    digits.parse::<u8>().ok().filter(|percent| *percent <= 100)
}

fn strip_terminal_controls(output: &[u8]) -> String {
    let mut clean = Vec::with_capacity(output.len());
    let mut index = 0;
    while index < output.len() {
        match output[index] {
            0x1b if output.get(index + 1) == Some(&b'[') => {
                index += 2;
                while index < output.len() {
                    let byte = output[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            0x1b if output.get(index + 1) == Some(&b']') => {
                index += 2;
                while index < output.len() {
                    if output[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if output[index] == 0x1b && output.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            0x1b => index += usize::from(output.get(index + 1).is_some()) + 1,
            b'\r' => {
                clean.push(b'\n');
                index += 1;
            }
            byte if byte == b'\n' || byte == b'\t' || byte >= 0x20 => {
                clean.push(byte);
                index += 1;
            }
            _ => index += 1,
        }
    }
    String::from_utf8_lossy(&clean).into_owned()
}

fn configure_probe_environment(
    command: &mut Command,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
) {
    command
        .env_clear()
        .current_dir(Path::new("/"))
        .env("PATH", probe_path);
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
}

#[cfg(unix)]
fn probe_codex_subscription(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<SubscriptionSnapshot, String> {
    let mut command = Command::new(program);
    command.args(["app-server", "--stdio"]);
    configure_probe_environment(&mut command, spec, probe_path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start Codex app-server probe: {error}"))?;
    let mut stdin = child.stdin.take().expect("provider stdin was piped");
    let mut stdout = child.stdout.take().expect("provider stdout was piped");
    if let Err(error) = set_nonblocking(&stdout) {
        stop_probe(&mut child);
        return Err(error);
    }
    if let Err(error) = writeln!(
        stdin,
        "{{\"method\":\"initialize\",\"id\":1,\"params\":{{\"clientInfo\":{{\"name\":\"afactory\",\"version\":\"{}\"}}}}}}",
        env!("CARGO_PKG_VERSION")
    )
    .and_then(|()| stdin.flush())
    {
        stop_probe(&mut child);
        return Err(format!("cannot initialize Codex app-server probe: {error}"));
    }

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT.min(4096));
    let mut exceeded = false;
    let mut requested_limits = false;
    let result = 'probe: loop {
        loop {
            match drain_available(&mut stdout, &mut captured, &mut exceeded) {
                Ok(true) if !exceeded => {}
                Ok(_) => break,
                Err(error) => break 'probe Err(error),
            }
        }
        if exceeded {
            break Err(format!(
                "Codex app-server output exceeds {MAX_PROBE_OUTPUT} bytes"
            ));
        }
        if !requested_limits && let Some(response) = response_for_id(&captured, 1) {
            if response.get("error").is_some() {
                break Err("Codex app-server rejected initialization".to_string());
            }
            if let Err(error) = writeln!(stdin, "{{\"method\":\"initialized\",\"params\":{{}}}}")
                .and_then(|()| {
                    writeln!(stdin, "{{\"method\":\"account/rateLimits/read\",\"id\":2}}")
                })
                .and_then(|()| stdin.flush())
            {
                break Err(format!("cannot request Codex subscription status: {error}"));
            }
            requested_limits = true;
        }
        if requested_limits && let Some(response) = response_for_id(&captured, 2) {
            break parse_codex_subscription_response(&response);
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                break Err("Codex app-server probe exited without a rate-limit response".into());
            }
            Ok(None) => {}
            Err(error) => {
                break Err(format!("Codex app-server probe failed: {error}"));
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break Err("provider status refresh cancelled".to_string());
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "Codex subscription probe timed out after {} seconds",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    stop_probe(&mut child);
    result
}

#[cfg(not(unix))]
fn probe_codex_subscription(
    _program: &Path,
    _spec: &ProviderSpec,
    _probe_path: &std::ffi::OsStr,
    _cancelled: &AtomicBool,
) -> Result<SubscriptionSnapshot, String> {
    Err("provider probes require Unix process-group isolation".to_string())
}

fn response_for_id(captured: &[u8], expected_id: u64) -> Option<serde_json::Value> {
    captured
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
        .filter_map(|line| {
            let line = line
                .strip_suffix(b"\n")
                .unwrap_or(line)
                .strip_suffix(b"\r")
                .unwrap_or(line);
            serde_json::from_slice::<serde_json::Value>(line).ok()
        })
        .find(|message| message.get("id").and_then(serde_json::Value::as_u64) == Some(expected_id))
}

fn parse_codex_subscription_response(
    response: &serde_json::Value,
) -> Result<SubscriptionSnapshot, String> {
    if response.get("error").is_some() {
        return Err("Codex app-server rejected the rate-limit request".to_string());
    }
    let result = response
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "unrecognized Codex rate-limit response".to_string())?;
    let mut snapshots: Vec<&serde_json::Value> = result
        .get("rateLimitsByLimitId")
        .and_then(serde_json::Value::as_object)
        .map(|snapshots| snapshots.values().collect())
        .unwrap_or_default();
    snapshots.extend(result.get("rateLimits"));
    snapshots.sort_by_key(|snapshot| {
        snapshot
            .get("limitId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    });
    let plan = snapshots.iter().find_map(|snapshot| {
        snapshot
            .get("planType")
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_codex_plan)
    });
    let mut limits = Vec::new();
    let mut seen_limits = BTreeSet::new();
    let mut skipped_windows = 0_usize;
    let mut truncated = false;
    for snapshot in snapshots {
        let bucket = snapshot
            .get("limitName")
            .and_then(serde_json::Value::as_str)
            .or_else(|| snapshot.get("limitId").and_then(serde_json::Value::as_str))
            .map(|value| safe_display(value, "Codex"))
            .unwrap_or_else(|| "Codex".to_string());
        for field in ["primary", "secondary"] {
            let Some(window) = snapshot.get(field).and_then(serde_json::Value::as_object) else {
                continue;
            };
            let used_percent = match window
                .get("usedPercent")
                .and_then(serde_json::Value::as_u64)
                .and_then(|used| u8::try_from(used).ok())
                .filter(|used| *used <= 100)
            {
                Some(used) => used,
                None => {
                    skipped_windows += 1;
                    continue;
                }
            };
            let window_minutes = window
                .get("windowDurationMins")
                .and_then(serde_json::Value::as_u64);
            if !seen_limits.insert((bucket.clone(), window_minutes)) {
                continue;
            }
            if limits.len() == MAX_PROVIDER_LIMITS {
                truncated = true;
                continue;
            }
            limits.push(ProviderLimit {
                name: match window_minutes {
                    Some(minutes) => format!("{bucket} {}", format_window(minutes)),
                    None => bucket.clone(),
                },
                used_percent,
                resets_at: window.get("resetsAt").and_then(serde_json::Value::as_u64),
            });
        }
    }
    if plan.is_none() && limits.is_empty() {
        return Err("Codex rate-limit response has no usable subscription information".to_string());
    }
    let mut warnings = Vec::new();
    if skipped_windows > 0 {
        warnings.push(format!(
            "skipped {skipped_windows} malformed rate-limit windows"
        ));
    }
    if truncated {
        warnings.push(format!(
            "additional rate-limit windows omitted at the {MAX_PROVIDER_LIMITS}-window safety limit"
        ));
    }
    Ok(SubscriptionSnapshot {
        subscription: plan
            .map(|plan| format!("ChatGPT {plan}"))
            .unwrap_or_else(|| "ChatGPT (plan unavailable)".to_string()),
        limits,
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    })
}

fn normalize_codex_plan(value: &str) -> Option<&'static str> {
    match value {
        "free" => Some("Free"),
        "go" => Some("Go"),
        "plus" => Some("Plus"),
        "pro" | "prolite" => Some("Pro"),
        "team" => Some("Team"),
        "self_serve_business_prolite" | "self_serve_business_usage_based" | "business" => {
            Some("Business")
        }
        "ent26" | "enterprise_cbp_automation" | "enterprise_cbp_usage_based" | "enterprise" => {
            Some("Enterprise")
        }
        "edu" | "edu_plus" | "edu_pro" => Some("Education"),
        "unknown" => Some("Unknown"),
        _ => None,
    }
}

fn safe_display(value: &str, fallback: &str) -> String {
    let value: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(48)
        .collect();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

pub fn format_window(minutes: u64) -> String {
    if minutes % (7 * 24 * 60) == 0 {
        format!("{}w", minutes / (7 * 24 * 60))
    } else if minutes % (24 * 60) == 0 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

#[cfg(unix)]
fn run_probe(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
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
    configure_probe_environment(&mut command, spec, probe_path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start provider status probe: {error}"))?;
    let mut stdout = child.stdout.take().expect("provider stdout was piped");
    let mut stderr = child.stderr.take().expect("provider stderr was piped");
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_probe(&mut child);
        return Err(error);
    }
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT.min(4096));
    let mut discarded = Vec::new();
    let mut exceeded = false;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Err(error) = drain_probe_streams(
            spec.kind,
            &mut stdout,
            &mut stderr,
            &mut captured,
            &mut discarded,
            &mut exceeded,
        ) {
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
    while drain_probe_streams(
        spec.kind,
        &mut stdout,
        &mut stderr,
        &mut captured,
        &mut discarded,
        &mut exceeded,
    )? && !exceeded
    {}
    if exceeded {
        return Err(format!(
            "provider status output exceeds {MAX_PROBE_OUTPUT} bytes"
        ));
    }
    let stdout = String::from_utf8(captured)
        .map_err(|_| "provider status output is not UTF-8".to_string())?;
    Ok(ProbeOutput { status, stdout })
}

fn drain_probe_streams(
    kind: ProviderKind,
    stdout: &mut impl Read,
    stderr: &mut impl Read,
    captured: &mut Vec<u8>,
    discarded: &mut Vec<u8>,
    exceeded: &mut bool,
) -> Result<bool, String> {
    let (stdout_read, stderr_read) = match kind {
        ProviderKind::Claude => (
            drain_available(stdout, captured, exceeded)?,
            drain_available(stderr, discarded, exceeded)?,
        ),
        ProviderKind::Codex => (
            drain_available(stdout, discarded, exceeded)?,
            // codex-cli 0.149.0 deliberately renders login status on stderr.
            drain_available(stderr, captured, exceeded)?,
        ),
    };
    Ok(stdout_read || stderr_read)
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
    _probe_path: &std::ffi::OsStr,
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
    let parsed: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(value) => value,
        Err(error) => {
            if !success {
                return (
                    "unavailable".to_string(),
                    "-".to_string(),
                    "Claude auth status exited unsuccessfully".to_string(),
                );
            }
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

fn claude_subscription(stdout: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return "-".to_string();
    };
    if parsed.get("loggedIn").and_then(serde_json::Value::as_bool) != Some(true) {
        return "-".to_string();
    }
    match parsed
        .get("apiProvider")
        .and_then(serde_json::Value::as_str)
    {
        Some("bedrock") => return "AWS Bedrock billing".to_string(),
        Some("vertex") => return "Google Vertex billing".to_string(),
        _ => {}
    }
    match parsed.get("authMethod").and_then(serde_json::Value::as_str) {
        Some("api_key") => "API billing".to_string(),
        Some("claude.ai" | "oauth") => ["subscriptionType", "planType", "plan"]
            .into_iter()
            .find_map(|field| {
                parsed
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .and_then(normalize_claude_plan)
            })
            .map(|plan| format!("Claude {plan}"))
            .unwrap_or_else(|| "Claude subscription (tier unavailable)".to_string()),
        _ => "-".to_string(),
    }
}

fn normalize_claude_plan(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "free" => Some("Free"),
        "pro" => Some("Pro"),
        "max" | "max_5x" | "max_20x" => Some("Max"),
        "team" => Some("Team"),
        "enterprise" => Some("Enterprise"),
        _ => None,
    }
}

fn parse_codex_status(success: bool, stdout: &str) -> (String, String, String) {
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
    if stdout.to_ascii_lowercase().contains("not logged in") {
        return (
            "not authenticated".to_string(),
            "-".to_string(),
            String::new(),
        );
    }
    if !success {
        return (
            "unavailable".to_string(),
            "-".to_string(),
            "Codex login status exited unsuccessfully".to_string(),
        );
    }
    ("unknown".to_string(), "-".to_string(), String::new())
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
        assert_eq!(
            claude_subscription(include_str!(
                "../tests/fixtures/providers/claude-2.1.238-authenticated.json"
            )),
            "API billing"
        );
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
    fn codex_subscription_plan_and_all_quota_windows_are_parsed() {
        // Captured from `account/rateLimits/read` on codex-cli 0.149.0, with opaque reset-credit
        // identifiers omitted because the provider inventory never consumes or displays them.
        let response: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/providers/codex-0.149.0-rate-limits.json"
        ))
        .unwrap();
        let snapshot = parse_codex_subscription_response(&response).unwrap();
        assert_eq!(snapshot.subscription, "ChatGPT Pro");
        assert_eq!(snapshot.limits.len(), 3);
        assert_eq!(snapshot.limits[0].name, "codex 1w");
        assert_eq!(snapshot.limits[0].used_percent, 37);
        assert_eq!(snapshot.limits[1].name, "GPT-5.3-Codex-Spark 5h");
        assert_eq!(snapshot.limits[2].name, "GPT-5.3-Codex-Spark 1w");
        assert!(snapshot.warning.is_none());
    }

    #[test]
    fn claude_weekly_limit_and_remaining_percentage_are_parsed_from_usage_screen() {
        // Captured from Claude Code 2.1.101 `/usage` on 2026-08-24. The provider adapter opens
        // this local screen in a PTY; it does not read the OAuth credential or call a model.
        let usage = parse_claude_weekly_limits(include_bytes!(
            "../tests/fixtures/providers/claude-2.1.101-usage.txt"
        ))
        .unwrap();
        let limits = usage.limits;
        assert_eq!(limits.len(), 1);
        let limit = &limits[0];
        assert_eq!(limit.name, "Claude all models 1w");
        assert_eq!(limit.used_percent, 88);
        assert_eq!(
            format_limit(limit),
            "Claude all models 1w: 88% used, 12% left, reset unavailable"
        );
    }

    #[test]
    fn claude_usage_parser_ignores_terminal_formatting_sequences() {
        let output = b"\x1b]0;Claude Code\x07\x1b[1mCurrent week (all models)\x1b[0m\r\n\
                       \x1b[48;5;102m89\x1b[0m%\x1b[2Kused\r\n";
        let limits = parse_claude_weekly_limits(output).unwrap().limits;
        assert_eq!(limits[0].used_percent, 89);
    }

    #[test]
    fn claude_usage_parser_handles_cursor_positioned_words() {
        let output = b"Current\x1b[11Gweek\x1b[16G(all\x1b[20Gmodels)\r\n\
                       \x1b[54G90%\x1b[58Gused\r\n\
                       Current\x1b[11Gweek\x1b[16G(Fable)\r\n\
                       \x1b[54G96%\x1b[58Gused\r\n\
                       Usage\x1b[6Gcredits\r\nEsc\x1b[5Gto\x1b[8Gcancel\r\n";
        let usage = parse_claude_weekly_limits(output).unwrap();
        assert!(usage.complete);
        assert_eq!(usage.limits[0].used_percent, 90);
        assert_eq!(usage.limits[1].used_percent, 96);
    }

    #[test]
    fn claude_fable_and_all_model_weekly_limits_are_parsed_from_usage_screen() {
        let usage = parse_claude_weekly_limits(include_bytes!(
            "../tests/fixtures/providers/claude-2.1.241-usage.txt"
        ))
        .unwrap();
        assert!(usage.complete);
        let limits = usage.limits;
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].name, "Claude all models 1w");
        assert_eq!(limits[0].used_percent, 90);
        assert_eq!(limits[1].name, "Claude Fable 1w");
        assert_eq!(limits[1].used_percent, 96);
        assert_eq!(
            format_limit(&limits[1]),
            "Claude Fable 1w: 96% used, 4% left, reset unavailable"
        );
    }

    #[test]
    fn claude_usage_probe_is_only_enabled_for_first_party_subscription_auth() {
        assert!(claude_subscription_usage_supported(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
        ));
        assert!(!claude_subscription_usage_supported(
            r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#
        ));
        assert!(!claude_subscription_usage_supported(
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"bedrock"}"#
        ));
    }

    #[cfg(unix)]
    #[test]
    fn claude_usage_probe_reads_the_fixed_screen_and_reaps_the_session() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("claude");
        fs::write(
            &program,
            "#!/bin/sh\n[ \"$3\" = /usage ] || exit 2\nprintf 'Current week (all models)\\r\\n42%%used\\r\\nExtra usage\\r\\n'\nsleep 30\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();
        let spec = ProviderSpec {
            id: "claude-test".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: None,
            explicit_selector: false,
            source: "test".to_string(),
        };

        let probe_path = sanitized_path();
        let limits =
            probe_claude_weekly_limits(&program, &spec, &probe_path, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].used_percent, 42);
        assert_eq!(limits[0].resets_at, None);
    }

    #[test]
    fn malformed_codex_windows_do_not_hide_valid_subscription_data() {
        let response: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/providers/codex-0.149.0-rate-limits-mixed.json"
        ))
        .unwrap();
        let snapshot = parse_codex_subscription_response(&response).unwrap();
        assert_eq!(snapshot.subscription, "ChatGPT Pro");
        assert_eq!(snapshot.limits.len(), 1);
        assert_eq!(snapshot.limits[0].name, "codex 1w");
        assert_eq!(
            snapshot.warning.as_deref(),
            Some("skipped 2 malformed rate-limit windows")
        );
    }

    #[test]
    fn unusable_per_limit_map_falls_back_to_legacy_snapshot() {
        let response: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/providers/codex-0.149.0-rate-limits-fallback.json"
        ))
        .unwrap();
        let snapshot = parse_codex_subscription_response(&response).unwrap();
        assert_eq!(snapshot.subscription, "ChatGPT Pro");
        assert_eq!(snapshot.limits.len(), 1);
        assert_eq!(snapshot.limits[0].name, "codex 1w");
        assert_eq!(
            snapshot.warning.as_deref(),
            Some("skipped 1 malformed rate-limit windows")
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_subscription_probe_reaps_descendants_after_an_early_exit() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex");
        let pid_file = directory.path().join("descendant.pid");
        let script = format!(
            "#!/bin/sh\n\
             IFS= read -r _\n\
             printf '%s\\n' '{{\"id\":1,\"result\":{{}}}}'\n\
             IFS= read -r _\n\
             IFS= read -r _\n\
             sleep 30 &\n\
             echo $! > '{}'\n\
             exit 0\n",
            pid_file.display()
        );
        fs::write(&program, script).unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();
        let spec = ProviderSpec {
            id: "codex-test".to_string(),
            kind: ProviderKind::Codex,
            auth_dir: None,
            explicit_selector: false,
            source: "test".to_string(),
        };

        let probe_path = sanitized_path();
        let Err(error) =
            probe_codex_subscription(&program, &spec, &probe_path, &AtomicBool::new(false))
        else {
            panic!("early app-server exit unexpectedly returned subscription data");
        };
        assert!(error.contains("exited without a rate-limit response"));
        let process = nix::unistd::Pid::from_raw(
            fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        for _ in 0..100 {
            if nix::sys::signal::kill(process, None) == Err(nix::errno::Errno::ESRCH) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("Codex app-server descendant {process} survived probe cleanup");
    }

    #[test]
    fn claude_subscription_tier_is_optional_and_never_inferred_for_api_keys() {
        assert_eq!(
            claude_subscription(
                r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","subscriptionType":"max_20x"}"#
            ),
            "Claude Max"
        );
        assert_eq!(
            claude_subscription(
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
            ),
            "Claude subscription (tier unavailable)"
        );
        assert_eq!(
            claude_subscription(
                r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","subscriptionType":"max"}"#
            ),
            "API billing"
        );
    }

    #[test]
    fn app_server_response_scanner_waits_for_a_complete_matching_line() {
        let captured = br#"{"id":1,"result":{}}
{"id":2,"result":{"rateLimits":{}}}"#;
        assert!(response_for_id(captured, 1).is_some());
        assert!(response_for_id(captured, 2).is_none());
        let complete = [captured.as_slice(), b"\n"].concat();
        assert!(response_for_id(&complete, 2).is_some());
    }

    #[test]
    fn captured_logged_out_shapes_are_distinct_from_contract_drift() {
        let (status, _, _) = parse_claude_status(
            false,
            include_str!("../tests/fixtures/providers/claude-2.1.238-logged-out.json"),
        );
        assert_eq!(status, "not authenticated");
        let (status, _, _) = parse_codex_status(
            false,
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
