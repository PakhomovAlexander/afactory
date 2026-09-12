use super::*;
use review_config::captured_review::ReviewMode;
use review_config::task::legacy_review::resources::ReviewResourcePolicy;
use review_core::{CampaignManifestV1, EventType, SubjectV1};
use review_source_git::{Entry, EntryKind};
use review_store::NewEvent;
use serde_json::json;

pub(super) fn open_round(cas: &Cas, store: &mut EventStore) -> String {
    open_round_with_pipeline(cas, store, PIPELINE)
}

pub(super) fn open_round_with_pipeline(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
) -> String {
    let pipeline = format!(
        "{definition}\n[budgets]\nunit = \"tokens\"\nattempt = 19\nrun = 50\n[convergence]\nclean_rounds = 2\nmax_rounds = 3\ngate = \"major\"\n"
    );
    let pipeline_id = cas.put(pipeline.as_bytes()).unwrap();
    let lock = b"version = 1\n";
    let lock_id = cas.put(lock).unwrap();
    let tree = Manifest::new(vec![
        Entry {
            path: ".af/pipelines/review.toml".into(),
            kind: EntryKind::File,
            content: pipeline_id.clone(),
            size: pipeline.len() as u64,
        },
        Entry {
            path: ".af/af.lock".into(),
            kind: EntryKind::File,
            content: lock_id.clone(),
            size: lock.len() as u64,
        },
    ])
    .unwrap();
    let tree_id = cas.put_json(&serde_json::to_value(&tree).unwrap()).unwrap();
    let head = cas
        .put_json(&json!({
            "repository_id": "fixture/captured-review", "vcs": "git",
            "capture": {"kind": "committed", "tree_id": "fixture-tree"},
            "source_revision": "fixture", "content_digest": tree.content_digest(),
            "artifact_manifest": tree_id,
        }))
        .unwrap();
    let finding_genesis = cas
        .put_json(&json!({
            "kind": "finding-set-genesis@1", "authority_snapshot_id": head,
        }))
        .unwrap();
    let demand_genesis = cas
        .put_json(&json!({
            "kind": "demand-set-genesis@1", "authority_snapshot_id": head,
        }))
        .unwrap();
    let manifest: CampaignManifestV1 = serde_json::from_value(json!({
        "authority_snapshot_id": head, "subject_kind": "whole-tree",
        "pipeline": {"path": ".af/pipelines/review.toml", "artifact_id": pipeline_id},
        "reviewer_lock": {"path": ".af/af.lock", "artifact_id": lock_id},
        "reviewers": [], "execution_policy_ids": [pipeline_id], "project_policy_ids": [],
        "convergence": {"clean_rounds": 1, "max_rounds": 1, "gate": "major"},
        "reviewer_timeout_seconds": 7, "check_timeout_seconds": 3600,
        "budgets": {"attempt_tokens": 19, "run_tokens": 50},
        "finding_identity_policy": review_core::CANONICAL_FINDING_IDENTITY_POLICY,
        "finding_genesis_id": finding_genesis, "demand_genesis_id": demand_genesis,
    }))
    .unwrap();
    let manifest_id = cas
        .put_json(&serde_json::to_value(manifest).unwrap())
        .unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let prior = cas
        .put_json(&json!({"subject_id": subject_id, "round": 1, "prior_findings": []}))
        .unwrap();
    let opened = store
        .append(
            "review",
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                json!({"campaign_manifest_id": manifest_id, "authority_snapshot_id": head}),
            )
            .referencing(vec![head.clone(), manifest_id.clone()]),
        )
        .unwrap();
    store
        .append(
            "review",
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                json!({
                    "round": 1, "epoch": 1, "campaign_manifest_id": manifest_id,
                    "subject_id": subject_id, "prior_finding_set_id": prior,
                    "prior_demand_set_id": demand_genesis,
                }),
            )
            .caused_by(opened.event_id)
            .referencing(vec![head, manifest_id, subject_id, prior, demand_genesis]),
        )
        .unwrap()
        .event_id
}

