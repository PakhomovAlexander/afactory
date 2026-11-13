//! Independent public-writer histories with explicit observation clocks and expected boundaries.
use super::*;
use af_contracts::task::event::TaskChangeV1;
use af_contracts::task::wake::*;
use af_runtime::wake::{envelope, project};
use af_store::store::task::{TaskLease, read_task_transition, task_run_id};

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn subscription(f: &Fixture) -> WakeSubscriptionV1 {
    let events = f
        .store
        .replay(&task_run_id(&f.task.task_id).unwrap())
        .unwrap();
    let opened = read_task_transition(&events[0]).unwrap().now_unix_ms;
    let mut value = WakeSubscriptionV1 {
        schema: WAKE_SUBSCRIPTION_V1.into(),
        subscription_id: digest('1'),
        registration_key: digest('2'),
        configuration_digest: digest('3'),
        task: WakeTaskV1 {
            store_incarnation: digest('4'),
            task_id: f.task.task_id.clone(),
            task_run_id: task_run_id(&f.task.task_id).unwrap(),
            opening_event_id: events[0].event_id.clone(),
        },
        recipient_ref: digest('5'),
        registered_unix_ms: opened,
        expires_unix_ms: opened + 100_000_000,
        max_observations: 128,
    };
    value.configuration_digest = af_runtime::wake::configuration_digest(&value).unwrap();
    value
}
fn stamp(f: &Fixture) -> u64 {
    let events = f
        .store
        .replay(&task_run_id(&f.task.task_id).unwrap())
        .unwrap();
    events
        .iter()
        .map(|e| {
            use af_store::store::task as stored;
            if stored::is_execution_started(e) {
                stored::read_execution_started(e).unwrap().now_unix_ms
            } else if stored::is_provider_event(e) {
                stored::read_provider_event(e).unwrap().now_unix_ms
            } else if stored::gate_retry::is_gate_retried(e) {
                stored::gate_retry::read_gate_retried(e)
                    .unwrap()
                    .now_unix_ms
            } else {
                read_task_transition(e).unwrap().now_unix_ms
            }
        })
        .max()
        .unwrap()
        + 1
}
fn finish(
    cas: &Cas,
    store: &mut EventStore,
    revision_id: &str,
    task: &TaskRevisionV1,
    lease: &TaskLease,
    authority: &dyn af_store::store::task::TaskAuthority,
) -> af_contracts::RunEvent {
    let result = TaskResultV1 {
        task_revision_id: revision_id.to_owned(),
        execution: TaskExecutionV1::Incomplete,
        acceptance: TaskAcceptanceV1::Inconclusive,
        domain_conclusion: "cancelled".into(),
        outputs: BTreeMap::new(),
        evidence: BTreeSet::new(),
        missing_obligations: task.acceptance.keys().cloned().collect(),
    };
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![revision_id.to_owned()],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    store.finish_task(cas, lease, &id, authority).unwrap()
}

#[test]
fn wake_downtime_replays_wait_resume_finish_and_ignores_cleanup() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let sub = subscription(&f);
    let wait = f
        .store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    let finished = finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let before = project(&f.store, &f.cas, &sub, None, stamp(&f)).unwrap();
    assert_eq!(
        before
            .observations
            .iter()
            .map(|o| o.kind)
            .collect::<Vec<_>>(),
        [WakeKindV1::AttentionRequired, WakeKindV1::Completed]
    );
    assert_eq!(before.observations[0].causal.sequence, wait.sequence);
    assert_eq!(before.observations[1].causal.sequence, finished.sequence);
    assert!(before.observations[1].episode > before.observations[0].episode);
    let envelopes: Vec<_> = before
        .observations
        .iter()
        .cloned()
        .map(|o| envelope(&sub, o).unwrap())
        .collect();
    f.store
        .record_task_storage_sweep(
            &f.cas,
            &lease,
            af_contracts::task::storage_sweep::TaskStorageSweepV2 {
                schema: af_contracts::task::storage_sweep::TASK_STORAGE_SWEEP_V2.into(),
                removals: vec![],
                omitted_removals: 1,
                failures: vec![],
                omitted_failures: 0,
                freed_bytes: 1,
                stop: af_contracts::task::storage_sweep::StorageSweepStopV2::Fits,
                max_bytes: 100,
                total_before: 2,
                total_after: Some(1),
            },
        )
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let after = project(&f.store, &f.cas, &sub, None, stamp(&f) + 10_000).unwrap();
    assert_eq!(before.observations, after.observations);
    assert_eq!(
        envelopes,
        after
            .observations
            .into_iter()
            .map(|o| envelope(&sub, o).unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        project(&f.store, &f.cas, &sub, Some(&before.checkpoint), stamp(&f))
            .unwrap()
            .observations
            .is_empty()
    );
    let mut other = sub.clone();
    other.subscription_id = digest('6');
    assert_ne!(
        envelope(&other, before.observations[0].clone())
            .unwrap()
            .delivery_id,
        envelopes[0].delivery_id
    );
    other = sub.clone();
    other.task.store_incarnation = digest('7');
    other.configuration_digest = af_runtime::wake::configuration_digest(&other).unwrap();
    let changed = project(&f.store, &f.cas, &other, None, stamp(&f)).unwrap();
    assert_ne!(
        changed.observations[0].observation_id,
        before.observations[0].observation_id
    );
}

#[test]
fn wake_immediate_finish_is_distinct_from_expired_execution_and_replay_is_stable() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let sub = subscription(&f);
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let deadline = state.lease_until_unix_ms();
    assert!(
        project(&f.store, &f.cas, &sub, None, deadline - 1)
            .unwrap()
            .observations
            .is_empty()
    );
    let stopped = project(&f.store, &f.cas, &sub, None, deadline)
        .unwrap()
        .observations;
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].kind, WakeKindV1::ExecutionStopped);
    assert!(
        matches!(stopped[0].evidence,WakeEvidenceV1::LeaseExpired{epoch:1,deadline_unix_ms} if deadline_unix_ms==deadline)
    );
    assert_eq!(
        stopped[0].observation_id,
        project(&f.store, &f.cas, &sub, None, deadline + 500)
            .unwrap()
            .observations[0]
            .observation_id
    );
    // Observing fake expiry does not mutate Store history or prevent a real current writer
    // from settling. A fresh scan must revalidate and suppress the superseded expiry.
    let event = finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let completed = project(&f.store, &f.cas, &sub, None, stamp(&f))
        .unwrap()
        .observations;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].kind, WakeKindV1::Completed);
    assert_eq!(completed[0].causal.event_id, event.event_id);
    assert!(completed[0].result_id.is_some());
}

