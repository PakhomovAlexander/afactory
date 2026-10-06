//! What `af task report` prints (ADR-0142): what recorded Tasks cost and how they ran, read from
//! the Store and never estimated. A figure the Store does not record is absent, never zero.
//!
//! The document names a Provider only by its kind, model and effort. It has no field for a
//! Provider registry label or principal, a path, a credential, a prompt or Worker output, and
//! every object is closed, so none of them can be carried by accident.

use serde::{Deserialize, Serialize};

use super::usage::{DecimalU128, TaskUnknownUsageCauseV1};
use super::{is_name, is_package_name, require, safe_number};

pub const TASK_REPORT_V1: &str = "af/task-report@1";

/// The first line of the Markdown block a pull request description carries.
pub const TASK_REPORT_BEGIN: &str = "<!-- af-task-report:v1 -->";
/// The last line of that block.
pub const TASK_REPORT_END: &str = "<!-- /af-task-report -->";
/// The round table's columns, in order: `scripts/check-pr-report.py` requires every one.
pub const TASK_REPORT_COLUMNS: [&str; 6] =
    ["Round", "Task", "Outcome", "Findings", "Tokens", "Active"];
/// The Task cell of the round table's last row, the totals row, starts with this text.
pub const TASK_REPORT_TOTAL: &str = "Total:";
/// The pipeline line printed, once, when a reported Task names no pipeline: its plan was
/// collected or it never reached planning. `scripts/check-pr-report.py` accepts it as a pipeline
/// line.
pub const TASK_REPORT_UNKNOWN_PIPELINE: &str = "**unknown pipeline**: not retained";
/// The role a check node's step has in a pipeline line.
pub const TASK_REPORT_GATE_ROLE: &str = "gate";
/// The model the report names in place of a recorded value that is not a model identity.
pub const TASK_REPORT_UNKNOWN_MODEL: &str = "unknown";
/// The longest model identity the report copies.
pub const TASK_REPORT_MODEL_MAX: usize = 96;

/// Path segments that name a home, account or state directory, compared case-insensitively.
/// `auth` also covers a segment that starts with `auth.`, `auth_` or `auth-` (`auth.json`).
const PRIVATE_SEGMENTS: [&str; 8] = [
    "home", "users", "root", "tmp", "var", "private", "state", "auth",
];

/// Whether `model` is a model identity the report may copy. The rule is an allow-list:
/// 1 to [`TASK_REPORT_MODEL_MAX`] characters from `A-Z a-z 0-9 ._:+-` plus at most one `/`;
/// the first character, and the first character after the `/`, alphanumeric; no `..`; no `:`
/// directly before the `/` (so no `://`); no drive-letter prefix such as `C:`; and neither
/// side of the `/` a name of a home, auth or state directory. Anything else, including any
/// value with an `@` such as an email address, may be a path, a URL or an account and is
/// reported as [`TASK_REPORT_UNKNOWN_MODEL`].
/// `schemas/task-report-v1.json` states the same rule as the model's pattern, and
/// [`TASK_REPORT_MODEL_CASES`] is the table both are held to.
pub fn is_model_identity(model: &str) -> bool {
    let bytes = model.as_bytes();
    let starts_alphanumeric = |part: &str| {
        part.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
    };
    (1..=TASK_REPORT_MODEL_MAX).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._:+-/".contains(b))
        && bytes.iter().filter(|&&b| b == b'/').count() <= 1
        && !model.contains("..")
        && !model.contains(":/")
        && !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        && model.split('/').all(|part| {
            let part = part.to_ascii_lowercase();
            starts_alphanumeric(&part)
                && !PRIVATE_SEGMENTS.contains(&part.as_str())
                && !["auth.", "auth_", "auth-"]
                    .iter()
                    .any(|prefix| part.starts_with(prefix))
        })
}

