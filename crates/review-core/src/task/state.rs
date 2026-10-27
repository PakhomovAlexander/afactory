//! The one state model every Task rendering prints: `af task list`, `af task show`, `af task
//! report` and the last line of `af task run`, in text and in their `--json` documents alike. A
//! Task is in one [`TaskStatePhaseV1`]; a Task that ended has one [`TaskOutcomeV1`] and, when it
//! did not verify, one [`TaskCauseV1`] from a closed vocabulary; and every terminal state names
//! the one command to run next, or none.
//!
//! The state is derived from what the Store recorded each time it is read, never persisted: a
//! changed derivation changes the documents that print it, which are versioned, not a record.

use serde::{Deserialize, Serialize};

use super::require;

/// Where a Task is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatePhaseV1 {
    /// A plan is recorded and waits for its first `af task run`; nothing has run.
    Planned,
    /// A writer holds the Task's lease and is running it now.
    Running,
    /// The Task waits for a person: a plan review, resources, input or a decision.
    Waiting,
    /// The Task ran, stopped before it finished, and no writer holds it: `af task run` resumes.
    Abandoned,
    /// The Task recorded its result, or was collected after it did.
    Finished,
}

impl TaskStatePhaseV1 {
    pub const ALL: [Self; 5] = [
        Self::Planned,
        Self::Running,
        Self::Waiting,
        Self::Abandoned,
        Self::Finished,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Abandoned => "abandoned",
            Self::Finished => "finished",
        }
    }

    /// One line a person reads in `af help task-states`.
    pub fn description(self) -> &'static str {
        match self {
            Self::Planned => "a plan is recorded and waits for `af task run` to confirm it",
            Self::Running => "a writer holds the Task's lease and runs it now",
            Self::Waiting => "the Task waits for a plan review, resources, input or a decision",
            Self::Abandoned => "the Task stopped before it finished and no writer holds it",
            Self::Finished => "the Task recorded its result (or was collected after it did)",
        }
    }
}

/// How a Task ended. Present only for a finished or abandoned Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcomeV1 {
    /// Every acceptance obligation is satisfied: the result can be delivered.
    Verified,
    /// The review or the evaluator ran to completion and asked for changes.
    ChangesRequested,
    /// A required check of the gate failed or could not run.
    GateFailed,
    /// A Worker's Provider or process failed, or its reply was not admissible.
    ProviderFailed,
    /// The run was stopped before it finished: an interrupt, or a writer that lost its lease.
    Cancelled,
    /// A token, Attempt or reserve limit ran out before the Task could finish.
    Exhausted,
    /// The Task's wall clock ran out before it could finish.
    Expired,
}

impl TaskOutcomeV1 {
    pub const ALL: [Self; 7] = [
        Self::Verified,
        Self::ChangesRequested,
        Self::GateFailed,
        Self::ProviderFailed,
        Self::Cancelled,
        Self::Exhausted,
        Self::Expired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::ChangesRequested => "changes_requested",
            Self::GateFailed => "gate_failed",
            Self::ProviderFailed => "provider_failed",
            Self::Cancelled => "cancelled",
            Self::Exhausted => "exhausted",
            Self::Expired => "expired",
        }
    }

    /// The exit code `af task run` and `af task start --execute` end with for this outcome, as
    /// `af help exit-codes` states it: every outcome a script needs to tell apart has its own.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Verified => 0,
            Self::ChangesRequested => 3,
            Self::Exhausted | Self::Cancelled => 4,
            Self::GateFailed => 5,
            Self::ProviderFailed => 6,
            Self::Expired => 7,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Verified => {
                "every acceptance obligation is satisfied; the result can be delivered"
            }
            Self::ChangesRequested => {
                "the review or the evaluator ran to completion and asked for changes"
            }
            Self::GateFailed => "a required check of the gate failed or could not run",
            Self::ProviderFailed => {
                "a Worker's Provider or process failed, or its reply was not admissible"
            }
            Self::Cancelled => "the run stopped before it finished; `af task run` resumes it",
            Self::Exhausted => "a token, Attempt or verification-reserve limit ran out",
            Self::Expired => "the Task's wall clock ran out",
        }
    }
}

