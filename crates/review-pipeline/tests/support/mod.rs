use review_config::Definition;
use review_core::{
    AuthorityFileV1, CampaignConvergenceV1, CampaignManifestV1, CampaignOpenedPayloadV1, EventType,
    ChangeSetV1, RoundStartedPayloadV1, SubjectKind, SubjectV1,
};
use review_pipeline::{Kernel, RoundAuthority};
use review_source_git::Manifest;
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

#[allow(dead_code)]
pub fn test_diff_round_authority(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    pipeline: &str,
) -> RoundAuthority {
    test_round_authority_with_subject(
        cas,
        store,
        run_id,
        snapshot,
        None,
        pipeline,
        SubjectKind::Diff,
    )
}

fn test_round_authority_with_prior(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    prior_finding_set_id: Option<String>,
    pipeline: &str,
) -> RoundAuthority {
    test_round_authority_with_subject(
        cas,
        store,
        run_id,
        snapshot,
        prior_finding_set_id,
        pipeline,
        SubjectKind::WholeTree,
    )
}

fn test_round_authority_with_subject(
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    snapshot: &Manifest,
    prior_finding_set_id: Option<String>,
    pipeline: &str,
    subject_kind: SubjectKind,
) -> RoundAuthority {
    let authority_manifest = Manifest::new(vec![]);
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
                subject_kind,
                base_snapshot_id: (subject_kind == SubjectKind::Diff)
                    .then(|| authority_snapshot_id.clone()),
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
    let subject = if subject_kind == SubjectKind::Diff {
        let change_set = ChangeSetV1::new(
            &authority_snapshot_id,
            &head_snapshot_id,
            snapshot.entries.iter().map(|entry| entry.path.clone()).collect(),
            vec![],
            b"",
            "git version test",
            "review.kernel/git-tree-diff@test",
        )
        .unwrap();
        let change_set_id = cas
            .put_json(&serde_json::to_value(change_set).unwrap())
            .unwrap();
        SubjectV1::diff(&head_snapshot_id, &authority_snapshot_id, change_set_id)
    } else {
        SubjectV1::whole_tree(&head_snapshot_id)
    };
    let subject_id = cas
        .put_json(&serde_json::to_value(&subject).unwrap())
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
    let mut round_refs = vec![
        authority_snapshot_id,
        campaign_manifest_id,
        head_snapshot_id,
        subject_id,
        prior_finding_set_id,
        prior_demand_set_id,
    ];
    round_refs.extend(subject.base_snapshot_id);
    round_refs.extend(subject.change_set_id);
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
            .referencing(round_refs),
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
