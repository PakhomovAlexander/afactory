//! The agent loop's documents (ADR-0202): what `af task findings`, `af task materialize`, `af
//! task diff`, `af task status`, `af task run --detach` and `af task cancel` print under
//! `--json`, and the two records a run of `af task run` leaves in its Store's `runs/` directory:
//! the process that runs it, and a request to cancel it.
//!
//! Every field of a printed document is present, `null` where absent, so a reader never has to
//! tell a missing field from an absent value. The records are versioned like every persisted
//! shape (ADR-0145): a changed shape is a new version, and the old one fails closed.

use serde::{Deserialize, Serialize};

use super::state::TaskStateV1;
use super::usage::DecimalU128;
use super::{is_name, require};
use crate::is_digest;

pub const TASK_FINDINGS_V1: &str = "af/task-findings@1";
pub const TASK_MATERIALIZED_V1: &str = "af/task-materialized@1";
pub const TASK_DIFF_V1: &str = "af/task-diff@1";
pub const TASK_STATUS_V1: &str = "af/task-status@1";
pub const TASK_DETACHED_V2: &str = "af/task-detached@2";
pub const TASK_CANCELLED_V1: &str = "af/task-cancelled@1";
pub const TASK_RUN_PROCESS_V1: &str = "af/task-run-process@1";
pub const TASK_CANCEL_REQUEST_V1: &str = "af/task-cancel-request@1";

/// The longest text a document carries in one field: a reviewer's evidence, an evaluator's
/// reason or a host name. Longer text is cut by its writer, never refused by its reader.
pub const MAX_TEXT: usize = 65_536;

fn bounded(text: &str) -> bool {
    text.chars().count() <= MAX_TEXT && !text.trim().is_empty()
}

fn schema_is(actual: &str, expected: &str) -> Result<(), String> {
    require(
        actual == expected,
        &format!("expected the schema {expected}, read {actual}"),
    )
}

/// `af task findings ID --json`: the findings of one review round of a Task, per reviewer, and
/// the evaluator's verdict. `round` is null when the Task recorded no review round, and
/// `evaluation` when no evaluator recorded a verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingsV1 {
    pub schema: String,
    pub task_id: String,
    pub state: TaskStateV1,
    /// How many review rounds the Task recorded.
    pub rounds: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub round: Option<TaskFindingsRoundV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub evaluation: Option<TaskFindingsEvaluationV1>,
}

/// One recorded review round: its number among the Task's rounds (1 is the first), the round
/// number the Review recorded, the Snapshot it reviewed, its conclusion, and every reviewer's
/// findings, a reviewer that reported none included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingsRoundV1 {
    pub index: u64,
    pub round: u32,
    pub snapshot_id: String,
    pub conclusion: super::review::ReviewConclusionV1,
    /// Whether the round's reduce step recorded a Finding Set. An incomplete round's findings
    /// are each reviewer's recorded reports, not gathered evidence (ADR-0034).
    pub complete: bool,
    pub missing_reviewers: Vec<String>,
    pub counts: TaskFindingCountsV1,
    pub reviewers: Vec<TaskReviewerFindingsV1>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingCountsV1 {
    pub blocker: u64,
    pub major: u64,
    pub minor: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReviewerFindingsV1 {
    pub reviewer: String,
    /// The reviewer's selected result, null for a reviewer whose result is missing.
    #[serde(deserialize_with = "Option::deserialize")]
    pub result_id: Option<String>,
    pub findings: Vec<TaskFindingV1>,
}

/// One finding as its reviewer reported it: severity, confidence, title, location, the quoted
/// evidence and the proposed fix. `finding_id` and `status` are the Finding Set's, null for a
/// report of an incomplete round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingV1 {
    #[serde(deserialize_with = "Option::deserialize")]
    pub finding_id: Option<String>,
    pub severity: crate::Severity,
    #[serde(deserialize_with = "Option::deserialize")]
    pub status: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub confidence: Option<f64>,
    pub title: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub file: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub line: Option<i64>,
    pub evidence: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub fix: Option<String>,
}

/// The evaluator's verdict: the node that judged, the recorded `af/TaskEvaluation@1` and what
/// it said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingsEvaluationV1 {
    pub node: String,
    pub artifact_id: String,
    pub outcome: super::pipeline::ReceiptOutcomeV1,
    pub reason: String,
}

