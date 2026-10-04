//! Remote Checks (ADR-0136): a declared code check that a machine may hand to a remote
//! executor, and the one typed record of everything the kernel observed while it did.
//!
//! The declaration is committed policy and grants nothing; the operator's machine-local
//! mapping selects it. The evidence is the only source a reader consults: replay derives a
//! remote check's status from it and never asks the remote again.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{present_option, require};
use crate::is_digest;

pub const REMOTE_CHECK_EVIDENCE_V1: &str = "af/RemoteCheckEvidence@1";

/// At most this many required job names per remote check.
pub const MAX_REMOTE_REQUIRED_JOBS: usize = 32;
/// Bound, in characters, of a job or step name.
pub const MAX_REMOTE_NAME_CHARS: usize = 128;
/// Bound, in bytes, of a kept and redacted subprocess diagnostic.
pub const MAX_REMOTE_DIAGNOSTIC_BYTES: usize = 2048;
/// At most this many unsuccessful steps are kept per job.
pub const MAX_REMOTE_STEPS: usize = 32;
const MAX_URL_BYTES: usize = 512;
const MAX_WORKFLOW_BYTES: usize = 255;

/// The closed set of remote executors. RC1 admits GitHub Actions pull-request runs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RemoteExecutorV1 {
    #[serde(rename = "github-pr")]
    GithubPr,
}

impl RemoteExecutorV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GithubPr => "github-pr",
        }
    }
}

/// The optional `remote` table of a declared check: the same check, run by another executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCheckV1 {
    pub executor: RemoteExecutorV1,
    /// The repository path of the workflow whose `pull_request` run earns the result.
    pub workflow: String,
    /// Job names exactly as the Actions jobs API reports them for that run.
    pub required: Vec<String>,
}

impl RemoteCheckV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_workflow_path(&self.workflow) {
            return Err(format!(
                "remote check workflow {:?} must be a `.github/workflows/<name>.yml` or `.yaml` \
                 repository path",
                self.workflow
            ));
        }
        if self.required.is_empty() || self.required.len() > MAX_REMOTE_REQUIRED_JOBS {
            return Err(format!(
                "remote check `required` lists 1 to {MAX_REMOTE_REQUIRED_JOBS} job names"
            ));
        }
        let mut seen = BTreeSet::new();
        for name in &self.required {
            if !is_remote_name(name) {
                return Err(format!(
                    "remote check job name {name:?} must be 1 to {MAX_REMOTE_NAME_CHARS} \
                     characters without control characters"
                ));
            }
            if !seen.insert(name) {
                return Err(format!("remote check job name {name:?} is listed twice"));
            }
        }
        Ok(())
    }
}

/// A workflow file GitHub Actions runs: `.github/workflows/<name>.yml|.yaml`, one level deep.
pub fn is_workflow_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    path.len() <= MAX_WORKFLOW_BYTES
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
        && !name.contains('/')
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A job or step name as kept in evidence.
pub fn is_remote_name(name: &str) -> bool {
    let chars = name.chars().count();
    (1..=MAX_REMOTE_NAME_CHARS).contains(&chars) && !name.chars().any(char::is_control)
}

fn is_commit(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_url(value: &str) -> bool {
    value.len() <= MAX_URL_BYTES
        && value.starts_with("https://")
        && !value.bytes().any(|b| b.is_ascii_control() || b == b' ')
}

fn is_github_repository(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    let part = |part: &str| {
        (1..=100).contains(&part.len())
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    part(owner) && part(name)
}

/// The `owner/name` GitHub repository spelling a mapping and the evidence use.
pub fn is_github_name(value: &str) -> bool {
    is_github_repository(value)
}

fn is_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if bytes.len() < 20 || bytes.len() > 30 || bytes[bytes.len() - 1] != b'Z' {
        return false;
    }
    let fraction = &bytes[19..bytes.len() - 1];
    digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19)
        && (fraction.is_empty()
            || (fraction[0] == b'.'
                && fraction.len() > 1
                && fraction[1..].iter().all(u8::is_ascii_digit)))
}

