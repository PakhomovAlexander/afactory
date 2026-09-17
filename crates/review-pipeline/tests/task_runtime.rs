use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::task::catalog::*;
use review_core::Producer;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::*;
use review_graph::task::{CompiledTask, OperatorAttemptCost, OperatorSignature};
use review_pipeline::task::host::*;
use review_pipeline::task::*;
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::{Cas, EventStore};
use serde_json::json;

#[path = "task_runtime/broker.rs"]
mod broker;
#[path = "task_runtime/control.rs"]
mod control;
#[path = "task_runtime/output_admission.rs"]
mod output_admission;
#[path = "task_runtime/publication.rs"]
mod publication;
#[path = "task_runtime/reservation.rs"]
mod reservation;
#[path = "task_runtime/retry.rs"]
mod retry;
#[path = "task_runtime/usage_observation.rs"]
mod usage_observation;
#[path = "task_runtime/usage_recovery.rs"]
mod usage_recovery;
#[path = "task_runtime/wide_usage.rs"]
mod wide_usage;

struct Fixture {
    _directory: tempfile::TempDir,
    cas: Cas,
    store: EventStore,
    compiler: TaskPlanCompiler,
    task: TaskRevisionV1,
    revision_id: String,
    plan: ExecutionPlanV1,
    plan_id: String,
    graph: CompiledTask,
}

