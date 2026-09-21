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

#[test]
fn optimization_economics_contracts_preserve_unknowns_and_pending_live_gates() {
    use review_core::task::optimization::*;
    let id = format!("sha256:{}", "1".repeat(64));
    let receipt = format!("sha256:{}", "2".repeat(64));
    let history = json!({
        "schema":"af.optimization-history/1", "project_id":id,
        "cutoff_unix_ms":"100", "receipts":[{
            "receipt_id":receipt,"adapter":"fixture","adapter_version":"v1",
            "project_id":id,"source_id":"fixture.jsonl","execution_id":"session-1",
            "byte_start":"0","byte_end":"0","prefix_digest":id,"cutoff_unix_ms":"100",
            "redaction_version":"v1","completeness":"complete"
        }], "observations":[], "gaps":["timing"]
    });
    assert_valid("optimization-history-v1.json", &history);
    serde_json::from_value::<OptimizationHistoryV1>(history.clone())
        .unwrap()
        .validate()
        .unwrap();
    let usage = json!({"chargeable_tokens":"0"});
    let economics = json!({
        "schema":"af.optimization-economics/1","project_id":id,"capture_ids":[id],
        "cutoff_unix_ms":"100","rows":[],"af_usage":usage,"outer_session_usage":usage,
        "active_ms":"0","elapsed_ms":"0","summed_work_ms":"0","verified":0,
        "failed_or_incomplete":0,"repeated_failures":0,"missing_fields":["timing"],
        "cache_economics":{"cargo":{"eligible":1,"ineligible":0,"hits":1,"misses":0,
            "unknown_results":0,"cold":0,"warm":1,"unknown_temperature":0,
            "bytes_reused":"10","invalidation_ids":["Cargo.lock:fixture"],
            "missing_fields":["lookup_time","tokens_reused","warmup_time"]}}
    });
    assert_valid("optimization-economics-v1.json", &economics);
    serde_json::from_value::<OptimizationEconomicsV1>(economics.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut invalid_history = history;
    invalid_history["gaps"] = json!(["missing\n* injected"]);
    assert_invalid(
        "optimization-history-v1.json",
        &invalid_history,
        "gaps are identifiers",
    );
    assert!(
        serde_json::from_value::<OptimizationHistoryV1>(invalid_history)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut invalid_economics = economics;
    invalid_economics["missing_fields"] = json!(["invalid field"]);
    assert_invalid(
        "optimization-economics-v1.json",
        &invalid_economics,
        "missing fields are identifiers",
    );
    assert!(
        serde_json::from_value::<OptimizationEconomicsV1>(invalid_economics)
            .unwrap()
            .validate()
            .is_err()
    );
    let report = json!({"schema":"af.optimization-report/1","economics_id":id,
        "project_id":id,"status":"partial","summary":"Timing is unknown.","highlights":[],
        "missing_measurements":["timing"]});
    assert_valid("optimization-report-v1.json", &report);
    let report: OptimizationReportV1 = serde_json::from_value(report).unwrap();
    report.validate().unwrap();
    assert!(
        report
            .render_markdown()
            .unwrap()
            .contains("Live paid demonstrations and adoption observations are still pending.")
    );
    let policy = json!({"schema":"af.optimization-policy/1","project_id":id,
        "strategy":"light","max_sessions":200,"max_raw_bytes":"268435456",
        "max_record_bytes":"1048576","max_normalized_bytes":"16777216"});
    assert_valid("optimization-policy-v1.json", &policy);
    serde_json::from_value::<OptimizationPolicyV1>(policy)
        .unwrap()
        .validate()
        .unwrap();
}

#[test]
fn document_contracts_keep_source_data_closed_and_never_use_code_snapshots() {
    use review_core::task::document::*;
    let sources = json!({"schema":"af.document-sources/1","sources":{"ticket":{"title":"Pagination","uri":"https://example.invalid/AF-42","revision":"42@1","text":"Add offset pagination."}}});
    assert_valid("document-sources-v1.json", &sources);
    serde_json::from_value::<DocumentSourcesV1>(sources.clone())
        .unwrap()
        .validate()
        .unwrap();
    let draft = json!({"schema":"af.document-draft/1","title":"Release notes","sections":[{"heading":"Summary","body":"Adds offset pagination."}],"citations":["ticket"]});
    assert_valid("document-draft-v1.json", &draft);
    serde_json::from_value::<DocumentDraftV1>(draft.clone())
        .unwrap()
        .validate()
        .unwrap();
    for (schema, value) in [
        ("document-sources-v1.json", sources),
        ("document-draft-v1.json", draft.clone()),
    ] {
        let mut invalid = value;
        invalid["allowed_effects"] = json!(["write-source"]);
        assert_invalid(
            schema,
            &invalid,
            "document source/draft data cannot grant execution effects",
        );
    }
    let mut duplicate = draft;
    let section = duplicate["sections"][0].clone();
    duplicate["sections"].as_array_mut().unwrap().push(section);
    assert!(
        serde_json::from_value::<DocumentDraftV1>(duplicate)
            .unwrap()
            .validate()
            .is_err()
    );
    let id = format!("sha256:{}", "1".repeat(64));
    let document = json!({"schema":"af.document/1","draft_id":id,"sources_id":id,"format":"markdown","text":"# Release notes\n"});
    assert_valid("document-v1.json", &document);
    serde_json::from_value::<DocumentV1>(document.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut invalid = document;
    invalid["snapshot_id"] = json!(id);
    assert_invalid(
        "document-v1.json",
        &invalid,
        "a document is not a fabricated code Snapshot",
    );
    let evaluation = json!({"document_id":id,"sources_id":id,"requirements_id":id,"check_receipt_id":id,"outcome":"passed","summary":"Exact sources verified."});
    assert_valid("document-evaluation-v1.json", &evaluation);
    serde_json::from_value::<DocumentEvaluationV1>(evaluation.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut missing = evaluation;
    missing.as_object_mut().unwrap().remove("document_id");
    assert_invalid(
        "document-evaluation-v1.json",
        &missing,
        "evaluation requires exact document identity",
    );
}

fn fixture(name: &str) -> Value {
    let path = workspace_root()
        .join("fixtures/task-contracts/v1")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[path = "../support/task_fixtures.rs"]
mod corpus;

#[test]
fn compiler_feedback_retains_a_bounded_exact_proposal_and_closes_its_fields() {
    use review_core::task::feedback::*;
    let id = format!("sha256:{}", "1".repeat(64));
    let value = json!({"attempt_id":"01AAAAAAAAAAAAAAAAAAAAAAAA","contract_id":id,"code":"compiler_rejected",
        "compiler":{"proposal_id":id,"proposal":{"schema":"af.pipeline-proposal/1","root":"generated/task","definitions":{"generated/task":"invalid TOML awaits compiler repair"}},"diagnostics":["Missing public output verification"]}});
    assert_valid("task-retry-feedback-v1.json", &value);
    serde_json::from_value::<TaskRetryFeedbackV1>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    for case in ["missing", "null", "extra", "wrong_code", "diagnostics"] {
        let mut invalid = value.clone();
        match case {
            "missing" => {
                invalid.as_object_mut().unwrap().remove("compiler");
            }
            "null" => invalid["compiler"] = Value::Null,
            "extra" => invalid["compiler"]["transcript"] = json!("undeclared history"),
            "wrong_code" => invalid["code"] = json!("provider_failure"),
            _ => invalid["compiler"]["diagnostics"] = json!([]),
        }
        assert_invalid("task-retry-feedback-v1.json", &invalid, case);
        assert!(
            serde_json::from_value::<TaskRetryFeedbackV1>(invalid)
                .map_err(|e| e.to_string())
                .and_then(|f| f.validate())
                .is_err()
        );
    }
}

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
    for kind in ["submitted", "ready", "running"] {
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
        TaskExecutionV1::Exhausted,
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

#[test]
fn repair_context_and_decisions_bind_every_current_view_and_keep_original_provenance() {
    use review_core::task::repair::*;
    use review_core::task::review::*;
    let id = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let invocation = review_core::task::execution::TaskInvocationV1 {
        plan_id: id('a'),
        node: "root.nodes.attest".into(),
        inputs: Default::default(),
    };
    let attestation = review_core::ChangeAttestationV1 {
        finding_id: "finding-one".into(),
        expected_finding_view_id: id('b'),
        subject_id: id('c'),
        change_set_id: Some(id('d')),
        changed_regions: vec![review_core::ChangedRegionV1 {
            path: "pagination.py".into(),
            start_line: None,
            end_line: None,
        }],
        actor: "af/attest-fixes".into(),
        reason: "Sealed S1-to-S2 diff".into(),
        evidence_ids: vec![id('e')],
    };
    let context = TaskRepairContextV1 {
        invocation: invocation.clone(),
        continuation_id: id('f'),
        continuation: VerificationContinuationV1 {
            task_revision_id: id('1'),
            plan_id: id('a'),
            prior_history_id: id('2'),
            previous_subject_id: id('3'),
            current_subject_id: id('c'),
            current_snapshot_id: id('4'),
            policy_id: id('5'),
            claims: std::collections::BTreeMap::from([("finding-one".into(), id('b'))]),
        },
        subject: review_core::SubjectV1::diff(id('4'), id('6'), id('7')),
        previous_snapshot_id: id('8'),
        claims: std::collections::BTreeMap::from([(
            "finding-one".into(),
            TaskRepairClaimV1 {
                file: "pagination.py".into(),
                line: Some(1),
                original_view_id: id('9'),
                current_view_id: id('b'),
                title: "Negative offset".into(),
                body: "Must reject negative offset".into(),
                remedy: "Raise ValueError".into(),
                attestation_id: id('0'),
                attestation,
            },
        )]),
    };
    context.validate().unwrap();
    assert_valid(
        "task-repair-context-v1.json",
        &serde_json::to_value(&context).unwrap(),
    );
    let decision = TaskFixDecisionV1 {
        expected_view_id: id('b'),
        attestation_id: id('0'),
        outcome: VerificationOutcomeV1::Positive,
        reason: "Executed original regression case on S2".into(),
    };
    let verified = TaskFixVerificationV1 {
        continuation_id: id('f'),
        subject_id: id('c'),
        claims: std::collections::BTreeMap::from([("finding-one".into(), decision.clone())]),
    };
    verified.validate_context(&context).unwrap();
    assert_valid(
        "task-fix-verification-v1.json",
        &serde_json::to_value(&verified).unwrap(),
    );
    for case in [
        "missing",
        "extra",
        "old_view",
        "old_subject",
        "another_continuation",
    ] {
        let mut changed = verified.clone();
        match case {
            "missing" => changed.claims.clear(),
            "extra" => {
                changed
                    .claims
                    .insert("unrelated-finding".into(), decision.clone());
            }
            "old_view" => {
                changed
                    .claims
                    .get_mut("finding-one")
                    .unwrap()
                    .expected_view_id = context.claims["finding-one"].original_view_id.clone()
            }
            "old_subject" => changed.subject_id = context.continuation.previous_subject_id.clone(),
            _ => changed.continuation_id = id('1'),
        }
        assert!(changed.validate_context(&context).is_err(), "{case}");
    }
    let mut receipt = TaskFixReceiptV1 {
        invocation,
        finding_id: "finding-one".into(),
        continuation_id: id('f'),
        subject_id: id('c'),
        decision,
        verifier_output_id: Some(id('2')),
    };
    receipt.validate().unwrap();
    assert_valid(
        "task-fix-receipt-v1.json",
        &serde_json::to_value(&receipt).unwrap(),
    );
    receipt.verifier_output_id = None;
    assert!(
        receipt.validate().is_err(),
        "Missing verifier cannot certify a fix"
    );
    receipt.decision.outcome = VerificationOutcomeV1::Inconclusive;
    receipt.validate().unwrap();
    assert_valid(
        "task-fix-receipt-v1.json",
        &serde_json::to_value(&receipt).unwrap(),
    );
    let claims = TaskReviewClaimsV1 {
        round_report_id: id('1'),
        snapshot_id: id('8'),
        claims: std::collections::BTreeMap::from([(
            "finding-one".into(),
            TaskReviewClaimV1 {
                file: "pagination.py".into(),
                line: Some(1),
                view_id: id('9'),
                title: "Negative offset".into(),
                body: "Original claim".into(),
                remedy: "Reject negative input".into(),
            },
        )]),
    };
    claims.validate().unwrap();
    assert_valid(
        "task-review-claims-v1.json",
        &serde_json::to_value(claims).unwrap(),
    );
}

#[test]
fn fixed_planning_preparation_cannot_claim_business_acceptance_or_generated_authority() {
    use review_core::task::plan::PlanPreparationV1;
    let original = fixture("execution-plan");
    let mut plan: ExecutionPlanV1 = serde_json::from_value(original.clone()).unwrap();
    assert!(plan.preparation.is_none());
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        original,
        "Adding preparation changed frozen execution identity"
    );
    let original_coverage = plan.acceptance.clone();
    let original_origins = plan.generated_origins.clone();
    plan.preparation = Some(PlanPreparationV1::Planning {});
    plan.acceptance.clear();
    plan.generated_origins.clear();
    plan.validate().unwrap();
    let valid = serde_json::to_value(&plan).unwrap();
    assert_valid("execution-plan-v1.json", &valid);
    let mut invalid = valid.clone();
    invalid["preparation"] = Value::Null;
    assert_invalid(
        "execution-plan-v1.json",
        &invalid,
        "Present preparation cannot be null",
    );
    assert!(serde_json::from_value::<ExecutionPlanV1>(invalid).is_err());
    let mut invalid = valid;
    invalid["preparation"]["auto_approved"] = json!(true);
    assert_invalid(
        "execution-plan-v1.json",
        &invalid,
        "Preparation cannot smuggle approval",
    );
    assert!(serde_json::from_value::<ExecutionPlanV1>(invalid).is_err());
    plan.acceptance = original_coverage;
    assert!(plan.validate().is_err());
    assert_invalid(
        "execution-plan-v1.json",
        &serde_json::to_value(&plan).unwrap(),
        "Preparation cannot certify business acceptance",
    );
    plan.acceptance.clear();
    plan.generated_origins = original_origins;
    assert!(!plan.generated_origins.is_empty());
    assert!(plan.validate().is_err());
    assert_invalid(
        "execution-plan-v1.json",
        &serde_json::to_value(&plan).unwrap(),
        "Bootstrap is fixed, never generated",
    );
}

#[test]
fn proposals_are_bounded_pipeline_toml_and_never_worker_installation_or_approval() {
    use review_core::task::planning::PipelineProposalV1;
    let proposal = PipelineProposalV1 {
        schema: "af.pipeline-proposal/1".into(),
        root: "generated/implementation".into(),
        definitions: std::collections::BTreeMap::from([(
            "generated/implementation".into(),
            "schema = \"af.pipeline/1\"\n".into(),
        )]),
    };
    proposal.validate().unwrap();
    let value = serde_json::to_value(&proposal).unwrap();
    assert_valid("pipeline-proposal-v1.json", &value);
    for field in ["workers", "approved", "limits"] {
        let mut invalid = value.clone();
        invalid[field] = json!({});
        assert_invalid(
            "pipeline-proposal-v1.json",
            &invalid,
            "Proposal cannot install other authority",
        );
        assert!(serde_json::from_value::<PipelineProposalV1>(invalid).is_err());
    }
    let mut invalid = proposal.clone();
    invalid.root = "generated/absent".into();
    assert!(invalid.validate().is_err());
    let mut invalid = proposal.clone();
    invalid.definitions.clear();
    assert!(invalid.validate().is_err());
    assert_invalid(
        "pipeline-proposal-v1.json",
        &serde_json::to_value(invalid).unwrap(),
        "Proposal must contain a definition",
    );
    let mut invalid = proposal;
    invalid.root = "project/existing".into();
    invalid
        .definitions
        .insert("project/existing".into(), "name = 'existing'".into());
    assert!(invalid.validate().is_err());
    assert_invalid(
        "pipeline-proposal-v1.json",
        &serde_json::to_value(invalid).unwrap(),
        "Generated code cannot shadow captured package names",
    );
}
