//! The schemas and the Rust types must not drift.
//!
//! Each contract is checked in both directions: a fully-populated Rust value must satisfy the
//! schema, and an instance the schema should reject must actually be rejected. A schema that
//! accepts everything passes the first check alone, which is why the negative cases are here.

use std::path::PathBuf;

use review_core::{
    ArtifactEnvelope, AuthorityFileV1, CacheManifestEntryV1, CacheManifestV1, CachePathEncodingV1,
    CampaignConvergenceV1, CampaignManifestV1, CampaignOpenedPayloadV1, ChangeAttestationV1,
    ChangeSetV1, ChangedRegionV1, ClaimRef, ClaimRefKind, CloseoutPolicyV1, DEMAND_REDUCER_VERSION,
    DemandRequirement, DemandSetEntryV1, DemandSetV1, DemandStatus, DemandV1, DemandWaiverV1,
    EventType, EvidenceReuseAdmissionV1, EvidenceSatisfactionV1, EvidenceV1,
    FindingDispositionPosition, FindingDispositionV1, FindingGroupingAction, FindingGroupingV1,
    FindingReport, FindingResolutionOutcome, FindingResolutionV1, FindingSetEntryV1, FindingSetV1,
    FixVerificationV1, IntegrationCandidateV1, IntegrationCheckV1, IntegrationChecksV1,
    IntegrationPlanV1, Location, MissingNodeV2, NodeInvocationPayloadV1,
    NodeOutputReceiptPayloadV1, PatchProposal, PathRenameV1, PolicyTimeV1, PortArtifactsV1,
    PortCardinality, Producer, ResolutionChallengeKind, ResolutionChallengeV1, ReviewSliceV1,
    ReviewerPackageV1, RunCacheFailureReasonV5, RunCacheFailureV5, RunCacheKindV5,
    RunCacheMaterializationV5, RunCacheSnapshotV5, RunEvent, RunExecutionBindingV4,
    RunExecutionProviderV4, RunFailureReasonV3, RunIsolationV4, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportExecutionV6, RunReportPayloadV6, RunSandboxModeV4, RunSuppressionReasonV2,
    RunVerdictV3, SemanticClosureV1, SemanticDispositionV1, ShardOutcomeV1, ShardReceiptV1,
    ShardSetV1, SliceCoverageV1, SliceSetV1, SnapshotAffinity, SourceSnapshot, SubjectKind,
    SubjectV1, TaskReviewAccountingV1,
    finding::{ClaimTargetKind, Relation, RelationKind, RelationTarget},
    snapshot::{Capture, DirtyBoundary, Submodule, Vcs},
};
use serde_json::{Value, json};

