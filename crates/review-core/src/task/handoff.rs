//! The findings handoff (ADR-0248): what an agent that will fix a review finding needs, beside
//! the finding itself, in `af/task-findings@2` and `af/review-report@5`.
//!
//! - [`FindingPromptV1`]: one self-contained prompt per finding — its location on the reviewed
//!   Snapshot, the quoted evidence, the requested change, and how verification will judge the
//!   fix — as fields and as one rendered text.
//! - [`CoverageV1`]: what a round reviewed, per reviewer (`reviewed`, or `not_run` with why) and
//!   per path (`reviewed`, `partially_reviewed`, `not_reviewed`, `outside_round`), so a reviewer
//!   that never ran is never read as one that found nothing.
//! - [`VerifiedFixV1`]: a finding verified fixed, only ever with the verification record that
//!   says so and the Snapshot it was verified on.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::require;
use crate::is_digest;

/// The longest text one field carries; the writer cuts longer text, the reader refuses it.
pub const MAX_PROMPT_TEXT: usize = 65_536;

fn bounded(text: &str) -> bool {
    text.chars().count() <= MAX_PROMPT_TEXT && !text.trim().is_empty()
}

/// `text` trimmed and cut to [`MAX_PROMPT_TEXT`] characters, the last one an ellipsis when cut.
pub fn cut(text: &str) -> String {
    let clean = text.trim();
    if clean.chars().count() <= MAX_PROMPT_TEXT {
        clean.to_owned()
    } else {
        let mut cut: String = clean.chars().take(MAX_PROMPT_TEXT - 1).collect();
        cut.push('…');
        cut
    }
}

/// A reviewer or path name a coverage entry carries: present and bounded.
fn named(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096
}

/// Where a finding is: a file and line, or neither for a change-wide finding, on the Snapshot
/// the round reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingLocationV1 {
    #[serde(deserialize_with = "Option::deserialize")]
    pub file: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub line: Option<i64>,
    pub snapshot_id: String,
}

/// Which review path judges a fix: a later round of the same Task, or the Campaign's next
/// Round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedBy {
    Task,
    Campaign,
}

/// One finding, handed to the agent that will fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingPromptV1 {
    pub location: FindingLocationV1,
    /// The reviewer's quoted evidence.
    pub evidence: String,
    /// The reviewer's proposed fix, or, when it proposed none, what is asked instead.
    pub requested_change: String,
    /// How a fix will be judged: only a verification record on a later Snapshot marks it fixed.
    pub verification: String,
    /// The fields above as one prompt an agent can be given as it is.
    pub text: String,
}

/// What a finding says, as the prompt builder reads it.
pub struct PromptSource<'a> {
    pub reviewer: &'a str,
    pub severity: crate::Severity,
    pub title: &'a str,
    pub file: Option<&'a str>,
    pub line: Option<i64>,
    pub snapshot_id: &'a str,
    pub evidence: &'a str,
    pub fix: Option<&'a str>,
}

/// The request when the reviewer proposed no change.
pub const NO_PROPOSED_CHANGE: &str = "The reviewer proposed no specific change: remove the \
defect the evidence describes, or answer it with evidence that it is not one.";

/// How a later round of the same Task judges a fix (ADR-0212's repair assessment).
pub const TASK_VERIFICATION: &str = "A later review round of this Task reviews the next \
candidate Snapshot. The finding counts as verified fixed only when a fix verification record \
(af/TaskFixReceipt@1) with a positive outcome names it on that Snapshot and the Task's checks \
pass there; a reviewer that reports it again keeps it open. A change that only touches the file \
is not proof of a fix.";

/// How the Campaign's next Round judges a fix.
pub const CAMPAIGN_VERIFICATION: &str = "The Campaign's next Round reviews the new head \
Snapshot. The finding counts as verified fixed only when a fixed Resolution is backed by a \
positive FixVerification@1 on that Round's Subject; a reviewer that reports it again reopens it. \
A change that only touches the file is not proof of a fix.";

