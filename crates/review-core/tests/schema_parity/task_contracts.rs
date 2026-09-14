use super::{assert_invalid, assert_valid, workspace_root};
use review_core::task::{
    ArtifactInputV1, TaskAcceptanceV1, TaskExecutionV1, TaskPhaseV1,
    plan::{
        EffectiveWorkerBindingV1, ExecutionPlanV1, IndependencePolicyV1, PlanDecisionV1,
        WorkerExecutionV1, validate_independent_bindings,
    },
    review::{RepairAssessmentV1, ReviewConclusionV1, ReviewHistoryV1, VerificationContinuationV1},
};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = workspace_root()
        .join("fixtures/task-contracts/v1")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[path = "../support/task_fixtures.rs"]
mod corpus;

fn typed(contract: &str, value: Value) -> Result<(), String> {
    corpus::typed_round_trip(contract, value).map(|_| ())
}

#[test]
fn task_contract_positive_and_negative_fixtures() {
    let manifest = fixture("content-ids");
    for name in manifest.as_object().unwrap().keys() {
        let contract = name.strip_suffix(".json").unwrap();
        let value = fixture(contract);
        assert_valid(&format!("{contract}-v1.json"), &value);
        typed(contract, value).unwrap_or_else(|e| panic!("{contract}: {e}"));
    }
    for case in fixture("negative").as_array().unwrap() {
        let contract = case["contract"].as_str().unwrap();
        let why = case["why"].as_str().unwrap();
        let value = corpus::expand(&fixture(contract), case);
        match case["layer"].as_str() {
            Some("shape") => assert_invalid(&format!("{contract}-v1.json"), &value, why),
            Some("semantic") => assert_valid(&format!("{contract}-v1.json"), &value),
            _ => panic!("unknown negative fixture layer"),
        }
        assert!(
            typed(contract, value.clone()).is_err(),
            "Rust admitted {contract}: {why}"
        );
    }
}

#[test]
fn many_inputs_and_explicit_empty_history_are_valid() {
    let input =
        json!({"artifact_ids": [], "artifact_type":"af/Requirements@1", "cardinality":"many"});
    serde_json::from_value::<ArtifactInputV1>(input)
        .unwrap()
        .validate()
        .unwrap();
    let history = json!({"kind":"empty"});
    assert_valid("review-history-v1.json", &history);
    typed("review-history", history).unwrap();
}

