use super::*;
use crate::store::task::execution::owned::*;
use review_core::task::{execution::*, owned_children::*};
use review_graph::task::{CompiledOperator, CompiledTask, OwnedChildTemplateV1, ReviewOperation};

const PARENT: &str = "root.nodes.write";

mod shards;

struct OwnedAuthority<'a>(&'a Authority);
impl TaskAuthority for OwnedAuthority<'_> {
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        self.0.validate_plan(cas, task, plan)
    }
    fn authorize_decision(
        &self,
        task: &TaskRevisionV1,
        plan: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        self.0.authorize_decision(task, plan, decision)
    }
    fn authorization_current(&self, decision: &PlanDecisionV1) -> Result<(), String> {
        self.0.authorization_current(decision)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.0.validate_result(cas, task, result)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        self.0
            .validate_output(cas, task, plan, input, output, definition)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        attempt: &crate::store::task::execution::ReservedTaskAttempt,
        context: &str,
    ) -> Result<(), String> {
        self.0
            .validate_context(cas, task, plan, input, attempt, context)
    }
    fn validate_owned_children(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        if parent.node != PARENT || set.children.len() != 2 {
            return Err("Fixture requires complete two-item source".into());
        }
        let source = cas
            .get_artifact(&set.source_artifact_id)
            .map_err(|e| e.to_string())?;
        if source.artifact_type == review_core::contract::SLICE_SET_V1 {
            let slices: review_core::SliceSetV1 =
                serde_json::from_value(source.payload).map_err(|e| e.to_string())?;
            slices.validate()?;
            for (child, slice) in set.children.iter().zip(&slices.slices) {
                if cas
                    .get_artifact(&child.source_item_id)
                    .map_err(|e| e.to_string())?
                    .payload
                    != serde_json::to_value(slice).map_err(|e| e.to_string())?
                {
                    return Err("Changed captured Review Slice".into());
                }
            }
            return Ok(());
        }
        for (index, child) in set.children.iter().enumerate() {
            if cas
                .get_artifact(&child.source_item_id)
                .map_err(|e| e.to_string())?
                .payload
                != json!({"index":index})
            {
                return Err("Changed source item".into());
            }
        }
        Ok(())
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
        facts: &[TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if let Some(port) = output.outputs.get("o0") {
            let slices: review_core::SliceSetV1 = serde_json::from_value(
                cas.get_artifact(&set.source_artifact_id)
                    .map_err(|e| e.to_string())?
                    .payload,
            )
            .map_err(|e| e.to_string())?;
            let expected = shards::missing_shards(&slices, &set.source_artifact_id);
            if parent.node != PARENT
                || facts
                    .iter()
                    .any(|fact| fact.attempts.iter().any(|attempt| attempt.started))
                || cas
                    .get_artifact(&port.artifact_ids[0])
                    .map_err(|e| e.to_string())?
                    .payload
                    != serde_json::to_value(expected).map_err(|e| e.to_string())?
            {
                return Err("Changed complete Missing Review fold".into());
            }
            return Ok(());
        }
        if parent.node != PARENT
            || cas
                .get_artifact(&output.outputs["output"].artifact_ids[0])
                .map_err(|e| e.to_string())?
                .payload
                != folded(facts)
        {
            return Err("Fold omitted or reclassified durable child facts".into());
        }
        Ok(())
    }
}

fn folded(facts: &[TaskOwnedChildEvidence]) -> serde_json::Value {
    json!({"children":facts.iter().map(|fact|json!({"node":fact.child.node,"outcome":if fact.completed_artifact_ids.is_some(){"completed"} else if fact.attempts.iter().any(|attempt|attempt.started){"failed"}else{"missing"}})).collect::<Vec<_>>()})
}

