//! Machine-local, token-free Provider suspension. Registry v1 remains unchanged.
use super::*;
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "af.provider-disables/1";
const LABEL: &str = "Provider disable state";
const MAX_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expiry {
    pub unix_seconds: u64,
    pub nanoseconds: u32,
}

impl Expiry {
    fn from_time(time: SystemTime) -> Result<Self, String> {
        let time = time
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "wall clock precedes Unix epoch")?;
        Ok(Self {
            unix_seconds: time.as_secs(),
            nanoseconds: time.subsec_nanos(),
        })
    }

    fn time(self) -> Result<SystemTime, String> {
        if self.nanoseconds >= 1_000_000_000 {
            return Err("invalid Provider disable expiry nanoseconds".into());
        }
        UNIX_EPOCH
            .checked_add(Duration::new(self.unix_seconds, self.nanoseconds))
            .ok_or_else(|| "Provider disable expiry overflows the wall clock".into())
    }

    pub(super) fn diagnostic(self, id: &str) -> String {
        format!(
            "Provider `{id}` is disabled until Unix {}.{:09} (UTC); run `af provider enable {id}` to enable immediately",
            self.unix_seconds, self.nanoseconds
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    expires_at: Expiry,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    providers: Vec<Entry>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: SCHEMA.into(),
            providers: Vec::new(),
        }
    }
}

impl State {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let state: Self =
            serde_json::from_slice(bytes).map_err(|_| "malformed Provider disable state")?;
        if state.schema != SCHEMA || state.providers.len() > MAX_PROVIDERS {
            return Err("unsupported or oversized Provider disable state".into());
        }
        let mut ids = BTreeSet::new();
        for entry in &state.providers {
            validate_explicit_id(&entry.id).map_err(|_| "invalid Provider disable ID")?;
            if entry.id.len() > 64 || !ids.insert(&entry.id) {
                return Err("invalid or duplicate Provider disable ID".into());
            }
            entry.expires_at.time()?;
        }
        Ok(state)
    }

    fn disabled_until(&self, id: &str, now: SystemTime) -> Result<Option<Expiry>, String> {
        for entry in &self.providers {
            if entry.id == id && now < entry.expires_at.time()? {
                return Ok(Some(entry.expires_at));
            }
        }
        Ok(None)
    }
}

pub(super) fn duration(value: &str) -> Result<Duration, String> {
    let error =
        || "--for requires a positive integer followed by s, m, h or d, at most 30d".to_string();
    let split = value.len().checked_sub(1).ok_or_else(error)?;
    let (number, unit) = value.split_at_checked(split).ok_or_else(error)?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error());
    }
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(error()),
    };
    let seconds = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| (1..=MAX_SECONDS).contains(n))
        .ok_or_else(error)?;
    Ok(Duration::from_secs(seconds))
}

fn path() -> Result<PathBuf, String> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(config) => PathBuf::from(config),
        None => PathBuf::from(
            std::env::var_os("HOME").ok_or("HOME is required for Provider disable state")?,
        )
        .join(".config"),
    };
    if !config.is_absolute()
        || config
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("Provider disable state requires an absolute config directory without parent components".into());
    }
    Ok(config.join("af/provider-disables-v1.json"))
}

fn read(path: &Path, directory: &File) -> Result<State, String> {
    let name = path
        .file_name()
        .ok_or("Provider disable state has no filename")?;
    // Distinguish absence from a dangling link or another hostile file type.
    match rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => {
            if !bound_directory_is_current(path.parent().unwrap(), directory).unwrap_or(false) {
                return Err("Provider disable state directory changed while reading".into());
            }
            return Ok(State::default());
        }
        Err(error) => return Err(format!("cannot inspect {LABEL}: {error}")),
        Ok(_) => {}
    }
    let file = open_regular_file_at(directory, name, path, LABEL)?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err("Provider disable state must be a private (0600), singly linked file".into());
    }
    let state = State::parse(&bounded_file_bytes(&file, path, LABEL)?)?;
    if !same_file_at(directory, name, &file) {
        return Err("Provider disable state changed while reading; retry".into());
    }
    if !bound_directory_is_current(path.parent().unwrap(), directory).unwrap_or(false) {
        return Err("Provider disable state directory changed while reading".into());
    }
    Ok(state)
}

