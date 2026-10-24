//! What sealing left out of a candidate (ADR-0201), `af.seal-ignored/1`: the paths a Worker
//! added that the project's ignore rules exclude (`.gitignore`, `.af/seal-ignore` and the
//! kernel defaults). The record is referenced from the `af/CandidateTree@1` envelope it belongs
//! to, beside the candidate's Manifest, and only when something was left out.

use serde::{Deserialize, Serialize};

use super::require;
use crate::is_digest;

pub const SEAL_IGNORED_V1: &str = "af.seal-ignored/1";
/// The most paths one record names; `ignored_count` keeps the whole number.
pub const MAX_NAMED_IGNORED_PATHS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealIgnoredV1 {
    pub schema: String,
    /// The candidate Manifest the paths were left out of.
    pub manifest_id: String,
    /// Every path left out, up to [`MAX_NAMED_IGNORED_PATHS`], sorted.
    pub ignored: Vec<String>,
    pub ignored_count: u64,
}

impl SealIgnoredV1 {
    pub fn new(manifest_id: String, mut ignored: Vec<String>) -> Self {
        ignored.sort();
        ignored.dedup();
        let ignored_count = ignored.len() as u64;
        ignored.truncate(MAX_NAMED_IGNORED_PATHS);
        Self {
            schema: SEAL_IGNORED_V1.into(),
            manifest_id,
            ignored,
            ignored_count,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == SEAL_IGNORED_V1
                && is_digest(&self.manifest_id)
                && !self.ignored.is_empty()
                && self.ignored.len() <= MAX_NAMED_IGNORED_PATHS
                && self.ignored.windows(2).all(|pair| pair[0] < pair[1])
                && self
                    .ignored
                    .iter()
                    .all(|path| !path.is_empty() && path.len() <= 4096 && !path.starts_with('/'))
                && self.ignored_count >= self.ignored.len() as u64
                && (self.ignored.len() == MAX_NAMED_IGNORED_PATHS
                    || self.ignored_count == self.ignored.len() as u64)
                && super::safe_number(self.ignored_count),
            "a seal-ignored record names its Manifest and its sorted, bounded ignored paths",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seal_ignored_record_is_sorted_bounded_and_counts_everything() {
        let manifest = format!("sha256:{}", "a".repeat(64));
        let record = SealIgnoredV1::new(
            manifest.clone(),
            vec!["b/__pycache__/x.pyc".into(), "a.pyc".into(), "a.pyc".into()],
        );
        record.validate().unwrap();
        assert_eq!(record.ignored, ["a.pyc", "b/__pycache__/x.pyc"]);
        let many = SealIgnoredV1::new(
            manifest,
            (0..1500).map(|n| format!("gen/{n:05}.pyc")).collect(),
        );
        many.validate().unwrap();
        assert_eq!((many.ignored.len(), many.ignored_count), (1000, 1500));
    }
}
