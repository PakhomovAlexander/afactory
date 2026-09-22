use super::*;
use crate::capture::captured_fixture;
use review_core::task::review_integration::TaskReviewIntegrationSelectionV1;
use review_core::task::{TaskAcceptanceV1, TaskPhaseV1};
use review_source_git::{Entry, EntryKind, Manifest, manifest_diff};

#[path = "../../support/captured_review_integration.rs"]
mod fixture;
use fixture::{admit_integration, definition};

#[test]
fn empty_integration_is_durable_without_an_attempt_and_preserves_original_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_integration(
        &cas,
        &mut store,
        &definition(&cas, false, false, &dir.path().join("calls")),
        3,
    );
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let before = runtime
        .projection()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting()
        .len();
    let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
    assert_eq!(conclusion.verdict, review_pipeline::RunVerdict::Pass);
    assert!(
        host.assemble_recorded_result(&cas)
            .unwrap_err()
            .contains("select")
    );
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    assert!(matches!(
        phase.phase().selection,
        TaskReviewIntegrationSelectionV1::Empty {}
    ));
    assert!(phase.finished());
    assert_eq!(phase.report_id(), None);
    assert_eq!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .attempt_accounting()
            .len(),
        before
    );
    assert_eq!(
        host.select_recorded_integration(&cas)
            .unwrap()
            .unwrap()
            .phase_id(),
        phase.phase_id()
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
    assert!(result.evidence.contains(phase.phase_id()));
    let id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&id).unwrap();
    assert!(matches!(
        runtime.projection().unwrap().phase,
        TaskPhaseV1::Finished { .. }
    ));
}