fn severity_word(severity: crate::Severity) -> &'static str {
    match severity {
        crate::Severity::Blocker => "blocker",
        crate::Severity::Major => "major",
        crate::Severity::Minor => "minor",
    }
}

/// The prompt of one finding. Every field is bounded to [`MAX_PROMPT_TEXT`].
pub fn prompt(source: &PromptSource<'_>, by: VerifiedBy) -> FindingPromptV1 {
    let file = source
        .file
        .map(str::trim)
        .filter(|file| !file.is_empty())
        .map(cut);
    let line = source.line.filter(|line| *line > 0 && file.is_some());
    let place = match (&file, line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.clone(),
        _ => "the change as a whole (no single location)".to_owned(),
    };
    let evidence = match cut(source.evidence) {
        empty if empty.is_empty() => "(no evidence recorded)".to_owned(),
        evidence => evidence,
    };
    let requested_change = source
        .fix
        .map(cut)
        .filter(|fix| !fix.is_empty())
        .unwrap_or_else(|| NO_PROPOSED_CHANGE.to_owned());
    let verification = match by {
        VerifiedBy::Task => TASK_VERIFICATION,
        VerifiedBy::Campaign => CAMPAIGN_VERIFICATION,
    }
    .to_owned();
    let text = cut(&format!(
        "Fix this {} review finding from the reviewer {}.\nTitle: {}\nLocation: {place} on \
         Snapshot {}\nEvidence: {evidence}\nRequested change: {requested_change}\nVerification: \
         {verification}",
        severity_word(source.severity),
        source.reviewer,
        source.title.trim(),
        source.snapshot_id,
    ));
    FindingPromptV1 {
        location: FindingLocationV1 {
            file,
            line,
            snapshot_id: source.snapshot_id.to_owned(),
        },
        evidence,
        requested_change,
        verification,
        text,
    }
}

impl FindingPromptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.location.snapshot_id)
                && self.location.file.as_deref().is_none_or(bounded)
                && self.location.line.is_none_or(|line| line > 0)
                && (self.location.file.is_some() || self.location.line.is_none())
                && [
                    &self.evidence,
                    &self.requested_change,
                    &self.verification,
                    &self.text,
                ]
                .into_iter()
                .all(|text| bounded(text)),
            "A finding prompt names its Snapshot, a line only with its file, and bounded \
             evidence, change, verification and text",
        )
    }
}

/// What the round reviewed: its Subject's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSubjectV1 {
    /// The round reviewed a change: the paths it changed.
    Diff,
    /// The round reviewed the whole tree.
    WholeTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerCoverageStateV1 {
    /// The reviewer ran and its result is the round's: no findings from it means none found.
    Reviewed,
    /// The reviewer recorded no result for the round: no findings from it means nothing.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerCoverageV1 {
    pub reviewer: String,
    pub state: ReviewerCoverageStateV1,
    /// Why a reviewer did not run; null for one that did.
    #[serde(deserialize_with = "Option::deserialize")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathCoverageStateV1 {
    /// In the round, and every reviewer of the round ran.
    Reviewed,
    /// In the round, and only some of its reviewers ran.
    PartiallyReviewed,
    /// In the round, and none of its reviewers ran.
    NotReviewed,
    /// A path a finding names that the round's Subject does not contain.
    OutsideRound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathCoverageV1 {
    pub path: String,
    pub state: PathCoverageStateV1,
}

/// What one round covered. `paths` are the paths a diff round changed, then any path a finding
/// names outside them; a whole-tree round lists the paths its findings name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageV1 {
    pub subject: CoverageSubjectV1,
    pub reviewers: Vec<ReviewerCoverageV1>,
    pub paths: Vec<PathCoverageV1>,
}

/// The state every path inside the round has, from its reviewers' states.
fn in_round(reviewers: &[ReviewerCoverageV1]) -> PathCoverageStateV1 {
    let ran = reviewers
        .iter()
        .filter(|reviewer| reviewer.state == ReviewerCoverageStateV1::Reviewed)
        .count();
    match ran {
        0 => PathCoverageStateV1::NotReviewed,
        ran if ran == reviewers.len() => PathCoverageStateV1::Reviewed,
        _ => PathCoverageStateV1::PartiallyReviewed,
    }
}

