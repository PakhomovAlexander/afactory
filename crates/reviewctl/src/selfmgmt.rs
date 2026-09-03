//! `af self`: the binary managing itself, and the dispatch that honours a project's pin.
//!
//! Layout: every installed version under `$XDG_DATA_HOME/af/versions/<v>/` with a receipt; the
//! default is the `$XDG_BIN_HOME/af` symlink; activation history in `$XDG_STATE_HOME/af/self.toml`;
//! the cached release check in `$XDG_CACHE_HOME/af/self/latest.toml`. Releases come from a
//! `ReleaseSource`: GitHub through `gh` (the private repository, no token stored) or a local
//! directory (`AF_RELEASE_SOURCE`, the second implementation and the test double).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::CommandFactory as _;
use clap_complete::engine::CompletionCandidate;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::{Af, Shell};
use crate::config::{self, AutoUpdate, Channel, SelfPolicy};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const TARGET: &str = env!("AF_TARGET");
pub(crate) const COMMIT: &str = env!("AF_GIT_COMMIT");
const OFFLINE_ENV: &str = "AF_SELF_OFFLINE";
const DISPATCHED_ENV: &str = "AF_DISPATCHED_FROM";
const VERSION_ENV: &str = "AF_VERSION";
const SOURCE_ENV: &str = "AF_RELEASE_SOURCE";

// ------------------------------------------------------------------------------------------
// records

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub(crate) version: String,
    pub(crate) target: String,
    pub(crate) source: String,
    pub(crate) asset: String,
    pub(crate) sha256: String,
    pub(crate) verified_by: String,
    pub(crate) installed_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SelfState {
    #[serde(default)]
    activations: Vec<Activation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_check: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Activation {
    version: String,
    at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CheckCache {
    #[serde(default)]
    checked_at_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(default)]
    channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_updated_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_updated_from: Option<String>,
}

// ------------------------------------------------------------------------------------------
// paths

pub(crate) struct Paths {
    pub(crate) versions: PathBuf,
    pub(crate) bin: PathBuf,
    pub(crate) state_file: PathBuf,
    pub(crate) cache_file: PathBuf,
}

pub(crate) fn paths() -> Result<Paths, String> {
    Ok(Paths {
        versions: config::data_home()?.join("af/versions"),
        bin: config::bin_home()?.join("af"),
        state_file: config::state_home()?.join("af/self.toml"),
        cache_file: config::cache_home()?.join("af/self/latest.toml"),
    })
}

fn version_dir(paths: &Paths, version: &str) -> PathBuf {
    paths.versions.join(version)
}

fn version_binary(paths: &Paths, version: &str) -> PathBuf {
    version_dir(paths, version).join("af")
}

fn read_receipt(paths: &Paths, version: &str) -> Option<Receipt> {
    let text = std::fs::read_to_string(version_dir(paths, version).join("receipt.toml")).ok()?;
    toml::from_str(&text).ok()
}

/// The one predicate for "af `version` is installed": the binary is a file and beside it sits a
/// receipt that names this version and this target. A bare binary someone dropped into the
/// directory is not installed; dispatch, activation, listing, and pruning all ask this.
fn installed(paths: &Paths, version: &str) -> Option<PathBuf> {
    let binary = version_binary(paths, version);
    if !binary.is_file() {
        return None;
    }
    let receipt = read_receipt(paths, version)?;
    (receipt.version == version && receipt.target == TARGET).then_some(binary)
}

fn installed_versions(paths: &Paths) -> Vec<Version> {
    let Ok(entries) = std::fs::read_dir(&paths.versions) else {
        return Vec::new();
    };
    let mut versions: Vec<Version> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| installed(paths, name).is_some())
        .filter_map(|name| Version::parse(&name).ok())
        .collect();
    versions.sort();
    versions
}

fn default_version(paths: &Paths) -> Option<String> {
    let target = std::fs::read_link(&paths.bin).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        paths.bin.parent()?.join(target)
    };
    let canonical_versions = std::fs::canonicalize(&paths.versions).ok()?;
    let canonical = std::fs::canonicalize(&target).ok()?;
    let relative = canonical.strip_prefix(&canonical_versions).ok()?;
    relative
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str().map(str::to_string))
}

fn running_receipt(paths: &Paths) -> Option<Receipt> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(exe).ok()?;
    let versions = std::fs::canonicalize(&paths.versions).ok()?;
    let relative = exe.strip_prefix(&versions).ok()?;
    let version = relative.components().next()?.as_os_str().to_str()?;
    read_receipt(paths, version)
}

