//! The configuration ladder: built-in defaults, system, user, directory, project, local, and
//! environment layers merged into one effective TOML document with the origin of every value.
//!
//! Tables deep-merge, scalars last-wins, arrays replace. The loader is lazy about the Store and
//! reads only small TOML files; `af config show --origin` is the audit surface.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use toml::Value;

use crate::cli::LayerArg;

/// Where the binary's own defaults come from; the lowest layer, always present.
const BUILT_IN: &str = r#"
[self]
update_check = true
check_every = "24h"
auto_update = "notify"
channel = "stable"
install_pins = "ask"
keep_versions = 3
source = "PakhomovAlexander/afactory"

[storage]
max_bytes = "auto"
min_free_bytes = "10GiB"
auto_gc = false
keep_days = 14
keep_tasks = 20
keep_campaigns = 20
keep_surveys = 50
keep_worker_transcripts = false
keep_gate_pull_requests = false
"#;

/// The built-in layer: [`BUILT_IN`] and the `[telemetry]` table with every default, which the
/// telemetry crate owns with its bounds (ADR-0153).
fn built_in() -> String {
    format!("{BUILT_IN}\n{}", review_telemetry::level::built_in_table())
}

/// The tables only machine layers may set: a repository's `.af/af.toml` can steer neither what
/// af downloads (`[self]`), nor what it keeps or removes on this machine (`[storage]`), nor what
/// it records about its own work there (`[telemetry]`, ADR-0153).
const MACHINE_ONLY: [(&str, &str); 3] = [
    ("self", "self"),
    ("storage", "storage"),
    ("telemetry", "telemetry"),
];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Origin {
    pub(crate) layer: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LayerFile {
    pub(crate) layer: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Config {
    /// True when only the machine-owned layers (built-in, system, user, environment) were read.
    /// The `[self]` policy — what to download, from where, and whether to exec it — is only ever
    /// taken from such a load, so a repository can never steer it.
    pub(crate) machine_scope: bool,
    /// `[self]`, `[storage]` and `[telemetry]` tables found in directory, project, or local
    /// layers: reported, never merged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) ignored: Vec<String>,
    /// Every file a layer would read, in ladder order, present or not.
    pub(crate) files: Vec<LayerFile>,
    /// The repository toplevel the project layers were taken from, when inside one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) toplevel: Option<PathBuf>,
    pub(crate) effective: Value,
    pub(crate) origins: BTreeMap<String, Origin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoUpdate {
    Notify,
    Always,
    Never,
}

/// Whether a project lock's pin that is not installed yet is installed on demand: `ask` on a
/// terminal first, `always`, or `never` (then `af self install <version>` is the step). The
/// booleans of earlier releases keep their meaning: `true` is `always`, `false` is `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallPins {
    Ask,
    Always,
    Never,
}

impl InstallPins {
    pub(crate) fn name(self) -> &'static str {
        match self {
            InstallPins::Ask => "ask",
            InstallPins::Always => "always",
            InstallPins::Never => "never",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Stable,
    Rc,
}

/// The `[self]` table, typed. Every field has a built-in default.
#[derive(Debug, Clone)]
pub(crate) struct SelfPolicy {
    pub(crate) update_check: bool,
    pub(crate) check_every: Duration,
    pub(crate) auto_update: AutoUpdate,
    pub(crate) channel: Channel,
    pub(crate) install_pins: InstallPins,
    pub(crate) keep_versions: usize,
    pub(crate) source: String,
}

impl Config {
    pub(crate) fn self_policy(&self) -> Result<SelfPolicy, String> {
        if !self.machine_scope {
            return Err("[self] policy must come from a machine-scope load (built-in, system, user, environment)".into());
        }
        let table = self
            .effective
            .get("self")
            .and_then(Value::as_table)
            .ok_or("[self] is missing from the effective configuration")?;
        let get = |key: &str| {
            table
                .get(key)
                .ok_or_else(|| format!("[self] {key} has no value"))
        };
        let bool_of = |key: &str| -> Result<bool, String> {
            get(key)?
                .as_bool()
                .ok_or_else(|| format!("[self] {key} must be true or false"))
        };
        let str_of = |key: &str| -> Result<String, String> {
            get(key)?
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("[self] {key} must be a string"))
        };
        let auto_update = match str_of("auto_update")?.as_str() {
            "notify" => AutoUpdate::Notify,
            "always" => AutoUpdate::Always,
            "never" => AutoUpdate::Never,
            other => {
                return Err(format!(
                    "[self] auto_update = \"{other}\" is not one of notify, always, never"
                ));
            }
        };
        let channel = match str_of("channel")?.as_str() {
            "stable" => Channel::Stable,
            "rc" => Channel::Rc,
            other => return Err(format!("[self] channel = \"{other}\" is not stable or rc")),
        };
        let install_pins = match get("install_pins")? {
            Value::Boolean(true) => InstallPins::Always,
            Value::Boolean(false) => InstallPins::Never,
            Value::String(text) if text == "ask" => InstallPins::Ask,
            Value::String(text) if text == "always" => InstallPins::Always,
            Value::String(text) if text == "never" => InstallPins::Never,
            other => {
                return Err(format!(
                    "[self] install_pins = {other} is not one of \"ask\", \"always\", \"never\" (or true, false)"
                ));
            }
        };
        let keep_versions = get("keep_versions")?
            .as_integer()
            .filter(|value| *value >= 1)
            .ok_or("[self] keep_versions must be an integer of at least 1")?
            as usize;
        Ok(SelfPolicy {
            update_check: bool_of("update_check")?,
            check_every: parse_duration(&str_of("check_every")?)
                .ok_or("[self] check_every must look like 24h, 30m, or 90s")?,
            auto_update,
            channel,
            install_pins,
            keep_versions,
            source: str_of("source")?,
        })
    }
}

/// `[storage] max_bytes = "auto"`, the default: [`AUTO_MAX_BYTES`], or on a host whose cache
/// volume has more than [`ROOMY_FREE_BYTES`] free, [`ROOMY_MAX_BYTES`].
pub(crate) const AUTO_MAX_BYTES: u64 = 20 << 30;
/// Room enough for one 16 GiB warm build key with half its size to spare, plus 16 GiB of Task
/// records, review campaigns and installed versions: a budget smaller than its own warm key
/// made every gate run cold (finding K18).
pub(crate) const ROOMY_MAX_BYTES: u64 = 40 << 30;
pub(crate) const ROOMY_FREE_BYTES: u64 = 100 << 30;

/// What `max_bytes = "auto"` is on a host whose cache volume has `free` bytes free.
pub(crate) fn auto_max_bytes(free: Option<u64>) -> u64 {
    if free.is_some_and(|free| free > ROOMY_FREE_BYTES) {
        ROOMY_MAX_BYTES
    } else {
        AUTO_MAX_BYTES
    }
}

/// Where `[storage] max_bytes` came from: a byte count a layer set, or `"auto"` and the free
/// bytes it was chosen by (`None` when they could not be measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaxBytesSource {
    Set,
    Auto { free: Option<u64> },
}

/// The `[storage]` table, typed (ADR-0144). Every field has a built-in default, and only
/// machine layers set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoragePolicy {
    /// The budget over every byte af keeps between runs.
    pub(crate) max_bytes: u64,
    pub(crate) max_bytes_source: MaxBytesSource,
    /// The free-disk floor below which a check or a Worker Attempt is refused.
    pub(crate) min_free_bytes: u64,
    /// Age-based collection after every `af task run` and `af review run`. Off by default: a
    /// run then holds af to the budget only, and age-based collection runs on request.
    pub(crate) auto_gc: bool,
    /// Collection never takes anything used in the last `keep_days`.
    pub(crate) keep_days: u64,
    /// The newest finished Tasks kept per Store.
    pub(crate) keep_tasks: usize,
    /// The newest review campaigns kept.
    pub(crate) keep_campaigns: usize,
    /// The newest Surveys collection keeps per Project (ADR-0159 clause 13): a target that the
    /// Surveys it protects may exceed.
    pub(crate) keep_surveys: usize,
    /// Keep each Claude Attempt's project directory in the Claude config directory.
    pub(crate) keep_worker_transcripts: bool,
    /// Leave remote-check gate pull requests and branches open when a Task finishes.
    pub(crate) keep_gate_pull_requests: bool,
    /// Where sandboxes and check runtime directories are made instead of the temporary
    /// directory (`TMPDIR`), and so where the free-disk floor measures them.
    pub(crate) sandbox_root: Option<PathBuf>,
    /// Where warm build keys live instead of `$XDG_CACHE_HOME/af/task-build-cache`, and so where
    /// the free-disk floor measures them.
    pub(crate) build_cache_root: Option<PathBuf>,
}

