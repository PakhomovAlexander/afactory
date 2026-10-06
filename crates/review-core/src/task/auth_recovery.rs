//! Runtime Provider auth recovery (ADR-0141). These records are closed, non-secret data: they
//! carry no native text, URL, code, token, account identity or credential. A login result can
//! only be recorded as `authenticated` (Stage 1's `authenticated_unverified`); it grants no
//! paid verification, Task execution, plan, publication or account-switch authority.
//!
//! The Store is the authority for every transition. Possessing one of these values, or a
//! serialized claim, grants nothing: verification probes reserve on the original Task ledger,
//! claims are fenced on the auth context's recovery generation, and dispatch rechecks both.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
use super::usage::DecimalU128;
use super::{TaskLimitsV1, is_name, require, safe_number};
use crate::is_digest;

pub const TASK_AUTH_SUSPENSION_V1: &str = "af/TaskAuthSuspension@1";
pub const TASK_AUTH_RESUME_CLAIM_V1: &str = "af/TaskAuthResumeClaim@1";
pub const TASK_CONTINUATION_V1: &str = "af/TaskContinuation@1";
pub const PROVIDER_AUTH_RECOVERY_EVENT_V1: &str = "af/ProviderAuthRecoveryEvent@1";
pub const TASK_AUTH_RECOVERY_V1: &str = "af/task-auth-recovery@1";

/// The dormant budget node a captured recovery allowance installs. It is outside every
/// Pipeline call scope: probes consume the Task's own tokens, Attempts and deadline, never a
/// Pipeline's per-call Attempt bound, and never the protected verification reserve.
pub const AUTH_RECOVERY_NODE: &str = "recovery.providers.verify";

/// At most this many distinct auth contexts may block one Task.
pub const MAX_AUTH_CONTEXTS: usize = 16;

/// Closed native authentication failure classes. Quota, model, network and unknown failures
/// are deliberately absent: they never suspend a Task for login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuthFailureV1 {
    AuthMissing,
    AuthRevoked,
    AuthExpired,
    AuthRefreshContended,
    AuthRefreshFailed,
    AuthRejected,
}

impl TaskAuthFailureV1 {
    /// A contended refresh may be another process finishing the same refresh. It is retried
    /// within the Attempt's existing allowance and, if it persists, verified without a login.
    pub fn transient(self) -> bool {
        self == Self::AuthRefreshContended
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthMissing => "auth_missing",
            Self::AuthRevoked => "auth_revoked",
            Self::AuthExpired => "auth_expired",
            Self::AuthRefreshContended => "auth_refresh_contended",
            Self::AuthRefreshFailed => "auth_refresh_failed",
            Self::AuthRejected => "auth_rejected",
        }
    }
}

/// An opaque, host-issued correlation reference. It is a name, never a URL, an address or a
/// message body; af cannot authenticate the human or chat behind it.
pub fn is_opaque_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-'))
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
}

fn is_attempt_id(value: &str) -> bool {
    value.len() == 26 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn bounded_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= 1024
        && !value.chars().any(char::is_control)
}

/// The exact machine-local authentication context a failure belongs to: the Provider label,
/// its implementation family and the admitted principal. Recovery is deduplicated by it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAuthContextV1 {
    pub provider: String,
    pub provider_kind: String,
    pub principal_id: String,
}

impl ProviderAuthContextV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.provider)
                && is_name(&self.provider_kind)
                && bounded_text(&self.principal_id),
            "Provider auth context needs a Provider label, kind and admitted principal",
        )
    }
}

/// Everything a paid verification probe and a resumed dispatch must find unchanged: the auth
/// context, the exact model and effort, and the captured invocation policy that pins the
/// executable, isolation and environment the Workers run with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthBindingV1 {
    pub context: ProviderAuthContextV1,
    pub model: String,
    pub effort: String,
    pub invocation_policy_id: String,
}