/// The table [`is_model_identity`] and the schema's model pattern are both tested against:
/// each value and whether it is a model identity. Lengths at and beyond
/// [`TASK_REPORT_MODEL_MAX`] are tested beside it.
#[doc(hidden)]
pub const TASK_REPORT_MODEL_CASES: [(&str, bool); 51] = [
    ("gpt-6-sol/high", true),
    ("claude-opus-5-5", true),
    ("gpt-5.3-codex-spark", true),
    ("us.anthropic.claude-opus-5-5-v1:0", true),
    ("codex-fixture-1", true),
    ("anthropic/claude-3.5", true),
    ("gpt-4o+tools", true),
    ("org:team/model-1", true),
    ("o1:2024", true),
    ("authors/model", true),
    ("statesman-1", true),
    ("unknown", true),
    ("file:///etc/passwd", false),
    ("C:/secrets/key", false),
    ("C:\\key", false),
    ("C:\\\\key", false),
    ("/etc/x", false),
    ("~/x", false),
    ("a//b", false),
    ("a/b/c", false),
    ("../x", false),
    ("", false),
    ("c:model-1", false),
    ("C:", false),
    ("https://example.invalid/m-1", false),
    ("a:/b", false),
    ("a/", false),
    ("/", false),
    ("-x", false),
    (".hidden", false),
    ("a/.codex", false),
    ("a/-b", false),
    ("m..1", false),
    ("models/../secret", false),
    ("home/fixture", false),
    ("fixture/HOME", false),
    ("Users/fixture", false),
    ("providers/State", false),
    ("provider/auth.json", false),
    ("provider/AUTH-dir", false),
    ("codex auth", false),
    ("m|x", false),
    ("m\\|x", false),
    ("m&#124;x", false),
    ("<b>m-1", false),
    ("fixture@example.invalid\n", false),
    ("alice@example.com", false),
    ("model@host", false),
    ("meta-llama/Llama-3-70b@latest", false),
    ("gpt\u{202e}", false),
    ("gpt-6-sol%2Fhigh", false),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportV1 {
    /// Always [`TASK_REPORT_V1`].
    pub schema: String,
    /// Each distinct pipeline the reported Tasks' current plans run, in first-use order.
    pub pipelines: Vec<TaskReportPipelineV1>,
    /// One entry per requested Task, in the order the command named them.
    pub tasks: Vec<TaskReportEntryV1>,
    pub totals: TaskReportTotalsV1,
}

/// One pipeline as a plan binds it: its steps in dependency order, each with its Worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportPipelineV1 {
    pub name: String,
    pub version: String,
    /// Every node that runs a Worker, and every check node (the gate), of the plan's compiled
    /// graph, in dependency order. Kernel bookkeeping nodes and Provider admission are left out.
    pub steps: Vec<TaskReportStepV1>,
}

impl TaskReportPipelineV1 {
    /// `name@version`, as a Task entry names its pipeline.
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// One step of a pipeline: the nodes of one role, Worker and check list that run in parallel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportStepV1 {
    /// The step's position in dependency order, from 1. Steps of one stage run in parallel.
    pub stage: u64,
    /// The Worker slot's role, or [`TASK_REPORT_GATE_ROLE`] for a check node.
    pub role: String,
    /// The qualified nodes of the step, in compiled order.
    pub nodes: Vec<String>,
    /// The Worker the plan binds the step's slot to; absent for a gate.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub worker: Option<TaskReportWorkerV1>,
    /// The checks a gate runs, local and remote, in name order; empty for a Worker step.
    pub checks: Vec<String>,
}

/// What a Task's review recorded. Absent for a Task whose plan has no review and for a
/// collected Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportFindingsV1 {
    /// Findings by severity, each counted once: the entries of the `FindingSet@1` each recorded
    /// review round's reduce step wrote whose `last_seen_round` is that round. All three are
    /// absent, unknown, when the review ran but a round it recorded has no complete
    /// `FindingSet@1`, or it recorded no round: an incomplete round never reads as no findings.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub blocker: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub major: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub minor: Option<u64>,
    /// A reviewer Attempt began, or a recorded round wrote its findings or admitted a
    /// reviewer's result.
    pub review_ran: bool,
    /// A check failed and no reviewer Attempt began.
    pub gate_failed: bool,
    /// Reviewer nodes with at least one failed Attempt.
    pub failed_reviewers: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportEntryV1 {
    /// The Task's 1-based position in the report: its row of the round table.
    pub round: u64,
    pub task_id: String,
    pub kind: String,
    /// The root Pipeline of the current plan as `name@version`; absent before a plan exists
    /// and for a collected Task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub pipeline: Option<String>,
    /// The outcome `af task show` states: the result's domain conclusion, or the phase of an
    /// unfinished Task.
    pub outcome: String,
    /// The Task was collected (ADR-0135): only its retained summary and its event times remain.
    pub collected: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub review_rounds: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub findings: Option<TaskReportFindingsV1>,
    /// Writer leases in which an Attempt of the Task started: `af task start --execute` and
    /// every `af task run` that resumed it with new work.
    pub runs: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub attempts: Option<TaskReportAttemptsV1>,
    pub chargeable_tokens: DecimalU128,
    /// From the Task's first to its last recorded event, the tombstone excluded.
    pub wall_ms: u64,
    /// The sum of the runs' spans, first to last event of each: waiting between runs is not work.
    pub active_ms: u64,
    /// Absent for a collected Task, whose execution records are gone.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub nodes: Option<Vec<TaskReportNodeV1>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportAttemptsV1 {
    pub total: u64,
    pub failed: u64,
    /// Tokens charged to the failed Attempts.
    pub failed_tokens: DecimalU128,
    /// The failed Attempts grouped by reason class, in class order.
    pub failures: Vec<TaskReportFailureV1>,
    /// Settled Attempts whose usage is unknown: no usage was reported, so each was charged
    /// zero (ADR-0143) and the Task's tokens are not their spend.
    pub unknown_usage: u64,
    /// Those Attempts by cause, in cause order; empty when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_usage_causes: Vec<TaskReportUnknownUsageV1>,
}