impl Config {
    pub(crate) fn storage_policy(&self) -> Result<StoragePolicy, String> {
        let table = self
            .effective
            .get("storage")
            .and_then(Value::as_table)
            .ok_or("[storage] is missing from the effective configuration")?;
        let get = |key: &str| {
            table
                .get(key)
                .ok_or_else(|| format!("[storage] {key} has no value"))
        };
        let bool_of = |key: &str| -> Result<bool, String> {
            get(key)?
                .as_bool()
                .ok_or_else(|| format!("[storage] {key} must be true or false"))
        };
        let bytes_of = |key: &str| -> Result<u64, String> {
            let value = get(key)?;
            match value {
                Value::Integer(bytes) => u64::try_from(*bytes).ok(),
                Value::String(text) => parse_bytes(text),
                _ => None,
            }
            .ok_or_else(|| {
                format!(
                    "[storage] {key} must be a byte count: an integer, or a number with B, KiB, \
                     MiB, GiB or TiB (like \"20GiB\")"
                )
            })
        };
        let count_of = |key: &str| -> Result<u64, String> {
            get(key)?
                .as_integer()
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| format!("[storage] {key} must be a non-negative integer"))
        };
        let path_of = |key: &str| -> Result<Option<PathBuf>, String> {
            match table.get(key) {
                None => Ok(None),
                Some(Value::String(text)) => expand_path(text)
                    .map(Some)
                    .map_err(|why| format!("[storage] {key} = \"{text}\" {why}")),
                Some(_) => Err(format!(
                    "[storage] {key} must be a path: absolute, or starting with ~/"
                )),
            }
        };
        let build_cache_root = path_of("build_cache_root")?;
        let (max_bytes, max_bytes_source) = match get("max_bytes")? {
            Value::String(text) if text.trim() == "auto" => {
                // Measured where the warm keys live: the budget has to hold one of them.
                let volume = match &build_cache_root {
                    Some(root) => root.clone(),
                    None => cache_home()?,
                };
                let free = crate::storage::measure_volume(&volume).free.ok();
                (auto_max_bytes(free), MaxBytesSource::Auto { free })
            }
            _ => (bytes_of("max_bytes")?, MaxBytesSource::Set),
        };
        Ok(StoragePolicy {
            max_bytes,
            max_bytes_source,
            min_free_bytes: bytes_of("min_free_bytes")?,
            auto_gc: bool_of("auto_gc")?,
            keep_days: count_of("keep_days")?,
            keep_tasks: usize::try_from(count_of("keep_tasks")?)
                .map_err(|_| "[storage] keep_tasks is too large".to_string())?,
            keep_campaigns: usize::try_from(count_of("keep_campaigns")?)
                .map_err(|_| "[storage] keep_campaigns is too large".to_string())?,
            keep_surveys: usize::try_from(count_of("keep_surveys")?)
                .map_err(|_| "[storage] keep_surveys is too large".to_string())?,
            keep_worker_transcripts: bool_of("keep_worker_transcripts")?,
            keep_gate_pull_requests: bool_of("keep_gate_pull_requests")?,
            sandbox_root: path_of("sandbox_root")?,
            build_cache_root,
        })
    }

    /// `[task] rust_toolchain_policy`: the operator's Rust toolchain mapping file
    /// (docs/task-execution/rust-toolchain.md), from the user's configuration, a project's
    /// `.af/af.toml` or the environment, `AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` above every
    /// layer. `None` when nothing sets it.
    pub(crate) fn rust_toolchain_policy(&self) -> Result<Option<PathBuf>, String> {
        match self
            .effective
            .get("task")
            .and_then(Value::as_table)
            .and_then(|table| table.get("rust_toolchain_policy"))
        {
            None => Ok(None),
            Some(Value::String(text)) => expand_path(text).map(Some).map_err(|why| {
                format!(
                    "[task] rust_toolchain_policy = \"{text}\" ({}) {why}",
                    self.origin_text("task.rust_toolchain_policy")
                )
            }),
            Some(_) => Err(format!(
                "[task] rust_toolchain_policy ({}) must be a path: absolute, or starting with ~/",
                self.origin_text("task.rust_toolchain_policy")
            )),
        }
    }

    /// Where the value of the dotted `key` came from, as `af config show --origin` says it.
    pub(crate) fn origin_text(&self, key: &str) -> String {
        self.origins
            .get(key)
            .map(|origin| match (&origin.path, origin.line) {
                (Some(path), Some(line)) => format!("{} {}:{line}", origin.layer, path.display()),
                (Some(path), None) => format!("{} {}", origin.layer, path.display()),
                (None, _) => origin.layer.to_string(),
            })
            .unwrap_or_else(|| "?".into())
    }
}