#[test]
fn approved_derived_model_child_uses_its_exact_context_and_replays_without_reexecution() {
    use review_core::task::optimization_experiment::*;
    use review_core::task::optimization_light::{
        OPTIMIZATION_EXECUTION_CONFIGURATION_V1, OptimizationExecutionConfigurationV1,
    };
    use review_graph::task::{
        EXPERIMENT_EXECUTION_PLAN_V1, ExperimentExecutionPlanV1, ExperimentPlannedChildV1,
        ExperimentalSlotTemplateV1,
    };
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    struct Model(Mutex<Vec<Value>>);
    impl WorkerModelAdapter for Model {
        fn provider_kind(&self) -> &'static str {
            "fixture"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("typed-model".into(), "high".into()))
        }
        fn invoke(
            &self,
            cas: &Cas,
            _: &std::path::Path,
            input: Vec<u8>,
            _: std::time::Duration,
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let request: Value = serde_json::from_slice(&input).unwrap();
            self.0.lock().unwrap().push(request);
            let reply = serde_json::to_vec(&json!({
                "schema":"af.worker-reply/1",
                "outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}
            }))
            .unwrap();
            ModelWorkerReturn {
                usage_observation: None,
                raw_artifact_ids: vec![cas.put(&reply).unwrap()],
                message: Ok(reply),
                usage: Some(review_runner::TokenUsage::charge_only(1).into()),
            }
        }
    }

    struct ExperimentDomain {
        prepared: Mutex<Option<String>>,
        root_outputs: BTreeMap<String, ArtifactInputV1>,
    }
    impl TaskOperatorHost for ExperimentDomain {
        fn prepare_experiment(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: u64,
        ) -> Result<review_pipeline::task::TaskExperimentInputs, String> {
            Ok(review_pipeline::task::TaskExperimentInputs {
                prepared_id: self
                    .prepared
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or("fixture is not prepared")?,
            })
        }
        fn complete_experiment(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &review_store::store::task::execution::experiment::RegisteredTaskExperiment,
            facts: &[review_store::store::task::execution::experiment::ExperimentChildEvidence],
        ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
            if facts.len() != 2 || facts.iter().any(|fact| fact.published_output_id.is_none()) {
                return Err(format!(
                    "experiment did not retain both command outcomes: {facts:?}"
                ));
            }
            Ok(self.root_outputs.clone())
        }
        fn prepare_context(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &[String],
        ) -> Result<String, String> {
            Err("dynamic command context must use its resolved captured Worker".into())
        }
        fn execute(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            panic!("dynamic command children must use the common command Worker")
        }
    }
    impl TaskDomain for ExperimentDomain {
        fn validate_experiment_preparation(
            &self,
            _: &Cas,
            _: &TaskRevisionV1,
            _: &ExecutionPlanV1,
            prepared_id: &str,
            _: &ExperimentPreparedV1,
        ) -> Result<(), String> {
            if self.prepared.lock().unwrap().as_deref() == Some(prepared_id) {
                Ok(())
            } else {
                Err("wrong prepared closure".into())
            }
        }
        fn validate_context(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &[String],
            _: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        fn validate_output(
            &self,
            _: &Cas,
            _: &TaskRevisionV1,
            _: &ExecutionPlanV1,
            _: &TaskInvocationV1,
            _: &TaskOutputV1,
        ) -> Result<(), String> {
            Ok(())
        }
        fn validate_result(
            &self,
            _: &Cas,
            _: &TaskRevisionV1,
            _: &TaskResultV1,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    struct Developer;
    impl review_pipeline::task::host::TaskDeveloper for Developer {
        fn decide(
            &self,
            _: &TaskRevisionV1,
            _: &str,
            _: PlanDecisionKindV1,
        ) -> Result<review_store::store::task::DeveloperGrant, String> {
            Err("outer plan is fixed".into())
        }
        fn current(&self, _: &PlanDecisionV1) -> Result<(), String> {
            Ok(())
        }
        fn experiment_current(&self, _: &ExperimentPlanDecisionV1) -> Result<(), String> {
            Ok(())
        }
    }

    let model = Model(Mutex::new(Vec::new()));
    let mut f = Fixture::with_model("unused model package file", true);
    let parent = "root.inputs";
    let policy = f.task.authority.policy_id.clone();
    let oracle = f.revision_id.clone();
    let binding_seed =
        review_store::content_id(&json!([&f.revision_id, &policy, "trial"])).unwrap();
    let worker_node = "root.nodes.write";
    let definition = f.graph.nodes[worker_node].clone();
    let allowance = f.graph.allowances[worker_node].clone();
    let mut experimental_allowance = allowance.clone();
    experimental_allowance.max_attempts = 1;
    experimental_allowance.verification_attempts = 0;
    let slot_name = match &definition.operator {
        review_graph::task::CompiledOperator::Primitive {
            operator: TaskOperatorV1::Worker { slot },
            ..
        } => slot.clone(),
        _ => panic!("fixture worker"),
    };
    let package = f.graph.slots[&slot_name].worker.clone();
    let worker_package_id = f.plan.bindings[&slot_name].package_artifact_id.clone();
    let slot = ExperimentalSlotV2 {
        schema: "af.experimental-slot/2".into(),
        slot: "trial".into(),
        outer_plan_binding_id: binding_seed,
        policy_id: policy.clone(),
        protected_oracle_id: oracle.clone(),
        allowed_task_kinds: BTreeSet::from(["document".into()]),
        allowed_packages: BTreeSet::from([package.clone()]),
        allowed_worker_package_ids: BTreeSet::from([worker_package_id.clone()]),
        allowed_efforts: BTreeSet::from(["high".into()]),
        allowed_effects: BTreeSet::new(),
        max_children: 2,
        max_depth: 8,
        max_concurrency: 1,
        max_development_candidates: 1,
        allowance: ExperimentAllowanceV1 {
            tokens: experimental_allowance.tokens_per_attempt * 2,
            attempts: 2,
            wall_ms: experimental_allowance.wall_ms_per_attempt * 2,
        },
    };
    slot.validate().unwrap();
    let slot_id = f
        .cas
        .put_artifact(
            EXPERIMENTAL_SLOT_V2,
            producer(),
            vec![policy.clone(), oracle.clone()],
            None,
            serde_json::to_value(&slot).unwrap(),
        )
        .unwrap()
        .0;
    f.compiler = f
        .compiler
        .clone()
        .with_experimental_slot(
            parent.into(),
            ExperimentalSlotTemplateV1 {
                slot_id: slot_id.clone(),
                max_concurrency: 1,
            },
        )
        .unwrap();
    (f.plan, f.graph) = f
        .compiler
        .compile(&f.cas, &f.revision_id, "builtin/document")
        .unwrap();
    f.plan_id = f
        .cas
        .put_artifact(
            EXECUTION_PLAN_V1,
            producer(),
            vec![f.revision_id.clone()],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;

    let requirement = f.task.inputs["requirements"].clone();
    let source_id = requirement.artifact_ids[0].clone();
    let candidate_snapshot_id = f
        .cas
        .put_json(&json!({"candidate":"instructions"}))
        .unwrap();
    let repin_id = f.cas.put_json(&json!({"repin":"candidate"})).unwrap();
    let candidate_instructions = "Use only the approved candidate instructions.";
    let instructions_id =
        review_store::canonical::blob_content_id(candidate_instructions.as_bytes());
    f.cas.put(candidate_instructions.as_bytes()).unwrap();
    let original_digest = f.plan.bindings[&slot_name].package_digest.clone();
    let mut derived_files = f.compiler.package_files(&package).unwrap().clone();
    derived_files.insert(
        "instructions.md".into(),
        candidate_instructions.as_bytes().to_vec(),
    );
    let derived_digest = review_config::lock::package_digest_from_files(&derived_files);
    let execution_configuration = OptimizationExecutionConfigurationV1 {
        schema: "af.optimization-execution-configuration/1".into(),
        recipe_id: "context_retrieval_dedup".into(),
        original_package_id: worker_package_id.clone(),
        original_package_digest: original_digest.clone(),
        source_snapshot_id: source_id.clone(),
        candidate_snapshot_id: candidate_snapshot_id.clone(),
        repin_id: repin_id.clone(),
        package: package.clone(),
        package_digest: derived_digest.clone(),
        instructions_id: instructions_id.clone(),
        instructions: candidate_instructions.into(),
    };
    let execution_configuration_id = f
        .cas
        .put_artifact(
            OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
            producer(),
            vec![
                worker_package_id.clone(),
                source_id.clone(),
                candidate_snapshot_id.clone(),
                repin_id.clone(),
                instructions_id.clone(),
            ],
            None,
            serde_json::to_value(&execution_configuration).unwrap(),
        )
        .unwrap()
        .0;
    let derived_package_id = TaskPlanCompiler::derive_worker_instructions_package(
        &f.cas,
        &worker_package_id,
        &original_digest,
        &derived_digest,
        &instructions_id,
        candidate_instructions,
        producer(),
        vec![execution_configuration_id.clone()],
    )
    .unwrap();
    let task_configuration_id = f
        .cas
        .put_artifact(
            "af/OptimizationConfiguration@1",
            producer(),
            vec![execution_configuration_id.clone()],
            None,
            json!({
                "candidate_execution_configuration_id":execution_configuration_id,
                "source_snapshot_id":source_id,
                "candidate_snapshot_id":candidate_snapshot_id,
                "repin_id":repin_id,
            }),
        )
        .unwrap()
        .0;
    let case = ExperimentCaseV1 {
        case_id: source_id.clone(),
        family_id: policy.clone(),
        membership: "holdout".into(),
        source_snapshot_id: source_id.clone(),
        requirements_id: source_id.clone(),
        compatibility_id: oracle.clone(),
    };
    let specification = ExperimentSpecificationV1 {
        schema: "af.experiment-specification/1".into(),
        slot_id: slot_id.clone(),
        policy_id: policy.clone(),
        profile_id: oracle.clone(),
        development_set_id: policy.clone(),
        holdout_set_id: oracle.clone(),
        protected_oracle_id: oracle.clone(),
        baseline_authority_id: policy.clone(),
        candidate_authority_id: derived_package_id.clone(),
        baseline_package: package.clone(),
        candidate_package: package.clone(),
        recipe: ComparisonRecipeV1::TokensPerVerifiedOutcome,
        uncertainty_rule: ComparisonUncertaintyRuleV1::RepetitionDispersion,
        repetitions: 1,
        minimum_families: 1,
        token_increase_ceiling_bps: 0,
        exposed_family_ids: BTreeSet::new(),
        cases: vec![case],
    };
    let specification_id = f
        .cas
        .put_artifact(
            EXPERIMENT_SPECIFICATION_V1,
            producer(),
            vec![slot_id.clone()],
            None,
            serde_json::to_value(&specification).unwrap(),
        )
        .unwrap()
        .0;
    let mut closures = Vec::new();
    let mut planned = BTreeMap::new();
    for (suffix, arm, authority_id) in [
        ("baseline", ExperimentArmV1::Baseline, policy.clone()),
        (
            "candidate",
            ExperimentArmV1::Candidate,
            derived_package_id.clone(),
        ),
    ] {
        let node = format!("{parent}.{suffix}");
        let candidate = arm == ExperimentArmV1::Candidate;
        let mut inputs = BTreeMap::from([("input".into(), requirement.clone())]);
        if candidate {
            inputs.insert(
                "configuration".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![task_configuration_id.clone()],
                    artifact_type: "af/OptimizationConfiguration@1".into(),
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: None,
                },
            );
        }
        let invocation = TaskInvocationV1 {
            plan_id: f.plan_id.clone(),
            node: node.clone(),
            inputs,
        };
        let invocation_id = f
            .cas
            .put_artifact(
                review_core::task::execution::TASK_INVOCATION_V1,
                producer(),
                vec![f.plan_id.clone(), source_id.clone()],
                None,
                serde_json::to_value(&invocation).unwrap(),
            )
            .unwrap()
            .0;
        let mut child_definition = definition.clone();
        if candidate {
            let review_graph::task::CompiledOperator::Primitive { signature, .. } =
                &mut child_definition.operator
            else {
                unreachable!()
            };
            *signature = format!("worker-derived/{derived_package_id}");
        }
        planned.insert(
            node.clone(),
            ExperimentPlannedChildV1 {
                definition: child_definition,
                invocation,
                allowance: experimental_allowance.clone(),
            },
        );
        closures.push(ExperimentChildClosureV1 {
            node,
            arm,
            case_id: source_id.clone(),
            repetition: 1,
            task_kind: "document".into(),
            package: package.clone(),
            worker_package_id: if candidate {
                derived_package_id.clone()
            } else {
                worker_package_id.clone()
            },
            effort: "high".into(),
            effects: BTreeSet::new(),
            source_snapshot_id: source_id.clone(),
            requirements_id: source_id.clone(),
            authority_id,
            invocation_id,
            allowance: ExperimentAllowanceV1 {
                tokens: experimental_allowance.tokens_per_attempt,
                attempts: experimental_allowance.max_attempts,
                wall_ms: experimental_allowance.wall_ms_per_attempt,
            },
        });
    }
    let child_plan = ExperimentExecutionPlanV1 {
        schema: "af.experiment-execution-plan/1".into(),
        parent_node: parent.into(),
        children: planned,
    };
    let child_plan_id = f
        .cas
        .put_artifact(
            EXPERIMENT_EXECUTION_PLAN_V1,
            producer(),
            closures.iter().map(|c| c.invocation_id.clone()).collect(),
            None,
            serde_json::to_value(&child_plan).unwrap(),
        )
        .unwrap()
        .0;
    let domain = ExperimentDomain {
        prepared: Mutex::new(None),
        root_outputs: f.graph.inputs.clone(),
    };
    let developer = Developer;
    let models = BTreeMap::from([(
        slot_name.clone(),
        TaskModelBinding {
            binding: f.plan.bindings[&slot_name].clone(),
            adapter: &model as &dyn WorkerModelAdapter,
        },
    )]);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &domain,
        &models,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &developer);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "experiment", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let prepared = ExperimentPreparedV1 {
        schema: "af.experiment-prepared/1".into(),
        task_revision_id: f.revision_id.clone(),
        outer_plan_id: f.plan_id.clone(),
        slot_id: slot_id.clone(),
        specification_id: specification_id.clone(),
        compiled_child_plan_id: child_plan_id.clone(),
        policy_id: policy.clone(),
        spent_accounting_prefix_id: f.revision_id.clone(),
        writer_epoch: lease.epoch(),
        children: closures,
    };
    let prepared_id = f
        .cas
        .put_artifact(
            EXPERIMENT_PREPARED_V1,
            producer(),
            vec![
                f.revision_id.clone(),
                f.plan_id.clone(),
                slot_id.clone(),
                specification_id.clone(),
                child_plan_id.clone(),
                policy.clone(),
            ],
            None,
            serde_json::to_value(&prepared).unwrap(),
        )
        .unwrap()
        .0;
    *domain.prepared.lock().unwrap() = Some(prepared_id.clone());

    {
        let runtime =
            TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        assert!(!report.complete(), "{report:?}");
        let state = runtime.projection().unwrap();
        assert_eq!(
            state.phase,
            TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsPlanReview
            },
            "{report:?}"
        );
        assert_eq!(state.execution.unwrap().budget.begun_attempts(), 0);
    }
    let decision = ExperimentPlanDecisionV1 {
        schema: "af.experiment-plan-decision/1".into(),
        prepared_id: prepared_id.clone(),
        task_revision_id: f.revision_id.clone(),
        outer_plan_id: f.plan_id.clone(),
        slot_id,
        specification_id,
        compiled_child_plan_id: child_plan_id,
        policy_id: policy.clone(),
        developer: "fixture".into(),
        authorization_id: f.revision_id.clone(),
        key_policy_id: policy.clone(),
        signature_id: worker_package_id.clone(),
        decision: ExperimentDecisionKindV1::Approved,
        expires_unix_ms: f.task.limits.deadline_unix_ms,
        reason: "bounded_fixture".into(),
    };
    let decision_id = f
        .cas
        .put_artifact(
            EXPERIMENT_PLAN_DECISION_V1,
            producer(),
            vec![prepared_id.clone()],
            None,
            serde_json::to_value(&decision).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .decide_task_experiment(&f.cas, &lease, &prepared_id, &decision_id, &authority)
        .unwrap();
    f.store
        .register_task_experiment(&f.cas, &lease, &prepared_id, &authority)
        .unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    let attempts = runtime
        .projection()
        .unwrap()
        .execution
        .unwrap()
        .budget
        .begun_attempts();
    assert_eq!(attempts, 3);
    let requests = model.0.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let candidate_requests = requests
        .iter()
        .filter(|request| request["instructions"] == candidate_instructions)
        .collect::<Vec<_>>();
    assert_eq!(candidate_requests.len(), 1, "{requests:#?}");
    assert_eq!(
        candidate_requests[0]["inputs"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["configuration".into(), "input".into()])
    );
    assert!(
        !candidate_requests[0]["inputs"]
            .to_string()
            .contains(candidate_instructions),
        "candidate instructions were duplicated as a data input"
    );
    assert!(
        requests
            .iter()
            .filter(|request| request["instructions"]
                == "Return the checked document using the declared output contract.")
            .count()
            == 2,
        "baseline and ordinary invocation must retain only the original instructions: {requests:#?}"
    );
    assert_ne!(derived_package_id, worker_package_id);
    assert!(runtime.execute().unwrap().complete());
    assert_eq!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .budget
            .begun_attempts(),
        attempts
    );
    assert_eq!(model.0.lock().unwrap().len(), 3);

    // Delivery installs the selected package bytes as ordinary project authority. Capture a
    // fresh ordinary Task package with those exact bytes and prove the common model adapter sees
    // the same instruction identity without an experimental data port.
    let adopted_model = Model(Mutex::new(Vec::new()));
    let mut adopted =
        Fixture::with_model_instructions("unused model package file", true, candidate_instructions);
    let adopted_slot = adopted.plan.bindings.keys().next().unwrap().clone();
    assert_eq!(
        adopted.plan.bindings[&adopted_slot].package_digest,
        derived_digest
    );
    let adopted_models = BTreeMap::from([(
        adopted_slot.clone(),
        TaskModelBinding {
            binding: adopted.plan.bindings[&adopted_slot].clone(),
            adapter: &adopted_model as &dyn WorkerModelAdapter,
        },
    )]);
    let adopted_host = CapturedTaskHost::capture_with_models(
        &adopted.cas,
        &adopted.compiler,
        &adopted.task,
        &adopted.plan,
        adopted.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &adopted_models,
    )
    .unwrap();
    let adopted_authority =
        CapturedTaskAuthority::new(&adopted.compiler, &adopted_host, &NoTaskDeveloper);
    let adopted_lease = adopted
        .store
        .open_task(&adopted.cas, &adopted.revision_id, "adopted", 60_000)
        .unwrap();
    adopted
        .store
        .propose_task_plan(
            &adopted.cas,
            &adopted_lease,
            &adopted.plan_id,
            &adopted_authority,
        )
        .unwrap();
    adopted
        .store
        .admit_task_plan(&adopted.cas, &adopted_lease, &adopted_authority)
        .unwrap();
    let adopted_runtime = TaskRuntime::new(
        &mut adopted.store,
        &adopted.cas,
        adopted_lease,
        &adopted_authority,
        &adopted_host,
    )
    .unwrap();
    assert!(adopted_runtime.execute().unwrap().complete());
    let adopted_requests = adopted_model.0.lock().unwrap();
    assert_eq!(adopted_requests.len(), 1);
    assert_eq!(adopted_requests[0]["instructions"], candidate_instructions);
    assert_eq!(
        adopted_requests[0]["inputs"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["input"]
    );
}

#[test]
fn substituted_resolved_context_is_rejected_before_model_dispatch() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Model(AtomicUsize);
    impl WorkerModelAdapter for Model {
        fn provider_kind(&self) -> &'static str {
            "fixture"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("typed-model".into(), "high".into()))
        }
        fn invoke(
            &self,
            _: &Cas,
            _: &std::path::Path,
            _: Vec<u8>,
            _: std::time::Duration,
            _: bool,
        ) -> ModelWorkerReturn {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("a substituted context reached model dispatch")
        }
    }

    struct Substitute<'a> {
        inner: &'a dyn TaskOperatorHost,
        replacement: String,
    }
    impl TaskOperatorHost for Substitute<'_> {
        fn prepare_context_for_resolved_attempt(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            definition: &review_graph::task::CompiledNode,
            attempt: &review_store::store::task::execution::ReservedTaskAttempt,
        ) -> Result<String, String> {
            self.inner
                .prepare_context_for_resolved_attempt(cas, input, definition, attempt)?;
            Ok(self.replacement.clone())
        }
        fn prepare_context(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            feedback: &[String],
        ) -> Result<String, String> {
            self.inner.prepare_context(cas, input, feedback)
        }
        fn execute(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            attempt: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            self.inner.execute(cas, input, attempt)
        }
    }

    let model = Model(AtomicUsize::new(0));
    let mut fixture = Fixture::with_model("unused", true);
    let slot = fixture.plan.bindings.keys().next().unwrap().clone();
    let models = BTreeMap::from([(
        slot.clone(),
        TaskModelBinding {
            binding: fixture.plan.bindings[&slot].clone(),
            adapter: &model as &dyn WorkerModelAdapter,
        },
    )]);
    let host = CapturedTaskHost::capture_with_models(
        &fixture.cas,
        &fixture.compiler,
        &fixture.task,
        &fixture.plan,
        fixture.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &models,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&fixture.compiler, &host, &NoTaskDeveloper);
    let lease = fixture
        .store
        .open_task(&fixture.cas, &fixture.revision_id, "substitution", 60_000)
        .unwrap();
    fixture
        .store
        .propose_task_plan(&fixture.cas, &lease, &fixture.plan_id, &authority)
        .unwrap();
    fixture
        .store
        .admit_task_plan(&fixture.cas, &lease, &authority)
        .unwrap();
    let substituted = Substitute {
        inner: &host,
        replacement: fixture.revision_id.clone(),
    };
    let runtime = TaskRuntime::new(
        &mut fixture.store,
        &fixture.cas,
        lease,
        &authority,
        &substituted,
    )
    .unwrap();
    let report = runtime.execute().unwrap();
    assert!(!report.complete(), "{report:?}");
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert!(execution.pending_attempts().is_empty());
}

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-runtime-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}

