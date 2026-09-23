use super::*;
use review_core::task::{execution::*, usage::*};

fn fixture_with_sibling() -> Fixture {
    let mut f = Fixture::new(false);
    // Both captured author slots can produce verification evidence. Protect both before
    // compiling the graph; the running test never replaces these original allowances.
    f.revision.limits.verification.attempts = 2;
    f.revision.limits.verification.wall_ms = 2000;
    f.revision_id = f
        .cas
        .put_artifact(
            task::TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.revision).unwrap(),
        )
        .unwrap()
        .0;
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.limits = f.revision.limits.clone();
    let mut pipeline: task::pipeline::PipelineDefinitionV1 =
        payload(&f.cas, &f.plan.pipeline_id, PIPELINE_V1).unwrap();
    let mut sibling = pipeline.nodes[0].clone();
    sibling.id = "sibling".into();
    pipeline.nodes.push(sibling);
    let (id, envelope) = f
        .cas
        .put_artifact(
            PIPELINE_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(pipeline).unwrap(),
        )
        .unwrap();
    f.plan.pipeline_id = id.clone();
    let dependency = f.plan.dependencies.get_mut("builtin/document").unwrap();
    dependency.artifact_id = id;
    dependency.content_digest = envelope.content_id;
    f.with_execution_graph()
}

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
fn settled_and_cumulative_attempt_usage_reopen_exactly_including_crash_recovery() {
    for recover in [false, true] {
        let mut f = fixture_with_sibling();
        let actual = u128::from(u64::MAX) + 7;
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let writer_input = f.record_execution_inputs(&lease);
        let mut sibling_input: TaskInvocationV1 =
            payload(&f.cas, &writer_input, TASK_INVOCATION_V1).unwrap();
        sibling_input.node = "root.nodes.sibling".into();
        let sibling_input_id = f
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                producer(),
                vec![f.plan_id.clone()],
                None,
                serde_json::to_value(sibling_input).unwrap(),
            )
            .unwrap()
            .0;
        f.store
            .record_task_invocation(&f.cas, &lease, &sibling_input_id, &f.authority)
            .unwrap();
        let context = f
            .cas
            .put_json(&json!({"context":"wide usage recovery"}))
            .unwrap();
        let diagnostic = f.cas.put_json(&json!({"error":"fixture failure"})).unwrap();
        let first = f
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
            .start_task_attempt(&f.cas, &lease, &first, &f.authority)
            .unwrap();
        let first_usage_id = f
            .cas
            .put_artifact(
                TASK_TOKEN_USAGE_V3,
                producer(),
                vec![],
                None,
                serde_json::to_value(TaskTokenUsageV3::charge_only(3)).unwrap(),
            )
            .unwrap()
            .0;
        f.store
            .observe_task_usage(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: first.id().into(),
                    charged_tokens: 3,
                    usage_id: first_usage_id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
        let settled = TaskExecutionRecordV1::Settled {
            attempt_id: first.id().into(),
            charged_tokens: 7,
            result: TaskAttemptResultV1::Failed {
                diagnostic_id: diagnostic.clone(),
                feedback_id: None,
            },
            raw_artifact_ids: vec![],
            usage_id: None,
        };
        f.store
            .settle_task_attempt(&f.cas, &lease, settled.clone(), &f.authority)
            .unwrap();
        let sequence = f.state().next_sequence;
        // The writer must neither replace nor re-encode an existing terminal receipt.
        f.store
            .settle_task_attempt(&f.cas, &lease, settled.clone(), &f.authority)
            .unwrap();
        assert_eq!(f.state().next_sequence, sequence);
        assert!(
            append(
                &mut f,
                &lease,
                TASK_EXECUTION_RECORD_V5,
                serde_json::to_value(&settled).unwrap()
            )
            .is_err()
        );
        assert_eq!(f.state().next_sequence, sequence);
        let active = f
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
            .start_task_attempt(&f.cas, &lease, &active, &f.authority)
            .unwrap();
        let sibling = f
            .store
            .reserve_and_bind_task_attempt(
                &f.cas,
                &lease,
                "root.nodes.sibling",
                &context,
                &f.authority,
            )
            .unwrap();
        assert_eq!(active.reservation().tokens, 10);
        assert_eq!(sibling.reservation().tokens, 10);
        let usage = TaskTokenUsageV3 {
            input_tokens: Some(u128::from(u64::MAX).into()),
            chargeable_tokens: actual.into(),
            ..Default::default()
        };
        f.store
            .record_task_attempt_wall(&crate::TaskAttemptWall {
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
        let usage_id = f
            .cas
            .put_artifact(
                TASK_TOKEN_USAGE_V3,
                producer(),
                vec![],
                None,
                serde_json::to_value(&usage).unwrap(),
            )
            .unwrap()
            .0;
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
            assert!(
                f.store
                    .start_task_attempt(&f.cas, &lease, &sibling, &f.authority)
                    .is_err()
            );
            assert!(
                f.store
                    .start_task_attempt(&f.cas, &next, &sibling, &f.authority)
                    .is_err()
            );
            assert!(f.state().execution.as_ref().unwrap().budget.breached());
            f.store
                .recover_task_attempts_at(&f.cas, &next, time)
                .unwrap();
        } else {
            for amount in [actual, actual, 1] {
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
                let state = f.state();
                let execution = state.execution.as_ref().unwrap();
                assert_eq!(execution.budget.committed_tokens(), actual + 7);
                assert_eq!(execution.budget.reserved_tokens(), 10);
                assert_eq!(execution.budget.begun_attempts(), 2);
                let error = f
                    .store
                    .start_task_attempt(&f.cas, &lease, &sibling, &f.authority)
                    .unwrap_err();
                assert!(error.to_string().contains("not dispatchable"), "{error}");
                assert_eq!(f.state().next_sequence, state.next_sequence);
            }
            let terminal = TaskExecutionRecordV1::Settled {
                attempt_id: active.id().into(),
                charged_tokens: 3,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: diagnostic,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: Some(usage_id.clone()),
            };
            f.store
                .settle_task_attempt(&f.cas, &lease, terminal.clone(), &f.authority)
                .unwrap();
            let sequence = f.state().next_sequence;
            f.store
                .settle_task_attempt(&f.cas, &lease, terminal, &f.authority)
                .unwrap();
            assert_eq!(f.state().next_sequence, sequence);
            f.store
                .release_task_attempt(&f.cas, &lease, &sibling, "budget breached before dispatch")
                .unwrap();
        }
        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), actual + 7);
        assert!(execution.budget.breached());
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.begun_attempts(), 2);
        assert_eq!(
            execution.budget.remaining_limits().deadline_unix_ms,
            f.revision.limits.deadline_unix_ms
        );
        let active_accounting = execution
            .attempt_accounting()
            .into_iter()
            .find(|attempt| attempt.attempt_id == active.id())
            .unwrap();
        assert_eq!(active_accounting.charged_tokens, actual);
        assert_eq!(active_accounting.reservation.tokens, 10);
        assert_eq!(active_accounting.plan_id, f.plan_id);
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
        let first_settled = records
            .iter()
            .find(|entry| {
                matches!(&entry.record,
            TaskExecutionRecordV1::Settled { attempt_id, .. } if attempt_id == first.id())
            })
            .unwrap();
        assert_eq!(
            first_settled.envelope.artifact_type,
            TASK_EXECUTION_RECORD_V5
        );
        assert_eq!(first_settled.envelope.payload["charged_tokens"], "7");
        let current = records
            .iter()
            .find(|entry| {
                matches!(&entry.record,
            TaskExecutionRecordV1::Settled { attempt_id, .. } if attempt_id == active.id())
            })
            .unwrap();
        assert_eq!(current.envelope.artifact_type, TASK_EXECUTION_RECORD_V5);
        assert_eq!(
            current.envelope.payload["charged_tokens"],
            if recover {
                actual.to_string()
            } else {
                "3".into()
            }
        );
        if !recover {
            let observed = records
                .iter()
                .find(|entry| {
                    matches!(&entry.record,
                TaskExecutionRecordV1::UsageObserved { attempt_id, charged_tokens, .. }
                if attempt_id == active.id() && *charged_tokens == actual)
                })
                .unwrap();
            assert_eq!(observed.envelope.artifact_type, TASK_EXECUTION_RECORD_V5);
            assert_eq!(
                observed.envelope.payload["charged_tokens"],
                actual.to_string()
            );
        }
        let TaskExecutionRecordV1::Settled {
            usage_id: Some(usage_id),
            ..
        } = &current.record
        else {
            panic!("missing recovered usage");
        };
        let exact: TaskTokenUsageV3 = payload(&f.cas, usage_id, TASK_TOKEN_USAGE_V3).unwrap();
        assert_eq!(exact, usage);
    }
}

