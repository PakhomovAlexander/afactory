//! `af task report` (ADR-0142): what recorded Tasks cost and how they ran, as one Markdown block
//! for a pull request description or one `af/task-report@1` document.
//!
//! Read-only. It reads what `af task show` and `af task list` read — the Task's projection,
//! execution records, review rounds, attempt walls and the event times of its log — opens the
//! Store read-only, and never dispatches a Worker or contacts a Provider. A figure the Store does
//! not record is left out, never estimated.

use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::feedback::{TASK_RETRY_FEEDBACK_V1, TaskRetryFeedbackV1};
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::plan::WorkerExecutionV1;
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1,
};
use review_core::task::task_report::*;
use review_core::task::usage::DecimalU128;
use review_core::task::verification::{TASK_CHECK_RECEIPT_V1, TaskCheckReceiptV1};
use review_graph::task::{CompiledOperator, ReviewOperation};
use review_store::store::task::execution::TaskAttemptAccounting;

/// The report over `task_ids`, in the order given, from the Store under `state`. An ID the
/// Store does not hold, or one named twice, is an error that names it.
pub(crate) fn read(state: &Path, task_ids: &[String]) -> Result<TaskReportV1, String> {
    let mut seen = BTreeSet::new();
    for id in task_ids {
        if !seen.insert(id.as_str()) {
            return Err(format!("Task `{id}` is named twice"));
        }
    }
    let first = task_ids.first().ok_or("name at least one Task")?;
    if !store_present(state)? {
        return Err(format!("Task `{first}` was not found"));
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let mut tasks = Vec::new();
    for id in task_ids {
        tasks.push(entry(&cas, &store, id)?);
    }
    TaskReportV1::new(tasks)
}

fn entry(cas: &Cas, store: &EventStore, id: &str) -> Result<TaskReportEntryV1, String> {
    let not_found = || format!("Task `{id}` was not found");
    let run_id = review_store::store::task::task_run_id(id).map_err(|_| not_found())?;
    // A collected Task keeps its retained summary and its event times; its records are gone.
    if let Some(collected) = store.collected_task(id).map_err(|e| e.to_string())? {
        let times = event_times(None, store, &run_id)?.ok_or_else(not_found)?;
        let summary = &collected.collected;
        return Ok(TaskReportEntryV1 {
            task_id: summary.task_id.clone(),
            kind: summary.kind.clone(),
            pipeline: None,
            outcome: summary.outcome.clone(),
            collected: true,
            review_rounds: None,
            runs: times.runs,
            attempts: None,
            chargeable_tokens: DecimalU128::from(
                summary
                    .chargeable_tokens
                    .parse::<u128>()
                    .map_err(|e| e.to_string())?,
            ),
            wall_ms: times.wall_ms,
            active_ms: times.active_ms,
            nodes: None,
        });
    }
    let state = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(not_found)?;
    let times = event_times(Some(cas), store, &run_id)?.ok_or_else(not_found)?;
    let result: Option<TaskResultV1> = match &state.phase {
        TaskPhaseV1::Finished { result_id } => Some(artifact(cas, result_id, TASK_RESULT_V1)?),
        _ => None,
    };
    let mut plans = Plans::default();
    let pipeline = match &state.plan_id {
        Some(plan_id) => {
            let (plan, graph) = plans.get(cas, plan_id)?;
            let root = graph
                .calls
                .get("root")
                .ok_or("Task has no captured root Pipeline")?;
            Some(preview::package_label(cas, plan, &root.pipeline)?)
        }
        None => None,
    };
    let (attempts, nodes, review_rounds) = match &state.execution {
        Some(execution) => {
            let walls = store
                .task_attempt_wall(&run_id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|wall| (wall.attempt_id, wall.elapsed_ms))
                .collect::<BTreeMap<_, _>>();
            let accounting = execution.attempt_accounting();
            let (summary, nodes) = attempts(cas, &mut plans, &accounting, &walls)?;
            let summary = TaskReportAttemptsV1 {
                total: execution.budget.begun_attempts(),
                ..summary
            };
            // The rounds `af task show` lists: every recorded Review Round output.
            let rounds = execution
                .outputs
                .values()
                .flat_map(|(_, output)| output.outputs.values())
                .filter(|port| port.artifact_type == TASK_REVIEW_ROUND_V1)
                .map(|port| port.artifact_ids.len() as u64)
                .sum::<u64>();
            (Some(summary), nodes, rounds)
        }
        // No execution began: `af task show` states zero Attempts, and so does the Store.
        None => (
            Some(TaskReportAttemptsV1 {
                total: 0,
                failed: 0,
                failed_tokens: DecimalU128::default(),
                failures: Vec::new(),
            }),
            Vec::new(),
            0,
        ),
    };
    Ok(TaskReportEntryV1 {
        task_id: state.task_id.clone(),
        kind: state.revision.kind.clone(),
        pipeline,
        outcome: outcome_label(&state.phase, result.as_ref()).to_owned(),
        collected: false,
        review_rounds: Some(review_rounds),
        runs: times.runs,
        attempts,
        chargeable_tokens: DecimalU128::from(
            state
                .execution
                .as_ref()
                .map_or(0, |e| e.budget.committed_tokens()),
        ),
        wall_ms: times.wall_ms,
        active_ms: times.active_ms,
        nodes: Some(nodes),
    })
}

/// What the Task log's event times say: how many runs executed work, and for how long.
#[derive(Debug, PartialEq, Eq)]
struct EventTimes {
    runs: u64,
    wall_ms: u64,
    active_ms: u64,
}

/// What one Task event says about the run its writer lease belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The command executed work: it admitted, planned, dispatched, settled or published.
    Work,
    /// The command refreshed the Task's source (`af task refresh`): never a run.
    Refresh,
    /// Anything else: a lease, a decision, a delivery, or settling and releasing an earlier
    /// writer's pending Attempts, which every command that takes a lease does first.
    Other,
}

