//! Private benchmark representation of an accepted native Task's exact source and candidate.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRequest {
    pub schema: String,
    pub receipt: TaskReceipt,
    pub baseline_snapshot_id: Option<String>,
    pub candidate_snapshot_id: String,
    pub control_sha: String,
    pub trusted_ref: String,
}
impl SourceRequest {
    /// The adapter protocol hashes canonical request JSON, independently of CAS content IDs.
    pub fn digest(&self) -> Result<String> {
        hash(self)
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-experiment-source-request@1",
            "Unsupported experiment source request",
        )?;
        Datum::Task(self.receipt.clone()).validate()?;
        require(
            self.baseline_snapshot_id
                .as_ref()
                .is_none_or(|s| af_contracts::is_digest(s))
                && af_contracts::is_digest(&self.candidate_snapshot_id),
            "Experiment sources require exact Snapshot identities",
        )?;
        require(
            self.control_sha.len() == 40
                && self
                    .control_sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && self.trusted_ref.starts_with("refs/heads/")
                && !self.trusted_ref.contains("..")
                && self
                    .trusted_ref
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
            "Experiment source needs exact installed control and a local trusted branch",
        )
    }
}

pub fn read_receipt(path: &std::path::Path) -> Result<SourceReceipt> {
    serde_json::from_slice(&super::store::read(path, 65536)?).map_err(|e| e.to_string())
}

pub fn isolated_destination(path: &std::path::Path) -> Result<()> {
    require(
        path.ancestors().all(|ancestor| {
            ancestor.join(".git").symlink_metadata().is_err()
                && !(ancestor.join("HEAD").is_file() && ancestor.join("objects").is_dir())
        }),
        "Experiment source capture must be outside every repository",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReceipt {
    pub schema: String,
    pub request_digest: String,
    pub repository_id: String,
    pub baseline_snapshot_id: String,
    pub candidate_snapshot_id: String,
    pub baseline_content_digest: String,
    pub candidate_content_digest: String,
    pub baseline_origin_id: String,
    pub candidate_origin_id: String,
    pub baseline_sha: String,
    pub candidate_sha: String,
    pub repository: PathBuf,
}
