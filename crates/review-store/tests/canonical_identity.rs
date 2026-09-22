mod support;

use review_core::{
    EventType, FindingGroupingAction, FindingGroupingEventPayloadV1, FindingGroupingV1,
    LegacyStageOutput, Producer, RoundStartedPayloadV1, RunEvent, SubjectV1,
};
use review_store::{
    CanonicalStage, Cas, ConvergencePolicy, EventStore, Ingest, LedgerProjection, NewEvent,
};
use support::opened_round;

fn stage() -> LegacyStageOutput {
    serde_json::from_value(serde_json::json!({
        "verdict": "request-changes",
        "summary": null,
        "findings": [{
            "severity": "major",
            "file": "src/lib.rs",
            "line": 7,
            "title": "Same presentation",
            "body": "same body",
            "fix": "same fix",
            "confidence": 0.9
        }],
        "benchmark_demands": [],
        "disputes": []
    }))
    .unwrap()
}

/// Publish `output` as the `ReviewerResult@2` a Task reviewer Attempt returns.
fn task_result(
    cas: &Cas,
    run_id: &str,
    source: &str,
    attempt_id: &str,
    input_artifacts: &[String],
    head: &str,
    output: &LegacyStageOutput,
) -> String {
    let mut payload = serde_json::to_value(output).unwrap();
    let object = payload.as_object_mut().unwrap();
    let reports = object.remove("findings").unwrap();
    object.insert("reports".into(), reports);
    let dispositions = object.remove("disputes").unwrap();
    assert_eq!(dispositions, serde_json::json!([]));
    object.insert("dispositions".into(), dispositions);
    cas.put_artifact(
        review_core::contract::REVIEWER_RESULT_V2,
        Producer::Attempt {
            run_id: run_id.into(),
            node_id: source.into(),
            attempt_id: attempt_id.into(),
        },
        input_artifacts.to_vec(),
        Some(head.into()),
        payload,
    )
    .unwrap()
    .0
}

fn apply_resolution(
    ledger: &mut review_store::Ledger,
    cas: &Cas,
    run_id: &str,
    head: &str,
    operation: &str,
    resolution: review_core::FindingResolutionV1,
) -> String {
    let finding_id = resolution.finding_id.clone();
    let (record_id, envelope) = cas
        .put_artifact(
            review_core::contract::FINDING_RESOLUTION_V1,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: None,
                operation_id: operation.into(),
            },
            Vec::new(),
            Some(head.into()),
            serde_json::to_value(resolution).unwrap(),
        )
        .unwrap();
    ledger
        .apply_event(
            &RunEvent {
                event_id: format!("{operation}-event"),
                run_id: run_id.into(),
                sequence: 98,
                event_type: EventType::FindingResolutionRecordedV1,
                occurred_at: "2026-08-28T00:00:00Z".into(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: Some(finding_id),
                artifact_refs: vec![record_id.clone()],
                payload: serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                    artifact_id: record_id,
                })
                .unwrap(),
            },
            cas,
        )
        .unwrap();
    envelope.artifact_id
}

#[test]
fn task_and_campaign_use_identical_pure_canonical_reduction_without_another_store() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "shared-domain-reducer";
    let authority = opened_round(&mut store, &cas, run_id);
    let mut output = stage();
    output
        .benchmark_demands
        .push(review_core::legacy::LegacyBenchmarkDemand {
            claim: "Pagination remains bounded".into(),
            why: "Large inputs must not allocate the full result".into(),
            suggested_method: "Measure allocation growth".into(),
        });
    let input = cas.put(b"declared input").unwrap();
    let attempt_id = "01aaaaaaaaaaaaaaaaaaaaaaaa";
    // The Task reviewer's actual Attempt address equals the one Campaign ingestion derives.
    let result = task_result(
        &cas,
        run_id,
        "root.review.correctness",
        attempt_id,
        std::slice::from_ref(&input),
        &authority.head,
        &output,
    );
    let stages = [CanonicalStage {
        source: "root.review.correctness",
        demand_requirement: review_core::DemandRequirement::Required,
        stage: &output,
        attempt_id,
        result_artifact_id: &result,
        input_artifacts: std::slice::from_ref(&input),
        subject_snapshot_id: &authority.head,
        subject_id: &authority.subject,
        result_contract: review_core::ReviewerResultContract::V2,
    }];
    let mut task_ledger =
        review_store::Ledger::for_task_subject(&cas, &authority.subject, 1).unwrap();
    let before = store.len(run_id).unwrap();
    let pure =
        review_store::prepare_canonical_task_review(&cas, run_id, &task_ledger, &stages).unwrap();
    assert_eq!(
        store.len(run_id).unwrap(),
        before,
        "pure reduction cannot append another execution history"
    );
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    assert_eq!(ingest.round(), 1);
    let historical = ingest.add_canonical_stage_outputs(&stages).unwrap();
    assert_eq!(pure.reduction, historical);
    assert_eq!(pure.ledger.finding_views(), ingest.ledger().finding_views());
    assert_eq!(pure.ledger.demand_views(), ingest.ledger().demand_views());
    assert!(review_store::prepare_canonical_task_review(&cas, run_id, &task_ledger, &[]).is_err());
    assert!(
        task_ledger
            .bind_task_subject(&cas, "sha256:missing", 2)
            .is_err()
    );
}