#[test]
fn empty_tagged_variants_reject_hidden_fields() {
    use review_core::task::pipeline::{PortAffinityV1, TaskOperatorV1};
    for kind in [
        "submitted",
        "resolving",
        "planning",
        "ready",
        "running",
        "verifying",
    ] {
        assert!(
            serde_json::from_value::<TaskPhaseV1>(json!({"kind":kind,"plan":"hidden"})).is_err()
        );
    }
    for op in ["seal", "review_bind", "attest_fixes", "select"] {
        assert!(
            serde_json::from_value::<TaskOperatorV1>(json!({"op":op,"script":"hidden"})).is_err()
        );
    }
    assert!(
        serde_json::from_value::<PortAffinityV1>(json!({"kind":"unbound","input":"hidden"}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<ReviewHistoryV1>(json!({"kind":"empty","subject_id":"hidden"}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<WorkerExecutionV1>(json!({"kind":"command","model":"hidden"}))
            .is_err()
    );
}

#[test]
fn optional_properties_accept_omission_but_reject_explicit_null() {
    for (contract, parent, field) in [
        ("task-revision", "/inputs/requirements", "snapshot_id"),
        ("task-revision", "", "previous_revision_id"),
        ("task-revision", "", "pipeline"),
        (
            "pipeline-definition",
            "/contract/inputs/requirements",
            "root_default",
        ),
        ("pipeline-definition", "/nodes/0", "when"),
    ] {
        let mut value = fixture(contract);
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_valid(&format!("{contract}-v1.json"), &value);
        typed(contract, value.clone()).unwrap();
        value.pointer_mut(parent).unwrap()[field] = Value::Null;
        assert_invalid(
            &format!("{contract}-v1.json"),
            &value,
            "optional means absent or a typed value",
        );
        assert!(
            typed(contract, value).is_err(),
            "{contract} accepted null {field}"
        );
    }
}

#[test]
fn output_affinity_can_reference_an_input_with_the_same_name() {
    let mut value = fixture("pipeline-definition");
    value["contract"]["outputs"]["document"]["affinity"] =
        json!({"kind":"same_as","input":"document"});
    value["contract"]["inputs"]["document"] = value["contract"]["inputs"]["requirements"].clone();
    typed("pipeline-definition", value).unwrap();
}

#[test]
fn approval_matches_only_exact_revision_plan_and_policy_and_never_dispatches() {
    let mut plan: ExecutionPlanV1 = serde_json::from_value(fixture("execution-plan")).unwrap();
    let approval: PlanDecisionV1 = serde_json::from_value(fixture("plan-decision")).unwrap();
    assert!(plan.requires_developer_approval());
    assert!(approval.approves(&approval.plan_id, &plan));
    assert!(!approval.approves(&plan.engine_id, &plan));
    plan.task_revision_id = plan.engine_id.clone();
    assert!(!approval.approves(&approval.plan_id, &plan));
    plan.task_revision_id = approval.task_revision_id.clone();
    plan.authority.policy_id = plan.engine_id.clone();
    assert!(!approval.approves(&approval.plan_id, &plan));
}

#[test]
fn provider_aliases_and_multi_role_packages_cannot_bypass_independence() {
    let plan: ExecutionPlanV1 = serde_json::from_value(fixture("execution-plan")).unwrap();
    let mut a: EffectiveWorkerBindingV1 = plan.bindings["author"].clone();
    a.execution = WorkerExecutionV1::Model {
        provider: "personal".into(),
        provider_kind: "claude".into(),
        principal_id: "account:one".into(),
        model: "fable".into(),
        effort: "high".into(),
    };
    let mut b = a.clone();
    let policy = IndependencePolicyV1::default();
    assert!(validate_independent_bindings(&a, &b, policy).is_err());
    assert!(
        validate_independent_bindings(
            &a,
            &b,
            IndependencePolicyV1 {
                distinct_principals: false,
                distinct_providers: true,
                distinct_models: false,
                ..policy
            }
        )
        .is_err()
    );
    b.package_digest = plan.engine_id.clone();
    if let WorkerExecutionV1::Model { provider, .. } = &mut b.execution {
        *provider = "alias".into();
    }
    assert!(validate_independent_bindings(&a, &b, policy).is_err());
    assert!(
        validate_independent_bindings(
            &a,
            &b,
            IndependencePolicyV1 {
                distinct_principals: false,
                ..policy
            }
        )
        .is_ok()
    );
    if let WorkerExecutionV1::Model { principal_id, .. } = &mut b.execution {
        *principal_id = "account:two".into();
    }
    assert!(validate_independent_bindings(&a, &b, policy).is_ok());
    b.execution = WorkerExecutionV1::Command {};
    assert!(validate_independent_bindings(&a, &b, policy).is_ok());
    a.execution = WorkerExecutionV1::Command {};
    assert!(validate_independent_bindings(&a, &b, policy).is_ok());
    assert!(
        validate_independent_bindings(
            &a,
            &b,
            IndependencePolicyV1 {
                command_workers_by_package: false,
                ..policy
            }
        )
        .is_err()
    );
    assert!(
        validate_independent_bindings(
            &a,
            &b,
            IndependencePolicyV1 {
                distinct_models: true,
                ..policy
            }
        )
        .is_err()
    );
}

#[test]
fn review_exit_preserves_findings_and_missing_output_precedence() {
    let cases = [
        (
            ReviewConclusionV1::Pass,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Satisfied,
            0,
        ),
        (
            ReviewConclusionV1::ChangesRequested,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Satisfied,
            3,
        ),
        (
            ReviewConclusionV1::ChangesRequested,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Unsatisfied,
            3,
        ),
        (
            ReviewConclusionV1::ConvergenceExhausted,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Satisfied,
            3,
        ),
        (
            ReviewConclusionV1::ConvergenceExhausted,
            TaskExecutionV1::Exhausted,
            TaskAcceptanceV1::Unsatisfied,
            3,
        ),
        (
            ReviewConclusionV1::Incomplete,
            TaskExecutionV1::Exhausted,
            TaskAcceptanceV1::Inconclusive,
            4,
        ),
        (
            ReviewConclusionV1::Incomplete,
            TaskExecutionV1::Cancelled,
            TaskAcceptanceV1::Inconclusive,
            4,
        ),
    ];
    for (conclusion, execution, acceptance, exit) in cases {
        conclusion
            .validate_result(
                execution,
                acceptance,
                conclusion != ReviewConclusionV1::Incomplete,
            )
            .unwrap();
        assert_eq!(conclusion.exit_code(), exit);
    }
    assert!(
        ReviewConclusionV1::Incomplete
            .validate_result(
                TaskExecutionV1::Incomplete,
                TaskAcceptanceV1::Satisfied,
                false
            )
            .is_err()
    );
    assert!(
        ReviewConclusionV1::ChangesRequested
            .validate_result(
                TaskExecutionV1::Incomplete,
                TaskAcceptanceV1::Unsatisfied,
                false
            )
            .is_err()
    );
    assert!(
        ReviewConclusionV1::ConvergenceExhausted
            .validate_result(
                TaskExecutionV1::Exhausted,
                TaskAcceptanceV1::Satisfied,
                false
            )
            .is_err()
    );
}

#[test]
fn repair_receipts_cannot_change_snapshot_or_drop_original_claims() {
    let continuation: VerificationContinuationV1 =
        serde_json::from_value(fixture("verification-continuation")).unwrap();
    let mut assessment: RepairAssessmentV1 =
        serde_json::from_value(fixture("repair-assessment")).unwrap();
    let id = assessment.continuation_id.clone();
    assessment
        .validate_continuation(&id, &continuation)
        .unwrap();
    assessment.current_subject_id = continuation.previous_subject_id.clone();
    assert!(
        assessment
            .validate_continuation(&id, &continuation)
            .is_err()
    );
    assessment.current_subject_id = continuation.current_subject_id.clone();
    assessment
        .claims
        .get_mut("finding-1")
        .unwrap()
        .expected_view_id = continuation.plan_id.clone();
    assert!(
        assessment
            .validate_continuation(&id, &continuation)
            .is_err()
    );
    assessment.claims.clear();
    assert!(
        assessment
            .validate_continuation(&id, &continuation)
            .is_err()
    );
}

#[test]
fn missing_review_receipts_cannot_be_relabeled_as_convergence_exhaustion() {
    for execution in [
        TaskExecutionV1::Completed,
        TaskExecutionV1::Incomplete,
        TaskExecutionV1::Blocked,
        TaskExecutionV1::Exhausted,
        TaskExecutionV1::Cancelled,
    ] {
        for acceptance in [
            TaskAcceptanceV1::Satisfied,
            TaskAcceptanceV1::Unsatisfied,
            TaskAcceptanceV1::Inconclusive,
        ] {
            for conclusion in [
                ReviewConclusionV1::Pass,
                ReviewConclusionV1::ChangesRequested,
                ReviewConclusionV1::ConvergenceExhausted,
                ReviewConclusionV1::Incomplete,
            ] {
                let expected = conclusion == ReviewConclusionV1::Incomplete
                    && execution != TaskExecutionV1::Completed
                    && acceptance != TaskAcceptanceV1::Satisfied;
                assert_eq!(
                    conclusion
                        .validate_result(execution, acceptance, false)
                        .is_ok(),
                    expected,
                    "{conclusion:?}/{execution:?}/{acceptance:?}"
                );
                if expected {
                    assert_eq!(conclusion.exit_code(), 4);
                }
                if conclusion == ReviewConclusionV1::Incomplete {
                    assert!(
                        conclusion
                            .validate_result(execution, acceptance, true)
                            .is_err()
                    );
                }
            }
        }
    }
}
