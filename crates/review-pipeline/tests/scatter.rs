mod support;

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use review_config::Definition;
use review_runner::{ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore, NewEvent};

const DYNAMIC_V5: &str = include_str!("../../review-config/tests/fixtures/dynamic-v5.toml");

struct CleanShard {
    calls: Arc<AtomicUsize>,
}

impl ReviewerAdapter for CleanShard {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        assert_eq!(inputs.artifacts["slice"].len(), 1);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(result(cas, vec![], b"clean shard"))
    }
}

struct BoundaryCloseout;

impl ReviewerAdapter for BoundaryCloseout {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        assert_eq!(inputs.artifacts["shards"].len(), 1);
        assert!(!inputs.artifacts.contains_key("slice"));
        Ok(result(
            cas,
            vec![serde_json::json!({
                "severity": "major",
                "file": "left.rs",
                "line": 1,
                "title": "cross-slice invariant is broken",
                "body": "each slice is locally clean but the whole Subject is not",
                "fix": "restore the invariant across both files",
                "confidence": 1.0
            })],
            b"whole-subject closeout",
        ))
    }
}

fn result(cas: &Cas, findings: Vec<serde_json::Value>, raw: &[u8]) -> ReviewerReturn {
    ReviewerReturn {
        output: serde_json::from_value(serde_json::json!({
            "verdict": if findings.is_empty() { "approve" } else { "request-changes" },
            "summary": null,
            "findings": findings,
            "benchmark_demands": [],
            "disputes": []
        }))
        .unwrap(),
        proposal: Ok(None),
        cost_tokens: 10,
        raw_artifact: cas.put(raw).unwrap(),
    }
}

fn manifest(cas: &Cas) -> Manifest {
    Manifest::new(
        ["left.rs", "right.rs"]
            .into_iter()
            .map(|path| {
                let bytes = format!("// {path}\n");
                Entry {
                    path: path.into(),
                    kind: EntryKind::File,
                    content: cas.put(bytes.as_bytes()).unwrap(),
                    size: bytes.len() as u64,
                }
            })
            .collect(),
    )
    .unwrap()
}

#[test]
fn scatter_is_lossless_budgeted_and_requires_whole_subject_closeout() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let snapshot = manifest(&cas);
    let loaded = Definition::from_toml(DYNAMIC_V5).unwrap().load().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.clone(),
        DYNAMIC_V5,
    )
    .with_checks(loaded.checks().to_vec())
    .with_budgets(100, 400)
    .with_adapter(
        "scatter",
        Box::new(CleanShard {
            calls: Arc::clone(&calls),
        }),
    )
    .with_adapter("closeout", Box::new(BoundaryCloseout));

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(kernel.fan_out_spent("scatter"), Some(20));
    drop(kernel);

    let round_event_id = store
        .replay("run")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == review_core::EventType::RoundStartedV1)
        .unwrap()
        .event_id;
    let round_events = store.replay("run").unwrap();
    let accepted = round_events
        .iter()
        .find(|event| event.event_type == review_core::EventType::SliceSetAcceptedV1)
        .unwrap();
    let accepted_payload: review_core::SliceSetAcceptedPayloadV1 =
        serde_json::from_value(accepted.payload.clone()).unwrap();
    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
        cas.get_json(&accepted_payload.slice_set_artifact_id)
            .unwrap(),
    )
    .unwrap();
    let mut forged_set: review_core::SliceSetV1 = serde_json::from_value(envelope.payload).unwrap();
    forged_set.max_fanout += 1;
    let (forged_record, forged_envelope) = cas
        .put_artifact(
            review_core::contract::SLICE_SET_V1,
            review_core::Producer::KernelOperation {
                run_id: "run".into(),
                node_id: Some("slicer".into()),
                operation_id: "forged-slice-policy".into(),
            },
            vec![],
            envelope.subject_snapshot_id,
            serde_json::to_value(forged_set).unwrap(),
        )
        .unwrap();
    let error = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_core::EventType::SliceSetAcceptedV1,
                serde_json::to_value(review_core::SliceSetAcceptedPayloadV1 {
                    slice_set_id: forged_envelope.artifact_id,
                    slice_set_artifact_id: forged_record.clone(),
                })
                .unwrap(),
            )
            .node("slicer")
            .caused_by(&round_event_id)
            .referencing(vec![forged_record]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("captured slicing policy"));

    let authority =
        review_pipeline::RoundAuthority::load(&store, &cas, "run", &round_event_id).unwrap();
    let resumed =
        review_pipeline::Kernel::from_loaded(&cas, &mut store, "run", snapshot, &loaded, authority)
            .unwrap()
            .with_budgets(100, 400);
    assert_eq!(resumed.fan_out_spent("scatter"), Some(20));
    drop(resumed);

    let events = store.replay("run").unwrap();
    let accepted = events
        .iter()
        .position(|event| event.event_type == review_core::EventType::SliceSetAcceptedV1)
        .unwrap();
    let first_dispatch = events
        .iter()
        .position(|event| {
            event.event_type == review_core::EventType::AttemptDispatchedV1
                && event
                    .node_id
                    .as_deref()
                    .is_some_and(|node| node.starts_with("scatter#slice:"))
        })
        .unwrap();
    assert!(accepted < first_dispatch);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::ShardSetRecordedV1)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::SemanticClosureCheckedV1)
            .count(),
        1
    );
    assert!(events.iter().any(|event| {
        event.event_type == review_core::EventType::FindingReportedV1
            && event.node_id.as_deref() == Some("closeout")
    }));
}