#[test]
fn task_fix_projection_rejects_stale_views_and_reopens_on_a_changed_subject() {
    use review_core::task::{execution::TaskInvocationV1, repair::*, review::*};
    use std::collections::BTreeMap;
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "task-fix-projection";
    let authority = opened_round(&mut store, &cas, run_id);
    let output = stage();
    let attempt_id = "01aaaaaaaaaaaaaaaaaaaaaaaa";
    let result = task_result(
        &cas,
        run_id,
        "root.reviewers.correctness",
        attempt_id,
        &[],
        &authority.head,
        &output,
    );
    let ledger = review_store::Ledger::for_task_subject(&cas, &authority.subject, 1).unwrap();
    let stages = [CanonicalStage {
        source: "correctness",
        demand_requirement: review_core::DemandRequirement::Required,
        stage: &output,
        attempt_id,
        result_artifact_id: &result,
        input_artifacts: &[],
        subject_snapshot_id: &authority.head,
        subject_id: &authority.subject,
        result_contract: review_core::ReviewerResultContract::V2,
    }];
    let mut ledger = review_store::prepare_canonical_task_review(&cas, run_id, &ledger, &stages)
        .unwrap()
        .ledger;
    let original = ledger.finding_views()[0].clone();
    let new_head = cas.put(b"sealed repaired snapshot").unwrap();
    let new_subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&new_head)).unwrap())
        .unwrap();
    ledger.bind_task_subject(&cas, &new_subject, 1).unwrap();
    let current_view = ledger.finding_view_id(&original.key).unwrap();
    let receipt = TaskFixReceiptV1 {
        invocation: TaskInvocationV1 {
            plan_id: result.clone(),
            node: "root.continue".into(),
            inputs: BTreeMap::new(),
        },
        finding_id: original.key.clone(),
        continuation_id: result.clone(),
        subject_id: new_subject.clone(),
        decision: TaskFixDecisionV1 {
            expected_view_id: current_view.clone(),
            attestation_id: result.clone(),
            outcome: VerificationOutcomeV1::Positive,
            reason: "Independent selected Task verification".into(),
        },
        verifier_output_id: Some(result.clone()),
    };
    let receipt_id = cas
        .put_artifact(
            TASK_FIX_RECEIPT_V1,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: Some("root.continue".into()),
                operation_id: "test-projection-input".into(),
            },
            vec![result.clone()],
            Some(new_head.clone()),
            serde_json::to_value(&receipt).unwrap(),
        )
        .unwrap()
        .0;
    let mut assessment = RepairAssessmentV1 {
        continuation_id: result.clone(),
        current_subject_id: new_subject.clone(),
        current_snapshot_id: new_head,
        check_receipt_id: result.clone(),
        scope: RepairScopeV1::TargetedFixes,
        claims: BTreeMap::from([(
            original.key.clone(),
            ClaimVerificationV1 {
                expected_view_id: result.clone(),
                attestation_id: result.clone(),
                receipt_id,
                outcome: VerificationOutcomeV1::Positive,
            },
        )]),
    };
    let before = ledger.finding_views();
    assert!(ledger.project_task_fixes(&cas, &assessment).is_err());
    assert_eq!(ledger.finding_views(), before);
    assessment
        .claims
        .get_mut(&original.key)
        .unwrap()
        .expected_view_id = current_view;
    ledger.project_task_fixes(&cas, &assessment).unwrap();
    assert_eq!(
        ledger.finding_view(&original.key).unwrap().status,
        review_store::Status::Fixed
    );
    assert_eq!(ledger.round, 1);
    ledger.bind_task_subject(&cas, &new_subject, 2).unwrap();
    assert_eq!(
        ledger.finding_view(&original.key).unwrap().status,
        review_store::Status::Fixed
    );
    let changed_head = cas.put(b"later source changes").unwrap();
    let changed_subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&changed_head)).unwrap())
        .unwrap();
    ledger.bind_task_subject(&cas, &changed_subject, 3).unwrap();
    assert_eq!(
        ledger.finding_view(&original.key).unwrap().status,
        review_store::Status::Open
    );
    assert!(ledger.project_task_fixes(&cas, &assessment).is_err());
    assert!(
        ledger.resolution(&original.key).is_none(),
        "Task receipts cannot impersonate legacy resolutions"
    );
}

#[test]
fn canonical_reports_are_enveloped_and_same_path_title_does_not_merge() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1xz";
    let authority = opened_round(&mut store, &cas, run_id);
    let result_a = cas.put_json(&serde_json::json!({"wire": "a"})).unwrap();
    let result_b = cas.put_json(&serde_json::json!({"wire": "b"})).unwrap();
    let input = cas.put(b"exact reviewer input").unwrap();
    let output = stage();
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    let reduction = ingest
        .add_canonical_stage_outputs(&[
            CanonicalStage {
                source: "architecture",
                demand_requirement: review_core::DemandRequirement::Required,
                stage: &output,
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202",
                result_artifact_id: &result_a,
                input_artifacts: std::slice::from_ref(&input),
                subject_snapshot_id: &authority.head,
                subject_id: &authority.subject,
                result_contract: review_core::ReviewerResultContract::V1,
            },
            CanonicalStage {
                source: "correctness",
                demand_requirement: review_core::DemandRequirement::Required,
                stage: &output,
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w203",
                result_artifact_id: &result_b,
                input_artifacts: std::slice::from_ref(&input),
                subject_snapshot_id: &authority.head,
                subject_id: &authority.subject,
                result_contract: review_core::ReviewerResultContract::V1,
            },
        ])
        .unwrap();

    assert_eq!(ingest.ledger().len(), 2);
    let corrupted_key = ingest.ledger().findings()[0].key.clone();
    let corrupted_report_id = ingest.ledger().findings()[0].reports[0].report_id.clone();
    let keys: Vec<_> = ingest
        .ledger()
        .findings()
        .iter()
        .map(|finding| finding.key.as_str())
        .collect();
    assert_ne!(keys[0], keys[1]);
    assert!(keys.iter().all(|key| key.starts_with("sha256:")));
    assert_eq!(reduction.selected_report_ids.len(), 2);
    assert_eq!(reduction.input_artifact_ids.len(), 2);

    for finding in ingest.ledger().findings() {
        let report = &finding.reports[0];
        let value = cas.get_json(&report.report_id).unwrap();
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value).unwrap();
        review_store::validate_envelope(&envelope).unwrap();
        assert_eq!(
            envelope.artifact_type,
            review_core::contract::FINDING_REPORT_V1
        );
        assert_eq!(
            envelope.subject_snapshot_id.as_deref(),
            Some(authority.head.as_str())
        );
        assert!(envelope.input_artifacts.contains(&input));
        assert_eq!(
            report.artifact_id.as_deref(),
            Some(envelope.artifact_id.as_str())
        );
    }

    drop(ingest);
    let rebuilt = LedgerProjection::rebuild(&store, &cas, run_id)
        .unwrap()
        .into_ledger();
    assert_eq!(rebuilt.len(), 2);

    let corrupt_path = directory
        .path()
        .join("cas/objects")
        .join(&corrupted_report_id[7..9])
        .join(&corrupted_report_id[9..]);
    std::fs::write(corrupt_path, b"corrupt envelope").unwrap();
    let rebuilt = LedgerProjection::rebuild(&store, &cas, run_id)
        .unwrap()
        .into_ledger();
    assert!(
        rebuilt.get(&corrupted_key).unwrap().authority_diagnostic,
        "unreadable canonical Report authority must remain replayable and fail closed"
    );

    // Keep all bootstrap IDs live until the manifest-corruption case deliberately breaks one.
    assert!(cas.contains(&authority.authority));
    assert!(cas.contains(&authority.manifest));
    assert!(cas.contains(&authority.subject));
    assert!(cas.contains(&authority.findings));
    assert!(cas.contains(&authority.demands));

    let manifest_path = directory
        .path()
        .join("cas/objects")
        .join(&authority.manifest[7..9])
        .join(&authority.manifest[9..]);
    std::fs::write(manifest_path, b"corrupt manifest").unwrap();
    let rebuilt = LedgerProjection::rebuild(&store, &cas, run_id)
        .unwrap()
        .into_ledger();
    assert_eq!(rebuilt.finding_identity_policy(), None);
    assert!(
        rebuilt
            .convergence(ConvergencePolicy::default())
            .authority_failures_recent
            > 0,
        "unreadable Campaign policy must remain replayable but block convergence"
    );
}

