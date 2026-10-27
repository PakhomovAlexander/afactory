//! The one Task state every `af task` rendering prints (`list`, `show`, `report` and the last
//! line of `run`), derived here from what the Store recorded: the phase, the outcome and its cause
//! ([`review_core::task::state`]), the next command, the gate's last check receipt with the
//! failing tests its runners named, and each failed node's first error line. Read-only: nothing
//! is written, no Worker runs, no Provider is contacted.

use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::feedback::{
    TASK_RETRY_FEEDBACK_V1, TaskFeedbackCodeV1, TaskRetryFeedbackV1,
};
use review_core::task::report::{
    TASK_DIAGNOSTIC_V1, TaskDiagnosticV1, TaskFailureClassV1, TaskNodeOutcomeV1,
};
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1,
};
use review_core::task::state::*;
use review_core::task::usage::TaskUnknownUsageCauseV1;
use review_core::task::verification::{TASK_CHECK_RECEIPT_V2, TaskCheckReceiptV2, TaskCheckStopV2};
use review_graph::task::{CompiledOperator, ReviewOperation};
use review_store::store::task::execution::TaskAttemptAccounting;
use review_store::store::task::gate_retry::GateRetry;

/// The schema of `af task show ID --checks --json`.
pub(crate) const TASK_CHECKS_V3: &str = "af/task-checks@3";

/// How many lines of a check's stderr `af task show --checks` prints.
const STDERR_TAIL_LINES: usize = 40;

/// The literal a next command names for the Task file a person edits.
pub(crate) const TASK_FILE: &str = "TASK_FILE";

/// The literal a next command names for the ID of a new Task a person chooses.
pub(crate) const NEW_TASK_ID: &str = "NEW_TASK_ID";

/// The state of one Task and the evidence its renderings print beside it.
pub(crate) struct TaskView {
    pub(crate) state: TaskStateV1,
    /// The gate's last check receipt, summarized; absent before a check recorded one.
    pub(crate) gate: Option<TaskGateV1>,
    /// Every check of that receipt, for `af task show --checks`.
    pub(crate) checks: Option<ChecksView>,
    /// Each failed node of the last run report, in plan order; a node that only lacks a
    /// receipt another node did not produce comes after the node that failed.
    pub(crate) failures: Vec<NodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NodeFailure {
    pub(crate) node: String,
    pub(crate) cause: Option<TaskCauseV1>,
    pub(crate) first_line: String,
    /// The recorded diagnostic that holds the whole text.
    pub(crate) diagnostic_id: String,
    /// The full stderr of the node's last failed Attempt or check, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stderr_id: Option<String>,
    /// What happened in af's own words when the Attempt wall ended the node's last Attempt
    /// (ADR-0253): `implementer reached its Attempt wall of 30 min after 30 min`. The report
    /// prints it where it prints no diagnostic.
    #[serde(skip)]
    pub(crate) wall: Option<String>,
}

/// One check receipt as `af task show --checks` prints it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChecksView {
    pub(crate) node: String,
    pub(crate) receipt_id: String,
    pub(crate) outcome: String,
    pub(crate) checks: Vec<CheckView>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckView {
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) elapsed_ms: Option<u64>,
    /// `cargo_target warm 4096, cargo_home cold toolchain_unresolved`, or `cold` for a check
    /// without a warm cache.
    pub(crate) cache: String,
    pub(crate) remote: bool,
    /// The wall the kernel stopped the check at, as its receipt records it (ADR-0255); absent
    /// for a check that ended on its own or never started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<TaskCheckStopV2>,
    /// The step the check's own step log says stopped it (`scripts/ci-step.py`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stopped_step: Option<StoppedStep>,
    pub(crate) failed_tests: Vec<String>,
    pub(crate) flaky: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stdout_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stderr_id: Option<String>,
    pub(crate) stderr_tail: Vec<String>,
    /// The check's warm layer, as `af/TaskCheckLayer@1` recorded it (ADR-0207).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layer: Option<review_core::task::check_layer::TaskCheckLayerRunV1>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StoppedStep {
    pub(crate) step: String,
    pub(crate) exit_code: i64,
}

/// The state of an uncollected Task at `now`. `cancelled` is the lease epoch of the run an
/// `af task cancel` request stopped, when the Store's `runs/` directory records one: a Task
/// that stopped under that very lease was cancelled by a person, `host_cancelled`, whatever its
/// last Attempt was doing when the signal came.
pub(crate) fn view(
    cas: &Cas,
    state: &TaskProjection,
    result: Option<&TaskResultV1>,
    now: u64,
    cancelled: Option<u64>,
) -> Result<TaskView, String> {
    let checks = last_checks(cas, state)?;
    let gate = checks.as_ref().map(gate_of);
    let failures = failures(cas, state)?;
    let id = &state.task_id;
    let phase = match &state.phase {
        TaskPhaseV1::Finished { .. } => TaskStatePhaseV1::Finished,
        TaskPhaseV1::Waiting { .. } => TaskStatePhaseV1::Waiting,
        _ if state.lease_until_unix_ms() > now => TaskStatePhaseV1::Running,
        // A plan that waits for `af task run` to confirm it, a refreshed one included.
        _ if !state.admitted => TaskStatePhaseV1::Planned,
        // A run that ended on a node's own failure without finishing waits for a person (a
        // signed decision, say); one that was interrupted or lost its writer was abandoned, and
        // so was a node whose Attempt was (ADR-0209).
        _ if abandoned_cause(cas, state, cancelled) != TaskCauseV1::HostCancelled
            && failures.iter().any(|f| {
                f.cause.is_some_and(|cause| {
                    !matches!(cause, TaskCauseV1::WriterLost | TaskCauseV1::HostCancelled)
                })
            }) =>
        {
            TaskStatePhaseV1::Waiting
        }
        _ => TaskStatePhaseV1::Abandoned,
    };
    let state_value = match phase {
        TaskStatePhaseV1::Running => TaskStateV1::new(phase),
        TaskStatePhaseV1::Planned => match &state.plan_id {
            Some(plan) => {
                TaskStateV1::new(phase).with_next(format!("af task run {id} --confirm-plan {plan}"))
            }
            None => TaskStateV1::new(phase).with_next(format!("af task explain {id}")),
        },
        TaskStatePhaseV1::Waiting => {
            let next = match &state.phase {
                TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsPlanReview,
                } => format!("af task explain {id} --tree"),
                TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsResources,
                } => format!("af task plan --file {TASK_FILE} --fit"),
                _ => format!("af task show {id}"),
            };
            TaskStateV1::new(phase).with_next(next)
        }
        // A node that used up its recovery allowance is never dispatched again: `af task run`
        // refuses, so the next command starts the Task again from its file (ADR-0209).
        TaskStatePhaseV1::Abandoned => TaskStateV1::ended(
            phase,
            TaskOutcomeV1::Cancelled,
            Some(abandoned_cause(cas, state, cancelled)),
        )
        .with_next(if super::recovery::exhausted_any(state) {
            format!("af task start --file {TASK_FILE}")
        } else {
            format!("af task run {id}")
        }),
        TaskStatePhaseV1::Finished => {
            let result = result.ok_or("A finished Task has no recorded result")?;
            finished(
                cas,
                state,
                result,
                gate.as_ref(),
                checks.as_ref(),
                &failures,
            )?
        }
    };
    state_value.validate()?;
    Ok(TaskView {
        state: state_value,
        gate,
        checks,
        failures,
    })
}

/// The state of a collected Task, from the summary its tombstone retained.
pub(crate) fn collected(outcome: &str) -> TaskStateV1 {
    let outcome = match outcome {
        "verified" | "pass" => TaskOutcomeV1::Verified,
        "changes_requested" | "convergence_exhausted" => TaskOutcomeV1::ChangesRequested,
        _ => TaskOutcomeV1::Exhausted,
    };
    TaskStateV1::ended(
        TaskStatePhaseV1::Finished,
        outcome,
        Some(TaskCauseV1::Collected),
    )
}

/// Whether any Attempt of the Task began or its plan was admitted: a Task that did neither has
/// not started, and its wall clock has not either.
pub(crate) fn started(state: &TaskProjection) -> bool {
    state.admitted
        || state
            .execution
            .as_ref()
            .is_some_and(|execution| execution.budget.begun_attempts() > 0)
}

