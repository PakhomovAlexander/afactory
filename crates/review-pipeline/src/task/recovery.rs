//! Verified Provider auth recovery for one suspended Task (ADR-0141).
//!
//! The coordinator never logs in and never trusts a login: a token-free status that reports
//! "unauthenticated" asks for the private handoff with no paid call, and an "authenticated"
//! status only earns one bounded verification probe on the Task's own ledger. Only an
//! acknowledged probe within its reservation verifies a generation, and only a claim covering
//! every still-required context lets the original work continue, through the ordinary runtime.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_core::Producer;
use review_core::task::auth_recovery::{
    AUTH_RECOVERY_NODE, TaskAuthBindingV1, TaskAuthBlockV1, TaskAuthOutcomeV1,
    TaskAuthProbeOutcomeV1,
};
use review_core::task::usage::TaskTokenUsageV3;
use review_store::store::task::auth_recovery::{AuthRecoveryStatus, TaskAuthClaim, TaskAuthProbe};
use review_store::store::task::{TaskAuthority, TaskLease, task_run_id};
use review_store::{Cas, EventStore, StoreError};

use super::host::TaskModelBinding;

/// What one bounded verification inference returned. Usage is exact when known; unknown or
/// incomplete usage is charged as the whole reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthProbeReturn {
    pub usage: Option<TaskTokenUsageV3>,
    pub usage_complete: bool,
    pub outcome: TaskAuthProbeOutcomeV1,
}

/// One paid verification inference through the exact captured binding. It receives no Task
/// input, source, prompt or credential, and runs only after the Store started its probe.
pub trait AuthProbeTransport {
    fn probe(
        &self,
        binding: &TaskAuthBindingV1,
        timeout: Duration,
        cancellation: Option<&AtomicBool>,
    ) -> AuthProbeReturn;
}

