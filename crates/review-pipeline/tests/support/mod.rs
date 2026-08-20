use review_config::Definition;
use review_core::{CampaignOpenedPayloadV1, EventType, RoundStartedPayloadV1, SubjectV1};
use review_pipeline::{Kernel, RoundAuthority};
use review_source_git::Manifest;
use review_store::{Cas, EventStore, NewEvent};

#[allow(dead_code)]
pub fn test_round_authority(cas: &Cas, store: &mut EventStore, run_id: &str) -> RoundAuthority {
    test_round_authority_with_prior(cas, store, run_id, None)
}

fn test_round_authority_with_prior(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    prior_finding_set_id: Option<String>,
) -> RoundAuthority {
    let authority_snapshot_id = cas.put(b"test authority snapshot").unwrap();
    let campaign_manifest_id = cas.put(b"test campaign manifest").unwrap();
    let head_snapshot_id = cas.put(b"test head snapshot").unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head_snapshot_id)).unwrap())
        .unwrap();
    let prior_finding_set_id = prior_finding_set_id.unwrap_or_else(|| {
        cas.put_json(&serde_json::json!({
            "round": 1,
            "prior_findings": [],
        }))
        .unwrap()
    });
    let prior_demand_set_id = cas.put(b"test prior demand set").unwrap();
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(CampaignOpenedPayloadV1 {
                    campaign_manifest_id: campaign_manifest_id.clone(),
                    authority_snapshot_id: authority_snapshot_id.clone(),
                })
                .unwrap(),
            )
            .referencing(vec![
                authority_snapshot_id.clone(),
                campaign_manifest_id.clone(),
            ]),
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
                    campaign_manifest_id: campaign_manifest_id.clone(),
                    subject_id: subject_id.clone(),
                    prior_finding_set_id: prior_finding_set_id.clone(),
                    prior_demand_set_id: prior_demand_set_id.clone(),
                })
                .unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(vec![
                authority_snapshot_id,
                campaign_manifest_id,
                head_snapshot_id,
                subject_id,
                prior_finding_set_id,
                prior_demand_set_id,
            ]),
        )
        .unwrap();
    RoundAuthority::load(store, cas, run_id, &round.event_id).unwrap()
}

/// Test composition follows the same validated Subject path as production.
#[allow(dead_code)]
pub fn whole_tree_kernel<'a>(
    cas: &'a Cas,
    store: &'a mut EventStore,
    run_id: impl Into<String>,
    snapshot: Manifest,
) -> Kernel<'a> {
    whole_tree_kernel_with_prior(cas, store, run_id, snapshot, None)
}

pub fn whole_tree_kernel_with_prior<'a>(
    cas: &'a Cas,
    store: &'a mut EventStore,
    run_id: impl Into<String>,
    snapshot: Manifest,
    prior_finding_set_id: Option<String>,
) -> Kernel<'a> {
    let loaded = Definition::from_toml(
        r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
runner = { program = "/bin/true" }
"#,
    )
    .unwrap()
    .load()
    .unwrap();

    let run_id = run_id.into();
    let authority = test_round_authority_with_prior(cas, store, &run_id, prior_finding_set_id);
    Kernel::from_loaded(cas, store, run_id, snapshot, &loaded, authority).unwrap()
}