/// Why a Task that ran stopped before it finished: an `af task cancel` of its last lease, a
/// started Attempt nobody settled (its writer was lost), the cause its last Attempt was
/// abandoned with (ADR-0209), an interrupt as its last Attempt's usage or diagnostic records it,
/// or a writer that lost its lease (the default when nothing says more).
fn abandoned_cause(cas: &Cas, state: &TaskProjection, cancelled: Option<u64>) -> TaskCauseV1 {
    use review_store::store::task::execution::TaskAbandonCause;
    if cancelled == Some(state.lease_epoch()) {
        return TaskCauseV1::HostCancelled;
    }
    if state
        .execution
        .as_ref()
        .is_some_and(|execution| !execution.orphaned_attempts().is_empty())
    {
        return TaskCauseV1::WriterLost;
    }
    let mut attempts = state
        .execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
        .collect::<Vec<_>>();
    attempts.sort_by_key(|attempt| (attempt.settled_unix_ms, attempt.started_unix_ms));
    let Some(last) = attempts.last() else {
        return TaskCauseV1::WriterLost;
    };
    match last.abandoned {
        Some(TaskAbandonCause::WriterLost) => return TaskCauseV1::WriterLost,
        Some(TaskAbandonCause::HostCancelled) => return TaskCauseV1::HostCancelled,
        None => {}
    }
    let interrupted = last.unknown_usage == Some(TaskUnknownUsageCauseV1::Interrupted)
        || attempt_error(cas, last).and_then(|error| TaskCauseV1::of_diagnostic(&error))
            == Some(TaskCauseV1::HostCancelled);
    if interrupted {
        TaskCauseV1::HostCancelled
    } else {
        TaskCauseV1::WriterLost
    }
}

fn finished(
    cas: &Cas,
    state: &TaskProjection,
    result: &TaskResultV1,
    gate: Option<&TaskGateV1>,
    checks: Option<&ChecksView>,
    failures: &[NodeFailure],
) -> Result<TaskStateV1, String> {
    let id = &state.task_id;
    let phase = TaskStatePhaseV1::Finished;
    // A review that ran to its conclusion asked for changes; a Review Task's acceptance is only
    // that the review ran, so its conclusion says what it found.
    let concluded = matches!(
        result.domain_conclusion.as_str(),
        "changes_requested" | "convergence_exhausted"
    );
    if result.acceptance == TaskAcceptanceV1::Satisfied && !concluded {
        let verified = TaskStateV1::ended(phase, TaskOutcomeV1::Verified, None);
        let deliverable = result.outputs.contains_key("snapshot")
            && state
                .revision
                .authority
                .allowed_effects
                .contains("write-source")
            && state.deliveries.is_empty();
        return Ok(if deliverable {
            verified.with_next(format!(
                "af task deliver {id} --branch af/{id} --worktree ../{id} --confirm {id}"
            ))
        } else {
            verified
        });
    }
    let ended = |cause: TaskCauseV1| -> Result<TaskStateV1, String> {
        Ok(TaskStateV1::ended(
            phase,
            cause.outcome().unwrap_or(TaskOutcomeV1::Exhausted),
            Some(cause),
        )
        .with_next(next_action(cas, state, cause)?))
    };
    // The last gate failing decides: what came after it never ran on a passing tree.
    if let (Some(gate), Some(checks)) = (
        gate.filter(|g| g.status != TaskGateStatusV1::Passed),
        checks,
    ) {
        return ended(gate_cause(checks, gate));
    }
    // Findings a review recorded are its verdict, whatever else failed beside it.
    if concluded && reviewed_with_findings(cas, state)? {
        return ended(TaskCauseV1::ReviewerFindings);
    }
    // The first node that failed on its own decides; a node that only missed another's receipt
    // follows from it.
    let cause = if let Some(failure) = failures.iter().find(|f| f.cause.is_some()) {
        failure.cause.expect("found by its cause")
    } else if concluded || result.acceptance == TaskAcceptanceV1::Unsatisfied {
        TaskCauseV1::EvaluatorRejected
    } else {
        planning_cause(cas, state).unwrap_or(TaskCauseV1::Budget)
    };
    ended(cause)
}

/// The one command a person runs after a Task ended with `cause`. After a failed gate it is
/// `--retry-gate` exactly when the kernel would run that gate again now
/// ([`TaskProjection::failed_gates`]: its approval, deadline and budget included, read on the
/// Store's clock that stamps a retry), so the state never names a command the kernel refuses;
/// otherwise the gate's last check receipt.
fn next_action(cas: &Cas, state: &TaskProjection, cause: TaskCauseV1) -> Result<String, String> {
    let id = &state.task_id;
    Ok(match cause {
        TaskCauseV1::ReviewerFindings => format!("af task findings {id}"),
        TaskCauseV1::CheckFailed
        | TaskCauseV1::CheckFlakyThenFailed
        | TaskCauseV1::PreflightFailed => {
            let now = review_store::store::task::now().map_err(|e| e.to_string())?;
            gate_next_action(
                id,
                &state.failed_gates(cas, now).map_err(|e| e.to_string())?,
            )
        }
        TaskCauseV1::ProviderNetwork
        | TaskCauseV1::ProviderCapacity
        | TaskCauseV1::ProviderTimeout
        | TaskCauseV1::ProviderEnvironment => "af provider status".into(),
        // The edits an Attempt wall stopped are kept: a continuation starts from them
        // (ADR-0253). A Worker that writes no source left nothing to continue from.
        TaskCauseV1::AttemptWall => match super::continuation::partial_candidate(cas, state) {
            Some(_) => format!("af task run {NEW_TASK_ID} --from {id}"),
            None => format!("af task show {id}"),
        },
        TaskCauseV1::IdentityChanged | TaskCauseV1::IdentityUnverified => {
            match failed_provider(cas, state) {
                Some(provider) => format!("af provider setup {provider} --login"),
                None => "af provider status".into(),
            }
        }
        TaskCauseV1::WriterLost | TaskCauseV1::HostCancelled => format!("af task run {id}"),
        TaskCauseV1::Deadline | TaskCauseV1::Budget => {
            format!("af task plan --file {TASK_FILE} --fit")
        }
        TaskCauseV1::EvaluatorRejected
        | TaskCauseV1::MalformedReply
        | TaskCauseV1::ModelRefused
        | TaskCauseV1::Collected => format!("af task show {id}"),
    })
}

/// The next command after a failed gate: `--retry-gate` when the kernel runs it again, else the
/// gate's last check receipt.
fn gate_next_action(id: &str, retry: &GateRetry) -> String {
    match retry {
        GateRetry::Gates(_) => format!("af task run {id} --retry-gate"),
        GateRetry::Refused(_) => format!("af task show {id} --checks"),
    }
}

/// The Provider ID the plan binds to the first failed Model Attempt's slot, when it names one
/// a shell takes as one word.
fn failed_provider(cas: &Cas, state: &TaskProjection) -> Option<String> {
    let execution = state.execution.as_ref()?;
    let plan: ExecutionPlanV1 = artifact(cas, state.plan_id.as_deref()?, EXECUTION_PLAN_V1).ok()?;
    execution
        .attempt_accounting()
        .iter()
        .filter(|a| matches!(a.result, Some(TaskAttemptResultV1::Failed { .. })))
        .find_map(|attempt| {
            let node = execution.graph.nodes.get(&attempt.reservation.node)?;
            let slot = node_slot(&node.operator)?;
            match &plan.bindings.get(slot)?.execution {
                WorkerExecutionV1::Model { provider, .. } if is_name(provider) => {
                    Some(provider.clone())
                }
                _ => None,
            }
        })
}

fn node_slot(operator: &CompiledOperator) -> Option<&str> {
    match operator {
        CompiledOperator::ProviderAdmission { bindings } => bindings.first().map(String::as_str),
        CompiledOperator::Primitive {
            operator:
                pipeline::TaskOperatorV1::Worker { slot }
                | pipeline::TaskOperatorV1::Verify { slot }
                | pipeline::TaskOperatorV1::FixVerify { slot },
            ..
        } => Some(slot),
        CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        } => Some(slot),
        _ => None,
    }
}

/// What kind of work a node does, for a failure no recorded word classifies.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Worker,
    Admission,
    Check,
    Kernel,
}

fn node_kind(operator: Option<&CompiledOperator>) -> NodeKind {
    match operator {
        Some(CompiledOperator::ProviderAdmission { .. }) => NodeKind::Admission,
        Some(CompiledOperator::Primitive {
            operator: pipeline::TaskOperatorV1::Check { .. },
            ..
        })
        | Some(CompiledOperator::ReviewIntegrationChecks { .. }) => NodeKind::Check,
        Some(operator) if node_slot(operator).is_some() => NodeKind::Worker,
        _ => NodeKind::Kernel,
    }
}

