use review_core::Producer;
use review_source_git::task::{capture_snapshot, derive_source_tree, read_snapshot, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::Cas;
use serde_json::json;

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "source-read-test".into(),
        node_id: Some("root.nodes.seal".into()),
        operation_id: "task-builtin@1".into(),
    }
}

#[test]
fn derived_source_keeps_exact_identity_and_revalidates_both_trees_on_every_call() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let origin = cas.put_json(&json!({"origin":"trusted capture"})).unwrap();
    let original = cas.put(b"original source\n").unwrap();
    let changed = cas.put(b"changed source\n").unwrap();
    let entry = |content: String, size| Entry {
        path: "source.txt".into(),
        kind: EntryKind::File,
        content,
        size,
    };
    let base = Manifest::new(vec![entry(original.clone(), 16)]).unwrap();
    let candidate = Manifest::new(vec![entry(changed.clone(), 15)]).unwrap();
    let parent = capture_snapshot(&cas, &base, &origin, None).unwrap();
    let candidate_manifest = cas
        .put_json(&serde_json::to_value(&candidate).unwrap())
        .unwrap();
    let expected_snapshot = capture_snapshot(&cas, &candidate, &origin, Some(&parent)).unwrap();
    let refs = vec![origin.clone(), candidate_manifest.clone()];
    let expected = source_tree(&cas, producer(), &expected_snapshot, refs.clone()).unwrap();
    let derive =
        || derive_source_tree(&cas, producer(), &candidate_manifest, &parent, refs.clone());
    assert_eq!(derive().unwrap(), expected);
    let (base_snapshot, _) = read_snapshot(&cas, &parent).unwrap();
    for id in [
        original,
        changed,
        origin,
        parent.clone(),
        base_snapshot.manifest_id,
        candidate_manifest.clone(),
    ] {
        let hex = id.strip_prefix("sha256:").unwrap();
        let path = dir.path().join("objects").join(&hex[..2]).join(&hex[2..]);
        let saved = std::fs::read(&path).unwrap();
        for missing in [false, true] {
            if missing {
                std::fs::remove_file(&path).unwrap();
            } else {
                std::fs::write(&path, b"corrupt source").unwrap();
            }
            assert!(
                derive().is_err(),
                "accepted stale source object {id}, missing={missing}"
            );
            std::fs::write(&path, &saved).unwrap();
            assert_eq!(derive().unwrap(), expected);
        }
    }
    let mut wrong_size = candidate.clone();
    wrong_size.entries[0].size += 1;
    let wrong_size = cas
        .put_json(&serde_json::to_value(wrong_size).unwrap())
        .unwrap();
    assert!(derive_source_tree(&cas, producer(), &wrong_size, &parent, refs).is_err());
}