#[test]
fn grouping_is_reversible_and_preserves_each_report_obligation() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1gy";
    let authority = opened_round(&mut store, &cas, run_id);
    let result_a = cas.put_json(&serde_json::json!({"wire": "a"})).unwrap();
    let result_b = cas.put_json(&serde_json::json!({"wire": "b"})).unwrap();
    let input = cas.put(b"exact reviewer input").unwrap();
    let output = stage();
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_canonical_stage_outputs(&[
            CanonicalStage {
                source: "architecture",
                demand_requirement: review_core::DemandRequirement::Required,
                stage: &output,
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w204",
                result_artifact_id: &result_a,
                input_artifacts: std::slice::from_ref(&input),
                subject_snapshot_id: &authority.head,
                subject_id: &authority.subject,
                result_contract: review_core::ReviewerResultContract::V1,
            },
            CanonicalStage {
                source: "correctness",
                demand_requirement: review_core::DemandRequirement::Required,
                stage: &output,
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w205",
                result_artifact_id: &result_b,
                input_artifacts: std::slice::from_ref(&input),
                subject_snapshot_id: &authority.head,
                subject_id: &authority.subject,
                result_contract: review_core::ReviewerResultContract::V1,
            },
        ])
        .unwrap();
    let mut ledger = ingest.into_projection().into_ledger();
    let keys: Vec<_> = ledger
        .findings()
        .iter()
        .map(|finding| finding.key.clone())
        .collect();
    assert_eq!(keys.len(), 2);

    let pre_group_view = ledger.finding_view_id(&keys[0]).unwrap();
    apply_resolution(
        &mut ledger,
        &cas,
        run_id,
        &authority.head,
        "pre-group-wontfix",
        review_core::FindingResolutionV1 {
            finding_id: keys[0].clone(),
            expected_finding_view_id: pre_group_view,
            subject_id: authority.subject.clone(),
            outcome: review_core::FindingResolutionOutcome::WontfixTracked,
            actor: "operator".into(),
            policy_revision: "risk-policy@1".into(),
            reason: "temporary tracked exception".into(),
            evidence_ids: vec![],
            verification_id: None,
            max_accepted_severity: Some(review_core::Severity::Major),
            tracking_reference: Some("ISSUE-42".into()),
            expires_at_policy_time: Some(10),
        },
    );
    assert!(
        ledger
            .validate_group(&keys[0], &keys[1])
            .unwrap_err()
            .to_string()
            .contains("unexpired tracked-wontfix"),
        "grouping must not detach a live tracked-wontfix expiry"
    );
    let rejected_view = ledger.finding_view_id(&keys[0]).unwrap();
    let rejected_id = apply_resolution(
        &mut ledger,
        &cas,
        run_id,
        &authority.head,
        "pre-group-rejected",
        review_core::FindingResolutionV1 {
            finding_id: keys[0].clone(),
            expected_finding_view_id: rejected_view,
            subject_id: authority.subject.clone(),
            outcome: review_core::FindingResolutionOutcome::Rejected,
            actor: "operator".into(),
            policy_revision: "resolution-policy@1".into(),
            reason: "supersede the temporary exception".into(),
            evidence_ids: vec![],
            verification_id: None,
            max_accepted_severity: None,
            tracking_reference: None,
            expires_at_policy_time: None,
        },
    );

    let transition = |action, record_id: String| RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w206".into(),
        run_id: run_id.into(),
        sequence: 99,
        event_type: match action {
            FindingGroupingAction::Group => EventType::FindingsGroupedV1,
            FindingGroupingAction::Ungroup => EventType::FindingsUngroupedV1,
        },
        occurred_at: "2026-08-28T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: Some(keys[0].clone()),
        artifact_refs: vec![record_id.clone()],
        payload: serde_json::to_value(FindingGroupingEventPayloadV1 {
            from: keys[0].clone(),
            into: keys[1].clone(),
            grouping_artifact_id: record_id,
        })
        .unwrap(),
    };
    let publish = |action| {
        cas.put_artifact(
            review_core::contract::FINDING_GROUPING_V1,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: None,
                operation_id: format!("test-{action:?}"),
            },
            Vec::new(),
            None,
            serde_json::to_value(FindingGroupingV1 {
                from: keys[0].clone(),
                into: keys[1].clone(),
                action,
                round: 1,
            })
            .unwrap(),
        )
        .unwrap()
        .0
    };

    ledger
        .apply_event(
            &transition(
                FindingGroupingAction::Group,
                publish(FindingGroupingAction::Group),
            ),
            &cas,
        )
        .unwrap();
    assert_eq!(
        ledger.resolution(&keys[0]).unwrap().artifact_id,
        rejected_id,
        "grouping must retain the member Resolution under its original Finding ID"
    );
    assert!(ledger.resolution(&keys[1]).is_none());
    let views = ledger.finding_views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].aliases, vec![keys[0].clone()]);
    assert_eq!(views[0].reports.len(), 2);
    assert_eq!(
        ledger
            .convergence(ConvergencePolicy::default())
            .open_blocking,
        1
    );

    let resolution = review_core::FindingResolutionV1 {
        finding_id: keys[0].clone(),
        expected_finding_view_id: ledger.finding_view_id(&keys[0]).unwrap(),
        subject_id: authority.subject.clone(),
        outcome: review_core::FindingResolutionOutcome::Rejected,
        actor: "operator".into(),
        policy_revision: "group-policy@1".into(),
        reason: "one decision covers the grouped claim".into(),
        evidence_ids: vec![],
        verification_id: None,
        max_accepted_severity: None,
        tracking_reference: None,
        expires_at_policy_time: None,
    };
    let (resolution_record, resolution_envelope) = cas
        .put_artifact(
            review_core::contract::FINDING_RESOLUTION_V1,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: None,
                operation_id: "group-resolution".into(),
            },
            Vec::new(),
            Some(authority.head.clone()),
            serde_json::to_value(resolution).unwrap(),
        )
        .unwrap();
    ledger
        .apply_event(
            &RunEvent {
                event_id: "group-resolution-event".into(),
                run_id: run_id.into(),
                sequence: 100,
                event_type: EventType::FindingResolutionRecordedV1,
                occurred_at: "2026-08-28T00:00:00Z".into(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: Some(keys[0].clone()),
                artifact_refs: vec![resolution_record.clone()],
                payload: serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                    artifact_id: resolution_record,
                })
                .unwrap(),
            },
            &cas,
        )
        .unwrap();
    let root = ledger.finding_view(&keys[0]).unwrap().key;
    assert_eq!(
        ledger.resolution(&keys[0]).unwrap().artifact_id,
        ledger.resolution(&root).unwrap().artifact_id
    );
    assert!(
        ledger
            .validate_ungroup(&keys[0], &keys[1])
            .unwrap_err()
            .to_string()
            .contains("challenge the Resolution first"),
        "ungroup must not detach a terminal status from its group Resolution"
    );

    let next_head = cas.put(b"next whole-tree head").unwrap();
    let next_subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&next_head)).unwrap())
        .unwrap();
    ledger
        .apply_event(
            &RunEvent {
                event_id: "group-round-2".into(),
                run_id: run_id.into(),
                sequence: 101,
                event_type: EventType::RoundStartedV1,
                occurred_at: "2026-08-28T00:00:00Z".into(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: Some(next_subject.clone()),
                artifact_refs: Vec::new(),
                payload: serde_json::to_value(RoundStartedPayloadV1 {
                    round: 2,
                    epoch: 1,
                    campaign_manifest_id: authority.manifest.clone(),
                    subject_id: next_subject.clone(),
                    prior_finding_set_id: authority.findings.clone(),
                    prior_demand_set_id: authority.demands.clone(),
                })
                .unwrap(),
            },
            &cas,
        )
        .unwrap();
    let mut lower_report = stage().findings.remove(0).into_report(0).unwrap();
    lower_report.severity = review_core::Severity::Minor;
    lower_report.body = "lower-severity evidence from a different Subject".into();
    let (lower_record, _) = cas
        .put_artifact(
            review_core::contract::FINDING_REPORT_V1,
            Producer::Attempt {
                run_id: run_id.into(),
                node_id: "correctness".into(),
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w211".into(),
            },
            Vec::new(),
            Some(next_head),
            serde_json::to_value(lower_report).unwrap(),
        )
        .unwrap();
    ledger
        .apply_event(
            &RunEvent {
                event_id: "group-lower-report".into(),
                run_id: run_id.into(),
                sequence: 102,
                event_type: EventType::FindingReportedV1,
                occurred_at: "2026-08-28T00:00:00Z".into(),
                node_id: Some("correctness".into()),
                attempt_id: Some("01jd8m4qz9k7v3n2p6r8t0w211".into()),
                causation_id: Some("group-round-2".into()),
                correlation_id: Some(keys[0].clone()),
                artifact_refs: vec![lower_record.clone()],
                payload: serde_json::json!({
                    "key": keys[0],
                    "round": 2,
                    "source": "correctness",
                    "report_id": lower_record,
                }),
            },
            &cas,
        )
        .unwrap();
    let challenged = ledger.finding_view(&keys[0]).unwrap();
    assert_eq!(challenged.status, review_store::Status::Contested);
    assert_eq!(challenged.severity, review_core::Severity::Major);
    assert_eq!(challenged.body, "same body");

    let challenge = review_core::ResolutionChallengeV1 {
        finding_id: root.clone(),
        resolution_id: resolution_envelope.artifact_id,
        subject_id: next_subject,
        kind: review_core::ResolutionChallengeKind::NewEvidence,
        actor: "operator".into(),
        reason: "new evidence contests the grouped decision".into(),
        evidence_ids: vec![],
    };
    let (challenge_record, _) = cas
        .put_artifact(
            review_core::contract::RESOLUTION_CHALLENGE_V1,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: None,
                operation_id: "group-challenge".into(),
            },
            Vec::new(),
            Some(authority.head.clone()),
            serde_json::to_value(challenge).unwrap(),
        )
        .unwrap();
    ledger
        .apply_event(
            &RunEvent {
                event_id: "group-challenge-event".into(),
                run_id: run_id.into(),
                sequence: 101,
                event_type: EventType::FindingResolutionChallengedV1,
                occurred_at: "2026-08-28T00:00:00Z".into(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: Some(root),
                artifact_refs: vec![challenge_record.clone()],
                payload: serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                    artifact_id: challenge_record,
                })
                .unwrap(),
            },
            &cas,
        )
        .unwrap();
    assert_eq!(
        ledger.finding_view(&keys[0]).unwrap().status,
        review_store::Status::Contested
    );

    ledger
        .apply_event(
            &transition(
                FindingGroupingAction::Ungroup,
                publish(FindingGroupingAction::Ungroup),
            ),
            &cas,
        )
        .unwrap();
    assert_eq!(ledger.finding_views().len(), 2);
    assert_eq!(
        ledger
            .convergence(ConvergencePolicy::default())
            .open_blocking,
        2
    );
}

