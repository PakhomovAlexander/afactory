use super::*;
use review_core::task::{execution::*, usage::*};

fn append(
    f: &mut Fixture,
    lease: &TaskLease,
    kind: &str,
    value: serde_json::Value,
) -> Result<String, StoreError> {
    let record_id = f
        .cas
        .put_artifact(kind, producer(), vec![], None, value)
        .unwrap()
        .0;
    f.store.append_task_transition(
        &f.cas,
        lease.task_id(),
        TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now().unwrap(),
            change: TaskChangeV1::ExecutionRecorded {
                record_id: record_id.clone(),
            },
        },
    )?;
    Ok(record_id)
}

#[test]
fn numeric_history_and_full_native_usage_reopen_exactly_including_crash_recovery() {
    for recover in [false, true] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context = f
            .cas
            .put_json(&json!({"context":"wide usage recovery"}))
            .unwrap();
        let diagnostic = f.cas.put_json(&json!({"error":"fixture failure"})).unwrap();
        let first = f
            .store
            .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &first, &f.authority)
            .unwrap();
        let historical = TaskExecutionRecordV1::Settled {
            attempt_id: first.id().into(),
            charged_tokens: 7,
            result: TaskAttemptResultV1::Failed {
                diagnostic_id: diagnostic.clone(),
                feedback_id: None,
            },
            raw_artifact_ids: vec![],
            usage_id: None,
        };
        let historical_id = append(
            &mut f,
            &lease,
            TASK_EXECUTION_RECORD_V1,
            serde_json::to_value(&historical).unwrap(),
        )
        .unwrap();
        let sequence = f.state().next_sequence;
        // The new writer must neither replace nor re-encode an existing terminal receipt.
        f.store
            .settle_task_attempt(&f.cas, &lease, historical.clone(), &f.authority)
            .unwrap();
        assert_eq!(f.state().next_sequence, sequence);
        let equivalent = TaskExecutionRecordV2::from_accounting(&historical).unwrap();
        assert!(
            append(
                &mut f,
                &lease,
                TASK_EXECUTION_RECORD_V2,
                serde_json::to_value(equivalent).unwrap()
            )
            .is_err()
        );
        assert_eq!(f.state().next_sequence, sequence);
        let active = f
            .store
            .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &active, &f.authority)
            .unwrap();
        let usage = crate::AttemptUsage {
            input_tokens: Some(u64::MAX),
            chargeable_tokens: u64::MAX,
            ..Default::default()
        };
        f.store
            .record_attempt_wall(&crate::AttemptWall {
                run_id: task_run_id(lease.task_id()).unwrap(),
                attempt_id: active.id().into(),
                node_id: "root.nodes.write".into(),
                round: 0,
                epoch: 1,
                started_unix_ms: now().unwrap(),
                elapsed_ms: 1,
                usage: Some(usage.clone()),
            })
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        if recover {
            let time = f.state().lease_until + 1;
            f.store
                .append_task_transition(
                    &f.cas,
                    lease.task_id(),
                    TaskTransitionV1 {
                        writer: "recovery".into(),
                        epoch: 2,
                        now_unix_ms: time,
                        change: TaskChangeV1::LeaseTaken {
                            lease_until_unix_ms: time + 10000,
                        },
                    },
                )
                .unwrap();
            let next = TaskLease {
                task_id: lease.task_id.clone(),
                writer: "recovery".into(),
                epoch: 2,
            };
            f.store
                .recover_task_attempts_at(&f.cas, &next, time)
                .unwrap();
            f.store
                .recover_task_attempts_at(&f.cas, &next, time)
                .unwrap();
        } else {
            let usage_id = f
                .cas
                .put_artifact(
                    TASK_TOKEN_USAGE_V1,
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(TaskTokenUsageV1::from(&usage)).unwrap(),
                )
                .unwrap()
                .0;
            for amount in [u64::MAX, u64::MAX, 1] {
                f.store
                    .observe_task_usage(
                        &f.cas,
                        &lease,
                        TaskExecutionRecordV1::UsageObserved {
                            attempt_id: active.id().into(),
                            charged_tokens: amount,
                            usage_id: usage_id.clone(),
                            raw_artifact_ids: vec![],
                        },
                    )
                    .unwrap();
                assert!(
                    f.store
                        .check_task_attempt_current(&f.cas, &lease, &active, &f.authority)
                        .is_err()
                );
            }
            f.store
                .settle_task_attempt(
                    &f.cas,
                    &lease,
                    TaskExecutionRecordV1::Settled {
                        attempt_id: active.id().into(),
                        charged_tokens: u64::MAX,
                        result: TaskAttemptResultV1::Failed {
                            diagnostic_id: diagnostic,
                            feedback_id: None,
                        },
                        raw_artifact_ids: vec![],
                        usage_id: Some(usage_id),
                    },
                    &f.authority,
                )
                .unwrap();
        }
        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.unwrap();
        assert_eq!(
            execution.budget.committed_tokens(),
            18_446_744_073_709_551_622_u128
        );
        assert!(execution.budget.breached());
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.begun_attempts(), 2);
        let legacy = execution::read_execution_record(&f.cas, &historical_id).unwrap();
        assert_eq!(legacy.envelope.artifact_id, historical_id);
        assert_eq!(legacy.envelope.artifact_type, TASK_EXECUTION_RECORD_V1);
        let records: Vec<_> = f
            .store
            .replay(&task_run_id(lease.task_id()).unwrap())
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                let transition: TaskTransitionV1 = serde_json::from_value(event.payload).unwrap();
                if let TaskChangeV1::ExecutionRecorded { record_id } = transition.change {
                    Some(execution::read_execution_record(&f.cas, &record_id).unwrap())
                } else {
                    None
                }
            })
            .collect();
        let current = records
            .iter()
            .find(|entry| {
                matches!(&entry.record,
            TaskExecutionRecordV1::Settled { attempt_id, .. } if attempt_id == active.id())
            })
            .unwrap();
        assert_eq!(current.envelope.artifact_type, TASK_EXECUTION_RECORD_V2);
        assert_eq!(
            current.envelope.payload["charged_tokens"],
            "18446744073709551615"
        );
        let TaskExecutionRecordV1::Settled {
            usage_id: Some(usage_id),
            ..
        } = &current.record
        else {
            panic!("missing recovered usage");
        };
        let exact: TaskTokenUsageV1 = payload(&f.cas, usage_id, TASK_TOKEN_USAGE_V1).unwrap();
        assert_eq!(exact.chargeable_tokens.get(), u64::MAX);
        assert_eq!(exact.input_tokens.map(DecimalU64::get), Some(u64::MAX));
    }
}