#[test]
fn prepared_integration_reuses_one_common_attempt_and_replays_its_exact_phase_report() {
    for failed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let (compiler, lease) =
            admit_integration(&cas, &mut store, &definition(&cas, true, failed, &calls), 3);
        let shared = SharedEventStore::new(&mut store);
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        let report = runtime.execute().unwrap();
        assert!(report.complete(), "{report:?}");
        let before = runtime.projection().unwrap();
        let prior_attempts = before.execution.as_ref().unwrap().attempt_accounting();
        let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
        assert_eq!(conclusion.verdict, review_pipeline::RunVerdict::Pass);
        let canonical = shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .into_iter()
            .find(|e| e.event_id == conclusion.canonical_report_event_id)
            .unwrap();
        let error = shared
            .lock()
            .unwrap()
            .prepare_task_review_round_publication(&cas, &lease, &authority)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unresolved captured Integration"),
            "{error}"
        );
        let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
        assert!(phase.requires_checks(), "{:?}", phase.phase());
        let error = shared
            .lock()
            .unwrap()
            .prepare_task_review_round_publication(&cas, &lease, &authority)
            .unwrap_err();
        assert!(
            error.to_string().contains("Integration is unfinished"),
            "{error}"
        );
        assert!(
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .is_err(),
            "ordinary Round dispatch stays closed"
        );
        let phase_runtime = TaskRuntime::with_review_integration(
            shared.clone(),
            &cas,
            lease.clone(),
            &authority,
            &host,
            &phase,
        )
        .unwrap();
        assert!(phase_runtime.execute().is_err());
        let (id, report) = phase_runtime.execute_review_integration(&phase).unwrap();
        assert!(report.complete(), "{report:?}");
        assert_eq!(std::fs::read(&calls).unwrap(), b"x");
        assert!(
            host.assemble_recorded_result(&cas)
                .unwrap_err()
                .contains("unresolved")
        );
        let current = phase_runtime.projection().unwrap();
        let attempts = current.execution.as_ref().unwrap().attempt_accounting();
        assert_eq!(attempts.len(), prior_attempts.len() + 1);
        assert!(prior_attempts.iter().all(|prior| {
            attempts.iter().any(|a| {
                a.attempt_id == prior.attempt_id && a.charged_tokens == prior.charged_tokens
            })
        }));
        let sql = rusqlite::Connection::open(dir.path().join("events.sqlite")).unwrap();
        let backup = dir.path().join("before-finish.sqlite");
        sql.execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
            .unwrap();
        let backup_lease = lease.clone();
        let canonical_start = shared.lock().unwrap().len("review").unwrap() as usize;
        sql.execute_batch("CREATE TRIGGER integration_atomicity_fault BEFORE INSERT ON events WHEN NEW.type='TaskTransition@3' AND json_extract(NEW.payload,'$.change.kind')='review_integration_finished' BEGIN SELECT RAISE(ABORT,'integration atomicity fault'); END;").unwrap();
        let task_run = review_store::store::task::task_run_id(lease.task_id()).unwrap();
        let before_task = shared.lock().unwrap().replay(&task_run).unwrap();
        let before_review = shared.lock().unwrap().replay("review").unwrap();
        let error = host
            .finish_recorded_integration(&cas, &phase, &id)
            .unwrap_err();
        assert!(error.contains("integration atomicity fault"), "{error}");
        assert_eq!(
            shared.lock().unwrap().replay(&task_run).unwrap(),
            before_task
        );
        assert_eq!(
            shared.lock().unwrap().replay("review").unwrap(),
            before_review
        );
        sql.execute_batch("DROP TRIGGER integration_atomicity_fault;")
            .unwrap();
        // A paid old writer may retain exact output, but cannot publish either canonical
        // stream after a takeover. The new writer recovers that same report without work.
        shared
            .lock()
            .unwrap()
            .release_task_lease(&cas, &lease)
            .unwrap();
        let lease = shared
            .lock()
            .unwrap()
            .take_task_lease(&cas, lease.task_id(), "replacement", 60000)
            .unwrap();
        let before_task = shared
            .lock()
            .unwrap()
            .len(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
            .unwrap();
        let before_review = shared.lock().unwrap().len("review").unwrap();
        assert!(host.finish_recorded_integration(&cas, &phase, &id).is_err());
        assert_eq!(shared.lock().unwrap().len("review").unwrap(), before_review);
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .len(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
                .unwrap(),
            before_task
        );
        let recovered = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let phase = recovered
            .finish_recorded_integration(&cas, &phase, &id)
            .unwrap();
        assert!(phase.finished());
        assert_eq!(phase.integration_committed_event_id().is_some(), !failed);
        let reopened = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let reopened_authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &reopened, &NoTaskDeveloper);
        let publication = shared
            .lock()
            .unwrap()
            .prepare_task_review_round_publication(&cas, &lease, &reopened_authority);
        if failed {
            assert!(
                publication
                    .unwrap_err()
                    .to_string()
                    .contains("no authorized successor")
            );
        } else {
            let publication = publication.unwrap();
            assert_eq!(publication.next_round(), 2);
            assert_eq!(publication.next_epoch(), 1);
            assert!(!publication.is_restart());
            assert_eq!(
                publication.integrated().map(|(id, _)| id),
                phase.integration_committed_event_id()
            );
        }
        let replay = TaskRuntime::with_review_integration(
            shared.clone(),
            &cas,
            lease.clone(),
            &reopened_authority,
            &reopened,
            &phase,
        )
        .unwrap();
        assert_eq!(replay.execute_review_integration(&phase).unwrap().0, id);
        assert_eq!(
            reopened
                .finish_recorded_integration(&cas, &phase, &id)
                .unwrap()
                .report_id(),
            Some(id.as_str())
        );
        assert_eq!(std::fs::read(&calls).unwrap(), b"x");
        assert_eq!(
            replay
                .projection()
                .unwrap()
                .execution
                .unwrap()
                .attempt_accounting()
                .iter()
                .map(|a| (&a.attempt_id, a.charged_tokens))
                .collect::<Vec<_>>(),
            attempts
                .iter()
                .map(|a| (&a.attempt_id, a.charged_tokens))
                .collect::<Vec<_>>()
        );
        let events = shared.lock().unwrap().replay("review").unwrap();
        assert_eq!(
            events.iter().find(|e| e.event_id == canonical.event_id),
            Some(&canonical)
        );
        if !failed {
            reject_forged_attestation(
                &cas,
                &backup,
                &compiler,
                &backup_lease,
                &phase,
                &id,
                &events[canonical_start..],
            );
        }
        if failed {
            let result = reopened.assemble_recorded_result(&cas).unwrap();
            assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
            let old = before.execution.as_ref().unwrap();
            let original_outputs: BTreeMap<_, _> = old
                .graph
                .outputs
                .iter()
                .map(|(name, address)| {
                    (
                        name.clone(),
                        old.outputs[&address.node].1.outputs[&address.port].clone(),
                    )
                })
                .collect();
            assert_eq!(result.outputs, original_outputs);
            assert!(result.evidence.contains(&id));
            let result_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
            replay.finish(&result_id).unwrap();
        } else {
            assert!(
                reopened
                    .assemble_recorded_result(&cas)
                    .unwrap_err()
                    .contains("full Review")
            );
        }
    }
}