pub(super) fn limits() -> TaskLimitsV1 {
    TaskLimitsV1 {
        tokens: 100,
        max_attempts: 4,
        deadline_unix_ms: 9999999999999,
        verification: VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        },
    }
}

pub(super) fn outputs() -> BTreeMap<String, Address> {
    BTreeMap::from([(
        "findings".into(),
        Address {
            node: "ledger".into(),
            port: "findings".into(),
        },
    )])
}

#[test]
fn real_captured_round_compiles_bound_resources_and_reopens_without_execution() {
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let store_path = directory.path().join("events.sqlite");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(&store_path).unwrap();
    let round_event = open_round(&cas, &mut store);
    let round = CapturedLegacyReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    let resources = ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    };
    let compiled = round
        .compile(&cas, ReviewMode::Light, &resources, limits(), outputs())
        .unwrap();
    let graph = &compiled.compilation.graph;
    assert_eq!(graph.allowances.len(), 1);
    assert_eq!(graph.slots.len(), 1);
    assert_eq!(graph.max_parallel, 4);
    let worker = &compiled.compilation.nodes["reviewer"].task_node;
    assert_eq!(graph.allowances[worker].tokens_per_attempt, 19);
    assert_eq!(graph.allowances[worker].wall_ms_per_attempt, 7000);
    assert_eq!(graph.allowances[worker].max_attempts, 2);
    assert_eq!(graph.token_scopes["review.round1"].tokens, 50);
    assert_eq!(graph.budget(limits()).unwrap().committed_tokens(), 0);
    assert_eq!(graph.inputs, round.capture_inputs(&cas).unwrap());
    assert_eq!(
        store.len("review").unwrap(),
        2,
        "compilation cannot allocate Attempts or dispatch"
    );
    let expected = compiled.compilation;
    drop(round);
    drop(store);
    drop(cas);
    let cas = Cas::open_existing(&cas_root).unwrap();
    let store = EventStore::open(&store_path).unwrap();
    let round = CapturedLegacyReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    assert_eq!(
        expected,
        round
            .compile(&cas, ReviewMode::Light, &resources, limits(), outputs())
            .unwrap()
            .compilation
    );
    let mut too_small = limits();
    too_small.tokens = 18;
    assert!(
        round
            .compile(&cas, ReviewMode::Light, &resources, too_small, outputs())
            .is_err()
    );
    assert!(
        round
            .compile(&cas, ReviewMode::Heavy, &resources, limits(), outputs())
            .is_err()
    );
    let manifest: CampaignManifestV1 = serde_json::from_value(
        cas.get_json(round.authority().campaign_manifest_id())
            .unwrap(),
    )
    .unwrap();
    let hex = manifest
        .pipeline
        .artifact_id
        .strip_prefix("sha256:")
        .unwrap();
    std::fs::write(
        cas_root.join("objects").join(&hex[..2]).join(&hex[2..]),
        b"version = 2\n",
    )
    .unwrap();
    assert!(
        round
            .compile(&cas, ReviewMode::Light, &resources, limits(), outputs())
            .is_err()
    );
    assert_eq!(store.len("review").unwrap(), 2);
}

