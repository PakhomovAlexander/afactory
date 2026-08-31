mod support;

use std::path::Path;

use review_config::Definition;
use review_runner::{
    ReviewerAdapter, ReviewerInputs, ReviewerProposalDeclaration, ReviewerReturn, RunnerError,
};
use review_source_git::{Entry, EntryKind, Manifest, manifest_diff};
use review_store::{Cas, EventStore, NewEvent};

const PIPELINE: &str = r#"
version = 5
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
[integration]
protected_paths = [".github"]
post_apply_checks = ["postapply"]
reviewer_priority = ["scatter"]
[[checks]]
name = "postapply"
program = "/usr/bin/true"
[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "slicer"
kind = "slicer"
outputs = [{ name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
slicing = { scatter = "scatter", max_paths_per_slice = 1, max_fanout = 2, coverage = "complete", all_shards_required = true, closeout = "required" }
[[nodes]]
id = "scatter"
kind = "scatter"
inputs = [
  { name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free", auto_apply = true }
[[nodes]]
id = "closeout"
kind = "reviewer"
closeout_for = "scatter"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free" }
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "closeout", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "scatter", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "closeout", port = "prior_findings" }
[[edges]]
from = { node = "slicer", port = "slices" }
to = { node = "scatter", port = "slices" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "closeout", port = "shards" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "ledger", port = "shards" }
[[edges]]
from = { node = "closeout", port = "result" }
to = { node = "ledger", port = "closeout" }
"#;

fn manifest(cas: &Cas, left: &[u8], right: &[u8]) -> Manifest {
    Manifest::new(
        [("left.rs", left), ("right.rs", right)]
            .into_iter()
            .map(|(path, bytes)| Entry {
                path: path.into(),
                kind: EntryKind::File,
                content: cas.put(bytes).unwrap(),
                size: bytes.len() as u64,
            })
            .collect(),
    )
    .unwrap()
}

struct ProposingShard;

impl ReviewerAdapter for ProposingShard {
    fn invoke(
        &self,
        cas: &Cas,
        root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let slice_record: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(inputs.artifacts["slice"][0].artifact_id())
                .unwrap(),
        )
        .unwrap();
        let slice: review_core::ReviewSliceV1 =
            serde_json::from_value(slice_record.payload).unwrap();
        let path = slice.paths[0].clone();
        let left = b"pub const LEFT: u8 = 1;\n";
        let right = b"pub const RIGHT: u8 = 1;\n";
        let changed = if path == "left.rs" {
            b"pub const LEFT: u8 = 2;\n".as_slice()
        } else {
            b"pub const RIGHT: u8 = 2;\n".as_slice()
        };
        std::fs::write(root.join(&path), changed).unwrap();
        let before = manifest(cas, left, right);
        let after = if path == "left.rs" {
            manifest(cas, changed, right)
        } else {
            manifest(cas, left, changed)
        };
        let patch =
            String::from_utf8(manifest_diff(&before, &after, cas).unwrap().patch().into()).unwrap();
        Ok(ReviewerReturn {
            output: serde_json::from_value(serde_json::json!({
                "verdict": "request-changes",
                "summary": null,
                "findings": [{
                    "severity": "minor",
                    "file": path,
                    "line": 1,
                    "title": "constant can be corrected",
                    "body": "the deterministic fixture expects two",
                    "fix": "set the constant to two",
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
                paths: vec![path],
                description: "set the fixture constant to two".into(),
                auto_apply_nominated: true,
            })),
            cost_tokens: 10,
            raw_artifact: cas.put(b"proposing shard").unwrap(),
        })
    }
}

struct CleanCloseout;

impl ReviewerAdapter for CleanCloseout {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        Ok(ReviewerReturn {
            output: serde_json::from_value(serde_json::json!({
                "verdict": "approve",
                "summary": null,
                "findings": [],
                "benchmark_demands": [],
                "disputes": []
            }))
            .unwrap(),
            proposal: Ok(None),
            cost_tokens: 10,
            raw_artifact: cas.put(b"clean closeout").unwrap(),
        })
    }
}

#[test]
fn checked_disjoint_proposals_promote_one_internal_snapshot_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let original = manifest(
        &cas,
        b"pub const LEFT: u8 = 1;\n",
        b"pub const RIGHT: u8 = 1;\n",
    );
    let loaded = Definition::from_toml(PIPELINE).unwrap().load().unwrap();
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        original.clone(),
        PIPELINE,
    )
    .with_checks(loaded.checks().to_vec())
    .with_adapter("scatter", Box::new(ProposingShard))
    .with_adapter("closeout", Box::new(CleanCloseout));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert_eq!(
        kernel
            .publish_report(&report, *loaded.convergence())
            .unwrap(),
        review_pipeline::RunVerdict::Pass
    );
    drop(kernel);

    let events = store.replay("run").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::IntegrationPreparedV1)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::IntegrationCommittedV1)
            .count(),
        1
    );
    let committed = events
        .iter()
        .find(|event| event.event_type == review_core::EventType::IntegrationCommittedV1)
        .unwrap();
    let payload: review_core::IntegrationCommittedPayloadV1 =
        serde_json::from_value(committed.payload.clone()).unwrap();
    assert_eq!(payload.proposal_ids.len(), 2);
    assert_eq!(payload.attestation_ids.len(), 2);
    let subject: review_core::SubjectV1 =
        serde_json::from_value(cas.get_json(&payload.derived_subject_id).unwrap()).unwrap();
    let snapshot: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(&subject.head_snapshot_id).unwrap()).unwrap();
    assert!(snapshot.is_derived());
    let derived: Manifest = serde_json::from_value(
        cas.get_json(snapshot.artifact_manifest.as_deref().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_ne!(derived.content_digest(), original.content_digest());
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::ChangeAttestedV1)
            .count(),
        2
    );

    let prepared = events
        .iter()
        .find(|event| event.event_type == review_core::EventType::IntegrationPreparedV1)
        .unwrap();
    let prepared_payload: review_core::IntegrationPreparedPayloadV1 =
        serde_json::from_value(prepared.payload.clone()).unwrap();
    let mut forged_plan: review_core::IntegrationPlanV1 =
        serde_json::from_value(cas.get_json(&prepared_payload.plan_artifact_id).unwrap()).unwrap();
    let unrelated_manifest_id = cas
        .put_json(&serde_json::to_value(&original).unwrap())
        .unwrap();
    forged_plan.derived_manifest_artifact_id = unrelated_manifest_id.clone();
    let forged_plan_id = cas
        .put_json(&serde_json::to_value(&forged_plan).unwrap())
        .unwrap();
    let forged_batch = format!("integration-{}", &forged_plan_id[7..23]);
    let prior: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(&payload.prior_snapshot_id).unwrap()).unwrap();
    let forged_snapshot = review_core::SourceSnapshot {
        repository_id: prior.repository_id,
        vcs: prior.vcs,
        capture: review_core::Capture::Derived {
            tree_id: original.content_digest(),
            parent_snapshot_id: payload.prior_snapshot_id.clone(),
            integration_batch_id: forged_batch.clone(),
        },
        content_digest: original.content_digest(),
        parent_snapshot_id: Some(payload.prior_snapshot_id.clone()),
        source_revision: prior.source_revision,
        artifact_manifest: Some(unrelated_manifest_id.clone()),
        submodules: prior.submodules,
    };
    let forged_snapshot_id = cas
        .put_json(&serde_json::to_value(forged_snapshot).unwrap())
        .unwrap();
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_core::EventType::IntegrationPreparedV1,
                serde_json::to_value(review_core::IntegrationPreparedPayloadV1 {
                    batch_id: forged_batch,
                    plan_artifact_id: forged_plan_id.clone(),
                    derived_snapshot_id: forged_snapshot_id.clone(),
                })
                .unwrap(),
            )
            .correlating(payload.prior_subject_id)
            .referencing(vec![
                forged_plan_id,
                forged_snapshot_id,
                unrelated_manifest_id,
            ]),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deterministic Proposal composition")
    );
}