#[test]
fn canonical_confirmation_becomes_current_corroborating_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1xz";
    let authority = opened_round(&mut store, &cas, run_id);
    let result_a = cas.put_json(&serde_json::json!({"wire": "a"})).unwrap();
    let result_b = cas.put_json(&serde_json::json!({"wire": "b"})).unwrap();
    let output = stage();
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "architecture",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &output,
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202",
            result_artifact_id: &result_a,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        }])
        .unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    let confirmation: LegacyStageOutput = serde_json::from_value(serde_json::json!({
        "verdict": "approve",
        "summary": "still present",
        "findings": [],
        "benchmark_demands": [],
        "disputes": [{
            "claim_id": key,
            "position": "confirm",
            "reason": "verified against the current snapshot"
        }, {
            "claim_id": "sha256:mistyped-prior-finding",
            "position": "confirm",
            "reason": "cannot be attached safely"
        }]
    }))
    .unwrap();

    let reduction = ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "correctness",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &confirmation,
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w203",
            result_artifact_id: &result_b,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        }])
        .unwrap();

    assert_eq!(reduction.selected_report_ids.len(), 1);
    let finding = ingest.ledger().get(&key).unwrap();
    assert_eq!(finding.reports.len(), 2);
    assert_eq!(finding.reports[1].source, "correctness");
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&finding.reports[1].report_id).unwrap()).unwrap();
    let report: review_core::FindingReport = serde_json::from_value(envelope.payload).unwrap();
    assert_eq!(report.relations.len(), 1);
    assert_eq!(
        report.relations[0].kind,
        review_core::RelationKind::Corroborates
    );
    assert_eq!(report.relations[0].target.id, key);
}

#[test]
fn canonical_confirmation_replay_reuses_the_exact_corroborating_report() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1xz";
    let authority = opened_round(&mut store, &cas, run_id);
    let seed_result = cas.put_json(&serde_json::json!({"wire": "seed"})).unwrap();
    let escalation_result = cas
        .put_json(&serde_json::json!({"wire": "escalation"}))
        .unwrap();
    let confirmation_result = cas
        .put_json(&serde_json::json!({"wire": "confirmation"}))
        .unwrap();
    let seed = stage();
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "seed",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &seed,
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202",
            result_artifact_id: &seed_result,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        }])
        .unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    let confirmation: LegacyStageOutput = serde_json::from_value(serde_json::json!({
        "verdict": "approve",
        "summary": "still present",
        "findings": [],
        "benchmark_demands": [],
        "disputes": [{
            "claim_id": key,
            "position": "confirm",
            "reason": "verified against the current snapshot"
        }]
    }))
    .unwrap();
    let stages = [CanonicalStage {
        source: "correctness",
        demand_requirement: review_core::DemandRequirement::Required,
        stage: &confirmation,
        attempt_id: "01jd8m4qz9k7v3n2p6r8t0w204",
        result_artifact_id: &confirmation_result,
        input_artifacts: &[],
        subject_snapshot_id: &authority.head,
        subject_id: &authority.subject,
        result_contract: review_core::ReviewerResultContract::V1,
    }];
    let first = ingest.add_canonical_stage_outputs(&stages).unwrap();
    assert_eq!(ingest.ledger().get(&key).unwrap().reports.len(), 2);
    drop(ingest);

    let escalation = review_core::FindingReport {
        title: "Same presentation".into(),
        severity: review_core::Severity::Blocker,
        locations: vec![review_core::Location {
            path: "src/lib.rs".into(),
            line: Some(7),
            end_line: None,
        }],
        body: "same body".into(),
        fix: "same fix".into(),
        confidence: 0.9,
        failure_trace: None,
        rule_id: None,
        occurrence_key: None,
        relations: vec![review_core::Relation {
            kind: review_core::RelationKind::Corroborates,
            target: review_core::finding::RelationTarget {
                kind: review_core::finding::ClaimTargetKind::Finding,
                id: key.clone(),
            },
            reason: Some("same claim, higher severity".into()),
        }],
    };
    let (escalation_id, _) = cas
        .put_artifact(
            review_core::contract::FINDING_REPORT_V1,
            review_core::Producer::Attempt {
                run_id: run_id.into(),
                node_id: "architecture".into(),
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w203".into(),
            },
            vec![escalation_result],
            Some(authority.head.clone()),
            serde_json::to_value(escalation).unwrap(),
        )
        .unwrap();
    store
        .append(
            run_id,
            &cas,
            NewEvent::new(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": key.clone(),
                    "round": 1,
                    "source": "architecture",
                    "report_id": escalation_id.clone(),
                }),
            )
            .node("architecture")
            .caused_by(authority.round_event_id.clone())
            .correlating(key.clone())
            .referencing(vec![escalation_id]),
        )
        .unwrap();
    let event_count = store.replay(run_id).unwrap().len();

    let mut resumed = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    assert_eq!(
        resumed.ledger().get(&key).unwrap().severity,
        review_core::Severity::Blocker
    );
    let second = resumed.add_canonical_stage_outputs(&stages).unwrap();
    assert_eq!(second.selected_report_ids, first.selected_report_ids);
    assert_eq!(second.input_artifact_ids, first.input_artifact_ids);
    assert_eq!(resumed.ledger().get(&key).unwrap().reports.len(), 3);
    drop(resumed);
    assert_eq!(store.replay(run_id).unwrap().len(), event_count);
}

