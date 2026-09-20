use std::collections::BTreeSet;

use review_core::task::optimization_experiment::*;

fn id(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn allowance(tokens: u64, attempts: u32, wall_ms: u64) -> ExperimentAllowanceV1 {
    ExperimentAllowanceV1 {
        tokens,
        attempts,
        wall_ms,
    }
}

fn fixture() -> (
    String,
    ExperimentalSlotV1,
    String,
    ExperimentSpecificationV1,
    String,
    ExperimentPreparedV1,
    ExperimentPlanDecisionV1,
) {
    let slot_id = id('1');
    let specification_id = id('2');
    let prepared_id = id('3');
    let case_id = id('4');
    let source = id('5');
    let requirements = id('6');
    let baseline_authority = id('7');
    let candidate_authority = id('8');
    let worker = id('9');
    let outer_plan = id('a');
    let policy = id('b');
    let oracle = id('c');
    let slot = ExperimentalSlotV1 {
        schema: "af.experimental-slot/1".into(),
        slot: "comparison".into(),
        outer_plan_id: outer_plan.clone(),
        policy_id: policy.clone(),
        protected_oracle_id: oracle.clone(),
        allowed_task_kinds: BTreeSet::from(["trial".into()]),
        allowed_packages: BTreeSet::from(["trial/baseline".into(), "trial/candidate".into()]),
        allowed_worker_package_ids: BTreeSet::from([worker.clone()]),
        allowed_efforts: BTreeSet::from(["low".into()]),
        allowed_effects: BTreeSet::new(),
        max_children: 2,
        max_depth: 4,
        max_concurrency: 1,
        max_development_candidates: 1,
        allowance: allowance(200, 2, 2_000),
    };
    let specification = ExperimentSpecificationV1 {
        schema: "af.experiment-specification/1".into(),
        slot_id: slot_id.clone(),
        policy_id: policy.clone(),
        profile_id: id('d'),
        development_set_id: id('e'),
        holdout_set_id: id('f'),
        protected_oracle_id: oracle,
        baseline_authority_id: baseline_authority.clone(),
        candidate_authority_id: candidate_authority.clone(),
        baseline_package: "trial/baseline".into(),
        candidate_package: "trial/candidate".into(),
        recipe: ComparisonRecipeV1::DeterministicCorrection,
        uncertainty_rule: ComparisonUncertaintyRuleV1::Deterministic,
        repetitions: 1,
        minimum_families: 1,
        token_increase_ceiling_bps: 0,
        exposed_family_ids: BTreeSet::new(),
        cases: vec![ExperimentCaseV1 {
            case_id: case_id.clone(),
            family_id: id('0'),
            membership: "holdout".into(),
            source_snapshot_id: source.clone(),
            requirements_id: requirements.clone(),
            compatibility_id: id('a'),
        }],
    };
    let child = |arm, package: &str, authority: &str, invocation: char| ExperimentChildClosureV1 {
        node: format!("root.trial.{}", package.rsplit('/').next().unwrap()),
        arm,
        case_id: case_id.clone(),
        repetition: 1,
        task_kind: "trial".into(),
        package: package.into(),
        worker_package_id: worker.clone(),
        effort: "low".into(),
        effects: BTreeSet::new(),
        source_snapshot_id: source.clone(),
        requirements_id: requirements.clone(),
        authority_id: authority.into(),
        invocation_id: id(invocation),
        allowance: allowance(100, 1, 1_000),
    };
    let prepared = ExperimentPreparedV1 {
        schema: "af.experiment-prepared/1".into(),
        task_revision_id: id('1'),
        outer_plan_id: outer_plan.clone(),
        slot_id: slot_id.clone(),
        specification_id: specification_id.clone(),
        compiled_child_plan_id: id('2'),
        policy_id: policy.clone(),
        spent_accounting_prefix_id: id('3'),
        writer_epoch: 4,
        children: vec![
            child(
                ExperimentArmV1::Baseline,
                "trial/baseline",
                &baseline_authority,
                '4',
            ),
            child(
                ExperimentArmV1::Candidate,
                "trial/candidate",
                &candidate_authority,
                '5',
            ),
        ],
    };
    let decision = ExperimentPlanDecisionV1 {
        schema: "af.experiment-plan-decision/1".into(),
        prepared_id: prepared_id.clone(),
        task_revision_id: prepared.task_revision_id.clone(),
        outer_plan_id: outer_plan,
        slot_id: slot_id.clone(),
        specification_id: specification_id.clone(),
        compiled_child_plan_id: prepared.compiled_child_plan_id.clone(),
        policy_id: policy,
        developer: "fixture-owner".into(),
        authorization_id: id('6'),
        key_policy_id: id('7'),
        signature_id: id('8'),
        decision: ExperimentDecisionKindV1::Approved,
        expires_unix_ms: 20_000,
        reason: "approved exact comparison".into(),
    };
    (
        slot_id,
        slot,
        specification_id,
        specification,
        prepared_id,
        prepared,
        decision,
    )
}

#[test]
fn exact_experimental_authority_admits_only_the_complete_bounded_closure() {
    let (slot_id, slot, specification_id, specification, prepared_id, prepared, decision) =
        fixture();
    validate_experiment_registration(
        &slot_id,
        &slot,
        &specification_id,
        &specification,
        &prepared_id,
        &prepared,
        &decision,
        10_000,
        &allowance(200, 2, 2_000),
    )
    .unwrap();

    let mut cases: Vec<(&str, ExperimentPreparedV1)> = Vec::new();
    let mut changed = prepared.clone();
    changed.children[1].package = "trial/baseline".into();
    cases.push(("package", changed));
    let mut changed = prepared.clone();
    changed.children[1].worker_package_id = id('f');
    cases.push(("worker", changed));
    let mut changed = prepared.clone();
    changed.children[1].effort = "high".into();
    cases.push(("effort", changed));
    let mut changed = prepared.clone();
    changed.children[1].effects.insert("network".into());
    cases.push(("effects", changed));
    let mut changed = prepared.clone();
    changed.children[1].source_snapshot_id = id('f');
    cases.push(("source", changed));
    let mut changed = prepared.clone();
    changed.children.pop();
    cases.push(("missing arm", changed));

    for (name, changed) in cases {
        assert!(
            validate_experiment_registration(
                &slot_id,
                &slot,
                &specification_id,
                &specification,
                &prepared_id,
                &changed,
                &decision,
                10_000,
                &allowance(200, 2, 2_000),
            )
            .is_err(),
            "changed {name} must fail before dispatch"
        );
    }

    assert!(
        validate_experiment_registration(
            &slot_id,
            &slot,
            &specification_id,
            &specification,
            &prepared_id,
            &prepared,
            &decision,
            20_000,
            &allowance(200, 2, 2_000),
        )
        .is_err(),
        "expired decision must fail"
    );
    assert!(
        validate_experiment_registration(
            &slot_id,
            &slot,
            &specification_id,
            &specification,
            &prepared_id,
            &prepared,
            &decision,
            10_000,
            &allowance(199, 2, 2_000),
        )
        .is_err(),
        "remaining parent reserve must be sufficient"
    );
    for (name, changed) in [
        ("wrong slot", {
            let mut changed = decision.clone();
            changed.slot_id = id('f');
            changed
        }),
        ("changed child plan", {
            let mut changed = decision.clone();
            changed.compiled_child_plan_id = id('f');
            changed
        }),
        ("rejected authority", {
            let mut changed = decision.clone();
            changed.decision = ExperimentDecisionKindV1::Rejected;
            changed
        }),
    ] {
        assert!(
            validate_experiment_registration(
                &slot_id,
                &slot,
                &specification_id,
                &specification,
                &prepared_id,
                &prepared,
                &changed,
                10_000,
                &allowance(200, 2, 2_000),
            )
            .is_err(),
            "{name} must fail before dispatch"
        );
    }
}

fn trial(
    invocation: char,
    arm: ExperimentArmV1,
    verified: bool,
    tokens: u64,
    elapsed_ms: u64,
) -> ExperimentTrialV1 {
    let intervals = vec![
        ExperimentMeasurementIntervalV1 {
            kind: ExperimentIntervalKindV1::Execution,
            start_ms: 0,
            end_ms: elapsed_ms,
        },
        ExperimentMeasurementIntervalV1 {
            kind: ExperimentIntervalKindV1::Preparation,
            start_ms: 0,
            end_ms: 10,
        },
        ExperimentMeasurementIntervalV1 {
            kind: ExperimentIntervalKindV1::CachePopulation,
            start_ms: 10,
            end_ms: 30,
        },
        ExperimentMeasurementIntervalV1 {
            kind: ExperimentIntervalKindV1::CacheLookup,
            start_ms: 30,
            end_ms: 35,
        },
        ExperimentMeasurementIntervalV1 {
            kind: ExperimentIntervalKindV1::CacheCopy,
            start_ms: 35,
            end_ms: 40,
        },
    ];
    ExperimentTrialV1 {
        invocation_id: id(invocation),
        case_id: id('4'),
        family_id: id('0'),
        arm,
        repetition: 1,
        compatibility_id: id('a'),
        verified,
        protected_checks_passed: true,
        billing_complete: true,
        charged_tokens: tokens,
        elapsed_ms,
        preparation_ms: 10,
        cache_population_ms: 20,
        cache_lookup_ms: 5,
        cache_copy_ms: 5,
        missing_measurements: BTreeSet::new(),
        intervals,
    }
}

#[test]
fn comparisons_include_failures_cache_overhead_and_explicit_non_success() {
    let (_, _, specification_id, mut specification, prepared_id, _, _) = fixture();
    let correction = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            trial('b', ExperimentArmV1::Baseline, false, 100, 100),
            trial('c', ExperimentArmV1::Candidate, true, 80, 80),
        ],
    )
    .unwrap();
    assert_eq!(correction.conclusion, ComparisonConclusionV1::Accepted);
    assert_eq!(correction.candidate_elapsed_ms, 80);

    specification.recipe = ComparisonRecipeV1::TokensPerVerifiedOutcome;
    specification.uncertainty_rule = ComparisonUncertaintyRuleV1::RepetitionDispersion;
    let zero = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            trial('b', ExperimentArmV1::Baseline, false, 100, 100),
            trial('c', ExperimentArmV1::Candidate, false, 1, 1),
        ],
    )
    .unwrap();
    assert_eq!(zero.conclusion, ComparisonConclusionV1::Inconclusive);
    assert_eq!(zero.reason, "zero_verified_outcomes");

    let mut partial = trial('c', ExperimentArmV1::Candidate, true, 50, 50);
    partial.billing_complete = false;
    let partial = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            trial('b', ExperimentArmV1::Baseline, true, 100, 100),
            partial,
        ],
    )
    .unwrap();
    assert_eq!(partial.conclusion, ComparisonConclusionV1::Inconclusive);
    assert_eq!(partial.reason, "partial_billing");
}