fn is_conclusion(value: &str) -> bool {
    (1..=32).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Every named reason a remote check did not pass or fail (ADR-0136 §3.6), plus the deadline
/// and cancellation every check shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCheckReasonV1 {
    RemoteSkippedLocalFailed,
    RemoteCandidateChangesCi,
    RemoteToolUnavailable,
    RemoteRefInvalid,
    RemoteRefConflict,
    RemotePushRefused,
    RemotePrRefused,
    RemoteCheckMissing,
    RemoteCheckAmbiguous,
    RemoteMergeMismatch,
    RemoteCheckInconclusive,
    /// The remote phase ended before the check's jobs completed.
    DeadlineExpired,
    /// The Attempt was cancelled during the remote phase.
    Cancelled,
}

impl RemoteCheckReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RemoteSkippedLocalFailed => "remote_skipped_local_failed",
            Self::RemoteCandidateChangesCi => "remote_candidate_changes_ci",
            Self::RemoteToolUnavailable => "remote_tool_unavailable",
            Self::RemoteRefInvalid => "remote_ref_invalid",
            Self::RemoteRefConflict => "remote_ref_conflict",
            Self::RemotePushRefused => "remote_push_refused",
            Self::RemotePrRefused => "remote_pr_refused",
            Self::RemoteCheckMissing => "remote_check_missing",
            Self::RemoteCheckAmbiguous => "remote_check_ambiguous",
            Self::RemoteMergeMismatch => "remote_merge_mismatch",
            Self::RemoteCheckInconclusive => "remote_check_inconclusive",
            Self::DeadlineExpired => "deadline_expired",
            Self::Cancelled => "cancelled",
        }
    }

    /// How a `CheckResult` reason for this cause begins. The deadline uses the words a local
    /// check uses, so a reader sees one deadline whatever executed the check.
    pub const fn result_prefix(self) -> &'static str {
        match self {
            Self::DeadlineExpired => "Task check deadline expired",
            Self::Cancelled => "check cancelled",
            other => other.as_str(),
        }
    }

    /// Whether this cause can leave the remote untouched by this check.
    const fn refusable(self) -> bool {
        matches!(
            self,
            Self::RemoteSkippedLocalFailed
                | Self::RemoteCandidateChangesCi
                | Self::RemoteToolUnavailable
                | Self::RemoteRefInvalid
                | Self::RemoteRefConflict
                | Self::RemotePushRefused
                | Self::DeadlineExpired
                | Self::Cancelled
        )
    }

    /// Whether this cause can occur once the gate branches exist.
    const fn publishable(self) -> bool {
        matches!(
            self,
            Self::RemoteToolUnavailable
                | Self::RemotePrRefused
                | Self::RemoteCheckMissing
                | Self::RemoteCheckAmbiguous
                | Self::RemoteMergeMismatch
                | Self::DeadlineExpired
                | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCheckStateV1 {
    /// Nothing was written to the remote for this check.
    Refused,
    /// The gate branches (and perhaps the pull request) exist; no job decided the check.
    Published,
    /// Jobs decided the check: passed, failed or inconclusive.
    Observed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePullRequestV1 {
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunV1 {
    pub id: u64,
    /// The run attempt whose jobs were read: always the latest one observed.
    pub attempt: u64,
    pub workflow: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteStepV1 {
    pub name: String,
    pub conclusion: String,
}

/// One required job the decision relied on. Its log is not evidence: the tail of an
/// unsuccessful job's log is the check result's `stdout`, and the whole log stays at
/// `url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteJobV1 {
    pub id: u64,
    pub name: String,
    pub conclusion: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub started_at: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub completed_at: Option<String>,
    pub url: String,
    /// The steps that did not succeed, when the job did not succeed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<RemoteStepV1>,
}

impl RemoteJobV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            self.id > 0
                && self.id <= crate::json::SAFE_INTEGER_MAX as u64
                && is_remote_name(&self.name)
                && is_conclusion(&self.conclusion)
                && self.started_at.as_deref().is_none_or(is_timestamp)
                && self.completed_at.as_deref().is_none_or(is_timestamp)
                && is_url(&self.url)
                && self.steps.len() <= MAX_REMOTE_STEPS
                && (self.conclusion != "success" || self.steps.is_empty())
                && self
                    .steps
                    .iter()
                    .all(|step| is_remote_name(&step.name) && is_conclusion(&step.conclusion)),
            "Remote check job requires an identity, a bounded name, a conclusion, a URL and \
             bounded unsuccessful steps",
        )
    }
}

/// What a remote check's evidence derives: the only status a stored remote result may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCheckVerdictV1 {
    Passed,
    Failed,
    NotRun(RemoteCheckReasonV1),
}