#[test]
fn wake_unknown_corrupt_history_and_mismatched_cursors_fail_closed() {
    let mut f = Fixture::new(SUCCESS);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let sub = subscription(&f);
    for lane in ["run", "opening", "version"] {
        let mut bad = sub.clone();
        match lane {
            "run" => bad.task.task_run_id = format!("task:{}", digest('a')),
            "opening" => bad.task.opening_event_id = "a".repeat(26),
            _ => bad.schema = "af/TaskWakeSubscription@0".into(),
        }
        assert!(
            project(&f.store, &f.cas, &bad, None, stamp(&f)).is_err(),
            "{lane}"
        );
    }
    let checkpoint = project(&f.store, &f.cas, &sub, None, stamp(&f))
        .unwrap()
        .checkpoint;
    let mut bytes = serde_json::to_value(&checkpoint).unwrap();
    bytes["through"]["sequence"] = json!(999);
    assert!(
        af_runtime::wake::restore_checkpoint(
            &f.store,
            &f.cas,
            &sub,
            &serde_json::to_vec(&bytes).unwrap()
        )
        .is_err()
    );
    let original = f.store.replay(&sub.task.task_run_id).unwrap();
    assert!(
        project(&f.store, &f.cas, &sub, None, stamp(&f))
            .unwrap()
            .observations
            .is_empty(),
        "unadmitted expiry is not completion"
    );
    assert_eq!(f.store.replay(&sub.task.task_run_id).unwrap(), original);
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let db = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    db.execute("UPDATE events SET payload=json_set(payload,'$.change.kind','unknown_wake') WHERE sequence=0",[]).unwrap();
    assert!(
        project(
            &f.store,
            &f.cas,
            &sub,
            None,
            stamp_before_corruption(&original)
        )
        .is_err()
    );
}
fn stamp_before_corruption(events: &[af_contracts::RunEvent]) -> u64 {
    read_task_transition(&events[0]).unwrap().now_unix_ms + 1_000_000
}

#[test]
fn wake_derived_waiting_uses_the_shared_checked_state_and_recorded_cancellation() {
    use af_contracts::task::{agent_loop::*, execution::*, run_report::*};
    use af_runtime::wake::{RecordedRunFacts, project_with_run_facts};
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let invocation = f
        .cas
        .put_artifact(
            TASK_INVOCATION_V1,
            producer(),
            vec![f.plan_id.clone()],
            None,
            serde_json::to_value(TaskInvocationV1 {
                plan_id: f.plan_id.clone(),
                node: "root.inputs".into(),
                inputs: BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .record_task_invocation(&f.cas, &lease, &invocation, &authority)
        .unwrap();
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let diagnostic = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(TaskDiagnosticV1::capture(
                "Task token limit protects verification",
            ))
            .unwrap(),
        )
        .unwrap()
        .0;
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        nodes: state
            .execution
            .as_ref()
            .unwrap()
            .graph
            .order
            .iter()
            .map(|node| TaskNodeReportV1 {
                node: node.clone(),
                outcome: TaskNodeOutcomeV1::Failed {
                    diagnostic_id: diagnostic.clone(),
                    class: TaskFailureClassV1::Resources,
                },
            })
            .collect(),
    };
    let report_id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V3,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .record_task_run_report(&f.cas, &lease, &report_id)
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let sub = subscription(&f);
    let at = stamp(&f);
    let observations = project(&f.store, &f.cas, &sub, None, at)
        .unwrap()
        .observations;
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].kind, WakeKindV1::AttentionRequired);
    assert_eq!(
        observations[0].state.waiting_reason, None,
        "waiting is derived without a Waiting event"
    );
    let process = TaskRunProcessV1 {
        schema: TASK_RUN_PROCESS_V1.into(),
        task_id: f.task.task_id.clone(),
        run: 1,
        pid: 123,
        host: "test-host".into(),
        writer: "wake-writer".into(),
        started_unix_ms: sub.registered_unix_ms,
        detached: false,
        log: None,
    };
    let cancel = TaskCancelRequestV1 {
        schema: TASK_CANCEL_REQUEST_V1.into(),
        task_id: f.task.task_id.clone(),
        run: 1,
        epoch: lease.epoch(),
        writer: process.writer.clone(),
        pid: process.pid,
        host: process.host.clone(),
        requester: TaskCancelRequesterV1 {
            user: "operator".into(),
            host: "test-host".into(),
            pid: 124,
        },
        requested_unix_ms: sub.registered_unix_ms,
    };
    let mut facts = vec![RecordedRunFacts {
        process,
        cancellation: Some(cancel),
    }];
    let cancelled = project_with_run_facts(&f.store, &f.cas, &sub, None, at, &facts)
        .unwrap()
        .observations;
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].kind, WakeKindV1::ExecutionStopped);
    assert_eq!(
        cancelled[0].state.cause,
        Some(af_contracts::task::state::TaskCauseV1::HostCancelled)
    );
    facts[0].cancellation.as_mut().unwrap().epoch += 1;
    assert!(project_with_run_facts(&f.store, &f.cas, &sub, None, at, &facts).is_err());
    facts[0].cancellation.as_mut().unwrap().epoch = lease.epoch();
    let next = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "wake-resumed", 60000)
        .unwrap();
    f.store.begin_task_execution(&f.cas, &next).unwrap();
    let diagnostic = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(TaskDiagnosticV1::capture(
                "The run was cancelled by its host",
            ))
            .unwrap(),
        )
        .unwrap()
        .0;
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        nodes: state
            .execution
            .as_ref()
            .unwrap()
            .graph
            .order
            .iter()
            .map(|node| TaskNodeReportV1 {
                node: node.clone(),
                outcome: TaskNodeOutcomeV1::Failed {
                    diagnostic_id: diagnostic.clone(),
                    class: TaskFailureClassV1::Execution,
                },
            })
            .collect(),
    };
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V3,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store.record_task_run_report(&f.cas, &next, &id).unwrap();
    finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &next,
        &authority,
    );
    let final_observations =
        project_with_run_facts(&f.store, &f.cas, &sub, None, stamp(&f), &facts)
            .unwrap()
            .observations;
    assert_eq!(
        final_observations
            .iter()
            .map(|o| o.kind)
            .collect::<Vec<_>>(),
        [WakeKindV1::ExecutionStopped, WakeKindV1::Completed]
    );
    let completed = &final_observations[1];
    assert_eq!(
        completed.state.outcome,
        Some(af_contracts::task::state::TaskOutcomeV1::Cancelled)
    );
    assert!(completed.result_id.is_some());
    assert!(completed.episode > final_observations[0].episode);
}