#[test]
fn exposed_holdouts_and_underpowered_broad_claims_fail_closed() {
    let (_, _, specification_id, mut specification, prepared_id, _, _) = fixture();
    specification.exposed_family_ids.insert(id('0'));
    assert!(specification.validate().is_err());

    specification.exposed_family_ids.clear();
    specification.recipe = ComparisonRecipeV1::TokensPerVerifiedOutcome;
    specification.uncertainty_rule = ComparisonUncertaintyRuleV1::RepetitionDispersion;
    let underpowered = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            trial('b', ExperimentArmV1::Baseline, true, 100, 100),
            trial('c', ExperimentArmV1::Candidate, true, 50, 80),
        ],
    )
    .unwrap();
    assert_eq!(
        underpowered.conclusion,
        ComparisonConclusionV1::Inconclusive
    );
    assert_eq!(underpowered.reason, "insufficient_samples");
}

#[test]
fn broad_comparison_rejects_a_development_family_regression_even_when_holdouts_pass() {
    let (_, _, specification_id, mut specification, prepared_id, _, _) = fixture();
    specification.recipe = ComparisonRecipeV1::TokensPerVerifiedOutcome;
    specification.uncertainty_rule = ComparisonUncertaintyRuleV1::RepetitionDispersion;
    specification.repetitions = 2;
    specification.minimum_families = 2;
    specification.cases.push(ExperimentCaseV1 {
        case_id: id('5'),
        family_id: id('1'),
        membership: "holdout".into(),
        source_snapshot_id: id('5'),
        requirements_id: id('6'),
        compatibility_id: id('a'),
    });
    specification.cases.push(ExperimentCaseV1 {
        case_id: id('6'),
        family_id: id('2'),
        membership: "development".into(),
        source_snapshot_id: id('5'),
        requirements_id: id('6'),
        compatibility_id: id('a'),
    });
    let measured = |invocation, case, family, repetition, arm, verified, tokens| {
        let mut value = trial(invocation, arm, verified, tokens, 100);
        value.case_id = id(case);
        value.family_id = id(family);
        value.repetition = repetition;
        value
    };
    let mut trials = Vec::new();
    let mut invocations = "0123456789abcdef".chars().skip(1);
    for (case, family, development) in [('4', '0', false), ('5', '1', false), ('6', '2', true)] {
        for repetition in 1..=2 {
            let invocation = invocations.next().unwrap();
            trials.push(measured(
                invocation,
                case,
                family,
                repetition,
                ExperimentArmV1::Baseline,
                true,
                100,
            ));
            let invocation = invocations.next().unwrap();
            trials.push(measured(
                invocation,
                case,
                family,
                repetition,
                ExperimentArmV1::Candidate,
                !development,
                50,
            ));
        }
    }
    let comparison =
        compare_experiment(&specification_id, &prepared_id, &specification, trials).unwrap();
    assert_eq!(comparison.conclusion, ComparisonConclusionV1::Rejected);
    assert_eq!(comparison.reason, "negative_quality");
}