/// `af/RemoteCheckEvidence@1`: every remote fact one remote check's decision used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCheckEvidenceV1 {
    pub state: RemoteCheckStateV1,
    pub executor: RemoteExecutorV1,
    pub github: String,
    pub workflow: String,
    pub required: Vec<String>,
    pub snapshot_id: String,
    pub source_snapshot_id: String,
    pub observed_unix_ms: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub reason: Option<RemoteCheckReasonV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub diagnostic: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub base_commit: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub head_commit: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub tree: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub pull_request: Option<RemotePullRequestV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub merge_commit: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub run: Option<RemoteRunV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<RemoteJobV1>,
}

impl RemoteCheckEvidenceV1 {
    /// The declaration this evidence was gathered for.
    pub fn declaration(&self) -> RemoteCheckV1 {
        RemoteCheckV1 {
            executor: self.executor,
            workflow: self.workflow.clone(),
            required: self.required.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.declaration().validate()?;
        require(
            is_github_repository(&self.github)
                && is_digest(&self.snapshot_id)
                && is_digest(&self.source_snapshot_id)
                && self.observed_unix_ms > 0
                && self.observed_unix_ms <= crate::json::SAFE_INTEGER_MAX as u64
                && self.diagnostic.as_deref().is_none_or(|diagnostic| {
                    !diagnostic.is_empty() && diagnostic.len() <= MAX_REMOTE_DIAGNOSTIC_BYTES
                }),
            "Remote check evidence requires a GitHub repository, exact Snapshots, an observation \
             time and a bounded diagnostic",
        )?;
        let published = self.base_commit.is_some() && self.head_commit.is_some();
        require(
            self.base_commit.as_deref().is_none_or(is_commit)
                && self.head_commit.as_deref().is_none_or(is_commit)
                && self.tree.as_deref().is_none_or(is_commit)
                && self.merge_commit.as_deref().is_none_or(is_commit)
                && self.pull_request.as_ref().is_none_or(|pull| {
                    pull.number > 0
                        && pull.number <= crate::json::SAFE_INTEGER_MAX as u64
                        && is_url(&pull.url)
                })
                && self.run.as_ref().is_none_or(|run| {
                    run.id > 0
                        && run.id <= crate::json::SAFE_INTEGER_MAX as u64
                        && run.attempt > 0
                        && run.attempt <= crate::json::SAFE_INTEGER_MAX as u64
                        && run.workflow == self.workflow
                }),
            "Remote check evidence names malformed commits, pull request or run",
        )?;
        match self.state {
            RemoteCheckStateV1::Refused => require(
                self.reason.is_some_and(RemoteCheckReasonV1::refusable)
                    && self.base_commit.is_none()
                    && self.head_commit.is_none()
                    && self.tree.is_none()
                    && self.pull_request.is_none()
                    && self.merge_commit.is_none()
                    && self.run.is_none()
                    && self.jobs.is_empty(),
                "Refused remote check evidence carries a refusal reason and nothing published",
            ),
            RemoteCheckStateV1::Published => require(
                self.reason.is_some_and(RemoteCheckReasonV1::publishable)
                    && published
                    && self.tree.is_some()
                    && self.merge_commit.is_none()
                    && self.run.is_none()
                    && self.jobs.is_empty(),
                "Published remote check evidence carries a reason, both gate commits and the \
                 tree, and no observed run",
            ),
            RemoteCheckStateV1::Observed => {
                require(
                    published
                        && self.tree.is_some()
                        && self.pull_request.is_some()
                        && self.merge_commit.is_some()
                        && self.run.is_some()
                        && self.diagnostic.is_none()
                        && self.jobs.len() == self.required.len()
                        && self
                            .jobs
                            .iter()
                            .zip(&self.required)
                            .all(|(job, name)| job.name == *name),
                    "Observed remote check evidence names the pull request, merge commit, run \
                     and exactly its required jobs in declared order",
                )?;
                for job in &self.jobs {
                    job.validate()?;
                }
                let failed = self.jobs.iter().any(|job| job.conclusion == "failure");
                let passed = self.jobs.iter().all(|job| job.conclusion == "success");
                require(
                    if failed || passed {
                        self.reason.is_none()
                    } else {
                        self.reason == Some(RemoteCheckReasonV1::RemoteCheckInconclusive)
                    },
                    "Observed remote check evidence states inconclusive exactly when no required \
                     job failed and not every one succeeded",
                )
            }
        }
    }

    /// The status this evidence derives. Call [`Self::validate`] first.
    pub fn verdict(&self) -> RemoteCheckVerdictV1 {
        match (self.state, self.reason) {
            (RemoteCheckStateV1::Observed, None)
                if self.jobs.iter().any(|job| job.conclusion == "failure") =>
            {
                RemoteCheckVerdictV1::Failed
            }
            (RemoteCheckStateV1::Observed, None) => RemoteCheckVerdictV1::Passed,
            (_, Some(reason)) => RemoteCheckVerdictV1::NotRun(reason),
            // Unreachable for validated evidence: refused and published both require a reason.
            (_, None) => RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteCheckMissing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn job(name: &str, conclusion: &str) -> RemoteJobV1 {
        RemoteJobV1 {
            id: 7,
            name: name.into(),
            conclusion: conclusion.into(),
            started_at: Some("2026-10-04T10:00:00Z".into()),
            completed_at: Some("2026-10-04T10:12:00Z".into()),
            url: "https://github.com/o/r/actions/runs/1/job/7".into(),
            steps: if conclusion == "success" {
                vec![]
            } else {
                vec![RemoteStepV1 {
                    name: "Run tests".into(),
                    conclusion: conclusion.into(),
                }]
            },
        }
    }

    fn observed(conclusions: &[&str]) -> RemoteCheckEvidenceV1 {
        let required: Vec<String> = (0..conclusions.len()).map(|i| format!("job {i}")).collect();
        RemoteCheckEvidenceV1 {
            state: RemoteCheckStateV1::Observed,
            executor: RemoteExecutorV1::GithubPr,
            github: "o/r".into(),
            workflow: ".github/workflows/ci.yml".into(),
            required: required.clone(),
            snapshot_id: digest('1'),
            source_snapshot_id: digest('2'),
            observed_unix_ms: 1,
            reason: None,
            diagnostic: None,
            base_commit: Some("a".repeat(40)),
            head_commit: Some("b".repeat(40)),
            tree: Some("c".repeat(40)),
            pull_request: Some(RemotePullRequestV1 {
                number: 1,
                url: "https://github.com/o/r/pull/1".into(),
            }),
            merge_commit: Some("d".repeat(40)),
            run: Some(RemoteRunV1 {
                id: 9,
                attempt: 2,
                workflow: ".github/workflows/ci.yml".into(),
            }),
            jobs: required
                .iter()
                .zip(conclusions)
                .map(|(name, conclusion)| job(name, conclusion))
                .collect(),
        }
    }

    #[test]
    fn the_declaration_is_bounded() {
        let mut declaration = RemoteCheckV1 {
            executor: RemoteExecutorV1::GithubPr,
            workflow: ".github/workflows/ci.yml".into(),
            required: vec!["validation / lint".into()],
        };
        declaration.validate().unwrap();
        for workflow in [
            "ci.yml",
            ".github/workflows/ci.txt",
            ".github/workflows/sub/ci.yml",
            ".github/workflows/../ci.yml",
            ".github/workflows/.yml",
        ] {
            declaration.workflow = workflow.into();
            assert!(declaration.validate().is_err(), "{workflow}");
        }
        declaration.workflow = ".github/workflows/ci.yaml".into();
        for required in [
            vec![],
            vec!["a".to_string(), "a".to_string()],
            vec!["x".repeat(129)],
            vec!["tab\there".into()],
            (0..33).map(|i| i.to_string()).collect(),
        ] {
            declaration.required = required.clone();
            assert!(declaration.validate().is_err(), "{required:?}");
        }
        declaration.required = vec!["é".repeat(128)];
        declaration.validate().unwrap();
    }

    #[test]
    fn observed_evidence_derives_its_status() {
        let passed = observed(&["success", "success"]);
        passed.validate().unwrap();
        assert_eq!(passed.verdict(), RemoteCheckVerdictV1::Passed);
        let failed = observed(&["success", "failure"]);
        failed.validate().unwrap();
        assert_eq!(failed.verdict(), RemoteCheckVerdictV1::Failed);
        let mut inconclusive = observed(&["success", "cancelled"]);
        assert!(inconclusive.validate().is_err(), "needs its reason");
        inconclusive.reason = Some(RemoteCheckReasonV1::RemoteCheckInconclusive);
        inconclusive.validate().unwrap();
        assert_eq!(
            inconclusive.verdict(),
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteCheckInconclusive)
        );
        let mut claimed = observed(&["success"]);
        claimed.reason = Some(RemoteCheckReasonV1::RemoteCheckInconclusive);
        assert!(claimed.validate().is_err(), "a pass cannot claim a reason");
        let mut reordered = observed(&["success", "success"]);
        reordered.jobs.reverse();
        assert!(reordered.validate().is_err());
        let mut stepped = observed(&["success"]);
        stepped.jobs[0].steps.push(RemoteStepV1 {
            name: "x".into(),
            conclusion: "failure".into(),
        });
        assert!(
            stepped.validate().is_err(),
            "a successful job keeps no steps"
        );
    }

    #[test]
    fn each_state_requires_and_forbids_its_fields() {
        let mut refused = observed(&["success"]);
        refused.state = RemoteCheckStateV1::Refused;
        refused.reason = Some(RemoteCheckReasonV1::RemoteRefConflict);
        assert!(refused.validate().is_err(), "refused publishes nothing");
        refused.base_commit = None;
        refused.head_commit = None;
        refused.tree = None;
        refused.pull_request = None;
        refused.merge_commit = None;
        refused.run = None;
        refused.jobs.clear();
        refused.validate().unwrap();
        assert_eq!(
            refused.verdict(),
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteRefConflict)
        );
        refused.reason = Some(RemoteCheckReasonV1::RemoteMergeMismatch);
        assert!(refused.validate().is_err(), "a merge mismatch needs a push");
        refused.reason = None;
        assert!(refused.validate().is_err());

        let mut published = observed(&["success"]);
        published.state = RemoteCheckStateV1::Published;
        published.reason = Some(RemoteCheckReasonV1::RemoteCheckMissing);
        assert!(published.validate().is_err(), "no run once only published");
        published.merge_commit = None;
        published.run = None;
        published.jobs.clear();
        published.validate().unwrap();
        published.diagnostic = Some("x".repeat(MAX_REMOTE_DIAGNOSTIC_BYTES + 1));
        assert!(published.validate().is_err());
        published.diagnostic = None;
        published.reason = Some(RemoteCheckReasonV1::RemoteSkippedLocalFailed);
        assert!(published.validate().is_err());
    }
}
