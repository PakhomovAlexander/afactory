use super::*;
use review_core::task::{execution::*, source::*};

fn put<T: serde::Serialize>(cas: &Cas, kind: &str, value: &T) -> String {
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
fn issue_input(cas: &Cas, version: &str) -> (String, NormalizedRequirementsV1) {
    let issue = IssueInputV1 {
        schema: "af.issue-input/1".into(),
        id: "10042".into(),
        key: "AF-42".into(),
        revision: version.into(),
        summary: "Update the guide".into(),
        description: format!("Describe {version} migration"),
        acceptance: BTreeMap::new(),
    };
    let raw = cas
        .put_json(&serde_json::to_value(&issue).unwrap())
        .unwrap();
    let mut refs = BTreeSet::from([raw.clone()]);
    let fields = issue
        .fields()
        .into_iter()
        .map(|(k, v)| {
            let value = cas.put_json(&json!(v)).unwrap();
            let text = cas.put(v.as_bytes()).unwrap();
            refs.insert(value.clone());
            refs.insert(text.clone());
            (
                k,
                TaskSourceFieldV1 {
                    value_id: value,
                    text_id: text,
                },
            )
        })
        .collect();
    let capture = TaskSourceCaptureV1 {
        schema: "af.task-source-capture/1".into(),
        adapter: TaskSourceAdapterV1::LocalIssue,
        locator: "issue.json".into(),
        external_id: issue.id.clone(),
        external_key: issue.key.clone(),
        source_revision: issue.revision.clone(),
        raw_source_id: raw,
        fields,
    };
    let capture = cas
        .put_artifact(
            TASK_SOURCE_CAPTURE_V1,
            producer(),
            refs.into_iter().collect(),
            None,
            serde_json::to_value(capture).unwrap(),
        )
        .unwrap()
        .0;
    let file = cas
        .put_json(&json!({"fixed_task_definition":true}))
        .unwrap();
    let normalized = issue.requirements(Some(
        json!({"schema":"guide/1"}).as_object().unwrap().clone(),
    ));
    let input = cas
        .put_artifact(
            "af/Requirements@1",
            producer(),
            vec![file, capture],
            None,
            serde_json::to_value(&normalized).unwrap(),
        )
        .unwrap()
        .0;
    (input, normalized)
}
pub(super) fn fixture(generated: bool) -> Fixture {
    let mut f = Fixture::new(generated);
    let (id, requirements) = issue_input(&f.cas, "v1");
    f.revision.goal = format!("Implement the ticket\n\n{}", requirements.text);
    f.revision
        .inputs
        .get_mut("requirements")
        .unwrap()
        .artifact_ids = vec![id.clone()];
    f.revision.provenance.input_artifact_ids = vec![id];
    f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.inputs = f.revision.inputs.clone();
    f.with_execution_graph()
}
pub(super) fn next(f: &Fixture) -> TaskRevisionV1 {
    let (id, requirements) = issue_input(&f.cas, "v2");
    let mut next = f.revision.clone();
    next.revision += 1;
    next.previous_revision_id = Some(f.revision_id.clone());
    next.goal = format!("Implement the ticket\n\n{}", requirements.text);
    next.inputs.get_mut("requirements").unwrap().artifact_ids = vec![id.clone()];
    next.provenance.input_artifact_ids = vec![id];
    next
}
pub(super) fn plan_for(f: &Fixture, revision: &TaskRevisionV1) -> (String, String) {
    let revision_id = put(&f.cas, task::TASK_REVISION_V1, revision);
    let mut graph: review_graph::task::CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs = revision.inputs.clone();
    let mut plan = f.plan.clone();
    plan.task_revision_id = revision_id.clone();
    plan.inputs = revision.inputs.clone();
    plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@1", &graph);
    (revision_id, put(&f.cas, task::EXECUTION_PLAN_V1, &plan))
}
#[test]
fn source_refresh_is_atomic_invalidates_approval_and_retains_paid_and_late_usage() {
    let mut f = fixture(true);
    let lease = f.open();
    f.propose(&lease);
    f.decide(&lease, PlanDecisionKindV1::Approved);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"context":"source-test"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let changed = next(&f);
    let (changed_id, changed_plan) = plan_for(&f, &changed);
    let sequence = f.state().next_sequence;
    assert!(
        f.store
            .refresh_task_source(
                &f.cas,
                &lease,
                &changed_id,
                Some(&changed_plan),
                None,
                &f.authority
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, sequence);
    let diagnostic = f.cas.put_json(&json!({"failure":"fixture"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Failed {
                    feedback_id: None,
                    diagnostic_id: diagnostic,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    let sequence = f.state().next_sequence;
    f.store
        .refresh_task_source(
            &f.cas,
            &lease,
            &changed_id,
            Some(&changed_plan),
            None,
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.next_sequence, sequence + 1);
    assert_eq!(state.revision, changed);
    assert_eq!(state.plan_id.as_ref(), Some(&changed_plan));
    assert_eq!(
        state.phase,
        TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsPlanReview
        }
    );
    assert!(!state.admitted);
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(
        execution.budget.remaining_limits().deadline_unix_ms,
        f.revision.limits.deadline_unix_ms
    );
    assert!(execution.invocations.is_empty() && execution.outputs.is_empty());
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .decide_task_plan(
                &f.cas,
                &lease,
                &f.plan_id,
                PlanDecisionKindV1::Approved,
                "old plan",
                &f.authority
            )
            .is_err()
    );
    f.store
        .decide_task_plan(
            &f.cas,
            &lease,
            &changed_plan,
            PlanDecisionKindV1::Approved,
            "new exact plan",
            &f.authority,
        )
        .unwrap();
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let usage = f.cas.put_json(&json!({"late_charge":9})).unwrap();
    f.store
        .observe_task_usage(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt.id().into(),
                charged_tokens: 9,
                usage_id: usage,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.execution.unwrap().budget.committed_tokens(), 9);
    assert_eq!(
        state.revision.previous_revision_id.as_ref(),
        Some(&f.revision_id)
    );
}
#[test]
fn source_refresh_cannot_change_authority_or_contracts_and_can_record_resource_waiting() {
    let mut f = fixture(false);
    let lease = f.open();
    f.propose(&lease);
    let changed = next(&f);
    let sequence = f.state().next_sequence;
    for case in [
        "tokens",
        "deadline",
        "acceptance",
        "goal",
        "facts",
        "predecessor",
        "origin",
        "specification",
        "producer",
    ] {
        let mut forged = changed.clone();
        match case {
            "tokens" => forged.limits.tokens += 1,
            "deadline" => forged.limits.deadline_unix_ms += 1,
            "acceptance" => forged.acceptance.clear(),
            "goal" => forged.goal = "New unrelated instructions".into(),
            "facts" => {
                forged
                    .facts
                    .insert("privilege".into(), task::TaskFactV1::Boolean(true));
            }
            "predecessor" => forged.previous_revision_id = Some(f.plan_id.clone()),
            "origin" | "specification" | "producer" => {
                let id = &forged.inputs["requirements"].artifact_ids[0];
                let mut input = envelope(&f.cas, id, "af/Requirements@1").unwrap();
                if case == "origin" {
                    input.input_artifacts.push(f.plan_id.clone());
                }
                if case == "specification" {
                    input.payload["specification"] = json!({"schema":"unreviewed/1"});
                }
                if case == "producer" {
                    input.producer = Producer::Attempt {
                        run_id: "worker".into(),
                        node_id: "writer".into(),
                        attempt_id: "a".repeat(26),
                    };
                }
                let id = f
                    .cas
                    .put_artifact(
                        "af/Requirements@1",
                        input.producer,
                        input.input_artifacts,
                        None,
                        input.payload,
                    )
                    .unwrap()
                    .0;
                forged.inputs.get_mut("requirements").unwrap().artifact_ids = vec![id.clone()];
                forged.provenance.input_artifact_ids = vec![id];
            }
            _ => unreachable!(),
        }
        let id = put(&f.cas, task::TASK_REVISION_V1, &forged);
        assert!(
            f.store
                .refresh_task_source(
                    &f.cas,
                    &lease,
                    &id,
                    None,
                    Some(TaskWaitingReasonV1::NeedsResources),
                    &f.authority
                )
                .is_err(),
            "{case}"
        );
        assert_eq!(f.state().next_sequence, sequence);
    }
    let id = put(&f.cas, task::TASK_REVISION_V1, &changed);
    f.store
        .refresh_task_source(
            &f.cas,
            &lease,
            &id,
            None,
            Some(TaskWaitingReasonV1::NeedsResources),
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.revision, changed);
    assert!(state.plan_id.is_none());
    assert_eq!(
        state.phase,
        TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsResources
        }
    );
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
}

#[test]
fn source_refresh_records_changed_input_when_a_valid_plan_can_no_longer_fit() {
    let mut f = fixture(false);
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"context":"resource-race"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let diagnostic = f
        .cas
        .put_json(&json!({"failure":"reported overrun"}))
        .unwrap();
    let overrun = f.revision.limits.tokens + 1;
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: u128::from(overrun),
                result: TaskAttemptResultV1::Failed {
                    feedback_id: None,
                    diagnostic_id: diagnostic,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    let changed = next(&f);
    let (revision_id, plan_id) = plan_for(&f, &changed);
    let sequence = f.state().next_sequence;
    let event = f
        .store
        .refresh_task_source(
            &f.cas,
            &lease,
            &revision_id,
            Some(&plan_id),
            None,
            &f.authority,
        )
        .unwrap();
    assert_eq!(event.payload["change"]["waiting"], "needs_resources");
    assert!(event.payload["change"].get("plan_id").is_none());
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.next_sequence, sequence + 1);
    assert_eq!(state.revision, changed);
    assert!(state.plan_id.is_none() && !state.admitted);
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), u128::from(overrun));
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
}