const SCHEMAS: [&str; 183] = [
    "session-snapshot-v1.json",
    "build-cache-v1.json",
    "worker-notes-v1.json",
    "head-delta-v1.json",
    "warm-set-v1.json",
    "optimization-recipe-catalog-v1.json",
    "optimization-profile-v1.json",
    "optimization-writable-configuration-v1.json",
    "optimization-execution-configuration-v1.json",
    "optimization-diagnostic-v1.json",
    "optimization-proposal-v1.json",
    "optimization-result-v1.json",
    "optimization-adoption-receipt-v1.json",
    "optimization-adoption-observation-v1.json",
    "optimization-adoption-task-evidence-v1.json",
    "optimization-evaluation-v1.json",
    "experimental-slot-v2.json",
    "task-inspection-v10.json",
    "task-inspection-v11.json",
    "task-execution-record-v5.json",
    "experiment-execution-plan-v1.json",
    "task-runtime-evidence-v1.json",
    "task-inspection-v9.json",
    "optimization-sources-v1.json",
    "optimization-history-v1.json",
    "optimization-economics-v1.json",
    "optimization-report-v1.json",
    "optimization-policy-v1.json",
    "optimization-harness-v1.json",
    "experiment-specification-v1.json",
    "experiment-prepared-v1.json",
    "experiment-plan-decision-v1.json",
    "experiment-comparison-v1.json",
    "optimization-verification-v1.json",
    "optimization-package-repin-v1.json",
    "harness-materialization-v1.json",
    "task-inspection-v8.json",
    "provider-doctor-v2.json",
    "review-outcome-v2.json",
    "review-outcome-v3.json",
    "review-report-v4.json",
    "task-context-v1.json",
    "task-builtin-context-v1.json",
    "task-provider-context-v1.json",
    "document-context-v1.json",
    "task-inspection-v7.json",
    "task-plan-inspection-v1.json",
    "task-inspection-v6.json",
    "task-inspection-v5.json",
    "task-provider-context-v2.json",
    "task-broker-binding-v1.json",
    "task-broker-operation-v1.json",
    "task-broker-transition-v1.json",
    "task-file-v1.json",
    "task-catalog-v1.json",
    "task-catalog-v2.json",
    "compiled-task-v1.json",
    "task-inspection-v3.json",
    "task-inspection-v4.json",
    "task-list-entry-v2.json",
    "task-execution-record-v3.json",
    "task-execution-record-v4.json",
    "task-owned-child-set-v1.json",
    "task-token-usage-v2.json",
    "task-token-usage-v3.json",
    "task-usage-observation-v1.json",
    "broker-operation-receipt-v2.json",
    "task-review-attempt-provenance-v1.json",
    "task-review-attempt-provenance-v2.json",
    "task-review-accounting-v1.json",
    "run-report-v6.json",
    "normalized-task-requirements-v1.json",
    "task-source-capture-v1.json",
    "issue-input-v1.json",
    "document-sources-v1.json",
    "document-draft-v1.json",
    "document-v1.json",
    "document-check-receipt-v1.json",
    "document-evaluation-v1.json",
    "document-verification-v1.json",
    "document-task-policy-v1.json",
    "catalog-contract-fixtures-v1.json",
    "shared-task-catalog-v1.json",
    "task-kind-v1.json",
    "planning-request-v1.json",
    "task-planner-settings-v1.json",
    "task-operator-signature-v1.json",
    "pipeline-proposal-v1.json",
    "task-provider-admission-v1.json",
    "task-provider-admission-v2.json",
    "task-provider-probe-policy-v1.json",
    "task-review-subject-v2.json",
    "task-review-assignment-v1.json",
    "task-review-round-v1.json",
    "task-review-result-metadata-v1.json",
    "task-review-context-v1.json",
    "task-review-gate-facts-v1.json",
    "legacy-review-round-v1.json",
    "legacy-review-dependency-v1.json",
    "legacy-review-invocation-policy-v1.json",
    "legacy-review-gate-outcome-v1.json",
    "task-review-result-selected-v1.json",
    "task-check-receipt-v1.json",
    "task-evaluation-v1.json",
    "verification-result-v1.json",
    "reviewed-implementation-v1.json",
    "repair-allowed-implementation-v1.json",
    "task-repair-context-v1.json",
    "task-fix-verification-v1.json",
    "task-fix-receipt-v1.json",
    "task-review-claims-v1.json",
    "task-review-continuation-v1.json",
    "task-snapshot-v1.json",
    "source-tree-v1.json",
    "candidate-tree-v1.json",
    "task-worker-reply-v1.json",
    "task-worker-request-v1.json",
    "task-retry-feedback-v1.json",
    "artifact-envelope-v1.json",
    "task-contracts-v1.json",
    "task-transition-v1.json",
    "task-transition-v2.json",
    "task-transition-v3.json",
    "task-transition-v4.json",
    "task-transition-v5.json",
    "task-review-check-sequence-policy-v1.json",
    "task-review-integration-phase-v1.json",
    "task-run-report-v2.json",
    "task-review-handoff-v2.json",
    "legacy-review-task-policy-v4.json",
    "task-review-handoff-v1.json",
    "task-delivery-record-v1.json",
    "task-invocation-v1.json",
    "task-output-v1.json",
    "task-run-report-v1.json",
    "task-diagnostic-v1.json",
    "task-execution-record-v1.json",
    "task-token-usage-v1.json",
    "task-revision-v1.json",
    "task-result-v1.json",
    "task-phase-v1.json",
    "pipeline-definition-v1.json",
    "execution-plan-v1.json",
    "plan-decision-v1.json",
    "review-history-v1.json",
    "verification-continuation-v1.json",
    "repair-assessment-v1.json",
    "cache-manifest-v1.json",
    "campaign-manifest-v1.json",
    "campaign-opened-v1.json",
    "change-attestation-v1.json",
    "change-set-v1.json",
    "demand-set-v1.json",
    "demand-v1.json",
    "demand-waiver-v1.json",
    "evidence-reuse-admission-v1.json",
    "evidence-satisfaction-v1.json",
    "evidence-v1.json",
    "finding-disposition-v1.json",
    "finding-grouping-v1.json",
    "finding-report-v1.json",
    "finding-resolution-v1.json",
    "finding-set-v1.json",
    "fix-verification-v1.json",
    "integration-checks-v1.json",
    "integration-plan-v1.json",
    "node-invocation-v1.json",
    "node-output-receipt-v1.json",
    "patch-proposal-v1.json",
    "policy-time-v1.json",
    "review-slice-v1.json",
    "reviewer-package-v1.json",
    "reviewer-result-v1.json",
    "reviewer-result-v2.json",
    "resolution-challenge-v1.json",
    "round-input-superseded-v1.json",
    "round-started-v1.json",
    "run-event-v1.json",
    "semantic-closure-v1.json",
    "shard-set-v1.json",
    "slice-set-v1.json",
    "source-snapshot-v1.json",
    "subject-v1.json",
];

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn schema(name: &str) -> Value {
    let path = workspace_root().join("schemas").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn task_invocations_and_attempt_records_are_versioned_and_closed() {
    use review_core::task::execution::*;
    let id = format!("sha256:{}", "1".repeat(64));
    let attempt_id = "a".repeat(26);
    let invocation = TaskInvocationV1 {
        plan_id: id.clone(),
        node: "root.nodes.implement".into(),
        inputs: Default::default(),
    };
    invocation.validate().unwrap();
    assert_valid(
        "task-invocation-v1.json",
        &serde_json::to_value(&invocation).unwrap(),
    );
    let output = TaskOutputV1 {
        invocation_id: id.clone(),
        outputs: Default::default(),
    };
    output.validate().unwrap();
    assert_valid(
        "task-output-v1.json",
        &serde_json::to_value(&output).unwrap(),
    );
    let records = [
        TaskExecutionRecordV1::Invocation {
            invocation_id: id.clone(),
        },
        TaskExecutionRecordV1::Reserved {
            invocation_id: id.clone(),
            attempt_id: attempt_id.clone(),
            reservation_id: "reservation:0".into(),
            reserved_tokens: 10,
            deadline_unix_ms: 1000,
            feedback_ids: vec![id.clone()],
        },
        TaskExecutionRecordV1::ContextBound {
            attempt_id: attempt_id.clone(),
            context_id: id.clone(),
        },
        TaskExecutionRecordV1::Started {
            attempt_id: attempt_id.clone(),
        },
        TaskExecutionRecordV1::Released {
            attempt_id: attempt_id.clone(),
            reason: "not dispatched".into(),
        },
        TaskExecutionRecordV1::Published {
            output_id: id.clone(),
            attempt_id: Some(attempt_id.clone()),
        },
    ];
    for record in records {
        record.validate().unwrap();
        let mut value = serde_json::to_value(record).unwrap();
        assert_valid("task-execution-record-v1.json", &value);
        value["undeclared"] = json!(true);
        assert!(!validator("task-execution-record-v1.json").is_valid(&value));
        assert!(serde_json::from_value::<TaskExecutionRecordV1>(value).is_err());
    }
    // Accounting has only its exact decimal encoding; the v1 wire never carries it.
    let accounting = [
        TaskExecutionRecordV1::Settled {
            attempt_id: attempt_id.clone(),
            charged_tokens: 11,
            result: TaskAttemptResultV1::Succeeded {
                output_id: id.clone(),
            },
            raw_artifact_ids: vec![],
            usage_id: Some(id.clone()),
        },
        TaskExecutionRecordV1::Settled {
            attempt_id: attempt_id.clone(),
            charged_tokens: 11,
            result: TaskAttemptResultV1::Failed {
                diagnostic_id: id.clone(),
                feedback_id: Some(id.clone()),
            },
            raw_artifact_ids: vec![id.clone()],
            usage_id: None,
        },
        TaskExecutionRecordV1::Settled {
            attempt_id: attempt_id.clone(),
            charged_tokens: 10,
            result: TaskAttemptResultV1::Abandoned {
                diagnostic_id: id.clone(),
            },
            raw_artifact_ids: vec![],
            usage_id: None,
        },
        TaskExecutionRecordV1::UsageObserved {
            charged_tokens: 12,
            raw_artifact_ids: vec![],
            usage_id: id,
            attempt_id,
        },
    ];
    for record in accounting {
        assert!(record.validate().is_err());
        assert!(serde_json::to_value(&record).is_err());
        let encoded = TaskExecutionRecordV3::from_accounting(&record).unwrap();
        encoded.validate().unwrap();
        let mut value = serde_json::to_value(&encoded).unwrap();
        assert_valid("task-execution-record-v3.json", &value);
        assert_invalid("task-execution-record-v1.json", &value, "v3 accounting");
        assert!(serde_json::from_value::<TaskExecutionRecordV1>(value.clone()).is_err());
        assert_eq!(encoded.into_record(), record);
        value["undeclared"] = json!(true);
        assert!(!validator("task-execution-record-v3.json").is_valid(&value));
        assert!(serde_json::from_value::<TaskExecutionRecordV3>(value).is_err());
    }
}

#[test]
fn task_retry_feedback_has_only_a_bounded_code_and_exact_attempt_contract() {
    use review_core::task::feedback::*;
    for code in [
        TaskFeedbackCodeV1::InvalidOutputContract,
        TaskFeedbackCodeV1::ProcessFailure,
        TaskFeedbackCodeV1::ProviderFailure,
        TaskFeedbackCodeV1::ContextRejected,
        TaskFeedbackCodeV1::OutputAdmissionRejected,
    ] {
        let feedback = TaskRetryFeedbackV1 {
            attempt_id: "01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            contract_id: format!("sha256:{}", "1".repeat(64)),
            code,
            compiler: None,
        };
        feedback.validate().unwrap();
        let mut value = serde_json::to_value(feedback).unwrap();
        assert_valid("task-retry-feedback-v1.json", &value);
        value["transcript"] = json!("unrelated prior conversation");
        assert!(!validator("task-retry-feedback-v1.json").is_valid(&value));
        assert!(serde_json::from_value::<TaskRetryFeedbackV1>(value).is_err());
    }
}

#[test]
fn task_delivery_contract_binds_the_exact_result_and_local_receipt() {
    use review_core::task::delivery::*;
    let id = format!("sha256:{}", "1".repeat(64));
    for status in [
        TaskDeliveryStatusV1::Prepared,
        TaskDeliveryStatusV1::Delivered,
        TaskDeliveryStatusV1::Failed,
    ] {
        let record = TaskDeliveryRecordV1 {
            task_id: "pagination".into(),
            result_id: id.clone(),
            source_snapshot_id: id.clone(),
            derived_snapshot_id: id.clone(),
            target_id: id.clone(),
            receipt_id: id.clone(),
            status,
        };
        record.validate().unwrap();
        let mut value = serde_json::to_value(record).unwrap();
        assert_valid("task-delivery-record-v1.json", &value);
        value["approved"] = json!(true);
        assert!(!validator("task-delivery-record-v1.json").is_valid(&value));
        assert!(serde_json::from_value::<TaskDeliveryRecordV1>(value).is_err());
    }
}

#[test]
fn task_verification_contracts_preserve_negative_results_and_require_positive_evidence() {
    use review_core::task::pipeline::ReceiptOutcomeV1;
    use review_core::task::verification::*;
    let id = format!("sha256:{}", "1".repeat(64));
    for outcome in [
        ReceiptOutcomeV1::Passed,
        ReceiptOutcomeV1::Failed,
        ReceiptOutcomeV1::Inconclusive,
    ] {
        let check = TaskCheckReceiptV1 {
            plan_id: id.clone(),
            snapshot_id: id.clone(),
            policy_id: id.clone(),
            outcome,
            checks: std::collections::BTreeMap::from([("unit".into(), id.clone())]),
        };
        check.validate().unwrap();
        assert_valid(
            "task-check-receipt-v1.json",
            &serde_json::to_value(check).unwrap(),
        );
        let evaluation = TaskEvaluationV1 {
            outcome,
            reason: "Verified the current source".into(),
        };
        evaluation.validate().unwrap();
        assert_valid(
            "task-evaluation-v1.json",
            &serde_json::to_value(evaluation).unwrap(),
        );
        let result = VerificationResultV1 {
            plan_id: id.clone(),
            snapshot_id: id.clone(),
            policy_id: id.clone(),
            outcome,
            check_receipt_id: id.clone(),
            evaluation_id: (outcome == ReceiptOutcomeV1::Passed).then(|| id.clone()),
        };
        result.validate().unwrap();
        let value = serde_json::to_value(&result).unwrap();
        assert_valid("verification-result-v1.json", &value);
        let reviewed = review_core::task::verification::ReviewedImplementationV1 {
            scope: review_core::task::verification::ImplementationReviewScopeV1::CompleteReview,
            invocation: review_core::task::execution::TaskInvocationV1 {
                plan_id: id.clone(),
                node: "root.nodes.accept".into(),
                inputs: std::collections::BTreeMap::new(),
            },
            snapshot_id: id.clone(),
            policy_id: id.clone(),
            outcome,
        };
        reviewed.validate().unwrap();
        let mut reviewed_value = serde_json::to_value(reviewed).unwrap();
        assert_valid("reviewed-implementation-v1.json", &reviewed_value);
        reviewed_value["approved"] = json!(true);
        assert!(!validator("reviewed-implementation-v1.json").is_valid(&reviewed_value));
        assert!(
            serde_json::from_value::<review_core::task::verification::ReviewedImplementationV1>(
                reviewed_value
            )
            .is_err()
        );
        let mut unknown = value.clone();
        unknown["approved"] = json!(true);
        assert!(!validator("verification-result-v1.json").is_valid(&unknown));
        assert!(serde_json::from_value::<VerificationResultV1>(unknown).is_err());
        let mut null = value;
        null["evaluation_id"] = Value::Null;
        assert!(!validator("verification-result-v1.json").is_valid(&null));
        assert!(serde_json::from_value::<VerificationResultV1>(null).is_err());
    }
    let missing = json!({"plan_id":id,"snapshot_id":id,"policy_id":id,"outcome":"passed","check_receipt_id":id});
    assert!(!validator("verification-result-v1.json").is_valid(&missing));
    assert!(
        serde_json::from_value::<VerificationResultV1>(missing)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn review_task_round_contracts_preserve_completeness() {
    use review_core::task::review::*;
    let id = format!("sha256:{}", "a".repeat(64));
    let subject = json!({"subject_id":id,"snapshot_id":id,"prior_history_id":id,"round":1,"subject":{"kind":"whole-tree","head_snapshot_id":id}});
    assert_valid("task-review-subject-v2.json", &subject);
    serde_json::from_value::<TaskReviewSubjectV2>(subject)
        .unwrap()
        .validate()
        .unwrap();
    for (conclusion, outcome, complete) in [
        ("pass", "passed", true),
        ("changes_requested", "failed", true),
        ("convergence_exhausted", "failed", true),
        ("incomplete", "inconclusive", false),
    ] {
        let mut value = json!({"invocation":{"plan_id":id,"node":"root.nodes.reduce","inputs":{}},"policy_id":id,
            "subject_id":id,"snapshot_id":id,"round":1,"outcome":outcome,"conclusion":conclusion,
            "selected_results":{"correctness":id},"missing_reviewers":[]});
        if complete {
            value["finding_set_id"] = json!(id);
            value["demand_set_id"] = json!(id);
        }
        assert_valid("task-review-round-v1.json", &value);
        serde_json::from_value::<TaskReviewRoundV1>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        let mut changed = value.clone();
        changed["outcome"] = json!(if outcome == "passed" {
            "failed"
        } else {
            "passed"
        });
        assert_invalid(
            "task-review-round-v1.json",
            &changed,
            "conclusion/outcome mismatch",
        );
        assert!(
            serde_json::from_value::<TaskReviewRoundV1>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
        if complete {
            value.as_object_mut().unwrap().remove("finding_set_id");
        } else {
            value["finding_set_id"] = json!(id);
        }
        assert_invalid(
            "task-review-round-v1.json",
            &value,
            "partial sets must not close a Review Round",
        );
        assert!(
            serde_json::from_value::<TaskReviewRoundV1>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn review_continuation_keeps_repair_evidence_distinct_from_closed_rounds() {
    use review_core::task::{repair::TaskReviewContinuationV1, review::TaskReviewSubjectV2};
    let id = format!("sha256:{}", "a".repeat(64));
    let value = json!({"invocation":{"plan_id":id,"node":"root.continue","inputs":{}},
        "original_round_report_id":id,"prior_history_id":id,
        "assessment":{"continuation_id":id,"current_subject_id":id,"current_snapshot_id":id,
            "check_receipt_id":id,"scope":"targeted_fixes","claims":{"finding":{
                "expected_view_id":id,"attestation_id":id,"receipt_id":id,"outcome":"positive"}}}});
    assert_valid("task-review-continuation-v1.json", &value);
    serde_json::from_value::<TaskReviewContinuationV1>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut forged = value;
    forged["assessment"]["scope"] = json!("complete_review");
    assert!(!validator("task-review-continuation-v1.json").is_valid(&forged));
    assert!(serde_json::from_value::<TaskReviewContinuationV1>(forged).is_err());
    let mut subject = json!({"subject_id":id,"snapshot_id":id,"prior_history_id":id,
        "continuation_id":id,"round":2,"subject":{"kind":"whole-tree","head_snapshot_id":id}});
    assert_valid("task-review-subject-v2.json", &subject);
    serde_json::from_value::<TaskReviewSubjectV2>(subject.clone())
        .unwrap()
        .validate()
        .unwrap();
    subject["round"] = json!(1);
    assert!(!validator("task-review-subject-v2.json").is_valid(&subject));
    assert!(
        serde_json::from_value::<TaskReviewSubjectV2>(subject)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn provider_admission_contract_requires_exact_model_capability() {
    use review_core::task::provider::TaskProviderAdmissionV1;
    let id = format!("sha256:{}", "b".repeat(64));
    let value = json!({"plan_id":id,"invocation_policy_id":id,"outcome":"passed","bindings":["root.slots.reviewer"],
        "execution":{"kind":"model","provider":"personal","provider_kind":"claude","principal_id":id,"model":"claude-fixture-1","effort":"high"}});
    assert_valid("task-provider-admission-v1.json", &value);
    serde_json::from_value::<TaskProviderAdmissionV1>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    for (field, replacement) in [
        ("outcome", json!("failed")),
        ("bindings", json!([])),
        ("execution", json!({"kind":"command"})),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        assert_invalid(
            "task-provider-admission-v1.json",
            &changed,
            "invalid capability proof",
        );
        assert!(
            serde_json::from_value::<TaskProviderAdmissionV1>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

fn validator(name: &str) -> &'static jsonschema::Validator {
    type Validators =
        std::collections::BTreeMap<String, std::sync::OnceLock<jsonschema::Validator>>;
    static VALIDATORS: std::sync::OnceLock<Validators> = std::sync::OnceLock::new();
    static RESOURCES: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    let validators = VALIDATORS.get_or_init(|| {
        SCHEMAS
            .into_iter()
            .map(|name| (name.to_owned(), std::sync::OnceLock::new()))
            .collect()
    });
    validators
        .get(name)
        .unwrap_or_else(|| panic!("unregistered schema: {name}"))
        .get_or_init(|| {
            let resources = RESOURCES.get_or_init(|| {
                SCHEMAS
                    .into_iter()
                    .chain([
                        "finding-report-v1.json",
                        "reviewer-result-v1.json",
                        "task-contracts-v1.json",
                        "task-token-usage-v1.json",
                        "task-token-usage-v2.json",
                        "task-review-accounting-v1.json",
                        "task-operator-signature-v1.json",
                        "task-kind-v1.json",
                        "task-invocation-v1.json",
                        "task-review-result-selected-v1.json",
                        "subject-v1.json",
                        "change-set-v1.json",
                        "change-attestation-v1.json",
                        "verification-continuation-v1.json",
                    ])
                    .map(schema)
                    .collect()
            });
            let root = schema(name);
            let mut builder = jsonschema::Registry::new()
                .add(
                    "urn:af:schema:task-transition:1",
                    jsonschema::Resource::from_contents(schema("task-transition-v1.json")),
                )
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            for resource in resources {
                let id = resource["$id"].as_str().unwrap();
                builder = builder
                    .add(id, jsonschema::Resource::from_contents(resource.clone()))
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            let registry = builder.prepare().unwrap_or_else(|e| panic!("{name}: {e}"));
            jsonschema::options()
                .with_registry(&registry)
                .build(&root)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        })
}

fn assert_valid(name: &str, instance: &Value) {
    let v = validator(name);
    if !v.is_valid(instance) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path()))
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
fn light_optimizer_schemas_reject_rust_validator_divergences() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let proposal = |path: &str| {
        json!({
            "schema":"af.optimization-proposal/1",
            "profile_id":digest,
            "diagnostic_id":digest,
            "recipe_catalog_id":digest,
            "recipe_id":"context_dedup",
            "hypothesis":"remove duplicate context",
            "edits":{path:{"text":"x","executable":false}},
            "expected":{
                "comparable_future_runs":"10",
                "gross_token_savings_per_run":"1",
                "gross_time_savings_ms_per_run":"0",
                "recurring_tokens_per_run":"0",
                "recurring_time_ms_per_run":"0",
                "maximum_validation_tokens":"10",
                "maximum_validation_time_ms":"10"
            }
        })
    };
    assert_valid(
        "optimization-proposal-v1.json",
        &proposal(".af/workers/implementer/worker.toml"),
    );
    assert_invalid(
        "optimization-proposal-v1.json",
        &proposal("config/.git/index"),
        ".git path components are reserved",
    );
    let catalog = json!({
        "schema":"af.optimization-recipe-catalog/1",
        "catalog_version":digest,
        "recipes":[{
            "recipe_id":"context_dedup",
            "capability":"context",
            "support":"installed",
            "applicability":["repeated_context"],
            "required_observations":["context_tokens"],
            "writable_effects":["project_configuration"],
            "validation":["matched_protected_trials"],
            "invalidation":["source_policy_worker_package"],
            "payoff_basis":"bad\u{0001}text"
        }]
    });
    assert_invalid(
        "optimization-recipe-catalog-v1.json",
        &catalog,
        "control characters are rejected by schema and Rust",
    );
    for control in ['\t', '\r', '\u{0085}'] {
        let mut catalog = catalog.clone();
        catalog["recipes"][0]["payoff_basis"] = Value::String(format!("bad{control}text"));
        assert_invalid(
            "optimization-recipe-catalog-v1.json",
            &catalog,
            "every Rust control character except newline is rejected",
        );
        let mut proposal = proposal(".af/workers/implementer/worker.toml");
        proposal["hypothesis"] = Value::String(format!("bad{control}text"));
        assert_invalid(
            "optimization-proposal-v1.json",
            &proposal,
            "proposal text uses the same control-character contract",
        );
    }
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
    let reuse = EvidenceReuseAdmissionV1 {
        demand_id: demand.demand_id.clone(),
        satisfaction_id: digest('2'),
        subject_id: demand.subject_id.clone(),
        actor: "operator".into(),
        policy_revision: "bench-policy@1".into(),
        reason: "the measurement is independent of source bytes".into(),
    };
    reuse.validate().unwrap();
    assert_valid(
        "evidence-reuse-admission-v1.json",
        &serde_json::to_value(&reuse).unwrap(),
    );
    let mut invalid_reuse = serde_json::to_value(&reuse).unwrap();
    invalid_reuse["actor"] = json!("");
    assert_invalid(
        "evidence-reuse-admission-v1.json",
        &invalid_reuse,
        "reuse requires an authenticated actor",
    );
    assert!(
        serde_json::from_value::<EvidenceReuseAdmissionV1>(invalid_reuse)
            .unwrap()
            .validate()
            .is_err()
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
        change_set_id: Some(digest('c')),
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
        check_timeout_seconds: 3600,
        git_timeout_seconds: 300,
        budgets: None,
        focus: Some("authority bootstrap".into()),
        finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
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
    assert!(review_core::contains_report_path(
        &change_set.changed_paths,
        "src/old.rs"
    ));
    assert!(!review_core::contains_report_path(
        &change_set.changed_paths,
        "src/untouched.rs"
    ));
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
fn slice_shard_and_semantic_closure_contracts_roundtrip() {
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let slices = SliceSetV1 {
        subject_id: digest('a'),
        coverage: SliceCoverageV1::Complete,
        max_fanout: 2,
        all_shards_required: true,
        closeout: CloseoutPolicyV1::Required,
        slices: vec![
            ReviewSliceV1 {
                slice_id: digest('b'),
                runtime_node_id: "correctness#slice-b".into(),
                paths: vec!["src/a.rs".into()],
                overlaps: vec![],
            },
            ReviewSliceV1 {
                slice_id: digest('c'),
                runtime_node_id: "correctness#slice-c".into(),
                paths: vec!["src/b.rs".into()],
                overlaps: vec![],
            },
        ],
    };
    slices
        .validate_coverage(&["src/a.rs".into(), "src/b.rs".into()])
        .unwrap();
    assert_valid("slice-set-v1.json", &serde_json::to_value(&slices).unwrap());

    let shards = ShardSetV1 {
        subject_id: slices.subject_id.clone(),
        slice_set_id: digest('d'),
        all_shards_required: true,
        shards: slices
            .slices
            .iter()
            .map(|slice| ShardReceiptV1 {
                slice_id: slice.slice_id.clone(),
                runtime_node_id: slice.runtime_node_id.clone(),
                outcome: ShardOutcomeV1::Completed {
                    result_artifact_ids: vec![digest('e')],
                },
            })
            .collect(),
    };
    shards.validate_against(&slices).unwrap();
    assert_valid("shard-set-v1.json", &serde_json::to_value(&shards).unwrap());

    let closure = SemanticClosureV1 {
        subject_id: slices.subject_id.clone(),
        required_artifact_ids: vec![digest('e')],
        dispositions: vec![SemanticDispositionV1 {
            artifact_id: digest('e'),
            sink: "ledger".into(),
        }],
        closeout_result_id: Some(digest('f')),
        closeout_waiver_policy_id: None,
    };
    closure.validate().unwrap();
    assert_valid(
        "semantic-closure-v1.json",
        &serde_json::to_value(&closure).unwrap(),
    );
}

#[test]
fn integration_contracts_roundtrip() {
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let plan = IntegrationPlanV1 {
        subject_id: digest('a'),
        base_snapshot_id: digest('b'),
        policy_id: digest('c'),
        protected_paths: vec![".github/workflows/release.yml".into()],
        candidates: vec![IntegrationCandidateV1 {
            proposal_id: digest('d'),
            candidate_artifact_id: digest('e'),
            node_id: "correctness".into(),
            priority: 0,
            patch_artifact_id: digest('f'),
            derived_manifest_artifact_id: digest('1'),
            paths: vec!["src/lib.rs".into()],
            finding_ids: vec![digest('2')],
            evidence_ids: vec![digest('3')],
        }],
        derived_manifest_artifact_id: digest('4'),
    };
    plan.validate().unwrap();
    assert_valid(
        "integration-plan-v1.json",
        &serde_json::to_value(&plan).unwrap(),
    );
    let checks = IntegrationChecksV1 {
        derived_snapshot_id: digest('5'),
        checks: vec![IntegrationCheckV1 {
            name: "check".into(),
            passed: true,
            result_artifact_id: digest('6'),
        }],
    };
    checks.validate().unwrap();
    assert!(checks.passed());
    assert_valid(
        "integration-checks-v1.json",
        &serde_json::to_value(checks).unwrap(),
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
    for event_type in EventType::ALL {
        assert_eq!(
            event_type.as_str().parse::<EventType>().unwrap(),
            event_type
        );
    }
    assert!(serde_json::from_str::<EventType>("\"Unknown@1\"").is_err());
    // A type only another release wrote, such as the pre-Task executor's Attempt events, names
    // the way forward instead of only the unknown type.
    assert_eq!(
        "AttemptDispatched@1"
            .parse::<EventType>()
            .unwrap_err()
            .to_string(),
        "unknown review-kernel event type: AttemptDispatched@1; this log was written by another \
         af release; start a new Campaign or Task"
    );
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

    let binding = RunExecutionBindingV4 {
        node: "gate".into(),
        provider: RunExecutionProviderV4::TrustedLocal,
        image: None,
        required_isolation: RunIsolationV4::None,
        provided_isolation: RunIsolationV4::None,
        mode: RunSandboxModeV4::EphemeralWrite,
        admitted: true,
    };
    let payload = serde_json::to_value(binding).unwrap();
    assert!(
        review_core::event::validate_event_payload(EventType::GateExecutionBoundV1, &payload)
            .is_ok()
    );
    let event = RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w1xy".into(),
        run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
        sequence: 1,
        event_type: EventType::GateExecutionBoundV1,
        occurred_at: "2026-08-30T12:00:00Z".into(),
        node_id: Some("gate".into()),
        attempt_id: None,
        causation_id: Some("01jd8m4qz9k7v3n2p6r8t0w201".into()),
        correlation_id: None,
        artifact_refs: vec![],
        payload,
    };
    assert_valid("run-event-v1.json", &serde_json::to_value(event).unwrap());
}

/// A structurally valid RunReport@6 around `outcomes`, `verdict` and `execution`.
fn run_report(
    outcomes: Vec<RunNodeReportV2>,
    blocked_gates: Vec<String>,
    verdict: RunVerdictV3,
    execution: RunReportExecutionV6,
) -> RunReportPayloadV6 {
    let id = format!("sha256:{}", "a".repeat(64));
    RunReportPayloadV6 {
        outcomes,
        blocked_gates,
        verdict,
        spent_tokens: 42u128.into(),
        task_accounting: TaskReviewAccountingV1 {
            task_id: "review-task".into(),
            task_revision_id: id.clone(),
            plan_id: id.clone(),
            task_report_id: id,
            through_sequence: 43,
        },
        execution,
    }
}

#[test]
fn run_reports_are_structural_and_close_a_round_only_with_a_terminal_verdict() {
    let closes = |report: &RunReportPayloadV6| {
        report.validate().unwrap();
        let value = serde_json::to_value(report).unwrap();
        assert_valid("run-report-v6.json", &value);
        assert_eq!(
            &serde_json::from_value::<RunReportPayloadV6>(value.clone()).unwrap(),
            report
        );
        review_core::run_report_closes_round(&RunEvent {
            event_id: "01jd8m4qz9k7v3n2p6r8t0w1xy".into(),
            run_id: "01jd8m4qz9k7v3n2p6r8t0w1xz".into(),
            sequence: 1,
            event_type: EventType::RunReportV6,
            occurred_at: "2026-08-16T12:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: vec![],
            payload: value,
        })
        .unwrap()
        .unwrap()
    };
    // A blocked Gate suppresses what it guards, and the Round stays open.
    let blocked = run_report(
        vec![
            RunNodeReportV2 {
                node: "architecture".into(),
                outcome: RunNodeOutcomeV2::Suppressed {
                    reason: RunSuppressionReasonV2::UpstreamMissing,
                },
            },
            RunNodeReportV2 {
                node: "gate".into(),
                outcome: RunNodeOutcomeV2::Completed {
                    output_artifacts: vec![],
                },
            },
        ],
        vec!["gate".into()],
        RunVerdictV3::Incomplete {
            missing_nodes: vec![MissingNodeV2 {
                node: "architecture".into(),
                reason: "BranchNotSelected".into(),
            }],
        },
        RunReportExecutionV6::Unbound {},
    );
    assert!(!closes(&blocked));
    let exhausted = run_report(
        vec![RunNodeReportV2 {
            node: "review".into(),
            outcome: RunNodeOutcomeV2::Failed {
                error: "run budget exhausted".into(),
            },
        }],
        vec![],
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::Exhausted,
        },
        RunReportExecutionV6::Unbound {},
    );
    assert!(closes(&exhausted));
    let binding = RunExecutionBindingV4 {
        node: "review".into(),
        provider: RunExecutionProviderV4::TrustedLocal,
        image: None,
        required_isolation: RunIsolationV4::None,
        provided_isolation: RunIsolationV4::None,
        mode: RunSandboxModeV4::EphemeralWrite,
        admitted: true,
    };
    let snapshot = RunCacheSnapshotV5 {
        node: "review".into(),
        kind: RunCacheKindV5::Cargo,
        source_digest: format!("sha256:{}", "d".repeat(64)),
        bytes: 42,
        files: 2,
        materialization: RunCacheMaterializationV5::Reflink,
    };
    let completed = vec![RunNodeReportV2 {
        node: "review".into(),
        outcome: RunNodeOutcomeV2::Completed {
            output_artifacts: vec![],
        },
    }];
    let unavailable = RunVerdictV3::Fail {
        reason: RunFailureReasonV3::AuthorityUnavailable,
    };
    for execution in [
        RunReportExecutionV6::Unbound {},
        RunReportExecutionV6::Bound {
            execution_bindings: vec![binding.clone()],
        },
        RunReportExecutionV6::Cached {
            execution_bindings: vec![binding.clone()],
            cache_snapshots: vec![snapshot.clone()],
            cache_failures: vec![],
        },
    ] {
        let report = run_report(completed.clone(), vec![], unavailable.clone(), execution);
        assert!(closes(&report));
    }

    let mut dishonest_isolation = binding.clone();
    dishonest_isolation.provided_isolation = RunIsolationV4::Container;
    let dishonest = run_report(
        completed.clone(),
        vec![],
        unavailable.clone(),
        RunReportExecutionV6::Bound {
            execution_bindings: vec![dishonest_isolation],
        },
    );
    assert!(dishonest.validate().is_err());
    let dishonest_cache = run_report(
        completed.clone(),
        vec![],
        unavailable.clone(),
        RunReportExecutionV6::Cached {
            execution_bindings: vec![binding.clone()],
            cache_snapshots: vec![RunCacheSnapshotV5 {
                node: "missing".into(),
                ..snapshot
            }],
            cache_failures: vec![],
        },
    );
    assert!(dishonest_cache.validate().is_err());
    // A Cache failure fails its Gate: a completed outcome cannot carry one, and the failed
    // Gate leaves the Round incomplete.
    let failed_cache = RunReportExecutionV6::Cached {
        execution_bindings: vec![binding],
        cache_snapshots: vec![],
        cache_failures: vec![RunCacheFailureV5 {
            node: "review".into(),
            kind: RunCacheKindV5::Cargo,
            reason: RunCacheFailureReasonV5::GateSetupFailed,
        }],
    };
    let mut failed = run_report(completed, vec![], unavailable, failed_cache);
    assert!(failed.validate().is_err());
    failed.outcomes[0].outcome = RunNodeOutcomeV2::Failed {
        error: "provider unavailable".into(),
    };
    failed.verdict = RunVerdictV3::Incomplete {
        missing_nodes: vec![MissingNodeV2 {
            node: "review".into(),
            reason: "provider unavailable".into(),
        }],
    };
    assert!(!closes(&failed));
}

#[test]
fn cache_manifest_v1_has_explicit_path_encoding_and_exact_totals() {
    let manifest = CacheManifestV1 {
        kind: RunCacheKindV5::Cargo,
        path_encoding: CachePathEncodingV1::PercentV2,
        entries: vec![CacheManifestEntryV1 {
            path: "registry/cache/index/ leading.crate".into(),
            content: format!("sha256:{}", "a".repeat(64)),
            size: 42,
        }],
    };
    manifest.validate().unwrap();
    assert_eq!(manifest.bytes(), 42);
    assert_valid(
        "cache-manifest-v1.json",
        &serde_json::to_value(&manifest).unwrap(),
    );

    let mut credential = manifest.clone();
    credential.entries[0].path = "registry/cache/index/.git-credentials".into();
    assert!(credential.validate().is_err());
    let mut outside_layout = manifest.clone();
    outside_layout.entries[0].path = "registry/src/index/lib.rs".into();
    assert!(outside_layout.validate().is_err());
    let mut over_ceiling = manifest.clone();
    over_ceiling.entries[0].size = review_core::MAX_CACHE_BYTES_V1 + 1;
    assert!(over_ceiling.validate().is_err());

    let failure = RunCacheFailureV5 {
        node: "gate".into(),
        kind: RunCacheKindV5::Cargo,
        reason: RunCacheFailureReasonV5::LimitExceeded,
    };
    failure.validate().unwrap();
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
    let report = |outcomes: Value, verdict: Value| {
        let mut report = serde_json::to_value(run_report(
            vec![],
            vec![],
            RunVerdictV3::Pass,
            RunReportExecutionV6::Unbound {},
        ))
        .unwrap();
        report["outcomes"] = outcomes;
        report["verdict"] = verdict;
        report
    };
    // Only an exhausted budget may conclude a Round with unresolved nodes; every other terminal
    // verdict, including an authority failure, contradicts a failed or suppressed outcome.
    let unresolved = json!([{"node":"reviewer", "outcome":{"kind":"failed", "error":"crashed"}}]);
    for contradictory in [
        json!({"kind":"pass"}),
        json!({"kind":"fail", "reason":"not_converged"}),
        json!({"kind":"fail", "reason":"authority_unavailable"}),
    ] {
        assert!(
            review_core::event::validate_event_payload(
                EventType::RunReportV6,
                &report(unresolved.clone(), contradictory.clone())
            )
            .is_err(),
            "{contradictory}"
        );
    }
    review_core::event::validate_event_payload(
        EventType::RunReportV6,
        &report(
            unresolved.clone(),
            json!({"kind":"fail", "reason":"exhausted"}),
        ),
    )
    .unwrap();

    let empty_reason = report(
        unresolved,
        json!({"kind":"incomplete", "missing_nodes":[{"node":"reviewer", "reason":""}]}),
    );
    assert!(
        review_core::event::validate_event_payload(EventType::RunReportV6, &empty_reason).is_err()
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

#[path = "schema_parity/task_contracts.rs"]
mod task_contracts;
#[path = "schema_parity/task_reports.rs"]
mod task_reports;

#[test]
fn task_lifecycle_events_have_closed_versioned_payloads() {
    use review_core::task::TaskWaitingReasonV1;
    use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
    let id = format!("sha256:{}", "a".repeat(64));
    let changes = [
        TaskChangeV1::Opened {
            revision_id: id.clone(),
            lease_until_unix_ms: 200,
        },
        TaskChangeV1::LeaseTaken {
            lease_until_unix_ms: 200,
        },
        TaskChangeV1::LeaseRenewed {
            lease_until_unix_ms: 200,
        },
        TaskChangeV1::PlanProposed {
            plan_id: id.clone(),
        },
        TaskChangeV1::PlanningCompleted {
            bootstrap_plan_id: id.clone(),
            proposal_id: id.clone(),
            revision_id: id.clone(),
            plan_id: id.clone(),
        },
        TaskChangeV1::SourceRefreshed {
            revision_id: id.clone(),
            plan_id: Some(id.clone()),
            waiting: None,
        },
        TaskChangeV1::SourceRefreshed {
            revision_id: id.clone(),
            plan_id: None,
            waiting: Some(TaskWaitingReasonV1::NeedsResources),
        },
        TaskChangeV1::PlanDecided {
            decision_id: id.clone(),
            valid_until_unix_ms: 200,
        },
        TaskChangeV1::ApprovalRevoked {
            decision_id: id.clone(),
            reason: "Revoked by developer".into(),
            revocation_id: id.clone(),
        },
        TaskChangeV1::PlanAdmitted {
            plan_id: id.clone(),
        },
        TaskChangeV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsInput,
        },
        TaskChangeV1::Resumed {},
        TaskChangeV1::LeaseReleased {},
        TaskChangeV1::ExecutionRecorded {
            record_id: id.clone(),
        },
        TaskChangeV1::DeliveryRecorded {
            record_id: id.clone(),
        },
        TaskChangeV1::Finished {
            result_id: id.clone(),
        },
    ];
    for change in changes {
        let transition = TaskTransitionV1 {
            writer: "writer-1".into(),
            epoch: 1,
            now_unix_ms: 100,
            change,
        };
        transition.validate().unwrap();
        let mut value = serde_json::to_value(&transition).unwrap();
        assert_valid("task-transition-v1.json", &value);
        review_core::event::validate_event_payload(EventType::TaskTransitionV1, &value).unwrap();
        value["change"]["unrecognized"] = json!(true);
        assert!(!validator("task-transition-v1.json").is_valid(&value));
        assert!(
            review_core::event::validate_event_payload(EventType::TaskTransitionV1, &value)
                .is_err()
        );
    }
    // A revocation always carries its retained proof.
    let unproven = json!({"writer":"writer-1","epoch":1,"now_unix_ms":100,
        "change":{"kind":"approval_revoked","decision_id":id,"reason":"Revoked by developer"}});
    assert_invalid("task-transition-v1.json", &unproven, "revocation proof");
    assert!(serde_json::from_value::<TaskTransitionV1>(unproven).is_err());
}

#[test]
fn source_contracts_keep_exact_fields_separate_from_normalized_requirements_and_authority() {
    use review_core::task::source::*;
    let id = format!("sha256:{}", "a".repeat(64));
    let issue = json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42","revision":"2026-09-12T10:00:00Z",
        "summary":"Add pagination","description":"Preserve input","acceptance":{"customfield_1":"Reject invalid bounds"}});
    assert_valid("issue-input-v1.json", &issue);
    let parsed: IssueInputV1 = serde_json::from_value(issue.clone()).unwrap();
    parsed.validate().unwrap();
    let requirements = parsed.requirements(Some(
        json!({"schema":"tutorial.pagination/1"})
            .as_object()
            .unwrap()
            .clone(),
    ));
    requirements.validate().unwrap();
    assert_valid(
        "normalized-task-requirements-v1.json",
        &serde_json::to_value(&requirements).unwrap(),
    );
    let capture = json!({"schema":"af.task-source-capture/1","adapter":"jira_cloud","locator":"https://example.atlassian.net/rest/api/3/issue/AF-42",
        "external_id":"10042","external_key":"AF-42","source_revision":"2026-09-12T10:00:00Z","raw_source_id":id,
        "fields":{"summary":{"value_id":id,"text_id":id},"description":{"value_id":id,"text_id":id}}});
    assert_valid("task-source-capture-v1.json", &capture);
    serde_json::from_value::<TaskSourceCaptureV1>(capture.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut forged = capture;
    forged["fields"]
        .as_object_mut()
        .unwrap()
        .remove("description");
    assert!(!validator("task-source-capture-v1.json").is_valid(&forged));
    assert!(
        serde_json::from_value::<TaskSourceCaptureV1>(forged)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut forged = issue;
    forged["allowed_effects"] = json!(["write-source"]);
    assert!(!validator("issue-input-v1.json").is_valid(&forged));
    assert!(serde_json::from_value::<IssueInputV1>(forged).is_err());
    let mut forged = serde_json::to_value(&requirements).unwrap();
    forged["specification"] = json!(null);
    assert!(!validator("normalized-task-requirements-v1.json").is_valid(&forged));
    assert!(serde_json::from_value::<NormalizedRequirementsV1>(forged).is_err());
}

#[test]
fn source_refresh_event_requires_exactly_one_plan_or_unresolved_reason() {
    use review_core::task::event::TaskTransitionV1;
    let id = format!("sha256:{}", "a".repeat(64));
    let base = json!({"writer":"writer-1","epoch":1,"now_unix_ms":100,
        "change":{"kind":"source_refreshed","revision_id":id}});
    for fields in [
        json!({}),
        json!({"plan_id":id,"waiting":"needs_resources"}),
        json!({"plan_id":null}),
        json!({"waiting":null}),
        json!({"waiting":"needs_plan_review"}),
    ] {
        let mut value = base.clone();
        value["change"]
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert_invalid(
            "task-transition-v1.json",
            &value,
            "ambiguous source barrier",
        );
        assert!(
            serde_json::from_value::<TaskTransitionV1>(value)
                .map(|v| v.validate().is_err())
                .unwrap_or(true)
        );
    }
}

#[test]
fn task_review_metadata_retains_typed_canonical_results_and_closed_proposal_dispositions() {
    use review_core::task::review_compat::*;
    let id = format!("sha256:{}", "a".repeat(64));
    for contract in [
        review_core::ReviewerResultContract::V1,
        review_core::ReviewerResultContract::V2,
    ] {
        for proposal in [
            TaskReviewProposalV1::None {},
            TaskReviewProposalV1::Prepared {
                candidate_artifact_id: id.clone(),
            },
            TaskReviewProposalV1::Refused {
                reason: review_core::ProposalRefusalReasonV1::PatchMismatch,
            },
        ] {
            let metadata = TaskReviewResultMetadataV1 {
                result_contract: contract,
                result_artifact_id: id.clone(),
                provenance_artifact_id: id.clone(),
                proposal,
            };
            metadata.validate().unwrap();
            let value = serde_json::to_value(&metadata).unwrap();
            assert_valid("task-review-result-metadata-v1.json", &value);
            for (field, bad) in [
                ("result_contract", json!("opaque")),
                ("result_artifact_id", json!("stale")),
                ("provenance_artifact_id", json!(null)),
                ("proposal", json!({"kind":"selected"})),
            ] {
                let mut wrong = value.clone();
                wrong[field] = bad;
                assert!(!validator("task-review-result-metadata-v1.json").is_valid(&wrong));
                assert!(
                    serde_json::from_value::<TaskReviewResultMetadataV1>(wrong)
                        .map_or(true, |v| v.validate().is_err())
                );
            }
            for extra in ["root", "proposal"] {
                let mut wrong = value.clone();
                if extra == "root" {
                    wrong["undeclared"] = json!(true);
                } else {
                    wrong["proposal"]["undeclared"] = json!(true);
                }
                assert!(!validator("task-review-result-metadata-v1.json").is_valid(&wrong));
                assert!(serde_json::from_value::<TaskReviewResultMetadataV1>(wrong).is_err());
            }
        }
    }
}

#[test]
fn task_review_attempt_provenance_preserves_wide_charge_and_unknown_usage() {
    use review_core::task::review_compat::TaskReviewAttemptProvenanceV1;
    let id = format!("sha256:{}", "a".repeat(64));
    let schema = "task-review-attempt-provenance-v1.json";
    for known in [false, true] {
        let mut value = json!({"context_id":id, "task_invocation_id":id,
            "attempt_id":"b".repeat(26), "review_node":"reviewer", "result_artifact_id":id,
            "mutations_artifact_id":id, "raw_artifact_id":id, "charged_tokens":u64::MAX.to_string()});
        if known {
            value["usage_id"] = json!(id);
        }
        assert_valid(schema, &value);
        let typed: TaskReviewAttemptProvenanceV1 = serde_json::from_value(value.clone()).unwrap();
        typed.validate().unwrap();
        assert_eq!(typed.charged_tokens.get(), u64::MAX);
        assert_eq!(typed.usage_id.is_some(), known);
        for (field, bad) in [
            ("charged_tokens", json!(1)),
            ("charged_tokens", json!("18446744073709551616")),
            ("charged_tokens", json!("01")),
            ("usage_id", json!(null)),
            ("result_artifact_id", json!("missing")),
            ("extra", json!(true)),
        ] {
            let mut bad_value = value.clone();
            bad_value[field] = bad;
            assert_invalid(schema, &bad_value, "closed exact provenance");
            assert!(
                serde_json::from_value::<TaskReviewAttemptProvenanceV1>(bad_value)
                    .map_or(true, |v| v.validate().is_err())
            );
        }
    }
}

#[test]
fn task_review_gate_facts_preserve_closed_failed_attempt_observations() {
    use review_core::task::review_compat::TaskReviewGateFactsV1;
    let valid = json!({
        "round_event_id":"a".repeat(26), "review_node":"gate",
        "attempt_id":"b".repeat(26), "cache_failures":[{
            "node":"gate", "kind":"cargo", "reason":"policy_unavailable"
        }],
    });
    let schema = "task-review-gate-facts-v1.json";
    let read = |v: Value| {
        serde_json::from_value::<TaskReviewGateFactsV1>(v).is_ok_and(|v| v.validate().is_ok())
    };
    assert_valid(schema, &valid);
    assert!(read(valid.clone()));
    for (field, replacement) in [
        ("round_event_id", json!("stale")),
        ("attempt_id", json!(null)),
        ("review_node", json!(" ")),
        (
            "cache_failures",
            json!([valid["cache_failures"][0], valid["cache_failures"][0]]),
        ),
        ("authority", json!("approved")),
    ] {
        let mut wrong = valid.clone();
        wrong[field] = replacement;
        assert_invalid(
            schema,
            &wrong,
            "Gate observations have a closed bounded contract",
        );
        assert!(!read(wrong));
    }
    let mut wrong = valid;
    wrong["cache_failures"][0]["node"] = json!("another_gate");
    assert!(
        !read(wrong),
        "semantic validation binds failures to their Gate"
    );
}

#[test]
fn task_review_context_and_selection_require_exact_closed_execution_identities() {
    use review_core::task::review_compat::*;
    let id = format!("sha256:{}", "a".repeat(64));
    let context = json!({
        "campaign_id":"review-task", "round_event_id":"a".repeat(26),
        "invocation_event_id":"b".repeat(26), "review_node":"reviewer",
        "subject_id":id, "campaign_manifest_id":id, "task_invocation_id":id,
        "attempt_id":"c".repeat(26), "reviewer_inputs_id":id,
        "rendered_input_id":id, "context_manifest_id":id,
    });
    let selection = json!({
        "task_id":"task-1", "task_node":"root.nodes.review", "task_revision_id":id,
        "plan_id":id, "invocation_id":id, "output_id":id, "context_id":id,
        "result_envelope_id":id, "metadata_envelope_id":id,
        "result_artifact_id":id, "provenance_artifact_id":id,
    });
    let read_context = |value: &Value| {
        serde_json::from_value::<TaskReviewContextV1>(value.clone())
            .is_ok_and(|v| v.validate().is_ok())
    };
    let read_selection = |value: &Value| {
        serde_json::from_value::<TaskReviewResultSelectedV1>(value.clone())
            .is_ok_and(|v| v.validate().is_ok())
    };
    for (schema, valid, parse) in [
        (
            "task-review-context-v1.json",
            context,
            &read_context as &dyn Fn(&Value) -> bool,
        ),
        (
            "task-review-result-selected-v1.json",
            selection.clone(),
            &read_selection as &dyn Fn(&Value) -> bool,
        ),
    ] {
        assert_valid(schema, &valid);
        assert!(parse(&valid));
        for key in valid.as_object().unwrap().keys() {
            for replacement in [json!(null), json!("")] {
                let mut wrong = valid.clone();
                wrong[key] = replacement;
                assert_invalid(schema, &wrong, "missing exact Review authority");
                assert!(!parse(&wrong));
            }
        }
        let mut wrong = valid;
        wrong["approved"] = json!(true);
        assert_invalid(
            schema,
            &wrong,
            "serialized data cannot grant selection authority",
        );
        assert!(!parse(&wrong));
    }
    review_core::event::validate_event_payload(EventType::TaskReviewResultSelectedV1, &selection)
        .unwrap();
    assert_valid(
        "run-event-v1.json",
        &json!({
            "event_id":"d".repeat(26), "run_id":"review-task", "sequence":4,
            "type":"TaskReviewResultSelected@1", "occurred_at":"2026-09-12T00:00:00Z",
            "payload":selection,
        }),
    );
}

#[test]
fn legacy_review_round_input_and_gate_outcome_have_closed_distinct_contracts() {
    use review_core::task::review_compat::{LegacyReviewGateOutcomeV1, LegacyReviewRoundV1};
    let id = format!("sha256:{}", "a".repeat(64));
    let round = json!({
        "campaign_id": "review-ticket", "round_event_id": "a".repeat(26),
        "campaign_manifest_id": id, "subject_id": id, "head_snapshot_id": id,
        "round": 1, "epoch": 1,
    });
    let gate = json!({"round_event_id": "a".repeat(26), "review_node": "gate",
        "gate_decision_id": id, "outcome": "passed"});
    assert_valid("legacy-review-round-v1.json", &round);
    let parsed = serde_json::from_value::<LegacyReviewRoundV1>(round.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), round);
    for outcome in ["passed", "failed"] {
        let mut value = gate.clone();
        value["outcome"] = json!(outcome);
        assert_valid("legacy-review-gate-outcome-v1.json", &value);
        let parsed = serde_json::from_value::<LegacyReviewGateOutcomeV1>(value.clone()).unwrap();
        parsed.validate().unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
    for (field, replacement) in [
        ("round", json!(0)),
        ("epoch", json!(0)),
        ("round_event_id", json!("a".repeat(25))),
        ("campaign_id", json!("")),
        ("head_snapshot_id", json!("HEAD")),
        ("unknown", json!(true)),
    ] {
        let mut value = round.clone();
        value[field] = replacement;
        assert!(!validator("legacy-review-round-v1.json").is_valid(&value));
        assert!(
            serde_json::from_value::<LegacyReviewRoundV1>(value)
                .map_err(|e| e.to_string())
                .and_then(|v| v.validate())
                .is_err()
        );
    }
    for (field, replacement) in [
        ("outcome", json!("inconclusive")),
        ("review_node", json!("")),
        ("gate_decision_id", json!("allow")),
        ("round_event_id", json!("A".repeat(26))),
        ("unknown", json!(true)),
    ] {
        let mut value = gate.clone();
        value[field] = replacement;
        assert!(!validator("legacy-review-gate-outcome-v1.json").is_valid(&value));
        assert!(
            serde_json::from_value::<LegacyReviewGateOutcomeV1>(value)
                .map_err(|e| e.to_string())
                .and_then(|v| v.validate())
                .is_err()
        );
    }
    assert!(!validator("task-review-round-v1.json").is_valid(&round));
}

#[test]
fn warm_layer_contracts_roundtrip_and_stay_closed() {
    use review_core::event::validate_event_payload;
    use review_core::{
        HeadDeltaEntryV1, HeadDeltaMarkV1, HeadDeltaV1, InspectedPathV1, PathHintV1, WarmLayerV1,
        WarmSetSelectedPayloadV1, WarmSetV1, WorkerNotesDropReasonV1, WorkerNotesRecordedPayloadV1,
        WorkerNotesV1,
    };
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let notes = WorkerNotesV1 {
        node: "correctness".into(),
        attempt_id: "a".repeat(26),
        head_snapshot_id: digest('1'),
        inspected: vec![InspectedPathV1 {
            path: "src/lib.rs".into(),
            tree_entry_digest: Some(digest('2')),
        }],
        model_of_change: "the retry loop gained a cap".into(),
        open_questions: vec!["is the cap configurable?".into()],
        hints: vec![PathHintV1 {
            path: "src/retry.rs".into(),
            note: "the cap is read once at start".into(),
        }],
    };
    notes.validate().unwrap();
    let mut value = serde_json::to_value(&notes).unwrap();
    assert_valid("worker-notes-v1.json", &value);
    let decoded: WorkerNotesV1 = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, notes);
    assert_eq!(
        notes.referenced_paths().into_iter().collect::<Vec<_>>(),
        ["src/lib.rs", "src/retry.rs"]
    );
    value["verdict"] = json!("approve");
    assert_invalid(
        "worker-notes-v1.json",
        &value,
        "notes are an inspection map and never carry a verdict or a disposition",
    );
    assert!(serde_json::from_value::<WorkerNotesV1>(value).is_err());

    let delta = HeadDeltaV1 {
        node: "correctness".into(),
        from_snapshot_id: digest('1'),
        to_snapshot_id: digest('3'),
        diff_policy_version: "review.kernel/git-tree-diff@test".into(),
        rename_detection_truncated: true,
        changed_paths: vec!["src/new.rs".into(), "src/old.rs".into()],
        marks: vec![
            HeadDeltaEntryV1 {
                path: "src/lib.rs".into(),
                mark: HeadDeltaMarkV1::Reverted,
                renamed_from: None,
            },
            HeadDeltaEntryV1 {
                path: "src/new.rs".into(),
                mark: HeadDeltaMarkV1::Renamed,
                renamed_from: Some("src/old.rs".into()),
            },
            HeadDeltaEntryV1 {
                path: "src/old.rs".into(),
                mark: HeadDeltaMarkV1::Removed,
                renamed_from: None,
            },
        ],
    };
    delta.validate().unwrap();
    let mut value = serde_json::to_value(&delta).unwrap();
    assert_valid("head-delta-v1.json", &value);
    let decoded: HeadDeltaV1 = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, delta);
    value["base_snapshot_id"] = json!(digest('9'));
    assert_invalid(
        "head-delta-v1.json",
        &value,
        "a Head Delta names two consecutive heads and never a Base Snapshot",
    );
    assert!(serde_json::from_value::<HeadDeltaV1>(value).is_err());
    let mut unmarked = delta.clone();
    unmarked.marks.retain(|entry| entry.path != "src/old.rs");
    assert!(unmarked.validate().is_err(), "every changed path is marked");

    let set = WarmSetV1 {
        node: "correctness".into(),
        round: 2,
        source_attempt_id: Some("a".repeat(26)),
        notes_artifact_id: Some(digest('4')),
        head_delta_artifact_id: Some(digest('5')),
        head_delta_dropped: None,
        build_cache_artifact_id: None,
        build_cache_dropped: None,
        workspace: None,
        workspace_id: None,
        session_artifact_id: None,
        session_dropped: None,
    };
    set.validate().unwrap();
    let dropped = WarmSetV1 {
        head_delta_artifact_id: None,
        head_delta_dropped: Some(review_core::HeadDeltaDropReasonV1::OverBound),
        ..set.clone()
    };
    dropped.validate().unwrap();
    assert_valid("warm-set-v1.json", &serde_json::to_value(&dropped).unwrap());
    let mut value = serde_json::to_value(&set).unwrap();
    assert_valid("warm-set-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<WarmSetV1>(value.clone()).unwrap(),
        set
    );
    value["transcript_artifact_id"] = json!(digest('6'));
    assert_invalid(
        "warm-set-v1.json",
        &value,
        "a Warm Set carries exactly the layers the vocabulary names and nothing else",
    );
    assert!(serde_json::from_value::<WarmSetV1>(value).is_err());
    let orphan = json!({"node": "correctness", "round": 2, "notes_artifact_id": digest('4')});
    assert_invalid(
        "warm-set-v1.json",
        &orphan,
        "Notes without their source Attempt",
    );
    let orphan: WarmSetV1 = serde_json::from_value(orphan).unwrap();
    assert!(orphan.validate().is_err());

    let selected = WarmSetSelectedPayloadV1 {
        warm_set_artifact_id: digest('7'),
        source_attempt_id: Some("a".repeat(26)),
        layers: vec![WarmLayerV1::Notes, WarmLayerV1::HeadDelta],
    };
    selected.validate().unwrap();
    validate_event_payload(
        EventType::WarmSetSelectedV1,
        &serde_json::to_value(&selected).unwrap(),
    )
    .unwrap();
    let event = RunEvent {
        event_id: "b".repeat(26),
        run_id: "run".into(),
        sequence: 8,
        event_type: EventType::WarmSetSelectedV1,
        occurred_at: "2026-09-17T00:00:00Z".into(),
        node_id: Some("correctness".into()),
        attempt_id: None,
        causation_id: Some("c".repeat(26)),
        correlation_id: None,
        artifact_refs: vec![digest('7')],
        payload: serde_json::to_value(&selected).unwrap(),
    };
    assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
    let mut duplicate = serde_json::to_value(&event).unwrap();
    duplicate["payload"]["layers"] = json!(["notes", "notes"]);
    assert_invalid("run-event-v1.json", &duplicate, "layers are unique");
    let payload = duplicate["payload"].clone();
    assert!(validate_event_payload(EventType::WarmSetSelectedV1, &payload).is_err());

    for recorded in [
        WorkerNotesRecordedPayloadV1 {
            result_artifact_id: digest('8'),
            notes_artifact_id: Some(digest('4')),
            dropped: None,
            bytes: 512,
        },
        WorkerNotesRecordedPayloadV1 {
            result_artifact_id: digest('8'),
            notes_artifact_id: None,
            dropped: Some(WorkerNotesDropReasonV1::OverBound),
            bytes: 70_000,
        },
    ] {
        recorded.validate().unwrap();
        let payload = serde_json::to_value(&recorded).unwrap();
        validate_event_payload(EventType::WorkerNotesRecordedV1, &payload).unwrap();
        let event = RunEvent {
            event_type: EventType::WorkerNotesRecordedV1,
            attempt_id: Some("a".repeat(26)),
            artifact_refs: vec![digest('8')],
            payload,
            ..event.clone()
        };
        assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
    }
    let both = json!({
        "result_artifact_id": digest('8'), "notes_artifact_id": digest('4'),
        "dropped": "over_bound", "bytes": 1
    });
    let mut event_value = serde_json::to_value(&event).unwrap();
    event_value["type"] = json!("WorkerNotesRecorded@1");
    event_value["payload"] = both.clone();
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a notes record is either an artifact or a drop, never both",
    );
    assert!(validate_event_payload(EventType::WorkerNotesRecordedV1, &both).is_err());
}

#[test]
fn build_cache_contracts_are_explicitly_unsafe_bounded_and_closed() {
    use review_core::event::validate_event_payload;
    use review_core::{
        BuildCacheCapturedPayloadV1, BuildCacheDropReasonV1, BuildCacheKindV1, BuildCacheLimitsV1,
        BuildCacheRefusalReasonV1, BuildCacheTrustV1, BuildCacheV1, WarmLayerV1,
        WarmSetSelectedPayloadV1, WarmSetV1,
    };
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let cache = BuildCacheV1 {
        kind: BuildCacheKindV1::CargoTarget,
        trust: BuildCacheTrustV1::CandidateBuilt,
        gate_node: "gate".into(),
        gate_attempt_id: Some("a".repeat(26)),
        head_snapshot_id: digest('1'),
        manifest_id: digest('2'),
        content_digest: digest('3'),
        entries: 12,
        bytes: 4096,
        limits: BuildCacheLimitsV1::default_v1(),
    };
    cache.validate().unwrap();
    let mut value = serde_json::to_value(&cache).unwrap();
    assert_valid("build-cache-v1.json", &value);
    assert_eq!(
        value["trust"], "candidate_built",
        "the payload itself says it was built by candidate code"
    );
    assert_eq!(
        serde_json::from_value::<BuildCacheV1>(value.clone()).unwrap(),
        cache
    );
    value["trust"] = json!("administrator_approved");
    assert_invalid(
        "build-cache-v1.json",
        &value,
        "a Build Cache can never claim Cache Snapshot approval",
    );
    assert!(serde_json::from_value::<BuildCacheV1>(value.clone()).is_err());
    value["trust"] = json!("candidate_built");
    value["kind"] = json!("cargo");
    assert_invalid(
        "build-cache-v1.json",
        &value,
        "the registry-only cargo snapshot is not a build cache kind",
    );
    value["kind"] = json!("cargo_target");
    value["source_path"] = json!("/home/operator/.cargo");
    assert_invalid(
        "build-cache-v1.json",
        &value,
        "a Build Cache never records a host path",
    );
    assert!(serde_json::from_value::<BuildCacheV1>(value).is_err());
    let mut empty = serde_json::to_value(&cache).unwrap();
    empty["entries"] = json!(0);
    assert_invalid("build-cache-v1.json", &empty, "an empty capture is refused");

    let captured = BuildCacheCapturedPayloadV1 {
        gate_node: "gate".into(),
        gate_attempt_id: None,
        head_snapshot_id: digest('1'),
        kind: BuildCacheKindV1::CargoTarget,
        limits: BuildCacheLimitsV1::default_v1(),
        build_cache_artifact_id: Some(digest('4')),
        refused: None,
        entries: 12,
        bytes: 4096,
    };
    let refused = BuildCacheCapturedPayloadV1 {
        build_cache_artifact_id: None,
        refused: Some(BuildCacheRefusalReasonV1::UnsafeContent),
        entries: 0,
        bytes: 0,
        ..captured.clone()
    };
    let event = RunEvent {
        event_id: "b".repeat(26),
        run_id: "run".into(),
        sequence: 9,
        event_type: EventType::BuildCacheCapturedV1,
        occurred_at: "2026-09-17T00:00:00Z".into(),
        node_id: Some("gate".into()),
        attempt_id: None,
        causation_id: Some("c".repeat(26)),
        correlation_id: None,
        artifact_refs: vec![digest('4'), digest('2')],
        payload: json!({}),
    };
    for payload in [&captured, &refused] {
        payload.validate().unwrap();
        let payload = serde_json::to_value(payload).unwrap();
        validate_event_payload(EventType::BuildCacheCapturedV1, &payload).unwrap();
        let event = RunEvent {
            payload,
            ..event.clone()
        };
        assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
    }
    let both = json!({
        "gate_node": "gate", "head_snapshot_id": digest('1'), "kind": "cargo_target",
        "limits": serde_json::to_value(BuildCacheLimitsV1::default_v1()).unwrap(),
        "build_cache_artifact_id": digest('4'), "refused": "limit_exceeded",
        "entries": 1, "bytes": 1
    });
    let mut event_value = serde_json::to_value(&event).unwrap();
    event_value["payload"] = both.clone();
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a capture record is either an artifact or a refusal, never both",
    );
    assert!(validate_event_payload(EventType::BuildCacheCapturedV1, &both).is_err());

    let set = WarmSetV1 {
        node: "tdd".into(),
        round: 1,
        source_attempt_id: None,
        notes_artifact_id: None,
        head_delta_artifact_id: None,
        head_delta_dropped: None,
        build_cache_artifact_id: Some(digest('4')),
        build_cache_dropped: None,
        workspace: None,
        workspace_id: None,
        session_artifact_id: None,
        session_dropped: None,
    };
    set.validate().unwrap();
    assert_eq!(set.layers(), vec![WarmLayerV1::BuildCache]);
    let value = serde_json::to_value(&set).unwrap();
    assert_valid("warm-set-v1.json", &value);
    assert_eq!(serde_json::from_value::<WarmSetV1>(value).unwrap(), set);
    let dropped = WarmSetV1 {
        build_cache_artifact_id: None,
        build_cache_dropped: Some(BuildCacheDropReasonV1::Refused),
        ..set
    };
    dropped.validate().unwrap();
    assert_valid("warm-set-v1.json", &serde_json::to_value(&dropped).unwrap());
    let selected = WarmSetSelectedPayloadV1 {
        warm_set_artifact_id: digest('7'),
        source_attempt_id: None,
        layers: vec![WarmLayerV1::BuildCache],
    };
    selected.validate().unwrap();
    let payload = serde_json::to_value(&selected).unwrap();
    validate_event_payload(EventType::WarmSetSelectedV1, &payload).unwrap();
    let event = RunEvent {
        event_type: EventType::WarmSetSelectedV1,
        node_id: Some("tdd".into()),
        artifact_refs: vec![digest('7')],
        payload,
        ..event
    };
    assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
}

#[test]
fn warm_workspace_contracts_roundtrip_and_stay_closed() {
    use review_core::event::validate_event_payload;
    use review_core::{
        WarmLayerV1, WarmSetSelectedPayloadV1, WarmSetV1, WorkspaceBasisV1,
        WorkspaceFallbackReasonV1, WorkspaceRebasedPayloadV1,
    };
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let workspace_id = "c".repeat(32);

    let set = WarmSetV1 {
        node: "correctness".into(),
        round: 2,
        source_attempt_id: None,
        notes_artifact_id: None,
        head_delta_artifact_id: None,
        head_delta_dropped: None,
        build_cache_artifact_id: None,
        build_cache_dropped: None,
        workspace: Some(WorkspaceBasisV1::Rebased),
        workspace_id: Some(workspace_id.clone()),
        session_artifact_id: None,
        session_dropped: None,
    };
    set.validate().unwrap();
    assert_eq!(set.layers(), vec![WarmLayerV1::Workspace]);
    let mut value = serde_json::to_value(&set).unwrap();
    assert_valid("warm-set-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<WarmSetV1>(value.clone()).unwrap(),
        set
    );
    for basis in [WorkspaceBasisV1::Full, WorkspaceBasisV1::Reused] {
        let other = WarmSetV1 {
            workspace: Some(basis),
            ..set.clone()
        };
        other.validate().unwrap();
        assert_valid("warm-set-v1.json", &serde_json::to_value(&other).unwrap());
    }
    value["workspace_id"] = json!("/Users/operator/.cache/af/workspaces/c");
    assert_invalid(
        "warm-set-v1.json",
        &value,
        "a workspace is named by an opaque identity, never a host path",
    );
    assert!(
        serde_json::from_value::<WarmSetV1>(value)
            .unwrap()
            .validate()
            .is_err()
    );
    let nameless = json!({"node": "correctness", "round": 2, "workspace": "rebased"});
    assert_invalid(
        "warm-set-v1.json",
        &nameless,
        "a workspace basis without its identity",
    );
    assert!(
        serde_json::from_value::<WarmSetV1>(nameless)
            .unwrap()
            .validate()
            .is_err()
    );
    let baseless = json!({"node": "correctness", "round": 2, "workspace_id": &workspace_id});
    assert_invalid(
        "warm-set-v1.json",
        &baseless,
        "a workspace identity without its basis",
    );

    let selected = WarmSetSelectedPayloadV1 {
        warm_set_artifact_id: digest('7'),
        source_attempt_id: None,
        layers: vec![WarmLayerV1::Workspace],
    };
    selected.validate().unwrap();
    let payload = serde_json::to_value(&selected).unwrap();
    validate_event_payload(EventType::WarmSetSelectedV1, &payload).unwrap();
    let event = RunEvent {
        event_id: "b".repeat(26),
        run_id: "run".into(),
        sequence: 10,
        event_type: EventType::WarmSetSelectedV1,
        occurred_at: "2026-09-18T00:00:00Z".into(),
        node_id: Some("correctness".into()),
        attempt_id: None,
        causation_id: Some("c".repeat(26)),
        correlation_id: None,
        artifact_refs: vec![digest('7')],
        payload,
    };
    assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());

    let rebased = WorkspaceRebasedPayloadV1 {
        node: "correctness".into(),
        workspace_id: workspace_id.clone(),
        from_snapshot_id: Some(digest('1')),
        to_snapshot_id: digest('2'),
        basis: WorkspaceBasisV1::Rebased,
        fallback: None,
        verified_digest: digest('3'),
        entries_touched: 4,
        preparation_ms: 250,
    };
    let full = WorkspaceRebasedPayloadV1 {
        from_snapshot_id: None,
        basis: WorkspaceBasisV1::Full,
        fallback: Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        entries_touched: 12,
        ..rebased.clone()
    };
    let fallen_back = WorkspaceRebasedPayloadV1 {
        basis: WorkspaceBasisV1::Full,
        fallback: Some(WorkspaceFallbackReasonV1::DigestMismatch),
        ..rebased.clone()
    };
    let reused = WorkspaceRebasedPayloadV1 {
        basis: WorkspaceBasisV1::Reused,
        entries_touched: 0,
        ..rebased.clone()
    };
    let unrecorded = WorkspaceRebasedPayloadV1 {
        basis: WorkspaceBasisV1::Full,
        fallback: Some(WorkspaceFallbackReasonV1::UnrecordedPreparation),
        entries_touched: 12,
        ..rebased.clone()
    };
    for payload in [&rebased, &full, &fallen_back, &reused, &unrecorded] {
        payload.validate().unwrap();
        let value = serde_json::to_value(payload).unwrap();
        validate_event_payload(EventType::WorkspaceRebasedV1, &value).unwrap();
        assert_eq!(
            serde_json::from_value::<WorkspaceRebasedPayloadV1>(value.clone()).unwrap(),
            *payload
        );
        let event = RunEvent {
            event_type: EventType::WorkspaceRebasedV1,
            artifact_refs: vec![digest('1'), digest('2')],
            payload: value,
            ..event.clone()
        };
        assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
    }
    let mut event_value = serde_json::to_value(&RunEvent {
        event_type: EventType::WorkspaceRebasedV1,
        payload: serde_json::to_value(&rebased).unwrap(),
        ..event.clone()
    })
    .unwrap();
    event_value["payload"]["fallback"] = json!("apply_failed");
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a rebase that succeeded records no fallback reason",
    );
    assert!(
        validate_event_payload(EventType::WorkspaceRebasedV1, &event_value["payload"]).is_err()
    );
    event_value["payload"] = serde_json::to_value(&full).unwrap();
    event_value["payload"]
        .as_object_mut()
        .unwrap()
        .remove("fallback");
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a full materialization records why the template was not re-based",
    );
    assert!(
        validate_event_payload(EventType::WorkspaceRebasedV1, &event_value["payload"]).is_err()
    );
    event_value["payload"] = serde_json::to_value(&reused).unwrap();
    event_value["payload"]["entries_touched"] = json!(1);
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a reused template materializes nothing",
    );
    assert!(
        validate_event_payload(EventType::WorkspaceRebasedV1, &event_value["payload"]).is_err()
    );
    event_value["payload"] = serde_json::to_value(&rebased).unwrap();
    event_value["payload"]["root"] = json!("/Users/operator/.cache/af/workspaces/c/tree");
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a workspace record never carries a host path",
    );
    assert!(
        validate_event_payload(EventType::WorkspaceRebasedV1, &event_value["payload"]).is_err()
    );
}

#[test]
fn session_snapshot_and_cold_closeout_contracts_roundtrip_and_stay_closed() {
    use review_core::event::validate_event_payload;
    use review_core::{
        ColdCloseoutDispatchedPayloadV1, SessionCleanupOutcomeV1, SessionCleanupRefusalV1,
        SessionDropReasonV1, SessionSnapshotCleanedPayloadV1, SessionSnapshotPreparedPayloadV1,
        SessionSnapshotV1, SessionSourceV1, WarmLayerV1, WarmSetSelectedPayloadV1, WarmSetV1,
        session_id_for_attempt,
    };
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let attempt_id = "a".repeat(26);
    let session_id = session_id_for_attempt(&attempt_id).unwrap();
    let event = RunEvent {
        event_id: "b".repeat(26),
        run_id: "run".into(),
        sequence: 11,
        event_type: EventType::SessionSnapshotPreparedV1,
        occurred_at: "2026-09-18T00:00:00Z".into(),
        node_id: Some("correctness".into()),
        attempt_id: Some(attempt_id.clone()),
        causation_id: Some("c".repeat(26)),
        correlation_id: None,
        artifact_refs: vec![digest('4'), digest('3')],
        payload: json!({}),
    };

    let source = SessionSourceV1 {
        provider_kind: "claude".into(),
        path_digest: digest('2'),
    };
    let snapshot = SessionSnapshotV1 {
        node: "correctness".into(),
        attempt_id: attempt_id.clone(),
        session_id: session_id.clone(),
        head_snapshot_id: digest('1'),
        source: source.clone(),
        transcript_artifact_id: digest('3'),
        bytes: 4096,
        estimated_tokens: 1024,
    };
    snapshot.validate().unwrap();
    let mut value = serde_json::to_value(&snapshot).unwrap();
    assert_valid("session-snapshot-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<SessionSnapshotV1>(value.clone()).unwrap(),
        snapshot
    );
    value["source"]["path_digest"] = json!("/Users/operator/.claude/projects/x/s.jsonl");
    assert_invalid(
        "session-snapshot-v1.json",
        &value,
        "a captured session names its source by identity, never by host path",
    );
    let mut value = serde_json::to_value(&snapshot).unwrap();
    value["session_id"] = json!("not-a-session");
    assert_invalid(
        "session-snapshot-v1.json",
        &value,
        "a session identity is the canonical shape the pinned CLI accepts",
    );
    assert!(
        serde_json::from_value::<SessionSnapshotV1>(value)
            .unwrap()
            .validate()
            .is_err()
    );

    let prepared = SessionSnapshotPreparedPayloadV1 {
        session_id: session_id.clone(),
        session_artifact_id: digest('4'),
        transcript_artifact_id: digest('3'),
        source,
        bytes: 4096,
        estimated_tokens: 1024,
        captured_at_unix_ms: 1_789_000_000_000,
    };
    prepared.validate().unwrap();
    let payload = serde_json::to_value(&prepared).unwrap();
    validate_event_payload(EventType::SessionSnapshotPreparedV1, &payload).unwrap();
    assert_eq!(
        serde_json::from_value::<SessionSnapshotPreparedPayloadV1>(payload.clone()).unwrap(),
        prepared
    );
    let event = RunEvent { payload, ..event };
    assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());

    for outcome in [
        SessionCleanupOutcomeV1::Deleted,
        SessionCleanupOutcomeV1::AlreadyAbsent,
    ] {
        let cleaned = SessionSnapshotCleanedPayloadV1 {
            session_id: session_id.clone(),
            outcome,
            refusal: None,
        };
        cleaned.validate().unwrap();
        assert!(cleaned.completed());
        let payload = serde_json::to_value(&cleaned).unwrap();
        validate_event_payload(EventType::SessionSnapshotCleanedV1, &payload).unwrap();
        let event = RunEvent {
            event_type: EventType::SessionSnapshotCleanedV1,
            artifact_refs: Vec::new(),
            payload,
            ..event.clone()
        };
        assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
    }
    let refused = SessionSnapshotCleanedPayloadV1 {
        session_id: session_id.clone(),
        outcome: SessionCleanupOutcomeV1::Refused,
        refusal: Some(SessionCleanupRefusalV1::SymlinkedParent),
    };
    refused.validate().unwrap();
    assert!(!refused.completed());
    let mut event_value = serde_json::to_value(&RunEvent {
        event_type: EventType::SessionSnapshotCleanedV1,
        artifact_refs: Vec::new(),
        payload: serde_json::to_value(&refused).unwrap(),
        ..event.clone()
    })
    .unwrap();
    assert_valid("run-event-v1.json", &event_value);
    event_value["payload"]["outcome"] = json!("deleted");
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a completed deletion records no refusal reason",
    );
    assert!(
        validate_event_payload(EventType::SessionSnapshotCleanedV1, &event_value["payload"])
            .is_err()
    );
    event_value["payload"] = json!({"session_id": session_id, "outcome": "refused"});
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a refusal says what stood where the transcript was",
    );

    let dispatched = ColdCloseoutDispatchedPayloadV1 {
        node: "correctness".into(),
        round: 2,
        warm_attempt_id: attempt_id.clone(),
        warm_result_artifact_id: digest('5'),
        cold_attempt_id: "b".repeat(26),
        reserved_tokens: Some(300_000),
        charged_tokens: 120_000,
        cold_result_artifact_id: Some(digest('6')),
        failed: None,
    };
    dispatched.validate().unwrap();
    let payload = serde_json::to_value(&dispatched).unwrap();
    validate_event_payload(EventType::ColdCloseoutDispatchedV1, &payload).unwrap();
    assert_eq!(
        serde_json::from_value::<ColdCloseoutDispatchedPayloadV1>(payload.clone()).unwrap(),
        dispatched
    );
    let mut event_value = serde_json::to_value(&RunEvent {
        event_type: EventType::ColdCloseoutDispatchedV1,
        attempt_id: Some("b".repeat(26)),
        artifact_refs: vec![digest('5'), digest('6')],
        payload,
        ..event.clone()
    })
    .unwrap();
    assert_valid("run-event-v1.json", &event_value);
    event_value["payload"]["failed"] = json!("timed out");
    assert_invalid(
        "run-event-v1.json",
        &event_value,
        "a closeout names exactly one of a cold result or a failure",
    );
    assert!(
        validate_event_payload(EventType::ColdCloseoutDispatchedV1, &event_value["payload"])
            .is_err()
    );
    event_value["payload"]
        .as_object_mut()
        .unwrap()
        .remove("cold_result_artifact_id");
    assert_valid("run-event-v1.json", &event_value);

    let set = WarmSetV1 {
        node: "correctness".into(),
        round: 3,
        source_attempt_id: Some(attempt_id),
        notes_artifact_id: None,
        head_delta_artifact_id: None,
        head_delta_dropped: None,
        build_cache_artifact_id: None,
        build_cache_dropped: None,
        workspace: None,
        workspace_id: None,
        session_artifact_id: Some(digest('4')),
        session_dropped: None,
    };
    set.validate().unwrap();
    assert_eq!(set.layers(), vec![WarmLayerV1::Session]);
    let value = serde_json::to_value(&set).unwrap();
    assert_valid("warm-set-v1.json", &value);
    assert_eq!(serde_json::from_value::<WarmSetV1>(value).unwrap(), set);
    for reason in [
        SessionDropReasonV1::ProviderUnsupported,
        SessionDropReasonV1::HostUnsupported,
        SessionDropReasonV1::NoSource,
        SessionDropReasonV1::NotCaptured,
        SessionDropReasonV1::CleanupIncomplete,
        SessionDropReasonV1::TooOld,
        SessionDropReasonV1::OverReservation,
        SessionDropReasonV1::MaterializationFailed,
        SessionDropReasonV1::HeadDeltaDropped,
    ] {
        let dropped = WarmSetV1 {
            session_artifact_id: None,
            session_dropped: Some(reason),
            ..set.clone()
        };
        dropped.validate().unwrap();
        assert!(
            dropped.layers().is_empty(),
            "a dropped session carries none"
        );
        assert_valid("warm-set-v1.json", &serde_json::to_value(&dropped).unwrap());
    }
    let orphan = json!({"node": "correctness", "round": 3, "session_artifact_id": digest('4')});
    assert_invalid(
        "warm-set-v1.json",
        &orphan,
        "a transcript without its source Attempt is not a warm layer",
    );
    let selected = WarmSetSelectedPayloadV1 {
        warm_set_artifact_id: digest('7'),
        source_attempt_id: Some("a".repeat(26)),
        layers: vec![WarmLayerV1::Session],
    };
    selected.validate().unwrap();
    let payload = serde_json::to_value(&selected).unwrap();
    validate_event_payload(EventType::WarmSetSelectedV1, &payload).unwrap();
    let event = RunEvent {
        event_type: EventType::WarmSetSelectedV1,
        attempt_id: None,
        artifact_refs: vec![digest('7')],
        payload,
        ..event
    };
    assert_valid("run-event-v1.json", &serde_json::to_value(&event).unwrap());
}