/// The environment variable that names the Rust toolchain mapping above every layer; it is
/// the configuration's `task.rust_toolchain_policy`, its origin `environment`.
pub(crate) const RUST_TOOLCHAIN_POLICY_ENV: &str = "AF_TASK_RUST_TOOLCHAIN_POLICY_FILE";

/// A path a setting names: absolute, or `~/…` below `HOME`. Anything else says why not.
pub(crate) fn expand_path(text: &str) -> Result<PathBuf, String> {
    let path = match text.strip_prefix("~/") {
        Some(rest) => home()?.join(rest),
        None => PathBuf::from(text),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("is not a path af can use: write it absolute, or starting with ~/".into())
    }
}

impl Config {
    /// The `[telemetry]` table, typed and bounded (ADR-0153): a value outside its bound, an
    /// unknown key, or `sink = "otlp"` without `sink_endpoint` refuses the load, naming the key.
    /// Only machine layers set it; `max_bytes` is bounded by `[storage] max_bytes`.
    pub(crate) fn telemetry_policy(&self) -> Result<review_telemetry::level::Settings, String> {
        use review_telemetry::level::Raw;
        let table = self
            .effective
            .get("telemetry")
            .and_then(Value::as_table)
            .ok_or("[telemetry] is missing from the effective configuration")?;
        let storage = self.storage_policy()?;
        let names: Vec<String> = table.keys().cloned().collect();
        review_telemetry::level::Settings::from_values(
            &names,
            |name| {
                table.get(name).map(|value| match value {
                    Value::String(text) => Raw::String(text.clone()),
                    Value::Integer(number) => Raw::Integer(*number),
                    Value::Boolean(flag) => Raw::Bool(*flag),
                    other => Raw::Other(other.to_string()),
                })
            },
            storage.max_bytes,
        )
    }
}

/// A byte count as `[storage]` spells one: digits, optionally followed by `B`, `KiB`, `MiB`,
/// `GiB` or `TiB` (binary units), with optional spaces between.
pub(crate) fn parse_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let factor: u64 = match unit.trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    number.checked_mul(factor)
}

pub(crate) fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let number: u64 = number.parse().ok()?;
    let seconds = match unit {
        "s" => number,
        "m" => number.checked_mul(60)?,
        "h" => number.checked_mul(3600)?,
        "d" => number.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

// ------------------------------------------------------------------------------------------
// directories

fn absolute_env(name: &str) -> Result<Option<PathBuf>, String> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(format!("{name} must be absolute"));
            }
            Ok(Some(path))
        }
        _ => Ok(None),
    }
}

pub(crate) fn home() -> Result<PathBuf, String> {
    absolute_env("HOME")?.ok_or_else(|| "HOME is not set or not absolute".to_string())
}

fn xdg(name: &str, fallback: &str) -> Result<PathBuf, String> {
    match absolute_env(name)? {
        Some(path) => Ok(path),
        None => Ok(home()?.join(fallback)),
    }
}

/// `path` with its longest existing prefix resolved, and the rest as given: the directory a
/// root names, whatever links lead to it. A HOME under a symlinked directory (macOS `/tmp`, a
/// home on another volume) then gives the roots af keeps state in without a link in them, so
/// the Storage Budget, which never follows one, measures and removes beneath them instead of
/// skipping them.
pub(crate) fn canonical_root(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(&existing) {
            for component in suffix.into_iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        match existing.file_name() {
            Some(name) => {
                suffix.push(name.to_os_string());
                existing.pop();
            }
            None => return path.to_path_buf(),
        }
    }
}

pub(crate) fn config_home() -> Result<PathBuf, String> {
    xdg("XDG_CONFIG_HOME", ".config").map(|path| canonical_root(&path))
}
pub(crate) fn state_home() -> Result<PathBuf, String> {
    xdg("XDG_STATE_HOME", ".local/state").map(|path| canonical_root(&path))
}
pub(crate) fn data_home() -> Result<PathBuf, String> {
    xdg("XDG_DATA_HOME", ".local/share").map(|path| canonical_root(&path))
}
pub(crate) fn cache_home() -> Result<PathBuf, String> {
    xdg("XDG_CACHE_HOME", ".cache").map(|path| canonical_root(&path))
}
pub(crate) fn bin_home() -> Result<PathBuf, String> {
    xdg("XDG_BIN_HOME", ".local/bin")
}

/// The git toplevel at or above `start`, if any: the first ancestor whose `.git` is a
/// repository git would open, not merely a path by that name.
pub(crate) fn git_toplevel(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).ok()?;
    start
        .ancestors()
        .find(|dir| is_git_repository(&dir.join(".git")))
        .map(Path::to_path_buf)
}

/// A `.git` git itself opens: a directory holding `HEAD`, or a linked worktree's file naming
/// a `gitdir:` that holds `HEAD`. An empty directory or an arbitrary file is neither.
fn is_git_repository(dot_git: &Path) -> bool {
    if dot_git.is_dir() {
        return dot_git.join("HEAD").is_file();
    }
    let Ok(text) = std::fs::read_to_string(dot_git) else {
        return false;
    };
    let Some(target) = text.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let target = Path::new(target.trim());
    let dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        dot_git.parent().unwrap_or(Path::new("")).join(target)
    };
    dir.join("HEAD").is_file()
}

// ------------------------------------------------------------------------------------------
// loading

/// A load's error and the byte range of the file it names in that text, when it names one, so
/// a display too narrow for the whole text can shorten the path as one path, spaces and all,
/// and keep the reason. The text is the error every other caller reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) text: String,
    pub(crate) path: Option<Range<usize>>,
}

impl Refusal {
    /// `<before><path><after>`, naming `path`.
    pub(crate) fn naming(before: &str, path: &Path, after: &str) -> Refusal {
        let named = path.display().to_string();
        let start = before.len();
        Refusal {
            text: format!("{before}{named}{after}"),
            path: Some(start..start + named.len()),
        }
    }
}

impl From<String> for Refusal {
    fn from(text: String) -> Refusal {
        Refusal { text, path: None }
    }
}

impl From<&str> for Refusal {
    fn from(text: &str) -> Refusal {
        Refusal::from(text.to_owned())
    }
}

impl From<Refusal> for String {
    fn from(refusal: Refusal) -> String {
        refusal.text
    }
}

/// The whole ladder, for `af config show` and project-facing settings.
pub(crate) fn load(repo: Option<&Path>) -> Result<Config, String> {
    Ok(load_naming(repo)?)
}

/// [`load`], its error naming the file it is about.
pub(crate) fn load_naming(repo: Option<&Path>) -> Result<Config, Refusal> {
    load_with(repo, false)
}

