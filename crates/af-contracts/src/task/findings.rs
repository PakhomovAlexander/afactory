//! The records a continued Task carries: what it continues, the findings it was handed, and the
//! files its Worker is told to read first.
//!
//! A Task planned with `af task run NEW --from TASK` (or a Task file's `continue` table) reads
//! the reviewed candidate of `TASK` as its source and receives one `af/TaskReviewFindings@1`
//! artifact: every report of the continued review round's selected reviewer results and the
//! evaluator's verdict, each under a stable ID the implementer answers by. The artifact is typed input data,
//! never authority: no acceptance, verification or budget crosses the Task boundary through it
//! (ADR-0212). `af/TaskReadFirst@2` names the tree the Task's Worker reads and, by content digest
//! in that tree, the standing rules and the context files a Task file points at, so a changed file
//! is a changed plan and a listed file is one the Worker's tree holds (ADR-0254).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require};
use crate::finding::Severity;
use crate::is_digest;

pub const TASK_REVIEW_FINDINGS_V1: &str = "af/TaskReviewFindings@1";
pub const TASK_REVIEW_FINDINGS_SCHEMA: &str = "af.task-review-findings/1";
/// The read-first record of an earlier release: it named no tree, so its rules could be another
/// Snapshot's than the Worker read. Never written; a reader refuses it by this name (ADR-0254).
pub const TASK_READ_FIRST_V1: &str = "af/TaskReadFirst@1";
pub const TASK_READ_FIRST_V2: &str = "af/TaskReadFirst@2";
pub const TASK_BASE_V1: &str = "af/TaskBase@1";
pub const TASK_BASE_SCHEMA: &str = "af.task-base/1";
pub const TASK_READ_FIRST_SCHEMA: &str = "af.task-read-first/2";

/// The most findings one continuation carries: 64 reviewers of 16 rounds' worth of reports
/// would never fit a Worker's context, and a bound keeps the artifact admissible.
pub const MAX_FINDINGS: usize = 512;
/// The most context entries one Task file may name.
pub const MAX_CONTEXT: usize = 32;
const MAX_TEXT: usize = 65536;
const MAX_LINE: usize = 1024;

fn bounded(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max
}

fn one_line(text: &str) -> bool {
    bounded(text, MAX_LINE) && !text.chars().any(char::is_control)
}

/// The Task a continuation reads from, and exactly which of its rounds: the reviewed candidate
/// Snapshot of that round is the new Task's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContinuesV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub result_id: String,
    pub round: u32,
    /// The `af/TaskReviewRound@1` artifact the findings were read from.
    pub round_id: String,
    /// The candidate that round reviewed.
    pub snapshot_id: String,
}

impl TaskContinuesV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id)
                && [
                    &self.task_revision_id,
                    &self.result_id,
                    &self.round_id,
                    &self.snapshot_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                && (1..=16).contains(&self.round),
            "A continuation names a valid Task ID, an exact revision, result, round and \
             Snapshot, and a round from 1 to 16",
        )
    }
}

/// Who said it: one reviewer of the round, or the evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReviewFindingOriginV1 {
    Reviewer,
    Evaluator,
}

/// One item the implementer resolves or disputes, under the ID its reply names it by.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewFindingV1 {
    /// `bugs-1`, `correctness-2`, `evaluator`: the reviewer's slot and the report's position,
    /// counted from one, or the evaluator.
    pub id: String,
    pub origin: TaskReviewFindingOriginV1,
    /// The reviewer slot, or `evaluator`.
    pub by: String,
    /// The `review.kernel/ReviewerResult@2` or `af/TaskEvaluation@1` artifact it came from.
    pub artifact_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub severity: Option<Severity>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub file: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub line: Option<u64>,
    pub title: String,
    pub body: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub fix: Option<String>,
}

impl TaskReviewFindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        let origin = match self.origin {
            TaskReviewFindingOriginV1::Reviewer => {
                is_name(&self.by) && self.by != "evaluator" && self.severity.is_some()
            }
            TaskReviewFindingOriginV1::Evaluator => {
                self.by == "evaluator"
                    && self.id == "evaluator"
                    && self.severity.is_none()
                    && self.file.is_none()
                    && self.line.is_none()
            }
        };
        require(
            origin
                && is_name(&self.id)
                && is_digest(&self.artifact_id)
                && self.file.as_deref().is_none_or(one_line)
                && one_line(&self.title)
                && bounded(&self.body, MAX_TEXT)
                && self.fix.as_deref().is_none_or(|fix| bounded(fix, MAX_TEXT)),
            "A finding names its ID, who said it and the artifact it came from, a one-line \
             title and a bounded body; a reviewer's finding has a severity, the evaluator's \
             names no place",
        )
    }
}