fn load_at(path: &Path) -> Result<State, String> {
    let parent = path
        .parent()
        .ok_or("Provider disable state has no parent")?;
    match fs::symlink_metadata(parent) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let (_, ancestor) = missing_directories(parent, LABEL)?;
            validate_creation_ancestor(&ancestor, LABEL)?;
            return Ok(State::default());
        }
        Err(error) => return Err(format!("cannot inspect {LABEL} directory: {error}")),
        Ok(_) => {}
    }
    let directory = bind_directory(parent, LABEL)?;
    read(path, &directory)
}

#[cfg(test)]
fn until_at(path: &Path, id: &str, now: SystemTime) -> Result<Option<Expiry>, String> {
    load_at(path)?.disabled_until(id, now)
}

pub(super) fn until(id: &str) -> Result<Option<Expiry>, String> {
    load_at(&path()?)?.disabled_until(id, SystemTime::now())
}

pub(crate) fn check_id(id: &str) -> Result<(), String> {
    match until(id)? {
        Some(expiry) => Err(expiry.diagnostic(id)),
        None => Ok(()),
    }
}

pub(super) fn check(spec: &ProviderSpec) -> Result<(), String> {
    check_id(&spec.id)
}

fn update(
    path: &Path,
    id: &str,
    duration: Option<Duration>,
    clock: impl FnOnce() -> SystemTime,
) -> Result<Option<Expiry>, String> {
    let lock = registry_lock(path)?;
    let now = clock();
    let mut state = read(path, &lock.directory)?;
    // Expired entries have no authority and are reclaimed only during explicit updates.
    state
        .providers
        .retain(|e| e.id != id && e.expires_at.time().is_ok_and(|at| now < at));
    let expiry = duration
        .map(|duration| {
            now.checked_add(duration)
                .ok_or_else(|| "Provider disable expiry overflows the wall clock".to_string())
                .and_then(Expiry::from_time)
        })
        .transpose()?;
    if let Some(expires_at) = expiry {
        if state.providers.len() >= MAX_PROVIDERS {
            return Err("Provider disable state is full".into());
        }
        state.providers.push(Entry {
            id: id.into(),
            expires_at,
        });
    }
    state.providers.sort_by(|a, b| a.id.cmp(&b.id));
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    State::parse(&bytes)?;
    let parent = path.parent().unwrap();
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    lock.ensure_directory_current(parent, LABEL)?;
    // Descriptor-relative rename publishes a complete file; no destination link is followed.
    rustix::fs::renameat(
        &lock.directory,
        temporary.path().file_name().unwrap(),
        &lock.directory,
        path.file_name().unwrap(),
    )
    .map_err(|e| format!("publishing {LABEL}: {e}"))?;
    // Publication is committed. Durability errors cannot claim the visible update failed.
    if let Err(error) = lock.directory.sync_all() {
        eprintln!("warning: {LABEL} committed but directory sync failed: {error}");
    }
    Ok(expiry)
}

pub fn disable(id: &str, value: &str) -> Result<i32, String> {
    let duration = duration(value)?;
    validate_explicit_id(id)?;
    registered_spec(id)?;
    let expiry = update(&path()?, id, Some(duration), SystemTime::now)?.unwrap();
    println!("{}", expiry.diagnostic(id));
    Ok(EXIT_OK)
}

pub fn enable(id: &str) -> Result<i32, String> {
    validate_explicit_id(id)?;
    registered_spec(id)?;
    update(&path()?, id, None, SystemTime::now)?;
    println!("Provider `{id}` enabled; next: af provider status");
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests;
