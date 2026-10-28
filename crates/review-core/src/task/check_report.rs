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
//!
//! A check writes one `tests` record per test run, and af folds them into one ([`fold_tests`]):
//! a gate that runs one nextest selection per test class writes one per class. The gate's
//! failed and flaky tests and the step that stopped a check come from its report when the check
//! writes one, and from the text scan of its output only for a check without one (absent or
//! refused). The report still never decides the check's status (ADR-0258).

use std::collections::{BTreeMap, BTreeSet};

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
    /// The `af/TaskCheckReceipt@2` these checks' results are recorded in.
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
/// `af.check-report/1` and either a `step` or a `tests` record, one per test run, each of which
/// must hold alone; the runs fold into one by [`fold_tests`]. Anything else refuses the whole
/// report, with the reason.
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
    let mut runs = Vec::new();
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
                let run = CheckTestsV1 {
                    test_threads: line.test_threads,
                    failed_count: line.failed_count,
                    failed: line.failed,
                    flaky: line.flaky,
                    rerun: line.rerun,
                };
                run.validate()
                    .map_err(|error| format!("line {number}: {error}"))?;
                runs.push((number, run));
            }
        }
    }
    let tests = fold_tests(runs)?;
    validate_reported(&steps, tests.as_ref())?;
    Ok((steps, tests))
}