impl TaskAuthBindingV1 {
    /// The recovery identity of a captured Model binding; a Command binding has none.
    pub fn of_plan_binding(binding: &EffectiveWorkerBindingV1) -> Option<Self> {
        match &binding.execution {
            WorkerExecutionV1::Model {
                provider,
                provider_kind,
                principal_id,
                model,
                effort,
            } => Some(Self {
                context: ProviderAuthContextV1 {
                    provider: provider.clone(),
                    provider_kind: provider_kind.clone(),
                    principal_id: principal_id.clone(),
                },
                model: model.clone(),
                effort: effort.clone(),
                invocation_policy_id: binding.invocation_policy_id.clone(),
            }),
            WorkerExecutionV1::Command {} => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.context.validate()?;
        require(
            bounded_text(&self.model)
                && is_name(&self.effort)
                && is_digest(&self.invocation_policy_id),
            "Provider auth binding needs exact model, effort and invocation policy",
        )
    }
}

/// Who asked for the work and which coordinator conversation receives its outcome. Both are
/// opaque host references persisted per Task participation, never derived from a login.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthParticipantV1 {
    pub requester_ref: String,
    pub coordinator_ref: String,
}

impl TaskAuthParticipantV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_opaque_ref(&self.requester_ref) && is_opaque_ref(&self.coordinator_ref),
            "Recovery participants are opaque bounded host references",
        )
    }
}

/// The accounting identity of one Attempt that failed authentication. It is retained exactly;
/// recovery never refunds, reopens or replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthFailedAttemptV1 {
    pub attempt_id: String,
    pub node: String,
    pub reservation_id: String,
    pub charged_tokens: DecimalU128,
    pub failure: TaskAuthFailureV1,
}

/// One auth context a suspended Task requires, with the generation it was suspended under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthRequirementV1 {
    pub binding: TaskAuthBindingV1,
    pub generation: u64,
}

/// `af/TaskAuthSuspension@1`: why one Task stopped before terminalization. Every unfinished
/// Attempt was settled or released first; its failed spend stays on the Task ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthSuspensionV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    /// The Task revision's original deadline. Recovery never manufactures another one.
    pub deadline_unix_ms: u64,
    /// Empty only when the Task was stopped by a newer generation another Task recorded.
    pub failures: Vec<TaskAuthFailedAttemptV1>,
    /// Keyed by the Store-derived context key.
    pub contexts: BTreeMap<String, TaskAuthRequirementV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub participant: Option<TaskAuthParticipantV1>,
}

impl TaskAuthSuspensionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id)
                && is_digest(&self.task_revision_id)
                && is_digest(&self.plan_id)
                && self.deadline_unix_ms > 0
                && safe_number(self.deadline_unix_ms),
            "Auth suspension needs its exact Task, revision, plan and original deadline",
        )?;
        require(
            !self.contexts.is_empty() && self.contexts.len() <= MAX_AUTH_CONTEXTS,
            "Auth suspension needs one to sixteen required auth contexts",
        )?;
        for (key, requirement) in &self.contexts {
            require(
                is_digest(key) && requirement.generation > 0 && safe_number(requirement.generation),
                "Auth suspension needs exact context keys and generations",
            )?;
            requirement.binding.validate()?;
        }
        require(
            self.failures.len() <= 64
                && self
                    .failures
                    .iter()
                    .map(|failure| &failure.attempt_id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == self.failures.len()
                && self.failures.iter().all(|failure| {
                    is_attempt_id(&failure.attempt_id)
                        && failure.node.split('.').all(is_name)
                        && failure.reservation_id.starts_with("reservation:")
                }),
            "Auth suspension retains distinct exact failed Attempts",
        )?;
        if let Some(participant) = &self.participant {
            participant.validate()?;
        }
        Ok(())
    }
}

/// One verified context a resume claim depends on: the generation and the recovery-log
/// sequence of its `verified` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthClaimedContextV1 {
    pub generation: u64,
    pub verified_sequence: u64,
}

/// `af/TaskAuthResumeClaim@1`: the durable claim to continue one suspended Task. It is bound
/// to the exact suspension, revision and plan, and to every still-required context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthResumeClaimV1 {
    pub task_id: String,
    pub suspension_id: String,
    pub task_revision_id: String,
    pub plan_id: String,
    pub contexts: BTreeMap<String, TaskAuthClaimedContextV1>,
}

impl TaskAuthResumeClaimV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id)
                && is_digest(&self.suspension_id)
                && is_digest(&self.task_revision_id)
                && is_digest(&self.plan_id)
                && !self.contexts.is_empty()
                && self.contexts.len() <= MAX_AUTH_CONTEXTS
                && self.contexts.iter().all(|(key, context)| {
                    is_digest(key)
                        && context.generation > 0
                        && safe_number(context.generation)
                        && safe_number(context.verified_sequence)
                }),
            "Resume claim needs its exact suspension, plan and verified generations",
        )
    }
}