#[test]
fn explicit_dispositions_are_immutable_and_only_disputes_contest() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1xz";
    let authority = opened_round(&mut store, &cas, run_id);
    let seed_result = cas.put_json(&serde_json::json!({"wire": "seed"})).unwrap();
    let disposition_result = cas
        .put_json(&serde_json::json!({"wire": "dispositions"}))
        .unwrap();
    let mut seed = stage();
    seed.findings.push({
        let mut finding = seed.findings[0].clone();
        finding.file = "src/drop.rs".into();
        finding.title = "Drop candidate".into();
        finding
    });
    seed.findings.push({
        let mut finding = seed.findings[0].clone();
        finding.file = "src/dispute.rs".into();
        finding.title = "Dispute candidate".into();
        finding
    });
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "seed",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &seed,
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202",
            result_artifact_id: &seed_result,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        }])
        .unwrap();
    let ids: Vec<_> = ingest
        .ledger()
        .findings()
        .iter()
        .map(|finding| finding.key.clone())
        .collect();
    drop(ingest);
    support::resolve(
        &mut store,
        &cas,
        run_id,
        &authority,
        &ids[0],
        review_store::Status::Fixed,
        "candidate fix",
    );
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest.advance().unwrap();
    assert_eq!(
        ingest.ledger().get(&ids[0]).unwrap().status,
        review_store::Status::Fixed
    );
    let dispositions: LegacyStageOutput = serde_json::from_value(serde_json::json!({
        "verdict": "request-changes",
        "summary": "explicit coverage",
        "findings": [],
        "benchmark_demands": [],
        "disputes": [
            {"claim_id": ids[0], "position": "corroborate", "reason": "reproduced"},
            {"claim_id": ids[1], "position": "not_reproduced", "reason": "branch removed"},
            {"claim_id": ids[2], "position": "dispute", "reason": "branch unreachable"}
        ]
    }))
    .unwrap();
    let reduction = ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "correctness",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &dispositions,
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w203",
            result_artifact_id: &disposition_result,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V2,
        }])
        .unwrap();

    assert_eq!(
        reduction.reducer_version,
        review_core::FINDING_REDUCER_VERSION_V2
    );
    assert_eq!(reduction.relation_ids.len(), 3);
    assert_eq!(reduction.selected_report_ids.len(), 1);
    assert_eq!(reduction.input_artifact_ids.len(), 4);
    let disposition_records: Vec<_> = reduction
        .input_artifact_ids
        .iter()
        .filter(|record_id| {
            let envelope: review_core::ArtifactEnvelope =
                serde_json::from_value(cas.get_json(record_id).unwrap()).unwrap();
            envelope.artifact_type == review_core::contract::FINDING_DISPOSITION_V1
        })
        .collect();
    assert_eq!(disposition_records.len(), 3);
    for (semantic_id, record_id) in reduction.relation_ids.iter().zip(disposition_records) {
        let envelope: review_core::ArtifactEnvelope =
            serde_json::from_value(cas.get_json(record_id).unwrap()).unwrap();
        assert_eq!(envelope.artifact_id, *semantic_id);
        assert_eq!(
            envelope.artifact_type,
            review_core::contract::FINDING_DISPOSITION_V1
        );
        let payload: review_core::FindingDispositionV1 =
            serde_json::from_value(envelope.payload).unwrap();
        assert_eq!(payload.source, "correctness");
        assert_eq!(payload.round, 2);
        assert_eq!(payload.subject_id, authority.subject);
    }
    assert_eq!(
        ingest.ledger().get(&ids[0]).unwrap().status,
        review_store::Status::Open,
        "corroboration reopens a Finding fixed in an earlier Round"
    );
    assert_eq!(ingest.ledger().get(&ids[0]).unwrap().reports.len(), 2);
    assert_eq!(
        ingest.ledger().get(&ids[1]).unwrap().status,
        review_store::Status::Open,
        "not_reproduced is evidence, not trusted fixed authority"
    );
    assert_eq!(
        ingest.ledger().get(&ids[2]).unwrap().status,
        review_store::Status::Contested
    );
}