#[test]
fn late_usage_before_checks_retains_a_failed_phase_without_dispatch_or_false_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let (compiler, lease) =
        admit_integration(&cas, &mut store, &definition(&cas, true, false, &calls), 3);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    let attempts = runtime
        .projection()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting();
    let usage = plan::artifact(
        &cas,
        review_core::task::usage::TASK_TOKEN_USAGE_V1,
        review_core::task::usage::TaskTokenUsageV1 {
            chargeable_tokens: u64::MAX.into(),
            ..Default::default()
        },
    );
    shared
        .lock()
        .unwrap()
        .observe_task_usage(
            &cas,
            &lease,
            review_core::task::execution::TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempts
                    .iter()
                    .find(|a| a.reservation.tokens > 0)
                    .unwrap()
                    .attempt_id
                    .clone(),
                charged_tokens: u128::from(u64::MAX),
                usage_id: usage,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    // A fresh CLI host must recover the already selected phase after late resource loss;
    // creating a new phase would still require the original passing/resource-current fence.
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let recovered = host.select_recorded_integration(&cas).unwrap().unwrap();
    assert_eq!(recovered.phase_id(), phase.phase_id());
    assert_eq!(recovered.phase(), phase.phase());
    let phase = recovered;
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime = TaskRuntime::with_review_integration(
        shared.clone(),
        &cas,
        lease.clone(),
        &authority,
        &host,
        &phase,
    )
    .unwrap();
    let (id, report) = runtime.execute_review_integration(&phase).unwrap();
    assert!(!report.complete());
    let phase = host.finish_recorded_integration(&cas, &phase, &id).unwrap();
    assert!(phase.finished());
    assert!(!calls.exists());
    assert_eq!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .attempt_accounting()
            .len(),
        attempts.len()
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
    assert_eq!(
        result.execution,
        review_core::task::TaskExecutionV1::Exhausted
    );
    assert!(result.evidence.contains(phase.phase_id()));
    assert!(result.evidence.contains(&id));
    let id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&id).unwrap();
}

