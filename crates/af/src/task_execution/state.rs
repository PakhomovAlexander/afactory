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
use review_core::task::verification::{TASK_CHECK_RECEIPT_V1, TaskCheckReceiptV1};
use review_graph::task::{CompiledOperator, ReviewOperation};
use review_store::store::task::execution::TaskAttemptAccounting;

/// The schema of `af task show ID --checks --json`.
pub(crate) const TASK_CHECKS_V1: &str = "af/task-checks@1";

/// How many lines of a check's stderr `af task show --checks` prints.
const STDERR_TAIL_LINES: usize = 40;

/// The literal a next command names for the Task file a person edits.
pub(crate) const TASK_FILE: &str = "TASK_FILE";

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
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StoppedStep {
    pub(crate) step: String,
    pub(crate) exit_code: i64,
}

/// The state of an uncollected Task at `now`.
pub(crate) fn view(
    cas: &Cas,
    state: &TaskProjection,
    result: Option<&TaskResultV1>,
    now: u64,
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
        // signed decision, say); one that was interrupted or lost its writer was abandoned.
        _ if abandoned_cause(cas, state) != TaskCauseV1::HostCancelled
            && failures.iter().any(|f| f.cause.is_some()) =>
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
        TaskStatePhaseV1::Abandoned => TaskStateV1::ended(
            phase,
            TaskOutcomeV1::Cancelled,
            Some(abandoned_cause(cas, state)),
        )
        .with_next(format!("af task run {id}")),
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

/// Why a Task that ran stopped before it finished: an interrupt, as its last Attempt's usage
/// or diagnostic records it, or a writer that lost its lease (the default when nothing says
/// more).
fn abandoned_cause(cas: &Cas, state: &TaskProjection) -> TaskCauseV1 {
    let mut attempts = state
        .execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
        .collect::<Vec<_>>();
    attempts.sort_by_key(|attempt| (attempt.settled_unix_ms, attempt.started_unix_ms));
    let Some(last) = attempts.last() else {
        return TaskCauseV1::WriterLost;
    };
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
    let ended = |cause: TaskCauseV1| {
        TaskStateV1::ended(
            phase,
            cause.outcome().unwrap_or(TaskOutcomeV1::Exhausted),
            Some(cause),
        )
        .with_next(next_action(cas, state, cause))
    };
    // The last gate failing decides: what came after it never ran on a passing tree.
    if let (Some(gate), Some(checks)) = (
        gate.filter(|g| g.status != TaskGateStatusV1::Passed),
        checks,
    ) {
        return Ok(ended(gate_cause(checks, gate)));
    }
    // Findings a review recorded are its verdict, whatever else failed beside it.
    if concluded && reviewed_with_findings(cas, state)? {
        return Ok(ended(TaskCauseV1::ReviewerFindings));
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
    Ok(ended(cause))
}

/// The one command a person runs after a Task ended with `cause`.
fn next_action(cas: &Cas, state: &TaskProjection, cause: TaskCauseV1) -> String {
    let id = &state.task_id;
    match cause {
        TaskCauseV1::ReviewerFindings => format!("af task findings {id}"),
        TaskCauseV1::CheckFailed | TaskCauseV1::CheckFlakyThenFailed => {
            format!("af task run {id} --retry-gate")
        }
        TaskCauseV1::PreflightFailed => format!("af task show {id} --checks"),
        TaskCauseV1::ProviderNetwork
        | TaskCauseV1::ProviderCapacity
        | TaskCauseV1::ProviderTimeout => "af provider status".into(),
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

/// A failed gate's cause: a check never started, or every failed check stopped at a step before
/// its tests and named no test; its tests failed after some were flaky; or it failed.
fn gate_cause(checks: &ChecksView, gate: &TaskGateV1) -> TaskCauseV1 {
    let preflight = checks.checks.iter().all(|c| {
        c.status != "failed"
            || (c.failed_tests.is_empty()
                && c.stopped_step
                    .as_ref()
                    .is_some_and(|s| !s.step.starts_with("test")))
    });
    match gate.status {
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
        let Some((output_id, output)) = execution.outputs.get(node) else {
            continue;
        };
        for port in output.outputs.values() {
            if port.artifact_type == TASK_CHECK_RECEIPT_V1
                && let Some(id) = port.artifact_ids.last()
            {
                found = Some((node.clone(), output_id.clone(), id.clone()));
            }
        }
    }
    let Some((node, output_id, receipt_id)) = found else {
        return Ok(None);
    };
    let receipt: TaskCheckReceiptV1 = artifact(cas, &receipt_id, TASK_CHECK_RECEIPT_V1)?;
    let attempt = execution.attempt_accounting().into_iter().find(|attempt| {
        matches!(&attempt.result, Some(TaskAttemptResultV1::Succeeded { output_id: id }) if *id == output_id)
    });
    let mut checks = Vec::new();
    for (name, result_id) in &receipt.checks {
        checks.push(check_view(cas, name, result_id, attempt.as_ref())?);
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
        stopped_step,
        failed_tests,
        flaky,
        stdout_id,
        stderr_id,
        stderr_tail,
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
            let mut text = format!(
                "{} {}",
                if failed.len() == 1 { "check" } else { "checks" },
                failed.join(", ")
            );
            let gate = view.gate.as_ref()?;
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

/// The `af/task-checks@1` document of `af task show ID --checks --json`.
pub(crate) fn checks_document(task_id: &str, view: &TaskView) -> serde_json::Value {
    json!({
        "schema": TASK_CHECKS_V1,
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
        match check.elapsed_ms {
            Some(ms) => head.push_str(&format!(" in {ms} ms")),
            None if check.remote => head.push_str(", run remotely"),
            None => head.push_str(", never started"),
        }
        head.push_str(&format!(", {}", check.cache));
        lines.push(head);
        if let Some(reason) = &check.reason {
            lines.push(format!("  reason: {reason}"));
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
            stopped_step: step.map(|s| StoppedStep {
                step: s.into(),
                exit_code: 2,
            }),
            failed_tests: tests.iter().map(|t| t.to_string()).collect(),
            flaky: flaky.iter().map(|t| t.to_string()).collect(),
            stdout_id: None,
            stderr_id: None,
            stderr_tail: vec![],
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
