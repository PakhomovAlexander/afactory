use super::*;
use review_core::task::VerificationReserveV1;
use review_store::{Cas, content_id};
use serde_json::json;

const PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "first/reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "second"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "many", optional = false, snapshot_affinity = "any" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "first/reviewer", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "second", port = "prior_findings" }
[[edges]]
from = { node = "first/reviewer", port = "result" }
to = { node = "gather", port = "reports" }
[[edges]]
from = { node = "second", port = "result" }
to = { node = "gather", port = "reports" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

fn context(loaded: &Loaded) -> ReviewCompileContext {
    let id = content_id(&json!({"fixture": true})).unwrap();
    let input = |ty: &str| ArtifactInputV1 {
        artifact_ids: vec![id.clone()],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(id.clone()),
    };
    let workers = loaded
        .planned()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter))
        .map(|(name, _)| {
            (
                name.clone(),
                ReviewWorker {
                    package: "fixture/reviewer".into(),
                    allowance: NodeAllowance {
                        tokens_per_attempt: 100,
                        wall_ms_per_attempt: 1000,
                        max_attempts: 2,
                        verification_attempts: 1,
                    },
                },
            )
        })
        .collect();
    ReviewCompileContext {
        inputs: BTreeMap::from([
            ("head".into(), input(contract::SOURCE_SNAPSHOT_V1)),
            ("round".into(), input(REVIEW_ROUND_V1)),
        ]),
        head_input: "head".into(),
        round_input: "round".into(),
        workers,
        outputs: BTreeMap::from([(
            "findings".into(),
            Address {
                node: "ledger".into(),
                port: "findings".into(),
            },
        )]),
        limits: TaskLimitsV1 {
            tokens: 1000,
            max_attempts: 8,
            deadline_unix_ms: 9999999999999,
            verification: VerificationReserveV1 {
                tokens: 400,
                attempts: 4,
                wall_ms: 4000,
            },
        },
        max_parallel: 2,
        gate_wall_ms: 2000,
    }
}

#[test]
fn static_review_has_public_contract_typed_lanes_transitive_gates_and_one_budget() {
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    let graph = &compilation.graph;
    assert_eq!(graph.nodes.len(), 7);
    assert_eq!(graph.allowances.len(), 3);
    assert_eq!(graph.calls.len(), 1);
    assert_eq!(graph.slots.len(), 2);
    assert_eq!(compilation.contract.inputs.len(), 2);
    assert_eq!(compilation.contract.outputs.len(), 1);
    compilation.contract.validate().unwrap();
    assert_eq!(
        compilation,
        compile_legacy_review(&loaded, context(&loaded)).unwrap()
    );
    let gate = &compilation.nodes["gate"];
    for name in ["first/reviewer", "second", "gather", "ledger"] {
        let mapping = &compilation.nodes[name];
        assert!(mapping.task_node.split('.').all(review_core::task::is_name));
        let node = &graph.nodes[&mapping.task_node];
        assert!(
            node.conditions
                .iter()
                .any(|condition| condition.source.node == gate.task_node
                    && condition.source.port == "outcome"
                    && condition.outcome == ReceiptOutcomeV1::Passed)
        );
        assert!(graph.requires_successful_predecessors(&mapping.task_node));
    }
    for name in ["first/reviewer", "second"] {
        let node = &graph.nodes[&compilation.nodes[name].task_node];
        assert_eq!(node.contract.outputs.len(), 2);
        assert_eq!(
            node.contract.outputs["o0"].artifact_type,
            contract::REVIEWER_RESULT_V2
        );
        assert_eq!(
            node.contract.outputs["metadata"].artifact_type,
            TASK_REVIEW_RESULT_METADATA_V1
        );
    }
    let gather = &compilation.nodes["gather"];
    assert_eq!(gather.inputs.len(), 2);
    assert!(
        gather
            .inputs
            .values()
            .all(|lane| lane.review_port == "reports")
    );
    let task_gather = &graph.nodes[&gather.task_node];
    assert_eq!(
        task_gather.contract.inputs["i0"].cardinality,
        PortCardinality::One
    );
    assert_eq!(
        task_gather.contract.inputs["i1"].artifact_type,
        contract::REVIEWER_RESULT_V2
    );
    assert_eq!(graph.scheduler_plan().unwrap().order, graph.order);
    let wire = serde_json::to_value(&compilation).unwrap();
    assert_eq!(
        serde_json::from_value::<LegacyReviewCompilation>(wire).unwrap(),
        compilation
    );
}

