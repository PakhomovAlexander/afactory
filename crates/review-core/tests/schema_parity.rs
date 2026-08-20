//! The schemas and the Rust types must not drift.
//!
//! Each contract is checked in both directions: a fully-populated Rust value must satisfy the
//! schema, and an instance the schema should reject must actually be rejected. A schema that
//! accepts everything passes the first check alone, which is why the negative cases are here.

use std::path::PathBuf;

use review_core::{
    ArtifactEnvelope, ClaimRef, ClaimRefKind, EventType, FindingReport, Location, MissingNodeV2,
    NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, PatchProposal, PortArtifactsV1,
    PortCardinality, Producer, RunEvent, RunFailureReasonV2, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportPayloadV2, RunSuppressionReasonV2, RunVerdictV2, SnapshotAffinity, SourceSnapshot,
    finding::{ClaimTargetKind, Relation, RelationKind, RelationTarget},
    snapshot::{Capture, DirtyBoundary, Submodule, Vcs},
};
use serde_json::{Value, json};

const SCHEMAS: [&str; 8] = [
    "artifact-envelope-v1.json",
    "finding-report-v1.json",
    "node-invocation-v1.json",
    "node-output-receipt-v1.json",
    "patch-proposal-v1.json",
    "run-event-v1.json",
    "run-report-v2.json",
    "source-snapshot-v1.json",
];

