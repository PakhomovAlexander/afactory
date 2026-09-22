use review_core::event::{
    AttemptAdmittedPayloadV1, AttemptDispatchedPayloadV1, AttemptFeedbackPayloadV1,
    AttemptFencedPayloadV1, AttemptInputPayloadV1,
};
use review_core::{
    AuthorityFileV1, BrokerCredentialModeV1, BrokerFailureReasonV1, BrokerOperationOutcomeV1,
    BrokerOperationPolicyV1, BrokerOperationReceiptV1, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1,
    PortArtifactsV1, PortCardinality, ReviewerExecutionBindingV1, RoundInputSupersededPayloadV1,
    RoundStartedPayloadV1, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV3, RunVerdictV3,
    SnapshotAffinity, SubjectKind, SubjectV1,
};
use review_store::{Cas, EventStore, NewEvent, StoreError};

struct Authority {
    authority: String,
    manifest: String,
    subject: String,
    head: String,
    findings: String,
    demands: String,
}

fn authority(cas: &Cas, label: &str) -> Authority {
    authority_with_pipeline(
        cas,
        label,
        br#"version = 2
check_timeout_seconds = 3600
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
runner = { program = "/bin/true" }
"#,
    )
}

fn authority_with_pipeline(cas: &Cas, label: &str, pipeline: &[u8]) -> Authority {
    let authority = cas.put(format!("{label} authority").as_bytes()).unwrap();
    let pipeline = cas.put(pipeline).unwrap();
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
                check_timeout_seconds: 3600,
                git_timeout_seconds: 300,
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
fn a_campaign_requires_a_readable_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let authority = cas.put(b"authority").unwrap();
    let manifest = cas.put(b"not a CampaignManifest").unwrap();

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(CampaignOpenedPayloadV1 {
                    campaign_manifest_id: manifest.clone(),
                    authority_snapshot_id: authority.clone(),
                })
                .unwrap(),
            )
            .referencing(vec![authority, manifest]),
        )
        .unwrap_err();

    assert!(error.to_string().contains("unreadable CampaignManifest"));
    assert!(store.replay("run").unwrap().is_empty());
}

#[test]
fn runtime_events_require_an_active_round() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::GenerationAdvancedV1,
                serde_json::json!({"round": 1}),
            ),
        )
        .unwrap_err();

    assert!(error.to_string().contains("requires an active Round"));
    assert!(store.replay("run").unwrap().is_empty());
}

#[test]
fn events_that_do_not_use_the_plan_do_not_reparse_it() {
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "plan-cache");
    let round = opened_round(&mut store, &cas, "run", &ids);

    let hex = ids.manifest.strip_prefix("sha256:").unwrap();
    std::fs::write(
        cas_root.join("objects").join(&hex[..2]).join(&hex[2..]),
        b"corrupt after Campaign opening",
    )
    .unwrap();

    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::GenerationAdvancedV1,
                serde_json::json!({"round": 1}),
            )
            .caused_by(&round.event_id),
        )
        .expect("a reducer event does not consume the Campaign plan");

    let error = store
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
            .caused_by(round.event_id),
        )
        .unwrap_err();
    assert!(error.to_string().contains("CampaignManifest"), "{error}");
}

#[test]
fn retry_input_and_its_dispatch_are_one_atomic_transition() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "retry-input");
    let round = opened_round(&mut store, &cas, "run", &ids);
    let refusal_history = cas
        .put_json(&serde_json::json!(["first answer was inadmissible"]))
        .unwrap();
    let attempt = "a".repeat(26);
    let input = || {
        NewEvent::new(
            EventType::AttemptInputV1,
            serde_json::to_value(AttemptInputPayloadV1 {
                refusal_history_id: refusal_history.clone(),
            })
            .unwrap(),
        )
        .node("reviewer")
        .attempt(&attempt)
        .caused_by(&round.event_id)
        .referencing(vec![refusal_history.clone()])
    };

    let error = store.append("run", &cas, input()).unwrap_err();
    assert!(error.to_string().contains("atomically"), "{error}");

    store
        .append_batch(
            "run",
            &cas,
            &[
                input(),
                NewEvent::new(
                    EventType::AttemptDispatchedV1,
                    serde_json::to_value(AttemptDispatchedPayloadV1 {
                        reserved: None,
                        prior_findings: None,
                    })
                    .unwrap(),
                )
                .node("reviewer")
                .attempt(attempt)
                .caused_by(round.event_id),
            ],
        )
        .unwrap();
}