/// `af/TaskReviewFindings@1`: the findings a continued Task's implementer receives on its
/// `findings` port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewFindingsV1 {
    pub schema: String,
    pub continues: TaskContinuesV1,
    pub findings: Vec<TaskReviewFindingV1>,
    /// Required reviewers the round recorded no result for; what they would have said is not
    /// in the Store, so it never reads as nothing to fix.
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "nonempty_set"
    )]
    pub missing_reviewers: BTreeSet<String>,
}

/// `missing_reviewers` is absent or names one: an explicit empty list would be a second spelling
/// of a round every reviewer answered.
fn nonempty_set<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeSet<String>, D::Error> {
    let set = super::unique_set::<'de, D, String>(deserializer)?;
    if set.is_empty() {
        return Err(serde::de::Error::custom(
            "`missing_reviewers` is absent or names a reviewer",
        ));
    }
    Ok(set)
}

impl TaskReviewFindingsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_REVIEW_FINDINGS_SCHEMA,
            "Task findings require schema af.task-review-findings/1",
        )?;
        self.continues.validate()?;
        require(
            self.findings.len() <= MAX_FINDINGS
                && self.missing_reviewers.len() <= 64
                && self.missing_reviewers.iter().all(|name| is_name(name)),
            "Task findings hold at most 512 findings and 64 named missing reviewers",
        )?;
        let mut ids = BTreeSet::new();
        for finding in &self.findings {
            finding.validate()?;
            require(
                ids.insert(finding.id.as_str()),
                "Each finding has its own ID",
            )?;
        }
        Ok(())
    }
}

/// One file a Worker reads first, as the Task file named it and as the record's tree holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReadFirstEntryV2 {
    /// What the Task file wrote: a repository path, optionally with a `#section` anchor.
    pub reference: String,
    /// The repository path the reference names.
    pub path: String,
    /// The content digest of that path in the record's tree.
    pub digest: String,
}

impl TaskReadFirstEntryV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            one_line(&self.reference)
                && one_line(&self.path)
                && self.reference.starts_with(self.path.as_str())
                && is_digest(&self.digest),
            "A read-first entry names its reference, the repository path it names and that \
             file's digest",
        )
    }
}

/// `af/TaskReadFirst@2`: the standing rules and context a Worker is told to read before the
/// goal, each bound by the digest of its bytes in one named tree: the Snapshot the Task's Worker
/// reads (ADR-0254). The rules are that tree's `.af/rules.md`, so a continued Task reads the
/// rules of the candidate it continues, never those of another Snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReadFirstV2 {
    pub schema: String,
    /// The Snapshot every entry was read from: the tree the Task's Worker reads.
    pub snapshot_id: String,
    /// `.af/rules.md` of that tree, when it holds one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub rules: Option<TaskReadFirstEntryV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<TaskReadFirstEntryV2>,
}

impl TaskReadFirstV2 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_READ_FIRST_SCHEMA && is_digest(&self.snapshot_id),
            "A read-first record requires schema af.task-read-first/2 and the Snapshot its \
             files were read from",
        )?;
        require(
            (self.rules.is_some() || !self.context.is_empty()) && self.context.len() <= MAX_CONTEXT,
            "A read-first record names the rules or one to 32 context files",
        )?;
        if let Some(rules) = &self.rules {
            rules.validate()?;
        }
        let mut seen = BTreeSet::new();
        for entry in &self.context {
            entry.validate()?;
            require(
                seen.insert(entry.reference.as_str()),
                "Each context reference is named once",
            )?;
        }
        Ok(())
    }

    /// The lines the Worker's requirements text ends with: the tree they were read from, then
    /// the rules, then each context file, each with the digest of its bytes in that tree.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Read first (captured by digest from Snapshot {}, the tree this Task reads; it holds \
             these bytes):",
            self.snapshot_id
        )];
        if let Some(rules) = &self.rules {
            lines.push(format!(
                "- {} ({}) — the project's standing rules",
                rules.reference, rules.digest
            ));
        }
        for entry in &self.context {
            lines.push(format!("- {} ({})", entry.reference, entry.digest));
        }
        lines
    }
}