#[path = "schema_parity/task_usage.rs"]
mod task_usage;
#[path = "schema_parity/task_usage_observation.rs"]
mod task_usage_observation;

#[test]
fn task_review_conclusions_preserve_exact_cumulative_charge_and_execution_contracts() {
    let id = format!("sha256:{}", "a".repeat(64));
    let accounting = TaskReviewAccountingV1 {
        task_id: "review-task".into(),
        task_revision_id: id.clone(),
        plan_id: id.clone(),
        task_report_id: id,
        through_sequence: 43,
    };
    assert_valid(
        "task-review-accounting-v1.json",
        &serde_json::to_value(&accounting).unwrap(),
    );
    let binding = RunExecutionBindingV4 {
        node: "gate".into(),
        provider: RunExecutionProviderV4::TrustedLocal,
        image: None,
        required_isolation: RunIsolationV4::None,
        provided_isolation: RunIsolationV4::None,
        mode: RunSandboxModeV4::EphemeralWrite,
        admitted: true,
    };
    for execution in [
        RunReportExecutionV6::Unbound {},
        RunReportExecutionV6::Bound {
            execution_bindings: vec![binding.clone()],
        },
        RunReportExecutionV6::Cached {
            execution_bindings: vec![binding.clone()],
            cache_snapshots: vec![],
            cache_failures: vec![RunCacheFailureV5 {
                node: "gate".into(),
                kind: RunCacheKindV5::Cargo,
                reason: RunCacheFailureReasonV5::PolicyUnavailable,
            }],
        },
    ] {
        for total in [
            0,
            9007199254740991,
            u64::MAX as u128,
            u64::MAX as u128 + 1,
            u128::MAX,
        ] {
            let report = RunReportPayloadV6 {
                outcomes: vec![RunNodeReportV2 {
                    node: "gate".into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: "setup failed".into(),
                    },
                }],
                blocked_gates: vec!["gate".into()],
                verdict: RunVerdictV3::Incomplete {
                    missing_nodes: vec![MissingNodeV2 {
                        node: "gate".into(),
                        reason: "setup failed".into(),
                    }],
                },
                spent_tokens: total.into(),
                task_accounting: accounting.clone(),
                execution: execution.clone(),
            };
            report.validate().unwrap();
            let value = serde_json::to_value(&report).unwrap();
            assert_eq!(value["spent_tokens"], total.to_string());
            assert_valid("run-report-v6.json", &value);
            assert_eq!(
                serde_json::from_value::<RunReportPayloadV6>(value.clone()).unwrap(),
                report
            );
            for invalid in [
                json!(0),
                json!(null),
                json!(""),
                json!("01"),
                json!("-1"),
                json!("1.0"),
                json!("1e3"),
                json!("1\n"),
                json!("340282366920938463463374607431768211456"),
            ] {
                let mut changed = value.clone();
                changed["spent_tokens"] = invalid;
                assert_invalid("run-report-v6.json", &changed, "inexact cumulative charge");
                assert!(serde_json::from_value::<RunReportPayloadV6>(changed).is_err());
            }
            for invalid in [
                json!({"kind":"bound"}),
                json!({"kind":"unbound","cache_snapshots":[]}),
                json!({"kind":"unknown"}),
            ] {
                let mut changed = value.clone();
                changed["execution"] = invalid;
                assert_invalid("run-report-v6.json", &changed, "invalid execution contract");
                assert!(
                    serde_json::from_value::<RunReportPayloadV6>(changed)
                        .map_or(true, |report| report.validate().is_err())
                );
            }
        }
    }

    let binding = serde_json::to_value(binding).unwrap();
    let snapshot = json!({"node":"gate", "kind":"cargo", "source_digest":accounting.plan_id,
        "bytes":12, "files":1, "materialization":"copy"});
    let failure = json!({"node":"gate", "kind":"cargo", "reason":"policy_unavailable"});
    let incomplete = json!({
        "outcomes":[
            {"node":"gate", "outcome":{"kind":"failed", "error":"setup failed"}},
            {"node":"unstarted", "outcome":{"kind":"suppressed", "reason":"upstream_missing"}}
        ],
        "blocked_gates":["gate"],
        "verdict":{"kind":"incomplete", "missing_nodes":[
            {"node":"gate", "reason":"setup failed"},
            {"node":"unstarted", "reason":"upstream missing"}
        ]},
        "spent_tokens":"0", "task_accounting":accounting,
        "execution":{"kind":"cached", "execution_bindings":[binding],
            "cache_snapshots":[], "cache_failures":[failure]}
    });
    let read = |value: Value| {
        serde_json::from_value::<RunReportPayloadV6>(value)
            .is_ok_and(|report| report.validate().is_ok())
    };
    // Incomplete reports retain the captured variant even when no Gate started, or only
    // part of its execution facts exist. Exact receipt coverage is checked by Store.
    for execution in [
        json!({"kind":"bound", "execution_bindings":[]}),
        json!({"kind":"bound", "execution_bindings":[binding]}),
        json!({"kind":"cached", "execution_bindings":[], "cache_snapshots":[], "cache_failures":[]}),
        json!({"kind":"cached", "execution_bindings":[binding], "cache_snapshots":[], "cache_failures":[]}),
        json!({"kind":"cached", "execution_bindings":[binding], "cache_snapshots":[snapshot], "cache_failures":[]}),
        incomplete["execution"].clone(),
    ] {
        let mut value = incomplete.clone();
        value["execution"] = execution;
        assert_valid("run-report-v6.json", &value);
        assert!(read(value));
    }
    for verdict in [
        json!({"kind":"pass"}),
        json!({"kind":"fail", "reason":"not_converged"}),
        json!({"kind":"fail", "reason":"authority_unavailable"}),
        json!({"kind":"fail", "reason":"exhausted"}),
    ] {
        let mut complete = incomplete.clone();
        complete["verdict"] = verdict;
        complete["outcomes"] =
            json!([{"node":"gate", "outcome":{"kind":"completed", "output_artifacts":[]}}]);
        complete["blocked_gates"] = json!([]);
        for execution in [
            json!({"kind":"unbound"}),
            json!({"kind":"bound", "execution_bindings":[binding]}),
            json!({"kind":"cached", "execution_bindings":[binding], "cache_snapshots":[snapshot], "cache_failures":[]}),
        ] {
            complete["execution"] = execution;
            assert_valid("run-report-v6.json", &complete);
            assert!(read(complete.clone()));
            for field in ["execution_bindings", "cache_snapshots"] {
                if complete["execution"][field].is_array() {
                    let mut unknown = complete.clone();
                    unknown["execution"][field][0]["unknown"] = json!(true);
                    assert_invalid(
                        "run-report-v6.json",
                        &unknown,
                        "closed complete execution entries",
                    );
                    assert!(!read(unknown));
                    let mut undeclared = complete.clone();
                    undeclared["execution"][field][0]["node"] = json!("undeclared");
                    assert!(
                        !read(undeclared),
                        "complete facts require declared bound nodes"
                    );
                }
            }
        }
        for execution in [
            json!({"kind":"bound", "execution_bindings":[]}),
            json!({"kind":"cached", "execution_bindings":[], "cache_snapshots":[], "cache_failures":[]}),
            json!({"kind":"cached", "execution_bindings":[], "cache_snapshots":[snapshot], "cache_failures":[]}),
            json!({"kind":"cached", "execution_bindings":[binding], "cache_snapshots":[], "cache_failures":[]}),
        ] {
            complete["execution"] = execution;
            assert_invalid(
                "run-report-v6.json",
                &complete,
                "complete reports require execution evidence",
            );
            assert!(!read(complete.clone()));
        }
    }
    for (pointer, invalid) in [
        ("/execution/execution_bindings/0/node", json!("")),
        ("/execution/execution_bindings/0/provider", json!("unknown")),
        ("/execution/cache_failures/0/node", json!("")),
        ("/execution/cache_failures/0/reason", json!("unknown")),
    ] {
        let mut value = incomplete.clone();
        *value.pointer_mut(pointer).unwrap() = invalid;
        assert_invalid(
            "run-report-v6.json",
            &value,
            "incomplete entries remain validated",
        );
        assert!(!read(value));
    }
    for field in ["execution_bindings", "cache_snapshots", "cache_failures"] {
        let mut value = incomplete.clone();
        value["execution"]["cache_snapshots"] = json!([snapshot]);
        value["execution"]["cache_failures"] = json!([]);
        if field == "cache_failures" {
            value["execution"]["cache_snapshots"] = json!([]);
            value["execution"]["cache_failures"] = json!([failure]);
        }
        let mut unknown = value.clone();
        unknown["execution"][field][0]["unknown"] = json!(true);
        assert_invalid("run-report-v6.json", &unknown, "closed execution entries");
        assert!(!read(unknown));
        // Cross-entry identities and node relations are semantic Core checks, not schema rules.
        let mut duplicate = value.clone();
        let entry = duplicate["execution"][field][0].clone();
        duplicate["execution"][field]
            .as_array_mut()
            .unwrap()
            .push(entry);
        assert!(!read(duplicate));
        value["execution"][field][0]["node"] = json!("undeclared");
        assert!(!read(value));
    }
    let mut value = incomplete.clone();
    value["execution"]["cache_snapshots"] = json!([snapshot]);
    assert!(
        !read(value),
        "snapshot and failure identities cannot overlap"
    );
    let mut value = incomplete.clone();
    value["execution"]["execution_bindings"] = json!([]);
    assert!(
        !read(value),
        "cache failures still require a recorded binding"
    );
    let mut value = incomplete.clone();
    value["execution"]["execution_bindings"][0]["node"] = json!("unstarted");
    value["execution"]["cache_failures"][0]["node"] = json!("unstarted");
    assert!(
        !read(value),
        "cache failures still require a failed outcome"
    );
    let mut value = incomplete;
    value["execution"]["cache_failures"] = json!([]);
    value["execution"]["cache_snapshots"] = json!([snapshot]);
    value["execution"]["cache_snapshots"][0]["files"] = json!(0);
    assert_invalid(
        "run-report-v6.json",
        &value,
        "cache snapshot bounds remain validated",
    );
    assert!(!read(value));
}

