//! The agent loop's documents (ADR-0211): what `af task findings`, `af task materialize`, `af
//! task diff`, `af task status`, `af task run --detach` and `af task cancel` print under
//! `--json`, the lines of `af task events` (ADR-0250), and the two records a run of `af task run` leaves in its Store's `runs/` directory:
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

pub const TASK_FINDINGS_V2: &str = "af/task-findings@2";
pub const TASK_MATERIALIZED_V1: &str = "af/task-materialized@1";
pub const TASK_DIFF_V1: &str = "af/task-diff@1";
pub const TASK_STATUS_V2: &str = "af/task-status@2";
pub const TASK_DETACHED_V3: &str = "af/task-detached@3";
pub const TASK_CANCELLED_V2: &str = "af/task-cancelled@2";
pub const TASK_RUN_PROCESS_V1: &str = "af/task-run-process@1";
pub const TASK_CANCEL_REQUEST_V1: &str = "af/task-cancel-request@1";
pub const TASK_EVENT_V1: &str = "af/task-event@1";

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
pub struct TaskFindingsV2 {
    pub schema: String,
    pub task_id: String,
    pub state: TaskStateV1,
    /// How many review rounds the Task recorded.
    pub rounds: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub round: Option<TaskFindingsRoundV2>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub evaluation: Option<TaskFindingsEvaluationV1>,
}

/// One recorded review round: its number among the Task's rounds (1 is the first), the round
/// number the Review recorded, the Snapshot it reviewed, its conclusion, every reviewer's
/// findings, a reviewer that reported none included, what the round covered, and the findings
/// a verification record proved fixed on its Snapshot (ADR-0248).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingsRoundV2 {
    pub index: u64,
    pub round: u32,
    pub snapshot_id: String,
    pub conclusion: super::review::ReviewConclusionV1,
    /// Whether the round's reduce step recorded a Finding Set. An incomplete round's findings
    /// are each reviewer's recorded reports, not gathered evidence (ADR-0034).
    pub complete: bool,
    pub missing_reviewers: Vec<String>,
    pub counts: TaskFindingCountsV1,
    pub reviewers: Vec<TaskReviewerFindingsV2>,
    /// Per reviewer and per path: reviewed, not run (and why), or outside the round.
    pub coverage: super::handoff::CoverageV1,
    /// Findings of an earlier round this round's Snapshot verified fixed, each with its
    /// positive `af/TaskFixReceipt@1`. Nothing else is reported as verified fixed.
    pub verified_fixed: Vec<super::handoff::VerifiedFixV1>,
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
pub struct TaskReviewerFindingsV2 {
    pub reviewer: String,
    /// The reviewer's selected result, null for a reviewer whose result is missing.
    #[serde(deserialize_with = "Option::deserialize")]
    pub result_id: Option<String>,
    pub findings: Vec<TaskFindingV2>,
}

/// One finding as its reviewer reported it: severity, confidence, title, location, the quoted
/// evidence and the proposed fix. `finding_id` and `status` are the Finding Set's, null for a
/// report of an incomplete round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFindingV2 {
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
    /// The finding as a prompt for the agent that will fix it (ADR-0248).
    pub agent_prompt: super::handoff::FindingPromptV1,
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