#[test]
fn broad_savings_claims_refuse_unknown_cache_overhead() {
    let (_, _, specification_id, mut specification, prepared_id, _, _) = fixture();
    specification.recipe = ComparisonRecipeV1::TokensPerVerifiedOutcome;
    specification.uncertainty_rule = ComparisonUncertaintyRuleV1::RepetitionDispersion;
    let baseline = trial('b', ExperimentArmV1::Baseline, true, 100, 100);
    let mut candidate = trial('c', ExperimentArmV1::Candidate, true, 50, 80);
    candidate.cache_population_ms = 0;
    candidate
        .intervals
        .retain(|interval| interval.kind != ExperimentIntervalKindV1::CachePopulation);
    candidate
        .missing_measurements
        .insert("cache_population".into());
    let comparison = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![baseline, candidate],
    )
    .unwrap();
    assert_eq!(comparison.conclusion, ComparisonConclusionV1::Inconclusive);
    assert_eq!(comparison.reason, "unsupported_measurements");

    let baseline = trial('d', ExperimentArmV1::Baseline, true, 100, 100);
    let mut candidate = trial('e', ExperimentArmV1::Candidate, true, 50, 80);
    candidate
        .missing_measurements
        .insert("cache_toolchain_identity".into());
    let comparison = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![baseline, candidate],
    )
    .unwrap();
    assert_eq!(comparison.conclusion, ComparisonConclusionV1::Inconclusive);
    assert_eq!(comparison.reason, "unsupported_measurements");
}

