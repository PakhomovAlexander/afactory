#[path = "task_campaign_review/capture.rs"]
mod capture;
#[path = "task_campaign_review/host.rs"]
mod host;
#[path = "task_campaign_review/plan.rs"]
mod plan;
mod support;

use std::collections::BTreeMap;

use review_core::ArtifactEnvelope;
use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use review_graph::task::Address;
use review_pipeline::task::campaign_review::CapturedCampaignReviewRound;
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

const PIPELINE: &str = r#"
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
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

/// The reviewer's typed output line in [`PIPELINE`], for tests that rebind the reviewer node.
#[allow(dead_code)]
const REVIEWER_OUTPUTS: &str = r#"outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]"#;

#[test]
fn actual_round_capture_reopens_without_a_second_execution_log() {
    let temp = tempfile::tempdir().unwrap();
    let cas_root = temp.path().join("cas");
    let store_path = temp.path().join("events.sqlite");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(&store_path).unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "review",
        &Manifest::default(),
        PIPELINE,
    );
    let round_event = authority.round_event_id().to_owned();
    let captured = CapturedCampaignReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    let before = store.len("review").unwrap();
    let inputs = captured.capture_inputs(&cas).unwrap();
    assert_eq!(inputs, captured.capture_inputs(&cas).unwrap());
    let binding = captured.binding();
    binding.validate().unwrap();
    let head: ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&inputs["head"].artifact_ids[0]).unwrap()).unwrap();
    assert_eq!(head.content_id, authority.head_snapshot_id());
    assert_eq!(head.input_artifacts, [authority.head_snapshot_id()]);
    assert_eq!(
        head.payload,
        cas.get_json(authority.head_snapshot_id()).unwrap()
    );
    let round: ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&inputs["round"].artifact_ids[0]).unwrap()).unwrap();
    assert_eq!(round.payload, serde_json::to_value(&binding).unwrap());
    assert_eq!(round.input_artifacts, binding.artifact_refs());
    assert_eq!(store.len("review").unwrap(), before);
    assert_eq!(
        before, 2,
        "input capture allocates no Attempt or Round-runtime events"
    );
    drop(captured);
    drop(store);
    drop(cas);
    let cas = Cas::open_existing(&cas_root).unwrap();
    let store = EventStore::open(&store_path).unwrap();
    let captured = CapturedCampaignReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    assert_eq!(captured.binding(), binding);
    assert_eq!(captured.capture_inputs(&cas).unwrap(), inputs);
    assert!(CapturedCampaignReviewRound::load(&cas, &store, "review", &"z".repeat(26)).is_err());
    let hex = binding.head_snapshot_id.strip_prefix("sha256:").unwrap();
    std::fs::write(
        cas_root.join("objects").join(&hex[..2]).join(&hex[2..]),
        b"{}",
    )
    .unwrap();
    assert!(CapturedCampaignReviewRound::load(&cas, &store, "review", &round_event).is_err());
    assert!(captured.capture_inputs(&cas).is_err());
}