#[test]
fn wake_registration_and_retained_envelope_identities_fail_closed() {
    use af_runtime::wake::{configuration_digest, decode_envelope, same_registration};
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let mut sub = subscription(&f);
    sub.configuration_digest = configuration_digest(&sub).unwrap();
    assert!(same_registration(&sub, &sub).unwrap());
    let mut changed = sub.clone();
    changed.max_observations += 1;
    changed.configuration_digest = configuration_digest(&changed).unwrap();
    assert!(same_registration(&sub, &changed).is_err());
    finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let observation = project(&f.store, &f.cas, &sub, None, stamp(&f))
        .unwrap()
        .observations
        .remove(0);
    let retained = envelope(&sub, observation).unwrap();
    let bytes = serde_json::to_vec(&retained).unwrap();
    assert_eq!(decode_envelope(&bytes).unwrap(), retained);
    for field in ["delivery_id", "payload_digest"] {
        let mut bad = serde_json::to_value(&retained).unwrap();
        bad[field] = json!(digest('f'));
        assert!(decode_envelope(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = serde_json::to_value(&retained).unwrap();
    bad["observation"]["state"]["cause"] = json!("deadline");
    assert!(decode_envelope(&serde_json::to_vec(&bad).unwrap()).is_err());
}

#[test]
fn wake_late_accounting_keeps_the_finished_cause_and_delivery_bytes() {
    use af_contracts::task::execution::TaskExecutionRecordV1;
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    let attempt = execution.attempt_accounting().remove(0).attempt_id;
    let output = execution.outputs["root.nodes.write"].1.outputs["output"].clone();
    let result = TaskResultV1 {
        task_revision_id: f.revision_id.clone(),
        execution: TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: "checked document produced".into(),
        evidence: output.artifact_ids.iter().cloned().collect(),
        outputs: BTreeMap::from([("document".into(), output)]),
        missing_obligations: BTreeSet::new(),
    };
    let result_id = f
        .cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&result_id).unwrap();
    drop(runtime);
    let sub = subscription(&f);
    let before = project(&f.store, &f.cas, &sub, None, stamp(&f))
        .unwrap()
        .observations;
    assert_eq!(before.len(), 1);
    let retained = envelope(&sub, before[0].clone()).unwrap();
    let usage_id = f.cas.put_json(&json!({"chargeable_tokens":1})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt,
                charged_tokens: 1,
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let after = project(&f.store, &f.cas, &sub, None, stamp(&f))
        .unwrap()
        .observations;
    assert_eq!(before, after);
    assert_eq!(retained, envelope(&sub, after[0].clone()).unwrap());
}

fn sweep(f: &mut Fixture, lease: &TaskLease) {
    f.store
        .record_task_storage_sweep(
            &f.cas,
            lease,
            af_contracts::task::storage_sweep::TaskStorageSweepV2 {
                schema: af_contracts::task::storage_sweep::TASK_STORAGE_SWEEP_V2.into(),
                removals: vec![],
                omitted_removals: 1,
                failures: vec![],
                omitted_failures: 0,
                freed_bytes: 1,
                stop: af_contracts::task::storage_sweep::StorageSweepStopV2::Fits,
                max_bytes: 100,
                total_before: 2,
                total_after: Some(1),
            },
        )
        .unwrap();
}

#[test]
fn wake_abandoned_separate_sweep_lease_preserves_boundary_and_real_recovery_reopens_it() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let sub = subscription(&f);
    let before = project(&f.store, &f.cas, &sub, None, stamp(&f)).unwrap();
    assert_eq!(before.observations.len(), 1);
    assert_eq!(before.observations[0].kind, WakeKindV1::ExecutionStopped);
    let maintenance = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "storage-sweep", 60000)
        .unwrap();
    sweep(&mut f, &maintenance);
    f.store.release_task_lease(&f.cas, &maintenance).unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let after = project(&f.store, &f.cas, &sub, None, stamp(&f)).unwrap();
    assert_eq!(before.observations, after.observations);
    assert_eq!(
        envelope(&sub, before.observations[0].clone()).unwrap(),
        envelope(&sub, after.observations[0].clone()).unwrap()
    );
    assert!(
        project(&f.store, &f.cas, &sub, Some(&before.checkpoint), stamp(&f))
            .unwrap()
            .observations
            .is_empty()
    );
    let recovered = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "recovery", 60000)
        .unwrap();
    f.store.begin_task_execution(&f.cas, &recovered).unwrap();
    f.store.release_task_lease(&f.cas, &recovered).unwrap();
    let later = project(&f.store, &f.cas, &sub, Some(&after.checkpoint), stamp(&f)).unwrap();
    assert_eq!(later.observations.len(), 1);
    assert!(later.observations[0].episode > before.observations[0].episode);
    assert_ne!(
        later.observations[0].observation_id,
        before.observations[0].observation_id
    );
}

#[test]
fn wake_long_history_advances_checked_pages_to_completion_and_restores_checkpoint() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    // Advance expiry even when consecutive checked writes share the same millisecond.
    for index in 1..=4100 {
        f.store
            .renew_task_lease(&f.cas, &lease, 60_000 + index)
            .unwrap();
    }
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    let finished = finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let sub = subscription(&f);
    let at = stamp(&f);
    let first = project(&f.store, &f.cas, &sub, None, at).unwrap();
    assert_eq!(first.through.sequence, 4095);
    assert!(!first.caught_up);
    assert_eq!(first.observations.len(), 1);
    let bytes = serde_json::to_vec(&first.checkpoint).unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let checkpoint = af_runtime::wake::restore_checkpoint(&f.store, &f.cas, &sub, &bytes).unwrap();
    let second = project(&f.store, &f.cas, &sub, Some(&checkpoint), at).unwrap();
    assert!(second.caught_up);
    assert_eq!(second.through.sequence, finished.sequence);
    assert_eq!(
        second
            .observations
            .iter()
            .map(|o| o.kind)
            .collect::<Vec<_>>(),
        [WakeKindV1::AttentionRequired, WakeKindV1::Completed]
    );
    let replay = project(&f.store, &f.cas, &sub, Some(&checkpoint), at).unwrap();
    assert_eq!(second.observations, replay.observations);
    assert!(
        project(&f.store, &f.cas, &sub, Some(&second.checkpoint), at)
            .unwrap()
            .observations
            .is_empty()
    );
    let mut malformed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    malformed["observations"] = json!(0);
    assert!(
        af_runtime::wake::restore_checkpoint(
            &f.store,
            &f.cas,
            &sub,
            &serde_json::to_vec(&malformed).unwrap()
        )
        .is_err()
    );
    let db = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    db.execute(
        "UPDATE events SET payload=json_set(payload,'$.change.kind','unknown') WHERE sequence=2",
        [],
    )
    .unwrap();
    assert!(project(&f.store, &f.cas, &sub, Some(&checkpoint), at).is_err());
}

