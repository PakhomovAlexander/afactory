//! Machine-local provider inventory.
//!
//! The registry names auth directories, never credentials, arbitrary commands, arguments, or
//! environment variables. Explicit review bindings are admitted through durable, fenced provider
//! operations before dispatch. Status is obtained from
//! the two fixed adapter CLIs with bounded output and wall time. Codex exposes its plan and quota
//! windows through the official local app-server protocol. Claude has no headless usage-status
//! command, so its fixed local `/usage` screen is opened in a bounded pseudo-terminal and only the
//! weekly percentages are parsed. The probe neither reads credentials nor starts a billable
//! model session. Accepted response shapes are pinned by fixtures.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use review_attempt::{BudgetLedger, Reservation, Scope};
use review_core::{
    Command as ReviewerCommand, EventType, ProviderFailureClassV1, ProviderNextActionV1,
    ProviderOperationStateV1, ProviderOperationTransitionPayloadV1,
};
use review_pipeline::RoundAuthority;
use review_store::{Cas, EventStore, NewEvent};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub mod task;

const MAX_PROVIDERS: usize = 32;
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_CONCURRENT_PROBES: usize = 4;
const MAX_PROVIDER_LIMITS: usize = 16;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CLAUDE_STRUCTURAL_TIMEOUT: Duration = Duration::from_secs(30);
const CLAUDE_USAGE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLAUDE_USAGE_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_ORPHANED_CLAUDE_READERS: usize = 2;
const MAX_CLAUDE_READER_SLOTS: usize = MAX_CONCURRENT_PROBES + MAX_ORPHANED_CLAUDE_READERS;
const SMOKE_TIMEOUT: Duration = Duration::from_secs(45);
const SMOKE_RESERVATION: u64 = 4_096;

static CLAUDE_USAGE_CACHE: OnceLock<Mutex<BTreeMap<ClaudeUsageCacheKey, CachedClaudeUsage>>> =
    OnceLock::new();
static CLAUDE_READER_SLOTS: AtomicUsize = AtomicUsize::new(0);

pub struct ProviderAdmission {
    auth_dir: PathBuf,
}

pub struct AdmissionRequest<'a> {
    pub node_id: &'a str,
    pub reviewer: &'a ReviewerCommand,
    pub state_dir: &'a Path,
    pub run_id: &'a str,
    pub authority: &'a RoundAuthority,
    pub cas: &'a Cas,
    pub store: &'a mut EventStore,
    pub resumes: &'a mut BTreeMap<String, u64>,
    pub budget: &'a mut BudgetLedger,
    pub structural_probes: &'a mut BTreeSet<String>,
}

impl ProviderAdmission {
    pub fn auth_dir_string(&self) -> Result<String, String> {
        self.auth_dir
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| "provider auth directory must be valid UTF-8".to_string())
    }
}

pub struct ProviderInventory {
    pub providers: Vec<ProviderStatus>,
    pub warning: Option<String>,
}

pub struct ProviderStatus {
    pub id: String,
    pub kind: String,
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
    registry_declared: bool,
    source: String,
}

pub fn discover() -> ProviderInventory {
    let (specs, warning) = load_specs();
    let mut providers = Vec::with_capacity(specs.len());
    for chunk in specs.chunks(MAX_CONCURRENT_PROBES) {
        thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .cloned()
                .map(|spec| {
                    let fallback = spec.clone();
                    (fallback, scope.spawn(move || probe_provider(spec)))
                })
                .collect();
            providers.extend(handles.into_iter().map(|(spec, handle)| {
                handle
                    .join()
                    .unwrap_or_else(|_| unavailable_status(&spec, "provider status probe panicked"))
            }));
        });
    }
    cross_reference_logged_in_siblings(&mut providers);
    ProviderInventory { providers, warning }
}

/// Point each logged-out context at the same-kind contexts on this machine that are logged in.
///
/// The usual cause of a `not authenticated` registry entry is an `auth_dir` naming a directory
/// the operator never logged in to, while the intended login sits in another directory `af`
/// also probed (often the ambient `CLAUDE_CONFIG_DIR`). Naming that sibling turns a bare
/// status into the exact `auth_dir` correction; nothing here reads credentials or probes again.
fn cross_reference_logged_in_siblings(providers: &mut [ProviderStatus]) {
    let logged_in: Vec<(String, String)> = providers
        .iter()
        .filter(|provider| provider.status == "authenticated")
        .map(|provider| {
            let context = if provider.auth_context == "CLI default" {
                "the CLI default directory".to_string()
            } else {
                provider.auth_context.clone()
            };
            (
                provider.kind.clone(),
                format!(
                    "{} is authenticated in {context} ({})",
                    provider.id, provider.subscription
                ),
            )
        })
        .collect();
    for provider in providers
        .iter_mut()
        .filter(|provider| provider.status == "not authenticated")
    {
        for (_, sibling) in logged_in.iter().filter(|(kind, _)| *kind == provider.kind) {
            if !provider.detail.is_empty() {
                provider.detail.push_str("; ");
            }
            provider.detail.push_str(sibling);
        }
    }
}

pub fn print_status() {
    let inventory = discover();
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
    let mut ambient = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for provider in inventory.providers {
        ids.insert(provider.id.clone());
        if matches!(provider.id.as_str(), "claude-ambient" | "codex-ambient")
            && provider
                .source
                .starts_with("ambient CLI candidate; unstable local context label")
        {
            ambient.insert(provider.kind.clone());
        }
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
    if !ambient.is_empty() {
        println!();
        println!("Ambient IDs are discovered only and cannot be selected by --provider.");
        for kind in ambient {
            let id = available_provider_id(&kind, &ids);
            println!("  set up {kind}: af provider setup {id} --kind {kind}");
        }
    }
}

fn available_provider_id(kind: &str, ids: &BTreeSet<String>) -> String {
    let base = format!("{kind}-main");
    if !ids.contains(&base) {
        return base;
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !ids.contains(candidate))
        .expect("the finite provider registry cannot exhaust numeric suffixes")
}

/// Add one explicit Provider without requiring the user to learn the registry's TOML shape.
/// Existing entries and auth contexts are immutable through this absent-only command.
pub fn add(id: &str, kind: &str, auth_dir: Option<&Path>) -> Result<(), String> {
    validate_explicit_id(id)?;
    let kind = ProviderKind::parse(kind)?;
    let auth_dir = resolve_auth_dir(kind, auth_dir, false)?;
    let path = registry_path()?.ok_or("no provider registry path is available")?;
    let preserved = add_to_registry(&path, id, kind, &auth_dir)?;
    println!(
        "provider {id} registered in {} ({}, {})",
        path.display(),
        kind.name(),
        auth_dir.display()
    );
    if let Some(previous) = preserved {
        println!(
            "previous provider registry preserved at {}",
            previous.display()
        );
    }
    println!("next: af provider status");
    Ok(())
}

/// Own the complete first-run Provider flow while leaving credentials with the harness CLI.
///
/// The complete status/login/publication flow is serialized on the registry lock. The waiter
/// rechecks authentication after acquiring the lock, so concurrent first-time setup never drives
/// one Provider auth context from two interactive processes.
pub fn setup(id: &str, kind: &str, auth_dir: Option<&Path>) -> Result<(), String> {
    validate_explicit_id(id)?;
    let kind = ProviderKind::parse(kind)?;
    let path = registry_path()?.ok_or("no provider registry path is available")?;
    let auth_dir = resolve_auth_dir(kind, auth_dir, true)?;
    // Registry paths are configurable, so the login lock lives in and is keyed by the canonical
    // auth context itself. A waiter rechecks authentication only after acquiring this lock.
    let auth_lock = auth_context_lock(kind, &auth_dir)?;
    let _registry_lock = registry_lock(&path)?;
    auth_lock.ensure_directory_current(&auth_dir, "auth directory")?;
    let already_registered = inspect_registration(&path, id, kind, &auth_dir)?;
    let spec = ProviderSpec {
        id: id.to_string(),
        kind,
        auth_dir: Some(auth_dir.clone()),
        explicit_selector: true,
        registry_declared: already_registered,
        source: path.display().to_string(),
    };

    auth_lock.ensure_directory_current(&auth_dir, "auth directory")?;
    if !authentication_ready(&spec)? {
        auth_lock.ensure_directory_current(&auth_dir, "auth directory")?;
        run_interactive_login(&spec)?;
        auth_lock.ensure_directory_current(&auth_dir, "auth directory")?;
        if !authentication_ready(&spec)? {
            return Err(format!(
                "{} login completed without authenticating {}",
                kind.name(),
                auth_dir.display()
            ));
        }
    }
    auth_lock.ensure_directory_current(&auth_dir, "auth directory")?;

    if already_registered {
        println!(
            "provider {id} is authenticated and registered ({}, {})",
            kind.name(),
            auth_dir.display()
        );
        println!("next: af provider status");
        return Ok(());
    }

    match add_to_registry_locked(&path, id, kind, &auth_dir, true)? {
        RegistryAdd::Added(preserved) => {
            println!(
                "provider {id} authenticated and registered in {} ({}, {})",
                path.display(),
                kind.name(),
                auth_dir.display()
            );
            if let Some(previous) = preserved {
                println!(
                    "previous provider registry preserved at {}",
                    previous.display()
                );
            }
        }
        RegistryAdd::AlreadyPresent => println!(
            "provider {id} is authenticated and registered ({}, {})",
            kind.name(),
            auth_dir.display()
        ),
    }
    println!("next: af provider status");
    Ok(())
}

/// Resolve an interrupted registry publication without guessing which version committed.
pub fn recover() -> Result<(), String> {
    let configured = registry_path()?.ok_or("no provider registry path is available")?;
    recover_configured_registry(&configured)
}

fn recover_configured_registry(configured: &Path) -> Result<(), String> {
    recover_configured_registry_with_hook(configured, || {})
}

fn recover_configured_registry_with_hook(
    configured: &Path,
    after_locks: impl FnOnce(),
) -> Result<(), String> {
    let configured_parent = configured
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", configured.display()))?;
    match fs::metadata(configured_parent) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "provider registry directory {} is not a directory",
                configured_parent.display()
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            create_dir_all_durable(configured_parent)?;
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect provider registry directory {}: {error}",
                configured_parent.display()
            ));
        }
    }
    let resolved = resolve_registry(configured)?;
    let locations = canonical_registry_locations(configured, resolved.as_deref())?;
    let mut locks = Vec::with_capacity(locations.len());
    for location in &locations {
        locks.push(registry_lock(location)?);
    }
    after_locks();
    let current_resolved = resolve_registry(configured)?;
    let current_locations = canonical_registry_locations(configured, current_resolved.as_deref())?;
    if current_resolved != resolved || current_locations != locations {
        return Err(format!(
            "provider registry alias {} changed while recovery locks were acquired; retry",
            configured.display()
        ));
    }
    let transactions = locations
        .iter()
        .filter_map(|location| match registry_transaction_exists(location) {
            Ok(true) => Some(Ok(location)),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if transactions.len() > 1 {
        return Err(format!(
            "provider registry {} and its resolved target both have unfinished publications; refusing ambiguous recovery",
            configured.display()
        ));
    }
    if let Some(location) = transactions.first() {
        return recover_registry(location);
    }
    println!(
        "provider registry {} has no unfinished publication",
        configured.display()
    );
    Ok(())
}

fn canonical_registry_locations(
    configured: &Path,
    resolved: Option<&Path>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut locations = BTreeSet::new();
    locations.insert(canonical_registry_location(configured)?);
    if let Some(resolved) = resolved {
        locations.insert(canonical_registry_location(resolved)?);
    }
    Ok(locations)
}

fn canonical_registry_location(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("provider registry {} has no file name", path.display()))?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        format!(
            "cannot resolve provider registry directory {}: {error}",
            parent.display()
        )
    })?;
    Ok(parent.join(name))
}

fn validate_explicit_id(id: &str) -> Result<(), String> {
    safe_id(id)?;
    if matches!(id, "claude-ambient" | "codex-ambient") {
        return Err(format!(
            "provider id `{id}` is reserved for ambient discovery"
        ));
    }
    Ok(())
}

fn resolve_auth_dir(
    kind: ProviderKind,
    auth_dir: Option<&Path>,
    create: bool,
) -> Result<PathBuf, String> {
    let auth_dir = match auth_dir {
        Some(path) => path.to_path_buf(),
        None => default_auth_dir(kind)?,
    };
    if !auth_dir.is_absolute()
        || auth_dir
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err("--auth-dir must be an absolute path without `..`".into());
    }
    // The registry format is TOML and therefore cannot represent an arbitrary Unix byte path.
    // Resolve the deepest existing ancestor first: a printable alias can otherwise point through
    // a symlink to a non-UTF-8 parent and create an unpublishable leaf before canonicalization.
    validate_utf8_destination(&auth_dir)?;
    match fs::symlink_metadata(&auth_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "auth directory {} must not be a symlink",
                auth_dir.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound && create => {
            create_private_directory(&auth_dir)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect auth directory {}: {error}",
                auth_dir.display()
            ));
        }
    }
    let unresolved = fs::symlink_metadata(&auth_dir).map_err(|error| {
        format!(
            "cannot inspect auth directory {}: {error}",
            auth_dir.display()
        )
    })?;
    if unresolved.file_type().is_symlink() {
        return Err(format!(
            "auth directory {} must not be a symlink",
            auth_dir.display()
        ));
    }
    validate_secure_directory_chain(&auth_dir, "auth directory")?;
    let auth_dir = fs::canonicalize(&auth_dir).map_err(|error| {
        format!(
            "auth directory {} cannot be resolved: {error}",
            auth_dir.display()
        )
    })?;
    if auth_dir.to_str().is_none() {
        return Err(format!(
            "auth directory {} is not valid UTF-8 and cannot be stored in TOML",
            auth_dir.display()
        ));
    }
    if !auth_dir.is_dir() {
        return Err(format!(
            "auth directory {} is not a directory",
            auth_dir.display()
        ));
    }
    validate_private_auth_directory(&auth_dir)?;
    Ok(auth_dir)
}

#[cfg(unix)]
fn validate_private_auth_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect auth directory {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "auth directory {} must be a real directory",
            path.display()
        ));
    }
    validate_secure_directory_chain(path, "auth directory")?;
    let effective_uid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != effective_uid {
        return Err(format!(
            "auth directory {} is owned by uid {}, not the current uid {effective_uid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        let fix = chmod_fix(path, "go-w");
        return Err(format!(
            "auth directory {} is writable by another user; fix: {fix}",
            path.display(),
        ));
    }
    Ok(())
}

fn validate_utf8_destination(path: &Path) -> Result<(), String> {
    if path.to_str().is_none() {
        return Err(format!(
            "auth directory {} is not valid UTF-8 and cannot be stored in TOML",
            path.display()
        ));
    }
    let mut suffix = Vec::new();
    let mut ancestor = path;
    let mut resolved = loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                break fs::canonicalize(ancestor).map_err(|error| {
                    format!(
                        "auth directory ancestor {} cannot be resolved: {error}",
                        ancestor.display()
                    )
                })?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    format!("auth directory {} has no existing ancestor", path.display())
                })?;
                suffix.push(name.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    format!("auth directory {} has no existing ancestor", path.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect auth directory ancestor {}: {error}",
                    ancestor.display()
                ));
            }
        }
    };
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    if resolved.to_str().is_none() {
        return Err(format!(
            "auth directory {} resolves through a non-UTF-8 path and cannot be stored in TOML",
            path.display()
        ));
    }
    Ok(())
}

fn missing_directories(path: &Path, what: &str) -> Result<(Vec<PathBuf>, PathBuf), String> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match fs::metadata(cursor) {
            Ok(metadata) if metadata.is_dir() => return Ok((missing, cursor.to_path_buf())),
            Ok(_) => {
                return Err(format!("{what} {} is not a directory", cursor.display()));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| format!("{what} {} has no existing ancestor", path.display()))?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect {what} {}: {error}",
                    cursor.display()
                ));
            }
        }
    }
}

#[cfg(unix)]
fn validate_secure_directory_chain(path: &Path, what: &str) -> Result<(), String> {
    validate_directory_chain(path, what, true)
}

#[cfg(unix)]
fn validate_creation_ancestor(path: &Path, what: &str) -> Result<(), String> {
    validate_directory_chain(path, what, false)
}

