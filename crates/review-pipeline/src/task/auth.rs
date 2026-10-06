//! Runtime auth failure suspension (ADR-0141). A typed native authentication failure stops the
//! node's retry loop and every later dispatch in this run; in-flight siblings settle normally.
//! Once the scheduler returns, the Task is durably suspended before any result is assembled, so
//! a failed login never terminalizes recoverable work. Quota, model, network and unknown
//! failures never reach this path.

use std::collections::BTreeMap;

use review_core::task::auth_recovery::{
    TaskAuthBindingV1, TaskAuthFailureV1, TaskAuthParticipantV1,
};
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::plan::ExecutionPlanV1;
use review_graph::task::{CompiledOperator, ReviewOperation};
use review_store::StoreError;
use review_store::store::task::auth_recovery::auth_context_key;

use super::{TaskRuntime, envelope};

/// What this run observed. Nothing here is durable until [`TaskRuntime::suspend_for_auth`].
#[derive(Debug, Default)]
pub(super) struct AuthStop {
    failures: Vec<(String, String, TaskAuthFailureV1)>,
    /// A newer generation invalidated this Task's resume claim at a dispatch boundary.
    invalidated: bool,
}

impl AuthStop {
    fn stopped(&self) -> bool {
        self.invalidated || !self.failures.is_empty()
    }
}

impl TaskRuntime<'_, '_> {
    /// The opaque requester and coordinator this Task's outcome belongs to. They are recorded
    /// with a suspension, never inferred from whoever later logs in.
    pub fn with_auth_participant(mut self, participant: TaskAuthParticipantV1) -> Self {
        self.auth_participant = Some(participant);
        self
    }

    /// Refuse new work, paid or not, once this run saw an auth failure. Nothing is reserved.
    pub(super) fn auth_halted(&self) -> Result<(), String> {
        if self.auth_stop.lock().expect("Task auth stop").stopped() {
            return Err(
                "Provider authentication failed; dispatch is suspended for recovery".into(),
            );
        }
        Ok(())
    }

    pub(super) fn note_auth_failure(&self, attempt: &str, node: &str, kind: TaskAuthFailureV1) {
        self.auth_stop
            .lock()
            .expect("Task auth stop")
            .failures
            .push((attempt.into(), node.into(), kind));
    }

    /// A Store refusal at a dispatch boundary caused by a newer recovery generation.
    pub(super) fn note_store_error(&self, error: &StoreError) {
        if matches!(error, StoreError::AuthRecoveryInvalidated(_)) {
            self.auth_stop.lock().expect("Task auth stop").invalidated = true;
        }
    }

    /// Whether one in-run retry may follow this failure: only a contended refresh, and only
    /// within the node's own captured Attempt allowance.
    pub(super) fn auth_retry_in_run(kind: TaskAuthFailureV1, next: u32, attempts: u32) -> bool {
        kind.transient() && next < attempts
    }

    /// The recovery identities of every Model binding the node dispatches with.
    fn auth_bindings(
        &self,
        plan: &ExecutionPlanV1,
        node: &str,
    ) -> Result<Vec<TaskAuthBindingV1>, String> {
        let resolved = self.resolve_node(node)?;
        let slots: Vec<String> = match &resolved.definition.operator {
            CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::Worker { slot }
                    | TaskOperatorV1::Verify { slot }
                    | TaskOperatorV1::FixVerify { slot },
                ..
            }
            | CompiledOperator::ReviewDomain {
                operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
                ..
            } => vec![slot.clone()],
            CompiledOperator::ProviderAdmission { bindings } => bindings.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let mut found = Vec::new();
        for slot in slots {
            if let Some(binding) = plan
                .bindings
                .get(&slot)
                .and_then(TaskAuthBindingV1::of_plan_binding)
                && !found.contains(&binding)
            {
                found.push(binding);
            }
        }
        Ok(found)
    }

    /// Durably suspend this Task when the run stopped on authentication. Returns whether it
    /// did; a failure on a node with no Model binding stays an ordinary failure.
    pub(super) fn suspend_for_auth(&self) -> Result<bool, String> {
        let stop = std::mem::take(&mut *self.auth_stop.lock().expect("Task auth stop"));
        if !stop.stopped() {
            return Ok(false);
        }
        let plan: ExecutionPlanV1 =
            serde_json::from_value(envelope(self.cas, &self.plan_id)?.payload)
                .map_err(|e| e.to_string())?;
        let mut contexts: BTreeMap<String, (TaskAuthBindingV1, Option<TaskAuthFailureV1>)> =
            BTreeMap::new();
        let mut failures = Vec::new();
        for (attempt, node, kind) in &stop.failures {
            let bindings = self.auth_bindings(&plan, node)?;
            if bindings.is_empty() {
                continue;
            }
            failures.push((attempt.clone(), *kind));
            for binding in bindings {
                let key = auth_context_key(&binding.context).map_err(|e| e.to_string())?;
                let entry = contexts.entry(key).or_insert((binding, None));
                entry.1 = entry.1.or(Some(*kind));
            }
        }
        if stop.invalidated
            && let Some((_, claim)) = self.projection()?.auth.active_claim().cloned()
        {
            for binding in plan
                .bindings
                .values()
                .filter_map(TaskAuthBindingV1::of_plan_binding)
            {
                let key = auth_context_key(&binding.context).map_err(|e| e.to_string())?;
                if claim.contexts.contains_key(&key) {
                    contexts.entry(key).or_insert((binding, None));
                }
            }
        }
        if contexts.is_empty() {
            return Ok(false);
        }
        let contexts: Vec<_> = contexts.into_values().collect();
        self.store
            .lock()
            .expect("Task Store")
            .suspend_task_for_provider_auth(
                self.cas,
                &self.lease,
                &failures,
                &contexts,
                self.auth_participant.clone(),
            )
            .map_err(|e| e.to_string())?;
        Ok(true)
    }
}
