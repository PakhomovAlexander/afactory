use super::*;
use review_core::task::execution::*;

// Reproduce the live-owner race without sleeps or a lucky scheduler: capture exactly the
// projection settlement validated, then append the own-connection heartbeat before its write.
#[test]
fn renewal_race_settles_once_with_exact_accounting_and_replay() {
    for intervening in ["renew", "takeover", "usage", "expired"] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context = f.cas.put_json(&json!({"context": true})).unwrap();
        let attempt = f
            .store
            .reserve_and_bind_task_attempt(
                &f.cas,
                &lease,
                "root.nodes.write",
                &context,
                &f.authority,
            )
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let usage = put(
            &f.cas,
            task::usage::TASK_TOKEN_USAGE_V3,
            task::usage::TaskTokenUsageV3 {
                chargeable_tokens: 7.into(),
                ..Default::default()
            },
        );
        f.store
            .observe_task_usage(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 7,
                    usage_id: usage.clone(),
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
        let checked = f.state();
        let diagnostic = f.cas.put_json(&json!({"failure": "fixture"})).unwrap();
        let record = TaskExecutionRecordV1::Settled {
            attempt_id: attempt.id().into(),
            charged_tokens: 7,
            result: TaskAttemptResultV1::Failed {
                diagnostic_id: diagnostic,
                feedback_id: None,
            },
            raw_artifact_ids: vec![],
            usage_id: Some(usage),
            unknown_usage: None,
        };
        let record_id = put(&f.cas, TASK_EXECUTION_RECORD_V5, &record);
        let mut transition = TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now().unwrap(),
            change: TaskChangeV1::ExecutionRecorded { record_id },
        };
        match intervening {
            "renew" => {
                // Same lease, second connection, just as the reserve heartbeat does.
                let mut own = f.store.reopen(std::time::Duration::from_secs(1)).unwrap();
                own.renew_task_lease(&f.cas, &lease, 1_000_000).unwrap();
            }
            "takeover" => {
                let time = checked.lease_until + 1;
                f.store
                    .append_task_transition(
                        &f.cas,
                        lease.task_id(),
                        TaskTransitionV1 {
                            writer: "successor".into(),
                            epoch: lease.epoch + 1,
                            now_unix_ms: time,
                            change: TaskChangeV1::LeaseTaken {
                                lease_until_unix_ms: time + 10_000,
                            },
                        },
                    )
                    .unwrap();
            }
            "expired" => {
                transition.now_unix_ms = checked.lease_until;
            }
            "usage" => {
                // Even a valid same-owner execution mutation is not a renewal-only suffix.
                let usage = put(
                    &f.cas,
                    task::usage::TASK_TOKEN_USAGE_V3,
                    task::usage::TaskTokenUsageV3 {
                        chargeable_tokens: 8.into(),
                        ..Default::default()
                    },
                );
                f.store
                    .observe_task_usage(
                        &f.cas,
                        &lease,
                        TaskExecutionRecordV1::UsageObserved {
                            attempt_id: attempt.id().into(),
                            charged_tokens: 8,
                            usage_id: usage,
                            raw_artifact_ids: vec![],
                        },
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = f.state().next_sequence;
        let result = f.store.append_task_transition_from_state(
            &f.cas,
            lease.task_id(),
            transition,
            Some((checked.next_sequence, None)),
            Some(checked),
        );
        assert_eq!(
            result.is_ok(),
            intervening == "renew",
            "{intervening}: {result:?}"
        );
        let healthy = f.state();
        let execution = healthy.execution.as_ref().unwrap();
        if intervening == "renew" {
            assert!(execution.pending_attempts().is_empty());
            assert_eq!(execution.budget.reserved_tokens(), 0);
            assert_eq!(execution.budget.begun_attempts(), 1);
            assert_eq!(execution.budget.committed_tokens(), 7);
            assert_eq!(healthy.next_sequence, before + 1);
            assert_eq!(
                execution.attempt_accounting()[0].result,
                match record {
                    TaskExecutionRecordV1::Settled { result, .. } => Some(result),
                    _ => unreachable!(),
                }
            );
        } else {
            assert_eq!(execution.pending_attempts(), [attempt.id()]);
            assert_eq!(healthy.next_sequence, before);
        }
        let cold = EventStore::open_read_only(&f.path).unwrap();
        let replay = cold
            .task_projection(&f.cas, lease.task_id())
            .unwrap()
            .unwrap();
        assert_eq!(replay.next_sequence, healthy.next_sequence);
        assert_eq!(
            format!("{:?}", replay.execution.unwrap().attempt_accounting()),
            format!("{:?}", execution.attempt_accounting())
        );
    }
}

fn put(cas: &Cas, kind: &str, value: impl serde::Serialize) -> String {
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
