//! Real common Runtime heavy Review fixture shared with fresh CLI inspection tests.
//! Stores `cas/`, `events.sqlite`, and exactly one Task named `heavy-review`.
use super::captured_fixture;
use review_core::EventType;
use review_core::task::TaskPhaseV1;
use review_core::task::execution::TaskExecutionRecordV1;
use review_pipeline::task::TaskRuntime;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper, TaskDomain};
use review_pipeline::task::legacy_review::plan::LegacyReviewPlanCompiler;
use review_pipeline::task::legacy_review::{CapturedLegacyReviewRound, host::LegacyReviewTaskHost};
use review_store::{Cas, EventStore, NewEvent, SharedEventStore};
use std::collections::BTreeMap;

pub(super) const PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

/// A reviewer that answers `result` and dispositions the one prior Finding it is assigned in a
/// later Round `not_reproduced`, which changes no Ledger state, reading its ID from its input.
fn command_pipeline_returning(result: &serde_json::Value) -> String {
    let mut answered = result.clone();
    answered["dispositions"] = serde_json::json!([{
        "finding_id": "FINDING", "position": "not_reproduced", "reason": "re-reported as its own claim",
    }]);
    let quoted = |text: &str| text.replace('\'', "'\\''");
    let answered = answered.to_string();
    let (head, tail) = answered.split_once("FINDING").unwrap();
    let command = format!(
        "finding=$(sed -n 's/.*\"finding_id\":\"\\(sha256:[0-9a-f]*\\)\".*/\\1/p'); \
         if test -n \"$finding\"; then printf '%s%s%s' '{}' \"$finding\" '{}'; \
         else printf '%s' '{}'; fi",
        quoted(head),
        quoted(tail),
        quoted(&result.to_string()),
    );
    PIPELINE.replace(
        "runner = { program = \"/bin/true\" }",
        &format!(
            "runner = {{ program = \"/bin/sh\", args = [{{value=\"-c\"}}, {{value={}}}] }}",
            serde_json::to_string(&command).unwrap()
        ),
    )
}

mod capture {
    pub(super) fn limits() -> review_core::task::TaskLimitsV1 {
        review_core::task::TaskLimitsV1 {
            tokens: 100,
            max_attempts: 4,
            deadline_unix_ms: 9999999999999,
            verification: review_core::task::VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            },
        }
    }
}
mod plan {
    use super::*;
    use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
    use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
    use review_core::task::{TaskResultV1, TaskRevisionV1};
    use review_graph::task::OperatorAttemptCost;
    use review_pipeline::task::legacy_review::plan::ReviewPlanSettings;
    use review_pipeline::task::{TaskOperatorHost, TaskWorkOutput};
    use review_store::store::task::execution::PreparedTaskAttempt;
    // Admission must not reach execution or silently grant domain acceptance.
    pub(super) struct RefuseExecution;
    impl TaskOperatorHost for RefuseExecution {
        fn prepare_context(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &[String],
        ) -> Result<String, String> {
            panic!("plan admission rendered Worker context")
        }
        fn execute(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: Option<&PreparedTaskAttempt>,
        ) -> TaskWorkOutput {
            panic!("plan admission executed work")
        }
    }
    impl TaskDomain for RefuseExecution {
        fn validate_context(
            &self,
            _: &Cas,
            _: &TaskInvocationV1,
            _: &[String],
            _: &str,
        ) -> Result<(), String> {
            Err("not executing".into())
        }
        fn validate_output(
            &self,
            _: &Cas,
            _: &TaskRevisionV1,
            _: &ExecutionPlanV1,
            _: &TaskInvocationV1,
            _: &TaskOutputV1,
        ) -> Result<(), String> {
            Err("not executing".into())
        }
        fn validate_result(
            &self,
            _: &Cas,
            _: &TaskRevisionV1,
            _: &TaskResultV1,
        ) -> Result<(), String> {
            Err("not accepting".into())
        }
    }