#[test]
fn wake_derived_waiting_separate_sweep_preserves_observations_and_delivery_bytes() {
    use af_contracts::task::{execution::*, run_report::*};
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let invocation = f
        .cas
        .put_artifact(
            TASK_INVOCATION_V1,
            producer(),
            vec![f.plan_id.clone()],
            None,
            serde_json::to_value(TaskInvocationV1 {
                plan_id: f.plan_id.clone(),
                node: "root.inputs".into(),
                inputs: BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .record_task_invocation(&f.cas, &lease, &invocation, &authority)
        .unwrap();
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let diagnostic = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(TaskDiagnosticV1::capture(
                "Task token limit protects verification",
            ))
            .unwrap(),
        )
        .unwrap()
        .0;
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        nodes: state
            .execution
            .as_ref()
            .unwrap()
            .graph
            .order
            .iter()
            .map(|node| TaskNodeReportV1 {
                node: node.clone(),
                outcome: TaskNodeOutcomeV1::Failed {
                    diagnostic_id: diagnostic.clone(),
                    class: TaskFailureClassV1::Resources,
                },
            })
            .collect(),
    };
    let report_id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V3,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .record_task_run_report(&f.cas, &lease, &report_id)
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let sub = subscription(&f);
    let at = stamp(&f);
    let observations = project(&f.store, &f.cas, &sub, None, at)
        .unwrap()
        .observations;
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].kind, WakeKindV1::AttentionRequired);
    assert_eq!(
        observations[0].state.waiting_reason, None,
        "waiting is derived without a Waiting event"
    );
    // Actual separate storage::record_on_task-style maintenance writer, not a Finished Task.
    let maintenance = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "storage-sweep", 60000)
        .unwrap();
    sweep(&mut f, &maintenance);
    f.store.release_task_lease(&f.cas, &maintenance).unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let after_sweep = project(&f.store, &f.cas, &sub, None, stamp(&f)).unwrap();
    assert_eq!(observations, after_sweep.observations);
    assert_eq!(
        envelope(&sub, observations[0].clone()).unwrap(),
        envelope(&sub, after_sweep.observations[0].clone()).unwrap()
    );
}

#[cfg(target_os = "linux")]
fn register(f: &Fixture, key: char, limit: u64) -> af_store::store::wake::WakeRegistration {
    use af_store::store::wake::{CatchUp, WakeRegistration};
    let mut sub = subscription(f);
    af_store::local_fs::narrow_owned_dir(f._directory.path()).unwrap();
    sub.task.store_incarnation = f.store.admit_wake_incarnation().unwrap();
    sub.registration_key = digest(key);
    sub.subscription_id = digest(key);
    sub.max_observations = limit;
    sub.configuration_digest = af_runtime::wake::configuration_digest(&sub).unwrap();
    let last = f
        .store
        .replay(&sub.task.task_run_id)
        .unwrap()
        .pop()
        .unwrap();
    let registration = WakeRegistration {
        schema: "af/task-wake-registration@1".into(),
        subscription: sub,
        policy: CatchUp::FromOpening,
        registered_prefix: WakeBoundaryV1 {
            sequence: last.sequence,
            event_id: last.event_id,
        },
        launch_intent: false,
    };
    let value = serde_json::to_value(&registration).unwrap();
    assert!(schema_registry::validator("task-wake-registration-v1.json").is_valid(&value));
    f.store.register_wake(&f.cas, &registration).unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn wake_journal_registration_atomic_replay_and_collection_barrier_use_real_store_transactions() {
    use af_runtime::wake::journal::reconcile;
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let registration = register(&f, 'a', 32);
    assert_eq!(
        f.store.register_wake(&f.cas, &registration).unwrap(),
        registration
    );
    let id = &registration.subscription.subscription_id;
    let mut conflict = registration.clone();
    conflict.launch_intent = true;
    assert!(f.store.register_wake(&f.cas, &conflict).is_err());
    conflict = registration.clone();
    conflict.subscription.task.store_incarnation = digest('f');
    assert!(f.store.register_wake(&f.cas, &conflict).is_err());
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    let finished = finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let task_ids = BTreeSet::from([f.task.task_id.clone()]);
    assert!(
        f.store.apply_task_collection_of(&f.cas, &task_ids).is_err(),
        "unextracted source must be retained"
    );
    assert!(f.store.task_tombstone(&f.task.task_id).unwrap().is_none());
    assert!(reconcile(&f.store, &f.cas, id, stamp(&f)).unwrap());
    let before = f.store.inspect_wake(id).unwrap();
    assert_eq!(
        before
            .envelopes
            .iter()
            .map(|e| e.observation.kind)
            .collect::<Vec<_>>(),
        [WakeKindV1::AttentionRequired, WakeKindV1::Completed]
    );
    assert_eq!(
        before.envelopes[1].observation.causal.sequence,
        finished.sequence
    );
    let checkpoint: serde_json::Value =
        serde_json::from_str(before.checkpoint.as_ref().unwrap()).unwrap();
    assert!(schema_registry::validator("task-wake-checkpoint-v1.json").is_valid(&checkpoint));
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert!(reconcile(&f.store, &f.cas, id, stamp(&f)).unwrap());
    assert_eq!(
        before.envelopes,
        f.store.inspect_wake(id).unwrap().envelopes
    );
    let result_id = before.envelopes[1]
        .observation
        .result_id
        .as_ref()
        .unwrap()
        .clone();
    let collected = f.store.apply_task_collection_of(&f.cas, &task_ids).unwrap();
    assert_eq!(collected.tombstoned, [f.task.task_id.clone()]);
    assert!(
        f.cas.get_artifact(&result_id).is_err(),
        "notification hashes must not pin CAS closure"
    );
    let after = f.store.inspect_wake(id).unwrap();
    assert!(!after.source_available);
    assert_eq!(before.envelopes, after.envelopes);
    assert!(
        reconcile(
            &f.store,
            &f.cas,
            id,
            stamp_before_corruption(
                &f.store
                    .replay(&registration.subscription.task.task_run_id)
                    .unwrap()
            )
        )
        .is_err()
    );
    assert!(f.store.register_wake(&f.cas, &registration).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn wake_journal_stale_extractors_and_capacity_failure_leave_cursor_and_outbox_atomic() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let registration = register(&f, 'a', 32);
    let sub = &registration.subscription;
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    let first = project(&f.store, &f.cas, sub, None, stamp(&f)).unwrap();
    let envelopes = first
        .observations
        .iter()
        .cloned()
        .map(|o| envelope(sub, o).unwrap())
        .collect::<Vec<_>>();
    let cursor = serde_json::to_string(&first.checkpoint).unwrap();
    let other = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    f.store
        .commit_wake_batch(
            &f.cas,
            &sub.subscription_id,
            None,
            &cursor,
            first.through.sequence,
            &first.source,
            &envelopes,
        )
        .unwrap();
    assert!(
        other
            .commit_wake_batch(
                &f.cas,
                &sub.subscription_id,
                None,
                &cursor,
                first.through.sequence,
                &first.source,
                &envelopes
            )
            .is_err(),
        "stale reconciler must not commit"
    );
    let before = f.store.inspect_wake(&sub.subscription_id).unwrap();
    let mut conflicting = envelopes[0].clone();
    conflicting.expires_unix_ms -= 1;
    // Shape-valid but changed payload bytes under an existing key are refused.
    assert!(
        f.store
            .commit_wake_batch(
                &f.cas,
                &sub.subscription_id,
                Some(&cursor),
                &cursor,
                first.through.sequence,
                &first.source,
                &[conflicting]
            )
            .is_err()
    );
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    assert!(
        f.store
            .commit_wake_batch(
                &f.cas,
                &sub.subscription_id,
                Some(&cursor),
                &cursor,
                first.through.sequence,
                &first.source,
                &envelopes
            )
            .is_err(),
        "source prefix must be revalidated at commit"
    );
    assert_eq!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .checkpoint,
        before.checkpoint
    );
    assert_eq!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .envelopes,
        before.envelopes
    );
    let limited = register(&f, 'b', 1);
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    assert!(
        af_runtime::wake::journal::reconcile(
            &f.store,
            &f.cas,
            &limited.subscription.subscription_id,
            stamp(&f)
        )
        .is_err()
    );
    let empty = f
        .store
        .inspect_wake(&limited.subscription.subscription_id)
        .unwrap();
    assert!(empty.checkpoint.is_none());
    assert!(empty.envelopes.is_empty());
    // Distinct subscriptions remain independently observable; the failed one changes no other cursor.
    assert_eq!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .checkpoint,
        before.checkpoint
    );
    f.store.release_task_lease(&f.cas, &lease).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn wake_incarnation_reopen_preserves_identity_and_copied_store_cannot_activate() {
    let mut f = Fixture::new(SUCCESS);
    f.store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(f._directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        f.store.admit_wake_incarnation().is_err(),
        "unsafe Store modes must fail closed"
    );
    af_store::local_fs::narrow_owned_dir(f._directory.path()).unwrap();
    let original = f.store.admit_wake_incarnation().unwrap();
    assert_eq!(f.store.admit_wake_incarnation().unwrap(), original);
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert_eq!(f.store.wake_incarnation().unwrap(), original);
    let copy_root = tempfile::tempdir().unwrap();
    af_store::local_fs::narrow_owned_dir(copy_root.path()).unwrap();
    let copy_path = copy_root.path().join("events.sqlite");
    // SQLite's backup stand-in: checkpoint first, then copy the durable main file.
    let sql = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    std::fs::copy(f._directory.path().join("events.sqlite"), &copy_path).unwrap();
    let copy = EventStore::open(&copy_path).unwrap();
    assert!(copy.wake_incarnation().is_err());
    assert!(copy.admit_wake_incarnation().is_err());
    drop(copy);
    std::fs::remove_file(&copy_path).unwrap();
    let recreated = EventStore::open(&copy_path).unwrap();
    assert_ne!(recreated.admit_wake_incarnation().unwrap(), original);
}

