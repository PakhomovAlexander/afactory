//! What `af task report` prints (ADR-0142): what recorded Tasks cost and how they ran, read from
//! the Store and never estimated. A figure the Store does not record is absent, never zero.
//!
//! The document names a Provider only by its kind, model and effort. It has no field for a
//! Provider registry label or principal, a path, a credential, a prompt or Worker output, and
//! every object is closed, so none of them can be carried by accident.

use serde::{Deserialize, Serialize};

use super::usage::DecimalU128;
use super::{is_name, is_package_name, require, safe_number};

pub const TASK_REPORT_V1: &str = "af/task-report@1";

/// The first line of the Markdown block a pull request description carries.
pub const TASK_REPORT_BEGIN: &str = "<!-- af-task-report:v1 -->";
/// The last line of that block.
pub const TASK_REPORT_END: &str = "<!-- /af-task-report -->";
/// The summary table's columns, in order: `scripts/check-pr-report.py` requires every one.
pub const TASK_REPORT_COLUMNS: [&str; 9] = [
    "Task",
    "Kind",
    "Pipeline",
    "Outcome",
    "Rounds",
    "Attempts",
    "Tokens",
    "Active time",
    "Wall time",
];
/// The totals line starts with this text.
pub const TASK_REPORT_TOTALS: &str = "**Totals:**";
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
    /// One entry per requested Task, in the order the command named them.
    pub tasks: Vec<TaskReportEntryV1>,
    pub totals: TaskReportTotalsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportEntryV1 {
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
    /// Writer leases during which the Task executed work: `af task start --execute` and every
    /// `af task run` that resumed it.
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
        for task in tasks {
            tokens = tokens
                .checked_add(task.chargeable_tokens.get())
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
            active_ms: active,
        })
    }
}

impl TaskReportV1 {
    pub fn new(tasks: Vec<TaskReportEntryV1>) -> Result<Self, String> {
        let report = Self {
            schema: TASK_REPORT_V1.into(),
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
        for task in &self.tasks {
            task.validate()?;
            require(
                seen.insert(task.task_id.as_str()),
                "Task report names a Task twice",
            )?;
        }
        require(
            self.totals == TaskReportTotalsV1::of(&self.tasks)?,
            "Task report totals must be the sums of its Tasks",
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
            !self.collected || self.nodes.is_none() && self.attempts.is_none(),
            "A collected Task records no Attempts",
        )?;
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
            self.attempts > 0 && self.failed_attempts <= self.attempts,
            "Task report node began an Attempt, and failed no more than it began",
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

    fn entry(task_id: &str, attempts: Option<(u64, u64)>) -> TaskReportEntryV1 {
        TaskReportEntryV1 {
            task_id: task_id.into(),
            kind: "implement".into(),
            pipeline: Some("fixture/plain@1.0.0".into()),
            outcome: "pass".into(),
            collected: false,
            review_rounds: Some(1),
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
            }]),
        }
    }

    #[test]
    fn totals_sum_known_parts_and_lose_a_part_any_task_does_not_record() {
        let report =
            TaskReportV1::new(vec![entry("a", Some((3, 1))), entry("b", Some((2, 0)))]).unwrap();
        assert_eq!(report.totals.tasks, 2);
        assert_eq!(report.totals.attempts, Some(5));
        assert_eq!(report.totals.failed_attempts, Some(1));
        assert_eq!(report.totals.review_rounds, Some(2));
        assert_eq!(report.totals.chargeable_tokens.get(), 200);
        assert_eq!(report.totals.active_ms, 6_000);

        let mut unknown = entry("c", None);
        unknown.nodes = None;
        unknown.review_rounds = None;
        let report = TaskReportV1::new(vec![entry("a", Some((3, 1))), unknown]).unwrap();
        assert_eq!(
            report.totals.attempts, None,
            "an unknown part is never zero"
        );
        assert_eq!(report.totals.review_rounds, None);
        assert_eq!(report.totals.chargeable_tokens.get(), 200);
    }

    #[test]
    fn a_report_refuses_inconsistent_figures() {
        assert!(TaskReportV1::new(vec![]).is_err());
        assert!(TaskReportV1::new(vec![entry("a", None), entry("a", None)]).is_err());
        let mut report = TaskReportV1::new(vec![entry("a", Some((3, 1)))]).unwrap();
        report.totals.active_ms += 1;
        assert!(report.validate().is_err(), "totals are the sums");
        let mut longer = entry("a", None);
        longer.active_ms = longer.wall_ms + 1;
        assert!(TaskReportV1::new(vec![longer]).is_err());
        let mut collected = entry("a", Some((1, 0)));
        collected.collected = true;
        assert!(TaskReportV1::new(vec![collected]).is_err());
        let mut classes = entry("a", Some((3, 1)));
        classes.attempts.as_mut().unwrap().failed = 2;
        assert!(TaskReportV1::new(vec![classes]).is_err());
        let mut pipeline = entry("a", None);
        pipeline.pipeline = Some("no-version".into());
        assert!(TaskReportV1::new(vec![pipeline]).is_err());
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
            TaskReportV1::new(vec![task.clone()]).is_err(),
            "the same node under the same Worker twice"
        );
        other.worker = Some(TaskReportWorkerV1::Model {
            provider_kind: "claude".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
        });
        task.nodes.as_mut().unwrap()[1] = other;
        TaskReportV1::new(vec![task.clone()]).unwrap();
        let mut path = task;
        path.nodes.as_mut().unwrap()[1].worker = Some(TaskReportWorkerV1::Model {
            provider_kind: "claude".into(),
            model: "/home/fixture/.claude".into(),
            effort: "high".into(),
        });
        assert!(TaskReportV1::new(vec![path]).is_err(), "a path as a model");
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
