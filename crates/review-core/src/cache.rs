//! Frozen contracts for package-cache content manifests.

use serde::{Deserialize, Serialize};

use crate::{RunCacheKindV5, decode_path, encode_path};

pub const MAX_CACHE_BYTES_V1: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_CACHE_COPY_BYTES_V1: u64 = 512 * 1024 * 1024;
pub const MAX_CACHE_ENTRIES_V1: u64 = 250_000;

const CREDENTIAL_COMPONENTS: &[&[u8]] = &[
    b"credentials",
    b"credentials.toml",
    b"credentials.json",
    b".git-credentials",
    b"config",
    b"config.toml",
    b".netrc",
    b".npmrc",
    b"token",
    b"tokens",
];

/// Validate one portable CacheManifest@1 path. Directories may stop at an admitted layout
/// prefix; files must be below `registry/cache` or `registry/index` so the manifest cannot claim
/// content the materializer would refuse.
pub fn validate_cache_path_v1(
    kind: RunCacheKindV5,
    path: &[u8],
    is_file: bool,
) -> Result<(), String> {
    std::str::from_utf8(path).map_err(|_| "CacheManifest@1 path is not portable UTF-8")?;
    if path.is_empty() || path.starts_with(b"/") || path.contains(&0) || path.contains(&b'\\') {
        return Err("CacheManifest@1 contains a non-portable relative path".into());
    }
    let components: Vec<&[u8]> = path.split(|byte| *byte == b'/').collect();
    if components
        .iter()
        .any(|component| matches!(*component, b"" | b"." | b".."))
    {
        return Err("CacheManifest@1 contains a non-portable relative path".into());
    }
    match kind {
        RunCacheKindV5::Cargo => {
            if components[0] != b"registry"
                || components
                    .get(1)
                    .is_some_and(|component| !matches!(*component, b"cache" | b"index"))
                || is_file && components.len() < 3
            {
                return Err(
                    "Cargo CacheManifest@1 path is outside registry/cache or registry/index".into(),
                );
            }
            if components.iter().any(|component| {
                CREDENTIAL_COMPONENTS
                    .iter()
                    .any(|credential| component.eq_ignore_ascii_case(credential))
            }) {
                return Err("Cargo CacheManifest@1 contains a credential-shaped path".into());
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePathEncodingV1 {
    PercentV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheManifestEntryV1 {
    pub path: String,
    pub content: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheManifestV1 {
    pub kind: RunCacheKindV5,
    pub path_encoding: CachePathEncodingV1,
    pub entries: Vec<CacheManifestEntryV1>,
}

impl CacheManifestV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.is_empty() {
            return Err("CacheManifest@1 must contain at least one file".into());
        }
        if u64::try_from(self.entries.len()).map_err(|_| "CacheManifest@1 entry count overflow")?
            > MAX_CACHE_ENTRIES_V1
        {
            return Err("CacheManifest@1 exceeds the kernel entry ceiling".into());
        }
        let mut previous: Option<&str> = None;
        let mut bytes = 0_u64;
        for entry in &self.entries {
            if previous.is_some_and(|previous| previous >= entry.path.as_str())
                || !crate::is_digest(&entry.content)
                || entry.size > 9_007_199_254_740_991
            {
                return Err("CacheManifest@1 entries must be sorted, unique, and bounded".into());
            }
            let decoded = decode_path(&entry.path);
            validate_cache_path_v1(self.kind, &decoded, true)?;
            if encode_path(&decoded) != entry.path {
                return Err("CacheManifest@1 contains a non-portable relative path".into());
            }
            bytes = bytes
                .checked_add(entry.size)
                .filter(|bytes| *bytes <= MAX_CACHE_BYTES_V1)
                .ok_or("CacheManifest@1 exceeds the kernel byte ceiling")?;
            previous = Some(&entry.path);
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.size).sum()
    }
}
