//! `af self`: the binary managing itself, and the dispatch that honours a project's pin.
//!
//! Layout: every installed version under `$XDG_DATA_HOME/af/versions/<v>/` with a receipt; the
//! default is the `$XDG_BIN_HOME/af` symlink; activation history and the pinned projects this
//! machine has seen in `$XDG_STATE_HOME/af/self.toml`; the cached release check in
//! `$XDG_CACHE_HOME/af/self/latest.toml`. Releases come from a `ReleaseSource`: GitHub through
//! `gh` (the private repository, no token stored) or a local directory (`AF_RELEASE_SOURCE`, the
//! second implementation and the test double).
//!
//! What binds bytes: under a project lock, the digest the lock records for this target; outside
//! one, the release's `SHA256SUMS`, which every release signs with the key embedded at build
//! time. A pin without a digest for this target is never installed on demand.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::CommandFactory as _;
use clap_complete::engine::CompletionCandidate;
use review_config::lock::AfPin;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::{Af, Shell};
use crate::config::{self, AutoUpdate, Channel, SelfPolicy};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const TARGET: &str = env!("AF_TARGET");
pub(crate) const COMMIT: &str = env!("AF_GIT_COMMIT");
/// The minisign public key the release job signs `SHA256SUMS` with, embedded at build time from
/// `crates/af/keys/release.pub`. Empty in a build without the file (a source build).
const RELEASE_KEY: &str = env!("AF_RELEASE_KEY");
/// The oldest release `af self` activates, installs or dispatches to: the first whose
/// `SHA256SUMS` is signed. Anything older is never made the default and never dispatched to.
const OLDEST_SUPPORTED: &str = "0.8.0";
const OFFLINE_ENV: &str = "AF_SELF_OFFLINE";
const DISPATCHED_ENV: &str = "AF_DISPATCHED_FROM";
const VERSION_ENV: &str = "AF_VERSION";
const SOURCE_ENV: &str = "AF_RELEASE_SOURCE";
const KEY_ENV: &str = "AF_RELEASE_KEY";
/// Pinned projects remembered for `remove` and `prune`; the oldest entries age out.
const MAX_SEEN_PINS: usize = 64;

// ------------------------------------------------------------------------------------------
// records

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub(crate) version: String,
    pub(crate) target: String,
    pub(crate) source: String,
    pub(crate) asset: String,
    pub(crate) sha256: String,
    /// `lock` (matched the project lock's digest), `minisign` (a signed `SHA256SUMS`), or
    /// `sha256sums` (the checksum alone: a build without a release key, or `install.sh` without
    /// `minisign`).
    pub(crate) verified_by: String,
    pub(crate) installed_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SelfState {
    #[serde(default)]
    activations: Vec<Activation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_check: Option<String>,
    /// Every project lock dispatch has honoured on this machine, so `remove` and `prune` keep
    /// the versions those projects still pin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pins: Vec<SeenPin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Activation {
    version: String,
    at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeenPin {
    lock: PathBuf,
    version: String,
    seen: String,
}

/// The cached release check's shape. The unversioned shape earlier releases wrote is not read: a
/// machine with one is due a check, which writes this one (ADR-0145).
const CHECK_CACHE_SCHEMA: &str = "af/self-check@2";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckCache {
    schema: String,
    #[serde(default)]
    checked_at_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(default)]
    channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_updated: Option<AutoUpdated>,
}

impl Default for CheckCache {
    fn default() -> Self {
        CheckCache {
            schema: CHECK_CACHE_SCHEMA.to_string(),
            checked_at_epoch: 0,
            latest: None,
            channel: String::new(),
            auto_updated: None,
        }
    }
}

/// A default a background check activated, for the one notice that announces it: bound to the
/// [`DefaultMark`] that activation left, so any activation since, even an update back to `to`
/// after a rollback, makes it stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutoUpdated {
    from: String,
    to: String,
    default: DefaultMark,
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

fn running_binary_is(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(running) = std::env::current_exe().and_then(std::fs::metadata) else {
        return false;
    };
    let Ok(candidate) = std::fs::metadata(path) else {
        return false;
    };
    running.dev() == candidate.dev() && running.ino() == candidate.ino()
}

/// One installed version as the Storage Budget sees it (ADR-0144): its directory, when it was
/// installed (its receipt's modification time), and why it may never be evicted — the
/// default, a pin some project on this machine still holds, or the running binary.
pub(crate) struct InstalledVersion {
    pub(crate) directory: PathBuf,
    pub(crate) installed_unix_ms: u64,
    pub(crate) protected: Option<&'static str>,
}

pub(crate) fn installed_for_storage(paths: &Paths) -> Result<Vec<InstalledVersion>, String> {
    let default = default_version(paths);
    let pinned = pinned_everywhere(paths);
    let mut versions: Vec<InstalledVersion> = installed_versions(paths)
        .into_iter()
        .map(|version| {
            let version = version.to_string();
            let directory = version_dir(paths, &version);
            let installed_unix_ms = modified_unix_ms(&directory.join("receipt.toml"));
            let protected = protection(paths, default.as_deref(), &pinned, &version);
            InstalledVersion {
                directory,
                installed_unix_ms,
                protected,
            }
        })
        .collect();
    // A staging directory's bytes count until it is gone: an installer's, kept while it runs,
    // or one a killed installer left, which the budget may remove like an unprotected version.
    versions.extend(staging_dirs(paths).into_iter().map(|directory| {
        let installed_unix_ms = modified_unix_ms(&directory);
        let protected = staging_in_use(&directory).then_some(STAGING_IN_USE);
        InstalledVersion {
            directory,
            installed_unix_ms,
            protected,
        }
    }));
    Ok(versions)
}

fn modified_unix_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
}

const STAGING_IN_USE: &str = "staged by a running installer";

/// Whether `name` is a staging directory's: `.<version>.<pid>`, as [`begin_staging`] names it.
fn staging_name(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(version, pid)| {
            !pid.is_empty()
                && pid.bytes().all(|byte| byte.is_ascii_digit())
                && Version::parse(version).is_ok()
        })
}

/// Every staging directory under the versions, a link never counted as one.
fn staging_dirs(paths: &Paths) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(&paths.versions) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(staging_name))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// Whether an installer may still own the staging directory `dir`. `af` holds its directory
/// locked for as long as it runs, and the kernel releases the lock when it dies; `install.sh`
/// takes no lock, so the process its name carries must be gone too. Whatever cannot be asked
/// about — the lock, or the process (another user's, or a reused id) — is taken to be in use,
/// never assumed abandoned.
fn staging_in_use(dir: &Path) -> bool {
    let Ok(handle) = std::fs::File::open(dir) else {
        return true;
    };
    if rustix::fs::flock(
        &handle,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_err()
    {
        return true;
    }
    let Some(pid) = dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once('.'))
        .and_then(|(_, pid)| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return true;
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) != Err(nix::errno::Errno::ESRCH)
}

/// Remove the staging directories killed installers left. Under the versions lock: a live
/// installer creates and locks its directory under it, so one unlocked here has no owner.
fn recover_abandoned_staging(paths: &Paths) {
    for dir in staging_dirs(paths) {
        if !staging_in_use(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Why `version` may never be evicted: the default, a pin some project on this machine still
/// holds, or the running binary.
fn protection(
    paths: &Paths,
    default: Option<&str>,
    pinned: &BTreeSet<String>,
    version: &str,
) -> Option<&'static str> {
    if default == Some(version) {
        Some("the default version")
    } else if pinned.contains(version) {
        Some("pinned by a project")
    } else if running_binary_is(&version_binary(paths, version)) {
        Some("the running binary")
    } else {
        None
    }
}

/// Why `version` may not be removed now, read afresh: for the Storage Budget immediately before
/// it removes an installed version or a staging directory, under [`try_lock_versions`].
pub(crate) fn protected_now(paths: &Paths, version: &str) -> Option<&'static str> {
    if staging_name(version) {
        return staging_in_use(&paths.versions.join(version)).then_some(STAGING_IN_USE);
    }
    protection(
        paths,
        default_version(paths).as_deref(),
        &pinned_everywhere_locked(paths),
        version,
    )
}

/// The versions lock without waiting, for the Storage Budget: an `af self` that holds it now
/// is changing the versions, the default or a pin, so nothing is evicted under it.
pub(crate) fn try_lock_versions(paths: &Paths) -> Result<std::fs::File, String> {
    let path = paths.versions.join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |_| "`af self` holds the versions lock now; the next sweep tries again".to_string(),
    )?;
    Ok(file)
}

fn default_status(paths: &Paths, version: Option<&str>) -> &'static str {
    if version.is_some() {
        return "managed";
    }
    match std::fs::symlink_metadata(&paths.bin) {
        Ok(metadata) if metadata.file_type().is_symlink() => "unmanaged-symlink",
        Ok(metadata) if metadata.is_file() => "unmanaged-file",
        Ok(_) => "unmanaged-path",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => "inaccessible",
    }
}

fn running_receipt(paths: &Paths) -> Option<Receipt> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(exe).ok()?;
    let versions = std::fs::canonicalize(&paths.versions).ok()?;
    let relative = exe.strip_prefix(&versions).ok()?;
    let version = relative.components().next()?.as_os_str().to_str()?;
    read_receipt(paths, version)
}

/// The running binary's install receipt, when `af self` (or the installer) installed it.
pub(crate) fn self_receipt() -> Option<Receipt> {
    paths().ok().and_then(|paths| running_receipt(&paths))
}

fn require_receipt(paths: &Paths) -> Result<Receipt, String> {
    running_receipt(paths).ok_or_else(|| {
        format!(
            "this af was not installed by `af self` (no receipt under {}) — fix: update it with the tool that installed it, or run `af self install {VERSION}` to adopt the self-managed layout",
            paths.versions.display()
        )
    })
}

/// One writer of the installed versions, the default and the pins at a time: two dispatches
/// racing to install the same pin would otherwise replace a directory the other is about to
/// exec, and the Storage Budget re-reads the default and the pins under it before it removes a
/// version (ADR-0144).
fn install_lock(paths: &Paths) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(&paths.versions)
        .map_err(|error| format!("creating {}: {error}", paths.versions.display()))?;
    let path = paths.versions.join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("opening {}: {error}", path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|error| format!("locking {}: {error}", path.display()))?;
    Ok(file)
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
        .and_then(|text| toml::from_str::<CheckCache>(&text).ok())
        .filter(|cache| cache.schema == CHECK_CACHE_SCHEMA)
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
    std::fs::rename(&tmp, path).map_err(|error| {
        // A replacement that failed leaves the file as it was, and no temporary beside it.
        let _ = std::fs::remove_file(&tmp);
        format!("renaming into {}: {error}", path.display())
    })
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            if stderr.contains("release not found") {
                return Err(format!(
                    "release {tag} does not exist at {} — fix: `af self update --check` (add --rc for pre-releases) names the newest",
                    self.describe()
                ));
            }
            return Err(format!("gh release download {tag} {asset}: {stderr}"));
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
        .map_err(|_| {
            "GitHub CLI is required to reach the release — fix: install gh and run `gh auth login`"
                .to_string()
        })?;
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

fn parse_version(version: &str) -> Result<Version, String> {
    Version::parse(version).map_err(|error| format!("{version}: {error}"))
}