#[test]
fn restored_fan_in_sorts_original_ids_and_refuses_missing_or_extra_lanes() {
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    let mapping = &compilation.nodes["gather"];
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let mut expected = Vec::new();
    let mut lanes = review_graph::ArtifactMap::new();
    for (index, (name, lane)) in mapping.inputs.iter().enumerate() {
        let raw = cas.put_json(&json!({"summary": index})).unwrap();
        let id = lane
            .codec
            .capture(
                &cas,
                &raw,
                review_core::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: None,
                    operation_id: format!("capture-{index}"),
                },
                None,
            )
            .unwrap();
        lanes.insert(name.clone(), vec![id]);
        expected.push(raw);
    }
    expected.sort();
    assert_eq!(
        mapping.restore_inputs(&cas, &lanes).unwrap(),
        BTreeMap::from([("reports".into(), expected)])
    );
    let removed = lanes.remove("i0").unwrap();
    assert!(mapping.restore_inputs(&cas, &lanes).is_err());
    lanes.insert("i0".into(), removed);
    lanes.insert("ambient".into(), Vec::new());
    assert!(mapping.restore_inputs(&cas, &lanes).is_err());
}

#[test]
fn codecs_follow_the_declared_contract_and_generation_stays_strict() {
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    // The shorthand Ledger output still reduces into the canonical-identity envelope.
    assert_eq!(
        compilation.nodes["ledger"].outputs["o0"].codec,
        ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into()
        }
    );
    assert_eq!(
        compilation.contract.outputs["findings"].artifact_type,
        contract::FINDING_SET_V1
    );
    let ledger = &compilation.graph.nodes[&compilation.nodes["ledger"].task_node];
    assert_eq!(
        ledger.contract.outputs["finding_set"].artifact_type,
        contract::FINDING_SET_V1
    );
    assert_eq!(
        ledger.contract.outputs["demand_set"].artifact_type,
        contract::DEMAND_SET_V1
    );
    assert_eq!(
        compilation.nodes["generation"].outputs["o0"].codec,
        ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into()
        }
    );
    // A Generation output is never retyped by its name.
    let opaque_generation = PIPELINE.replace(
        "outputs = [{ name = \"findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = true, snapshot_affinity = \"any\" }]",
        "outputs = [\"findings\"]",
    );
    assert_ne!(opaque_generation, PIPELINE);
    let error = crate::Definition::from_toml(&opaque_generation)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(
        error.to_string().contains("typed port declaration"),
        "{error}"
    );
    let mut bad = context(&loaded);
    bad.workers.remove("second");
    assert!(compile_legacy_review(&loaded, bad).is_err());
    let mut bad = context(&loaded);
    bad.inputs.get_mut("round").unwrap().artifact_type = contract::OPAQUE_V1.into();
    assert!(compile_legacy_review(&loaded, bad).is_err());
    let mut bad = context(&loaded);
    bad.outputs.get_mut("findings").unwrap().port = "undeclared".into();
    assert!(compile_legacy_review(&loaded, bad).is_err());
}