    pub(super) fn settings() -> ReviewPlanSettings {
        ReviewPlanSettings {
            mode: "light".into(),
            resources: review_config::task::legacy_review::resources::ReviewResourcePolicy {
                uncapped_attempt_tokens: 1,
            },
            outputs: BTreeMap::from([(
                "findings".into(),
                review_graph::task::Address {
                    node: "ledger".into(),
                    port: "findings".into(),
                },
            )]),
            executions: BTreeMap::from([("reviewer".into(), WorkerExecutionV1::Command {})]),
            provider_admission: OperatorAttemptCost {
                tokens: 1,
                wall_ms: 1000,
            },
            allowed_effects: Default::default(),
        }
    }
    pub(super) fn artifact(cas: &Cas, ty: &str, value: impl serde::Serialize) -> String {
        cas.put_artifact(
            ty,
            review_core::Producer::KernelOperation {
                run_id: "review-plan-test".into(),
                node_id: None,
                operation_id: "capture".into(),
            },
            vec![],
            None,
            serde_json::to_value(value).unwrap(),
        )
        .unwrap()
        .0
    }
}
fn admit_heavy(
    cas: &Cas,
    store: &mut EventStore,
    declared_demands: bool,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    let returned = serde_json::json!({
        "findings":[{"severity":"major", "file":".af/pipelines/review.toml", "line":1,
            "title":"Missing required behavior", "body":"The required behavior is absent",
            "fix":"Implement the missing behavior", "confidence":0.9,
            "rule_id":"fixture/required-behavior@1", "occurrence_key":"required-behavior"}],
        "benchmark_demands":[{"claim":"latency is bounded", "why":"measure the acceptance limit",
            "suggested_method":"run the latency benchmark"}], "dispositions":[]
    });
    let definition = command_pipeline_returning(&returned);
    let definition = if declared_demands {
        definition.replace(
        "outputs = [{ name = \"findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"same_subject\" }]",
        "outputs = [{ name = \"findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"same_subject\" }, { name = \"demands\", type = \"review.kernel/DemandSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"same_subject\" }]",
    )
    } else {
        definition
    };
    admit_heavy_definition(cas, store, &definition)
}

pub(super) fn admit_heavy_definition(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    admit_heavy_definition_with_limits(cas, store, definition, capture::limits())
}

pub(super) fn admit_heavy_definition_with_limits(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    limits: review_core::task::TaskLimitsV1,
) -> (
    LegacyReviewPlanCompiler,
    review_store::store::task::TaskLease,
) {
    let round = captured_fixture::open_round_authority_with_convergence(
        cas,
        store,
        definition,
        None,
        review_core::CampaignConvergenceV1 {
            clean_rounds: 2,
            max_rounds: 3,
            gate: "major".into(),
        },
    );
    let mut settings = plan::settings();
    settings.mode = "heavy".into();
    let compiler = LegacyReviewPlanCompiler::capture(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"heavy Review fixture engine").unwrap(),
        settings,
    )
    .unwrap();
    let task = compiler
        .prepare_revision(cas, "heavy-review", limits)
        .unwrap();
    let revision_id = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let compiled = compiler.compile(cas, &revision_id).unwrap().0;
    let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &compiled);
    let authority = CapturedTaskAuthority::for_legacy_review(
        &compiler,
        &plan::RefuseExecution,
        &NoTaskDeveloper,
    );
    let lease = store
        .open_task(cas, &revision_id, "developer", 60000)
        .unwrap();
    store
        .propose_task_plan(cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, &authority).unwrap();
    (compiler, lease)
}

