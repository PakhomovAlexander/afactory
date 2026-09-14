//! Exercise the actual Store barrier with charged work, forged transitions and late usage.
use super::*;
use review_core::task::execution::*;
use review_core::task::plan::PlanPreparationV1;
use review_core::task::planning::*;
use review_graph::task::{Address, CompiledTask};

fn artifact<T: serde::Serialize>(cas: &Cas, kind: &str, value: &T) -> String {
    cas.put_artifact(
        kind,
        producer(),
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

#[test]
fn planning_barrier_preserves_paid_history_fences_forgery_and_charges_late_usage() {
    let mut f = Fixture::new(false).with_execution_graph();
    let mut next_plan = f.plan.clone();
    let mut bootstrap: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    bootstrap.coverage.clear();
    bootstrap.outputs = BTreeMap::from([(
        "proposal".into(),
        Address {
            node: "root.nodes.write".into(),
            port: "output".into(),
        },
    )]);
    bootstrap.calls.get_mut("root").unwrap().coverage.clear();
    bootstrap.calls.get_mut("root").unwrap().outputs = bootstrap.outputs.clone();
    bootstrap.slots.get_mut("root.slots.author").unwrap().role = "plan".into();
    let output = bootstrap
        .nodes
        .get_mut("root.nodes.write")
        .unwrap()
        .contract
        .outputs
        .get_mut("output")
        .unwrap();
    output.artifact_type = PIPELINE_PROPOSAL_V1.into();
    output.covers.clear();
    bootstrap
        .allowances
        .get_mut("root.nodes.write")
        .unwrap()
        .verification_attempts = 0;
    f.plan.preparation = Some(PlanPreparationV1::Planning {});
    f.plan.acceptance.clear();
    f.plan.compiled_graph_id = artifact(&f.cas, "af/CompiledTask@1", &bootstrap);
    f.plan_id = artifact(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
    let bootstrap_id = f.plan_id.clone();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation_id = f.record_execution_inputs(&lease);
    let context_id = f
        .cas
        .put_json(&json!({"fixture_context":"planning"}))
        .unwrap();
    let planner = f
        .store
        .prepare_task_attempt(
            &f.cas,
            &lease,
            "root.nodes.write",
            &context_id,
            &f.authority,
        )
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &planner, &f.authority)
        .unwrap();
    assert!(f.state().planning_proof(&f.cas).is_err());
    let producer = Producer::Attempt {
        run_id: task_run_id("task-1").unwrap(),
        node_id: "root.nodes.write".into(),
        attempt_id: planner.id().into(),
    };
    let proposal = f.cas.put_artifact(PIPELINE_PROPOSAL_V1,producer.clone(),vec![invocation_id.clone()],None,
        json!({"schema":"af.pipeline-proposal/1","root":"generated/document","definitions":{"generated/document":"Fixture compiler owns TOML admission"}})).unwrap().0;
    let wrapper = f
        .cas
        .put_artifact(
            TASK_OUTPUT_V1,
            producer,
            vec![invocation_id.clone(), proposal.clone()],
            None,
            serde_json::to_value(TaskOutputV1 {
                invocation_id,
                outputs: BTreeMap::from([(
                    "output".into(),
                    task::ArtifactInputV1 {
                        artifact_ids: vec![proposal.clone()],
                        artifact_type: PIPELINE_PROPOSAL_V1.into(),
                        cardinality: review_core::PortCardinality::One,
                        snapshot_id: None,
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: planner.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: wrapper.clone(),
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    assert!(
        f.state().planning_proof(&f.cas).is_err(),
        "Settlement alone is not publication"
    );
    f.store
        .publish_task_output(&f.cas, &lease, &wrapper, Some(planner.id()), &f.authority)
        .unwrap();
    let proof = f.state().planning_proof(&f.cas).unwrap();
    assert_eq!(proof.proposal_id(), proposal);
    assert_eq!(proof.bootstrap_plan_id(), bootstrap_id);
    let mut next = f.revision.clone();
    next.revision += 1;
    next.previous_revision_id = Some(f.revision_id.clone());
    let next_id = artifact(&f.cas, task::TASK_REVISION_V1, &next);
    next_plan.task_revision_id = next_id.clone();
    next_plan.generated_origins = vec![GeneratedOriginV1 {
        pipeline_id: next_plan.pipeline_id.clone(),
        proposal_id: proposal.clone(),
        bootstrap_plan_id: bootstrap_id.clone(),
    }];
    let next_plan_id = artifact(&f.cas, task::EXECUTION_PLAN_V1, &next_plan);
    let sequence = f.state().next_sequence;
    for changed in ["goal", "tokens", "deadline", "input", "predecessor"] {
        let mut forged = next.clone();
        match changed {
            "goal" => forged.goal = "Unreviewed replacement business objective".into(),
            "tokens" => forged.limits.tokens += 1,
            "deadline" => forged.limits.deadline_unix_ms += 1,
            "input" => {
                forged.inputs.get_mut("requirements").unwrap().artifact_ids =
                    vec![proposal.clone()];
            }
            _ => forged.previous_revision_id = Some(proposal.clone()),
        }
        let forged_id = artifact(&f.cas, task::TASK_REVISION_V1, &forged);
        let mut forged_plan = next_plan.clone();
        forged_plan.task_revision_id = forged_id.clone();
        forged_plan.inputs = forged.inputs.clone();
        forged_plan.limits = forged.limits.clone();
        let forged_plan_id = artifact(&f.cas, task::EXECUTION_PLAN_V1, &forged_plan);
        assert!(
            f.store
                .task_change(
                    &f.cas,
                    &lease,
                    TaskChangeV1::PlanningCompleted {
                        bootstrap_plan_id: bootstrap_id.clone(),
                        proposal_id: proposal.clone(),
                        revision_id: forged_id,
                        plan_id: forged_plan_id,
                    },
                    now().unwrap()
                )
                .is_err(),
            "{changed}"
        );
        assert_eq!(f.state().next_sequence, sequence);
    }
    f.authority.generated = next_plan.generated_origins.clone();
    f.store
        .complete_task_planning(&f.cas, &lease, &next_id, &next_plan_id, &f.authority)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert!(!state.admitted);
    assert_eq!(state.planning_proof(&f.cas).unwrap(), proof);
    assert_eq!(
        state.phase,
        TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsPlanReview
        }
    );
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(execution.invocations.is_empty() && execution.outputs.is_empty());
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .complete_task_planning(&f.cas, &lease, &next_id, &next_plan_id, &f.authority)
            .is_err()
    );
    f.plan = next_plan;
    f.plan_id = next_plan_id;
    f.revision = next;
    f.revision_id = next_id;
    f.decide(&lease, PlanDecisionKindV1::Approved);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation = f.record_execution_inputs(&lease);
    let work = f
        .store
        .prepare_task_attempt(
            &f.cas,
            &lease,
            "root.nodes.write",
            &context_id,
            &f.authority,
        )
        .unwrap();
    assert_ne!(work.id(), planner.id());
    f.store
        .start_task_attempt(&f.cas, &lease, &work, &f.authority)
        .unwrap();
    let output = f.execution_output(&invocation, work.id());
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: work.id().into(),
                charged_tokens: 9,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: output.clone(),
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    f.store
        .publish_task_output(&f.cas, &lease, &output, Some(work.id()), &f.authority)
        .unwrap();
    let usage = f.cas.put_json(&json!({"late_usage":11})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: planner.id().into(),
                charged_tokens: 11,
                usage_id: usage,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 20);
    assert_eq!(execution.budget.begun_attempts(), 2);
    assert_eq!(
        execution.budget.remaining_limits().deadline_unix_ms,
        f.revision.limits.deadline_unix_ms
    );
    assert_eq!(execution.outputs["root.nodes.write"].0, output);
}