#[test]
fn dynamic_review_retains_enveloped_history_and_shared_provider_guards() {
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    use review_graph::task::OperatorAttemptCost;
    let loaded =
        crate::Definition::from_toml(include_str!("../../../tests/fixtures/dynamic-v5.toml"))
            .unwrap()
            .load()
            .unwrap();
    let config = context(&loaded);
    let limits = config.limits.clone();
    let mut compilation = compile_legacy_review(&loaded, config).unwrap();
    let generation = &compilation.nodes["generation"];
    assert_eq!(
        generation.outputs["o0"].codec,
        ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into()
        }
    );
    let scatter = &compilation.nodes["scatter"];
    let history = scatter
        .inputs
        .iter()
        .find(|(_, lane)| lane.review_port == "prior_findings")
        .unwrap();
    let history_port = &compilation.graph.nodes[&scatter.task_node].contract.inputs[history.0];
    assert!(history_port.optional);
    assert_eq!(history_port.affinity, PortAffinityV1::Unbound {});
    assert_eq!(
        compilation.contract.outputs["findings"].affinity,
        PortAffinityV1::SameAs {
            input: "head".into()
        }
    );
    let id = content_id(&json!({"binding": "captured"})).unwrap();
    let bindings = compilation
        .graph
        .slots
        .keys()
        .map(|slot| {
            (
                slot.clone(),
                EffectiveWorkerBindingV1 {
                    package_digest: id.clone(),
                    package_artifact_id: id.clone(),
                    invocation_policy_id: id.clone(),
                    execution: WorkerExecutionV1::Model {
                        provider: "personal".into(),
                        provider_kind: "fixture".into(),
                        principal_id: "same-person".into(),
                        model: "fixture-model".into(),
                        effort: "high".into(),
                    },
                },
            )
        })
        .collect();
    compilation
        .graph
        .require_provider_admission(
            &bindings,
            &OperatorAttemptCost {
                tokens: 10,
                wall_ms: 1000,
            },
            &limits,
        )
        .unwrap();
    let admissions: Vec<_> = compilation
        .graph
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.operator, CompiledOperator::ProviderAdmission { .. }))
        .collect();
    assert_eq!(admissions.len(), 1);
    for name in ["scatter", "closeout"] {
        let node = &compilation.graph.nodes[&compilation.nodes[name].task_node];
        assert!(
            node.conditions
                .iter()
                .any(|condition| condition.source.node == *admissions[0].0)
        );
    }
}

fn resource_manifest(loaded: &Loaded) -> review_core::CampaignManifestV1 {
    let id = content_id(&json!({"resource_fixture": true})).unwrap();
    serde_json::from_value(json!({
        "authority_snapshot_id": id, "subject_kind": "whole-tree",
        "pipeline": {"path": ".af/pipelines/review.toml", "artifact_id": id},
        "reviewer_lock": {"path": ".af/af.lock", "artifact_id": id},
        "reviewers": [], "execution_policy_ids": [id], "project_policy_ids": [],
        "convergence": {"clean_rounds": 1, "max_rounds": 1, "gate": "major"},
        "reviewer_timeout_seconds": 9,
        "check_timeout_seconds": loaded.check_timeout_seconds(),
        "git_timeout_seconds": 300,
        "budgets": loaded.budgets().map(|caps| review_core::CampaignBudgetV1 {
            attempt_tokens: caps.attempt, run_tokens: caps.run,
        }),
        "finding_identity_policy": review_core::CANONICAL_FINDING_IDENTITY_POLICY,
        "finding_genesis_id": id, "demand_genesis_id": id,
    }))
    .unwrap()
}

