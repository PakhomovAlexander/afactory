//! Runtime Provider auth recovery in the common Store (ADR-0141).
//!
//! Two durable logs cooperate. The Task log records suspension, paid verification probes on the
//! Task's own ledger and the resume claim. Each auth context has one append-only recovery log in
//! the same database, numbered in generations: concurrent failures share a generation, and a
//! failure after verification opens the next one. Every write that depends on a recovery log
//! carries its exact next sequence into the Task write transaction, so a racing failure, probe
//! or claim makes the dependent write fail instead of acting under a stale generation.

use std::collections::BTreeMap;

use review_attempt::task_budget::TaskReservation;
use review_core::task::auth_recovery::*;
use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::{TaskPhaseV1, TaskWaitingReasonV1};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use super::{
    EventStore, TaskAuthority, TaskLease, TaskProjection, conflict, now, payload, task_run_id,
};
use crate::store::{StoreError, u64_column};
use crate::{Cas, content_id};

const TABLE_EXISTS: &str = "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_auth_recovery')";

const SUSPENDED: TaskPhaseV1 = TaskPhaseV1::Waiting {
    reason: TaskWaitingReasonV1::NeedsProviderAuth,
};

fn invalidated(message: impl Into<String>) -> StoreError {
    StoreError::AuthRecoveryInvalidated(message.into())
}

/// The Store-derived identity of one auth context. It names the context; it is not a secret,
/// an account identity proof or a native directory path.
pub fn auth_context_key(context: &ProviderAuthContextV1) -> Result<String, StoreError> {
    context.validate().map_err(conflict)?;
    content_id(&json!({
        "namespace": "af/provider-auth-context/1",
        "provider": context.provider,
        "provider_kind": context.provider_kind,
        "principal_id": context.principal_id,
    }))
    .map_err(|error| conflict(error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRecoveryStatus {
    /// No failure was ever recorded for this context in this Store.
    Healthy,
    /// The current generation is unverified. Login alone never leaves this state.
    Failed,
    /// A probe within its own reservation acknowledged the current generation.
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthParticipation {
    pub sequence: u64,
    pub task_id: String,
    pub suspension_id: String,
    pub failure: Option<TaskAuthFailureV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthProbeFailure {
    pub sequence: u64,
    pub task_id: String,
    pub attempt_id: String,
    pub outcome: TaskAuthProbeOutcomeV1,
    pub overrun: bool,
}

/// One outcome a coordinator must learn, delivered at most once to its own participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOutcomeRecord {
    pub context_key: String,
    pub sequence: u64,
    pub generation: u64,
    pub task_id: String,
    pub suspension_id: String,
    pub outcome: TaskAuthOutcomeV1,
    pub participant: Option<TaskAuthParticipantV1>,
    pub delivery_ref: Option<String>,
    pub recorded_unix_ms: u64,
}

/// The fold of one auth context's recovery log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRecoveryState {
    pub context_key: String,
    pub context: Option<ProviderAuthContextV1>,
    pub next_sequence: u64,
    pub generation: u64,
    pub status: AuthRecoveryStatus,
    /// Stage 1 login references recorded for the current generation. They verify nothing.
    pub login_refs: Vec<String>,
    /// The sequence, Task and probe Attempt that verified the current generation.
    pub verified: Option<(u64, String, String)>,
    pub participants: Vec<AuthParticipation>,
    pub probe_failures: Vec<AuthProbeFailure>,
    pub outcomes: Vec<AuthOutcomeRecord>,
    last_time: u64,
}

impl AuthRecoveryState {
    fn empty(context_key: &str) -> Self {
        Self {
            context_key: context_key.into(),
            context: None,
            next_sequence: 0,
            generation: 0,
            status: AuthRecoveryStatus::Healthy,
            login_refs: Vec::new(),
            verified: None,
            participants: Vec::new(),
            probe_failures: Vec::new(),
            outcomes: Vec::new(),
            last_time: 0,
        }
    }

    /// The generation a newly failing Task joins: the open one, or the next after verification.
    pub fn joining_generation(&self) -> u64 {
        if self.status == AuthRecoveryStatus::Failed {
            self.generation
        } else {
            self.generation + 1
        }
    }

    /// Outcomes that still owe their coordinator a notification. An outcome without a private
    /// route stays visible as undeliverable and is never redirected to another coordinator.
    pub fn pending_notifications(&self) -> impl Iterator<Item = &AuthOutcomeRecord> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.participant.is_some() && outcome.delivery_ref.is_none())
    }

    fn apply(&mut self, sequence: u64, event: &ProviderAuthRecoveryEventV1) -> Result<(), String> {
        event.validate()?;
        if sequence != self.next_sequence
            || event.context_key != self.context_key
            || event.now_unix_ms < self.last_time
            || self
                .context
                .as_ref()
                .is_some_and(|context| context != &event.context)
        {
            return Err("Recovery log sequence, context or policy time is inconsistent".into());
        }
        let current = event.generation == self.generation;
        match &event.change {
            ProviderAuthRecoveryChangeV1::Failed {
                task_id,
                suspension_id,
                failure,
            } => {
                if event.generation != self.joining_generation() {
                    return Err("A recovery failure must join the open generation".into());
                }
                if event.generation != self.generation {
                    self.generation = event.generation;
                    self.login_refs.clear();
                    self.verified = None;
                    self.participants.clear();
                    self.probe_failures.clear();
                }
                if self
                    .participants
                    .iter()
                    .any(|p| &p.suspension_id == suspension_id)
                {
                    return Err("A suspension joins one generation once".into());
                }
                self.status = AuthRecoveryStatus::Failed;
                self.participants.push(AuthParticipation {
                    sequence,
                    task_id: task_id.clone(),
                    suspension_id: suspension_id.clone(),
                    failure: *failure,
                });
            }
            ProviderAuthRecoveryChangeV1::Authenticated { login_ref } => {
                if !current || self.status != AuthRecoveryStatus::Failed {
                    return Err("Login can only join an open recovery generation".into());
                }
                if !self.login_refs.contains(login_ref) {
                    self.login_refs.push(login_ref.clone());
                }
            }
            ProviderAuthRecoveryChangeV1::ProbeFailed {
                task_id,
                attempt_id,
                outcome,
                overrun,
            } => {
                if !current || self.status != AuthRecoveryStatus::Failed {
                    return Err("A probe failure belongs to the open generation".into());
                }
                self.probe_failures.push(AuthProbeFailure {
                    sequence,
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    outcome: outcome.clone(),
                    overrun: *overrun,
                });
            }
            ProviderAuthRecoveryChangeV1::Verified {
                task_id,
                attempt_id,
            } => {
                if !current || self.status != AuthRecoveryStatus::Failed {
                    return Err("Only the open generation can be verified".into());
                }
                self.status = AuthRecoveryStatus::Verified;
                self.verified = Some((sequence, task_id.clone(), attempt_id.clone()));
            }
            ProviderAuthRecoveryChangeV1::Outcome {
                task_id,
                suspension_id,
                outcome,
                participant,
            } => {
                if event.generation > self.generation
                    || self.outcomes.iter().any(|recorded| {
                        &recorded.task_id == task_id
                            && &recorded.suspension_id == suspension_id
                            && &recorded.outcome == outcome
                    })
                {
                    return Err("A recovery outcome is recorded once for its generation".into());
                }
                self.outcomes.push(AuthOutcomeRecord {
                    context_key: self.context_key.clone(),
                    sequence,
                    generation: event.generation,
                    task_id: task_id.clone(),
                    suspension_id: suspension_id.clone(),
                    outcome: outcome.clone(),
                    participant: participant.clone(),
                    delivery_ref: None,
                    recorded_unix_ms: event.now_unix_ms,
                });
            }
            ProviderAuthRecoveryChangeV1::Notified {
                outcome_sequence,
                coordinator_ref,
                delivery_ref,
            } => {
                let outcome = self
                    .outcomes
                    .iter_mut()
                    .find(|outcome| outcome.sequence == *outcome_sequence)
                    .ok_or("Notification names no recorded outcome")?;
                if outcome.generation != event.generation
                    || outcome.delivery_ref.is_some()
                    || outcome
                        .participant
                        .as_ref()
                        .is_none_or(|p| &p.coordinator_ref != coordinator_ref)
                {
                    return Err(
                        "Only the outcome's own coordinator acknowledges it, exactly once".into(),
                    );
                }
                outcome.delivery_ref = Some(delivery_ref.clone());
            }
        }
        self.context = Some(event.context.clone());
        self.next_sequence = sequence + 1;
        self.last_time = event.now_unix_ms;
        Ok(())
    }

    fn event(
        &self,
        context: &ProviderAuthContextV1,
        generation: u64,
        change: ProviderAuthRecoveryChangeV1,
    ) -> Result<ProviderAuthRecoveryEventV1, StoreError> {
        Ok(ProviderAuthRecoveryEventV1 {
            schema: PROVIDER_AUTH_RECOVERY_EVENT_V1.into(),
            context_key: self.context_key.clone(),
            context: context.clone(),
            generation,
            now_unix_ms: now()?.max(self.last_time),
            change,
        })
    }
}

fn table_exists(connection: &Connection) -> Result<bool, StoreError> {
    Ok(connection.query_row(TABLE_EXISTS, [], |row| row.get(0))?)
}

fn next_sequence(connection: &Connection, key: &str) -> Result<u64, StoreError> {
    if !table_exists(connection)? {
        return Ok(0);
    }
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(sequence)+1,0) FROM provider_auth_recovery WHERE context_key=?1",
        [key],
        |row| u64_column(row, 0),
    )?)
}

