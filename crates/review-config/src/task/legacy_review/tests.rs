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
outputs = [{ name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
[[nodes]]
id = "first/reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "second"
kind = "reviewer"
inputs = []
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reports", type = "review.kernel/Opaque@1", cardinality = "many", optional = false, snapshot_affinity = "any" }]
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
        finding_identity_policy: review_core::LEGACY_FINDING_IDENTITY_POLICY.into(),
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
            contract::REVIEWER_RESULT_V1
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
        contract::REVIEWER_RESULT_V1
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
fn v1_opaque_generation_is_explicitly_adapted_but_v2_stays_strict() {
    let v1 = PIPELINE.replace("version = 2\n[subject]\nkind = \"whole-tree\"", "version = 1")
        .replace("outputs = [{ name = \"findings\", type = \"review.kernel/PriorFindings@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]", "outputs = [\"findings\"]")
        .replace("inputs = [{ name = \"prior_findings\", type = \"review.kernel/PriorFindings@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]", "inputs = [\"prior_findings\"]");
    let loaded = crate::Definition::from_toml(&v1).unwrap().load().unwrap();
    let compilation = compile_legacy_review(&loaded, context(&loaded)).unwrap();
    assert_eq!(
        compilation.nodes["ledger"].outputs["o0"].codec,
        ReviewArtifactCodec::Flat {
            artifact_type: contract::OPAQUE_V1.into()
        }
    );
    let mut canonical = context(&loaded);
    canonical.finding_identity_policy = review_core::CANONICAL_FINDING_IDENTITY_POLICY.into();
    let canonical = compile_legacy_review(&loaded, canonical).unwrap();
    assert_eq!(
        canonical.nodes["ledger"].outputs["o0"].codec,
        ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into()
        }
    );
    assert_eq!(
        canonical.contract.outputs["findings"].artifact_type,
        contract::FINDING_SET_V1
    );
    assert_eq!(
        canonical.nodes["generation"],
        compilation.nodes["generation"]
    );
    let mut unknown = context(&loaded);
    unknown.finding_identity_policy = "inferred-from-result".into();
    assert!(compile_legacy_review(&loaded, unknown).is_err());
    assert_eq!(
        compilation.nodes["generation"].outputs["o0"].codec,
        ReviewArtifactCodec::Flat {
            artifact_type: contract::PRIOR_FINDINGS_V1.into()
        }
    );
    let v2 = v1.replace(
        "version = 1",
        "version = 2\n[subject]\nkind = \"whole-tree\"",
    );
    assert!(crate::Definition::from_toml(&v2).unwrap().load().is_err());
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
        "budgets": loaded.budgets().map(|caps| review_core::CampaignBudgetV1 {
            attempt_tokens: caps.attempt, run_tokens: caps.run,
        }),
        "finding_identity_policy": review_core::CANONICAL_FINDING_IDENTITY_POLICY,
        "finding_genesis_id": id, "demand_genesis_id": id,
    }))
    .unwrap()
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
    budget.settle(&attempt.id, 5).unwrap();
    assert!(
        budget.prepare(first, 2).unwrap_err().contains("scope"),
        "an explicit Node cap bounds the aggregate of retries"
    );
    for time in [2, 3] {
        let attempt = budget.prepare(second, time).unwrap();
        budget.begin(&attempt.id, time).unwrap();
        budget.settle(&attempt.id, 100).unwrap();
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
fn scatter_result_contract_tracks_inherited_history_contract() {
    let fixture = include_str!("../../../tests/fixtures/dynamic-v5.toml");
    let history = "  { name = \"prior_findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = true, snapshot_affinity = \"any\" },\n";
    let edge = "[[edges]]\nfrom = { node = \"generation\", port = \"findings\" }\nto = { node = \"scatter\", port = \"prior_findings\" }\n";
    for (definition, expected) in [
        (fixture.to_owned(), contract::REVIEWER_RESULT_V2),
        (
            fixture.replacen(history, "", 1).replace(edge, ""),
            contract::REVIEWER_RESULT_V1,
        ),
    ] {
        let loaded = crate::Definition::from_toml(&definition)
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
        assert_eq!(compilation.graph.slots[slot].output_type, expected);
    }
}