#[test]
fn execution_record_readers_preserve_each_declared_counter_domain() {
    let f = Fixture::new(false);
    let usage_id = f
        .cas
        .put_json(&json!({"usage":"observed evidence"}))
        .unwrap();
    let record = |charged_tokens: serde_json::Value| {
        json!({
            "kind":"usage_observed", "attempt_id":"A".repeat(26),
            "charged_tokens":charged_tokens, "usage_id":usage_id, "raw_artifact_ids":[],
        })
    };
    let wide = u128::from(u64::MAX) + 7;
    for (value, expected) in [(json!("7"), 7), (json!(wide.to_string()), wide)] {
        let (id, envelope) = f
            .cas
            .put_artifact(
                TASK_EXECUTION_RECORD_V5,
                producer(),
                vec![],
                None,
                record(value),
            )
            .unwrap();
        let read = execution::read_execution_record(&f.cas, &id).unwrap();
        assert_eq!(read.envelope, envelope);
        assert!(
            matches!(read.record, TaskExecutionRecordV1::UsageObserved { charged_tokens, .. } if charged_tokens == expected)
        );
    }
    // Exact charges travel only as canonical decimal text, never as a JSON number.
    for value in [
        json!(7),
        json!("07"),
        json!(""),
        json!("340282366920938463463374607431768211456"),
    ] {
        let (id, _) = f
            .cas
            .put_artifact(
                TASK_EXECUTION_RECORD_V5,
                producer(),
                vec![],
                None,
                record(value.clone()),
            )
            .unwrap();
        assert!(
            execution::read_execution_record(&f.cas, &id).is_err(),
            "{value} is not a canonical charge"
        );
    }
    let normalized = TaskExecutionRecordV1::UsageObserved {
        attempt_id: "A".repeat(26),
        charged_tokens: wide,
        usage_id,
        raw_artifact_ids: vec![],
    };
    normalized.validate().unwrap();
    let encoded = serde_json::to_value(&normalized).unwrap();
    assert_eq!(encoded["charged_tokens"], json!(wide.to_string()));
    assert_eq!(
        serde_json::from_value::<TaskExecutionRecordV1>(encoded).unwrap(),
        normalized
    );
}