pub(super) fn observe_charge(
    cas: &Cas,
    store: &mut EventStore,
    lease: &review_store::store::task::TaskLease,
    attempt_id: &str,
    charge: u64,
) {
    let usage_id = plan::artifact(
        cas,
        review_core::task::usage::TASK_TOKEN_USAGE_V1,
        review_core::task::usage::TaskTokenUsageV1 {
            chargeable_tokens: charge.into(),
            ..Default::default()
        },
    );
    store
        .observe_task_usage(
            cas,
            lease,
            TaskExecutionRecordV1::UsageObserved {
                attempt_id: attempt_id.into(),
                charged_tokens: u128::from(charge),
                usage_id,
                raw_artifact_ids: vec![],
            },
        )
        .unwrap();
}

fn start_next_round(cas: &Cas, store: &mut EventStore, declared_demands: bool) -> String {
    let old = store.latest_round_started("review").unwrap().unwrap();
    let mut payload: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(old.payload).unwrap();
    let state = store.task_projection(cas, "heavy-review").unwrap().unwrap();
    let execution = state.execution.as_ref().unwrap();
    let ledger = execution
        .graph
        .nodes
        .iter()
        .find(|(_, node)| {
            matches!(
                node.operator,
                review_graph::task::CompiledOperator::ReviewDomain {
                    operation: review_graph::task::ReviewOperation::Ledger,
                    ..
                }
            )
        })
        .unwrap()
        .0;
    let receipt = &execution.outputs[ledger].1;
    let port = |ty: &str| {
        receipt
            .outputs
            .values()
            .find(|port| port.artifact_type == ty)
            .unwrap()
            .artifact_ids[0]
            .clone()
    };
    payload.round += 1;
    payload.epoch = 1;
    let finding_set_id = port(review_core::contract::FINDING_SET_V1);
    payload.prior_demand_set_id = port(review_core::contract::DEMAND_SET_V1);
    let findings: review_core::FindingSetV1 =
        serde_json::from_value(cas.get_artifact(&finding_set_id).unwrap().payload).unwrap();
    // This fixed fixture has only in-scope, located, unchanged-severity open Findings.
    // Match the CLI's raw PriorFindings view and retain the separate canonical set.
    let prior_rows: Vec<_> = findings.findings.iter().map(|finding| {
        assert_eq!(finding.scope, "in");
        assert_eq!(finding.status, "open");
        assert_eq!(finding.effective_severity, Some(finding.severity));
        serde_json::json!({"key":finding.finding_id,"severity":format!("{:?}", finding.severity).to_lowercase(),
            "status":finding.status,"file":finding.file,"line":finding.line,"title":finding.title,
            "body":finding.body,"source":finding.source,"last_seen_round":finding.last_seen_round})
    }).collect();
    payload.prior_finding_set_id = cas
        .put_json(&serde_json::json!({
            "subject_id":payload.subject_id,"round":payload.round,"prior_findings":prior_rows,
        }))
        .unwrap();
    let canonical = store
        .replay("review")
        .unwrap()
        .into_iter()
        .find(|event| {
            event.event_type == EventType::NodeOutputReceiptV1
                && event.node_id.as_deref() == Some("ledger")
                && event.causation_id.as_ref() == Some(&old.event_id)
        })
        .unwrap();
    let canonical: review_core::NodeOutputReceiptPayloadV1 =
        serde_json::from_value(canonical.payload).unwrap();
    assert!(
        canonical
            .outputs
            .iter()
            .any(|port| port.artifact_ids.contains(&finding_set_id))
    );
    assert_eq!(
        canonical
            .outputs
            .iter()
            .any(|port| port.artifact_ids.contains(&payload.prior_demand_set_id)),
        declared_demands
    );
    let mut refs = old.artifact_refs;
    refs.extend([
        payload.prior_finding_set_id.clone(),
        payload.prior_demand_set_id.clone(),
    ]);
    refs.sort();
    refs.dedup();
    let opened = store.campaign_opened("review").unwrap().unwrap();
    let next_round_number = payload.round;
    let next = store
        .append(
            "review",
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(payload).unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(refs),
        )
        .unwrap()
        .event_id;
    let projection = review_store::LedgerProjection::from_events(
        "review",
        &store.replay("review").unwrap(),
        cas,
    )
    .unwrap();
    let mut ingest = review_store::Ingest::from_projection(store, cas, "review", projection)
        .unwrap()
        .under_round(&next);
    while ingest.ledger().round < next_round_number {
        ingest.advance().unwrap();
    }
    next
}