#[test]
fn fixed_requires_current_attestation_and_verification_and_resolutions_can_expire() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1rv";
    let authority = opened_round(&mut store, &cas, run_id);
    let result = cas.put_json(&serde_json::json!({"wire": "seed"})).unwrap();
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_canonical_stage_outputs(&[CanonicalStage {
            source: "correctness",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &stage(),
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w210",
            result_artifact_id: &result,
            input_artifacts: &[],
            subject_snapshot_id: &authority.head,
            subject_id: &authority.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        }])
        .unwrap();
    let mut ledger = ingest.into_projection().into_ledger();
    let finding_id = ledger.findings()[0].key.clone();

    let base = cas.put(b"base").unwrap();
    let head = cas.put(b"changed head").unwrap();
    let change_set = review_core::ChangeSetV1::new(
        &base,
        &head,
        vec!["src/lib.rs".into()],
        vec![],
        b"diff --git a/src/lib.rs b/src/lib.rs\n",
        "git version test",
        "review.kernel/git-diff@1",
    )
    .unwrap();
    let change_set_id = cas
        .put_json(&serde_json::to_value(change_set).unwrap())
        .unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(&head, &base, &change_set_id)).unwrap())
        .unwrap();
    let round = RunEvent {
        event_id: "round-2".into(),
        run_id: run_id.into(),
        sequence: 100,
        event_type: EventType::RoundStartedV1,
        occurred_at: "2026-08-28T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: Some(subject_id.clone()),
        artifact_refs: vec![],
        payload: serde_json::to_value(RoundStartedPayloadV1 {
            round: 2,
            epoch: 1,
            campaign_manifest_id: authority.manifest,
            subject_id: subject_id.clone(),
            prior_finding_set_id: authority.findings,
            prior_demand_set_id: authority.demands,
        })
        .unwrap(),
    };
    ledger.apply_event(&round, &cas).unwrap();

    let recorded_event = |event_type, record_id: String| RunEvent {
        event_id: format!("{event_type}-{record_id}"),
        run_id: run_id.into(),
        sequence: 101,
        event_type,
        occurred_at: "2026-08-28T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: Some(finding_id.clone()),
        artifact_refs: vec![record_id.clone()],
        payload: serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
            artifact_id: record_id,
        })
        .unwrap(),
    };
    let publish = |artifact_type: &str, operation: &str, payload: serde_json::Value| {
        cas.put_artifact(
            artifact_type,
            Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: None,
                operation_id: operation.into(),
            },
            vec![],
            Some(head.clone()),
            payload,
        )
        .unwrap()
    };

    let initial_view = ledger.finding_view_id(&finding_id).unwrap();
    let attestation = review_core::ChangeAttestationV1 {
        finding_id: finding_id.clone(),
        expected_finding_view_id: initial_view.clone(),
        subject_id: subject_id.clone(),
        change_set_id: Some(change_set_id.clone()),
        changed_regions: vec![review_core::ChangedRegionV1 {
            path: "src/lib.rs".into(),
            start_line: Some(7),
            end_line: Some(9),
        }],
        actor: "implementer".into(),
        reason: "changed the guarded path".into(),
        evidence_ids: vec![],
    };
    let (attestation_record, attestation_envelope) = publish(
        review_core::contract::CHANGE_ATTESTATION_V1,
        "attest",
        serde_json::to_value(attestation).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::ChangeAttestedV1, attestation_record.clone()),
            &cas,
        )
        .unwrap();
    assert_eq!(
        ledger.finding_view(&finding_id).unwrap().status,
        review_store::Status::PendingVerification
    );
    assert!(
        ledger
            .apply_event(
                &recorded_event(EventType::ChangeAttestedV1, attestation_record),
                &cas,
            )
            .unwrap_err()
            .to_string()
            .contains("stale"),
        "the optimistic current-view binding must make replayed stale requests fail closed"
    );

    let pending_view = ledger.finding_view_id(&finding_id).unwrap();
    let verification = review_core::FixVerificationV1 {
        finding_id: finding_id.clone(),
        attestation_id: attestation_envelope.artifact_id,
        expected_finding_view_id: pending_view.clone(),
        subject_id: subject_id.clone(),
        verifier: "trusted-verifier".into(),
        policy_revision: "fix-policy@1".into(),
        positive: true,
        reason: "current claim and checks pass".into(),
        evidence_ids: vec![],
    };
    let (verification_record, verification_envelope) = publish(
        review_core::contract::FIX_VERIFICATION_V1,
        "verify",
        serde_json::to_value(verification).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::FixVerifiedV1, verification_record),
            &cas,
        )
        .unwrap();
    assert_ne!(
        ledger.finding_view(&finding_id).unwrap().status,
        review_store::Status::Fixed,
        "verification evidence alone is not a Resolution"
    );

    let fixed = review_core::FindingResolutionV1 {
        finding_id: finding_id.clone(),
        expected_finding_view_id: pending_view,
        subject_id: subject_id.clone(),
        outcome: review_core::FindingResolutionOutcome::Fixed,
        actor: "trusted-verifier".into(),
        policy_revision: "fix-policy@1".into(),
        reason: "positive current-Subject verification".into(),
        evidence_ids: vec![],
        verification_id: Some(verification_envelope.artifact_id),
        max_accepted_severity: None,
        tracking_reference: None,
        expires_at_policy_time: None,
    };
    let (fixed_record, _) = publish(
        review_core::contract::FINDING_RESOLUTION_V1,
        "fixed-resolution",
        serde_json::to_value(fixed).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::FindingResolutionRecordedV1, fixed_record),
            &cas,
        )
        .unwrap();
    assert_eq!(
        ledger.finding_view(&finding_id).unwrap().status,
        review_store::Status::Fixed
    );

    let below_current_ceiling = review_core::FindingResolutionV1 {
        finding_id: finding_id.clone(),
        expected_finding_view_id: ledger.finding_view_id(&finding_id).unwrap(),
        subject_id: subject_id.clone(),
        outcome: review_core::FindingResolutionOutcome::WontfixTracked,
        actor: "operator".into(),
        policy_revision: "risk-policy@1".into(),
        reason: "invalid exception below the current risk".into(),
        evidence_ids: vec![],
        verification_id: None,
        max_accepted_severity: Some(review_core::Severity::Minor),
        tracking_reference: Some("ISSUE-LOW".into()),
        expires_at_policy_time: Some(3),
    };
    let (below_current_record, _) = publish(
        review_core::contract::FINDING_RESOLUTION_V1,
        "below-current-wontfix-resolution",
        serde_json::to_value(below_current_ceiling).unwrap(),
    );
    assert!(
        ledger
            .apply_event(
                &recorded_event(EventType::FindingResolutionRecordedV1, below_current_record),
                &cas,
            )
            .unwrap_err()
            .to_string()
            .contains("severity ceiling")
    );

    let wontfix = review_core::FindingResolutionV1 {
        finding_id: finding_id.clone(),
        expected_finding_view_id: ledger.finding_view_id(&finding_id).unwrap(),
        subject_id: subject_id.clone(),
        outcome: review_core::FindingResolutionOutcome::WontfixTracked,
        actor: "operator".into(),
        policy_revision: "risk-policy@1".into(),
        reason: "tracked temporary exception".into(),
        evidence_ids: vec![],
        verification_id: None,
        max_accepted_severity: Some(review_core::Severity::Major),
        tracking_reference: Some("ISSUE-42".into()),
        expires_at_policy_time: Some(3),
    };
    let (wontfix_record, wontfix_envelope) = publish(
        review_core::contract::FINDING_RESOLUTION_V1,
        "wontfix-resolution",
        serde_json::to_value(wontfix).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::FindingResolutionRecordedV1, wontfix_record),
            &cas,
        )
        .unwrap();
    let policy_time = review_core::PolicyTimeV1 {
        tick: 3,
        actor: "policy".into(),
        reason: "evaluate expiry".into(),
    };
    let (time_record, _) = publish(
        review_core::contract::POLICY_TIME_V1,
        "policy-time",
        serde_json::to_value(policy_time).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::PolicyTimeAdvancedV1, time_record),
            &cas,
        )
        .unwrap();
    let already_expired = review_core::FindingResolutionV1 {
        finding_id: finding_id.clone(),
        expected_finding_view_id: ledger.finding_view_id(&finding_id).unwrap(),
        subject_id: subject_id.clone(),
        outcome: review_core::FindingResolutionOutcome::WontfixTracked,
        actor: "operator".into(),
        policy_revision: "risk-policy@2".into(),
        reason: "already expired exception".into(),
        evidence_ids: vec![],
        verification_id: None,
        max_accepted_severity: Some(review_core::Severity::Major),
        tracking_reference: Some("ISSUE-EXPIRED".into()),
        expires_at_policy_time: Some(3),
    };
    let (expired_record, _) = publish(
        review_core::contract::FINDING_RESOLUTION_V1,
        "already-expired-wontfix-resolution",
        serde_json::to_value(already_expired).unwrap(),
    );
    assert!(
        ledger
            .apply_event(
                &recorded_event(EventType::FindingResolutionRecordedV1, expired_record),
                &cas,
            )
            .unwrap_err()
            .to_string()
            .contains("persisted policy time")
    );
    assert_eq!(ledger.expiring_resolutions(3).len(), 1);
    let challenge = review_core::ResolutionChallengeV1 {
        finding_id: finding_id.clone(),
        resolution_id: wontfix_envelope.artifact_id,
        subject_id,
        kind: review_core::ResolutionChallengeKind::Expired,
        actor: "policy".into(),
        reason: "persisted policy-time expiry".into(),
        evidence_ids: vec![],
    };
    let (challenge_record, _) = publish(
        review_core::contract::RESOLUTION_CHALLENGE_V1,
        "challenge",
        serde_json::to_value(challenge).unwrap(),
    );
    ledger
        .apply_event(
            &recorded_event(EventType::FindingResolutionChallengedV1, challenge_record),
            &cas,
        )
        .unwrap();
    assert_eq!(
        ledger.finding_view(&finding_id).unwrap().status,
        review_store::Status::Contested
    );
}

