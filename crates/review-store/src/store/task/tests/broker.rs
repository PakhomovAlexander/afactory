//! Broker operations are observations inside one Task Attempt, including late paid work.
mod provider;
use super::*;
use execution::PreparedTaskAttempt;
use execution::broker::{
    BoundTaskBroker, TaskBrokerReceiptDisposition, apply_event, read_task_broker_record,
};
use review_core::task::{broker::*, execution::*, plan::*};
use review_core::{
    BrokerFailureReasonV1 as Reason, BrokerOperationOutcomeV1 as Outcome, BrokerOperationPolicyV1,
    BrokerOperationReceiptV2,
};
use review_graph::task::{CompiledOperator, CompiledTask, ReviewOperation};

const NODE: &str = "root.nodes.write";
const SLOT: &str = "root.slots.author";
const HANDLE: &str = "hhhhhhhhhhhhhhhhhhhhhhhhhh";

/// The domain authority rereads the captured invocation policy. Caller-supplied operation
/// names and limits never become authority merely because their individual shapes validate.
struct BrokerAuthority;

impl TaskAuthority for BrokerAuthority {
    fn validate_plan(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        Ok(plan.generated_origins.clone())
    }

    fn authorize_decision(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("Broker fixture does not create developer grants".into())
    }

    fn authorization_current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        Ok(())
    }

    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("Broker receipt is not an accepted Task result".into())
    }

    fn validate_broker_binding(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &TaskBrokerBindingV1,
    ) -> Result<(), String> {
        let effective = plan.bindings.get(SLOT).ok_or("Missing captured slot")?;
        let captured: Vec<BrokerOperationPolicyV1> = serde_json::from_value(
            cas.get_json(&effective.invocation_policy_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if binding.target
            != (TaskBrokerTargetV1::Worker {
                slot: SLOT.into(),
                invocation_policy_id: effective.invocation_policy_id.clone(),
            })
            || binding.operations != captured
        {
            return Err("Broker operations differ from the captured invocation policy".into());
        }
        Ok(())
    }
}

fn policy() -> BrokerOperationPolicyV1 {
    BrokerOperationPolicyV1 {
        name: "generate".into(),
        destination: "fixture-provider".into(),
        method: "generate".into(),
        max_request_bytes: 16,
        max_response_bytes: 16,
        max_calls: 2,
        max_usage: 10,
    }
}

fn put<T: serde::Serialize>(cas: &Cas, kind: &str, value: &T) -> String {
    cas.put_artifact(
        kind,
        producer(),
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

fn install(mut f: Fixture, operations: &[BrokerOperationPolicyV1], review: bool) -> Fixture {
    let policy_id = f.cas.put_json(&json!(operations)).unwrap();
    let package_id = f.cas.put_json(&json!({"fixture_worker":"broker"})).unwrap();
    f.plan.bindings.insert(
        SLOT.into(),
        EffectiveWorkerBindingV1 {
            package_digest: package_id.clone(),
            package_artifact_id: package_id,
            execution: WorkerExecutionV1::Command {},
            invocation_policy_id: policy_id,
        },
    );
    // Capture a generous original wall allowance before opening the Task. These tests
    // exercise many durable validation failures, not timing-dependent deadline behavior.
    f.revision.limits.verification.wall_ms = 60_000;
    f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.limits = f.revision.limits.clone();
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.allowances.get_mut(NODE).unwrap().wall_ms_per_attempt = 60_000;
    if review {
        graph.nodes.get_mut(NODE).unwrap().operator = CompiledOperator::ReviewDomain {
            review_node: "reviewer".into(),
            operation: ReviewOperation::Reviewer { slot: SLOT.into() },
        };
    }
    f.plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@1", &graph);
    f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
    f
}

fn fixture(operations: &[BrokerOperationPolicyV1]) -> Fixture {
    install(
        Fixture::new(false).with_execution_graph(),
        operations,
        false,
    )
}

fn prepare(f: &mut Fixture) -> (TaskLease, PreparedTaskAttempt) {
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context_id = f.cas.put_json(&json!({"broker_context":true})).unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, NODE, &context_id, &f.authority)
        .unwrap();
    (lease, attempt)
}

fn start(f: &mut Fixture) -> (TaskLease, PreparedTaskAttempt, BoundTaskBroker) {
    let (lease, attempt) = prepare(f);
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let operations = serde_json::from_value::<Vec<BrokerOperationPolicyV1>>(
        f.cas
            .get_json(&f.plan.bindings[SLOT].invocation_policy_id)
            .unwrap(),
    )
    .unwrap();
    let bound = f
        .store
        .bind_task_broker(
            &f.cas,
            &lease,
            &attempt,
            HANDLE,
            &operations,
            &BrokerAuthority,
        )
        .unwrap();
    (lease, attempt, bound)
}

fn receipt(bound: &BoundTaskBroker, ordinal: u32, charge: u64) -> BrokerOperationReceiptV2 {
    let binding = bound.binding();
    let operation = &binding.operations[0];
    BrokerOperationReceiptV2 {
        handle_id: binding.handle_id.clone(),
        node: binding.lease.node_id.clone(),
        attempt_id: binding.attempt_id.clone(),
        lease_epoch: binding.writer_epoch,
        operation: operation.name.clone(),
        destination: operation.destination.clone(),
        method: operation.method.clone(),
        ordinal,
        outcome: Outcome::Succeeded,
        failure_reason: None,
        request_digest: format!("sha256:{}", "a".repeat(64)),
        response_digest: Some(format!("sha256:{}", "b".repeat(64))),
        request_bytes: 2,
        response_bytes: 2,
        reserved_usage: charge,
        charged_usage: charge.into(),
    }
}

fn record(f: &mut Fixture, bound: &BoundTaskBroker, receipt: &BrokerOperationReceiptV2) {
    assert_eq!(
        f.store
            .record_task_broker_receipt(&f.cas, bound, receipt, &BrokerAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::Recorded
    );
}

fn rejected(f: &mut Fixture, bound: &BoundTaskBroker, receipt: &BrokerOperationReceiptV2) {
    let before = f.state();
    let budget = before.execution.as_ref().unwrap().budget.committed_tokens();
    let error = f
        .store
        .record_task_broker_receipt(&f.cas, bound, receipt, &BrokerAuthority)
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict(_)), "{error}");
    let after = f.state();
    assert_eq!(after.next_sequence, before.next_sequence);
    assert_eq!(after.execution.unwrap().budget.committed_tokens(), budget);
}

fn broker_records(f: &Fixture, kind: &str) -> Vec<ArtifactEnvelope> {
    f.store
        .replay(&task_run_id(&f.revision.task_id).unwrap())
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == EventType::TaskBrokerTransitionV1)
        .map(|e| {
            let transition: TaskBrokerTransitionV1 = serde_json::from_value(e.payload).unwrap();
            read_task_broker_record(&f.cas, &transition.record_id).unwrap()
        })
        .filter(|envelope| envelope.artifact_type == kind)
        .collect()
}

