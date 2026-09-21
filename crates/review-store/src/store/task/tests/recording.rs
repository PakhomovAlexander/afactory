use super::*;
use task::execution::*;
use task::report::*;

fn expired_publication(f: Fixture, revoke: bool) -> (Fixture, TaskLease, String) {
    expired_publication_at_report(f, revoke, false)
}

#[test]
fn recording_resume_cannot_acquire_an_output_published_after_the_failed_report() {
    let (mut f, lease, output) =
        expired_publication_at_report(Fixture::new(true).with_execution_graph(), false, true);
    assert_eq!(
        f.state().execution.as_ref().unwrap().outputs["root.nodes.write"].0,
        output
    );
    let events = f
        .store
        .replay(&task_run_id(lease.task_id()).unwrap())
        .unwrap();
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("already-published selected output at the failed report")
    );
    assert_eq!(
        f.store
            .replay(&task_run_id(lease.task_id()).unwrap())
            .unwrap(),
        events
    );
    assert!(!f.state().has_recording_recovery());
}

fn expired_publication_at_report(
    mut f: Fixture,
    revoke: bool,
    publish_after_report: bool,
) -> (Fixture, TaskLease, String) {
    f.revision.limits.deadline_unix_ms = now().unwrap() + 3000;
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
    let lease = f.open();
    f.propose(&lease);
    if !f.plan.generated_origins.is_empty() {
        f.decide(&lease, PlanDecisionKindV1::Approved);
    }
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation = f.record_execution_inputs(&lease);
    let context = f
        .cas
        .put_json(&json!({"context":"exact fixture inputs"}))
        .unwrap();
    let attempt = f
        .store
        .reserve_and_bind_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let output = f.execution_output(&invocation, attempt.id());
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
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    if !publish_after_report {
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
    }
    let state = f.state();
    let execution = state.execution.as_ref().unwrap();
    let diagnostic = f
        .cas
        .put_artifact(
            TASK_DIAGNOSTIC_V1,
            producer(),
            vec![],
            None,
            json!({"message":"domain receipt acknowledgement lost", "truncated":false}),
        )
        .unwrap()
        .0;
    let report = TaskRunReportV1 {
        task_revision_id: f.revision_id.clone(),
        plan_id: f.plan_id.clone(),
        through_sequence: state.next_sequence,
        nodes: execution
            .graph
            .order
            .iter()
            .map(|node| TaskNodeReportV1 {
                node: node.clone(),
                outcome: if node == "root.nodes.write" {
                    TaskNodeOutcomeV1::Failed {
                        diagnostic_id: diagnostic.clone(),
                        class: TaskFailureClassV1::DomainPublication,
                    }
                } else {
                    TaskNodeOutcomeV1::Completed {
                        output_id: execution.outputs[node].0.clone(),
                    }
                },
            })
            .collect(),
    };
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store.record_task_run_report(&f.cas, &lease, &id).unwrap();
    if publish_after_report {
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
    }
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsHuman)
        .unwrap();
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .is_err(),
        "unexpired pauses keep ordinary resume authority"
    );
    if revoke {
        f.store
            .revoke_task_approval(
                &f.cas,
                &lease,
                &f.plan_id,
                "explicit revocation",
                &f.authority,
            )
            .unwrap();
    }
    let delay = f
        .revision
        .limits
        .deadline_unix_ms
        .saturating_sub(now().unwrap())
        + 5;
    std::thread::sleep(std::time::Duration::from_millis(delay));
    f.store = EventStore::open(&f.path).unwrap();
    (f, lease, output)
}

fn transition(f: &Fixture, lease: &TaskLease) -> TaskTransitionV1 {
    TaskTransitionV1 {
        writer: lease.writer.clone(),
        epoch: lease.epoch,
        now_unix_ms: now().unwrap(),
        change: TaskChangeV1::RecordingResumed {
            task_revision_id: f.revision_id.clone(),
            plan_id: f.plan_id.clone(),
            report_id: f.state().run_reports.last().unwrap().clone(),
        },
    }
}