fn schema(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn validator(name: &str) -> jsonschema::Validator {
    jsonschema::validator_for(&schema(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn assert_valid(name: &str, instance: &Value) {
    let v = validator(name);
    if !v.is_valid(instance) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();
        panic!(
            "{name} rejected a value it must accept: {}",
            errors.join("; ")
        );
    }
}

fn assert_invalid(name: &str, instance: &Value, why: &str) {
    assert!(
        !validator(name).is_valid(instance),
        "{name} accepted a value it must reject ({why})"
    );
}

#[test]
fn every_schema_is_a_valid_json_schema() {
    for name in SCHEMAS {
        let _ = validator(name);
    }
}

#[test]
fn finding_report_roundtrips() {
    let report = FindingReport {
        title: "Retry loop can spin forever".into(),
        severity: review_core::Severity::Blocker,
        locations: vec![Location::at("src/a.rs", 12), Location::file("src/b.rs")],
        body: "no backoff, no cap".into(),
        fix: "cap the retries and add jitter".into(),
        confidence: 0.93,
        failure_trace: Some("thread 'main' panicked".into()),
        rule_id: Some("review.rules.perf/quadratic-scan@2".into()),
        occurrence_key: Some("src/a.rs::retry_loop".into()),
        relations: vec![Relation {
            kind: RelationKind::Corroborates,
            target: RelationTarget {
                kind: ClaimTargetKind::Finding,
                id: "finding:01j".into(),
            },
            reason: Some("same loop, independent reproduction".into()),
        }],
    };
    let value = serde_json::to_value(&report).unwrap();
    assert_valid("finding-report-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<FindingReport>(value).unwrap(),
        report
    );
}

#[test]
fn finding_report_rejects_what_the_design_forbids() {
    let base = json!({
        "title": "t", "severity": "major", "locations": [],
        "body": "b", "fix": "f", "confidence": 0.5
    });
    assert_valid("finding-report-v1.json", &base);

    let mut no_fix = base.clone();
    no_fix.as_object_mut().unwrap().remove("fix");
    assert_invalid("finding-report-v1.json", &no_fix, "fix is required");

    let mut bad_severity = base.clone();
    bad_severity["severity"] = json!("critical");
    assert_invalid(
        "finding-report-v1.json",
        &bad_severity,
        "severity is a closed enum — an unknown rank must not slip under a gate",
    );

    let mut status = base.clone();
    status["status"] = json!("open");
    assert_invalid(
        "finding-report-v1.json",
        &status,
        "a report carries no status: state belongs to the projection",
    );

    let mut bad_confidence = base.clone();
    bad_confidence["confidence"] = json!(1.5);
    assert_invalid(
        "finding-report-v1.json",
        &bad_confidence,
        "confidence is 0..=1",
    );
}

#[test]
fn source_snapshot_roundtrips_every_capture_kind() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let captures = [
        Capture::Committed {
            tree_id: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        },
        Capture::SyntheticWorktree {
            tree_id: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
            boundary: DirtyBoundary::Revalidated,
            attempts: Some(2),
        },
        Capture::Derived {
            tree_id: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
            parent_snapshot_id: digest.clone(),
            integration_batch_id: "integ:01j".into(),
        },
    ];

    for capture in captures {
        let snapshot = SourceSnapshot {
            repository_id: "example-org/project-hub".into(),
            vcs: Vcs::Git,
            capture,
            content_digest: digest.clone(),
            parent_snapshot_id: None,
            source_revision: Some("bba24cb".into()),
            artifact_manifest: Some(digest.clone()),
            submodules: vec![Submodule {
                path: "contrib/x".into(),
                revision: "0123456".into(),
                included: Some(false),
            }],
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_valid("source-snapshot-v1.json", &value);
        assert_eq!(
            serde_json::from_value::<SourceSnapshot>(value).unwrap(),
            snapshot
        );
    }
}

#[test]
fn source_snapshot_has_no_best_effort_capture() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let value = json!({
        "repository_id": "r", "vcs": "git",
        "capture": { "kind": "best_effort", "tree_id": "t" },
        "content_digest": digest
    });
    assert_invalid(
        "source-snapshot-v1.json",
        &value,
        "a best-effort copy must not be expressible as a capture",
    );
}

#[test]
fn patch_proposal_roundtrips() {
    let digest = format!("sha256:{}", "b".repeat(64));
    let proposal = PatchProposal {
        base_snapshot_id: digest.clone(),
        patch_artifact_id: digest.clone(),
        finding_refs: vec![ClaimRef {
            kind: ClaimRefKind::Report,
            id: "report:01j".into(),
        }],
        evidence_ids: vec![digest.clone()],
        paths: vec!["src/a.rs".into()],
        description: "cap the retries".into(),
        auto_apply_nominated: true,
    };
    let value = serde_json::to_value(&proposal).unwrap();
    assert_valid("patch-proposal-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<PatchProposal>(value).unwrap(),
        proposal
    );
    assert!(proposal.check_shape().is_ok());
}

#[test]
fn patch_proposal_must_name_a_claim() {
    let digest = format!("sha256:{}", "b".repeat(64));
    let value = json!({
        "base_snapshot_id": digest, "patch_artifact_id": digest,
        "finding_refs": [], "paths": ["src/a.rs"], "description": "d"
    });
    assert_invalid(
        "patch-proposal-v1.json",
        &value,
        "a patch that names no claim cannot be verified",
    );
}

#[test]
fn run_event_roundtrips() {
    let event = RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w1xy".into(),
        run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
        sequence: 184,
        event_type: EventType::FindingReportedV1,
        occurred_at: "2026-08-16T12:00:00Z".into(),
        node_id: Some("architecture.storage".into()),
        attempt_id: Some("01jd8m4qz9k7v3n2p6r8t0w200".into()),
        causation_id: Some("01jd8m4qz9k7v3n2p6r8t0w201".into()),
        correlation_id: Some("finding:01j".into()),
        artifact_refs: vec![format!("sha256:{}", "c".repeat(64))],
        payload: json!({ "severity": "major" }),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_valid("run-event-v1.json", &value);
    assert_eq!(serde_json::from_value::<RunEvent>(value).unwrap(), event);
    assert_eq!(event.typed(), ("FindingReported", 1));
}

#[test]
fn run_event_schema_and_rust_vocabulary_are_identical() {
    let schema = schema("run-event-v1.json");
    let declared = schema["properties"]["type"]["enum"].as_array().unwrap();
    let rust: Vec<Value> = EventType::ALL
        .into_iter()
        .map(|event_type| serde_json::to_value(event_type).unwrap())
        .collect();
    assert_eq!(declared, &rust);
    assert!(serde_json::from_str::<EventType>("\"Unknown@1\"").is_err());
}

#[test]
fn run_report_v2_is_structural_and_both_report_versions_remain_readable() {
    let report = RunReportPayloadV2 {
        outcomes: vec![
            RunNodeReportV2 {
                node: "architecture".into(),
                outcome: RunNodeOutcomeV2::Suppressed {
                    reason: RunSuppressionReasonV2::GateBlocked,
                },
            },
            RunNodeReportV2 {
                node: "gate".into(),
                outcome: RunNodeOutcomeV2::Completed {
                    output_artifacts: vec![],
                },
            },
        ],
        blocked_gates: vec!["gate".into()],
        verdict: RunVerdictV2::Incomplete {
            missing_nodes: vec![MissingNodeV2 {
                node: "architecture".into(),
                reason: "gate blocked".into(),
            }],
        },
        spent_tokens: Some(42),
    };
    let value = serde_json::to_value(&report).unwrap();
    assert_valid("run-report-v2.json", &value);
    assert_eq!(
        serde_json::from_value::<RunReportPayloadV2>(value).unwrap(),
        report
    );

    let mut event = RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w1xy".into(),
        run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
        sequence: 1,
        event_type: EventType::RunReportV1,
        occurred_at: "2026-08-16T12:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: vec![],
        payload: json!({ "verdict": "Fail(NotConverged)" }),
    };
    assert_eq!(
        review_core::run_report_closes_round(&event).unwrap(),
        Some(true)
    );
    event.payload = json!({ "verdict": "Incomplete { missing: [] }" });
    assert_eq!(
        review_core::run_report_closes_round(&event).unwrap(),
        Some(false)
    );
    event.event_type = EventType::RunReportV2;
    event.payload = serde_json::to_value(report).unwrap();
    assert_eq!(
        review_core::run_report_closes_round(&event).unwrap(),
        Some(false)
    );

    event.payload = serde_json::to_value(RunReportPayloadV2 {
        outcomes: vec![RunNodeReportV2 {
            node: "review".into(),
            outcome: RunNodeOutcomeV2::Completed {
                output_artifacts: vec![],
            },
        }],
        blocked_gates: vec![],
        verdict: RunVerdictV2::Fail {
            reason: RunFailureReasonV2::Exhausted,
        },
        spent_tokens: None,
    })
    .unwrap();
    assert_eq!(
        review_core::run_report_closes_round(&event).unwrap(),
        Some(true)
    );
}

#[test]
fn node_invocation_and_output_receipt_roundtrip() {
    let selection = PortArtifactsV1 {
        port: "subject".into(),
        artifact_type: review_core::contract::SOURCE_SNAPSHOT_V1.into(),
        cardinality: PortCardinality::One,
        optional: false,
        snapshot_affinity: SnapshotAffinity::SameSubject,
        artifact_ids: vec![format!("sha256:{}", "a".repeat(64))],
        subject_snapshot_id: Some(format!("sha256:{}", "b".repeat(64))),
    };
    let invocation = NodeInvocationPayloadV1 {
        node: "architecture".into(),
        inputs: vec![selection.clone()],
    };
    let value = serde_json::to_value(&invocation).unwrap();
    assert_valid("node-invocation-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<NodeInvocationPayloadV1>(value).unwrap(),
        invocation
    );

    let receipt = NodeOutputReceiptPayloadV1 {
        node: "architecture".into(),
        outputs: vec![selection],
    };
    let value = serde_json::to_value(&receipt).unwrap();
    assert_valid("node-output-receipt-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<NodeOutputReceiptPayloadV1>(value).unwrap(),
        receipt
    );
}

#[test]
fn artifact_envelope_roundtrips_both_producers() {
    let digest = format!("sha256:{}", "d".repeat(64));
    let producers = [
        Producer::Attempt {
            run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
            node_id: "architecture.api".into(),
            attempt_id: "01jd8m4qz9k7v3n2p6r8t0w202".into(),
        },
        Producer::KernelOperation {
            run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
            node_id: None,
            operation_id: "reduction:01j".into(),
        },
    ];
    for producer in producers {
        let deterministic = producer.is_deterministic();
        let envelope = ArtifactEnvelope {
            artifact_type: review_core::contract::FINDING_REPORT_V1.into(),
            artifact_id: digest.clone(),
            content_id: digest.clone(),
            producer,
            input_artifacts: vec![digest.clone()],
            subject_snapshot_id: Some(digest.clone()),
            payload: json!({}),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_valid("artifact-envelope-v1.json", &value);
        assert_eq!(
            serde_json::from_value::<ArtifactEnvelope>(value).unwrap(),
            envelope
        );
        assert_eq!(envelope.producer.is_deterministic(), deterministic);
    }
}