/// `None` when the log holds no Task event. Every command that writes a Task holds its own
/// writer lease, and each lease has its own epoch. The tombstone is the collector's, not the
/// Task's, and counts toward neither wall nor active time. `records` is the CAS that still holds
/// the Task's execution records; a collected Task has none, so each of its recorded executions
/// counts as work, and only its source refreshes are told apart from runs.
fn event_times(
    records: Option<&Cas>,
    store: &EventStore,
    run_id: &str,
) -> Result<Option<EventTimes>, String> {
    use review_core::task::event::TaskChangeV1 as Change;
    use review_core::task::execution::TaskExecutionRecordV1 as Record;
    let mut events = Vec::new();
    for event in store.replay(run_id).map_err(|e| e.to_string())? {
        let transition =
            review_store::store::task::read_task_transition(&event).map_err(|e| e.to_string())?;
        let mark = match &transition.change {
            Change::TaskCollected { .. } => continue,
            Change::SourceRefreshed { .. } => Mark::Refresh,
            // Recovery settles an earlier writer's started Attempt as abandoned and releases
            // one that never started; neither is this command's work.
            Change::ExecutionRecorded { record_id } => match records {
                Some(cas) => match review_store::store::task::execution::read_execution_record(
                    cas, record_id,
                )
                .map_err(|e| e.to_string())?
                .record
                {
                    Record::Released { .. }
                    | Record::Settled {
                        result: TaskAttemptResultV1::Abandoned { .. },
                        ..
                    } => Mark::Other,
                    _ => Mark::Work,
                },
                None => Mark::Work,
            },
            Change::PlanAdmitted { .. }
            | Change::RunReported { .. }
            | Change::Finished { .. }
            | Change::Resumed {}
            | Change::RecordingResumed { .. }
            | Change::PlanningCompleted { .. }
            | Change::ReviewIntegrationSelected { .. }
            | Change::ReviewIntegrationFinished { .. }
            | Change::ReviewContinued { .. } => Mark::Work,
            _ => Mark::Other,
        };
        events.push((transition.epoch, transition.now_unix_ms, mark));
    }
    Ok(tally_runs(&events))
}

