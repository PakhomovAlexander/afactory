//! Shared read-only Task semantics over checked Store projections. No CLI commands or adapters.
use af_contracts::task::check_report::*;
use af_contracts::task::execution::TaskAttemptResultV1;
use af_contracts::task::feedback::{
    TASK_RETRY_FEEDBACK_V1, TaskFeedbackCodeV1, TaskRetryFeedbackV1,
};
use af_contracts::task::review::{TASK_REVIEW_ROUND_V1, TaskReviewRoundV1};
use af_contracts::task::run_report::{
    TASK_DIAGNOSTIC_V1, TaskDiagnosticV1, TaskFailureClassV1, TaskNodeOutcomeV1,
};
use af_contracts::task::usage::TaskUnknownUsageCauseV1;
use af_contracts::task::verification::*;
use af_contracts::task::{pipeline, state::*, wake::WakeStateV1};
use af_contracts::{
    Producer,
    task::{self, *},
};
use af_graph::task::CompiledOperator;
use af_store::{
    Cas,
    store::task::{TaskProjection, execution::TaskAttemptAccounting},
};
use std::collections::BTreeMap;

fn artifact<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, kind: &str) -> Result<T, String> {
    let value = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if value.artifact_id != id || value.artifact_type != kind {
        return Err(format!("Expected exact {kind} artifact"));
    }
    serde_json::from_value(value.payload).map_err(|e| e.to_string())
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Worker,
    Admission,
    Check,
    Kernel,
}
/// Why a Task that ran stopped before it finished: an `af task cancel` of its last lease, a
/// started Attempt nobody settled (its writer was lost), the cause its last Attempt was
/// abandoned with (ADR-0209), an interrupt as its last Attempt's usage or diagnostic records it,
/// or a writer that lost its lease (the default when nothing says more).
pub fn abandoned_cause(cas: &Cas, state: &TaskProjection, cancelled: Option<u64>) -> TaskCauseV1 {
    use af_store::store::task::execution::TaskAbandonCause;
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

pub fn node_slot(operator: &CompiledOperator) -> Option<&str> {
    match operator {
        CompiledOperator::ProviderAdmission { bindings } => bindings.first().map(String::as_str),
        CompiledOperator::Primitive {
            operator:
                pipeline::TaskOperatorV1::Worker { slot }
                | pipeline::TaskOperatorV1::Verify { slot }
                | pipeline::TaskOperatorV1::FixVerify { slot },
            ..
        } => Some(slot),
        _ => None,
    }
}

pub fn node_kind(operator: Option<&CompiledOperator>) -> NodeKind {
    match operator {
        Some(CompiledOperator::ProviderAdmission { .. }) => NodeKind::Admission,
        Some(CompiledOperator::Primitive {
            operator: pipeline::TaskOperatorV1::Check { .. },
            ..
        }) => NodeKind::Check,
        Some(operator) if node_slot(operator).is_some() => NodeKind::Worker,
        _ => NodeKind::Kernel,
    }
}

/// Whether a review round the Task recorded asked for changes.
pub fn reviewed_with_findings(cas: &Cas, state: &TaskProjection) -> Result<bool, String> {
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
            af_contracts::task::review::ReviewConclusionV1::ChangesRequested
                | af_contracts::task::review::ReviewConclusionV1::ConvergenceExhausted
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The cause a planning diagnostic the result cites names, if it names one.
pub fn planning_cause(cas: &Cas, state: &TaskProjection) -> Option<TaskCauseV1> {
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

/// The `error` an Attempt's own diagnostic records.
pub fn attempt_error(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<String> {
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
pub fn diagnostic_cause(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<String> {
    let (Some(TaskAttemptResultV1::Failed { diagnostic_id, .. })
    | Some(TaskAttemptResultV1::Abandoned { diagnostic_id })) = &attempt.result
    else {
        return None;
    };
    let value = cas.get_json(diagnostic_id).ok()?;
    value["cause"].as_str().map(str::to_owned)
}

/// Settlement appends the final adapter class as an Attempt-produced artifact. Bare native
/// output cannot supply it, and an earlier call's evidence cannot replace the last call's.
pub fn native_failure_cause(cas: &Cas, attempt: &TaskAttemptAccounting) -> Option<TaskCauseV1> {
    use af_runner::native_failure::{NATIVE_FAILURE_EVIDENCE_V1, NativeFailureEvidenceV1};
    let envelope = cas.get_artifact(attempt.raw_artifact_ids.last()?).ok()?;
    if envelope.artifact_type != NATIVE_FAILURE_EVIDENCE_V1
        || !matches!(&envelope.producer, Producer::Attempt { node_id, attempt_id, .. }
            if node_id == &attempt.reservation.node && attempt_id == &attempt.attempt_id)
        || envelope.input_artifacts.len() != 1
        || envelope.input_artifacts.first() != attempt.context_id.as_ref()
    {
        return None;
    }
    NativeFailureEvidenceV1::decode(envelope.payload)
        .ok()?
        .task_cause()
}

/// The `af/TaskAttemptWallEnd@1` an Attempt's settlement keeps among its raw evidence, with its
/// ID, when the Attempt wall ended it (ADR-0257).
pub fn wall_end(
    cas: &Cas,
    attempt: &TaskAttemptAccounting,
) -> Option<(
    String,
    af_contracts::task::attempt_wall::TaskAttemptWallEndV1,
)> {
    use af_contracts::task::attempt_wall::*;
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

/// The cause of one failed node: typed evidence first (the Attempt wall, final native class,
/// unknown usage and retry feedback), af's own words second, and the kind of work
/// the node does last.
pub fn node_cause(
    cas: &Cas,
    class: TaskFailureClassV1,
    kind: NodeKind,
    attempt: Option<&TaskAttemptAccounting>,
    attempt_error: Option<&str>,
    message: &str,
) -> TaskCauseV1 {
    let text = format!("{}\n{message}", attempt_error.unwrap_or_default());
    if let Some(attempt) = attempt {
        // The typed records of ADR-0257 first: an Attempt its wall ended, and a failure whose
        // diagnostic names the Provider's environment as its closed class.
        if wall_end(cas, attempt).is_some() {
            return TaskCauseV1::AttemptWall;
        }
        if diagnostic_cause(cas, attempt).as_deref() == Some("environment") {
            return TaskCauseV1::ProviderEnvironment;
        }
        if let Some(cause) = native_failure_cause(cas, attempt) {
            return cause;
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
fn failure_causes(cas: &Cas, state: &TaskProjection) -> Result<Vec<Option<TaskCauseV1>>, String> {
    let Some(report_id) = state.run_reports.last() else {
        return Ok(Vec::new());
    };
    let report =
        af_store::store::task::read_task_run_report(cas, report_id).map_err(|e| e.to_string())?;
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
        let failure = cause;
        if consequence {
            consequences.push(failure);
        } else {
            own.push(failure);
        }
    }
    own.extend(consequences);
    Ok(own)
}
/// Pure phase precedence shared by state rendering and wake history replay.
pub fn semantic_state(
    cas: &Cas,
    state: &TaskProjection,
    result: Option<&TaskResultV1>,
    now: u64,
    cancelled: Option<u64>,
) -> Result<WakeStateV1, String> {
    let failures = failure_causes(cas, state)?;
    let phase = task::lifecycle::phase(
        &state.phase,
        state.lease_until_unix_ms() > now,
        state.admitted,
        abandoned_cause(cas, state, cancelled),
        failures.iter().copied().flatten(),
    );
    let waiting_reason = match state.phase {
        TaskPhaseV1::Waiting { reason } => Some(reason),
        _ => None,
    };
    let (outcome, cause) = match phase {
        TaskStatePhaseV1::Abandoned => (
            Some(TaskOutcomeV1::Cancelled),
            Some(abandoned_cause(cas, state, cancelled)),
        ),
        TaskStatePhaseV1::Finished => {
            let result = result.ok_or("A finished Task has no recorded result")?;
            let concluded = matches!(
                result.domain_conclusion.as_str(),
                "changes_requested" | "convergence_exhausted"
            );
            let cause = if result.acceptance == TaskAcceptanceV1::Satisfied && !concluded {
                None
            } else if let Some(cause) = last_gate_cause(cas, state)? {
                Some(cause)
            } else if concluded && reviewed_with_findings(cas, state)? {
                Some(TaskCauseV1::ReviewerFindings)
            } else {
                failures.iter().copied().flatten().next().or_else(|| {
                    Some(
                        if concluded || result.acceptance == TaskAcceptanceV1::Unsatisfied {
                            TaskCauseV1::EvaluatorRejected
                        } else {
                            planning_cause(cas, state).unwrap_or(TaskCauseV1::Budget)
                        },
                    )
                })
            };
            (
                Some(
                    cause
                        .and_then(TaskCauseV1::outcome)
                        .unwrap_or(TaskOutcomeV1::Verified),
                ),
                cause,
            )
        }
        _ => (None, None),
    };
    let value = WakeStateV1 {
        phase,
        outcome,
        cause,
        waiting_reason,
    };
    value.validate()?;
    Ok(value)
}

/// Exact gate precedence; presentation supplies the same checked facts.
pub fn gate_failure_cause(
    status: TaskGateStatusV1,
    preflight: bool,
    walled: bool,
    flaky: bool,
) -> TaskCauseV1 {
    match status {
        _ if walled && !flaky => TaskCauseV1::CheckFailed,
        _ if walled => TaskCauseV1::CheckFlakyThenFailed,
        TaskGateStatusV1::NotRun => TaskCauseV1::PreflightFailed,
        _ if preflight => TaskCauseV1::PreflightFailed,
        _ if flaky => TaskCauseV1::CheckFlakyThenFailed,
        _ => TaskCauseV1::CheckFailed,
    }
}

fn last_gate_cause(cas: &Cas, state: &TaskProjection) -> Result<Option<TaskCauseV1>, String> {
    let Some(execution) = &state.execution else {
        return Ok(None);
    };
    let mut found = None;
    for node in &execution.graph.order {
        let output = match execution.outputs.get(node) {
            Some((id, output)) => Some((id.clone(), output.clone())),
            None => state
                .gate_retries
                .iter()
                .rev()
                .flat_map(|retry| &retry.gates)
                .find(|gate| gate.node == *node)
                .map(|gate| {
                    artifact::<task::execution::TaskOutputV1>(
                        cas,
                        &gate.output_id,
                        task::execution::TASK_OUTPUT_V1,
                    )
                    .map(|output| (gate.output_id.clone(), output))
                })
                .transpose()?,
        };
        if let Some((output_id, output)) = output {
            for port in output.outputs.values() {
                if port.artifact_type == TASK_CHECK_RECEIPT_V2
                    && let Some(id) = port.artifact_ids.last()
                {
                    found = Some((output_id.clone(), id.clone()));
                }
            }
        }
    }
    let Some((output_id, receipt_id)) = found else {
        return Ok(None);
    };
    let receipt: TaskCheckReceiptV2 = artifact(cas, &receipt_id, TASK_CHECK_RECEIPT_V2)?;
    let attempt = execution.attempt_accounting().into_iter().find(|attempt| {
        matches!(&attempt.result,Some(TaskAttemptResultV1::Succeeded {output_id:id}) if *id == output_id)
    });
    let runs: BTreeMap<String, TaskCheckRunV1> = attempt
        .iter()
        .flat_map(|a| &a.raw_artifact_ids)
        .filter_map(|id| {
            let artifact = cas.get_optional_artifact(id).ok()??;
            if artifact.artifact_type != TASK_CHECK_REPORT_V1
                || !matches!(artifact.producer, Producer::Attempt { .. })
            {
                return None;
            }
            let report: TaskCheckReportV1 = serde_json::from_value(artifact.payload).ok()?;
            report.validate().ok()?;
            (report.receipt_id == receipt_id).then_some(report)
        })
        .flat_map(|r| r.checks)
        .collect();
    let mut failed = false;
    let mut not_run = false;
    let mut preflight = true;
    let mut flaky = false;
    let mut walled = false;
    for (name, id) in &receipt.checks {
        let result = cas.get_json(id).map_err(|e| e.to_string())?;
        let status = result["status"].as_str().unwrap_or("unknown");
        failed |= status == "failed";
        not_run |= status == "not_run";
        walled |= status != "passed" && receipt.stopped.contains_key(name);
        let (failed_tests, has_flaky, before_tests) = match runs
            .get(name)
            .filter(|r| r.report == CheckReportStateV1::Admitted)
        {
            Some(run) => (
                run.tests.as_ref().is_some_and(|t| !t.failed.is_empty()),
                run.tests.as_ref().is_some_and(|t| !t.flaky.is_empty()),
                run.stopped_at()
                    .is_some_and(|s| s.stage < CheckStageV1::Tests),
            ),
            None => {
                let mut text = String::new();
                for field in ["stdout", "stderr"] {
                    if let Some(id) = result[field].as_str() {
                        let bytes = cas
                            .get_bounded(id, 64 * 1024 * 1024)
                            .map_err(|e| e.to_string())?;
                        text.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    text.push('\n');
                }
                let (failed, flaky) = failing_tests(&text);
                (
                    !failed.is_empty(),
                    !flaky.is_empty(),
                    stopped_step(&text).is_some_and(|(step, _)| !step.starts_with("test")),
                )
            }
        };
        flaky |= has_flaky;
        preflight &= status != "failed" || (!failed_tests && before_tests);
    }
    let status = if failed {
        TaskGateStatusV1::Failed
    } else if not_run {
        TaskGateStatusV1::NotRun
    } else {
        TaskGateStatusV1::Passed
    };
    Ok((status != TaskGateStatusV1::Passed)
        .then(|| gate_failure_cause(status, preflight, walled, flaky)))
}

/// Checked step metadata shared with CLI presentation, without any formatting or commands.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoppedStep {
    pub step: String,
    pub exit_code: i64,
    #[serde(skip)]
    pub stage: Option<CheckStageV1>,
}
