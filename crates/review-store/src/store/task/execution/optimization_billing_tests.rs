use super::*;
use serde_json::json;

#[test]
fn optimization_does_not_treat_a_native_reservation_floor_as_complete_billing() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let id = format!("sha256:{}", "1".repeat(64));
    let producer = review_core::Producer::Attempt {
        run_id: "billing-test".into(),
        node_id: "trial".into(),
        attempt_id: "a".repeat(26),
    };
    let put_usage = |payload| {
        cas.put_artifact(
            task::usage::TASK_TOKEN_USAGE_V3,
            producer.clone(),
            vec![],
            None,
            payload,
        )
        .unwrap()
        .0
    };
    let mut accounting = TaskAttemptAccounting {
        attempt_id: "a".repeat(26),
        invocation_id: id.clone(),
        plan_id: id.clone(),
        reservation: TaskReservation {
            id: "reservation".into(),
            node: "trial".into(),
            tokens: 100,
            deadline_unix_ms: 1000,
        },
        started: true,
        started_unix_ms: Some(10),
        settled_unix_ms: Some(20),
        released: false,
        charged_tokens: 100,
        state: None,
        context_id: None,
        result: Some(TaskAttemptResultV1::Failed {
            diagnostic_id: id,
            feedback_id: None,
        }),
        raw_artifact_ids: vec![],
        usage_id: Some(put_usage(json!({"chargeable_tokens":"100"}))),
    };
    assert!(!accounting.billing_complete(&cas, true).unwrap());
    accounting.usage_id = Some(put_usage(json!({"chargeable_tokens":"0"})));
    accounting.charged_tokens = 0;
    assert!(accounting.billing_complete(&cas, false).unwrap());
    accounting.usage_id = Some(put_usage(
        json!({"input_tokens":"0","output_tokens":"0","chargeable_tokens":"0"}),
    ));
    assert!(accounting.billing_complete(&cas, true).unwrap());
    accounting.raw_artifact_ids.push(
        cas.put_artifact(
            task::usage::TASK_USAGE_OBSERVATION_V1,
            producer,
            vec![],
            None,
            json!({"charge_complete":false}),
        )
        .unwrap()
        .0,
    );
    assert!(!accounting.billing_complete(&cas, true).unwrap());
}