/// Why a Task that did not verify ended as it did: one closed vocabulary, each cause belonging to
/// exactly one outcome but [`TaskCauseV1::Collected`], which keeps the outcome its tombstone
/// retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCauseV1 {
    ReviewerFindings,
    EvaluatorRejected,
    CheckFailed,
    CheckFlakyThenFailed,
    PreflightFailed,
    MalformedReply,
    ProviderNetwork,
    ProviderCapacity,
    ProviderTimeout,
    /// The Provider's machine environment failed af, not the model or the login: its CLI
    /// could not start or serve its protocol, or its identity probes proved nothing within
    /// their bounded retries (ADR-0208, ADR-0253).
    ProviderEnvironment,
    IdentityChanged,
    IdentityUnverified,
    ModelRefused,
    WriterLost,
    Deadline,
    Budget,
    /// A Worker's Attempt reached its Attempt wall before it answered (ADR-0253).
    AttemptWall,
    Collected,
    HostCancelled,
}

impl TaskCauseV1 {
    pub const ALL: [Self; 19] = [
        Self::ReviewerFindings,
        Self::EvaluatorRejected,
        Self::CheckFailed,
        Self::CheckFlakyThenFailed,
        Self::PreflightFailed,
        Self::MalformedReply,
        Self::ProviderNetwork,
        Self::ProviderCapacity,
        Self::ProviderTimeout,
        Self::ProviderEnvironment,
        Self::IdentityChanged,
        Self::IdentityUnverified,
        Self::ModelRefused,
        Self::WriterLost,
        Self::Deadline,
        Self::Budget,
        Self::AttemptWall,
        Self::Collected,
        Self::HostCancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewerFindings => "reviewer_findings",
            Self::EvaluatorRejected => "evaluator_rejected",
            Self::CheckFailed => "check_failed",
            Self::CheckFlakyThenFailed => "check_flaky_then_failed",
            Self::PreflightFailed => "preflight_failed",
            Self::MalformedReply => "malformed_reply",
            Self::ProviderNetwork => "provider_network",
            Self::ProviderCapacity => "provider_capacity",
            Self::ProviderTimeout => "provider_timeout",
            Self::ProviderEnvironment => "provider_environment",
            Self::IdentityChanged => "identity_changed",
            Self::IdentityUnverified => "identity_unverified",
            Self::ModelRefused => "model_refused",
            Self::WriterLost => "writer_lost",
            Self::Deadline => "deadline",
            Self::Budget => "budget",
            Self::AttemptWall => "attempt_wall",
            Self::Collected => "collected",
            Self::HostCancelled => "host_cancelled",
        }
    }

    /// The outcome this cause belongs to; `None` for [`TaskCauseV1::Collected`], which goes with
    /// whatever outcome the collected Task had.
    pub fn outcome(self) -> Option<TaskOutcomeV1> {
        Some(match self {
            Self::ReviewerFindings | Self::EvaluatorRejected => TaskOutcomeV1::ChangesRequested,
            Self::CheckFailed | Self::CheckFlakyThenFailed | Self::PreflightFailed => {
                TaskOutcomeV1::GateFailed
            }
            Self::MalformedReply
            | Self::ProviderNetwork
            | Self::ProviderCapacity
            | Self::ProviderTimeout
            | Self::ProviderEnvironment
            | Self::IdentityChanged
            | Self::IdentityUnverified
            | Self::ModelRefused => TaskOutcomeV1::ProviderFailed,
            Self::WriterLost | Self::HostCancelled => TaskOutcomeV1::Cancelled,
            Self::Deadline => TaskOutcomeV1::Expired,
            Self::Budget | Self::AttemptWall => TaskOutcomeV1::Exhausted,
            Self::Collected => return None,
        })
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ReviewerFindings => "the review recorded findings at or above its gate",
            Self::EvaluatorRejected => "the independent evaluator did not accept the result",
            Self::CheckFailed => {
                "a required check ran and failed, or the kernel stopped it at its wall"
            }
            Self::CheckFlakyThenFailed => {
                "a required check failed; some of its tests were flaky before"
            }
            Self::PreflightFailed => {
                "a required check stopped at a step before its tests, or never started"
            }
            Self::MalformedReply => {
                "a Worker's process failed or its reply broke its output contract"
            }
            Self::ProviderNetwork => "a request to the Provider failed on the network",
            Self::ProviderCapacity => {
                "the Provider's model was at capacity or its rate limit was reached"
            }
            Self::ProviderTimeout => "a Worker or Provider call ran out of its Attempt time",
            Self::ProviderEnvironment => {
                "the Provider's environment on this machine failed, not the model or the login"
            }
            Self::IdentityChanged => {
                "the Provider answered as another account, model or policy than the plan bound"
            }
            Self::IdentityUnverified => "the Provider's login could not be verified",
            Self::ModelRefused => {
                "the bound model refused, was switched or is unknown to its Provider"
            }
            Self::WriterLost => "the writer lost its lease before the Task finished",
            Self::Deadline => "the Task's wall clock ran out",
            Self::Budget => "a token, Attempt or verification-reserve limit ran out",
            Self::AttemptWall => {
                "a Worker reached its Attempt wall; a source-writing Worker's edits are kept"
            }
            Self::Collected => "the Task was collected; only its summary remains",
            Self::HostCancelled => "the run was interrupted (Ctrl-C or a signal)",
        }
    }

    /// The cause af's own words in a recorded diagnostic name, if any: a pure reading of the
    /// text, used only where no typed record says more. The order matters: a Provider timeout
    /// names its own deadline, and the Task's deadline is only the Task's; an account that
    /// answered as another is a change, and a probe that proved nothing on a failing
    /// environment (`(environment)`) is the environment's failure, never the login's.
    pub fn of_diagnostic(text: &str) -> Option<Self> {
        let lower = text.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
        if has(&["reached its attempt wall"]) {
            Some(Self::AttemptWall)
        } else if has(&[
            "model_refused",
            "model_switched",
            "model_unavailable",
            "model_not_accepted",
        ]) {
            Some(Self::ModelRefused)
        } else if has(&[
            "identity changed",
            "changed its exact account",
            "answered as another",
            "identity is no longer current",
            "differs from the one captured",
        ]) {
            Some(Self::IdentityChanged)
        } else if has(&["(environment)", "provider environment failure"]) {
            Some(Self::ProviderEnvironment)
        } else if has(&[
            "authentication",
            "not logged in",
            "login",
            "unauthorized",
            "identity",
        ]) {
            Some(Self::IdentityUnverified)
        } else if has(&["capacity", "overloaded", "rate limit", "rate_limit", "429"]) {
            Some(Self::ProviderCapacity)
        } else if has(&["network", "connection", "dns", "econnreset", "unreachable"]) {
            Some(Self::ProviderNetwork)
        } else if has(&["lease"]) {
            Some(Self::WriterLost)
        } else if has(&["interrupted", "sigint", "cancelled"]) {
            Some(Self::HostCancelled)
        } else if has(&[
            "task deadline",
            "deadline expired",
            "deadline protects",
            "remaining task deadline",
        ]) {
            Some(Self::Deadline)
        } else if has(&["timed out", "timeout", "deadline"]) {
            Some(Self::ProviderTimeout)
        } else if has(&[
            "token limit",
            "attempt limit",
            "exhausted",
            "budget",
            "reserve",
            "allowance",
        ]) {
            Some(Self::Budget)
        } else {
            None
        }
    }
}

