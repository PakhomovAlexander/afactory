#[path = "task_legacy_review/capture.rs"]
mod capture;
#[path = "task_legacy_review/plan.rs"]
mod plan;
mod support;

use std::collections::BTreeMap;

use review_attempt::task_budget::NodeAllowance;
use review_config::task::legacy_review::{
    ReviewCompileContext, ReviewWorker, compile_legacy_review,
};
use review_core::task::{TaskLimitsV1, VerificationReserveV1};
use review_core::{ArtifactEnvelope, RoundStartedPayloadV1};
use review_graph::task::Address;
use review_pipeline::task::legacy_review::CapturedLegacyReviewRound;
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

const PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "assigned", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
runner = { program = "/bin/true" }
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

#[test]
fn actual_round_capture_and_generation_reopen_without_a_second_execution_log() {
    let temp = tempfile::tempdir().unwrap();
    let cas_root = temp.path().join("cas");
    let store_path = temp.path().join("events.sqlite");
    let cas = Cas::open(&cas_root).unwrap();
    let mut store = EventStore::open(&store_path).unwrap();
    let authority = support::test_canonical_round_authority_for_pipeline(
        &cas,
        &mut store,
        "review",
        &Manifest::default(),
        PIPELINE,
    );
    let round_event = authority.round_event_id().to_owned();
    let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round_event).unwrap();
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
    let loaded = review_config::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(
        &loaded,
        ReviewCompileContext {
            finding_identity_policy: captured.authority().finding_identity_policy().into(),
            inputs: inputs.clone(),
            head_input: "head".into(),
            round_input: "round".into(),
            workers: BTreeMap::from([(
                "reviewer".into(),
                ReviewWorker {
                    package: "fixture/reviewer".into(),
                    allowance: NodeAllowance {
                        tokens_per_attempt: 0,
                        wall_ms_per_attempt: 1000,
                        max_attempts: 1,
                        verification_attempts: 0,
                    },
                },
            )]),
            outputs: BTreeMap::from([(
                "prior".into(),
                Address {
                    node: "generation".into(),
                    port: "assigned".into(),
                },
            )]),
            limits: TaskLimitsV1 {
                tokens: 0,
                max_attempts: 1,
                deadline_unix_ms: 9999999999999,
                verification: VerificationReserveV1 {
                    tokens: 0,
                    attempts: 0,
                    wall_ms: 0,
                },
            },
            max_parallel: 1,
            gate_wall_ms: 1000,
        },
    )
    .unwrap();
    let mapping = &compilation.nodes["generation"];
    let node = &loaded.planned().nodes["generation"];
    let outputs = captured
        .generation_outputs(&cas, loaded.version(), node, mapping)
        .unwrap();
    assert_eq!(
        outputs.len(),
        1,
        "genesis is absent, never an invented Finding Set"
    );
    let assigned = mapping
        .outputs
        .iter()
        .find(|(_, port)| port.review_port == "assigned")
        .unwrap();
    let round_payload: RoundStartedPayloadV1 = serde_json::from_value(
        store
            .latest_round_started("review")
            .unwrap()
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(
        assigned
            .1
            .codec
            .restore(&cas, &outputs[assigned.0].artifact_ids[0])
            .unwrap(),
        round_payload.prior_finding_set_id
    );
    assert_eq!(store.len("review").unwrap(), before);
    assert_eq!(
        before, 2,
        "input capture and Generation allocate no Attempt or legacy events"
    );
    captured.check_current(&cas, &store).unwrap();
    drop(captured);
    drop(store);
    drop(cas);
    let cas = Cas::open_existing(&cas_root).unwrap();
    let store = EventStore::open(&store_path).unwrap();
    let captured = CapturedLegacyReviewRound::load(&cas, &store, "review", &round_event).unwrap();
    assert_eq!(captured.capture_inputs(&cas).unwrap(), inputs);
    assert_eq!(
        captured
            .generation_outputs(&cas, loaded.version(), node, mapping)
            .unwrap(),
        outputs
    );
    assert!(CapturedLegacyReviewRound::load(&cas, &store, "review", &"z".repeat(26)).is_err());
    let hex = binding.head_snapshot_id.strip_prefix("sha256:").unwrap();
    std::fs::write(
        cas_root.join("objects").join(&hex[..2]).join(&hex[2..]),
        b"{}",
    )
    .unwrap();
    assert!(captured.check_current(&cas, &store).is_err());
    assert!(captured.capture_inputs(&cas).is_err());
}