fn operations(f: &Fixture) -> Vec<(ArtifactEnvelope, TaskBrokerOperationV1)> {
    broker_records(f, TASK_BROKER_OPERATION_V1)
        .into_iter()
        .map(|envelope| {
            let operation = serde_json::from_value(envelope.payload.clone()).unwrap();
            (envelope, operation)
        })
        .collect()
}

/// The committed binding record is the only public handle on the binding artifact.
fn binding_id(f: &Fixture) -> String {
    let [binding] = <[ArtifactEnvelope; 1]>::try_from(broker_records(f, TASK_BROKER_BINDING_V1))
        .expect("one committed Broker binding");
    binding.artifact_id
}

fn settle(f: &mut Fixture, lease: &TaskLease, attempt: &PreparedTaskAttempt, charge: u128) {
    let diagnostic_id = f.cas.put_json(&json!({"failure":"fixture ended"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
}

#[test]
fn binding_requires_started_original_attempt_and_captured_slot_policy() {
    let policies = vec![policy()];
    let mut f = fixture(&policies);
    let (lease, attempt) = prepare(&mut f);
    let before = f.state().next_sequence;
    assert!(
        f.store
            .task_broker_lease(&f.cas, &lease, &attempt, &BrokerAuthority)
            .is_err()
    );
    assert!(
        f.store
            .bind_task_broker(
                &f.cas,
                &lease,
                &attempt,
                HANDLE,
                &policies,
                &BrokerAuthority
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let before = f.state();
    // The default domain authority refuses Broker capability even for a started Attempt.
    assert!(
        f.store
            .bind_task_broker(&f.cas, &lease, &attempt, HANDLE, &policies, &f.authority)
            .is_err()
    );
    let mut changed = policies.clone();
    changed[0].method = "other".into();
    assert!(
        f.store
            .bind_task_broker(&f.cas, &lease, &attempt, HANDLE, &changed, &BrokerAuthority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before.next_sequence);
    let bound = f
        .store
        .bind_task_broker(
            &f.cas,
            &lease,
            &attempt,
            HANDLE,
            &policies,
            &BrokerAuthority,
        )
        .unwrap();
    let binding = bound.binding();
    assert_eq!(binding.task_revision_id, f.revision_id);
    assert_eq!(binding.plan_id, f.plan_id);
    assert_eq!(binding.context_id, attempt.context_id());
    assert_eq!(binding.attempt_id, attempt.id());
    assert_eq!(binding.reservation_id, attempt.reservation().id);
    assert_eq!(
        binding.target,
        TaskBrokerTargetV1::Worker {
            slot: SLOT.into(),
            invocation_policy_id: f.plan.bindings[SLOT].invocation_policy_id.clone(),
        }
    );
    assert_eq!(
        binding.lease,
        f.store
            .task_broker_lease(&f.cas, &lease, &attempt, &BrokerAuthority)
            .unwrap()
    );
    assert_eq!(
        binding.invocation_id,
        before.execution.as_ref().unwrap().invocations[NODE].0
    );
    let event = f
        .store
        .replay(&task_run_id(&f.revision.task_id).unwrap())
        .unwrap()
        .pop()
        .unwrap();
    let alternative = f
        .cas
        .put_json(&json!({"other":"captured identity"}))
        .unwrap();
    let mut variants = Vec::new();
    for field in ["task_revision_id", "plan_id", "invocation_id", "context_id"] {
        let mut value = serde_json::to_value(binding).unwrap();
        value[field] = json!(alternative);
        variants.push((
            field,
            serde_json::from_value::<TaskBrokerBindingV1>(value).unwrap(),
        ));
    }
    let mut wrong = binding.clone();
    wrong.reservation_id = "reservation:999".into();
    variants.push(("reservation", wrong));
    let mut wrong = binding.clone();
    wrong.target = TaskBrokerTargetV1::Worker {
        slot: "root.slots.other".into(),
        invocation_policy_id: f.plan.bindings[SLOT].invocation_policy_id.clone(),
    };
    variants.push(("slot", wrong));
    let mut wrong = binding.clone();
    wrong.target = TaskBrokerTargetV1::Worker {
        slot: SLOT.into(),
        invocation_policy_id: alternative.clone(),
    };
    variants.push(("invocation policy", wrong));
    let mut wrong = binding.clone();
    wrong.target = TaskBrokerTargetV1::ProviderAdmission {
        probe_policy_id: alternative,
    };
    variants.push(("Provider target on Worker Attempt", wrong));
    let mut wrong = binding.clone();
    wrong.node = "root.nodes.other".into();
    variants.push(("node", wrong));
    let mut wrong = binding.clone();
    wrong.writer = "writer-2".into();
    variants.push(("writer", wrong));
    let mut wrong = binding.clone();
    wrong.writer_epoch += 1;
    wrong.lease.lease_epoch += 1;
    variants.push(("epoch", wrong));
    let mut wrong = binding.clone();
    wrong.lease.round_event_id = "z".repeat(26);
    variants.push(("round", wrong));
    let mut wrong = binding.clone();
    wrong.attempt_id = "z".repeat(26);
    wrong.lease.attempt_id = wrong.attempt_id.clone();
    variants.push(("attempt", wrong));
    for (field, wrong) in variants {
        let record_id = f
            .cas
            .put_artifact(
                TASK_BROKER_BINDING_V1,
                Producer::KernelOperation {
                    run_id: event.run_id.clone(),
                    node_id: None,
                    operation_id: "task-broker@1".into(),
                },
                wrong
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(wrong).unwrap(),
            )
            .unwrap()
            .0;
        let mut forged = event.clone();
        forged.artifact_refs = vec![record_id.clone()];
        forged.payload["record_id"] = json!(record_id);
        assert!(
            apply_event(&f.cas, &mut before.clone(), &forged).is_err(),
            "accepted forged {field}"
        );
    }
    let sequence = f.state().next_sequence;
    let forged = NewEvent::new(EventType::TaskBrokerTransitionV1, event.payload)
        .referencing(event.artifact_refs);
    let error = f.store.append(&event.run_id, &f.cas, forged).unwrap_err();
    assert!(
        error.to_string().contains("trusted Task entry point"),
        "{error}"
    );
    assert_eq!(f.state().next_sequence, sequence);
    assert!(
        f.store
            .bind_task_broker(
                &f.cas,
                &lease,
                &attempt,
                HANDLE,
                &policies,
                &BrokerAuthority
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, sequence);
}

#[test]
fn binding_cannot_expand_original_allowance_or_borrow_another_slot() {
    for missing_slot in [false, true] {
        let mut operation = policy();
        if !missing_slot {
            operation.max_usage = 11;
        }
        let mut f = fixture(&[operation.clone()]);
        if missing_slot {
            let binding = f.plan.bindings.remove(SLOT).unwrap();
            f.plan.bindings.insert("root.slots.other".into(), binding);
            f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
        }
        let (lease, attempt) = prepare(&mut f);
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let sequence = f.state().next_sequence;
        assert!(
            f.store
                .bind_task_broker(
                    &f.cas,
                    &lease,
                    &attempt,
                    HANDLE,
                    &[operation],
                    &BrokerAuthority
                )
                .is_err()
        );
        let state = f.state();
        assert_eq!(state.next_sequence, sequence);
        assert_eq!(state.execution.unwrap().budget.reserved_tokens(), 10);
    }
}

#[test]
fn receipts_enforce_dense_ordinals_routes_body_bounds_and_real_quota_refusals() {
    let mut f = fixture(&[policy()]);
    let (_, _, bound) = start(&mut f);
    let first = receipt(&bound, 1, 7);
    let mut variants = Vec::new();
    let mut wrong = first.clone();
    wrong.ordinal = 2;
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.handle_id = "z".repeat(26);
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.attempt_id = "z".repeat(26);
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.lease_epoch += 1;
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.node = "other".into();
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.operation = "other".into();
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.destination = "other-provider".into();
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.method = "other".into();
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.request_bytes = 17;
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.response_bytes = 17;
    variants.push(wrong);
    let mut wrong = first.clone();
    wrong.reserved_usage = 11;
    variants.push(wrong);
    let mut false_quota = first.clone();
    false_quota.outcome = Outcome::Refused;
    false_quota.failure_reason = Some(Reason::QuotaExceeded);
    false_quota.response_digest = None;
    false_quota.response_bytes = 0;
    false_quota.charged_usage = 0.into();
    variants.push(false_quota.clone());
    let mut false_size = false_quota.clone();
    false_size.failure_reason = Some(Reason::RequestTooLarge);
    variants.push(false_size);
    for wrong in variants {
        rejected(&mut f, &bound, &wrong);
    }
    record(&mut f, &bound, &first);
    let sequence = f.state().next_sequence;
    record(&mut f, &bound, &first);
    assert_eq!(f.state().next_sequence, sequence);
    let mut conflicting = first.clone();
    conflicting.charged_usage = 6.into();
    rejected(&mut f, &bound, &conflicting);
    // 7 + 4 exceeds the captured usage cap even though there is one call remaining.
    rejected(&mut f, &bound, &receipt(&bound, 2, 4));
    let mut refused = false_quota;
    refused.ordinal = 2;
    refused.reserved_usage = 4;
    record(&mut f, &bound, &refused);
    assert!(
        f.store
            .check_task_broker_current(&f.cas, &bound, &BrokerAuthority)
            .is_err()
    );
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 7);
    assert_eq!(operations(&f).len(), 2);

    let mut operation = policy();
    operation.max_calls = 1;
    let mut f = fixture(&[operation]);
    let (_, _, bound) = start(&mut f);
    record(&mut f, &bound, &receipt(&bound, 1, 1));
    rejected(&mut f, &bound, &receipt(&bound, 2, 1));
    let mut refused = receipt(&bound, 2, 1);
    refused.outcome = Outcome::Refused;
    refused.failure_reason = Some(Reason::QuotaExceeded);
    refused.response_digest = None;
    refused.response_bytes = 0;
    refused.charged_usage = 0.into();
    record(&mut f, &bound, &refused);
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 1);
}

#[test]
fn distinct_operation_quotas_share_dense_order_and_one_common_charge() {
    let mut first_policy = policy();
    first_policy.max_usage = 4;
    first_policy.max_calls = 1;
    let mut second_policy = policy();
    second_policy.name = "inspect".into();
    second_policy.method = "inspect".into();
    second_policy.max_usage = 6;
    let mut f = fixture(&[first_policy, second_policy.clone()]);
    let (_, _, bound) = start(&mut f);
    record(&mut f, &bound, &receipt(&bound, 1, 3));
    let mut second = receipt(&bound, 2, 4);
    second.operation = second_policy.name;
    second.method = second_policy.method;
    // The first operation's spend must not consume the second operation's captured cap.
    // Conversely the unused part of this reservation is released by exact actual usage.
    second.charged_usage = 2.into();
    let mut wrong_ordinal = second.clone();
    wrong_ordinal.ordinal = 1;
    rejected(&mut f, &bound, &wrong_ordinal);
    record(&mut f, &bound, &second);
    second.ordinal = 3;
    second.charged_usage = 4.into();
    record(&mut f, &bound, &second);
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 9);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.attempt_accounting().len(), 1);
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, 9);
    assert_eq!(operations(&f).len(), 3);
    // One unused token in the original Task reservation does not restore either
    // operation's exhausted call quota or authorize a fourth external effect.
    rejected(&mut f, &bound, &receipt(&bound, 4, 1));
    second.ordinal = 4;
    second.reserved_usage = 1;
    second.charged_usage = 1.into();
    rejected(&mut f, &bound, &second);
}

#[test]
fn receipt_and_common_charge_are_one_transaction() {
    let mut f = fixture(&[policy()]);
    let (_, _, bound) = start(&mut f);
    let receipt = receipt(&bound, 1, 7);
    let before = f.state().next_sequence;
    f.store
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fail_broker_append BEFORE INSERT ON events
         WHEN NEW.type = 'TaskBrokerTransition@1'
         BEGIN SELECT RAISE(ABORT, 'fixture disk write failure'); END;",
        )
        .unwrap();
    let error = f
        .store
        .record_task_broker_receipt(&f.cas, &bound, &receipt, &BrokerAuthority)
        .unwrap_err();
    assert_eq!(f.state().next_sequence, before);
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 0);
    assert!(operations(&f).is_empty());
    assert!(
        error.to_string().contains("fixture disk write failure"),
        "{error}"
    );
    f.store
        .conn
        .execute_batch("DROP TRIGGER fail_broker_append")
        .unwrap();
    record(&mut f, &bound, &receipt);
    assert_eq!(f.state().next_sequence, before + 1);
    assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 7);
    assert_eq!(operations(&f).len(), 1);
}

