use std::collections::{BTreeMap, BTreeSet};

use review_core::task::optimization_experiment::ComparisonConclusionV1;
use review_core::task::optimization_light::*;

fn id(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn cost(tokens: u64, time_ms: u64) -> OptimizationCostV1 {
    OptimizationCostV1 { tokens, time_ms }
}

fn recipe(support: OptimizationRecipeSupportV1) -> OptimizationRecipeV1 {
    OptimizationRecipeV1 {
        recipe_id: "context_dedup".into(),
        capability: OptimizationRecipeCapabilityV1::Context,
        support,
        applicability: BTreeSet::from(["repeated_context".into()]),
        required_observations: BTreeSet::from(["context_tokens".into()]),
        writable_effects: BTreeSet::from(["project_configuration".into()]),
        validation: BTreeSet::from(["matched_protected_trials".into()]),
        invalidation: BTreeSet::from(["source_policy_toolchain".into()]),
        payoff_basis: "Measured context removed per verified comparable run.".into(),
        upstream_work: (support == OptimizationRecipeSupportV1::Unsupported)
            .then(|| "No installed context edit hook.".into()),
    }
}

#[test]
fn light_proposal_requires_an_installed_registered_recipe() {
    let catalog = OptimizationRecipeCatalogV1 {
        schema: "af.optimization-recipe-catalog/1".into(),
        catalog_version: id('1'),
        recipes: vec![recipe(OptimizationRecipeSupportV1::Installed)],
    };
    let proposal = OptimizationProposalV1 {
        schema: "af.optimization-proposal/1".into(),
        profile_id: id('2'),
        diagnostic_id: id('3'),
        recipe_catalog_id: id('1'),
        recipe_id: "context_dedup".into(),
        hypothesis: "Remove one duplicated declared context input.".into(),
        edits: BTreeMap::from([(
            ".af/workers/implementer/worker.toml".into(),
            OptimizationCandidateEditV1 {
                text: "schema = \"af.worker/2\"\n".into(),
                executable: false,
            },
        )]),
        candidate_binding: None,
        objective_exception: None,
        expected: OptimizationExpectedEconomicsV1 {
            comparable_future_runs: 10.into(),
            gross_token_savings_per_run: 100.into(),
            gross_time_savings_ms_per_run: 0.into(),
            recurring_tokens_per_run: 0.into(),
            recurring_time_ms_per_run: 0.into(),
            maximum_validation_tokens: 500.into(),
            maximum_validation_time_ms: 10_000.into(),
        },
    };
    proposal
        .validate_against(&id('2'), &id('3'), &catalog)
        .unwrap();
    let unsupported = OptimizationRecipeCatalogV1 {
        recipes: vec![recipe(OptimizationRecipeSupportV1::Unsupported)],
        ..catalog
    };
    assert!(
        proposal
            .validate_against(&id('2'), &id('3'), &unsupported)
            .unwrap_err()
            .contains("unsupported")
    );
}

#[test]
fn economics_separates_gross_recurring_one_off_and_each_break_even() {
    let economics = OptimizationRealizedEconomicsV1::calculate(
        cost(1_000, 10_000),
        cost(700, 8_000),
        cost(50, 500),
        cost(1_000, 6_000),
    )
    .unwrap();
    assert_eq!(economics.gross_token_savings_per_run, 300);
    assert_eq!(economics.gross_time_savings_ms_per_run, 2_000);
    assert_eq!(economics.token_break_even_runs.unwrap().get(), 4);
    assert_eq!(economics.time_break_even_runs.unwrap().get(), 4);
    assert!(!economics.pays_back(3));
    assert!(economics.pays_back(4));

    let negative = OptimizationRealizedEconomicsV1::calculate(
        cost(100, 10),
        cost(200, 20),
        cost(0, 0),
        cost(1, 1),
    )
    .unwrap();
    assert!(negative.token_break_even_runs.is_none());
    assert!(negative.time_break_even_runs.is_none());

    let token_loss_with_time_payback = OptimizationRealizedEconomicsV1::calculate(
        cost(100, 1_000),
        cost(200, 500),
        cost(0, 0),
        cost(10, 100),
    )
    .unwrap();
    assert!(token_loss_with_time_payback.token_break_even_runs.is_none());
    assert_eq!(
        token_loss_with_time_payback
            .time_break_even_runs
            .unwrap()
            .get(),
        1
    );
    assert!(
        !token_loss_with_time_payback.pays_back(10),
        "time savings alone require an explicit latency or correctness exception"
    );
}

#[test]
fn economics_normalizes_matched_trials_and_refuses_lossy_boundaries() {
    let economics = OptimizationRealizedEconomicsV1::calculate_normalized(
        cost(2_000, 20_000),
        cost(1_400, 16_000),
        cost(50, 500),
        cost(1_000, 6_000),
        2,
    )
    .unwrap();
    assert_eq!(economics.gross_token_savings_per_run, 300);
    assert_eq!(economics.gross_time_savings_ms_per_run, 2_000);
    assert_eq!(economics.normalization_units.get(), 2);

    assert!(
        OptimizationRealizedEconomicsV1::calculate_normalized(
            cost(10, 10),
            cost(9, 9),
            cost(0, 0),
            cost(0, 0),
            2,
        )
        .unwrap_err()
        .contains("exactly representable")
    );
    assert!(
        OptimizationRealizedEconomicsV1::calculate(
            cost(u64::MAX, 0),
            cost(0, 0),
            cost(0, 0),
            cost(0, 0),
        )
        .unwrap_err()
        .contains("signed wire domain")
    );
}

#[test]
fn proposal_rejects_binding_when_recipe_has_no_installed_binding_hook() {
    let catalog = OptimizationRecipeCatalogV1 {
        schema: "af.optimization-recipe-catalog/1".into(),
        catalog_version: id('1'),
        recipes: vec![recipe(OptimizationRecipeSupportV1::Installed)],
    };
    let mut proposal = OptimizationProposalV1 {
        schema: "af.optimization-proposal/1".into(),
        profile_id: id('2'),
        diagnostic_id: id('3'),
        recipe_catalog_id: id('1'),
        recipe_id: "context_dedup".into(),
        hypothesis: "Try a binding that this recipe cannot install.".into(),
        edits: BTreeMap::from([(
            ".af/workers/implementer/worker.toml".into(),
            OptimizationCandidateEditV1 {
                text: "schema = \"af.worker/2\"\n".into(),
                executable: false,
            },
        )]),
        candidate_binding: Some(OptimizationCandidateBindingV1 {
            package: "project/implementer".into(),
            provider_kind: "codex".into(),
            model: "gpt-test".into(),
            effort: "high".into(),
        }),
        objective_exception: None,
        expected: OptimizationExpectedEconomicsV1 {
            comparable_future_runs: 10.into(),
            gross_token_savings_per_run: 100.into(),
            gross_time_savings_ms_per_run: 0.into(),
            recurring_tokens_per_run: 0.into(),
            recurring_time_ms_per_run: 0.into(),
            maximum_validation_tokens: 500.into(),
            maximum_validation_time_ms: 10_000.into(),
        },
    };
    assert!(
        proposal
            .validate_against(&id('2'), &id('3'), &catalog)
            .is_err()
    );
    proposal.candidate_binding = None;
    proposal
        .validate_against(&id('2'), &id('3'), &catalog)
        .unwrap();
}

#[test]
fn adoption_observation_marks_edits_and_cannot_claim_causality() {
    let receipt = OptimizationAdoptionReceiptV1 {
        schema: "af.optimization-adoption-receipt/1".into(),
        task_id: "optimize-fixture".into(),
        result_id: id('1'),
        source_snapshot_id: id('2'),
        delivered_snapshot_id: id('3'),
        delivery_record_id: id('4'),
        delivered_tree_id: id('5'),
    };
    receipt.validate().unwrap();
    let mut observation = OptimizationAdoptionObservationV1 {
        schema: "af.optimization-adoption-observation/1".into(),
        adoption_receipt_id: id('6'),
        commit_snapshot_id: id('7'),
        commit_tree_id: id('8'),
        equivalence: AdoptionEquivalenceV1::Edited,
        observed_unix_ms: 10.into(),
        workload_id: id('9'),
        model_id: id('a'),
        engine_id: id('b'),
        environment_id: id('c'),
        causal_claim: false,
    };
    observation.validate().unwrap();
    observation.causal_claim = true;
    assert!(observation.validate().is_err());

    let economics = OptimizationRealizedEconomicsV1::calculate(
        cost(100, 100),
        cost(50, 50),
        cost(0, 0),
        cost(10, 10),
    )
    .unwrap();
    let result = OptimizationResultV1 {
        schema: "af.optimization-result/1".into(),
        source_snapshot_id: id('1'),
        candidate_snapshot_id: id('2'),
        profile_id: id('3'),
        proposal_id: id('4'),
        comparison_id: id('5'),
        evaluation_id: id('6'),
        verification_id: id('7'),
        conclusion: OptimizationResultConclusionV1::Validated,
        experiment_conclusion: ComparisonConclusionV1::Accepted,
        economics,
        expected_comparable_workload: 1.into(),
        objective_exception: None,
        adoption_offered: true,
    };
    result.validate().unwrap();
}

#[test]
fn adoption_task_evidence_retains_failed_attempts_missing_fields_and_no_causal_claim() {
    let mut evidence = OptimizationAdoptionTaskEvidenceV1 {
        schema: "af.optimization-adoption-task-evidence/1".into(),
        adoption_receipt_id: id('1'),
        commit_snapshot_id: id('2'),
        observed_task_id: "later-task".into(),
        observed_task_revision_id: id('3'),
        observed_task_result_id: id('4'),
        observed_plan_id: id('5'),
        outcome: "inconclusive".into(),
        attempt_ids: vec!["attempt-1".into(), "attempt-2".into()],
        unsuccessful_attempt_ids: vec!["attempt-2".into()],
        usage_ids: vec![id('6')],
        runtime_evidence_ids: vec![],
        binding_ids: vec![id('7')],
        engine_id: id('8'),
        environment_id: id('9'),
        missing_fields: BTreeSet::from(["runtime_evidence".into()]),
        causal_claim: false,
    };
    evidence.validate().unwrap();
    evidence.causal_claim = true;
    assert!(evidence.validate().is_err());
    evidence.causal_claim = false;
    evidence.unsuccessful_attempt_ids = vec!["unknown-attempt".into()];
    assert!(evidence.validate().is_err());
}