/// The state a Task is in, as every rendering prints it. `outcome` is present exactly when the
/// Task ended (finished or abandoned), `cause` exactly when that outcome is not `verified` or the
/// Task was collected, and `next_action` is the one command to run next, absent when there is
/// none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStateV1 {
    pub phase: TaskStatePhaseV1,
    // Every field is present in every document, `null` when absent: a reader never has to tell a
    // missing field from an absent value.
    #[serde(deserialize_with = "Option::deserialize")]
    pub outcome: Option<TaskOutcomeV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cause: Option<TaskCauseV1>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub next_action: Option<String>,
}

impl TaskStateV1 {
    pub fn new(phase: TaskStatePhaseV1) -> Self {
        Self {
            phase,
            outcome: None,
            cause: None,
            next_action: None,
        }
    }

    pub fn ended(
        phase: TaskStatePhaseV1,
        outcome: TaskOutcomeV1,
        cause: Option<TaskCauseV1>,
    ) -> Self {
        Self {
            phase,
            outcome: Some(outcome),
            cause,
            next_action: None,
        }
    }

    pub fn with_next(mut self, command: impl Into<String>) -> Self {
        self.next_action = Some(command.into());
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        let ended = matches!(
            self.phase,
            TaskStatePhaseV1::Finished | TaskStatePhaseV1::Abandoned
        );
        require(
            ended == self.outcome.is_some(),
            "Task state has an outcome exactly when the Task ended",
        )?;
        require(
            self.phase != TaskStatePhaseV1::Abandoned
                || self.outcome == Some(TaskOutcomeV1::Cancelled),
            "An abandoned Task is cancelled",
        )?;
        match (self.outcome, self.cause) {
            (None, None) => (),
            (Some(TaskOutcomeV1::Verified), None) => (),
            (Some(_), Some(TaskCauseV1::Collected)) => require(
                self.phase == TaskStatePhaseV1::Finished && self.next_action.is_none(),
                "Only a finished Task is collected, and nothing is left to run on it",
            )?,
            (Some(outcome), Some(cause)) => require(
                cause.outcome() == Some(outcome),
                "Task cause belongs to another outcome",
            )?,
            _ => {
                return Err("A Task that did not verify names its cause".into());
            }
        }
        require(
            self.next_action.as_deref().is_none_or(|command| {
                command.starts_with("af ")
                    && command.len() <= 2048
                    && !command.chars().any(char::is_control)
            }) && (self.outcome.is_none_or(|o| o == TaskOutcomeV1::Verified)
                || self.cause == Some(TaskCauseV1::Collected)
                || self.next_action.is_some()),
            "Task state names one af command to run next, and every failure names one",
        )
    }