fn require_receipt(paths: &Paths) -> Result<Receipt, String> {
    running_receipt(paths).ok_or_else(|| {
        format!(
            "this af was not installed by `af self` (no receipt under {}) — fix: update it with the tool that installed it, or run `af self install {VERSION}` to adopt the self-managed layout",
            paths.versions.display()
        )
    })
}

fn read_state(paths: &Paths) -> SelfState {
    std::fs::read_to_string(&paths.state_file)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_state(paths: &Paths, state: &SelfState) -> Result<(), String> {
    write_toml(&paths.state_file, state)
}

fn read_cache(paths: &Paths) -> CheckCache {
    std::fs::read_to_string(&paths.cache_file)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_cache(paths: &Paths, cache: &CheckCache) -> Result<(), String> {
    write_toml(&paths.cache_file, cache)
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let text = toml::to_string_pretty(value).map_err(|error| error.to_string())?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|error| format!("writing {}: {error}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|error| format!("renaming into {}: {error}", path.display()))
}

// ------------------------------------------------------------------------------------------
// time (no chrono: a record needs a readable stamp, nothing more)

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn iso_now() -> String {
    iso(epoch_now())
}

fn iso(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ------------------------------------------------------------------------------------------
// release sources

trait ReleaseSource {
    fn describe(&self) -> String;
    /// Every release tag, newest first is not guaranteed; the caller sorts by semver.
    fn tags(&self, include_rc: bool) -> Result<Vec<String>, String>;
    fn fetch(&self, tag: &str, asset: &str, dir: &Path) -> Result<PathBuf, String>;
}

struct GhSource {
    repo: String,
}

impl ReleaseSource for GhSource {
    fn describe(&self) -> String {
        format!("github:{}", self.repo)
    }

    fn tags(&self, include_rc: bool) -> Result<Vec<String>, String> {
        require_gh()?;
        let output = Command::new("gh")
            .args([
                "release",
                "list",
                "--repo",
                &self.repo,
                "--limit",
                "100",
                "--json",
                "tagName,isPrerelease,isDraft",
            ])
            .output()
            .map_err(|error| format!("running gh: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "gh release list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        #[derive(Deserialize)]
        struct Row {
            #[serde(rename = "tagName")]
            tag: String,
            #[serde(rename = "isPrerelease", default)]
            prerelease: bool,
            #[serde(rename = "isDraft", default)]
            draft: bool,
        }
        let rows: Vec<Row> = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("gh output: {error}"))?;
        Ok(rows
            .into_iter()
            .filter(|row| !row.draft && (include_rc || !row.prerelease))
            .map(|row| row.tag)
            .collect())
    }

    fn fetch(&self, tag: &str, asset: &str, dir: &Path) -> Result<PathBuf, String> {
        require_gh()?;
        let output = Command::new("gh")
            .args([
                "release",
                "download",
                tag,
                "--repo",
                &self.repo,
                "--pattern",
                asset,
                "--clobber",
                "--dir",
            ])
            .arg(dir)
            .output()
            .map_err(|error| format!("running gh: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "gh release download {tag} {asset}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let path = dir.join(asset);
        if !path.is_file() {
            return Err(format!("release {tag} has no asset named {asset}"));
        }
        Ok(path)
    }
}

fn require_gh() -> Result<(), String> {
    let status = Command::new("gh")
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "GitHub CLI is required to reach the private release — fix: install gh and run `gh auth login`".to_string())?;
    if !status.success() {
        return Err("gh is not authenticated — fix: gh auth login (an account with access to the release repository)".into());
    }
    Ok(())
}

/// `<root>/<tag>/<asset>` on disk. Offline installs, mirrors, and the test double.
struct DirSource {
    root: PathBuf,
}

impl ReleaseSource for DirSource {
    fn describe(&self) -> String {
        format!("dir:{}", self.root.display())
    }

    fn tags(&self, include_rc: bool) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| format!("reading {}: {error}", self.root.display()))?;
        Ok(entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|tag| include_rc || !tag.contains('-'))
            .collect())
    }

    fn fetch(&self, tag: &str, asset: &str, dir: &Path) -> Result<PathBuf, String> {
        let from = self.root.join(tag).join(asset);
        if !from.is_file() {
            return Err(format!(
                "{} has no asset {asset}",
                self.root.join(tag).display()
            ));
        }
        let to = dir.join(asset);
        std::fs::copy(&from, &to)
            .map_err(|error| format!("copying {}: {error}", from.display()))?;
        Ok(to)
    }
}

fn source(policy: &SelfPolicy) -> Box<dyn ReleaseSource> {
    match std::env::var_os(SOURCE_ENV) {
        Some(root) if !root.is_empty() => Box::new(DirSource {
            root: PathBuf::from(root),
        }),
        _ => Box::new(GhSource {
            repo: policy.source.clone(),
        }),
    }
}

fn offline() -> bool {
    std::env::var_os(OFFLINE_ENV).is_some_and(|value| !value.is_empty() && value != "0")
}

fn asset_name(version: &str) -> String {
    format!("af-v{version}-{TARGET}.tar.gz")
}

fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

fn newest(tags: &[String], channel: Channel) -> Option<Version> {
    tags.iter()
        .filter_map(|tag| parse_tag(tag))
        .filter(|version| channel == Channel::Rc || version.pre.is_empty())
        .max()
}

// ------------------------------------------------------------------------------------------
// install

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Download, verify against the release's checksums, extract, prove the version, and record a
/// receipt. Never touches the default symlink.
fn install(paths: &Paths, policy: &SelfPolicy, version: &str) -> Result<PathBuf, String> {
    if offline() {
        return Err(format!(
            "af {version} is not installed and {OFFLINE_ENV} is set — fix: unset it and run `af self install {version}`"
        ));
    }
    let src = source(policy);
    let tag = format!("v{version}");
    let asset = asset_name(version);
    let tmp = tempfile::tempdir().map_err(|error| format!("temporary directory: {error}"))?;
    eprintln!(
        "af self: installing af {version} for {TARGET} from {}",
        src.describe()
    );
    let archive = src.fetch(&tag, &asset, tmp.path())?;
    let actual = sha256_file(&archive)?;
    let (expected, verified_by) = match src.fetch(&tag, "SHA256SUMS", tmp.path()) {
        Ok(sums) => (
            find_sum(
                &std::fs::read_to_string(&sums).map_err(|error| error.to_string())?,
                &asset,
            )
            .ok_or_else(|| format!("SHA256SUMS of {tag} does not list {asset}"))?,
            "sha256sums",
        ),
        Err(_) => {
            let sidecar = src.fetch(&tag, &format!("{asset}.sha256"), tmp.path())?;
            let text = std::fs::read_to_string(&sidecar).map_err(|error| error.to_string())?;
            (
                text.split_whitespace()
                    .next()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{asset}.sha256 is empty"))?,
                "sha256-sidecar",
            )
        }
    };
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {asset}\nexpected {expected}\nactual   {actual}"
        ));
    }
    let unpack = tmp.path().join("unpack");
    std::fs::create_dir_all(&unpack).map_err(|error| error.to_string())?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpack)
        .status()
        .map_err(|error| format!("running tar: {error}"))?;
    if !status.success() {
        return Err(format!("tar could not extract {asset}"));
    }
    let extracted = unpack.join("af");
    if !extracted.is_file() {
        return Err(format!(
            "{asset} does not contain an `af` binary at its root"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&extracted, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
    }
    let reported = Command::new(&extracted)
        .arg("--version")
        .env(OFFLINE_ENV, "1")
        .output()
        .map_err(|error| format!("running the downloaded af: {error}"))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("af {version}") {
        return Err(format!(
            "release binary reported `{reported}`, expected `af {version}`"
        ));
    }
    let dir = version_dir(paths, version);
    let staging = paths
        .versions
        .join(format!(".{version}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("creating {}: {error}", staging.display()))?;
    std::fs::copy(&extracted, staging.join("af")).map_err(|error| error.to_string())?;
    let receipt = Receipt {
        version: version.to_string(),
        target: TARGET.to_string(),
        source: src.describe(),
        asset,
        sha256: actual,
        verified_by: verified_by.to_string(),
        installed_at: iso_now(),
    };
    std::fs::write(
        staging.join("receipt.toml"),
        toml::to_string_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&staging, &dir)
        .map_err(|error| format!("installing into {}: {error}", dir.display()))?;
    Ok(dir.join("af"))
}

fn find_sum(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset).then(|| hash.to_string())
    })
}