impl Fixture {
    fn new(script: &str) -> Self {
        Self::with_model(script, false)
    }
    fn with_model(script: &str, model: bool) -> Self {
        Self::with_model_instructions(
            script,
            model,
            "Return the checked document using the declared output contract.",
        )
    }
    fn with_model_instructions(script: &str, model: bool, instructions: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1");
        let pipeline: PipelineDefinitionV1 =
            serde_json::from_slice(&std::fs::read(root.join("pipeline-definition.json")).unwrap())
                .unwrap();
        let mut task: TaskRevisionV1 =
            serde_json::from_slice(&std::fs::read(root.join("task-revision.json")).unwrap())
                .unwrap();
        let policy = cas
            .put_json(&json!({"fixture":"trusted kind and invocation policy"}))
            .unwrap();
        let source = cas
            .put_artifact(
                "af/Requirements@1",
                producer(),
                vec![],
                None,
                json!({"text":"A checked migration guide"}),
            )
            .unwrap()
            .0;
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source.clone()];
        task.provenance.input_artifact_ids = vec![source];
        task.provenance.adapter_id = policy.clone();
        task.authority.policy_id = policy.clone();
        task.authority.allowed_effects.clear();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        task.limits.deadline_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000;
        task.limits.verification.wall_ms = 5000;
        if model {
            task.limits.tokens = 5000;
            task.limits.verification.tokens = 1000;
        }
        let revision_id = cas
            .put_artifact(
                TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&task).unwrap(),
            )
            .unwrap()
            .0;
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let mut configuration_port = pipeline.contract.inputs["requirements"].clone();
        configuration_port.artifact_type = "af/OptimizationConfiguration@1".into();
        configuration_port.optional = true;
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([
                    (
                        "input".into(),
                        pipeline.contract.inputs["requirements"].clone(),
                    ),
                    ("configuration".into(), configuration_port),
                ]),
                outputs: BTreeMap::from([("output".into(), output)]),
            },
            effects: BTreeSet::new(),
            evidence: BTreeMap::from([("output".into(), BTreeSet::from([policy.clone()]))]),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["author".into()]),
            worker_input_type: Some("af/Requirements@1".into()),
            worker_output_type: Some("af/CheckedDocument@1".into()),
            outcome_port: Some("output".into()),
            attempt: Some(OperatorAttemptCost {
                tokens: if model { 1000 } else { 0 },
                wall_ms: 5000,
            }),
        };
        let worker = TaskWorkerManifest {
            schema: "af.worker/1".into(),
            name: "builtin/document-author".into(),
            version: "1.0.0".into(),
            signature,
            runner: if model {
                TaskWorkerRunner::Model {
                    provider_kind: "fixture".into(),
                    model: "typed-model".into(),
                    effort: "high".into(),
                }
            } else {
                TaskWorkerRunner::Command { command: serde_json::from_value(json!({"program":"/usr/bin/python3", "args":[{"value":"@package/worker.py", "provenance":"literal"}]})).unwrap() }
            },
        };
        let input_schema = json!({"type":"object", "additionalProperties":false, "required":["input"], "properties":{
            "input":{"type":"array", "minItems":1, "maxItems":1, "items":{"type":"object", "additionalProperties":false,
                "required":["artifact_id","artifact_type","payload"], "properties":{
                    "artifact_id":{"type":"string"}, "artifact_type":{"const":"af/Requirements@1"},
                    "payload":{"type":"object", "additionalProperties":false, "required":["text"], "properties":{"text":{"type":"string"}}}
                }}},
            "configuration":{"type":"array", "minItems":1, "maxItems":1, "items":{"type":"object", "additionalProperties":false,
                "required":["artifact_id","artifact_type","payload"], "properties":{
                    "artifact_id":{"type":"string"}, "artifact_type":{"const":"af/OptimizationConfiguration@1"},
                    "payload":{"type":"object"}
                }}}
        }});
        let output_schema = json!({"type":"object", "additionalProperties":false, "required":["outcome","text"],
            "properties":{"outcome":{"enum":["passed","failed","inconclusive"]},"text":{"type":"string"}}});
        let mut compiler = TaskPlanCompiler::new(
            policy.clone(),
            policy.clone(),
            BTreeMap::new(),
            BTreeMap::from([("checked".into(), "document".into())]),
            IndependencePolicyV1::default(),
        )
        .unwrap();
        for (name, files) in [
            (
                "builtin/document",
                BTreeMap::from([(
                    "pipeline.toml".into(),
                    toml::to_string(&pipeline).unwrap().into_bytes(),
                )]),
            ),
            (
                "builtin/document-author",
                BTreeMap::from([
                    (
                        "worker.toml".into(),
                        toml::to_string(&worker).unwrap().into_bytes(),
                    ),
                    (
                        "input.schema.json".into(),
                        serde_json::to_vec(&input_schema).unwrap(),
                    ),
                    (
                        "outputs/output.schema.json".into(),
                        serde_json::to_vec(&output_schema).unwrap(),
                    ),
                    ("instructions.md".into(), instructions.as_bytes().to_vec()),
                    ("worker.py".into(), script.as_bytes().to_vec()),
                ]),
            ),
        ] {
            let pin = TaskPackagePin {
                version: "1.0.0".into(),
                digest: review_config::lock::package_digest_from_files(&files),
                path: "package".into(),
            };
            let files = files
                .into_iter()
                .map(|(path, bytes)| (format!("package/{path}"), bytes))
                .collect();
            compiler.capture_package(&cas, name, &pin, &files).unwrap();
        }
        compiler
            .bind_worker(
                "builtin/document-author",
                AdmittedWorkerSettings {
                    execution: if model {
                        WorkerExecutionV1::Model {
                            provider: "personal".into(),
                            provider_kind: "fixture".into(),
                            principal_id: policy.clone(),
                            model: "typed-model".into(),
                            effort: "high".into(),
                        }
                    } else {
                        WorkerExecutionV1::Command {}
                    },
                    invocation_policy_id: policy,
                },
            )
            .unwrap();
        let (plan, graph) = compiler
            .compile(&cas, &revision_id, "builtin/document")
            .unwrap();
        let plan_id = cas
            .put_artifact(
                EXECUTION_PLAN_V1,
                producer(),
                vec![revision_id.clone()],
                None,
                serde_json::to_value(&plan).unwrap(),
            )
            .unwrap()
            .0;
        Self {
            _directory: directory,
            cas,
            store,
            compiler,
            task,
            revision_id,
            plan,
            plan_id,
            graph,
        }
    }
}

