use review_core::{Producer, task::usage::*};
use review_runner::TokenUsage;
use review_runner::task::usage::{persist_task_usage_exact, read_task_usage_exact};
use review_store::Cas;

#[test]
fn every_usage_width_is_one_exact_generation_that_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cas");
    let cas = Cas::open(&path).unwrap();
    let context = cas.put(b"exact context").unwrap();
    let producer = Producer::KernelOperation {
        run_id: "usage-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    };
    let narrow = TokenUsage {
        input_tokens: Some(u64::MAX),
        chargeable_tokens: 7,
        ..TokenUsage::default()
    };
    let wide = TaskTokenUsageV3 {
        input_tokens: Some((u128::from(u64::MAX) + 20).into()),
        chargeable_tokens: (u128::from(u64::MAX) + 50).into(),
        ..Default::default()
    };
    let narrow_id = persist_task_usage_exact(&cas, producer.clone(), &context, &narrow).unwrap();
    let wide_id = persist_task_usage_exact(&cas, producer.clone(), &context, &wide).unwrap();
    for id in [&narrow_id, &wide_id] {
        let envelope = cas.get_artifact(id).unwrap();
        assert_eq!(envelope.artifact_type, TASK_TOKEN_USAGE_V3);
        assert_eq!(envelope.producer, producer);
        assert_eq!(envelope.input_artifacts, [context.clone()]);
    }
    drop(cas);
    let cas = Cas::open(&path).unwrap();
    assert_eq!(
        read_task_usage_exact(&cas, &narrow_id).unwrap(),
        TaskTokenUsageV3::from(&narrow)
    );
    assert_eq!(read_task_usage_exact(&cas, &wide_id).unwrap(), wide);

    // Another generation or an unenveloped usage blob is not Task usage.
    let other = cas
        .put_artifact(
            "af/TaskTokenUsage@2",
            producer,
            vec![context],
            None,
            serde_json::to_value(TaskTokenUsageV3::charge_only(7)).unwrap(),
        )
        .unwrap()
        .0;
    assert!(read_task_usage_exact(&cas, &other).is_err());
    let bare = cas
        .put_json(&serde_json::json!({"chargeable_tokens":7}))
        .unwrap();
    assert!(read_task_usage_exact(&cas, &bare).is_err());
}
