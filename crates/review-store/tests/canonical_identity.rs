use review_core::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, LegacyStageOutput, RoundStartedPayloadV1, SubjectKind,
    SubjectV1,
};
use review_store::{CanonicalStage, Cas, EventStore, Ingest, LedgerProjection, NewEvent};

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
            },
            CanonicalStage {
                source: "correctness",
                stage: &output,
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w203",
                result_artifact_id: &result_b,
                input_artifacts: std::slice::from_ref(&input),
                subject_snapshot_id: &authority.head,
            },
        ])
        .unwrap();

    assert_eq!(ingest.ledger().len(), 2);
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

    // Keep all bootstrap IDs live in the test so accidental fixture weakening is visible.
    assert!(cas.contains(&authority.authority));
    assert!(cas.contains(&authority.manifest));
    assert!(cas.contains(&authority.subject));
    assert!(cas.contains(&authority.findings));
    assert!(cas.contains(&authority.demands));
}