/// How many of a Task's Attempts have unknown usage for one cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportUnknownUsageV1 {
    pub cause: TaskUnknownUsageCauseV1,
    pub attempts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportFailureV1 {
    pub class: TaskReportFailureClassV1,
    pub attempts: u64,
    pub tokens: DecimalU128,
}

/// A failed Attempt's typed retry feedback code, `abandoned` for an Attempt recovery settled,
/// and `unclassified` for a failure that recorded no feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportFailureClassV1 {
    ProviderFailure,
    ProcessFailure,
    InvalidOutputContract,
    OutputAdmissionRejected,
    ContextRejected,
    CompilerRejected,
    Abandoned,
    Unclassified,
}

impl TaskReportFailureClassV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderFailure => "provider_failure",
            Self::ProcessFailure => "process_failure",
            Self::InvalidOutputContract => "invalid_output_contract",
            Self::OutputAdmissionRejected => "output_admission_rejected",
            Self::ContextRejected => "context_rejected",
            Self::CompilerRejected => "compiler_rejected",
            Self::Abandoned => "abandoned",
            Self::Unclassified => "unclassified",
        }
    }
}

impl From<super::feedback::TaskFeedbackCodeV1> for TaskReportFailureClassV1 {
    fn from(code: super::feedback::TaskFeedbackCodeV1) -> Self {
        use super::feedback::TaskFeedbackCodeV1 as Code;
        match code {
            Code::ProviderFailure => Self::ProviderFailure,
            Code::ProcessFailure => Self::ProcessFailure,
            Code::InvalidOutputContract => Self::InvalidOutputContract,
            Code::OutputAdmissionRejected => Self::OutputAdmissionRejected,
            Code::ContextRejected => Self::ContextRejected,
            Code::CompilerRejected => Self::CompilerRejected,
        }
    }
}

/// One node that began at least one Attempt, under one Worker binding: a node whose Attempts
/// ran under plans that bound it to different Workers (after `af task refresh`) has one entry
/// per Worker, so no Attempt is charged to a Worker it did not use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportNodeV1 {
    /// The qualified node, as in `root.nodes.implement`.
    pub node: String,
    /// The Worker slot's role, or the kernel operation a Worker-less node performs.
    pub role: String,
    /// The Worker the node's slot was bound to in the plans its Attempts ran under; absent for a
    /// kernel node.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub worker: Option<TaskReportWorkerV1>,
    pub attempts: u64,
    pub failed_attempts: u64,
    pub tokens: DecimalU128,
    /// The node's Attempts in this entry whose usage is unknown (ADR-0143).
    pub unknown_usage: u64,
    /// Absent unless every Attempt of the node recorded its elapsed time.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub elapsed_ms: Option<u64>,
    /// Every check result the node's Attempts recorded, in Attempt order.
    pub checks: Vec<TaskReportCheckV1>,
}

/// A Worker as the report may name it: a command, or a model by Provider kind, model and
/// effort. The machine-local Provider label and the principal are never part of it, and a
/// model value that is not a model identity ([`is_model_identity`]) is
/// [`TASK_REPORT_UNKNOWN_MODEL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskReportWorkerV1 {
    Command {},
    Model {
        provider_kind: String,
        model: String,
        effort: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportCheckV1 {
    pub name: String,
    pub status: TaskReportCheckStatusV1,
    /// The check's recorded span; absent when it never started or recorded none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportCheckStatusV1 {
    Passed,
    Failed,
    NotRun,
}

impl TaskReportCheckStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NotRun => "not_run",
        }
    }
}

/// Sums over every reported Task. A sum is absent when any Task does not record its part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportTotalsV1 {
    pub tasks: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub review_rounds: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub attempts: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub failed_attempts: Option<u64>,
    pub chargeable_tokens: DecimalU128,
    /// Attempts whose usage is unknown, over every Task that records its Attempts (ADR-0143).
    pub unknown_usage: u64,
    pub active_ms: u64,
}

