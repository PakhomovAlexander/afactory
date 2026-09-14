use super::*;
use crate::store::task::review_handoff::*;
use review_core::task::event::TaskTransitionV2;
use review_core::task::execution::*;
use review_core::task::review_compat::*;
use review_core::task::review_handoff::*;
use review_graph::task::CompiledTask;

pub(super) struct HandoffAuthority<'a>(pub(super) &'a Authority);
impl TaskAuthority for HandoffAuthority<'_> {
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
    fn validate_review_continuation(
        &self,
        cas: &Cas,
        old: &TaskRevisionV1,
        new: &TaskRevisionV1,
        old_plan: &ExecutionPlanV1,
        new_plan: &ExecutionPlanV1,
        handoff: &TaskReviewHandoffV1,
    ) -> Result<(), String> {
        let mut expected = old.clone();
        expected.revision += 1;
        expected.previous_revision_id = Some(handoff.predecessor_revision_id.clone());
        expected
            .inputs
            .insert("round".into(), new.inputs["round"].clone());
        let old_graph: CompiledTask =
            payload(cas, &old_plan.compiled_graph_id, "af/CompiledTask@1")
                .map_err(|e| e.to_string())?;
        let new_graph: CompiledTask =
            payload(cas, &new_plan.compiled_graph_id, "af/CompiledTask@1")
                .map_err(|e| e.to_string())?;
        let mut graph = old_graph;
        graph.inputs = new.inputs.clone();
        if expected != *new
            || graph != new_graph
            || old_plan.authority != new_plan.authority
            || old_plan.limits != new_plan.limits
        {
            return Err("Captured fixture recompile disagrees with successor".into());
        }
        Ok(())
    }
}
fn fixture(generated: bool) -> (Fixture, LegacyReviewRoundV1) {
    let (mut f, round) = review::round::round_fixture();
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.token_scopes.insert(
        "review.round1".into(),
        review_attempt::task_budget::TaskTokenScope {
            tokens: 30,
            members: BTreeSet::from(["root.nodes.write".into()]),
        },
    );
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
    if generated {
        let origin = GeneratedOriginV1 {
            pipeline_id: f.plan.pipeline_id.clone(),
            proposal_id: f.revision.authority.policy_id.clone(),
            bootstrap_plan_id: f.revision.authority.policy_id.clone(),
        };
        f.authority.generated = vec![origin.clone()];
        f.plan.generated_origins = vec![origin];
    }
    f.plan_id = put(&f, task::EXECUTION_PLAN_V1, &f.plan);
    (f, round)
}
fn put(f: &Fixture, kind: &str, value: &impl serde::Serialize) -> String {
    f.cas
        .put_artifact(
            kind,
            producer(),
            vec![],
            None,
            serde_json::to_value(value).unwrap(),
        )
        .unwrap()
        .0
}
fn start(f: &mut Fixture) -> (TaskLease, execution::PreparedTaskAttempt) {
    let lease = f.open();
    f.propose(&lease);
    if !f.plan.generated_origins.is_empty() {
        f.decide(&lease, PlanDecisionKindV1::Approved);
    }
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"context":"old epoch"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    (lease, attempt)
}
fn settle(
    f: &mut Fixture,
    lease: &TaskLease,
    attempt: &execution::PreparedTaskAttempt,
    charge: u128,
) {
    let diagnostic = f.cas.put_json(&json!({"failed":"worker"})).unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: diagnostic,
                    feedback_id: None,
                },
                usage_id: None,
                raw_artifact_ids: vec![],
            },
            &f.authority,
        )
        .unwrap();
}
fn successor(f: &Fixture, old: &LegacyReviewRoundV1) -> (String, TaskReviewHandoffV1) {
    let event = f
        .store
        .latest_round_started(&old.campaign_id)
        .unwrap()
        .unwrap();
    let recorded: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(event.payload).unwrap();
    let mut round = old.clone();
    round.round_event_id = event.event_id;
    round.round = recorded.round;
    round.epoch = recorded.epoch;
    let id = f
        .cas
        .put_artifact(
            LEGACY_REVIEW_ROUND_V1,
            producer(),
            round
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(round.head_snapshot_id.clone()),
            serde_json::to_value(&round).unwrap(),
        )
        .unwrap()
        .0;
    let mut next = f.revision.clone();
    next.revision += 1;
    next.previous_revision_id = Some(f.revision_id.clone());
    next.inputs.get_mut("round").unwrap().artifact_ids = vec![id.clone()];
    let revision_id = put(f, task::TASK_REVISION_V1, &next);
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs = next.inputs.clone();
    let mut plan = f.plan.clone();
    plan.task_revision_id = revision_id.clone();
    plan.inputs = next.inputs;
    plan.compiled_graph_id = put(f, "af/CompiledTask@1", &graph);
    let handoff = TaskReviewHandoffV1 {
        task_id: f.revision.task_id.clone(),
        predecessor_revision_id: f.revision_id.clone(),
        predecessor_plan_id: f.plan_id.clone(),
        successor_revision_id: revision_id,
        successor_plan_id: put(f, task::EXECUTION_PLAN_V1, &plan),
        predecessor_round_id: f.revision.inputs["round"].artifact_ids[0].clone(),
        successor_round_id: id,
        evidence: TaskReviewHandoffEvidenceV1::SupersededInput {
            superseded_event_id: f
                .store
                .replay(&round.campaign_id)
                .unwrap()
                .into_iter()
                .find(|e| e.event_type == EventType::RoundInputSupersededV1)
                .unwrap()
                .event_id,
        },
    };
    (
        capture_task_review_handoff(&f.cas, &handoff).unwrap(),
        handoff,
    )
}