/// A round's coverage: `changed` is the Subject's changed paths, `None` for a whole-tree round;
/// `named` the paths its findings name.
pub fn coverage<'a>(
    changed: Option<&[String]>,
    reviewers: BTreeMap<String, Option<String>>,
    named: impl IntoIterator<Item = &'a str>,
) -> CoverageV1 {
    let reviewers: Vec<ReviewerCoverageV1> = reviewers
        .into_iter()
        .map(|(reviewer, reason)| ReviewerCoverageV1 {
            reviewer,
            state: if reason.is_some() {
                ReviewerCoverageStateV1::NotRun
            } else {
                ReviewerCoverageStateV1::Reviewed
            },
            reason: reason.map(|reason| cut(&reason)),
        })
        .collect();
    let inside = in_round(&reviewers);
    let mut paths: BTreeMap<String, PathCoverageStateV1> = BTreeMap::new();
    if let Some(changed) = changed {
        for path in changed {
            paths.insert(path.clone(), inside);
        }
    }
    for path in named
        .into_iter()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let state = if changed.is_some() {
            PathCoverageStateV1::OutsideRound
        } else {
            inside
        };
        paths.entry(path.to_owned()).or_insert(state);
    }
    CoverageV1 {
        subject: if changed.is_some() {
            CoverageSubjectV1::Diff
        } else {
            CoverageSubjectV1::WholeTree
        },
        reviewers,
        paths: paths
            .into_iter()
            .map(|(path, state)| PathCoverageV1 { path, state })
            .collect(),
    }
}

impl CoverageV1 {
    /// Whether every reviewer of the round ran.
    pub fn complete(&self) -> bool {
        self.reviewers
            .iter()
            .all(|reviewer| reviewer.state == ReviewerCoverageStateV1::Reviewed)
    }

    pub fn not_run(&self) -> impl Iterator<Item = &ReviewerCoverageV1> {
        self.reviewers
            .iter()
            .filter(|reviewer| reviewer.state == ReviewerCoverageStateV1::NotRun)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        for reviewer in &self.reviewers {
            require(
                named(&reviewer.reviewer)
                    && names.insert(reviewer.reviewer.as_str())
                    && match reviewer.state {
                        ReviewerCoverageStateV1::Reviewed => reviewer.reason.is_none(),
                        ReviewerCoverageStateV1::NotRun => {
                            reviewer.reason.as_deref().is_some_and(bounded)
                        }
                    },
                "A coverage reviewer is named once, with a reason exactly when it did not run",
            )?;
        }
        require(
            self.reviewers
                .windows(2)
                .all(|pair| pair[0].reviewer < pair[1].reviewer)
                && self
                    .paths
                    .windows(2)
                    .all(|pair| pair[0].path < pair[1].path)
                && self.paths.iter().all(|path| named(&path.path)),
            "Coverage reviewers and paths are named, sorted and unique",
        )?;
        let inside = in_round(&self.reviewers);
        require(
            self.paths.iter().all(|path| {
                path.state == inside
                    || (path.state == PathCoverageStateV1::OutsideRound
                        && self.subject == CoverageSubjectV1::Diff)
            }),
            "A path inside the round has the state its reviewers give it; only a diff round has \
             paths outside it",
        )
    }
}

/// A finding verified fixed: the verification record that says so and the Snapshot it was
/// verified on. Nothing else is ever reported as verified fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedFixV1 {
    pub finding_id: String,
    /// The positive verification record: an `af/TaskFixReceipt@1` on the Task path, a
    /// `FixVerification@1` on the Campaign path.
    pub record_id: String,
    pub snapshot_id: String,
}