#[test]
fn overlapping_captured_proposals_record_conflict_without_a_check_attempt_or_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let manifest = |bytes: &[u8]| {
        Manifest::new(vec![Entry {
            path: "value.txt".into(),
            kind: EntryKind::File,
            content: cas.put(bytes).unwrap(),
            size: bytes.len() as u64,
        }])
        .unwrap()
    };
    let patch = String::from_utf8(
        manifest_diff(&manifest(b"before\n"), &manifest(b"other\n"), &cas)
            .unwrap()
            .patch()
            .into(),
    )
    .unwrap();
    let reply = serde_json::json!({"verdict":"request-changes","summary":null,"findings":[{"severity":"minor","file":"value.txt","line":1,"title":"Alternative fixture value","body":"The alternative changes the same path","fix":"Use other","confidence":1.0}],"benchmark_demands":[],"dispositions":[],
        "proposal":{"patch":patch,"report_indexes":[0],"paths":["value.txt"],"description":"Alternative value","auto_apply_nominated":true}});
    let command = format!(
        "cat >/dev/null; printf 'other\\n' > value.txt; printf '%s' '{}'",
        reply.to_string().replace('\'', "'\\''")
    );
    let clean = "cat >/dev/null; printf '%s' '{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"dispositions\":[]}'";
    let definition = definition(&cas, true, false, &calls)
        .replace(
            &serde_json::to_string(clean).unwrap(),
            &serde_json::to_string(&command).unwrap(),
        )
        .replace(
            "execution = { credential_mode = \"credential_free\" }",
            "execution={credential_mode=\"credential_free\",auto_apply=true}",
        );
    let (compiler, lease) = admit_integration(&cas, &mut store, &definition, 3);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let before = runtime
        .projection()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting()
        .len();
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    assert!(
        matches!(
            phase.phase().selection,
            TaskReviewIntegrationSelectionV1::Conflict { .. }
        ),
        "{:?}",
        phase.phase()
    );
    assert!(phase.finished());
    assert!(!phase.requires_checks());
    assert_eq!(phase.report_id(), None);
    assert_eq!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .attempt_accounting()
            .len(),
        before
    );
    assert!(!calls.exists());
    let events = shared.lock().unwrap().replay("review").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == EventType::IntegrationConflictV1)
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == EventType::IntegrationPreparedV1
                || e.event_type == EventType::IntegrationCommittedV1)
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
    assert!(result.evidence.contains(phase.phase_id()));
    let id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&id).unwrap();
}

#[test]
fn integrated_head_requires_and_receives_a_full_successor_review_in_the_same_task() {
    fixture::run_integration_handoff();
}

#[test]
fn final_permitted_successor_round_captures_no_integration() {
    fixture::run_integration_handoff_with(std::time::Duration::ZERO, 2);
}

fn reject_forged_attestation(
    cas: &Cas,
    path: &std::path::Path,
    compiler: &LegacyReviewPlanCompiler,
    lease: &review_store::store::task::TaskLease,
    phase: &review_store::store::task::review_integration::RegisteredTaskReviewIntegration,
    report_id: &str,
    batch: &[review_core::RunEvent],
) {
    let mut store = EventStore::open(path).unwrap();
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        cas,
        shared.clone(),
        compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(compiler, &host, &NoTaskDeveloper);
    let mut forged: Vec<_> = batch
        .iter()
        .map(|event| {
            let mut value = review_store::NewEvent::new(event.event_type, event.payload.clone());
            value.occurred_at = event.occurred_at.clone();
            value.node_id = event.node_id.clone();
            value.attempt_id = event.attempt_id.clone();
            value.causation_id = event.causation_id.clone();
            value.correlation_id = event.correlation_id.clone();
            value.artifact_refs = event.artifact_refs.clone();
            value
        })
        .collect();
    let event = forged
        .iter_mut()
        .find(|e| e.event_type == EventType::ChangeAttestedV1)
        .unwrap();
    let old = event.payload["artifact_id"].as_str().unwrap().to_owned();
    let mut artifact = cas.get_artifact(&old).unwrap();
    artifact.payload["actor"] = serde_json::json!("fixture/forged-actor");
    let new = cas
        .put_artifact(
            &artifact.artifact_type,
            artifact.producer,
            artifact.input_artifacts,
            artifact.subject_snapshot_id,
            artifact.payload,
        )
        .unwrap()
        .0;
    event.payload["artifact_id"] = serde_json::json!(new);
    event.artifact_refs = vec![new.clone()];
    let commit = forged.last_mut().unwrap();
    assert_eq!(commit.event_type, EventType::IntegrationCommittedV1);
    for id in commit.payload["attestation_ids"].as_array_mut().unwrap() {
        if id.as_str() == Some(&old) {
            *id = serde_json::json!(new);
        }
    }
    for id in &mut commit.artifact_refs {
        if id == &old {
            *id = new.clone();
        }
    }
    let task_run = review_store::store::task::task_run_id(lease.task_id()).unwrap();
    let before_task = shared.lock().unwrap().replay(&task_run).unwrap();
    let before_review = shared.lock().unwrap().replay("review").unwrap();
    let error = shared
        .lock()
        .unwrap()
        .finish_task_review_integration(cas, lease, phase, report_id, &forged, &authority)
        .unwrap_err()
        .to_string();
    assert!(error.contains("attestation"), "{error}");
    assert_eq!(
        shared.lock().unwrap().replay(&task_run).unwrap(),
        before_task
    );
    assert_eq!(
        shared.lock().unwrap().replay("review").unwrap(),
        before_review
    );
}

