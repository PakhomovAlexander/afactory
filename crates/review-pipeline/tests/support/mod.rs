use review_config::Definition;
use review_core::{
    AuthorityFileV1, CampaignConvergenceV1, CampaignManifestV1, CampaignOpenedPayloadV1, EventType,
    RoundStartedPayloadV1, SubjectKind, SubjectV1,
};
use review_pipeline::{Kernel, RoundAuthority};
use review_source_git::{Manifest, Snapshot};
use review_store::{Cas, EventStore, NewEvent};

const TEST_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
runner = { program = "/bin/true" }
"#;

#[allow(dead_code)]
pub fn test_round_authority(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
) -> RoundAuthority {
    test_round_authority_with_prior(cas, store, run_id, snapshot, None, TEST_PIPELINE)
}

#[allow(dead_code)]
pub fn test_round_authority_for_pipeline(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    pipeline: &str,
) -> RoundAuthority {
    test_round_authority_with_prior(cas, store, run_id, snapshot, None, pipeline)
}

fn test_round_authority_with_prior(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    prior_finding_set_id: Option<String>,
    pipeline: &str,
) -> RoundAuthority {
    let authority_snapshot_id = cas.put(b"test authority snapshot").unwrap();
    let pipeline_id = cas.put(pipeline.as_bytes()).unwrap();
    let lock_id = cas.put(b"test reviewer lock").unwrap();
    let finding_genesis_id = cas.put(b"test finding genesis").unwrap();
    let demand_genesis_id = cas.put(b"test demand genesis").unwrap();
    let campaign_manifest_id = cas
        .put_json(
            &serde_json::to_value(CampaignManifestV1 {
                authority_snapshot_id: authority_snapshot_id.clone(),
                subject_kind: SubjectKind::WholeTree,
                base_snapshot_id: None,
                pipeline: AuthorityFileV1 {
                    path: "test.toml".into(),
                    artifact_id: pipeline_id.clone(),
                },
                reviewer_lock: AuthorityFileV1 {
                    path: "test.lock".into(),
                    artifact_id: lock_id,
                },
                reviewers: vec![],
                execution_policy_ids: vec![pipeline_id],
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
                finding_genesis_id,
                demand_genesis_id,
            })
            .unwrap(),
        )
        .unwrap();
    let head_manifest_id = cas
        .put_json(&serde_json::to_value(snapshot).unwrap())
        .unwrap();
    let head_snapshot_id = cas
        .put_json(
            &Snapshot {
                manifest: snapshot.clone(),
                content_digest: snapshot.content_digest(),
                repository_id: "test/repository".into(),
                source_revision: Some("test".into()),
                dirty: false,
                attempts: 1,
            }
            .to_payload("test-tree", Some(&head_manifest_id)),
        )
        .unwrap();
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
    whole_tree_kernel_for_pipeline(
        cas,
        store,
        run_id,
        snapshot,
        prior_finding_set_id,
        TEST_PIPELINE,
    )
}

pub fn whole_tree_kernel_for_pipeline<'a>(
    cas: &'a Cas,
    store: &'a mut EventStore,
    run_id: impl Into<String>,
    snapshot: Manifest,
    prior_finding_set_id: Option<String>,
    pipeline: &str,
) -> Kernel<'a> {
    let loaded = Definition::from_toml(pipeline).unwrap().load().unwrap();

    let run_id = run_id.into();
    let authority = test_round_authority_with_prior(
        cas,
        store,
        &run_id,
        &snapshot,
        prior_finding_set_id,
        pipeline,
    );
    Kernel::from_loaded(cas, store, run_id, snapshot, &loaded, authority).unwrap()
}