#[test]
fn review_handoff_retains_original_budget_late_charge_and_exact_reopen_without_autoapproval() {
    for generated in [false, true] {
        let (mut f, round) = fixture(generated);
        let (lease, attempt) = start(&mut f);
        settle(&mut f, &lease, &attempt, 7);
        review::round::supersede(&f, &round);
        let (id, handoff) = successor(&f, &round);
        let before = f.state();
        let original = before.execution.as_ref().unwrap().attempt_accounting();
        let event = f
            .store
            .continue_task_review(&f.cas, &lease, &id, &HandoffAuthority(&f.authority))
            .unwrap();
        assert_eq!(event.event_type, EventType::TaskTransitionV2);
        assert!(serde_json::from_value::<TaskTransitionV1>(event.payload.clone()).is_err());
        assert_eq!(
            read_task_transition(&event).unwrap().change,
            TaskChangeV1::ReviewContinued {
                handoff_id: id.clone()
            }
        );
        f.store = EventStore::open(&f.path).unwrap();
        REVIEW_REPLAY_LOADS.with(|loads| loads.set(0));
        let next = f.state();
        assert_eq!(
            REVIEW_REPLAY_LOADS.with(|loads| loads.get()),
            1,
            "cold replay shares one canonical prefix"
        );
        let refs = next.artifact_refs.len();
        let bytes: u64 = next
            .artifact_refs
            .iter()
            .map(|id| f.cas.stored_len(id).unwrap())
            .sum();
        let mut durations = Vec::new();
        for _ in 0..16 {
            REVIEW_REPLAY_LOADS.with(|loads| loads.set(0));
            let start = std::time::Instant::now();
            assert_eq!(f.state().next_sequence, next.next_sequence);
            durations.push(start.elapsed().as_micros());
            assert_eq!(
                REVIEW_REPLAY_LOADS.with(|loads| loads.get()),
                1,
                "each warm call must replay its own canonical prefix exactly once"
            );
        }
        durations.sort_unstable();
        eprintln!(
            "task-projection: generated={generated} handoffs=1 samples=16 refs={refs} closure_bytes={bytes} campaign_replays=1 p50_us={} p95_us={}",
            durations[8], durations[15]
        );
        assert!(!next.admitted);
        assert_eq!(next.revision.limits, f.revision.limits);
        assert_eq!(next.plan_id.as_ref(), Some(&handoff.successor_plan_id));
        assert_eq!(
            next.phase,
            if generated {
                TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsPlanReview,
                }
            } else {
                TaskPhaseV1::Ready {}
            }
        );
        let execution = next.execution.as_ref().unwrap();
        assert!(execution.invocations.is_empty());
        assert!(execution.outputs.is_empty());
        assert_eq!(execution.budget.committed_tokens(), 7);
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert_eq!(
            execution.budget.scope_committed_tokens("review.round1"),
            Some(7)
        );
        assert_eq!(
            execution.attempt_accounting()[0].reservation,
            original[0].reservation
        );
        let count = f
            .store
            .len(&task_run_id(&f.revision.task_id).unwrap())
            .unwrap();
        assert_eq!(
            f.store
                .continue_task_review(&f.cas, &lease, &id, &HandoffAuthority(&f.authority))
                .unwrap(),
            event
        );
        assert_eq!(
            f.store
                .len(&task_run_id(&f.revision.task_id).unwrap())
                .unwrap(),
            count
        );
        if generated {
            assert!(
                f.store
                    .admit_task_plan(&f.cas, &lease, &f.authority)
                    .is_err()
            );
            f.store
                .decide_task_plan(
                    &f.cas,
                    &lease,
                    &handoff.successor_plan_id,
                    PlanDecisionKindV1::Approved,
                    "new exact successor",
                    &f.authority,
                )
                .unwrap();
        }
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        assert_eq!(f.state().execution.unwrap().budget.begun_attempts(), 1);
        f.revision_id = handoff.successor_revision_id.clone();
        f.revision = revision(&f.cas, &f.revision_id).unwrap();
        f.plan_id = handoff.successor_plan_id.clone();
        f.plan = payload(&f.cas, &f.plan_id, task::EXECUTION_PLAN_V1).unwrap();
        f.record_execution_inputs(&lease);
        let usage = f.cas.put_json(&json!({"actual":"late"})).unwrap();
        let wide = u128::from(u64::MAX) + 7;
        f.store
            .observe_task_usage(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.id().into(),
                    charged_tokens: wide,
                    usage_id: usage,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
        settle(&mut f, &lease, &attempt, 7);
        f.store = EventStore::open(&f.path).unwrap();
        let state = f.state();
        let execution = state.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), wide);
        assert_eq!(
            execution.budget.scope_committed_tokens("review.round1"),
            Some(wide)
        );
        assert_eq!(
            execution.attempt_accounting()[0].reservation,
            original[0].reservation
        );
        let before = f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap();
        assert!(
            f.store
                .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
                .is_err()
        );
        assert_eq!(
            f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn review_handoff_refuses_pending_untrusted_or_changed_epoch_caps_without_append() {
    let (mut f, round) = fixture(false);
    let (lease, attempt) = start(&mut f);
    review::round::supersede(&f, &round);
    let (id, handoff) = successor(&f, &round);
    let before = f
        .store
        .len(&task_run_id(&f.revision.task_id).unwrap())
        .unwrap();
    let error = f
        .store
        .continue_task_review(&f.cas, &lease, &id, &HandoffAuthority(&f.authority))
        .unwrap_err();
    assert!(
        error.to_string().contains("pending common Attempts"),
        "{error}"
    );
    assert_eq!(
        f.store
            .len(&task_run_id(&f.revision.task_id).unwrap())
            .unwrap(),
        before
    );
    settle(&mut f, &lease, &attempt, 7);
    assert!(
        f.store
            .continue_task_review(&f.cas, &lease, &id, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("not configured")
    );
    let mut bad = handoff.clone();
    let mut plan: ExecutionPlanV1 =
        payload(&f.cas, &bad.successor_plan_id, task::EXECUTION_PLAN_V1).unwrap();
    let mut graph: CompiledTask =
        payload(&f.cas, &plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.token_scopes.get_mut("review.round1").unwrap().tokens = 31;
    plan.compiled_graph_id = put(&f, "af/CompiledTask@1", &graph);
    bad.successor_plan_id = put(&f, task::EXECUTION_PLAN_V1, &plan);
    // Replay enforces the cap independently of the captured compiler callback.
    let bad_id = capture_task_review_handoff(&f.cas, &bad).unwrap();
    let mut projected = f.state();
    assert!(
        projected
            .apply_review_handoff(&f.cas, &bad_id, now().unwrap())
            .unwrap_err()
            .to_string()
            .contains("aggregate scopes")
    );
    let raw = TaskTransitionV2::from_continuation(&TaskTransitionV1 {
        writer: lease.writer.clone(),
        epoch: lease.epoch,
        now_unix_ms: now().unwrap(),
        change: TaskChangeV1::ReviewContinued { handoff_id: id },
    })
    .unwrap();
    let error = f
        .store
        .append(
            &task_run_id(lease.task_id()).unwrap(),
            &f.cas,
            NewEvent::new(
                EventType::TaskTransitionV2,
                serde_json::to_value(raw).unwrap(),
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("trusted Task"), "{error}");
}

#[test]
fn review_handoff_compares_task_and_successor_round_inside_the_append_transaction() {
    for change_task in [false, true] {
        let (mut f, old) = fixture(false);
        let (lease, attempt) = start(&mut f);
        settle(&mut f, &lease, &attempt, 7);
        review::round::supersede(&f, &old);
        let (id, handoff) = successor(&f, &old);
        let state = f.state();
        let transition = TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now().unwrap(),
            change: TaskChangeV1::ReviewContinued {
                handoff_id: id.clone(),
            },
        };
        let mut proof = state.clone();
        proof
            .apply_review_handoff(&f.cas, &id, transition.now_unix_ms)
            .unwrap();
        let (event_type, value) =
            crate::store::task::review_handoff::encode_transition(&transition).unwrap();
        let run = task_run_id(lease.task_id()).unwrap();
        let event = NewEvent::new(event_type, value.clone())
            .referencing(references(&f.cas, &transition.change, Some(&state)).unwrap());
        let next: TaskRevisionV1 = revision(&f.cas, &handoff.successor_revision_id).unwrap();
        let permit = WritePermit {
            run_id: run.clone(),
            first: state.next_sequence,
            payloads: vec![value],
            event_type,
            valid_until: Some(state.lease_until),
            review_round: super::super::review_round::ReviewRoundFence::capture(&f.cas, &next)
                .unwrap(),
            review_prefix: Some((
                old.campaign_id.clone(),
                f.store.len(&old.campaign_id).unwrap(),
            )),
        };
        permit
            .validate(
                &f.store.conn,
                &run,
                state.next_sequence as i64,
                &[event.clone()],
            )
            .unwrap();
        if change_task {
            let mut other = EventStore::open(&f.path).unwrap();
            other.release_task_lease(&f.cas, &lease).unwrap();
            other
                .take_task_lease(&f.cas, lease.task_id(), "other-writer", 1_000_000)
                .unwrap();
        } else {
            let next_round: LegacyReviewRoundV1 =
                payload(&f.cas, &handoff.successor_round_id, LEGACY_REVIEW_ROUND_V1).unwrap();
            review::round::supersede(&f, &next_round);
        }
        let before = f.store.replay(&run).unwrap();
        assert!(
            f.store
                .append_batch_inner(&run, &f.cas, &[event], Some(&permit), None)
                .is_err()
        );
        assert_eq!(f.store.replay(&run).unwrap(), before);
        assert_eq!(f.state().revision_id, handoff.predecessor_revision_id);
    }
}
