use super::{assert_invalid, assert_valid};
use review_core::{
    BrokerFailureReasonV1, BrokerLeaseV1, BrokerOperationOutcomeV1, BrokerOperationPolicyV1,
    BrokerOperationReceiptV2, EventType, RunEvent, TaskBrokerBindingV1, TaskBrokerOperationV1,
    TaskBrokerTargetV1, TaskBrokerTransitionV1,
};
use serde_json::{Value, json};

fn digest(value: char) -> String {
    format!("sha256:{}", value.to_string().repeat(64))
}

pub(super) fn binding() -> TaskBrokerBindingV1 {
    TaskBrokerBindingV1 {
        task_id: "review-task".into(),
        task_revision_id: digest('1'),
        plan_id: digest('2'),
        invocation_id: digest('3'),
        context_id: digest('4'),
        attempt_id: "a".repeat(26),
        reservation_id: "reservation:7".into(),
        writer: "task-host".into(),
        writer_epoch: 7,
        node: "root.nodes.reviewer".into(),
        target: TaskBrokerTargetV1::Worker {
            slot: "root.reviewer".into(),
            invocation_policy_id: digest('5'),
        },
        lease: BrokerLeaseV1 {
            campaign_id: "captured-campaign".into(),
            round_event_id: "b".repeat(26),
            node_id: "reviewer".into(),
            attempt_id: "a".repeat(26),
            lease_epoch: 7,
        },
        handle_id: "c".repeat(26),
        operations: vec![BrokerOperationPolicyV1 {
            name: "complete".into(),
            destination: "review-provider".into(),
            method: "generate".into(),
            max_request_bytes: 1024,
            max_response_bytes: 2048,
            max_calls: 4,
            max_usage: 40,
        }],
    }
}

fn operation() -> TaskBrokerOperationV1 {
    let binding = binding();
    TaskBrokerOperationV1 {
        binding_id: digest('6'),
        receipt: BrokerOperationReceiptV2 {
            handle_id: binding.handle_id,
            node: binding.lease.node_id,
            attempt_id: binding.attempt_id,
            lease_epoch: binding.writer_epoch,
            operation: "complete".into(),
            destination: "review-provider".into(),
            method: "generate".into(),
            ordinal: 1,
            outcome: BrokerOperationOutcomeV1::Failed,
            failure_reason: Some(BrokerFailureReasonV1::UsageOverrun),
            request_digest: digest('7'),
            response_digest: Some(digest('8')),
            request_bytes: 120,
            response_bytes: 240,
            reserved_usage: 20,
            charged_usage: u64::MAX.into(),
        },
    }
}

#[test]
fn broker_inspection_retains_closed_history_and_exact_record_generations() {
    let value = json!({
        "schema":"af/task-inspection@4", "task_id":"review-task", "revision_id":digest('1'),
        "phase":{"kind":"running"}, "plan_id":digest('2'),
        "chargeable_tokens":(u128::from(u64::MAX) + 7).to_string(), "attempts":1,
        "history":[{"sequence":4,"broker_transition":{"now_unix_ms":123,"record_id":digest('6')}}],
        "execution_records":[], "run_reports":[],
        "broker_records":[
            {"artifact_id":digest('6'),"artifact_type":"af/TaskBrokerBinding@1","record":binding()},
            {"artifact_id":digest('9'),"artifact_type":"af/TaskBrokerOperation@1","record":operation()}
        ]
    });
    assert_valid("task-inspection-v4.json", &value);
    assert_invalid(
        "task-inspection-v3.json",
        &value,
        "frozen v3 cannot mislabel Broker evidence",
    );
    for (path, replacement) in [
        (
            "/broker_records/1/artifact_type",
            json!("af/TaskBrokerBinding@1"),
        ),
        ("/broker_records/1/record/receipt/charged_usage", json!(7)),
        ("/history/0/broker_transition/record_id", json!("invented")),
        ("/schema", json!("af/task-inspection@3")),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("task-inspection-v4.json", &invalid, path);
    }
    let mut foreign = value.clone();
    foreign["history"][0]["broker_transition"]["approved"] = json!(true);
    assert_invalid(
        "task-inspection-v4.json",
        &foreign,
        "unknown transition authority",
    );
}