impl TaskFindingsV2 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_FINDINGS_V2)?;
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
            round.coverage.validate()?;
            require(
                round.coverage.reviewers.len() == round.reviewers.len()
                    && round.coverage.reviewers.iter().zip(&round.reviewers).all(
                        |(covered, reviewer)| {
                            covered.reviewer == reviewer.reviewer
                                && (covered.state
                                    == super::handoff::ReviewerCoverageStateV1::Reviewed)
                                    == reviewer.result_id.is_some()
                        },
                    )
                    && round
                        .coverage
                        .not_run()
                        .map(|reviewer| &reviewer.reviewer)
                        .eq(round.missing_reviewers.iter()),
                "A round covers exactly its reviewers: reviewed with a result, not run without \
                 one, and its missing reviewers are the ones that did not run",
            )?;
            let mut fixed = std::collections::BTreeSet::new();
            for verified in &round.verified_fixed {
                verified.validate()?;
                require(
                    verified.snapshot_id == round.snapshot_id
                        && fixed.insert(verified.finding_id.as_str()),
                    "A finding is verified fixed once, on the Snapshot of the round",
                )?;
            }
            for reviewer in &round.reviewers {
                require(
                    is_name(&reviewer.reviewer)
                        && reviewer.result_id.as_deref().is_none_or(is_digest),
                    "A reviewer is named, with its exact result when it has one",
                )?;
                for finding in &reviewer.findings {
                    finding.validate()?;
                    require(
                        finding.agent_prompt.location.snapshot_id == round.snapshot_id,
                        "A finding's prompt names the Snapshot its round reviewed",
                    )?;
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

impl TaskFindingV2 {
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
        )?;
        self.agent_prompt.validate()?;
        require(
            self.agent_prompt.location.file == self.file
                && self.agent_prompt.location.line == self.line.filter(|_| self.file.is_some()),
            "A finding's prompt names the finding's own location",
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
/// runs, to its last event once it stopped), exact token accounting and last comparable Gate
/// duration when recorded, `run` the run process whose writer took the Task's
/// last lease (the newest one recorded when no record names that writer), `cancel` the request
/// that stopped it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStatusV2 {
    pub schema: String,
    pub task_id: String,
    pub state: TaskStateV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub node: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub elapsed_ms: Option<u64>,
    pub charged_tokens: DecimalU128,
    /// Tokens held by currently admitted Attempts.
    pub reserved_tokens: DecimalU128,
    /// Tokens still available for admission after charges and reservations.
    pub remaining_tokens: DecimalU128,
    /// Attempts whose usage was not reported, so the charged total is not their spend.
    pub unknown_usage_attempts: u64,
    /// Last measured duration for a check in this plan, with the same repository and code policy.
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_gate: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub run: Option<TaskRunProcessV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cancel: Option<TaskCancelRequestV1>,
}

impl TaskStatusV2 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_STATUS_V2)?;
        require(is_name(&self.task_id), "A status names an exact Task")?;
        self.state.validate()?;
        require(
            self.unknown_usage_attempts <= crate::json::SAFE_INTEGER_MAX as u64
                && self.last_gate.as_deref().is_none_or(bounded),
            "Task status requires bounded unknown usage and Gate history",
        )?;
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
pub struct TaskDetachedV3 {
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

impl TaskDetachedV3 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_DETACHED_V3)?;
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
pub struct TaskCancelledV2 {
    pub schema: String,
    pub task_id: String,
    pub request: TaskCancelRequestV1,
    pub state: TaskStateV1,
}

impl TaskCancelledV2 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_CANCELLED_V2)?;
        require(
            is_name(&self.task_id) && self.request.task_id == self.task_id,
            "A cancellation names its Task and that Task's request",
        )?;
        self.request.validate()?;
        self.state.validate()
    }
}

/// One line of `af task events ID` (ADR-0250): an event of the Task's log as the Store recorded
/// it, or the one terminal record that ends the stream once the Task settled. A line is one
/// document; `record` says which of the two it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum TaskEventV1 {
    Event(TaskEventEntryV1),
    Terminal(TaskEventTerminalV1),
}

impl TaskEventV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Event(entry) => entry.validate(),
            Self::Terminal(terminal) => terminal.validate(),
        }
    }
}

/// What one event of a Task's log records: the `kind` of its `TaskTransition@5` change, or the
/// log's three other event kinds. Every kind the Store can record maps to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventChangeV1 {
    RecordingResumed,
    ReviewIntegrationSelected,
    ReviewIntegrationFinished,
    ReviewContinued,
    AdoptionObservationRecorded,
    Opened,
    LeaseTaken,
    LeaseRenewed,
    LeaseReleased,
    SourceRefreshed,
    PlanProposed,
    PlanningCompleted,
    PlanDecided,
    ApprovalRevoked,
    PlanAdmitted,
    Waiting,
    Resumed,
    Finished,
    ExecutionRecorded,
    RunReported,
    DeliveryRecorded,
    TaskCollected,
    GateCleanup,
    StorageSweep,
    /// `TaskExecutionStarted@1`: a lease began executing the plan.
    ExecutionStarted,
    /// `TaskGateRetried@1`: a gate retry reopened the finished Task.
    GateRetried,
    /// `TaskProviderEvent@1`: an Attempt waited, re-asked or probed its Provider again.
    ProviderEvent,
}

