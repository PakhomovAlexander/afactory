//! Captured Review test authority shared by runtime and public inspection regressions.
use review_core::{CampaignManifestV1, EventType, SubjectV1};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore, NewEvent};
use serde_json::json;
use std::collections::BTreeMap;

pub fn open_round_authority(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    files: Option<BTreeMap<String, Vec<u8>>>,
) -> String {
    open_round_authority_with_convergence(
        cas,
        store,
        definition,
        files,
        review_core::CampaignConvergenceV1 {
            clean_rounds: 1,
            max_rounds: 1,
            gate: "major".into(),
        },
    )
}

pub fn open_round_authority_with_convergence(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    files: Option<BTreeMap<String, Vec<u8>>>,
    convergence: review_core::CampaignConvergenceV1,
) -> String {
    open_round_inner(
        cas,
        store,
        definition,
        files,
        convergence,
        BTreeMap::new(),
        false,
    )
}

/// Extra ordinary source files and an exact heavy convergence policy for integration tests.
#[allow(dead_code)]
pub fn open_round_authority_with_source(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    files: Option<BTreeMap<String, Vec<u8>>>,
    convergence: review_core::CampaignConvergenceV1,
    source: BTreeMap<String, Vec<u8>>,
) -> String {
    open_round_inner(cas, store, definition, files, convergence, source, true)
}

fn open_round_inner(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    files: Option<BTreeMap<String, Vec<u8>>>,
    convergence: review_core::CampaignConvergenceV1,
    source: BTreeMap<String, Vec<u8>>,
    exact_convergence: bool,
) -> String {
    let (attempt_tokens, run_tokens) = if files.is_some() {
        (20_000, 50_000)
    } else {
        (19, 50)
    };
    let pipeline = format!(
        "{definition}\n[budgets]\nunit = \"tokens\"\nattempt = {attempt_tokens}\nrun = {run_tokens}\n[convergence]\nclean_rounds = 2\nmax_rounds = 3\ngate = \"major\"\n"
    );
    let pipeline = if exact_convergence {
        pipeline.replace(
            "clean_rounds = 2\nmax_rounds = 3\ngate = \"major\"",
            &format!(
                "clean_rounds = {}\nmax_rounds = {}\ngate = \"{}\"",
                convergence.clean_rounds, convergence.max_rounds, convergence.gate
            ),
        )
    } else {
        pipeline
    };
    let pipeline_id = cas.put(pipeline.as_bytes()).unwrap();
    let mut lock = review_config::lock::Lockfile::empty();
    let mut entries = source
        .into_iter()
        .map(|(path, bytes)| Entry {
            path,
            kind: EntryKind::File,
            content: cas.put(&bytes).unwrap(),
            size: bytes.len() as u64,
        })
        .collect::<Vec<_>>();
    let mut reviewers = Vec::new();
    let mut execution_policy_ids = vec![pipeline_id.clone()];
    if let Some(files) = files {
        let digest = review_config::lock::package_digest_from_files(&files);
        let mut package_files = BTreeMap::new();
        for (path, bytes) in files {
            let id = cas.put(&bytes).unwrap();
            entries.push(Entry {
                path: format!(".af/workers/fixture/{path}"),
                kind: EntryKind::File,
                content: id.clone(),
                size: bytes.len() as u64,
            });
            package_files.insert(path, id);
        }
        let package = review_core::ReviewerPackageV1 {
            name: "fixture".into(),
            version: "1.0.0".into(),
            digest: digest.clone(),
            files: package_files,
        };
        let id = cas
            .put_json(&serde_json::to_value(&package).unwrap())
            .unwrap();
        reviewers.push(review_core::CampaignReviewerV1 {
            node: "reviewer".into(),
            name: package.name,
            version: package.version,
            digest: digest.clone(),
            package_artifact_id: id.clone(),
        });
        lock.workers.insert(
            "fixture".into(),
            review_config::lock::Pin {
                version: "1.0.0".into(),
                digest,
            },
        );
        execution_policy_ids.push(id);
    }
    let lock = lock.to_toml();
    let lock_id = cas.put(lock.as_bytes()).unwrap();
    entries.extend([
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
    ]);
    let tree = Manifest::new(entries).unwrap();
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
        "reviewers": reviewers, "execution_policy_ids": execution_policy_ids, "project_policy_ids": [],
        "convergence": convergence,
        "reviewer_timeout_seconds": 7, "check_timeout_seconds": 3600,
        "budgets": {"attempt_tokens": attempt_tokens, "run_tokens": run_tokens},
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