#[test]
fn task_broker_binding_is_closed_and_retains_exact_captured_references() {
    let binding = binding();
    binding.validate().unwrap();
    let value = serde_json::to_value(&binding).unwrap();
    assert_valid("task-broker-binding-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskBrokerBindingV1>(value.clone()).unwrap(),
        binding
    );
    assert_eq!(
        binding.artifact_refs(),
        vec![
            binding.task_revision_id.as_str(),
            &binding.plan_id,
            &binding.invocation_id,
            &binding.context_id,
            binding.target.policy_id(),
        ]
    );
    for (field, replacement) in [
        ("task_id", json!("")),
        ("writer", json!("host\n")),
        ("writer_epoch", json!(0)),
        ("writer_epoch", json!(9_007_199_254_740_992_u64)),
        ("node", json!("root..reviewer")),
        ("slot", json!(".reviewer")),
        ("handle_id", json!("C".repeat(26))),
        ("attempt_id", json!("A".repeat(26))),
        ("reservation_id", json!(digest('9'))),
        ("reservation_id", json!("reservation:")),
        ("reservation_id", json!("reservation:123456789012345678901")),
        ("reservation_id", json!("reservation:1\n")),
        ("task_revision_id", json!("missing")),
        ("plan_id", json!(null)),
        ("invocation_policy_id", json!("mutable-policy")),
        ("operations", json!([])),
        ("operations", json!(null)),
        ("admitted", json!(true)),
        ("credential", json!("must-not-be-here")),
    ] {
        let mut invalid = value.clone();
        invalid[field] = replacement;
        assert_invalid("task-broker-binding-v1.json", &invalid, field);
        assert!(
            serde_json::from_value::<TaskBrokerBindingV1>(invalid)
                .map_err(|error| error.to_string())
                .and_then(|binding| binding.validate())
                .is_err(),
            "{field}"
        );
    }
    // This is the existing execution-record reservation syntax, not an invented ID scheme.
    for reservation in [
        "reservation:0",
        "reservation:0007",
        "reservation:99999999999999999999",
    ] {
        let mut value = value.clone();
        value["reservation_id"] = json!(reservation);
        assert_valid("task-broker-binding-v1.json", &value);
        serde_json::from_value::<TaskBrokerBindingV1>(value)
            .unwrap()
            .validate()
            .unwrap();
    }
}

#[test]
fn task_broker_binding_enforces_lease_and_captured_operation_invariants() {
    let original = binding();
    for mutate in [
        |binding: &mut TaskBrokerBindingV1| binding.attempt_id = "d".repeat(26),
        |binding: &mut TaskBrokerBindingV1| binding.writer_epoch += 1,
        |binding: &mut TaskBrokerBindingV1| binding.lease.node_id = " ".into(),
        |binding: &mut TaskBrokerBindingV1| binding.lease.campaign_id = "\n".into(),
        |binding: &mut TaskBrokerBindingV1| binding.lease.round_event_id = "invalid".into(),
        |binding: &mut TaskBrokerBindingV1| binding.lease.lease_epoch = 0,
        |binding: &mut TaskBrokerBindingV1| binding.operations[0].name.clear(),
        |binding: &mut TaskBrokerBindingV1| {
            binding.operations[0].destination = "https://provider".into()
        },
        |binding: &mut TaskBrokerBindingV1| binding.operations[0].max_calls = 0,
        |binding: &mut TaskBrokerBindingV1| binding.operations[0].max_usage = 9_007_199_254_740_991,
    ] {
        let mut binding = original.clone();
        mutate(&mut binding);
        assert!(binding.validate().is_err());
    }
    let mut duplicate = original.clone();
    let mut second = duplicate.operations[0].clone();
    second.method = "other-method".into();
    duplicate.operations.push(second);
    assert!(duplicate.validate().unwrap_err().contains("duplicate"));
    let mut too_wide = original;
    too_wide.operations[0].max_usage = 9_007_199_254_740_990;
    let mut second = too_wide.operations[0].clone();
    second.name = "second".into();
    too_wide.operations.push(second);
    assert!(
        too_wide
            .validate()
            .unwrap_err()
            .contains("durable numeric domain")
    );
}

