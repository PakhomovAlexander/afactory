//! The schemas and the Rust types must not drift.
//!
//! Each contract is checked in both directions: a fully-populated Rust value must satisfy the
//! schema, and an instance the schema should reject must actually be rejected. A schema that
//! accepts everything passes the first check alone, which is why the negative cases are here.

use std::path::PathBuf;

use review_core::{
    ArtifactEnvelope, AuthorityFileV1, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, ChangeAttestationV1, ChangeSetV1, ChangedRegionV1, ClaimRef,
    ClaimRefKind, DEMAND_REDUCER_VERSION, DemandRequirement, DemandSetEntryV1, DemandSetV1,
    DemandStatus, DemandV1, DemandWaiverV1, EventType, EvidenceSatisfactionV1, EvidenceV1,
    FindingDispositionPosition, FindingDispositionV1, FindingGroupingAction, FindingGroupingV1,
    FindingReport, FindingResolutionOutcome, FindingResolutionV1, FindingSetEntryV1, FindingSetV1,
    FixVerificationV1, Location, MissingNodeV2, NodeInvocationPayloadV1,
    NodeOutputReceiptPayloadV1, PatchProposal, PathRenameV1, PolicyTimeV1, PortArtifactsV1,
    PortCardinality, Producer, ProviderOperationStateV1, ProviderOperationTransitionPayloadV1,
    ResolutionChallengeKind, ResolutionChallengeV1, ReviewerPackageV1, RunEvent,
    RunFailureReasonV2, RunFailureReasonV3, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV2,
    RunReportPayloadV3, RunSuppressionReasonV2, RunVerdictV2, RunVerdictV3, SnapshotAffinity,
    SourceSnapshot, SubjectKind, SubjectV1,
    finding::{ClaimTargetKind, Relation, RelationKind, RelationTarget},
    snapshot::{Capture, DirtyBoundary, Submodule, Vcs},
};
use serde_json::{Value, json};

const SCHEMAS: [&str; 32] = [
    "artifact-envelope-v1.json",
    "campaign-manifest-v1.json",
    "campaign-opened-v1.json",
    "change-attestation-v1.json",
    "change-set-v1.json",
    "demand-set-v1.json",
    "demand-v1.json",
    "demand-waiver-v1.json",
    "evidence-satisfaction-v1.json",
    "evidence-v1.json",
    "finding-disposition-v1.json",
    "finding-grouping-v1.json",
    "finding-report-v1.json",
    "finding-resolution-v1.json",
    "finding-set-v1.json",
    "fix-verification-v1.json",
    "node-invocation-v1.json",
    "node-output-receipt-v1.json",
    "patch-proposal-v1.json",
    "policy-time-v1.json",
    "provider-operation-transition-v1.json",
    "reviewer-package-v1.json",
    "reviewer-result-v1.json",
    "reviewer-result-v2.json",
    "resolution-challenge-v1.json",
    "round-input-superseded-v1.json",
    "round-started-v1.json",
    "run-event-v1.json",
    "run-report-v2.json",
    "run-report-v3.json",
    "source-snapshot-v1.json",
    "subject-v1.json",
];