#[cfg(unix)]
fn validate_directory_chain(
    path: &Path,
    what: &str,
    require_owned_leaf: bool,
) -> Result<(), String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve {what} {}: {error}", path.display()))?;
    let mut roots = vec![path.to_path_buf()];
    if canonical != path {
        roots.push(canonical);
    }
    let mut checked = BTreeSet::new();
    for root in roots {
        let mut current = Some(root.as_path());
        let mut leaf = true;
        while let Some(directory) = current {
            if checked.insert(directory.to_path_buf()) {
                let metadata = fs::metadata(directory).map_err(|error| {
                    format!(
                        "cannot inspect rename-controlling directory {} for {what}: {error}",
                        directory.display()
                    )
                })?;
                if !metadata.is_dir() {
                    return Err(format!(
                        "rename-controlling path {} for {what} is not a directory",
                        directory.display()
                    ));
                }
                if leaf && require_owned_leaf {
                    validate_owned_not_writable_by_others(directory, &metadata, what)?;
                } else {
                    validate_rename_controlling_directory(directory, &metadata, what)?;
                }
            }
            leaf = false;
            current = directory.parent();
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_rename_controlling_directory(
    path: &Path,
    metadata: &fs::Metadata,
    what: &str,
) -> Result<(), String> {
    validate_rename_controlling_directory_for_uid(
        path,
        metadata,
        what,
        nix::unistd::geteuid().as_raw(),
    )
}

#[cfg(unix)]
fn validate_rename_controlling_directory_for_uid(
    path: &Path,
    metadata: &fs::Metadata,
    what: &str,
    effective_uid: u32,
) -> Result<(), String> {
    validate_rename_controlling_directory_values(
        path,
        metadata.uid(),
        metadata.mode(),
        what,
        effective_uid,
    )
}

#[cfg(unix)]
fn validate_rename_controlling_directory_values(
    path: &Path,
    owner_uid: u32,
    mode: u32,
    what: &str,
    effective_uid: u32,
) -> Result<(), String> {
    if owner_uid != effective_uid && owner_uid != 0 {
        return Err(format!(
            "rename-controlling directory {} for {what} is owned by uid {}, not the current uid {effective_uid} or root; move {what} under a directory controlled by the current user",
            path.display(),
            owner_uid
        ));
    }
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(format!(
            "rename-controlling directory {} for {what} is writable by another user without the sticky bit; fix: secure that directory or move {what} under a private parent",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_secure_directory_chain(_path: &Path, _what: &str) -> Result<(), String> {
    Ok(())
}

fn chmod_fix(path: &Path, mode: &str) -> String {
    path.to_str().map_or_else(
        || format!("remove the unsafe permission bits from {}", path.display()),
        |path| format!("chmod {mode} -- {}", shell_words::quote(path)),
    )
}

#[cfg(not(unix))]
fn validate_private_auth_directory(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "auth directory {} must be a real directory",
            path.display()
        ))
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), String> {
    create_private_directory_tree(path, "auth directory", false)
}

#[cfg(unix)]
fn create_private_directory_tree(path: &Path, what: &str, durable: bool) -> Result<(), String> {
    use rustix::fs::{AtFlags, Mode, OFlags, chmodat, mkdirat, open, openat};

    let (missing, existing) = missing_directories(path, what)?;
    // Reject an unsafe rename-controlling ancestor before creating even the first descendant.
    validate_creation_ancestor(&existing, what)?;
    let canonical = fs::canonicalize(&existing)
        .map_err(|error| format!("cannot resolve {what} {}: {error}", existing.display()))?;
    let mut parent: File = open(
        &canonical,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("opening {what} ancestor {}: {error}", existing.display()))?
    .into();
    if !bound_directory_is_current(&existing, &parent).unwrap_or(false) {
        return Err(format!(
            "{what} ancestor {} changed before directory creation; retry",
            existing.display()
        ));
    }

    for directory in missing.into_iter().rev() {
        let name = directory
            .file_name()
            .ok_or_else(|| format!("{what} {} has no directory component", directory.display()))?;
        let created = match mkdirat(&parent, name, Mode::RWXU) {
            Ok(()) => true,
            Err(error) if error == rustix::io::Errno::EXIST => false,
            Err(error) => {
                return Err(format!(
                    "cannot create {what} {}: {error}",
                    directory.display()
                ));
            }
        };
        if created {
            // Mode 0700 never exposes group/world access, while this handle-relative chmod repairs
            // owner access removed by a restrictive umask before the next descendant is opened.
            chmodat(&parent, name, Mode::RWXU, AtFlags::empty()).map_err(|error| {
                format!("cannot secure {what} {}: {error}", directory.display())
            })?;
        }
        let child: File = openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "opening newly created {what} {} without following links: {error}",
                directory.display()
            )
        })?
        .into();
        let metadata = child
            .metadata()
            .map_err(|error| format!("inspecting {what} {}: {error}", directory.display()))?;
        validate_owned_not_writable_by_others(&directory, &metadata, what)?;
        if durable {
            child
                .sync_all()
                .map_err(|error| format!("syncing {what} {}: {error}", directory.display()))?;
            parent.sync_all().map_err(|error| {
                format!("syncing parent of {what} {}: {error}", directory.display())
            })?;
        }
        parent = child;
    }
    validate_secure_directory_chain(path, what)
}

struct BoundDirectoryLock {
    _lock: File,
    #[cfg(unix)]
    directory: File,
}

impl BoundDirectoryLock {
    fn ensure_directory_current(&self, path: &Path, what: &str) -> Result<(), String> {
        #[cfg(unix)]
        if !bound_directory_is_current(path, &self.directory).unwrap_or(false) {
            return Err(format!(
                "{what} {} changed while locked; retry",
                path.display()
            ));
        }
        #[cfg(not(unix))]
        let _ = (path, what);
        Ok(())
    }
}

#[cfg(unix)]
fn bind_directory(path: &Path, what: &str) -> Result<File, String> {
    use rustix::fs::{Mode, OFlags, open};

    validate_secure_directory_chain(path, what)?;
    let directory: File = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "opening {what} {} without following links: {error}",
            path.display()
        )
    })?
    .into();
    if !bound_directory_is_current(path, &directory).unwrap_or(false) {
        return Err(format!(
            "{what} {} changed while it was being secured; retry",
            path.display()
        ));
    }
    Ok(directory)
}

#[cfg(unix)]
fn bound_directory_is_current(path: &Path, directory: &File) -> std::io::Result<bool> {
    let path = fs::symlink_metadata(path)?;
    let directory = directory.metadata()?;
    Ok(!path.file_type().is_symlink()
        && path.is_dir()
        && directory.is_dir()
        && path.dev() == directory.dev()
        && path.ino() == directory.ino())
}

fn auth_context_lock(kind: ProviderKind, auth_dir: &Path) -> Result<BoundDirectoryLock, String> {
    auth_context_lock_with_hook(kind, auth_dir, || {})
}

fn auth_context_lock_with_hook(
    kind: ProviderKind,
    auth_dir: &Path,
    before_wait: impl FnOnce(),
) -> Result<BoundDirectoryLock, String> {
    #[cfg(unix)]
    let directory = bind_directory(auth_dir, "auth directory")?;
    let lock_path = auth_dir.join(format!(".af-{}-setup.lock", kind.name()));
    #[cfg(unix)]
    let file: File = {
        use rustix::fs::{Mode, OFlags, openat};

        openat(
            &directory,
            lock_path.file_name().ok_or_else(|| {
                format!("auth-context lock {} has no file name", lock_path.display())
            })?,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| {
            format!(
                "opening {} auth-context lock {}: {error}",
                kind.name(),
                lock_path.display()
            )
        })?
        .into()
    };
    #[cfg(not(unix))]
    let file = {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        options.open(&lock_path).map_err(|error| {
            format!(
                "opening {} auth-context lock {}: {error}",
                kind.name(),
                lock_path.display()
            )
        })?
    };
    let metadata = file.metadata().map_err(|error| {
        format!(
            "inspecting {} auth-context lock {}: {error}",
            kind.name(),
            lock_path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "{} auth-context lock {} must be a regular file",
            kind.name(),
            lock_path.display()
        ));
    }
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "securing {} auth-context lock {}: {error}",
                    kind.name(),
                    lock_path.display()
                )
            })?;
        let secured_metadata = file.metadata().map_err(|error| {
            format!(
                "re-inspecting {} auth-context lock {}: {error}",
                kind.name(),
                lock_path.display()
            )
        })?;
        validate_owned_not_writable_by_others(&lock_path, &secured_metadata, "auth-context lock")?;
        if !bound_directory_is_current(auth_dir, &directory).unwrap_or(false) {
            return Err(format!(
                "auth directory {} changed while its setup lock was opened; retry",
                auth_dir.display()
            ));
        }
    }
    before_wait();
    fs2::FileExt::lock_exclusive(&file).map_err(|error| {
        format!(
            "locking {} auth context {}: {error}",
            kind.name(),
            auth_dir.display()
        )
    })?;
    #[cfg(unix)]
    if !bound_directory_is_current(auth_dir, &directory).unwrap_or(false) {
        return Err(format!(
            "auth directory {} changed while waiting for its setup lock; retry",
            auth_dir.display()
        ));
    }
    Ok(BoundDirectoryLock {
        _lock: file,
        #[cfg(unix)]
        directory,
    })
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create auth directory {}: {error}", path.display()))
}

/// Return true only when the requested ID already names this exact context. Conflicts are
/// rejected before an interactive login; `add_to_registry` repeats these checks under its lock.
fn inspect_registration(
    path: &Path,
    id: &str,
    kind: ProviderKind,
    auth_dir: &Path,
) -> Result<bool, String> {
    let Some(text) = read_registry(path)? else {
        return Ok(false);
    };
    let specs = parse_registry(&text, path)?;
    if let Some(existing) = specs.iter().find(|spec| spec.id == id) {
        if existing.kind == kind && existing.auth_dir.as_deref() == Some(auth_dir) {
            return Ok(true);
        }
        return Err(format!(
            "provider `{id}` already names a different auth context"
        ));
    }
    if let Some(existing) = specs
        .iter()
        .find(|spec| spec.kind == kind && spec.auth_dir.as_deref() == Some(auth_dir))
    {
        return Err(format!(
            "{} auth context {} is already registered as provider `{}`",
            kind.name(),
            auth_dir.display(),
            existing.id
        ));
    }
    if specs.len() >= MAX_PROVIDERS {
        return Err(format!(
            "provider registry {} already has the limit of {MAX_PROVIDERS} entries",
            path.display()
        ));
    }
    Ok(false)
}

fn authentication_ready(spec: &ProviderSpec) -> Result<bool, String> {
    if let Some(auth_dir) = spec.auth_dir.as_deref() {
        validate_private_auth_directory(auth_dir)?;
    }
    let program = resolve_program(spec.kind.command())
        .ok_or_else(|| format!("{} is not on PATH", spec.kind.command()))?;
    let output = run_probe(&program, spec, &sanitized_path(), &AtomicBool::new(false))?;
    let (status, _, detail) = match spec.kind {
        ProviderKind::Claude => parse_claude_status(output.status.success(), &output.stdout),
        ProviderKind::Codex => parse_codex_status(output.status.success(), &output.stdout),
    };
    match status.as_str() {
        "authenticated" if output.status.success() => Ok(true),
        "authenticated" => Err(format!(
            "{} authentication status reported a login but exited unsuccessfully",
            spec.kind.name()
        )),
        "not authenticated" => Ok(false),
        _ => Err(if detail.is_empty() {
            format!("{} authentication status is {status}", spec.kind.name())
        } else {
            detail
        }),
    }
}

fn run_interactive_login(spec: &ProviderSpec) -> Result<(), String> {
    let program = resolve_program(spec.kind.command())
        .ok_or_else(|| format!("{} is not on PATH", spec.kind.command()))?;
    let auth_dir = spec
        .auth_dir
        .as_deref()
        .ok_or("provider setup requires an explicit auth directory")?;
    validate_private_auth_directory(auth_dir)?;
    println!(
        "starting interactive {} login for {}",
        spec.kind.name(),
        auth_dir.display()
    );
    let mut command = Command::new(program);
    command
        .env_clear()
        .current_dir(Path::new("/"))
        .env("PATH", sanitized_path());
    for name in [
        "HOME",
        "USER",
        "TERM",
        "COLORTERM",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    match spec.kind {
        ProviderKind::Claude => {
            command.args(["auth", "login", "--claudeai"]);
            command.env("CLAUDE_CONFIG_DIR", auth_dir);
        }
        ProviderKind::Codex => {
            command.arg("login");
            command.env("CODEX_HOME", auth_dir);
        }
    }
    let status = command
        .status()
        .map_err(|error| format!("cannot start {} login: {error}", spec.kind.name()))?;
    if !status.success() {
        return Err(format!(
            "{} login exited with {status}; no provider was registered",
            spec.kind.name()
        ));
    }
    Ok(())
}

fn default_auth_dir(kind: ProviderKind) -> Result<PathBuf, String> {
    let variable = match kind {
        ProviderKind::Claude => "CLAUDE_CONFIG_DIR",
        ProviderKind::Codex => "CODEX_HOME",
    };
    if let Some(value) = std::env::var_os(variable)
        && !value.is_empty()
    {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(format!("{variable} must be absolute"));
        }
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or("HOME is not set; pass --auth-dir")?;
    if !home.is_absolute() {
        return Err("HOME must be absolute; pass --auth-dir".into());
    }
    Ok(home.join(match kind {
        ProviderKind::Claude => ".claude",
        ProviderKind::Codex => ".codex",
    }))
}

fn add_to_registry(
    path: &Path,
    id: &str,
    kind: ProviderKind,
    auth_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    match add_to_registry_inner(path, id, kind, auth_dir, false)? {
        RegistryAdd::Added(preserved) => Ok(preserved),
        RegistryAdd::AlreadyPresent => unreachable!("plain add rejects existing providers"),
    }
}

enum RegistryAdd {
    Added(Option<PathBuf>),
    AlreadyPresent,
}

fn add_to_registry_inner(
    path: &Path,
    id: &str,
    kind: ProviderKind,
    auth_dir: &Path,
    exact_is_success: bool,
) -> Result<RegistryAdd, String> {
    let _lock = registry_lock(path)?;
    add_to_registry_locked(path, id, kind, auth_dir, exact_is_success)
}

fn add_to_registry_locked(
    path: &Path,
    id: &str,
    kind: ProviderKind,
    auth_dir: &Path,
    exact_is_success: bool,
) -> Result<RegistryAdd, String> {
    // A prior first-file publication may have crashed after creating its marker but before the
    // registry appeared. Never treat that recovery state as an empty registry.
    ensure_no_registry_transaction(path)?;
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "provider registry {} must be a regular file, not a symlink",
                    path.display()
                ));
            }
            Some(
                read_registry(path)?
                    .ok_or_else(|| format!("provider registry {} disappeared", path.display()))?,
            )
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "cannot inspect provider registry {}: {error}",
                path.display()
            ));
        }
    };
    let mut document = match existing.as_deref() {
        Some(text) => {
            let specs = parse_registry(text, path)?;
            if let Some(existing) = specs.iter().find(|spec| spec.id == id) {
                if exact_is_success
                    && existing.kind == kind
                    && existing.auth_dir.as_deref() == Some(auth_dir)
                {
                    return Ok(RegistryAdd::AlreadyPresent);
                }
                return Err(format!("provider `{id}` already exists"));
            }
            if let Some(duplicate) = specs
                .iter()
                .find(|spec| spec.kind == kind && spec.auth_dir.as_deref() == Some(auth_dir))
            {
                return Err(format!(
                    "{} auth context {} duplicates provider `{}`",
                    kind.name(),
                    auth_dir.display(),
                    duplicate.id
                ));
            }
            if specs.len() >= MAX_PROVIDERS {
                return Err(format!(
                    "provider registry {} already has the limit of {MAX_PROVIDERS} entries",
                    path.display()
                ));
            }
            text.parse::<DocumentMut>()
                .map_err(|error| format!("provider registry {}: {error}", path.display()))?
        }
        None => {
            let mut document = DocumentMut::new();
            document["version"] = value(1);
            document
        }
    };
    normalize_provider_tables(&mut document, path)?;
    let providers = document["providers"]
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            format!(
                "provider registry {} `providers` must be an array of tables",
                path.display()
            )
        })?;
    let mut provider = Table::new();
    provider["id"] = value(id);
    provider["kind"] = value(kind.name());
    provider["auth_dir"] = value(auth_dir.to_str().ok_or_else(|| {
        format!(
            "auth directory {} is not valid UTF-8 and cannot be stored in TOML",
            auth_dir.display()
        )
    })?);
    providers.push(provider);
    let bytes = document.to_string();
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(format!(
            "provider registry {} would exceed {MAX_REGISTRY_BYTES} bytes",
            path.display()
        ));
    }
    parse_registry(&bytes, path)?;
    write_registry(path, bytes.as_bytes(), existing.as_deref()).map(RegistryAdd::Added)
}