#[path = "schema_parity/task_broker.rs"]
mod task_broker;

#[path = "schema_parity/task_provider_probe.rs"]
mod task_provider_probe;

#[path = "schema_parity/task_owned.rs"]
mod task_owned;
#[path = "schema_parity/task_review_handoff.rs"]
mod task_review_handoff;

#[path = "schema_parity/task_review_inspection.rs"]
mod task_review_inspection;

#[path = "schema_parity/task_contexts.rs"]
mod task_contexts;
#[path = "schema_parity/task_review_integration.rs"]
mod task_review_integration;

#[path = "schema_parity/review_outcome.rs"]
mod review_outcome;

#[path = "schema_parity/provider_doctor.rs"]
mod provider_doctor;

#[path = "schema_parity/task_recording.rs"]
mod task_recording;

#[test]
fn task_review_readable_subject_is_strict() {
    use review_core::task::review::*;
    let id = format!("sha256:{}", "a".repeat(64));
    let value = TaskReviewSubjectV2 {
        subject_id: id.clone(),
        subject: SubjectV1::diff(&id, &id, &id),
        snapshot_id: id.clone(),
        prior_history_id: id.clone(),
        continuation_id: None,
        round: 2,
        change_scope: Some(TaskReviewChangeScopeV1 {
            changed_paths: vec!["src/lib.rs".into()],
            renames: vec![],
            rename_detection_truncated: false,
            git_version: "fixture".into(),
            diff_policy_version: "fixture".into(),
            patch: TaskReviewFileV1 {
                path: TaskReviewFileV1::path_for(&id),
                content_id: id.clone(),
                bytes: 1_245_548,
            },
        }),
    };
    value.validate().unwrap();
    let encoded = serde_json::to_value(value).unwrap();
    assert_valid("task-review-subject-v2.json", &encoded);
    for (field, replacement) in [
        ("bytes", json!(4194305)),
        ("bytes", json!(-1)),
        ("bytes", json!(1.5)),
        ("path", json!("../escape")),
        ("content_id", json!("invented")),
    ] {
        let mut bad = encoded.clone();
        bad["change_scope"]["patch"][field] = replacement;
        assert_invalid("task-review-subject-v2.json", &bad, "invalid declared file");
        assert!(
            serde_json::from_value::<TaskReviewSubjectV2>(bad)
                .map_err(|e| e.to_string())
                .and_then(|v| v.validate())
                .is_err()
        );
    }
    let mut bad = encoded.clone();
    bad["change_scope"]["canonical_patch_base64"] = json!("AAAA");
    assert_invalid(
        "task-review-subject-v2.json",
        &bad,
        "no hidden inline patch",
    );
    assert!(serde_json::from_value::<TaskReviewSubjectV2>(bad).is_err());
    let assignment = TaskReviewAssignmentV1 {
        subject_id: id.clone(),
        prior_history_id: id,
        round: 2,
        reviewer: "correctness".into(),
        findings: vec![],
    };
    assignment.validate().unwrap();
    let encoded = serde_json::to_value(&assignment).unwrap();
    assert_valid("task-review-assignment-v1.json", &encoded);
    let mut bad = encoded;
    bad["all_reviewers"] = json!(true);
    assert_invalid(
        "task-review-assignment-v1.json",
        &bad,
        "assignments are source scoped",
    );
    assert!(serde_json::from_value::<TaskReviewAssignmentV1>(bad).is_err());
}