#[test]
fn multiple_broker_operations_share_one_attempt_and_exact_overrun_floor_after_reopen() {
    let mut f = fixture(&[policy()]);
    let (lease, attempt, bound) = start(&mut f);
    let original_limits = f.revision.limits.clone();
    record(&mut f, &bound, &receipt(&bound, 1, 7));
    f.store
        .check_task_broker_current(&f.cas, &bound, &BrokerAuthority)
        .unwrap();
    let mut overrun = receipt(&bound, 2, u64::MAX);
    overrun.reserved_usage = 3;
    overrun.outcome = Outcome::Failed;
    overrun.failure_reason = Some(Reason::UsageOverrun);
    record(&mut f, &bound, &overrun);
    let total = u128::from(u64::MAX) + 7;
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), total);
    assert!(execution.budget.breached());
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.attempt_accounting().len(), 1);
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, total);
    assert_eq!(
        execution.attempt_accounting()[0].reservation,
        *attempt.reservation()
    );
    assert!(
        f.store
            .check_task_broker_current(&f.cas, &bound, &BrokerAuthority)
            .is_err()
    );
    assert!(
        f.store
            .check_task_attempt_current(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
    rejected(&mut f, &bound, &receipt(&bound, 3, 1));
    let before = f.state().next_sequence;
    record(&mut f, &bound, &overrun);
    assert_eq!(f.state().next_sequence, before);
    settle(&mut f, &lease, &attempt, 3);
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.revision.limits, original_limits);
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), total);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    assert!(execution.budget.breached());
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, total);
    assert_eq!(execution.attempt_accounting()[0].plan_id, f.plan_id);
    let records = operations(&f);
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[1].0.payload["receipt"]["charged_usage"],
        u64::MAX.to_string()
    );
    assert_eq!(records[1].1.receipt, overrun);
    assert_eq!(records[1].1.binding_id, binding_id(&f));
    let before = f.state().next_sequence;
    settle(&mut f, &lease, &attempt, 3);
    assert_eq!(f.state().next_sequence, before);
    assert_eq!(
        f.state().execution.unwrap().budget.committed_tokens(),
        total
    );
}