fn fixture(generated: bool, deadline: Option<u64>) -> Fixture {
    let mut f = Fixture::new(generated);
    if let Some(deadline) = deadline {
        f.revision.limits.deadline_unix_ms = deadline;
        f.revision_id = f
            .cas
            .put_artifact(
                task::TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&f.revision).unwrap(),
            )
            .unwrap()
            .0;
        f.plan.task_revision_id = f.revision_id.clone();
        f.plan.limits = f.revision.limits.clone();
    }
    with_owned_graph(f.with_execution_graph())
}

fn with_owned_graph(mut f: Fixture) -> Fixture {
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    let mut child = graph.nodes[PARENT].clone();
    let mut item = child.contract.inputs["input"].clone();
    item.artifact_type = "af/OwnedItem@1".into();
    child.contract.inputs.insert("item".into(), item);
    let mut allowance = graph.allowances.remove(PARENT).unwrap();
    allowance.verification_attempts = 0;
    graph.nodes.get_mut(PARENT).unwrap().operator = CompiledOperator::ReviewDomain {
        review_node: "scatter".into(),
        operation: ReviewOperation::Scatter {
            slot: "author".into(),
        },
    };
    graph.owned_children.insert(
        PARENT.into(),
        OwnedChildTemplateV1 {
            operator: child.operator,
            contract: child.contract,
            allowance,
            max_children: 2,
            source_input: "input".into(),
            item_input: "item".into(),
            inherited_inputs: BTreeMap::from([("input".into(), "input".into())]),
        },
    );
    graph.token_scopes.insert(
        "review.round1.scatter".into(),
        review_attempt::task_budget::TaskTokenScope {
            tokens: 30,
            members: BTreeSet::from([PARENT.into()]),
        },
    );
    graph.budget(f.revision.limits.clone()).unwrap();
    f.plan.compiled_graph_id = f
        .cas
        .put_artifact(
            "af/CompiledTask@1",
            producer(),
            vec![],
            None,
            serde_json::to_value(graph).unwrap(),
        )
        .unwrap()
        .0;
    f.plan_id = f
        .cas
        .put_artifact(
            task::EXECUTION_PLAN_V1,
            producer(),
            vec![f.revision_id.clone()],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;
    f
}
fn started(f: &mut Fixture) -> (TaskLease, String) {
    let lease = f.open();
    f.propose(&lease);
    if !f.plan.generated_origins.is_empty() {
        f.decide(&lease, PlanDecisionKindV1::Approved {});
    }
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let parent = f.record_execution_inputs(&lease);
    (lease, parent)
}
fn children(f: &Fixture, parent: &str) -> (String, TaskOwnedChildSetV1) {
    let source = f.revision.inputs["requirements"].artifact_ids[0].clone();
    let mut children = vec![];
    for index in 0..2 {
        let item = f
            .cas
            .put_artifact(
                "af/OwnedItem@1",
                producer(),
                vec![source.clone()],
                None,
                json!({"index":index}),
            )
            .unwrap()
            .0;
        let node = format!("{PARENT}.slice{}", index + 1);
        let input = TaskInvocationV1 {
            plan_id: f.plan_id.clone(),
            node: node.clone(),
            inputs: BTreeMap::from([
                ("input".into(), f.revision.inputs["requirements"].clone()),
                (
                    "item".into(),
                    task::ArtifactInputV1 {
                        artifact_ids: vec![item.clone()],
                        artifact_type: "af/OwnedItem@1".into(),
                        cardinality: review_core::PortCardinality::One,
                        snapshot_id: None,
                    },
                ),
            ]),
        };
        let invocation = f
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                producer(),
                vec![f.plan_id.clone(), item.clone()],
                None,
                serde_json::to_value(input).unwrap(),
            )
            .unwrap()
            .0;
        children.push(TaskOwnedChildV1 {
            node,
            source_item_id: item,
            invocation_id: invocation,
        });
    }
    let set = TaskOwnedChildSetV1 {
        plan_id: f.plan_id.clone(),
        parent_invocation_id: parent.into(),
        source_artifact_id: source,
        children,
    };
    (write_set(f, &set), set)
}
fn write_set(f: &Fixture, set: &TaskOwnedChildSetV1) -> String {
    f.cas
        .put_artifact(
            TASK_OWNED_CHILD_SET_V1,
            producer(),
            set.artifact_refs().into_iter().map(str::to_owned).collect(),
            None,
            serde_json::to_value(set).unwrap(),
        )
        .unwrap()
        .0
}
fn register(f: &mut Fixture, lease: &TaskLease, id: &str) -> RegisteredTaskChildren {
    f.store
        .register_task_owned_children(&f.cas, lease, id, &OwnedAuthority(&f.authority))
        .unwrap()
}
fn run_child(
    f: &mut Fixture,
    lease: &TaskLease,
    child: &TaskOwnedChildV1,
) -> execution::PreparedTaskAttempt {
    f.store
        .record_task_invocation(&f.cas, lease, &child.invocation_id, &f.authority)
        .unwrap();
    let context = f.cas.put_json(&json!({"context":child.node})).unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, lease, &child.node, &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, lease, &attempt, &f.authority)
        .unwrap();
    attempt
}
fn out(
    f: &Fixture,
    input: &str,
    node: &str,
    attempt: Option<&str>,
    value: serde_json::Value,
) -> String {
    let producer = attempt.map_or_else(producer, |attempt| Producer::Attempt {
        run_id: task_run_id(&f.revision.task_id).unwrap(),
        node_id: node.into(),
        attempt_id: attempt.into(),
    });
    let result = f
        .cas
        .put_artifact(
            "af/CheckedDocument@1",
            producer.clone(),
            vec![input.into()],
            None,
            value,
        )
        .unwrap()
        .0;
    let output = TaskOutputV1 {
        invocation_id: input.into(),
        outputs: BTreeMap::from([(
            "output".into(),
            task::ArtifactInputV1 {
                artifact_ids: vec![result.clone()],
                artifact_type: "af/CheckedDocument@1".into(),
                cardinality: review_core::PortCardinality::One,
                snapshot_id: None,
            },
        )]),
    };
    f.cas
        .put_artifact(
            TASK_OUTPUT_V1,
            producer,
            vec![input.into(), result],
            None,
            serde_json::to_value(output).unwrap(),
        )
        .unwrap()
        .0
}
fn parent_out(f: &Fixture, handle: &RegisteredTaskChildren) -> String {
    let facts = f.store.task_owned_child_evidence(&f.cas, handle).unwrap();
    out(
        f,
        &handle.child_set().parent_invocation_id,
        PARENT,
        None,
        folded(&facts),
    )
}
fn observe(f: &mut Fixture, lease: &TaskLease, attempt: &str, charge: u128) {
    let usage_id = f
        .cas
        .put_json(&json!({"actual":charge.to_string()}))
        .unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.into(),
                charged_tokens: charge,
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
}

