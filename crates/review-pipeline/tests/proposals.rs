mod support;

use std::path::Path;

use review_graph::{Node, NodeKind, Pipeline, Port, PortContract, Scheduler};
use review_runner::{
    ReviewerAdapter, ReviewerInputs, ReviewerProposalDeclaration, ReviewerReturn, RunnerError,
};
use review_source_git::{Entry, EntryKind, Manifest, manifest_diff};
use review_store::{Cas, EventStore, NewEvent};

const AUTHORITY: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "correctness"
kind = "reviewer"
outputs = ["result"]
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["correctness"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "correctness", port = "result" }
to = { node = "gather", port = "correctness" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

fn graph() -> Pipeline {
    Pipeline::default()
        .node(Node::new("correctness", NodeKind::Reviewer).emitting(&["result"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["correctness"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting_contracts(vec![
                    PortContract::new("findings", review_core::contract::FINDING_SET_V1),
                    PortContract::new("demands", review_core::contract::DEMAND_SET_V1),
                ]),
        )
        .edge(
            Port::new("correctness", "result"),
            Port::new("gather", "correctness"),
        )
        .edge(
            Port::new("gather", "reports"),
            Port::new("ledger", "reports"),
        )
}

fn manifest(cas: &Cas, bytes: &[u8]) -> Manifest {
    Manifest::new(vec![Entry {
        path: "src/lib.rs".into(),
        kind: EntryKind::File,
        content: cas.put(bytes).unwrap(),
        size: bytes.len() as u64,
    }])
    .unwrap()
}

struct Proposer {
    mismatch: bool,
}

impl ReviewerAdapter for Proposer {
    fn invoke(
        &self,
        cas: &Cas,
        root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let before = b"pub fn value() -> u8 { 1 }\n";
        let after = b"pub fn value() -> u8 { 2 }\n";
        std::fs::write(root.join("src/lib.rs"), after).unwrap();
        let diff = manifest_diff(&manifest(cas, before), &manifest(cas, after), cas).unwrap();
        let mut patch = String::from_utf8(diff.patch().to_vec()).unwrap();
        if self.mismatch {
            patch.push_str("# unrelated declaration\n");
        }
        Ok(ReviewerReturn {
            output: serde_json::from_value(serde_json::json!({
                "verdict": "request-changes",
                "summary": null,
                "findings": [{
                    "severity": "major",
                    "file": "src/lib.rs",
                    "line": 1,
                    "title": "value is wrong",
                    "body": "the old value violates the contract",
                    "fix": "return two",
                    "confidence": 1.0
                }],
                "benchmark_demands": [],
                "disputes": []
            }))
            .unwrap(),
            proposal: Ok(Some(ReviewerProposalDeclaration {
                patch,
                report_indexes: vec![0],
                finding_ids: vec![],
                evidence_ids: vec![],
                paths: vec!["src/lib.rs".into()],
                description: "return the required value".into(),
                auto_apply_nominated: true,
            })),
            cost_tokens: 10,
            raw_artifact: cas.put(b"proposal reviewer response").unwrap(),
        })
    }
}

fn run_case(mismatch: bool) -> (tempfile::TempDir, Vec<review_core::RunEvent>) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let snapshot = manifest(&cas, b"pub fn value() -> u8 { 1 }\n");
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas, &mut store, "run", snapshot, AUTHORITY,
    )
    .with_adapter("correctness", Box::new(Proposer { mismatch }));
    let report = Scheduler::new(&graph().plan().unwrap()).run(&kernel);
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);
    let events = store.replay("run").unwrap();
    drop(store);
    drop(cas);
    (directory, events)
}

#[test]
fn exact_sealed_diff_becomes_one_finalized_proposal() {
    let (directory, events) = run_case(false);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::ProposalPreparedV1)
            .count(),
        1
    );
    let accepted = events
        .iter()
        .find(|event| event.event_type == review_core::EventType::ProposalAcceptedV1)
        .unwrap();
    let payload: review_core::ProposalAcceptedPayloadV1 =
        serde_json::from_value(accepted.payload.clone()).unwrap();
    let cas = Cas::open_existing(directory.path().join("cas")).unwrap();
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&payload.proposal_artifact_id).unwrap()).unwrap();
    assert_eq!(envelope.artifact_id, payload.proposal_id);
    let proposal: review_core::PatchProposal = serde_json::from_value(envelope.payload).unwrap();
    assert_eq!(proposal.paths, ["src/lib.rs"]);
    assert_eq!(proposal.finding_refs.len(), 1);
    assert_eq!(
        proposal.finding_refs[0].kind,
        review_core::ClaimRefKind::Report
    );

    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let mut forged = payload;
    forged.proposal_id = format!("sha256:{}", "f".repeat(64));
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_core::EventType::ProposalAcceptedV1,
                serde_json::to_value(forged).unwrap(),
            )
            .node(accepted.node_id.as_deref().unwrap())
            .attempt(accepted.attempt_id.as_deref().unwrap())
            .caused_by(accepted.causation_id.as_deref().unwrap())
            .correlating(accepted.correlation_id.as_deref().unwrap())
            .referencing(accepted.artifact_refs.clone()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("Proposal envelope"));
}

#[test]
fn declaration_unequal_to_the_sealed_diff_is_refused_and_absent() {
    let (_directory, events) = run_case(true);
    let refused = events
        .iter()
        .find(|event| event.event_type == review_core::EventType::ProposalRefusedV1)
        .unwrap();
    let payload: review_core::ProposalRefusedPayloadV1 =
        serde_json::from_value(refused.payload.clone()).unwrap();
    assert_eq!(
        payload.reason,
        review_core::ProposalRefusalReasonV1::PatchMismatch
    );
    assert!(
        events
            .iter()
            .all(|event| event.event_type != review_core::EventType::ProposalAcceptedV1)
    );
}