fn run_pipeline(pipeline: &str) -> (tempfile::TempDir, Vec<review_core::RunEvent>) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let original = manifest(
        &cas,
        b"pub const LEFT: u8 = 1;\n",
        b"pub const RIGHT: u8 = 1;\n",
    );
    let loaded = Definition::from_toml(pipeline).unwrap().load().unwrap();
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas, &mut store, "run", original, pipeline,
    )
    .with_checks(loaded.checks().to_vec())
    .with_adapter("scatter", Box::new(ProposingShard))
    .with_adapter("closeout", Box::new(CleanCloseout));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert_eq!(
        kernel
            .publish_report(&report, *loaded.convergence())
            .unwrap(),
        review_pipeline::RunVerdict::Pass
    );
    drop(kernel);
    let events = store.replay("run").unwrap();
    drop(store);
    drop(cas);
    (directory, events)
}

#[test]
fn protected_path_refusal_never_prepares_or_promotes_a_snapshot() {
    let pipeline = PIPELINE.replace(
        "protected_paths = [\".github\"]",
        "protected_paths = [\"left.rs\"]",
    );
    let (_directory, events) = run_pipeline(&pipeline);
    assert!(events.iter().any(|event| {
        event.event_type == review_core::EventType::IntegrationConflictV1
            && event.payload["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("protected"))
    }));
    assert!(!events.iter().any(|event| matches!(
        event.event_type,
        review_core::EventType::IntegrationPreparedV1
            | review_core::EventType::IntegrationCommittedV1
    )));
}

#[test]
fn failed_postapply_check_retains_preparation_but_never_promotes() {
    let pipeline = PIPELINE.replace(
        "program = \"/usr/bin/true\"",
        "program = \"/usr/bin/grep\"\nargs = [{ value = \" = 1;\" }, { value = \"left.rs\" }]",
    );
    let (_directory, events) = run_pipeline(&pipeline);
    assert!(
        events
            .iter()
            .any(|event| { event.event_type == review_core::EventType::IntegrationPreparedV1 })
    );
    assert!(events.iter().any(|event| {
        event.event_type == review_core::EventType::IntegrationChecksCompletedV1
            && event.payload["passed"] == false
    }));
    assert!(
        !events
            .iter()
            .any(|event| { event.event_type == review_core::EventType::IntegrationCommittedV1 })
    );
}