#[test]
fn original_review_task_envelope_preserves_round_caps_and_all_retry_wall_bounds() {
    use crate::captured_review::ReviewMode;
    use review_core::task::plan::WorkerExecutionV1;
    let definition = PIPELINE.replace("version = 2", "version = 2\ncheck_timeout_seconds = 2")
        + "\n[[checks]]\nname = \"one\"\nprogram = \"/bin/true\"\n"
        + "\n[[checks]]\nname = \"two\"\nprogram = \"/bin/true\"\n"
        + "\n[budgets]\nunit = \"tokens\"\nattempt = 9\nrun = 70\n"
        + "\n[convergence]\nclean_rounds = 2\nmax_rounds = 3\ngate = \"major\"\n";
    let loaded = crate::Definition::from_toml(&definition)
        .unwrap()
        .load()
        .unwrap();
    let mut manifest = resource_manifest(&loaded);
    let mut executions: BTreeMap<_, _> = loaded
        .reviewers()
        .keys()
        .map(|name| (name.clone(), WorkerExecutionV1::Command {}))
        .collect();
    let policy = resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: 1000,
    };
    let probe = review_graph::task::OperatorAttemptCost {
        tokens: 4,
        wall_ms: 5000,
    };
    let light = policy
        .task_limits(
            &loaded,
            &manifest,
            ReviewMode::Light,
            &executions,
            &probe,
            1000,
        )
        .unwrap();
    assert_eq!(
        light.tokens, 70,
        "The fallback cannot replace a captured Round cap"
    );
    assert_eq!(
        light.max_attempts, 5,
        "Two retryable Workers and one complete Gate sequence"
    );
    assert_eq!(
        light.deadline_unix_ms, 41_000,
        "Four nine-second Worker Attempts plus two two-second checks"
    );
    manifest.convergence.clean_rounds = 2;
    manifest.convergence.max_rounds = 3;
    executions.insert(
        "second".into(),
        WorkerExecutionV1::Model {
            provider: "personal".into(),
            provider_kind: "claude".into(),
            principal_id: content_id(&json!({"account":1})).unwrap(),
            model: "claude-fable-5-1".into(),
            effort: "high".into(),
        },
    );
    let heavy = policy
        .task_limits(
            &loaded,
            &manifest,
            ReviewMode::Heavy,
            &executions,
            &probe,
            1000,
        )
        .unwrap();
    assert_eq!(
        heavy.tokens, 210,
        "Probes consume the existing Round cap, not an added token grant"
    );
    assert_eq!(heavy.max_attempts, 18);
    assert_eq!(heavy.deadline_unix_ms, 136_000);
    assert_eq!(heavy.verification.tokens, 0);
    assert!(
        policy
            .task_limits(
                &loaded,
                &manifest,
                ReviewMode::Light,
                &executions,
                &probe,
                1000
            )
            .is_err()
    );
    let mut missing = executions.clone();
    missing.remove("second");
    assert!(
        policy
            .task_limits(
                &loaded,
                &manifest,
                ReviewMode::Heavy,
                &missing,
                &probe,
                1000
            )
            .is_err()
    );
    let original = manifest.clone();
    manifest.budgets.as_mut().unwrap().run_tokens += 1;
    assert!(
        policy
            .task_limits(
                &loaded,
                &manifest,
                ReviewMode::Heavy,
                &executions,
                &probe,
                1000
            )
            .is_err()
    );
    assert!(
        policy
            .task_limits(
                &loaded,
                &original,
                ReviewMode::Heavy,
                &executions,
                &probe,
                review_core::json::SAFE_INTEGER_MAX as u64
            )
            .is_err()
    );
}