/// Only this test document kind is installed; production kinds must provide their own
/// acceptance and receipt validation rather than inheriting an always-successful handler.
struct DocumentDomain;
impl TaskOperatorHost for DocumentDomain {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        Err("No built-in Worker".into())
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("Only the command Worker should execute")
    }
}
impl TaskDomain for DocumentDomain {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        for port in output.outputs.values() {
            if port.artifact_type != "af/CheckedDocument@1" {
                return Err("Unknown document output".into());
            }
            for id in &port.artifact_ids {
                if cas.get_json(id).map_err(|e| e.to_string())?["payload"]["text"]
                    .as_str()
                    .is_none()
                {
                    return Err("Document text is absent".into());
                }
            }
        }
        Ok(())
    }
    fn validate_result(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        if result.acceptance == TaskAcceptanceV1::Satisfied
            && result.evidence.iter().any(|id| {
                cas.get_json(id)
                    .map_or(true, |v| v["payload"]["outcome"] != "passed")
            })
        {
            return Err("Document has no positive verifier receipt".into());
        }
        Ok(())
    }
}

const SUCCESS: &str = r#"import json, sys
request = json.load(sys.stdin)
assert request['schema'] == 'af.worker-request/1'
assert set(request['inputs']) == {'input'}
assert 'goal' not in request and 'task' not in request
assert request['feedback'] == []
text = request['inputs']['input'][0]['payload']['text']
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'output':[{'outcome':'passed','text':text}]}}))
"#;