#[test]
#[cfg(target_os = "linux")]
fn wake_journal_renewed_lease_and_failed_cursor_write_cannot_publish_partial_or_stale_extraction() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let before = f
        .store
        .replay(&task_run_id(&f.task.task_id).unwrap())
        .unwrap();
    let mut registration = register(&f, 'a', 32);
    registration.subscription.registration_key = digest('b');
    registration.launch_intent = true;
    let registration = f.store.register_wake(&f.cas, &registration).unwrap();
    let sub = &registration.subscription;
    assert!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .registration
            .launch_intent
    );
    assert_eq!(
        before,
        f.store.replay(&sub.task.task_run_id).unwrap(),
        "registration/intent must not execute or change canonical history"
    );
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let expired = project(&f.store, &f.cas, sub, None, state.lease_until_unix_ms()).unwrap();
    assert_eq!(expired.observations[0].kind, WakeKindV1::ExecutionStopped);
    let envelopes = expired
        .observations
        .iter()
        .cloned()
        .map(|o| envelope(sub, o).unwrap())
        .collect::<Vec<_>>();
    let checkpoint = serde_json::to_string(&expired.checkpoint).unwrap();
    f.store.renew_task_lease(&f.cas, &lease, 60000).unwrap();
    assert!(
        f.store
            .commit_wake_batch(
                &f.cas,
                &sub.subscription_id,
                None,
                &checkpoint,
                expired.through.sequence,
                &expired.source,
                &envelopes
            )
            .is_err()
    );
    assert!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .envelopes
            .is_empty()
    );
    finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let db = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_wake_cursor BEFORE UPDATE OF checkpoint ON wake_subscriptions_v1
        BEGIN SELECT RAISE(ABORT,'deterministic failure between outbox insertion and cursor commit'); END;").unwrap();
    assert!(
        af_runtime::wake::journal::reconcile(&f.store, &f.cas, &sub.subscription_id, stamp(&f))
            .is_err()
    );
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let rolled_back = f.store.inspect_wake(&sub.subscription_id).unwrap();
    assert!(rolled_back.checkpoint.is_none());
    assert!(rolled_back.envelopes.is_empty());
    db.execute_batch("DROP TRIGGER fail_wake_cursor").unwrap();
    let validations = std::cell::Cell::new(0);
    assert!(
        af_runtime::wake::journal::reconcile_with_run_facts(
            &f.store,
            &f.cas,
            &sub.subscription_id,
            stamp(&f),
            &[],
            || {
                validations.set(validations.get() + 1);
                if validations.get() == 2 {
                    return Err(af_store::StoreError::Conflict(
                        "recorder changed at atomic commit".into(),
                    ));
                }
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(validations.get(), 2);
    assert!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .envelopes
            .is_empty()
    );
    assert!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .checkpoint
            .is_none()
    );
    assert!(
        af_runtime::wake::journal::reconcile(&f.store, &f.cas, &sub.subscription_id, stamp(&f))
            .unwrap()
    );
    assert_eq!(
        f.store
            .inspect_wake(&sub.subscription_id)
            .unwrap()
            .envelopes[0]
            .observation
            .kind,
        WakeKindV1::Completed
    );
    // Mutation of a covered canonical row invalidates the retained continuation atomically.
    db.execute("UPDATE events SET payload=payload WHERE sequence=0", [])
        .unwrap();
    assert!(f.store.inspect_wake(&sub.subscription_id).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn wake_registration_capacity_exhaustion_retains_exact_existing_control_state() {
    let mut f = Fixture::new(SUCCESS);
    f.store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let registration = register(&f, 'a', 1);
    let events = f
        .store
        .replay(&registration.subscription.task.task_run_id)
        .unwrap();
    for i in 1..128 {
        let mut next = registration.clone();
        next.subscription.registration_key =
            af_store::content_id(&json!({"registration":i})).unwrap();
        f.store.register_wake(&f.cas, &next).unwrap();
    }
    assert_eq!(f.store.wake_registrations().unwrap().len(), 128);
    let mut overflow = registration.clone();
    overflow.subscription.registration_key = digest('f');
    assert!(f.store.register_wake(&f.cas, &overflow).is_err());
    assert_eq!(f.store.wake_registrations().unwrap().len(), 128);
    assert_eq!(
        f.store.register_wake(&f.cas, &registration).unwrap(),
        registration
    );
    assert_eq!(
        f.store
            .replay(&registration.subscription.task.task_run_id)
            .unwrap(),
        events
    );
}

#[cfg(target_os = "linux")]
fn receiver_registration(
    f: &Fixture,
) -> (
    af_contracts::task::wake::delivery::WakeRouteV1,
    af_store::store::wake::WakeRegistration,
) {
    use af_contracts::task::wake::delivery::*;
    use af_store::store::wake::{CatchUp, WakeRegistration, delivery::*};
    af_store::local_fs::narrow_owned_dir(f._directory.path()).unwrap();
    let mut sub = subscription(f);
    sub.task.store_incarnation = f.store.admit_wake_incarnation().unwrap();
    let mut route = WakeRouteV1 {
        schema: "af/task-wake-route@1".into(),
        route_id: digest('a'),
        installation_id: digest('b'),
        capability_digest: capability_digest(&digest('c')).unwrap(),
        task: sub.task.clone(),
        host: WakeHostV1::Pi,
        session: "original-session".into(),
        mode: "follow_up".into(),
        expires_unix_ms: sub.expires_unix_ms,
        installation: WakeInstallationV1 {
            executable: "/owner/node".into(),
            executable_digest: digest('d'),
            extension: Some("/owner/pi.ts".into()),
            extension_digest: Some(digest('e')),
            socket: None,
            socket_device: None,
            socket_inode: None,
        },
    };
    route.route_id = route_identity(&route).unwrap();
    f.store.install_wake_route(&route).unwrap();
    sub.recipient_ref = route.route_id.clone();
    sub.configuration_digest = af_store::store::wake::configuration_digest(&sub).unwrap();
    let last = f
        .store
        .replay(&sub.task.task_run_id)
        .unwrap()
        .pop()
        .unwrap();
    let registration = f
        .store
        .register_wake(
            &f.cas,
            &WakeRegistration {
                schema: "af/task-wake-registration@1".into(),
                subscription: sub,
                policy: CatchUp::FromOpening,
                registered_prefix: WakeBoundaryV1 {
                    sequence: last.sequence,
                    event_id: last.event_id,
                },
                launch_intent: true,
            },
        )
        .unwrap();
    (route, registration)
}

#[test]
#[cfg(target_os = "linux")]
fn wake_receiver_restart_lost_ack_fencing_native_uncertainty_and_compaction() {
    use af_contracts::task::wake::delivery::*;
    use af_store::store::wake::delivery::*;
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let (route, registration) = receiver_registration(&f);
    let id = &registration.subscription.subscription_id;
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    f.store.resume_task(&f.cas, &lease, &authority).unwrap();
    finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let clock = stamp(&f);
    assert!(af_runtime::wake::journal::reconcile(&f.store, &f.cas, id, clock).unwrap());
    let envelopes = f.store.inspect_wake(id).unwrap().envelopes;
    assert_eq!(envelopes.len(), 2);
    let first = f.store.claim_wake(&envelopes[0], clock).unwrap().unwrap();
    assert!(f.store.claim_wake(&envelopes[0], clock).unwrap().is_none());
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let second = f
        .store
        .claim_wake(&envelopes[0], clock + CLAIM_MS)
        .unwrap()
        .unwrap();
    assert!(second.epoch > first.epoch);
    assert!(f.store.accept_wake(&first, clock + CLAIM_MS).is_err());
    assert!(f.store.backoff_wake(&first, clock + CLAIM_MS).is_err());
    let mut conflicting = second.clone();
    conflicting.envelope.payload_digest = digest('f');
    assert!(f.store.accept_wake(&conflicting, clock + CLAIM_MS).is_err());
    let accepted = f.store.accept_wake(&second, clock + CLAIM_MS).unwrap();
    assert_eq!(accepted.state, WakeDeliveryStateV1::InboxAccepted);
    assert_eq!(accepted.receipts.len(), 1);
    // Drop acknowledgement, reopen, replay exact packet: same inbox evidence, no new claim.
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert_eq!(
        serde_json::to_value(&accepted).unwrap(),
        serde_json::to_value(f.store.accept_wake(&second, clock + CLAIM_MS + 1).unwrap()).unwrap()
    );
    let native = f
        .store
        .take_wake_native(&route, clock + CLAIM_MS + 2)
        .unwrap()
        .unwrap();
    assert_eq!(native.state, WakeDeliveryStateV1::AdmissionUnknown);
    assert_eq!(native.receipts.len(), 3);
    assert!(matches!(
        native.receipts[1].stage,
        WakeReceiptStageV1::NativeAdmissionIntent { .. }
    ));
    assert!(matches!(
        native.receipts[2].stage,
        WakeReceiptStageV1::AdmissionUnknown { .. }
    ));
    let claim = f
        .store
        .claim_wake(&envelopes[1], clock + CLAIM_MS + 3)
        .unwrap()
        .unwrap();
    f.store.accept_wake(&claim, clock + CLAIM_MS + 4).unwrap();
    // Crash before, during or after native invocation all retain one intent; a later
    // notification cannot overtake it, including via a fresh grant for the same session.
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert!(
        f.store
            .take_wake_native(&route, clock + CLAIM_MS + 5)
            .unwrap()
            .is_none()
    );
    let mut replacement = route.clone();
    replacement.capability_digest = digest('9');
    replacement.route_id = route_identity(&replacement).unwrap();
    f.store.install_wake_route(&replacement).unwrap();
    assert!(
        f.store
            .take_wake_native(&replacement, clock + CLAIM_MS + 6)
            .unwrap()
            .is_none()
    );
    let ids = BTreeSet::from([f.task.task_id.clone()]);
    assert_eq!(
        f.store
            .apply_task_collection_of(&f.cas, &ids)
            .unwrap()
            .tombstoned
            .len(),
        1
    );
    assert!(!f.store.inspect_wake(id).unwrap().source_available);
    f.store
        .maintain_wakes(registration.subscription.expires_unix_ms)
        .unwrap();
    assert!(f.store.inspect_wake(id).unwrap().closure.is_some());
    assert_eq!(
        f.store.wake_deliveries().unwrap()[0].state,
        WakeDeliveryStateV1::AdmissionUnknown
    );
    f.store
        .maintain_wakes(registration.subscription.expires_unix_ms + RETENTION_MS + 1)
        .unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert!(f.store.wake_deliveries().unwrap().is_empty());
    assert!(f.store.wake_registrations().unwrap().is_empty());
    assert_eq!(
        f.store
            .wake_uncertainty()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.store.wake_uncertainty().unwrap()[0]["state"],
        "admission_unknown"
    );
    let db = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    db.execute(
        "UPDATE wake_session_fences_v1 SET intent_id=?1",
        [digest('f')],
    )
    .unwrap();
    assert!(
        f.store.wake_uncertainty().is_err(),
        "corrupt uncertainty cannot look like absent admission"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn wake_expired_unextracted_subscription_closes_and_actual_collection_removes_cas() {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let (_, registration) = receiver_registration(&f);
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    f.store.begin_task_execution(&f.cas, &lease).unwrap();
    let event = finish(
        &f.cas,
        &mut f.store,
        &f.revision_id,
        &f.task,
        &lease,
        &authority,
    );
    let result = match read_task_transition(&event).unwrap().change {
        TaskChangeV1::Finished { result_id } => result_id,
        _ => unreachable!(),
    };
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let ids = BTreeSet::from([f.task.task_id.clone()]);
    assert!(f.store.apply_task_collection_of(&f.cas, &ids).is_err());
    f.store
        .maintain_wakes(registration.subscription.expires_unix_ms)
        .unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    f.store.apply_task_collection_of(&f.cas, &ids).unwrap();
    assert!(f.cas.get_artifact(&result).is_err());
    let journal = f
        .store
        .inspect_wake(&registration.subscription.subscription_id)
        .unwrap();
    assert!(!journal.source_available);
    assert!(journal.closure.is_some());
    assert!(journal.envelopes.is_empty());
}

#[test]
#[cfg(target_os = "linux")]
fn wake_route_scope_revocation_expiry_and_persisted_retry_limits_refuse() {
    use af_contracts::task::wake::delivery::*;
    let mut f = Fixture::new(SUCCESS);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let (route, registration) = receiver_registration(&f);
    let session = af_store::store::wake::delivery::session_key(&route).unwrap();
    f.store
        .bind_wake_session_store(&session, &route.task.store_incarnation)
        .unwrap();
    f.store
        .bind_wake_session_store(&session, &route.task.store_incarnation)
        .unwrap();
    assert!(
        f.store
            .bind_wake_session_store(&session, &digest('f'))
            .is_err()
    );
    assert!(
        f.store
            .check_wake_session_store(&session, &digest('f'))
            .is_err()
    );
    f.store
        .check_wake_session_store(&session, &route.task.store_incarnation)
        .unwrap();
    let grant = WakeGrantV1 {
        schema: "af/task-wake-grant@1".into(),
        route_id: route.route_id.clone(),
        capability: digest('c'),
    };
    assert!(f.store.authenticate_wake_grant(&grant, stamp(&f)).is_ok());
    let mut wrong = grant.clone();
    wrong.capability = digest('f');
    assert!(f.store.authenticate_wake_grant(&wrong, stamp(&f)).is_err());
    let mut wrong = route.clone();
    wrong.session = "substituted".into();
    assert!(f.store.take_wake_native(&wrong, stamp(&f)).is_err());
    let mut wrong = route.clone();
    wrong.host = WakeHostV1::Claude;
    assert!(f.store.install_wake_route(&wrong).is_err());
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    let mut clock = stamp(&f);
    af_runtime::wake::journal::reconcile(
        &f.store,
        &f.cas,
        &registration.subscription.subscription_id,
        clock,
    )
    .unwrap();
    let envelope = f
        .store
        .inspect_wake(&registration.subscription.subscription_id)
        .unwrap()
        .envelopes[0]
        .clone();
    for attempt in 1..=8 {
        let claim = f.store.claim_wake(&envelope, clock).unwrap().unwrap();
        assert_eq!(claim.attempts, attempt);
        f.store.backoff_wake(&claim, clock).unwrap();
        f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        let retained = f.store.wake_deliveries().unwrap().pop().unwrap();
        assert_eq!(retained.attempts, attempt);
        assert!(f.store.claim_wake(&envelope, clock).unwrap().is_none());
        clock = retained.next_attempt_unix_ms;
    }
    assert_eq!(
        f.store.wake_deliveries().unwrap()[0].state,
        WakeDeliveryStateV1::Blocked
    );
    f.store.revoke_wake_route(&grant, stamp(&f)).unwrap();
    assert!(f.store.wake_route(&route.route_id, stamp(&f)).is_err());
    assert!(
        f.store
            .wake_route(&route.route_id, route.expires_unix_ms)
            .is_err()
    );
}

#[test]
#[cfg(target_os = "linux")]
fn wake_receiver_persisted_expiry_prevents_native_admission_after_clock_rollback() {
    let mut f = Fixture::new(SUCCESS);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let (route, registration) = receiver_registration(&f);
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    let clock = stamp(&f);
    af_runtime::wake::journal::reconcile(
        &f.store,
        &f.cas,
        &registration.subscription.subscription_id,
        clock,
    )
    .unwrap();
    let envelope = f
        .store
        .inspect_wake(&registration.subscription.subscription_id)
        .unwrap()
        .envelopes[0]
        .clone();
    let claim = f.store.claim_wake(&envelope, clock).unwrap().unwrap();
    f.store.accept_wake(&claim, clock).unwrap();
    f.store
        .maintain_wakes(registration.subscription.expires_unix_ms)
        .unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert!(
        f.store.wake_route(&route.route_id, clock).is_ok(),
        "simulate the clock moving back before grant expiry"
    );
    assert!(
        f.store.take_wake_native(&route, clock).unwrap().is_none(),
        "persisted closure must still forbid native scheduling"
    );
    assert!(
        f.store
            .wake_uncertainty()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty(),
        "expiry creates no admission or success"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn wake_traversal_restart_removal_expiry_and_corruption_preserve_bounds() {
    use af_store::store::wake::WakeTraversalStage::{Extraction, Native};
    let mut f = Fixture::new(SUCCESS);
    f.store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let a = register(&f, 'a', 128);
    let b = register(&f, 'b', 128);
    let ids = [
        a.subscription.subscription_id.clone(),
        b.subscription.subscription_id.clone(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let first = ids.first().unwrap().clone();
    let last = ids.last().unwrap().clone();
    assert_eq!(
        f.store.next_wake_target(Extraction, &ids).unwrap(),
        Some(first.clone())
    );
    let routes = [a.subscription.recipient_ref.clone()].into_iter().collect();
    assert_eq!(
        f.store.next_wake_target(Native, &routes).unwrap(),
        Some(a.subscription.recipient_ref.clone())
    );
    // A second connection and reopened Store observe the committed position, not an in-memory hint.
    let other = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert_eq!(
        other.next_wake_target(Extraction, &ids).unwrap(),
        Some(last.clone())
    );
    drop(other);
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert_eq!(
        f.store.next_wake_target(Extraction, &ids).unwrap(),
        Some(first.clone())
    );
    // Disappearing targets need no retained row or index: sorted successor then wrap is bounded.
    let only_last = [last.clone()].into_iter().collect();
    assert_eq!(
        f.store.next_wake_target(Extraction, &only_last).unwrap(),
        Some(last.clone())
    );
    assert_eq!(
        f.store
            .next_wake_target(Extraction, &BTreeSet::new())
            .unwrap(),
        None
    );
    let only_first = [first.clone()].into_iter().collect();
    assert_eq!(
        f.store.next_wake_target(Extraction, &only_first).unwrap(),
        Some(first)
    );
    f.store
        .maintain_wakes(a.subscription.expires_unix_ms)
        .unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert_eq!(
        f.store
            .inspect_wake(&a.subscription.subscription_id)
            .unwrap()
            .closure
            .as_deref(),
        Some("expired")
    );
    assert_eq!(
        f.store.next_wake_target(Extraction, &ids).unwrap(),
        Some(last)
    );
    // Scheduling never resets expiry/closure and cannot manufacture an eligible send.
    assert!(
        f.store
            .inspect_wake(&a.subscription.subscription_id)
            .unwrap()
            .closure
            .is_some()
    );
    let unknown = [digest('f')].into_iter().collect();
    assert!(f.store.next_wake_target(Extraction, &unknown).is_err());
    let too_many = (0..129)
        .map(|n| af_store::content_id(&json!(n)).unwrap())
        .collect();
    assert!(f.store.next_wake_target(Extraction, &too_many).is_err());
    let db = rusqlite::Connection::open(f._directory.path().join("events.sqlite")).unwrap();
    for record in [
        json!({"schema":"af/task-wake-traversal@0","extraction_after":null,"native_after":null}),
        json!({"schema":"af/task-wake-traversal@1","extraction_after":"corrupt","native_after":null}),
        json!({"schema":"af/task-wake-traversal@1","extraction_after":null,"native_after":null,"callback":"bad"}),
    ] {
        db.execute(
            "UPDATE wake_traversal_v1 SET record=?1",
            [record.to_string()],
        )
        .unwrap();
        assert!(f.store.next_wake_target(Extraction, &ids).is_err());
        assert!(f.store.next_wake_target(Native, &routes).is_err());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn wake_traversal_native_expiry_and_collection_do_not_reset_positions_or_admission() {
    use af_store::store::wake::{WakeTraversalStage::Native, delivery::route_identity};
    let mut f = Fixture::new(SUCCESS);
    f.store
        .open_task(&f.cas, &f.revision_id, "wake-writer", 60000)
        .unwrap();
    let (first, registration) = receiver_registration(&f);
    let mut later = first.clone();
    later.session = "later-original-session".into();
    later.expires_unix_ms += 1000;
    later.route_id = route_identity(&later).unwrap();
    f.store.install_wake_route(&later).unwrap();
    let mut second = registration.clone();
    second.subscription.recipient_ref = later.route_id.clone();
    second.subscription.subscription_id = digest('9');
    second.subscription.registration_key = digest('8');
    second.subscription.expires_unix_ms = later.expires_unix_ms;
    second.subscription.configuration_digest =
        af_runtime::wake::configuration_digest(&second.subscription).unwrap();
    let second = f.store.register_wake(&f.cas, &second).unwrap();
    let first_only = [first.route_id.clone()].into_iter().collect();
    assert_eq!(
        f.store.next_wake_target(Native, &first_only).unwrap(),
        Some(first.route_id.clone())
    );
    let clock = first.expires_unix_ms;
    f.store.maintain_wakes(clock).unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    assert!(f.store.wake_route(&first.route_id, clock).is_err());
    assert!(f.store.take_wake_native(&first, clock).is_err());
    assert!(f.store.wake_route(&later.route_id, clock).is_ok());
    let both = [first.route_id.clone(), later.route_id.clone()]
        .into_iter()
        .collect();
    assert_eq!(
        f.store.next_wake_target(Native, &both).unwrap(),
        Some(later.route_id.clone())
    );
    assert!(
        f.store.take_wake_native(&later, clock).unwrap().is_none(),
        "rotation never invents a packet"
    );
    f.store
        .maintain_wakes(clock + af_store::store::wake::delivery::RETENTION_MS)
        .unwrap();
    f.store = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
    let remaining = [later.route_id.clone()].into_iter().collect();
    assert_eq!(
        f.store.next_wake_target(Native, &remaining).unwrap(),
        Some(later.route_id)
    );
    assert_eq!(
        f.store
            .inspect_wake(&second.subscription.subscription_id)
            .unwrap()
            .closure
            .as_deref(),
        Some("expired")
    );
    assert_eq!(f.store.wake_uncertainty().unwrap(), json!([]));
}
