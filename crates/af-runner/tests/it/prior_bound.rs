use af_contracts::{ChangeSetV1, MAX_CHANGE_SET_BYTES, SubjectV1};

#[test]
fn resolved_change_sets_bind_verified_bytes_without_requiring_reserialization_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(directory.path()).unwrap();
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let change_set = ChangeSetV1::new(
        base.clone(),
        head.clone(),
        vec!["src/a.rs".into()],
        vec![],
        b"diff --git a/src/a.rs b/src/a.rs\n-\xff\n+\xfe\n",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let mut value = serde_json::to_value(change_set).unwrap();
    // This is schema-valid but the Rust serializer deliberately omits the false default. The
    // verified stored bytes, not a later re-serialization, remain artifact identity authority.
    value["rename_detection_truncated"] = serde_json::Value::Bool(false);
    let encoded = af_store::canonical::canonicalize(&value).unwrap();
    let artifact_id = cas.put(&encoded).unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(head, base, artifact_id.clone())).unwrap())
        .unwrap();
    let resolved = af_store::resolve_subject(&cas, &subject_id).unwrap();
    let resolved_change_set = resolved.change_set.unwrap();
    assert_eq!(resolved_change_set.artifact_id(), artifact_id);
    assert_eq!(resolved_change_set.encoded_bytes(), encoded.len());

    assert_eq!(resolved_change_set.change_set().changed_paths, ["src/a.rs"]);
    assert_eq!(
        resolved_change_set.change_set().canonical_patch().unwrap(),
        b"diff --git a/src/a.rs b/src/a.rs\n-\xff\n+\xfe\n"
    );
    assert_eq!(cas.get(&artifact_id).unwrap(), encoded);
    // The same verified Change Set travels as captured data on a native Task input. Its
    // base64 patch and metadata remain lossless; no Campaign prompt renderer interprets it.
    use af_contracts::task::{ArtifactInputV1, execution::TaskInvocationV1};
    use af_runner::task::{WorkerContract, WorkerRequest};
    use serde_json::json;
    use std::collections::BTreeMap;
    let frame = cas
        .put_artifact(
            af_contracts::contract::CHANGE_SET_V1,
            af_contracts::Producer::KernelOperation {
                run_id: "native-input".into(),
                node_id: None,
                operation_id: "capture@1".into(),
            },
            vec![artifact_id.clone()],
            None,
            value.clone(),
        )
        .unwrap()
        .0;
    let invocation = TaskInvocationV1 {
        plan_id: cas.put(b"plan").unwrap(),
        node: "root.worker".into(),
        inputs: BTreeMap::from([(
            "change".into(),
            ArtifactInputV1 {
                artifact_type: af_contracts::contract::CHANGE_SET_V1.into(),
                artifact_ids: vec![frame.clone()],
                cardinality: af_contracts::PortCardinality::One,
                snapshot_id: None,
            },
        )]),
    };
    let contract = WorkerContract::capture(
        &cas,
        json!({"type":"object"}),
        BTreeMap::from([("result".into(), json!({"type":"object"}))]),
    )
    .unwrap();
    let context_id = contract
        .prepare(&cas, &invocation, &[], "Inspect only declared data")
        .unwrap();
    let (context, rendered) = contract.read_context(&cas, &context_id).unwrap();
    let request: WorkerRequest = serde_json::from_slice(&rendered).unwrap();
    assert_eq!(request.inputs["change"][0].artifact_id, frame);
    assert_eq!(request.inputs["change"][0].payload, value);
    assert_eq!(context.manifest.rendered_bytes, rendered.len() as u64);
}

#[test]
fn oversized_change_sets_are_refused_while_resolving_subject_authority() {
    let directory = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(directory.path()).unwrap();
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let patch = vec![b'x'; MAX_CHANGE_SET_BYTES * 3 / 4 + 1024];
    let change_set = ChangeSetV1::new(
        base.clone(),
        head.clone(),
        vec!["src/a.rs".into()],
        vec![],
        &patch,
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let encoded = serde_json::to_vec(&change_set).unwrap();
    assert!(encoded.len() > MAX_CHANGE_SET_BYTES);
    let artifact_id = cas.put(&encoded).unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(head, base, artifact_id)).unwrap())
        .unwrap();
    let error = af_store::resolve_subject(&cas, &subject_id).unwrap_err();
    assert!(error.to_string().contains("limit is 4194304"), "{error}");
}

#[test]
fn unsorted_change_set_paths_refuse_at_current_subject_admission() {
    let directory = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(directory.path()).unwrap();
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let change = ChangeSetV1::new(
        base.clone(),
        head.clone(),
        vec!["src/a.rs".into(), "src/b.rs".into()],
        vec![],
        b"patch",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let mut value = serde_json::to_value(change).unwrap();
    value["changed_paths"] = serde_json::json!(["src/b.rs", "src/a.rs"]);
    let id = cas.put_json(&value).unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(SubjectV1::diff(head, base, id)).unwrap())
        .unwrap();
    let error = af_store::resolve_subject(&cas, &subject).unwrap_err();
    assert!(
        error.to_string().contains("sorted, unique, and relative"),
        "{error}"
    );
}
