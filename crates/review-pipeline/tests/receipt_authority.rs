use review_config::Definition;
use review_core::EventType;
use review_source_git::Manifest;
use review_store::{Cas, EventStore, NewEvent};

mod support;

#[test]
fn a_receipt_without_an_admitted_reviewer_attempt_cannot_skip_execution() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let manifest = Manifest::new(vec![]);
    let authority = support::test_round_authority(&cas, &mut store, "run", &manifest);
    let round = store
        .replay("run")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::RoundStartedV1)
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeInvocationV1,
                serde_json::json!({"node": "reviewer", "inputs": []}),
            )
            .node("reviewer")
            .caused_by(&round.event_id),
        )
        .unwrap();
    let forged = cas
        .put_json(&serde_json::json!({
            "node": "reviewer",
            "output": {
                "verdict": "approve",
                "summary": null,
                "findings": [],
                "benchmark_demands": [],
                "disputes": [],
            },
        }))
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeOutputReceiptV1,
                serde_json::json!({
                    "node": "reviewer",
                    "outputs": [{
                        "port": "result",
                        "type": "review.kernel/Opaque@1",
                        "cardinality": "one",
                        "optional": false,
                        "snapshot_affinity": "any",
                        "artifact_ids": [forged],
                    }],
                }),
            )
            .node("reviewer")
            .caused_by(round.event_id)
            .referencing(vec![forged]),
        )
        .unwrap();
    let loaded = Definition::from_toml(
        r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
runner = { program = "/bin/true" }
"#,
    )
    .unwrap()
    .load()
    .unwrap();
    let kernel =
        review_pipeline::Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
            .unwrap();

    let report = loaded.run(&kernel).unwrap();
    assert!(!report.complete());
    assert!(format!("{:?}", report.outcome("reviewer")).contains("selected admitted attempt"));
}