#[test]
fn task_catalog_review_generation_is_omitted_or_two() {
    // Task Review has one generation: an omitted selector and `generation = 2` both mean it.
    let mut value = json!({"schema":"af.task-catalog/1","code_policy":".af/code-policy.toml","packages":{"fixture/review":{"version":"1.0.0","digest":format!("sha256:{}","a".repeat(64)),"path":".af/packages/review"}},"independence":{"command_workers_by_package":true,"distinct_principals":true,"distinct_providers":false,"distinct_models":false},"review":{"reviewers":{"correctness":"required"},"gate":"major","clean_rounds":1,"max_rounds":2}});
    for generation in [1, 2] {
        let name = format!("task-catalog-v{generation}.json");
        value["schema"] = json!(format!("af.task-catalog/{generation}"));
        if generation == 2 {
            value["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
        }
        value["review"]
            .as_object_mut()
            .unwrap()
            .remove("generation");
        assert_valid(&name, &value);
        value["review"]["generation"] = json!(2);
        assert_valid(&name, &value);
        for invalid in [
            json!(0),
            json!(1),
            json!(3),
            json!(null),
            json!("2"),
            json!(4294967296_u64),
        ] {
            let mut bad = value.clone();
            bad["review"]["generation"] = invalid;
            assert_invalid(&name, &bad, "unsupported explicit Review generation");
        }
        if generation == 2 {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove("provider_admission");
            assert_invalid(
                &name,
                &bad,
                "Review generation does not waive Provider authority",
            );
        }
    }
}