fn normalize_provider_tables(document: &mut DocumentMut, path: &Path) -> Result<(), String> {
    let Some(providers) = document.get_mut("providers") else {
        document["providers"] = Item::ArrayOfTables(ArrayOfTables::new());
        return Ok(());
    };
    if providers.is_array_of_tables() {
        return Ok(());
    }
    let array = providers.as_array().ok_or_else(|| {
        format!(
            "provider registry {} `providers` must be an array of tables",
            path.display()
        )
    })?;
    let mut tables = ArrayOfTables::new();
    for entry in array.iter() {
        let inline = entry.as_inline_table().ok_or_else(|| {
            format!(
                "provider registry {} `providers` must contain only tables",
                path.display()
            )
        })?;
        tables.push(inline.clone().into_table());
    }
    *providers = Item::ArrayOfTables(tables);
    Ok(())
}

fn registry_lock(path: &Path) -> Result<BoundDirectoryLock, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    create_dir_all_durable(parent)?;
    validate_registry_directory(parent)?;
    #[cfg(unix)]
    let directory = bind_directory(parent, "provider registry directory")?;
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    #[cfg(unix)]
    let file: File = {
        use rustix::fs::{Mode, OFlags, openat};

        openat(
            &directory,
            lock_path.file_name().ok_or_else(|| {
                format!(
                    "provider registry lock {} has no file name",
                    lock_path.display()
                )
            })?,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| {
            format!(
                "opening provider registry lock {}: {error}",
                lock_path.display()
            )
        })?
        .into()
    };
    #[cfg(not(unix))]
    let file = {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        options.open(&lock_path).map_err(|error| {
            format!(
                "opening provider registry lock {}: {error}",
                lock_path.display()
            )
        })?
    };
    let metadata = file.metadata().map_err(|error| {
        format!(
            "inspecting provider registry lock {}: {error}",
            lock_path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "provider registry lock {} must be a regular file",
            lock_path.display()
        ));
    }
    #[cfg(unix)]
    {
        validate_owned_not_writable_by_others(&lock_path, &metadata, "provider registry lock")?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "securing provider registry lock {}: {error}",
                    lock_path.display()
                )
            })?;
        if !bound_directory_is_current(parent, &directory).unwrap_or(false) {
            return Err(format!(
                "provider registry directory {} changed while its lock was opened; retry",
                parent.display()
            ));
        }
    }
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|error| format!("locking provider registry {}: {error}", lock_path.display()))?;
    #[cfg(unix)]
    if !bound_directory_is_current(parent, &directory).unwrap_or(false) {
        return Err(format!(
            "provider registry directory {} changed while waiting for its lock; retry",
            parent.display()
        ));
    }
    Ok(BoundDirectoryLock {
        _lock: file,
        #[cfg(unix)]
        directory,
    })
}

fn write_registry(
    path: &Path,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    create_dir_all_durable(parent)?;
    validate_registry_directory(parent)?;
    #[cfg(unix)]
    let directory = bind_directory(parent, "provider registry directory")?;
    #[cfg(not(unix))]
    let permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("creating provider registry temporary file: {error}"))?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("setting provider registry permissions: {error}"))?;
    #[cfg(not(unix))]
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| format!("setting provider registry permissions: {error}"))?;
    }
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("writing provider registry: {error}"))?;
    #[cfg(unix)]
    if !bound_directory_is_current(parent, &directory).unwrap_or(false) {
        return Err(format!(
            "provider registry directory {} changed before publication; retry",
            parent.display()
        ));
    }
    let preserved = match expected {
        None => {
            temporary.persist_noclobber(path).map_err(|error| {
                format!(
                    "provider registry {} appeared while adding an entry; retry: {}",
                    path.display(),
                    error.error
                )
            })?;
            None
        }
        Some(expected) => Some(replace_registry_if_unchanged(path, temporary, expected)?),
    };
    #[cfg(unix)]
    if !bound_directory_is_current(parent, &directory).unwrap_or(false) {
        eprintln!(
            "warning: provider registry committed, but its directory {} changed during publication; inspect the registry and its preserved versions",
            parent.display()
        );
    }
    if let Err(error) = sync_directory(parent) {
        eprintln!(
            "warning: provider registry committed, but durability could not be confirmed: {error}"
        );
    }
    Ok(preserved)
}

fn create_dir_all_durable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    return create_private_directory_tree(path, "provider registry directory", true);
    #[cfg(not(unix))]
    fs::create_dir_all(path).map_err(|error| {
        format!(
            "creating provider registry directory {}: {error}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn validate_owned_not_writable_by_others(
    path: &Path,
    metadata: &fs::Metadata,
    what: &str,
) -> Result<(), String> {
    let effective_uid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != effective_uid {
        return Err(format!(
            "{what} {} is owned by uid {}, not the current uid {effective_uid}",
            path.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        let fix = chmod_fix(path, "go-w");
        return Err(format!(
            "{what} {} is writable by another user; fix: {fix}",
            path.display(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_registry_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "cannot inspect provider registry directory {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "provider registry directory {} is not a directory",
            path.display()
        ));
    }
    validate_owned_not_writable_by_others(path, &metadata, "provider registry directory")?;
    validate_secure_directory_chain(path, "provider registry directory")
}

#[cfg(not(unix))]
fn validate_registry_directory(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "provider registry directory {} is not a directory",
            path.display()
        ))
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "syncing provider registry directory {}: {error}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn replace_registry_if_unchanged(
    path: &Path,
    mut temporary: tempfile::NamedTempFile,
    expected: &str,
) -> Result<PathBuf, String> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    let temporary_path = temporary.path().to_path_buf();
    let candidate = read_registry_unchecked(&temporary_path)?
        .ok_or_else(|| "staged provider registry disappeared".to_string())?;
    let recovery = prepare_registry_recovery(
        path,
        &temporary_path,
        temporary.as_file(),
        expected,
        &candidate,
    )?;
    let (transaction, transaction_file) = write_registry_transaction(
        path,
        &temporary_path,
        expected,
        temporary.as_file(),
        &recovery,
    )?;
    // After the first exchange this pathname may hold somebody else's registry. Disable
    // automatic cleanup before publishing so no error, rollback race, or Drop path can unlink a
    // foreign inode.
    temporary.disable_cleanup(true);
    if let Err(error) = renameat_with(CWD, &temporary_path, CWD, path, RenameFlags::EXCHANGE) {
        let _ = temporary.keep();
        let mut maintenance_warnings = Vec::new();
        if let Err(error) = make_recovery_inspectable(&recovery) {
            maintenance_warnings.push(error);
        }
        if let Err(error) =
            archive_transaction_if_ours(path, &transaction, &transaction_file, &recovery)
        {
            maintenance_warnings.push(error);
        }
        if !maintenance_warnings.is_empty() {
            eprintln!(
                "warning: provider registry publication failed and recovery maintenance was incomplete: {}",
                maintenance_warnings.join("; ")
            );
        }
        return Err(format!(
            "conditionally replacing provider registry {} failed: {error}; the candidate and prior registry were preserved in {}",
            path.display(),
            recovery.directory.display()
        ));
    }

    // The exchange is the irreversible cross-release commit point: every released reader sees
    // either the complete old registry or this already-validated complete candidate. The marker
    // gives newer readers stronger fail-closed recovery, but correctness never depends on an older
    // pinned binary understanding it. Nothing after this point rolls the candidate back.
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    let mut post_commit_warnings = Vec::new();
    if let Err(error) = sync_directory(parent) {
        post_commit_warnings.push(error);
    }

    // Move whatever the exchange displaced into the recovery directory. The original inode was
    // hard-linked there before publication, so even a replacement of this mutable stage pathname
    // cannot destroy a late write through an already-open descriptor.
    let displaced_path = match fs::rename(&temporary_path, &recovery.exchange_stage) {
        Ok(()) => recovery.exchange_stage.as_path(),
        Err(error) => {
            post_commit_warnings.push(format!(
                "moving exchanged provider registry into {}: {error}; displaced version remains at {}",
                recovery.exchange_stage.display(),
                temporary_path.display()
            ));
            temporary_path.as_path()
        }
    };
    if let Err(error) = recovery.directory_file.sync_all() {
        post_commit_warnings.push(format!(
            "syncing provider registry recovery directory: {error}"
        ));
    }
    if let Err(error) = sync_directory(parent) {
        post_commit_warnings.push(error);
    }

    if !registry_file_matches(displaced_path, &recovery.original_file, expected) {
        post_commit_warnings.push(format!(
            "provider registry {} changed immediately before commit; the displaced version was preserved in {}",
            path.display(),
            recovery.directory.display()
        ));
    }
    if !registry_file_matches(path, temporary.as_file(), &candidate) {
        if let Err(error) = make_recovery_inspectable(&recovery) {
            post_commit_warnings.push(error);
        }
        if let Err(error) =
            archive_transaction_if_ours(path, &transaction, &transaction_file, &recovery)
        {
            post_commit_warnings.push(error);
        }
        emit_registry_commit_warnings(&post_commit_warnings);
        return Err(format!(
            "provider registry {} was replaced by another writer after this update committed; the candidate and prior registry were preserved in {}",
            path.display(),
            recovery.directory.display()
        ));
    }

    if let Err(error) = make_recovery_inspectable(&recovery) {
        post_commit_warnings.push(error);
    }
    if let Err(error) =
        archive_transaction_if_ours(path, &transaction, &transaction_file, &recovery)
    {
        post_commit_warnings.push(error);
    }
    // Rechecking afterward distinguishes a later non-cooperating replacement from publication.
    // The candidate copy remains recoverable either way and was never rolled back.
    if !registry_file_matches(path, temporary.as_file(), &candidate) {
        emit_registry_commit_warnings(&post_commit_warnings);
        return Err(format!(
            "provider registry {} changed after commit; the candidate and prior registry were preserved in {}",
            path.display(),
            recovery.directory.display()
        ));
    }
    if let Err(error) = ensure_no_registry_transaction(path) {
        post_commit_warnings.push(error);
    }
    emit_registry_commit_warnings(&post_commit_warnings);
    Ok(recovery.displaced_copy)
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn emit_registry_commit_warnings(warnings: &[String]) {
    if !warnings.is_empty() {
        eprintln!(
            "warning: provider registry committed, but post-commit maintenance was incomplete: {}",
            warnings.join("; ")
        );
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
struct RegistryRecovery {
    directory: PathBuf,
    directory_file: File,
    candidate_copy: PathBuf,
    displaced_copy: PathBuf,
    exchange_stage: PathBuf,
    archived_marker: PathBuf,
    original_file: File,
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn prepare_registry_recovery(
    path: &Path,
    stage: &Path,
    candidate_file: &File,
    expected: &str,
    candidate: &str,
) -> Result<RegistryRecovery, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    let original_file = open_registry(path)
        .map_err(|error| format!("opening provider registry before publication: {error}"))?;
    if !registry_file_matches(path, &original_file, expected) {
        return Err(format!(
            "provider registry {} changed before publication; retry",
            path.display()
        ));
    }

    let mut recovery_builder = tempfile::Builder::new();
    recovery_builder
        .prefix(".af-provider-recovery-")
        .permissions(fs::Permissions::from_mode(0o700));
    let recovery = recovery_builder
        .tempdir_in(parent)
        .map_err(|error| format!("creating provider registry recovery directory: {error}"))?;
    // Requesting 0700 prevents a permissive umask from exposing the directory. Repair owner
    // access immediately so restrictive umasks such as 0777 cannot break this or later updates.
    fs::set_permissions(recovery.path(), fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("securing provider registry recovery directory: {error}"))?;
    let directory_file = File::open(recovery.path())
        .map_err(|error| format!("opening provider registry recovery directory: {error}"))?;
    let candidate_copy = recovery.path().join("candidate");
    let displaced_copy = recovery.path().join("displaced");
    let exchange_stage = recovery.path().join("exchange-stage");
    let archived_marker = recovery.path().join("transaction-marker");
    let mut candidate_copy_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&candidate_copy)
        .map_err(|error| format!("creating candidate provider registry recovery copy: {error}"))?;
    candidate_copy_file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| candidate_copy_file.write_all(candidate.as_bytes()))
        .and_then(|()| candidate_copy_file.sync_all())
        .map_err(|error| format!("writing candidate provider registry recovery copy: {error}"))?;
    fs::hard_link(path, &displaced_copy)
        .map_err(|error| format!("linking prior provider registry for recovery: {error}"))?;
    if !registry_file_matches(stage, candidate_file, candidate)
        || !matches!(
            read_registry_unchecked(&candidate_copy),
            Ok(Some(current)) if current == candidate
        )
        || !registry_file_matches(&displaced_copy, &original_file, expected)
    {
        return Err(format!(
            "provider registry {} changed while preparing publication; retry",
            path.display()
        ));
    }
    original_file
        .sync_all()
        .map_err(|error| format!("syncing prior provider registry: {error}"))?;
    directory_file
        .sync_all()
        .map_err(|error| format!("syncing provider registry recovery directory: {error}"))?;
    sync_directory(parent)?;

    let directory = recovery.keep();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("securing provider registry recovery directory: {error}"))?;
    directory_file.sync_all().map_err(|error| {
        format!("syncing secured provider registry recovery directory: {error}")
    })?;
    sync_directory(parent)?;
    Ok(RegistryRecovery {
        directory,
        directory_file,
        candidate_copy,
        displaced_copy,
        exchange_stage,
        archived_marker,
        original_file,
    })
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn registry_file_matches(path: &Path, file: &File, expected: &str) -> bool {
    same_file(path, file).unwrap_or(false)
        && matches!(read_registry_unchecked(path), Ok(Some(current)) if current == expected)
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn make_recovery_inspectable(recovery: &RegistryRecovery) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Err(error) = fs::set_permissions(&recovery.directory, fs::Permissions::from_mode(0o700))
    {
        failures.push(format!(
            "making provider registry recovery directory inspectable: {error}"
        ));
    }
    if let Err(error) = recovery.directory_file.sync_all() {
        failures.push(format!(
            "syncing inspectable provider registry recovery directory: {error}"
        ));
    }
    if let Some(parent) = recovery.directory.parent() {
        if let Err(error) = sync_directory(parent) {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn archive_transaction_if_ours(
    path: &Path,
    transaction: &Path,
    transaction_file: &File,
    recovery: &RegistryRecovery,
) -> Result<(), String> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(
        CWD,
        transaction,
        CWD,
        &recovery.archived_marker,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        format!(
            "archiving provider registry transaction {}: {error}",
            transaction.display()
        )
    })?;
    let mut durability_warnings = Vec::new();
    if let Err(error) = recovery.directory_file.sync_all() {
        durability_warnings.push(format!(
            "syncing archived provider registry transaction: {error}"
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    if let Err(error) = sync_directory(parent) {
        durability_warnings.push(error);
    }
    if same_file(&recovery.archived_marker, transaction_file).unwrap_or(false) {
        if !durability_warnings.is_empty() {
            eprintln!(
                "warning: provider registry transaction committed, but durability could not be confirmed: {}",
                durability_warnings.join("; ")
            );
        }
        return Ok(());
    }

    // A non-cooperating writer replaced the marker between publication and archival. Preserve
    // its inode in recovery and restore a hard link beside the registry so readers stay fail
    // closed. If another marker appeared in the meantime, the path is already fail closed.
    match fs::hard_link(&recovery.archived_marker, transaction) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "provider registry transaction {} changed and restoring its marker failed: {error}; replacement preserved at {}",
                transaction.display(),
                recovery.archived_marker.display()
            ));
        }
    }
    sync_directory(parent)?;
    Err(format!(
        "provider registry transaction {} changed; replacement preserved at {} and registry remains fail closed",
        transaction.display(),
        recovery.archived_marker.display()
    ))
}

fn registry_transaction_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".transaction");
    PathBuf::from(name)
}

fn registry_transaction_exists(path: &Path) -> Result<bool, String> {
    let transaction = registry_transaction_path(path);
    match fs::symlink_metadata(&transaction) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect provider registry transaction {}: {error}",
            transaction.display()
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryTransactionMarker {
    version: u8,
    stage: String,
    recovery: String,
    candidate_copy: String,
    displaced_copy: String,
    exchange_stage: String,
    archived_marker: String,
    expected_sha256: String,
    candidate_sha256: String,
}

fn safe_relative_component(value: &str, field: &str) -> Result<(), String> {
    let path = Path::new(value);
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(format!(
            "provider registry transaction `{field}` must be one relative path component"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!(
            "provider registry transaction `{field}` is not a SHA-256 digest"
        ))
    }
}

#[cfg(unix)]
fn open_regular_file_at(
    directory: &File,
    name: &std::ffi::OsStr,
    display: &Path,
    label: &str,
) -> Result<File, String> {
    use rustix::fs::{Mode, OFlags, openat};

    let file: File = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "opening {label} {} without following links: {error}",
            display.display()
        )
    })?
    .into();
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspecting {label} {}: {error}", display.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_REGISTRY_BYTES {
        return Err(format!(
            "{label} {} must be a bounded regular file",
            display.display()
        ));
    }
    validate_owned_not_writable_by_others(display, &metadata, label)?;
    Ok(file)
}