#[test]
fn domain_observes_started_attempt_and_persists_through_the_runtime_store() {
    use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
    use review_core::task::execution::TaskExecutionRecordV1;
    use review_store::SharedEventStore;
    use review_store::store::task::{TaskLease, task_run_id};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Observer<'a> {
        store: SharedEventStore<'a>,
        lease: TaskLease,
        inner: &'a dyn TaskOperatorHost,
        calls: AtomicUsize,
    }
    impl<'a> Observer<'a> {
        fn lock(&self) -> std::sync::MutexGuard<'_, &'a mut EventStore> {
            // A heartbeat may briefly own the connection. Bound the wait so a runtime that
            // accidentally calls the host while holding its lock fails instead of hanging.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match self.store.try_lock() {
                    Ok(store) => return store,
                    Err(std::sync::TryLockError::WouldBlock)
                        if std::time::Instant::now() < until =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("host cannot access the shared Store: {error}"),
                }
            }
        }
    }
    impl TaskOperatorHost for Observer<'_> {
        fn prepare_context(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            feedback: &[String],
        ) -> Result<String, String> {
            {
                let store = self.lock();
                let state = store
                    .task_projection(cas, self.lease.task_id())
                    .unwrap()
                    .unwrap();
                assert_eq!(state.plan_id.as_deref(), Some(input.plan_id.as_str()));
            }
            self.inner.prepare_context(cas, input, feedback)
        }

        fn execute(
            &self,
            cas: &Cas,
            input: &TaskInvocationV1,
            attempt: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            let attempt = attempt.expect("the fixture invokes one Worker");
            {
                let mut store = self.lock();
                let events = store
                    .replay(&task_run_id(self.lease.task_id()).unwrap())
                    .unwrap();
                let started = events.iter().any(|event| {
                    let transition: TaskTransitionV1 = serde_json::from_value(event.payload.clone()).unwrap();
                    let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                        return false;
                    };
                    let record = review_store::store::task::execution::read_execution_record(cas, &record_id).unwrap().record;
                    matches!(record, TaskExecutionRecordV1::Started { attempt_id } if attempt_id == attempt.id())
                });
                assert!(
                    started,
                    "the domain must observe the durable Started barrier"
                );
                // Exercise a domain-side durable mutation on the same connection while the
                // runtime owns a live Attempt. The subsequent settlement must see this lease.
                store.renew_task_lease(cas, &self.lease, 60_000).unwrap();
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(cas, input, Some(attempt))
        }
    }

    let mut f = Fixture::new(SUCCESS);
    let host = CommandTaskHost::capture(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "shared-store", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let shared = SharedEventStore::new(&mut f.store);
    let observer = Observer {
        store: shared.clone(),
        lease: lease.clone(),
        inner: &host,
        calls: AtomicUsize::new(0),
    };
    let runtime =
        TaskRuntime::with_store(shared.clone(), &f.cas, lease.clone(), &authority, &observer)
            .unwrap();
    assert!(runtime.execute().unwrap().complete());
    assert!(runtime.execute().unwrap().complete());
    assert_eq!(observer.calls.load(Ordering::SeqCst), 1);
    let projection = shared
        .lock()
        .unwrap()
        .task_projection(&f.cas, lease.task_id())
        .unwrap()
        .unwrap();
    let execution = projection.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.pending_attempts().is_empty());
    assert!(execution.outputs.contains_key("root.nodes.write"));
}