/// Run two real Rounds; optionally close the third permitted Round and finish.
pub fn run_numeric_rounds(
    declared_demands: bool,
    finish_terminal_round: bool,
) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path().join("cas")).unwrap();
    let mut store = EventStore::open(temp.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_heavy(&cas, &mut store, declared_demands);
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let state = runtime.projection().unwrap();
    let attempt = state
        .execution
        .as_ref()
        .unwrap()
        .attempt_accounting()
        .pop()
        .unwrap();
    observe_charge(
        &cas,
        &mut shared.lock().unwrap(),
        &lease,
        &attempt.attempt_id,
        3,
    );
    let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
    assert_eq!(
        conclusion.verdict,
        review_pipeline::RunVerdict::Fail(review_store::Verdict::NotConverged)
    );
    assert!(conclusion.can_continue);
    assert!(!conclusion.resources_failed);
    assert_eq!(runtime.projection().unwrap().phase, TaskPhaseV1::Running {});
    let reopened = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        reopened.publish_recorded_round_conclusion(&cas).unwrap(),
        conclusion
    );
    for assembly_host in [&host, &reopened] {
        assert!(
            assembly_host
                .assemble_recorded_result(&cas)
                .unwrap_err()
                .contains("another permitted Round")
        );
        assert_eq!(
            assembly_host
                .publish_recorded_round_conclusion(&cas)
                .unwrap(),
            conclusion
        );
        let candidate = review_core::task::TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: review_core::task::TaskExecutionV1::Completed,
            acceptance: review_core::task::TaskAcceptanceV1::Unsatisfied,
            domain_conclusion: "premature_review_finish".into(),
            outputs: BTreeMap::new(),
            evidence: Default::default(),
            missing_obligations: Default::default(),
        };
        candidate.validate().unwrap();
        assert!(
            assembly_host
                .validate_result(&cas, &state.revision, &candidate)
                .unwrap_err()
                .contains("not been assembled")
        );
        let candidate_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, candidate);
        let finish_authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, assembly_host, &NoTaskDeveloper);
        assert!(
            shared
                .lock()
                .unwrap()
                .finish_task(&cas, &lease, &candidate_id, &finish_authority)
                .unwrap_err()
                .to_string()
                .contains("not been assembled")
        );
    }
    assert_eq!(runtime.projection().unwrap().phase, TaskPhaseV1::Running {});
    let next_round = start_next_round(&cas, &mut shared.lock().unwrap(), declared_demands);
    let successor = LegacyReviewPlanCompiler::reopen(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &shared.lock().unwrap(), "review", &next_round)
            .unwrap(),
        &state.revision.provenance.adapter_id,
        compiler.policy_id(),
    )
    .unwrap();
    let next = successor
        .prepare_continuation_revision(&cas, &state.revision_id, &state.revision)
        .unwrap();
    assert_eq!(next.task_id, state.revision.task_id);
    assert_eq!(next.revision, 2);
    assert_eq!(next.previous_revision_id.as_ref(), Some(&state.revision_id));
    assert_ne!(next.inputs, state.revision.inputs);
    let mut restored = next.clone();
    restored.revision = state.revision.revision;
    restored.previous_revision_id = state.revision.previous_revision_id.clone();
    restored.inputs = state.revision.inputs.clone();
    assert_eq!(
        restored, state.revision,
        "all immutable Task fields and original limits survive"
    );
    let next_id = plan::artifact(&cas, review_core::task::TASK_REVISION_V1, &next);
    let (next_plan, compiled) = successor.compile(&cas, &next_id).unwrap();
    assert_eq!(next_plan.limits, state.revision.limits);
    assert!(
        compiled
            .compilation
            .graph
            .token_scopes
            .contains_key("review.round2")
    );
    assert!(
        !compiled
            .compilation
            .graph
            .token_scopes
            .contains_key("review.round1")
    );
    assert_eq!(
        successor
            .recompile(&cas, &next, &next_plan)
            .unwrap()
            .compilation
            .graph,
        compiled.compilation.graph
    );
    for field in ["authority", "strategy", "required_outputs"] {
        let mut altered = serde_json::to_value(&state.revision).unwrap();
        match field {
            "authority" => {
                altered[field]["policy_id"] = serde_json::json!(cas.put(b"other policy").unwrap())
            }
            "strategy" => altered[field] = serde_json::json!("light"),
            _ => altered[field] = serde_json::json!({}),
        }
        let altered: review_core::task::TaskRevisionV1 = serde_json::from_value(altered).unwrap();
        let id = plan::artifact(&cas, review_core::task::TASK_REVISION_V1, &altered);
        assert!(
            successor
                .prepare_continuation_revision(&cas, &id, &altered)
                .is_err(),
            "{field}"
        );
    }
    assert!(
        successor
            .prepare_continuation_revision(&cas, &next_id, &next)
            .is_err()
    );
    let next_plan_id = plan::artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &next_plan);
    let handoff = review_core::task::review_handoff::TaskReviewHandoffV1 {
        task_id: lease.task_id().into(),
        predecessor_revision_id: state.revision_id.clone(),
        predecessor_plan_id: state.plan_id.clone().unwrap(),
        successor_revision_id: next_id,
        successor_plan_id: next_plan_id,
        predecessor_round_id: state.revision.inputs["round"].artifact_ids[0].clone(),
        successor_round_id: next.inputs["round"].artifact_ids[0].clone(),
        evidence: review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::ClosedRound {
            report_event_id: conclusion.canonical_report_event_id.clone(),
        },
    };
    let handoff_id =
        review_store::store::task::review_handoff::capture_task_review_handoff(&cas, &handoff)
            .unwrap();
    let next_authority =
        CapturedTaskAuthority::for_legacy_review(&successor, &host, &NoTaskDeveloper);
    let transition = shared
        .lock()
        .unwrap()
        .continue_task_review(&cas, &lease, &handoff_id, &next_authority)
        .unwrap();
    assert_eq!(transition.event_type, EventType::TaskTransitionV2);
    assert!(
        serde_json::from_value::<review_core::task::event::TaskTransitionV1>(
            transition.payload.clone()
        )
        .is_err()
    );
    assert_eq!(
        review_store::store::task::review_handoff::read_task_transition(&transition)
            .unwrap()
            .change,
        review_core::task::event::TaskChangeV1::ReviewContinued {
            handoff_id: handoff_id.clone()
        }
    );
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .continue_task_review(&cas, &lease, &handoff_id, &next_authority)
            .unwrap(),
        transition
    );
    let continued = runtime.projection().unwrap();
    assert_eq!(continued.phase, TaskPhaseV1::Ready {});
    assert!(!continued.admitted);
    assert_eq!(
        continued
            .execution
            .as_ref()
            .unwrap()
            .budget
            .committed_tokens(),
        3
    );
    shared
        .lock()
        .unwrap()
        .admit_task_plan(&cas, &lease, &next_authority)
        .unwrap();
    let next_host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &successor,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let next_authority =
        CapturedTaskAuthority::for_legacy_review(&successor, &next_host, &NoTaskDeveloper);
    let next_runtime = TaskRuntime::with_store(
        shared.clone(),
        &cas,
        lease.clone(),
        &next_authority,
        &next_host,
    )
    .unwrap();
    let second_report = next_runtime.execute().unwrap();
    assert!(second_report.complete(), "{second_report:?}");
    // Late evidence still belongs to the first Round's original reservation and scopes.
    observe_charge(
        &cas,
        &mut shared.lock().unwrap(),
        &lease,
        &attempt.attempt_id,
        5,
    );
    let second = next_host.publish_recorded_round_conclusion(&cas).unwrap();
    assert!(second.can_continue);
    assert_eq!(second.verdict, conclusion.verdict);
    let second_state = next_runtime.projection().unwrap();
    let execution = second_state.execution.as_ref().unwrap();
    assert_eq!(execution.budget.committed_tokens(), 5);
    assert_eq!(
        execution.budget.scope_committed_tokens("review.round1"),
        Some(5)
    );
    assert_eq!(
        execution.budget.scope_committed_tokens("review.round2"),
        Some(0)
    );
    assert_eq!(execution.budget.begun_attempts(), 2);
    assert_eq!(
        execution
            .attempt_accounting()
            .iter()
            .find(|a| a.attempt_id == attempt.attempt_id)
            .unwrap()
            .plan_id,
        state.plan_id.clone().unwrap()
    );
    let locked = shared.lock().unwrap();
    assert_eq!(locked.task_ids(&cas).unwrap(), [lease.task_id()]);
    let events = locked.replay("review").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == EventType::RunReportV6)
            .count(),
        2
    );
    let first = events
        .iter()
        .find(|e| e.event_id == conclusion.canonical_report_event_id)
        .unwrap();
    let second_event = events
        .iter()
        .find(|e| e.event_id == second.canonical_report_event_id)
        .unwrap();
    assert_eq!(first.payload["spent_tokens"], "3");
    assert_eq!(second_event.payload["spent_tokens"], "5");
    let round_payload: review_core::RoundStartedPayloadV1 = serde_json::from_value(
        events
            .iter()
            .find(|e| e.event_id == next_round)
            .unwrap()
            .payload
            .clone(),
    )
    .unwrap();
    let first_finding_id = &state.execution.as_ref().unwrap().outputs
        [&compiled.compilation.nodes["ledger"].task_node]
        .1
        .outputs
        .values()
        .find(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
        .unwrap()
        .artifact_ids[0];
    let first_findings: review_core::FindingSetV1 =
        serde_json::from_value(cas.get_artifact(first_finding_id).unwrap().payload).unwrap();
    let first_demands: review_core::DemandSetV1 = serde_json::from_value(
        cas.get_artifact(&round_payload.prior_demand_set_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    let selected = |ty: &str| {
        execution.outputs[&compiled.compilation.nodes["ledger"].task_node]
            .1
            .outputs
            .values()
            .find(|port| port.artifact_type == ty)
            .unwrap()
            .artifact_ids[0]
            .clone()
    };
    let findings: review_core::FindingSetV1 = serde_json::from_value(
        cas.get_artifact(&selected(review_core::contract::FINDING_SET_V1))
            .unwrap()
            .payload,
    )
    .unwrap();
    let demands: review_core::DemandSetV1 = serde_json::from_value(
        cas.get_artifact(&selected(review_core::contract::DEMAND_SET_V1))
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(findings.round, 2);
    assert_eq!(&findings.prior_finding_set_id, first_finding_id);
    assert_eq!(
        demands.prior_demand_set_id,
        round_payload.prior_demand_set_id
    );
    assert!(!first_findings.findings.is_empty());
    assert!(!first_demands.demands.is_empty());
    for first in first_findings.findings {
        assert!(
            findings
                .findings
                .iter()
                .any(|finding| finding.finding_id == first.finding_id)
        );
    }
    for first in first_demands.demands {
        assert!(
            demands
                .demands
                .iter()
                .any(|demand| demand.demand_id == first.demand_id)
        );
    }
    let reopened_store = EventStore::open_read_only(temp.path().join("events.sqlite")).unwrap();
    let replayed = reopened_store
        .task_projection(&cas, lease.task_id())
        .unwrap()
        .unwrap();
    assert_eq!(replayed.revision_id, second_state.revision_id);
    assert_eq!(replayed.review_handoffs, [(handoff_id, handoff)]);
    assert_eq!(replayed.execution.unwrap().budget.committed_tokens(), 5);

    drop(locked);
    if finish_terminal_round {
        let third_round = start_next_round(&cas, &mut shared.lock().unwrap(), declared_demands);
        let third_compiler = LegacyReviewPlanCompiler::reopen(
            &cas,
            CapturedLegacyReviewRound::load(&cas, &shared.lock().unwrap(), "review", &third_round)
                .unwrap(),
            &second_state.revision.provenance.adapter_id,
            successor.policy_id(),
        )
        .unwrap();
        let third_revision = third_compiler
            .prepare_continuation_revision(&cas, &second_state.revision_id, &second_state.revision)
            .unwrap();
        let third_revision_id =
            plan::artifact(&cas, review_core::task::TASK_REVISION_V1, &third_revision);
        let third_plan = third_compiler.compile(&cas, &third_revision_id).unwrap().0;
        let third_plan_id = plan::artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &third_plan);
        let third_handoff = review_core::task::review_handoff::TaskReviewHandoffV1 {
            task_id: lease.task_id().into(),
            predecessor_revision_id: second_state.revision_id.clone(),
            predecessor_plan_id: second_state.plan_id.clone().unwrap(),
            successor_revision_id: third_revision_id,
            successor_plan_id: third_plan_id,
            predecessor_round_id: second_state.revision.inputs["round"].artifact_ids[0].clone(),
            successor_round_id: third_revision.inputs["round"].artifact_ids[0].clone(),
            evidence: review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::ClosedRound {
                report_event_id: second.canonical_report_event_id,
            },
        };
        let third_handoff_id =
            review_store::store::task::review_handoff::capture_task_review_handoff(
                &cas,
                &third_handoff,
            )
            .unwrap();
        let third_authority =
            CapturedTaskAuthority::for_legacy_review(&third_compiler, &next_host, &NoTaskDeveloper);
        shared
            .lock()
            .unwrap()
            .continue_task_review(&cas, &lease, &third_handoff_id, &third_authority)
            .unwrap();
        shared
            .lock()
            .unwrap()
            .admit_task_plan(&cas, &lease, &third_authority)
            .unwrap();
        let third_host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &third_compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let third_authority = CapturedTaskAuthority::for_legacy_review(
            &third_compiler,
            &third_host,
            &NoTaskDeveloper,
        );
        let third_runtime = TaskRuntime::with_store(
            shared.clone(),
            &cas,
            lease.clone(),
            &third_authority,
            &third_host,
        )
        .unwrap();
        let report = third_runtime.execute().unwrap();
        assert!(report.complete(), "{report:?}");
        let terminal = third_host.publish_recorded_round_conclusion(&cas).unwrap();
        assert_eq!(
            terminal.verdict,
            review_pipeline::RunVerdict::Fail(review_store::Verdict::Exhausted)
        );
        assert!(
            !terminal.resources_failed,
            "the semantic Round cap does not exhaust Task resources"
        );
        assert!(!terminal.can_continue);
        let result = third_host.assemble_recorded_result(&cas).unwrap();
        assert_eq!(third_host.assemble_recorded_result(&cas).unwrap(), result);
        assert_eq!(
            result.execution,
            review_core::task::TaskExecutionV1::Completed
        );
        assert_eq!(
            result.acceptance,
            review_core::task::TaskAcceptanceV1::Unsatisfied
        );
        let result_id = plan::artifact(&cas, review_core::task::TASK_RESULT_V1, result);
        third_runtime.finish(&result_id).unwrap();
        let finished = third_runtime.projection().unwrap();
        assert_eq!(finished.phase, TaskPhaseV1::Finished { result_id });
        assert_eq!(finished.execution.unwrap().budget.committed_tokens(), 5);
    }
    temp
}