#[test]
fn original_review_task_envelope_counts_owned_children_and_only_eligible_integration_phases() {
    use crate::captured_review::ReviewMode;
    use review_core::task::plan::WorkerExecutionV1;
    let definition = include_str!("../../../tests/fixtures/dynamic-v5.toml").to_string()
        + "\n[[checks]]\nname = \"build\"\nprogram = \"/bin/true\"\n"
        + "\n[integration]\npost_apply_checks = [\"build\"]\n"
        + "\n[convergence]\nclean_rounds = 2\nmax_rounds = 3\ngate = \"major\"\n";
    let loaded = crate::Definition::from_toml(&definition)
        .unwrap()
        .load()
        .unwrap();
    let mut manifest = resource_manifest(&loaded);
    manifest.convergence.clean_rounds = 2;
    manifest.convergence.max_rounds = 3;
    let executions = loaded
        .reviewers()
        .keys()
        .map(|name| (name.clone(), WorkerExecutionV1::Command {}))
        .collect();
    let policy = resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    };
    let probe = review_graph::task::OperatorAttemptCost {
        tokens: 4,
        wall_ms: 5000,
    };
    let heavy = policy
        .task_limits(
            &loaded,
            &manifest,
            ReviewMode::Heavy,
            &executions,
            &probe,
            1000,
        )
        .unwrap();
    // Two Scatter children, each with its own retry; one retryable whole-Subject closeout;
    // one Gate sequence. Integration can run after only the first two Rounds.
    assert_eq!(heavy.max_attempts, (4 + 2 + 1) * 3 + 2);
    assert_eq!(heavy.tokens, loaded.budgets().unwrap().run * 3);
    assert_eq!(
        heavy.deadline_unix_ms,
        1000 + (6 * 9000 + loaded.check_timeout_seconds() * 1000) * 3
            + 2 * loaded.check_timeout_seconds() * 1000
    );
    manifest.convergence.clean_rounds = 1;
    manifest.convergence.max_rounds = 1;
    let light = policy
        .task_limits(
            &loaded,
            &manifest,
            ReviewMode::Light,
            &executions,
            &probe,
            1000,
        )
        .unwrap();
    assert_eq!(
        light.max_attempts, 7,
        "Light Review never grants a promotion Attempt"
    );
}

#[test]
fn captured_review_resources_preserve_node_retry_and_round_caps_on_the_task_ledger() {
    use super::resources::{ReviewResourcePolicy, review_token_scopes};
    let definition = PIPELINE.replace(
        "id = \"first/reviewer\"",
        "id = \"first/reviewer\"\nbudget = { attempt = 30 }",
    ) + "\n[budgets]\nunit = \"tokens\"\nattempt = 100\nrun = 250\n";
    let loaded = crate::Definition::from_toml(&definition)
        .unwrap()
        .load()
        .unwrap();
    let mut context = context(&loaded);
    context.limits.verification = VerificationReserveV1 {
        tokens: 0,
        attempts: 0,
        wall_ms: 0,
    };
    let limits = context.limits.clone();
    ReviewResourcePolicy {
        uncapped_attempt_tokens: 999,
    }
    .apply(&loaded, &resource_manifest(&loaded), &mut context)
    .unwrap();
    assert_eq!(
        context.limits, limits,
        "the original Task allowance is retained"
    );
    assert_eq!(context.max_parallel, 4);
    assert_eq!(
        context.workers["first/reviewer"]
            .allowance
            .tokens_per_attempt,
        30
    );
    assert_eq!(context.workers["second"].allowance.tokens_per_attempt, 100);
    assert!(
        context
            .workers
            .values()
            .all(|worker| worker.allowance.max_attempts == 2
                && worker.allowance.wall_ms_per_attempt == 9000)
    );
    let mut compilation = compile_legacy_review(&loaded, context).unwrap();
    let scopes = review_token_scopes(&loaded, &compilation, 2).unwrap();
    assert_eq!(scopes.len(), 2);
    assert_eq!(scopes["review.round2"].tokens, 250);
    assert!(scopes["review.round2"].contains("root.providers.admit0"));
    assert_eq!(
        scopes,
        review_token_scopes(&loaded, &compilation, 2).unwrap()
    );
    assert!(
        !review_token_scopes(&loaded, &compilation, 3)
            .unwrap()
            .contains_key("review.round2")
    );
    compilation.graph.token_scopes = scopes;
    let first = &compilation.nodes["first/reviewer"].task_node;
    let second = &compilation.nodes["second"].task_node;
    let mut budget = compilation.graph.budget(limits).unwrap();
    let attempt = budget.prepare(first, 1).unwrap();
    budget.begin(&attempt.id, 1).unwrap();
    budget.settle_exact(&attempt.id, 5).unwrap();
    assert!(
        budget.prepare(first, 2).unwrap_err().contains("scope"),
        "an explicit Node cap bounds the aggregate of retries"
    );
    for time in [2, 3] {
        let attempt = budget.prepare(second, time).unwrap();
        budget.begin(&attempt.id, time).unwrap();
        budget.settle_exact(&attempt.id, 100).unwrap();
    }
    assert_eq!(budget.committed_tokens(), 205);
    assert_eq!(budget.scope_committed_tokens("review.round2"), Some(205));
    assert_eq!(budget.reserved_tokens(), 0);
}