/// Built-in, system, user, and environment only: the layers a repository cannot write. This is
/// the only load `af self` and dispatch consult.
pub(crate) fn load_machine() -> Result<Config, String> {
    Ok(load_machine_naming()?)
}

/// [`load_machine`], its error naming the file it is about.
pub(crate) fn load_machine_naming() -> Result<Config, Refusal> {
    load_with(None, true)
}

/// The machine-owned inputs of a load: the system and user directories and the environment.
/// Every command reads the machine's own; a test pins them, so a render of the ladder cannot
/// depend on the machine running it.
pub(crate) struct MachineRoots {
    pub(crate) system: PathBuf,
    pub(crate) user: PathBuf,
    pub(crate) environment: Vec<(String, String)>,
}

impl MachineRoots {
    fn current() -> Result<Self, String> {
        Ok(Self {
            system: PathBuf::from("/etc/af"),
            user: config_home()?.join("af"),
            environment: std::env::vars().collect(),
        })
    }
}

fn load_with(repo: Option<&Path>, machine_scope: bool) -> Result<Config, Refusal> {
    load_rooted(repo, machine_scope, &MachineRoots::current()?)
}

pub(crate) fn load_rooted(
    repo: Option<&Path>,
    machine_scope: bool,
    roots: &MachineRoots,
) -> Result<Config, Refusal> {
    let start = match repo {
        Some(repo) => repo.to_path_buf(),
        None => std::env::current_dir().map_err(|error| format!("current directory: {error}"))?,
    };
    let toplevel = git_toplevel(&start);
    let mut files = Vec::new();
    let mut effective = toml::from_str::<Value>(&built_in()).expect("built-in defaults parse");
    let mut origins = BTreeMap::new();
    record_origins(
        &effective,
        "",
        "built-in",
        None,
        &BTreeMap::new(),
        &mut origins,
    );

    let mut planned: Vec<(&'static str, PathBuf)> = Vec::new();
    let system = &roots.system;
    planned.push(("system", system.join("config.toml")));
    planned.extend(conf_d(system).into_iter().map(|path| ("system", path)));
    let user = &roots.user;
    planned.push(("user", user.join("config.toml")));
    planned.extend(conf_d(user).into_iter().map(|path| ("user", path)));
    let directory_root = toplevel
        .clone()
        .unwrap_or_else(|| std::fs::canonicalize(&start).unwrap_or(start.clone()));
    let mut ancestors: Vec<&Path> = directory_root.ancestors().collect();
    if toplevel.is_some() {
        // Inside a repository the toplevel itself is the project layer, not a directory layer.
        ancestors.retain(|dir| *dir != directory_root.as_path());
    }
    ancestors.reverse();
    if !machine_scope {
        for dir in ancestors {
            planned.push(("directory", dir.join(".af/af.toml")));
        }
        if let Some(toplevel) = &toplevel {
            planned.push(("project", toplevel.join(".af/af.toml")));
            planned.push(("local", toplevel.join(".af/af.local.toml")));
        }
    }

    let mut ignored = Vec::new();
    for (layer, path) in planned {
        // A link where a layer belongs is present, so that reading it refuses it in one line;
        // it is never followed to whatever it names.
        let present = std::fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_file() || metadata.file_type().is_symlink());
        if present {
            let text = read_layer(&path)
                .map_err(|error| Refusal::naming("reading ", &path, &format!(": {error}")))?;
            let mut value: Value = toml::from_str(&text).map_err(|error| {
                Refusal::naming(
                    "",
                    &path,
                    &review_config::excerpt::bounded(&format!(": {}", error.message())),
                )
            })?;
            if matches!(layer, "directory" | "project" | "local")
                && let Some(table) = value.as_table_mut()
            {
                for (name, topic) in MACHINE_ONLY {
                    if table.remove(name).is_some() {
                        ignored.push(format!(
                            "[{name}] in {} ({layer} layer): machine-only table, ignored — see af help {topic}",
                            path.display()
                        ));
                    }
                }
            }
            let lines = key_lines(&text);
            record_origins(&value, "", layer, Some(&path), &lines, &mut origins);
            merge(&mut effective, value);
        }
        files.push(LayerFile {
            layer,
            path,
            present,
        });
    }

    for (name, raw) in roots.environment.iter().cloned() {
        let Some(rest) = name.strip_prefix("AF_") else {
            continue;
        };
        let Some((table, key)) = rest.split_once("__") else {
            continue;
        };
        if table.is_empty() || key.is_empty() || key.contains("__") {
            continue;
        }
        let table = table.to_ascii_lowercase();
        let key = key.to_ascii_lowercase();
        let value = toml::from_str::<Value>(&format!("v = {raw}"))
            .ok()
            .and_then(|doc| doc.get("v").cloned())
            .unwrap_or(Value::String(raw));
        let mut overlay = toml::map::Map::new();
        let mut inner = toml::map::Map::new();
        inner.insert(key.clone(), value);
        overlay.insert(table.clone(), Value::Table(inner));
        origins.insert(
            format!("{table}.{key}"),
            Origin {
                layer: "environment",
                path: Some(PathBuf::from(name)),
                line: None,
            },
        );
        merge(&mut effective, Value::Table(overlay));
    }
    // The variable that named the Rust toolchain mapping before the configuration did stays its
    // override, above every layer, and `af config show --origin` names it.
    if let Some((name, raw)) = roots
        .environment
        .iter()
        .find(|(name, value)| name == RUST_TOOLCHAIN_POLICY_ENV && !value.is_empty())
    {
        let mut inner = toml::map::Map::new();
        inner.insert("rust_toolchain_policy".into(), Value::String(raw.clone()));
        let mut overlay = toml::map::Map::new();
        overlay.insert("task".into(), Value::Table(inner));
        origins.insert(
            "task.rust_toolchain_policy".into(),
            Origin {
                layer: "environment",
                path: Some(PathBuf::from(name)),
                line: None,
            },
        );
        merge(&mut effective, Value::Table(overlay));
    }

    Ok(Config {
        machine_scope,
        ignored,
        files,
        toplevel: if machine_scope { None } else { toplevel },
        effective,
        origins,
    })
}

/// One layer file, whole, through [`read_authority`].
fn read_layer(path: &Path) -> Result<String, String> {
    read_authority(path)
}

