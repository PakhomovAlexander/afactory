//! Frozen contracts for package-cache content manifests.

use serde::{Deserialize, Serialize};

use crate::{RunCacheKindV5, decode_path, encode_path};

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
            std::str::from_utf8(&decoded)
                .map_err(|_| "CacheManifest@1 path is not portable UTF-8")?;
            let valid_relative = !decoded.is_empty()
                && !decoded.starts_with(b"/")
                && !decoded.contains(&0)
                && decoded
                    .split(|byte| *byte == b'/')
                    .all(|component| !matches!(component, b"" | b"." | b".."));
            if !valid_relative || encode_path(&decoded) != entry.path {
                return Err("CacheManifest@1 contains a non-portable relative path".into());
            }
            bytes = bytes
                .checked_add(entry.size)
                .filter(|bytes| *bytes <= 9_007_199_254_740_991)
                .ok_or("CacheManifest@1 byte total exceeds the I-JSON safe range")?;
            previous = Some(&entry.path);
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.size).sum()
    }
}