#[test]
fn late_usage_after_successful_checks_seals_a_new_resource_observation_without_promotion() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let (compiler, lease) =
        admit_integration(&cas, &mut store, &definition(&cas, true, false, &calls), 3);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    let runtime = TaskRuntime::with_review_integration(
        shared.clone(),
        &cas,
        lease.clone(),
        &authority,
        &host,
        &phase,
    )
    .unwrap();
    let (original, report) = runtime.execute_review_integration(&phase).unwrap();
    assert!(report.complete());
    let original_bytes = cas.get_artifact(&original).unwrap();
    let state = runtime.projection().unwrap();
    let output_id = state.execution.as_ref().unwrap().outputs[phase.node()]
        .0
        .clone();
    let attempts = state.execution.as_ref().unwrap().attempt_accounting();
    let usage = plan::artifact(
        &cas,
        review_core::task::usage::TASK_TOKEN_USAGE_V1,
        review_core::task::usage::TaskTokenUsageV1 {
            chargeable_tokens: u64::MAX.into(),
            ..Default::default()
        },
    );
    shared
        .lock()
        .unwrap()
        .observe_task_usage(
            &cas,
            &lease,
            review_core::task::execution::TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempts
                    .iter()
                    .find(|a| a.reservation.tokens > 0)
                    .unwrap()
                    .attempt_id
                    .clone(),
                charged_tokens: u128::from(u64::MAX),
                usage_id: usage,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    let phase = host
        .finish_recorded_integration(&cas, &phase, &original)
        .unwrap();
    assert!(phase.finished());
    assert_eq!(phase.integration_committed_event_id(), None);
    assert_ne!(phase.report_id(), Some(original.as_str()));
    assert_eq!(cas.get_artifact(&original).unwrap(), original_bytes);
    let latest = cas.get_artifact(phase.report_id().unwrap()).unwrap();
    assert_eq!(latest.payload["nodes"][0]["outcome"]["class"], "resources");
    let diagnostic = latest.payload["nodes"][0]["outcome"]["diagnostic_id"]
        .as_str()
        .unwrap();
    assert!(
        cas.get_artifact(diagnostic).unwrap().payload["message"]
            .as_str()
            .unwrap()
            .contains("promotion refused")
    );
    let after = runtime.projection().unwrap();
    assert!(after.run_reports.contains(&original));
    assert!(
        after
            .run_reports
            .contains(&phase.report_id().unwrap().into())
    );
    assert_eq!(
        after.execution.as_ref().unwrap().outputs[phase.node()].0,
        output_id
    );
    assert_eq!(
        after.execution.as_ref().unwrap().attempt_accounting().len(),
        attempts.len()
    );
    assert_eq!(std::fs::read(&calls).unwrap(), b"x");
    assert!(
        !shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .iter()
            .any(|e| e.event_type == EventType::IntegrationCommittedV1)
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
    assert_eq!(
        result.execution,
        review_core::task::TaskExecutionV1::Exhausted
    );
    assert!(result.evidence.contains(&output_id));
    let id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&id).unwrap();
}