impl TaskFindingsV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_FINDINGS_V1)?;
        require(is_name(&self.task_id), "Findings name an exact Task")?;
        self.state.validate()?;
        if let Some(round) = &self.round {
            require(
                round.index >= 1 && round.index <= self.rounds,
                "A findings round is one of the Task's recorded rounds",
            )?;
            require(
                is_digest(&round.snapshot_id),
                "A findings round names the exact Snapshot it reviewed",
            )?;
            let mut counts = TaskFindingCountsV1::default();
            for reviewer in &round.reviewers {
                require(
                    is_name(&reviewer.reviewer)
                        && reviewer.result_id.as_deref().is_none_or(is_digest),
                    "A reviewer is named, with its exact result when it has one",
                )?;
                for finding in &reviewer.findings {
                    finding.validate()?;
                    match finding.severity {
                        crate::Severity::Blocker => counts.blocker += 1,
                        crate::Severity::Major => counts.major += 1,
                        crate::Severity::Minor => counts.minor += 1,
                    }
                }
            }
            require(
                counts == round.counts,
                "A round's counts are its reviewers' findings by severity",
            )?;
        } else {
            require(
                self.rounds == 0,
                "Findings of a Task that recorded a round name one",
            )?;
        }
        if let Some(evaluation) = &self.evaluation {
            require(
                is_digest(&evaluation.artifact_id)
                    && !evaluation.node.is_empty()
                    && bounded(&evaluation.reason),
                "An evaluation names its exact verdict and a bounded reason",
            )?;
        }
        Ok(())
    }
}

impl TaskFindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.finding_id.as_deref().is_none_or(is_digest)
                && self.confidence.is_none_or(|c| (0.0..=1.0).contains(&c))
                && self.line.is_none_or(|line| line > 0)
                && bounded(&self.title)
                && bounded(&self.evidence)
                && self.fix.as_deref().is_none_or(bounded)
                && self.file.as_deref().is_none_or(bounded),
            "A finding has a bounded title and evidence, a confidence in 0..1 and a positive line",
        )
    }
}

/// `af task materialize ID --into DIR --json`: what was written into the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMaterializedV1 {
    pub schema: String,
    pub task_id: String,
    pub snapshot_id: String,
    /// Whether the Snapshot is the verified result of the Task. An unverified one was written
    /// only under `--allow-unverified`.
    pub verified: bool,
    pub into: String,
    /// The paths written (created, or changed in content or kind), deleted, and changed only in
    /// their executable bit, each in the Snapshot's path spelling, sorted.
    pub written: Vec<String>,
    pub deleted: Vec<String>,
    pub mode: Vec<String>,
    pub next: Vec<String>,
}

impl TaskMaterializedV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_MATERIALIZED_V1)?;
        require(
            is_name(&self.task_id) && is_digest(&self.snapshot_id) && !self.into.is_empty(),
            "A materialization names its Task, its exact Snapshot and its directory",
        )?;
        for list in [&self.written, &self.deleted, &self.mode] {
            require(
                list.windows(2).all(|pair| pair[0] < pair[1]),
                "Materialized paths are sorted and unique",
            )?;
        }
        require(
            self.next.iter().all(|line| !line.trim().is_empty()),
            "Every next command is a command",
        )
    }
}

/// How a path differs between two Snapshots, as `git diff --name-status` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDiffStatusV1 {
    Added,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDiffFileV1 {
    pub path: String,
    pub status: TaskDiffStatusV1,
    /// The path a renamed file had, null for every other status.
    #[serde(deserialize_with = "Option::deserialize")]
    pub old_path: Option<String>,
    /// Lines added and deleted, null for a binary file.
    #[serde(deserialize_with = "Option::deserialize")]
    pub added: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub deleted: Option<u64>,
}

/// `af task diff ID --json`: the files and the unified diff from one Snapshot to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDiffV1 {
    pub schema: String,
    pub task_id: String,
    pub base_snapshot_id: String,
    pub snapshot_id: String,
    pub files: Vec<TaskDiffFileV1>,
    /// The unified diff `git diff` prints for the same two trees; bytes that are not UTF-8 are
    /// replaced.
    pub patch: String,
}

impl TaskDiffV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_DIFF_V1)?;
        require(
            is_name(&self.task_id)
                && is_digest(&self.base_snapshot_id)
                && is_digest(&self.snapshot_id),
            "A diff names its Task and its two exact Snapshots",
        )?;
        for file in &self.files {
            require(
                !file.path.is_empty()
                    && (file.status == TaskDiffStatusV1::Renamed) == file.old_path.is_some()
                    && file.added.is_some() == file.deleted.is_some(),
                "A diffed file has a path, an old path exactly when renamed, and both counts or none",
            )?;
        }
        Ok(())
    }
}

