//! What a Task check node observed while each of its local checks ran, `af/TaskCheckReport@1`,
//! retained beside the check receipt as evidence of the Attempt that ran them.
//!
//! Two sources, kept apart:
//!
//! - **The host, as af measured it**: the 1-minute load average and the CPUs available to af
//!   when the check started and when it ended. A gate that failed on a loaded host says so.
//! - **The check's own account**, the lines it wrote to the file `$AF_CHECK_REPORT` names
//!   (`af.check-report/1`): every step it ran with the stage the step belongs to (so a gate that
//!   stopped at a project preflight is not read as a test failure), its test thread count, the
//!   tests that failed in its parallel run, and those that failed there but passed when run
//!   again alone (`flaky`). That account never decides the check's status: the exit code does,
//!   and the receipt records it. A check that wrote nothing is `absent`; one whose lines this
//!   release does not accept is `refused`, with the reason, and its result stands.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{is_name, present_option, require};
use crate::is_digest;

pub const TASK_CHECK_REPORT_V1: &str = "af/TaskCheckReport@1";
/// The schema every line a check writes to `$AF_CHECK_REPORT` names.
pub const CHECK_REPORT_LINE_SCHEMA: &str = "af.check-report/1";
/// The environment variable naming the file a local check may write its account to.
pub const CHECK_REPORT_ENV: &str = "AF_CHECK_REPORT";
/// The most bytes af reads of that file; a longer one is refused.
pub const MAX_CHECK_REPORT_BYTES: u64 = 256 * 1024;
pub const MAX_CHECK_REPORT_LINES: usize = 256;
pub const MAX_CHECK_STEPS: usize = 64;
pub const MAX_STEP_COMMAND_ARGS: usize = 64;
/// The failed tests a report names; `failed_count` keeps the whole number.
pub const MAX_NAMED_FAILED_TESTS: usize = 100;
/// The tests a gate reruns alone at most (ADR-0124), so the most it can report flaky.
pub const MAX_FLAKY_TESTS: usize = 25;
const MAX_TEXT_CHARS: usize = 1024;
const MAX_TEST_NAME_CHARS: usize = 512;
const MAX_REFUSAL_CHARS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckReportV1 {
    /// The `af/TaskCheckReceipt@1` these checks' results are recorded in.
    pub receipt_id: String,
    /// One entry per local check that started, by check name.
    pub checks: BTreeMap<String, TaskCheckRunV1>,
}

impl TaskCheckReportV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.receipt_id) && !self.checks.is_empty() && self.checks.len() <= 32,
            "Check report requires its receipt and at most 32 named checks",
        )?;
        for (name, run) in &self.checks {
            require(is_name(name), "Check report names an invalid check")?;
            run.validate()
                .map_err(|error| format!("Check report of `{name}`: {error}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckRunV1 {
    pub host: TaskCheckHostV1,
    /// Whether the check wrote an account af admitted.
    pub report: CheckReportStateV1,
    /// Why a written account was refused; present exactly when `report` is `refused`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub refusal: Option<String>,
    /// The steps the check reported, in the order it ran them; empty unless `admitted`.
    pub steps: Vec<CheckStepV1>,
    /// Its test run, when it reported one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub tests: Option<CheckTestsV1>,
}

impl TaskCheckRunV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.host.validate()?;
        require(
            (self.report == CheckReportStateV1::Refused) == self.refusal.is_some()
                && self
                    .refusal
                    .as_deref()
                    .is_none_or(|reason| bounded_text(reason, MAX_REFUSAL_CHARS))
                && (self.report == CheckReportStateV1::Admitted
                    || (self.steps.is_empty() && self.tests.is_none())),
            "a refusal names its reason, and only an admitted report carries steps or tests",
        )?;
        validate_reported(&self.steps, self.tests.as_ref())
    }

    /// The first step that did not exit 0: where the check stopped.
    pub fn stopped_at(&self) -> Option<&CheckStepV1> {
        self.steps.iter().find(|step| step.exit_code != 0)
    }
}

/// The host as af measured it around one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckHostV1 {
    /// The CPUs available to af (its scheduler affinity and quota, where the host has them)
    /// when the check started.
    pub cpus_start: u32,
    /// The same when the check ended.
    pub cpus_end: u32,
    /// The 1-minute load average when the check started, to two decimals (`"7.42"`); absent
    /// when the host did not report one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub load_1m_start: Option<String>,
    /// The same when the check ended.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub load_1m_end: Option<String>,
}

