//! Phase 1 CI evidence. Absence is unknown; a diagnosis grants no mutation authority.
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SNAPSHOT_TYPE: &str = "custom/CiHealthSnapshot@1";
pub const DIAGNOSIS_TYPE: &str = "custom/CiDiagnosis@1";
pub const SNAPSHOT_SCHEMA: &str = "af.ci-health-snapshot/1";
pub const DIAGNOSIS_SCHEMA: &str = "af.ci-diagnosis/1";
pub const FINGERPRINT_RULE: &str = "af.ci-fingerprint/1";
fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
pub const MAX_BYTES: usize = 65_536;
fn need(ok: bool, why: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why.into()) }
}
fn text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.contains('\0')
}
fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn positive(n: u64) -> bool {
    n > 0 && n <= crate::json::SAFE_INTEGER_MAX as u64
}
fn timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 20
        || !s.starts_with("20")
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
        || b.iter()
            .enumerate()
            .any(|(i, c)| ![4, 7, 10, 13, 16, 19].contains(&i) && !c.is_ascii_digit())
    {
        return false;
    }
    let number = |a: usize, z: usize| s[a..z].parse::<u32>().unwrap_or(u32::MAX);
    let year = number(0, 4);
    let month = number(5, 7);
    let days = match month {
        2 if year.is_multiple_of(4) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    (1..=days).contains(&number(8, 10))
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}
fn bounded(value: &impl Serialize) -> Result<(), String> {
    need(
        serde_json::to_vec(value).map_err(|e| e.to_string())?.len() <= MAX_BYTES,
        "CI evidence exceeds 64 KiB",
    )
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePr {
    pub number: u64,
    pub head_sha: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NightlyReceipt {
    pub identity: String,
    pub conclusion: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub occurrence_id: String,
    pub signature_id: String,
    pub workflow_id: u64,
    pub workflow_file: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub job_id: u64,
    pub job_name: String,
    pub platform: String,
    pub head_sha: String,
    pub conclusion: String,
    #[serde(deserialize_with = "required_option")]
    pub normalized_fingerprint: Option<String>,
    pub log_artifacts: Vec<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub occurrence_count: u64,
    pub release_blocking: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: String,
    pub repository: String,
    pub captured_at: String,
    /// Observed main head. A selected release occurrence can have a different `head_sha`.
    pub source_sha: String,
    #[serde(deserialize_with = "required_option")]
    pub release_pr: Option<ReleasePr>,
    #[serde(deserialize_with = "required_option")]
    pub nightly_receipt: Option<NightlyReceipt>,
    pub complete: bool,
    pub omissions: Vec<String>,
    pub failures: Vec<Failure>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        bounded(self)?;
        let parts: Vec<_> = self.repository.split('/').collect();
        need(
            self.schema == SNAPSHOT_SCHEMA
                && parts.len() == 2
                && parts.iter().all(|s| {
                    !s.is_empty()
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                })
                && timestamp(&self.captured_at)
                && sha(&self.source_sha),
            "Invalid CI snapshot identity",
        )?;
        need(
            self.release_pr
                .as_ref()
                .is_none_or(|p| positive(p.number) && sha(&p.head_sha))
                && self
                    .nightly_receipt
                    .as_ref()
                    .is_none_or(|r| is_digest(&r.identity) && text(&r.conclusion, 128)),
            "Invalid CI receipt",
        )?;
        need(
            (!self.complete || self.nightly_receipt.is_some())
                && self.complete == self.omissions.is_empty()
                && self.omissions.len() <= 100
                && self.omissions.iter().all(|s| text(s, 256))
                && self.omissions.iter().collect::<BTreeSet<_>>().len() == self.omissions.len()
                && self.failures.len() <= 100,
            "CI snapshot completeness or bounds differ",
        )?;
        let mut seen = BTreeSet::new();
        for f in &self.failures {
            need(
                is_digest(&f.occurrence_id)
                    && is_digest(&f.signature_id)
                    && seen.insert(&f.occurrence_id)
                    && [
                        f.workflow_id,
                        f.run_id,
                        f.run_attempt,
                        f.job_id,
                        f.occurrence_count,
                    ]
                    .into_iter()
                    .all(positive)
                    && text(&f.workflow_file, 512)
                    && text(&f.job_name, 256)
                    && text(&f.platform, 128)
                    && sha(&f.head_sha)
                    && [
                        "failure",
                        "timed_out",
                        "cancelled",
                        "startup_failure",
                        "action_required",
                    ]
                    .contains(&f.conclusion.as_str())
                    && f.normalized_fingerprint
                        .as_ref()
                        .is_none_or(|s| text(s, 2048))
                    && f.log_artifacts.len() <= 20
                    && f.log_artifacts.iter().all(|s| is_digest(s))
                    && f.log_artifacts.iter().collect::<BTreeSet<_>>().len()
                        == f.log_artifacts.len()
                    && timestamp(&f.first_seen_at)
                    && timestamp(&f.last_seen_at)
                    && f.first_seen_at <= f.last_seen_at
                    && f.last_seen_at <= self.captured_at,
                "Invalid CI failure evidence",
            )?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Transient,
    ProductBug,
    TestBug,
    CiInfra,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub snapshot_id: String,
    /// The selected failure's head, not the enclosing observation's main head.
    pub source_sha: String,
    pub reference: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnosis {
    pub schema: String,
    pub snapshot_id: String,
    pub source_artifact_ids: Vec<String>,
    pub occurrence_id: String,
    pub signature_id: String,
    /// Exact failed head delivered in the captured Selection Requirements.
    pub source_sha: String,
    pub category: Category,
    pub rationale: String,
    pub evidence: Vec<Evidence>,
}
impl Diagnosis {
    pub fn validate(&self) -> Result<(), String> {
        bounded(self)?;
        need(
            self.schema == DIAGNOSIS_SCHEMA
                && is_digest(&self.snapshot_id)
                && self.source_artifact_ids == [self.snapshot_id.clone()]
                && is_digest(&self.occurrence_id)
                && is_digest(&self.signature_id)
                && sha(&self.source_sha)
                && text(&self.rationale, 4096)
                && !self.evidence.is_empty()
                && self.evidence.len() <= 20
                && self.evidence.iter().collect::<BTreeSet<_>>().len() == self.evidence.len()
                && self.evidence.iter().all(|e| {
                    e.snapshot_id == self.snapshot_id
                        && e.source_sha == self.source_sha
                        && text(&e.reference, 256)
                }),
            "Invalid CI diagnosis",
        )
    }
    pub fn validate_against(&self, id: &str, snapshot: &Snapshot) -> Result<(), String> {
        self.validate()?;
        snapshot.validate()?;
        let f = snapshot
            .failures
            .iter()
            .find(|f| f.occurrence_id == self.occurrence_id)
            .ok_or("CI diagnosis occurrence absent from snapshot")?;
        need(
            self.snapshot_id == id && self.source_sha == f.head_sha,
            "CI diagnosis source mismatch",
        )?;
        need(
            self.signature_id == f.signature_id
                && (f.normalized_fingerprint.is_some() || self.category == Category::Unknown)
                && self
                    .evidence
                    .iter()
                    .all(|e| e.reference == "snapshot" || f.log_artifacts.contains(&e.reference)),
            "CI diagnosis references evidence outside selected snapshot occurrence",
        )
    }
}

/// Immutable informational selection in a diagnosis Task's captured Requirements specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub schema: String,
    pub occurrence_id: String,
    pub signature_id: String,
    /// Exact selected occurrence's `head_sha`; this Task has no repository checkout.
    pub source_sha: String,
}
impl Selection {
    pub fn validate(&self, snapshot: &Snapshot) -> Result<(), String> {
        snapshot.validate()?;
        need(
            self.schema == "af.ci-diagnosis-selection/1"
                && snapshot.failures.iter().any(|f| {
                    f.occurrence_id == self.occurrence_id
                        && f.signature_id == self.signature_id
                        && self.source_sha == f.head_sha
                }),
            "CI diagnosis selection does not name an exact snapshot occurrence",
        )
    }
}
