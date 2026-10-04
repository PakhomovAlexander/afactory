//! Remote Checks (ADR-0136): the seam between the code check operator and a remote executor.
//!
//! The check operator partitions a Check node's checks, runs the local ones first, and calls
//! [`github_pr::run`] once with the candidate Snapshot, the remote phase's deadline and the
//! Attempt's cancellation flag. The executor returns one [`RemoteCheckOutcome`] per
//! remote-selected check; the operator records each as an `af/RemoteCheckEvidence@1` artifact
//! and a remote-shaped `CheckResult`. Nothing the executor keeps from a subprocess reaches a
//! record without passing through [`Redactor`].

mod gate;
pub mod github_pr;
pub mod mapping;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use review_check::{CheckDefinition, CheckStatus};
use review_core::task::remote_check::{
    MAX_REMOTE_DIAGNOSTIC_BYTES, RemoteCheckEvidenceV1, RemoteCheckReasonV1, RemoteCheckStateV1,
    RemoteCheckV1, RemoteCheckVerdictV1,
};

pub use github_pr::{GithubPrSettings, RemotePhase};
pub use mapping::{GithubPrTarget, MAPPING_KNOB, RemoteCheckMapping};

/// Resolves a Task ID to the Task's durable identity in its Store, the owner every gate commit
/// names. The coordinator supplies it; the identity must survive resume and differ between
/// Stores (ADR-0136 records which one).
pub type OwnerResolver = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// Machine-local Remote Check configuration the coordinator hands the code domain. Committed
/// policy contributes none of it.
#[derive(Clone, Default)]
pub struct RemoteCheckHost {
    /// The mapping file's path; `None` means no mapping, so every check runs locally.
    pub mapping: Option<PathBuf>,
    /// The Task owner resolver; a remote phase without one is a kernel error.
    pub owner: Option<Arc<OwnerResolver>>,
    pub github_pr: GithubPrSettings,
}

/// One remote-selected check of a Check node.
#[derive(Debug, Clone)]
pub struct RemoteCheckRequest {
    pub name: String,
    pub declaration: RemoteCheckV1,
}

/// What the executor concluded for one check: its evidence, and the human message a
/// `CheckResult` reason carries after the evidence's reason code.
#[derive(Debug, Clone)]
pub struct RemoteCheckOutcome {
    pub name: String,
    pub evidence: RemoteCheckEvidenceV1,
    pub message: Option<String>,
}

impl RemoteCheckOutcome {
    /// The status and `CheckResult` reason this outcome's evidence derives. A not-run reason
    /// begins with the reason's prefix, which is what the reader checks.
    pub fn result(&self, definition: &CheckDefinition) -> (CheckStatus, Option<String>) {
        match self.evidence.verdict() {
            RemoteCheckVerdictV1::Passed => (CheckStatus::Passed, None),
            RemoteCheckVerdictV1::Failed => (CheckStatus::Failed, self.message.clone()),
            RemoteCheckVerdictV1::NotRun(reason) => (
                CheckStatus::NotRun,
                Some(match &self.message {
                    Some(message) => format!("{}: {message}", reason.result_prefix()),
                    None => format!(
                        "{}: remote check `{}`",
                        reason.result_prefix(),
                        definition.name
                    ),
                }),
            ),
        }
    }
}

/// Whether a stored remote result's status and reason are the ones its evidence derives.
pub fn result_matches_evidence(
    status: CheckStatus,
    reason: Option<&str>,
    evidence: &RemoteCheckEvidenceV1,
) -> bool {
    match evidence.verdict() {
        RemoteCheckVerdictV1::Passed => status == CheckStatus::Passed,
        RemoteCheckVerdictV1::Failed => status == CheckStatus::Failed,
        RemoteCheckVerdictV1::NotRun(code) => {
            status == CheckStatus::NotRun
                && reason.is_some_and(|reason| {
                    reason
                        .strip_prefix(code.result_prefix())
                        .is_some_and(|rest| rest.starts_with(':'))
                })
        }
    }
}

/// The common fields of every evidence document for one check.
pub(crate) struct EvidenceBase<'a> {
    pub github: &'a str,
    pub snapshot_id: &'a str,
    pub source_snapshot_id: &'a str,
}

impl EvidenceBase<'_> {
    pub(crate) fn evidence(
        &self,
        declaration: &RemoteCheckV1,
        state: RemoteCheckStateV1,
        reason: Option<RemoteCheckReasonV1>,
    ) -> RemoteCheckEvidenceV1 {
        RemoteCheckEvidenceV1 {
            state,
            executor: declaration.executor,
            github: self.github.into(),
            workflow: declaration.workflow.clone(),
            required: declaration.required.clone(),
            snapshot_id: self.snapshot_id.into(),
            source_snapshot_id: self.source_snapshot_id.into(),
            observed_unix_ms: now_unix_ms(),
            reason,
            diagnostic: None,
            base_commit: None,
            head_commit: None,
            tree: None,
            pull_request: None,
            merge_commit: None,
            run: None,
            jobs: Vec::new(),
        }
    }

    /// A refusal that wrote nothing to the remote.
    pub(crate) fn refused(
        &self,
        request: &RemoteCheckRequest,
        reason: RemoteCheckReasonV1,
        message: String,
        diagnostic: Option<String>,
    ) -> RemoteCheckOutcome {
        let mut evidence = self.evidence(
            &request.declaration,
            RemoteCheckStateV1::Refused,
            Some(reason),
        );
        evidence.diagnostic = diagnostic.filter(|d| !d.is_empty());
        RemoteCheckOutcome {
            name: request.name.clone(),
            evidence,
            message: Some(message),
        }
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| (duration.as_millis() as u64).max(1))
}