/// The floor under every version `af self` will activate or dispatch to.
fn require_self_managed(version: &str) -> Result<(), String> {
    let parsed = parse_version(version)?;
    let floor = Version::parse(OLDEST_SUPPORTED).expect("the floor is a version");
    if parsed < floor {
        return Err(format!(
            "af {version} is older than the oldest supported release ({OLDEST_SUPPORTED}); it can be neither the default nor a dispatch target"
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// the release key and the checksum file

/// Where this binary's release key comes from: `AF_RELEASE_KEY` (a minisign `.pub` file; a
/// developer knob, like `AF_RELEASE_SOURCE`), the key embedded at build time, or nothing.
fn release_key() -> Result<Option<(minisign_verify::PublicKey, &'static str)>, String> {
    if let Some(path) = std::env::var_os(KEY_ENV).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {KEY_ENV} {}: {error}", path.display()))?;
        let key = minisign_verify::PublicKey::decode(&text)
            .map_err(|error| format!("{KEY_ENV} {}: {error}", path.display()))?;
        return Ok(Some((key, "environment")));
    }
    if RELEASE_KEY.trim().is_empty() {
        return Ok(None);
    }
    let key = minisign_verify::PublicKey::from_base64(RELEASE_KEY.trim())
        .map_err(|error| format!("the embedded release key is malformed: {error}"))?;
    Ok(Some((key, "embedded")))
}

fn release_key_source() -> &'static str {
    match release_key() {
        Ok(Some((_, source))) => source,
        Ok(None) => "none",
        Err(_) => "malformed",
    }
}

/// The release's `SHA256SUMS`, verified as far as this build allows: its signature when a key is
/// available (a missing or bad signature is a refusal), the bare file in a build without one.
/// Returns the text and how it was verified.
fn verified_sums(
    src: &dyn ReleaseSource,
    tag: &str,
    dir: &Path,
) -> Result<(String, &'static str), String> {
    let sums_path = src
        .fetch(tag, "SHA256SUMS", dir)
        .map_err(|error| format!("release {tag} has no SHA256SUMS: {error}"))?;
    let sums = std::fs::read_to_string(&sums_path).map_err(|error| error.to_string())?;
    let Some((key, _)) = release_key()? else {
        return Ok((sums, "sha256sums"));
    };
    let signature_path = src.fetch(tag, "SHA256SUMS.minisig", dir).map_err(|error| {
        format!(
            "release {tag} has no SHA256SUMS.minisig ({error}); every release must be signed — refusing to install from checksums alone"
        )
    })?;
    let signature = std::fs::read_to_string(&signature_path).map_err(|error| error.to_string())?;
    let signature = minisign_verify::Signature::decode(&signature)
        .map_err(|error| format!("SHA256SUMS.minisig of {tag}: {error}"))?;
    key.verify(sums.as_bytes(), &signature, false)
        .map_err(|error| {
            format!("SHA256SUMS of {tag} does not verify against the release key: {error}")
        })?;
    Ok((sums, "minisign"))
}

fn find_sum(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset).then(|| hash.to_string())
    })
}

/// Every archive digest a release publishes, keyed by target: what a lock records.
fn parse_sums(sums: &str, version: &str) -> BTreeMap<String, String> {
    let prefix = format!("af-v{version}-");
    sums.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            let target = name.strip_prefix(&prefix)?.strip_suffix(".tar.gz")?;
            (!target.is_empty() && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (target.to_string(), format!("sha256:{hash}")))
        })
        .collect()
}

/// The per-target digests of a released version, from its verified `SHA256SUMS`, and how they
/// were verified. Needs the release source; `AF_SELF_OFFLINE` refuses.
pub(crate) fn release_digests(
    version: &str,
) -> Result<(BTreeMap<String, String>, &'static str), String> {
    if offline() {
        return Err(format!("{OFFLINE_ENV} is set"));
    }
    parse_version(version)?;
    let policy = config::load_machine()?.self_policy()?;
    let src = source(&policy);
    let tmp = tempfile::tempdir().map_err(|error| format!("temporary directory: {error}"))?;
    let (sums, verified_by) = verified_sums(src.as_ref(), &format!("v{version}"), tmp.path())?;
    let digests = parse_sums(&sums, version);
    if digests.is_empty() {
        return Err(format!("SHA256SUMS of v{version} lists no af archives"));
    }
    Ok((digests, verified_by))
}

// ------------------------------------------------------------------------------------------
// install

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    Ok(review_core::hex::encode(&Sha256::digest(bytes)))
}

/// Why an install did not happen. A mismatch is never a reason to run another version instead.
enum InstallError {
    /// The bytes fetched are not the bytes expected (lock digest or release checksums).
    Mismatch(String),
    Other(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Mismatch(text) | InstallError::Other(text) => f.write_str(text),
        }
    }
}

impl From<String> for InstallError {
    fn from(text: String) -> Self {
        InstallError::Other(text)
    }
}

/// The installed binary of `version`, when one is installed; under a lock its bytes must still be
/// the lock's. Asked under the versions lock.
fn already_installed(
    paths: &Paths,
    version: &str,
    expected: Option<&str>,
) -> Result<Option<PathBuf>, InstallError> {
    let Some(binary) = installed(paths, version) else {
        return Ok(None);
    };
    if let (Some(expected), Some(receipt)) = (expected, read_receipt(paths, version))
        && receipt.sha256 != strip_sha256(expected)
    {
        return Err(InstallError::Mismatch(format!(
            "installed af {version} (sha256 {}) is not the release the project lock recorded for {TARGET} ({expected})",
            receipt.sha256
        )));
    }
    Ok(Some(binary))
}

/// Download, verify, extract, prove the version, and record a receipt. Never touches the default
/// symlink. `lock_digest` is the digest a project lock records for this target: when present it
/// is the only thing the bytes must match; otherwise the release's checksums decide.
///
/// The versions lock is held only to look for an installed copy and to publish the verified one,
/// never across the release source: a command that records a seen pin, activates or sweeps waits
/// for no download (ADR-0153).
fn install(
    paths: &Paths,
    policy: &SelfPolicy,
    version: &str,
    lock_digest: Option<&str>,
) -> Result<PathBuf, InstallError> {
    if offline() {
        return Err(InstallError::Other(format!(
            "af {version} is not installed and {OFFLINE_ENV} is set — fix: unset it and run `af self install {version}`"
        )));
    }
    require_self_managed(version)?;
    let anchor = {
        let _guard = install_lock(paths)?;
        if let Some(binary) = already_installed(paths, version, lock_digest)? {
            return Ok(binary);
        }
        std::fs::File::open(&paths.versions)
            .map_err(|error| format!("opening {}: {error}", paths.versions.display()))?
    };
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
    let (expected_sha256, verified_by) = match lock_digest {
        Some(digest) => (strip_sha256(digest).to_string(), "lock"),
        None => {
            let (sums, verified_by) = verified_sums(src.as_ref(), &tag, tmp.path())?;
            (
                find_sum(&sums, &asset)
                    .ok_or_else(|| format!("SHA256SUMS of {tag} does not list {asset}"))?,
                verified_by,
            )
        }
    };
    if actual != expected_sha256 {
        return Err(InstallError::Mismatch(format!(
            "checksum mismatch for {asset} ({})\nexpected {expected_sha256}\nactual   {actual}",
            if verified_by == "lock" {
                "against the project lock's digest"
            } else {
                "against the release checksums"
            }
        )));
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
        return Err(InstallError::Other(format!(
            "tar could not extract {asset}"
        )));
    }
    let extracted = unpack.join("af");
    if !extracted.is_file() {
        return Err(InstallError::Other(format!(
            "{asset} does not contain an `af` binary at its root"
        )));
    }
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
        return Err(InstallError::Other(format!(
            "release binary reported `{reported}`, expected `af {version}`"
        )));
    }
    let receipt = Receipt {
        version: version.to_string(),
        target: TARGET.to_string(),
        source: src.describe(),
        asset,
        sha256: actual,
        verified_by: verified_by.to_string(),
        installed_at: iso_now(),
    };
    // Staged beside the versions, one directory per process, so publishing is one rename; a
    // staging directory that did not become the version is removed whatever happened, and one
    // a killed installer left is recovered by the next installer or the Storage Budget.
    let (staging, _held) = begin_staging(paths, version, &anchor)?;
    let published = stage(&staging, &extracted, &receipt)
        .map_err(InstallError::Other)
        .and_then(|()| publish(paths, version, lock_digest, &staging, &anchor));
    let _ = std::fs::remove_dir_all(&staging);
    published
}

/// The versions lock for an installer that began in the versions directory `anchor` holds open:
/// refused when that directory is gone or replaced, as `af self uninstall` leaves it, so an
/// installer that fetched meanwhile never recreates the tree an uninstall removed. Asked under
/// the lock the uninstall removes the tree under. While `anchor` is open, its directory's
/// identity is never reused.
fn anchored_lock(paths: &Paths, anchor: &std::fs::File) -> Result<std::fs::File, String> {
    let file = open_anchored_lock(paths)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).map_err(|error| {
        format!(
            "locking {}: {error}",
            paths.versions.join(".lock").display()
        )
    })?;
    still_anchored(paths, anchor)?;
    Ok(file)
}

/// [`anchored_lock`] without waiting: `None` while another process holds the versions lock, a
/// prune removing releases, an install publishing one or an uninstall tearing the versions down.
fn try_anchored_lock(
    paths: &Paths,
    anchor: &std::fs::File,
) -> Result<Option<std::fs::File>, String> {
    let file = open_anchored_lock(paths)?;
    if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
        return Ok(None);
    }
    still_anchored(paths, anchor)?;
    Ok(Some(file))
}

/// The versions lock's file, opened in the versions directory as it stands: never creating that
/// directory, refused when it is gone.
fn open_anchored_lock(paths: &Paths) -> Result<std::fs::File, String> {
    let path = paths.versions.join(".lock");
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => versions_gone(paths),
            _ => format!("opening {}: {error}", path.display()),
        })
}

/// Refused unless the versions directory is still the one `anchor` holds open.
fn still_anchored(paths: &Paths, anchor: &std::fs::File) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let began = anchor
        .metadata()
        .map_err(|error| format!("inspecting {}: {error}", paths.versions.display()))?;
    let same = std::fs::metadata(&paths.versions)
        .is_ok_and(|now| began.nlink() > 0 && now.dev() == began.dev() && now.ino() == began.ino());
    if same {
        Ok(())
    } else {
        Err(versions_gone(paths))
    }
}

fn versions_gone(paths: &Paths) -> String {
    format!(
        "{} was removed while the release was fetched (`af self uninstall`); nothing was installed",
        paths.versions.display()
    )
}