#[test]
fn native_usage_observation_binds_context_producer_floor_and_preserves_late_charge() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"context":"native usage"})).unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let observation = TaskUsageObservationV1 {
        reported_usage: Some(TaskTokenUsageV3::charge_only(3)),
        charge_complete: false,
    };
    let producer = Producer::Attempt {
        run_id: task_run_id(lease.task_id()).unwrap(),
        node_id: "root.nodes.write".into(),
        attempt_id: attempt.id().into(),
    };
    let usage_id = f.cas.put_json(&json!({"chargeable_tokens":"10"})).unwrap();
    let charge = u128::from(attempt.reservation().tokens);
    for variant in [
        "producer",
        "context",
        "subject",
        "duplicate",
        "charge",
        "malformed",
    ] {
        let mut producer = producer.clone();
        let mut refs = vec![context.clone()];
        let mut subject = None;
        let mut value = serde_json::to_value(&observation).unwrap();
        if variant == "producer" {
            if let Producer::Attempt { node_id, .. } = &mut producer {
                *node_id = "root.nodes.other".into();
            }
        }
        if variant == "context" {
            refs.clear();
        }
        if variant == "subject" {
            subject = Some(context.clone());
        }
        if variant == "malformed" {
            value["reported_usage"] = json!(null);
        }
        let id = f
            .cas
            .put_artifact(TASK_USAGE_OBSERVATION_V1, producer, refs, subject, value)
            .unwrap()
            .0;
        let raw = if variant == "duplicate" {
            vec![id.clone(), id]
        } else {
            vec![id]
        };
        let before = f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap();
        assert!(
            f.store
                .observe_task_usage(
                    &f.cas,
                    &lease,
                    TaskExecutionRecordV1::UsageObserved {
                        attempt_id: attempt.id().into(),
                        charged_tokens: if variant == "charge" {
                            charge - 1
                        } else {
                            charge
                        },
                        usage_id: usage_id.clone(),
                        raw_artifact_ids: raw,
                    }
                )
                .is_err(),
            "{variant}"
        );
        assert_eq!(
            f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap(),
            before
        );
    }
    let id = execution::usage_observation::capture_task_usage_observation(
        &f.cas,
        producer,
        &context,
        &observation,
    )
    .unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                usage_id: usage_id.clone(),
                raw_artifact_ids: vec![id.clone()],
            },
        )
        .unwrap();
    let late = u128::from(u64::MAX) + 7;
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: late,
                usage_id: usage_id.clone(),
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    let diagnostic_id = f
        .cas
        .put_json(&json!({"error":"malformed native usage"}))
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                raw_artifact_ids: vec![id],
                usage_id: Some(usage_id),
            },
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), late);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.attempt_accounting()[0].charged_tokens, late);
}