/// Replaces every machine-local string a diagnostic may carry before it exists anywhere: the
/// exact push URL, the mapping path and the private repository's path. A kept diagnostic is
/// then bounded to 2 KiB.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    replacements: Vec<(String, &'static str)>,
}

impl Redactor {
    pub fn new(push_url: &str, mapping: Option<&Path>) -> Self {
        let mut redactor = Self::default();
        redactor.add(push_url.to_owned(), "<push-url>");
        if let Some(path) = mapping {
            redactor.add_path(path, "<mapping>");
        }
        redactor
    }

    pub(crate) fn add_path(&mut self, path: &Path, label: &'static str) {
        self.add(path.display().to_string(), label);
        if let Ok(canonical) = path.canonicalize() {
            self.add(canonical.display().to_string(), label);
        }
    }

    fn add(&mut self, text: String, label: &'static str) {
        if text.is_empty() || self.replacements.iter().any(|(known, _)| *known == text) {
            return;
        }
        self.replacements.push((text, label));
        // Longer first, so a path is never half-replaced by one of its prefixes.
        self.replacements
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    /// The text with every machine-local string replaced, unbounded: for kernel errors.
    pub fn apply(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (secret, label) in &self.replacements {
            text = text.replace(secret.as_str(), label);
        }
        text
    }

    /// A diagnostic as evidence may keep it: redacted, trimmed and bounded to 2 KiB on a
    /// character boundary. `None` when nothing is left.
    pub fn diagnostic(&self, raw: &[u8]) -> Option<String> {
        let text = self.apply(&String::from_utf8_lossy(raw));
        let text: String = text
            .chars()
            .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
            .collect();
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut end = text.len().min(MAX_REMOTE_DIAGNOSTIC_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Some(text[..end].trim_end().to_owned()).filter(|d| !d.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::remote_check::RemoteExecutorV1;

    #[test]
    fn redaction_replaces_the_push_url_and_paths_and_bounds_the_text() {
        let directory = tempfile::tempdir().unwrap();
        let mapping = directory.path().join("remote-checks.toml");
        let mut redactor = Redactor::new("/srv/git/gate.git", Some(&mapping));
        redactor.add_path(directory.path(), "<gate-repository>");
        let raw = format!(
            "To /srv/git/gate.git\n ! [remote rejected] (hook declined)\nerror: failed to push \
             some refs to '/srv/git/gate.git'\nmapping {} in {}",
            mapping.display(),
            directory.path().display()
        );
        let kept = redactor.diagnostic(raw.as_bytes()).unwrap();
        assert!(kept.contains("To <push-url>") && kept.contains("'<push-url>'"));
        assert!(kept.contains("mapping <mapping>") && kept.contains("in <gate-repository>"));
        assert!(
            !kept.contains("/srv/git") && !kept.contains(&directory.path().display().to_string())
        );
        let long = "é".repeat(MAX_REMOTE_DIAGNOSTIC_BYTES);
        let bounded = redactor.diagnostic(long.as_bytes()).unwrap();
        assert!(bounded.len() <= MAX_REMOTE_DIAGNOSTIC_BYTES);
        assert_eq!(redactor.diagnostic(b"  \n "), None);
    }

    #[test]
    fn a_stored_status_must_be_the_one_the_evidence_derives() {
        let base = EvidenceBase {
            github: "o/r",
            snapshot_id: &format!("sha256:{}", "1".repeat(64)),
            source_snapshot_id: &format!("sha256:{}", "2".repeat(64)),
        };
        let request = RemoteCheckRequest {
            name: "kernel".into(),
            declaration: RemoteCheckV1 {
                executor: RemoteExecutorV1::GithubPr,
                workflow: ".github/workflows/ci.yml".into(),
                required: vec!["lint".into()],
            },
        };
        let outcome = base.refused(
            &request,
            RemoteCheckReasonV1::RemoteSkippedLocalFailed,
            "fix the local failure".into(),
            None,
        );
        outcome.evidence.validate().unwrap();
        let definition = CheckDefinition::new(
            "kernel",
            review_check::Command::new("/bin/true", Vec::new()),
        );
        let (status, reason) = outcome.result(&definition);
        assert_eq!(status, CheckStatus::NotRun);
        assert!(result_matches_evidence(
            status,
            reason.as_deref(),
            &outcome.evidence
        ));
        assert!(!result_matches_evidence(
            CheckStatus::Passed,
            None,
            &outcome.evidence
        ));
        assert!(!result_matches_evidence(
            CheckStatus::NotRun,
            Some("remote_check_missing: no run"),
            &outcome.evidence
        ));
        assert!(!result_matches_evidence(
            CheckStatus::NotRun,
            Some("remote_skipped_local_failedX"),
            &outcome.evidence
        ));
    }
}