/// `af/TaskBase@1`: the tree before the change a Task's judging Workers read at `../base`, when
/// its Task file names one with `"base": {"commit": "<sha>"}` instead of leaving it to the
/// source Snapshot's recorded base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBaseV1 {
    pub schema: String,
    /// The commit as Git resolved it: 40 or 64 lowercase hexadecimal digits.
    pub commit: String,
    pub snapshot_id: String,
}

impl TaskBaseV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_BASE_SCHEMA
                && matches!(self.commit.len(), 40 | 64)
                && self
                    .commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && is_digest(&self.snapshot_id),
            "A Task base names af.task-base/1, a full commit ID and the Snapshot captured from it",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn continues() -> TaskContinuesV1 {
        TaskContinuesV1 {
            task_id: "p11-round-1".into(),
            task_revision_id: id('1'),
            result_id: id('2'),
            round: 1,
            round_id: id('3'),
            snapshot_id: id('4'),
        }
    }

    fn reviewer(n: usize) -> TaskReviewFindingV1 {
        TaskReviewFindingV1 {
            id: format!("bugs-{n}"),
            origin: TaskReviewFindingOriginV1::Reviewer,
            by: "bugs".into(),
            artifact_id: id('5'),
            severity: Some(Severity::Major),
            file: Some("src/lib.rs".into()),
            line: Some(7),
            title: "Off by one".into(),
            body: "The last item is dropped.".into(),
            fix: Some("Use an inclusive bound.".into()),
        }
    }

    #[test]
    fn findings_name_each_item_once_and_say_who_said_it() {
        let mut findings = TaskReviewFindingsV1 {
            schema: TASK_REVIEW_FINDINGS_SCHEMA.into(),
            continues: continues(),
            findings: vec![
                reviewer(1),
                TaskReviewFindingV1 {
                    id: "evaluator".into(),
                    origin: TaskReviewFindingOriginV1::Evaluator,
                    by: "evaluator".into(),
                    artifact_id: id('6'),
                    severity: None,
                    file: None,
                    line: None,
                    title: "Evaluator verdict: failed".into(),
                    body: "Pagination ignores the offset.".into(),
                    fix: None,
                },
            ],
            missing_reviewers: BTreeSet::new(),
        };
        findings.validate().unwrap();
        findings.findings.push(reviewer(1));
        assert!(findings.validate().is_err(), "a duplicate ID is refused");
        findings.findings.pop();
        findings.findings[1].file = Some("src/lib.rs".into());
        assert!(findings.validate().is_err(), "the evaluator names no place");
        findings.findings[1].file = None;
        findings.findings[0].severity = None;
        assert!(
            findings.validate().is_err(),
            "a reviewer's finding has a severity"
        );
        findings.findings[0].severity = Some(Severity::Minor);
        findings.continues.round = 0;
        assert!(findings.validate().is_err(), "rounds count from one");
    }

    #[test]
    fn read_first_lists_the_rules_before_the_context() {
        let entry = |reference: &str, path: &str| TaskReadFirstEntryV2 {
            reference: reference.into(),
            path: path.into(),
            digest: id('a'),
        };
        let record = TaskReadFirstV2 {
            schema: TASK_READ_FIRST_SCHEMA.into(),
            snapshot_id: id('b'),
            rules: Some(entry(".af/rules.md", ".af/rules.md")),
            context: vec![entry("docs/design.md#items", "docs/design.md")],
        };
        record.validate().unwrap();
        let lines = record.lines();
        assert!(
            lines[0].contains(&id('b')),
            "the tree is named: {}",
            lines[0]
        );
        assert!(lines[1].starts_with("- .af/rules.md ("));
        assert!(lines[2].starts_with("- docs/design.md#items ("));
        let mut wrong = record.clone();
        wrong.context[0].path = "docs/other.md".into();
        assert!(wrong.validate().is_err(), "the path is the reference's own");
        let empty = TaskReadFirstV2 {
            schema: TASK_READ_FIRST_SCHEMA.into(),
            snapshot_id: id('b'),
            rules: None,
            context: Vec::new(),
        };
        assert!(
            empty.validate().is_err(),
            "an empty record is never written"
        );
        let mut treeless = record.clone();
        treeless.snapshot_id = "HEAD".into();
        assert!(treeless.validate().is_err(), "the tree is a Snapshot ID");
        let mut first = record;
        first.schema = "af.task-read-first/1".into();
        assert!(first.validate().is_err(), "@1 is never read");
    }
}