// Flat `ReviewerResult@1` answers reach the canonical reduction a Campaign runs at its barrier
// only through the strict gate: every finding must satisfy FindingReport@1, and one violation
// refuses every result at that barrier, so a blocking verdict cannot degrade into an empty pass.

fn flat_stage(findings: serde_json::Value, disputes: serde_json::Value) -> LegacyStageOutput {
    serde_json::from_value(serde_json::json!({
        "verdict": "request-changes",
        "summary": null,
        "findings": findings,
        "benchmark_demands": [],
        "disputes": disputes,
    }))
    .unwrap()
}

fn flat_finding(file: &str, title: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "severity": "major",
        "file": file,
        "line": 7,
        "title": title,
        "body": body,
        "fix": "bound it",
        "confidence": 0.9
    })
}

fn key_of(ledger: &review_store::Ledger, title: &str) -> String {
    ledger
        .findings()
        .into_iter()
        .find(|finding| finding.title == title)
        .unwrap_or_else(|| panic!("no Finding titled `{title}`"))
        .key
        .clone()
}

/// Admit one finding per reviewer at one barrier of a canonical Round; on refusal, prove that
/// nothing was appended for any reviewer.
fn admit_flat(findings: &[serde_json::Value]) -> Result<review_store::Ledger, String> {
    const REVIEWERS: [(&str, &str); 2] = [
        ("deep", "01jd8m4qz9k7v3n2p6r8t0w301"),
        ("cross", "01jd8m4qz9k7v3n2p6r8t0w302"),
    ];
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round = opened_round(&mut store, &cas, "run");
    let before = store.len("run").unwrap();
    let outputs: Vec<_> = findings
        .iter()
        .map(|finding| flat_stage(serde_json::json!([finding]), serde_json::json!([])))
        .collect();
    let results: Vec<_> = REVIEWERS
        .iter()
        .zip(&outputs)
        .map(|((source, attempt), output)| (*source, *attempt, output))
        .collect();
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&round.round_event_id);
    match support::add_flat_results(&mut ingest, &cas, "run", &round, &results) {
        Ok(_) => Ok(ingest.into_projection().into_ledger()),
        Err(error) => {
            assert!(ingest.ledger().is_empty());
            drop(ingest);
            assert_eq!(
                store.len("run").unwrap(),
                before,
                "a refusal appends nothing"
            );
            Err(error.to_string())
        }
    }
}

#[test]
fn a_contract_complete_flat_finding_is_ingested() {
    let ledger = admit_flat(&[flat_finding("src/a.rs", "T", "b")]).unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger.findings()[0].fix, "bound it");
    assert!(ledger.findings()[0].key.starts_with("sha256:"));
}

/// The violation is in the second reviewer's result; the first reviewer's valid result is
/// refused with it.
#[test]
fn a_flat_finding_that_violates_finding_report_v1_refuses_every_result_at_the_barrier() {
    for (field, value) in [
        ("fix", serde_json::Value::Null),
        ("confidence", serde_json::json!(1.5)),
        ("line", serde_json::json!(0)),
    ] {
        let mut finding = flat_finding("src/b.rs", "T", "b");
        finding[field] = value;
        let error = admit_flat(&[flat_finding("src/a.rs", "valid", "b"), finding]).unwrap_err();
        assert!(
            error.contains("cross finding 0 violates FindingReport@1"),
            "{field}: {error}"
        );
    }
}

/// An empty file is a change-wide claim: empty locations are valid FindingReport@1.
#[test]
fn a_change_wide_flat_finding_is_admitted() {
    let mut finding = flat_finding("", "Whole-change concern", "b");
    finding["line"] = serde_json::Value::Null;
    let ledger = admit_flat(&[finding]).unwrap();
    assert_eq!(ledger.findings()[0].file, "(change-wide)");
}

#[test]
fn a_noncanonical_report_path_is_refused_instead_of_projecting_out() {
    let error = admit_flat(&[flat_finding("./src/in.rs", "bad spelling", "body")]).unwrap_err();
    assert!(
        error.contains("canonical repository-relative path"),
        "{error}"
    );
}

/// Two reviewers naming the same rule occurrence at one barrier share one Finding, and both
/// Reports stay attached as distinct artifacts. The same path and title without an occurrence
/// is another Finding: presentation is never identity.
#[test]
fn reviewers_naming_one_rule_occurrence_share_a_finding_and_keep_every_report() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round = opened_round(&mut store, &cas, "run");
    let occurrence = |body: &str| {
        let mut finding = flat_finding("src/a.rs", "Retry loop can spin forever", body);
        finding["rule_id"] = serde_json::json!("retry/unbounded@1");
        finding["occurrence_key"] = serde_json::json!("src/a.rs#retry");
        finding
    };
    let deep = flat_stage(
        serde_json::json!([occurrence("deep")]),
        serde_json::json!([]),
    );
    let cross = flat_stage(
        serde_json::json!([
            occurrence("cross"),
            flat_finding("src/a.rs", "Retry loop can spin forever", "no occurrence"),
        ]),
        serde_json::json!([]),
    );
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&round.round_event_id);
    support::add_flat_results(
        &mut ingest,
        &cas,
        "run",
        &round,
        &[
            ("deep", "01jd8m4qz9k7v3n2p6r8t0w301", &deep),
            ("cross", "01jd8m4qz9k7v3n2p6r8t0w302", &cross),
        ],
    )
    .unwrap();
    drop(ingest);

    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    assert_eq!(ledger.len(), 2);
    let shared = ledger
        .findings()
        .into_iter()
        .find(|finding| finding.reports.len() == 2)
        .expect("the shared occurrence is one Finding");
    assert_eq!(shared.source, "deep");
    assert_eq!(shared.body, "deep");
    let sources: Vec<&str> = shared.reports.iter().map(|r| r.source.as_str()).collect();
    assert_eq!(sources, ["deep", "cross"]);
    assert_ne!(shared.reports[0].report_id, shared.reports[1].report_id);
}