impl TaskCheckHostV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [self.cpus_start, self.cpus_end]
                .iter()
                .all(|cpus| (1..=65_536).contains(cpus))
                && [&self.load_1m_start, &self.load_1m_end]
                    .into_iter()
                    .flatten()
                    .all(|load| is_load(load)),
            "host observation needs CPU counts and two-decimal load averages",
        )
    }
}

/// A load average as af records it: two decimals, as `uptime` prints it.
pub fn render_load(load: f64) -> Option<String> {
    (load.is_finite() && (0.0..1_000_000.0).contains(&load)).then(|| format!("{load:.2}"))
}

fn is_load(value: &str) -> bool {
    value.split_once('.').is_some_and(|(whole, fraction)| {
        !whole.is_empty()
            && whole.len() <= 6
            && whole.bytes().all(|b| b.is_ascii_digit())
            && (whole == "0" || !whole.starts_with('0'))
            && fraction.len() == 2
            && fraction.bytes().all(|b| b.is_ascii_digit())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckReportStateV1 {
    /// The check wrote nothing to `$AF_CHECK_REPORT`.
    Absent,
    Admitted,
    /// The check wrote lines this release does not accept; nothing of them is kept.
    Refused,
}

/// Where in a gate a step belongs. `preflight` is a project check that runs before any build
/// (a stale pin, a script's own tests); `tests` is the test run itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStageV1 {
    Preflight,
    Lint,
    Build,
    Tests,
    Release,
}

impl CheckStageV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Lint => "lint",
            Self::Build => "build",
            Self::Tests => "tests",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckStepV1 {
    pub label: String,
    pub stage: CheckStageV1,
    pub exit_code: i32,
    /// The command the step ran, to reproduce it.
    pub command: Vec<String>,
    /// The one command that repairs what the step refuses, when the gate names one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub fix: Option<String>,
}

impl CheckStepV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.label)
                && !self.command.is_empty()
                && self.command.len() <= MAX_STEP_COMMAND_ARGS
                && self
                    .command
                    .iter()
                    .all(|arg| arg.is_empty() || bounded_text(arg, MAX_TEXT_CHARS))
                && !self.command[0].is_empty()
                && self
                    .fix
                    .as_deref()
                    .is_none_or(|fix| bounded_text(fix, MAX_TEXT_CHARS)),
            "a step needs a name, its command and at most a bounded printable fix",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcomeV1 {
    Passed,
    Failed,
}

/// A test that failed in the parallel run and passed when run again alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlakyTestV1 {
    pub name: String,
    pub parallel: TestOutcomeV1,
    pub isolated: TestOutcomeV1,
}

/// What the gate did with the tests that failed in its parallel run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRerunV1 {
    /// Nothing failed.
    NotNeeded,
    /// Each failed test ran again alone, one at a time with no other test alive.
    Isolated,
    /// More failed than the rerun's bound (25 tests, 15 minutes): none was rerun past it, and
    /// the check failed with the whole list.
    OverBound,
    /// The run failed before any test result could be read (a build error, say): nothing ran
    /// again.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckTestsV1 {
    /// The test threads the parallel run used.
    pub test_threads: u32,
    /// Every test that failed and was not shown flaky: it failed again alone, or never ran
    /// again.
    pub failed_count: u32,
    /// The first of those, by name.
    pub failed: Vec<String>,
    pub flaky: Vec<FlakyTestV1>,
    pub rerun: CheckRerunV1,
}

impl CheckTestsV1 {
    fn validate(&self) -> Result<(), String> {
        require(
            self.test_threads > 0
                && self.test_threads <= 65_536
                && self.failed.len() <= MAX_NAMED_FAILED_TESTS
                && self.failed.len() as u64 <= u64::from(self.failed_count)
                && (self.failed_count as usize >= MAX_NAMED_FAILED_TESTS
                    || self.failed.len() == self.failed_count as usize)
                && self.flaky.len() <= MAX_FLAKY_TESTS
                && self
                    .failed
                    .iter()
                    .chain(self.flaky.iter().map(|test| &test.name))
                    .all(|name| bounded_text(name, MAX_TEST_NAME_CHARS))
                && self.flaky.iter().all(|test| {
                    test.parallel == TestOutcomeV1::Failed && test.isolated == TestOutcomeV1::Passed
                })
                && match self.rerun {
                    CheckRerunV1::NotNeeded => self.failed_count == 0 && self.flaky.is_empty(),
                    CheckRerunV1::Isolated => self.failed_count > 0 || !self.flaky.is_empty(),
                    CheckRerunV1::OverBound => self.failed_count > 0,
                    CheckRerunV1::NotRun => self.flaky.is_empty(),
                },
            "a test report needs its thread count, bounded test names, flaky tests that failed \
             in parallel and passed alone, and a rerun state that agrees with them",
        )
    }
}