impl VerifiedFixV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            named(&self.finding_id) && is_digest(&self.record_id) && is_digest(&self.snapshot_id),
            "A verified fix names its finding, its exact verification record and Snapshot",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    #[test]
    fn a_prompt_names_location_evidence_change_and_verification() {
        let snapshot = id('a');
        let fixed = prompt(
            &PromptSource {
                reviewer: "bugs",
                severity: crate::Severity::Major,
                title: "Reject negative offset",
                file: Some("pagination.py"),
                line: Some(3),
                snapshot_id: &snapshot,
                evidence: "A negative offset slices from the end.",
                fix: Some("Raise ValueError."),
            },
            VerifiedBy::Task,
        );
        fixed.validate().unwrap();
        assert_eq!(fixed.location.file.as_deref(), Some("pagination.py"));
        assert_eq!(fixed.requested_change, "Raise ValueError.");
        assert_eq!(fixed.verification, TASK_VERIFICATION);
        for needle in [
            "major review finding from the reviewer bugs",
            "Location: pagination.py:3 on Snapshot sha256:aaaa",
            "Evidence: A negative offset slices from the end.",
            "Requested change: Raise ValueError.",
            "af/TaskFixReceipt@1",
        ] {
            assert!(fixed.text.contains(needle), "{needle}: {}", fixed.text);
        }
        // A change-wide finding without a proposed fix still asks for something exact.
        let wide = prompt(
            &PromptSource {
                reviewer: "correctness",
                severity: crate::Severity::Minor,
                title: "Document it",
                file: None,
                line: Some(4),
                snapshot_id: &snapshot,
                evidence: " ",
                fix: None,
            },
            VerifiedBy::Campaign,
        );
        wide.validate().unwrap();
        assert_eq!(wide.location.line, None);
        assert_eq!(wide.requested_change, NO_PROPOSED_CHANGE);
        assert_eq!(wide.evidence, "(no evidence recorded)");
        assert!(wide.text.contains("the change as a whole"), "{}", wide.text);
        assert!(wide.verification.contains("FixVerification@1"));
    }

    /// A reviewer that never ran is `not_run` with its reason, and the paths it should have
    /// read are not `reviewed`; a path a finding names outside a diff round is `outside_round`.
    #[test]
    fn a_reviewer_that_never_ran_is_not_reviewed_rather_than_clean() {
        let changed = vec!["a.rs".to_owned(), "b.rs".to_owned()];
        let partial = coverage(
            Some(&changed),
            BTreeMap::from([
                ("bugs".to_owned(), None),
                (
                    "correctness".to_owned(),
                    Some("its Attempt failed".to_owned()),
                ),
            ]),
            ["a.rs", "docs/x.md"],
        );
        partial.validate().unwrap();
        assert!(!partial.complete());
        assert_eq!(partial.subject, CoverageSubjectV1::Diff);
        assert_eq!(
            partial
                .not_run()
                .map(|r| r.reviewer.as_str())
                .collect::<Vec<_>>(),
            ["correctness"]
        );
        let states: Vec<_> = partial
            .paths
            .iter()
            .map(|path| (path.path.as_str(), path.state))
            .collect();
        assert_eq!(
            states,
            [
                ("a.rs", PathCoverageStateV1::PartiallyReviewed),
                ("b.rs", PathCoverageStateV1::PartiallyReviewed),
                ("docs/x.md", PathCoverageStateV1::OutsideRound),
            ]
        );
        let none = coverage(
            None,
            BTreeMap::from([("bugs".to_owned(), Some("never dispatched".to_owned()))]),
            ["a.rs"],
        );
        none.validate().unwrap();
        assert_eq!(none.paths[0].state, PathCoverageStateV1::NotReviewed);
        let all = coverage(None, BTreeMap::from([("bugs".to_owned(), None)]), []);
        all.validate().unwrap();
        assert!(all.complete() && all.paths.is_empty());

        // A reviewed path beside a reviewer that did not run, or a reason on one that did, is
        // refused.
        let mut lying = partial.clone();
        lying.paths[0].state = PathCoverageStateV1::Reviewed;
        assert!(lying.validate().is_err());
        let mut lying = partial;
        lying.reviewers[0].reason = Some("ran".into());
        assert!(lying.validate().is_err());
    }
}