#[test]
fn recording_resume_pins_exact_report_and_outputs_without_dispatch_or_new_resources() {
    let (mut f, lease, output) =
        expired_publication(Fixture::new(true).with_execution_graph(), false);
    let before = f.state();
    assert!(
        f.store
            .resume_task(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("deadline expired")
    );
    for lane in ["revision", "plan", "report"] {
        let mut bad = transition(&f, &lease);
        let TaskChangeV1::RecordingResumed {
            task_revision_id,
            plan_id,
            report_id,
        } = &mut bad.change
        else {
            unreachable!()
        };
        match lane {
            "revision" => *task_revision_id = f.plan_id.clone(),
            "plan" => *plan_id = f.revision_id.clone(),
            _ => *report_id = f.plan_id.clone(),
        }
        assert!(
            f.store
                .append_task_transition(&f.cas, lease.task_id(), bad)
                .is_err(),
            "{lane}"
        );
        assert_eq!(f.state().next_sequence, before.next_sequence);
    }
    let mut missing = before.clone();
    missing
        .execution
        .as_mut()
        .unwrap()
        .outputs
        .remove("root.nodes.write");
    assert!(
        missing
            .validate_recording_resume(
                &f.cas,
                &f.revision_id,
                &f.plan_id,
                before.run_reports.last().unwrap(),
                now().unwrap()
            )
            .is_err(),
        "a publication diagnostic alone is not selected output authority"
    );
    f.authority.current = false;
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    f.authority.current = true;
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let current = f
        .store
        .take_task_lease(&f.cas, lease.task_id(), "new-writer", 60_000)
        .unwrap();
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("fenced")
    );
    let forged = transition(&f, &current);
    let (kind, raw) = crate::store::task::review_handoff::encode_transition(&forged).unwrap();
    let state = f.state();
    let event = NewEvent::new(kind, raw)
        .referencing(references(&f.cas, &forged.change, Some(&state)).unwrap());
    assert!(
        f.store
            .append(&task_run_id(current.task_id()).unwrap(), &f.cas, event)
            .unwrap_err()
            .to_string()
            .contains("trusted Task entry point")
    );
    assert_eq!(f.state().next_sequence, state.next_sequence);
    let event = f
        .store
        .resume_task_for_recording(&f.cas, &current, &f.authority)
        .unwrap();
    assert_eq!(event.event_type, EventType::TaskTransitionV4);
    assert!(event.artifact_refs.contains(&output));
    f.store = EventStore::open(&f.path).unwrap();
    let after = f.state();
    assert!(after.has_recording_recovery());
    assert_eq!(after.revision, before.revision);
    let a = after.execution.unwrap();
    let b = before.execution.unwrap();
    assert_eq!(a.outputs, b.outputs);
    assert_eq!(a.invocations, b.invocations);
    assert_eq!(a.budget.committed_tokens(), b.budget.committed_tokens());
    assert_eq!(a.budget.begun_attempts(), b.budget.begun_attempts());
    assert!(
        f.store
            .checked_recorded_output(&f.cas, &current, &output, &f.authority)
            .is_ok()
    );
    assert!(
        f.store
            .checked_recorded_output(&f.cas, &current, &f.plan_id, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &current, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .reserve_task_attempt(&f.cas, &current, "root.nodes.write", &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .record_task_invocation(
                &f.cas,
                &current,
                &a.invocations["root.inputs"].0,
                &f.authority
            )
            .is_err()
    );
    let attempt = a.reusable_output("root.nodes.write").unwrap().1;
    assert!(
        f.store
            .publish_task_output(&f.cas, &current, &output, Some(&attempt), &f.authority)
            .is_err()
    );
    // The new recording capability itself never grants goal acceptance, even in this
    // common Store fixture whose entire output graph was published before the pause.
    let result = TaskResultV1 {
        task_revision_id: f.revision_id.clone(),
        execution: task::TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: "done".into(),
        outputs: a
            .graph
            .outputs
            .iter()
            .map(|(name, address)| {
                (
                    name.clone(),
                    a.outputs[&address.node].1.outputs[&address.port].clone(),
                )
            })
            .collect(),
        evidence: a
            .graph
            .coverage
            .values()
            .flat_map(|address| {
                a.outputs[&address.node].1.outputs[&address.port]
                    .artifact_ids
                    .iter()
                    .cloned()
            })
            .collect(),
        missing_obligations: BTreeSet::new(),
    };
    let id = f
        .cas
        .put_artifact(
            task::TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    let error = f
        .store
        .task_change(
            &f.cas,
            &current,
            TaskChangeV1::Finished { result_id: id },
            now().unwrap(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("recording-only"), "{error}");
    f.authority.current = false;
    assert!(
        f.store
            .checked_recorded_output(&f.cas, &current, &output, &f.authority)
            .is_err()
    );
}

#[test]
fn recording_resume_rejects_revoked_decision_or_replaced_latest_report() {
    let mut expired_decision = Fixture::new(true).with_execution_graph();
    expired_decision.authority.valid_until = now().unwrap() + 1500;
    let (mut expired_decision, paused, _) = expired_publication(expired_decision, false);
    let before = expired_decision.state().next_sequence;
    assert!(
        expired_decision
            .store
            .resume_task_for_recording(&expired_decision.cas, &paused, &expired_decision.authority)
            .is_err()
    );
    assert_eq!(expired_decision.state().next_sequence, before);
    let (mut f, lease, _) = expired_publication(Fixture::new(true).with_execution_graph(), true);
    let before = f.state().next_sequence;
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
    let (mut f, lease, _) = expired_publication(Fixture::new(true).with_execution_graph(), false);
    let state = f.state();
    let old_report = state.run_reports.last().unwrap();
    let (mut report, _) = read_task_run_report(&f.cas, old_report).unwrap();
    report.through_sequence = state.next_sequence;
    for node in &mut report.nodes {
        if let TaskNodeOutcomeV1::Failed { class, .. } = &mut node.outcome {
            *class = TaskFailureClassV1::Resources;
        }
    }
    let id = f
        .cas
        .put_artifact(
            TASK_RUN_REPORT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(report).unwrap(),
        )
        .unwrap()
        .0;
    f.store.record_task_run_report(&f.cas, &lease, &id).unwrap();
    let before = f.state().next_sequence;
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .is_err()
    );
    let mut stale = transition(&f, &lease);
    let TaskChangeV1::RecordingResumed { report_id, .. } = &mut stale.change else {
        unreachable!()
    };
    *report_id = old_report.clone();
    assert!(
        f.store
            .append_task_transition(&f.cas, lease.task_id(), stale)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, before);
}

#[test]
fn recording_resume_compares_review_round_inside_the_write_transaction() {
    let (f, round) = review::round::round_fixture();
    let (mut f, lease, _) = expired_publication(f, false);
    let state = f.state();
    let transition = transition(&f, &lease);
    let (event_type, value) =
        crate::store::task::review_handoff::encode_transition(&transition).unwrap();
    let event = NewEvent::new(event_type, value.clone())
        .referencing(references(&f.cas, &transition.change, Some(&state)).unwrap());
    let run = task_run_id(lease.task_id()).unwrap();
    let permit = WritePermit {
        run_id: run.clone(),
        first: state.next_sequence,
        payloads: vec![value],
        event_type,
        valid_until: Some(state.lease_until),
        review_round: review_round::fence_for_transition(&f.cas, &transition, Some(&state))
            .unwrap(),
        review_prefix: None,
    };
    permit
        .validate(
            &f.store.conn,
            &run,
            state.next_sequence as i64,
            &[event.clone()],
        )
        .unwrap();
    review::round::supersede(&f, &round);
    assert_eq!(f.state().next_sequence, state.next_sequence);
    assert!(
        f.store
            .append_batch_inner(&run, &f.cas, &[event], Some(&permit), None)
            .unwrap_err()
            .to_string()
            .contains("superseded or closed")
    );
    assert!(
        f.store
            .resume_task_for_recording(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, state.next_sequence);
}