#[test]
fn recorded_input_recompilation_is_read_only_and_refuses_missing_or_forged_wrappers() {
    use review_pipeline::task::legacy_review::ReviewCompilationRequest;
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let round_event = open_round(&cas, &mut store);
    let round = CapturedLegacyReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    let resources = ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    };
    let captured = round
        .compile(&cas, ReviewMode::Light, &resources, limits(), outputs())
        .unwrap();
    let inputs = captured.compilation.graph.inputs.clone();
    let compile = |inputs| {
        round.compile_existing(
            &cas,
            ReviewMode::Light,
            &resources,
            ReviewCompilationRequest {
                limits: limits(),
                inputs,
                outputs: outputs(),
            },
        )
    };
    assert_eq!(
        compile(inputs.clone()).unwrap().compilation,
        captured.compilation
    );
    let mut changed = inputs.clone();
    changed.insert("extra".into(), inputs["head"].clone());
    assert!(compile(changed).is_err());
    let original = cas.get_artifact(&inputs["round"].artifact_ids[0]).unwrap();
    for forged_payload in [false, true] {
        let mut wrapper = original.clone();
        if forged_payload {
            wrapper.payload["epoch"] = 2.into();
        } else {
            wrapper.producer = review_core::Producer::KernelOperation {
                run_id: "another-campaign".into(),
                node_id: None,
                operation_id: "capture@1".into(),
            };
        }
        let id = cas
            .put_artifact(
                wrapper.artifact_type,
                wrapper.producer,
                wrapper.input_artifacts,
                wrapper.subject_snapshot_id,
                wrapper.payload,
            )
            .unwrap()
            .0;
        let mut changed = inputs.clone();
        changed.get_mut("round").unwrap().artifact_ids = vec![id];
        assert!(compile(changed).is_err());
    }
    for name in ["head", "round"] {
        let id = &inputs[name].artifact_ids[0];
        let bytes = cas.get(id).unwrap();
        let hex = id.strip_prefix("sha256:").unwrap();
        let path = cas_root.join("objects").join(&hex[..2]).join(&hex[2..]);
        std::fs::remove_file(&path).unwrap();
        assert!(compile(inputs.clone()).is_err());
        assert!(
            !path.exists(),
            "recompilation cannot heal missing recorded input bytes"
        );
        std::fs::write(path, bytes).unwrap();
    }
    assert_eq!(compile(inputs).unwrap().compilation, captured.compilation);
    assert_eq!(store.len("review").unwrap(), 2);
}

#[test]
fn historical_round_recompilation_does_not_grant_current_epoch_authority() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let first = open_round(&cas, &mut store);
    let round = CapturedLegacyReviewRound::load(&cas, &store, "review", &first).unwrap();
    let resources = ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    };
    let compiled = round
        .compile(&cas, ReviewMode::Light, &resources, limits(), outputs())
        .unwrap();
    let old = store.latest_round_started("review").unwrap().unwrap();
    let mut next_payload = old.payload.clone();
    next_payload["epoch"] = json!(2);
    let binding = round.binding();
    let next = store
        .append_batch(
            "review",
            &cas,
            &[
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(review_core::RoundInputSupersededPayloadV1 {
                        round: binding.round,
                        old_epoch: binding.epoch,
                        new_epoch: binding.epoch + 1,
                        campaign_manifest_id: binding.campaign_manifest_id,
                        old_subject_id: binding.subject_id.clone(),
                        replacement_subject_id: binding.subject_id,
                    })
                    .unwrap(),
                )
                .caused_by(first.clone()),
                NewEvent::new(EventType::RoundStartedV1, next_payload)
                    .caused_by(first.clone())
                    .referencing(old.artifact_refs),
            ],
        )
        .unwrap()
        .pop()
        .unwrap();
    let before = store.len("review").unwrap();
    assert!(CapturedLegacyReviewRound::load(&cas, &store, "review", &first).is_err());
    assert!(round.check_current(&cas, &store).is_err());
    let historical =
        CapturedLegacyReviewRound::load_recorded(&cas, &store, "review", &first).unwrap();
    assert_eq!(historical.binding(), round.binding());
    assert_eq!(
        historical
            .compile_existing(
                &cas,
                ReviewMode::Light,
                &resources,
                review_pipeline::task::legacy_review::ReviewCompilationRequest {
                    limits: limits(),
                    inputs: compiled.compilation.graph.inputs.clone(),
                    outputs: outputs(),
                }
            )
            .unwrap()
            .compilation,
        compiled.compilation
    );
    assert!(historical.check_current(&cas, &store).is_err());
    assert!(CapturedLegacyReviewRound::load(&cas, &store, "review", &next.event_id).is_ok());
    assert!(CapturedLegacyReviewRound::load_recorded(&cas, &store, "different", &first).is_err());
    let opened = store.campaign_opened("review").unwrap().unwrap();
    assert!(
        CapturedLegacyReviewRound::load_recorded(&cas, &store, "review", &opened.event_id).is_err()
    );
    assert_eq!(store.len("review").unwrap(), before);
}