/// A failed gate's cause: a check the kernel stopped at its wall ran and failed (ADR-0255); a
/// check never started, or every failed check stopped at a step before its tests and named no
/// test; its tests failed after some were flaky; or it failed.
fn gate_cause(checks: &ChecksView, gate: &TaskGateV1) -> TaskCauseV1 {
    let preflight = checks.checks.iter().all(|c| {
        c.status != "failed"
            || (c.failed_tests.is_empty()
                && c.stopped_step
                    .as_ref()
                    .is_some_and(|s| !s.step.starts_with("test")))
    });
    let walled = checks
        .checks
        .iter()
        .any(|c| c.status != "passed" && c.stop.is_some());
    match gate.status {
        _ if walled && gate.flaky.is_empty() => TaskCauseV1::CheckFailed,
        _ if walled => TaskCauseV1::CheckFlakyThenFailed,
        TaskGateStatusV1::NotRun => TaskCauseV1::PreflightFailed,
        _ if preflight => TaskCauseV1::PreflightFailed,
        _ if !gate.flaky.is_empty() => TaskCauseV1::CheckFlakyThenFailed,
        _ => TaskCauseV1::CheckFailed,
    }
}

fn gate_of(checks: &ChecksView) -> TaskGateV1 {
    let failed = checks.checks.iter().any(|c| c.status == "failed");
    let not_run = checks.checks.iter().any(|c| c.status == "not_run");
    let mut failed_tests = Vec::new();
    let mut flaky = Vec::new();
    for check in &checks.checks {
        for name in &check.failed_tests {
            if !failed_tests.contains(name) && failed_tests.len() < MAX_FAILED_TESTS {
                failed_tests.push(name.clone());
            }
        }
        for name in &check.flaky {
            if !flaky.contains(name) && flaky.len() < MAX_FAILED_TESTS {
                flaky.push(name.clone());
            }
        }
    }
    let status = if failed {
        TaskGateStatusV1::Failed
    } else if not_run {
        TaskGateStatusV1::NotRun
    } else {
        TaskGateStatusV1::Passed
    };
    TaskGateV1 {
        status,
        failed_tests,
        flaky,
    }
}