#[test]
fn uncapped_review_requires_new_bounded_policy_and_never_invents_a_legacy_cap() {
    use super::resources::{ReviewResourcePolicy, review_token_scopes};
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = resource_manifest(&loaded);
    assert!(manifest.budgets.is_none());
    let mut context = context(&loaded);
    let policy = ReviewResourcePolicy {
        uncapped_attempt_tokens: 17,
    };
    policy.apply(&loaded, &manifest, &mut context).unwrap();
    assert!(
        context
            .workers
            .values()
            .all(|worker| worker.allowance.tokens_per_attempt == 17)
    );
    let compilation = compile_legacy_review(&loaded, context).unwrap();
    assert!(
        review_token_scopes(&loaded, &compilation, 1)
            .unwrap()
            .is_empty()
    );
    assert!(review_token_scopes(&loaded, &compilation, 0).is_err());
    assert!(manifest.budgets.is_none());
}

#[test]
fn review_resource_translation_checks_all_bounds_before_changing_the_context() {
    use super::resources::ReviewResourcePolicy;
    let loaded = crate::Definition::from_toml(PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let manifest = resource_manifest(&loaded);
    let mut context = context(&loaded);
    let before = context
        .workers
        .iter()
        .map(|(name, worker)| (name.clone(), worker.allowance.clone()))
        .collect::<BTreeMap<_, _>>();
    let policy = ReviewResourcePolicy {
        uncapped_attempt_tokens: 1001,
    };
    assert!(policy.apply(&loaded, &manifest, &mut context).is_err());
    assert_eq!(context.max_parallel, 2);
    assert_eq!(context.gate_wall_ms, 2000);
    assert_eq!(
        before,
        context
            .workers
            .iter()
            .map(|(name, worker)| (name.clone(), worker.allowance.clone()))
            .collect()
    );
    let mut manifest = manifest;
    manifest.reviewer_timeout_seconds = review_core::json::SAFE_INTEGER_MAX as u64;
    assert!(
        ReviewResourcePolicy {
            uncapped_attempt_tokens: 17
        }
        .apply(&loaded, &manifest, &mut context)
        .unwrap_err()
        .contains("timeout")
    );
    assert_eq!(context.max_parallel, 2);
}

#[test]
fn review_gate_bound_covers_the_complete_captured_check_sequence() {
    let definition = PIPELINE.replace("version = 2", "version = 2\ncheck_timeout_seconds = 2")
        + "\n[[checks]]\nname = \"first\"\nprogram = \"/bin/true\"\n\n[[checks]]\nname = \"second\"\nprogram = \"/bin/true\"\n\n[[checks]]\nname = \"third\"\nprogram = \"/bin/true\"\n";
    let loaded = crate::Definition::from_toml(&definition)
        .unwrap()
        .load()
        .unwrap();
    let mut context = context(&loaded);
    resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: 10,
    }
    .apply(&loaded, &resource_manifest(&loaded), &mut context)
    .unwrap();
    assert_eq!(context.gate_wall_ms, 6000);
    let compilation = compile_legacy_review(&loaded, context).unwrap();
    let gate = &compilation.nodes["gate"].task_node;
    assert_eq!(compilation.graph.allowances[gate].wall_ms_per_attempt, 6000);
    assert_eq!(compilation.graph.allowances[gate].max_attempts, 1);
    assert_eq!(compilation.graph.allowances[gate].tokens_per_attempt, 0);
}

