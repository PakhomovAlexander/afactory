use review_core::EventType;
use review_source_git::Manifest;
use review_store::{Cas, EventStore, NewEvent};

mod support;

#[test]
fn a_receipt_without_an_admitted_reviewer_attempt_cannot_skip_execution() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let manifest = Manifest::new(vec![]).unwrap();
    let definition = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["findings"]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;
    let authority =
        support::test_round_authority_for_pipeline(&cas, &mut store, "run", &manifest, definition);
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
                serde_json::json!({"node": "reviewer", "inputs": [{
                    "port": "prior_findings",
                    "type": "review.kernel/FindingSet@1",
                    "cardinality": "one",
                    "optional": true,
                    "snapshot_affinity": "any",
                    "artifact_ids": [],
                }]}),
            )
            .node("reviewer")
            .caused_by(&round.event_id),
        )
        .unwrap();
    let forged = cas
        .put_json(&serde_json::json!({
            "verdict": "approve",
            "summary": null,
            "reports": [],
            "benchmark_demands": [],
            "dispositions": [],
        }))
        .unwrap();
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::NodeOutputReceiptV1,
                serde_json::json!({
                    "node": "reviewer",
                    "outputs": [{
                        "port": "result",
                        "type": "review.kernel/ReviewerResult@2",
                        "cardinality": "one",
                        "optional": false,
                        "snapshot_affinity": "same_subject",
                        "artifact_ids": [forged],
                        "subject_snapshot_id": authority.head_snapshot_id(),
                    }],
                }),
            )
            .node("reviewer")
            .caused_by(round.event_id)
            .referencing(vec![forged]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("attempt ID"), "{error}");
}