#[cfg(unix)]
fn bounded_file_bytes(file: &File, display: &Path, label: &str) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::FileExt;

    let before = file
        .metadata()
        .map_err(|error| format!("inspecting {label} {}: {error}", display.display()))?;
    if !before.is_file() || before.len() > MAX_REGISTRY_BYTES {
        return Err(format!(
            "{label} {} must be a bounded regular file",
            display.display()
        ));
    }
    validate_owned_not_writable_by_others(display, &before, label)?;
    let mut bytes = vec![0; MAX_REGISTRY_BYTES as usize + 1];
    let mut offset = 0;
    loop {
        let read = file
            .read_at(&mut bytes[offset..], offset as u64)
            .map_err(|error| format!("reading {label} {}: {error}", display.display()))?;
        if read == 0 {
            break;
        }
        offset += read;
        if offset > MAX_REGISTRY_BYTES as usize {
            return Err(format!(
                "{label} {} exceeds {MAX_REGISTRY_BYTES} bytes",
                display.display()
            ));
        }
    }
    bytes.truncate(offset);
    let after = file
        .metadata()
        .map_err(|error| format!("re-inspecting {label} {}: {error}", display.display()))?;
    if before.dev() != after.dev() || before.ino() != after.ino() || after.len() != offset as u64 {
        return Err(format!(
            "{label} {} changed while it was read; retry",
            display.display()
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn registry_digest_from_file(file: &File, display: &Path, label: &str) -> Result<String, String> {
    let bytes = bounded_file_bytes(file, display, label)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        format!(
            "provider registry recovery {label} {} is not UTF-8",
            display.display()
        )
    })?;
    parse_registry(text, display).map_err(|error| {
        format!(
            "provider registry recovery {label} {} is invalid: {error}",
            display.display()
        )
    })?;
    Ok(review_core::hex::encode(&Sha256::digest(bytes)))
}

#[cfg(unix)]
fn same_file_at(directory: &File, name: &std::ffi::OsStr, expected: &File) -> bool {
    let Ok(current) = open_regular_file_at(directory, name, Path::new(name), "secured file") else {
        return false;
    };
    let Ok(current) = current.metadata() else {
        return false;
    };
    let Ok(expected) = expected.metadata() else {
        return false;
    };
    current.dev() == expected.dev() && current.ino() == expected.ino()
}

#[cfg(unix)]
fn same_directory_at(directory: &File, name: &std::ffi::OsStr, expected: &File) -> bool {
    use rustix::fs::{Mode, OFlags, openat};

    let Ok(current): Result<File, _> = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(Into::into) else {
        return false;
    };
    let Ok(current) = current.metadata() else {
        return false;
    };
    let Ok(expected) = expected.metadata() else {
        return false;
    };
    current.dev() == expected.dev() && current.ino() == expected.ino()
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn recover_registry(path: &Path) -> Result<(), String> {
    recover_registry_with_hook(path, || {})
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn recover_registry_with_hook(path: &Path, before_archive: impl FnOnce()) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags, RenameFlags, openat, renameat_with};

    let transaction = registry_transaction_path(path);
    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    let parent_directory = bind_directory(parent, "provider registry directory")?;
    let transaction_name = transaction.file_name().ok_or_else(|| {
        format!(
            "provider registry transaction {} has no file name",
            transaction.display()
        )
    })?;
    if !registry_transaction_exists(path)? {
        println!(
            "provider registry {} has no unfinished publication",
            path.display()
        );
        return Ok(());
    }
    let marker_file = open_regular_file_at(
        &parent_directory,
        transaction_name,
        &transaction,
        "provider registry transaction",
    )?;
    let marker_bytes =
        bounded_file_bytes(&marker_file, &transaction, "provider registry transaction")?;
    let marker_text = std::str::from_utf8(&marker_bytes).map_err(|_| {
        format!(
            "provider registry transaction {} is not UTF-8",
            transaction.display()
        )
    })?;
    let marker: RegistryTransactionMarker = toml::from_str(marker_text).map_err(|error| {
        format!(
            "provider registry transaction {} is invalid: {error}",
            transaction.display()
        )
    })?;
    if marker.version != 1 {
        return Err(format!(
            "provider registry transaction {} has unsupported version {}",
            transaction.display(),
            marker.version
        ));
    }
    safe_relative_component(&marker.stage, "stage")?;
    safe_relative_component(&marker.recovery, "recovery")?;
    if !marker.recovery.starts_with(".af-provider-recovery-") {
        return Err("provider registry transaction names an unexpected recovery directory".into());
    }
    for (field, value, leaf) in [
        ("candidate_copy", &marker.candidate_copy, "candidate"),
        ("displaced_copy", &marker.displaced_copy, "displaced"),
        ("exchange_stage", &marker.exchange_stage, "exchange-stage"),
        (
            "archived_marker",
            &marker.archived_marker,
            "transaction-marker",
        ),
    ] {
        let expected = Path::new(&marker.recovery).join(leaf);
        if Path::new(value) != expected {
            return Err(format!(
                "provider registry transaction `{field}` does not name its recorded recovery child"
            ));
        }
    }
    validate_sha256(&marker.expected_sha256, "expected_sha256")?;
    validate_sha256(&marker.candidate_sha256, "candidate_sha256")?;

    let recovery = parent.join(&marker.recovery);
    let recovery_name = std::ffi::OsStr::new(&marker.recovery);
    let recovery_directory: File = openat(
        &parent_directory,
        recovery_name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "opening provider registry recovery directory {} without following links: {error}",
            recovery.display()
        )
    })?
    .into();
    let recovery_metadata = recovery_directory.metadata().map_err(|error| {
        format!(
            "inspecting provider registry recovery directory {}: {error}",
            recovery.display()
        )
    })?;
    if !recovery_metadata.is_dir() {
        return Err(format!(
            "provider registry recovery path {} is not a directory",
            recovery.display()
        ));
    }
    validate_owned_not_writable_by_others(
        &recovery,
        &recovery_metadata,
        "provider registry recovery directory",
    )?;

    let live_name = path
        .file_name()
        .ok_or_else(|| format!("provider registry {} has no file name", path.display()))?;
    let live_file = open_regular_file_at(
        &parent_directory,
        live_name,
        path,
        "provider registry recovery live file",
    )?;
    let candidate = parent.join(&marker.candidate_copy);
    let displaced = parent.join(&marker.displaced_copy);
    let candidate_file = open_regular_file_at(
        &recovery_directory,
        std::ffi::OsStr::new("candidate"),
        &candidate,
        "provider registry recovery candidate copy",
    )?;
    let displaced_file = open_regular_file_at(
        &recovery_directory,
        std::ffi::OsStr::new("displaced"),
        &displaced,
        "provider registry recovery prior copy",
    )?;
    let live_digest = registry_digest_from_file(&live_file, path, "live file")?;
    let candidate_digest =
        registry_digest_from_file(&candidate_file, &candidate, "candidate copy")?;
    let displaced_digest = registry_digest_from_file(&displaced_file, &displaced, "prior copy")?;
    if candidate_digest != marker.candidate_sha256 || displaced_digest != marker.expected_sha256 {
        return Err(format!(
            "provider registry recovery files in {} do not match the transaction hashes; marker retained",
            recovery.display()
        ));
    }
    let state = if live_digest == marker.candidate_sha256 {
        "the committed candidate is live"
    } else if live_digest == marker.expected_sha256 {
        "publication did not commit and the prior registry is live"
    } else {
        return Err(format!(
            "provider registry {} matches neither transaction hash; marker retained",
            path.display()
        ));
    };
    before_archive();
    let final_live_digest = registry_digest_from_file(&live_file, path, "live file")?;
    let final_candidate_digest =
        registry_digest_from_file(&candidate_file, &candidate, "candidate copy")?;
    let final_displaced_digest =
        registry_digest_from_file(&displaced_file, &displaced, "prior copy")?;
    let final_marker_bytes =
        bounded_file_bytes(&marker_file, &transaction, "provider registry transaction")?;
    if !bound_directory_is_current(parent, &parent_directory).unwrap_or(false)
        || !same_directory_at(&parent_directory, recovery_name, &recovery_directory)
        || !same_file_at(&parent_directory, transaction_name, &marker_file)
        || !same_file_at(&parent_directory, live_name, &live_file)
        || !same_file_at(
            &recovery_directory,
            std::ffi::OsStr::new("candidate"),
            &candidate_file,
        )
        || !same_file_at(
            &recovery_directory,
            std::ffi::OsStr::new("displaced"),
            &displaced_file,
        )
        || final_live_digest != live_digest
        || final_candidate_digest != candidate_digest
        || final_displaced_digest != displaced_digest
        || final_marker_bytes != marker_bytes
    {
        return Err("provider registry recovery state changed while validating it; retry".into());
    }
    let archived = parent.join(&marker.archived_marker);
    renameat_with(
        &parent_directory,
        transaction_name,
        &recovery_directory,
        std::ffi::OsStr::new("transaction-marker"),
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        format!(
            "archiving validated provider registry transaction {}: {error}",
            transaction.display()
        )
    })?;
    if !same_file_at(
        &recovery_directory,
        std::ffi::OsStr::new("transaction-marker"),
        &marker_file,
    ) {
        return Err(format!(
            "archived provider registry transaction {} changed identity; inspect {}",
            archived.display(),
            recovery.display()
        ));
    }
    recovery_directory
        .sync_all()
        .map_err(|error| format!("syncing provider registry recovery directory: {error}"))?;
    parent_directory.sync_all().map_err(|error| {
        format!(
            "syncing provider registry directory {}: {error}",
            parent.display()
        )
    })?;
    println!(
        "provider registry recovery complete: {state}; candidate and prior versions remain in {}",
        recovery.display()
    );
    Ok(())
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
)))]
fn recover_registry(path: &Path) -> Result<(), String> {
    Err(format!(
        "provider registry recovery is not supported on this platform for {}",
        path.display()
    ))
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn write_registry_transaction(
    path: &Path,
    stage: &Path,
    expected: &str,
    candidate: &File,
    recovery: &RegistryRecovery,
) -> Result<(PathBuf, File), String> {
    use std::io::Seek;

    let parent = path
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", path.display()))?;
    let stage_name = stage
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "provider registry stage {} has no UTF-8 file name",
                stage.display()
            )
        })?;
    let recovery_name = recovery
        .directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "provider registry recovery directory {} has no UTF-8 file name",
                recovery.directory.display()
            )
        })?;
    let recovery_path = |item: &Path| {
        item.strip_prefix(parent)
            .ok()
            .and_then(Path::to_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "provider registry recovery path {} is not a UTF-8 child of {}",
                    item.display(),
                    parent.display()
                )
            })
    };
    let candidate_copy = recovery_path(&recovery.candidate_copy)?;
    let displaced_copy = recovery_path(&recovery.displaced_copy)?;
    let exchange_stage = recovery_path(&recovery.exchange_stage)?;
    let archived_marker = recovery_path(&recovery.archived_marker)?;
    let mut candidate_bytes = Vec::new();
    candidate
        .try_clone()
        .and_then(|mut file| {
            file.rewind()?;
            file.read_to_end(&mut candidate_bytes)
        })
        .map_err(|error| format!("reading staged provider registry: {error}"))?;
    let marker = format!(
        "version = 1\nstage = {:?}\nrecovery = {:?}\ncandidate_copy = {:?}\ndisplaced_copy = {:?}\nexchange_stage = {:?}\narchived_marker = {:?}\nexpected_sha256 = {:?}\ncandidate_sha256 = {:?}\n",
        stage_name,
        recovery_name,
        candidate_copy,
        displaced_copy,
        exchange_stage,
        archived_marker,
        review_core::hex::encode(&Sha256::digest(expected.as_bytes())),
        review_core::hex::encode(&Sha256::digest(&candidate_bytes))
    );
    let transaction = registry_transaction_path(path);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("creating provider transaction marker: {error}"))?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("setting provider transaction permissions: {error}"))?;
    temporary
        .write_all(marker.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("writing provider transaction marker: {error}"))?;
    let marker_file = temporary.persist_noclobber(&transaction).map_err(|error| {
        format!(
            "provider registry {} already has an unfinished transaction at {}: {}",
            path.display(),
            transaction.display(),
            error.error
        )
    })?;
    sync_directory(parent)?;
    Ok((transaction, marker_file))
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn same_file(path: &Path, file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let path = fs::symlink_metadata(path)?;
    let file = file.metadata()?;
    Ok(path.dev() == file.dev() && path.ino() == file.ino())
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
)))]
fn replace_registry_if_unchanged(
    path: &Path,
    _temporary: tempfile::NamedTempFile,
    _expected: &str,
) -> Result<PathBuf, String> {
    Err(format!(
        "provider registry {} already exists, but this platform has no conditional replacement primitive",
        path.display()
    ))
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

fn load_specs() -> (Vec<ProviderSpec>, Option<String>) {
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

    (specs, warning)
}

fn registry_path() -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("AF_PROVIDERS_FILE") {
        if path.is_empty() {
            return Err("AF_PROVIDERS_FILE is empty".to_string());
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("AF_PROVIDERS_FILE must be absolute".to_string());
        }
        return Ok(Some(path));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        if !config.is_empty() {
            let config = PathBuf::from(config);
            if !config.is_absolute() {
                return Err("XDG_CONFIG_HOME must be absolute".to_string());
            }
            return Ok(Some(config_file(&config, "providers.toml")));
        }
    }
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| config_file(&home.join(".config"), "providers.toml"));
    if path.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err("HOME must be absolute to locate the provider registry".to_string());
    }
    Ok(path)
}

fn read_registry(path: &Path) -> Result<Option<String>, String> {
    read_registry_with_hooks(path, || {}, || {})
}

fn read_registry_with_hooks(
    path: &Path,
    before_read: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<Option<String>, String> {
    let configured_parent = path.parent().filter(|parent| parent.exists());
    let configured_parent_resolved = configured_parent
        .map(|parent| {
            fs::canonicalize(parent).map_err(|error| {
                format!(
                    "cannot resolve provider registry directory {}: {error}",
                    parent.display()
                )
            })
        })
        .transpose()?;
    if let Some(parent) = configured_parent_resolved.as_deref() {
        validate_registry_directory(parent)?;
    }
    #[cfg(unix)]
    let configured_directory = configured_parent_resolved
        .as_deref()
        .map(|parent| bind_directory(parent, "provider registry directory"))
        .transpose()?;
    // Check beside the configured pathname even when the registry is absent. A writer may have
    // created its marker before publishing the first file, or the pathname may have been swapped
    // between a regular file and a symlink.
    ensure_no_registry_transaction(path)?;
    let Some(resolved) = resolve_registry(path)? else {
        ensure_no_registry_transaction(path)?;
        return Ok(None);
    };
    let resolved_parent = resolved
        .parent()
        .ok_or_else(|| format!("provider registry {} has no parent", resolved.display()))?;
    validate_registry_directory(resolved_parent)?;
    #[cfg(unix)]
    let resolved_directory = bind_directory(resolved_parent, "provider registry directory")?;
    ensure_registry_transactions_clear(path, &resolved)?;
    before_read();
    let registry = read_registry_resolved(path, &resolved)?;
    after_read();
    // A writer may have created the marker after the first check and exchanged the candidate
    // while this read was in flight. Rechecking gives current readers the stronger recovery fence;
    // older readers still see only the complete registry committed by the atomic exchange.
    let current_resolved = resolve_registry(path)?;
    if let Some(current_resolved) = current_resolved.as_deref() {
        ensure_registry_transactions_clear(path, current_resolved)?;
    } else {
        ensure_no_registry_transaction(path)?;
    }
    if current_resolved.as_deref() != Some(resolved.as_path()) {
        return Err(format!(
            "provider registry target changed while reading {}; retry",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        let configured_alias_changed = configured_parent
            .zip(configured_parent_resolved.as_deref())
            .is_some_and(|(parent, expected)| {
                fs::canonicalize(parent).as_deref().ok() != Some(expected)
            });
        if configured_alias_changed
            || configured_parent_resolved
                .as_deref()
                .zip(configured_directory.as_ref())
                .is_some_and(|(parent, directory)| {
                    !bound_directory_is_current(parent, directory).unwrap_or(false)
                })
            || !bound_directory_is_current(resolved_parent, &resolved_directory).unwrap_or(false)
        {
            return Err(format!(
                "provider registry directory changed while reading {}; retry",
                path.display()
            ));
        }
    }
    Ok(Some(registry))
}

fn ensure_registry_transactions_clear(configured: &Path, resolved: &Path) -> Result<(), String> {
    ensure_no_registry_transaction(configured)?;
    if configured != resolved {
        ensure_no_registry_transaction(resolved)?;
    }
    Ok(())
}

fn resolve_registry(path: &Path) -> Result<Option<PathBuf>, String> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot resolve provider registry {}: {error}",
            path.display()
        )),
    }
}

