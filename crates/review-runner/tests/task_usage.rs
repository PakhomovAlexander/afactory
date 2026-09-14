use review_core::{Producer, task::usage::*};
use review_runner::task::usage::{persist_task_usage_exact, read_task_usage_exact};
use review_store::Cas;

#[test]
fn adaptive_usage_preserves_narrow_identity_and_reopens_all_exact_generations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cas");
    let cas = Cas::open(&path).unwrap();
    let context = cas.put(b"exact context").unwrap();
    let producer = Producer::KernelOperation {
        run_id: "usage-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    };
    let narrow = TaskTokenUsageV2 {
        input_tokens: Some(u64::MAX.into()),
        chargeable_tokens: (u128::from(u64::MAX) + 20).into(),
        ..Default::default()
    };
    let original = cas
        .put_artifact(
            TASK_TOKEN_USAGE_V2,
            producer.clone(),
            vec![context.clone()],
            None,
            serde_json::to_value(&narrow).unwrap(),
        )
        .unwrap()
        .0;
    let bytes = cas.get_json(&original).unwrap();
    let id = persist_task_usage_exact(
        &cas,
        producer.clone(),
        &context,
        &TaskTokenUsageV3::from(narrow.clone()),
    )
    .unwrap();
    assert_eq!(
        id, original,
        "unchanged @2 encoding retains exact artifact identity"
    );
    assert_eq!(cas.get_json(&id).unwrap(), bytes);
    let wide = TaskTokenUsageV3 {
        input_tokens: Some((u128::from(u64::MAX) + 20).into()),
        chargeable_tokens: (u128::from(u64::MAX) + 50).into(),
        ..Default::default()
    };
    let id = persist_task_usage_exact(&cas, producer, &context, &wide).unwrap();
    assert_eq!(
        cas.get_artifact(&id).unwrap().artifact_type,
        TASK_TOKEN_USAGE_V3
    );
    drop(cas);
    let cas = Cas::open(&path).unwrap();
    assert_eq!(read_task_usage_exact(&cas, &id).unwrap(), wide);
    assert_eq!(
        read_task_usage_exact(&cas, &original).unwrap(),
        TaskTokenUsageV3::from(narrow)
    );
    assert!(review_runner::TokenUsage::try_from(&wide).is_err());
}