#[test]
fn retry_feedback_and_its_terminal_attempt_are_one_atomic_transition() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "retry-feedback");
    let round = opened_round(&mut store, &cas, "run", &ids);
    let attempt = "d".repeat(26);
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptDispatchedV1,
                serde_json::to_value(AttemptDispatchedPayloadV1 {
                    reserved: None,
                    prior_findings: None,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap();
    let history = cas
        .put_json(&serde_json::json!(["the answer violated the contract"]))
        .unwrap();
    let feedback = || {
        NewEvent::new(
            EventType::AttemptFeedbackV1,
            serde_json::to_value(AttemptFeedbackPayloadV1 {
                refusal_history_id: history.clone(),
            })
            .unwrap(),
        )
        .node("reviewer")
        .attempt(&attempt)
        .caused_by(&round.event_id)
        .referencing(vec![history.clone()])
    };

    let error = store.append("run", &cas, feedback()).unwrap_err();
    assert!(error.to_string().contains("atomically"), "{error}");

    store
        .append_batch(
            "run",
            &cas,
            &[
                NewEvent::new(
                    EventType::AttemptFailedV1,
                    serde_json::json!({"error": "invalid result", "charged": 5}),
                )
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&round.event_id),
                feedback(),
            ],
        )
        .unwrap();
}

#[test]
fn node_metadata_must_match_its_payload() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "metadata");
    let round = opened_round(&mut store, &cas, "run", &ids);

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeInvocationV1,
                serde_json::json!({"node": "invented", "inputs": []}),
            )
            .node("reviewer")
            .caused_by(round.event_id),
        )
        .unwrap_err();

    assert!(error.to_string().contains("metadata disagrees"));
}

#[test]
fn supersession_requires_every_outstanding_attempt_to_be_fenced() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let old = authority(&cas, "old-unfenced");
    let old_round = opened_round(&mut store, &cas, "run", &old);
    store
        .append(
            "run",
            &cas,
            NewEvent::new(EventType::AttemptDispatchedV1, serde_json::json!({}))
                .node("reviewer")
                .attempt("c".repeat(26))
                .caused_by(&old_round.event_id),
        )
        .unwrap();

    let replacement = authority(&cas, "replacement-unfenced");
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

    let error = store
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
                    EventType::RoundStartedV1,
                    serde_json::to_value(replacement_payload).unwrap(),
                )
                .caused_by(&old_round.event_id)
                .referencing(replacement_refs),
            ],
        )
        .unwrap_err();

    assert!(error.to_string().contains("outstanding attempts unfenced"));
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
    let report = RunReportPayloadV3 {
        outcomes: vec![RunNodeReportV2 {
            node: "reviewer".into(),
            outcome: RunNodeOutcomeV2::Completed {
                output_artifacts: vec![output],
            },
        }],
        blocked_gates: vec![],
        verdict: RunVerdictV3::Pass,
        spent_tokens: None,
    };

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::RunReportV3,
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
fn a_receipt_rejects_a_noncanonical_report_path_in_its_pinned_type() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority(&cas, "typed-receipt");
    let round = opened_round(&mut store, &cas, "run", &ids);
    let attempt = "b".repeat(26);
    let malformed_result = cas
        .put_json(&serde_json::json!({
            "verdict": "request-changes",
            "summary": null,
            "reports": [{
                "severity": "major",
                "file": "./src/main.rs",
                "line": 1,
                "title": "bad path",
                "body": "body",
                "fix": "fix",
                "confidence": 0.9
            }],
            "benchmark_demands": [],
            "disputes": []
        }))
        .unwrap();
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

    assert!(error.to_string().contains("canonical"), "{error}");
}