/// Retarget the default symlink atomically and record the activation.
fn set_default(paths: &Paths, version: &str) -> Result<(), String> {
    let binary = version_binary(paths, version);
    if !binary.is_file() {
        return Err(format!(
            "af {version} is not installed — fix: af self install {version}"
        ));
    }
    if let Some(parent) = paths.bin.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    if paths.bin.exists()
        && std::fs::symlink_metadata(&paths.bin).is_ok_and(|m| !m.file_type().is_symlink())
    {
        return Err(format!(
            "{} exists and is not a symlink — fix: move it aside; af manages this path as a symlink into {}",
            paths.bin.display(),
            paths.versions.display()
        ));
    }
    let tmp = paths
        .bin
        .with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&binary, &tmp)
        .map_err(|error| format!("linking {}: {error}", tmp.display()))?;
    #[cfg(not(unix))]
    return Err("the default symlink is supported on Unix only".to_string());
    std::fs::rename(&tmp, &paths.bin)
        .map_err(|error| format!("activating {}: {error}", paths.bin.display()))?;
    let mut state = read_state(paths);
    if state
        .activations
        .last()
        .is_none_or(|last| last.version != version)
    {
        state.activations.push(Activation {
            version: version.to_string(),
            at: iso_now(),
        });
    }
    write_state(paths, &state)
}

