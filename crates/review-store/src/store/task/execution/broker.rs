//! Durable Broker operations under the original Task reservation. The receipt and cumulative
//! usage floor enter the same log transaction, including paid responses after authority loss.

use super::*;
use review_core::task::broker::*;
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::{
    BrokerFailureReasonV1 as Reason, BrokerLeaseV1, BrokerOperationOutcomeV1 as Outcome,
    BrokerOperationPolicyV1, BrokerOperationReceiptV2, Producer,
};
use review_graph::task::ReviewOperation;

#[derive(Debug, Clone)]
pub(super) struct RecordedBroker {
    id: String,
    binding: TaskBrokerBindingV1,
    receipts: Vec<(String, TaskBrokerOperationV1)>,
}

/// A host capability returned only after the exact binding is committed. Deserializing its
/// public evidence cannot construct this value or authorize an operation.
#[derive(Debug, Clone)]
pub struct BoundTaskBroker {
    id: String,
    binding: TaskBrokerBindingV1,
    writer: TaskLease,
    attempt: PreparedTaskAttempt,
}

impl BoundTaskBroker {
    pub fn binding(&self) -> &TaskBrokerBindingV1 {
        &self.binding
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskBrokerReceiptDisposition {
    Recorded,
    /// Paid usage is already durable, but the body must be withheld. A Broker may submit
    /// the corresponding revoked receipt again; that exact duplicate is idempotent.
    AuthorityRevoked,
}

fn consumes(receipt: &BrokerOperationReceiptV2) -> bool {
    matches!(receipt.outcome, Outcome::Succeeded | Outcome::Failed)
        || (receipt.outcome == Outcome::Revoked
            && (receipt.response_digest.is_some() || receipt.charged_usage.get() > 0))
}

fn terminates(receipt: &BrokerOperationReceiptV2) -> bool {
    matches!(
        receipt.failure_reason,
        Some(
            Reason::AuthorityRevoked
                | Reason::RequestTooLarge
                | Reason::QuotaExceeded
                | Reason::CredentialExposure
                | Reason::UsageOverrun
        )
    )
}

fn revoked(receipt: &BrokerOperationReceiptV2) -> BrokerOperationReceiptV2 {
    let mut result = receipt.clone();
    result.outcome = Outcome::Revoked;
    result.failure_reason = Some(Reason::AuthorityRevoked);
    result
}

fn check_receipt(
    binding: &RecordedBroker,
    receipt: &BrokerOperationReceiptV2,
) -> Result<u128, StoreError> {
    receipt.validate().map_err(conflict)?;
    let b = &binding.binding;
    if receipt.handle_id != b.handle_id
        || receipt.attempt_id != b.attempt_id
        || receipt.node != b.lease.node_id
        || receipt.lease_epoch != b.writer_epoch
        || binding.receipts.len().checked_add(1) != usize::try_from(receipt.ordinal).ok()
    {
        return Err(conflict(
            "Task Broker receipt differs from its handle, Attempt or dense ordinal",
        ));
    }
    let policy = b
        .operations
        .iter()
        .find(|p| p.name == receipt.operation)
        .ok_or_else(|| conflict("Task Broker operation has no captured policy"))?;
    if policy.destination != receipt.destination || policy.method != receipt.method {
        return Err(conflict(
            "Task Broker receipt changed its captured connector route",
        ));
    }
    let mut calls = 0_u32;
    let mut charged = 0_u64;
    let mut total = 0_u128;
    let mut terminal = false;
    for (_, prior) in &binding.receipts {
        let prior = &prior.receipt;
        total = total
            .checked_add(u128::from(prior.charged_usage.get()))
            .ok_or_else(|| conflict("Task Broker cumulative usage overflow"))?;
        terminal |= terminates(prior);
        if prior.operation == receipt.operation && consumes(prior) {
            calls = calls
                .checked_add(1)
                .ok_or_else(|| conflict("Task Broker call count overflow"))?;
            charged = charged
                .checked_add(prior.charged_usage.get().min(prior.reserved_usage))
                .ok_or_else(|| conflict("Task Broker policy usage overflow"))?;
        }
    }
    if terminal
        && !(receipt.outcome == Outcome::Revoked
            && receipt.failure_reason == Some(Reason::AuthorityRevoked)
            && !consumes(receipt)
            && receipt.response_digest.is_none()
            && receipt.response_bytes == 0
            && receipt.charged_usage.get() == 0)
    {
        return Err(conflict(
            "Task Broker operation follows terminal handle revocation",
        ));
    }
    let usage = charged.checked_add(receipt.reserved_usage);
    let exact = match (receipt.outcome, receipt.failure_reason, consumes(receipt)) {
        (Outcome::Refused, Some(Reason::RequestTooLarge), false) => {
            receipt.request_bytes > policy.max_request_bytes
        }
        (Outcome::Refused, Some(Reason::QuotaExceeded), false) => {
            receipt.request_bytes <= policy.max_request_bytes
                && (receipt.reserved_usage == 0
                    || calls >= policy.max_calls
                    || usage.is_none_or(|v| v > policy.max_usage))
        }
        (_, _, true) => {
            receipt.request_bytes <= policy.max_request_bytes
                && receipt.reserved_usage > 0
                && calls < policy.max_calls
                && usage.is_some_and(|v| v <= policy.max_usage)
                && match receipt.failure_reason {
                    None => receipt.response_bytes <= policy.max_response_bytes,
                    Some(Reason::ResponseTooLarge) => {
                        receipt.response_bytes > policy.max_response_bytes
                    }
                    Some(Reason::ConnectorFailed | Reason::AuthorityRevoked) => true,
                    Some(Reason::CredentialExposure) => {
                        receipt.charged_usage.get() <= receipt.reserved_usage
                    }
                    Some(Reason::UsageOverrun) => {
                        receipt.charged_usage.get() > receipt.reserved_usage
                    }
                    _ => false,
                }
        }
        (Outcome::Revoked, Some(Reason::AuthorityRevoked), false) => true,
        _ => false,
    };
    if !exact {
        return Err(conflict(
            "Task Broker receipt contradicts captured operation bounds",
        ));
    }
    total
        .checked_add(u128::from(receipt.charged_usage.get()))
        .ok_or_else(|| conflict("Task Broker cumulative usage overflow"))
}

fn broker_target(
    cas: &Cas,
    state: &TaskProjection,
    plan: &ExecutionPlanV1,
    node: &str,
) -> Result<TaskBrokerTargetV1, StoreError> {
    let resolved = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Task has no execution"))?
        .resolve_node(node)?;
    match &resolved.definition.operator {
        CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        }
        | CompiledOperator::Primitive {
            operator: TaskOperatorV1::Worker { slot } | TaskOperatorV1::Verify { slot },
            ..
        } => Ok(TaskBrokerTargetV1::Worker {
            slot: slot.clone(),
            invocation_policy_id: plan
                .bindings
                .get(slot)
                .ok_or_else(|| conflict("Task Broker slot has no effective binding"))?
                .invocation_policy_id
                .clone(),
        }),
        CompiledOperator::ProviderAdmissionBrokered {
            bindings,
            probe_policy_id,
        } => {
            let policy = provider_policy(cas, plan, probe_policy_id)?;
            if bindings.is_empty()
                || bindings.iter().any(|slot| {
                    plan.bindings
                        .get(slot)
                        .is_none_or(|binding| binding.execution != policy.execution)
                })
            {
                return Err(conflict(
                    "Task Broker probe changed its protected Provider execution",
                ));
            }
            Ok(TaskBrokerTargetV1::ProviderAdmission {
                probe_policy_id: probe_policy_id.clone(),
            })
        }
        _ => Err(conflict(
            "Task Broker requires captured Worker or Provider probe authority",
        )),
    }
}

fn provider_policy(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    id: &str,
) -> Result<review_core::task::provider::TaskProviderProbePolicyV1, StoreError> {
    use review_core::task::provider::*;
    let envelope = envelope(cas, id, TASK_PROVIDER_PROBE_POLICY_V1)?;
    let policy: TaskProviderProbePolicyV1 = serde_json::from_value(envelope.payload)?;
    policy.validate().map_err(conflict)?;
    if policy.authority_policy_id != plan.authority.policy_id
        || envelope.input_artifacts != policy.artifact_refs()
        || envelope.subject_snapshot_id.is_some()
        || !plan.dependencies.values().any(|dependency| {
            dependency.artifact_id == id && dependency.content_digest == envelope.content_id
        })
    {
        return Err(conflict(
            "Task Broker probe is not an exact captured plan dependency",
        ));
    }
    Ok(policy)
}

fn broker_lease(
    cas: &Cas,
    state: &TaskProjection,
    attempt: &PreparedTaskAttempt,
) -> Result<BrokerLeaseV1, StoreError> {
    let mut lease = BrokerLeaseV1 {
        campaign_id: task_run_id(&state.task_id)?,
        round_event_id: crate::store::derive_event_id(&task_run_id(&state.task_id)?, 0),
        node_id: attempt.node.clone(),
        attempt_id: attempt.id.clone(),
        lease_epoch: attempt.writer_epoch,
    };
    if let Some(input) = state
        .revision
        .inputs
        .values()
        .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
    {
        let round: LegacyReviewRoundV1 =
            payload(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
        let resolved = state
            .execution
            .as_ref()
            .expect("checked execution")
            .resolve_node(&attempt.node)?;
        match &resolved.definition.operator {
            CompiledOperator::ReviewDomain { review_node, .. } => {
                lease.node_id = resolved
                    .owned
                    .as_ref()
                    .and_then(|owner| owner.review_node.clone())
                    .unwrap_or_else(|| review_node.clone())
            }
            CompiledOperator::ProviderAdmissionBrokered { .. } => {}
            _ => {
                return Err(conflict(
                    "Captured Review Broker requires a Review Worker or Provider probe",
                ));
            }
        }
        lease.campaign_id = round.campaign_id;
        lease.round_event_id = round.round_event_id;
    }
    lease.validate().map_err(conflict)?;
    Ok(lease)
}

fn record_envelope(cas: &Cas, id: &str) -> Result<ArtifactEnvelope, StoreError> {
    let value = cas
        .get_artifact(id)
        .map_err(|e| StoreError::Artifact(e.to_string()))?;
    let (task_id, expected_refs) = match value.artifact_type.as_str() {
        TASK_BROKER_BINDING_V1 => {
            let binding: TaskBrokerBindingV1 = serde_json::from_value(value.payload.clone())?;
            binding.validate().map_err(conflict)?;
            (
                binding.task_id.clone(),
                binding
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
        }
        TASK_BROKER_OPERATION_V1 => {
            let operation: TaskBrokerOperationV1 = serde_json::from_value(value.payload.clone())?;
            operation.validate().map_err(conflict)?;
            // Require the binding type before following its references. A malformed chain
            // of operation artifacts cannot turn this reader into recursive traversal.
            envelope(cas, &operation.binding_id, TASK_BROKER_BINDING_V1)?;
            let binding: TaskBrokerBindingV1 =
                serde_json::from_value(record_envelope(cas, &operation.binding_id)?.payload)?;
            (
                binding.task_id,
                operation
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        }
        _ => return Err(conflict("Unsupported Task Broker record")),
    };
    if value.artifact_id != id
        || value.input_artifacts != expected_refs
        || value.subject_snapshot_id.is_some()
        || value.producer
            != (Producer::KernelOperation {
                run_id: task_run_id(&task_id)?,
                node_id: None,
                operation_id: "task-broker@1".into(),
            })
    {
        return Err(conflict(
            "Task Broker artifact differs from its typed authority",
        ));
    }
    for reference in expected_refs {
        cas.verify(&reference)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    Ok(value)
}

/// Decode exact typed Broker evidence for inspection. This grants no handle capability.
pub fn read_task_broker_record(cas: &Cas, id: &str) -> Result<ArtifactEnvelope, StoreError> {
    record_envelope(cas, id)
}

pub(in crate::store::task) fn validate_cached(
    cas: &Cas,
    state: &TaskProjection,
) -> Result<(), StoreError> {
    if let Some(execution) = &state.execution {
        for broker in execution.brokers.values() {
            if record_envelope(cas, &broker.id)?.payload != serde_json::to_value(&broker.binding)? {
                return Err(conflict("Cached Task Broker binding changed identity"));
            }
            for (id, operation) in &broker.receipts {
                if record_envelope(cas, id)?.payload != serde_json::to_value(operation)? {
                    return Err(conflict("Cached Task Broker receipt changed identity"));
                }
            }
        }
    }
    Ok(())
}

impl EventStore {
    /// Derive the compatibility lease from current Task authority. The caller may construct
    /// the local Broker, but no operation is current until bind_task_broker commits its handle.
    pub fn task_broker_lease(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        authority: &dyn TaskAuthority,
    ) -> Result<BrokerLeaseV1, StoreError> {
        self.check_task_attempt_current(cas, lease, attempt, authority)?;
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let plan = plan(
            cas,
            state
                .plan_id
                .as_ref()
                .ok_or_else(|| conflict("Task has no plan"))?,
            &state,
        )?;
        broker_target(cas, &state, &plan, attempt.node())?;
        broker_lease(cas, &state, attempt)
    }

    pub fn bind_task_broker(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        handle_id: &str,
        operations: &[BrokerOperationPolicyV1],
        authority: &dyn TaskAuthority,
    ) -> Result<BoundTaskBroker, StoreError> {
        self.check_task_attempt_current(cas, lease, attempt, authority)?;
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
        let target = broker_target(cas, &state, &plan, attempt.node())?;
        let recorded = &state
            .execution
            .as_ref()
            .expect("checked execution")
            .attempts[attempt.id()];
        let binding = TaskBrokerBindingV1 {
            task_id: state.task_id.clone(),
            task_revision_id: state.revision_id.clone(),
            plan_id: recorded.plan_id.clone(),
            invocation_id: recorded.invocation_id.clone(),
            context_id: attempt.context_id.clone(),
            attempt_id: attempt.id.clone(),
            reservation_id: attempt.reservation.id.clone(),
            writer: lease.writer.clone(),
            writer_epoch: lease.epoch,
            node: attempt.node.clone(),
            target,
            lease: broker_lease(cas, &state, attempt)?,
            handle_id: handle_id.into(),
            operations: operations.to_vec(),
        };
        binding.validate().map_err(conflict)?;
        authority
            .validate_broker_binding(cas, &state.revision, &plan, &binding)
            .map_err(conflict)?;
        // Trusted callbacks may inspect external authorization. Recheck its original prefix
        // and the exact Attempt before creating executable handle authority.
        self.check_task_attempt_current(cas, lease, attempt, authority)?;
        let id = self.put_task_broker_record(
            cas,
            &state.task_id,
            TASK_BROKER_BINDING_V1,
            binding.artifact_refs(),
            serde_json::to_value(&binding)?,
        )?;
        self.append_task_broker_record(
            cas,
            &state,
            &id,
            Some(Self::broker_deadline(&state, attempt)),
            super::super::review_round::ReviewRoundFence::capture(cas, &state.revision)?,
        )?;
        Ok(BoundTaskBroker {
            id,
            binding,
            writer: lease.clone(),
            attempt: attempt.clone(),
        })
    }

    fn checked_bound_broker(
        &self,
        cas: &Cas,
        bound: &BoundTaskBroker,
    ) -> Result<TaskProjection, StoreError> {
        let state = self
            .task_projection(cas, &bound.binding.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let recorded = state
            .execution
            .as_ref()
            .and_then(|e| e.brokers.get(&bound.binding.attempt_id))
            .ok_or_else(|| conflict("Task Broker handle was never bound"))?;
        if recorded.id != bound.id || recorded.binding != bound.binding {
            return Err(conflict(
                "Task Broker capability differs from its durable binding",
            ));
        }
        Ok(state)
    }

    pub fn check_task_broker_current(
        &self,
        cas: &Cas,
        bound: &BoundTaskBroker,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let state = self.checked_bound_broker(cas, bound)?;
        if state.execution.as_ref().expect("bound execution").brokers[&bound.binding.attempt_id]
            .receipts
            .iter()
            .any(|(_, record)| terminates(&record.receipt))
        {
            return Err(conflict("Task Broker handle is revoked"));
        }
        self.check_task_attempt_current(cas, &bound.writer, &bound.attempt, authority)?;
        let (_, plan) = self.checked_task_dispatch(cas, &bound.writer, authority)?;
        authority
            .validate_broker_binding(cas, &state.revision, &plan, &bound.binding)
            .map_err(conflict)
    }

    /// A paid response is recording authority under its original binding even after lease,
    /// approval, plan, Round or Task completion. Currentness only permits releasing its body.
    /// A stale response is converted to Revoked before the receipt and usage commit together.
    pub fn record_task_broker_receipt(
        &mut self,
        cas: &Cas,
        bound: &BoundTaskBroker,
        receipt: &BrokerOperationReceiptV2,
        authority: &dyn TaskAuthority,
    ) -> Result<TaskBrokerReceiptDisposition, StoreError> {
        receipt.validate().map_err(conflict)?;
        // A competing writer invalidates the entire candidate append. Re-read its prefix so
        // a late paid response can be retained under the resulting revocation state.
        for _ in 0..8 {
            let state = self.checked_bound_broker(cas, bound)?;
            let recorded = &state.execution.as_ref().expect("bound execution").brokers
                [&bound.binding.attempt_id];
            if let Some((_, prior)) = receipt
                .ordinal
                .checked_sub(1)
                .and_then(|n| recorded.receipts.get(n as usize))
            {
                if prior.receipt == *receipt {
                    return Ok(TaskBrokerReceiptDisposition::Recorded);
                }
                if prior.receipt == revoked(receipt) {
                    return Ok(TaskBrokerReceiptDisposition::AuthorityRevoked);
                }
                return Err(conflict(
                    "Conflicting Task Broker receipt at the same ordinal",
                ));
            }
            // Validate the submitted evidence before changing its outcome. Revocation never
            // excuses an impossible original policy result or fabricated connector route.
            check_receipt(recorded, receipt)?;
            let current = self
                .check_task_broker_current(cas, bound, authority)
                .is_ok();
            let stored = if current {
                receipt.clone()
            } else {
                revoked(receipt)
            };
            check_receipt(recorded, &stored)?;
            let operation = TaskBrokerOperationV1 {
                binding_id: bound.id.clone(),
                receipt: stored,
            };
            let id = self.put_task_broker_record(
                cas,
                &state.task_id,
                TASK_BROKER_OPERATION_V1,
                operation.artifact_refs(),
                serde_json::to_value(&operation)?,
            )?;
            let releasing = operation.receipt.outcome != Outcome::Revoked;
            let result = self.append_task_broker_record(
                cas,
                &state,
                &id,
                releasing.then(|| Self::broker_deadline(&state, &bound.attempt)),
                if releasing {
                    super::super::review_round::ReviewRoundFence::capture(cas, &state.revision)?
                } else {
                    None
                },
            );
            match result {
                Ok(_) => {
                    return Ok(if operation.receipt == *receipt {
                        TaskBrokerReceiptDisposition::Recorded
                    } else {
                        TaskBrokerReceiptDisposition::AuthorityRevoked
                    });
                }
                Err(error @ StoreError::Conflict(_)) => {
                    let latest = self.checked_bound_broker(cas, bound)?;
                    if latest.next_sequence == state.next_sequence
                        && (!releasing
                            || self
                                .check_task_broker_current(cas, bound, authority)
                                .is_ok())
                    {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(conflict(
            "Task Broker receipt could not acquire a stable accounting prefix",
        ))
    }

    fn broker_deadline(state: &TaskProjection, attempt: &PreparedTaskAttempt) -> u64 {
        state
            .lease_until
            .min(attempt.reservation.deadline_unix_ms)
            .min(
                state
                    .plan_id
                    .as_ref()
                    .and_then(|id| state.decisions.get(id))
                    .map_or(u64::MAX, |decision| decision.valid_until),
            )
    }

    fn put_task_broker_record(
        &self,
        cas: &Cas,
        task_id: &str,
        kind: &str,
        refs: Vec<&str>,
        value: serde_json::Value,
    ) -> Result<String, StoreError> {
        cas.put_artifact(
            kind,
            Producer::KernelOperation {
                run_id: task_run_id(task_id)?,
                node_id: None,
                operation_id: "task-broker@1".into(),
            },
            refs.into_iter().map(str::to_owned).collect(),
            None,
            value,
        )
        .map(|(id, _)| id)
        .map_err(|e| StoreError::Artifact(e.to_string()))
    }

    fn append_task_broker_record(
        &mut self,
        cas: &Cas,
        state: &TaskProjection,
        record_id: &str,
        valid_until: Option<u64>,
        review_round: Option<super::super::review_round::ReviewRoundFence>,
    ) -> Result<RunEvent, StoreError> {
        let transition = TaskBrokerTransitionV1 {
            now_unix_ms: now()?,
            record_id: record_id.into(),
        };
        let value = serde_json::to_value(&transition)?;
        let run_id = task_run_id(&state.task_id)?;
        let first = state.next_sequence;
        let event = NewEvent::new(EventType::TaskBrokerTransitionV1, value.clone())
            .referencing(vec![record_id.into()]);
        let mut next = state.clone();
        apply_event(
            cas,
            &mut next,
            &RunEvent {
                run_id: run_id.clone(),
                event_id: crate::store::derive_event_id(&run_id, first as i64),
                sequence: first,
                event_type: event.event_type,
                occurred_at: event.occurred_at.clone(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: None,
                artifact_refs: event.artifact_refs.clone(),
                payload: value.clone(),
            },
        )?;
        let permit = super::super::WritePermit {
            run_id: run_id.clone(),
            first,
            payloads: vec![value],
            event_type: EventType::TaskBrokerTransitionV1,
            valid_until,
            review_round,
            review_prefix: None,
        };
        self.append_batch_inner(&run_id, cas, &[event], Some(&permit), None)?
            .pop()
            .ok_or_else(|| conflict("Task Broker append produced no event"))
    }
}

pub(in crate::store::task) fn apply_event(
    cas: &Cas,
    state: &mut TaskProjection,
    event: &RunEvent,
) -> Result<(), StoreError> {
    let transition: TaskBrokerTransitionV1 = serde_json::from_value(event.payload.clone())?;
    transition.validate().map_err(conflict)?;
    if event.sequence != state.next_sequence
        || transition.now_unix_ms < state.last_time
        || event.artifact_refs != vec![transition.record_id.clone()]
        || event.node_id.is_some()
        || event.attempt_id.is_some()
        || event.causation_id.is_some()
        || event.correlation_id.is_some()
    {
        return Err(conflict(
            "Task Broker event differs from its exact log position or typed references",
        ));
    }
    let envelope = record_envelope(cas, &transition.record_id)?;
    match envelope.artifact_type.as_str() {
        TASK_BROKER_BINDING_V1 => {
            let binding: TaskBrokerBindingV1 = serde_json::from_value(envelope.payload)?;
            state.check_lease(&TaskTransitionV1 {
                writer: binding.writer.clone(),
                epoch: binding.writer_epoch,
                now_unix_ms: transition.now_unix_ms,
                change: TaskChangeV1::Resumed {},
            })?;
            state.check_approval(cas, transition.now_unix_ms)?;
            let execution = state
                .execution
                .as_ref()
                .ok_or_else(|| conflict("Task Broker binding has no execution"))?;
            let attempt = execution
                .attempts
                .get(&binding.attempt_id)
                .ok_or_else(|| conflict("Task Broker binding has no original Attempt"))?;
            let prepared = PreparedTaskAttempt {
                task_id: state.task_id.clone(),
                writer_epoch: attempt.prepared_epoch,
                id: binding.attempt_id.clone(),
                node: attempt.reservation.node.clone(),
                reservation: attempt.reservation.clone(),
                context_id: attempt
                    .context_id
                    .clone()
                    .ok_or_else(|| conflict("Task Broker Attempt has no context"))?,
            };
            let plan = plan(cas, &binding.plan_id, state)?;
            if state.phase != (TaskPhaseV1::Running {})
                || !state.admitted
                || execution.budget.breached()
                || binding.task_id != state.task_id
                || binding.task_revision_id != state.revision_id
                || state.plan_id.as_ref() != Some(&binding.plan_id)
                || attempt.plan_id != binding.plan_id
                || binding.invocation_id != attempt.invocation_id
                || binding.context_id != prepared.context_id
                || binding.node != prepared.node
                || binding.reservation_id != attempt.reservation.id
                || binding.writer_epoch != attempt.prepared_epoch
                || !attempt.started
                || attempt.released
                || attempt.settlement.is_some()
                || transition.now_unix_ms >= attempt.reservation.deadline_unix_ms
                || binding.target != broker_target(cas, state, &plan, &binding.node)?
                || match &binding.target {
                    TaskBrokerTargetV1::ProviderAdmission { probe_policy_id } => {
                        provider_policy(cas, &plan, probe_policy_id)?.operations
                            != binding.operations
                    }
                    TaskBrokerTargetV1::Worker { .. } => false,
                }
                || binding.lease != broker_lease(cas, state, &prepared)?
                || review_core::broker_authority_usage(&binding.operations).map_err(conflict)?
                    > attempt.reservation.tokens
                || execution.brokers.contains_key(&binding.attempt_id)
            {
                return Err(conflict(
                    "Task Broker binding differs from current captured Attempt authority",
                ));
            }
            state
                .execution
                .as_mut()
                .expect("checked execution")
                .brokers
                .insert(
                    binding.attempt_id.clone(),
                    RecordedBroker {
                        id: transition.record_id,
                        binding,
                        receipts: Vec::new(),
                    },
                );
        }
        TASK_BROKER_OPERATION_V1 => {
            let operation: TaskBrokerOperationV1 = serde_json::from_value(envelope.payload)?;
            if operation.receipt.outcome != Outcome::Revoked {
                let execution = state
                    .execution
                    .as_ref()
                    .ok_or_else(|| conflict("Task Broker operation has no execution"))?;
                let broker = execution
                    .brokers
                    .get(&operation.receipt.attempt_id)
                    .ok_or_else(|| conflict("Task Broker operation has no binding"))?;
                let attempt = &execution.attempts[&broker.binding.attempt_id];
                state.check_lease(&TaskTransitionV1 {
                    writer: broker.binding.writer.clone(),
                    epoch: broker.binding.writer_epoch,
                    now_unix_ms: transition.now_unix_ms,
                    change: TaskChangeV1::Resumed {},
                })?;
                state.check_approval(cas, transition.now_unix_ms)?;
                if !state.admitted
                    || state.phase != (TaskPhaseV1::Running {})
                    || state.plan_id.as_ref() != Some(&broker.binding.plan_id)
                    || state.revision_id != broker.binding.task_revision_id
                    || !attempt.started
                    || attempt.released
                    || attempt.settlement.is_some()
                    || execution.budget.breached()
                    || transition.now_unix_ms >= attempt.reservation.deadline_unix_ms
                {
                    return Err(conflict(
                        "Task Broker response has lost current execution authority",
                    ));
                }
            }
            let execution = state
                .execution
                .as_mut()
                .ok_or_else(|| conflict("Task Broker operation has no execution"))?;
            let broker = execution
                .brokers
                .get_mut(&operation.receipt.attempt_id)
                .ok_or_else(|| conflict("Task Broker operation has no bound handle"))?;
            if broker.id != operation.binding_id {
                return Err(conflict("Task Broker operation changed binding"));
            }
            let total = check_receipt(broker, &operation.receipt)?;
            let attempt = &execution.attempts[&broker.binding.attempt_id];
            execution
                .budget
                .observe_charge_exact(&attempt.reservation.id, total)
                .map_err(conflict)?;
            execution
                .ledger
                .charge_exact(&AttemptId(broker.binding.attempt_id.clone()), total)
                .map_err(conflict)?;
            broker.receipts.push((transition.record_id, operation));
        }
        _ => unreachable!(),
    }
    state.last_time = transition.now_unix_ms;
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or_else(|| conflict("Task event sequence overflow"))?;
    Ok(())
}