/// One authority file (a configuration layer, a project's `.af/af.lock`), whole, within
/// [`review_config::lock::MAX_AUTHORITY_FILE_BYTES`]: opened by the path as named, never
/// canonicalised first, without following a link at its final component, and read no further
/// than the bound. A link is refused, and so is a file of gigabytes, before it is read whole;
/// either in one line. Canonicalisation, where a caller still needs it for display or
/// ancestry, comes after this read and never decides which file is read.
pub(crate) fn read_authority(path: &Path) -> Result<String, String> {
    let bytes =
        review_store::local_fs::read_bounded(path, review_config::lock::MAX_AUTHORITY_FILE_BYTES)
            .map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn conf_d(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("conf.d")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    paths.sort();
    paths
}

/// Deep-merge `overlay` into `base`: tables merge, everything else (arrays included) replaces.
fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Table(base), Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) if existing.is_table() && value.is_table() => {
                        merge(existing, value);
                    }
                    _ => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn record_origins(
    value: &Value,
    prefix: &str,
    layer: &'static str,
    path: Option<&Path>,
    lines: &BTreeMap<String, usize>,
    origins: &mut BTreeMap<String, Origin>,
) {
    match value {
        Value::Table(table) => {
            for (key, value) in table {
                let dotted = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                record_origins(value, &dotted, layer, path, lines, origins);
            }
        }
        _ => {
            origins.insert(
                prefix.to_string(),
                Origin {
                    layer,
                    path: path.map(Path::to_path_buf),
                    line: lines.get(prefix).copied(),
                },
            );
        }
    }
}

/// Dotted leaf key → 1-based line, from the document's spans.
fn key_lines(text: &str) -> BTreeMap<String, usize> {
    let mut lines = BTreeMap::new();
    let Ok(doc) = toml_edit::Document::parse(text) else {
        return lines;
    };
    fn walk(
        table: &toml_edit::Table,
        prefix: &str,
        text: &str,
        lines: &mut BTreeMap<String, usize>,
    ) {
        for (key, item) in table.iter() {
            let dotted = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            match item {
                toml_edit::Item::Table(inner) => walk(inner, &dotted, text, lines),
                toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)) => {
                    let inner = inline.clone().into_table();
                    walk(&inner, &dotted, text, lines);
                }
                other => {
                    let span = table
                        .key(key)
                        .and_then(|key| key.span())
                        .or_else(|| other.span());
                    if let Some(span) = span {
                        let line = text[..span.start.min(text.len())]
                            .bytes()
                            .filter(|byte| *byte == b'\n')
                            .count()
                            + 1;
                        lines.insert(dotted, line);
                    }
                }
            }
        }
    }
    walk(doc.as_table(), "", text, &mut lines);
    lines
}

// ------------------------------------------------------------------------------------------
// commands

pub(crate) fn show(repo: Option<&Path>, origin: bool, json: bool) -> Result<(), String> {
    let config = load(repo)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let mut out = String::new();
    if let Some(toplevel) = &config.toplevel {
        let _ = writeln!(out, "# repository: {}", toplevel.display());
    }
    for note in &config.ignored {
        let _ = writeln!(out, "# ignored: {note}");
    }
    let mut leaves = Vec::new();
    flatten(&config.effective, "", &mut leaves);
    let width = leaves
        .iter()
        .map(|(key, value)| key.len() + 3 + value.len())
        .max()
        .unwrap_or(0);
    for (key, rendered) in leaves {
        if origin {
            let note = config.origin_text(&key);
            let _ = writeln!(out, "{:<width$}   # {note}", format!("{key} = {rendered}"));
        } else {
            let _ = writeln!(out, "{key} = {rendered}");
        }
    }
    print!("{out}");
    Ok(())
}

pub(crate) fn flatten(value: &Value, prefix: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Table(table) => {
            for (key, value) in table {
                let dotted = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(value, &dotted, out);
            }
        }
        other => out.push((prefix.to_string(), other.to_string())),
    }
}

pub(crate) fn paths(repo: Option<&Path>) -> Result<(), String> {
    let config = load(repo)?;
    for file in &config.files {
        println!(
            "{:<10} {}{}",
            file.layer,
            file.path.display(),
            if file.present { "" } else { "   (absent)" }
        );
    }
    Ok(())
}

pub(crate) fn edit(layer: LayerArg, repo: Option<&Path>) -> Result<(), String> {
    open_in_editor(&edit_target(layer, repo)?)
}

/// The file `af config edit` opens for one layer, created with `version = 1` when absent.
pub(crate) fn edit_target(layer: LayerArg, repo: Option<&Path>) -> Result<PathBuf, String> {
    let path = layer_path(layer, repo)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, "version = 1\n")
            .map_err(|error| format!("creating {}: {error}", path.display()))?;
    }
    Ok(path)
}

/// The file one layer lives in. The user layer is machine-owned and named without reading
/// the ladder, so a directory or project layer that does not parse cannot stand in its way;
/// the other layers need the ladder to know where they are.
pub(crate) fn layer_path(layer: LayerArg, repo: Option<&Path>) -> Result<PathBuf, String> {
    if layer == LayerArg::User {
        return Ok(config_home()?.join("af/config.toml"));
    }
    let config = load(repo)?;
    Ok(match layer {
        LayerArg::User => unreachable!("named above"),
        LayerArg::Directory => {
            let candidates: Vec<&LayerFile> = config
                .files
                .iter()
                .filter(|file| file.layer == "directory")
                .collect();
            match candidates.iter().rev().find(|file| file.present) {
                Some(file) => file.path.clone(),
                None => candidates
                    .last()
                    .map(|file| file.path.clone())
                    .ok_or("no directory above this one to hold a directory layer")?,
            }
        }
        LayerArg::Project | LayerArg::Local => {
            let toplevel = config
                .toplevel
                .clone()
                .ok_or("not inside a git repository; the project and local layers need one — fix: cd into the repository or pass --repo DIR")?;
            toplevel.join(if layer == LayerArg::Project {
                ".af/af.toml"
            } else {
                ".af/af.local.toml"
            })
        }
    })
}

/// The program and arguments `$EDITOR` (the value `editor`) runs to edit `path`. `af config
/// edit` and the browser's `gf` and `:e` share it.
pub(crate) fn editor_words(
    editor: Option<&str>,
    path: &Path,
) -> Result<(String, Vec<String>), String> {
    let editor = editor
        .filter(|value| !value.trim().is_empty())
        .ok_or("EDITOR is not set — fix: export EDITOR=vim, or open the file yourself")?;
    let mut words = shell_words::split(editor).map_err(|error| format!("EDITOR: {error}"))?;
    if words.is_empty() {
        return Err("EDITOR is empty — fix: export EDITOR=vim".to_owned());
    }
    let program = words.remove(0);
    words.push(path.display().to_string());
    Ok((program, words))
}