#[test]
fn provider_admission_is_charged_once_and_failed_admission_dispatches_no_business_worker() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model {
        calls: AtomicUsize,
        pass: bool,
    }
    impl WorkerModelAdapter for Model {
        fn provider_kind(&self) -> &'static str {
            "fixture"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("typed-model".into(), "high".into()))
        }
        fn invoke(
            &self,
            cas: &Cas,
            _: &std::path::Path,
            input: Vec<u8>,
            _: std::time::Duration,
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let (bytes, cost) = if n == 0 {
                assert_eq!(input, b"Reply with exactly: OK\n");
                (
                    if self.pass {
                        b"OK".to_vec()
                    } else {
                        b"capability unavailable".to_vec()
                    },
                    7,
                )
            } else {
                assert!(self.pass, "failed admission reached a business Worker");
                let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
                assert_eq!(request["inputs"].as_object().unwrap().len(), 1);
                assert!(request["inputs"]["input"].is_array());
                (serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"Checked document"}]}})).unwrap(),11)
            };
            ModelWorkerReturn {
                usage_observation: None,
                raw_artifact_ids: vec![cas.put(&bytes).unwrap()],
                message: Ok(bytes),
                usage: Some(review_runner::TokenUsage::charge_only(cost).into()),
            }
        }
    }
    for pass in [true, false] {
        let model = Model {
            calls: AtomicUsize::new(0),
            pass,
        };
        let mut f = Fixture::with_model("unused", true);
        f.task.limits.verification.tokens = 1100;
        f.task.limits.verification.attempts = 2;
        f.task.limits.verification.wall_ms = 6000;
        f.revision_id = f
            .cas
            .put_artifact(
                TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&f.task).unwrap(),
            )
            .unwrap()
            .0;
        f.compiler = f.compiler.with_provider_admission(OperatorAttemptCost {
            tokens: 100,
            wall_ms: 1000,
        });
        (f.plan, f.graph) = f
            .compiler
            .compile(&f.cas, &f.revision_id, "builtin/document")
            .unwrap();
        f.plan_id = f
            .cas
            .put_artifact(
                EXECUTION_PLAN_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&f.plan).unwrap(),
            )
            .unwrap()
            .0;
        assert_eq!(
            f.graph.allowances["root.providers.admit0"].verification_attempts,
            1
        );
        let models = f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: &model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let domain = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &f.graph,
            models: &models,
            inner: &DocumentDomain,
        };
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &domain,
            &models,
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer", 60000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        let report = runtime.execute().unwrap();
        assert_eq!(report.complete(), pass, "{report:?}");
        assert_eq!(runtime.execute().unwrap().complete(), pass);
        assert_eq!(model.calls.load(Ordering::SeqCst), if pass { 2 } else { 1 });
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(
            execution.budget.committed_tokens(),
            if pass { 18 } else { 7 }
        );
        assert_eq!(execution.budget.begun_attempts(), if pass { 2 } else { 1 });
        assert_eq!(execution.outputs.contains_key("root.nodes.write"), pass);
        assert_eq!(
            execution.outputs.contains_key("root.providers.admit0"),
            pass
        );
        assert!(execution.pending_attempts().is_empty());
    }
}

