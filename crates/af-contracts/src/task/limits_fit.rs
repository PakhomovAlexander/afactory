//! The smallest Task limits a compiled plan fits, as a planning refusal states them and
//! `af task plan --fit` plans with: what the Task file supplied, what the plan's protected
//! verification reserves and mandatory work require, and each reserve per verification slot.
//! Computed by the planner from the declared allowances alone: token-free, no estimate.

use serde::{Deserialize, Serialize};

use super::{VerificationReserveV1, require};

/// The four Task limits a Task file declares, `wall_ms` as the wall the Task has once it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFitLimitsV1 {
    pub tokens: u64,
    pub max_attempts: u32,
    pub wall_ms: u64,
    pub verification: VerificationReserveV1,
}

/// One verification slot's protected reserve: its Attempts and what each may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskVerificationSlotReserveV1 {
    pub node: String,
    pub attempts: u32,
    pub tokens_per_attempt: u64,
    pub wall_ms_per_attempt: u64,
}

/// One Attempt of every unconditional Worker that is not a verifier: the work a plan must keep
/// room for beside its verification reserve. `wall_ms` is the longest dependency path of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMandatoryWorkV1 {
    pub tokens: u64,
    pub attempts: u64,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLimitsFitV1 {
    pub supplied: TaskFitLimitsV1,
    /// Each limit at least what was supplied, raised only as far as the plan needs.
    pub required: TaskFitLimitsV1,
    pub reserves: Vec<TaskVerificationSlotReserveV1>,
    pub mandatory: TaskMandatoryWorkV1,
    /// Refusals no Task limit can fix: a Pipeline's own token scope or Attempt bound.
    pub unfixable: Vec<String>,
}

impl TaskLimitsFitV1 {
    /// Every limit `required` raises, as `(name, supplied, required)`, in declaration order.
    pub fn raised(&self) -> Vec<(&'static str, u64, u64)> {
        let (s, r) = (&self.supplied, &self.required);
        [
            ("tokens", s.tokens, r.tokens),
            (
                "max_attempts",
                u64::from(s.max_attempts),
                u64::from(r.max_attempts),
            ),
            ("wall_ms", s.wall_ms, r.wall_ms),
            (
                "verification.tokens",
                s.verification.tokens,
                r.verification.tokens,
            ),
            (
                "verification.attempts",
                u64::from(s.verification.attempts),
                u64::from(r.verification.attempts),
            ),
            (
                "verification.wall_ms",
                s.verification.wall_ms,
                r.verification.wall_ms,
            ),
        ]
        .into_iter()
        .filter(|(_, supplied, required)| required > supplied)
        .collect()
    }

    /// Whether planning with `required` can succeed: nothing a Pipeline fixes stands in the way.
    pub fn fixable(&self) -> bool {
        self.unfixable.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        let (s, r) = (&self.supplied, &self.required);
        require(
            r.tokens >= s.tokens
                && r.max_attempts >= s.max_attempts
                && r.wall_ms >= s.wall_ms
                && r.verification.tokens >= s.verification.tokens
                && r.verification.attempts >= s.verification.attempts
                && r.verification.wall_ms >= s.verification.wall_ms
                && r.verification.tokens <= r.tokens
                && r.verification.attempts <= r.max_attempts,
            "Fitted limits only raise what was supplied and keep the reserve inside the allowance",
        )
    }
}