fn assert_late(f: &mut Fixture, bound: &BoundTaskBroker) {
    assert!(
        f.store
            .check_task_broker_current(&f.cas, bound, &BrokerAuthority)
            .is_err()
    );
    let original = operations(f).pop().unwrap();
    assert_eq!(original.1.receipt.charged_usage.get(), 7);
    let total = u128::from(u64::MAX) + 7;
    let mut late = receipt(bound, 2, u64::MAX);
    late.reserved_usage = 3;
    late.outcome = Outcome::Failed;
    late.failure_reason = Some(Reason::UsageOverrun);
    let mut forged = late.clone();
    forged.destination = "uncaptured-provider".into();
    rejected(f, bound, &forged);
    let before = f.state().next_sequence;
    assert_eq!(
        f.store
            .record_task_broker_receipt(&f.cas, bound, &late, &BrokerAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::AuthorityRevoked
    );
    assert_eq!(f.state().next_sequence, before + 1);
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), total);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.attempt_accounting().len(), 1);
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, total);
    let records = operations(f);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].0, original.0);
    let stored = &records[1].1.receipt;
    assert_eq!(stored.outcome, Outcome::Revoked);
    assert_eq!(stored.failure_reason, Some(Reason::AuthorityRevoked));
    assert_eq!(stored.charged_usage, late.charged_usage);
    assert_eq!(stored.reserved_usage, late.reserved_usage);
    assert_eq!(stored.response_digest, late.response_digest);
    assert_eq!(records[1].1.binding_id, binding_id(f));
    assert_eq!(
        f.store
            .record_task_broker_receipt(&f.cas, bound, &late, &BrokerAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::AuthorityRevoked
    );
    assert_eq!(f.state().next_sequence, before + 1);
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(
        f.state().execution.unwrap().budget.committed_tokens(),
        total
    );
    assert!(
        f.store
            .check_task_broker_current(&f.cas, bound, &BrokerAuthority)
            .is_err()
    );
    assert_eq!(
        f.store
            .record_task_broker_receipt(&f.cas, bound, &late, &BrokerAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::AuthorityRevoked
    );
    assert_eq!(f.state().next_sequence, before + 1);
}

#[test]
fn paid_receipt_after_writer_replacement_preserves_original_attempt_and_lower_settlement() {
    let mut f = fixture(&[policy()]);
    let (lease, attempt, bound) = start(&mut f);
    record(&mut f, &bound, &receipt(&bound, 1, 7));
    settle(&mut f, &lease, &attempt, 3);
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let replacement = f
        .store
        .take_task_lease(&f.cas, lease.task_id(), "writer-2", 1_000_000)
        .unwrap();
    assert!(replacement.epoch > lease.epoch);
    assert_late(&mut f, &bound);
    assert_eq!(bound.binding().writer_epoch, lease.epoch);
    assert_eq!(
        f.state().execution.unwrap().attempt_accounting()[0].reservation,
        *attempt.reservation()
    );
}

#[test]
fn paid_receipt_after_captured_review_round_supersession_is_revoked_and_charged() {
    let (f, round) = review::round::round_fixture();
    let mut f = install(f, &[policy()], true);
    let (_, _, bound) = start(&mut f);
    assert_eq!(bound.binding().lease.campaign_id, round.campaign_id);
    assert_eq!(bound.binding().lease.round_event_id, round.round_event_id);
    assert_eq!(bound.binding().lease.node_id, "reviewer");
    record(&mut f, &bound, &receipt(&bound, 1, 7));
    review::round::supersede(&f, &round);
    assert_late(&mut f, &bound);
    assert_eq!(f.state().execution.unwrap().pending_attempts().len(), 1);
}

#[test]
fn paid_receipt_after_source_replan_keeps_original_plan_and_task_limits() {
    let mut f = install(source::fixture(false), &[policy()], false);
    let (lease, attempt, bound) = start(&mut f);
    record(&mut f, &bound, &receipt(&bound, 1, 7));
    settle(&mut f, &lease, &attempt, 3);
    let next = source::next(&f);
    let (revision_id, plan_id) = source::plan_for(&f, &next);
    f.store
        .refresh_task_source(
            &f.cas,
            &lease,
            &revision_id,
            Some(&plan_id),
            None,
            &f.authority,
        )
        .unwrap();
    assert_ne!(
        f.state().plan_id.as_deref(),
        Some(bound.binding().plan_id.as_str())
    );
    assert_late(&mut f, &bound);
    let state = f.state();
    assert_eq!(state.plan_id, Some(plan_id));
    assert_eq!(state.revision.limits, f.revision.limits);
    assert_eq!(
        state.execution.unwrap().attempt_accounting()[0].plan_id,
        bound.binding().plan_id
    );
}