/// The runs among `(epoch, time, mark)` events in log order. A run is an epoch that executed
/// work and did not refresh the source, so an `af task run` (or `af task start --execute`)
/// counts and an `af task refresh` that only recovered pending Attempts does not; its span runs
/// from the epoch's first event to its last. Wall time runs from the first event to the last.
fn tally_runs(events: &[(u64, u64, Mark)]) -> Option<EventTimes> {
    let first = events.iter().map(|(_, time, _)| *time).min()?;
    let last = events.iter().map(|(_, time, _)| *time).max()?;
    // Epoch -> (first event, last event, executed work, refreshed), in log order.
    let mut epochs: Vec<(u64, u64, u64, bool, bool)> = Vec::new();
    for &(epoch, time, mark) in events {
        let index = match epochs.iter().position(|(e, ..)| *e == epoch) {
            Some(index) => index,
            None => {
                epochs.push((epoch, time, time, false, false));
                epochs.len() - 1
            }
        };
        let (_, start, end, work, refresh) = &mut epochs[index];
        *start = (*start).min(time);
        *end = (*end).max(time);
        *work |= mark == Mark::Work;
        *refresh |= mark == Mark::Refresh;
    }
    let runs = epochs
        .iter()
        .filter(|(_, _, _, work, refresh)| *work && !*refresh);
    Some(EventTimes {
        runs: runs.clone().count() as u64,
        wall_ms: last - first,
        active_ms: runs.map(|(_, start, end, ..)| end - start).sum(),
    })
}

/// Each recorded plan and its compiled graph, read once.
#[derive(Default)]
struct Plans(BTreeMap<String, (ExecutionPlanV1, CompiledTask)>);

impl Plans {
    fn get(&mut self, cas: &Cas, id: &str) -> Result<(&ExecutionPlanV1, &CompiledTask), String> {
        if !self.0.contains_key(id) {
            let plan: ExecutionPlanV1 = artifact(cas, id, EXECUTION_PLAN_V1)?;
            let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
            self.0.insert(id.to_owned(), (plan, graph));
        }
        let (plan, graph) = &self.0[id];
        Ok((plan, graph))
    }
}

/// The Task's Attempt figures and its per-node breakdown, from the common ledger. `total` is
/// filled by the caller from the budget, as `af task show` counts it. Each Attempt's role and
/// Worker come from the plan that Attempt ran under, not from the node's first Attempt.
fn attempts(
    cas: &Cas,
    plans: &mut Plans,
    accounting: &[TaskAttemptAccounting],
    walls: &BTreeMap<String, u64>,
) -> Result<(TaskReportAttemptsV1, Vec<TaskReportNodeV1>), String> {
    let mut started: Vec<&TaskAttemptAccounting> =
        accounting.iter().filter(|a| a.started).collect();
    started.sort_by_key(|a| (a.started_unix_ms, a.attempt_id.clone()));
    let mut counted = Vec::new();
    for attempt in started {
        let node = attempt.reservation.node.clone();
        let (role, worker) = role_and_worker(cas, plans, &attempt.plan_id, &node)?;
        counted.push(CountedAttempt {
            node,
            role,
            worker,
            failure: failure_class(cas, attempt)?,
            tokens: attempt.charged_tokens,
            elapsed_ms: walls.get(&attempt.attempt_id).copied().or_else(|| {
                attempt
                    .started_unix_ms
                    .zip(attempt.settled_unix_ms)
                    .and_then(|(start, end)| end.checked_sub(start))
            }),
            checks: checks(cas, attempt)?,
        });
    }
    tally_attempts(counted)
}