#[test]
fn task_broker_operation_retains_exact_paid_usage_without_cas_body_requirements() {
    let operation = operation();
    operation.validate().unwrap();
    let value = serde_json::to_value(&operation).unwrap();
    review_core::json::admit(&value).unwrap();
    assert_valid("task-broker-operation-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskBrokerOperationV1>(value.clone()).unwrap(),
        operation
    );
    assert_eq!(
        operation.artifact_refs(),
        vec![operation.binding_id.as_str()]
    );
    assert_eq!(value["receipt"]["charged_usage"], u64::MAX.to_string());
    assert!(operation.receipt.clone().try_into_legacy().is_err());
    let mut late = operation.clone();
    late.receipt.outcome = BrokerOperationOutcomeV1::Revoked;
    late.receipt.failure_reason = Some(BrokerFailureReasonV1::AuthorityRevoked);
    late.validate().unwrap();
    assert_valid(
        "task-broker-operation-v1.json",
        &serde_json::to_value(late).unwrap(),
    );
    for (pointer, replacement) in [
        ("/binding_id", json!("missing")),
        ("/receipt/charged_usage", json!(u64::MAX)),
        ("/receipt/charged_usage", json!("18446744073709551616")),
        ("/receipt/charged_usage", json!("01")),
        ("/receipt/response_digest", json!(null)),
        ("/receipt/failure_reason", json!(null)),
        ("/receipt/ordinal", json!(0)),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert_invalid("task-broker-operation-v1.json", &invalid, pointer);
        assert!(
            serde_json::from_value::<TaskBrokerOperationV1>(invalid)
                .map_err(|error| error.to_string())
                .and_then(|operation| operation.validate())
                .is_err()
        );
    }
    let mut impossible = operation;
    impossible.receipt.outcome = BrokerOperationOutcomeV1::Succeeded;
    impossible.receipt.failure_reason = None;
    assert!(
        impossible.validate().is_err(),
        "paid overrun cannot become a successful receipt"
    );
}

#[test]
fn task_broker_transition_is_a_closed_additive_event_with_one_record_reference() {
    let transition = TaskBrokerTransitionV1 {
        now_unix_ms: 123,
        record_id: digest('9'),
    };
    transition.validate().unwrap();
    assert_eq!(
        transition.artifact_refs(),
        vec![transition.record_id.as_str()]
    );
    let payload = serde_json::to_value(&transition).unwrap();
    assert_valid("task-broker-transition-v1.json", &payload);
    review_core::event::validate_event_payload(EventType::TaskBrokerTransitionV1, &payload)
        .unwrap();
    assert_eq!(
        "TaskBrokerTransition@1".parse::<EventType>().unwrap(),
        EventType::TaskBrokerTransitionV1
    );
    assert_eq!(
        EventType::TaskBrokerTransitionV1.typed(),
        ("TaskBrokerTransition", 1)
    );
    let event = RunEvent {
        event_id: "d".repeat(26),
        run_id: "task:review-task".into(),
        sequence: 1,
        event_type: EventType::TaskBrokerTransitionV1,
        occurred_at: "2026-09-12T12:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: vec![transition.record_id.clone()],
        payload: payload.clone(),
    };
    let event_value = serde_json::to_value(event).unwrap();
    assert_valid("run-event-v1.json", &event_value);
    for (field, replacement) in [
        ("now_unix_ms", json!(0)),
        ("now_unix_ms", json!(9_007_199_254_740_992_u64)),
        ("record_id", json!("untyped-record")),
        ("record_id", Value::Null),
        ("writer", json!("independent-authority")),
        ("charged_tokens", json!(7)),
    ] {
        let mut invalid = payload.clone();
        invalid[field] = replacement;
        assert_invalid("task-broker-transition-v1.json", &invalid, field);
        assert!(
            review_core::event::validate_event_payload(EventType::TaskBrokerTransitionV1, &invalid)
                .is_err()
        );
        let mut invalid_event = event_value.clone();
        invalid_event["payload"] = invalid;
        assert_invalid("run-event-v1.json", &invalid_event, field);
    }
}

#[test]
fn task_broker_target_is_one_exact_worker_slot() {
    let value = serde_json::to_value(binding()).unwrap();
    assert_valid("task-broker-binding-v1.json", &value);
    let policy = digest('5');
    for replacement in [
        json!({"kind":"provider_admission","probe_policy_id":policy}),
        json!({"kind":"worker","probe_policy_id":policy}),
        json!({"kind":"worker","slot":"root..writer","invocation_policy_id":policy}),
        json!({"kind":"worker","slot":"root.writer","invocation_policy_id":policy,"probe_policy_id":policy}),
    ] {
        let mut invalid = value.clone();
        invalid["target"] = replacement;
        assert_invalid(
            "task-broker-binding-v1.json",
            &invalid,
            "retired or malformed target",
        );
        assert!(
            serde_json::from_value::<TaskBrokerBindingV1>(invalid)
                .map_err(|e| e.to_string())
                .and_then(|b| b.validate())
                .is_err()
        );
    }
}