#[test]
fn a_final_check_that_could_not_run_remains_incomplete_with_its_raw_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let mut definition: toml::Value =
        toml::from_str(&definition(&cas, true, false, &dir.path().join("calls"))).unwrap();
    let checks = definition["checks"].as_array_mut().unwrap();
    checks[0]["args"][1]["value"]=toml::Value::String("printf '#!/bin/sh\\nexit 0\\n' > next-check; if test \"$(cat value.txt)\" = after; then chmod 000 next-check; else chmod 700 next-check; fi".into());
    checks[1]["program"] = toml::Value::String("./next-check".into());
    checks[1]["args"] = toml::Value::Array(vec![]);
    let (compiler, lease) =
        admit_integration(&cas, &mut store, &toml::to_string(&definition).unwrap(), 3);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    let runtime = TaskRuntime::with_review_integration(
        shared.clone(),
        &cas,
        lease.clone(),
        &authority,
        &host,
        &phase,
    )
    .unwrap();
    let (report_id, report) = runtime.execute_review_integration(&phase).unwrap();
    assert!(!report.complete(), "{report:?}");
    let after = runtime.projection().unwrap();
    let execution = after.execution.as_ref().unwrap();
    assert!(!execution.outputs.contains_key(phase.node()));
    let attempt = execution
        .attempt_accounting()
        .into_iter()
        .find(|a| a.reservation.node == phase.node())
        .unwrap();
    let events = shared
        .lock()
        .unwrap()
        .replay(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
        .unwrap();
    let raw = events
        .iter()
        .find_map(|event| {
            let transition = review_store::store::task::read_task_transition(event).ok()?;
            let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            else {
                return None;
            };
            let record =
                review_store::store::task::execution::read_execution_record(&cas, &record_id)
                    .ok()?
                    .record;
            match record {
                review_core::task::execution::TaskExecutionRecordV1::Settled {
                    attempt_id,
                    raw_artifact_ids,
                    ..
                } if attempt_id == attempt.attempt_id => Some(raw_artifact_ids),
                _ => None,
            }
        })
        .unwrap();
    let retained: Vec<review_check::CheckResult> = raw
        .iter()
        .filter_map(|id| serde_json::from_value(cas.get_json(id).ok()?).ok())
        .collect();
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0].status, review_check::CheckStatus::Passed);
    assert_eq!(retained[1].status, review_check::CheckStatus::NotRun);
    let phase = host
        .finish_recorded_integration(&cas, &phase, &report_id)
        .unwrap();
    assert_eq!(phase.integration_committed_event_id(), None);
    assert!(
        !shared
            .lock()
            .unwrap()
            .replay("review")
            .unwrap()
            .iter()
            .any(|e| e.event_type == EventType::IntegrationChecksCompletedV1)
    );
    let result = host.assemble_recorded_result(&cas).unwrap();
    assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
    assert_eq!(
        result.execution,
        review_core::task::TaskExecutionV1::Exhausted
    );
    let id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, &result);
    runtime.finish(&id).unwrap();
}

#[test]
fn delayed_prospective_review_preparation_renews_lease_and_refreshes_only_the_task_prefix() {
    fixture::run_integration_handoff_with(std::time::Duration::from_secs(16), 3);
}

/// A Proposal that touches a protected path is a recorded conflict: no Integration Snapshot is
/// prepared or promoted, and no post-apply check runs.
#[test]
fn a_protected_path_refuses_integration_before_any_check_or_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let definition = definition(&cas, true, false, &calls).replace(
        "\n[integration]\n",
        "\n[integration]\nprotected_paths=[\"value.txt\"]\n",
    );
    assert!(definition.contains("protected_paths"), "{definition}");
    let (compiler, lease) = admit_integration(&cas, &mut store, &definition, 3);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let phase = host.select_recorded_integration(&cas).unwrap().unwrap();
    assert!(
        matches!(
            phase.phase().selection,
            TaskReviewIntegrationSelectionV1::Conflict { .. }
        ),
        "{:?}",
        phase.phase()
    );
    assert!(phase.finished() && !phase.requires_checks());
    let events = shared.lock().unwrap().replay("review").unwrap();
    assert!(events.iter().any(|event| {
        event.event_type == EventType::IntegrationConflictV1
            && event.payload["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("protected"))
    }));
    assert!(!events.iter().any(|event| matches!(
        event.event_type,
        EventType::IntegrationPreparedV1 | EventType::IntegrationCommittedV1
    )));
    assert!(!calls.exists(), "no post-apply check ran");
}