/// One started Attempt, as the report counts it.
#[derive(Debug, Clone)]
struct CountedAttempt {
    node: String,
    role: String,
    worker: Option<TaskReportWorkerV1>,
    failure: Option<TaskReportFailureClassV1>,
    tokens: u128,
    elapsed_ms: Option<u64>,
    checks: Vec<TaskReportCheckV1>,
}

/// The failure figures and the per-node rows of `counted`, in start order. A row is one node
/// under one role and Worker: a node whose Attempts ran under plans that bound it to different
/// Workers gets one row per Worker, so each Worker carries only its own Attempts' tokens.
fn tally_attempts(
    counted: Vec<CountedAttempt>,
) -> Result<(TaskReportAttemptsV1, Vec<TaskReportNodeV1>), String> {
    let mut failures: BTreeMap<TaskReportFailureClassV1, (u64, u128)> = BTreeMap::new();
    let mut nodes: Vec<TaskReportNodeV1> = Vec::new();
    for attempt in counted {
        if let Some(class) = attempt.failure {
            let entry = failures.entry(class).or_default();
            entry.0 += 1;
            entry.1 = entry
                .1
                .checked_add(attempt.tokens)
                .ok_or("Task report total overflow")?;
        }
        let index = match nodes.iter().position(|row| {
            row.node == attempt.node && row.role == attempt.role && row.worker == attempt.worker
        }) {
            Some(index) => index,
            None => {
                nodes.push(TaskReportNodeV1 {
                    node: attempt.node.clone(),
                    role: attempt.role.clone(),
                    worker: attempt.worker.clone(),
                    attempts: 0,
                    failed_attempts: 0,
                    tokens: DecimalU128::default(),
                    elapsed_ms: Some(0),
                    checks: Vec::new(),
                });
                nodes.len() - 1
            }
        };
        let row = &mut nodes[index];
        row.attempts += 1;
        row.failed_attempts += u64::from(attempt.failure.is_some());
        row.tokens = DecimalU128::from(
            row.tokens
                .get()
                .checked_add(attempt.tokens)
                .ok_or("Task report total overflow")?,
        );
        // Known only while every Attempt of the row recorded its elapsed time.
        row.elapsed_ms = row
            .elapsed_ms
            .zip(attempt.elapsed_ms)
            .and_then(|(sum, elapsed)| sum.checked_add(elapsed));
        row.checks.extend(attempt.checks);
    }
    let failed = failures.values().map(|(count, _)| count).sum();
    let failed_tokens = failures.values().map(|(_, tokens)| tokens).sum::<u128>();
    Ok((
        TaskReportAttemptsV1 {
            total: 0,
            failed,
            failed_tokens: DecimalU128::from(failed_tokens),
            failures: failures
                .into_iter()
                .map(|(class, (attempts, tokens))| TaskReportFailureV1 {
                    class,
                    attempts,
                    tokens: DecimalU128::from(tokens),
                })
                .collect(),
        },
        nodes,
    ))
}

/// Why a settled Attempt failed, by its typed retry feedback; `None` when it did not fail or
/// has not settled.
fn failure_class(
    cas: &Cas,
    attempt: &TaskAttemptAccounting,
) -> Result<Option<TaskReportFailureClassV1>, String> {
    Ok(match &attempt.result {
        Some(TaskAttemptResultV1::Failed {
            feedback_id: Some(id),
            ..
        }) => {
            let feedback: TaskRetryFeedbackV1 = artifact(cas, id, TASK_RETRY_FEEDBACK_V1)?;
            Some(feedback.code.into())
        }
        Some(TaskAttemptResultV1::Failed {
            feedback_id: None, ..
        }) => Some(TaskReportFailureClassV1::Unclassified),
        Some(TaskAttemptResultV1::Abandoned { .. }) => Some(TaskReportFailureClassV1::Abandoned),
        Some(TaskAttemptResultV1::Succeeded { .. }) | None => None,
    })
}