    /// `finished/gate_failed/check_failed`, `finished/verified`, `planned`: the triple as text.
    pub fn label(&self) -> String {
        let mut label = self.phase.as_str().to_owned();
        for part in [
            self.outcome.map(TaskOutcomeV1::as_str),
            self.cause.map(TaskCauseV1::as_str),
        ]
        .into_iter()
        .flatten()
        {
            label.push('/');
            label.push_str(part);
        }
        label
    }

    /// The command's verb as `af task list` prints it: `deliver` for `af task deliver …`,
    /// `setup` for `af provider setup …`, `-` for none.
    pub fn next_verb(&self) -> &str {
        self.next_action
            .as_deref()
            .and_then(|command| command.split_whitespace().nth(2))
            .unwrap_or("-")
    }

    /// The exit code a run that ends in this state returns: its outcome's, or 0 for a Task that
    /// has not ended (a plan preview, a running or waiting Task).
    pub fn exit_code(&self) -> u8 {
        self.outcome.map_or(0, TaskOutcomeV1::exit_code)
    }
}

/// What the gate's last check receipt said, in `af task list --json` and `af task show --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGateV1 {
    pub status: TaskGateStatusV1,
    /// The failing tests the checks' runners named (`FAIL`, `TIMEOUT`, `SIGABRT`), in the order
    /// they first appeared, each once.
    pub failed_tests: Vec<String>,
    /// The tests the runners named flaky: failed at first, passed on a retry.
    pub flaky: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskGateStatusV1 {
    Passed,
    Failed,
    NotRun,
}

impl TaskGateStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NotRun => "not_run",
        }
    }
}

/// The most failing-test names a document or a line carries; the rest are counted.
pub const MAX_FAILED_TESTS: usize = 200;

/// The test names a runner's output names as failing and as flaky. Recognized lines are
/// nextest's status lines (`FAIL [ 0.004s] crate path::to::test`, `TIMEOUT`, `SIGABRT`,
/// `SIGSEGV`, and `FLAKY 2/3 [...]`), libtest's `test path::to::test ... FAILED`, and a
/// `flaky: a, b` line. A test is named by its last word, its path; a name is listed once, in
/// first-appearance order, and a test that is flaky and later failed for good is failing.
pub fn failing_tests(output: &str) -> (Vec<String>, Vec<String>) {
    let mut failed: Vec<String> = Vec::new();
    let mut flaky: Vec<String> = Vec::new();
    let push = |list: &mut Vec<String>, name: &str| {
        let name = name.trim_matches(|c: char| c == ',' || c == ';');
        if !name.is_empty()
            && name.len() <= 512
            && !name.chars().any(char::is_control)
            && !list.iter().any(|known| known == name)
            && list.len() < MAX_FAILED_TESTS
        {
            list.push(name.to_owned());
        }
    };
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("flaky:") {
            for name in rest.split([',', ' ']).filter(|n| !n.is_empty()) {
                push(&mut flaky, name);
            }
            continue;
        }
        let mut words = trimmed.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        let nextest = matches!(
            first,
            "FAIL" | "TIMEOUT" | "SIGABRT" | "SIGSEGV" | "SIGKILL"
        );
        let is_flaky = first == "FLAKY";
        if (nextest || is_flaky) && trimmed.contains(']') {
            // The name is what follows the bracketed duration, its last word the test path.
            let after = &trimmed[trimmed.find(']').map_or(0, |i| i + 1)..];
            if let Some(name) = after.split_whitespace().last() {
                if is_flaky {
                    push(&mut flaky, name);
                } else {
                    push(&mut failed, name);
                }
            }
            continue;
        }
        if first == "test"
            && let Some(rest) = trimmed.strip_prefix("test ")
            && let Some(name) = rest.strip_suffix(" ... FAILED")
        {
            push(&mut failed, name.trim());
        }
    }
    (failed, flaky)
}

