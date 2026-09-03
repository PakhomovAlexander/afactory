//! The configuration ladder: built-in defaults, system, user, directory, project, local, and
//! environment layers merged into one effective TOML document with the origin of every value.
//!
//! Tables deep-merge, scalars last-wins, arrays replace. The loader is lazy about the Store and
//! reads only small TOML files; `af config show --origin` is the audit surface.

use std::collections::BTreeMap;
use std::fmt::Write as _;
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
install_pins = true
keep_versions = 3
source = "PakhomovAlexander/afactory"
"#;

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
    /// `[self]` tables found in directory, project, or local layers: reported, never merged.
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
    pub(crate) install_pins: bool,
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
            install_pins: bool_of("install_pins")?,
            keep_versions,
            source: str_of("source")?,
        })
    }
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

pub(crate) fn config_home() -> Result<PathBuf, String> {
    xdg("XDG_CONFIG_HOME", ".config")
}
pub(crate) fn state_home() -> Result<PathBuf, String> {
    xdg("XDG_STATE_HOME", ".local/state")
}
pub(crate) fn data_home() -> Result<PathBuf, String> {
    xdg("XDG_DATA_HOME", ".local/share")
}
pub(crate) fn cache_home() -> Result<PathBuf, String> {
    xdg("XDG_CACHE_HOME", ".cache")
}
pub(crate) fn bin_home() -> Result<PathBuf, String> {
    xdg("XDG_BIN_HOME", ".local/bin")
}

/// The git toplevel at or above `start`, if any: the first ancestor holding `.git`.
pub(crate) fn git_toplevel(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).ok()?;
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

// ------------------------------------------------------------------------------------------
// loading

/// The whole ladder, for `af config show` and project-facing settings.
pub(crate) fn load(repo: Option<&Path>) -> Result<Config, String> {
    load_with(repo, false)
}

/// Built-in, system, user, and environment only: the layers a repository cannot write. This is
/// the only load `af self` and dispatch consult.
pub(crate) fn load_machine() -> Result<Config, String> {
    load_with(None, true)
}

fn load_with(repo: Option<&Path>, machine_scope: bool) -> Result<Config, String> {
    let start = match repo {
        Some(repo) => repo.to_path_buf(),
        None => std::env::current_dir().map_err(|error| format!("current directory: {error}"))?,
    };
    let toplevel = git_toplevel(&start);
    let mut files = Vec::new();
    let mut effective = toml::from_str::<Value>(BUILT_IN).expect("built-in defaults parse");
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
    let system = PathBuf::from("/etc/af");
    planned.push(("system", system.join("config.toml")));
    planned.extend(conf_d(&system).into_iter().map(|path| ("system", path)));
    let user = config_home()?.join("af");
    planned.push(("user", user.join("config.toml")));
    planned.extend(conf_d(&user).into_iter().map(|path| ("user", path)));
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
        let present = path.is_file();
        if present {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            let mut value: Value = toml::from_str(&text)
                .map_err(|error| format!("{}: {}", path.display(), error.message()))?;
            if matches!(layer, "directory" | "project" | "local")
                && let Some(table) = value.as_table_mut()
                && table.remove("self").is_some()
            {
                ignored.push(format!(
                    "[self] in {} ({layer} layer): machine-only table, ignored — see af help self",
                    path.display()
                ));
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

    for (name, raw) in std::env::vars() {
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

    Ok(Config {
        machine_scope,
        ignored,
        files,
        toplevel: if machine_scope { None } else { toplevel },
        effective,
        origins,
    })
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
    let Ok(doc) = toml_edit::ImDocument::parse(text) else {
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
            let note = config
                .origins
                .get(&key)
                .map(|origin| match (&origin.path, origin.line) {
                    (Some(path), Some(line)) => {
                        format!("{} {}:{line}", origin.layer, path.display())
                    }
                    (Some(path), None) => format!("{} {}", origin.layer, path.display()),
                    (None, _) => origin.layer.to_string(),
                })
                .unwrap_or_else(|| "?".into());
            let _ = writeln!(out, "{:<width$}   # {note}", format!("{key} = {rendered}"));
        } else {
            let _ = writeln!(out, "{key} = {rendered}");
        }
    }
    print!("{out}");
    Ok(())
}

fn flatten(value: &Value, prefix: &str, out: &mut Vec<(String, String)>) {
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
    let config = load(repo)?;
    let path = match layer {
        LayerArg::User => config_home()?.join("af/config.toml"),
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
    };
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, "version = 1\n")
            .map_err(|error| format!("creating {}: {error}", path.display()))?;
    }
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("EDITOR is not set — fix: export EDITOR=vim, or open the file yourself")?;
    let words = shell_words::split(&editor).map_err(|error| format!("EDITOR: {error}"))?;
    let (program, args) = words
        .split_first()
        .ok_or("EDITOR is empty — fix: export EDITOR=vim")?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status()
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
        assert_eq!(policy.keep_versions, 3);
    }
}
