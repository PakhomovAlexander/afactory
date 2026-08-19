//! The gate decision over a set of check results.
//!
//! Three rules, each of which the shell harness also enforced and each of which is easy to lose
//! in a rewrite:
//!
//! 1. A required check that failed blocks.
//! 2. A required check that *could not run* blocks, identically. "Nothing failed" is not "the
//!    checks passed".
//! 3. A gate with no required checks blocks. A vacuous run is the most dangerous green there
//!    is — it looks exactly like a clean one and asserts nothing at all.

use serde::{Deserialize, Serialize};

use crate::runner::{CheckResult, CheckStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOutcome {
    Passed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDecision {
    pub outcome: GateOutcome,
    /// Names of the required checks that did not pass, in the order they ran.
    pub blocking: Vec<String>,
    /// Why the gate blocked, in a form a human can act on.
    pub reasons: Vec<String>,
    pub executed: usize,
    pub required: usize,
}

impl GateDecision {
    pub fn evaluate(results: &[CheckResult]) -> GateDecision {
        let required: Vec<&CheckResult> = results.iter().filter(|r| r.required).collect();
        let mut blocking = Vec::new();
        let mut reasons = Vec::new();

        for result in &required {
            match result.status {
                CheckStatus::Passed => {}
                CheckStatus::Failed => {
                    blocking.push(result.name.clone());
                    reasons.push(format!(
                        "{} failed (exit {})",
                        result.name,
                        result.exit_code.unwrap_or(-1)
                    ));
                }
                CheckStatus::NotRun => {
                    blocking.push(result.name.clone());
                    reasons.push(format!(
                        "not verified: {} ({})",
                        result.name,
                        result.reason.as_deref().unwrap_or("no reason recorded")
                    ));
                }
            }
        }

        if required.is_empty() {
            reasons.push("no required checks executed — a vacuous run is not a pass".to_string());
        }

        let outcome = if blocking.is_empty() && !required.is_empty() {
            GateOutcome::Passed
        } else {
            GateOutcome::Blocked
        };

        GateDecision {
            outcome,
            blocking,
            reasons,
            executed: results.len(),
            required: required.len(),
        }
    }

    pub fn passed(&self) -> bool {
        self.outcome == GateOutcome::Passed
    }

    /// The exit code `checks.sh` would have used, so a caller can be compared against it
    /// directly: 0 when every executed check passed and at least one ran, 1 otherwise.
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.passed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CheckResult;
    use review_core::exec::{Arg, Command};

    fn result(name: &str, status: CheckStatus, required: bool) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            status,
            exit_code: match status {
                CheckStatus::Passed => Some(0),
                CheckStatus::Failed => Some(1),
                CheckStatus::NotRun => None,
            },
            reason: (status == CheckStatus::NotRun).then(|| "host unreachable".to_string()),
            program: Some("true".to_string()),
            args: vec![Arg::literal("--x")],
            stdout: None,
            stderr: None,
            required,
        }
    }

    #[test]
    fn all_required_passing_is_a_pass() {
        let decision = GateDecision::evaluate(&[
            result("style", CheckStatus::Passed, true),
            result("build", CheckStatus::Passed, true),
        ]);
        assert!(decision.passed());
        assert_eq!(decision.exit_code(), 0);
    }

    #[test]
    fn a_check_that_could_not_run_blocks_exactly_like_a_failure() {
        let blocked = GateDecision::evaluate(&[
            result("style", CheckStatus::Passed, true),
            result("build", CheckStatus::NotRun, true),
        ]);
        assert!(!blocked.passed());
        assert_eq!(blocked.blocking, vec!["build"]);
        assert!(
            blocked.reasons[0].starts_with("not verified: build"),
            "{:?}",
            blocked.reasons
        );
    }

    #[test]
    fn a_vacuous_gate_blocks() {
        let empty = GateDecision::evaluate(&[]);
        assert!(!empty.passed());
        assert_eq!(empty.exit_code(), 1);
        assert!(empty.reasons[0].contains("vacuous"));

        // ...and so does a gate whose checks are all optional: nothing required ran.
        let optional_only = GateDecision::evaluate(&[result("lint", CheckStatus::Passed, false)]);
        assert!(!optional_only.passed());
        assert_eq!(optional_only.required, 0);
    }

    #[test]
    fn an_optional_failure_is_recorded_but_does_not_block() {
        let decision = GateDecision::evaluate(&[
            result("build", CheckStatus::Passed, true),
            result("spellcheck", CheckStatus::Failed, false),
        ]);
        assert!(decision.passed());
        assert_eq!(decision.executed, 2);
        assert_eq!(decision.required, 1);
        assert!(decision.blocking.is_empty());
    }

    #[test]
    fn a_command_refused_before_execution_is_not_run_not_failed() {
        // An argument-injection refusal must not read as "the check ran and failed" — nothing
        // was verified, and the gate's reason has to say so.
        let refused = CheckResult {
            name: "tests".to_string(),
            status: CheckStatus::NotRun,
            exit_code: None,
            reason: Some("refused before execution: untrusted value \"--config=/tmp/evil\" would be read as an option; a check must pass such values in a value position".to_string()),
            program: Some("./tests/hub-test".to_string()),
            args: vec![Arg::untrusted("--config=/tmp/evil")],
            stdout: None,
            stderr: None,
            required: true,
        };
        let decision = GateDecision::evaluate(&[refused]);
        assert!(!decision.passed());
        assert!(decision.reasons[0].contains("not verified: tests"));
        assert!(decision.reasons[0].contains("refused before execution"));
    }

    #[test]
    fn the_command_type_is_carried_into_the_record() {
        // The record keeps provenance, so a reader can see which values came from the change.
        let command = Command::new("cargo", vec![Arg::literal("test"), Arg::untrusted("t.rs")]);
        assert_eq!(command.resolve().unwrap(), vec!["test", "t.rs"]);
    }
}