/// What one paid verification probe concluded. Only an acknowledgement within its own
/// reservation can verify a generation; the Store derives an overrun from the exact charge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskAuthProbeOutcomeV1 {
    Acknowledged {},
    AuthFailed {
        failure: TaskAuthFailureV1,
    },
    /// Quota, model, network, protocol or usage-reporting failure: not a login problem.
    Failed {},
    /// A lost writer's started probe, charged its full reservation.
    Abandoned {},
}

/// The closed reasons recovery stays blocked pending a human decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuthBlockV1 {
    DeadlineExpired,
    AuthorityChanged,
    BindingChanged,
    AllowanceMissing,
    AllowanceExhausted,
    BudgetBreached,
}

/// The outcome a coordinator is told about. It never carries a challenge or credential.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskAuthOutcomeV1 {
    /// The Task was claimed under a verified generation and its work continues.
    Resumed {},
    /// Verification found the context still unauthenticated; a private login is required.
    LoginRequired {},
    /// Verification failed for a reason that is not authentication.
    VerificationFailed {},
    Blocked {
        reason: TaskAuthBlockV1,
    },
}

/// One row of an auth context's append-only recovery log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderAuthRecoveryChangeV1 {
    /// A Task joined this generation. A failure opens a new generation only when the previous
    /// one was verified; concurrent failures share one generation and one login.
    Failed {
        task_id: String,
        suspension_id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        failure: Option<TaskAuthFailureV1>,
    },
    /// Stage 1 reported `authenticated_unverified` for this context. It verifies nothing.
    Authenticated {
        login_ref: String,
    },
    ProbeFailed {
        task_id: String,
        attempt_id: String,
        outcome: TaskAuthProbeOutcomeV1,
        overrun: bool,
    },
    Verified {
        task_id: String,
        attempt_id: String,
    },
    Outcome {
        task_id: String,
        suspension_id: String,
        outcome: TaskAuthOutcomeV1,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        participant: Option<TaskAuthParticipantV1>,
    },
    Notified {
        outcome_sequence: u64,
        coordinator_ref: String,
        delivery_ref: String,
    },
}

/// `af/ProviderAuthRecoveryEvent@1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAuthRecoveryEventV1 {
    pub schema: String,
    pub context_key: String,
    pub context: ProviderAuthContextV1,
    pub generation: u64,
    pub now_unix_ms: u64,
    pub change: ProviderAuthRecoveryChangeV1,
}

impl ProviderAuthRecoveryEventV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROVIDER_AUTH_RECOVERY_EVENT_V1
                && is_digest(&self.context_key)
                && self.generation > 0
                && safe_number(self.generation)
                && self.now_unix_ms > 0
                && safe_number(self.now_unix_ms),
            "Recovery event needs its schema, context key, generation and policy time",
        )?;
        self.context.validate()?;
        match &self.change {
            ProviderAuthRecoveryChangeV1::Failed {
                task_id,
                suspension_id,
                ..
            } => require(
                is_name(task_id) && is_digest(suspension_id),
                "Recovery failure names its Task and suspension",
            ),
            ProviderAuthRecoveryChangeV1::Authenticated { login_ref } => require(
                is_opaque_ref(login_ref),
                "Recovery login reference is opaque",
            ),
            ProviderAuthRecoveryChangeV1::ProbeFailed {
                task_id,
                attempt_id,
                outcome,
                overrun,
            } => require(
                is_name(task_id)
                    && is_attempt_id(attempt_id)
                    && (*overrun || *outcome != TaskAuthProbeOutcomeV1::Acknowledged {}),
                "Recovery probe failure names its Task and Attempt",
            ),
            ProviderAuthRecoveryChangeV1::Verified {
                task_id,
                attempt_id,
            } => require(
                is_name(task_id) && is_attempt_id(attempt_id),
                "Recovery verification names its Task and Attempt",
            ),
            ProviderAuthRecoveryChangeV1::Outcome {
                task_id,
                suspension_id,
                participant,
                ..
            } => {
                require(
                    is_name(task_id) && is_digest(suspension_id),
                    "Recovery outcome names its Task and suspension",
                )?;
                participant.as_ref().map_or(Ok(()), |p| p.validate())
            }
            ProviderAuthRecoveryChangeV1::Notified {
                outcome_sequence,
                coordinator_ref,
                delivery_ref,
            } => require(
                safe_number(*outcome_sequence)
                    && is_opaque_ref(coordinator_ref)
                    && is_opaque_ref(delivery_ref),
                "Recovery notification names its outcome and opaque delivery",
            ),
        }
    }
}