impl TaskReportTotalsV1 {
    /// The totals of `tasks`, exactly: a sum that would overflow is refused.
    pub fn of(tasks: &[TaskReportEntryV1]) -> Result<Self, String> {
        fn sum(values: impl Iterator<Item = Option<u64>>) -> Result<Option<u64>, String> {
            let mut total = Some(0u64);
            for value in values {
                total = match (total, value) {
                    (Some(total), Some(value)) => Some(
                        total
                            .checked_add(value)
                            .ok_or("Task report total overflow")?,
                    ),
                    _ => None,
                };
            }
            Ok(total)
        }
        let mut tokens = 0u128;
        let mut active = 0u64;
        let mut unknown = 0u64;
        for task in tasks {
            tokens = tokens
                .checked_add(task.chargeable_tokens.get())
                .ok_or("Task report total overflow")?;
            unknown = unknown
                .checked_add(task.attempts.as_ref().map_or(0, |a| a.unknown_usage))
                .ok_or("Task report total overflow")?;
            active = active
                .checked_add(task.active_ms)
                .ok_or("Task report total overflow")?;
        }
        Ok(Self {
            tasks: u64::try_from(tasks.len()).map_err(|e| e.to_string())?,
            review_rounds: sum(tasks.iter().map(|task| task.review_rounds))?,
            attempts: sum(tasks
                .iter()
                .map(|task| task.attempts.as_ref().map(|a| a.total)))?,
            failed_attempts: sum(tasks
                .iter()
                .map(|task| task.attempts.as_ref().map(|a| a.failed)))?,
            chargeable_tokens: DecimalU128::from(tokens),
            unknown_usage: unknown,
            active_ms: active,
        })
    }
}

impl TaskReportV1 {
    /// The report over `tasks` in this order, each numbered by its position, with the distinct
    /// `pipelines` their plans run in first-use order.
    pub fn new(
        pipelines: Vec<TaskReportPipelineV1>,
        mut tasks: Vec<TaskReportEntryV1>,
    ) -> Result<Self, String> {
        for (index, task) in tasks.iter_mut().enumerate() {
            task.round = u64::try_from(index + 1).map_err(|e| e.to_string())?;
        }
        let report = Self {
            schema: TASK_REPORT_V1.into(),
            pipelines,
            totals: TaskReportTotalsV1::of(&tasks)?,
            tasks,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_REPORT_V1,
            "Task report needs its schema",
        )?;
        require(
            !self.tasks.is_empty(),
            "Task report needs at least one Task",
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for (index, task) in self.tasks.iter().enumerate() {
            task.validate()?;
            require(
                seen.insert(task.task_id.as_str()),
                "Task report names a Task twice",
            )?;
            require(
                usize::try_from(task.round).is_ok_and(|round| round == index + 1),
                "Task report rounds are the Tasks' positions, from 1",
            )?;
        }
        for (index, pipeline) in self.pipelines.iter().enumerate() {
            pipeline.validate()?;
            require(
                !self.pipelines[..index].contains(pipeline),
                "Task report lists a pipeline twice",
            )?;
        }
        // Every pipeline a Task names is listed, every listed one is used, in first-use order.
        let mut used: Vec<&str> = Vec::new();
        for task in &self.tasks {
            if let Some(label) = task.pipeline.as_deref()
                && !used.contains(&label)
            {
                used.push(label);
            }
        }
        let labels = self
            .pipelines
            .iter()
            .map(TaskReportPipelineV1::label)
            .collect::<Vec<_>>();
        let mut listed: Vec<&str> = Vec::new();
        for label in &labels {
            if !listed.contains(&label.as_str()) {
                listed.push(label);
            }
        }
        require(
            listed == used,
            "Task report pipelines are the Tasks' pipelines, in first-use order",
        )?;
        require(
            self.totals == TaskReportTotalsV1::of(&self.tasks)?,
            "Task report totals must be the sums of its Tasks",
        )
    }
}

impl TaskReportPipelineV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_package_name(&self.name)
                && !self.version.is_empty()
                && self.version.len() <= 128
                && !self.version.chars().any(char::is_whitespace),
            "Task report pipeline needs its name and version",
        )?;
        let mut stage = 0u64;
        for step in &self.steps {
            require(
                step.stage == stage || step.stage == stage + 1,
                "Task report pipeline stages count up from 1",
            )?;
            stage = step.stage;
            require(
                is_name(&step.role)
                    && !step.nodes.is_empty()
                    && step
                        .nodes
                        .iter()
                        .all(|node| node.len() <= 4096 && node.split('.').all(is_name)),
                "Task report pipeline step needs its role and nodes",
            )?;
            if let Some(TaskReportWorkerV1::Model {
                provider_kind,
                model,
                effort,
            }) = &step.worker
            {
                require(
                    is_name(provider_kind) && is_model_identity(model) && is_name(effort),
                    "Task report Worker names its Provider kind, a model identity and effort",
                )?;
            }
            require(
                step.checks.iter().all(|check| is_name(check))
                    && step.checks.windows(2).all(|pair| pair[0] < pair[1]),
                "Task report gate lists distinct check names in name order",
            )?;
            require(
                (step.role == TASK_REPORT_GATE_ROLE) != step.checks.is_empty()
                    && (step.role != TASK_REPORT_GATE_ROLE || step.worker.is_none()),
                "Task report gate lists its checks and has no Worker",
            )?;
        }
        Ok(())
    }
}