/// One process that runs a Task: `runs/<task>-<run>.json` in its Store, written by `af task
/// run` before it takes the Task's lease. `writer` is the lease's writer, so `af task cancel`
/// finds the process by the lease; `pid` and `host` say which process to stop, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunProcessV1 {
    pub schema: String,
    pub task_id: String,
    pub run: u64,
    pub pid: u32,
    pub host: String,
    pub writer: String,
    pub started_unix_ms: u64,
    pub detached: bool,
    /// The log a detached run writes, `runs/<task>-<run>.log`; null for a run in a terminal.
    #[serde(deserialize_with = "Option::deserialize")]
    pub log: Option<String>,
}

impl TaskRunProcessV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_RUN_PROCESS_V1)?;
        require(
            is_name(&self.task_id)
                && self.run >= 1
                && self.pid > 0
                && bounded(&self.host)
                && self.host.chars().count() <= 255
                && bounded(&self.writer)
                && self.detached == self.log.is_some(),
            "A run process names its Task, its run, a process and host, its writer, and a log \
             exactly when it is detached",
        )
    }
}

/// Who asked to cancel a run: the account, the host and the process of `af task cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancelRequesterV1 {
    pub user: String,
    pub host: String,
    pub pid: u32,
}

/// A request to cancel one run: `runs/<task>-<run>.cancel.json`, written by `af task cancel`
/// before it signals the run's process. `epoch` is the lease epoch of the run it stops, so the
/// Task's state names the cancellation exactly for that run (`host_cancelled`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancelRequestV1 {
    pub schema: String,
    pub task_id: String,
    pub run: u64,
    pub epoch: u64,
    pub writer: String,
    pub pid: u32,
    pub host: String,
    pub requester: TaskCancelRequesterV1,
    pub requested_unix_ms: u64,
}

impl TaskCancelRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_CANCEL_REQUEST_V1)?;
        require(
            is_name(&self.task_id)
                && self.run >= 1
                && self.epoch >= 1
                && self.pid > 0
                && self.requester.pid > 0
                && [
                    &self.writer,
                    &self.host,
                    &self.requester.user,
                    &self.requester.host,
                ]
                .into_iter()
                .all(|text| bounded(text) && text.chars().count() <= 255),
            "A cancel request names its Task, the run and lease it stops, and who asked",
        )
    }
}

/// `af task status ID --json`: where a Task is, without waiting. `node` is the node whose
/// Attempt runs now, `elapsed_ms` the time since the Task's clock started (to now while it
/// runs, to its last event once it stopped), `run` the run process whose writer took the Task's
/// last lease (the newest one recorded when no record names that writer), `cancel` the request
/// that stopped it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStatusV1 {
    pub schema: String,
    pub task_id: String,
    pub state: TaskStateV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub node: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub elapsed_ms: Option<u64>,
    pub chargeable_tokens: DecimalU128,
    #[serde(deserialize_with = "Option::deserialize")]
    pub run: Option<TaskRunProcessV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cancel: Option<TaskCancelRequestV1>,
}

impl TaskStatusV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_STATUS_V1)?;
        require(is_name(&self.task_id), "A status names an exact Task")?;
        self.state.validate()?;
        if let Some(run) = &self.run {
            run.validate()?;
        }
        if let Some(cancel) = &self.cancel {
            cancel.validate()?;
        }
        Ok(())
    }
}

/// `af task run ID --detach --json`: the run it started in the background, where it logs, and
/// whether it was admitted. `admitted` is true only once that run holds the Task's lease and its
/// plan is admitted; a run still starting when the command gave up waiting, or one that ended
/// before it went to the background, is not. `next_action` is the command to run next: `af task
/// wait ID` for an admitted run, `af task status ID` for one still starting, and the Task's own
/// next command, or null, for a run that ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDetachedV2 {
    pub schema: String,
    pub task_id: String,
    pub run: u64,
    pub pid: u32,
    pub host: String,
    pub log: String,
    pub admitted: bool,
    pub state: TaskStateV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub next_action: Option<String>,
}

impl TaskDetachedV2 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_DETACHED_V2)?;
        require(
            is_name(&self.task_id)
                && self.run >= 1
                && self.pid > 0
                && bounded(&self.host)
                && bounded(&self.log)
                && self.next_action.as_deref().is_none_or(bounded),
            "A detached run names its Task, run, process, host and log, and a next command \
             when it has one",
        )?;
        require(
            !self.admitted || self.next_action.is_some(),
            "An admitted detached run names the command that waits for it",
        )?;
        self.state.validate()
    }
}