#[test]
fn review_shards_share_fanout_without_acquiring_the_static_parent_node_cap() {
    let definition = include_str!("../../../tests/fixtures/dynamic-v5.toml").replace(
        "id = \"scatter\"",
        "id = \"scatter\"\nbudget = { attempt = 30 }",
    );
    let loaded = crate::Definition::from_toml(&definition)
        .unwrap()
        .load()
        .unwrap();
    let mut context = context(&loaded);
    context.limits.verification = VerificationReserveV1 {
        tokens: 0,
        attempts: 0,
        wall_ms: 0,
    };
    resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    }
    .apply(&loaded, &resource_manifest(&loaded), &mut context)
    .unwrap();
    assert_eq!(context.workers["scatter"].allowance.tokens_per_attempt, 30);
    let compilation = compile_legacy_review(&loaded, context).unwrap();
    let scopes = resources::review_token_scopes(&loaded, &compilation, 1).unwrap();
    let scatter = &compilation.nodes["scatter"].task_node;
    assert_eq!(scopes.len(), 2);
    assert!(!scopes.contains_key(&format!("review.round1.node.{scatter}")));
    let fanout = &scopes[&format!("review.round1.fanout.{scatter}")];
    assert_eq!(fanout.tokens, 200);
    assert!(fanout.contains(&format!("{scatter}.shard0")));
    assert!(fanout.contains(&format!("{scatter}.shard1")));
    assert!(!fanout.contains(&compilation.nodes["closeout"].task_node));
    assert!(!fanout.contains(&format!("{scatter}suffix.shard0")));
}

#[test]
fn scatter_results_are_reviewer_result_v2_and_require_the_history_input() {
    let fixture = include_str!("../../../tests/fixtures/dynamic-v5.toml");
    let loaded = crate::Definition::from_toml(fixture)
        .unwrap()
        .load()
        .unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    let node = &compilation.graph.nodes[&compilation.nodes["scatter"].task_node];
    let CompiledOperator::ReviewDomain {
        operation: ReviewOperation::Scatter { slot },
        ..
    } = &node.operator
    else {
        panic!("expected Scatter")
    };
    assert_eq!(
        compilation.graph.slots[slot].output_type,
        contract::REVIEWER_RESULT_V2
    );
    // A Scatter's slices answer ReviewerResult@2, so an unwired Scatter is refused at plan
    // time rather than mid-Round after its Gate has run.
    let history = "  { name = \"prior_findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = true, snapshot_affinity = \"any\" },\n";
    let edge = "[[edges]]\nfrom = { node = \"generation\", port = \"findings\" }\nto = { node = \"scatter\", port = \"prior_findings\" }\n";
    let unwired = fixture.replacen(history, "", 1).replace(edge, "");
    assert_ne!(unwired, fixture);
    let error = crate::Definition::from_toml(&unwired)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Scatter `scatter` must declare exactly one FindingSet@1 input"),
        "{error}"
    );
}