#[test]
fn owned_registration_is_complete_non_reserving_exact_and_replayable() {
    let mut f = fixture(false, None);
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    let before = f.state().next_sequence;
    assert!(
        f.store
            .record_task_invocation(&f.cas, &lease, &set.children[0].invocation_id, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, PARENT, &f.authority)
            .is_err()
    );
    let mut incomplete = set.clone();
    incomplete.children.pop();
    let bad = write_set(&f, &incomplete);
    assert!(
        f.store
            .register_task_owned_children(&f.cas, &lease, &bad, &OwnedAuthority(&f.authority))
            .is_err()
    );
    assert!(
        f.store
            .register_task_owned_children(&f.cas, &lease, &id, &f.authority)
            .is_err(),
        "default authority refuses ownership"
    );
    assert_eq!(f.state().next_sequence, before);
    let handle = register(&mut f, &lease, &id);
    assert_eq!(f.state().execution.unwrap().budget.begun_attempts(), 0);
    assert_eq!(register(&mut f, &lease, &id).child_set_id(), id);
    assert_eq!(f.state().next_sequence, before + 1);
    f.store = EventStore::open(&f.path).unwrap();
    let restored = f
        .store
        .get_task_owned_children(&f.cas, "task-1", &parent)
        .unwrap()
        .unwrap();
    assert_eq!(restored.child_set(), &set);
    let state = f.state();
    let execution = state.execution.unwrap();
    assert_eq!(
        execution
            .resolve_node(&set.children[0].node)
            .unwrap()
            .allowance
            .unwrap()
            .tokens_per_attempt,
        10
    );
    assert_eq!(execution.budget.committed_tokens(), 0);
    assert_eq!(
        f.store
            .task_owned_child_evidence(&f.cas, &handle)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn owned_common_attempt_overrun_can_publish_facts_and_seal_without_new_dispatch() {
    let mut f = fixture(false, None);
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    let handle = register(&mut f, &lease, &id);
    let child = &set.children[0];
    let attempt = run_child(&mut f, &lease, child);
    let result = out(
        &f,
        &child.invocation_id,
        &child.node,
        Some(attempt.id()),
        json!({"business":"selected"}),
    );
    let charge = u128::from(u64::MAX) + 7;
    observe(&mut f, &lease, attempt.id(), charge);
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 3,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: result.clone(),
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    assert!(
        f.store
            .publish_task_output(&f.cas, &lease, &result, Some(attempt.id()), &f.authority)
            .is_err()
    );
    PROJECTION_CALLS.with(|calls| calls.set(0));
    f.store
        .publish_task_owned_child(&f.cas, &lease, &handle, &result, attempt.id(), &f.authority)
        .unwrap();
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 2);
    let parent = parent_out(&f, &handle);
    PROJECTION_CALLS.with(|calls| calls.set(0));
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &parent,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 3);
    let before = f.state().next_sequence;
    assert!(
        f.store
            .record_task_invocation(&f.cas, &lease, &set.children[1].invocation_id, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, &child.node, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    observe(&mut f, &lease, attempt.id(), charge + 9);
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), charge + 9);
    assert_eq!(
        execution
            .budget
            .scope_committed_tokens("review.round1.scatter"),
        Some(charge + 9)
    );
    assert_eq!(execution.outputs[PARENT].0, parent);
    assert_eq!(execution.budget.begun_attempts(), 1);
    let next = f.state().next_sequence;
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &parent,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    assert_eq!(f.state().next_sequence, next);
    let records = f
        .store
        .replay(&task_run_id("task-1").unwrap())
        .unwrap()
        .into_iter()
        .filter_map(|event| {
            let transition: TaskTransitionV1 = serde_json::from_value(event.payload).ok()?;
            let TaskChangeV1::ExecutionRecorded { record_id } = transition.change else {
                return None;
            };
            let frame = f.cas.get_artifact(&record_id).unwrap();
            Some((
                frame.artifact_type,
                frame.payload["kind"].as_str()?.to_string(),
            ))
        })
        .collect::<Vec<_>>();
    // One closed record type carries every lifecycle kind, owned children included.
    assert!(
        records
            .iter()
            .all(|(kind, _)| kind == TASK_EXECUTION_RECORD_V5)
    );
    assert_eq!(
        records
            .iter()
            .filter(|(_, kind)| kind.starts_with("owned_child"))
            .count(),
        3
    );
    assert!(records.iter().any(|(_, kind)| kind == "settled"));
}