#[test]
fn broker_evidence_cannot_forge_a_lease_epoch_or_exceed_pinned_calls() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let ids = authority_with_pipeline(
        &cas,
        "broker",
        br#"version = 4
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "brokered", operations = [{ name = "model_inference", destination = "provider.test", method = "responses.create", max_request_bytes = 32, max_response_bytes = 32, max_calls = 2, max_usage = 10 }] }
"#,
    );
    let round = opened_round(&mut store, &cas, "run", &ids);
    let attempt = "a".repeat(26);
    let handle = "b".repeat(26);
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptDispatchedV1,
                serde_json::to_value(AttemptDispatchedPayloadV1 {
                    reserved: None,
                    prior_findings: None,
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap();
    let operation = BrokerOperationPolicyV1 {
        name: "model_inference".into(),
        destination: "provider.test".into(),
        method: "responses.create".into(),
        max_request_bytes: 32,
        max_response_bytes: 32,
        max_calls: 2,
        max_usage: 10,
    };
    let binding = |lease_epoch| ReviewerExecutionBindingV1 {
        node: "reviewer".into(),
        attempt_id: attempt.clone(),
        lease_epoch,
        credential_mode: BrokerCredentialModeV1::Brokered,
        auto_apply: false,
        broker_handle: Some(handle.clone()),
        operations: vec![operation.clone()],
        admitted: true,
    };
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::ReviewerExecutionBoundV1,
                serde_json::to_value(binding(2)).unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap_err();
    assert!(error.to_string().contains("dispatch epoch"), "{error}");
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::ReviewerExecutionBoundV1,
                serde_json::to_value(binding(1)).unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap();

    let receipt = |ordinal, outcome, failure_reason, reserved_usage, charged_usage| {
        BrokerOperationReceiptV1 {
            handle_id: handle.clone(),
            node: "reviewer".into(),
            attempt_id: attempt.clone(),
            lease_epoch: 1,
            operation: "model_inference".into(),
            destination: "provider.test".into(),
            method: "responses.create".into(),
            ordinal,
            outcome,
            failure_reason,
            request_digest: format!("sha256:{}", "c".repeat(64)),
            response_digest: (outcome == BrokerOperationOutcomeV1::Succeeded)
                .then(|| format!("sha256:{}", "d".repeat(64))),
            request_bytes: 7,
            response_bytes: if outcome == BrokerOperationOutcomeV1::Succeeded {
                8
            } else {
                0
            },
            reserved_usage,
            charged_usage,
        }
    };
    let append_receipt = |store: &mut EventStore, receipt: BrokerOperationReceiptV1| {
        store.append(
            "run",
            &cas,
            NewEvent::new(
                EventType::BrokerOperationCompletedV1,
                serde_json::to_value(receipt).unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
    };
    let over_budget = receipt(1, BrokerOperationOutcomeV1::Succeeded, None, 11, 7);
    let error = append_receipt(&mut store, over_budget).unwrap_err();
    assert!(error.to_string().contains("pinned policy"), "{error}");

    append_receipt(
        &mut store,
        receipt(1, BrokerOperationOutcomeV1::Succeeded, None, 10, 7),
    )
    .unwrap();
    let extra_call = receipt(2, BrokerOperationOutcomeV1::Succeeded, None, 4, 1);
    let error = append_receipt(&mut store, extra_call).unwrap_err();
    assert!(error.to_string().contains("pinned policy"), "{error}");

    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptFencedV1,
                serde_json::to_value(AttemptFencedPayloadV1 {
                    reason: "under-settled broker fence".into(),
                    charged: Some(9),
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap_err();
    assert!(error.to_string().contains("fence authority"), "{error}");
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::AttemptFencedV1,
                serde_json::to_value(AttemptFencedPayloadV1 {
                    reason: "test fence".into(),
                    charged: Some(10),
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(&attempt)
            .caused_by(&round.event_id),
        )
        .unwrap();
    let late_success = receipt(3, BrokerOperationOutcomeV1::Succeeded, None, 1, 1);
    assert!(matches!(
        append_receipt(&mut store, late_success),
        Err(StoreError::AttemptNotCurrent)
    ));
    append_receipt(
        &mut store,
        receipt(
            2,
            BrokerOperationOutcomeV1::Revoked,
            Some(BrokerFailureReasonV1::AuthorityRevoked),
            3,
            4,
        ),
    )
    .expect("the fenced Attempt preserves a late observed overrun");
    assert_eq!(
        store
            .round_committed_tokens("run", &round.event_id)
            .unwrap(),
        11
    );
    let replacement = authority(&cas, "replacement-broker");
    let superseded = RoundInputSupersededPayloadV1 {
        round: 1,
        old_epoch: 1,
        new_epoch: 2,
        campaign_manifest_id: ids.manifest.clone(),
        old_subject_id: ids.subject.clone(),
        replacement_subject_id: replacement.subject.clone(),
    };
    let mut replacement_payload = round_payload(&replacement, 2);
    replacement_payload.campaign_manifest_id = ids.manifest.clone();
    let mut replacement_refs = round_refs(&replacement);
    replacement_refs.push(ids.manifest.clone());
    store
        .append_batch(
            "run",
            &cas,
            &[
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(superseded).unwrap(),
                )
                .caused_by(&round.event_id),
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(replacement_payload).unwrap(),
                )
                .caused_by(&round.event_id)
                .referencing(replacement_refs),
            ],
        )
        .unwrap();
    let error = append_receipt(
        &mut store,
        receipt(
            3,
            BrokerOperationOutcomeV1::Revoked,
            Some(BrokerFailureReasonV1::AuthorityRevoked),
            1,
            1,
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("terminal handle"), "{error}");
    append_receipt(
        &mut store,
        receipt(
            3,
            BrokerOperationOutcomeV1::Revoked,
            Some(BrokerFailureReasonV1::AuthorityRevoked),
            1,
            0,
        ),
    )
    .expect("a revoked handle may append only a zero-charge revoked acknowledgement");
}

#[test]
fn terminal_broker_failures_allow_only_revoked_acknowledgements() {
    let terminal_cases = [
        (
            "credential-exposure",
            BrokerOperationOutcomeV1::Failed,
            BrokerFailureReasonV1::CredentialExposure,
            false,
            10,
            7,
        ),
        (
            "usage-overrun",
            BrokerOperationOutcomeV1::Failed,
            BrokerFailureReasonV1::UsageOverrun,
            true,
            10,
            11,
        ),
        (
            "authority-revoked",
            BrokerOperationOutcomeV1::Revoked,
            BrokerFailureReasonV1::AuthorityRevoked,
            true,
            10,
            7,
        ),
        (
            "quota-exhausted",
            BrokerOperationOutcomeV1::Refused,
            BrokerFailureReasonV1::QuotaExceeded,
            false,
            0,
            0,
        ),
    ];
    for (label, outcome, failure_reason, has_response, reserved_usage, charged_usage) in
        terminal_cases
    {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let ids = authority_with_pipeline(
            &cas,
            label,
            br#"version = 4
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
[[checks]]
name = "gate"
program = "/bin/true"
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = []
outputs = ["result"]
runner = { program = "/bin/true" }
execution = { credential_mode = "brokered", operations = [{ name = "model_inference", destination = "provider.test", method = "responses.create", max_request_bytes = 32, max_response_bytes = 32, max_calls = 3, max_usage = 100 }] }
"#,
        );
        let round = opened_round(&mut store, &cas, "run", &ids);
        let attempt = "a".repeat(26);
        let handle = "b".repeat(26);
        let operation = BrokerOperationPolicyV1 {
            name: "model_inference".into(),
            destination: "provider.test".into(),
            method: "responses.create".into(),
            max_request_bytes: 32,
            max_response_bytes: 32,
            max_calls: 3,
            max_usage: 100,
        };
        store
            .append(
                "run",
                &cas,
                NewEvent::new(
                    EventType::AttemptDispatchedV1,
                    serde_json::to_value(AttemptDispatchedPayloadV1 {
                        reserved: None,
                        prior_findings: None,
                    })
                    .unwrap(),
                )
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&round.event_id),
            )
            .unwrap();
        if label == "credential-exposure" {
            let result = cas.put(b"result").unwrap();
            let provenance = cas.put(b"provenance").unwrap();
            let error = store
                .append(
                    "run",
                    &cas,
                    NewEvent::new(
                        EventType::AttemptAdmittedV1,
                        serde_json::to_value(AttemptAdmittedPayloadV1 {
                            selection: "selected".into(),
                            cost_tokens: 0,
                            result_artifact: Some(result.clone()),
                            provenance_artifact: Some(provenance.clone()),
                        })
                        .unwrap(),
                    )
                    .node("reviewer")
                    .attempt(&attempt)
                    .caused_by(&round.event_id)
                    .referencing(vec![result, provenance]),
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("no durable Execution Binding"),
                "{error}"
            );
        }
        store
            .append(
                "run",
                &cas,
                NewEvent::new(
                    EventType::ReviewerExecutionBoundV1,
                    serde_json::to_value(ReviewerExecutionBindingV1 {
                        node: "reviewer".into(),
                        attempt_id: attempt.clone(),
                        lease_epoch: 1,
                        credential_mode: BrokerCredentialModeV1::Brokered,
                        auto_apply: false,
                        broker_handle: Some(handle.clone()),
                        operations: vec![operation],
                        admitted: true,
                    })
                    .unwrap(),
                )
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&round.event_id),
            )
            .unwrap();
        let receipt = |ordinal: u32,
                       outcome: BrokerOperationOutcomeV1,
                       failure_reason: Option<BrokerFailureReasonV1>,
                       has_response: bool,
                       reserved_usage: u64,
                       charged_usage: u64| {
            BrokerOperationReceiptV1 {
                handle_id: handle.clone(),
                node: "reviewer".into(),
                attempt_id: attempt.clone(),
                lease_epoch: 1,
                operation: "model_inference".into(),
                destination: "provider.test".into(),
                method: "responses.create".into(),
                ordinal,
                outcome,
                failure_reason,
                request_digest: format!("sha256:{}", "c".repeat(64)),
                response_digest: has_response.then(|| format!("sha256:{}", "d".repeat(64))),
                request_bytes: 7,
                response_bytes: if has_response { 8 } else { 0 },
                reserved_usage,
                charged_usage,
            }
        };
        let append = |store: &mut EventStore, receipt: BrokerOperationReceiptV1| {
            store.append(
                "run",
                &cas,
                NewEvent::new(
                    EventType::BrokerOperationCompletedV1,
                    serde_json::to_value(receipt).unwrap(),
                )
                .node("reviewer")
                .attempt(&attempt)
                .caused_by(&round.event_id),
            )
        };
        if label == "credential-exposure" {
            let error = append(
                &mut store,
                receipt(
                    1,
                    BrokerOperationOutcomeV1::Failed,
                    Some(BrokerFailureReasonV1::CredentialExposure),
                    false,
                    10,
                    11,
                ),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("contradicts its outcome"),
                "{error}"
            );
        }
        append(
            &mut store,
            receipt(
                1,
                outcome,
                Some(failure_reason),
                has_response,
                reserved_usage,
                charged_usage,
            ),
        )
        .unwrap();

        let error = append(
            &mut store,
            receipt(2, BrokerOperationOutcomeV1::Succeeded, None, true, 10, 7),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("terminal handle"),
            "{label}: {error}"
        );
        append(
            &mut store,
            receipt(
                2,
                BrokerOperationOutcomeV1::Revoked,
                Some(BrokerFailureReasonV1::AuthorityRevoked),
                false,
                10,
                0,
            ),
        )
        .expect("terminal broker state accepts only a revoked acknowledgement");
    }
}