/// Machine facts the host resolves now, never from a record: the binding this machine would
/// dispatch with for the required context, and a token-free authentication status.
pub trait AuthContextHost {
    fn current(&self, required: &TaskAuthBindingV1) -> Result<TaskAuthBindingV1, String>;
    fn authenticated(&self, binding: &TaskAuthBindingV1) -> Result<bool, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRecoveryStep {
    /// The Task is not paused on Provider auth; nothing was spent.
    NotSuspended,
    /// A claim covers every required context; continue the original Task now. `replayed`
    /// means an interrupted earlier recovery already claimed it.
    Resumed { claim_id: String, replayed: bool },
    /// These contexts need the private login handoff. Nothing verified them.
    LoginRequired { context_keys: Vec<String> },
    /// A probe failed for a reason that is not authentication.
    VerificationFailed { context_keys: Vec<String> },
    /// The original authority, deadline, binding or allowance no longer permits recovery.
    Blocked { reason: TaskAuthBlockV1 },
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

type Store<'a> = Mutex<&'a mut EventStore>;

fn locked<'g, 'a>(store: &'g Store<'a>) -> std::sync::MutexGuard<'g, &'a mut EventStore> {
    store.lock().expect("Task Store")
}

fn blocked(
    store: &Store<'_>,
    cas: &Cas,
    task_id: &str,
    error: StoreError,
) -> Result<AuthRecoveryStep, String> {
    match error {
        StoreError::AuthRecoveryBlocked(reason) => {
            locked(store)
                .record_task_auth_block(cas, task_id, reason)
                .map_err(|e| e.to_string())?;
            Ok(AuthRecoveryStep::Blocked { reason })
        }
        other => Err(other.to_string()),
    }
}

/// Verify every still-unverified context of a suspended Task and claim its continuation.
/// Contexts are handled in key order and the first that cannot be verified stops the pass, so
/// no call reaches a later context while an earlier one is still broken. The Store is locked
/// per durable step, never across the paid call, so a lease heartbeat can renew meanwhile.
pub fn verify_and_claim(
    store: &Mutex<&mut EventStore>,
    cas: &Cas,
    lease: &TaskLease,
    authority: &dyn TaskAuthority,
    host: &dyn AuthContextHost,
    transport: &dyn AuthProbeTransport,
    cancellation: Option<&AtomicBool>,
) -> Result<AuthRecoveryStep, String> {
    let task_id = lease.task_id().to_owned();
    locked(store)
        .recover_task_auth_probes(cas, lease)
        .map_err(|e| e.to_string())?;
    let state = locked(store)
        .task_projection(cas, &task_id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    if state.auth.active_suspension().is_none() {
        if state.auth.active_claim().is_some() {
            return claim(store, cas, lease, authority);
        }
        return Ok(AuthRecoveryStep::NotSuspended);
    }
    let (_, suspension) = state.auth.active_suspension().cloned().expect("checked");
    for (key, requirement) in &suspension.contexts {
        let recovery = locked(store)
            .provider_auth_recovery(key)
            .map_err(|e| e.to_string())?;
        if recovery.status == AuthRecoveryStatus::Verified
            && recovery.generation >= requirement.generation
        {
            continue;
        }
        let current = match host.current(&requirement.binding) {
            Ok(current) if current == requirement.binding => current,
            _ => {
                return blocked(
                    store,
                    cas,
                    &task_id,
                    StoreError::AuthRecoveryBlocked(TaskAuthBlockV1::BindingChanged),
                );
            }
        };
        // A missing login asks for the private handoff before any paid call. A contended
        // refresh is transient and may verify without one.
        if !host.authenticated(&current)? {
            locked(store)
                .record_task_auth_outcome(cas, &task_id, TaskAuthOutcomeV1::LoginRequired {})
                .map_err(|e| e.to_string())?;
            return Ok(AuthRecoveryStep::LoginRequired {
                context_keys: vec![key.clone()],
            });
        }
        let reserved = locked(store).reserve_task_auth_probe(cas, lease, key, &current, authority);
        let mut probe = match reserved {
            Ok(probe) => probe,
            Err(error) => return blocked(store, cas, &task_id, error),
        };
        let started =
            locked(store).start_task_auth_probe(cas, lease, &mut probe, &current, authority);
        if let Err(error) = started {
            locked(store)
                .release_task_auth_probe(cas, lease, &probe, "Verification dispatch was refused")
                .map_err(|e| e.to_string())?;
            return blocked(store, cas, &task_id, error);
        }
        let timeout =
            Duration::from_millis(probe.reservation().deadline_unix_ms.saturating_sub(now()));
        let returned = if timeout.is_zero() {
            AuthProbeReturn {
                usage: None,
                usage_complete: false,
                outcome: TaskAuthProbeOutcomeV1::Failed {},
            }
        } else {
            transport.probe(&current, timeout, cancellation)
        };
        let settlement = settle(store, cas, lease, &probe, returned)?;
        if settlement.overrun {
            return Ok(AuthRecoveryStep::Blocked {
                reason: TaskAuthBlockV1::BudgetBreached,
            });
        }
        // Another Task's concurrent probe may have verified this generation first; this probe
        // keeps its charge either way, and the claim rechecks every generation.
        let latest = locked(store)
            .provider_auth_recovery(key)
            .map_err(|e| e.to_string())?;
        if settlement.verified
            || latest.status == AuthRecoveryStatus::Verified
                && latest.generation >= requirement.generation
        {
            continue;
        }
        return Ok(match settlement.outcome {
            TaskAuthProbeOutcomeV1::AuthFailed { .. } => AuthRecoveryStep::LoginRequired {
                context_keys: vec![key.clone()],
            },
            _ => AuthRecoveryStep::VerificationFailed {
                context_keys: vec![key.clone()],
            },
        });
    }
    claim(store, cas, lease, authority)
}

fn claim(
    store: &Store<'_>,
    cas: &Cas,
    lease: &TaskLease,
    authority: &dyn TaskAuthority,
) -> Result<AuthRecoveryStep, String> {
    let claimed = locked(store).claim_task_auth_resume(cas, lease, authority);
    match claimed {
        Ok(TaskAuthClaim::Claimed {
            claim_id, replayed, ..
        }) => Ok(AuthRecoveryStep::Resumed { claim_id, replayed }),
        Ok(TaskAuthClaim::Unverified { context_keys }) => {
            Ok(AuthRecoveryStep::LoginRequired { context_keys })
        }
        Err(error) => blocked(store, cas, lease.task_id(), error),
    }
}

fn settle(
    store: &Store<'_>,
    cas: &Cas,
    lease: &TaskLease,
    probe: &TaskAuthProbe,
    returned: AuthProbeReturn,
) -> Result<review_store::store::task::auth_recovery::TaskAuthProbeSettlement, String> {
    let reserved = u128::from(probe.reservation().tokens);
    let reported = returned
        .usage
        .as_ref()
        .map(|usage| usage.chargeable_tokens.get());
    // Unknown or incomplete native usage keeps the whole reservation charged.
    let charge = match reported {
        Some(reported) if returned.usage_complete => reported,
        Some(reported) => reported.max(reserved),
        None => reserved,
    };
    let usage_id = match returned.usage {
        Some(mut usage) => {
            usage.chargeable_tokens = charge.into();
            let state = locked(store)
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
            let suspension = state
                .auth
                .active_suspension()
                .map(|(id, _)| id.clone())
                .ok_or("Verification lost its suspension")?;
            Some(review_runner::task::usage::persist_task_usage_exact(
                cas,
                Producer::Attempt {
                    run_id: task_run_id(lease.task_id()).map_err(|e| e.to_string())?,
                    node_id: AUTH_RECOVERY_NODE.into(),
                    attempt_id: probe.attempt_id().into(),
                },
                &suspension,
                &usage,
            )?)
        }
        None => None,
    };
    locked(store)
        .settle_task_auth_probe(cas, lease, probe, charge, returned.outcome, usage_id)
        .map_err(|e| e.to_string())
}

/// The production probe: the same native adapter and fixed readiness input as Provider
/// admission, selected by the exact captured binding.
pub struct ModelAuthProbe<'a> {
    pub models: &'a std::collections::BTreeMap<String, TaskModelBinding<'a>>,
}

impl AuthProbeTransport for ModelAuthProbe<'_> {
    fn probe(
        &self,
        binding: &TaskAuthBindingV1,
        timeout: Duration,
        cancellation: Option<&AtomicBool>,
    ) -> AuthProbeReturn {
        let failed = AuthProbeReturn {
            usage: None,
            usage_complete: false,
            outcome: TaskAuthProbeOutcomeV1::Failed {},
        };
        let Some(model) = self.models.values().find(|model| {
            TaskAuthBindingV1::of_plan_binding(&model.binding).as_ref() == Some(binding)
                && model.adapter.model_settings()
                    == Some((binding.model.clone(), binding.effort.clone()))
                && model.adapter.provider_kind() == binding.context.provider_kind
        }) else {
            return failed;
        };
        let Ok(directory) = tempfile::tempdir() else {
            return failed;
        };
        let returned = model.adapter.invoke(
            // The probe has no Task CAS evidence to retain; its raw capture stays local.
            &match Cas::open(directory.path().join("cas")) {
                Ok(cas) => cas,
                Err(_) => return failed,
            },
            directory.path(),
            review_runner::task::provider::PROBE_INPUT.to_vec(),
            timeout,
            review_runner::task::WorkerAccess::ReadOnly,
            cancellation,
            &[],
        );
        let usage_complete = returned
            .usage_observation
            .as_ref()
            .is_none_or(|observation| observation.charge_complete);
        let outcome = match (&returned.message, returned.auth_failure()) {
            (Ok(message), _)
                if message.len() <= 64
                    && std::str::from_utf8(message).is_ok_and(|text| {
                        text.trim().trim_end_matches('.').eq_ignore_ascii_case("OK")
                    }) =>
            {
                TaskAuthProbeOutcomeV1::Acknowledged {}
            }
            (_, Some(failure)) => TaskAuthProbeOutcomeV1::AuthFailed { failure },
            _ => TaskAuthProbeOutcomeV1::Failed {},
        };
        AuthProbeReturn {
            usage: returned.usage,
            usage_complete,
            outcome,
        }
    }
}