/// This process's staging directory for `version`, created and locked under the versions lock
/// after the abandoned ones are recovered, in the versions directory the install began in. The
/// returned handle holds it until the install ends.
fn begin_staging(
    paths: &Paths,
    version: &str,
    anchor: &std::fs::File,
) -> Result<(PathBuf, std::fs::File), String> {
    let _guard = anchored_lock(paths, anchor)?;
    recover_abandoned_staging(paths);
    let staging = paths
        .versions
        .join(format!(".{version}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging)
        .map_err(|error| format!("creating {}: {error}", staging.display()))?;
    let held = std::fs::File::open(&staging)
        .map_err(|error| format!("opening {}: {error}", staging.display()))?;
    // Waits only for an inventory that is asking whether the directory is in use.
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|error| format!("locking {}: {error}", staging.display()))?;
    Ok((staging, held))
}

/// The verified binary and its receipt in `staging`.
fn stage(staging: &Path, binary: &Path, receipt: &Receipt) -> Result<(), String> {
    std::fs::create_dir_all(staging)
        .map_err(|error| format!("creating {}: {error}", staging.display()))?;
    std::fs::copy(binary, staging.join("af")).map_err(|error| error.to_string())?;
    std::fs::write(
        staging.join("receipt.toml"),
        toml::to_string_pretty(receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

/// Make the staged version the installed one, under the versions lock: unless another installer
/// finished first while this one fetched, whose copy stands, still bound by the lock's digest;
/// and only into the versions directory `anchor` holds, the one the install began in.
fn publish(
    paths: &Paths,
    version: &str,
    expected: Option<&str>,
    staging: &Path,
    anchor: &std::fs::File,
) -> Result<PathBuf, InstallError> {
    let _guard = anchored_lock(paths, anchor)?;
    if let Some(binary) = already_installed(paths, version, expected)? {
        return Ok(binary);
    }
    let dir = version_dir(paths, version);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
    }
    std::fs::rename(staging, &dir)
        .map_err(|error| format!("installing into {}: {error}", dir.display()))?;
    Ok(dir.join("af"))
}

/// Retarget the default symlink atomically and record the activation.
fn set_default(paths: &Paths, version: &str) -> Result<(), String> {
    require_self_managed(version)?;
    // No sweep removes a version while it becomes the default.
    let _versions = install_lock(paths)?;
    set_default_locked(paths, version)
}

/// What a background activation must find unchanged before it replaces the default: the
/// default's version, how many activations the state records, and the link itself. Every
/// activation that changes the version records one more and replaces the link, so a change
/// away and back — an update, then a rollback — never reads as no change, even when the new
/// link reuses the old one's inode number. The link is `device:inode:ctime.nanoseconds`, kept
/// as text so a cached notice can carry it whatever the numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultMark {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    activations: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link: Option<String>,
}

/// The default's [`DefaultMark`]. Read under the versions lock by an activation that compares it;
/// read without it by the notice, which waits for no lock and for which a change in progress
/// reads as a change.
fn default_mark(paths: &Paths) -> DefaultMark {
    use std::os::unix::fs::MetadataExt;

    DefaultMark {
        version: default_version(paths),
        activations: read_state(paths).activations.len(),
        link: std::fs::symlink_metadata(&paths.bin).ok().map(|link| {
            format!(
                "{}:{}:{}.{:09}",
                link.dev(),
                link.ino(),
                link.ctime(),
                link.ctime_nsec()
            )
        }),
    }
}

/// Make `version` the default only while the default is still as `from` marked it, read under
/// the versions lock before the install: an `af self update` or `rollback` made meanwhile is the
/// operator's choice and stands, even one that ends at the version read. The comparison and the
/// retargeting are one critical section under the versions lock. Returns the mark the
/// activation left, `None` when `version` did not become the default.
fn set_default_unless_changed(
    paths: &Paths,
    version: &str,
    from: &DefaultMark,
) -> Result<Option<DefaultMark>, String> {
    require_self_managed(version)?;
    let _versions = install_lock(paths)?;
    if default_mark(paths) != *from {
        return Ok(None);
    }
    set_default_locked(paths, version)?;
    Ok(Some(default_mark(paths)))
}

/// [`set_default`] for a caller that holds the versions lock.
fn set_default_locked(paths: &Paths, version: &str) -> Result<(), String> {
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
        && !running_binary_is(&paths.bin)
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
    std::os::unix::fs::symlink(&binary, &tmp)
        .map_err(|error| format!("linking {}: {error}", tmp.display()))?;
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

/// The `af` pin a repository's lock records, and where the lock is.
pub(crate) struct Pinned {
    pub(crate) pin: AfPin,
    pub(crate) lock: PathBuf,
}

/// The pin a repository carries, read leniently so a lock written by a newer release still
/// dispatches to it.
pub(crate) fn pinned(repo: &Path) -> Option<Pinned> {
    let toplevel = config::git_toplevel(repo)?;
    let lock = toplevel.join(".af/af.lock");
    let text = std::fs::read_to_string(&lock).ok()?;
    review_config::lock::pinned_af(&text).map(|pin| Pinned { pin, lock })
}

fn strip_sha256(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
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

/// `af onboard --af V`: run the onboarding under release V (moving a pin forward means the new
/// release must write the lock), the same override as `AF_VERSION` for one command.
fn onboard_af_from_argv(argv: &[String]) -> Option<String> {
    if argv.get(1).map(String::as_str) != Some("onboard") {
        return None;
    }
    let mut iter = argv.iter().skip(2);
    while let Some(word) = iter.next() {
        if word == "--af" {
            return iter
                .next()
                .map(|value| value.trim_start_matches('v').to_string());
        } else if let Some(value) = word.strip_prefix("--af=") {
            return Some(value.trim_start_matches('v').to_string());
        }
    }
    None
}

/// `af` with no subcommand — bare, or with only `--repo DIR` — opens the browser, which is
/// machine-local like bootstrap: a project pin naming an older release must not exec it away.
fn browser_invocation(argv: &[String]) -> bool {
    let mut words = argv.iter().skip(1);
    while let Some(word) = words.next() {
        if word == "--repo" {
            if words.next().is_none() {
                return false;
            }
        } else if !word.starts_with("--repo=") {
            return false;
        }
    }
    true
}

fn exempt_from_dispatch(argv: &[String]) -> bool {
    if browser_invocation(argv) {
        return true;
    }
    let mut command_index = 1;
    while let Some(word) = argv.get(command_index) {
        if word == "--repo" && argv.get(command_index + 1).is_some() {
            command_index += 2;
        } else if word.starts_with("--repo=") {
            command_index += 1;
        } else {
            break;
        }
    }
    let first = argv.get(command_index).map(String::as_str);
    // Bootstrap is machine-local, not project authority. Dispatching it through an older project
    // pin would make the newly installed command disappear precisely where users need it.
    // `self optimize` is the exception: it reads project authority, so it dispatches (ADR-0105).
    let binary_management_self = first == Some("self")
        && argv.get(command_index + 1).map(String::as_str) != Some("optimize");
    matches!(first, None | Some("help" | "completions" | "config"))
        || binary_management_self
        || matches!(
            (first, argv.get(command_index + 1).map(String::as_str)),
            (
                Some("provider"),
                Some("setup" | "remove" | "recover" | "auth")
            )
        )
        || argv
            .iter()
            .skip(1)
            .any(|word| matches!(word.as_str(), "--version" | "-V" | "--help" | "-h"))
}

struct Request {
    version: String,
    /// The digest the lock records for this target, when a lock made the request.
    digest: Option<String>,
    lock: Option<PathBuf>,
}

/// Remember that a lock pinned a version on this machine, so `remove` and `prune` keep it.
fn record_seen_pin(paths: &Paths, lock: &Path, version: &str) {
    // No sweep removes a version while a pin of it is recorded. Recording stays best effort, but
    // never unlocked: a write without the lock could undo what a check or an activation records.
    let Ok(_versions) = install_lock(paths) else {
        return;
    };
    let _ = record_seen_pin_locked(paths, lock, version);
}

/// [`record_seen_pin`] for a caller that holds the versions lock. `Ok` only once the pin is in
/// the self state: already there, or written.
fn record_seen_pin_locked(paths: &Paths, lock: &Path, version: &str) -> Result<(), String> {
    let mut state = read_state(paths);
    let lock = std::fs::canonicalize(lock).unwrap_or_else(|_| lock.to_path_buf());
    if state
        .pins
        .iter()
        .any(|seen| seen.lock == lock && seen.version == version)
    {
        return Ok(());
    }
    state.pins.retain(|seen| seen.lock != lock);
    state.pins.push(SeenPin {
        lock,
        version: version.to_string(),
        seen: iso_now(),
    });
    let overflow = state.pins.len().saturating_sub(MAX_SEEN_PINS);
    state.pins.drain(..overflow);
    write_state(paths, &state)
}

/// Every version some project on this machine still pins: the lock in the current directory
/// and every lock dispatch has honoured that still exists and still pins. Forgotten entries are
/// dropped from the state on the way, under the versions lock the state's other writers hold,
/// taken only when there is one to drop.
fn pinned_everywhere(paths: &Paths) -> BTreeSet<String> {
    pins_still_held(paths, false)
}

/// [`pinned_everywhere`] for a caller that holds the versions lock: taking it again from this
/// process would wait on itself.
fn pinned_everywhere_locked(paths: &Paths) -> BTreeSet<String> {
    pins_still_held(paths, true)
}

fn pins_still_held(paths: &Paths, locked: bool) -> BTreeSet<String> {
    let mut versions = BTreeSet::new();
    if let Some(here) = pinned(Path::new(".")) {
        versions.insert(here.pin.version);
    }
    let mut state = read_state(paths);
    if !keep_live_pins(&mut state, &mut versions) {
        return versions;
    }
    // Dropping is a read-modify-write: read again under the lock, so what a check or a dispatch
    // wrote since the first read (`last_check`, a pin, an activation) is never undone.
    let _versions = if locked {
        None
    } else {
        match install_lock(paths) {
            Ok(guard) => Some(guard),
            Err(_) => return versions,
        }
    };
    let mut state = read_state(paths);
    if keep_live_pins(&mut state, &mut versions) {
        let _ = write_state(paths, &state);
    }
    versions
}

/// Keep the seen pins whose lock still exists and still pins, adding each version to
/// `versions`. Returns whether any was dropped.
fn keep_live_pins(state: &mut SelfState, versions: &mut BTreeSet<String>) -> bool {
    let before = state.pins.len();
    state.pins.retain_mut(|seen| {
        let Some(text) = std::fs::read_to_string(&seen.lock).ok() else {
            return false;
        };
        let Some(pin) = review_config::lock::pinned_af(&text) else {
            return false;
        };
        seen.version = pin.version.clone();
        versions.insert(pin.version);
        true
    });
    state.pins.len() != before
}

/// The variable a child of this `af` carries so that it runs as this binary, never dispatched
/// to another version: the browser's command-line hand-off (ADR-0123). A browser that was
/// itself dispatched passes on the version it was dispatched from.
pub(crate) fn undispatched_child() -> (&'static str, String) {
    let from = std::env::var(DISPATCHED_ENV)
        .ok()
        .filter(|from| !from.is_empty());
    (DISPATCHED_ENV, from.unwrap_or_else(|| VERSION.to_owned()))
}

/// Exec the version this project pins, when it is not the one running. Installs it on demand.
/// Returns only when the running binary should continue.
pub(crate) fn maybe_dispatch(argv: &[String]) {
    if exempt_from_dispatch(argv) || std::env::var_os(DISPATCHED_ENV).is_some() {
        return;
    }
    let explicit = std::env::var(VERSION_ENV)
        .ok()
        .map(|version| version.trim().trim_start_matches('v').to_string())
        .filter(|version| !version.is_empty())
        .or_else(|| onboard_af_from_argv(argv));
    let request = match explicit {
        Some(version) => Request {
            version,
            digest: None,
            lock: None,
        },
        None => match pinned(&repo_from_argv(argv)) {
            Some(Pinned { pin, lock }) => Request {
                digest: pin.digest_for(TARGET).map(str::to_string),
                version: pin.version,
                lock: Some(lock),
            },
            None => return,
        },
    };
    let Ok(paths) = paths() else {
        return;
    };
    if request.version == VERSION {
        // The pin names this very release: the running bytes must still be the lock's bytes.
        if let (Some(lock), Some(expected), Some(receipt)) = (
            &request.lock,
            request.digest.as_deref(),
            running_receipt(&paths),
        ) && receipt.sha256 != strip_sha256(expected)
        {
            eprintln!(
                "af: {} pins af {VERSION}, but this af (sha256 {}) is not the release the lock recorded for {TARGET} ({expected}) — fix: af self remove {VERSION} && af self install {VERSION}, or re-pin with `{VERSION_ENV}={VERSION} af onboard --refresh-lock`",
                lock.display(),
                receipt.sha256
            );
            std::process::exit(1);
        }
        if let Some(lock) = &request.lock {
            record_seen_pin(&paths, lock, &request.version);
        }
        return;
    }
    let why = match &request.lock {
        Some(lock) => format!("{} pins af {}", lock.display(), request.version),
        None => format!("af {} was requested", request.version),
    };
    // Below the floor nothing can run under a pin: an explicit request is refused; a lock's pin
    // is treated like any older release this binary cannot dispatch to — it runs instead, and
    // says so.
    if let Err(reason) = require_self_managed(&request.version) {
        if request.lock.is_none() {
            eprintln!("af: {why}: {reason}");
            std::process::exit(1);
        }
        eprintln!(
            "af: {why}: {reason}; this af {VERSION} runs instead — fix: af onboard --refresh-lock --af <version>"
        );
        return;
    }
    // The look and the install below take the versions lock only briefly, so under a project
    // lock what they found is bound to its pin in one critical section afterwards: a removal in
    // between is seen there, and the release is looked for once more.
    let mut looked_again = false;
    let binary = loop {
        let binary = match installed(&paths, &request.version) {
            Some(binary) => {
                // The lock binds bytes, so an installed copy must carry the digest it records.
                if let (Some(expected), Some(receipt)) = (
                    request.digest.as_deref(),
                    read_receipt(&paths, &request.version),
                ) && receipt.sha256 != strip_sha256(expected)
                {
                    eprintln!(
                        "af: {why}, but the installed af {} (sha256 {}) is not the release the lock recorded for {TARGET} ({expected}) — fix: af self remove {0} && af self install {0}, or re-pin with `af onboard --refresh-lock --af {0}`",
                        request.version, receipt.sha256
                    );
                    std::process::exit(1);
                }
                binary
            }
            None => {
                let policy = config::load_machine().and_then(|config| config.self_policy());
                let attempt = match policy {
                    Err(error) => Err(InstallError::Other(format!("self policy: {error}"))),
                    Ok(_) if offline() => Err(InstallError::Other(format!("{OFFLINE_ENV} is set"))),
                    Ok(policy) if !policy.install_pins => Err(InstallError::Other(
                        "[self] install_pins = false".to_string(),
                    )),
                    Ok(_) if request.lock.is_some() && request.digest.is_none() => {
                        Err(InstallError::Other(format!(
                            "the lock records no digest for {TARGET}, and a pin without bytes is never installed on demand"
                        )))
                    }
                    Ok(policy) => {
                        install(&paths, &policy, &request.version, request.digest.as_deref())
                    }
                };
                match attempt {
                    Ok(binary) => binary,
                    Err(InstallError::Mismatch(reason)) => {
                        eprintln!("af: {why}, and the release could not be installed: {reason}");
                        std::process::exit(1);
                    }
                    Err(InstallError::Other(reason)) => {
                        not_installed(&request, &why, &reason);
                        return;
                    }
                }
            }
        };
        let Some(lock) = &request.lock else {
            break binary;
        };
        match pin_installed(&paths, lock, &request.version, request.digest.as_deref()) {
            Ok(Some(binary)) => break binary,
            Ok(None) if !looked_again => looked_again = true,
            Ok(None) => {
                not_installed(&request, &why, "it was removed while this command started");
                return;
            }
            Err(InstallError::Mismatch(reason)) => {
                eprintln!(
                    "af: {why}, but the {reason} — fix: af self remove {0} && af self install {0}, or re-pin with `af onboard --refresh-lock --af {0}`",
                    request.version
                );
                std::process::exit(1);
            }
            Err(InstallError::Other(reason)) => {
                // Nothing was recorded, so nothing keeps the release from `af self remove` or
                // `af self prune` while it runs: it is never run unprotected.
                eprintln!(
                    "af: {why}, but the pin could not be recorded ({reason}), and without it `af self remove` and `af self prune` would not keep af {} — fix: make {} and {} writable, then run the command again",
                    request.version,
                    paths.versions.display(),
                    paths.state_file.display()
                );
                std::process::exit(1);
            }
        }
    };
    // The exec replaces this process, so no `after_command` follows it: the machine's own af
    // shows the cached notice to a person at a terminal and schedules the check for the pinned
    // release's command here, and the pin is not touched.
    if let Some((check_paths, policy)) = update_policy(argv) {
        if notice_wanted(argv, std::io::stderr().is_terminal()) {
            print_notice(&check_paths, &policy);
        }
        schedule_check(&check_paths, &policy);
    }
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

/// The installed binary of `version`, still carrying the lock's bytes, and the pin that keeps it
/// from `af self remove` and `af self prune`, recorded in the same critical section under the
/// versions lock their removals hold. `None` when it is no longer installed. A binary is
/// returned only once its pin is recorded: when the lock cannot be taken or the state cannot be
/// written, the error says why and nothing keeps the release.
fn pin_installed(
    paths: &Paths,
    lock: &Path,
    version: &str,
    digest: Option<&str>,
) -> Result<Option<PathBuf>, InstallError> {
    let _versions = install_lock(paths)?;
    let Some(binary) = already_installed(paths, version, digest)? else {
        return Ok(None);
    };
    record_seen_pin_locked(paths, lock, version)?;
    Ok(Some(binary))
}

/// The requested release is not installed and will not be, for `reason`. Fail closed whenever
/// running on would mean an older binary interpreting a newer release's pin, or ignoring an
/// explicit request. An older pin under a newer binary is the #47 case: return, so this binary
/// runs, and say so.
fn not_installed(request: &Request, why: &str, reason: &str) {
    let explicit = request.lock.is_none();
    let newer = match (Version::parse(&request.version), Version::parse(VERSION)) {
        (Ok(pinned), Ok(running)) => pinned > running,
        _ => true,
    };
    let fix = if request.lock.is_some() && request.digest.is_none() {
        format!(
            "af onboard --refresh-lock --af {0} (online, records digests), or af self install {0} to trust the release checksums",
            request.version
        )
    } else {
        format!("af self install {}", request.version)
    };
    if explicit || newer {
        eprintln!(
            "af: {why}, {} this af {VERSION}, and that release is not installed ({reason}) — fix: {fix}",
            if explicit { "not" } else { "newer than" }
        );
        std::process::exit(1);
    }
    eprintln!(
        "af: {why}; that release is not installed ({reason}), so this af {VERSION} runs instead — fix: {fix}"
    );
}

// ------------------------------------------------------------------------------------------
// update policy: the detached check and the notice

fn ci() -> bool {
    std::env::var_os("CI")
        .is_some_and(|value| !value.is_empty() && value != "0" && value != "false")
}

/// Set once this process has spawned the detached check, so a dispatch whose exec failed and
/// that runs its command after all does not spawn a second one.
static CHECK_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// The update policy for this invocation, when it takes part in the check at all: never for a
/// command exempt from dispatch (help, version, `self`, `config`, completions, the browser),
/// offline, under CI, with `update_check = false`, or with `auto_update = "never"`.
fn update_policy(argv: &[String]) -> Option<(Paths, SelfPolicy)> {
    if exempt_from_dispatch(argv) || offline() || ci() {
        return None;
    }
    let paths = paths().ok()?;
    let policy = config::load_machine()
        .and_then(|config| config.self_policy())
        .ok()?;
    checks_enabled(&policy).then_some((paths, policy))
}

fn checks_enabled(policy: &SelfPolicy) -> bool {
    policy.update_check && policy.auto_update != AutoUpdate::Never
}

/// Whether the cached check is due at `now`: checks are enabled and the cache is at least
/// `check_every` old. `check_every` throttles invocations; nothing runs between them.
fn check_due(policy: &SelfPolicy, cache: &CheckCache, now: u64) -> bool {
    checks_enabled(policy)
        && now.saturating_sub(cache.checked_at_epoch) >= policy.check_every.as_secs().max(1)
}

/// Whether the cached notice may be printed: only to a person at a terminal, and never by a
/// `--json` run, whose output a program reads.
fn notice_wanted(argv: &[String], stderr_terminal: bool) -> bool {
    stderr_terminal && !argv.iter().any(|word| word == "--json")
}

/// Spawn the detached check when it is due, at most once per process, terminal or not. The
/// child holds none of this process's pipes and needs no terminal: its stdin, stdout and stderr
/// are null, and its own process group keeps a terminal's signals away from it. Its failure is
/// never this command's.
fn schedule_check(paths: &Paths, policy: &SelfPolicy) {
    if !check_due(policy, &read_cache(paths), epoch_now())
        || CHECK_SCHEDULED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    // The check takes the lock this command creates: it never creates one itself, so one that
    // starts after an uninstall removed its files leaves them removed.
    if !matches!(
        create_check_lock_while_installed(paths, running_binary_present),
        Ok(Some(_))
    ) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command
        .args(["self", "refresh-check"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    if let Ok(child) = command.spawn() {
        log_scheduled_check(child.id());
    }
}

/// A debug build tells deterministic fixtures of every detached check it spawns, one line in
/// the file `AF_TEST_SELF_CHECK_LOG` names, so a test can tell "not scheduled" from "not yet
/// finished" without waiting. Production release binaries never read this setting.
fn log_scheduled_check(pid: u32) {
    #[cfg(debug_assertions)]
    {
        if let Some(path) = std::env::var_os("AF_TEST_SELF_CHECK_LOG")
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{pid}");
        }
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = pid;
    }
}

/// After a command's result is written: to a person at a terminal only, print the notice the
/// cache holds, then maybe spawn the detached check.
pub(crate) fn after_command(argv: &[String]) {
    // A dispatched af runs in place of the af that dispatched it, which scheduled the check
    // before its exec; a browser's command hand-off carries the same variable.
    if std::env::var_os(DISPATCHED_ENV).is_some() {
        return;
    }
    let Some((paths, policy)) = update_policy(argv) else {
        return;
    };
    // The notice comes first, so the check lock it may take is free again when the check this
    // command spawns asks for it.
    if notice_wanted(argv, std::io::stderr().is_terminal()) {
        print_notice(&paths, &policy);
    }
    schedule_check(&paths, &policy);
}

/// Print the notice the cache holds.
fn print_notice(paths: &Paths, policy: &SelfPolicy) {
    if let Some(notice) = take_notice(paths, policy) {
        eprintln!("{notice}");
    }
}

/// The notice the cache holds, read under the check lock, taken without waiting: while a check
/// holds it, that check may be replacing what the cache says, so neither notice is shown and the
/// cache stays as it is for a later command. An update notice is shown once: it is cleared under
/// the same lock, so a check's newer cache is never undone. It is shown only while the default
/// is still the one its activation left, by the [`DefaultMark`] it carries: an update or
/// rollback made since is the operator's, even one that ends at the same version, and the notice
/// it made stale is cleared unshown. The mark is read without the versions lock, never waiting
/// for it. The update notice is shown only once its clearing is written: a cache that could not
/// be replaced keeps it, unshown, for a later command. The lock is taken without creating its
/// file, as a check takes it: a command that finishes after `af self uninstall --purge` removed
/// it recreates nothing, and the purge holds it while it removes it.
fn take_notice(paths: &Paths, policy: &SelfPolicy) -> Option<String> {
    let Ok(Some(_check)) = try_lock_existing_check(paths) else {
        return None;
    };
    let mut cache = read_cache(paths);
    if let Some(updated) = cache.auto_updated.take() {
        if write_cache(paths, &cache).is_err() || default_mark(paths) != updated.default {
            return None;
        }
        Some(format!(
            "af self: updated the default from {} to {}; new shells run it",
            updated.from, updated.to
        ))
    } else if let Some(latest) = cache.latest.as_deref().and_then(|v| Version::parse(v).ok())
        && let Ok(running) = Version::parse(VERSION)
        && latest > running
        && policy.auto_update == AutoUpdate::Notify
    {
        Some(format!(
            "af {latest} is available (running {VERSION}) — af self update"
        ))
    } else {
        None
    }
}

/// The check lock without waiting: `None` while another detached check holds it.
fn try_lock_check(paths: &Paths) -> Result<Option<std::fs::File>, String> {
    let file = create_check_lock(paths)?;
    let locked =
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
    Ok(locked.then_some(file))
}

/// The check lock's file, created with its directory when missing.
fn create_check_lock(paths: &Paths) -> Result<std::fs::File, String> {
    let path = paths.cache_file.with_extension("lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("opening {}: {error}", path.display()))
}

/// The check lock's file for a check this command schedules, created only while the binary this
/// process runs is still installed (`present`). Asked under the versions lock, taken in the
/// versions directory as it stands and never creating it, which `af self uninstall` removes the
/// default and the versions under, a self-managed binary among them, before its purge: the file
/// is made before that teardown, and the purge removes it, or the binary is found gone and
/// nothing is made. The lock is taken without waiting: a finished command never waits for a
/// prune, an install or an uninstall holding it, and leaves this check to the next command.
/// Without a versions directory no self-managed binary is installed, so one still present is
/// another installer's, which no uninstall removes. `None` when the binary or its versions are
/// gone, or the versions lock is held.
fn create_check_lock_while_installed(
    paths: &Paths,
    present: impl Fn() -> bool,
) -> Result<Option<std::fs::File>, String> {
    let _versions = match std::fs::File::open(&paths.versions) {
        Ok(anchor) => match try_anchored_lock(paths, &anchor) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) | Err(_) => return Ok(None),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("opening {}: {error}", paths.versions.display())),
    };
    if !present() {
        return Ok(None);
    }
    create_check_lock(paths).map(Some)
}

/// The check lock for the detached check and for the notice, without waiting and without
/// creating anything: the command that schedules the check creates the lock's file first.
/// `None` while another holds it, and when the file is gone or was removed while this process
/// opened it, as `af self uninstall --purge` leaves it: neither recreates what a purge removed.
fn try_lock_existing_check(paths: &Paths) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::MetadataExt;

    let path = paths.cache_file.with_extension("lock");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("opening {}: {error}", path.display())),
    };
    if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
        return Ok(None);
    }
    let linked = file
        .metadata()
        .map_err(|error| format!("inspecting {}: {error}", path.display()))?
        .nlink()
        > 0;
    Ok(linked.then_some(file))
}

/// Whether the binary this process runs is still installed where it was started from.
/// `af self uninstall` removes a self-managed one, and the installation a check would serve
/// goes with it.
fn running_binary_present() -> bool {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .is_ok_and(|metadata| metadata.is_file())
}

/// The detached child: refresh the cache, and under `always` install and activate.
pub(crate) fn refresh_check() -> Result<(), String> {
    let paths = paths()?;
    let policy = config::load_machine()?.self_policy()?;
    if offline() {
        return Ok(());
    }
    // Overlapping commands may each spawn a check; while one runs, the others leave. The cache's
    // other writers, a terminal consuming the update notice and `af self update`, take the same
    // lock without waiting, and `af self uninstall` refuses while a check holds it.
    let Some(_check) = try_lock_existing_check(&paths)? else {
        return Ok(());
    };
    // A check scheduled before an uninstall and started after it finds its binary gone, and
    // leaves without writing: the cache, the state and the versions stay as the uninstall left
    // them. While this check holds the lock, no uninstall starts.
    if !running_binary_present() {
        return Ok(());
    }
    let mut cache = read_cache(&paths);
    // Read again under the lock: two commands that saw the same stale cache spawn two checks,
    // and the later one leaves when the earlier one has answered, a failed query included.
    let now = epoch_now();
    if !check_due(&policy, &cache, now) {
        return Ok(());
    }
    // A failed check is an attempt too, whatever failed: the query, the state's write or an
    // install. The cache records it still under the check lock, so an outage or a state that
    // cannot be written costs one query per `check_every`, and what the last good query found
    // stays in the cache.
    cache.checked_at_epoch = now;
    let checked = run_check(&paths, &policy, &mut cache);
    let written = write_cache(&paths, &cache);
    checked.and(written)
}

/// The query, and under `always` the install and the activation, of a check that is due,
/// recording in `cache` what it found and what it activated. An error leaves in `cache` what
/// was found before it: never an activation that did not happen.
fn run_check(paths: &Paths, policy: &SelfPolicy, cache: &mut CheckCache) -> Result<(), String> {
    let tags = source(policy).tags(policy.channel == Channel::Rc)?;
    let latest = newest(&tags, policy.channel);
    cache.channel = match policy.channel {
        Channel::Stable => "stable".into(),
        Channel::Rc => "rc".into(),
    };
    cache.latest = latest.as_ref().map(ToString::to_string);
    // The default this check may replace, marked in the same critical section, before an
    // install that may take long: an explicit change made meanwhile is never overwritten.
    let observed = {
        // The state's other writers (activations, seen pins) hold the versions lock, so this
        // one does too, and a check never drops what they recorded meanwhile.
        let _versions = install_lock(paths)?;
        let mut state = read_state(paths);
        state.last_check = Some(iso_now());
        write_state(paths, &state)?;
        default_mark(paths)
    };
    if policy.auto_update == AutoUpdate::Always
        && let Some(latest) = &latest
        && Version::parse(VERSION).is_ok_and(|running| *latest > running)
        && running_receipt(paths).is_some()
    {
        let latest = latest.to_string();
        if installed(paths, &latest).is_none() {
            install(paths, policy, &latest, None).map_err(|error| error.to_string())?;
        }
        // Only an activation that happened is announced, bound to the default it left; a failed
        // one is retried when the next check is due, not by every command until then.
        if let Some(default) = set_default_unless_changed(paths, &latest, &observed)? {
            cache.auto_updated = Some(AutoUpdated {
                from: observed.version.unwrap_or_else(|| VERSION.to_string()),
                to: latest,
                default,
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// commands

#[derive(Serialize)]
struct StatusView {
    version: &'static str,
    target: &'static str,
    commit: &'static str,
    release_key: &'static str,
    running: PathBuf,
    receipt: Option<Receipt>,
    default: Option<String>,
    default_status: &'static str,
    installed: Vec<String>,
    pin: Option<PinView>,
    pinned_projects: Vec<SeenPin>,
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
    /// The digest the lock records for this machine's target: the bytes the pin binds.
    digest: Option<String>,
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
    let pin = pinned(Path::new(".")).map(|pinned| PinView {
        installed: installed(&paths, &pinned.pin.version).is_some(),
        digest: pinned.pin.digest_for(TARGET).map(str::to_string),
        version: pinned.pin.version,
        lock: pinned.lock,
    });
    let state = read_state(&paths);
    let cache = read_cache(&paths);
    let default = default_version(&paths);
    let view = StatusView {
        version: VERSION,
        target: TARGET,
        commit: COMMIT,
        release_key: release_key_source(),
        running: running.clone(),
        receipt: running_receipt(&paths),
        default_status: default_status(&paths, default.as_deref()),
        default,
        installed: installed_list,
        pin,
        pinned_projects: state.pins,
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
    println!(
        "key:        {}",
        match view.release_key {
            "embedded" => "embedded release key (SHA256SUMS signatures verified)".to_string(),
            "environment" => format!("{KEY_ENV} (SHA256SUMS signatures verified)"),
            "none" => "none (this build verifies checksums only; not a release build)".to_string(),
            other => other.to_string(),
        }
    );
    match (&view.default, view.default_status) {
        (Some(version), _) => println!("default:    {} -> af {version}", paths.bin.display()),
        (None, "absent") => println!("default:    {} (absent)", paths.bin.display()),
        (None, "unmanaged-file") if running_binary_is(&paths.bin) => println!(
            "default:    {} (unmanaged running binary; `af self install {VERSION}` adopts it)",
            paths.bin.display()
        ),
        (None, status) => println!(
            "default:    {} ({status}; move it aside before `af self install {VERSION}`)",
            paths.bin.display()
        ),
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
            "pin here:   af {} ({}) · {} · {}",
            pin.version,
            pin.lock.display(),
            if pin.digest.is_some() {
                format!("bytes bound for {TARGET}")
            } else {
                format!(
                    "no digest for {TARGET} (af onboard --refresh-lock --af {} records one)",
                    pin.version
                )
            },
            if pin.installed {
                "installed".to_string()
            } else {
                format!("not installed — af self install {}", pin.version)
            }
        ),
        None => println!("pin here:   none"),
    }
    if !view.pinned_projects.is_empty() {
        println!(
            "pinned:     {} project(s) seen; their versions survive remove and prune",
            view.pinned_projects.len()
        );
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
        None => {
            let latest = newest(&src.tags(channel == Channel::Rc)?, channel)
                .ok_or_else(|| format!("no release found at {}", src.describe()))?;
            // Only a release the source actually listed is "latest". An explicit
            // `--version` is a request, not a discovery: recording it here made
            // `af self status` report a version that may not exist. The cache is written under
            // the check lock, taken without waiting: a detached check holding it writes its own.
            if let Ok(Some(_check)) = try_lock_check(&paths) {
                let mut cache = read_cache(&paths);
                record_check(&mut cache, &latest);
                let _ = write_cache(&paths, &cache);
            }
            latest
        }
    };
    let running = Version::parse(VERSION).map_err(|error| error.to_string())?;
    if check {
        if target > running {
            println!("af {target} is available (running {VERSION})");
            std::process::exit(10);
        }
        println!("af {VERSION} is up to date");
        return Ok(());
    }
    let target = target.to_string();
    require_self_managed(&target)?;
    require_receipt(&paths)?;
    if installed(&paths, &target).is_none() {
        install(&paths, &policy, &target, None)
            .map_err(|error| update_failed(&error.to_string(), &target))?;
    }
    set_default(&paths, &target)?;
    println!("af {target} is now the default ({})", paths.bin.display());
    Ok(())
}

/// Note a completed release check in the cache: when it ran and what it found.
fn record_check(cache: &mut CheckCache, latest: &Version) {
    cache.checked_at_epoch = epoch_now();
    cache.latest = Some(latest.to_string());
}

/// A failed install leaves the default untouched; say so, because the progress line that
/// preceded the error announced an installation.
fn update_failed(error: &str, target: &str) -> String {
    format!("{error}\naf {target} was not installed — af {VERSION} remains the default")
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
    parse_version(version)?;
    let binary = match installed(&paths, version) {
        Some(binary) => binary,
        None => install(&paths, &policy, version, None).map_err(|error| error.to_string())?,
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
    // Only an installed version names a directory to remove: a version string is never a path.
    parse_version(version)?;
    remove_version(&paths, version)?;
    println!("af {version} removed");
    Ok(())
}

/// Remove the installed `version` unless it is the default or a project pins it. The check and
/// the removal are one critical section under the versions lock, which every activation holds:
/// a version a background check makes the default meanwhile is never deleted from under it.
fn remove_version(paths: &Paths, version: &str) -> Result<(), String> {
    let _versions = install_lock(paths)?;
    if installed(paths, version).is_none() {
        return Err(format!("af {version} is not installed"));
    }
    if default_version(paths).as_deref() == Some(version) {
        return Err(format!(
            "af {version} is the default — fix: af self update or af self rollback first"
        ));
    }
    if pinned_everywhere_locked(paths).contains(version) {
        return Err(format!(
            "af {version} is pinned by a project this machine has seen (`af self status --json` lists them) — fix: re-pin those projects first"
        ));
    }
    let dir = version_dir(paths, version);
    std::fs::remove_dir_all(&dir).map_err(|error| format!("removing {}: {error}", dir.display()))
}

pub(crate) fn prune() -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    let policy = config::load_machine()?.self_policy()?;
    let keep = policy.keep_versions;
    if prune_versions(&paths, keep)? == 0 {
        println!("nothing to prune (keeping {keep}, plus the default and every pin seen)");
    }
    Ok(())
}

/// Remove the installed versions beyond the newest `keep`, never the default or a pinned one,
/// all read and removed under the versions lock as [`remove_version`] does. Returns how many
/// were removed.
fn prune_versions(paths: &Paths, keep: usize) -> Result<usize, String> {
    let _versions = install_lock(paths)?;
    let default = default_version(paths);
    let pinned = pinned_everywhere_locked(paths);
    let versions = installed_versions(paths);
    let mut removed = 0;
    let total = versions.len();
    for (index, version) in versions.iter().enumerate() {
        let version = version.to_string();
        let protected = Some(&version) == default.as_ref() || pinned.contains(&version);
        let within_keep = total - index <= keep;
        if protected || within_keep {
            continue;
        }
        std::fs::remove_dir_all(version_dir(paths, &version)).map_err(|error| error.to_string())?;
        println!("removed af {version}");
        removed += 1;
    }
    Ok(removed)
}

pub(crate) fn uninstall(purge: bool) -> Result<(), String> {
    let paths = paths()?;
    require_receipt(&paths)?;
    // A detached check may be fetching a release to install into the versions about to go: refuse
    // rather than wait for its release source. Held to the end, the lock keeps a check scheduled
    // meanwhile from starting, and one that starts after finds this binary gone.
    let Some(_check) = try_lock_check(&paths)? else {
        return Err(
            "a background update check is running now, and it may install a release — fix: run `af self uninstall` again in a minute"
                .to_string(),
        );
    };
    // The versions lock serializes the teardown with every publication: an installer that began
    // in these versions finds them gone and installs nothing. One that is staging now keeps its
    // directory. The lock is held only here, never across the purge below.
    let had_versions = std::fs::symlink_metadata(&paths.versions).is_ok();
    {
        let _versions = install_lock(&paths)?;
        if let Some(staging) = staging_dirs(&paths)
            .into_iter()
            .find(|dir| staging_in_use(dir))
        {
            return Err(format!(
                "an installer is staging into {} now — fix: run `af self uninstall` again when it has finished",
                staging.display()
            ));
        }
        if paths.bin.exists() || std::fs::symlink_metadata(&paths.bin).is_ok() {
            std::fs::remove_file(&paths.bin)
                .map_err(|error| format!("removing {}: {error}", paths.bin.display()))?;
            println!("removed {}", paths.bin.display());
        }
        if let Some(identity) = measured_directory(&paths.versions)? {
            remove_below_parent(&paths.versions, identity)?;
            if had_versions {
                println!("removed {}", paths.versions.display());
            }
        }
    }
    if purge {
        // What af left outside its own directories goes first, while the Provider and Store
        // registries it reads are still there (ADR-0144).
        for line in crate::storage::purge_outside_roots() {
            println!("{line}");
        }
        for dir in [
            config::config_home()?.join("af"),
            config::state_home()?.join("af"),
            config::cache_home()?.join("af"),
            config::data_home()?.join("af"),
        ] {
            if let Some(identity) = measured_directory(&dir)? {
                remove_below_parent(&dir, identity)?;
                println!("removed {}", dir.display());
            }
        }
    } else {
        println!("kept config, state, and cache (add --purge to remove them)");
    }
    Ok(())
}

/// The identity of the directory `dir` names when uninstall chooses it, without following a
/// final link; `None` when nothing is there.
fn measured_directory(dir: &Path) -> Result<Option<review_sandbox::Identity>, String> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspecting {}: {error}", dir.display())),
        Ok(metadata) if !metadata.is_dir() => Err(format!(
            "{} is not a directory af made (a symlink or a file); left in place",
            dir.display()
        )),
        Ok(_) => review_sandbox::Identity::of(dir)
            .map(Some)
            .map_err(|error| format!("inspecting {}: {error}", dir.display())),
    }
}

/// Remove `dir` and everything below it through descriptors opened from its parent as given,
/// never following a link below it, and only while it is still the directory `identity`
/// measured when uninstall chose it (ADR-0144).
fn remove_below_parent(dir: &Path, identity: review_sandbox::Identity) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("removing {}: it has no parent", dir.display()))?;
    review_sandbox::remove_beneath(parent, dir, Some(identity))
        .map_err(|error| format!("removing {}: {error}", dir.display()))
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
        let body = crate::topics::body(topic, text);
        let command = clap::Command::new(format!("af-{topic}"))
            .about(*about)
            .long_about(body);
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
    release_key: &'static str,
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
        release_key: release_key_source(),
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
    fn record_check_stores_the_discovered_release() {
        let mut cache = CheckCache::default();
        record_check(&mut cache, &Version::parse("0.9.0-rc.8").unwrap());
        assert_eq!(cache.latest.as_deref(), Some("0.9.0-rc.8"));
        assert!(cache.checked_at_epoch > 0);
    }

    #[test]
    fn update_failure_names_the_surviving_default() {
        let text = update_failed("release v9.9.9 does not exist", "9.9.9");
        assert!(text.starts_with("release v9.9.9 does not exist\n"));
        assert!(text.contains("af 9.9.9 was not installed"));
        assert!(text.contains(&format!("af {VERSION} remains the default")));
    }

    fn policy(update_check: bool, auto_update: AutoUpdate) -> SelfPolicy {
        SelfPolicy {
            update_check,
            check_every: std::time::Duration::from_secs(86_400),
            auto_update,
            channel: Channel::Stable,
            install_pins: true,
            keep_versions: 3,
            source: "owner/repo".to_string(),
        }
    }

    #[test]
    fn check_every_throttles_invocations_and_the_policy_can_turn_checks_off() {
        let now = 1_000_000;
        let fresh = CheckCache {
            checked_at_epoch: now - 60,
            ..CheckCache::default()
        };
        let stale = CheckCache {
            checked_at_epoch: now - 86_400,
            ..CheckCache::default()
        };
        let notify = policy(true, AutoUpdate::Notify);
        assert!(check_due(&notify, &CheckCache::default(), now));
        assert!(check_due(&notify, &stale, now));
        assert!(!check_due(&notify, &fresh, now));
        assert!(check_due(&policy(true, AutoUpdate::Always), &stale, now));
        assert!(!check_due(&policy(false, AutoUpdate::Notify), &stale, now));
        assert!(!check_due(&policy(true, AutoUpdate::Never), &stale, now));
    }

    fn staged(staging: &Path, version: &str, sha256: &str) {
        let receipt = Receipt {
            version: version.to_string(),
            target: TARGET.to_string(),
            source: "test".to_string(),
            asset: asset_name(version),
            sha256: sha256.to_string(),
            verified_by: "test".to_string(),
            installed_at: iso_now(),
        };
        let binary = staging.with_extension("bin");
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        stage(staging, &binary, &receipt).unwrap();
    }

    #[test]
    fn publishing_keeps_a_copy_another_installer_finished_first_and_its_lock_digest() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            versions: root.path().join("versions"),
            bin: root.path().join("bin/af"),
            state_file: root.path().join("state/self.toml"),
            cache_file: root.path().join("cache/latest.toml"),
        };
        std::fs::create_dir_all(&paths.versions).unwrap();
        let anchor = std::fs::File::open(&paths.versions).unwrap();
        let first = paths.versions.join(".0.9.0.1");
        staged(&first, "0.9.0", "aa");
        let Ok(binary) = publish(&paths, "0.9.0", Some("sha256:aa"), &first, &anchor) else {
            panic!("the first installer publishes");
        };
        assert_eq!(binary, version_binary(&paths, "0.9.0"));
        assert!(
            !first.exists(),
            "publishing is the staging directory's rename"
        );

        // A second installer that fetched meanwhile keeps the published copy, not its own.
        let second = paths.versions.join(".0.9.0.2");
        staged(&second, "0.9.0", "bb");
        let Ok(kept) = publish(&paths, "0.9.0", None, &second, &anchor) else {
            panic!("the second installer keeps the published copy");
        };
        assert_eq!(kept, binary);
        assert_eq!(read_receipt(&paths, "0.9.0").unwrap().sha256, "aa");
        // Under a project lock the published bytes must still be the lock's.
        assert!(matches!(
            publish(&paths, "0.9.0", Some("sha256:bb"), &second, &anchor),
            Err(InstallError::Mismatch(_))
        ));
        assert!(matches!(
            already_installed(&paths, "0.9.0", Some("sha256:aa")),
            Ok(Some(_))
        ));
        assert!(matches!(already_installed(&paths, "0.9.1", None), Ok(None)));
    }

    fn private_paths(root: &Path) -> Paths {
        Paths {
            versions: root.join("versions"),
            bin: root.join("bin/af"),
            state_file: root.join("state/self.toml"),
            cache_file: root.join("cache/latest.toml"),
        }
    }

    /// A pid no process can have: past every system's process id limit.
    const NO_PROCESS: i32 = i32::MAX;

    #[test]
    fn only_an_abandoned_staging_directory_is_recovered() {
        assert!(staging_name(".0.9.0.123"));
        assert!(staging_name(".0.9.0-rc.1.123"));
        assert!(!staging_name(".lock"));
        assert!(!staging_name("0.9.0"));
        assert!(!staging_name(".0.9.0."));
        assert!(!staging_name(".nope.123"));

        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        let dir = |name: String| {
            let dir = paths.versions.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("af"), "partial").unwrap();
            dir
        };
        // Killed after it began staging: nothing holds the directory and its process is gone.
        let abandoned = dir(format!(".0.9.0.{NO_PROCESS}"));
        // An `af` installer holds its directory locked.
        let held = dir(format!(".0.9.1.{NO_PROCESS}"));
        let lock = std::fs::File::open(&held).unwrap();
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
        // `install.sh` holds no lock, but its process is alive.
        let live = dir(format!(".0.9.2.{}", std::process::id()));
        // A link is never a staging directory, nor is what it leads to removed.
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(
            &elsewhere,
            paths.versions.join(format!(".0.9.3.{NO_PROCESS}")),
        )
        .unwrap();

        assert_eq!(
            staging_dirs(&paths),
            vec![abandoned.clone(), held.clone(), live.clone()]
        );
        let inventory = installed_for_storage(&paths).unwrap();
        let protected = |dir: &Path| {
            inventory
                .iter()
                .find(|version| version.directory == dir)
                .map(|version| version.protected)
        };
        assert_eq!(protected(&abandoned), Some(None), "counted and evictable");
        assert_eq!(protected(&held), Some(Some(STAGING_IN_USE)));
        assert_eq!(protected(&live), Some(Some(STAGING_IN_USE)));

        recover_abandoned_staging(&paths);
        assert!(!abandoned.exists());
        assert!(held.join("af").is_file());
        assert!(live.join("af").is_file());
        assert!(elsewhere.is_dir());
        drop(lock);
        assert_eq!(
            protected_now(&paths, &format!(".0.9.1.{NO_PROCESS}")),
            None,
            "released by its installer"
        );
        assert_eq!(
            protected_now(&paths, &format!(".0.9.2.{}", std::process::id())),
            Some(STAGING_IN_USE)
        );
    }

    #[test]
    fn an_activation_never_overwrites_a_default_changed_since_it_was_read() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        std::fs::create_dir_all(&paths.versions).unwrap();
        let anchor = std::fs::File::open(&paths.versions).unwrap();
        for (version, sha256) in [("0.9.0", "aa"), ("0.9.1", "bb"), ("0.9.2", "cc")] {
            let staging = paths.versions.join(format!(".{version}.1"));
            staged(&staging, version, sha256);
            assert!(publish(&paths, version, None, &staging, &anchor).is_ok());
        }
        // Read before the install: no default. An explicit update made 0.9.0 the default since.
        let none = default_mark(&paths);
        set_default(&paths, "0.9.0").unwrap();
        assert_eq!(set_default_unless_changed(&paths, "0.9.2", &none), Ok(None));
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.0"));
        assert_eq!(read_state(&paths).activations.len(), 1);

        // Read at 0.9.0; since, an update to 0.9.1 and a rollback to 0.9.0. The version is the
        // one read, but the operator chose it again: kept.
        let read = default_mark(&paths);
        set_default(&paths, "0.9.1").unwrap();
        set_default(&paths, "0.9.0").unwrap();
        assert_eq!(default_mark(&paths).version, read.version);
        assert_eq!(set_default_unless_changed(&paths, "0.9.2", &read), Ok(None));
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.0"));
        assert_eq!(read_state(&paths).activations.len(), 3);

        // Unchanged since it was read, what other writers record meanwhile included: activated.
        let read = default_mark(&paths);
        let mut state = read_state(&paths);
        state.last_check = Some(iso_now());
        write_state(&paths, &state).unwrap();
        let left = set_default_unless_changed(&paths, "0.9.2", &read).unwrap();
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.2"));
        assert_eq!(
            left,
            Some(default_mark(&paths)),
            "the mark the activation left"
        );
    }

    /// A private layout with `versions` installed, published as an installer publishes them.
    fn installed_releases(root: &Path, versions: &[&str]) -> Paths {
        let paths = private_paths(root);
        std::fs::create_dir_all(&paths.versions).unwrap();
        let anchor = std::fs::File::open(&paths.versions).unwrap();
        for version in versions {
            let staging = paths.versions.join(format!(".{version}.1"));
            staged(&staging, version, "aa");
            assert!(publish(&paths, version, None, &staging, &anchor).is_ok());
        }
        paths
    }

    #[test]
    fn a_removal_never_deletes_a_version_activated_or_pinned_while_it_waited() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0", "0.9.1", "0.9.2"]);
        set_default(&paths, "0.9.0").unwrap();
        // Read before: neither 0.9.1 nor 0.9.2 is protected.
        assert_eq!(protected_now(&paths, "0.9.1"), None);
        assert_eq!(protected_now(&paths, "0.9.2"), None);
        let remove_later = |version: &'static str| {
            let root = root.path().to_path_buf();
            std::thread::spawn(move || remove_version(&private_paths(&root), version))
        };

        // A background check holds the versions lock and activates 0.9.2 while its removal
        // waits: the removal reads the default again under the lock and keeps it.
        let activation = install_lock(&paths).unwrap();
        let removal = remove_later("0.9.2");
        set_default_locked(&paths, "0.9.2").unwrap();
        drop(activation);
        let removed = removal.join().unwrap();
        assert!(
            removed
                .as_ref()
                .is_err_and(|error| error.contains("is the default")),
            "{removed:?}"
        );
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.2"));
        assert!(version_binary(&paths, "0.9.2").is_file());

        // A dispatch records a pin of 0.9.1 under the lock while its removal waits: kept.
        let lock = root.path().join("repo/.af/af.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "version = 1\n\n[af]\nversion = \"0.9.1\"\n").unwrap();
        let dispatch = install_lock(&paths).unwrap();
        let removal = remove_later("0.9.1");
        let mut state = read_state(&paths);
        state.pins.push(SeenPin {
            lock: lock.clone(),
            version: "0.9.1".to_string(),
            seen: iso_now(),
        });
        write_state(&paths, &state).unwrap();
        drop(dispatch);
        let removed = removal.join().unwrap();
        assert!(
            removed
                .as_ref()
                .is_err_and(|error| error.contains("pinned by a project")),
            "{removed:?}"
        );
        assert!(version_binary(&paths, "0.9.1").is_file());

        // Neither the default nor pinned any longer: removed.
        set_default(&paths, "0.9.0").unwrap();
        remove_version(&paths, "0.9.2").unwrap();
        assert!(!version_dir(&paths, "0.9.2").exists());
        std::fs::remove_file(&lock).unwrap();
        remove_version(&paths, "0.9.1").unwrap();
        assert!(!version_dir(&paths, "0.9.1").exists());
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.0"));
    }

    #[test]
    fn a_prune_keeps_a_version_activated_while_it_waited() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0", "0.9.1", "0.9.2"]);
        set_default(&paths, "0.9.0").unwrap();

        // Keeping none, the prune would remove 0.9.1 and 0.9.2; a background check activates
        // 0.9.2 under the versions lock while the prune waits for it.
        let activation = install_lock(&paths).unwrap();
        let pruning = {
            let root = root.path().to_path_buf();
            std::thread::spawn(move || prune_versions(&private_paths(&root), 0))
        };
        set_default_locked(&paths, "0.9.2").unwrap();
        drop(activation);
        assert_eq!(pruning.join().unwrap(), Ok(2));

        assert_eq!(default_version(&paths).as_deref(), Some("0.9.2"));
        assert!(version_binary(&paths, "0.9.2").is_file());
        assert!(
            !version_dir(&paths, "0.9.0").exists(),
            "no longer the default"
        );
        assert!(!version_dir(&paths, "0.9.1").exists());
    }

    #[test]
    fn an_update_notice_is_shown_only_while_the_activation_it_names_stands() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0", "0.9.2"]);
        let always = policy(true, AutoUpdate::Always);
        // What a background check caches once it activated 0.9.2 over 0.9.0.
        let activated = |paths: &Paths| {
            set_default(paths, "0.9.0").unwrap();
            let from = default_mark(paths);
            let default = set_default_unless_changed(paths, "0.9.2", &from)
                .unwrap()
                .unwrap();
            let cache = CheckCache {
                checked_at_epoch: 1_000_000,
                latest: Some("0.9.2".to_string()),
                channel: "stable".to_string(),
                auto_updated: Some(AutoUpdated {
                    from: "0.9.0".to_string(),
                    to: "0.9.2".to_string(),
                    default,
                }),
                ..CheckCache::default()
            };
            write_cache(paths, &cache).unwrap();
            // Beside the lock's file, which the command that scheduled the check created.
            drop(create_check_lock(paths).unwrap());
        };
        let assert_cleared = |paths: &Paths| {
            let cache = read_cache(paths);
            assert_eq!(cache.auto_updated, None);
            assert_eq!(cache.latest.as_deref(), Some("0.9.2"));
            assert_eq!(cache.checked_at_epoch, 1_000_000);
        };

        // Shown once, while the default is the one the activation left.
        activated(&paths);
        assert_eq!(
            take_notice(&paths, &always).as_deref(),
            Some("af self: updated the default from 0.9.0 to 0.9.2; new shells run it")
        );
        assert_cleared(&paths);
        assert_eq!(take_notice(&paths, &always), None);

        // Rolled back to 0.9.0 before the next command: cleared unshown.
        activated(&paths);
        set_default(&paths, "0.9.0").unwrap();
        // While a check holds the check lock, even a stale notice is left to it.
        let check = try_lock_check(&paths).unwrap().unwrap();
        assert_eq!(take_notice(&paths, &always), None);
        assert!(read_cache(&paths).auto_updated.is_some());
        drop(check);
        assert_eq!(take_notice(&paths, &always), None);
        assert_cleared(&paths);

        // Rolled back to 0.9.0 and updated to 0.9.2 again by the operator: the default is the
        // version the notice names, but not the activation that made it. Cleared unshown.
        activated(&paths);
        set_default(&paths, "0.9.0").unwrap();
        set_default(&paths, "0.9.2").unwrap();
        assert_eq!(default_version(&paths).as_deref(), Some("0.9.2"));
        assert_eq!(take_notice(&paths, &always), None);
        assert_cleared(&paths);
    }

    #[test]
    fn the_check_cache_reads_only_the_shape_this_release_writes() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        let cache = CheckCache {
            checked_at_epoch: 1_000_000,
            latest: Some("0.9.2".to_string()),
            channel: "stable".to_string(),
            auto_updated: Some(AutoUpdated {
                from: "0.9.0".to_string(),
                to: "0.9.2".to_string(),
                default: DefaultMark {
                    version: Some("0.9.2".to_string()),
                    activations: 2,
                    link: Some("1:2:3.000000004".to_string()),
                },
            }),
            ..CheckCache::default()
        };
        write_cache(&paths, &cache).unwrap();
        let text = std::fs::read_to_string(&paths.cache_file).unwrap();
        assert!(
            text.starts_with(&format!("schema = \"{CHECK_CACHE_SCHEMA}\"\n")),
            "{text}"
        );
        let read = read_cache(&paths);
        assert_eq!(read.checked_at_epoch, 1_000_000);
        assert_eq!(read.latest.as_deref(), Some("0.9.2"));
        assert_eq!(read.channel, "stable");
        assert_eq!(read.auto_updated, cache.auto_updated);

        // The unversioned shape earlier releases wrote, another schema, and an unknown field
        // are each read as no cache: the next command is due a check.
        for refused in [
            "checked_at_epoch = 1000000\nlatest = \"0.9.2\"\nchannel = \"stable\"\nauto_updated_to = \"0.9.2\"\nauto_updated_from = \"0.9.0\"\n".to_string(),
            text.replace(CHECK_CACHE_SCHEMA, "af/self-check@1"),
            format!("{text}surprise = 1\n"),
        ] {
            std::fs::write(&paths.cache_file, &refused).unwrap();
            let read = read_cache(&paths);
            assert_eq!(read.checked_at_epoch, 0, "{refused}");
            assert_eq!(read.latest, None, "{refused}");
            assert_eq!(read.auto_updated, None, "{refused}");
            assert_eq!(read.schema, CHECK_CACHE_SCHEMA);
        }
    }

    #[test]
    fn an_install_never_publishes_into_versions_removed_while_it_fetched() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0"]);
        // An installer began in these versions, then fetched while an uninstall removed them.
        let anchor = std::fs::File::open(&paths.versions).unwrap();
        std::fs::remove_dir_all(&paths.versions).unwrap();
        let staging = root.path().join("fetched");
        staged(&staging, "0.9.1", "bb");
        let gone = |result: Result<PathBuf, InstallError>| matches!(result, Err(InstallError::Other(text)) if text.contains("was removed while the release was fetched"));
        assert!(gone(publish(&paths, "0.9.1", None, &staging, &anchor)));
        assert!(begin_staging(&paths, "0.9.1", &anchor).is_err());
        assert!(!paths.versions.exists(), "nothing recreated the versions");

        // Versions made again since, by another installation, are not the ones it began in.
        std::fs::create_dir_all(&paths.versions).unwrap();
        assert!(gone(publish(&paths, "0.9.1", None, &staging, &anchor)));
        assert!(begin_staging(&paths, "0.9.1", &anchor).is_err());
        assert!(!version_dir(&paths, "0.9.1").exists());
        assert!(staging_dirs(&paths).is_empty());
        assert!(
            staging.join("af").is_file(),
            "the staged bytes stay the installer's"
        );
    }

    #[test]
    fn a_dispatch_pins_only_the_release_still_installed_under_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0", "0.9.1"]);
        set_default(&paths, "0.9.0").unwrap();
        let lock = root.path().join("repo/.af/af.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "version = 1\n\n[af]\nversion = \"0.9.1\"\n").unwrap();

        // A dispatch saw 0.9.1 installed without the lock; a removal took the lock first and
        // deleted it before the pin was recorded. The dispatch's recheck finds it gone and
        // records nothing.
        assert!(installed(&paths, "0.9.1").is_some());
        remove_version(&paths, "0.9.1").unwrap();
        assert!(matches!(
            pin_installed(&paths, &lock, "0.9.1", Some("sha256:aa")),
            Ok(None)
        ));
        assert!(read_state(&paths).pins.is_empty());

        // Installed again, it is pinned in the same critical section that finds it, and a
        // removal that comes after keeps it.
        let staging = paths.versions.join(".0.9.1.1");
        staged(&staging, "0.9.1", "aa");
        let anchor = std::fs::File::open(&paths.versions).unwrap();
        assert!(publish(&paths, "0.9.1", None, &staging, &anchor).is_ok());
        let Ok(Some(binary)) = pin_installed(&paths, &lock, "0.9.1", Some("sha256:aa")) else {
            panic!("the installed release is pinned");
        };
        assert_eq!(binary, version_binary(&paths, "0.9.1"));
        let removed = remove_version(&paths, "0.9.1");
        assert!(
            removed
                .as_ref()
                .is_err_and(|error| error.contains("pinned by a project")),
            "{removed:?}"
        );
        assert!(binary.is_file());

        // Bytes that are not the lock's are refused under the lock as well, and not pinned.
        let other = root.path().join("repo2/.af/af.lock");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "version = 1\n\n[af]\nversion = \"0.9.0\"\n").unwrap();
        assert!(matches!(
            pin_installed(&paths, &other, "0.9.0", Some("sha256:bb")),
            Err(InstallError::Mismatch(_))
        ));
        assert!(
            !read_state(&paths)
                .pins
                .iter()
                .any(|seen| seen.version == "0.9.0")
        );
    }

    #[test]
    fn a_scheduled_check_never_creates_its_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        // Purged, or never scheduled: nothing to lock, and nothing is created.
        assert!(matches!(try_lock_existing_check(&paths), Ok(None)));
        assert!(!paths.cache_file.parent().unwrap().exists());

        // Scheduled: the command created the lock, which one check holds at a time.
        drop(create_check_lock(&paths).unwrap());
        let held = try_lock_existing_check(&paths).unwrap().unwrap();
        assert!(matches!(try_lock_existing_check(&paths), Ok(None)));
        assert!(matches!(try_lock_check(&paths), Ok(None)));
        drop(held);

        // Removed by a purge: a check that starts after never makes it again.
        let lock = paths.cache_file.with_extension("lock");
        std::fs::remove_dir_all(lock.parent().unwrap()).unwrap();
        assert!(matches!(try_lock_existing_check(&paths), Ok(None)));
        assert!(!lock.parent().unwrap().exists());
    }

    #[test]
    fn a_command_that_finishes_after_a_purge_recreates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.1"]);
        let notify = policy(true, AutoUpdate::Notify);
        let notice = format!("af 999.0.0 is available (running {VERSION}) — af self update");
        let cache = CheckCache {
            checked_at_epoch: 1_000_000,
            latest: Some("999.0.0".to_string()),
            channel: "stable".to_string(),
            ..CheckCache::default()
        };
        write_cache(&paths, &cache).unwrap();
        let lock = paths.cache_file.with_extension("lock");
        let cache_dir = paths.cache_file.parent().unwrap().to_path_buf();

        // Without its lock's file the notice is not read, and the file is not made for it; a
        // binary that is gone schedules nothing, its versions standing or not.
        assert_eq!(take_notice(&paths, &notify), None);
        assert!(matches!(
            create_check_lock_while_installed(&paths, || false),
            Ok(None)
        ));
        assert!(!lock.exists());

        // Installed: the command makes the lock's file under the versions lock, and the notice
        // is read under it.
        assert!(matches!(
            create_check_lock_while_installed(&paths, || true),
            Ok(Some(_))
        ));
        assert!(lock.is_file());
        assert_eq!(
            take_notice(&paths, &notify).as_deref(),
            Some(notice.as_str())
        );

        // An uninstall holds the check lock to the end: removes the versions, with the binary
        // they hold, then purges the cache. A command that finishes meanwhile or afterwards reads
        // no notice and makes no lock, cache or versions again.
        let uninstall = try_lock_check(&paths).unwrap().unwrap();
        assert_eq!(take_notice(&paths, &notify), None);
        std::fs::remove_dir_all(&paths.versions).unwrap();
        std::fs::remove_dir_all(&cache_dir).unwrap();
        assert_eq!(take_notice(&paths, &notify), None);
        drop(uninstall);
        assert_eq!(take_notice(&paths, &notify), None);
        assert!(matches!(
            create_check_lock_while_installed(&paths, || false),
            Ok(None)
        ));
        assert!(!cache_dir.exists(), "the purged cache was recreated");
        assert!(
            !paths.versions.exists(),
            "the removed versions were recreated"
        );

        // A binary another installer put there, which no uninstall removes, still schedules,
        // and makes no versions directory for it.
        assert!(matches!(
            create_check_lock_while_installed(&paths, || true),
            Ok(Some(_))
        ));
        assert!(lock.is_file());
        assert!(!paths.versions.exists());
    }

    #[test]
    fn scheduling_a_check_never_waits_for_the_versions_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.1"]);
        let lock = paths.cache_file.with_extension("lock");

        // A prune, an install or an uninstall holds the versions lock: the attempt is skipped
        // at once, and nothing is made for it.
        let prune = install_lock(&paths).unwrap();
        assert!(matches!(
            create_check_lock_while_installed(&paths, || true),
            Ok(None)
        ));
        assert!(!lock.exists());

        // Free again: a later command makes the lock's file.
        drop(prune);
        assert!(matches!(
            create_check_lock_while_installed(&paths, || true),
            Ok(Some(_))
        ));
        assert!(lock.is_file());
    }

    #[test]
    fn an_update_notice_whose_clearing_failed_is_kept_for_a_later_command() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.0", "0.9.2"]);
        let always = policy(true, AutoUpdate::Always);
        set_default(&paths, "0.9.0").unwrap();
        let from = default_mark(&paths);
        let default = set_default_unless_changed(&paths, "0.9.2", &from)
            .unwrap()
            .unwrap();
        let cache = CheckCache {
            checked_at_epoch: 1_000_000,
            latest: Some("0.9.2".to_string()),
            channel: "stable".to_string(),
            auto_updated: Some(AutoUpdated {
                from: "0.9.0".to_string(),
                to: "0.9.2".to_string(),
                default,
            }),
            ..CheckCache::default()
        };
        write_cache(&paths, &cache).unwrap();
        drop(create_check_lock(&paths).unwrap());
        let written = std::fs::read(&paths.cache_file).unwrap();

        // The cache stays readable, but its replacement cannot be written: the temporary's path
        // is a directory. Nothing is shown, however often, and the notice stays.
        let tmp = paths
            .cache_file
            .with_extension(format!("tmp.{}", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        assert_eq!(take_notice(&paths, &always), None);
        assert_eq!(take_notice(&paths, &always), None);
        assert_eq!(std::fs::read(&paths.cache_file).unwrap(), written);

        // Writable again: shown exactly once, and cleared.
        std::fs::remove_dir(&tmp).unwrap();
        assert_eq!(
            take_notice(&paths, &always).as_deref(),
            Some("af self: updated the default from 0.9.0 to 0.9.2; new shells run it")
        );
        assert_eq!(take_notice(&paths, &always), None);
        assert_eq!(read_cache(&paths).auto_updated, None);
        assert_eq!(read_cache(&paths).latest.as_deref(), Some("0.9.2"));
    }

    #[test]
    fn a_dispatch_gets_its_binary_only_once_the_pin_that_keeps_it_is_recorded() {
        let root = tempfile::tempdir().unwrap();
        let paths = installed_releases(root.path(), &["0.9.1"]);
        let lock = root.path().join("repo/.af/af.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "version = 1\n\n[af]\nversion = \"0.9.1\"\n").unwrap();

        // The state cannot be written: its path is a directory. No binary, and no pin.
        std::fs::create_dir_all(&paths.state_file).unwrap();
        assert!(matches!(
            pin_installed(&paths, &lock, "0.9.1", Some("sha256:aa")),
            Err(InstallError::Other(_))
        ));
        std::fs::remove_dir(&paths.state_file).unwrap();

        // The versions lock cannot be taken: its path is a directory. No binary, and no state.
        let versions_lock = paths.versions.join(".lock");
        std::fs::remove_file(&versions_lock).unwrap();
        std::fs::create_dir(&versions_lock).unwrap();
        assert!(matches!(
            pin_installed(&paths, &lock, "0.9.1", Some("sha256:aa")),
            Err(InstallError::Other(_))
        ));
        assert!(!paths.state_file.exists());
        std::fs::remove_dir(&versions_lock).unwrap();

        // Both writable: recorded, then returned, and a removal keeps it.
        let Ok(Some(binary)) = pin_installed(&paths, &lock, "0.9.1", Some("sha256:aa")) else {
            panic!("the installed release is pinned");
        };
        assert_eq!(binary, version_binary(&paths, "0.9.1"));
        assert_eq!(read_state(&paths).pins.len(), 1);
        assert!(remove_version(&paths, "0.9.1").is_err());
        assert!(binary.is_file());
    }

    #[test]
    fn a_pin_is_never_recorded_without_the_versions_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        let lock = root.path().join("repo/.af/af.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "version = 1\n\n[af]\nversion = \"0.9.7\"\n").unwrap();
        let state = SelfState {
            last_check: Some("2026-10-19T00:00:00Z".to_string()),
            ..SelfState::default()
        };
        write_state(&paths, &state).unwrap();
        let before = std::fs::read(&paths.state_file).unwrap();

        // The versions lock cannot be taken: its path is a directory, which no one opens to write.
        let versions_lock = paths.versions.join(".lock");
        std::fs::create_dir_all(&versions_lock).unwrap();
        assert!(install_lock(&paths).is_err());
        record_seen_pin(&paths, &lock, "0.9.7");
        assert_eq!(std::fs::read(&paths.state_file).unwrap(), before);

        // With the lock, the pin is recorded beside what was there.
        std::fs::remove_dir(&versions_lock).unwrap();
        record_seen_pin(&paths, &lock, "0.9.7");
        let state = read_state(&paths);
        assert_eq!(state.last_check.as_deref(), Some("2026-10-19T00:00:00Z"));
        assert_eq!(state.pins.len(), 1);
        assert_eq!(state.pins[0].version, "0.9.7");
    }

    #[test]
    fn dropping_a_forgotten_pin_keeps_what_another_writer_recorded_under_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(root.path());
        let live = root.path().join("repo/.af/af.lock");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(&live, "version = 1\n\n[af]\nversion = \"0.9.7\"\n").unwrap();
        let seen = |lock: &Path, version: &str| SeenPin {
            lock: lock.to_path_buf(),
            version: version.to_string(),
            seen: iso_now(),
        };
        let state = SelfState {
            pins: vec![
                seen(&root.path().join("gone/.af/af.lock"), "0.9.6"),
                seen(&live, "0.9.7"),
            ],
            ..SelfState::default()
        };
        write_state(&paths, &state).unwrap();

        // A check holds the versions lock and records itself while a sweep reads the pins.
        let guard = install_lock(&paths).unwrap();
        let reader = {
            let root = root.path().to_path_buf();
            std::thread::spawn(move || pinned_everywhere(&private_paths(&root)))
        };
        write_state(
            &paths,
            &SelfState {
                last_check: Some("2026-10-19T00:00:00Z".to_string()),
                ..state
            },
        )
        .unwrap();
        drop(guard);
        let pinned = reader.join().unwrap();

        assert!(pinned.contains("0.9.7"), "{pinned:?}");
        assert!(!pinned.contains("0.9.6"), "{pinned:?}");
        let state = read_state(&paths);
        assert_eq!(state.last_check.as_deref(), Some("2026-10-19T00:00:00Z"));
        assert_eq!(state.pins.len(), 1);
        assert_eq!(state.pins[0].version, "0.9.7");
    }

    #[test]
    fn the_notice_is_for_a_terminal_only_and_the_check_never_checks_itself() {
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(notice_wanted(&argv(&["af", "task", "list"]), true));
        assert!(!notice_wanted(&argv(&["af", "task", "list"]), false));
        assert!(!notice_wanted(
            &argv(&["af", "task", "list", "--json"]),
            true
        ));
        // The detached child is exempt, so it never schedules another.
        assert!(exempt_from_dispatch(&argv(&[
            "af",
            "self",
            "refresh-check"
        ])));
        assert!(update_policy(&argv(&["af", "self", "refresh-check"])).is_none());
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
    fn sums_are_found_by_asset_name_and_parsed_per_target() {
        let sums = "abc  af-v0.8.0-x.tar.gz\ndef *af-v0.8.0-y.tar.gz\n";
        assert_eq!(find_sum(sums, "af-v0.8.0-y.tar.gz").as_deref(), Some("def"));
        assert_eq!(find_sum(sums, "nope"), None);
        let hex = "0".repeat(64);
        let sums = format!(
            "{hex}  af-v0.8.0-aarch64-apple-darwin.tar.gz\n{hex}  af-v0.8.0-x86_64-unknown-linux-musl.tar.gz\nshort  af-v0.8.0-bad.tar.gz\n{hex}  install.sh\n"
        );
        let digests = parse_sums(&sums, "0.8.0");
        assert_eq!(
            digests.keys().cloned().collect::<Vec<_>>(),
            vec!["aarch64-apple-darwin", "x86_64-unknown-linux-musl"]
        );
        assert_eq!(digests["aarch64-apple-darwin"], format!("sha256:{hex}"));
        assert!(parse_sums(&sums, "0.9.0").is_empty());
    }

    #[test]
    fn the_floor_is_the_oldest_supported_release() {
        assert!(require_self_managed("0.7.1").is_err());
        assert!(require_self_managed("0.8.0-rc.1").is_err());
        assert!(require_self_managed("0.8.0").is_ok());
        assert!(require_self_managed("0.9.0-rc.3").is_ok());
        assert!(require_self_managed("latest").is_err());
    }

    #[test]
    fn dispatch_exemptions_and_overrides() {
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(exempt_from_dispatch(&argv(&["af", "self", "status"])));
        assert!(!exempt_from_dispatch(&argv(&["af", "self", "optimize"])));
        assert_eq!(
            repo_from_argv(&argv(&["af", "self", "optimize", "--repo", "/optimizer"])),
            PathBuf::from("/optimizer")
        );
        assert!(exempt_from_dispatch(&argv(&[
            "af", "review", "plan", "--help"
        ])));
        assert!(exempt_from_dispatch(&argv(&[
            "af",
            "provider",
            "setup",
            "codex-main",
            "--kind",
            "codex"
        ])));
        assert!(exempt_from_dispatch(&argv(&["af", "provider", "recover"])));
        // An older pinned release has no `remove`, and the registry is machine-local (ADR-0136).
        assert!(exempt_from_dispatch(&argv(&[
            "af",
            "provider",
            "remove",
            "codex-main"
        ])));
        assert!(!exempt_from_dispatch(&argv(&["af", "provider", "status"])));
        assert!(exempt_from_dispatch(&argv(&[
            "af", "--repo", "/tmp/x", "provider", "auth", "begin"
        ])));
        assert!(exempt_from_dispatch(&argv(&[
            "af",
            "--repo=/tmp/x",
            "provider",
            "auth",
            "status"
        ])));

        // The browser: bare, or with only a repository to open.
        assert!(exempt_from_dispatch(&argv(&["af", "--repo", "/tmp/x"])));
        assert!(exempt_from_dispatch(&argv(&["af", "--repo=/tmp/x"])));
        assert!(!exempt_from_dispatch(&argv(&["af", "--repo"])));
        assert!(!exempt_from_dispatch(&argv(&[
            "af", "--repo", "/tmp/x", "task", "list"
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
        assert_eq!(
            onboard_af_from_argv(&argv(&[
                "af",
                "onboard",
                "--refresh-lock",
                "--af",
                "v0.9.0"
            ])),
            Some("0.9.0".to_string())
        );
        assert_eq!(
            onboard_af_from_argv(&argv(&["af", "onboard", "--af=0.9.0"])),
            Some("0.9.0".to_string())
        );
        assert_eq!(
            onboard_af_from_argv(&argv(&["af", "review", "--af", "1"])),
            None
        );
    }
}