#[test]
fn owned_review_capture_preserves_static_plan_and_inherits_only_captured_child_authority() {
    let loaded =
        crate::Definition::from_toml(include_str!("../../../tests/fixtures/dynamic-v5.toml"))
            .unwrap()
            .load()
            .unwrap();
    let mut context = context(&loaded);
    context.limits.verification = VerificationReserveV1 {
        tokens: 0,
        attempts: 0,
        wall_ms: 0,
    };
    resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: 1,
    }
    .apply(&loaded, &resource_manifest(&loaded), &mut context)
    .unwrap();
    let limits = context.limits.clone();
    let frozen = compile_legacy_review(&loaded, context).unwrap();
    assert!(
        serde_json::to_value(&frozen.graph)
            .unwrap()
            .get("owned_children")
            .is_none()
    );
    let mut owned = frozen.clone();
    owned::install_owned_review_children(&loaded, &mut owned).unwrap();
    let owner = &owned.nodes["scatter"].task_node;
    let template = &owned.graph.owned_children[owner];
    assert_eq!(owned.graph.nodes, frozen.graph.nodes);
    assert_eq!(owned.graph.order, frozen.graph.order);
    assert_eq!(owned.nodes, frozen.nodes);
    assert_eq!(template.allowance, frozen.graph.allowances[owner]);
    assert!(!owned.graph.allowances.contains_key(owner));
    assert_eq!(template.max_children, 2);
    assert_eq!(
        template.contract.inputs[&template.item_input].artifact_type,
        contract::REVIEW_SLICE_V1
    );
    assert_eq!(
        owned.graph.nodes[owner].contract.inputs[&template.source_input].artifact_type,
        contract::SLICE_SET_V1
    );
    assert_eq!(
        template.contract.outputs["o0"].artifact_type,
        contract::REVIEWER_RESULT_V2
    );
    assert_eq!(
        template.contract.outputs["metadata"].artifact_type,
        TASK_REVIEW_RESULT_METADATA_V1
    );
    assert!(
        !template
            .inherited_inputs
            .values()
            .any(|port| port == &template.source_input)
    );
    for (child, parent) in &template.inherited_inputs {
        assert_eq!(
            template.contract.inputs[child],
            frozen.graph.nodes[owner].contract.inputs[parent]
        );
    }
    owned.graph.token_scopes = resources::review_token_scopes(&loaded, &owned, 1).unwrap();
    let mut budget = owned.graph.budget(limits).unwrap();
    budget
        .register_owned_children(
            owner,
            &[format!("{owner}.slice0"), format!("{owner}.slice1")],
        )
        .unwrap();
    assert!(
        budget
            .register_owned_children(owner, &[format!("{owner}.slice0")])
            .is_err()
    );
    assert!(owned::install_owned_review_children(&loaded, &mut owned).is_err());
}

#[test]
fn uncapped_broker_authority_must_fit_the_captured_fallback_reservation() {
    use super::resources::ReviewResourcePolicy;
    let mut definition = crate::Definition::from_toml(PIPELINE).unwrap();
    definition.version = 4;
    definition.gate = Some(toml::from_str("provider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"").unwrap());
    for node in &mut definition.nodes {
        if matches!(node.kind, crate::NodeKindSpec::Reviewer) {
            node.execution = Some(crate::ReviewerExecutionSpec {
                credential_mode: review_core::BrokerCredentialModeV1::Brokered,
                auto_apply: false,
                operations: vec![review_core::BrokerOperationPolicyV1 {
                    name: "ask".into(),
                    destination: "provider.personal".into(),
                    method: "inference".into(),
                    max_request_bytes: 100,
                    max_response_bytes: 100,
                    max_calls: 2,
                    max_usage: 18,
                }],
            });
        }
    }
    let loaded = definition.load().unwrap();
    let manifest = resource_manifest(&loaded);
    assert!(manifest.budgets.is_none());
    let mut context = context(&loaded);
    let before: BTreeMap<_, _> = context
        .workers
        .iter()
        .map(|(name, worker)| (name.clone(), worker.allowance.clone()))
        .collect();
    let error = ReviewResourcePolicy {
        uncapped_attempt_tokens: 17,
    }
    .apply(&loaded, &manifest, &mut context)
    .unwrap_err();
    assert!(error.contains("Broker authority"), "{error}");
    assert_eq!(
        context
            .workers
            .iter()
            .map(|(name, worker)| (name.clone(), worker.allowance.clone()))
            .collect::<BTreeMap<_, _>>(),
        before
    );
    ReviewResourcePolicy {
        uncapped_attempt_tokens: 18,
    }
    .apply(&loaded, &manifest, &mut context)
    .unwrap();
    assert!(
        context
            .workers
            .values()
            .all(|w| w.allowance.tokens_per_attempt == 18)
    );
}