// ------------------------------------------------------------------------------------------
// pins and dispatch

/// The `af` version a repository pins, and where the lock is.
pub(crate) fn pinned_version(repo: &Path) -> Option<(String, PathBuf)> {
    let toplevel = config::git_toplevel(repo)?;
    let lock = toplevel.join(".af/af.lock");
    let text = std::fs::read_to_string(&lock).ok()?;
    let lockfile = review_config::lock::Lockfile::from_toml(&text).ok()?;
    lockfile.af_version.map(|version| (version, lock))
}

fn repo_from_argv(argv: &[String]) -> PathBuf {
    let mut iter = argv.iter().skip(1);
    while let Some(word) = iter.next() {
        if word == "--repo" {
            if let Some(value) = iter.next() {
                return PathBuf::from(value);
            }
        } else if let Some(value) = word.strip_prefix("--repo=") {
            return PathBuf::from(value);
        }
    }
    PathBuf::from(".")
}

fn exempt_from_dispatch(argv: &[String]) -> bool {
    let first = argv.get(1).map(String::as_str);
    matches!(
        first,
        None | Some("self" | "help" | "completions" | "config")
    ) || argv
        .iter()
        .skip(1)
        .any(|word| matches!(word.as_str(), "--version" | "-V" | "--help" | "-h"))
}