/// The step a gate's own step log says stopped it: the label of the first `[label] 1.234s,
/// exit N` line with a non-zero N (`scripts/ci-step.py` prints one per step).
pub fn stopped_step(output: &str) -> Option<(String, i64)> {
    output.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix('[')?;
        let (label, rest) = rest.split_once("] ")?;
        let code = rest.split_once(", exit ")?.1.trim().parse::<i64>().ok()?;
        (code != 0
            && !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .then(|| (label.to_owned(), code))
    })
}

/// The first line of a diagnostic a person should read: its first non-blank line, without
/// controls, at most 240 characters.
pub fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let clean: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.chars().count() > 240 {
        let cut: String = clean.chars().take(239).collect();
        format!("{cut}…")
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cause_belongs_to_one_outcome_and_every_outcome_has_its_own_exit_code() {
        for cause in TaskCauseV1::ALL {
            let value = serde_json::to_value(cause).unwrap();
            assert_eq!(value, cause.as_str());
            match cause.outcome() {
                Some(outcome) => assert_ne!(outcome, TaskOutcomeV1::Verified, "{cause:?}"),
                None => assert_eq!(cause, TaskCauseV1::Collected),
            }
        }
        for outcome in TaskOutcomeV1::ALL {
            assert_eq!(serde_json::to_value(outcome).unwrap(), outcome.as_str());
        }
        // A failed gate and reviewer findings never share an exit code, nor a Provider failure
        // and an expired deadline: a script branches on each.
        let codes = [
            TaskOutcomeV1::Verified,
            TaskOutcomeV1::ChangesRequested,
            TaskOutcomeV1::Exhausted,
            TaskOutcomeV1::GateFailed,
            TaskOutcomeV1::ProviderFailed,
            TaskOutcomeV1::Expired,
        ]
        .map(TaskOutcomeV1::exit_code);
        let distinct: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(distinct.len(), codes.len());
        for phase in TaskStatePhaseV1::ALL {
            assert_eq!(serde_json::to_value(phase).unwrap(), phase.as_str());
        }
    }

    #[test]
    fn a_state_names_its_cause_and_next_command_exactly_when_it_failed() {
        let gate = TaskStateV1::ended(
            TaskStatePhaseV1::Finished,
            TaskOutcomeV1::GateFailed,
            Some(TaskCauseV1::CheckFailed),
        )
        .with_next("af task run t --retry-gate");
        gate.validate().unwrap();
        assert_eq!(gate.label(), "finished/gate_failed/check_failed");
        assert_eq!(gate.next_verb(), "run");
        assert_eq!(gate.exit_code(), 5);
        let without_next = TaskStateV1 {
            next_action: None,
            ..gate.clone()
        };
        assert!(without_next.validate().is_err());
        let wrong = TaskStateV1 {
            cause: Some(TaskCauseV1::ReviewerFindings),
            ..gate.clone()
        };
        assert!(wrong.validate().is_err());
        let verified =
            TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None);
        verified.validate().unwrap();
        assert_eq!(verified.label(), "finished/verified");
        assert_eq!(verified.next_verb(), "-");
        let planned = TaskStateV1::new(TaskStatePhaseV1::Planned).with_next("af task run t");
        planned.validate().unwrap();
        assert_eq!(planned.label(), "planned");
        assert!(
            TaskStateV1::ended(TaskStatePhaseV1::Planned, TaskOutcomeV1::Verified, None)
                .validate()
                .is_err()
        );
        assert!(
            TaskStateV1::ended(
                TaskStatePhaseV1::Abandoned,
                TaskOutcomeV1::Expired,
                Some(TaskCauseV1::Deadline)
            )
            .with_next("af task run t")
            .validate()
            .is_err()
        );
        let collected = TaskStateV1::ended(
            TaskStatePhaseV1::Finished,
            TaskOutcomeV1::ChangesRequested,
            Some(TaskCauseV1::Collected),
        );
        collected.validate().unwrap();
        assert!(
            TaskStateV1::new(TaskStatePhaseV1::Waiting)
                .with_next("rm -rf /")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn runner_output_names_failing_and_flaky_tests_once_each() {
        let output = "\
        PASS [   0.010s] af::it task_planning::plans
        FAIL [   0.004s] af::it task_planning::refuses
     TIMEOUT [  60.000s] (3/9) af::it task_heavy::slow
     SIGABRT [   0.100s] review-core schema_parity::aborts
       FLAKY 2/3 [   1.000s] af::it tui::the_user_scope_flakes
test lib::unit ... FAILED
test lib::ok ... ok
        FAIL [   0.004s] af::it task_planning::refuses
flaky: a::b, c::d
";
        let (failed, flaky) = failing_tests(output);
        assert_eq!(
            failed,
            [
                "task_planning::refuses",
                "task_heavy::slow",
                "schema_parity::aborts",
                "lib::unit"
            ]
        );
        assert_eq!(flaky, ["tui::the_user_scope_flakes", "a::b", "c::d"]);
        assert_eq!(failing_tests("all good\n"), (vec![], vec![]));
    }

    #[test]
    fn a_step_log_names_the_step_that_stopped_the_gate() {
        let log = "[task-preflight] 1.000s, exit 0\n[fmt] 2.500s, exit 1\n[lint] 1.0s, exit 101\n";
        assert_eq!(stopped_step(log), Some(("fmt".into(), 1)));
        assert_eq!(stopped_step("[fmt] 2.5s, exit 0\n"), None);
        assert_eq!(stopped_step("[a b] 1s, exit 2\n"), None);
    }

    #[test]
    fn diagnostics_name_their_cause_in_af_s_own_words() {
        for (text, cause) in [
            (
                "Claude Worker failed: model_refused by the classifier",
                TaskCauseV1::ModelRefused,
            ),
            (
                "Provider admission changed its exact account, model or invocation policy",
                TaskCauseV1::IdentityChanged,
            ),
            (
                "Codex authentication failed",
                TaskCauseV1::IdentityUnverified,
            ),
            (
                "Captured Task Provider identity is no longer current or could not be verified",
                TaskCauseV1::IdentityChanged,
            ),
            (
                "Selected model is at capacity",
                TaskCauseV1::ProviderCapacity,
            ),
            ("HTTP 429 rate limit", TaskCauseV1::ProviderCapacity),
            ("network request failed", TaskCauseV1::ProviderNetwork),
            ("writer lease expired", TaskCauseV1::WriterLost),
            (
                "Task deadline protects still-required verification",
                TaskCauseV1::Deadline,
            ),
            ("Worker timed out after 300s", TaskCauseV1::ProviderTimeout),
            (
                "Task token limit protects still-required verification",
                TaskCauseV1::Budget,
            ),
            (
                "Task node root.nodes.x exhausted its Attempt limit",
                TaskCauseV1::Budget,
            ),
            // ADR-0253: the Attempt wall is its own cause, before the timeout words it carries.
            (
                "implementer reached its Attempt wall of 30 min after 30 min; its edits are kept \
                 as partial candidate sha256:…; implementer did not answer within 1800000ms",
                TaskCauseV1::AttemptWall,
            ),
            // An exhausted identity re-probe is the environment's failure, not the login's.
            (
                "identity recheck: probe timed out 3× (up to 30 s each); the account was not \
                 verified as changed: the Provider's environment failed, not the model or the \
                 login (environment); run the Task again with `af task run`",
                TaskCauseV1::ProviderEnvironment,
            ),
            (
                "Provider environment failure, not a model or credential failure: claude \
                 cannot start",
                TaskCauseV1::ProviderEnvironment,
            ),
            // Another account is still the change it is.
            (
                "identity recheck: the Claude account differs from the one captured at plan: \
                 run `af task plan` again",
                TaskCauseV1::IdentityChanged,
            ),
        ] {
            assert_eq!(TaskCauseV1::of_diagnostic(text), Some(cause), "{text}");
        }
        assert_eq!(TaskCauseV1::of_diagnostic("assertion failed"), None);
    }

    #[test]
    fn the_first_line_is_the_first_nonblank_line_without_controls() {
        assert_eq!(first_line("\n  \nerror: boom\nmore"), "error: boom");
        assert_eq!(first_line("a\u{1b}[31mb"), "a [31mb");
        assert_eq!(first_line(&"x".repeat(300)).chars().count(), 240);
        assert_eq!(first_line(""), "");
    }
}
