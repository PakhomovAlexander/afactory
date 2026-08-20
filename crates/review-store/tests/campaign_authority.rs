use review_core::event::{AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1};
use review_core::{
    AuthorityFileV1, CampaignConvergenceV1, CampaignManifestV1, CampaignOpenedPayloadV1, EventType,
    NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, PortArtifactsV1, PortCardinality,
    RoundInputSupersededPayloadV1, RoundStartedPayloadV1, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportPayloadV2, RunVerdictV2, SnapshotAffinity, SubjectKind, SubjectV1,
};
use review_store::{Cas, EventStore, NewEvent};

struct Authority {
    authority: String,
    manifest: String,
    subject: String,
    head: String,
    findings: String,
    demands: String,
}

fn authority(cas: &Cas, label: &str) -> Authority {
    let authority = cas.put(format!("{label} authority").as_bytes()).unwrap();
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
    let lock = cas.put(b"test lock").unwrap();
    let finding_genesis = cas.put(b"finding genesis").unwrap();
    let demand_genesis = cas.put(b"demand genesis").unwrap();
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
                budgets: None,
                focus: None,
                finding_identity_policy: "legacy-path-title@1".into(),
                finding_genesis_id: finding_genesis,
                demand_genesis_id: demand_genesis,
            })
            .unwrap(),
        )
        .unwrap();
    let head = cas.put(format!("{label} head").as_bytes()).unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    Authority {
        authority,
        manifest,
        subject,
        head,
        findings: cas.put(format!("{label} findings").as_bytes()).unwrap(),
        demands: cas.put(format!("{label} demands").as_bytes()).unwrap(),
    }
}

fn round_payload(ids: &Authority, epoch: u32) -> RoundStartedPayloadV1 {
    RoundStartedPayloadV1 {
        round: 1,
        epoch,
        campaign_manifest_id: ids.manifest.clone(),
        subject_id: ids.subject.clone(),
        prior_finding_set_id: ids.findings.clone(),
        prior_demand_set_id: ids.demands.clone(),
    }
}

fn round_refs(ids: &Authority) -> Vec<String> {
    vec![
        ids.authority.clone(),
        ids.manifest.clone(),
        ids.head.clone(),
        ids.subject.clone(),
        ids.findings.clone(),
        ids.demands.clone(),
    ]
}

fn opened_round(
    store: &mut EventStore,
    cas: &Cas,
    run_id: &str,
    ids: &Authority,
) -> review_core::RunEvent {
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(CampaignOpenedPayloadV1 {
                    campaign_manifest_id: ids.manifest.clone(),
                    authority_snapshot_id: ids.authority.clone(),
                })
                .unwrap(),
            )
            .referencing(vec![ids.authority.clone(), ids.manifest.clone()]),
        )
        .unwrap();
    store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(round_payload(ids, 1)).unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(round_refs(ids)),
        )
        .unwrap()
}

#[test]
fn a_round_requires_a_durable_campaign() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "first");

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(round_payload(&ids, 1)).unwrap(),
            )
            .referencing(round_refs(&ids)),
        )
        .unwrap_err();

    assert!(error.to_string().contains("CampaignOpened@1"), "{error}");
    assert!(store.replay("run").unwrap().is_empty());
}

#[test]
fn a_superseded_epoch_cannot_publish_late_output() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let old = authority(&cas, "old");
    let old_round = opened_round(&mut store, &cas, "run", &old);
    let attempt = "a".repeat(26);
    store
        .append(
            "run",
            &cas,
            NewEvent::new(EventType::AttemptDispatchedV1, serde_json::json!({}))
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&old_round.event_id),
        )
        .unwrap();

    let replacement = authority(&cas, "replacement");
    let superseded = RoundInputSupersededPayloadV1 {
        round: 1,
        old_epoch: 1,
        new_epoch: 2,
        campaign_manifest_id: old.manifest.clone(),
        old_subject_id: old.subject.clone(),
        replacement_subject_id: replacement.subject.clone(),
    };
    let mut replacement_payload = round_payload(&replacement, 2);
    replacement_payload.campaign_manifest_id = old.manifest.clone();
    let mut replacement_refs = round_refs(&replacement);
    replacement_refs.push(old.manifest.clone());
    let published = store
        .append_batch(
            "run",
            &cas,
            &[
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(superseded).unwrap(),
                )
                .caused_by(&old_round.event_id),
                NewEvent::new(
                    EventType::AttemptFencedV1,
                    serde_json::json!({"reason": "superseded", "charged": null}),
                )
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&old_round.event_id),
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(replacement_payload).unwrap(),
                )
                .caused_by(&old_round.event_id)
                .referencing(replacement_refs),
            ],
        )
        .unwrap();
    assert_eq!(published.len(), 3);

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptFailedV1,
                serde_json::json!({"error": "late output"}),
            )
            .node("reviewer")
            .attempt(attempt)
            .caused_by(old_round.event_id),
        )
        .unwrap_err();
    assert!(error.to_string().contains("active Round epoch"), "{error}");
}