/// Exec the version this project pins, when it is not the one running. Installs it on demand.
/// Returns only when the running binary should continue.
pub(crate) fn maybe_dispatch(argv: &[String]) {
    if exempt_from_dispatch(argv) || std::env::var_os(DISPATCHED_ENV).is_some() {
        return;
    }
    let requested = match std::env::var(VERSION_ENV) {
        Ok(version) if !version.trim().is_empty() => Some((version.trim().to_string(), None)),
        _ => pinned_version(&repo_from_argv(argv)).map(|(version, lock)| (version, Some(lock))),
    };
    let Some((version, lock)) = requested else {
        return;
    };
    if version == VERSION {
        return;
    }
    let Ok(paths) = paths() else {
        return;
    };
    let why = match &lock {
        Some(lock) => format!("{} is pinned by af {version}", lock.display()),
        None => format!("{VERSION_ENV}={version} was requested"),
    };
    let binary = match installed(&paths, &version) {
        Some(binary) => binary,
        None => {
            let policy = config::load_machine().and_then(|config| config.self_policy());
            let attempt = match policy {
                Err(error) => Err(format!("self policy: {error}")),
                Ok(_) if offline() => Err(format!("{OFFLINE_ENV} is set")),
                Ok(policy) if !policy.install_pins => {
                    Err("[self] install_pins = false".to_string())
                }
                Ok(policy) => install(&paths, &policy, &version),
            };
            match attempt {
                Ok(binary) => binary,
                Err(reason) => {
                    // Fail closed whenever running on would mean an older binary interpreting a
                    // newer release's pin, or ignoring an explicit request. An older pin under a
                    // newer binary is the #47 case: proceed and say so.
                    let explicit = lock.is_none();
                    let newer = match (Version::parse(&version), Version::parse(VERSION)) {
                        (Ok(pinned), Ok(running)) => pinned > running,
                        _ => true,
                    };
                    if explicit || newer {
                        eprintln!(
                            "af: {why}, {} this af {VERSION}, and that release is not installed ({reason}) — fix: af self install {version}",
                            if explicit { "not" } else { "newer than" }
                        );
                        std::process::exit(1);
                    }
                    eprintln!(
                        "af: {why}; that release is not installed ({reason}), so this af {VERSION} runs instead — fix: af self install {version}"
                    );
                    return;
                }
            }
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = Command::new(&binary)
            .args(&argv[1..])
            .env(DISPATCHED_ENV, VERSION)
            .exec();
        eprintln!(
            "af: could not exec {} ({error}); running af {VERSION}",
            binary.display()
        );
    }
}

// ------------------------------------------------------------------------------------------
// update policy: the detached check and the notice

fn ci() -> bool {
    std::env::var_os("CI")
        .is_some_and(|value| !value.is_empty() && value != "0" && value != "false")
}

/// After a command's result is written: maybe spawn the detached check, then print the notice.
pub(crate) fn after_command(argv: &[String]) {
    if exempt_from_dispatch(argv)
        || std::env::var_os(DISPATCHED_ENV).is_some()
        || offline()
        || ci()
        || argv.iter().any(|word| word == "--json")
        || !std::io::stderr().is_terminal()
    {
        return;
    }
    let Ok(paths) = paths() else { return };
    let Ok(policy) = config::load_machine().and_then(|config| config.self_policy()) else {
        return;
    };
    if !policy.update_check || policy.auto_update == AutoUpdate::Never {
        return;
    }
    let mut cache = read_cache(&paths);
    let stale =
        epoch_now().saturating_sub(cache.checked_at_epoch) >= policy.check_every.as_secs().max(1);
    if stale {
        if let Ok(exe) = std::env::current_exe() {
            let mut command = Command::new(exe);
            command
                .args(["self", "refresh-check"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                command.process_group(0);
            }
            let _ = command.spawn();
        }
    }
    if let (Some(to), Some(from)) = (cache.auto_updated_to.take(), cache.auto_updated_from.take()) {
        eprintln!("af self: updated the default from {from} to {to}; new shells run it");
        let _ = write_cache(&paths, &cache);
    } else if let Some(latest) = cache.latest.as_deref().and_then(|v| Version::parse(v).ok())
        && let Ok(running) = Version::parse(VERSION)
        && latest > running
        && policy.auto_update == AutoUpdate::Notify
    {
        eprintln!("af {latest} is available (running {VERSION}) — af self update");
    }
}

/// The detached child: refresh the cache, and under `always` install and activate.
pub(crate) fn refresh_check() -> Result<(), String> {
    let paths = paths()?;
    let policy = config::load_machine()?.self_policy()?;
    if offline() {
        return Ok(());
    }
    let src = source(&policy);
    let tags = src.tags(policy.channel == Channel::Rc)?;
    let latest = newest(&tags, policy.channel);
    let mut cache = read_cache(&paths);
    cache.checked_at_epoch = epoch_now();
    cache.channel = match policy.channel {
        Channel::Stable => "stable".into(),
        Channel::Rc => "rc".into(),
    };
    cache.latest = latest.as_ref().map(ToString::to_string);
    let mut state = read_state(&paths);
    state.last_check = Some(iso_now());
    write_state(&paths, &state)?;
    if policy.auto_update == AutoUpdate::Always
        && let Some(latest) = &latest
        && Version::parse(VERSION).is_ok_and(|running| *latest > running)
        && running_receipt(&paths).is_some()
    {
        let from = default_version(&paths).unwrap_or_else(|| VERSION.to_string());
        let latest = latest.to_string();
        if installed(&paths, &latest).is_none() {
            install(&paths, &policy, &latest)?;
        }
        set_default(&paths, &latest)?;
        cache.auto_updated_to = Some(latest);
        cache.auto_updated_from = Some(from);
    }
    write_cache(&paths, &cache)
}

// ------------------------------------------------------------------------------------------
// commands

#[derive(Serialize)]
struct StatusView {
    version: &'static str,
    target: &'static str,
    commit: &'static str,
    running: PathBuf,
    receipt: Option<Receipt>,
    default: Option<String>,
    installed: Vec<String>,
    pin: Option<PinView>,
    last_check: Option<String>,
    latest: Option<String>,
    policy: PolicyView,
    paths: BTreeMap<&'static str, PathBuf>,
}

#[derive(Serialize)]
struct PinView {
    version: String,
    lock: PathBuf,
    installed: bool,
}

#[derive(Serialize)]
struct PolicyView {
    update_check: bool,
    check_every_secs: u64,
    auto_update: &'static str,
    channel: &'static str,
    install_pins: bool,
    keep_versions: usize,
    source: String,
}

pub(crate) fn status(json: bool) -> Result<(), String> {
    let paths = paths()?;
    let policy = config::load_machine()?.self_policy()?;
    let running = std::env::current_exe().map_err(|error| error.to_string())?;
    let installed_list: Vec<String> = installed_versions(&paths)
        .iter()
        .map(ToString::to_string)
        .collect();
    let pin = pinned_version(Path::new(".")).map(|(version, lock)| PinView {
        installed: installed(&paths, &version).is_some(),
        version,
        lock,
    });
    let state = read_state(&paths);
    let cache = read_cache(&paths);
    let view = StatusView {
        version: VERSION,
        target: TARGET,
        commit: COMMIT,
        running: running.clone(),
        receipt: running_receipt(&paths),
        default: default_version(&paths),
        installed: installed_list,
        pin,
        last_check: state.last_check,
        latest: cache.latest,
        policy: PolicyView {
            update_check: policy.update_check,
            check_every_secs: policy.check_every.as_secs(),
            auto_update: match policy.auto_update {
                AutoUpdate::Notify => "notify",
                AutoUpdate::Always => "always",
                AutoUpdate::Never => "never",
            },
            channel: match policy.channel {
                Channel::Stable => "stable",
                Channel::Rc => "rc",
            },
            install_pins: policy.install_pins,
            keep_versions: policy.keep_versions,
            source: policy.source.clone(),
        },
        paths: BTreeMap::from([
            ("versions", paths.versions.clone()),
            ("default", paths.bin.clone()),
            ("state", paths.state_file.clone()),
            ("cache", paths.cache_file.clone()),
        ]),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    println!("af {VERSION} ({TARGET}, {COMMIT})");
    println!(
        "running:    {}{}",
        view.running.display(),
        if view.receipt.is_some() {
            ""
        } else {
            "   (no receipt: not installed by af self)"
        }
    );
    match &view.default {
        Some(version) => println!("default:    {} -> af {version}", paths.bin.display()),
        None => println!("default:    {} (absent)", paths.bin.display()),
    }
    println!(
        "installed:  {}",
        if view.installed.is_empty() {
            "none".to_string()
        } else {
            view.installed.join(", ")
        }
    );
    match &view.pin {
        Some(pin) => println!(
            "pin here:   af {} ({}){}",
            pin.version,
            pin.lock.display(),
            if pin.installed {
                ""
            } else {
                "   not installed — af self install"
            }
        ),
        None => println!("pin here:   none"),
    }
    println!(
        "last check: {}{}",
        view.last_check.as_deref().unwrap_or("never"),
        view.latest
            .as_deref()
            .map(|v| format!(" · latest {v}"))
            .unwrap_or_default()
    );
    println!(
        "policy:     auto_update={} channel={} check_every={}s install_pins={} keep_versions={} source={}",
        view.policy.auto_update,
        view.policy.channel,
        view.policy.check_every_secs,
        view.policy.install_pins,
        view.policy.keep_versions,
        view.policy.source
    );
    Ok(())
}

pub(crate) fn update(check: bool, version: Option<String>, rc: bool) -> Result<(), String> {
    let paths = paths()?;
    let policy = config::load_machine()?.self_policy()?;
    let channel = if rc { Channel::Rc } else { policy.channel };
    let src = source(&policy);
    let target = match version {
        Some(version) => Version::parse(version.trim_start_matches('v'))
            .map_err(|error| format!("--version {version}: {error}"))?,
        None => newest(&src.tags(channel == Channel::Rc)?, channel)
            .ok_or_else(|| format!("no release found at {}", src.describe()))?,
    };
    let running = Version::parse(VERSION).map_err(|error| error.to_string())?;
    let mut cache = read_cache(&paths);
    cache.checked_at_epoch = epoch_now();
    cache.latest = Some(target.to_string());
    let _ = write_cache(&paths, &cache);
    if check {
        if target > running {
            println!("af {target} is available (running {VERSION})");
            std::process::exit(10);
        }
        println!("af {VERSION} is up to date");
        return Ok(());
    }
    require_receipt(&paths)?;
    let target = target.to_string();
    if installed(&paths, &target).is_none() {
        install(&paths, &policy, &target)?;
    }
    set_default(&paths, &target)?;
    println!("af {target} is now the default ({})", paths.bin.display());
    Ok(())
}

pub(crate) fn rollback() -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    let state = read_state(&paths);
    let current = default_version(&paths);
    let previous = state
        .activations
        .iter()
        .rev()
        .map(|activation| activation.version.clone())
        .find(|version| Some(version) != current.as_ref())
        .ok_or("nothing to roll back to: only one version was ever activated")?;
    set_default(&paths, &previous)?;
    println!("af {previous} is the default again");
    Ok(())
}

pub(crate) fn install_command(version: &str) -> Result<(), String> {
    let paths = paths()?;
    let policy = config::load_machine()?.self_policy()?;
    let version = version.trim_start_matches('v');
    Version::parse(version).map_err(|error| format!("{version}: {error}"))?;
    let binary = match installed(&paths, version) {
        Some(binary) => binary,
        None => install(&paths, &policy, version)?,
    };
    println!("af {version} installed at {}", binary.display());
    if default_version(&paths).is_none() {
        set_default(&paths, version)?;
        println!("af {version} is the default ({})", paths.bin.display());
    }
    Ok(())
}

pub(crate) fn remove(version: &str) -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    let version = version.trim_start_matches('v');
    if default_version(&paths).as_deref() == Some(version) {
        return Err(format!(
            "af {version} is the default — fix: af self update or af self rollback first"
        ));
    }
    if let Some((pinned, lock)) = pinned_version(Path::new("."))
        && pinned == version
    {
        return Err(format!(
            "af {version} is pinned by {} — fix: run this outside that project",
            lock.display()
        ));
    }
    let dir = version_dir(&paths, version);
    if !dir.is_dir() {
        return Err(format!("af {version} is not installed"));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|error| format!("removing {}: {error}", dir.display()))?;
    println!("af {version} removed");
    Ok(())
}

pub(crate) fn prune() -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    let policy = config::load_machine()?.self_policy()?;
    let keep = policy.keep_versions;
    let default = default_version(&paths);
    let pinned = pinned_version(Path::new(".")).map(|(version, _)| version);
    let versions = installed_versions(&paths);
    let mut removed = 0;
    let total = versions.len();
    for (index, version) in versions.iter().enumerate() {
        let version = version.to_string();
        let protected = Some(&version) == default.as_ref() || Some(&version) == pinned.as_ref();
        let within_keep = total - index <= keep;
        if protected || within_keep {
            continue;
        }
        std::fs::remove_dir_all(version_dir(&paths, &version))
            .map_err(|error| error.to_string())?;
        println!("removed af {version}");
        removed += 1;
    }
    if removed == 0 {
        println!("nothing to prune (keeping {keep}, plus the default and any pin)");
    }
    Ok(())
}

