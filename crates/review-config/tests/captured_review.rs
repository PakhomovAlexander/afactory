use review_config::captured_review::{ReviewMode, load_captured_review};
use review_core::{CampaignManifestV1, SubjectKind};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::Cas;
use serde_json::json;

const PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[convergence]
clean_rounds = 2
max_rounds = 3
gate = "major"
[[nodes]]
id = "reviewer"
kind = "reviewer"
runner = { program = "/bin/true" }
"#;

fn fixture(cas: &Cas, root: &str) -> CampaignManifestV1 {
    let pipeline = cas.put(PIPELINE.as_bytes()).unwrap();
    let lock = cas.put(b"version = 1\n").unwrap();
    let lock_path = if root == ".af" {
        ".af/af.lock"
    } else {
        ".review/review.lock"
    };
    let tree = Manifest::new(vec![
        Entry {
            path: format!("{root}/pipelines/review.toml"),
            kind: EntryKind::File,
            content: pipeline.clone(),
            size: PIPELINE.len() as u64,
        },
        Entry {
            path: lock_path.into(),
            kind: EntryKind::File,
            content: lock.clone(),
            size: 12,
        },
    ])
    .unwrap();
    let tree_id = cas.put_json(&serde_json::to_value(&tree).unwrap()).unwrap();
    let snapshot = cas
        .put_json(&json!({
            "repository_id": "fixture/review", "vcs": "git",
            "capture": {"kind": "committed", "tree_id": "fixture-tree"},
            "content_digest": tree.content_digest(), "source_revision": "fixture",
            "artifact_manifest": tree_id,
        }))
        .unwrap();
    let genesis = |kind| {
        cas.put_json(&json!({"kind": kind, "authority_snapshot_id": snapshot}))
            .unwrap()
    };
    serde_json::from_value(json!({
        "authority_snapshot_id": snapshot, "subject_kind": "whole-tree",
        "pipeline": {"path": format!("{root}/pipelines/review.toml"), "artifact_id": pipeline},
        "reviewer_lock": {"path": lock_path, "artifact_id": lock},
        "reviewers": [], "execution_policy_ids": [pipeline], "project_policy_ids": [],
        "convergence": {"clean_rounds": 1, "max_rounds": 1, "gate": "major"},
        "reviewer_timeout_seconds": 60, "check_timeout_seconds": 3600,
        "git_timeout_seconds": 300, "budgets": null, "focus": null,
        "finding_identity_policy": review_core::CANONICAL_FINDING_IDENTITY_POLICY,
        "finding_genesis_id": genesis("finding-set-genesis@1"),
        "demand_genesis_id": genesis("demand-set-genesis@1"),
    }))
    .unwrap()
}

#[test]
fn captured_review_reopens_both_recorded_layouts_and_preserves_selected_mode() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    for root in [".af", ".review"] {
        let mut manifest = fixture(&cas, root);
        let loaded = load_captured_review(&cas, &manifest, ReviewMode::Light).unwrap();
        assert_eq!(loaded.subject_kind(), SubjectKind::WholeTree);
        assert_eq!(loaded.convergence().max_rounds, 3);
        assert!(
            load_captured_review(&cas, &manifest, ReviewMode::Heavy)
                .err()
                .unwrap()
                .contains("requested heavy mode")
        );
        manifest.convergence.clean_rounds = 2;
        manifest.convergence.max_rounds = 3;
        assert!(load_captured_review(&cas, &manifest, ReviewMode::Heavy).is_ok());
        assert!(load_captured_review(&cas, &manifest, ReviewMode::Light).is_err());
        drop(loaded);
        let reopened = Cas::open(temp.path()).unwrap();
        assert!(load_captured_review(&reopened, &manifest, ReviewMode::Heavy).is_ok());
    }
}

#[test]
fn captured_review_refuses_valid_but_unreachable_bytes_and_changed_authority() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let original = fixture(&cas, ".af");
    let mut changed = original.clone();
    changed.pipeline.artifact_id = cas
        .put(format!("{PIPELINE}\n# different captured bytes\n").as_bytes())
        .unwrap();
    changed.execution_policy_ids = vec![changed.pipeline.artifact_id.clone()];
    assert!(
        load_captured_review(&cas, &changed, ReviewMode::Light)
            .err()
            .unwrap()
            .contains("not reachable")
    );
    changed = original.clone();
    changed.check_timeout_seconds = Some(19);
    assert!(
        load_captured_review(&cas, &changed, ReviewMode::Light)
            .err()
            .unwrap()
            .contains("check timeout")
    );
    changed = original.clone();
    changed.execution_policy_ids.clear();
    assert!(load_captured_review(&cas, &changed, ReviewMode::Light).is_err());
    changed = original.clone();
    changed.finding_genesis_id = cas.put_json(&json!({"kind": "demand-set-genesis@1", "authority_snapshot_id": changed.authority_snapshot_id})).unwrap();
    assert!(
        load_captured_review(&cas, &changed, ReviewMode::Light)
            .err()
            .unwrap()
            .contains("invalid `finding-set-genesis@1`")
    );
    assert!(load_captured_review(&cas, &original, ReviewMode::Light).is_ok());
}