#[test]
fn a_terminal_report_requires_matching_output_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "report");
    let round = opened_round(&mut store, &cas, "run", &ids);
    let output = cas.put(b"unreceipted output").unwrap();
    let report = RunReportPayloadV2 {
        outcomes: vec![RunNodeReportV2 {
            node: "reviewer".into(),
            outcome: RunNodeOutcomeV2::Completed {
                output_artifacts: vec![output],
            },
        }],
        blocked_gates: vec![],
        verdict: RunVerdictV2::Pass,
        spent_tokens: None,
    };

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::RunReportV2,
                serde_json::to_value(report).unwrap(),
            )
            .caused_by(round.event_id),
        )
        .unwrap_err();

    assert!(
        error.to_string().contains("without a durable receipt"),
        "{error}"
    );
}

#[test]
fn a_campaign_cannot_append_a_legacy_run_report() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "legacy-report");
    let round = opened_round(&mut store, &cas, "run", &ids);

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::RunReportV1,
                serde_json::json!({
                    "outcomes": [{"node": "reviewer", "status": "completed", "detail": {}}],
                    "blocked_gates": [],
                    "verdict": "Pass",
                    "spent_tokens": 0
                }),
            )
            .caused_by(round.event_id),
        )
        .unwrap_err();

    assert!(error.to_string().contains("replay-only"), "{error}");
}

#[test]
fn a_receipt_rejects_an_artifact_that_violates_its_pinned_type() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "typed-receipt");
    let round = opened_round(&mut store, &cas, "run", &ids);
    let attempt = "b".repeat(26);
    let malformed_result = cas.put_json(&serde_json::json!({})).unwrap();
    let provenance = cas.put(b"test provenance").unwrap();

    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeInvocationV1,
                serde_json::to_value(NodeInvocationPayloadV1 {
                    node: "reviewer".into(),
                    inputs: vec![],
                })
                .unwrap(),
            )
            .node("reviewer")
            .caused_by(&round.event_id),
        )
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptDispatchedV1,
                serde_json::to_value(AttemptDispatchedPayloadV1 {
                    reserved: Some(1),
                    prior_findings: None,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptAdmittedV1,
                serde_json::to_value(AttemptAdmittedPayloadV1 {
                    selection: "selected".into(),
                    cost_tokens: 1,
                    result_artifact: Some(malformed_result.clone()),
                    provenance_artifact: Some(provenance.clone()),
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id)
            .referencing(vec![malformed_result.clone(), provenance]),
        )
        .unwrap();
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeOutputReceiptV1,
                serde_json::to_value(NodeOutputReceiptPayloadV1 {
                    node: "reviewer".into(),
                    outputs: vec![PortArtifactsV1 {
                        port: "out".into(),
                        artifact_type: "review.kernel/ReviewerResult@1".into(),
                        cardinality: PortCardinality::One,
                        optional: false,
                        snapshot_affinity: SnapshotAffinity::Any,
                        artifact_ids: vec![malformed_result.clone()],
                        subject_snapshot_id: None,
                    }],
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(attempt)
            .caused_by(&round.event_id)
            .referencing(vec![malformed_result]),
        )
        .unwrap_err();

    assert!(error.to_string().contains("ReviewerResult@1"), "{error}");
}