#[test]
fn latency_requires_each_holdout_family_and_a_dispersion_margin() {
    let (_, _, specification_id, mut specification, prepared_id, _, _) = fixture();
    specification.recipe = ComparisonRecipeV1::Latency;
    specification.uncertainty_rule = ComparisonUncertaintyRuleV1::RepetitionDispersion;
    specification.repetitions = 2;
    specification.minimum_families = 2;
    specification.cases.push(ExperimentCaseV1 {
        case_id: id('5'),
        family_id: id('1'),
        membership: "holdout".into(),
        source_snapshot_id: id('5'),
        requirements_id: id('6'),
        compatibility_id: id('a'),
    });

    let measured =
        |invocation: char, case: char, family: char, repetition: u32, arm, tokens, elapsed| {
            let mut value = trial(invocation, arm, true, tokens, elapsed);
            value.case_id = id(case);
            value.family_id = id(family);
            value.repetition = repetition;
            value
        };
    // Accepted latency is a deterministic comparison of captured measurements, not a
    // bet about the scheduler. Keep the command-path fixture for real clock plumbing.
    let stable_trials = vec![
        measured('1', '4', '0', 1, ExperimentArmV1::Baseline, 100, 100),
        measured('2', '4', '0', 1, ExperimentArmV1::Candidate, 100, 50),
        measured('3', '4', '0', 2, ExperimentArmV1::Baseline, 100, 100),
        measured('4', '4', '0', 2, ExperimentArmV1::Candidate, 100, 50),
        measured('5', '5', '1', 1, ExperimentArmV1::Baseline, 100, 100),
        measured('6', '5', '1', 1, ExperimentArmV1::Candidate, 100, 50),
        measured('7', '5', '1', 2, ExperimentArmV1::Baseline, 100, 100),
        measured('8', '5', '1', 2, ExperimentArmV1::Candidate, 100, 50),
    ];
    let stable = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        stable_trials.clone(),
    )
    .unwrap();
    assert_eq!(stable.conclusion, ComparisonConclusionV1::Accepted);
    assert_eq!(stable.reason, "latency_objective_met");
    let mut reversed = stable_trials;
    reversed.reverse();
    assert_eq!(
        compare_experiment(&specification_id, &prepared_id, &specification, reversed)
            .unwrap()
            .conclusion,
        stable.conclusion,
        "measurement arrival order cannot change the verdict"
    );

    let aggregate_only = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            measured('1', '4', '0', 1, ExperimentArmV1::Baseline, 100, 100),
            measured('2', '4', '0', 1, ExperimentArmV1::Candidate, 100, 50),
            measured('3', '4', '0', 2, ExperimentArmV1::Baseline, 100, 100),
            measured('4', '4', '0', 2, ExperimentArmV1::Candidate, 100, 50),
            measured('5', '5', '1', 1, ExperimentArmV1::Baseline, 100, 100),
            measured('6', '5', '1', 1, ExperimentArmV1::Candidate, 100, 140),
            measured('7', '5', '1', 2, ExperimentArmV1::Baseline, 100, 100),
            measured('8', '5', '1', 2, ExperimentArmV1::Candidate, 100, 140),
        ],
    )
    .unwrap();
    assert_eq!(aggregate_only.conclusion, ComparisonConclusionV1::Rejected);
    assert_eq!(aggregate_only.reason, "latency_or_token_ceiling_not_met");

    let noisy_point_estimate = compare_experiment(
        &specification_id,
        &prepared_id,
        &specification,
        vec![
            measured('1', '4', '0', 1, ExperimentArmV1::Baseline, 100, 100),
            measured('2', '4', '0', 1, ExperimentArmV1::Candidate, 100, 10),
            measured('3', '4', '0', 2, ExperimentArmV1::Baseline, 100, 100),
            measured('4', '4', '0', 2, ExperimentArmV1::Candidate, 100, 99),
            measured('5', '5', '1', 1, ExperimentArmV1::Baseline, 100, 100),
            measured('6', '5', '1', 1, ExperimentArmV1::Candidate, 100, 10),
            measured('7', '5', '1', 2, ExperimentArmV1::Baseline, 100, 100),
            measured('8', '5', '1', 2, ExperimentArmV1::Candidate, 100, 99),
        ],
    )
    .unwrap();
    assert_eq!(
        noisy_point_estimate.conclusion,
        ComparisonConclusionV1::Inconclusive
    );
    assert_eq!(noisy_point_estimate.reason, "insufficient_samples");
}