#[test]
fn model_schema_failure_keeps_usage_and_retry_runs_through_the_same_task_budget() {
    use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model(AtomicUsize);
    impl WorkerModelAdapter for Model {
        fn provider_kind(&self) -> &'static str {
            "fixture"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("typed-model".into(), "high".into()))
        }
        fn invoke(
            &self,
            cas: &Cas,
            _: &std::path::Path,
            bytes: Vec<u8>,
            _: std::time::Duration,
            writable: bool,
        ) -> ModelWorkerReturn {
            assert!(!writable);
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                request["inputs"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>(),
                ["input"]
            );
            assert_eq!(
                request["reply_format"],
                review_runner::task::WORKER_REPLY_FORMAT
            );
            assert!(request.get("task").is_none());
            let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
            assert_eq!(
                request["feedback"].as_array().unwrap().len(),
                usize::from(!first)
            );
            if !first {
                assert_eq!(
                    request["feedback"][0]["payload"]["code"],
                    "invalid_output_contract"
                );
                assert!(!String::from_utf8_lossy(&bytes).contains("malformed response"));
            }
            let message = if first {
                b"malformed response".to_vec()
            } else {
                serde_json::to_vec(&json!({"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"A checked migration guide"}]}})).unwrap()
            };
            ModelWorkerReturn {
                usage_observation: None,
                raw_artifact_ids: vec![cas.put(&message).unwrap()],
                message: Ok(message),
                usage: Some(
                    review_runner::TokenUsage::charge_only(if first { 20 } else { 30 }).into(),
                ),
            }
        }
    }
    let model = Model(AtomicUsize::new(0));
    let mut f = Fixture::with_model("unused model package file", true);
    let slot = f.plan.bindings.keys().next().unwrap().clone();
    let mut models = BTreeMap::from([(
        slot.clone(),
        TaskModelBinding {
            binding: f.plan.bindings[&slot].clone(),
            adapter: &model as &dyn WorkerModelAdapter,
        },
    )]);
    models.get_mut(&slot).unwrap().binding.invocation_policy_id = f.revision_id.clone();
    assert!(
        CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &DocumentDomain,
            &models
        )
        .is_err()
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    models.get_mut(&slot).unwrap().binding = f.plan.bindings[&slot].clone();
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &models,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "model-test", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    assert!(runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 2);
    assert_eq!(execution.budget.committed_tokens(), 50);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    assert!(execution.pending_attempts().is_empty());
    drop(runtime);
    let wall = f
        .store
        .attempt_wall(&review_store::store::task::task_run_id(lease.task_id()).unwrap())
        .unwrap();
    assert_eq!(wall.len(), 2);
    assert_eq!(
        wall.iter()
            .map(|a| a.usage.as_ref().unwrap().chargeable_tokens)
            .sum::<u64>(),
        50
    );
}

#[test]
fn captured_command_worker_executes_and_replays_through_the_common_task_runtime() {
    let mut f = Fixture::new(SUCCESS);
    let host = CommandTaskHost::capture(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "test-writer", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(report.complete(), "{report:?}");
    assert!(runtime.execute().unwrap().complete());
    let execution = runtime.projection().unwrap().execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.committed_tokens(), 0);
    let output = execution.outputs["root.nodes.write"].1.outputs["output"].clone();
    assert_eq!(
        f.cas.get_json(&output.artifact_ids[0]).unwrap()["payload"]["text"],
        "A checked migration guide"
    );
    let result = TaskResultV1 {
        task_revision_id: f.revision_id.clone(),
        execution: TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: "checked document produced".into(),
        evidence: output.artifact_ids.iter().cloned().collect(),
        outputs: BTreeMap::from([("document".into(), output)]),
        missing_obligations: BTreeSet::new(),
    };
    let result_id = f
        .cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&result_id).unwrap();
    drop(runtime);
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &authority)
            .is_err()
    );
}

#[test]
fn failed_command_and_schema_refusal_exhaust_bounded_attempts_without_publishing_a_result() {
    for script in [
        "import sys\nprint('paid process failed')\nsys.exit(17)\n",
        "print('{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"output\":[{\"outcome\":\"passed\"}]}}')\n",
    ] {
        let mut f = Fixture::new(script);
        let host = CommandTaskHost::capture(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &DocumentDomain,
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "test-writer", 60_000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        assert!(!runtime.execute().unwrap().complete());
        let execution = runtime.projection().unwrap().execution.unwrap();
        assert_eq!(execution.budget.begun_attempts(), 2);
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert!(!execution.outputs.contains_key("root.nodes.write"));
        assert!(execution.pending_attempts().is_empty());
    }
}