impl TaskReportFindingsV1 {
    pub fn validate(&self) -> Result<(), String> {
        let counts = [self.blocker, self.major, self.minor];
        require(
            counts
                .into_iter()
                .flatten()
                .chain([self.failed_reviewers])
                .all(safe_number),
            "Task report finding counts are bounded",
        )?;
        require(
            counts.iter().all(Option::is_some) || counts.iter().all(Option::is_none),
            "Task report finding counts are all known or all unknown",
        )?;
        require(
            self.review_ran || counts.iter().all(|count| *count == Some(0)),
            "Task report findings need a review that ran",
        )?;
        require(
            !(self.gate_failed && self.review_ran),
            "Task report gate failure means the review did not run",
        )?;
        require(
            self.review_ran || self.failed_reviewers == 0,
            "Task report failed reviewers began an Attempt",
        )
    }
}

impl TaskReportEntryV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id) && is_package_name(&self.kind),
            "Task report entry needs its Task ID and kind",
        )?;
        require(
            !self.outcome.trim().is_empty() && self.outcome.chars().count() <= 256,
            "Task report entry needs a bounded outcome",
        )?;
        require(
            self.pipeline.as_ref().is_none_or(|pipeline| {
                pipeline.split_once('@').is_some_and(|(name, version)| {
                    is_package_name(name)
                        && !version.is_empty()
                        && !version.chars().any(char::is_whitespace)
                })
            }),
            "Task report Pipeline is `name@version`",
        )?;
        require(
            [self.runs, self.wall_ms, self.active_ms]
                .into_iter()
                .chain(self.review_rounds)
                .all(safe_number)
                && self.active_ms <= self.wall_ms,
            "Task report times are bounded and active time never exceeds wall time",
        )?;
        require(
            !self.collected
                || self.nodes.is_none() && self.attempts.is_none() && self.findings.is_none(),
            "A collected Task records no Attempts",
        )?;
        if let Some(findings) = &self.findings {
            findings.validate()?;
        }
        if let Some(attempts) = &self.attempts {
            attempts.validate()?;
        }
        if let Some(nodes) = &self.nodes {
            let mut seen = Vec::new();
            for node in nodes {
                node.validate()?;
                let key = (node.node.as_str(), node.role.as_str(), node.worker.as_ref());
                require(
                    !seen.contains(&key),
                    "Task report lists a node under one Worker twice",
                )?;
                seen.push(key);
            }
            if let Some(attempts) = &self.attempts {
                require(
                    nodes.iter().map(|node| node.failed_attempts).sum::<u64>() == attempts.failed,
                    "Task report node failures must add up to the Task's",
                )?;
                require(
                    nodes.iter().map(|node| node.unknown_usage).sum::<u64>()
                        == attempts.unknown_usage,
                    "Task report node unknown usage must add up to the Task's",
                )?;
            }
        }
        Ok(())
    }
}