fn validate_reported(steps: &[CheckStepV1], tests: Option<&CheckTestsV1>) -> Result<(), String> {
    require(
        steps.len() <= MAX_CHECK_STEPS,
        "a check reports at most 64 steps",
    )?;
    for step in steps {
        step.validate()?;
    }
    tests.map_or(Ok(()), CheckTestsV1::validate)
}

/// Printable, bounded and one line: what a report names is printed to an operator's terminal.
fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

/// One line of `$AF_CHECK_REPORT`.
#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ReportLine {
    Step(StepLine),
    Tests(TestsLine),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepLine {
    schema: String,
    label: String,
    stage: CheckStageV1,
    exit_code: i32,
    command: Vec<String>,
    #[serde(default, deserialize_with = "present_option")]
    fix: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestsLine {
    schema: String,
    test_threads: u32,
    failed_count: u32,
    failed: Vec<String>,
    flaky: Vec<FlakyTestV1>,
    rerun: CheckRerunV1,
}

/// The steps and test run a check wrote to `$AF_CHECK_REPORT`: one JSON object per line, each
/// `af.check-report/1` and either a `step` or the one `tests` record. Anything else refuses the
/// whole report, with the reason.
pub fn parse_check_report(
    bytes: &[u8],
) -> Result<(Vec<CheckStepV1>, Option<CheckTestsV1>), String> {
    if bytes.len() as u64 > MAX_CHECK_REPORT_BYTES {
        return Err(format!(
            "the report holds more than {MAX_CHECK_REPORT_BYTES} bytes"
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the report is not UTF-8".to_owned())?;
    let mut steps = Vec::new();
    let mut tests = None;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_CHECK_REPORT_LINES {
            return Err(format!(
                "the report holds more than {MAX_CHECK_REPORT_LINES} lines"
            ));
        }
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 1;
        let record: ReportLine = serde_json::from_str(line).map_err(|error| {
            format!("line {number} is not an {CHECK_REPORT_LINE_SCHEMA} record: {error}")
        })?;
        let schema = match &record {
            ReportLine::Step(step) => &step.schema,
            ReportLine::Tests(tests) => &tests.schema,
        };
        if schema != CHECK_REPORT_LINE_SCHEMA {
            return Err(format!(
                "line {number} names schema {schema:?}, not {CHECK_REPORT_LINE_SCHEMA:?}"
            ));
        }
        match record {
            ReportLine::Step(step) => steps.push(CheckStepV1 {
                label: step.label,
                stage: step.stage,
                exit_code: step.exit_code,
                command: step.command,
                fix: step.fix,
            }),
            ReportLine::Tests(line) => {
                if tests.is_some() {
                    return Err(format!("line {number} is a second tests record"));
                }
                tests = Some(CheckTestsV1 {
                    test_threads: line.test_threads,
                    failed_count: line.failed_count,
                    failed: line.failed,
                    flaky: line.flaky,
                    rerun: line.rerun,
                });
            }
        }
    }
    validate_reported(&steps, tests.as_ref())?;
    Ok((steps, tests))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(value: serde_json::Value) -> String {
        value.to_string() + "\n"
    }

    fn step(label: &str, stage: &str, exit_code: i32) -> String {
        line(
            json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "step", "label": label,
            "stage": stage, "exit_code": exit_code, "command": ["python3", "scripts/x.py"]}),
        )
    }

    #[test]
    fn a_report_names_its_steps_in_order_and_where_the_check_stopped() {
        let text = step("task-preflight", "preflight", 0)
            + &line(json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "step",
                "label": "performance-kernel-harness", "stage": "preflight", "exit_code": 1,
                "command": ["python3", "scripts/performance/cli.py", "harness-digest", "--check"],
                "fix": "python3 -I scripts/performance/cli.py harness-digest --write"}));
        let (steps, tests) = parse_check_report(text.as_bytes()).unwrap();
        assert!(tests.is_none());
        let run = TaskCheckRunV1 {
            host: TaskCheckHostV1 {
                cpus_start: 14,
                cpus_end: 14,
                load_1m_start: render_load(7.4199),
                load_1m_end: render_load(0.5),
            },
            report: CheckReportStateV1::Admitted,
            refusal: None,
            steps,
            tests,
        };
        run.validate().unwrap();
        assert_eq!(run.host.load_1m_start.as_deref(), Some("7.42"));
        assert_eq!(run.host.load_1m_end.as_deref(), Some("0.50"));
        let stopped = run.stopped_at().unwrap();
        assert_eq!(
            (stopped.label.as_str(), stopped.stage),
            ("performance-kernel-harness", CheckStageV1::Preflight)
        );
        assert_eq!(
            stopped.fix.as_deref(),
            Some("python3 -I scripts/performance/cli.py harness-digest --write")
        );
    }

    #[test]
    fn a_test_record_keeps_both_outcomes_of_each_flaky_test() {
        let text = step("test", "tests", 0)
            + &line(
                json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "tests",
                "test_threads": 7, "failed_count": 0, "failed": [],
                "flaky": [{"name": "af::it task_heavy::x", "parallel": "failed",
                    "isolated": "passed"}],
                "rerun": "isolated"}),
            );
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        assert_eq!(tests.test_threads, 7);
        assert_eq!(tests.flaky[0].name, "af::it task_heavy::x");
        assert_eq!(
            (tests.flaky[0].parallel, tests.flaky[0].isolated),
            (TestOutcomeV1::Failed, TestOutcomeV1::Passed)
        );
    }

    #[test]
    fn every_shape_outside_the_contract_refuses_the_whole_report() {
        let tests = |flaky: serde_json::Value, failed_count: u32, rerun: &str| {
            line(
                json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "tests",
                "test_threads": 4, "failed_count": failed_count, "failed": [],
                "flaky": flaky, "rerun": rerun}),
            )
        };
        for (why, text) in [
            ("not JSON", "flaky: x\n".to_owned()),
            (
                "another schema",
                line(
                    json!({"schema": "af.check-report/2", "record": "step", "label": "a",
                    "stage": "tests", "exit_code": 0, "command": ["x"]}),
                ),
            ),
            (
                "an unknown field",
                line(
                    json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "step", "label": "a",
                    "stage": "tests", "exit_code": 0, "command": ["x"], "passed": true}),
                ),
            ),
            (
                "an unknown stage",
                line(
                    json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "step", "label": "a",
                    "stage": "deploy", "exit_code": 0, "command": ["x"]}),
                ),
            ),
            (
                "an unknown record",
                line(json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "verdict"})),
            ),
            (
                "a control character a terminal would act on",
                line(
                    json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "step", "label": "a",
                    "stage": "tests", "exit_code": 1, "command": ["x"],
                    "fix": "run \u{1b}[2Jthis"}),
                ),
            ),
            (
                "a flaky test that passed in parallel",
                tests(
                    json!([{"name": "t", "parallel": "passed", "isolated": "passed"}]),
                    0,
                    "isolated",
                ),
            ),
            ("failures nothing reran", tests(json!([]), 3, "not_needed")),
            (
                "two test records",
                tests(json!([]), 0, "not_needed") + &tests(json!([]), 0, "not_needed"),
            ),
        ] {
            assert!(parse_check_report(text.as_bytes()).is_err(), "{why}");
        }
        assert!(parse_check_report(&[0xff, b'\n']).is_err(), "not UTF-8");
        let long = step("a", "tests", 0).repeat(300);
        assert!(
            parse_check_report(long.as_bytes()).is_err(),
            "too many lines"
        );
        assert_eq!(parse_check_report(b"").unwrap(), (Vec::new(), None));
    }

    #[test]
    fn only_two_decimal_loads_and_a_named_refusal_are_recorded() {
        for load in ["0.00", "7.42", "120.50"] {
            assert!(is_load(load), "{load}");
        }
        for load in ["7.4", "07.42", "-1.00", "1e3", "nan", "7.420"] {
            assert!(!is_load(load), "{load}");
        }
        assert_eq!(render_load(f64::NAN), None);
        let refused = TaskCheckRunV1 {
            host: TaskCheckHostV1 {
                cpus_start: 4,
                cpus_end: 4,
                load_1m_start: None,
                load_1m_end: None,
            },
            report: CheckReportStateV1::Refused,
            refusal: None,
            steps: Vec::new(),
            tests: None,
        };
        assert!(refused.validate().is_err());
        TaskCheckRunV1 {
            refusal: Some("line 1 is not JSON".into()),
            ..refused
        }
        .validate()
        .unwrap();
    }
}