/// The check results an Attempt published, each with the span its runtime evidence recorded.
fn checks(cas: &Cas, attempt: &TaskAttemptAccounting) -> Result<Vec<TaskReportCheckV1>, String> {
    let Some(TaskAttemptResultV1::Succeeded { output_id }) = &attempt.result else {
        return Ok(Vec::new());
    };
    let output: review_core::task::execution::TaskOutputV1 =
        artifact(cas, output_id, review_core::task::execution::TASK_OUTPUT_V1)?;
    let receipts = output
        .outputs
        .values()
        .filter(|port| port.artifact_type == TASK_CHECK_RECEIPT_V1)
        .flat_map(|port| port.artifact_ids.iter());
    let mut spans = BTreeMap::new();
    for id in &attempt.raw_artifact_ids {
        // Raw captures share this list and may not be CAS envelopes, as in `af task show`.
        let Ok(raw) = cas.get_artifact(id) else {
            continue;
        };
        if raw.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
            continue;
        }
        let evidence: TaskRuntimeEvidenceV1 =
            serde_json::from_value(raw.payload).map_err(|e| e.to_string())?;
        evidence.validate()?;
        for span in evidence.spans {
            if span.kind == TaskRuntimeSpanKindV1::Check {
                spans.insert(span.label, span.elapsed_ms);
            }
        }
    }
    let mut checks = Vec::new();
    for id in receipts {
        let receipt: TaskCheckReceiptV1 = artifact(cas, id, TASK_CHECK_RECEIPT_V1)?;
        for (name, result_id) in &receipt.checks {
            let result = cas.get_json(result_id).map_err(|e| e.to_string())?;
            let status = match result["status"].as_str() {
                Some("passed") => TaskReportCheckStatusV1::Passed,
                Some("failed") => TaskReportCheckStatusV1::Failed,
                Some("not_run") => TaskReportCheckStatusV1::NotRun,
                _ => return Err(format!("check `{name}` has no recorded status")),
            };
            checks.push(TaskReportCheckV1 {
                name: name.clone(),
                status,
                elapsed_ms: spans.get(name).copied(),
            });
        }
    }
    Ok(checks)
}

/// A node's role and the Worker its slot is bound to in the Attempt's own plan. The Worker is
/// named by Provider kind, model and effort only: never the binding's label or principal, and
/// a model value that does not look like a model identity is `unknown`.
fn role_and_worker(
    cas: &Cas,
    plans: &mut Plans,
    plan_id: &str,
    node: &str,
) -> Result<(String, Option<TaskReportWorkerV1>), String> {
    let (plan, graph) = plans.get(cas, plan_id)?;
    // An owned child runs its owner's template operator.
    let operator = graph.nodes.get(node).map(|n| &n.operator).or_else(|| {
        graph
            .owned_children
            .iter()
            .find(|(owner, _)| {
                node.strip_prefix(owner.as_str())
                    .is_some_and(|tail| tail.starts_with('.'))
            })
            .map(|(_, template)| &template.operator)
    });
    let worker = |slot: &str| {
        plan.bindings
            .get(slot)
            .map(|binding| match &binding.execution {
                WorkerExecutionV1::Command {} => TaskReportWorkerV1::Command {},
                WorkerExecutionV1::Model {
                    provider_kind,
                    model,
                    effort,
                    ..
                } => TaskReportWorkerV1::Model {
                    provider_kind: provider_kind.clone(),
                    model: report_model(model),
                    effort: effort.clone(),
                },
            })
    };
    let slot_role = |slot: &str| {
        graph
            .slots
            .get(slot)
            .map_or_else(|| "worker".to_owned(), |slot| slot.role.clone())
    };
    Ok(match operator {
        None => ("unknown".into(), None),
        Some(CompiledOperator::Primitive { operator, .. }) => match operator {
            TaskOperatorV1::Worker { slot }
            | TaskOperatorV1::Verify { slot }
            | TaskOperatorV1::FixVerify { slot } => (slot_role(slot), worker(slot)),
            other => (
                serde_json::to_value(other).map_err(|e| e.to_string())?["op"]
                    .as_str()
                    .unwrap_or("kernel")
                    .to_owned(),
                None,
            ),
        },
        Some(CompiledOperator::ReviewDomain { operation, .. }) => match operation {
            ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot } => {
                (slot_role(slot), worker(slot))
            }
            ReviewOperation::Generation => ("review_generation".into(), None),
            ReviewOperation::Gate => ("review_gate".into(), None),
            ReviewOperation::Gather => ("review_gather".into(), None),
            ReviewOperation::Ledger => ("review_ledger".into(), None),
            ReviewOperation::Slicer => ("review_slicer".into(), None),
        },
        // All listed slots share one captured Provider capability.
        Some(CompiledOperator::ProviderAdmission { bindings }) => (
            "provider_admission".into(),
            bindings.iter().next().and_then(|slot| worker(slot)),
        ),
        Some(CompiledOperator::ReviewIntegrationChecks { .. }) => {
            ("integration_checks".into(), None)
        }
        Some(CompiledOperator::RootInputs) => ("root_inputs".into(), None),
        Some(CompiledOperator::Select) => ("select".into(), None),
    })
}