/// A reviewer's `refute` on a prior claim contests it, which blocks convergence and flags the
/// claim for human adjudication.
#[test]
fn a_refute_dispute_contests_the_prior_claim() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round = opened_round(&mut store, &cas, "run");
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&round.round_event_id);
    let claim = flat_stage(
        serde_json::json!([flat_finding("src/a.rs", "Claim", "b")]),
        serde_json::json!([]),
    );
    support::add_flat_results(
        &mut ingest,
        &cas,
        "run",
        &round,
        &[("architecture", "01jd8m4qz9k7v3n2p6r8t0w301", &claim)],
    )
    .unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    assert_eq!(
        ingest.ledger().get(&key).unwrap().status,
        review_store::Status::Open
    );

    let refutation = flat_stage(
        serde_json::json!([]),
        serde_json::json!([{"claim_id": key, "position": "refute", "reason": "not reproducible"}]),
    );
    support::add_flat_results(
        &mut ingest,
        &cas,
        "run",
        &round,
        &[("performance", "01jd8m4qz9k7v3n2p6r8t0w302", &refutation)],
    )
    .unwrap();
    drop(ingest);

    // Rebuilt from the log alone, the claim is contested: the dispute reached the Ledger.
    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    let finding = ledger.get(&key).unwrap();
    assert_eq!(finding.status, review_store::Status::Contested);
    assert_eq!(
        finding.current_note(),
        Some("contested by performance: not reproducible")
    );
}

/// A typed Resolution that declines a claim is challenged, not silently kept, when a reviewer
/// re-reports the claim in scope at a higher severity than the Resolution accepted: the reducer
/// records a `HigherSeverity` challenge and the Finding becomes `contested`. A re-report at the
/// accepted severity leaves the Resolution standing.
#[test]
fn an_in_scope_higher_severity_re_report_challenges_a_typed_declined_resolution() {
    use review_core::{FindingResolutionOutcome, FindingResolutionV1, Severity};
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let run_id = "01jd8m4qz9k7v3n2p6r8t0w1ch";
    let round = opened_round(&mut store, &cas, run_id);
    let claim = |title: &str, severity: &str| {
        let mut finding = flat_finding("src/lib.rs", title, "b");
        finding["severity"] = serde_json::json!(severity);
        finding["rule_id"] = serde_json::json!("review/defect@1");
        finding["occurrence_key"] = serde_json::json!(title);
        finding
    };
    let first = flat_stage(
        serde_json::json!([
            claim("Rejected then escalated", "major"),
            claim("Accepted risk then escalated", "major"),
            claim("Rejected then repeated", "major"),
        ]),
        serde_json::json!([]),
    );
    let mut ingest = Ingest::new(&mut store, &cas, run_id)
        .unwrap()
        .under_round(&round.round_event_id);
    support::add_flat_results(
        &mut ingest,
        &cas,
        run_id,
        &round,
        &[("deep", "01jd8m4qz9k7v3n2p6r8t0w401", &first)],
    )
    .unwrap();
    let mut ledger = ingest.into_projection().into_ledger();

    // Operator decisions after the first Round, in the scope of the same Subject.
    for (title, outcome) in [
        (
            "Rejected then escalated",
            FindingResolutionOutcome::Rejected,
        ),
        (
            "Accepted risk then escalated",
            FindingResolutionOutcome::WontfixTracked,
        ),
        ("Rejected then repeated", FindingResolutionOutcome::Rejected),
    ] {
        let finding_id = key_of(&ledger, title);
        let tracked = outcome == FindingResolutionOutcome::WontfixTracked;
        let resolution = FindingResolutionV1 {
            expected_finding_view_id: ledger.finding_view_id(&finding_id).unwrap(),
            finding_id,
            subject_id: round.subject.clone(),
            outcome,
            actor: "operator".into(),
            policy_revision: "risk-policy@1".into(),
            reason: format!("decided: {title}"),
            evidence_ids: vec![],
            verification_id: None,
            max_accepted_severity: tracked.then_some(Severity::Major),
            tracking_reference: tracked.then(|| "ISSUE-7".to_string()),
            expires_at_policy_time: tracked.then_some(9),
        };
        apply_resolution(
            &mut ledger,
            &cas,
            run_id,
            &round.head,
            &format!("resolve {title}"),
            resolution,
        );
    }

    // The next Round reviews the same Subject.
    for (event_type, payload) in [
        (
            EventType::RoundStartedV1,
            serde_json::to_value(RoundStartedPayloadV1 {
                round: 2,
                epoch: 1,
                campaign_manifest_id: round.manifest.clone(),
                subject_id: round.subject.clone(),
                prior_finding_set_id: round.findings.clone(),
                prior_demand_set_id: round.demands.clone(),
            })
            .unwrap(),
        ),
        (
            EventType::GenerationAdvancedV1,
            serde_json::json!({ "round": 2 }),
        ),
    ] {
        ledger
            .apply_event(
                &RunEvent {
                    event_id: format!("round-2-{event_type}"),
                    run_id: run_id.into(),
                    sequence: 200,
                    event_type,
                    occurred_at: "2026-08-28T00:00:00Z".into(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: None,
                    artifact_refs: vec![],
                    payload,
                },
                &cas,
            )
            .unwrap();
    }
    let second = flat_stage(
        serde_json::json!([
            claim("Rejected then escalated", "blocker"),
            claim("Accepted risk then escalated", "blocker"),
            claim("Rejected then repeated", "major"),
        ]),
        serde_json::json!([]),
    );
    let attempt_id = "01jd8m4qz9k7v3n2p6r8t0w402";
    let result = task_result(&cas, run_id, "cross", attempt_id, &[], &round.head, &second);
    let reduced = review_store::prepare_canonical_task_review(
        &cas,
        run_id,
        &ledger,
        &[CanonicalStage {
            source: "cross",
            demand_requirement: review_core::DemandRequirement::Required,
            stage: &second,
            attempt_id,
            result_artifact_id: &result,
            input_artifacts: &[],
            subject_snapshot_id: &round.head,
            subject_id: &round.subject,
            result_contract: review_core::ReviewerResultContract::V2,
        }],
    )
    .unwrap();

    let challenged: Vec<String> = reduced
        .events
        .iter()
        .filter(|event| event.event_type == EventType::FindingResolutionChallengedV1)
        .map(|event| event.correlation_id.clone().unwrap())
        .collect();
    assert_eq!(
        challenged,
        [
            key_of(&ledger, "Rejected then escalated"),
            key_of(&ledger, "Accepted risk then escalated"),
        ]
    );
    let status = |title: &str| {
        reduced
            .ledger
            .finding_view(&key_of(&ledger, title))
            .unwrap()
            .status
    };
    assert_eq!(
        status("Rejected then escalated"),
        review_store::Status::Contested
    );
    assert_eq!(
        status("Accepted risk then escalated"),
        review_store::Status::Contested
    );
    assert_eq!(
        status("Rejected then repeated"),
        review_store::Status::Rejected
    );
}
