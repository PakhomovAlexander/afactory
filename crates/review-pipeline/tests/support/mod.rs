use review_core::{
    AuthorityFileV1, CampaignConvergenceV1, CampaignManifestV1, CampaignOpenedPayloadV1, EventType,
    RoundStartedPayloadV1, SubjectKind, SubjectV1,
};
use review_pipeline::RoundAuthority;
use review_source_git::Manifest;
use review_store::{Cas, EventStore, NewEvent};

#[allow(dead_code)]
pub fn test_round_authority_for_pipeline(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    pipeline: &str,
) -> RoundAuthority {
    test_round_authority(
        cas,
        store,
        run_id,
        snapshot,
        pipeline,
        review_core::LEGACY_FINDING_IDENTITY_POLICY,
    )
}

#[allow(dead_code)]
pub fn test_canonical_round_authority_for_pipeline(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    pipeline: &str,
) -> RoundAuthority {
    test_round_authority(
        cas,
        store,
        run_id,
        snapshot,
        pipeline,
        review_core::CANONICAL_FINDING_IDENTITY_POLICY,
    )
}

/// Open a whole-tree Campaign over `snapshot` and its first Round, as the authority layer does.
fn test_round_authority(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    pipeline: &str,
    identity_policy: &str,
) -> RoundAuthority {
    let authority_manifest = Manifest::new(vec![]).unwrap();
    let authority_manifest_id = cas
        .put_json(&serde_json::to_value(&authority_manifest).unwrap())
        .unwrap();
    let authority_snapshot_id = cas
        .put_json(&serde_json::json!({
            "repository_id": "test/repository",
            "vcs": "git",
            "capture": { "kind": "committed", "tree_id": "test-base-tree" },
            "content_digest": authority_manifest.content_digest(),
            "source_revision": "test-base",
            "artifact_manifest": authority_manifest_id,
        }))
        .unwrap();
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
                check_timeout_seconds: 3600,
                git_timeout_seconds: 300,
                budgets: None,
                focus: None,
                finding_identity_policy: identity_policy.into(),
                finding_genesis_id,
                demand_genesis_id: demand_genesis_id.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    let head_manifest_id = cas
        .put_json(&serde_json::to_value(snapshot).unwrap())
        .unwrap();
    let head_snapshot_id = cas
        .put_json(&serde_json::json!({
            "repository_id": "test/repository",
            "vcs": "git",
            "capture": {
                "kind": "committed",
                "tree_id": "test-tree",
            },
            "content_digest": snapshot.content_digest(),
            "source_revision": "test",
            "artifact_manifest": head_manifest_id,
        }))
        .unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head_snapshot_id)).unwrap())
        .unwrap();
    let prior_finding_set_id = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": 1,
            "prior_findings": [],
        }))
        .unwrap();
    let prior_demand_set_id = demand_genesis_id;
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