fn workspace_root() -> PathBuf {
    std::env::var_os("AFACTORY_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn schema(name: &str) -> Value {
    let path = workspace_root().join("schemas").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn validator(name: &str) -> jsonschema::Validator {
    let finding_report = jsonschema::Resource::from_contents(schema("finding-report-v1.json"))
        .expect("FindingReport@1 is a schema resource");
    let reviewer_result = jsonschema::Resource::from_contents(schema("reviewer-result-v1.json"))
        .expect("ReviewerResult@1 is a schema resource");
    jsonschema::options()
        .with_resource("urn:review-kernel:schema:finding-report:1", finding_report)
        .with_resource(
            "urn:review-kernel:schema:reviewer-result:1",
            reviewer_result,
        )
        .build(&schema(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
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
fn reviewer_result_schema_names_the_live_flat_report_shape() {
    let result = |report| {
        json!({
            "verdict": "request-changes",
            "summary": null,
            "reports": [report],
            "benchmark_demands": [],
            "disputes": [],
        })
    };
    assert_valid(
        "reviewer-result-v1.json",
        &result(json!({
            "severity": "major",
            "file": "src/a.rs",
            "line": 1,
            "title": "legacy",
            "body": "body",
            "fix": "fix",
            "confidence": 0.9
        })),
    );
    assert_invalid(
        "reviewer-result-v1.json",
        &result(json!({
            "title": "typed",
            "severity": "major",
            "locations": [{"path": "src/a.rs"}],
            "body": "body",
            "fix": "fix",
            "confidence": 0.9
        })),
        "typed FindingReport artifacts are produced only after ingestion",
    );
    assert_invalid(
        "reviewer-result-v1.json",
        &result(json!({"title": "no shape discriminator"})),
        "a report must use the live flat shape",
    );
    assert_invalid(
        "reviewer-result-v1.json",
        &result(json!({"file": "src/a.rs", "locations": []})),
        "a report cannot mix wire and durable shapes",
    );
}

#[test]
fn reviewer_result_legacy_conformance_corpus_matches_schema() {
    let path = workspace_root().join("schemas/reviewer-result-v1-conformance.json");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for case in corpus["valid"].as_array().unwrap() {
        assert_valid("reviewer-result-v1.json", &case["payload"]);
    }
    for case in corpus["invalid"].as_array().unwrap() {
        assert_invalid(
            "reviewer-result-v1.json",
            &case["payload"],
            case["name"].as_str().unwrap(),
        );
    }
}

#[test]
fn reviewer_result_v2_names_explicit_dispositions() {
    let value = json!({
        "verdict": "approve",
        "summary": null,
        "reports": [],
        "benchmark_demands": [],
        "dispositions": [{
            "finding_id": "finding:one",
            "position": "not_reproduced",
            "reason": "the guarded branch no longer reaches the failing call"
        }]
    });
    assert_valid("reviewer-result-v2.json", &value);
    review_core::validate_reviewer_result_v2(&value).unwrap();

    let mut omitted = value.clone();
    omitted.as_object_mut().unwrap().remove("dispositions");
    assert_invalid(
        "reviewer-result-v2.json",
        &omitted,
        "silence cannot stand in for explicit coverage",
    );
    assert!(review_core::validate_reviewer_result_v2(&omitted).is_err());
}

#[test]
fn finding_disposition_roundtrips() {
    let disposition = FindingDispositionV1 {
        finding_id: "finding:one".into(),
        source: "correctness".into(),
        position: FindingDispositionPosition::Dispute,
        reason: "the claimed branch is unreachable".into(),
        round: 2,
        subject_id: format!("sha256:{}", "a".repeat(64)),
    };
    disposition.validate().unwrap();
    let value = serde_json::to_value(&disposition).unwrap();
    assert_valid("finding-disposition-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<FindingDispositionV1>(value).unwrap(),
        disposition
    );
}

#[test]
fn finding_grouping_roundtrips() {
    let grouping = FindingGroupingV1 {
        from: "finding:duplicate".into(),
        into: "finding:canonical".into(),
        action: FindingGroupingAction::Group,
        round: 3,
    };
    grouping.validate().unwrap();
    let value = serde_json::to_value(&grouping).unwrap();
    assert_valid("finding-grouping-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<FindingGroupingV1>(value).unwrap(),
        grouping
    );
}

#[test]
fn demand_evidence_and_exact_set_roundtrip() {
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let demand = DemandV1 {
        demand_id: digest('a'),
        claim: "the index remains linear".into(),
        why: "a regression would dominate large reviews".into(),
        suggested_method: "measure 10k and 20k inputs".into(),
        source: "performance".into(),
        requirement: DemandRequirement::Required,
        round: 2,
        subject_id: digest('b'),
    };
    demand.validate().unwrap();
    assert_valid("demand-v1.json", &serde_json::to_value(&demand).unwrap());
    let evidence = EvidenceV1 {
        demand_id: demand.demand_id.clone(),
        subject_id: demand.subject_id.clone(),
        content_artifact_id: digest('c'),
        actor: "operator".into(),
    };
    evidence.validate().unwrap();
    assert_valid(
        "evidence-v1.json",
        &serde_json::to_value(&evidence).unwrap(),
    );
    let satisfaction = EvidenceSatisfactionV1 {
        demand_id: demand.demand_id.clone(),
        evidence_id: digest('d'),
        subject_id: demand.subject_id.clone(),
        policy_revision: "bench-policy@1".into(),
        reason: "the required scaling envelope passed".into(),
    };
    satisfaction.validate().unwrap();
    assert_valid(
        "evidence-satisfaction-v1.json",
        &serde_json::to_value(&satisfaction).unwrap(),
    );
    let waiver = DemandWaiverV1 {
        demand_id: demand.demand_id.clone(),
        subject_id: demand.subject_id.clone(),
        actor: "operator".into(),
        policy_revision: "bench-policy@1".into(),
        reason: "the affected feature is disabled".into(),
    };
    waiver.validate().unwrap();
    assert_valid(
        "demand-waiver-v1.json",
        &serde_json::to_value(&waiver).unwrap(),
    );
    let set = DemandSetV1 {
        subject_id: demand.subject_id.clone(),
        round: 2,
        prior_demand_set_id: digest('e'),
        reducer_version: DEMAND_REDUCER_VERSION.into(),
        selected_demand_artifact_ids: vec![digest('f')],
        satisfaction_artifact_ids: vec![digest('1')],
        waiver_artifact_ids: vec![],
        demands: vec![DemandSetEntryV1 {
            demand_id: demand.demand_id,
            claim: demand.claim,
            why: demand.why,
            suggested_method: demand.suggested_method,
            source: demand.source,
            requirement: demand.requirement,
            status: DemandStatus::Satisfied,
            subject_id: demand.subject_id,
            evidence_ids: vec![digest('d')],
            satisfaction_ids: vec![digest('1')],
            waiver_ids: vec![],
        }],
    };
    set.validate().unwrap();
    assert_valid("demand-set-v1.json", &serde_json::to_value(set).unwrap());
}

#[test]
fn resolution_authority_roundtrips() {
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let attestation = ChangeAttestationV1 {
        finding_id: "finding:one".into(),
        expected_finding_view_id: digest('a'),
        subject_id: digest('b'),
        change_set_id: digest('c'),
        changed_regions: vec![ChangedRegionV1 {
            path: "src/lib.rs".into(),
            start_line: Some(10),
            end_line: Some(14),
        }],
        actor: "implementer".into(),
        reason: "guarded the failing path".into(),
        evidence_ids: vec![digest('d')],
    };
    attestation.validate().unwrap();
    assert_valid(
        "change-attestation-v1.json",
        &serde_json::to_value(&attestation).unwrap(),
    );
    let verification = FixVerificationV1 {
        finding_id: attestation.finding_id.clone(),
        attestation_id: digest('e'),
        expected_finding_view_id: digest('f'),
        subject_id: attestation.subject_id.clone(),
        verifier: "trusted-verifier".into(),
        policy_revision: "fix-policy@1".into(),
        positive: true,
        reason: "all active claims and required checks pass".into(),
        evidence_ids: vec![],
    };
    verification.validate().unwrap();
    assert_valid(
        "fix-verification-v1.json",
        &serde_json::to_value(&verification).unwrap(),
    );
    let resolution = FindingResolutionV1 {
        finding_id: attestation.finding_id.clone(),
        expected_finding_view_id: digest('1'),
        subject_id: attestation.subject_id.clone(),
        outcome: FindingResolutionOutcome::Fixed,
        actor: "trusted-verifier".into(),
        policy_revision: "fix-policy@1".into(),
        reason: "positive verification covers the current view".into(),
        evidence_ids: vec![],
        verification_id: Some(digest('2')),
        max_accepted_severity: None,
        tracking_reference: None,
        expires_at_policy_time: None,
    };
    resolution.validate().unwrap();
    assert_valid(
        "finding-resolution-v1.json",
        &serde_json::to_value(&resolution).unwrap(),
    );
    let challenge = ResolutionChallengeV1 {
        finding_id: resolution.finding_id,
        resolution_id: digest('3'),
        subject_id: resolution.subject_id,
        kind: ResolutionChallengeKind::NewEvidence,
        actor: "operator".into(),
        reason: "new reproduction evidence changes the claim view".into(),
        evidence_ids: vec![digest('4')],
    };
    challenge.validate().unwrap();
    assert_valid(
        "resolution-challenge-v1.json",
        &serde_json::to_value(challenge).unwrap(),
    );
    let time = PolicyTimeV1 {
        tick: 7,
        actor: "policy".into(),
        reason: "evaluate tracked resolution expiry".into(),
    };
    time.validate().unwrap();
    assert_valid("policy-time-v1.json", &serde_json::to_value(time).unwrap());
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
fn finding_report_semantic_conformance_corpus_matches_schema_and_reader() {
    let path = workspace_root().join("schemas/finding-report-v1-conformance.json");
    let corpus: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for case in corpus["valid"].as_array().unwrap() {
        assert_valid("finding-report-v1.json", &case["payload"]);
        let report: FindingReport = serde_json::from_value(case["payload"].clone()).unwrap();
        assert!(report.validate().is_ok(), "{}", case["name"]);
    }
    for case in corpus["invalid"].as_array().unwrap() {
        assert_invalid(
            "finding-report-v1.json",
            &case["payload"],
            case["name"].as_str().unwrap(),
        );
        let refused = serde_json::from_value::<FindingReport>(case["payload"].clone())
            .map_or(true, |report| report.validate().is_err());
        assert!(refused, "{}", case["name"]);
    }
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
        let source_revision =
            matches!(&capture, Capture::Committed { .. }).then(|| "bba24cb".to_string());
        let snapshot = SourceSnapshot {
            repository_id: "example-org/project-hub".into(),
            vcs: Vcs::Git,
            capture,
            content_digest: digest.clone(),
            parent_snapshot_id: None,
            source_revision,
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

    let synthetic_with_revision = json!({
        "repository_id": "r",
        "vcs": "git",
        "capture": {
            "kind": "synthetic_worktree",
            "tree_id": "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "boundary": "revalidated"
        },
        "content_digest": digest,
        "source_revision": "HEAD"
    });
    assert_invalid(
        "source-snapshot-v1.json",
        &synthetic_with_revision,
        "synthetic content cannot claim a committed source revision",
    );
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
fn subject_and_campaign_authority_roundtrip() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let subject = SubjectV1::whole_tree(&digest);
    subject.validate().unwrap();
    let value = serde_json::to_value(&subject).unwrap();
    assert_valid("subject-v1.json", &value);
    assert_eq!(serde_json::from_value::<SubjectV1>(value).unwrap(), subject);

    let package = ReviewerPackageV1 {
        name: "architecture".into(),
        version: "1.0.0".into(),
        digest: digest.clone(),
        files: std::collections::BTreeMap::from([("reviewer.toml".into(), digest.clone())]),
    };
    package.validate().unwrap();
    let value = serde_json::to_value(&package).unwrap();
    assert_valid("reviewer-package-v1.json", &value);

    let manifest = CampaignManifestV1 {
        authority_snapshot_id: digest.clone(),
        subject_kind: SubjectKind::WholeTree,
        base_snapshot_id: None,
        pipeline: AuthorityFileV1 {
            path: ".review/pipelines/heavy.toml".into(),
            artifact_id: digest.clone(),
        },
        reviewer_lock: AuthorityFileV1 {
            path: ".review/review.lock".into(),
            artifact_id: digest.clone(),
        },
        reviewers: vec![],
        execution_policy_ids: vec![digest.clone()],
        project_policy_ids: vec![],
        convergence: CampaignConvergenceV1 {
            clean_rounds: 1,
            max_rounds: 3,
            gate: "major".into(),
        },
        reviewer_timeout_seconds: 1800,
        check_timeout_seconds: Some(3600),
        git_timeout_seconds: Some(300),
        budgets: None,
        focus: Some("authority bootstrap".into()),
        finding_identity_policy: "legacy-path-title@1".into(),
        finding_genesis_id: digest.clone(),
        demand_genesis_id: digest.clone(),
    };
    manifest.validate().unwrap();
    let mut unknown_policy = manifest.clone();
    unknown_policy.finding_identity_policy = "future-policy@9".into();
    assert!(
        unknown_policy
            .validate()
            .unwrap_err()
            .contains("unknown finding identity policy")
    );
    assert_invalid(
        "campaign-manifest-v1.json",
        &serde_json::to_value(&unknown_policy).unwrap(),
        "unknown finding identity policy",
    );
    let value = serde_json::to_value(&manifest).unwrap();
    assert_valid("campaign-manifest-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<CampaignManifestV1>(value).unwrap(),
        manifest
    );
}

#[test]
fn change_set_roundtrips_with_exact_patch_bytes() {
    let base = format!("sha256:{}", "a".repeat(64));
    let head = format!("sha256:{}", "b".repeat(64));
    let change_set = ChangeSetV1::new(
        base,
        head,
        vec!["src/new.rs".into(), "src/old.rs".into()],
        vec![PathRenameV1 {
            old_path: "src/old.rs".into(),
            new_path: "src/new.rs".into(),
            similarity: 100,
        }],
        b"diff --git a/src/old.rs b/src/new.rs\n\0\xff",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    change_set.validate().unwrap();
    assert!(change_set.contains_report_path("src/old.rs"));
    assert!(!change_set.contains_report_path("src/untouched.rs"));
    assert_eq!(
        change_set.canonical_patch().unwrap(),
        b"diff --git a/src/old.rs b/src/new.rs\n\0\xff"
    );
    let value = serde_json::to_value(&change_set).unwrap();
    assert_valid("change-set-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<ChangeSetV1>(value).unwrap(),
        change_set
    );
}

#[test]
fn change_set_semantic_conformance_corpus_matches_the_permanent_reader() {
    let path = workspace_root().join("schemas/change-set-v1-conformance.json");
    let corpus: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for case in corpus["valid"].as_array().unwrap() {
        let value: ChangeSetV1 = serde_json::from_value(case["payload"].clone()).unwrap();
        assert!(value.validate().is_ok(), "{}", case["name"]);
    }
    for case in corpus["invalid"].as_array().unwrap() {
        let value: ChangeSetV1 = serde_json::from_value(case["payload"].clone()).unwrap();
        assert!(value.validate().is_err(), "{}", case["name"]);
    }
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
fn bootstrap_event_payloads_are_semantically_validated() {
    let digest = format!("sha256:{}", "b".repeat(64));
    let opened = CampaignOpenedPayloadV1 {
        campaign_manifest_id: digest.clone(),
        authority_snapshot_id: digest,
    };
    assert!(
        review_core::event::validate_event_payload(
            EventType::CampaignOpenedV1,
            &serde_json::to_value(opened).unwrap(),
        )
        .is_ok()
    );
    assert!(review_core::event::validate_event_payload(
        EventType::RoundStartedV1,
        &json!({"round":0,"epoch":1,"campaign_manifest_id":"x","subject_id":"x","prior_finding_set_id":"x","prior_demand_set_id":"x"}),
    )
    .is_err());
}

#[test]
fn provider_operation_payload_is_closed_and_schema_valid() {
    let payload = ProviderOperationTransitionPayloadV1 {
        operation_id: "a".repeat(26),
        provider_id: "claude-work".into(),
        capability_id: format!("sha256:{}", "b".repeat(64)),
        node_id: "architecture".into(),
        round: 1,
        round_epoch: 1,
        operation_epoch: 1,
        state: ProviderOperationStateV1::Running,
        attempt: Some(1),
        attempt_id: Some("c".repeat(26)),
        failure_class: None,
        failure_fingerprint: None,
        continuation_handle: None,
        reserved_tokens: 4096,
        charged_tokens: 0,
        elapsed_ms: 0,
        retry_permitted: false,
        circuit_open: false,
        next_action: None,
    };
    let value = serde_json::to_value(payload).unwrap();
    assert_valid("provider-operation-transition-v1.json", &value);
    let mut secret = value;
    secret["oauth_code"] = json!("must-never-be-stored");
    assert_invalid(
        "provider-operation-transition-v1.json",
        &secret,
        "secret-bearing fields must be rejected",
    );
}

#[test]
fn provider_operation_continuation_is_exact_and_secret_free() {
    let running = ProviderOperationTransitionPayloadV1 {
        operation_id: "a".repeat(26),
        provider_id: "claude-work".into(),
        capability_id: format!("sha256:{}", "b".repeat(64)),
        node_id: "architecture".into(),
        round: 1,
        round_epoch: 1,
        operation_epoch: 1,
        state: ProviderOperationStateV1::Running,
        attempt: Some(1),
        attempt_id: Some("c".repeat(26)),
        failure_class: None,
        failure_fingerprint: None,
        continuation_handle: None,
        reserved_tokens: 4096,
        charged_tokens: 0,
        elapsed_ms: 0,
        retry_permitted: false,
        circuit_open: false,
        next_action: None,
    };
    let mut waiting = running.clone();
    waiting.state = ProviderOperationStateV1::WaitingForHuman;
    waiting.failure_class =
        Some(review_core::ProviderFailureClassV1::InvalidOrExpiredAuthentication);
    waiting.failure_fingerprint = Some(format!("sha256:{}", "d".repeat(64)));
    waiting.continuation_handle = Some("e".repeat(26));
    waiting.charged_tokens = 4096;
    waiting.elapsed_ms = 12;
    waiting.retry_permitted = true;
    waiting.next_action = Some(review_core::ProviderNextActionV1::CompleteInteractiveLogin);
    waiting.validate_after(Some(&running)).unwrap();

    let mut resumed = waiting.clone();
    resumed.state = ProviderOperationStateV1::Resumed;
    resumed.operation_epoch = 1;
    resumed.attempt = Some(2);
    resumed.attempt_id = Some("f".repeat(26));
    resumed.failure_class = None;
    resumed.failure_fingerprint = None;
    resumed.reserved_tokens = 0;
    resumed.charged_tokens = 0;
    resumed.elapsed_ms = 0;
    resumed.retry_permitted = false;
    resumed.next_action = None;
    assert!(resumed.validate_after(Some(&waiting)).is_err());
    resumed.operation_epoch = 2;
    resumed.validate_after(Some(&waiting)).unwrap();

    let mut resumed_running = resumed.clone();
    resumed_running.state = ProviderOperationStateV1::Running;
    resumed_running.continuation_handle = None;
    resumed_running.reserved_tokens = 4096;
    resumed_running.validate_after(Some(&resumed)).unwrap();

    let mut done = resumed_running.clone();
    done.state = ProviderOperationStateV1::Done;
    done.charged_tokens = 7;
    done.elapsed_ms = 4;
    done.validate_after(Some(&resumed_running)).unwrap();

    let mut transient = running.clone();
    transient.failure_class = Some(review_core::ProviderFailureClassV1::TransientTransportFailure);
    transient.failure_fingerprint = Some(format!("sha256:{}", "1".repeat(64)));
    transient.charged_tokens = 5;
    transient.retry_permitted = true;
    transient.validate_after(Some(&running)).unwrap();
    let mut automatic_retry = running.clone();
    automatic_retry.attempt = Some(2);
    automatic_retry.attempt_id = Some("2".repeat(26));
    automatic_retry.validate_after(Some(&transient)).unwrap();

    let persisted = serde_json::to_string(&[
        running,
        waiting,
        resumed,
        resumed_running,
        done,
        transient,
        automatic_retry,
    ])
    .unwrap();
    assert!(!persisted.contains("oauth-code-value"));
    assert!(!persisted.contains("access-token-value"));
}

#[test]
fn run_reports_are_structural_and_every_report_version_remains_readable() {
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
        payload: json!({
            "outcomes": [{"node":"review", "status":"completed", "detail":{}}],
            "blocked_gates": [],
            "verdict": "Fail(NotConverged)",
            "spent_tokens": null
        }),
    };
    assert_eq!(
        review_core::run_report_closes_round(&event).unwrap(),
        Some(true)
    );
    event.payload = json!({
        "outcomes": [{"node":"review", "status":"failed", "detail":"crashed"}],
        "blocked_gates": [],
        "verdict": "Incomplete { missing: [(\"review\", \"crashed\")] }",
        "spent_tokens": 7
    });
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
            outcome: RunNodeOutcomeV2::Failed {
                error: "run budget exhausted".into(),
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

    let report_v3 = RunReportPayloadV3 {
        outcomes: vec![RunNodeReportV2 {
            node: "review".into(),
            outcome: RunNodeOutcomeV2::Completed {
                output_artifacts: vec![],
            },
        }],
        blocked_gates: vec![],
        verdict: RunVerdictV3::Fail {
            reason: RunFailureReasonV3::AuthorityUnavailable,
        },
        spent_tokens: Some(43),
    };
    let value = serde_json::to_value(&report_v3).unwrap();
    assert_valid("run-report-v3.json", &value);
    assert_eq!(
        serde_json::from_value::<RunReportPayloadV3>(value.clone()).unwrap(),
        report_v3
    );
    event.event_type = EventType::RunReportV3;
    event.payload = value;
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

    let invalid_port = json!({
        "node": "reviewer",
        "inputs": [{
            "port": "subject",
            "type": "review.kernel/SourceSnapshot@1",
            "cardinality": "one",
            "optional": false,
            "snapshot_affinity": "same_subject",
            "artifact_ids": ["not-a-digest", "not-a-digest"]
        }]
    });
    assert!(
        review_core::event::validate_event_payload(EventType::NodeInvocationV1, &invalid_port)
            .is_err()
    );
    let invalid_receipt = json!({"node":"reviewer", "outputs":invalid_port["inputs"]});
    assert!(
        review_core::event::validate_event_payload(
            EventType::NodeOutputReceiptV1,
            &invalid_receipt
        )
        .is_err()
    );
}

#[test]
fn event_validation_rejects_semantically_malformed_run_reports() {
    let contradictory_legacy = json!({
        "outcomes": [{"node":"reviewer", "status":"failed", "detail":"crashed"}],
        "blocked_gates": [],
        "verdict": "Pass",
        "spent_tokens": null
    });
    assert!(
        review_core::event::validate_event_payload(EventType::RunReportV1, &contradictory_legacy)
            .is_err()
    );

    let empty_reason = json!({
        "outcomes": [{"node":"reviewer", "outcome":{"kind":"failed", "error":"x"}}],
        "blocked_gates": [],
        "verdict": {"kind":"incomplete", "missing_nodes":[{"node":"reviewer", "reason":""}]}
    });
    assert!(
        review_core::event::validate_event_payload(EventType::RunReportV2, &empty_reason).is_err()
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

#[test]
fn finding_set_roundtrips_as_an_exact_reducer_projection() {
    let digest = format!("sha256:{}", "d".repeat(64));
    let set = FindingSetV1 {
        subject_id: digest.clone(),
        round: 1,
        prior_finding_set_id: digest.clone(),
        reducer_version: review_core::FINDING_REDUCER_VERSION.into(),
        identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
        selected_report_ids: vec![digest.clone()],
        relation_ids: Vec::new(),
        resolution_ids: Vec::new(),
        findings: vec![FindingSetEntryV1 {
            finding_id: digest.clone(),
            status: "open".into(),
            severity: review_core::Severity::Major,
            effective_severity: Some(review_core::Severity::Major),
            scope: "in".into(),
            file: Some("src/lib.rs".into()),
            line: Some(7),
            location_unrecorded: false,
            title: "claim".into(),
            body: "body".into(),
            fix: Some("fix".into()),
            confidence: Some(0.9),
            source: "correctness".into(),
            last_seen_round: 1,
            report_ids: vec![digest],
        }],
    };
    set.validate().unwrap();
    let value = serde_json::to_value(&set).unwrap();
    assert_valid("finding-set-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<FindingSetV1>(value.clone()).unwrap(),
        set
    );
    let mut missing_effective_severity = value;
    missing_effective_severity["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("effective_severity");
    assert!(serde_json::from_value::<FindingSetV1>(missing_effective_severity.clone()).is_err());
    assert_invalid(
        "finding-set-v1.json",
        &missing_effective_severity,
        "effective severity is required",
    );

    let mut out_of_scope = set.clone();
    out_of_scope.findings[0].scope = "out".into();
    out_of_scope.findings[0].effective_severity = None;
    let out_of_scope_value = serde_json::to_value(&out_of_scope).unwrap();
    assert!(out_of_scope_value["findings"][0]["effective_severity"].is_null());
    assert_valid("finding-set-v1.json", &out_of_scope_value);
    assert_eq!(
        serde_json::from_value::<FindingSetV1>(out_of_scope_value).unwrap(),
        out_of_scope
    );

    let mut empty_file = set.clone();
    empty_file.findings[0].file = Some(String::new());
    assert!(empty_file.validate().is_err());

    for invalid in [
        {
            let mut invalid = set.clone();
            invalid.findings[0].status = "triaged".into();
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].scope = "maybe".into();
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].line = Some(0);
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].file = Some("../../etc/passwd".into());
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].file = None;
            invalid.findings[0].line = Some(1);
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].location_unrecorded = true;
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].confidence = Some(1.1);
            invalid
        },
        {
            let mut invalid = set.clone();
            invalid.findings[0].fix = Some(String::new());
            invalid
        },
    ] {
        assert!(invalid.validate().is_err());
        assert_invalid(
            "finding-set-v1.json",
            &serde_json::to_value(invalid).unwrap(),
            "invalid Finding projection",
        );
    }
}