impl TaskEventChangeV1 {
    /// The kind of a `TaskTransition@5` change.
    pub fn of(change: &super::event::TaskChangeV1) -> Self {
        use super::event::TaskChangeV1 as C;
        match change {
            C::RecordingResumed { .. } => Self::RecordingResumed,
            C::ReviewIntegrationSelected { .. } => Self::ReviewIntegrationSelected,
            C::ReviewIntegrationFinished { .. } => Self::ReviewIntegrationFinished,
            C::ReviewContinued { .. } => Self::ReviewContinued,
            C::AdoptionObservationRecorded { .. } => Self::AdoptionObservationRecorded,
            C::Opened { .. } => Self::Opened,
            C::LeaseTaken { .. } => Self::LeaseTaken,
            C::LeaseRenewed { .. } => Self::LeaseRenewed,
            C::LeaseReleased {} => Self::LeaseReleased,
            C::SourceRefreshed { .. } => Self::SourceRefreshed,
            C::PlanProposed { .. } => Self::PlanProposed,
            C::PlanningCompleted { .. } => Self::PlanningCompleted,
            C::PlanDecided { .. } => Self::PlanDecided,
            C::ApprovalRevoked { .. } => Self::ApprovalRevoked,
            C::PlanAdmitted { .. } => Self::PlanAdmitted,
            C::Waiting { .. } => Self::Waiting,
            C::Resumed {} => Self::Resumed,
            C::Finished { .. } => Self::Finished,
            C::ExecutionRecorded { .. } => Self::ExecutionRecorded,
            C::RunReported { .. } => Self::RunReported,
            C::DeliveryRecorded { .. } => Self::DeliveryRecorded,
            C::TaskCollected { .. } => Self::TaskCollected,
            C::GateCleanup { .. } => Self::GateCleanup,
            C::StorageSweep { .. } => Self::StorageSweep,
        }
    }

    /// The Store event type that records this kind.
    pub fn event_type(self) -> &'static str {
        match self {
            Self::ExecutionStarted => "TaskExecutionStarted@1",
            Self::GateRetried => "TaskGateRetried@1",
            Self::ProviderEvent => "TaskProviderEvent@1",
            _ => "TaskTransition@5",
        }
    }

    /// Whether an event of this kind names what happened within it in `detail`: the execution
    /// record's kind, the reason a Task waits, or the Provider event's kind.
    fn has_detail(self) -> bool {
        matches!(
            self,
            Self::ExecutionRecorded | Self::Waiting | Self::ProviderEvent
        )
    }
}

/// How a settled Attempt ended, as its execution record says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventResultV1 {
    Succeeded,
    Failed,
    Abandoned,
}

/// One event of the Task's log. `sequence` is the Store's own sequence of it in the Task's log,
/// gap-free from 0; `event_id` the Store's ID of it; `at_unix_ms` the policy time it records.
/// `detail` is the kind of what an `execution_recorded`, `waiting` or `provider_event` change
/// records and null on every other; `node` and `attempt_id` name the node and Attempt it
/// concerns, when it concerns one; `result` is how a `settled` Attempt ended, and
/// `charged_tokens` the exact charge a `settled` or `usage_observed` record carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEventEntryV1 {
    pub schema: String,
    pub task_id: String,
    pub sequence: u64,
    pub event_id: String,
    pub event_type: String,
    pub at_unix_ms: u64,
    pub change: TaskEventChangeV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub detail: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub node: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub attempt_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub result: Option<TaskEventResultV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub charged_tokens: Option<DecimalU128>,
}

impl TaskEventEntryV1 {
    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_EVENT_V1)?;
        require(
            is_name(&self.task_id)
                && self.sequence <= crate::json::SAFE_INTEGER_MAX as u64
                && self.event_id.len() == 26
                && self
                    .event_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && self.at_unix_ms > 0
                && self.at_unix_ms <= crate::json::SAFE_INTEGER_MAX as u64,
            "A Task event names its Task, its sequence, the Store's event ID and its time",
        )?;
        require(
            self.event_type == self.change.event_type(),
            "A Task event's change is one its Store event type records",
        )?;
        require(
            self.detail.is_some() == self.change.has_detail()
                && self.detail.as_deref().is_none_or(|detail| {
                    detail.len() <= 64
                        && detail.starts_with(|c: char| c.is_ascii_lowercase())
                        && detail
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                }),
            "An execution record, a wait or a Provider event names its kind in `detail`, and \
             no other change does",
        )?;
        require(
            [&self.node, &self.attempt_id]
                .into_iter()
                .flatten()
                .all(|text| bounded(text) && text.chars().count() <= 255),
            "A Task event's node and Attempt are bounded names",
        )?;
        let settled = self.change == TaskEventChangeV1::ExecutionRecorded
            && self.detail.as_deref() == Some("settled");
        let charged = settled
            || (self.change == TaskEventChangeV1::ExecutionRecorded
                && self.detail.as_deref() == Some("usage_observed"));
        require(
            self.result.is_some() == settled && self.charged_tokens.is_some() == charged,
            "Only a settled Attempt has a result, and only a settlement or a usage observation \
             a charge",
        )
    }
}