impl TaskReportAttemptsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.failed <= self.total && safe_number(self.total),
            "Task report failed Attempts never exceed its Attempts",
        )?;
        let mut classes = std::collections::BTreeSet::new();
        let mut attempts = 0u64;
        let mut tokens = 0u128;
        for failure in &self.failures {
            require(
                failure.attempts > 0 && classes.insert(failure.class),
                "Task report failure classes are distinct and non-empty",
            )?;
            attempts = attempts.saturating_add(failure.attempts);
            tokens = tokens.saturating_add(failure.tokens.get());
        }
        require(
            classes
                .iter()
                .copied()
                .eq(self.failures.iter().map(|f| f.class)),
            "Task report failure classes are in class order",
        )?;
        require(
            attempts == self.failed && tokens == self.failed_tokens.get(),
            "Task report failure classes must add up to its failed Attempts",
        )?;
        require(
            self.unknown_usage <= self.total,
            "Task report unknown usage never exceeds its Attempts",
        )?;
        let mut causes = std::collections::BTreeSet::new();
        let mut unknown = 0u64;
        for entry in &self.unknown_usage_causes {
            require(
                entry.attempts > 0 && causes.insert(entry.cause),
                "Task report unknown usage causes are distinct and non-empty",
            )?;
            unknown = unknown.saturating_add(entry.attempts);
        }
        require(
            causes
                .iter()
                .copied()
                .eq(self.unknown_usage_causes.iter().map(|u| u.cause)),
            "Task report unknown usage causes are in cause order",
        )?;
        require(
            unknown == self.unknown_usage,
            "Task report unknown usage causes must add up to its unknown usage",
        )
    }
}