/// Whether a review round the Task recorded asked for changes.
fn reviewed_with_findings(cas: &Cas, state: &TaskProjection) -> Result<bool, String> {
    let Some(execution) = &state.execution else {
        return Ok(false);
    };
    for id in execution
        .outputs
        .values()
        .flat_map(|(_, output)| output.outputs.values())
        .filter(|port| port.artifact_type == TASK_REVIEW_ROUND_V1)
        .flat_map(|port| port.artifact_ids.iter())
    {
        let round: TaskReviewRoundV1 = artifact(cas, id, TASK_REVIEW_ROUND_V1)?;
        if matches!(
            round.conclusion,
            review_core::task::review::ReviewConclusionV1::ChangesRequested
                | review_core::task::review::ReviewConclusionV1::ConvergenceExhausted
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The cause a planning diagnostic the result cites names, if it names one.
fn planning_cause(cas: &Cas, state: &TaskProjection) -> Option<TaskCauseV1> {
    let TaskPhaseV1::Finished { result_id } = &state.phase else {
        return None;
    };
    let envelope = cas.get_artifact(result_id).ok()?;
    envelope.input_artifacts.iter().find_map(|id| {
        let diagnostic = cas.get_json(id).ok()?;
        (diagnostic["schema"] == "af.planning-diagnostic/1")
            .then(|| {
                diagnostic["reason"]
                    .as_str()
                    .and_then(TaskCauseV1::of_diagnostic)
            })
            .flatten()
            .filter(|cause| {
                matches!(
                    cause.outcome(),
                    Some(TaskOutcomeV1::Exhausted | TaskOutcomeV1::Expired)
                )
            })
    })
}

/// Each failed node of the Task's last run report, with the cause its own records name.
fn failures(cas: &Cas, state: &TaskProjection) -> Result<Vec<NodeFailure>, String> {
    let Some(report_id) = state.run_reports.last() else {
        return Ok(Vec::new());
    };
    let report = review_store::store::task::read_task_run_report(cas, report_id)
        .map_err(|e| e.to_string())?;
    let attempts = state
        .execution
        .as_ref()
        .map(|execution| execution.attempt_accounting())
        .unwrap_or_default();
    let mut own = Vec::new();
    let mut consequences = Vec::new();
    for node in &report.nodes {
        let TaskNodeOutcomeV1::Failed {
            diagnostic_id,
            class,
        } = &node.outcome
        else {
            continue;
        };
        let diagnostic: TaskDiagnosticV1 = artifact(cas, diagnostic_id, TASK_DIAGNOSTIC_V1)?;
        let message = diagnostic.message;
        let operator = state
            .execution
            .as_ref()
            .and_then(|execution| execution.graph.nodes.get(&node.node))
            .map(|definition| &definition.operator);
        let last_failed = attempts
            .iter()
            .filter(|a| a.reservation.node == node.node)
            .filter(|a| {
                matches!(
                    a.result,
                    Some(
                        TaskAttemptResultV1::Failed { .. } | TaskAttemptResultV1::Abandoned { .. }
                    )
                )
            })
            .max_by_key(|a| (a.settled_unix_ms, a.started_unix_ms));
        let attempt_error = last_failed.and_then(|attempt| attempt_error(cas, attempt));
        let consequence = message.starts_with("Required branch receipt was not produced");
        let cause = if consequence {
            None
        } else {
            Some(node_cause(
                cas,
                *class,
                node_kind(operator),
                last_failed,
                attempt_error.as_deref(),
                &message,
            ))
        };
        let first = first_line(attempt_error.as_deref().unwrap_or(&message));
        let failure = NodeFailure {
            node: node.node.clone(),
            cause,
            first_line: if first.is_empty() {
                first_line(&message)
            } else {
                first
            },
            diagnostic_id: last_failed
                .and_then(|a| match &a.result {
                    Some(TaskAttemptResultV1::Failed { diagnostic_id, .. })
                    | Some(TaskAttemptResultV1::Abandoned { diagnostic_id }) => {
                        Some(diagnostic_id.clone())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| diagnostic_id.clone()),
            stderr_id: last_failed.and_then(|attempt| stderr_capture(cas, attempt)),
            wall: last_failed
                .and_then(|attempt| wall_end(cas, attempt))
                .map(|(_, end)| end.describe(operator.and_then(node_slot))),
        };
        if consequence {
            consequences.push(failure);
        } else {
            own.push(failure);
        }
    }
    own.extend(consequences);
    Ok(own)
}

/// The `error` an Attempt's own diagnostic records.
fn attempt_error(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<String> {
    let (Some(TaskAttemptResultV1::Failed { diagnostic_id, .. })
    | Some(TaskAttemptResultV1::Abandoned { diagnostic_id })) = &attempt.result
    else {
        return None;
    };
    let value = cas.get_json(diagnostic_id).ok()?;
    value["error"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .map(str::to_owned)
}

/// The closed class an Attempt's own diagnostic names, when af wrote one (`cause`):
/// `host_cancelled`, `writer_lost`, `attempt_wall` or `environment`.
fn diagnostic_cause(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<String> {
    let (Some(TaskAttemptResultV1::Failed { diagnostic_id, .. })
    | Some(TaskAttemptResultV1::Abandoned { diagnostic_id })) = &attempt.result
    else {
        return None;
    };
    let value = cas.get_json(diagnostic_id).ok()?;
    value["cause"].as_str().map(str::to_owned)
}

/// The `af/TaskAttemptWallEnd@1` an Attempt's settlement keeps among its raw evidence, with its
/// ID, when the Attempt wall ended it (ADR-0253).
pub(crate) fn wall_end(
    cas: &Cas,
    attempt: &TaskAttemptAccounting,
) -> Option<(
    String,
    review_core::task::attempt_wall::TaskAttemptWallEndV1,
)> {
    use review_core::task::attempt_wall::*;
    attempt.raw_artifact_ids.iter().find_map(|id| {
        let envelope = cas.get_optional_artifact(id).ok()??;
        if envelope.artifact_type != TASK_ATTEMPT_WALL_END_V1 {
            return None;
        }
        let end: TaskAttemptWallEndV1 = serde_json::from_value(envelope.payload).ok()?;
        end.validate().ok()?;
        Some((id.clone(), end))
    })
}

/// The raw capture of an Attempt's stderr, when its Worker adapter recorded one by that name.
fn stderr_capture(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<String> {
    attempt.raw_artifact_ids.iter().find_map(|id| {
        let envelope = cas.get_artifact(id).ok()?;
        envelope
            .artifact_type
            .to_ascii_lowercase()
            .contains("stderr")
            .then(|| id.clone())
    })
}

/// The cause of one failed node: the typed records first (the failure class, the Attempt's
/// retry feedback, the cause of its unknown usage), af's own words second, and the kind of work
/// the node does last.
fn node_cause(
    cas: &Cas,
    class: TaskFailureClassV1,
    kind: NodeKind,
    attempt: Option<&TaskAttemptAccounting>,
    attempt_error: Option<&str>,
    message: &str,
) -> TaskCauseV1 {
    let text = format!("{}\n{message}", attempt_error.unwrap_or_default());
    if let Some(attempt) = attempt {
        // The typed records of ADR-0253 first: an Attempt its wall ended, and a failure whose
        // diagnostic names the Provider's environment as its closed class.
        if wall_end(cas, attempt).is_some() {
            return TaskCauseV1::AttemptWall;
        }
        if diagnostic_cause(cas, attempt).as_deref() == Some("environment") {
            return TaskCauseV1::ProviderEnvironment;
        }
        match attempt.unknown_usage {
            Some(TaskUnknownUsageCauseV1::Capacity | TaskUnknownUsageCauseV1::RateLimit) => {
                return TaskCauseV1::ProviderCapacity;
            }
            Some(TaskUnknownUsageCauseV1::Network) => return TaskCauseV1::ProviderNetwork,
            Some(TaskUnknownUsageCauseV1::Authentication) => {
                return TaskCauseV1::IdentityUnverified;
            }
            Some(TaskUnknownUsageCauseV1::ModelUnavailable) => return TaskCauseV1::ModelRefused,
            Some(TaskUnknownUsageCauseV1::LeaseExpired) => return TaskCauseV1::WriterLost,
            Some(TaskUnknownUsageCauseV1::Interrupted) => return TaskCauseV1::HostCancelled,
            Some(TaskUnknownUsageCauseV1::Unreported) | None => (),
        }
        if matches!(attempt.result, Some(TaskAttemptResultV1::Abandoned { .. })) {
            return TaskCauseV1::WriterLost;
        }
        let code = match &attempt.result {
            Some(TaskAttemptResultV1::Failed {
                feedback_id: Some(id),
                ..
            }) => artifact::<TaskRetryFeedbackV1>(cas, id, TASK_RETRY_FEEDBACK_V1)
                .ok()
                .map(|feedback| feedback.code),
            _ => None,
        };
        // A node whose last Attempt failed ran out of retries because of that failure: the
        // Attempt's own failure is the cause, not the limit it then reached.
        if matches!(
            code,
            Some(
                TaskFeedbackCodeV1::InvalidOutputContract
                    | TaskFeedbackCodeV1::OutputAdmissionRejected
                    | TaskFeedbackCodeV1::CompilerRejected
            )
        ) {
            return TaskCauseV1::MalformedReply;
        }
        if let Some(cause) = attempt_error.and_then(TaskCauseV1::of_diagnostic) {
            return cause;
        }
        if code.is_some() && kind == NodeKind::Worker {
            return TaskCauseV1::MalformedReply;
        }
    }
    if class == TaskFailureClassV1::Resources {
        return if text.to_ascii_lowercase().contains("deadline") {
            TaskCauseV1::Deadline
        } else {
            TaskCauseV1::Budget
        };
    }
    match (TaskCauseV1::of_diagnostic(&text), kind) {
        (Some(cause), _) => cause,
        (None, NodeKind::Worker) => TaskCauseV1::MalformedReply,
        (None, NodeKind::Admission) => TaskCauseV1::IdentityUnverified,
        (None, NodeKind::Check) => TaskCauseV1::PreflightFailed,
        (None, NodeKind::Kernel) => TaskCauseV1::Budget,
    }
}

/// The last check receipt the Task's current execution recorded, in graph order, with every
/// check it names.
fn last_checks(cas: &Cas, state: &TaskProjection) -> Result<Option<ChecksView>, String> {
    let Some(execution) = &state.execution else {
        return Ok(None);
    };
    let mut found = None;
    for node in &execution.graph.order {
        // A gate whose retry is pending lost its published output to `TaskGateRetried@1`; until
        // the new check publishes, the receipt the retry superseded is still the gate's last.
        let retried = || {
            state
                .gate_retries
                .iter()
                .rev()
                .flat_map(|retry| &retry.gates)
                .find(|gate| gate.node == *node)
                .map(|gate| gate.output_id.clone())
        };
        let (output_id, output) = match execution.outputs.get(node) {
            Some((id, output)) => (id.clone(), output.clone()),
            None => match retried() {
                Some(id) => {
                    let output: review_core::task::execution::TaskOutputV1 =
                        artifact(cas, &id, review_core::task::execution::TASK_OUTPUT_V1)?;
                    (id, output)
                }
                None => continue,
            },
        };
        for port in output.outputs.values() {
            if port.artifact_type == TASK_CHECK_RECEIPT_V2
                && let Some(id) = port.artifact_ids.last()
            {
                found = Some((node.clone(), output_id.clone(), id.clone()));
            }
        }
    }
    let Some((node, output_id, receipt_id)) = found else {
        return Ok(None);
    };
    let receipt: TaskCheckReceiptV2 = artifact(cas, &receipt_id, TASK_CHECK_RECEIPT_V2)?;
    let attempt = execution.attempt_accounting().into_iter().find(|attempt| {
        matches!(&attempt.result, Some(TaskAttemptResultV1::Succeeded { output_id: id }) if *id == output_id)
    });
    let layers = check_layers(cas, &receipt_id, attempt.as_ref());
    let mut checks = Vec::new();
    for (name, result_id) in &receipt.checks {
        let mut view = check_view(cas, name, result_id, attempt.as_ref())?;
        view.layer = layers.get(name).cloned();
        view.stop = receipt.stopped.get(name).cloned();
        checks.push(view);
    }
    Ok(Some(ChecksView {
        node,
        receipt_id,
        outcome: serde_json::to_value(receipt.outcome)
            .map_err(|e| e.to_string())?
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        checks,
    }))
}

/// The `af/TaskCheckLayer@1` entries of a receipt, by check, from the Attempt that recorded
/// it. A record that does not decode or validate is skipped, as it was never admitted.
fn check_layers(
    cas: &Cas,
    receipt_id: &str,
    attempt: Option<&TaskAttemptAccounting>,
) -> BTreeMap<String, review_core::task::check_layer::TaskCheckLayerRunV1> {
    use review_core::task::check_layer::{TASK_CHECK_LAYER_V1, TaskCheckLayerV1};
    attempt
        .iter()
        .flat_map(|attempt| attempt.raw_artifact_ids.iter())
        .filter_map(|id| cas.get_optional_artifact(id).ok().flatten())
        .filter(|artifact| artifact.artifact_type == TASK_CHECK_LAYER_V1)
        .filter_map(|artifact| serde_json::from_value::<TaskCheckLayerV1>(artifact.payload).ok())
        .filter(|layer| layer.receipt_id == receipt_id && layer.validate().is_ok())
        .flat_map(|layer| layer.checks)
        .collect()
}

/// The lines `af task show --checks` prints under a check for its warm layer.
fn layer_lines(layer: &review_core::task::check_layer::TaskCheckLayerRunV1) -> Vec<String> {
    use review_core::task::check_layer::{TaskLayerLockV1, TaskLayerStartV1};
    let key = layer.key.as_deref().map_or_else(
        || "no key".to_owned(),
        |key| match key.split_once('/') {
            Some((project, toolchain)) => format!("{}/{}", &project[..12], &toolchain[..12]),
            None => key.to_owned(),
        },
    );
    let start = match layer.start {
        TaskLayerStartV1::Warm => "warm",
        TaskLayerStartV1::Cold => "cold",
    };
    let holder = layer.holder.as_ref().map_or_else(String::new, |holder| {
        format!(
            " for af task run {} on {}",
            holder.pid,
            preview::text(&holder.host)
        )
    });
    let lock = match layer.lock {
        TaskLayerLockV1::Held => "held at once".to_owned(),
        TaskLayerLockV1::Waited => format!("waited {} s{holder}", layer.waited_ms / 1000),
        TaskLayerLockV1::Cloned => format!("busy{holder}, built in a private copy"),
        TaskLayerLockV1::Busy => format!("busy{holder} for the whole wait, built cold"),
        TaskLayerLockV1::Unavailable => "unavailable, built cold".to_owned(),
        TaskLayerLockV1::Unresolved => "toolchain unresolved, built cold".to_owned(),
    };
    let phases = &layer.phases;
    let mut lines = vec![
        format!(
            "  layer {key}: {start} at start, {lock}; source groups {} stale, {} fresh",
            layer.sources.stale, layer.sources.fresh
        ),
        match (phases.build_ms, phases.tests_ms, phases.build_wall_ms) {
            (Some(build), Some(tests), Some(build_wall)) => format!(
                "  phases: build {build} ms of {build_wall} ms, tests {tests} ms of {} ms",
                phases.check_wall_ms
            ),
            (Some(build), Some(tests), None) => format!(
                "  phases: build {build} ms, tests {tests} ms, together of {} ms (no \
                 [warm] build_wall_ms)",
                phases.check_wall_ms
            ),
            (_, _, Some(build_wall)) => format!(
                "  phases: no build step reported, {} ms in one (build {build_wall} ms + tests \
                 {} ms)",
                phases.elapsed_ms, phases.check_wall_ms
            ),
            (_, _, None) => format!(
                "  phases: no build step reported, {} ms of {} ms",
                phases.elapsed_ms, phases.check_wall_ms
            ),
        },
    ];
    if let Some(phase) = phases.deadline {
        lines.push(format!("  ended by the {} deadline", phase.as_str()));
    }
    if let Some(evicted) = &layer.evicted {
        lines.push(format!(
            "  {}",
            evicted.sentence(&local_time(evicted.at_unix_ms))
        ));
    }
    lines
}

fn check_view(
    cas: &Cas,
    name: &str,
    result_id: &str,
    attempt: Option<&TaskAttemptAccounting>,
) -> Result<CheckView, String> {
    let result = cas.get_json(result_id).map_err(|e| e.to_string())?;
    let text = |key: &str| -> Result<(Option<String>, String), String> {
        match result[key].as_str() {
            Some(id) => {
                let bytes = cas
                    .get_bounded(id, 64 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
                Ok((
                    Some(id.to_owned()),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ))
            }
            None => Ok((None, String::new())),
        }
    };
    let (stdout_id, stdout) = text("stdout")?;
    let (stderr_id, stderr) = text("stderr")?;
    let both = format!("{stdout}\n{stderr}");
    let (failed_tests, flaky) = failing_tests(&both);
    let stopped_step = stopped_step(&both).map(|(step, exit_code)| StoppedStep { step, exit_code });
    let lines: Vec<&str> = stderr.lines().collect();
    let stderr_tail = lines[lines.len().saturating_sub(STDERR_TAIL_LINES)..]
        .iter()
        .map(|line| preview::text(line))
        .collect();
    let (elapsed_ms, cache) = check_runtime(cas, name, attempt);
    Ok(CheckView {
        name: name.to_owned(),
        status: result["status"].as_str().unwrap_or("unknown").to_owned(),
        exit_code: result["exit_code"].as_i64(),
        reason: result["reason"].as_str().map(preview::text),
        elapsed_ms,
        cache,
        remote: result["remote"].is_string(),
        stop: None,
        stopped_step,
        failed_tests,
        flaky,
        stdout_id,
        stderr_id,
        stderr_tail,
        layer: None,
    })
}

/// A check's recorded span and its warm directories, from its Attempt's runtime evidence.
fn check_runtime(
    cas: &Cas,
    name: &str,
    attempt: Option<&TaskAttemptAccounting>,
) -> (Option<u64>, String) {
    let mut elapsed = None;
    let mut caches = Vec::new();
    for id in attempt.iter().flat_map(|a| a.raw_artifact_ids.iter()) {
        let Ok(raw) = cas.get_artifact(id) else {
            continue;
        };
        if raw.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
            continue;
        }
        let Ok(evidence) = serde_json::from_value::<TaskRuntimeEvidenceV1>(raw.payload) else {
            continue;
        };
        let spans = evidence
            .spans
            .iter()
            .filter(|span| span.kind == TaskRuntimeSpanKindV1::Check && span.label == name)
            .collect::<Vec<_>>();
        if let Some(span) = spans.first() {
            elapsed = Some(span.elapsed_ms);
        }
        let named = evidence
            .check
            .as_ref()
            .is_some_and(|check| check.name == name);
        if named || (!spans.is_empty() && !evidence.caches.is_empty()) {
            caches.extend(
                evidence
                    .caches
                    .iter()
                    .filter_map(|cache| serde_json::to_value(cache).ok()),
            );
        }
    }
    let cache = if caches.is_empty() {
        "cold (no warm cache)".to_owned()
    } else {
        cache_states(&caches)
    };
    (elapsed, cache)
}

/// The one-line detail `af task show` prints after a failed Task's state: `check kernel, 2
/// tests: a::b, c::d`, or a failed node and its first error line.
pub(crate) fn summary(view: &TaskView) -> Option<String> {
    summary_with(view, true)
}

/// The same line as `af task report` prints it in a pull request: a failed node is named with
/// its cause, never with its diagnostic, which may quote a Provider or a Worker.
pub(crate) fn report_summary(view: &TaskView) -> Option<String> {
    summary_with(view, false)
}

fn summary_with(view: &TaskView, diagnostics: bool) -> Option<String> {
    match view.state.outcome {
        None | Some(TaskOutcomeV1::Verified) => None,
        Some(TaskOutcomeV1::GateFailed) => {
            let checks = view.checks.as_ref()?;
            let failed = checks
                .checks
                .iter()
                .filter(|c| c.status != "passed")
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>();
            // A check stopped at its wall says so first, with its wall and how long it ran
            // (ADR-0255), and what to do about it.
            let walled = checks
                .checks
                .iter()
                .filter(|c| c.status != "passed")
                .filter_map(|c| {
                    c.stop
                        .as_ref()
                        .map(|stop| format!("check {} {}", c.name, wall_words(stop)))
                })
                .collect::<Vec<_>>();
            let gate = view.gate.as_ref()?;
            if !walled.is_empty() {
                let mut text = walled.join("; ");
                let others = checks
                    .checks
                    .iter()
                    .filter(|c| c.status != "passed" && c.stop.is_none())
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>();
                if !others.is_empty() {
                    text.push_str(&format!("; also not passed: {}", others.join(", ")));
                }
                if !gate.failed_tests.is_empty() {
                    text.push_str(&format!(
                        "; failing tests: {}",
                        tests_list(&gate.failed_tests, 5)
                    ));
                }
                text.push_str(&format!("; {WALL_NEXT}"));
                return Some(text);
            }
            let mut text = format!(
                "{} {}",
                if failed.len() == 1 { "check" } else { "checks" },
                failed.join(", ")
            );
            if !gate.failed_tests.is_empty() {
                text.push_str(&format!(
                    ", {} {}: {}",
                    gate.failed_tests.len(),
                    if gate.failed_tests.len() == 1 {
                        "test"
                    } else {
                        "tests"
                    },
                    tests_list(&gate.failed_tests, 5)
                ));
            } else if let Some(step) = checks.checks.iter().find_map(|c| c.stopped_step.as_ref()) {
                text.push_str(&format!(
                    ", stopped at {} (exit {})",
                    step.step, step.exit_code
                ));
            } else if let Some(reason) = checks.checks.iter().find_map(|c| c.reason.as_deref()) {
                text.push_str(&format!(": {reason}"));
            }
            Some(text)
        }
        Some(_) => view.failures.first().map(|failure| {
            if diagnostics {
                format!("{}: {}", failure.node, failure.first_line)
            } else if let Some(wall) = &failure.wall {
                // af's own words, never a Worker's: the wall and the time it took (ADR-0253).
                format!("{}: {wall}", failure.node)
            } else {
                format!("{} failed", failure.node)
            }
        }),
    }
}

/// A duration as a person reads it, to the nearest second: `45 s`, `12 min`, `2 h 59 min`.
pub(crate) fn duration_words(ms: u64) -> String {
    let seconds = ms.saturating_add(500) / 1_000;
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        _ => match seconds % 3_600 / 60 {
            0 => format!("{} h", seconds / 3_600),
            minutes => format!("{} h {minutes} min", seconds / 3_600),
        },
    }
}

/// What a person does about a check stopped at its wall (ADR-0255).
pub(crate) const WALL_NEXT: &str = "make the check faster, or raise its wall in \
     .af/code-policy.toml (check_wall_ms, check_process_wall_ms or [warm] build_wall_ms) and plan \
     the Task again";

/// `stopped at its 90 min wall after 88 min`: a check the kernel stopped at its wall, in whole
/// minutes, or in seconds below one (ADR-0255).
pub(crate) fn wall_words(stop: &TaskCheckStopV2) -> String {
    let minutes = |ms: u64| {
        if ms < 59_500 {
            format!("{} s", ms.saturating_add(500) / 1_000)
        } else {
            format!("{} min", ms.saturating_add(30_000) / 60_000)
        }
    };
    format!(
        "stopped at its {} wall after {}",
        minutes(stop.wall_ms),
        minutes(stop.elapsed_ms)
    )
}

/// A Unix time as this machine's local wall clock: `17:41:18 +03`, with the date before it
/// when it is not today (`2026-10-24 09:00:00 +03`). The UTC offset is the system's own, as
/// `date` reports it under `TZ`; without one the time is printed in UTC (`17:41:18 Z`).
pub(crate) fn local_time(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1_000) as i64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let offset = utc_offset(seconds);
    let civil = |at: i64| {
        let local = at + offset.unwrap_or(0);
        let days = local.div_euclid(86_400);
        let of_day = local.rem_euclid(86_400);
        (civil_from_days(days), of_day)
    };
    let ((year, month, day), of_day) = civil(seconds);
    let (today, _) = civil(now);
    let zone = match offset {
        None => "Z".to_owned(),
        Some(offset) => {
            let sign = if offset < 0 { '-' } else { '+' };
            match offset.abs() % 3_600 / 60 {
                0 => format!("{sign}{:02}", offset.abs() / 3_600),
                minutes => format!("{sign}{:02}:{minutes:02}", offset.abs() / 3_600),
            }
        }
    };
    let clock = format!(
        "{:02}:{:02}:{:02} {zone}",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60
    );
    if (year, month, day) == today {
        clock
    } else {
        format!("{year:04}-{month:02}-{day:02} {clock}")
    }
}

/// The system's UTC offset in seconds at `seconds`, from `date +%z`: GNU `date -d @N`, then BSD
/// `date -r N`. `None` when neither answers.
fn utc_offset(seconds: i64) -> Option<i64> {
    let at = seconds.to_string();
    let gnu = format!("@{at}");
    [["-d", gnu.as_str()], ["-r", at.as_str()]]
        .into_iter()
        .find_map(|args| {
            // Supervised like every child af reaps (ADR-0026), and bounded: a display only.
            let output = ["/bin/date", "/usr/bin/date"]
                .into_iter()
                .find_map(|program| {
                    let mut command = std::process::Command::new(program);
                    command.args(args).arg("+%z");
                    review_process::run_supervised(
                        &mut command,
                        None,
                        std::time::Duration::from_secs(2),
                    )
                    .ok()
                    .filter(|output| output.status.success())
                })?;
            let text = String::from_utf8(output.stdout).ok()?;
            let text = text.trim();
            let (sign, digits) = match text.as_bytes().first()? {
                b'+' => (1, &text[1..]),
                b'-' => (-1, &text[1..]),
                _ => return None,
            };
            if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = digits[2..].parse().ok()?;
            Some(sign * (hours * 3_600 + minutes * 60))
        })
}

/// Days since 1970-01-01 as a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// `deadline 17:41:18 +03 (2 h 59 min from now)`: the line `af task run` prints on stderr.
pub(crate) fn deadline_line(deadline_unix_ms: u64, now: u64) -> String {
    match deadline_unix_ms.checked_sub(now).filter(|left| *left > 0) {
        Some(left) => format!(
            "deadline {} ({} from now)",
            local_time(deadline_unix_ms),
            duration_words(left)
        ),
        None => format!("deadline {} (passed)", local_time(deadline_unix_ms)),
    }
}

/// At most `max` test names joined by `, `, then `… and N more`.
pub(crate) fn tests_list(names: &[String], max: usize) -> String {
    let shown = names
        .iter()
        .take(max)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > max {
        format!("{shown} … and {} more", names.len() - max)
    } else {
        shown
    }
}

/// The `af/task-checks@3` document of `af task show ID --checks --json`.
pub(crate) fn checks_document(task_id: &str, view: &TaskView) -> serde_json::Value {
    json!({
        "schema": TASK_CHECKS_V3,
        "task_id": task_id,
        "state": view.state,
        "gate": view.gate,
        "receipt": view.checks,
    })
}

/// The text of `af task show ID --checks`.
pub(crate) fn checks_lines(task_id: &str, view: &TaskView) -> Vec<String> {
    let mut lines = vec![format!(
        "Task {}: {}",
        preview::text(task_id),
        view.state.label()
    )];
    let Some(checks) = &view.checks else {
        lines.push("no check receipt recorded yet".into());
        return lines;
    };
    lines.push(format!(
        "receipt {} from {}: {}",
        checks.receipt_id, checks.node, checks.outcome
    ));
    for check in &checks.checks {
        let mut head = format!("check {}: {}", preview::text(&check.name), check.status);
        if let Some(code) = check.exit_code {
            head.push_str(&format!(" (exit {code})"));
        }
        match (&check.stop, check.elapsed_ms) {
            (Some(stop), _) => head.push_str(&format!(", {}", wall_words(stop))),
            (None, Some(ms)) => head.push_str(&format!(" in {ms} ms")),
            (None, None) if check.remote => head.push_str(", run remotely"),
            (None, None) => head.push_str(", never started"),
        }
        head.push_str(&format!(", {}", check.cache));
        lines.push(head);
        if check.stop.is_some() {
            lines.push(format!("  next: {WALL_NEXT}"));
        }
        if let Some(reason) = &check.reason {
            lines.push(format!("  reason: {reason}"));
        }
        if let Some(layer) = &check.layer {
            lines.extend(layer_lines(layer));
        }
        if let Some(step) = &check.stopped_step {
            lines.push(format!(
                "  stopped at step {} (exit {})",
                preview::text(&step.step),
                step.exit_code
            ));
        }
        if !check.failed_tests.is_empty() {
            lines.push(format!("  failing tests ({}):", check.failed_tests.len()));
            for name in &check.failed_tests {
                lines.push(format!("    {}", preview::text(name)));
            }
        }
        if !check.flaky.is_empty() {
            lines.push(format!(
                "  flaky: {}",
                check
                    .flaky
                    .iter()
                    .map(|n| preview::text(n))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(id) = &check.stderr_id {
            lines.push(format!(
                "  stderr {id}, last {} lines:",
                check.stderr_tail.len()
            ));
            for line in &check.stderr_tail {
                lines.push(format!("    {line}"));
            }
        }
    }
    lines.push(format!(
        "next: {}",
        view.state.next_action.as_deref().unwrap_or("none")
    ));
    lines
}

/// `check kernel stopped at its 90 min wall after 88 min; make the check faster, …`: the line
/// every rendering prints for a check the kernel stopped at its wall (ADR-0255).
fn wall_line(check: &CheckView) -> Option<String> {
    check
        .stop
        .as_ref()
        .filter(|_| check.status != "passed")
        .map(|stop| {
            format!(
                "check {} {}; {WALL_NEXT}",
                preview::text(&check.name),
                wall_words(stop)
            )
        })
}

/// The line of each check of the gate's last receipt the kernel stopped at its wall: what a
/// run prints on stderr in every format (ADR-0255).
pub(crate) fn wall_lines(view: &TaskView) -> Vec<String> {
    view.checks
        .iter()
        .flat_map(|checks| &checks.checks)
        .filter_map(wall_line)
        .collect()
}

/// The lines `af task run` adds on stderr after a run whose gate failed: the failing tests, at
/// most 20, then where the rest are.
pub(crate) fn failed_gate_tail(task_id: &str, view: &TaskView) -> Vec<String> {
    let Some(gate) = view
        .gate
        .as_ref()
        .filter(|gate| gate.status != TaskGateStatusV1::Passed)
    else {
        return Vec::new();
    };
    let Some(checks) = &view.checks else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for check in checks.checks.iter().filter(|c| c.status != "passed") {
        if let Some(line) = wall_line(check) {
            lines.push(line);
            continue;
        }
        let detail = match (&check.stopped_step, check.failed_tests.len()) {
            (_, n) if n > 0 => format!("{n} failing {}", if n == 1 { "test" } else { "tests" }),
            (Some(step), _) => format!("stopped at step {} (exit {})", step.step, step.exit_code),
            (None, _) => check.reason.clone().unwrap_or_else(|| check.status.clone()),
        };
        lines.push(format!(
            "check {}: {}: {detail}",
            preview::text(&check.name),
            check.status
        ));
    }
    for name in gate.failed_tests.iter().take(20) {
        lines.push(format!("  FAIL {}", preview::text(name)));
    }
    for check in checks.checks.iter().filter(|c| c.status != "passed") {
        if let Some(id) = &check.stderr_id {
            lines.push(format!(
                "  stderr of check {}: {id}",
                preview::text(&check.name)
            ));
        }
    }
    if gate.failed_tests.len() > 20 {
        lines.push(format!(
            "  … and {} more; af task show {task_id} --checks",
            gate.failed_tests.len() - 20
        ));
    }
    if !gate.flaky.is_empty() {
        lines.push(format!("  flaky: {}", tests_list(&gate.flaky, 20)));
    }
    lines
}

/// The lines `af task show` prints for each failed node: its cause, its first error line and
/// where its whole diagnostic and stderr are.
pub(crate) fn failure_lines(view: &TaskView) -> Vec<String> {
    let mut lines = Vec::new();
    for failure in &view.failures {
        let cause = failure
            .cause
            .map_or("follows from the failure above", TaskCauseV1::as_str);
        lines.push(format!(
            "{}: {cause}: {}",
            failure.node,
            preview::text(&failure.first_line)
        ));
        let mut detail = format!("  diagnostic {}", failure.diagnostic_id);
        if let Some(stderr) = &failure.stderr_id {
            detail.push_str(&format!(", stderr {stderr}"));
        }
        lines.push(detail);
    }
    lines
}

/// The node's failure as `--json` documents carry it.
pub(crate) fn failures_value(view: &TaskView) -> serde_json::Value {
    json!(view.failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: &str, tests: &[&str], flaky: &[&str], step: Option<&str>) -> CheckView {
        CheckView {
            name: "kernel".into(),
            status: status.into(),
            exit_code: Some(1),
            reason: None,
            elapsed_ms: Some(10),
            cache: "cold (no warm cache)".into(),
            remote: false,
            stop: None,
            stopped_step: step.map(|s| StoppedStep {
                step: s.into(),
                exit_code: 2,
            }),
            failed_tests: tests.iter().map(|t| t.to_string()).collect(),
            flaky: flaky.iter().map(|t| t.to_string()).collect(),
            stdout_id: None,
            stderr_id: None,
            stderr_tail: vec![],
            layer: None,
        }
    }

    fn view(checks: Vec<CheckView>) -> ChecksView {
        ChecksView {
            node: "root.nodes.check".into(),
            receipt_id: format!("sha256:{}", "1".repeat(64)),
            outcome: "failed".into(),
            checks,
        }
    }

    /// `af task show --checks` under a warm layer (ADR-0207): the key, how the check came by
    /// it, warm or cold, the source groups, both phases against their deadlines, the deadline
    /// that ended it and an eviction with its cause.
    #[test]
    fn the_checks_text_names_the_layer_its_lock_its_phases_and_its_eviction() {
        use review_core::task::check_layer::*;
        let key = format!("{}/{}", "a".repeat(64), "b".repeat(64));
        let mut kernel = check("not_run", &[], &[], None);
        kernel.exit_code = None;
        kernel.reason = Some("layer evicted during build by storage sweep at 12:00:00 Z".into());
        kernel.layer = Some(TaskCheckLayerRunV1 {
            key: Some(key.clone()),
            lock: TaskLayerLockV1::Waited,
            waited_ms: 61_400,
            holder: Some(TaskLayerHolderV1 {
                pid: 4242,
                host: "claw-3".into(),
                since_unix_ms: 1,
            }),
            start: TaskLayerStartV1::Warm,
            sources: TaskLayerSourcesV1 {
                stale: 2,
                fresh: 15,
            },
            phases: TaskLayerPhasesV1 {
                build_wall_ms: Some(2_400_000),
                check_wall_ms: 3_600_000,
                build_ms: Some(812_000),
                tests_ms: Some(3_600_000),
                elapsed_ms: 4_412_000,
                deadline: Some(TaskLayerPhaseV1::Tests),
            },
            evicted: None,
        });
        let mut lint = check("passed", &[], &[], None);
        lint.name = "markdownlint".into();
        lint.exit_code = Some(0);
        lint.layer = Some(TaskCheckLayerRunV1 {
            key: Some(key),
            lock: TaskLayerLockV1::Held,
            waited_ms: 3,
            holder: None,
            start: TaskLayerStartV1::Cold,
            sources: TaskLayerSourcesV1 {
                stale: 17,
                fresh: 0,
            },
            phases: TaskLayerPhasesV1 {
                build_wall_ms: Some(2_400_000),
                check_wall_ms: 3_600_000,
                build_ms: None,
                tests_ms: None,
                elapsed_ms: 9_000,
                deadline: None,
            },
            evicted: None,
        });
        let state = TaskStateV1::new(TaskStatePhaseV1::Finished);
        let task = TaskView {
            state,
            gate: None,
            checks: Some(view(vec![kernel, lint])),
            failures: vec![],
        };
        let lines = checks_lines("t", &task);
        assert_eq!(
            lines[2..9],
            [
                "check kernel: not_run in 10 ms, cold (no warm cache)",
                "  reason: layer evicted during build by storage sweep at 12:00:00 Z",
                "  layer aaaaaaaaaaaa/bbbbbbbbbbbb: warm at start, waited 61 s for af task run \
                 4242 on claw-3; source groups 2 stale, 15 fresh",
                "  phases: build 812000 ms of 2400000 ms, tests 3600000 ms of 3600000 ms",
                "  ended by the tests deadline",
                "check markdownlint: passed (exit 0) in 10 ms, cold (no warm cache)",
                "  layer aaaaaaaaaaaa/bbbbbbbbbbbb: cold at start, held at once; source groups \
                 17 stale, 0 fresh",
            ]
        );
        assert_eq!(
            lines[9],
            "  phases: no build step reported, 9000 ms in one (build 2400000 ms + tests 3600000 ms)"
        );
        let document = checks_document("t", &task);
        assert_eq!(document["schema"], "af/task-checks@3");
        assert_eq!(document["receipt"]["checks"][0]["layer"]["lock"], "waited");
        assert_eq!(
            document["receipt"]["checks"][0]["layer"]["phases"]["deadline"],
            "tests"
        );
        assert!(document["receipt"]["checks"][1]["layer"]["phases"]["build_ms"].is_null());
    }

    /// A check the kernel stopped at its wall (ADR-0255) is `check_failed`, and every rendering
    /// names its wall, how long it ran and what to do; a check that never started still says so
    /// and nothing else does, and alone it is still `preflight_failed`.
    #[test]
    fn a_check_stopped_at_its_wall_is_named_so_and_never_as_never_started() {
        use review_core::task::verification::TaskCheckStopCauseV2;
        let mut kernel = check("not_run", &[], &[], None);
        kernel.exit_code = None;
        kernel.reason = Some("no result within 5400s; the check was killed".into());
        kernel.elapsed_ms = Some(5_280_000);
        kernel.stop = Some(TaskCheckStopV2 {
            cause: TaskCheckStopCauseV2::Wall,
            wall_ms: 5_400_000,
            elapsed_ms: 5_280_000,
        });
        let mut unstarted = check("not_run", &[], &[], None);
        unstarted.name = "markdownlint".into();
        unstarted.exit_code = None;
        unstarted.elapsed_ms = None;
        unstarted.reason = Some("Task check deadline expired".into());
        let walled = view(vec![kernel.clone(), unstarted.clone()]);
        let gate = gate_of(&walled);
        assert_eq!(gate.status, TaskGateStatusV1::NotRun);
        assert_eq!(gate_cause(&walled, &gate), TaskCauseV1::CheckFailed);
        let wall = "check kernel stopped at its 90 min wall after 88 min";
        let task = TaskView {
            state: TaskStateV1::ended(
                TaskStatePhaseV1::Finished,
                TaskOutcomeV1::GateFailed,
                Some(TaskCauseV1::CheckFailed),
            )
            .with_next("af task run t --retry-gate"),
            gate: Some(gate.clone()),
            checks: Some(walled),
            failures: vec![],
        };
        let lines = checks_lines("t", &task);
        assert_eq!(
            lines[2..6],
            [
                "check kernel: not_run, stopped at its 90 min wall after 88 min, cold (no warm \
                 cache)",
                &format!("  next: {WALL_NEXT}"),
                "  reason: no result within 5400s; the check was killed",
                "check markdownlint: not_run, never started, cold (no warm cache)",
            ]
        );
        let line = format!("{wall}; {WALL_NEXT}");
        assert_eq!(wall_lines(&task), [line.clone()]);
        let tail = failed_gate_tail("t", &task);
        assert_eq!(tail[0], line);
        assert_eq!(
            tail[1],
            "check markdownlint: not_run: Task check deadline expired"
        );
        let summary = format!("{wall}; also not passed: markdownlint; {WALL_NEXT}");
        assert_eq!(summary_with(&task, true).unwrap(), summary);
        assert_eq!(report_summary(&task).unwrap(), summary);
        let document = checks_document("t", &task);
        assert_eq!(
            document["receipt"]["checks"][0]["stop"],
            json!({"cause": "wall", "wall_ms": 5_400_000, "elapsed_ms": 5_280_000})
        );
        assert!(document["receipt"]["checks"][1].get("stop").is_none());

        // Without the stopped check, the one that never started reads as before.
        let alone = view(vec![unstarted]);
        let gate = gate_of(&alone);
        assert_eq!(gate_cause(&alone, &gate), TaskCauseV1::PreflightFailed);
        let task = TaskView {
            checks: Some(alone),
            gate: Some(gate),
            ..task
        };
        assert!(wall_lines(&task).is_empty());
        assert!(
            checks_lines("t", &task)
                .iter()
                .all(|line| !line.contains("wall"))
        );
        // Seconds below a minute: a short test wall reads as one.
        let short = TaskCheckStopV2 {
            cause: TaskCheckStopCauseV2::Wall,
            wall_ms: 2_000,
            elapsed_ms: 2_049,
        };
        assert_eq!(wall_words(&short), "stopped at its 2 s wall after 2 s");
    }

    #[test]
    fn a_gate_names_its_failing_tests_and_whether_its_tests_ran() {
        let cause = |checks: ChecksView| (gate_of(&checks), gate_cause(&checks, &gate_of(&checks)));
        let (failed, of) = cause(view(vec![check(
            "failed",
            &["a::b", "c::d"],
            &[],
            Some("test"),
        )]));
        assert_eq!(failed.status, TaskGateStatusV1::Failed);
        assert_eq!(failed.failed_tests, ["a::b", "c::d"]);
        assert_eq!(of, TaskCauseV1::CheckFailed);
        let (_, of) = cause(view(vec![check("failed", &["a::b"], &["e::f"], None)]));
        assert_eq!(of, TaskCauseV1::CheckFlakyThenFailed);
        let (lint, of) = cause(view(vec![check("failed", &[], &[], Some("lint"))]));
        assert_eq!(lint.status, TaskGateStatusV1::Failed);
        assert_eq!(of, TaskCauseV1::PreflightFailed);
        let (never, of) = cause(view(vec![check("not_run", &[], &[], None)]));
        assert_eq!(never.status, TaskGateStatusV1::NotRun);
        assert_eq!(of, TaskCauseV1::PreflightFailed);
        let (passed, _) = cause(view(vec![check("passed", &[], &[], None)]));
        assert_eq!(passed.status, TaskGateStatusV1::Passed);
    }

    /// A failed gate names `--retry-gate` only when the kernel would run it again; one it
    /// refuses (an evaluator ran after it, say) names its check receipt instead, never a command
    /// the kernel then refuses.
    #[test]
    fn a_failed_gate_names_retry_only_when_the_kernel_retries_it() {
        assert_eq!(
            gate_next_action("t", &GateRetry::Gates(vec!["root.nodes.check".into()])),
            "af task run t --retry-gate"
        );
        assert_eq!(
            gate_next_action(
                "t",
                &GateRetry::Refused("a reviewer or evaluator ran after its failed gate".into())
            ),
            "af task show t --checks"
        );
    }

    #[test]
    fn durations_and_deadlines_read_as_a_person_says_them() {
        assert_eq!(duration_words(45_000), "45 s");
        assert_eq!(duration_words(59_700), "1 min");
        assert_eq!(duration_words(12 * 60_000 + 5_000), "12 min");
        assert_eq!(duration_words(3 * 3_600_000 - 60_000), "2 h 59 min");
        assert_eq!(duration_words(2 * 3_600_000), "2 h");
        let now = 1_800_000_000_000;
        let line = deadline_line(now + 3 * 3_600_000 - 60_000, now);
        assert!(line.starts_with("deadline "), "{line}");
        assert!(line.ends_with("(2 h 59 min from now)"), "{line}");
        assert!(deadline_line(now - 1, now).ends_with("(passed)"));
        // A local clock time with its UTC offset (or `Z`), never a raw Unix millisecond count.
        let shown = local_time(now);
        assert!(
            shown.contains(':')
                && (shown.contains(" +") || shown.contains(" -") || shown.ends_with(" Z")),
            "{shown}"
        );
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_749), (2026, 10, 23));
    }

    /// A node that ran out of a resource names which: its deadline, or its budget; an Attempt's
    /// own failure decides before the limit it then reached.
    #[test]
    fn a_resource_failure_names_the_deadline_or_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let cause = |class, kind, message: &str| node_cause(&cas, class, kind, None, None, message);
        assert_eq!(
            cause(
                TaskFailureClassV1::Resources,
                NodeKind::Worker,
                "Task deadline protects still-required verification"
            ),
            TaskCauseV1::Deadline
        );
        assert_eq!(
            cause(
                TaskFailureClassV1::Resources,
                NodeKind::Kernel,
                "Task token limit protects still-required verification"
            ),
            TaskCauseV1::Budget
        );
        assert_eq!(
            cause(
                TaskFailureClassV1::Execution,
                NodeKind::Check,
                "check runner crashed"
            ),
            TaskCauseV1::PreflightFailed
        );
        assert_eq!(
            cause(
                TaskFailureClassV1::Execution,
                NodeKind::Admission,
                "no answer"
            ),
            TaskCauseV1::IdentityUnverified
        );
        assert_eq!(
            cause(
                TaskFailureClassV1::Execution,
                NodeKind::Worker,
                "Worker timed out after 300s"
            ),
            TaskCauseV1::ProviderTimeout
        );
        assert_eq!(
            TaskCauseV1::Deadline.outcome(),
            Some(TaskOutcomeV1::Expired),
            "an expired Task exits 7"
        );
        assert_eq!(
            TaskCauseV1::Budget.outcome(),
            Some(TaskOutcomeV1::Exhausted)
        );
    }

    /// ADR-0253: an Attempt its wall ended reads `attempt_wall`, and a failure whose diagnostic
    /// names the Provider's environment reads `provider_environment`, whatever their words say;
    /// an Attempt without either record is classified as before.
    #[test]
    fn typed_wall_ends_and_environment_failures_name_their_own_cause() {
        use review_core::task::attempt_wall::*;
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let producer = review_core::Producer::KernelOperation {
            run_id: "state-test".into(),
            node_id: None,
            operation_id: "fixture@1".into(),
        };
        let end = TaskAttemptWallEndV1 {
            schema: TASK_ATTEMPT_WALL_END_SCHEMA.into(),
            task_id: "t".into(),
            node: "root.nodes.implement".into(),
            attempt_id: "01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            wall_ms: 2_000,
            elapsed_ms: 2_100,
            partial_candidate_id: None,
        };
        let (end_id, _) = cas
            .put_artifact(
                TASK_ATTEMPT_WALL_END_V1,
                producer,
                vec![],
                None,
                serde_json::to_value(&end).unwrap(),
            )
            .unwrap();
        let attempt = |diagnostic: serde_json::Value, raw: Vec<String>| TaskAttemptAccounting {
            attempt_id: end.attempt_id.clone(),
            invocation_id: end_id.clone(),
            plan_id: end_id.clone(),
            reservation: review_attempt::task_budget::TaskReservation {
                id: "reservation:0".into(),
                node: end.node.clone(),
                tokens: 10,
                deadline_unix_ms: 1,
            },
            started: true,
            started_unix_ms: Some(1),
            settled_unix_ms: Some(2),
            released: false,
            charged_tokens: 0,
            state: None,
            result: Some(TaskAttemptResultV1::Failed {
                diagnostic_id: cas.put_json(&diagnostic).unwrap(),
                feedback_id: None,
            }),
            context_id: None,
            raw_artifact_ids: raw,
            usage_id: None,
            unknown_usage: Some(TaskUnknownUsageCauseV1::Unreported),
            abandoned: None,
        };
        let cause = |attempt: &TaskAttemptAccounting| {
            let error = attempt_error(&cas, attempt);
            node_cause(
                &cas,
                TaskFailureClassV1::Resources,
                NodeKind::Worker,
                Some(attempt),
                error.as_deref(),
                "Task node root.nodes.implement exhausted its Attempt limit",
            )
        };
        let walled = attempt(
            serde_json::json!({"schema":"af.task-diagnostic/1",
                "error":"implementer did not answer within 2000ms and was killed",
                "cause":"attempt_wall"}),
            vec![end_id.clone()],
        );
        assert_eq!(cause(&walled), TaskCauseV1::AttemptWall);
        assert_eq!(wall_end(&cas, &walled).unwrap().1, end);
        let environment = attempt(
            serde_json::json!({"schema":"af.task-diagnostic/1",
                "error":"identity recheck: probe timed out 3×; the account was not verified",
                "cause":"environment"}),
            vec![],
        );
        assert_eq!(cause(&environment), TaskCauseV1::ProviderEnvironment);
        let changed = attempt(
            serde_json::json!({"schema":"af.task-diagnostic/1",
                "error":"identity recheck: the Claude account differs from the one captured at \
                         plan: run `af task plan` again"}),
            vec![],
        );
        assert_eq!(cause(&changed), TaskCauseV1::IdentityChanged);
    }

    #[test]
    fn a_collected_task_keeps_its_retained_outcome_and_names_its_collection() {
        for (retained, outcome) in [
            ("verified", TaskOutcomeV1::Verified),
            ("pass", TaskOutcomeV1::Verified),
            ("changes_requested", TaskOutcomeV1::ChangesRequested),
            ("incomplete", TaskOutcomeV1::Exhausted),
        ] {
            let state = collected(retained);
            state.validate().unwrap();
            assert_eq!(state.outcome, Some(outcome));
            assert_eq!(state.cause, Some(TaskCauseV1::Collected));
            assert!(state.next_action.is_none());
        }
    }

    #[test]
    fn a_long_test_list_is_cut_with_a_count() {
        let names: Vec<String> = (0..7).map(|n| format!("t{n}")).collect();
        assert_eq!(tests_list(&names, 5), "t0, t1, t2, t3, t4 … and 2 more");
        assert_eq!(tests_list(&names[..2], 5), "t0, t1");
    }
}
