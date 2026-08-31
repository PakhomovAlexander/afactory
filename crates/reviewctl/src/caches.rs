//! Resolve symbolic pipeline cache requests through bounded machine-local policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use review_config::CacheKindSpec;
use review_sandbox::{CacheError, CacheErrorKind, CacheKind, CacheLimits, CacheSource};
use serde::Deserialize;

const MAX_POLICY_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    cache: BTreeMap<String, Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    source: PathBuf,
    max_bytes: u64,
    max_files: u64,
    max_copy_bytes: u64,
}

pub fn resolve(requested: &[CacheKindSpec]) -> Result<BTreeMap<CacheKind, CacheSource>, String> {
    if requested.is_empty() {
        return Ok(BTreeMap::new());
    }
    let path = policy_path()?.ok_or_else(|| {
        "Gate requests a cache but neither HOME nor XDG_CONFIG_HOME can locate machine-local cache policy"
            .to_string()
    })?;
    let source = read_policy(&path)?.ok_or_else(|| {
        format!(
            "Gate requests a cache but machine-local policy {} does not exist",
            path.display()
        )
    })?;
    parse_policy(&source, requested)
        .map_err(|error| format!("invalid cache policy {}: {error}", path.display()))
}

pub fn resolve_kind(kind: CacheKind) -> Result<CacheSource, CacheError> {
    let requested = match kind {
        CacheKind::Cargo => CacheKindSpec::Cargo,
    };
    resolve(&[requested])
        .map_err(|error| CacheError::new(CacheErrorKind::PolicyUnavailable, error))?
        .remove(&kind)
        .ok_or_else(|| {
            CacheError::new(
                CacheErrorKind::PolicyUnavailable,
                format!("requested cache `{}` was not resolved", kind.name()),
            )
        })
}

fn policy_path() -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("AFACTORY_CACHE_POLICY_FILE") {
        if path.is_empty() {
            return Err("AFACTORY_CACHE_POLICY_FILE is empty".into());
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("AFACTORY_CACHE_POLICY_FILE must be absolute".into());
        }
        return Ok(Some(path));
    }
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        if !root.is_empty() {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err("XDG_CONFIG_HOME must be absolute".into());
            }
            return Ok(Some(root.join("afactory/caches.toml")));
        }
    }
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/afactory/caches.toml"));
    if path.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err("HOME must be absolute to locate cache policy".into());
    }
    Ok(path)
}

fn read_policy(path: &Path) -> Result<Option<String>, String> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot resolve cache policy {}: {error}",
                path.display()
            ));
        }
    };
    let file = open_policy(&resolved)
        .map_err(|error| format!("cannot read cache policy {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect cache policy {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "cache policy {} must resolve to a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_POLICY_BYTES {
        return Err(format!(
            "cache policy {} exceeds {MAX_POLICY_BYTES} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read cache policy {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(format!(
            "cache policy {} changed while reading or exceeds {MAX_POLICY_BYTES} bytes",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("cache policy {} is not UTF-8", path.display()))
}

#[cfg(unix)]
fn open_policy(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_policy(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

fn parse_policy(
    source: &str,
    requested: &[CacheKindSpec],
) -> Result<BTreeMap<CacheKind, CacheSource>, String> {
    let policy: Policy = toml::from_str(source).map_err(|error| error.to_string())?;
    if policy.version != 1 {
        return Err(format!(
            "unsupported cache policy version {}; expected 1",
            policy.version
        ));
    }
    for key in policy.cache.keys() {
        if key != "cargo" {
            return Err(format!("unsupported cache kind `{key}`"));
        }
    }
    let mut result = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for requested in requested {
        let kind = match requested {
            CacheKindSpec::Cargo => CacheKind::Cargo,
        };
        if !seen.insert(kind) {
            return Err(format!("duplicate cache request `{}`", kind.name()));
        }
        let entry = policy
            .cache
            .get(kind.name())
            .ok_or_else(|| format!("requested cache `{}` has no machine mapping", kind.name()))?;
        if !entry.source.is_absolute() {
            return Err(format!(
                "{} cache source must be an absolute path",
                kind.name()
            ));
        }
        let limits = CacheLimits {
            max_bytes: entry.max_bytes,
            max_files: entry.max_files,
            max_copy_bytes: entry.max_copy_bytes,
        };
        limits.validate()?;
        result.insert(
            kind,
            CacheSource {
                kind,
                source: entry.source.clone(),
                limits,
            },
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"
version = 1

[cache.cargo]
source = "/var/cache/afactory/cargo"
max_bytes = 1048576
max_files = 1000
max_copy_bytes = 524288
"#;

    #[test]
    fn resolves_only_requested_symbolic_cache() {
        let resolved = parse_policy(POLICY, &[CacheKindSpec::Cargo]).unwrap();
        let cargo = resolved.get(&CacheKind::Cargo).unwrap();
        assert_eq!(cargo.source, PathBuf::from("/var/cache/afactory/cargo"));
        assert_eq!(cargo.limits.max_copy_bytes, 524_288);
    }

    #[test]
    fn rejects_unknown_kind_relative_source_and_missing_mapping() {
        assert!(
            parse_policy(&POLICY.replace("cargo]", "npm]"), &[CacheKindSpec::Cargo])
                .unwrap_err()
                .contains("unsupported cache kind")
        );
        assert!(
            parse_policy(
                &POLICY.replace("/var/cache/afactory/cargo", "relative"),
                &[CacheKindSpec::Cargo]
            )
            .unwrap_err()
            .contains("absolute path")
        );
        assert!(
            parse_policy("version = 1\ncache = {}\n", &[CacheKindSpec::Cargo])
                .unwrap_err()
                .contains("no machine mapping")
        );
    }
}