/// `af task cancel ID --json`: the request it recorded and the state the Task is left in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancelledV1 {
    pub schema: String,
    pub task_id: String,
    pub request: TaskCancelRequestV1,
    pub state: TaskStateV1,
}

impl TaskCancelledV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_CANCELLED_V1)?;
        require(
            is_name(&self.task_id) && self.request.task_id == self.task_id,
            "A cancellation names its Task and that Task's request",
        )?;
        self.request.validate()?;
        self.state.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::state::*;

    fn id(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn finding(severity: crate::Severity) -> TaskFindingV1 {
        TaskFindingV1 {
            finding_id: Some(id('1')),
            severity,
            status: Some("open".into()),
            confidence: Some(0.9),
            title: "Reject negative offset".into(),
            file: Some("pagination.py".into()),
            line: Some(1),
            evidence: "A negative offset slices from the end.".into(),
            fix: Some("Raise ValueError.".into()),
        }
    }

    #[test]
    fn a_round_counts_its_reviewers_findings_by_severity() {
        let mut document = TaskFindingsV1 {
            schema: TASK_FINDINGS_V1.into(),
            task_id: "t".into(),
            state: TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None),
            rounds: 1,
            round: Some(TaskFindingsRoundV1 {
                index: 1,
                round: 1,
                snapshot_id: id('2'),
                conclusion: crate::task::review::ReviewConclusionV1::Pass,
                complete: true,
                missing_reviewers: vec![],
                counts: TaskFindingCountsV1 {
                    blocker: 0,
                    major: 1,
                    minor: 1,
                },
                reviewers: vec![TaskReviewerFindingsV1 {
                    reviewer: "bugs".into(),
                    result_id: Some(id('3')),
                    findings: vec![
                        finding(crate::Severity::Major),
                        finding(crate::Severity::Minor),
                    ],
                }],
            }),
            evaluation: None,
        };
        document.validate().unwrap();
        document.round.as_mut().unwrap().counts.minor = 2;
        assert!(document.validate().is_err(), "counts that disagree");
        document.round = None;
        assert!(document.validate().is_err(), "a recorded round left out");
        document.rounds = 0;
        document.validate().unwrap();
        let mut bad = finding(crate::Severity::Major);
        bad.confidence = Some(1.5);
        assert!(bad.validate().is_err());
        bad.confidence = None;
        bad.line = Some(0);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_run_process_has_a_log_exactly_when_it_is_detached_and_a_request_names_its_lease() {
        let mut run = TaskRunProcessV1 {
            schema: TASK_RUN_PROCESS_V1.into(),
            task_id: "t".into(),
            run: 1,
            pid: 42,
            host: "builder".into(),
            writer: "cli-42".into(),
            started_unix_ms: 1,
            detached: true,
            log: Some("/state/runs/t-1.log".into()),
        };
        run.validate().unwrap();
        run.log = None;
        assert!(run.validate().is_err());
        run.detached = false;
        run.validate().unwrap();
        run.schema = "af/task-run-process@0".into();
        assert!(run.validate().is_err(), "another version fails closed");
        let mut request = TaskCancelRequestV1 {
            schema: TASK_CANCEL_REQUEST_V1.into(),
            task_id: "t".into(),
            run: 1,
            epoch: 3,
            writer: "cli-42".into(),
            pid: 42,
            host: "builder".into(),
            requester: TaskCancelRequesterV1 {
                user: "dev".into(),
                host: "builder".into(),
                pid: 77,
            },
            requested_unix_ms: 2,
        };
        request.validate().unwrap();
        request.epoch = 0;
        assert!(request.validate().is_err());
    }

    #[test]
    fn a_diff_names_an_old_path_exactly_for_a_rename() {
        let mut diff = TaskDiffV1 {
            schema: TASK_DIFF_V1.into(),
            task_id: "t".into(),
            base_snapshot_id: id('1'),
            snapshot_id: id('2'),
            files: vec![TaskDiffFileV1 {
                path: "b".into(),
                status: TaskDiffStatusV1::Renamed,
                old_path: Some("a".into()),
                added: Some(1),
                deleted: Some(0),
            }],
            patch: String::new(),
        };
        diff.validate().unwrap();
        diff.files[0].status = TaskDiffStatusV1::Modified;
        assert!(diff.validate().is_err());
    }
}
