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

/// One installer at a time per layout: two dispatches racing to install the same pin would
/// otherwise replace a directory the other is about to exec.
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

/// Download, verify, extract, prove the version, and record a receipt. Never touches the default
/// symlink. `expected` is the digest a project lock records for this target: when present it is
/// the only thing the bytes must match; otherwise the release's checksums decide.
fn install(
    paths: &Paths,
    policy: &SelfPolicy,
    version: &str,
    expected: Option<&str>,
) -> Result<PathBuf, InstallError> {
    if offline() {
        return Err(InstallError::Other(format!(
            "af {version} is not installed and {OFFLINE_ENV} is set — fix: unset it and run `af self install {version}`"
        )));
    }
    require_self_managed(version)?;
    let _guard = install_lock(paths)?;
    if let Some(binary) = installed(paths, version) {
        // Another installer finished first; under a lock its bytes must still be the lock's.
        if let (Some(expected), Some(receipt)) = (expected, read_receipt(paths, version))
            && receipt.sha256 != strip_sha256(expected)
        {
            return Err(InstallError::Mismatch(format!(
                "installed af {version} (sha256 {}) is not the release the project lock recorded for {TARGET} ({expected})",
                receipt.sha256
            )));
        }
        return Ok(binary);
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
    let (expected, verified_by) = match expected {
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
    if actual != expected {
        return Err(InstallError::Mismatch(format!(
            "checksum mismatch for {asset} ({})\nexpected {expected}\nactual   {actual}",
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

/// Retarget the default symlink atomically and record the activation.
fn set_default(paths: &Paths, version: &str) -> Result<(), String> {
    require_self_managed(version)?;
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
    let first = argv.get(1).map(String::as_str);
    // Bootstrap is machine-local, not project authority. Dispatching it through an older project
    // pin would make the newly installed command disappear precisely where users need it.
    // `self optimize` is the exception: it reads project authority, so it dispatches (ADR-0105).
    let binary_management_self =
        first == Some("self") && argv.get(2).map(String::as_str) != Some("optimize");
    matches!(first, None | Some("help" | "completions" | "config"))
        || binary_management_self
        || matches!(
            (first, argv.get(2).map(String::as_str)),
            (Some("provider"), Some("setup" | "recover"))
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
    let mut state = read_state(paths);
    let lock = std::fs::canonicalize(lock).unwrap_or_else(|_| lock.to_path_buf());
    if state
        .pins
        .iter()
        .any(|seen| seen.lock == lock && seen.version == version)
    {
        return;
    }
    state.pins.retain(|seen| seen.lock != lock);
    state.pins.push(SeenPin {
        lock,
        version: version.to_string(),
        seen: iso_now(),
    });
    let overflow = state.pins.len().saturating_sub(MAX_SEEN_PINS);
    state.pins.drain(..overflow);
    let _ = write_state(paths, &state);
}

/// Every version some project on this machine still pins: the lock in the current directory
/// and every lock dispatch has honoured that still exists and still pins. Forgotten entries are
/// dropped from the state on the way.
fn pinned_everywhere(paths: &Paths) -> BTreeSet<String> {
    let mut versions = BTreeSet::new();
    if let Some(here) = pinned(Path::new(".")) {
        versions.insert(here.pin.version);
    }
    let mut state = read_state(paths);
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
    if state.pins.len() != before {
        let _ = write_state(paths, &state);
    }
    versions
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
                Ok(policy) => install(&paths, &policy, &request.version, request.digest.as_deref()),
            };
            match attempt {
                Ok(binary) => binary,
                Err(InstallError::Mismatch(reason)) => {
                    eprintln!("af: {why}, and the release could not be installed: {reason}");
                    std::process::exit(1);
                }
                Err(InstallError::Other(reason)) => {
                    // Fail closed whenever running on would mean an older binary interpreting a
                    // newer release's pin, or ignoring an explicit request. An older pin under a
                    // newer binary is the #47 case: proceed and say so.
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
                    return;
                }
            }
        }
    };
    if let Some(lock) = &request.lock {
        record_seen_pin(&paths, lock, &request.version);
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
            install(&paths, &policy, &latest, None).map_err(|error| error.to_string())?;
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
    let target = target.to_string();
    require_self_managed(&target)?;
    require_receipt(&paths)?;
    if installed(&paths, &target).is_none() {
        install(&paths, &policy, &target, None).map_err(|error| error.to_string())?;
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
    if installed(&paths, version).is_none() {
        return Err(format!("af {version} is not installed"));
    }
    if default_version(&paths).as_deref() == Some(version) {
        return Err(format!(
            "af {version} is the default — fix: af self update or af self rollback first"
        ));
    }
    if pinned_everywhere(&paths).contains(version) {
        return Err(format!(
            "af {version} is pinned by a project this machine has seen (`af self status --json` lists them) — fix: re-pin those projects first"
        ));
    }
    let dir = version_dir(&paths, version);
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
    let pinned = pinned_everywhere(&paths);
    let versions = installed_versions(&paths);
    let mut removed = 0;
    let total = versions.len();
    for (index, version) in versions.iter().enumerate() {
        let version = version.to_string();
        let protected = Some(&version) == default.as_ref() || pinned.contains(&version);
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
        println!("nothing to prune (keeping {keep}, plus the default and every pin seen)");
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