/// Why an already-terminal Task is continued by a linked successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskContinuationReasonV1 {
    ProviderAuth,
}

/// `af/TaskContinuation@1`: the explicit link from a successor Task's revision to its finished
/// predecessor. It retains the predecessor's exact charge and original bounds; the successor's
/// own limits must fit in what remains, and its plan needs its own approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContinuationV1 {
    pub predecessor_task_id: String,
    pub predecessor_revision_id: String,
    pub predecessor_plan_id: String,
    pub predecessor_result_id: String,
    pub predecessor_chargeable_tokens: DecimalU128,
    pub predecessor_begun_attempts: u64,
    pub original_limits: TaskLimitsV1,
    pub reason: TaskContinuationReasonV1,
}

impl TaskContinuationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.predecessor_task_id)
                && is_digest(&self.predecessor_revision_id)
                && is_digest(&self.predecessor_plan_id)
                && is_digest(&self.predecessor_result_id)
                && safe_number(self.predecessor_begun_attempts),
            "Continuation needs its exact finished predecessor",
        )?;
        self.original_limits.validate()
    }

    /// The largest limits a successor may declare: the original deadline and verification
    /// reserve, minus every token and Attempt the predecessor already spent.
    pub fn remaining_limits(&self) -> Result<TaskLimitsV1, String> {
        let spent = self.predecessor_chargeable_tokens.get();
        let tokens = u128::from(self.original_limits.tokens)
            .checked_sub(spent)
            .and_then(|left| u64::try_from(left).ok())
            .ok_or("Predecessor spent its whole token allowance")?;
        let attempts = u64::from(self.original_limits.max_attempts)
            .checked_sub(self.predecessor_begun_attempts)
            .and_then(|left| u32::try_from(left).ok())
            .filter(|left| *left > 0)
            .ok_or("Predecessor spent its whole Attempt allowance")?;
        Ok(TaskLimitsV1 {
            tokens,
            max_attempts: attempts,
            deadline_unix_ms: self.original_limits.deadline_unix_ms,
            verification: self.original_limits.verification.clone(),
        })
    }

    /// Whether `limits` fit inside [`Self::remaining_limits`] without a later deadline.
    pub fn admits(&self, limits: &TaskLimitsV1) -> Result<(), String> {
        let remaining = self.remaining_limits()?;
        require(
            limits.tokens <= remaining.tokens
                && limits.max_attempts <= remaining.max_attempts
                && limits.deadline_unix_ms <= remaining.deadline_unix_ms
                && limits.verification.tokens <= remaining.verification.tokens
                && limits.verification.attempts <= remaining.verification.attempts
                && limits.verification.wall_ms <= remaining.verification.wall_ms,
            "Successor limits exceed what its predecessor left within the original bounds",
        )
    }
}

/// A recovery allowance captured by the compiled plan. Without it, login cannot create one:
/// an exhausted or absent allowance requires an explicitly linked continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthRecoveryAllowanceV1 {
    pub probes: u32,
    pub tokens_per_probe: u64,
    pub wall_ms_per_probe: u64,
}