/// The recorded model when it looks like a model identity; otherwise `unknown`, since the
/// value may be a path or an account.
fn report_model(model: &str) -> String {
    if is_model_identity(model) {
        model.to_owned()
    } else {
        TASK_REPORT_UNKNOWN_MODEL.to_owned()
    }
}

/// The Markdown block: the markers, a heading, the summary table, the totals line, and each
/// Task's node breakdown in a collapsed `<details>` element. Every value is sanitized display
/// text, so recorded data can neither break the table nor carry markup.
pub(crate) fn markdown(report: &TaskReportV1) -> String {
    let mut out = String::new();
    let mut line = |text: &str| {
        out.push_str(text);
        out.push('\n');
    };
    line(TASK_REPORT_BEGIN);
    line("### af task report");
    line("");
    line(&row(TASK_REPORT_COLUMNS.iter().map(|c| c.to_string())));
    line("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: |");
    for task in &report.tasks {
        let outcome = if task.collected {
            format!("{} (collected)", task.outcome)
        } else {
            task.outcome.clone()
        };
        line(&row([
            task.task_id.clone(),
            task.kind.clone(),
            task.pipeline.clone().unwrap_or_else(|| UNKNOWN.into()),
            outcome,
            count(task.review_rounds),
            attempts_cell(
                task.attempts.as_ref().map(|a| a.total),
                task.attempts.as_ref().map(|a| a.failed),
            ),
            thousands(task.chargeable_tokens.get()),
            duration(task.active_ms),
            duration(task.wall_ms),
        ]));
    }
    line("");
    let totals = &report.totals;
    line(&format!(
        "{TASK_REPORT_TOTALS} {} {} · {} {} · {} · {} tokens · {} active",
        totals.tasks,
        plural(totals.tasks, "Task", "Tasks"),
        count(totals.review_rounds),
        if totals.review_rounds == Some(1) {
            "round"
        } else {
            "rounds"
        },
        match totals.attempts {
            Some(1) => format!("1 Attempt{}", failed_suffix(totals.failed_attempts)),
            Some(n) => format!("{n} Attempts{}", failed_suffix(totals.failed_attempts)),
            None => format!("{UNKNOWN} Attempts"),
        },
        thousands(totals.chargeable_tokens.get()),
        duration(totals.active_ms),
    ));
    for task in &report.tasks {
        line("");
        line("<details>");
        line(&format!(
            "<summary>{}</summary>",
            cell(&details_summary(task))
        ));
        line("");
        match &task.nodes {
            None => line("The Task was collected: its Attempts are no longer recorded."),
            Some(nodes) if nodes.is_empty() => line("No Attempt began."),
            Some(nodes) => {
                line("| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |");
                line("| --- | --- | --- | ---: | ---: | ---: | --- |");
                for node in nodes {
                    let checks = node
                        .checks
                        .iter()
                        .map(|check| {
                            format!(
                                "{} {} {}",
                                check.name,
                                check.status.as_str(),
                                check.elapsed_ms.map_or_else(|| UNKNOWN.into(), duration)
                            )
                        })
                        .collect::<Vec<_>>();
                    line(&row([
                        node.node.clone(),
                        node.role.clone(),
                        worker_label(node.worker.as_ref()),
                        attempts_cell(Some(node.attempts), Some(node.failed_attempts)),
                        thousands(node.tokens.get()),
                        node.elapsed_ms.map_or_else(|| UNKNOWN.into(), duration),
                        if checks.is_empty() {
                            "-".into()
                        } else {
                            checks.join(", ")
                        },
                    ]));
                }
            }
        }
        line("");
        line("</details>");
    }
    line(TASK_REPORT_END);
    out
}