impl TaskReportNodeV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.node.len() <= 4096 && self.node.split('.').all(is_name),
            "Task report node needs a qualified node",
        )?;
        require(is_name(&self.role), "Task report node needs a role")?;
        require(
            self.attempts > 0
                && self.failed_attempts <= self.attempts
                && self.unknown_usage <= self.attempts,
            "Task report node began an Attempt, and failed or left usage unknown no more than it began",
        )?;
        if let Some(TaskReportWorkerV1::Model {
            provider_kind,
            model,
            effort,
        }) = &self.worker
        {
            require(
                is_name(provider_kind) && is_model_identity(model) && is_name(effort),
                "Task report Worker names its Provider kind, a model identity and effort",
            )?;
        }
        for check in &self.checks {
            require(is_name(&check.name), "Task report check needs its name")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TaskReportPipelineV1 {
        TaskReportPipelineV1 {
            name: "fixture/plain".into(),
            version: "1.0.0".into(),
            steps: vec![
                TaskReportStepV1 {
                    stage: 1,
                    role: "implement".into(),
                    nodes: vec!["root.nodes.implement".into()],
                    worker: Some(TaskReportWorkerV1::Command {}),
                    checks: vec![],
                },
                TaskReportStepV1 {
                    stage: 2,
                    role: TASK_REPORT_GATE_ROLE.into(),
                    nodes: vec!["root.nodes.check".into()],
                    worker: None,
                    checks: vec!["fmt".into(), "test".into()],
                },
            ],
        }
    }

    /// The report over `tasks`, which all run `fixture/plain@1.0.0`.
    fn with_tasks(tasks: Vec<TaskReportEntryV1>) -> Result<TaskReportV1, String> {
        TaskReportV1::new(vec![plain()], tasks)
    }

    fn entry(task_id: &str, attempts: Option<(u64, u64)>) -> TaskReportEntryV1 {
        TaskReportEntryV1 {
            round: 0,
            task_id: task_id.into(),
            kind: "implement".into(),
            pipeline: Some("fixture/plain@1.0.0".into()),
            outcome: "pass".into(),
            collected: false,
            review_rounds: Some(1),
            findings: None,
            runs: 2,
            attempts: attempts.map(|(total, failed)| TaskReportAttemptsV1 {
                total,
                failed,
                failed_tokens: DecimalU128::from(u128::from(failed) * 10),
                failures: if failed == 0 {
                    vec![]
                } else {
                    vec![TaskReportFailureV1 {
                        class: TaskReportFailureClassV1::ProviderFailure,
                        attempts: failed,
                        tokens: DecimalU128::from(u128::from(failed) * 10),
                    }]
                },
                unknown_usage: 0,
                unknown_usage_causes: Vec::new(),
            }),
            chargeable_tokens: DecimalU128::from(100),
            wall_ms: 5_000,
            active_ms: 3_000,
            nodes: Some(vec![TaskReportNodeV1 {
                node: "root.nodes.implement".into(),
                role: "implement".into(),
                worker: Some(TaskReportWorkerV1::Model {
                    provider_kind: "codex".into(),
                    model: "gpt-6-sol".into(),
                    effort: "high".into(),
                }),
                attempts: attempts.map_or(1, |(total, _)| total.max(1)),
                failed_attempts: attempts.map_or(0, |(_, failed)| failed),
                tokens: DecimalU128::from(100),
                elapsed_ms: Some(2_000),
                checks: vec![],
                unknown_usage: 0,
            }]),
        }
    }

    #[test]
    fn totals_sum_known_parts_and_lose_a_part_any_task_does_not_record() {
        let report = with_tasks(vec![entry("a", Some((3, 1))), entry("b", Some((2, 0)))]).unwrap();
        assert_eq!(report.totals.tasks, 2);
        assert_eq!(report.totals.attempts, Some(5));
        assert_eq!(report.totals.failed_attempts, Some(1));
        assert_eq!(report.totals.review_rounds, Some(2));
        assert_eq!(report.totals.chargeable_tokens.get(), 200);
        assert_eq!(report.totals.active_ms, 6_000);

        let mut unknown = entry("c", None);
        unknown.nodes = None;
        unknown.review_rounds = None;
        let report = with_tasks(vec![entry("a", Some((3, 1))), unknown]).unwrap();
        assert_eq!(
            report.totals.attempts, None,
            "an unknown part is never zero"
        );
        assert_eq!(report.totals.review_rounds, None);
        assert_eq!(report.totals.chargeable_tokens.get(), 200);
    }

    #[test]
    fn a_report_refuses_inconsistent_figures() {
        assert!(with_tasks(vec![]).is_err());
        assert!(with_tasks(vec![entry("a", None), entry("a", None)]).is_err());
        let mut report = with_tasks(vec![entry("a", Some((3, 1)))]).unwrap();
        report.totals.active_ms += 1;
        assert!(report.validate().is_err(), "totals are the sums");
        let mut longer = entry("a", None);
        longer.active_ms = longer.wall_ms + 1;
        assert!(with_tasks(vec![longer]).is_err());
        let mut collected = entry("a", Some((1, 0)));
        collected.collected = true;
        assert!(with_tasks(vec![collected]).is_err());
        let mut classes = entry("a", Some((3, 1)));
        classes.attempts.as_mut().unwrap().failed = 2;
        assert!(with_tasks(vec![classes]).is_err());
        let mut pipeline = entry("a", None);
        pipeline.pipeline = Some("no-version".into());
        assert!(with_tasks(vec![pipeline]).is_err());
    }

    #[test]
    fn rounds_are_positions_and_pipelines_are_the_tasks_own_in_first_use_order() {
        let report = with_tasks(vec![entry("a", None), entry("b", None)]).unwrap();
        assert_eq!(
            report.tasks.iter().map(|t| t.round).collect::<Vec<_>>(),
            [1, 2]
        );
        let mut renumbered = report.clone();
        renumbered.tasks[1].round = 1;
        assert!(renumbered.validate().is_err(), "a round is a position");
        assert!(
            TaskReportV1::new(vec![], vec![entry("a", None)]).is_err(),
            "a Task's pipeline is listed"
        );
        assert!(
            TaskReportV1::new(vec![plain(), plain()], vec![entry("a", None)]).is_err(),
            "a pipeline is listed once"
        );
        let mut other = plain();
        other.name = "fixture/other".into();
        assert!(
            TaskReportV1::new(vec![plain(), other.clone()], vec![entry("a", None)]).is_err(),
            "a listed pipeline is used"
        );
        let mut second = entry("b", None);
        second.pipeline = Some("fixture/other@1.0.0".into());
        assert!(
            TaskReportV1::new(
                vec![other.clone(), plain()],
                vec![entry("a", None), second.clone()]
            )
            .is_err(),
            "pipelines are in first-use order"
        );
        TaskReportV1::new(vec![plain(), other], vec![entry("a", None), second]).unwrap();

        let step = |edit: &dyn Fn(&mut TaskReportPipelineV1)| {
            let mut pipeline = plain();
            edit(&mut pipeline);
            TaskReportV1::new(vec![pipeline], vec![entry("a", None)])
        };
        assert!(step(&|p| p.steps[1].stage = 3).is_err(), "a skipped stage");
        assert!(
            step(&|p| p.steps[0].stage = 2).is_err(),
            "stages start at 1"
        );
        assert!(
            step(&|p| p.steps[1].checks.clear()).is_err(),
            "a gate lists checks"
        );
        assert!(
            step(&|p| p.steps[1].checks.reverse()).is_err(),
            "checks in name order"
        );
        assert!(
            step(&|p| p.steps[0].checks = vec!["fmt".into()]).is_err(),
            "only a gate lists checks"
        );
        assert!(
            step(&|p| p.steps[0].nodes.clear()).is_err(),
            "a step has nodes"
        );
        assert!(
            step(&|p| p.steps[0].worker = Some(TaskReportWorkerV1::Model {
                provider_kind: "codex".into(),
                model: "/home/x/.codex".into(),
                effort: "high".into(),
            }))
            .is_err(),
            "a path as a model"
        );
        step(&|p| p.steps[1].stage = 1).unwrap();
    }

    #[test]
    fn findings_need_a_review_that_ran_and_a_failed_gate_means_it_did_not() {
        let findings = |review_ran, gate_failed, major: Option<u64>| TaskReportFindingsV1 {
            blocker: major.map(|_| 0),
            major,
            minor: major.map(|_| 0),
            review_ran,
            gate_failed,
            failed_reviewers: 0,
        };
        let partly_known = TaskReportFindingsV1 {
            minor: None,
            ..findings(true, false, Some(1))
        };
        for (value, valid) in [
            (findings(true, false, Some(6)), true),
            (findings(true, false, Some(0)), true),
            (findings(false, true, Some(0)), true),
            (findings(false, false, Some(0)), true),
            (findings(false, false, Some(1)), false),
            (findings(true, true, Some(0)), false),
            // An incomplete round: the review ran, its counts are unknown.
            (findings(true, false, None), true),
            (findings(false, false, None), false),
            (findings(false, true, None), false),
            (partly_known, false),
        ] {
            let mut task = entry("a", None);
            task.findings = Some(value.clone());
            assert_eq!(with_tasks(vec![task]).is_ok(), valid, "{value:?}");
        }
        let mut collected = entry("a", None);
        collected.collected = true;
        collected.nodes = None;
        collected.findings = Some(findings(true, false, Some(1)));
        assert!(with_tasks(vec![collected]).is_err());
    }

    #[test]
    fn a_model_is_copied_only_when_it_looks_like_a_model_identity() {
        for (model, identity) in TASK_REPORT_MODEL_CASES {
            assert_eq!(is_model_identity(model), identity, "{model:?}");
        }
        assert!(is_model_identity(&"m".repeat(TASK_REPORT_MODEL_MAX)));
        assert!(!is_model_identity(&"m".repeat(TASK_REPORT_MODEL_MAX + 1)));
        let rejected = TASK_REPORT_MODEL_CASES
            .iter()
            .filter(|(_, identity)| !identity)
            .map(|(model, _)| *model)
            .collect::<Vec<_>>();
        for model in [
            "alice@example.com",
            "model@host",
            "file:///etc/passwd",
            "C:/secrets/key",
            "C:\\key",
            "C:\\\\key",
            "/etc/x",
            "~/x",
            "a//b",
            "a/b/c",
            "../x",
            "",
        ] {
            assert!(rejected.contains(&model), "{model:?} is in the table");
        }
        for model in [
            "gpt-6-sol/high",
            "claude-opus-5-5",
            "gpt-5.3-codex-spark",
            "us.anthropic.claude-opus-5-5-v1:0",
        ] {
            assert!(
                TASK_REPORT_MODEL_CASES.contains(&(model, true)),
                "{model:?} is in the table"
            );
        }
    }

    #[test]
    fn a_node_may_appear_once_per_worker_binding() {
        let mut task = entry("a", Some((3, 1)));
        let mut other = task.nodes.as_ref().unwrap()[0].clone();
        other.failed_attempts = 0;
        task.nodes.as_mut().unwrap().push(other.clone());
        assert!(
            with_tasks(vec![task.clone()]).is_err(),
            "the same node under the same Worker twice"
        );
        other.worker = Some(TaskReportWorkerV1::Model {
            provider_kind: "claude".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
        });
        task.nodes.as_mut().unwrap()[1] = other;
        with_tasks(vec![task.clone()]).unwrap();
        let mut path = task;
        path.nodes.as_mut().unwrap()[1].worker = Some(TaskReportWorkerV1::Model {
            provider_kind: "claude".into(),
            model: "/home/fixture/.claude".into(),
            effort: "high".into(),
        });
        assert!(with_tasks(vec![path]).is_err(), "a path as a model");
    }

    #[test]
    fn failure_classes_spell_their_feedback_codes() {
        use super::super::feedback::TaskFeedbackCodeV1;
        for code in [
            TaskFeedbackCodeV1::InvalidOutputContract,
            TaskFeedbackCodeV1::ProcessFailure,
            TaskFeedbackCodeV1::ProviderFailure,
            TaskFeedbackCodeV1::ContextRejected,
            TaskFeedbackCodeV1::OutputAdmissionRejected,
            TaskFeedbackCodeV1::CompilerRejected,
        ] {
            let class = TaskReportFailureClassV1::from(code);
            assert_eq!(serde_json::to_value(code).unwrap(), class.as_str());
            assert_eq!(serde_json::to_value(class).unwrap(), class.as_str());
        }
    }
}