impl AuthRecoveryAllowanceV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            (1..=16).contains(&self.probes)
                && self.tokens_per_probe > 0
                && self.wall_ms_per_probe > 0
                && safe_number(self.tokens_per_probe)
                && safe_number(self.wall_ms_per_probe),
            "Auth recovery allowance needs one to sixteen bounded positive probes",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(n: char) -> String {
        format!("sha256:{}", n.to_string().repeat(64))
    }

    fn binding() -> TaskAuthBindingV1 {
        TaskAuthBindingV1 {
            context: ProviderAuthContextV1 {
                provider: "work".into(),
                provider_kind: "claude".into(),
                principal_id: "principal-1".into(),
            },
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            invocation_policy_id: digest('1'),
        }
    }

    #[test]
    fn only_authentication_classes_exist_and_contention_is_the_one_transient() {
        use TaskAuthFailureV1::*;
        for failure in [
            AuthMissing,
            AuthRevoked,
            AuthExpired,
            AuthRefreshFailed,
            AuthRejected,
        ] {
            assert!(!failure.transient());
            assert_eq!(
                serde_json::to_value(failure).unwrap(),
                serde_json::json!(failure.as_str())
            );
        }
        assert!(AuthRefreshContended.transient());
        assert!(serde_json::from_str::<TaskAuthFailureV1>("\"quota\"").is_err());
        assert!(serde_json::from_str::<TaskAuthFailureV1>("\"network\"").is_err());
    }

    #[test]
    fn opaque_references_refuse_urls_whitespace_and_secrets_shapes() {
        for good in ["chat-1", "u:42", "telegram@123", "a.b_c"] {
            assert!(is_opaque_ref(good), "{good}");
        }
        for bad in [
            "",
            "https://claude.ai/x",
            "a b",
            "-lead",
            "x/y",
            "code=ABC",
            &"a".repeat(129),
        ] {
            assert!(!is_opaque_ref(bad), "{bad}");
        }
    }

    #[test]
    fn suspensions_bind_exact_identities_and_bounded_contexts() {
        let mut suspension = TaskAuthSuspensionV1 {
            task_id: "task-a".into(),
            task_revision_id: digest('2'),
            plan_id: digest('3'),
            deadline_unix_ms: 10,
            failures: vec![TaskAuthFailedAttemptV1 {
                attempt_id: "a".repeat(26),
                node: "root.nodes.implement".into(),
                reservation_id: "reservation:3".into(),
                charged_tokens: 7u128.into(),
                failure: TaskAuthFailureV1::AuthRevoked,
            }],
            contexts: BTreeMap::from([(
                digest('4'),
                TaskAuthRequirementV1 {
                    binding: binding(),
                    generation: 1,
                },
            )]),
            participant: Some(TaskAuthParticipantV1 {
                requester_ref: "user-1".into(),
                coordinator_ref: "chat-1".into(),
            }),
        };
        suspension.validate().unwrap();
        let value = serde_json::to_value(&suspension).unwrap();
        assert_eq!(value["failures"][0]["charged_tokens"], "7");
        assert_eq!(
            serde_json::from_value::<TaskAuthSuspensionV1>(value).unwrap(),
            suspension
        );
        suspension.contexts.clear();
        assert!(suspension.validate().is_err());
    }

    #[test]
    fn continuation_limits_retain_spend_and_never_extend_the_deadline() {
        let link = TaskContinuationV1 {
            predecessor_task_id: "task-a".into(),
            predecessor_revision_id: digest('1'),
            predecessor_plan_id: digest('2'),
            predecessor_result_id: digest('3'),
            predecessor_chargeable_tokens: 900u128.into(),
            predecessor_begun_attempts: 3,
            original_limits: TaskLimitsV1 {
                tokens: 1000,
                max_attempts: 5,
                deadline_unix_ms: 5000,
                verification: crate::task::VerificationReserveV1 {
                    tokens: 50,
                    attempts: 1,
                    wall_ms: 10,
                },
            },
            reason: TaskContinuationReasonV1::ProviderAuth,
        };
        link.validate().unwrap();
        let remaining = link.remaining_limits().unwrap();
        assert_eq!((remaining.tokens, remaining.max_attempts), (100, 2));
        link.admits(&remaining).unwrap();
        let mut wider = remaining.clone();
        wider.tokens = 101;
        assert!(link.admits(&wider).is_err());
        let mut later = remaining.clone();
        later.deadline_unix_ms = 5001;
        assert!(link.admits(&later).is_err());
        let mut spent = link.clone();
        spent.predecessor_chargeable_tokens = 1001u128.into();
        assert!(spent.remaining_limits().is_err());
    }

    #[test]
    fn recovery_events_are_closed_and_secret_free_by_shape() {
        let event = ProviderAuthRecoveryEventV1 {
            schema: PROVIDER_AUTH_RECOVERY_EVENT_V1.into(),
            context_key: digest('5'),
            context: binding().context,
            generation: 1,
            now_unix_ms: 1,
            change: ProviderAuthRecoveryChangeV1::Authenticated {
                login_ref: "https://claude.ai/oauth?code=SECRET".into(),
            },
        };
        assert!(event.validate().is_err());
        let mut value = serde_json::to_value(ProviderAuthRecoveryEventV1 {
            change: ProviderAuthRecoveryChangeV1::Verified {
                task_id: "task-a".into(),
                attempt_id: "b".repeat(26),
            },
            ..event
        })
        .unwrap();
        serde_json::from_value::<ProviderAuthRecoveryEventV1>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        value["change"]["token"] = serde_json::json!("x");
        assert!(serde_json::from_value::<ProviderAuthRecoveryEventV1>(value).is_err());
    }
}