#[test]
fn owned_pending_and_false_completion_refused_without_event() {
    let mut f = fixture(false, None);
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    let handle = register(&mut f, &lease, &id);
    let attempt = run_child(&mut f, &lease, &set.children[0]);
    let result = parent_out(&f, &handle);
    let before = f.state().next_sequence;
    assert!(
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &result,
                &OwnedAuthority(&f.authority)
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    let diagnostic_id = f.cas.put_json(&json!({"failure":"transport"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 4,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    let fake = out(&f, &parent, PARENT, None, json!({"children":[]}));
    let before = f.state().next_sequence;
    assert!(
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &fake,
                &OwnedAuthority(&f.authority)
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    let result = parent_out(&f, &handle);
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &result,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    let facts = f.store.task_owned_child_evidence(&f.cas, &handle).unwrap();
    assert_eq!(folded(&facts)["children"][0]["outcome"], "failed");
    assert_eq!(folded(&facts)["children"][1]["outcome"], "missing");
    assert!(
        f.store
            .publish_task_output(&f.cas, &lease, &result, None, &f.authority)
            .is_err(),
        "ordinary parent publication must not bypass sealing"
    );
}

#[test]
fn owned_recording_after_execution_deadline_preserves_missing_children_without_admitting_work() {
    let deadline = now().unwrap() + 1600;
    let mut f = fixture(false, Some(deadline));
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    std::thread::sleep(std::time::Duration::from_millis(
        deadline.saturating_sub(now().unwrap()) + 5,
    ));
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err()
    );
    f.store
        .check_current_task_plan_for_recording(&f.cas, &lease, &f.authority)
        .unwrap();
    let before = f.state().next_sequence;
    assert!(
        f.store
            .record_task_invocation(&f.cas, &lease, &set.children[0].invocation_id, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    let handle = register(&mut f, &lease, &id);
    let output = parent_out(&f, &handle);
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &output,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 0);
    assert_eq!(execution.budget.committed_tokens(), 0);
    assert_eq!(execution.outputs[PARENT].0, output);
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &lease, &set.children[0].node, &f.authority)
            .is_err()
    );
}

#[test]
fn owned_recording_requires_current_writer_and_unrevoked_plan_decision() {
    let mut f = fixture(true, None);
    let (lease, parent) = started(&mut f);
    let (id, _) = children(&f, &parent);
    let handle = register(&mut f, &lease, &id);
    let output = parent_out(&f, &handle);
    f.authority.current = false;
    let before = f.state().next_sequence;
    assert!(
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &output,
                &OwnedAuthority(&f.authority)
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    f.authority.current = true;
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let current = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-2", 100_000)
        .unwrap();
    assert!(
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &output,
                &OwnedAuthority(&f.authority)
            )
            .is_err()
    );
    f.store
        .complete_task_owned_children(
            &f.cas,
            &current,
            &handle,
            &output,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
}

#[test]
fn owned_selected_output_without_publication_becomes_failed_and_cannot_publish_after_sealing() {
    let mut f = fixture(false, None);
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    let handle = register(&mut f, &lease, &id);
    let child = &set.children[0];
    let attempt = run_child(&mut f, &lease, child);
    let result = out(
        &f,
        &child.invocation_id,
        &child.node,
        Some(attempt.id()),
        json!({"held":"acknowledgement failed"}),
    );
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: result.clone(),
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    let facts = f.store.task_owned_child_evidence(&f.cas, &handle).unwrap();
    assert!(facts[0].selected_output_id.is_some());
    assert!(facts[0].completed_artifact_ids.is_none());
    assert_eq!(folded(&facts)["children"][0]["outcome"], "failed");
    let output = parent_out(&f, &handle);
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &output,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    let before = f.state().next_sequence;
    assert!(
        f.store
            .publish_task_owned_child(&f.cas, &lease, &handle, &result, attempt.id(), &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(f.state().execution.unwrap().outputs[PARENT].0, output);
}

#[test]
fn owned_registration_handoff_preserves_original_attempt_scopes_and_historical_resolution() {
    let mut f = with_owned_graph(source::fixture(false));
    let (lease, parent) = started(&mut f);
    let (id, set) = children(&f, &parent);
    let handle = register(&mut f, &lease, &id);
    let first = run_child(&mut f, &lease, &set.children[0]);
    let diagnostic_id = f
        .cas
        .put_json(&json!({"failure":"original child"}))
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: first.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
    let original: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    let next = source::next(&f);
    let (next_id, next_plan) = source::plan_for(&f, &next);
    let mut plan: ExecutionPlanV1 = payload(&f.cas, &next_plan, task::EXECUTION_PLAN_V1).unwrap();
    let mut graph: CompiledTask =
        payload(&f.cas, &plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    let scope = graph.token_scopes.remove("review.round1.scatter").unwrap();
    graph
        .token_scopes
        .insert("review.round2.scatter".into(), scope);
    plan.compiled_graph_id = f
        .cas
        .put_artifact(
            "af/CompiledTask@1",
            producer(),
            vec![],
            None,
            serde_json::to_value(graph).unwrap(),
        )
        .unwrap()
        .0;
    let next_plan = f
        .cas
        .put_artifact(
            task::EXECUTION_PLAN_V1,
            producer(),
            vec![next_id.clone()],
            None,
            serde_json::to_value(&plan).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .refresh_task_source(
            &f.cas,
            &lease,
            &next_id,
            Some(&next_plan),
            None,
            &f.authority,
        )
        .unwrap();
    f.revision = next;
    f.revision_id = next_id;
    f.plan = plan;
    f.plan_id = next_plan;
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let parent = f.record_execution_inputs(&lease);
    let (id, nextset) = children(&f, &parent);
    register(&mut f, &lease, &id);
    assert!(
        f.store.task_owned_child_evidence(&f.cas, &handle).is_err(),
        "old registry is archival only"
    );
    observe(&mut f, &lease, first.id(), 13);
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    let execution = state.execution.unwrap();
    assert_eq!(
        execution
            .budget
            .scope_committed_tokens("review.round1.scatter"),
        Some(13)
    );
    assert_eq!(
        execution
            .budget
            .scope_committed_tokens("review.round2.scatter"),
        Some(0)
    );
    let old = execution
        .attempt_accounting()
        .into_iter()
        .find(|attempt| attempt.attempt_id == first.id())
        .unwrap();
    let resolved = execution.resolve_attempt_node(&old, &original).unwrap();
    assert_eq!(resolved.owned.unwrap().child_set_id, handle.child_set_id());
    assert_eq!(
        execution
            .resolve_node(&nextset.children[0].node)
            .unwrap()
            .owned
            .unwrap()
            .parent_invocation_id,
        parent
    );
    assert_ne!(
        set.children[0].invocation_id,
        nextset.children[0].invocation_id
    );
}

#[test]
fn owned_canonical_receipt_is_fenced_by_parent_seal_but_recorded_selection_replays() {
    use review_core::task::campaign_review::*;
    for receipt_before_seal in [false, true] {
        let mut f = fixture(false, None);
        let mut graph: CompiledTask =
            payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
        let contract = &mut graph.owned_children.get_mut(PARENT).unwrap().contract;
        contract.outputs.get_mut("output").unwrap().artifact_type =
            review_core::contract::REVIEWER_RESULT_V2.into();
        let mut metadata = contract.outputs["output"].clone();
        metadata.artifact_type = TASK_REVIEW_RESULT_METADATA_V1.into();
        contract.outputs.insert("metadata".into(), metadata);
        f.plan.compiled_graph_id = f
            .cas
            .put_artifact(
                "af/CompiledTask@1",
                producer(),
                vec![],
                None,
                serde_json::to_value(graph).unwrap(),
            )
            .unwrap()
            .0;
        f.plan_id = f
            .cas
            .put_artifact(
                task::EXECUTION_PLAN_V1,
                producer(),
                vec![f.revision_id.clone()],
                None,
                serde_json::to_value(&f.plan).unwrap(),
            )
            .unwrap()
            .0;
        let (lease, parent) = started(&mut f);
        let (id, set) = children(&f, &parent);
        let handle = register(&mut f, &lease, &id);
        let child = &set.children[0];
        f.store
            .record_task_invocation(&f.cas, &lease, &child.invocation_id, &f.authority)
            .unwrap();
        let mut context = review::canonical_context(&mut f, &child.invocation_id, &"a".repeat(26));
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, &child.node, &f.authority)
            .unwrap();
        context.attempt_id = reserved.id().into();
        let context_id = f
            .cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer(),
                context
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&context).unwrap(),
            )
            .unwrap()
            .0;
        let attempt = f
            .store
            .bind_task_attempt_context(&f.cas, &lease, &reserved, &context_id, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output =
            review::review_output(&f, &child.invocation_id, attempt.id(), false, &context_id);
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 7,
                    result: TaskAttemptResultV1::Succeeded {
                        output_id: output.clone(),
                    },
                    usage_id: None,
                    raw_artifact_ids: vec![],
                },
                &f.authority,
            )
            .unwrap();
        f.store
            .publish_task_owned_child(&f.cas, &lease, &handle, &output, attempt.id(), &f.authority)
            .unwrap();
        f.store
            .publish_task_owned_review_result(&f.cas, &lease, &handle, &output, &f.authority)
            .unwrap();
        let selected = f.store.replay("review-task").unwrap().pop().unwrap();
        let selected: TaskReviewResultSelectedV1 =
            serde_json::from_value(selected.payload).unwrap();
        let run = task_run_id(&selected.task_id).unwrap();
        let sql = rusqlite::Connection::open(&f.path).unwrap();
        let check = || {
            check_canonical_child_receipt(
                &sql,
                &f.cas,
                "review-task",
                &context.round_event_id,
                "reviewer",
                attempt.id(),
            )
        };
        check().unwrap();
        let (registered_sequence, registered_refs): (i64, String) = sql.query_row(
            "SELECT sequence,artifact_refs FROM events WHERE run_id=?1 AND instr(artifact_refs,?2)>0 ORDER BY sequence LIMIT 1",
            rusqlite::params![run, id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        let records = receipt_records_referencing(&sql, &run, &output).unwrap();
        let publication = records
            .iter()
            .find(|record| {
                matches!(
                    execution::read_execution_record(&f.cas, record)
                        .unwrap()
                        .record,
                    TaskExecutionRecordV1::OwnedChildPublished { .. }
                )
            })
            .unwrap();
        let (published_sequence, published_refs): (i64, String) = sql.query_row(
            "SELECT sequence,artifact_refs FROM events WHERE run_id=?1 AND json_extract(payload,'$.change.record_id')=?2",
            rusqlite::params![run, publication], |r| Ok((r.get(0)?,r.get(1)?)),
        ).unwrap();
        for (sequence, refs) in [
            (registered_sequence, &registered_refs),
            (published_sequence, &published_refs),
        ] {
            sql.execute(
                "UPDATE events SET artifact_refs='[]' WHERE run_id=?1 AND sequence=?2",
                rusqlite::params![run, sequence],
            )
            .unwrap();
            assert!(
                check().is_err(),
                "missing exact registration/publication must refuse"
            );
            sql.execute(
                "UPDATE events SET artifact_refs=?3 WHERE run_id=?1 AND sequence=?2",
                rusqlite::params![run, sequence, refs],
            )
            .unwrap();
        }
        // A string containing the digest is not that exact reference.
        let aliased = serde_json::to_string(&vec![
            format!("prefix-{output}-suffix"),
            publication.clone(),
        ])
        .unwrap();
        sql.execute(
            "UPDATE events SET artifact_refs=?3 WHERE run_id=?1 AND sequence=?2",
            rusqlite::params![run, published_sequence, aliased],
        )
        .unwrap();
        assert!(check().is_err());
        sql.execute(
            "UPDATE events SET artifact_refs=?3 WHERE run_id=?1 AND sequence=?2",
            rusqlite::params![run, published_sequence, published_refs],
        )
        .unwrap();
        let temporary = i64::try_from(f.store.len(&run).unwrap() + 1).unwrap();
        let swap = || {
            sql.execute(
                "UPDATE events SET sequence=?3 WHERE run_id=?1 AND sequence=?2",
                rusqlite::params![run, registered_sequence, temporary],
            )
            .unwrap();
            sql.execute(
                "UPDATE events SET sequence=?3 WHERE run_id=?1 AND sequence=?2",
                rusqlite::params![run, published_sequence, registered_sequence],
            )
            .unwrap();
            sql.execute(
                "UPDATE events SET sequence=?3 WHERE run_id=?1 AND sequence=?2",
                rusqlite::params![run, temporary, published_sequence],
            )
            .unwrap();
        };
        swap();
        assert!(check().is_err(), "publication cannot precede registration");
        swap();
        check().unwrap();
        // Narrowing the history query must still freshly verify the selected record and
        // the complete registered set, including its other child's captured invocation.
        for id in [publication, &set.children[1].invocation_id] {
            let hex = id.strip_prefix("sha256:").unwrap();
            let file = f
                ._dir
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]);
            let original = std::fs::read(&file).unwrap();
            std::fs::write(&file, b"corrupt owned receipt evidence").unwrap();
            assert!(
                check().is_err(),
                "matching ownership CAS remains freshly verified"
            );
            std::fs::write(&file, original).unwrap();
        }
        check().unwrap();
        for index in 1..=128 {
            f.store
                .renew_task_lease(&f.cas, &lease, 1_000_000 + index * 1000)
                .unwrap();
        }
        let rows = f.store.len(&run).unwrap();
        let matched = receipt_records_referencing(&sql, &run, &output)
            .unwrap()
            .len()
            + receipt_records_referencing(&sql, &run, &id).unwrap().len();
        assert!(matched < 16, "queries must not decode unrelated renewals");
        let start = std::time::Instant::now();
        for _ in 0..32 {
            check_canonical_child_receipt(
                &sql,
                &f.cas,
                "review-task",
                &context.round_event_id,
                "reviewer",
                attempt.id(),
            )
            .unwrap();
        }
        eprintln!(
            "owned receipt benchmark: Task_rows={rows}, matching_rows={matched}, iterations=32, guarded_check_us={}; actual selected child with 128 protected renewals, not multi-Round latency evidence",
            start.elapsed().as_micros()
        );
        let receipt = NewEvent::new(
            EventType::NodeOutputReceiptV1,
            serde_json::to_value(review_core::NodeOutputReceiptPayloadV1 {
                node: "reviewer".into(),
                outputs: vec![review_core::PortArtifactsV1 {
                    port: "out".into(),
                    artifact_type: review_core::contract::REVIEWER_RESULT_V2.into(),
                    cardinality: review_core::PortCardinality::One,
                    optional: false,
                    snapshot_affinity: review_core::SnapshotAffinity::Any,
                    artifact_ids: vec![selected.result_artifact_id.clone()],
                    subject_snapshot_id: None,
                }],
            })
            .unwrap(),
        )
        .node("reviewer")
        .attempt(attempt.id())
        .caused_by(&context.round_event_id)
        .referencing(vec![selected.result_artifact_id]);
        if receipt_before_seal {
            f.store
                .append("review-task", &f.cas, receipt.clone())
                .unwrap();
        }
        let completed = parent_out(&f, &handle);
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &completed,
                &OwnedAuthority(&f.authority),
            )
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let before = f.store.len("review-task").unwrap();
        f.store
            .publish_task_owned_review_result(&f.cas, &lease, &handle, &output, &f.authority)
            .unwrap();
        if !receipt_before_seal {
            let error = f.store.append("review-task", &f.cas, receipt).unwrap_err();
            assert!(error.to_string().contains("sealed"), "{error}");
        }
        assert_eq!(f.store.len("review-task").unwrap(), before);
        assert_eq!(f.state().execution.unwrap().outputs[PARENT].0, completed);
    }
}
