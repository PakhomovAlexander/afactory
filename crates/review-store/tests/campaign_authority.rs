use review_core::{
    CampaignOpenedPayloadV1, EventType, RoundInputSupersededPayloadV1, RoundStartedPayloadV1,
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
    Authority {
        authority: cas.put(format!("{label} authority").as_bytes()).unwrap(),
        manifest: cas.put(format!("{label} manifest").as_bytes()).unwrap(),
        subject: cas.put(format!("{label} subject").as_bytes()).unwrap(),
        head: cas.put(format!("{label} head").as_bytes()).unwrap(),
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