/// The last line of `af task events ID`, written once when the Task settled (no run holds it
/// and none of this host is starting). `last_sequence` is the sequence of the last event the
/// stream covered, so `af task events ID --after LAST_SEQUENCE` prints only what the Task
/// records later; `state` carries the outcome and `next_action`, and `exit_code` is the code
/// the command exits with, as `af task wait` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEventTerminalV1 {
    pub schema: String,
    pub task_id: String,
    pub last_sequence: u64,
    pub state: TaskStateV1,
    pub exit_code: u8,
}

impl TaskEventTerminalV1 {
    /// The exit code a follower of a settled Task ends with: its outcome's, 4 for a Task that
    /// waits for a person, 0 for one that neither ended nor waits.
    pub fn exit_code_of(state: &TaskStateV1) -> u8 {
        match state.outcome {
            Some(outcome) => outcome.exit_code(),
            None if state.phase == super::state::TaskStatePhaseV1::Waiting => 4,
            None => 0,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        schema_is(&self.schema, TASK_EVENT_V1)?;
        require(
            is_name(&self.task_id) && self.last_sequence <= crate::json::SAFE_INTEGER_MAX as u64,
            "A terminal record names its Task and the last sequence it covers",
        )?;
        self.state.validate()?;
        require(
            self.state.phase != super::state::TaskStatePhaseV1::Running
                && self.exit_code == Self::exit_code_of(&self.state),
            "A terminal record ends a settled Task with that Task's exit code",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::handoff;
    use crate::task::state::*;
    use std::collections::BTreeMap;

    fn id(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn finding(severity: crate::Severity) -> TaskFindingV2 {
        let snapshot = id('2');
        TaskFindingV2 {
            finding_id: Some(id('1')),
            severity,
            status: Some("open".into()),
            confidence: Some(0.9),
            title: "Reject negative offset".into(),
            file: Some("pagination.py".into()),
            line: Some(1),
            evidence: "A negative offset slices from the end.".into(),
            fix: Some("Raise ValueError.".into()),
            agent_prompt: handoff::prompt(
                &handoff::PromptSource {
                    reviewer: "bugs",
                    severity,
                    title: "Reject negative offset",
                    file: Some("pagination.py"),
                    line: Some(1),
                    snapshot_id: &snapshot,
                    evidence: "A negative offset slices from the end.",
                    fix: Some("Raise ValueError."),
                },
                handoff::VerifiedBy::Task,
            ),
        }
    }

    #[test]
    fn a_round_counts_its_reviewers_findings_by_severity() {
        let mut document = TaskFindingsV2 {
            schema: TASK_FINDINGS_V2.into(),
            task_id: "t".into(),
            state: TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None),
            rounds: 1,
            round: Some(TaskFindingsRoundV2 {
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
                reviewers: vec![
                    TaskReviewerFindingsV2 {
                        reviewer: "bugs".into(),
                        result_id: Some(id('3')),
                        findings: vec![
                            finding(crate::Severity::Major),
                            finding(crate::Severity::Minor),
                        ],
                    },
                    TaskReviewerFindingsV2 {
                        reviewer: "correctness".into(),
                        result_id: Some(id('4')),
                        findings: vec![],
                    },
                ],
                coverage: handoff::coverage(
                    Some(&["pagination.py".to_owned()]),
                    BTreeMap::from([("bugs".into(), None), ("correctness".into(), None)]),
                    ["pagination.py"],
                ),
                verified_fixed: vec![],
            }),
            evaluation: None,
        };
        document.validate().unwrap();
        // A reviewer whose result is missing is covered as not run, never as clean.
        let mut missing = document.clone();
        missing.round.as_mut().unwrap().reviewers[1].result_id = None;
        assert!(
            missing.validate().is_err(),
            "a missing result covered as reviewed"
        );
        missing.round.as_mut().unwrap().coverage = handoff::coverage(
            Some(&["pagination.py".to_owned()]),
            BTreeMap::from([
                ("bugs".into(), None),
                ("correctness".into(), Some("no result".into())),
            ]),
            ["pagination.py"],
        );
        assert!(
            missing.validate().is_err(),
            "a not-run reviewer left out of missing"
        );
        missing.round.as_mut().unwrap().missing_reviewers = vec!["correctness".into()];
        missing.validate().unwrap();
        // A verified fix names the round's own Snapshot.
        let mut fixed = document.clone();
        fixed.round.as_mut().unwrap().verified_fixed = vec![handoff::VerifiedFixV1 {
            finding_id: id('9'),
            record_id: id('8'),
            snapshot_id: id('7'),
        }];
        assert!(
            fixed.validate().is_err(),
            "a fix verified on another Snapshot"
        );
        fixed.round.as_mut().unwrap().verified_fixed[0].snapshot_id = id('2');
        fixed.validate().unwrap();
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