pub(crate) fn uninstall(purge: bool) -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    if paths.bin.exists() || std::fs::symlink_metadata(&paths.bin).is_ok() {
        std::fs::remove_file(&paths.bin)
            .map_err(|error| format!("removing {}: {error}", paths.bin.display()))?;
        println!("removed {}", paths.bin.display());
    }
    if paths.versions.is_dir() {
        std::fs::remove_dir_all(&paths.versions).map_err(|error| error.to_string())?;
        println!("removed {}", paths.versions.display());
    }
    if purge {
        for dir in [
            config::config_home()?.join("af"),
            config::state_home()?.join("af"),
            config::cache_home()?.join("af"),
            config::data_home()?.join("af"),
        ] {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .map_err(|error| format!("removing {}: {error}", dir.display()))?;
                println!("removed {}", dir.display());
            }
        }
    } else {
        println!("kept config, state, and cache (add --purge to remove them)");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// completions, shell setup, man pages

pub(crate) fn completion_script(shell: Shell) -> Result<String, String> {
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(shell.name())
        .ok_or_else(|| format!("no completer for {}", shell.name()))?;
    let mut buf = Vec::new();
    completer
        .write_registration("COMPLETE", "af", "af", "af", &mut buf)
        .map_err(|error| error.to_string())?;
    String::from_utf8(buf).map_err(|error| error.to_string())
}

/// Candidates for `--campaign`: every Campaign under the default state root.
pub(crate) fn complete_campaign(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    crate::campaign_names_for_completion()
        .into_iter()
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

/// Candidates for a version argument: installed versions.
pub(crate) fn complete_version(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    paths()
        .map(|paths| installed_versions(&paths))
        .unwrap_or_default()
        .into_iter()
        .map(|version| version.to_string())
        .filter(|version| version.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

fn detect_shell() -> Option<Shell> {
    let shell = std::env::var("SHELL").ok()?;
    match Path::new(&shell).file_name()?.to_str()? {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::Powershell),
        _ => None,
    }
}

pub(crate) fn setup_shell(shell: Option<Shell>, write: bool) -> Result<(), String> {
    let shell = shell.or_else(detect_shell).ok_or(
        "could not detect the shell from $SHELL — fix: af self setup-shell --shell fish|zsh|bash",
    )?;
    let data = config::data_home()?;
    let (completion_path, rc_line): (PathBuf, Option<String>) = match shell {
        Shell::Fish => (config::config_home()?.join("fish/completions/af.fish"), None),
        Shell::Bash => (
            data.join("bash-completion/completions/af"),
            Some("# bash-completion 2.x loads it on demand; older bash: source the file from ~/.bashrc".into()),
        ),
        Shell::Zsh => (
            data.join("zsh/site-functions/_af"),
            Some(format!(
                "fpath=({} $fpath); autoload -Uz compinit && compinit   # add to ~/.zshrc before compinit",
                data.join("zsh/site-functions").display()
            )),
        ),
        Shell::Elvish => (config::config_home()?.join("elvish/lib/af.elv"), Some("use af   # in rc.elv".into())),
        Shell::Powershell => (
            config::config_home()?.join("powershell/af.ps1"),
            Some(". ~/.config/powershell/af.ps1   # in $PROFILE".into()),
        ),
    };
    let man_dir = data.join("man/man1");
    println!("shell:       {}", shell.name());
    println!("completions: {}", completion_path.display());
    println!("man pages:   {}", man_dir.display());
    if let Some(line) = &rc_line {
        println!("rc line:     {line}");
    }
    if !write {
        println!("(dry run — add --write to install)");
        return Ok(());
    }
    let script = completion_script(shell)?;
    if let Some(parent) = completion_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    std::fs::write(&completion_path, script)
        .map_err(|error| format!("writing {}: {error}", completion_path.display()))?;
    man(&man_dir)?;
    println!("written");
    Ok(())
}

pub(crate) fn man(out_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|error| format!("creating {}: {error}", out_dir.display()))?;
    let mut root = Af::command();
    root.build();
    let mut count = 0;
    render_man(&root, "af", out_dir, &mut count)?;
    for (topic, about, text) in crate::topics::TOPICS {
        let command = clap::Command::new(format!("af-{topic}"))
            .about(*about)
            .long_about(*text);
        let page = clap_mangen::Man::new(command).section("7");
        let mut buf = Vec::new();
        page.render(&mut buf).map_err(|error| error.to_string())?;
        std::fs::write(out_dir.join(format!("af-{topic}.7")), buf)
            .map_err(|error| error.to_string())?;
        count += 1;
    }
    eprintln!(
        "af self: wrote {count} man pages into {}",
        out_dir.display()
    );
    Ok(())
}

fn render_man(
    command: &clap::Command,
    name: &str,
    out_dir: &Path,
    count: &mut usize,
) -> Result<(), String> {
    if command.is_hide_set() {
        return Ok(());
    }
    let page = clap_mangen::Man::new(command.clone().name(name.to_string()));
    let mut buf = Vec::new();
    page.render(&mut buf).map_err(|error| error.to_string())?;
    std::fs::write(out_dir.join(format!("{name}.1")), buf).map_err(|error| error.to_string())?;
    *count += 1;
    for sub in command.get_subcommands() {
        render_man(sub, &format!("{name}-{}", sub.get_name()), out_dir, count)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// version

#[derive(Serialize)]
struct VersionView {
    version: &'static str,
    commit: &'static str,
    target: &'static str,
    dispatched_from: Option<String>,
    receipt: Option<PathBuf>,
}

pub(crate) fn print_version(json: bool) -> Result<(), String> {
    if !json {
        println!("af {VERSION}");
        return Ok(());
    }
    let receipt = paths().ok().and_then(|paths| {
        running_receipt(&paths)
            .map(|receipt| version_dir(&paths, &receipt.version).join("receipt.toml"))
    });
    let view = VersionView {
        version: VERSION,
        commit: COMMIT,
        target: TARGET,
        dispatched_from: std::env::var(DISPATCHED_ENV).ok(),
        receipt,
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &view).map_err(|error| error.to_string())?;
    writeln!(stdout).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_renders_known_instants() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_756_857_600), "2025-09-03T00:00:00Z");
    }

    #[test]
    fn newest_respects_channel() {
        let tags = vec!["v0.7.0".into(), "v0.8.0-rc.1".into(), "v0.6.0".into()];
        assert_eq!(newest(&tags, Channel::Stable).unwrap().to_string(), "0.7.0");
        assert_eq!(
            newest(&tags, Channel::Rc).unwrap().to_string(),
            "0.8.0-rc.1"
        );
    }

    #[test]
    fn sums_are_found_by_asset_name() {
        let sums = "abc  af-v0.8.0-x.tar.gz\ndef *af-v0.8.0-y.tar.gz\n";
        assert_eq!(find_sum(sums, "af-v0.8.0-y.tar.gz").as_deref(), Some("def"));
        assert_eq!(find_sum(sums, "nope"), None);
    }

    #[test]
    fn dispatch_exemptions() {
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(exempt_from_dispatch(&argv(&["af", "self", "status"])));
        assert!(exempt_from_dispatch(&argv(&[
            "af", "review", "plan", "--help"
        ])));
        assert!(!exempt_from_dispatch(&argv(&["af", "review", "plan"])));
        assert_eq!(
            repo_from_argv(&argv(&["af", "review", "plan", "--repo", "/x"])),
            PathBuf::from("/x")
        );
        assert_eq!(
            repo_from_argv(&argv(&["af", "review", "--repo=/y"])),
            PathBuf::from("/y")
        );
    }
}