/// Run `$EDITOR` on one file and wait for it.
pub(crate) fn open_in_editor(path: &Path) -> Result<(), String> {
    let editor = std::env::var("EDITOR").ok();
    let (program, args) = editor_words(editor.as_deref(), path)?;
    let status = review_process::reap::status(std::process::Command::new(&program).args(&args))
        .map_err(|error| format!("running {program}: {error}"))?;
    if !status.success() {
        return Err(format!("{program} exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_merge_scalars_replace_arrays_replace() {
        let mut base: Value = toml::from_str("[a]\nx = 1\ny = [1, 2]\n[a.b]\nz = 1\n").unwrap();
        let overlay: Value = toml::from_str("[a]\ny = [3]\n[a.b]\nw = 2\n").unwrap();
        merge(&mut base, overlay);
        assert_eq!(base["a"]["x"].as_integer(), Some(1));
        assert_eq!(base["a"]["y"].as_array().unwrap().len(), 1);
        assert_eq!(base["a"]["b"]["z"].as_integer(), Some(1));
        assert_eq!(base["a"]["b"]["w"].as_integer(), Some(2));
    }

    #[test]
    fn key_lines_are_one_based() {
        let lines = key_lines("version = 1\n\n[self]\nauto_update = \"never\"\n");
        assert_eq!(lines.get("version"), Some(&1));
        assert_eq!(lines.get("self.auto_update"), Some(&4));
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("x"), None);
    }

    #[test]
    fn built_in_policy_is_valid() {
        let effective: Value = toml::from_str(BUILT_IN).unwrap();
        let config = Config {
            machine_scope: true,
            ignored: Vec::new(),
            files: Vec::new(),
            toplevel: None,
            effective,
            origins: BTreeMap::new(),
        };
        let policy = config.self_policy().unwrap();
        assert_eq!(policy.auto_update, AutoUpdate::Notify);
        assert_eq!(policy.install_pins, InstallPins::Ask);
        assert_eq!(policy.keep_versions, 3);
    }

    #[test]
    fn install_pins_takes_three_values_and_the_booleans_keep_their_meaning() {
        let policy = |value: &str| {
            let mut effective: Value = toml::from_str(BUILT_IN).unwrap();
            effective["self"]["install_pins"] =
                toml::from_str::<Value>(&format!("v = {value}")).unwrap()["v"].clone();
            Config {
                machine_scope: true,
                ignored: Vec::new(),
                files: Vec::new(),
                toplevel: None,
                effective,
                origins: BTreeMap::new(),
            }
            .self_policy()
            .map(|policy| policy.install_pins)
        };
        assert_eq!(policy("\"ask\""), Ok(InstallPins::Ask));
        assert_eq!(policy("\"always\""), Ok(InstallPins::Always));
        assert_eq!(policy("\"never\""), Ok(InstallPins::Never));
        assert_eq!(policy("true"), Ok(InstallPins::Always));
        assert_eq!(policy("false"), Ok(InstallPins::Never));
        for refused in ["\"sometimes\"", "1"] {
            let error = policy(refused).unwrap_err();
            assert!(error.contains("[self] install_pins"), "{error}");
        }
    }

    #[test]
    fn built_in_storage_policy_is_the_documented_default() {
        let effective: Value = toml::from_str(BUILT_IN).unwrap();
        let config = Config {
            machine_scope: true,
            ignored: Vec::new(),
            files: Vec::new(),
            toplevel: None,
            effective,
            origins: BTreeMap::new(),
        };
        let policy = config.storage_policy().unwrap();
        let MaxBytesSource::Auto { free } = policy.max_bytes_source else {
            panic!("the built-in budget is auto: {policy:?}");
        };
        assert_eq!(
            policy,
            StoragePolicy {
                max_bytes: auto_max_bytes(free),
                max_bytes_source: MaxBytesSource::Auto { free },
                min_free_bytes: 10 << 30,
                auto_gc: false,
                keep_days: 14,
                keep_tasks: 20,
                keep_campaigns: 20,
                keep_surveys: 50,
                keep_worker_transcripts: false,
                keep_gate_pull_requests: false,
                sandbox_root: None,
                build_cache_root: None,
            }
        );
    }

    /// The default budget fits one warm key on a host with room for it (finding K18): 20 GiB,
    /// or 40 GiB when the cache volume has more than 100 GiB free.
    #[test]
    fn the_auto_budget_grows_on_a_roomy_host() {
        assert_eq!(auto_max_bytes(None), 20 << 30);
        assert_eq!(auto_max_bytes(Some(100 << 30)), 20 << 30);
        assert_eq!(auto_max_bytes(Some((100 << 30) + 1)), 40 << 30);
        assert_eq!(auto_max_bytes(Some(1 << 40)), 40 << 30);
    }

    /// `[storage] sandbox_root` and `build_cache_root` parse as paths, `~/` below HOME, and a
    /// relative one is refused naming its key (finding T18).
    #[test]
    fn storage_roots_parse_as_absolute_paths() {
        let parse = |text: &str| {
            let effective: Value = toml::from_str(&format!("{BUILT_IN}\n")).unwrap();
            let mut effective = effective;
            merge(&mut effective, toml::from_str(text).unwrap());
            Config {
                machine_scope: true,
                ignored: Vec::new(),
                files: Vec::new(),
                toplevel: None,
                effective,
                origins: BTreeMap::new(),
            }
            .storage_policy()
        };
        let policy = parse(
            "[storage]\nmax_bytes = \"30GiB\"\nsandbox_root = \"/srv/af-sandboxes\"\n\
             build_cache_root = \"/srv/af-cache\"\n",
        )
        .unwrap();
        assert_eq!(policy.max_bytes, 30 << 30);
        assert_eq!(policy.max_bytes_source, MaxBytesSource::Set);
        assert_eq!(
            policy.sandbox_root.as_deref(),
            Some(Path::new("/srv/af-sandboxes"))
        );
        assert_eq!(
            policy.build_cache_root.as_deref(),
            Some(Path::new("/srv/af-cache"))
        );
        let refused = parse("[storage]\nsandbox_root = \"relative/dir\"\n").unwrap_err();
        assert!(
            refused.starts_with("[storage] sandbox_root = \"relative/dir\"")
                && refused.contains("absolute"),
            "{refused}"
        );
        let refused = parse("[storage]\nbuild_cache_root = 3\n").unwrap_err();
        assert!(
            refused.contains("[storage] build_cache_root must be a path"),
            "{refused}"
        );
    }

    /// `[task] rust_toolchain_policy`: the environment variable above every layer, then the local,
    /// project, directory, user and system layers, each origin as `af config show --origin`
    /// names it (finding C1).
    #[test]
    fn the_rust_toolchain_policy_comes_from_the_ladder_and_the_variable_overrides_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(root.join("user")).unwrap();
        let roots = |environment: Vec<(String, String)>| MachineRoots {
            system: root.join("etc"),
            user: root.join("user"),
            environment,
        };
        let load = |environment| load_rooted(Some(&repo), false, &roots(environment)).unwrap();
        assert_eq!(load(Vec::new()).rust_toolchain_policy().unwrap(), None);

        std::fs::write(
            root.join("user/config.toml"),
            "[task]\nrust_toolchain_policy = \"/operator/user.toml\"\n",
        )
        .unwrap();
        let user = load(Vec::new());
        assert_eq!(
            user.rust_toolchain_policy().unwrap().as_deref(),
            Some(Path::new("/operator/user.toml"))
        );
        assert_eq!(user.origins["task.rust_toolchain_policy"].layer, "user");

        std::fs::create_dir_all(repo.join(".af")).unwrap();
        std::fs::write(
            repo.join(".af/af.toml"),
            "[task]\nrust_toolchain_policy = \"/operator/project.toml\"\n",
        )
        .unwrap();
        let project = load(Vec::new());
        assert_eq!(
            project.rust_toolchain_policy().unwrap().as_deref(),
            Some(Path::new("/operator/project.toml"))
        );
        assert_eq!(
            project.origin_text("task.rust_toolchain_policy"),
            format!("project {}:2", repo.join(".af/af.toml").display())
        );
        // The machine scope never reads the project layer.
        let machine = load_rooted(Some(&repo), true, &roots(Vec::new())).unwrap();
        assert_eq!(
            machine.rust_toolchain_policy().unwrap().as_deref(),
            Some(Path::new("/operator/user.toml"))
        );

        let variable = load(vec![(
            RUST_TOOLCHAIN_POLICY_ENV.to_string(),
            "/operator/variable.toml".to_string(),
        )]);
        assert_eq!(
            variable.rust_toolchain_policy().unwrap().as_deref(),
            Some(Path::new("/operator/variable.toml"))
        );
        assert_eq!(
            variable.origin_text("task.rust_toolchain_policy"),
            format!("environment {RUST_TOOLCHAIN_POLICY_ENV}")
        );
        // An empty variable sets nothing.
        let empty = load(vec![(RUST_TOOLCHAIN_POLICY_ENV.to_string(), String::new())]);
        assert_eq!(
            empty.rust_toolchain_policy().unwrap().as_deref(),
            Some(Path::new("/operator/project.toml"))
        );

        std::fs::write(
            repo.join(".af/af.toml"),
            "[task]\nrust_toolchain_policy = \"mapping.toml\"\n",
        )
        .unwrap();
        let refused = load(Vec::new()).rust_toolchain_policy().unwrap_err();
        assert!(
            refused.contains("[task] rust_toolchain_policy = \"mapping.toml\" (project ")
                && refused.contains("absolute"),
            "{refused}"
        );
    }

    #[test]
    fn byte_counts_parse_in_binary_units_only() {
        assert_eq!(parse_bytes("20GiB"), Some(20 << 30));
        assert_eq!(parse_bytes("1 MiB"), Some(1 << 20));
        assert_eq!(parse_bytes("512"), Some(512));
        assert_eq!(parse_bytes("7B"), Some(7));
        assert_eq!(parse_bytes("2TiB"), Some(2 << 40));
        for bad in ["", "GiB", "1GB", "1.5GiB", "-1", "99999999999TiB"] {
            assert_eq!(parse_bytes(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_repository_storage_table_is_ignored_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.join(".af")).unwrap();
        std::fs::write(
            repo.join(".af/af.toml"),
            "[storage]\nmax_bytes = \"1B\"\nauto_gc = true\n",
        )
        .unwrap();
        let roots = MachineRoots {
            system: root.join("etc"),
            user: root.join("user"),
            environment: vec![("AF_STORAGE__KEEP_DAYS".to_string(), "3".to_string())],
        };
        let config = load_rooted(Some(&repo), false, &roots).unwrap();
        let policy = config.storage_policy().unwrap();
        assert!(
            matches!(policy.max_bytes_source, MaxBytesSource::Auto { .. })
                && policy.max_bytes >= 20 << 30,
            "the repository cannot shrink the budget: {policy:?}"
        );
        assert!(!policy.auto_gc, "the repository cannot turn collection on");
        assert_eq!(policy.keep_days, 3, "the environment is a machine layer");
        assert!(
            config
                .ignored
                .iter()
                .any(|note| note.starts_with("[storage] in ") && note.contains("project layer")),
            "{:?}",
            config.ignored
        );
        assert_eq!(config.origins["storage.max_bytes"].layer, "built-in");
        assert_eq!(config.origins["storage.keep_days"].layer, "environment");
    }

    /// ADR-0153 clause 4: `[telemetry]` is machine-only like `[self]` and `[storage]`. A
    /// project, directory or local layer that carries it is reported and ignored; the
    /// environment form `AF_TELEMETRY__<KEY>` is a machine layer and is honoured; a value
    /// outside its bound refuses the load naming the key.
    #[test]
    fn telemetry_machine_only_table_is_ignored_in_project_layers() {
        use review_telemetry::level::Level;
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let above = root.join("above");
        let repo = above.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.join(".af")).unwrap();
        std::fs::create_dir_all(above.join(".af")).unwrap();
        std::fs::write(
            repo.join(".af/af.toml"),
            "[telemetry]\nlevel = \"content\"\nsink = \"otlp\"\nsink_endpoint = \"https://collector.example\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.join(".af/af.local.toml"),
            "[telemetry]\nlevel = \"verbose\"\n",
        )
        .unwrap();
        std::fs::write(
            above.join(".af/af.toml"),
            "[telemetry]\ndir = \"/tmp/elsewhere\"\n",
        )
        .unwrap();
        let roots = |environment: Vec<(String, String)>| MachineRoots {
            system: root.join("etc"),
            user: root.join("user"),
            environment,
        };
        let config = load_rooted(Some(&repo), false, &roots(Vec::new())).unwrap();
        let policy = config.telemetry_policy().unwrap();
        assert_eq!(policy, review_telemetry::level::Settings::default());
        for layer in ["project layer", "local layer", "directory layer"] {
            assert!(
                config
                    .ignored
                    .iter()
                    .any(|note| note.starts_with("[telemetry] in ")
                        && note.contains(layer)
                        && note.contains("af help telemetry")),
                "{layer}: {:?}",
                config.ignored
            );
        }
        assert_eq!(config.origins["telemetry.level"].layer, "built-in");

        // The one-run override is a machine layer.
        let environment = vec![("AF_TELEMETRY__LEVEL".to_string(), "content".to_string())];
        let config = load_rooted(Some(&repo), false, &roots(environment)).unwrap();
        assert_eq!(config.telemetry_policy().unwrap().level, Level::Content);
        assert_eq!(config.origins["telemetry.level"].layer, "environment");

        // A value outside its bound, from a machine layer, refuses the load naming the key.
        std::fs::create_dir_all(root.join("user")).unwrap();
        std::fs::write(
            root.join("user/config.toml"),
            "[telemetry]\nlevel = \"verbose\"\n",
        )
        .unwrap();
        let config = load_rooted(Some(&repo), false, &roots(Vec::new())).unwrap();
        let refusal = config.telemetry_policy().unwrap_err();
        assert!(
            refusal.starts_with("[telemetry] level = \"verbose\""),
            "{refusal}"
        );
        let environment = vec![(
            "AF_TELEMETRY__SEGMENT_MAX_BYTES".to_string(),
            "\"1MiB\"".to_string(),
        )];
        std::fs::remove_file(root.join("user/config.toml")).unwrap();
        let config = load_rooted(Some(&repo), false, &roots(environment)).unwrap();
        let refusal = config.telemetry_policy().unwrap_err();
        assert!(refusal.contains("segment_max_bytes"), "{refusal}");
    }

    /// A layer that does not parse is refused with the text `load` always gave, and the span
    /// of exactly the file it names, spaces and all.
    #[test]
    fn a_broken_layer_is_refused_naming_its_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = root.join("a repo with spaces");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.join(".af")).unwrap();
        let file = repo.join(".af/af.toml");
        std::fs::write(&file, "this is not toml\n").unwrap();
        let roots = MachineRoots {
            system: root.join("etc"),
            user: root.join("user"),
            environment: Vec::new(),
        };
        let refusal = load_rooted(Some(&repo), false, &roots).unwrap_err();
        let reason = toml::from_str::<Value>("this is not toml\n")
            .unwrap_err()
            .message()
            .to_owned();
        assert_eq!(refusal.text, format!("{}: {reason}", file.display()));
        let span = refusal.path.clone().unwrap();
        assert_eq!(&refusal.text[span], file.display().to_string());
        assert_eq!(
            String::from(refusal),
            format!("{}: {reason}", file.display())
        );

        // A layer that cannot be read is named after `reading `; a refusal naming nothing
        // carries no span.
        let refusal = Refusal::naming("reading ", &file, ": denied");
        assert_eq!(refusal.text, format!("reading {}: denied", file.display()));
        let span = refusal.path.clone().unwrap();
        assert_eq!(&refusal.text[span], file.display().to_string());
        assert_eq!(Refusal::from("plain").path, None);
    }

    /// L10 of the 2026-10-20 audit: a layer file beyond 1 MiB — an ancestor's or the
    /// repository's `.af/af.toml` — is refused in one line naming it, never read whole; and a
    /// layer is opened as named, never through a link (V3 of the close-out): a linked project,
    /// local or directory layer is refused in one line, not followed.
    #[test]
    fn an_oversized_layer_is_refused_naming_its_file_and_a_linked_layer_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.join(".af")).unwrap();
        let file = repo.join(".af/af.toml");
        std::fs::write(&file, format!("# {}\n", "x".repeat(2 * 1024 * 1024))).unwrap();
        let roots = MachineRoots {
            system: root.join("etc"),
            user: root.join("user"),
            environment: Vec::new(),
        };
        let refusal = load_rooted(Some(&repo), false, &roots).unwrap_err();
        assert!(
            refusal
                .text
                .starts_with(&format!("reading {}: ", file.display())),
            "{}",
            refusal.text
        );
        assert!(refusal.text.contains("over the bound of 1048576 bytes"));
        assert_eq!(refusal.text.lines().count(), 1);

        let kept = root.join("dotfiles/af.toml");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "[storage]\nkeep_days = 3\n").unwrap();
        std::fs::remove_file(&file).unwrap();
        assert!(load_rooted(Some(&repo), false, &roots).is_ok());
        let local = repo.join(".af/af.local.toml");
        let ancestor = root.join(".af/af.toml");
        std::fs::create_dir_all(ancestor.parent().unwrap()).unwrap();
        for linked in [&file, &local, &ancestor] {
            std::os::unix::fs::symlink(&kept, linked).unwrap();
            let refusal = load_rooted(Some(&repo), false, &roots).unwrap_err();
            assert_eq!(
                refusal.text,
                format!(
                    "reading {}: it is a symbolic link; af reads it only as a regular file",
                    linked.display()
                )
            );
            std::fs::remove_file(linked).unwrap();
        }
        // A dangling link is refused too, never skipped as absent.
        std::os::unix::fs::symlink(root.join("nowhere.toml"), &file).unwrap();
        let refusal = load_rooted(Some(&repo), false, &roots).unwrap_err();
        assert!(refusal.text.contains("symbolic link"), "{}", refusal.text);
        assert_eq!(refusal.text.lines().count(), 1);
    }

    #[test]
    fn the_user_layer_path_ignores_a_broken_directory_layer() {
        let temp = tempfile::tempdir().unwrap();
        let above = temp.path().join("above");
        let plain = above.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(above.join(".af")).unwrap();
        std::fs::write(above.join(".af/af.toml"), "this = is not [toml\n").unwrap();
        let user = layer_path(LayerArg::User, Some(&plain)).unwrap();
        assert!(user.ends_with("af/config.toml"), "{}", user.display());
        assert!(layer_path(LayerArg::Directory, Some(&plain)).is_err());
    }

    #[test]
    fn only_a_real_git_directory_or_worktree_file_makes_a_toplevel() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        // A directory named `.git` with nothing in it, and a file that is not a gitdir pointer.
        std::fs::create_dir_all(root.join("fake/.git")).unwrap();
        std::fs::create_dir_all(root.join("junk")).unwrap();
        std::fs::write(root.join("junk/.git"), "not a repository\n").unwrap();
        assert_eq!(git_toplevel(&root.join("fake")), None);
        assert_eq!(git_toplevel(&root.join("junk")), None);
        // A repository proper, and a linked worktree whose `.git` file names it.
        std::fs::create_dir_all(root.join("real/.git")).unwrap();
        std::fs::write(root.join("real/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(root.join("real/deeper")).unwrap();
        assert_eq!(
            git_toplevel(&root.join("real/deeper")),
            Some(root.join("real"))
        );
        std::fs::create_dir_all(root.join("linked")).unwrap();
        std::fs::write(root.join("linked/.git"), "gitdir: ../real/.git\n").unwrap();
        assert_eq!(
            git_toplevel(&root.join("linked")),
            Some(root.join("linked"))
        );
        std::fs::write(root.join("linked/.git"), "gitdir: ../nowhere/.git\n").unwrap();
        assert_eq!(git_toplevel(&root.join("linked")), None);
    }
}