const UNKNOWN: &str = "unknown";

fn details_summary(task: &TaskReportEntryV1) -> String {
    let mut summary = format!(
        "{}: {} {}",
        task.task_id,
        task.runs,
        plural(task.runs, "run", "runs")
    );
    if let Some(attempts) = &task.attempts {
        if attempts.failed == 0 {
            summary.push_str(", no failed Attempt");
        } else {
            let classes = attempts
                .failures
                .iter()
                .map(|f| format!("{} {}", f.attempts, f.class.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            summary.push_str(&format!(
                ", {} failed {} ({classes}; {} tokens)",
                attempts.failed,
                plural(attempts.failed, "Attempt", "Attempts"),
                thousands(attempts.failed_tokens.get())
            ));
        }
    }
    summary
}

fn worker_label(worker: Option<&TaskReportWorkerV1>) -> String {
    match worker {
        None => "-".into(),
        Some(TaskReportWorkerV1::Command {}) => "command".into(),
        Some(TaskReportWorkerV1::Model {
            provider_kind,
            model,
            effort,
        }) => format!("{provider_kind} {model}/{effort}"),
    }
}

fn plural(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| UNKNOWN.into(), |n| n.to_string())
}

fn failed_suffix(failed: Option<u64>) -> String {
    match failed {
        Some(0) => String::new(),
        Some(n) => format!(" ({n} failed)"),
        None => format!(" ({UNKNOWN} failed)"),
    }
}

fn attempts_cell(total: Option<u64>, failed: Option<u64>) -> String {
    match total {
        Some(total) => format!("{total}{}", failed_suffix(failed)),
        None => UNKNOWN.into(),
    }
}

/// A duration as a person reads it: `850ms`, `3.2s`, `4m 05s`, `1h 02m`. Truncated, never
/// rounded up.
fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{}.{}s", ms / 1_000, ms % 1_000 / 100),
        60_000..3_600_000 => format!("{}m {:02}s", ms / 60_000, ms % 60_000 / 1_000),
        _ => format!("{}h {:02}m", ms / 3_600_000, ms % 3_600_000 / 60_000),
    }
}

/// A token count as a person reads it, with thousands separators: `205,295`. The JSON
/// document keeps the exact decimal text.
fn thousands(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// One cell of recorded data: ASCII without controls, and nothing that ends a cell or opens
/// markup. A backslash and a pipe are written as character references, so neither a `|` nor a
/// backslash before one can end the cell, in a table or in the `<summary>` line.
fn cell(value: &str) -> String {
    preview::text(value)
        .replace('\\', "&#92;")
        .replace('|', "&#124;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn row(cells: impl IntoIterator<Item = String>) -> String {
    let cells = cells.into_iter().map(|c| cell(&c)).collect::<Vec<_>>();
    format!("| {} |", cells.join(" | "))
}

#[cfg(test)]
mod tests;