/// A prepared Integration must be the deterministic composition of its selected Proposals.
/// A Task-backed Campaign refuses a raw preparation event outright, and its protected phase
/// publication refuses a plan whose derived Manifest is anything else.
#[test]
fn a_prepared_integration_must_be_the_deterministic_proposal_composition() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let (compiler, lease) =
        admit_integration(&cas, &mut store, &definition(&cas, true, false, &calls), 3);
    let backup = dir.path().join("before-selection.sqlite");
    let phase = {
        let shared = SharedEventStore::new(&mut store);
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        assert!(runtime.execute().unwrap().complete());
        host.publish_recorded_round_conclusion(&cas).unwrap();
        rusqlite::Connection::open(dir.path().join("events.sqlite"))
            .unwrap()
            .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
            .unwrap();
        host.select_recorded_integration(&cas).unwrap().unwrap()
    };
    let TaskReviewIntegrationSelectionV1::Prepared {
        integration_plan_id,
        derived_snapshot_id,
    } = &phase.phase().selection
    else {
        panic!(
            "the selected Proposal was not prepared: {:?}",
            phase.phase()
        )
    };
    let mut forged_plan: review_core::IntegrationPlanV1 =
        serde_json::from_value(cas.get_json(integration_plan_id).unwrap()).unwrap();
    let derived: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(derived_snapshot_id).unwrap()).unwrap();
    let prior: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(derived.parent_snapshot_id.as_deref().unwrap())
            .unwrap(),
    )
    .unwrap();
    // The forged plan claims the unchanged head is the composition of the Proposal.
    let original = prior.artifact_manifest.clone().unwrap();
    let unchanged: Manifest = serde_json::from_value(cas.get_json(&original).unwrap()).unwrap();
    forged_plan.derived_manifest_artifact_id = original.clone();
    let forged_plan_id = cas
        .put_json(&serde_json::to_value(&forged_plan).unwrap())
        .unwrap();
    let forged_batch = format!("integration-{}", &forged_plan_id[7..23]);
    let forged_snapshot = review_core::SourceSnapshot {
        capture: review_core::Capture::Derived {
            tree_id: unchanged.content_digest(),
            parent_snapshot_id: derived.parent_snapshot_id.clone().unwrap(),
            integration_batch_id: forged_batch.clone(),
        },
        content_digest: unchanged.content_digest(),
        artifact_manifest: Some(original.clone()),
        ..derived.clone()
    };
    let forged_snapshot_id = cas
        .put_json(&serde_json::to_value(forged_snapshot).unwrap())
        .unwrap();

    let mut store = EventStore::open(&backup).unwrap();
    let raw = store
        .append(
            "review",
            &cas,
            review_store::NewEvent::new(
                EventType::IntegrationPreparedV1,
                serde_json::to_value(review_core::IntegrationPreparedPayloadV1 {
                    batch_id: forged_batch,
                    plan_artifact_id: forged_plan_id.clone(),
                    derived_snapshot_id: forged_snapshot_id.clone(),
                })
                .unwrap(),
            )
            .correlating(forged_plan.subject_id.clone())
            .referencing(vec![
                forged_plan_id.clone(),
                forged_snapshot_id.clone(),
                original,
            ]),
        )
        .unwrap_err();
    assert!(
        raw.to_string()
            .contains("requires its protected phase publication"),
        "{raw}"
    );
    let mut forged_phase = phase.phase().clone();
    forged_phase.selection = TaskReviewIntegrationSelectionV1::Prepared {
        integration_plan_id: forged_plan_id,
        derived_snapshot_id: forged_snapshot_id,
    };
    let forged_phase_id =
        review_store::store::task::review_integration::capture_task_review_integration(
            &cas,
            &forged_phase,
        )
        .unwrap();
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let before = shared.lock().unwrap().replay("review").unwrap();
    let error = shared
        .lock()
        .unwrap()
        .select_task_review_integration(&cas, &lease, &forged_phase_id, &authority)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic Proposal composition"),
        "{error}"
    );
    assert_eq!(shared.lock().unwrap().replay("review").unwrap(), before);
    assert!(
        !before
            .iter()
            .any(|event| event.event_type == EventType::IntegrationPreparedV1)
    );
}