pub(in crate::store) fn read_state(
    connection: &Connection,
    key: &str,
) -> Result<AuthRecoveryState, StoreError> {
    let mut state = AuthRecoveryState::empty(key);
    if !table_exists(connection)? {
        return Ok(state);
    }
    let mut statement = connection.prepare(
        "SELECT sequence, payload FROM provider_auth_recovery WHERE context_key=?1 ORDER BY sequence",
    )?;
    let rows = statement.query_map([key], |row| {
        Ok((u64_column(row, 0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (sequence, text) = row?;
        let event: ProviderAuthRecoveryEventV1 = serde_json::from_str(&text)?;
        state
            .apply(sequence, &event)
            .map_err(|error| conflict(format!("Corrupt Provider auth recovery log: {error}")))?;
    }
    Ok(state)
}

/// Recovery-log rows written in the same transaction as a Task transition, and the exact next
/// sequence of every recovery log that write depended on.
#[derive(Debug, Clone, Default)]
pub(in crate::store) struct RecoveryWrite {
    fences: BTreeMap<String, u64>,
    /// A claimed dispatch needs each context still verified under its claimed generation and
    /// verification, whatever unrelated rows (outcomes, acknowledgements) landed meanwhile.
    verified: BTreeMap<String, (u64, u64)>,
    rows: Vec<(String, u64, String)>,
    /// A successor's explicit predecessor link: each finished Task is continued at most once.
    continuation: Option<(String, String)>,
}

impl RecoveryWrite {
    fn fence(&mut self, state: &AuthRecoveryState) {
        self.fences
            .entry(state.context_key.clone())
            .or_insert(state.next_sequence);
    }

    /// Validate an event against a working copy of the log and queue it.
    fn push(
        &mut self,
        state: &mut AuthRecoveryState,
        event: ProviderAuthRecoveryEventV1,
    ) -> Result<(), StoreError> {
        self.fence(state);
        let sequence = state.next_sequence;
        state.apply(sequence, &event).map_err(conflict)?;
        self.rows.push((
            state.context_key.clone(),
            sequence,
            serde_json::to_string(&event)?,
        ));
        Ok(())
    }

    /// Runs inside the Store's immediate write transaction, before the Task rows land.
    pub(in crate::store) fn publish(&self, connection: &Connection) -> Result<(), StoreError> {
        for (key, expected) in &self.fences {
            if next_sequence(connection, key)? != *expected {
                return Err(invalidated(
                    "Provider auth recovery changed while this write was prepared",
                ));
            }
        }
        for (key, (generation, verified_sequence)) in &self.verified {
            let current = read_state(connection, key)?;
            if !claim_current(&current, *generation, *verified_sequence) {
                return Err(invalidated(
                    "A newer Provider auth failure invalidated this Task's resume claim",
                ));
            }
        }
        for (key, sequence, payload) in &self.rows {
            connection.execute(
                "INSERT INTO provider_auth_recovery (context_key, sequence, payload) VALUES (?1, ?2, ?3)",
                params![key, *sequence as i64, payload],
            )?;
        }
        if let Some((predecessor, successor)) = &self.continuation {
            let existing: Option<String> = connection
                .query_row(
                    "SELECT successor_task_id FROM task_continuation WHERE predecessor_task_id=?1",
                    [predecessor],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                return Err(conflict(format!(
                    "Task `{predecessor}` is already continued by `{existing}`"
                )));
            }
            connection.execute(
                "INSERT INTO task_continuation (predecessor_task_id, successor_task_id) VALUES (?1, ?2)",
                params![predecessor, successor],
            )?;
        }
        Ok(())
    }
}

/// One live verification probe on the Task ledger. Only the Store constructs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAuthProbe {
    pub context_key: String,
    pub generation: u64,
    pub reservation: TaskReservation,
    pub suspension_id: String,
    pub epoch: u64,
    pub started: bool,
    pub released: bool,
    pub settled: Option<(u128, TaskAuthProbeOutcomeV1)>,
}

/// What a Task log says about auth recovery.
#[derive(Debug, Clone, Default)]
pub struct TaskAuthProjection {
    pub suspensions: Vec<(String, TaskAuthSuspensionV1)>,
    pub claims: Vec<(String, TaskAuthResumeClaimV1)>,
    /// Index of the suspension this Task is paused on.
    suspended: Option<usize>,
    /// Index of the claim this Task's current execution continues under.
    claimed: Option<usize>,
    pub probes: BTreeMap<String, RecordedAuthProbe>,
}

impl TaskAuthProjection {
    pub fn active_suspension(&self) -> Option<&(String, TaskAuthSuspensionV1)> {
        self.suspended.map(|index| &self.suspensions[index])
    }

    pub fn active_claim(&self) -> Option<&(String, TaskAuthResumeClaimV1)> {
        self.claimed.map(|index| &self.claims[index])
    }

    pub fn pending_probes(&self) -> impl Iterator<Item = (&String, &RecordedAuthProbe)> {
        self.probes
            .iter()
            .filter(|(_, probe)| !probe.released && probe.settled.is_none())
    }

    /// Exact charge retained by every settled verification probe of this Task.
    pub fn probe_charge(&self) -> u128 {
        self.probes
            .values()
            .filter_map(|probe| probe.settled.as_ref().map(|(charge, _)| *charge))
            .sum()
    }
}

/// Model bindings of the current plan, keyed by context key.
fn plan_bindings(plan: &ExecutionPlanV1) -> Result<Vec<(String, TaskAuthBindingV1)>, StoreError> {
    let mut bindings = Vec::new();
    for binding in plan.bindings.values() {
        if let Some(value) = TaskAuthBindingV1::of_plan_binding(binding) {
            let key = auth_context_key(&value.context)?;
            if !bindings.contains(&(key.clone(), value.clone())) {
                bindings.push((key, value));
            }
        }
    }
    Ok(bindings)
}

fn plan_binds(
    plan: &ExecutionPlanV1,
    key: &str,
    binding: &TaskAuthBindingV1,
) -> Result<bool, StoreError> {
    Ok(plan_bindings(plan)?
        .iter()
        .any(|(known, value)| known == key && value == binding))
}

impl TaskProjection {
    pub(super) fn apply_auth(
        &mut self,
        cas: &Cas,
        change: &TaskChangeV1,
        time: u64,
    ) -> Result<(), StoreError> {
        match change {
            TaskChangeV1::AuthSuspended { suspension_id } => {
                let suspension: TaskAuthSuspensionV1 =
                    payload(cas, suspension_id, TASK_AUTH_SUSPENSION_V1)?;
                suspension.validate().map_err(conflict)?;
                let plan_id = self
                    .plan_id
                    .clone()
                    .ok_or_else(|| conflict("Task has no plan"))?;
                if suspension.task_id != self.task_id
                    || suspension.task_revision_id != self.revision_id
                    || suspension.plan_id != plan_id
                    || suspension.deadline_unix_ms != self.revision.limits.deadline_unix_ms
                {
                    return Err(conflict(
                        "Auth suspension must bind this Task's exact revision, plan and deadline",
                    ));
                }
                if !self.admitted || self.phase != (TaskPhaseV1::Running {}) {
                    return Err(conflict("Only admitted running work can suspend for auth"));
                }
                let plan = super::plan(cas, &plan_id, self)?;
                for (key, requirement) in &suspension.contexts {
                    if !plan_binds(&plan, key, &requirement.binding)? {
                        return Err(conflict(
                            "Auth suspension names a context the captured plan does not bind",
                        ));
                    }
                }
                let execution = self
                    .execution
                    .as_ref()
                    .ok_or_else(|| conflict("Auth suspension has no execution"))?;
                if !execution.pending_attempts().is_empty()
                    || self.auth.pending_probes().next().is_some()
                {
                    return Err(conflict(
                        "Settle or release every Attempt before suspending for auth",
                    ));
                }
                let accounting: BTreeMap<_, _> = execution
                    .attempt_accounting()
                    .into_iter()
                    .map(|row| (row.attempt_id.clone(), row))
                    .collect();
                for failure in &suspension.failures {
                    let row = accounting
                        .get(&failure.attempt_id)
                        .ok_or_else(|| conflict("Auth suspension names an unknown Attempt"))?;
                    if row.plan_id != plan_id
                        || row.reservation.node != failure.node
                        || row.reservation.id != failure.reservation_id
                        || row.charged_tokens != failure.charged_tokens.get()
                        || !matches!(row.result, Some(TaskAttemptResultV1::Failed { .. }))
                    {
                        return Err(conflict(
                            "Auth suspension must retain each failed Attempt's exact accounting",
                        ));
                    }
                }
                self.resume_phase = Some(self.phase.clone());
                self.phase = SUSPENDED;
                self.auth
                    .suspensions
                    .push((suspension_id.clone(), suspension));
                self.auth.suspended = Some(self.auth.suspensions.len() - 1);
                self.auth.claimed = None;
            }
            TaskChangeV1::AuthProbeReserved {
                context_key,
                generation,
                attempt_id,
                reservation_id,
                reserved_tokens,
                deadline_unix_ms,
            } => {
                let (suspension_id, suspension) = self
                    .auth
                    .active_suspension()
                    .cloned()
                    .ok_or_else(|| conflict("Verification needs an auth-suspended Task"))?;
                let requirement = suspension
                    .contexts
                    .get(context_key)
                    .ok_or_else(|| conflict("Verification names a context this Task lacks"))?;
                if *generation < requirement.generation
                    || self.auth.pending_probes().next().is_some()
                {
                    return Err(conflict(
                        "Verification must target a current generation, one probe at a time",
                    ));
                }
                self.check_approval(cas, time)?;
                let execution = self
                    .execution
                    .as_mut()
                    .ok_or_else(|| conflict("Verification has no Task ledger"))?;
                let reservation = execution
                    .budget
                    .prepare(AUTH_RECOVERY_NODE, time)
                    .map_err(conflict)?;
                if reservation.id != *reservation_id
                    || reservation.tokens != *reserved_tokens
                    || reservation.deadline_unix_ms != *deadline_unix_ms
                    || execution.dispatch_auth_probe() != *attempt_id
                {
                    return Err(conflict(
                        "Verification reservation differs from the Task's shared accounting",
                    ));
                }
                self.auth.probes.insert(
                    attempt_id.clone(),
                    RecordedAuthProbe {
                        context_key: context_key.clone(),
                        generation: *generation,
                        reservation,
                        suspension_id,
                        epoch: self.epoch,
                        started: false,
                        released: false,
                        settled: None,
                    },
                );
            }
            TaskChangeV1::AuthProbeStarted { attempt_id } => {
                if self.phase != SUSPENDED {
                    return Err(conflict("Verification starts only while auth-suspended"));
                }
                self.check_approval(cas, time)?;
                let probe = self
                    .auth
                    .probes
                    .get_mut(attempt_id)
                    .ok_or_else(|| conflict("Unknown verification probe"))?;
                if probe.started
                    || probe.released
                    || probe.settled.is_some()
                    || probe.epoch != self.epoch
                {
                    return Err(conflict(
                        "Verification probe cannot start under this writer",
                    ));
                }
                self.execution
                    .as_mut()
                    .ok_or_else(|| conflict("Verification has no Task ledger"))?
                    .budget
                    .begin(&probe.reservation.id, time)
                    .map_err(conflict)?;
                probe.started = true;
            }
            TaskChangeV1::AuthProbeReleased { attempt_id, .. } => {
                let probe = self
                    .auth
                    .probes
                    .get_mut(attempt_id)
                    .ok_or_else(|| conflict("Unknown verification probe"))?;
                if probe.started || probe.released || probe.settled.is_some() {
                    return Err(conflict("A started verification probe keeps its charge"));
                }
                self.execution
                    .as_mut()
                    .ok_or_else(|| conflict("Verification has no Task ledger"))?
                    .budget
                    .release(&probe.reservation.id)
                    .map_err(conflict)?;
                probe.released = true;
            }
            TaskChangeV1::AuthProbeSettled {
                attempt_id,
                charged_tokens,
                outcome,
                usage_id,
            } => {
                let probe = self
                    .auth
                    .probes
                    .get(attempt_id)
                    .ok_or_else(|| conflict("Unknown verification probe"))?
                    .clone();
                let charge = charged_tokens.get();
                if !probe.started || probe.released || probe.settled.is_some() {
                    return Err(conflict("Verification settlement has no started probe"));
                }
                if *outcome == (TaskAuthProbeOutcomeV1::Abandoned {})
                    && charge < u128::from(probe.reservation.tokens)
                {
                    return Err(conflict(
                        "An abandoned verification probe retains its full reservation",
                    ));
                }
                if let Some(id) = usage_id {
                    let envelope =
                        super::envelope(cas, id, review_core::task::usage::TASK_TOKEN_USAGE_V3)?;
                    let usage: review_core::task::usage::TaskTokenUsageV3 =
                        serde_json::from_value(envelope.payload)?;
                    if envelope.producer
                        != (review_core::Producer::Attempt {
                            run_id: task_run_id(&self.task_id)?,
                            node_id: AUTH_RECOVERY_NODE.into(),
                            attempt_id: attempt_id.clone(),
                        })
                        || usage.chargeable_tokens.get() > charge
                    {
                        return Err(conflict(
                            "Verification usage belongs to another probe or exceeds its charge",
                        ));
                    }
                }
                let execution = self
                    .execution
                    .as_mut()
                    .ok_or_else(|| conflict("Verification has no Task ledger"))?;
                execution
                    .budget
                    .settle_exact(&probe.reservation.id, charge)
                    .map_err(conflict)?;
                execution.charge_auth_probe(attempt_id, charge)?;
                self.auth
                    .probes
                    .get_mut(attempt_id)
                    .expect("known probe")
                    .settled = Some((charge, outcome.clone()));
            }
            TaskChangeV1::AuthResumeClaimed { claim_id } => {
                let claim: TaskAuthResumeClaimV1 =
                    payload(cas, claim_id, TASK_AUTH_RESUME_CLAIM_V1)?;
                claim.validate().map_err(conflict)?;
                let (suspension_id, suspension) = self
                    .auth
                    .active_suspension()
                    .cloned()
                    .ok_or_else(|| conflict("Resume claim needs an auth-suspended Task"))?;
                if self.phase != SUSPENDED
                    || claim.task_id != self.task_id
                    || claim.suspension_id != suspension_id
                    || claim.task_revision_id != self.revision_id
                    || Some(&claim.plan_id) != self.plan_id.as_ref()
                    || !claim.contexts.keys().eq(suspension.contexts.keys())
                    || claim.contexts.iter().any(|(key, claimed)| {
                        claimed.generation < suspension.contexts[key].generation
                    })
                {
                    return Err(conflict(
                        "Resume claim must cover the exact suspension and every required context",
                    ));
                }
                if self.auth.pending_probes().next().is_some()
                    || self
                        .execution
                        .as_ref()
                        .is_none_or(|execution| execution.budget.breached())
                {
                    return Err(conflict(
                        "A breached or still-probing Task cannot resume its work",
                    ));
                }
                self.check_approval(cas, time)?;
                self.phase = self
                    .resume_phase
                    .take()
                    .ok_or_else(|| conflict("Auth suspension has no resumable phase"))?;
                self.auth.claims.push((claim_id.clone(), claim));
                self.auth.claimed = Some(self.auth.claims.len() - 1);
                self.auth.suspended = None;
            }
            _ => unreachable!("auth recovery applies only auth changes"),
        }
        Ok(())
    }

    /// Another path left the auth pause (source refresh, revocation, terminalization): the old
    /// suspension cannot be claimed and an old claim no longer covers the current plan.
    pub(super) fn reconcile_auth(&mut self) {
        if self.phase != SUSPENDED {
            self.auth.suspended = None;
        }
        if self.auth.active_claim().is_some_and(|(_, claim)| {
            claim.task_revision_id != self.revision_id
                || Some(&claim.plan_id) != self.plan_id.as_ref()
        }) {
            self.auth.claimed = None;
        }
    }
}

/// A claimed context is current while its generation is still the verified one the claim names.
fn claim_current(recovery: &AuthRecoveryState, generation: u64, verified_sequence: u64) -> bool {
    recovery.generation == generation
        && recovery.status == AuthRecoveryStatus::Verified
        && recovery.verified.as_ref().map(|v| v.0) == Some(verified_sequence)
}

/// Whether this transition starts a new effect that a resume claim must still cover.
fn dispatching(cas: &Cas, change: &TaskChangeV1) -> Result<bool, StoreError> {
    Ok(match change {
        TaskChangeV1::ExecutionRecorded { record_id } => matches!(
            super::execution::read_execution_record(cas, record_id)?.record,
            TaskExecutionRecordV1::Invocation { .. }
                | TaskExecutionRecordV1::Reserved { .. }
                | TaskExecutionRecordV1::ContextBound { .. }
                | TaskExecutionRecordV1::Started { .. }
                | TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
                | TaskExecutionRecordV1::ExperimentPrepared { .. }
                | TaskExecutionRecordV1::ExperimentChildrenRegistered { .. }
        ),
        TaskChangeV1::PlanAdmitted { .. } | TaskChangeV1::Resumed {} => true,
        _ => false,
    })
}

impl EventStore {
    /// The current fold of one auth context's recovery log.
    pub fn provider_auth_recovery(
        &self,
        context_key: &str,
    ) -> Result<AuthRecoveryState, StoreError> {
        if !review_core::is_digest(context_key) {
            return Err(conflict("Invalid auth context key"));
        }
        read_state(&self.conn, context_key)
    }

    /// Every recorded auth context in this Store, in key order.
    pub fn provider_auth_recoveries(&self) -> Result<Vec<AuthRecoveryState>, StoreError> {
        if !table_exists(&self.conn)? {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = self
            .conn
            .prepare(
                "SELECT DISTINCT context_key FROM provider_auth_recovery ORDER BY context_key",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        keys.iter().map(|key| read_state(&self.conn, key)).collect()
    }

    /// The dispatch fence: after a resume claim, every new effect of the Task must still find
    /// each claimed context verified under the claimed generation, atomically with its append.
    pub(super) fn auth_dispatch_fence(
        &self,
        cas: &Cas,
        transition: &TaskTransitionV1,
        state: Option<&TaskProjection>,
    ) -> Result<Option<RecoveryWrite>, StoreError> {
        let Some((_, claim)) = state.and_then(|state| state.auth.active_claim()) else {
            return Ok(None);
        };
        if !dispatching(cas, &transition.change)? {
            return Ok(None);
        }
        let mut write = RecoveryWrite::default();
        for (key, claimed) in &claim.contexts {
            let recovery = read_state(&self.conn, key)?;
            if !claim_current(&recovery, claimed.generation, claimed.verified_sequence) {
                return Err(invalidated(
                    "A newer Provider auth failure invalidated this Task's resume claim",
                ));
            }
            write
                .verified
                .insert(key.clone(), (claimed.generation, claimed.verified_sequence));
        }
        Ok(Some(write))
    }

    fn recovery_transition(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        change: TaskChangeV1,
        state: TaskProjection,
        write: RecoveryWrite,
    ) -> Result<review_core::RunEvent, StoreError> {
        let time = now()?;
        self.recovery_transition_at(cas, lease, change, state, write, time)
    }

    /// A reservation's deadline derives from its policy time, so it is recorded at that time.
    fn recovery_transition_at(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        change: TaskChangeV1,
        state: TaskProjection,
        write: RecoveryWrite,
        time: u64,
    ) -> Result<review_core::RunEvent, StoreError> {
        let task_id = lease.task_id.clone();
        self.append_task_transition_checked(
            cas,
            &task_id,
            TaskTransitionV1 {
                writer: lease.writer.clone(),
                epoch: lease.epoch,
                now_unix_ms: time,
                change,
            },
            None,
            Some(state),
            Some(write),
        )
    }

    fn current_lease(&self, cas: &Cas, lease: &TaskLease) -> Result<TaskProjection, StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now()?,
            change: TaskChangeV1::Resumed {},
        })?;
        Ok(state)
    }

    /// Durably suspend running work after an auth failure, before any terminal result. The
    /// failed Attempts' accounting is read from the ledger, never supplied by the runtime.
    /// Each context joins its open generation or opens the next one; all recovery rows and the
    /// Task transition land in one transaction.
    pub fn suspend_task_for_provider_auth(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        failures: &[(String, TaskAuthFailureV1)],
        contexts: &[(TaskAuthBindingV1, Option<TaskAuthFailureV1>)],
        participant: Option<TaskAuthParticipantV1>,
    ) -> Result<(String, TaskAuthSuspensionV1), StoreError> {
        let state = self.current_lease(cas, lease)?;
        let plan_id = state
            .plan_id
            .clone()
            .ok_or_else(|| conflict("Task has no plan"))?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Auth suspension has no execution"))?;
        let accounting: BTreeMap<_, _> = execution
            .attempt_accounting()
            .into_iter()
            .map(|row| (row.attempt_id.clone(), row))
            .collect();
        let mut retained = Vec::new();
        for (attempt_id, failure) in failures {
            let row = accounting
                .get(attempt_id)
                .ok_or_else(|| conflict("Auth suspension names an unknown Attempt"))?;
            retained.push(TaskAuthFailedAttemptV1 {
                attempt_id: attempt_id.clone(),
                node: row.reservation.node.clone(),
                reservation_id: row.reservation.id.clone(),
                charged_tokens: row.charged_tokens.into(),
                failure: *failure,
            });
        }
        let mut recoveries = BTreeMap::new();
        let mut required = BTreeMap::new();
        for (binding, failure) in contexts {
            binding.validate().map_err(conflict)?;
            let key = auth_context_key(&binding.context)?;
            let recovery = read_state(&self.conn, &key)?;
            required.insert(
                key.clone(),
                TaskAuthRequirementV1 {
                    binding: binding.clone(),
                    generation: recovery.joining_generation(),
                },
            );
            recoveries.insert(key, (recovery, binding.context.clone(), *failure));
        }
        let suspension = TaskAuthSuspensionV1 {
            task_id: state.task_id.clone(),
            task_revision_id: state.revision_id.clone(),
            plan_id: plan_id.clone(),
            deadline_unix_ms: state.revision.limits.deadline_unix_ms,
            failures: retained,
            contexts: required,
            participant,
        };
        suspension.validate().map_err(conflict)?;
        let (suspension_id, _) = cas
            .put_artifact(
                TASK_AUTH_SUSPENSION_V1,
                review_core::Producer::KernelOperation {
                    run_id: task_run_id(&state.task_id)?,
                    node_id: None,
                    operation_id: "task-auth-suspension@1".into(),
                },
                vec![suspension.task_revision_id.clone(), plan_id],
                None,
                serde_json::to_value(&suspension)?,
            )
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        let mut write = RecoveryWrite::default();
        for (recovery, context, failure) in recoveries.values_mut() {
            let generation = recovery.joining_generation();
            let event = recovery.event(
                context,
                generation,
                ProviderAuthRecoveryChangeV1::Failed {
                    task_id: state.task_id.clone(),
                    suspension_id: suspension_id.clone(),
                    failure: *failure,
                },
            )?;
            write.push(recovery, event)?;
        }
        self.recovery_transition(
            cas,
            lease,
            TaskChangeV1::AuthSuspended {
                suspension_id: suspension_id.clone(),
            },
            state,
            write,
        )?;
        Ok((suspension_id, suspension))
    }

    /// Every fence a paid verification probe must pass, rechecked before reservation and again
    /// before dispatch: current writer, the exact suspended revision and plan, a current
    /// developer approval and authorization, the original deadline, an unbreached ledger, and
    /// the same principal, model, effort and invocation policy the plan captured.
    fn checked_auth_probe(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        context_key: &str,
        current: &TaskAuthBindingV1,
        authority: &dyn TaskAuthority,
    ) -> Result<
        (
            TaskProjection,
            String,
            TaskAuthSuspensionV1,
            AuthRecoveryState,
        ),
        StoreError,
    > {
        let state = self.current_lease(cas, lease)?;
        let (suspension_id, suspension) = state
            .auth
            .active_suspension()
            .cloned()
            .ok_or_else(|| conflict("Task is not suspended for Provider auth"))?;
        let time = now()?;
        if suspension.task_revision_id != state.revision_id
            || Some(&suspension.plan_id) != state.plan_id.as_ref()
        {
            return Err(block(TaskAuthBlockV1::AuthorityChanged));
        }
        if time >= suspension.deadline_unix_ms {
            return Err(block(TaskAuthBlockV1::DeadlineExpired));
        }
        let plan = self
            .current_auth_plan(cas, &state, authority, time)
            .map_err(|_| block(TaskAuthBlockV1::AuthorityChanged))?;
        let requirement = suspension
            .contexts
            .get(context_key)
            .ok_or_else(|| conflict("Task does not require this auth context"))?;
        if &requirement.binding != current || !plan_binds(&plan, context_key, current)? {
            return Err(block(TaskAuthBlockV1::BindingChanged));
        }
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Verification has no Task ledger"))?;
        if execution.budget.breached() {
            return Err(block(TaskAuthBlockV1::BudgetBreached));
        }
        if execution.graph.auth_recovery.is_none() {
            return Err(block(TaskAuthBlockV1::AllowanceMissing));
        }
        let recovery = read_state(&self.conn, context_key)?;
        if recovery.generation < requirement.generation {
            return Err(conflict("Recovery log lost this Task's generation"));
        }
        Ok((state, suspension_id, suspension, recovery))
    }

    /// The plan authority check `current_task_plan` performs, for a Task paused on auth.
    fn current_auth_plan(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        authority: &dyn TaskAuthority,
        time: u64,
    ) -> Result<ExecutionPlanV1, StoreError> {
        let id = state
            .plan_id
            .as_ref()
            .ok_or_else(|| conflict("Task has no plan"))?;
        let plan = self.authorized_plan(cas, state, id, authority)?;
        state.check_approval(cas, time)?;
        if let Some(decision) = state.decisions.get(id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        if !state.admitted {
            return Err(conflict("Task plan is not admitted"));
        }
        Ok(plan)
    }

    /// Settle a lost writer's verification probe before any new probe: started work keeps its
    /// full reservation, an unstarted reservation returns its credit.
    pub fn recover_task_auth_probes(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
    ) -> Result<(), StoreError> {
        let state = self.current_lease(cas, lease)?;
        for (attempt_id, probe) in state.auth.pending_probes() {
            if probe.epoch == lease.epoch {
                return Err(conflict("Current writer still owns a verification probe"));
            }
            let change = if probe.started {
                TaskChangeV1::AuthProbeSettled {
                    attempt_id: attempt_id.clone(),
                    charged_tokens: u128::from(probe.reservation.tokens).into(),
                    outcome: TaskAuthProbeOutcomeV1::Abandoned {},
                    usage_id: None,
                }
            } else {
                TaskChangeV1::AuthProbeReleased {
                    attempt_id: attempt_id.clone(),
                    reason: "Recovered before durable dispatch".into(),
                }
            };
            let fresh = self.current_lease(cas, lease)?;
            let mut write = RecoveryWrite::default();
            if probe.started {
                let mut recovery = read_state(&self.conn, &probe.context_key)?;
                if recovery.generation == probe.generation
                    && recovery.status == AuthRecoveryStatus::Failed
                {
                    let context = recovery
                        .context
                        .clone()
                        .ok_or_else(|| conflict("Recovery log has no context"))?;
                    let event = recovery.event(
                        &context,
                        probe.generation,
                        ProviderAuthRecoveryChangeV1::ProbeFailed {
                            task_id: lease.task_id.clone(),
                            attempt_id: attempt_id.clone(),
                            outcome: TaskAuthProbeOutcomeV1::Abandoned {},
                            overrun: false,
                        },
                    )?;
                    write.push(&mut recovery, event)?;
                }
            }
            self.recovery_transition(cas, lease, change, fresh, write)?;
        }
        Ok(())
    }

    /// Reserve one verification probe on the original Task ledger. The probe draws on the same
    /// token, Attempt and deadline limits as every Worker, under the plan's captured allowance.
    pub fn reserve_task_auth_probe(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        context_key: &str,
        current: &TaskAuthBindingV1,
        authority: &dyn TaskAuthority,
    ) -> Result<TaskAuthProbe, StoreError> {
        let (state, suspension_id, _, recovery) =
            self.checked_auth_probe(cas, lease, context_key, current, authority)?;
        if recovery.status != AuthRecoveryStatus::Failed {
            return Err(conflict(
                "This auth context generation needs no verification",
            ));
        }
        if state.auth.pending_probes().next().is_some() {
            return Err(conflict(
                "A verification probe is still pending; recover it first",
            ));
        }
        // Stamped exactly as the writer's transition will be, so replay derives this deadline.
        let time = now()?.max(state.last_time);
        let mut execution = state
            .execution
            .clone()
            .ok_or_else(|| conflict("Verification has no Task ledger"))?;
        let reservation = execution
            .budget
            .prepare(AUTH_RECOVERY_NODE, time)
            .map_err(|error| {
                if error.contains("exhausted its Attempt limit") {
                    block(TaskAuthBlockV1::AllowanceExhausted)
                } else {
                    conflict(error)
                }
            })?;
        let attempt_id = execution.dispatch_auth_probe();
        let mut write = RecoveryWrite::default();
        write.fence(&recovery);
        self.recovery_transition_at(
            cas,
            lease,
            TaskChangeV1::AuthProbeReserved {
                context_key: context_key.into(),
                generation: recovery.generation,
                attempt_id: attempt_id.clone(),
                reservation_id: reservation.id.clone(),
                reserved_tokens: reservation.tokens,
                deadline_unix_ms: reservation.deadline_unix_ms,
            },
            state,
            write,
            time,
        )?;
        Ok(TaskAuthProbe {
            task_id: lease.task_id.clone(),
            writer_epoch: lease.epoch,
            attempt_id,
            context_key: context_key.into(),
            generation: recovery.generation,
            suspension_id,
            reservation,
            started: false,
        })
    }

    /// Recheck every fence again, then durably start the probe. Nothing is sent before this.
    pub fn start_task_auth_probe(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        probe: &mut TaskAuthProbe,
        current: &TaskAuthBindingV1,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, _, _, recovery) =
            self.checked_auth_probe(cas, lease, &probe.context_key, current, authority)?;
        check_probe(&state, lease, probe)?;
        if recovery.generation != probe.generation || recovery.status != AuthRecoveryStatus::Failed
        {
            return Err(invalidated(
                "The auth context changed generation before this probe started",
            ));
        }
        let mut write = RecoveryWrite::default();
        write.fence(&recovery);
        self.recovery_transition(
            cas,
            lease,
            TaskChangeV1::AuthProbeStarted {
                attempt_id: probe.attempt_id.clone(),
            },
            state,
            write,
        )?;
        probe.started = true;
        Ok(())
    }

    /// Return an unstarted probe's credit after a refused dispatch check.
    pub fn release_task_auth_probe(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        probe: &TaskAuthProbe,
        reason: &str,
    ) -> Result<(), StoreError> {
        let state = self.current_lease(cas, lease)?;
        check_probe(&state, lease, probe)?;
        if probe.started {
            return Err(conflict("A started verification probe keeps its charge"));
        }
        self.recovery_transition(
            cas,
            lease,
            TaskChangeV1::AuthProbeReleased {
                attempt_id: probe.attempt_id.clone(),
                reason: reason.into(),
            },
            state,
            RecoveryWrite::default(),
        )?;
        Ok(())
    }

    /// Retain the probe's exact charge and record what it proved. Only an acknowledgement whose
    /// exact charge fits its own reservation verifies the generation; an overrun keeps the
    /// charge, breaches the Task ledger and verifies nothing. An authentication failure tells
    /// every participant of this generation that a private login is still required.
    pub fn settle_task_auth_probe(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        probe: &TaskAuthProbe,
        charged_tokens: u128,
        outcome: TaskAuthProbeOutcomeV1,
        usage_id: Option<String>,
    ) -> Result<TaskAuthProbeSettlement, StoreError> {
        if outcome == (TaskAuthProbeOutcomeV1::Abandoned {}) {
            return Err(conflict("Only recovery abandons a verification probe"));
        }
        let state = self.current_lease(cas, lease)?;
        check_probe(&state, lease, probe)?;
        if !probe.started {
            return Err(conflict("Verification settlement has no started probe"));
        }
        let overrun = charged_tokens > u128::from(probe.reservation.tokens);
        let mut recovery = read_state(&self.conn, &probe.context_key)?;
        let context = recovery
            .context
            .clone()
            .ok_or_else(|| conflict("Recovery log has no context"))?;
        let mut write = RecoveryWrite::default();
        let open = recovery.generation == probe.generation
            && recovery.status == AuthRecoveryStatus::Failed;
        let verified = open && !overrun && outcome == TaskAuthProbeOutcomeV1::Acknowledged {};
        if verified {
            let event = recovery.event(
                &context,
                probe.generation,
                ProviderAuthRecoveryChangeV1::Verified {
                    task_id: lease.task_id.clone(),
                    attempt_id: probe.attempt_id.clone(),
                },
            )?;
            write.push(&mut recovery, event)?;
        } else if open {
            let event = recovery.event(
                &context,
                probe.generation,
                ProviderAuthRecoveryChangeV1::ProbeFailed {
                    task_id: lease.task_id.clone(),
                    attempt_id: probe.attempt_id.clone(),
                    outcome: outcome.clone(),
                    overrun,
                },
            )?;
            write.push(&mut recovery, event)?;
            let notified: Vec<(String, String)> =
                if matches!(outcome, TaskAuthProbeOutcomeV1::AuthFailed { .. }) && !overrun {
                    recovery
                        .participants
                        .iter()
                        .map(|p| (p.task_id.clone(), p.suspension_id.clone()))
                        .collect()
                } else {
                    vec![(lease.task_id.clone(), probe.suspension_id.clone())]
                };
            let result = if overrun {
                TaskAuthOutcomeV1::Blocked {
                    reason: TaskAuthBlockV1::BudgetBreached,
                }
            } else if matches!(outcome, TaskAuthProbeOutcomeV1::AuthFailed { .. }) {
                TaskAuthOutcomeV1::LoginRequired {}
            } else {
                TaskAuthOutcomeV1::VerificationFailed {}
            };
            for (task_id, suspension_id) in notified {
                let participant = self.participant_of(cas, &task_id, &suspension_id)?;
                push_outcome(
                    &mut write,
                    &mut recovery,
                    &context,
                    &task_id,
                    &suspension_id,
                    result.clone(),
                    participant,
                )?;
            }
        }
        self.recovery_transition(
            cas,
            lease,
            TaskChangeV1::AuthProbeSettled {
                attempt_id: probe.attempt_id.clone(),
                charged_tokens: charged_tokens.into(),
                outcome: outcome.clone(),
                usage_id,
            },
            state,
            write,
        )?;
        Ok(TaskAuthProbeSettlement {
            verified,
            overrun,
            outcome,
        })
    }

    fn participant_of(
        &self,
        cas: &Cas,
        task_id: &str,
        suspension_id: &str,
    ) -> Result<Option<TaskAuthParticipantV1>, StoreError> {
        let suspension: TaskAuthSuspensionV1 =
            payload(cas, suspension_id, TASK_AUTH_SUSPENSION_V1)?;
        if suspension.task_id != task_id {
            return Err(conflict(
                "Recovery participant names another Task's suspension",
            ));
        }
        Ok(suspension.participant)
    }

    /// Claim the suspended Task's continuation once every still-required context is verified
    /// under its current generation and the original authority still holds. The claim is
    /// durable before any dispatch; a crash afterwards is finished by continuing the claimed
    /// Task, never by a second claim. Each participant's coordinator gets one `resumed` outcome.
    pub fn claim_task_auth_resume(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<TaskAuthClaim, StoreError> {
        let state = self.current_lease(cas, lease)?;
        if state.phase == (TaskPhaseV1::Running {})
            && let Some((id, claim)) = state.auth.active_claim()
        {
            return Ok(TaskAuthClaim::Claimed {
                claim_id: id.clone(),
                claim: claim.clone(),
                replayed: true,
            });
        }
        let (suspension_id, suspension) = state
            .auth
            .active_suspension()
            .cloned()
            .ok_or_else(|| conflict("Task is not suspended for Provider auth"))?;
        let time = now()?;
        if time >= suspension.deadline_unix_ms {
            return Err(block(TaskAuthBlockV1::DeadlineExpired));
        }
        if suspension.task_revision_id != state.revision_id
            || Some(&suspension.plan_id) != state.plan_id.as_ref()
        {
            return Err(block(TaskAuthBlockV1::AuthorityChanged));
        }
        self.current_auth_plan(cas, &state, authority, time)
            .map_err(|_| block(TaskAuthBlockV1::AuthorityChanged))?;
        if state
            .execution
            .as_ref()
            .is_none_or(|execution| execution.budget.breached())
        {
            return Err(block(TaskAuthBlockV1::BudgetBreached));
        }
        let mut write = RecoveryWrite::default();
        let mut recoveries = BTreeMap::new();
        let mut contexts = BTreeMap::new();
        let mut unverified = Vec::new();
        for (key, requirement) in &suspension.contexts {
            let recovery = read_state(&self.conn, key)?;
            write.fence(&recovery);
            match (&recovery.status, &recovery.verified) {
                (AuthRecoveryStatus::Verified, Some((sequence, _, _)))
                    if recovery.generation >= requirement.generation =>
                {
                    contexts.insert(
                        key.clone(),
                        TaskAuthClaimedContextV1 {
                            generation: recovery.generation,
                            verified_sequence: *sequence,
                        },
                    );
                }
                _ => unverified.push(key.clone()),
            }
            recoveries.insert(key.clone(), recovery);
        }
        if !unverified.is_empty() {
            return Ok(TaskAuthClaim::Unverified {
                context_keys: unverified,
            });
        }
        let claim = TaskAuthResumeClaimV1 {
            task_id: state.task_id.clone(),
            suspension_id: suspension_id.clone(),
            task_revision_id: state.revision_id.clone(),
            plan_id: suspension.plan_id.clone(),
            contexts,
        };
        claim.validate().map_err(conflict)?;
        let (claim_id, _) = cas
            .put_artifact(
                TASK_AUTH_RESUME_CLAIM_V1,
                review_core::Producer::KernelOperation {
                    run_id: task_run_id(&state.task_id)?,
                    node_id: None,
                    operation_id: "task-auth-resume-claim@1".into(),
                },
                vec![suspension_id.clone()],
                None,
                serde_json::to_value(&claim)?,
            )
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        // One outcome per Task participation, kept in its first context's log.
        let recovery = recoveries
            .values_mut()
            .next()
            .ok_or_else(|| conflict("Resume claim needs a context"))?;
        let context = recovery
            .context
            .clone()
            .ok_or_else(|| conflict("Recovery log has no context"))?;
        push_outcome(
            &mut write,
            recovery,
            &context,
            &state.task_id,
            &suspension_id,
            TaskAuthOutcomeV1::Resumed {},
            suspension.participant.clone(),
        )?;
        self.recovery_transition(
            cas,
            lease,
            TaskChangeV1::AuthResumeClaimed {
                claim_id: claim_id.clone(),
            },
            state,
            write,
        )?;
        Ok(TaskAuthClaim::Claimed {
            claim_id,
            claim,
            replayed: false,
        })
    }

    fn recovery_only(
        &mut self,
        context_key: &str,
        build: impl FnOnce(&AuthRecoveryState) -> Result<Vec<ProviderAuthRecoveryEventV1>, StoreError>,
    ) -> Result<AuthRecoveryState, StoreError> {
        let mut state = read_state(&self.conn, context_key)?;
        let events = build(&state)?;
        let mut write = RecoveryWrite::default();
        write.fence(&state);
        for event in events {
            write.push(&mut state, event)?;
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        write.publish(&tx)?;
        tx.commit()?;
        Ok(state)
    }

    /// Join Stage 1's private login to the open generation. The reference is opaque and the
    /// login grants nothing: the generation stays unverified until a paid probe acknowledges.
    pub fn record_provider_auth_login(
        &mut self,
        context_key: &str,
        login_ref: &str,
    ) -> Result<AuthRecoveryState, StoreError> {
        self.recovery_only(context_key, |state| {
            if state.status != AuthRecoveryStatus::Failed {
                return Err(conflict(
                    "No open recovery generation for this auth context",
                ));
            }
            if state.login_refs.iter().any(|known| known == login_ref) {
                return Ok(Vec::new());
            }
            let context = state
                .context
                .clone()
                .ok_or_else(|| conflict("Recovery log has no context"))?;
            Ok(vec![state.event(
                &context,
                state.generation,
                ProviderAuthRecoveryChangeV1::Authenticated {
                    login_ref: login_ref.into(),
                },
            )?])
        })
    }

    /// Record a blocking outcome for a suspended Task once, for its own coordinator.
    pub fn record_task_auth_block(
        &mut self,
        cas: &Cas,
        task_id: &str,
        reason: TaskAuthBlockV1,
    ) -> Result<Option<AuthOutcomeRecord>, StoreError> {
        self.record_task_auth_outcome(cas, task_id, TaskAuthOutcomeV1::Blocked { reason })
    }

    /// Record one non-claim outcome of a suspended Task once, for its own coordinator: still
    /// blocked, or a private login required before any paid verification. It never changes the
    /// Task, its ledger or the generation.
    pub fn record_task_auth_outcome(
        &mut self,
        cas: &Cas,
        task_id: &str,
        outcome: TaskAuthOutcomeV1,
    ) -> Result<Option<AuthOutcomeRecord>, StoreError> {
        if outcome == (TaskAuthOutcomeV1::Resumed {}) {
            return Err(conflict("Only a verified claim records a resumed outcome"));
        }
        let state = self
            .task_projection(cas, task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let Some((suspension_id, suspension)) = state.auth.active_suspension().cloned() else {
            return Ok(None);
        };
        let key = suspension
            .contexts
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| conflict("Suspension has no context"))?;
        let recovery = self.recovery_only(&key, |recovery| {
            let generation = suspension.contexts[&key]
                .generation
                .min(recovery.generation);
            if generation == 0
                || recovery.outcomes.iter().any(|recorded| {
                    recorded.task_id == task_id
                        && recorded.suspension_id == suspension_id
                        && recorded.outcome == outcome
                })
            {
                return Ok(Vec::new());
            }
            let context = recovery
                .context
                .clone()
                .ok_or_else(|| conflict("Recovery log has no context"))?;
            Ok(vec![recovery.event(
                &context,
                generation,
                ProviderAuthRecoveryChangeV1::Outcome {
                    task_id: task_id.into(),
                    suspension_id: suspension_id.clone(),
                    outcome: outcome.clone(),
                    participant: suspension.participant.clone(),
                },
            )?])
        })?;
        Ok(recovery
            .outcomes
            .iter()
            .find(|recorded| {
                recorded.task_id == task_id
                    && recorded.suspension_id == suspension_id
                    && recorded.outcome == outcome
            })
            .cloned())
    }

    /// Every recorded outcome of this Task's suspensions, across their auth contexts, in log
    /// order. Pending ones have a private route and no delivery yet.
    pub fn task_auth_outcomes(
        &self,
        cas: &Cas,
        task_id: &str,
    ) -> Result<Vec<AuthOutcomeRecord>, StoreError> {
        let state = self
            .task_projection(cas, task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let keys: std::collections::BTreeSet<_> = state
            .auth
            .suspensions
            .iter()
            .flat_map(|(_, suspension)| suspension.contexts.keys().cloned())
            .collect();
        let mut outcomes = Vec::new();
        for key in keys {
            outcomes.extend(
                read_state(&self.conn, &key)?
                    .outcomes
                    .into_iter()
                    .filter(|outcome| outcome.task_id == task_id),
            );
        }
        Ok(outcomes)
    }

    /// Acknowledge one delivered outcome. Repeating the same acknowledgement is idempotent; a
    /// different coordinator, or a different delivery of an acknowledged outcome, is refused.
    pub fn acknowledge_task_auth_outcome(
        &mut self,
        context_key: &str,
        outcome_sequence: u64,
        coordinator_ref: &str,
        delivery_ref: &str,
    ) -> Result<bool, StoreError> {
        if !is_opaque_ref(coordinator_ref) || !is_opaque_ref(delivery_ref) {
            return Err(conflict(
                "Notification references must be opaque host references",
            ));
        }
        let mut fresh = true;
        self.recovery_only(context_key, |state| {
            let outcome = state
                .outcomes
                .iter()
                .find(|outcome| outcome.sequence == outcome_sequence)
                .ok_or_else(|| conflict("Unknown recovery outcome"))?;
            let route = outcome
                .participant
                .as_ref()
                .ok_or_else(|| conflict("This outcome has no private coordinator route"))?;
            if route.coordinator_ref != coordinator_ref {
                return Err(conflict("Recovery outcome belongs to another coordinator"));
            }
            if let Some(previous) = &outcome.delivery_ref {
                return if previous == delivery_ref {
                    fresh = false;
                    Ok(Vec::new())
                } else {
                    Err(conflict("Recovery outcome was already delivered"))
                };
            }
            let context = state
                .context
                .clone()
                .ok_or_else(|| conflict("Recovery log has no context"))?;
            Ok(vec![state.event(
                &context,
                outcome.generation,
                ProviderAuthRecoveryChangeV1::Notified {
                    outcome_sequence,
                    coordinator_ref: coordinator_ref.into(),
                    delivery_ref: delivery_ref.into(),
                },
            )?])
        })?;
        Ok(fresh)
    }
}

impl EventStore {
    /// The exact link a successor of this finished Task must carry: its result, plan, original
    /// limits and every token and Attempt it spent. Nothing here is inferred from a login.
    pub fn task_continuation_link(
        &self,
        cas: &Cas,
        predecessor_task_id: &str,
    ) -> Result<TaskContinuationV1, StoreError> {
        let predecessor = self
            .task_projection(cas, predecessor_task_id)?
            .ok_or_else(|| conflict("Unknown predecessor Task"))?;
        let TaskPhaseV1::Finished { result_id } = &predecessor.phase else {
            return Err(conflict(
                "Only a finished Task is continued by a linked successor; resume a suspended one",
            ));
        };
        let result: review_core::task::TaskResultV1 =
            payload(cas, result_id, review_core::task::TASK_RESULT_V1)?;
        if result.acceptance == review_core::task::TaskAcceptanceV1::Satisfied {
            return Err(conflict("A satisfied Task needs no continuation"));
        }
        if let Some(successor) = self.task_successor(predecessor_task_id)? {
            return Err(conflict(format!(
                "Task `{predecessor_task_id}` is already continued by `{successor}`"
            )));
        }
        let budget = predecessor.execution.as_ref().map(|e| &e.budget);
        let link = TaskContinuationV1 {
            predecessor_task_id: predecessor.task_id.clone(),
            predecessor_revision_id: predecessor.revision_id.clone(),
            predecessor_plan_id: predecessor
                .plan_id
                .clone()
                .ok_or_else(|| conflict("Predecessor has no plan"))?,
            predecessor_result_id: result_id.clone(),
            predecessor_chargeable_tokens: budget.map_or(0, |b| b.committed_tokens()).into(),
            predecessor_begun_attempts: budget.map_or(0, |b| b.begun_attempts()),
            original_limits: predecessor.revision.limits.clone(),
            reason: TaskContinuationReasonV1::ProviderAuth,
        };
        link.validate().map_err(conflict)?;
        link.remaining_limits().map_err(conflict)?;
        Ok(link)
    }

    /// The successor that continues a finished Task, if one was opened.
    pub fn task_successor(&self, predecessor_task_id: &str) -> Result<Option<String>, StoreError> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='task_continuation')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT successor_task_id FROM task_continuation WHERE predecessor_task_id=?1",
                [predecessor_task_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The explicit predecessor link a revision carries, if any. At most one is allowed.
    pub fn revision_continuation(
        cas: &Cas,
        revision: &review_core::task::TaskRevisionV1,
    ) -> Result<Option<(String, TaskContinuationV1)>, StoreError> {
        let mut found = None;
        for id in &revision.provenance.input_artifact_ids {
            let Some(envelope) = cas
                .get_optional_artifact(id)
                .map_err(|error| StoreError::Artifact(error.to_string()))?
            else {
                continue;
            };
            if envelope.artifact_type != TASK_CONTINUATION_V1 {
                continue;
            }
            let link: TaskContinuationV1 = serde_json::from_value(envelope.payload)?;
            link.validate().map_err(conflict)?;
            if found.replace((id.clone(), link)).is_some() {
                return Err(conflict("A Task continues at most one predecessor"));
            }
        }
        Ok(found)
    }

    /// Validate a successor's opening against its finished predecessor: same request, same
    /// authority, exact retained accounting, and limits inside what the original bounds left.
    pub(super) fn continuation_write(
        &self,
        cas: &Cas,
        revision: &review_core::task::TaskRevisionV1,
    ) -> Result<Option<RecoveryWrite>, StoreError> {
        let Some((_, link)) = Self::revision_continuation(cas, revision)? else {
            return Ok(None);
        };
        if link.predecessor_task_id == revision.task_id {
            return Err(conflict("A Task cannot continue itself"));
        }
        let expected = self.task_continuation_link(cas, &link.predecessor_task_id)?;
        if expected != link {
            return Err(conflict(
                "Continuation link differs from its predecessor's exact result and accounting",
            ));
        }
        let predecessor = self
            .task_projection(cas, &link.predecessor_task_id)?
            .ok_or_else(|| conflict("Unknown predecessor Task"))?;
        let before = &predecessor.revision;
        if revision.kind != before.kind
            || revision.goal != before.goal
            || revision.inputs != before.inputs
            || revision.required_outputs != before.required_outputs
            || revision.acceptance != before.acceptance
            || revision.authority != before.authority
            || revision.strategy != before.strategy
            || revision.pipeline != before.pipeline
            || revision.facts != before.facts
        {
            return Err(conflict(
                "A continuation keeps its predecessor's request, acceptance and authority",
            ));
        }
        link.admits(&revision.limits).map_err(conflict)?;
        Ok(Some(RecoveryWrite {
            continuation: Some((link.predecessor_task_id.clone(), revision.task_id.clone())),
            ..RecoveryWrite::default()
        }))
    }
}

fn push_outcome(
    write: &mut RecoveryWrite,
    recovery: &mut AuthRecoveryState,
    context: &ProviderAuthContextV1,
    task_id: &str,
    suspension_id: &str,
    outcome: TaskAuthOutcomeV1,
    participant: Option<TaskAuthParticipantV1>,
) -> Result<(), StoreError> {
    if recovery.outcomes.iter().any(|recorded| {
        recorded.task_id == task_id
            && recorded.suspension_id == suspension_id
            && recorded.outcome == outcome
    }) {
        return Ok(());
    }
    let event = recovery.event(
        context,
        recovery.generation,
        ProviderAuthRecoveryChangeV1::Outcome {
            task_id: task_id.into(),
            suspension_id: suspension_id.into(),
            outcome,
            participant,
        },
    )?;
    write.push(recovery, event)
}

fn check_probe(
    state: &TaskProjection,
    lease: &TaskLease,
    probe: &TaskAuthProbe,
) -> Result<(), StoreError> {
    let recorded = state
        .auth
        .probes
        .get(&probe.attempt_id)
        .ok_or_else(|| conflict("Unknown verification probe"))?;
    if probe.task_id != lease.task_id
        || probe.writer_epoch != lease.epoch
        || recorded.epoch != lease.epoch
        || recorded.reservation != probe.reservation
        || recorded.context_key != probe.context_key
        || recorded.generation != probe.generation
        || recorded.started != probe.started
        || recorded.released
        || recorded.settled.is_some()
    {
        return Err(conflict(
            "Verification probe is stale or belongs to another writer",
        ));
    }
    Ok(())
}

/// A closed blocking reason, distinguishable from other conflicts by its caller.
fn block(reason: TaskAuthBlockV1) -> StoreError {
    StoreError::AuthRecoveryBlocked(reason)
}

/// Unstarted or started verification authority. Only the Store constructs it.
#[derive(Debug, Clone)]
pub struct TaskAuthProbe {
    task_id: String,
    writer_epoch: u64,
    attempt_id: String,
    context_key: String,
    generation: u64,
    suspension_id: String,
    reservation: TaskReservation,
    started: bool,
}

impl TaskAuthProbe {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub fn context_key(&self) -> &str {
        &self.context_key
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn reservation(&self) -> &TaskReservation {
        &self.reservation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAuthProbeSettlement {
    pub verified: bool,
    pub overrun: bool,
    pub outcome: TaskAuthProbeOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAuthClaim {
    /// The claim is durable. `replayed` means an earlier attempt already made it, so the
    /// caller finishes that continuation instead of claiming again.
    Claimed {
        claim_id: String,
        claim: TaskAuthResumeClaimV1,
        replayed: bool,
    },
    /// These still-required contexts are not verified under their current generation.
    Unverified { context_keys: Vec<String> },
}