#[test]
fn protected_harness_and_delivery_profile_fail_closed() {
    let mut harness = OptimizationHarnessV1 {
        schema: "af.optimization-harness/1".into(),
        oracle_id: id('1'),
        checks: BTreeSet::from(["regression".into()]),
        transitive_dependencies: BTreeSet::from(["fixtures/oracle.sh".into()]),
        candidate_writable_paths: BTreeSet::from(["scripts/gate.sh".into()]),
        cache_read_scopes: BTreeSet::from(["cache/build".into()]),
        cache_write_scopes: BTreeSet::from(["cache/build".into()]),
    };
    harness.validate().unwrap();
    harness
        .candidate_writable_paths
        .insert("fixtures/oracle.sh".into());
    assert!(harness.validate().is_err());
    harness.candidate_writable_paths = BTreeSet::from(["fixtures".into()]);
    assert!(
        harness.validate().is_err(),
        "an ancestor may not contain the oracle"
    );
    harness.candidate_writable_paths = BTreeSet::from(["scripts/gate.sh".into()]);
    harness.transitive_dependencies = BTreeSet::from(["fixtures/oracle.sh".into()]);
    harness.cache_read_scopes = BTreeSet::from(["cache/build".into()]);
    harness.cache_write_scopes = BTreeSet::from(["cache/build/incremental".into()]);
    harness.validate().unwrap();
    harness.cache_write_scopes = BTreeSet::from(["cache".into()]);
    assert!(
        harness.validate().is_err(),
        "a cache write ancestor widens access"
    );

    let mut verification = OptimizationVerificationV1 {
        schema: "af.optimization-verification/1".into(),
        profile: OptimizationProfileV1::Candidate,
        source_snapshot_id: id('1'),
        candidate_snapshot_id: id('2'),
        requirements_id: id('3'),
        harness_id: id('4'),
        comparison_id: id('5'),
        evaluation_id: id('6'),
        package_repin_id: id('7'),
        conclusion: ComparisonConclusionV1::Accepted,
        deliverable: true,
        protected_checks_passed: true,
    };
    verification.validate().unwrap();
    verification.conclusion = ComparisonConclusionV1::Rejected;
    assert!(verification.validate().is_err());

    let mut repin = OptimizationPackageRepinV1 {
        schema: "af.optimization-package-repin/1".into(),
        source_snapshot_id: id('1'),
        candidate_snapshot_id: id('2'),
        before_lock_id: id('3'),
        after_lock_id: id('4'),
        engine_release_id: id('5'),
        entailed: [(
            "project/candidate".into(),
            PackagePinChangeV1 {
                before: id('6'),
                after: id('7'),
            },
        )]
        .into(),
        protected_pins: [("engine/release".into(), id('5'))].into(),
    };
    repin.validate().unwrap();
    repin
        .protected_pins
        .insert("project/candidate".into(), id('6'));
    assert!(repin.validate().is_err());

    let mut materialization = HarnessMaterializationV1 {
        schema: "af.harness-materialization/1".into(),
        fixture_constructor_id: id('1'),
        product_source_snapshot_id: id('2'),
        requirements_id: id('3'),
        protected_oracle_id: id('4'),
        baseline_harness_id: id('5'),
        candidate_harness_id: id('6'),
        baseline_derived_snapshot_id: id('7'),
        candidate_derived_snapshot_id: id('8'),
        environment_id: id('9'),
    };
    materialization.validate().unwrap();
    materialization.candidate_derived_snapshot_id =
        materialization.product_source_snapshot_id.clone();
    assert!(materialization.validate().is_err());
}
