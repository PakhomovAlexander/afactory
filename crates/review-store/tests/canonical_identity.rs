use review_core::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, LegacyStageOutput, RoundStartedPayloadV1, SubjectKind,
    SubjectV1,
};
use review_store::{
    CanonicalStage, Cas, ConvergencePolicy, EventStore, Ingest, LedgerProjection, NewEvent,
};

struct Authority {
    authority: String,
    manifest: String,
    subject: String,
    head: String,
    findings: String,
    demands: String,
    round_event_id: String,
}

fn opened_round(store: &mut EventStore, cas: &Cas, run_id: &str) -> Authority {
    let authority = cas.put(b"authority").unwrap();
    let pipeline = cas
        .put(
            br#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
runner = { program = "/bin/true" }
"#,
        )
        .unwrap();
    let lock = cas.put(b"lock").unwrap();
    let findings = cas.put(b"finding genesis").unwrap();
    let demands = cas.put(b"demand genesis").unwrap();
    let manifest = cas
        .put_json(
            &serde_json::to_value(CampaignManifestV1 {
                authority_snapshot_id: authority.clone(),
                subject_kind: SubjectKind::WholeTree,
                base_snapshot_id: None,
                pipeline: AuthorityFileV1 {
                    path: "review.toml".into(),
                    artifact_id: pipeline.clone(),
                },
                reviewer_lock: AuthorityFileV1 {
                    path: "review.lock".into(),
                    artifact_id: lock,
                },
                reviewers: vec![],
                execution_policy_ids: vec![pipeline],
                project_policy_ids: vec![],
                convergence: CampaignConvergenceV1 {
                    clean_rounds: 1,
                    max_rounds: 2,
                    gate: "major".into(),
                },
                reviewer_timeout_seconds: 60,
                check_timeout_seconds: Some(3600),
                git_timeout_seconds: Some(300),
                budgets: None,
                focus: None,
                finding_identity_policy: CANONICAL_FINDING_IDENTITY_POLICY.into(),
                finding_genesis_id: findings.clone(),
                demand_genesis_id: demands.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    let head = cas.put(b"head snapshot").unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(CampaignOpenedPayloadV1 {
                    campaign_manifest_id: manifest.clone(),
                    authority_snapshot_id: authority.clone(),
                })
                .unwrap(),
            )
            .referencing(vec![authority.clone(), manifest.clone()]),
        )
        .unwrap();
    let round = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(RoundStartedPayloadV1 {
                    round: 1,
                    epoch: 1,
                    campaign_manifest_id: manifest.clone(),
                    subject_id: subject.clone(),
                    prior_finding_set_id: findings.clone(),
                    prior_demand_set_id: demands.clone(),
                })
                .unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(vec![
                authority.clone(),
                manifest.clone(),
                subject.clone(),
                head.clone(),
                findings.clone(),
                demands.clone(),
            ]),
        )
        .unwrap();
    Authority {
        authority,
        manifest,
        subject,
        head,
        findings,
        demands,
        round_event_id: round.event_id,
    }
}

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
    ingest
        .resolve(&ids[0], review_store::Status::Fixed, Some("candidate fix"))
        .unwrap();
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