/// The one test run of a report whose check wrote `runs`, each valid alone and paired with the
/// line it stands on, in the order it wrote them; `None` for none. `test_threads` is the first
/// run's. `failed` is the ordered union of the runs' names, each once, the first
/// [`MAX_NAMED_FAILED_TESTS`]. `failed_count` is the number of distinct failed tests:
///
/// - when every run names all of its failures (each run's `failed_count` below
///   [`MAX_NAMED_FAILED_TESTS`]), it is the size of that union: the names are complete, so a
///   repeated name, inside one run or across runs, is one test;
/// - when any run is capped (its `failed_count` at or above [`MAX_NAMED_FAILED_TESTS`], so it
///   counts failures it does not name), it is the checked sum of the runs' counts less one for
///   each name a later run repeats from an earlier run; a name repeated inside one run is
///   deduplicated in `failed` but stays counted, because the capped run's unnamed failures make
///   the true distinct count unknowable and the fold never undercounts. A sum that does not fit
///   `failed_count` refuses the whole report, naming the line of the record that overflowed.
///
/// `flaky` is the runs' entries in order, each name once, the first [`MAX_FLAKY_TESTS`].
/// `rerun` is the strongest of the runs' by `over_bound` > `isolated` > `not_run` >
/// `not_needed`. The result holds [`CheckTestsV1::validate`] (ADR-0258 gives the reasons).
pub fn fold_tests(runs: Vec<(usize, CheckTestsV1)>) -> Result<Option<CheckTestsV1>, String> {
    let precedence = |rerun: CheckRerunV1| match rerun {
        CheckRerunV1::OverBound => 3,
        CheckRerunV1::Isolated => 2,
        CheckRerunV1::NotRun => 1,
        CheckRerunV1::NotNeeded => 0,
    };
    let mut folded: Option<CheckTestsV1> = None;
    let mut capped = false;
    // The runs' counts less their cross-run repeats: the count when a run is capped.
    let mut summed = 0u32;
    // Every name an earlier run wrote.
    let mut earlier = BTreeSet::new();
    for (number, run) in runs {
        let folded = folded.get_or_insert_with(|| CheckTestsV1 {
            test_threads: run.test_threads,
            failed_count: 0,
            failed: Vec::new(),
            flaky: Vec::new(),
            rerun: run.rerun,
        });
        capped |= run.failed_count as usize >= MAX_NAMED_FAILED_TESTS;
        let mut own = BTreeSet::new();
        let mut repeated = 0u32;
        for name in run.failed {
            if !own.insert(name.clone()) {
                continue;
            }
            if earlier.contains(&name) {
                repeated += 1;
            } else {
                folded.failed.push(name);
            }
        }
        earlier.extend(own);
        // A valid run names at most the tests it counts, so its repeats never exceed its count.
        summed = run
            .failed_count
            .checked_sub(repeated)
            .and_then(|added| summed.checked_add(added))
            .ok_or_else(|| {
                format!(
                    "line {number}: the runs' failed tests number more than {}",
                    u32::MAX
                )
            })?;
        folded.flaky.extend(run.flaky);
        if precedence(run.rerun) > precedence(folded.rerun) {
            folded.rerun = run.rerun;
        }
    }
    Ok(folded.map(|mut folded| {
        // Below the bound every run names fewer than 100 tests in at most 256 lines, so the
        // union's size fits `failed_count`.
        folded.failed_count = if capped {
            summed
        } else {
            folded.failed.len() as u32
        };
        folded.failed.truncate(MAX_NAMED_FAILED_TESTS);
        let mut seen = BTreeSet::new();
        folded.flaky.retain(|test| seen.insert(test.name.clone()));
        folded.flaky.truncate(MAX_FLAKY_TESTS);
        folded
    }))
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

    fn tests_record(
        threads: u32,
        failed_count: u32,
        failed: &[&str],
        flaky: &[&str],
        rerun: &str,
    ) -> String {
        let flaky: Vec<_> = flaky
            .iter()
            .map(|name| json!({"name": name, "parallel": "failed", "isolated": "passed"}))
            .collect();
        line(
            json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "tests",
            "test_threads": threads, "failed_count": failed_count, "failed": failed,
            "flaky": flaky, "rerun": rerun}),
        )
    }

    fn refs(names: &[String]) -> Vec<&str> {
        names.iter().map(String::as_str).collect()
    }

    fn names(tests: &CheckTestsV1) -> (Vec<&str>, Vec<&str>) {
        (
            tests.failed.iter().map(String::as_str).collect(),
            tests.flaky.iter().map(|test| test.name.as_str()).collect(),
        )
    }

    /// One `tests` record per test run (a gate that runs one nextest selection per class): the
    /// runs fold into the report's one run, the first run's threads, the distinct failed tests
    /// counted, the names in order and each once, and the strongest rerun state.
    #[test]
    fn several_check_report_tests_records_fold_into_one_run() {
        let text = step("task-preflight", "preflight", 0)
            + &tests_record(7, 0, &[], &["af::it a::flaky"], "isolated")
            + &tests_record(
                4,
                2,
                &["af::it b::one", "af::it b::two"],
                &["af::it c::flaky", "af::it a::flaky"],
                "isolated",
            )
            + &tests_record(2, 2, &["af::it b::two", "af::it d::three"], &[], "not_run")
            + &step("test", "tests", 100);
        let (steps, tests) = parse_check_report(text.as_bytes()).unwrap();
        assert_eq!(steps.len(), 2);
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!(tests.test_threads, 7, "the first run's threads");
        assert_eq!(
            names(&tests),
            (
                vec!["af::it b::one", "af::it b::two", "af::it d::three"],
                vec!["af::it a::flaky", "af::it c::flaky"]
            )
        );
        assert_eq!(
            tests.failed_count, 3,
            "the distinct failed tests, one that failed in two runs counted once"
        );
        assert_eq!(tests.rerun, CheckRerunV1::Isolated);

        // `over_bound` > `isolated` > `not_run` > `not_needed`, whatever the order.
        let hundred: Vec<String> = (0..100).map(|n| format!("e::{n}")).collect();
        let hundred: Vec<&str> = hundred.iter().map(String::as_str).collect();
        for (records, rerun) in [
            (
                vec![
                    tests_record(1, 0, &[], &[], "not_needed"),
                    tests_record(1, 150, &hundred, &[], "over_bound"),
                    tests_record(1, 1, &["f::1"], &["f::2"], "isolated"),
                ],
                CheckRerunV1::OverBound,
            ),
            (
                vec![
                    tests_record(1, 1, &["f::1"], &[], "not_run"),
                    tests_record(1, 0, &[], &[], "not_needed"),
                ],
                CheckRerunV1::NotRun,
            ),
            (
                vec![
                    tests_record(1, 0, &[], &[], "not_needed"),
                    tests_record(1, 0, &[], &[], "not_needed"),
                ],
                CheckRerunV1::NotNeeded,
            ),
        ] {
            let (_, tests) = parse_check_report(records.concat().as_bytes()).unwrap();
            let tests = tests.unwrap();
            tests.validate().unwrap();
            assert_eq!(tests.rerun, rerun, "{records:?}");
        }

        // One record is admitted as it was written.
        let one = tests_record(3, 2, &["g::1", "g::2"], &["g::3"], "isolated");
        let (_, tests) = parse_check_report(one.as_bytes()).unwrap();
        let tests = tests.unwrap();
        assert_eq!((tests.test_threads, tests.failed_count), (3, 2));
        assert_eq!(names(&tests), (vec!["g::1", "g::2"], vec!["g::3"]));
    }

    /// The fold keeps the report's bounds: at most 100 failed tests named and 25 flaky, the
    /// first in the order the runs wrote them, while `failed_count` keeps the whole number of
    /// distinct failed tests.
    #[test]
    fn a_check_report_fold_keeps_the_name_bounds_and_the_distinct_count() {
        let first: Vec<String> = (0..100).map(|n| format!("first::{n}")).collect();
        let second: Vec<String> = (0..100).map(|n| format!("second::{n}")).collect();
        let flaky_first: Vec<String> = (0..20).map(|n| format!("flaky_first::{n}")).collect();
        let flaky_second: Vec<String> = (0..20).map(|n| format!("flaky_second::{n}")).collect();
        let text = tests_record(8, 150, &refs(&first), &refs(&flaky_first), "over_bound")
            + &tests_record(8, 120, &refs(&second), &refs(&flaky_second), "over_bound");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!(tests.failed_count, 270);
        assert_eq!(tests.failed, first, "the first 100 names, in order");
        assert_eq!(tests.flaky.len(), MAX_FLAKY_TESTS);
        let (_, flaky) = names(&tests);
        assert_eq!(flaky[..20], refs(&flaky_first)[..]);
        assert_eq!(flaky[20..], refs(&flaky_second)[..5]);

        // 60 + 60 named, 20 of them in both: 100 distinct failed tests.
        let a: Vec<String> = (0..60).map(|n| format!("t::{n}")).collect();
        let b: Vec<String> = (40..100).map(|n| format!("t::{n}")).collect();
        let text = tests_record(8, 60, &refs(&a), &[], "isolated")
            + &tests_record(8, 60, &refs(&b), &[], "isolated");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!((tests.failed_count, tests.failed.len()), (100, 100));

        // A run that names 30 of the 150 it counts contributes its whole count; the second run
        // repeats two of those names, so 150 distinct failed tests.
        let text = tests_record(8, 150, &refs(&a[..30]), &[], "over_bound")
            + &tests_record(8, 2, &refs(&a[..2]), &[], "isolated");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!((tests.failed_count, tests.failed.len()), (150, 30));
        assert_eq!(tests.rerun, CheckRerunV1::OverBound);
    }

    /// Two runs that fail the same test fail one test: the folded count is the number of
    /// distinct failed tests, and the name appears once.
    #[test]
    fn two_check_report_tests_records_that_fail_one_test_count_it_once() {
        let text = tests_record(4, 1, &["af::it x"], &[], "isolated")
            + &tests_record(4, 1, &["af::it x"], &[], "isolated");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!(tests.failed_count, 1);
        assert_eq!(tests.failed, ["af::it x"]);
    }

    /// A run below the bound names all of its failures, so a name it writes twice is one test:
    /// the folded count is the size of the names' union.
    #[test]
    fn a_check_report_run_below_the_bound_that_names_a_test_twice_counts_it_once() {
        let text = tests_record(4, 2, &["af::it x", "af::it x"], &[], "isolated");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!(tests.failed_count, 1);
        assert_eq!(tests.failed, ["af::it x"]);
    }

    /// A capped run (count 101, 100 names, two of them repeated inside it) is admitted alone, so
    /// the fold admits it too: its names are deduplicated, but its count stays whole, since its
    /// unnamed failure makes the distinct count unknowable and the fold never undercounts.
    #[test]
    fn a_capped_check_report_run_that_repeats_names_inside_it_keeps_its_whole_count() {
        let mut named: Vec<String> = (0..98).map(|n| format!("c::{n}")).collect();
        named.extend(["c::3".to_owned(), "c::50".to_owned()]);
        let text = tests_record(8, 101, &refs(&named), &[], "over_bound");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!((tests.failed_count, tests.failed.len()), (101, 98));
        assert_eq!(tests.failed, named[..98]);
    }

    /// Two capped runs that share names: the count is the sum of the runs' counts less one for
    /// each name the later run repeats from the earlier, and stays at or above the bound.
    #[test]
    fn two_capped_check_report_runs_that_share_names_count_the_sum_less_the_repeats() {
        let first: Vec<String> = (0..100).map(|n| format!("s::{n}")).collect();
        // 50 names of the first run, then 50 of its own, one of them written twice.
        let mut second: Vec<String> = (50..150).map(|n| format!("s::{n}")).collect();
        second[99] = "s::120".to_owned();
        let text = tests_record(8, 120, &refs(&first), &[], "over_bound")
            + &tests_record(8, 110, &refs(&second), &[], "over_bound");
        let (_, tests) = parse_check_report(text.as_bytes()).unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!(tests.failed_count, 120 + 110 - 50);
        assert!(tests.failed_count as usize >= MAX_NAMED_FAILED_TESTS);
        assert_eq!(
            tests.failed, first,
            "the first 100 names of the union, in order"
        );
    }

    /// The count uses checked arithmetic: a sum of the runs' counts that does not fit
    /// `failed_count` refuses the whole report, naming the line of the record that overflowed. A
    /// name a later run repeats is one test, so a run that only repeats does not overflow.
    #[test]
    fn a_check_report_whose_distinct_failed_count_overflows_is_refused_at_its_line() {
        let hundred: Vec<String> = (0..100).map(|n| format!("h::{n}")).collect();
        let full = tests_record(1, u32::MAX, &refs(&hundred), &[], "over_bound");
        let error = parse_check_report(
            (step("lint", "lint", 0) + &full + &tests_record(1, 1, &["x"], &[], "isolated"))
                .as_bytes(),
        )
        .unwrap_err();
        assert!(
            error.starts_with("line 3: ") && error.contains(&u32::MAX.to_string()),
            "{error}"
        );

        let (_, tests) =
            parse_check_report((full + &tests_record(1, 1, &["h::7"], &[], "isolated")).as_bytes())
                .unwrap();
        let tests = tests.unwrap();
        tests.validate().unwrap();
        assert_eq!((tests.failed_count, tests.failed.len()), (u32::MAX, 100));
    }

    /// A report whose check ran no tests has no test run; a malformed record refuses the whole
    /// report wherever it stands, a second one too.
    #[test]
    fn a_check_report_without_tests_has_none_and_a_bad_second_record_refuses_it() {
        let text = step("task-preflight", "preflight", 0) + &step("lint", "lint", 0);
        assert_eq!(parse_check_report(text.as_bytes()).unwrap().1, None);
        let good = tests_record(4, 0, &[], &[], "not_needed");
        for (why, second) in [
            (
                "a flaky test that passed in parallel",
                line(
                    json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "tests",
                    "test_threads": 4, "failed_count": 0, "failed": [],
                    "flaky": [{"name": "t", "parallel": "passed", "isolated": "passed"}],
                    "rerun": "isolated"}),
                ),
            ),
            (
                "failures nothing reran",
                tests_record(4, 3, &["a", "b", "c"], &[], "not_needed"),
            ),
            (
                "no test threads",
                tests_record(0, 0, &[], &[], "not_needed"),
            ),
            (
                "an unknown field",
                line(
                    json!({"schema": CHECK_REPORT_LINE_SCHEMA, "record": "tests",
                    "test_threads": 4, "failed_count": 0, "failed": [], "flaky": [],
                    "rerun": "not_needed", "passed": 9}),
                ),
            ),
            (
                "another schema",
                line(json!({"schema": "af.check-report/2", "record": "tests",
                    "test_threads": 4, "failed_count": 0, "failed": [], "flaky": [],
                    "rerun": "not_needed"})),
            ),
        ] {
            let error = parse_check_report((good.clone() + &second).as_bytes()).unwrap_err();
            assert!(error.starts_with("line 2"), "{why}: {error}");
        }
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