fn ensure_no_registry_transaction(path: &Path) -> Result<(), String> {
    let transaction = registry_transaction_path(path);
    match fs::symlink_metadata(&transaction) {
        Ok(_) => {
            return Err(format!(
                "provider registry {} has an unfinished publication transaction at {}; registry reads fail closed — fix: af provider recover",
                path.display(),
                transaction.display()
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect provider registry transaction {}: {error}",
                transaction.display()
            ));
        }
    }
    Ok(())
}

fn read_registry_unchecked(path: &Path) -> Result<Option<String>, String> {
    let Some(resolved) = resolve_registry(path)? else {
        return Ok(None);
    };
    read_registry_resolved(path, &resolved).map(Some)
}

fn read_registry_resolved(path: &Path, resolved: &Path) -> Result<String, String> {
    let file = open_registry(resolved)
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
    #[cfg(unix)]
    validate_owned_not_writable_by_others(path, &metadata, "provider registry")?;
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
        let configured_home = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        defaults.push(ProviderSpec {
            id: "claude-ambient".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: canonical_if_present(
                configured_home
                    .clone()
                    .or_else(|| home.as_ref().map(|home| home.join(".claude"))),
            ),
            explicit_selector: configured_home.is_some(),
            registry_declared: false,
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
            registry_declared: false,
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
        let unresolved = fs::symlink_metadata(&auth_dir).map_err(|error| {
            format!(
                "provider `{id}` auth_dir {} cannot be inspected: {error}",
                auth_dir.display()
            )
        })?;
        if unresolved.file_type().is_symlink() {
            return Err(format!(
                "provider `{id}` auth_dir {} must not be a symlink",
                auth_dir.display()
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
            registry_declared: true,
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

fn probe_provider(spec: ProviderSpec) -> ProviderStatus {
    // Status probes run to completion; only Task identity checks cancel these shared probes.
    let cancelled = &AtomicBool::new(false);
    if spec
        .auth_dir
        .as_ref()
        .is_some_and(|path| spec.explicit_selector && !path.is_absolute())
    {
        return unavailable_status(&spec, "provider auth selector must be absolute");
    }
    if spec.explicit_selector
        && let Some(auth_dir) = spec.auth_dir.as_deref()
        && let Err(error) = validate_private_auth_directory(auth_dir)
    {
        return unavailable_status(&spec, &error);
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
    if status == "not authenticated" {
        detail = logged_out_detail(&spec);
    }
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
        // Keep this sequential: only a fresh first-party subscription result authorizes opening
        // `/usage`. Speculatively starting the interactive probe would touch unsupported API-key
        // and third-party contexts merely to hide one provider-process startup.
        match cached_claude_weekly_limits(&program, &spec, &probe_path, cancelled) {
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

fn unavailable_status(spec: &ProviderSpec, detail: &str) -> ProviderStatus {
    ProviderStatus {
        id: spec.id.clone(),
        kind: spec.kind.name().to_string(),
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

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct ClaudeUsageCacheKey {
    program: PathBuf,
    auth_dir: Option<PathBuf>,
}

struct CachedClaudeUsage {
    captured_at: Instant,
    limits: Vec<ProviderLimit>,
}

struct ClaudeReaderSlot;

impl ClaudeReaderSlot {
    fn acquire() -> Result<Self, String> {
        CLAUDE_READER_SLOTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |slots| {
                (slots < MAX_CLAUDE_READER_SLOTS).then_some(slots + 1)
            })
            .map(|_| Self)
            .map_err(|_| "Claude usage terminal is still held by earlier probes".to_string())
    }
}

impl Drop for ClaudeReaderSlot {
    fn drop(&mut self) {
        CLAUDE_READER_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
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
fn cached_claude_weekly_limits(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<Vec<ProviderLimit>, String> {
    let key = ClaudeUsageCacheKey {
        program: program.to_path_buf(),
        auth_dir: spec
            .explicit_selector
            .then(|| spec.auth_dir.clone())
            .flatten(),
    };
    let cache = CLAUDE_USAGE_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(&key)
        && cached.captured_at.elapsed() < CLAUDE_USAGE_CACHE_TTL
    {
        return Ok(cached.limits.clone());
    }
    let limits = probe_claude_weekly_limits(program, spec, probe_path, cancelled)?;
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|_, cached| cached.captured_at.elapsed() < CLAUDE_USAGE_CACHE_TTL);
        if cache.len() < MAX_PROVIDERS || cache.contains_key(&key) {
            cache.insert(
                key,
                CachedClaudeUsage {
                    captured_at: Instant::now(),
                    limits: limits.clone(),
                },
            );
        }
    }
    Ok(limits)
}

#[cfg(not(unix))]
fn cached_claude_weekly_limits(
    _program: &Path,
    _spec: &ProviderSpec,
    _probe_path: &std::ffi::OsStr,
    _cancelled: &AtomicBool,
) -> Result<Vec<ProviderLimit>, String> {
    Err("Claude usage probes require a Unix pseudo-terminal".to_string())
}

#[cfg(unix)]
fn probe_claude_weekly_limits(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) -> Result<Vec<ProviderLimit>, String> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};

    let reader_slot = ClaudeReaderSlot::acquire()?;
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

    // Backpressure bounds unread PTY data even if the renderer outpaces the inventory worker.
    let (sender, receiver) = sync_channel(1);
    let (recycle_sender, recycle_receiver) = sync_channel(1);
    let (reader_done_sender, reader_done_receiver) = sync_channel(1);
    let reader_thread = thread::spawn(move || {
        let _reader_slot = reader_slot;
        let mut chunk = vec![0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if sender.send(Ok((chunk, count))).is_err() {
                        break;
                    }
                    chunk = match recycle_receiver.recv() {
                        Ok(chunk) => chunk,
                        Err(_) => break,
                    };
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
        let _ = reader_done_sender.send(());
    });

    let deadline = Instant::now() + CLAUDE_USAGE_PROBE_TIMEOUT;
    let mut next_parse = Instant::now();
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT * 2);
    let mut dirty = false;
    let result = loop {
        let quiet = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok((chunk, count))) => {
                append_bounded_window(&mut captured, &chunk[..count], MAX_PROBE_OUTPUT);
                let _ = recycle_sender.send(chunk);
                dirty = true;
                false
            }
            Ok(Err(error)) => {
                break Err(format!("cannot read Claude usage terminal: {error}"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err("Claude usage terminal closed without a weekly limit".to_string());
            }
            Err(RecvTimeoutError::Timeout) => true,
        };
        if dirty && (quiet || Instant::now() >= next_parse) {
            if let Ok(usage) = parse_claude_weekly_limits(bounded_tail(&captured, MAX_PROBE_OUTPUT))
                && usage.complete
            {
                break Ok(usage.limits);
            }
            dirty = false;
            next_parse = Instant::now() + Duration::from_millis(250);
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
    drop(receiver);
    drop(recycle_sender);
    // A descendant that escaped the process group may retain the PTY slave. Never let that turn
    // cancellation or shutdown into an unbounded join.
    match reader_done_receiver.recv_timeout(Duration::from_millis(250)) {
        Ok(()) => {
            let _ = reader_thread.join();
        }
        Err(_) if reader_done_receiver.try_recv().is_ok() => {
            let _ = reader_thread.join();
        }
        Err(_) => {}
    }
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
    let screen = compact_terminal_text(output);
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

fn compact_terminal_text(output: &[u8]) -> String {
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
            byte if byte.is_ascii_whitespace() => index += 1,
            byte if byte >= 0x20 => {
                clean.push(byte);
                index += 1;
            }
            _ => index += 1,
        }
    }
    match String::from_utf8(clean) {
        Ok(clean) => clean,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

fn append_bounded_window(buffer: &mut Vec<u8>, chunk: &[u8], window: usize) {
    if chunk.len() >= window {
        buffer.clear();
        buffer.extend_from_slice(&chunk[chunk.len() - window..]);
        return;
    }
    if buffer.len().saturating_add(chunk.len()) > window.saturating_mul(2) {
        let keep_from = buffer.len().saturating_sub(window);
        buffer.copy_within(keep_from.., 0);
        buffer.truncate(buffer.len() - keep_from);
    }
    buffer.extend_from_slice(chunk);
}

fn bounded_tail(buffer: &[u8], window: usize) -> &[u8] {
    &buffer[buffer.len().saturating_sub(window)..]
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
    parse_codex_subscription_response(&probe_codex_request(
        program,
        spec,
        probe_path,
        cancelled,
        &serde_json::json!({"method":"account/rateLimits/read","id":2}),
    )?)
}

#[cfg(unix)]
fn probe_codex_request(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    probe_codex_request_before(program, spec, probe_path, cancelled, request, None)
}

#[cfg(unix)]
fn probe_codex_request_before(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    request: &serde_json::Value,
    attempt_deadline: Option<Instant>,
) -> Result<serde_json::Value, String> {
    check_task_probe_control(attempt_deadline, cancelled)?;
    let mut command = Command::new(program);
    command.args(["app-server", "--stdio"]);
    configure_probe_environment(&mut command, spec, probe_path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = review_process::spawn(&mut command)
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
    let deadline = attempt_deadline.map_or(deadline, |limit| deadline.min(limit));
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
                .and_then(|()| writeln!(stdin, "{request}"))
                .and_then(|()| stdin.flush())
            {
                break Err(format!("cannot request Codex subscription status: {error}"));
            }
            requested_limits = true;
        }
        if requested_limits && let Some(response) = response_for_id(&captured, 2) {
            break Ok(response);
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

pub fn operation_id_for(
    provider_id: &str,
    node_id: &str,
    reviewer: &ReviewerCommand,
    authority: &RoundAuthority,
) -> Result<String, String> {
    let spec = configured_spec(provider_id)?;
    operation_identity(&spec, node_id, reviewer, authority).map(|(_, operation_id)| operation_id)
}

fn configured_spec(provider_id: &str) -> Result<ProviderSpec, String> {
    let (specs, warning) = load_specs();
    let spec = specs
        .into_iter()
        .find(|spec| spec.id == provider_id && spec.registry_declared)
        .ok_or_else(|| {
            warning.unwrap_or_else(|| {
                format!(
                    "provider `{provider_id}` is not an explicit entry in the machine-local registry"
                )
            })
        })?;
    let auth_dir = spec
        .auth_dir
        .as_deref()
        .ok_or_else(|| format!("provider `{provider_id}` has no explicit auth directory"))?;
    validate_private_auth_directory(auth_dir)
        .map_err(|error| format!("provider `{provider_id}` has an unsafe auth context: {error}"))?;
    Ok(spec)
}

fn operation_identity(
    spec: &ProviderSpec,
    node_id: &str,
    reviewer: &ReviewerCommand,
    authority: &RoundAuthority,
) -> Result<(String, String), String> {
    let reviewer_kind = Path::new(&reviewer.program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if reviewer_kind != spec.kind.name() {
        return Err(format!(
            "node `{node_id}` runs `{reviewer_kind}` but provider `{}` is {}",
            spec.id,
            spec.kind.name()
        ));
    }
    let argv = reviewer
        .resolve()
        .map_err(|error| format!("node `{node_id}` has invalid runner arguments: {error}"))?;
    let capability_id = digest_parts(
        std::iter::once(reviewer.program.as_str()).chain(argv.iter().map(String::as_str)),
    );
    let auth_dir = spec
        .auth_dir
        .as_deref()
        .and_then(Path::to_str)
        .ok_or_else(|| format!("provider `{}` auth directory must be UTF-8", spec.id))?;
    let auth_context_id = digest_parts([auth_dir]);
    let operation_id = short_id(&[
        authority.round_event_id(),
        &spec.id,
        node_id,
        &capability_id,
        &auth_context_id,
    ]);
    Ok((capability_id, operation_id))
}

pub fn admit(
    provider_id: &str,
    request: AdmissionRequest<'_>,
) -> Result<ProviderAdmission, String> {
    let AdmissionRequest {
        node_id,
        reviewer,
        state_dir,
        run_id,
        authority,
        cas,
        store,
        resumes,
        budget,
        structural_probes,
    } = request;
    let spec = configured_spec(provider_id)?;
    let auth_dir = spec
        .auth_dir
        .clone()
        .ok_or_else(|| format!("provider `{provider_id}` has no explicit auth directory"))?;
    let (capability_id, operation_id) = operation_identity(&spec, node_id, reviewer, authority)?;
    let identity = OperationIdentity {
        operation_id: &operation_id,
        spec: &spec,
        capability_id: &capability_id,
        node_id,
        authority,
    };
    let mut history = store
        .provider_operation_transitions(run_id, &operation_id)
        .map_err(|error| error.to_string())?;

    if history
        .last()
        .is_some_and(|transition| transition.state == ProviderOperationStateV1::Done)
    {
        if resumes.remove(&operation_id).is_some() {
            return Err(format!(
                "--resume-provider `{operation_id}` is stale; the provider operation is already done"
            ));
        }
        return Ok(ProviderAdmission { auth_dir });
    }

    let mut epoch = 1_u64;
    let mut attempt = 1_u32;
    let mut resumed = false;
    if let Some(previous) = history.last().cloned() {
        epoch = previous.operation_epoch;
        attempt = previous.attempt.unwrap_or(0);
        match previous.state {
            ProviderOperationStateV1::WaitingForHuman => {
                let supplied = resumes.remove(&operation_id).ok_or_else(|| {
                    continuation_error(&spec, &operation_id, previous.operation_epoch)
                })?;
                if supplied != previous.operation_epoch || !previous.retry_permitted {
                    return Err(format!(
                        "stale or superseded provider continuation `{operation_id}:{supplied}`"
                    ));
                }
                epoch = previous
                    .operation_epoch
                    .checked_add(1)
                    .ok_or_else(|| "provider operation epoch overflow".to_string())?;
                attempt = previous
                    .attempt
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| "provider operation attempt overflow".to_string())?;
                let attempt_id =
                    short_id(&[&operation_id, &epoch.to_string(), &attempt.to_string()]);
                let resumed_transition = transition(
                    &identity,
                    epoch,
                    ProviderOperationStateV1::Resumed,
                    Some(attempt),
                    Some(attempt_id),
                );
                let mut resumed_transition = resumed_transition;
                resumed_transition.continuation_handle = previous.continuation_handle.clone();
                append_transition(store, cas, run_id, authority, resumed_transition.clone())?;
                history.push(resumed_transition);
                resumed = true;
            }
            ProviderOperationStateV1::Failed => {
                if !previous.retry_permitted {
                    return Err(format!(
                        "provider operation `{operation_id}` is terminal for this Round: {:?}; next action: {:?}; circuit_open={}; rerun with --restart-round after correction",
                        previous.failure_class, previous.next_action, previous.circuit_open
                    ));
                }
                let supplied = resumes.remove(&operation_id).ok_or_else(|| {
                    format!(
                        "provider operation `{operation_id}` failed but permits one explicit retry; rerun with --resume-provider {operation_id}:{}",
                        previous.operation_epoch
                    )
                })?;
                if supplied != previous.operation_epoch {
                    return Err(format!(
                        "stale or superseded provider continuation `{operation_id}:{supplied}`"
                    ));
                }
                epoch = previous
                    .operation_epoch
                    .checked_add(1)
                    .ok_or_else(|| "provider operation epoch overflow".to_string())?;
                attempt = previous
                    .attempt
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| "provider operation attempt overflow".to_string())?;
                let attempt_id =
                    short_id(&[&operation_id, &epoch.to_string(), &attempt.to_string()]);
                let mut resumed_transition = transition(
                    &identity,
                    epoch,
                    ProviderOperationStateV1::Resumed,
                    Some(attempt),
                    Some(attempt_id),
                );
                resumed_transition.continuation_handle = previous.continuation_handle.clone();
                append_transition(store, cas, run_id, authority, resumed_transition.clone())?;
                history.push(resumed_transition);
                resumed = true;
            }
            ProviderOperationStateV1::Running if previous.failure_class.is_none() => {
                let failure = ProviderFailure::new(
                    ProviderFailureClassV1::UnknownProviderFailure,
                    "abandoned_running_operation",
                    true,
                    ProviderNextActionV1::RetryExplicitly,
                    SMOKE_RESERVATION,
                );
                let failed = failed_transition(
                    &previous,
                    failure,
                    SMOKE_RESERVATION,
                    previous.elapsed_ms,
                    false,
                    previous.attempt == Some(1),
                );
                append_transition(store, cas, run_id, authority, failed)?;
                return Err(format!(
                    "provider operation `{operation_id}` was abandoned and charged; resume explicitly with --resume-provider {operation_id}:{epoch}"
                ));
            }
            ProviderOperationStateV1::Running => {
                if previous.retry_permitted && previous.attempt == Some(1) {
                    attempt = 2;
                } else {
                    let failed = failed_transition_from_recorded(&previous);
                    append_transition(store, cas, run_id, authority, failed)?;
                    return Err(format!(
                        "provider operation `{operation_id}` failed with its retry exhausted"
                    ));
                }
            }
            ProviderOperationStateV1::Resumed => {
                resumed = true;
            }
            ProviderOperationStateV1::Done => unreachable!(),
        }
    } else if let Some(supplied) = resumes.remove(&operation_id) {
        return Err(format!(
            "stale provider continuation `{operation_id}:{supplied}`; the operation has not started"
        ));
    }

    loop {
        let reservation = budget
            .reserve(&[Scope::Run], SMOKE_RESERVATION)
            .map_err(|error| format!("provider operation `{operation_id}` refused: {error}"))?;
        let attempt_id = short_id(&[&operation_id, &epoch.to_string(), &attempt.to_string()]);
        let running = transition(
            &identity,
            epoch,
            ProviderOperationStateV1::Running,
            Some(attempt),
            Some(attempt_id),
        );
        if let Err(error) = append_transition(store, cas, run_id, authority, running.clone()) {
            budget.release(&reservation);
            return Err(error);
        }
        history.push(running.clone());

        let started = Instant::now();
        match perform_preflight(&spec, reviewer, state_dir, structural_probes) {
            Ok(charged_tokens) => {
                let mut done = running;
                done.state = ProviderOperationStateV1::Done;
                done.charged_tokens = charged_tokens;
                done.elapsed_ms = elapsed_ms(started);
                append_transition(store, cas, run_id, authority, done)?;
                settle_provider_budget(budget, &reservation, charged_tokens);
                return Ok(ProviderAdmission { auth_dir });
            }
            Err(failure) => {
                let elapsed = elapsed_ms(started);
                let repeated = history
                    .iter()
                    .filter(|transition| {
                        transition.failure_fingerprint.as_deref()
                            == Some(failure.fingerprint.as_str())
                    })
                    .count()
                    >= 1;
                if failure.class == ProviderFailureClassV1::InvalidOrExpiredAuthentication
                    || failure.class == ProviderFailureClassV1::InteractiveLoginRequired
                {
                    if !resumed && !repeated {
                        let mut waiting = running;
                        waiting.state = ProviderOperationStateV1::WaitingForHuman;
                        waiting.failure_class = Some(failure.class);
                        waiting.failure_fingerprint = Some(failure.fingerprint);
                        waiting.continuation_handle = Some(short_id(&[
                            &operation_id,
                            &epoch.to_string(),
                            "interactive-login",
                        ]));
                        waiting.charged_tokens = failure.charged_tokens;
                        waiting.elapsed_ms = elapsed;
                        waiting.retry_permitted = true;
                        waiting.next_action = Some(ProviderNextActionV1::CompleteInteractiveLogin);
                        append_transition(store, cas, run_id, authority, waiting)?;
                        settle_provider_budget(budget, &reservation, failure.charged_tokens);
                        return Err(continuation_error(&spec, &operation_id, epoch));
                    }
                } else if failure.retryable && attempt == 1 && !repeated {
                    let mut recorded = running;
                    recorded.failure_class = Some(failure.class);
                    recorded.failure_fingerprint = Some(failure.fingerprint);
                    recorded.charged_tokens = failure.charged_tokens;
                    recorded.elapsed_ms = elapsed;
                    recorded.retry_permitted = true;
                    append_transition(store, cas, run_id, authority, recorded.clone())?;
                    settle_provider_budget(budget, &reservation, failure.charged_tokens);
                    history.push(recorded);
                    attempt = 2;
                    continue;
                }
                let circuit_open = repeated;
                let failure_class = failure.class;
                let next_action = failure.next_action;
                let charged_tokens = failure.charged_tokens;
                let failed = failed_transition(
                    &running,
                    failure,
                    charged_tokens,
                    elapsed,
                    circuit_open,
                    false,
                );
                append_transition(store, cas, run_id, authority, failed)?;
                settle_provider_budget(budget, &reservation, charged_tokens);
                return Err(if circuit_open {
                    format!(
                        "provider operation `{operation_id}` opened its circuit after {failure_class:?}; next action: {next_action:?}; restart the Round after correction"
                    )
                } else {
                    format!(
                        "provider operation `{operation_id}` failed preflight: {failure_class:?}; next action: {next_action:?}; restart the Round after correction"
                    )
                });
            }
        }
    }
}

#[derive(Clone)]
struct ProviderFailure {
    class: ProviderFailureClassV1,
    fingerprint: String,
    retryable: bool,
    next_action: ProviderNextActionV1,
    charged_tokens: u64,
}

impl ProviderFailure {
    fn new(
        class: ProviderFailureClassV1,
        code: &'static str,
        retryable: bool,
        next_action: ProviderNextActionV1,
        charged_tokens: u64,
    ) -> Self {
        let fingerprint = digest_parts([format!("{class:?}").as_str(), code]);
        Self {
            class,
            fingerprint,
            retryable,
            next_action,
            charged_tokens,
        }
    }
}

fn settle_provider_budget(
    budget: &mut BudgetLedger,
    reservation: &Reservation,
    charged_tokens: u64,
) {
    if charged_tokens == 0 {
        budget.release(reservation);
    } else {
        budget.charge(reservation, charged_tokens);
    }
}

fn perform_preflight(
    spec: &ProviderSpec,
    reviewer: &ReviewerCommand,
    state_dir: &Path,
    structural_probes: &mut BTreeSet<String>,
) -> Result<u64, ProviderFailure> {
    let program = PathBuf::from(&reviewer.program);
    if !structural_probes.contains(&spec.id) {
        let cancelled = AtomicBool::new(false);
        let probe_path = sanitized_path();
        let probe = run_probe(&program, spec, &probe_path, &cancelled)
            .map_err(|error| classify_failure(&error, "structural_probe", 0))?;
        let authenticated = match spec.kind {
            ProviderKind::Claude => parse_claude_status(probe.status.success(), &probe.stdout).0,
            ProviderKind::Codex => parse_codex_status(probe.status.success(), &probe.stdout).0,
        };
        if authenticated != "authenticated" {
            return Err(ProviderFailure::new(
                ProviderFailureClassV1::InvalidOrExpiredAuthentication,
                "structural_auth_rejected",
                false,
                ProviderNextActionV1::RefreshAuthentication,
                0,
            ));
        }
        structural_probes.insert(spec.id.clone());
    }
    let smoke = run_smoke(&program, spec, reviewer, state_dir).map_err(|error| {
        let charged = if error.starts_with("cannot start provider smoke") {
            0
        } else {
            SMOKE_RESERVATION
        };
        classify_failure(&error, "smoke_transport", charged)
    })?;
    let assessment = assess_smoke(spec.kind, &smoke.stdout);
    if !smoke.status.success() {
        let stderr = String::from_utf8_lossy(&smoke.stderr);
        let detail = assessment
            .error
            .as_deref()
            .or_else(|| stderr.lines().last())
            .unwrap_or("provider smoke failed without a structured error");
        return Err(classify_failure(
            detail,
            "smoke_exit",
            assessment.cost_tokens,
        ));
    }
    if !assessment.acknowledged {
        return Err(ProviderFailure::new(
            ProviderFailureClassV1::UnknownProviderFailure,
            "smoke_missing_acknowledgement",
            true,
            ProviderNextActionV1::RetryExplicitly,
            assessment.cost_tokens,
        ));
    }
    Ok(assessment.cost_tokens.max(1))
}

fn classify_failure(detail: &str, code: &'static str, charged_tokens: u64) -> ProviderFailure {
    let lower = detail.to_ascii_lowercase();
    if [
        "login",
        "logged out",
        "unauthorized",
        "authentication",
        "expired",
        "401",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ProviderFailure::new(
            ProviderFailureClassV1::InvalidOrExpiredAuthentication,
            code,
            false,
            ProviderNextActionV1::RefreshAuthentication,
            charged_tokens,
        )
    } else if [
        "quota",
        "rate limit",
        "rate_limit",
        "too many requests",
        "429",
        "usage limit",
        "usage_limit",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ProviderFailure::new(
            ProviderFailureClassV1::RateLimitOrQuotaExhaustion,
            code,
            false,
            ProviderNextActionV1::WaitForQuota,
            charged_tokens,
        )
    } else if ["model", "capability", "not available", "unsupported"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        ProviderFailure::new(
            ProviderFailureClassV1::UnavailableModelOrCapability,
            code,
            false,
            ProviderNextActionV1::SelectAvailableModel,
            charged_tokens,
        )
    } else if lower.contains("timed out") || lower.contains("timeout") {
        ProviderFailure::new(
            ProviderFailureClassV1::SmokeTimeout,
            code,
            true,
            ProviderNextActionV1::IncreaseSmokeTimeout,
            charged_tokens,
        )
    } else if [
        "network",
        "connection",
        "dns",
        "transport",
        "broken pipe",
        "unreachable",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ProviderFailure::new(
            ProviderFailureClassV1::TransientTransportFailure,
            code,
            true,
            ProviderNextActionV1::CheckTransport,
            charged_tokens,
        )
    } else {
        ProviderFailure::new(
            ProviderFailureClassV1::UnknownProviderFailure,
            code,
            false,
            ProviderNextActionV1::InspectProviderFailure,
            charged_tokens,
        )
    }
}

#[derive(Default)]
struct SmokeAssessment {
    acknowledged: bool,
    cost_tokens: u64,
    error: Option<String>,
}

fn assess_smoke(kind: ProviderKind, stdout: &[u8]) -> SmokeAssessment {
    match kind {
        ProviderKind::Claude => assess_claude_smoke(stdout),
        ProviderKind::Codex => assess_codex_smoke(stdout),
    }
}

fn assess_claude_smoke(stdout: &[u8]) -> SmokeAssessment {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return SmokeAssessment::default();
    };
    let usage = value.get("usage");
    let count = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let result = value.get("result").and_then(serde_json::Value::as_str);
    let is_error = value
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    SmokeAssessment {
        acknowledged: !is_error && result.is_some_and(smoke_acknowledgement),
        cost_tokens: count("input_tokens")
            + count("cache_creation_input_tokens")
            + count("output_tokens"),
        error: value
            .pointer("/error/type")
            .or_else(|| value.get("subtype"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| is_error.then(|| result.unwrap_or("claude_error").to_string())),
    }
}

fn assess_codex_smoke(stdout: &[u8]) -> SmokeAssessment {
    let mut assessment = SmokeAssessment::default();
    let mut final_message = None;
    for line in stdout.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("turn.completed") => {
                if let Some(usage) = value.get("usage") {
                    let count = |key: &str| {
                        usage
                            .get(key)
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0)
                    };
                    assessment.cost_tokens = count("input_tokens")
                        .saturating_sub(count("cached_input_tokens"))
                        + count("output_tokens");
                }
            }
            Some("item.completed") => {
                if let Some(item) = value.get("item")
                    && item.get("type").and_then(serde_json::Value::as_str) == Some("agent_message")
                {
                    final_message = item
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                }
            }
            Some("error") | Some("turn.failed") => {
                assessment.error = value
                    .pointer("/error/type")
                    .or_else(|| value.get("message"))
                    .or_else(|| value.pointer("/error/message"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    assessment.acknowledged = final_message.as_deref().is_some_and(smoke_acknowledgement);
    assessment
}

fn smoke_acknowledgement(message: &str) -> bool {
    message
        .trim()
        .trim_end_matches('.')
        .eq_ignore_ascii_case("OK")
}

struct SmokeOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(unix)]
fn run_smoke(
    program: &Path,
    spec: &ProviderSpec,
    reviewer: &ReviewerCommand,
    state_dir: &Path,
) -> Result<SmokeOutput, String> {
    let smoke_dir = state_dir.join("provider-smoke");
    fs::create_dir_all(&smoke_dir)
        .map_err(|error| format!("cannot create provider smoke directory: {error}"))?;
    let smoke_command = match spec.kind {
        ProviderKind::Claude => review_runner_claude::smoke_command(reviewer),
        ProviderKind::Codex => review_runner_codex::smoke_command(reviewer, &smoke_dir),
    }?;
    let args = smoke_command.resolve().map_err(|error| error.to_string())?;
    let mut command = Command::new(program);
    command.args(args);
    let probe_path = sanitized_path();
    configure_probe_environment(&mut command, spec, &probe_path);
    command.env("LC_ALL", "C");
    if spec.kind == ProviderKind::Codex {
        command.env("HOME", &smoke_dir);
    }
    command
        .current_dir(&smoke_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = review_process::spawn(&mut command)
        .map_err(|error| format!("cannot start provider smoke: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(b"Reply with exactly: OK\n") {
            stop_probe(&mut child);
            return Err(format!("cannot write provider smoke prompt: {error}"));
        }
    }
    let mut stdout = child.stdout.take().expect("provider stdout was piped");
    let mut stderr = child.stderr.take().expect("provider stderr was piped");
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_probe(&mut child);
        return Err(error);
    }
    let deadline = Instant::now() + SMOKE_TIMEOUT;
    let mut stdout_output = Vec::with_capacity(4096);
    let mut stderr_output = Vec::with_capacity(1024);
    let mut captured = 0_usize;
    let mut exceeded = false;
    let status = loop {
        let stdout_read = drain_smoke_available(
            &mut stdout,
            &mut stdout_output,
            &mut captured,
            &mut exceeded,
        )?;
        let stderr_read = drain_smoke_available(
            &mut stderr,
            &mut stderr_output,
            &mut captured,
            &mut exceeded,
        )?;
        if exceeded {
            stop_probe(&mut child);
            return Err(format!(
                "provider smoke output exceeds {MAX_PROBE_OUTPUT} bytes"
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                stop_probe(&mut child);
                return Err(format!("provider smoke failed: {error}"));
            }
        }
        if Instant::now() >= deadline {
            stop_probe(&mut child);
            return Err(format!(
                "provider smoke timed out after {} seconds",
                SMOKE_TIMEOUT.as_secs()
            ));
        }
        if !stdout_read && !stderr_read {
            thread::sleep(Duration::from_millis(25));
        }
    };
    terminate_probe_group(child.id());
    loop {
        let stdout_read = drain_smoke_available(
            &mut stdout,
            &mut stdout_output,
            &mut captured,
            &mut exceeded,
        )?;
        let stderr_read = drain_smoke_available(
            &mut stderr,
            &mut stderr_output,
            &mut captured,
            &mut exceeded,
        )?;
        if exceeded || (!stdout_read && !stderr_read) {
            break;
        }
    }
    if exceeded {
        return Err(format!(
            "provider smoke output exceeds {MAX_PROBE_OUTPUT} bytes"
        ));
    }
    Ok(SmokeOutput {
        status,
        stdout: stdout_output,
        stderr: stderr_output,
    })
}

fn drain_smoke_available(
    stream: &mut impl Read,
    output: &mut Vec<u8>,
    captured: &mut usize,
    exceeded: &mut bool,
) -> Result<bool, String> {
    let mut chunk = [0_u8; 8192];
    match stream.read(&mut chunk) {
        Ok(0) => Ok(false),
        Ok(count) => {
            let remaining = MAX_PROBE_OUTPUT.saturating_sub(*captured);
            let kept = count.min(remaining);
            output.extend_from_slice(&chunk[..kept]);
            *captured += kept;
            *exceeded |= count > remaining;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(format!("cannot read provider smoke output: {error}")),
    }
}

#[cfg(not(unix))]
fn run_smoke(
    _program: &Path,
    _spec: &ProviderSpec,
    _reviewer: &ReviewerCommand,
    _state_dir: &Path,
) -> Result<SmokeOutput, String> {
    Err("provider smoke requires Unix process-group isolation".to_string())
}

struct OperationIdentity<'a> {
    operation_id: &'a str,
    spec: &'a ProviderSpec,
    capability_id: &'a str,
    node_id: &'a str,
    authority: &'a RoundAuthority,
}

fn transition(
    identity: &OperationIdentity<'_>,
    operation_epoch: u64,
    state: ProviderOperationStateV1,
    attempt: Option<u32>,
    attempt_id: Option<String>,
) -> ProviderOperationTransitionPayloadV1 {
    ProviderOperationTransitionPayloadV1 {
        operation_id: identity.operation_id.to_string(),
        provider_id: identity.spec.id.clone(),
        capability_id: identity.capability_id.to_string(),
        node_id: identity.node_id.to_string(),
        round: identity.authority.round(),
        round_epoch: identity.authority.epoch(),
        operation_epoch,
        state,
        attempt,
        attempt_id,
        failure_class: None,
        failure_fingerprint: None,
        continuation_handle: None,
        reserved_tokens: if state == ProviderOperationStateV1::Resumed {
            0
        } else {
            SMOKE_RESERVATION
        },
        charged_tokens: 0,
        elapsed_ms: 0,
        retry_permitted: false,
        circuit_open: false,
        next_action: None,
    }
}

fn failed_transition(
    running: &ProviderOperationTransitionPayloadV1,
    failure: ProviderFailure,
    charged_tokens: u64,
    elapsed_ms: u64,
    circuit_open: bool,
    retry_permitted: bool,
) -> ProviderOperationTransitionPayloadV1 {
    let mut failed = running.clone();
    failed.state = ProviderOperationStateV1::Failed;
    failed.failure_class = Some(failure.class);
    failed.failure_fingerprint = Some(failure.fingerprint);
    failed.charged_tokens = charged_tokens;
    failed.elapsed_ms = elapsed_ms;
    failed.retry_permitted = retry_permitted;
    failed.circuit_open = circuit_open;
    failed.next_action = Some(failure.next_action);
    failed
}

fn failed_transition_from_recorded(
    recorded: &ProviderOperationTransitionPayloadV1,
) -> ProviderOperationTransitionPayloadV1 {
    let mut failed = recorded.clone();
    failed.state = ProviderOperationStateV1::Failed;
    failed.charged_tokens = 0;
    failed.retry_permitted = false;
    failed.circuit_open = true;
    failed.next_action = Some(ProviderNextActionV1::InspectProviderFailure);
    failed
}

fn append_transition(
    store: &mut EventStore,
    cas: &Cas,
    run_id: &str,
    authority: &RoundAuthority,
    transition: ProviderOperationTransitionPayloadV1,
) -> Result<(), String> {
    let mut event = NewEvent::new(
        EventType::ProviderOperationTransitionV1,
        serde_json::to_value(&transition).map_err(|error| error.to_string())?,
    )
    .node(transition.node_id.clone())
    .caused_by(authority.round_event_id())
    .correlating(transition.operation_id.clone());
    if let Some(attempt_id) = &transition.attempt_id {
        event = event.attempt(attempt_id.clone());
    }
    store
        .append(run_id, cas, event)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn continuation_error(spec: &ProviderSpec, operation_id: &str, epoch: u64) -> String {
    let login = authentication_command(spec);
    format!(
        "provider operation `{operation_id}` is waiting_for_human; run `{login}` in a persistent terminal, then rerun with --resume-provider {operation_id}:{epoch}"
    )
}

fn authentication_command(spec: &ProviderSpec) -> String {
    if spec.registry_declared
        && let Some(auth_dir) = spec.auth_dir.as_deref().and_then(Path::to_str)
    {
        return format!(
            "af provider setup {} --kind {} --auth-dir {}",
            shell_words::quote(&spec.id),
            spec.kind.name(),
            shell_words::quote(auth_dir)
        );
    }
    login_command(spec)
}

/// The harness login command that writes credentials into exactly the context `spec` probes.
///
/// The selector is repeated on the command line so a login started from another shell cannot
/// land in a different directory than the one `af` reads for this Provider.
fn login_command(spec: &ProviderSpec) -> String {
    let (selector, login) = match spec.kind {
        ProviderKind::Claude => ("CLAUDE_CONFIG_DIR", "claude auth login"),
        ProviderKind::Codex => ("CODEX_HOME", "codex login"),
    };
    match spec.auth_dir.as_deref() {
        Some(auth_dir) => auth_dir.to_str().map_or_else(
            || {
                format!(
                    "set {selector} to the displayed non-UTF-8 auth directory, then run `{login}`"
                )
            },
            |auth_dir| format!("{selector}={} {login}", shell_words::quote(auth_dir)),
        ),
        None => login.to_string(),
    }
}

/// Why a `not authenticated` row is not authenticated, and the one command that fixes it.
///
/// The harness CLI is the authority on whether a directory holds a login; `af` never inspects
/// credential files. What it can add is the directory it actually asked about and, for a
/// registry entry, the reminder that `auth_dir` itself may be the mistake.
fn logged_out_detail(spec: &ProviderSpec) -> String {
    let harness = match spec.kind {
        ProviderKind::Claude => "Claude",
        ProviderKind::Codex => "Codex",
    };
    let context = spec
        .auth_dir
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the CLI default directory".to_string());
    let mut detail = format!(
        "no {harness} login in {context}; fix: {}",
        authentication_command(spec)
    );
    if spec.registry_declared {
        detail.push_str(", or point auth_dir at the directory that holds the intended login");
    }
    detail
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn short_id(parts: &[&str]) -> String {
    digest_hex(parts.iter().copied())[..26].to_string()
}

fn digest_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    format!("sha256:{}", digest_hex(parts))
}

fn digest_hex<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    review_core::hex::encode(&digest.finalize())
}

#[cfg(test)]
mod provider_operation_tests {
    use super::*;

    #[test]
    fn claude_usage_metadata_is_not_a_smoke_acknowledgement() {
        let rejected = assess_claude_smoke(
            br#"{"is_error":false,"result":"","usage":{"input_tokens":8,"output_tokens":1}}"#,
        );
        assert!(!rejected.acknowledged);
        assert_eq!(rejected.cost_tokens, 9);

        let accepted = assess_claude_smoke(
            br#"{"is_error":false,"result":" OK\n","usage":{"input_tokens":8,"cache_creation_input_tokens":3,"output_tokens":1}}"#,
        );
        assert!(accepted.acknowledged);
        assert_eq!(accepted.cost_tokens, 12);
    }

    #[test]
    fn codex_smoke_uses_the_final_message_and_final_cumulative_usage() {
        let assessment = assess_codex_smoke(
            br#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":2}}
{"type":"item.completed","item":{"type":"agent_message","text":"OK"}}
{"type":"turn.completed","usage":{"input_tokens":120,"cached_input_tokens":90,"output_tokens":3}}
"#,
        );
        assert!(assessment.acknowledged);
        assert_eq!(assessment.cost_tokens, 33);
    }

    #[test]
    fn normalized_failure_fingerprint_never_contains_provider_detail() {
        let first = classify_failure(
            "authentication failed with oauth-code-value",
            "smoke_exit",
            0,
        );
        let second = classify_failure(
            "authentication failed with access-token-value",
            "smoke_exit",
            0,
        );
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(!first.fingerprint.contains("oauth"));
        assert!(!first.fingerprint.contains("token"));
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
    run_probe_before(program, spec, probe_path, cancelled, None)
}

#[cfg(unix)]
fn run_probe_before(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Result<ProbeOutput, String> {
    check_task_probe_control(attempt_deadline, cancelled)?;
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
    let mut child = review_process::spawn(&mut command)
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
    let timeout = match spec.kind {
        ProviderKind::Claude => CLAUDE_STRUCTURAL_TIMEOUT,
        ProviderKind::Codex => PROBE_TIMEOUT,
    };
    let deadline = Instant::now() + timeout;
    let deadline = attempt_deadline.map_or(deadline, |limit| deadline.min(limit));
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
                timeout.as_secs()
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

// The optional limit is the original native Attempt deadline. Historical inventory probes retain
// their own timeout and cancellation behavior; a Task recheck never receives a fresh wall budget.
fn check_task_probe_control(
    deadline: Option<Instant>,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if let Some(deadline) = deadline {
        if cancelled.load(Ordering::Acquire) {
            return Err("Task Provider identity check cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("Task Provider identity check deadline elapsed".into());
        }
    }
    Ok(())
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
        Some(true) if !success => {
            return (
                "unavailable".to_string(),
                "-".to_string(),
                "Claude auth status exited unsuccessfully".to_string(),
            );
        }
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
        if !success {
            return (
                "unavailable".to_string(),
                "-".to_string(),
                "Codex login status exited unsuccessfully".to_string(),
            );
        }
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

pub(crate) fn resolve_program(program: &str) -> Option<PathBuf> {
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
    fn conditional_registry_replace_preserves_a_noncooperating_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let external = "version = 1\n# external change\nproviders = []\n";
        fs::write(&path, external).unwrap();
        assert!(write_registry(&path, b"version = 1\n", Some(original)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        assert!(!registry_transaction_path(&path).exists());
        assert_eq!(read_registry(&path).unwrap().as_deref(), Some(external));
    }

    #[test]
    fn crash_between_exchange_and_validation_fails_closed_with_both_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let stage = root.path().join(".provider-stage");
        let transaction = registry_transaction_path(&path);
        fs::write(&path, "candidate\n").unwrap();
        fs::write(&stage, "displaced\n").unwrap();
        fs::write(&transaction, "version = 1\nstage = \".provider-stage\"\n").unwrap();

        let error = read_registry(&path).unwrap_err();
        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "candidate\n");
        assert_eq!(fs::read_to_string(&stage).unwrap(), "displaced\n");
    }

    #[test]
    fn marker_created_during_a_read_blocks_the_tentative_value() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let transaction = registry_transaction_path(&path);
        fs::write(&path, "version = 1\nproviders = []\n").unwrap();

        ensure_no_registry_transaction(&path).unwrap();
        let tentative = read_registry_unchecked(&path).unwrap();
        fs::write(&transaction, "version = 1\n").unwrap();

        assert!(tentative.is_some());
        assert!(ensure_no_registry_transaction(&path).is_err());
        assert!(read_registry(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_registry_checks_the_marker_beside_the_resolved_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let alias = root.path().join("providers-alias.toml");
        fs::write(&path, "version = 1\nproviders = []\n").unwrap();
        symlink(&path, &alias).unwrap();
        fs::write(registry_transaction_path(&path), "version = 1\n").unwrap();

        let error = read_registry(&alias).unwrap_err();
        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn reader_rechecks_a_leaf_symlink_retargeted_to_a_fenced_registry() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("providers-first.toml");
        let second = root.path().join("providers-second.toml");
        let alias = root.path().join("providers-alias.toml");
        fs::write(&first, "version = 1\n# first\nproviders = []\n").unwrap();
        fs::write(&second, "version = 1\n# second\nproviders = []\n").unwrap();
        symlink(&first, &alias).unwrap();
        let second_transaction = registry_transaction_path(&second);
        fs::write(&second_transaction, "version = 1\n").unwrap();

        let error = read_registry_with_hooks(
            &alias,
            || {},
            || {
                fs::remove_file(&alias).unwrap();
                symlink(&second, &alias).unwrap();
            },
        )
        .unwrap_err();

        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
        assert!(error.contains(second.to_str().unwrap()), "{error}");
        assert!(second_transaction.is_file());
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn reader_rejects_a_candidate_that_is_exchanged_and_rolled_back_mid_read() {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let stage = root.path().join("stage");
        let transaction = registry_transaction_path(&path);
        fs::write(&path, "version = 1\n# original\nproviders = []\n").unwrap();
        fs::write(&stage, "version = 1\n# candidate\nproviders = []\n").unwrap();

        let result = read_registry_with_hooks(
            &path,
            || {
                fs::write(&transaction, "version = 1\n").unwrap();
                renameat_with(CWD, &stage, CWD, &path, RenameFlags::EXCHANGE).unwrap();
            },
            || {
                renameat_with(CWD, &stage, CWD, &path, RenameFlags::EXCHANGE).unwrap();
            },
        );

        let error = result.unwrap_err();
        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
        assert!(fs::read_to_string(&path).unwrap().contains("# original"));
        assert!(fs::read_to_string(&stage).unwrap().contains("# candidate"));
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn rollback_race_preserves_the_second_replacement_at_the_stage_path() {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        fs::write(&path, "candidate\n").unwrap();
        let candidate = File::open(&path).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(b"first external replacement\n").unwrap();
        let stage_path = stage.path().to_path_buf();
        stage.disable_cleanup(true);

        assert!(same_file(&path, &candidate).unwrap());
        let second = root.path().join("second");
        fs::write(&second, "second external replacement\n").unwrap();
        fs::rename(&second, &path).unwrap();
        renameat_with(CWD, &stage_path, CWD, &path, RenameFlags::EXCHANGE).unwrap();
        let _ = stage.keep();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "first external replacement\n"
        );
        assert_eq!(
            fs::read_to_string(&stage_path).unwrap(),
            "second external replacement\n"
        );
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn recovery_hardlink_preserves_late_writes_after_stage_replacement() {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut displaced = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let stage_path = stage.path().to_path_buf();
        let recovery =
            prepare_registry_recovery(&path, &stage_path, stage.as_file(), original, candidate)
                .unwrap();

        renameat_with(CWD, &stage_path, CWD, &path, RenameFlags::EXCHANGE).unwrap();
        let external = "version = 1\n# second replacement\nproviders = []\n";
        let second = root.path().join("second");
        fs::write(&second, external).unwrap();
        fs::rename(&second, &stage_path).unwrap();
        displaced.write_all(b"# late concurrent write\n").unwrap();
        displaced.sync_all().unwrap();

        let preserved = fs::read_to_string(&recovery.displaced_copy).unwrap();
        assert!(preserved.starts_with(original));
        assert!(preserved.ends_with("# late concurrent write\n"));
        assert_eq!(fs::read_to_string(&stage_path).unwrap(), external);
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn successful_replace_records_and_preserves_recovery_versions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();

        let preserved = write_registry(&path, candidate.as_bytes(), Some(original))
            .unwrap()
            .unwrap();
        let recovery = preserved.parent().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), candidate);
        assert_eq!(fs::read_to_string(&preserved).unwrap(), original);
        assert_eq!(
            fs::read_to_string(recovery.join("candidate")).unwrap(),
            candidate
        );
        assert_eq!(
            fs::read_to_string(recovery.join("exchange-stage")).unwrap(),
            original
        );
        assert!(!registry_transaction_path(&path).exists());
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn recovery_archives_only_a_hash_validated_precommit_marker() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);

        recover_registry(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!transaction.exists());
        assert!(recovery.archived_marker.is_file());
        assert_eq!(
            fs::read_to_string(recovery.candidate_copy).unwrap(),
            candidate
        );
        assert_eq!(
            fs::read_to_string(recovery.displaced_copy).unwrap(),
            original
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_a_symlinked_candidate_and_retains_the_marker() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);
        fs::remove_file(&recovery.candidate_copy).unwrap();
        symlink(&recovery.displaced_copy, &recovery.candidate_copy).unwrap();

        let error = recover_registry(&path).unwrap_err();
        assert!(error.contains("without following links"), "{error}");
        assert!(transaction.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_revalidates_held_files_immediately_before_archival() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);
        let replaced = recovery.directory.join("replaced-candidate");
        let replacement = recovery.directory.join("replacement-candidate");

        let error = recover_registry_with_hook(&path, || {
            fs::write(&replacement, candidate).unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(&recovery.candidate_copy, &replaced).unwrap();
            fs::rename(&replacement, &recovery.candidate_copy).unwrap();
        })
        .unwrap_err();
        assert!(error.contains("state changed while validating"), "{error}");
        assert!(transaction.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_revalidates_marker_bytes_immediately_before_archival() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);

        let error = recover_registry_with_hook(&path, || {
            let mut marker = OpenOptions::new().write(true).open(&transaction).unwrap();
            marker.write_all(b"X").unwrap();
            marker.sync_all().unwrap();
        })
        .unwrap_err();
        assert!(error.contains("state changed while validating"), "{error}");
        assert!(transaction.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_parent_replaced_by_a_symlink_to_the_held_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("registry");
        let displaced_parent = root.path().join("registry-held");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(&parent).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);

        let error = recover_registry_with_hook(&path, || {
            fs::rename(&parent, &displaced_parent).unwrap();
            symlink(&displaced_parent, &parent).unwrap();
        })
        .unwrap_err();
        assert!(error.contains("state changed while validating"), "{error}");
        assert!(transaction.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_through_an_alias_handles_the_marker_that_fences_reads() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let alias = root.path().join("providers-alias.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        symlink(&path, &alias).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);
        assert!(read_registry(&alias).is_err());

        recover_configured_registry(&alias).unwrap();

        assert!(!transaction.exists());
        assert!(recovery.archived_marker.is_file());
        assert_eq!(read_registry(&alias).unwrap().as_deref(), Some(original));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_deduplicates_a_registry_reached_through_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("registry");
        let alias_directory = root.path().join("registry-alias");
        fs::create_dir(&directory).unwrap();
        symlink(&directory, &alias_directory).unwrap();
        let path = directory.join("providers.toml");
        let alias = alias_directory.join("providers.toml");
        let original = "version = 1\nproviders = []\n";
        let candidate = "version = 1\n# candidate\nproviders = []\n";
        fs::write(&path, original).unwrap();
        let mut stage = tempfile::NamedTempFile::new_in(&directory).unwrap();
        stage.write_all(candidate.as_bytes()).unwrap();
        stage.as_file().sync_all().unwrap();
        let recovery =
            prepare_registry_recovery(&path, stage.path(), stage.as_file(), original, candidate)
                .unwrap();
        let (transaction, marker_file) =
            write_registry_transaction(&path, stage.path(), original, stage.as_file(), &recovery)
                .unwrap();
        drop(marker_file);

        recover_configured_registry(&alias).unwrap();

        assert!(!transaction.exists());
        assert!(recovery.archived_marker.is_file());
        assert_eq!(read_registry(&alias).unwrap().as_deref(), Some(original));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_revalidates_a_symlinked_ancestor_after_acquiring_locks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("registry-first");
        let second = root.path().join("registry-second");
        let alias_directory = root.path().join("registry-alias");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        symlink(&first, &alias_directory).unwrap();
        let alias = alias_directory.join("providers.toml");
        let second_registry = second.join("providers.toml");
        let second_transaction = registry_transaction_path(&second_registry);
        fs::write(&second_transaction, "version = 1\n").unwrap();

        let error = recover_configured_registry_with_hook(&alias, || {
            fs::remove_file(&alias_directory).unwrap();
            symlink(&second, &alias_directory).unwrap();
        })
        .unwrap_err();

        assert!(
            error.contains("changed while recovery locks were acquired"),
            "{error}"
        );
        assert!(second_transaction.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_through_an_alias_refuses_two_competing_markers() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let alias = root.path().join("providers-alias.toml");
        fs::write(&path, "version = 1\nproviders = []\n").unwrap();
        symlink(&path, &alias).unwrap();
        fs::write(registry_transaction_path(&path), "version = 1\n").unwrap();
        fs::write(registry_transaction_path(&alias), "version = 1\n").unwrap();

        let error = recover_configured_registry(&alias).unwrap_err();
        assert!(error.contains("refusing ambiguous recovery"), "{error}");
        assert!(registry_transaction_path(&path).is_file());
        assert!(registry_transaction_path(&alias).is_file());
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    #[test]
    fn recovery_identifies_a_committed_candidate_and_refuses_unknown_live_bytes() {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        for tamper in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("providers.toml");
            let original = "version = 1\nproviders = []\n";
            let candidate = "version = 1\n# candidate\nproviders = []\n";
            fs::write(&path, original).unwrap();
            let mut stage = tempfile::NamedTempFile::new_in(root.path()).unwrap();
            stage.write_all(candidate.as_bytes()).unwrap();
            stage.as_file().sync_all().unwrap();
            let stage_path = stage.path().to_path_buf();
            let recovery =
                prepare_registry_recovery(&path, &stage_path, stage.as_file(), original, candidate)
                    .unwrap();
            let (transaction, marker_file) = write_registry_transaction(
                &path,
                &stage_path,
                original,
                stage.as_file(),
                &recovery,
            )
            .unwrap();
            drop(marker_file);
            renameat_with(CWD, &stage_path, CWD, &path, RenameFlags::EXCHANGE).unwrap();
            if tamper {
                fs::write(&path, "version = 1\n# unknown\nproviders = []\n").unwrap();
                let error = recover_registry(&path).unwrap_err();
                assert!(
                    error.contains("matches neither transaction hash"),
                    "{error}"
                );
                assert!(transaction.is_file());
            } else {
                recover_registry(&path).unwrap();
                assert_eq!(fs::read_to_string(&path).unwrap(), candidate);
                assert!(!transaction.exists());
                assert!(recovery.archived_marker.is_file());
            }
        }
    }

    #[test]
    fn a_marker_beside_a_missing_registry_still_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        fs::write(registry_transaction_path(&path), "version = 1\n").unwrap();

        let error = read_registry(&path).unwrap_err();
        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
    }

    #[test]
    fn add_rejects_a_marker_beside_a_missing_registry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        fs::write(registry_transaction_path(&path), "version = 1\n").unwrap();

        let error =
            add_to_registry(&path, "codex-main", ProviderKind::Codex, root.path()).unwrap_err();
        assert!(
            error.contains("unfinished publication transaction"),
            "{error}"
        );
        assert!(!path.exists());
        assert!(registry_transaction_path(&path).is_file());
    }

    #[test]
    fn first_registry_publication_never_clobbers_an_external_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("providers.toml");
        let external = "version = 1\n# external creation\nproviders = []\n";
        fs::write(&path, external).unwrap();
        assert!(write_registry(&path, b"version = 1\n", None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
    }

    #[test]
    fn ambient_registration_hints_choose_an_unused_id() {
        let ids = BTreeSet::from(["claude-main".to_string(), "claude-main-2".to_string()]);
        assert_eq!(available_provider_id("claude", &ids), "claude-main-3");
    }

    #[test]
    fn durable_directory_creation_builds_the_complete_hierarchy() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("config/af");
        create_dir_all_durable(&directory).unwrap();
        assert!(directory.is_dir());
        create_dir_all_durable(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directory_creation_rejects_an_unsafe_ancestor_without_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        let auth = shared.join("new/auth");
        let registry = shared.join("new-config/af");

        assert!(create_private_directory(&auth).is_err());
        assert!(create_dir_all_durable(&registry).is_err());
        assert!(!shared.join("new").exists());
        assert!(!shared.join("new-config").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rename_controlling_ancestor_rejects_a_foreign_non_root_owner() {
        let error = validate_rename_controlling_directory_values(
            Path::new("/foreign-owned"),
            1001,
            0o040755,
            "auth directory",
            1000,
        )
        .unwrap_err();
        assert!(error.contains("owned by uid 1001"), "{error}");
        assert!(
            validate_rename_controlling_directory_values(
                Path::new("/root-owned"),
                0,
                0o040755,
                "auth directory",
                1000,
            )
            .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn auth_lock_rejects_a_directory_replaced_while_waiting() {
        use std::sync::mpsc;

        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("auth");
        let displaced = root.path().join("old-auth");
        fs::create_dir(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        let first = auth_context_lock(ProviderKind::Codex, &auth).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_auth = auth.clone();
        let worker = std::thread::spawn(move || {
            auth_context_lock_with_hook(ProviderKind::Codex, &worker_auth, || {
                ready_tx.send(()).unwrap();
            })
        });
        ready_rx.recv().unwrap();
        fs::rename(&auth, &displaced).unwrap();
        fs::create_dir(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        drop(first);

        let error = match worker.join().unwrap() {
            Ok(_) => panic!("replaced auth directory was accepted after lock wait"),
            Err(error) => error,
        };
        assert!(error.contains("changed while waiting"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn auth_lock_rejects_a_symlink_to_the_held_directory_after_waiting() {
        use std::os::unix::fs::symlink;
        use std::sync::mpsc;

        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("auth");
        let displaced = root.path().join("old-auth");
        fs::create_dir(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        let first = auth_context_lock(ProviderKind::Codex, &auth).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_auth = auth.clone();
        let worker = std::thread::spawn(move || {
            auth_context_lock_with_hook(ProviderKind::Codex, &worker_auth, || {
                ready_tx.send(()).unwrap();
            })
        });
        ready_rx.recv().unwrap();
        fs::rename(&auth, &displaced).unwrap();
        symlink(&displaced, &auth).unwrap();
        drop(first);

        let error = match worker.join().unwrap() {
            Ok(_) => panic!("symlink to held auth directory was accepted after lock wait"),
            Err(error) => error,
        };
        assert!(error.contains("changed while waiting"), "{error}");
    }

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
    fn registry_rejects_ids_reserved_for_ambient_discovery() {
        let root = tempfile::tempdir().unwrap();
        for (id, kind) in [("claude-ambient", "claude"), ("codex-ambient", "codex")] {
            let registry = format!(
                "version = 1\n[[providers]]\nid = {id:?}\nkind = {kind:?}\nauth_dir = {:?}\n",
                root.path().to_str().unwrap()
            );
            let error = match parse_registry(&registry, Path::new("/registry.toml")) {
                Ok(_) => panic!("reserved ambient id was accepted"),
                Err(error) => error,
            };
            assert!(error.contains("reserved for ambient discovery"), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn registry_rejects_a_symlinked_auth_context_before_canonicalization() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("real-auth");
        let linked = root.path().join("linked-auth");
        fs::create_dir(&target).unwrap();
        symlink(&target, &linked).unwrap();
        let registry = format!(
            "version = 1\n[[providers]]\nid = \"codex-main\"\nkind = \"codex\"\nauth_dir = {:?}\n",
            linked.to_str().unwrap()
        );

        let error = match parse_registry(&registry, Path::new("/registry.toml")) {
            Ok(_) => panic!("symlinked auth context was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("must not be a symlink"), "{error}");
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
    fn claude_usage_capture_retains_only_the_newest_bounded_bytes() {
        let mut captured = b"abcdef".to_vec();
        append_bounded_window(&mut captured, b"ghi", 6);
        assert_eq!(bounded_tail(&captured, 6), b"defghi");

        append_bounded_window(&mut captured, b"0123456789", 6);
        assert_eq!(bounded_tail(&captured, 6), b"456789");
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
            registry_declared: false,
        };

        let probe_path = sanitized_path();
        let limits =
            probe_claude_weekly_limits(&program, &spec, &probe_path, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].used_percent, 42);
        assert_eq!(limits[0].resets_at, None);
    }

    #[cfg(unix)]
    #[test]
    fn claude_usage_cache_skips_a_second_interactive_probe() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("claude");
        fs::write(
            &program,
            "#!/bin/sh\nprintf x >> \"$0.count\"\nprintf 'Current week (all models)\\r\\n42%%used\\r\\nExtra usage\\r\\n'\nsleep 30\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();
        let spec = ProviderSpec {
            id: "claude-cache-test".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: Some(directory.path().to_path_buf()),
            explicit_selector: true,
            source: "test".to_string(),
            registry_declared: false,
        };
        let probe_path = sanitized_path();

        let first =
            cached_claude_weekly_limits(&program, &spec, &probe_path, &AtomicBool::new(false))
                .unwrap();
        let second =
            cached_claude_weekly_limits(&program, &spec, &probe_path, &AtomicBool::new(false))
                .unwrap();

        assert_eq!(first[0].used_percent, 42);
        assert_eq!(second[0].used_percent, 42);
        assert_eq!(
            fs::read_to_string(program.with_extension("count")).unwrap(),
            "x"
        );
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
            registry_declared: false,
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
        assert_eq!(
            parse_claude_status(
                false,
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
            )
            .0,
            "unavailable"
        );
        assert_eq!(
            parse_codex_status(false, "Logged in using ChatGPT").0,
            "unavailable"
        );
    }

    #[test]
    fn logged_out_rows_name_the_context_the_login_command_and_the_registry_fix() {
        let declared = ProviderSpec {
            id: "claude-personal".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: Some(PathBuf::from("/profiles/claude-personal")),
            explicit_selector: true,
            registry_declared: true,
            source: "/registry.toml".to_string(),
        };
        assert_eq!(
            logged_out_detail(&declared),
            "no Claude login in /profiles/claude-personal; fix: af provider setup claude-personal --kind claude --auth-dir /profiles/claude-personal, or point auth_dir at the directory that holds the intended login"
        );
        let ambient = ProviderSpec {
            id: "claude-ambient".to_string(),
            kind: ProviderKind::Claude,
            auth_dir: None,
            explicit_selector: false,
            registry_declared: false,
            source: "ambient".to_string(),
        };
        assert_eq!(
            logged_out_detail(&ambient),
            "no Claude login in the CLI default directory; fix: claude auth login"
        );
        let codex = ProviderSpec {
            id: "codex-ambient".to_string(),
            kind: ProviderKind::Codex,
            auth_dir: Some(PathBuf::from("/profiles/codex")),
            explicit_selector: false,
            registry_declared: false,
            source: "ambient".to_string(),
        };
        assert_eq!(
            logged_out_detail(&codex),
            "no Codex login in /profiles/codex; fix: CODEX_HOME=/profiles/codex codex login"
        );
        // The admission continuation names the same command, so the two surfaces cannot drift.
        assert!(
            continuation_error(&declared, "op", 1).contains(&authentication_command(&declared))
        );
        let spaced = ProviderSpec {
            auth_dir: Some(PathBuf::from("/profiles/claude personal;work")),
            ..declared
        };
        assert_eq!(
            authentication_command(&spaced),
            "af provider setup claude-personal --kind claude --auth-dir '/profiles/claude personal;work'"
        );
        assert_eq!(
            chmod_fix(Path::new("/profiles/claude personal;work"), "go-w"),
            "chmod go-w -- '/profiles/claude personal;work'"
        );
    }

    #[test]
    fn logged_out_rows_point_at_logged_in_siblings_of_the_same_kind() {
        fn row(id: &str, kind: &str, context: &str, status: &str, plan: &str) -> ProviderStatus {
            ProviderStatus {
                id: id.to_string(),
                kind: kind.to_string(),
                auth_context: context.to_string(),
                source: String::new(),
                status: status.to_string(),
                auth_type: "-".to_string(),
                subscription: plan.to_string(),
                limits: Vec::new(),
                detail: if status == "not authenticated" {
                    "no login".to_string()
                } else {
                    String::new()
                },
            }
        }
        let mut providers = vec![
            row(
                "claude-ambient",
                "claude",
                "CLI default",
                "authenticated",
                "Claude Max",
            ),
            row(
                "claude-personal",
                "claude",
                "/profiles/stale",
                "not authenticated",
                "-",
            ),
            row(
                "claude-work",
                "claude",
                "/profiles/claude-work",
                "unavailable",
                "-",
            ),
            row(
                "codex-personal",
                "codex",
                "/profiles/codex",
                "authenticated",
                "ChatGPT Pro",
            ),
            row(
                "codex-work",
                "codex",
                "/profiles/codex-work",
                "not authenticated",
                "-",
            ),
        ];
        cross_reference_logged_in_siblings(&mut providers);
        assert_eq!(
            providers[1].detail,
            "no login; claude-ambient is authenticated in the CLI default directory (Claude Max)"
        );
        assert_eq!(
            providers[4].detail,
            "no login; codex-personal is authenticated in /profiles/codex (ChatGPT Pro)"
        );
        assert!(providers[0].detail.is_empty());
        assert!(providers[2].detail.is_empty());
        assert!(providers[3].detail.is_empty());

        let mut alone = vec![row(
            "claude-personal",
            "claude",
            "/profiles/stale",
            "not authenticated",
            "-",
        )];
        cross_reference_logged_in_siblings(&mut alone);
        assert_eq!(alone[0].detail, "no login");
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

/// `<config>/af/providers.toml`, or the pre-rename `<config>/afactory/providers.toml` while only that exists.
fn config_file(config: &Path, file: &str) -> PathBuf {
    let current = config.join("af").join(file);
    let legacy = config.join("afactory").join(file);
    if !current.exists() && legacy.exists() {
        eprintln!(
            "af: reading {}; move it to {} (the `afactory/` config directory is deprecated)",
            legacy.display(),
            current.display()
        );
        return legacy;
    }
    current
}
