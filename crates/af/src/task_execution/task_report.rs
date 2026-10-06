//! `af task report` (ADR-0142): what recorded Tasks cost and how they ran, as one Markdown block
//! for a pull request description or one `af/task-report@1` document.
//!
//! Read-only. It reads what `af task show` and `af task list` read — the Task's projection,
//! execution records, review rounds, attempt walls and the event times of its log — opens the
//! Store read-only, and never dispatches a Worker or contacts a Provider. A figure the Store does
//! not record is left out, never estimated.

use super::*;
use review_core::Severity;
use review_core::finding_set::FindingSetV1;
use review_core::task::execution::{TASK_OUTPUT_V1, TaskAttemptResultV1, TaskOutputV1};
use review_core::task::feedback::{TASK_RETRY_FEEDBACK_V1, TaskRetryFeedbackV1};
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::plan::WorkerExecutionV1;
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1,
};
use review_core::task::task_report::*;
use review_core::task::usage::DecimalU128;
use review_core::task::verification::{TASK_CHECK_RECEIPT_V1, TaskCheckReceiptV1};
use review_graph::task::{CompiledNode, CompiledOperator, ReviewOperation};
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
    let mut pipelines: Vec<TaskReportPipelineV1> = Vec::new();
    let mut tasks = Vec::new();
    for id in task_ids {
        let (task, pipeline) = entry(&cas, &store, id)?;
        if let Some(pipeline) = pipeline
            && !pipelines.contains(&pipeline)
        {
            pipelines.push(pipeline);
        }
        tasks.push(task);
    }
    TaskReportV1::new(pipelines, tasks)
}

/// One Task's entry, and the pipeline its current plan runs.
fn entry(
    cas: &Cas,
    store: &EventStore,
    id: &str,
) -> Result<(TaskReportEntryV1, Option<TaskReportPipelineV1>), String> {
    let not_found = || format!("Task `{id}` was not found");
    let run_id = review_store::store::task::task_run_id(id).map_err(|_| not_found())?;
    // A collected Task keeps its retained summary and its event times; its records are gone.
    if let Some(collected) = store.collected_task(id).map_err(|e| e.to_string())? {
        let times = event_times(None, store, &run_id)?.ok_or_else(not_found)?;
        let summary = &collected.collected;
        let task = TaskReportEntryV1 {
            round: 0,
            task_id: summary.task_id.clone(),
            kind: summary.kind.clone(),
            pipeline: None,
            outcome: summary.outcome.clone(),
            collected: true,
            review_rounds: None,
            findings: None,
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
        };
        return Ok((task, None));
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
    let (pipeline, planned_review) = match &state.plan_id {
        Some(plan_id) => {
            let (plan, graph) = plans.get(cas, plan_id)?;
            (Some(pipeline(cas, plan, graph)?), has_review(graph))
        }
        None => (None, false),
    };
    let rounds = recorded_rounds(cas, store, &run_id)?;
    let (attempts, nodes, review) = match &state.execution {
        Some(execution) => {
            let walls = store
                .task_attempt_wall(&run_id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|wall| (wall.attempt_id, wall.elapsed_ms))
                .collect::<BTreeMap<_, _>>();
            let accounting = execution.attempt_accounting();
            let counted = attempts(cas, &mut plans, &accounting, &walls)?;
            let review = ReviewAttempts::of(&counted);
            let (summary, nodes) = tally_attempts(counted)?;
            let summary = TaskReportAttemptsV1 {
                total: execution.budget.begun_attempts(),
                ..summary
            };
            (summary, nodes, review)
        }
        // No execution began: `af task show` states zero Attempts, and so does the Store.
        None => (
            TaskReportAttemptsV1 {
                total: 0,
                failed: 0,
                failed_tokens: DecimalU128::default(),
                failures: Vec::new(),
            },
            Vec::new(),
            ReviewAttempts::default(),
        ),
    };
    let findings = if planned_review || review.reviewers || !rounds.is_empty() {
        Some(findings(cas, &rounds, &review)?)
    } else {
        None
    };
    let task = TaskReportEntryV1 {
        round: 0,
        task_id: state.task_id.clone(),
        kind: state.revision.kind.clone(),
        pipeline: pipeline.as_ref().map(TaskReportPipelineV1::label),
        outcome: outcome_label(&state.phase, result.as_ref()).to_owned(),
        collected: false,
        review_rounds: Some(rounds.len() as u64),
        findings,
        runs: times.runs,
        attempts: Some(attempts),
        chargeable_tokens: DecimalU128::from(
            state
                .execution
                .as_ref()
                .map_or(0, |e| e.budget.committed_tokens()),
        ),
        wall_ms: times.wall_ms,
        active_ms: times.active_ms,
        nodes: Some(nodes),
    };
    Ok((task, pipeline))
}

/// Every review round the Task recorded, in the order it published them: each
/// `af/TaskReviewRound@1` an execution record published, read from the Task log rather than from
/// the current execution outputs, so a round published before `af task refresh` or a review
/// handoff cleared those outputs still counts. A round published twice counts once.
fn recorded_rounds(
    cas: &Cas,
    store: &EventStore,
    run_id: &str,
) -> Result<Vec<TaskReviewRoundV1>, String> {
    use review_core::task::event::TaskChangeV1 as Change;
    use review_core::task::execution::TaskExecutionRecordV1 as Record;
    let mut seen = BTreeSet::new();
    let mut rounds = Vec::new();
    for event in store.replay(run_id).map_err(|e| e.to_string())? {
        let transition =
            review_store::store::task::read_task_transition(&event).map_err(|e| e.to_string())?;
        let Change::ExecutionRecorded { record_id } = &transition.change else {
            continue;
        };
        let record = review_store::store::task::execution::read_execution_record(cas, record_id)
            .map_err(|e| e.to_string())?
            .record;
        let (Record::Published { output_id, .. }
        | Record::OwnedChildPublished { output_id, .. }
        | Record::OwnedChildrenCompleted { output_id, .. }) = &record
        else {
            continue;
        };
        let output: TaskOutputV1 = artifact(cas, output_id, TASK_OUTPUT_V1)?;
        for id in output
            .outputs
            .values()
            .filter(|port| port.artifact_type == TASK_REVIEW_ROUND_V1)
            .flat_map(|port| port.artifact_ids.iter())
        {
            if seen.insert(id.clone()) {
                rounds.push(artifact(cas, id, TASK_REVIEW_ROUND_V1)?);
            }
        }
    }
    Ok(rounds)
}

/// What the Task's reviewer Attempts and checks say about its review.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ReviewAttempts {
    /// A reviewer Attempt began.
    reviewers: bool,
    /// Reviewer nodes with at least one failed Attempt.
    failed_reviewers: u64,
    /// A check result the Task recorded failed.
    check_failed: bool,
}

impl ReviewAttempts {
    fn of(counted: &[CountedAttempt]) -> Self {
        let failed: BTreeSet<&str> = counted
            .iter()
            .filter(|a| a.reviewer && a.failure.is_some())
            .map(|a| a.node.as_str())
            .collect();
        Self {
            reviewers: counted.iter().any(|a| a.reviewer),
            failed_reviewers: failed.len() as u64,
            check_failed: counted.iter().any(|a| {
                a.checks
                    .iter()
                    .any(|c| c.status == TaskReportCheckStatusV1::Failed)
            }),
        }
    }
}

/// The Task's findings: every finding its recorded rounds' reduce steps wrote, counted once.
/// Each complete round names the `FindingSet@1` its reduce step wrote; that set also carries
/// earlier rounds' findings, so a round contributes only the entries it saw itself
/// (`last_seen_round` is the set's round), and a finding seen in several rounds of the Task is
/// counted once, at the severity its last round recorded.
///
/// The counts are unknown when the review ran but a recorded round has no complete
/// `FindingSet@1` (its gather was incomplete, say a required reviewer's result is missing), or
/// it recorded no round yet: what that round would have found is not in the Store, so it never
/// reads as no findings.
fn findings(
    cas: &Cas,
    rounds: &[TaskReviewRoundV1],
    review: &ReviewAttempts,
) -> Result<TaskReportFindingsV1, String> {
    let mut seen: BTreeMap<String, Severity> = BTreeMap::new();
    let mut complete = true;
    for round in rounds {
        let Some(id) = &round.finding_set_id else {
            complete = false;
            continue;
        };
        let set: FindingSetV1 = artifact(cas, id, review_core::contract::FINDING_SET_V1)?;
        set.validate()?;
        for finding in set.findings {
            if finding.last_seen_round == set.round {
                seen.insert(finding.finding_id, finding.severity);
            }
        }
    }
    // A round that admitted a reviewer's result ran, complete or not.
    let review_ran = review.reviewers
        || rounds
            .iter()
            .any(|r| r.finding_set_id.is_some() || !r.selected_results.is_empty());
    // Before the review runs every count is zero; once it ran, only complete rounds count.
    let known = !review_ran || complete && !rounds.is_empty();
    let count = |severity| known.then(|| seen.values().filter(|s| **s == severity).count() as u64);
    Ok(TaskReportFindingsV1 {
        blocker: count(Severity::Blocker),
        major: count(Severity::Major),
        minor: count(Severity::Minor),
        review_ran,
        gate_failed: !review_ran && review.check_failed,
        failed_reviewers: review.failed_reviewers,
    })
}

/// Whether a compiled graph reviews: it reduces reviewer results or runs a Review frontend.
fn has_review(graph: &CompiledTask) -> bool {
    graph.nodes.values().any(|node| {
        matches!(
            node.operator,
            CompiledOperator::Primitive {
                operator: TaskOperatorV1::ReviewReduce {},
                ..
            } | CompiledOperator::ReviewDomain { .. }
        )
    })
}

/// A node's Worker slot: the slot a Worker, Verify, FixVerify, reviewer or scatter node runs.
fn slot_of(node: &CompiledNode) -> Option<&str> {
    match &node.operator {
        CompiledOperator::Primitive {
            operator:
                TaskOperatorV1::Worker { slot }
                | TaskOperatorV1::Verify { slot }
                | TaskOperatorV1::FixVerify { slot },
            ..
        }
        | CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        } => Some(slot),
        _ => None,
    }
}

/// The Worker `plan` binds `slot` to, named by Provider kind, model and effort only: never the
/// binding's label or principal, and a model value that is not a model identity is `unknown`.
fn bound_worker(plan: &ExecutionPlanV1, slot: &str) -> Option<TaskReportWorkerV1> {
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
}

fn slot_role(graph: &CompiledTask, slot: &str) -> String {
    graph
        .slots
        .get(slot)
        .map_or_else(|| "worker".to_owned(), |slot| slot.role.clone())
}

/// The pipeline a plan runs, as its pipeline line states it: the root pipeline's name and
/// version, and every node that runs a Worker or checks (the gate), in dependency order. A
/// node's stage is its depth in the compiled graph, every node it reads or is conditioned on
/// coming before it; nodes of one stage, role, Worker and check list form one step. Kernel
/// bookkeeping nodes (seal, bind, reduce, accept, select) and Provider admission are left out.
fn pipeline(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
) -> Result<TaskReportPipelineV1, String> {
    let root = graph
        .calls
        .get("root")
        .ok_or("Task has no captured root Pipeline")?;
    let (name, version) = preview::package_name_version(cas, plan, &root.pipeline)?;
    let mut depth: BTreeMap<&str, u64> = BTreeMap::new();
    for id in &graph.order {
        let Some(node) = graph.nodes.get(id) else {
            continue;
        };
        let level = node
            .inputs
            .values()
            .map(|address| address.node.as_str())
            .chain(node.conditions.iter().map(|c| c.source.node.as_str()))
            .filter_map(|source| source_depth(graph, &depth, source, 0))
            .max()
            .map_or(0, |level| level + 1);
        depth.insert(id, level);
    }
    let mut found = Vec::new();
    for (index, id) in graph.order.iter().enumerate() {
        let Some(node) = graph.nodes.get(id) else {
            continue;
        };
        let (role, worker, checks) = match &node.operator {
            CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::Check {
                        checks,
                        remote_checks,
                    },
                ..
            } => (
                TASK_REPORT_GATE_ROLE.to_owned(),
                None,
                checks
                    .union(remote_checks)
                    .map(|name| preview::text(name))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            ),
            _ => match slot_of(node) {
                Some(slot) => (slot_role(graph, slot), bound_worker(plan, slot), Vec::new()),
                None => continue,
            },
        };
        found.push((depth[id.as_str()], index, role, id.clone(), worker, checks));
    }
    found.sort_by_key(|(level, index, ..)| (*level, *index));
    let mut steps: Vec<TaskReportStepV1> = Vec::new();
    let (mut stage, mut last) = (0u64, None);
    for (level, _, role, node, worker, checks) in found {
        if last != Some(level) {
            stage += 1;
            last = Some(level);
        }
        match steps.iter_mut().find(|step| {
            step.stage == stage
                && step.role == role
                && step.worker == worker
                && step.checks == checks
        }) {
            Some(step) => step.nodes.push(node),
            None => steps.push(TaskReportStepV1 {
                stage,
                role,
                nodes: vec![node],
                worker,
                checks,
            }),
        }
    }
    Ok(TaskReportPipelineV1 {
        name,
        version,
        steps,
    })
}

/// The depth of what `source` produces: a node's own depth, or for a call's address the
/// deepest node its outputs come from. `None` for an address the graph does not resolve.
fn source_depth(
    graph: &CompiledTask,
    depth: &BTreeMap<&str, u64>,
    source: &str,
    nesting: usize,
) -> Option<u64> {
    if let Some(level) = depth.get(source) {
        return Some(*level);
    }
    // Calls nest no deeper than the compiled graph; the bound only guards a malformed one.
    if nesting > graph.calls.len() {
        return None;
    }
    graph.calls.get(source).and_then(|call| {
        call.outputs
            .values()
            .filter_map(|address| source_depth(graph, depth, &address.node, nesting + 1))
            .max()
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
    /// The command executed new work: an Attempt started.
    Work,
    /// The command refreshed the Task's source (`af task refresh`): never a run.
    Refresh,
    /// Anything else: a lease, a decision, a delivery, planning, settling or publishing an
    /// Attempt's result, finishing the Task, or recovering an earlier writer's pending Attempts.
    Other,
}

/// What one execution record says about its lease: only a started Attempt is new work. A
/// command that resumes a Task whose Attempts have all settled only publishes the selected
/// results and finishes it, which is not a run. `None` is a record a collected Task no longer
/// holds: each of its recorded executions counts as work, since which one started an Attempt is
/// gone with it.
fn record_mark(record: Option<&review_core::task::execution::TaskExecutionRecordV1>) -> Mark {
    use review_core::task::execution::TaskExecutionRecordV1 as Record;
    match record {
        Some(Record::Started { .. }) | None => Mark::Work,
        Some(_) => Mark::Other,
    }
}

/// `None` when the log holds no Task event. Every command that writes a Task holds its own
/// writer lease, and each lease has its own epoch. The tombstone is the collector's, not the
/// Task's, and counts toward neither wall nor active time. `records` is the CAS that still holds
/// the Task's execution records; a collected Task has none.
fn event_times(
    records: Option<&Cas>,
    store: &EventStore,
    run_id: &str,
) -> Result<Option<EventTimes>, String> {
    use review_core::task::event::TaskChangeV1 as Change;
    let mut events = Vec::new();
    for event in store.replay(run_id).map_err(|e| e.to_string())? {
        let transition =
            review_store::store::task::read_task_transition(&event).map_err(|e| e.to_string())?;
        let mark = match &transition.change {
            Change::TaskCollected { .. } => continue,
            Change::SourceRefreshed { .. } => Mark::Refresh,
            Change::ExecutionRecorded { record_id } => match records {
                Some(cas) => record_mark(Some(
                    &review_store::store::task::execution::read_execution_record(cas, record_id)
                        .map_err(|e| e.to_string())?
                        .record,
                )),
                None => record_mark(None),
            },
            _ => Mark::Other,
        };
        events.push((transition.epoch, transition.now_unix_ms, mark));
    }
    Ok(tally_runs(&events))
}

/// The runs among `(epoch, time, mark)` events in log order. A run is an epoch in which an
/// Attempt started and that did not refresh the source, so an `af task run` (or `af task start
/// --execute`) that began work counts, and an `af task refresh` that only recovered pending
/// Attempts, or a resume that only published settled results, does not; its span runs from the
/// epoch's first event to its last. Wall time runs from the first event to the last.
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

/// The Task's started Attempts, from the common ledger, in start order. Each Attempt's role and
/// Worker come from the plan that Attempt ran under, not from the node's first Attempt.
fn attempts(
    cas: &Cas,
    plans: &mut Plans,
    accounting: &[TaskAttemptAccounting],
    walls: &BTreeMap<String, u64>,
) -> Result<Vec<CountedAttempt>, String> {
    let mut started: Vec<&TaskAttemptAccounting> =
        accounting.iter().filter(|a| a.started).collect();
    started.sort_by_key(|a| (a.started_unix_ms, a.attempt_id.clone()));
    let mut counted = Vec::new();
    for attempt in started {
        let node = attempt.reservation.node.clone();
        let (role, worker, reviewer) = role_and_worker(cas, plans, &attempt.plan_id, &node)?;
        counted.push(CountedAttempt {
            node,
            role,
            worker,
            reviewer,
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
    Ok(counted)
}

/// One started Attempt, as the report counts it.
#[derive(Debug, Clone)]
struct CountedAttempt {
    node: String,
    role: String,
    worker: Option<TaskReportWorkerV1>,
    /// The node is a reviewer: its result feeds a review reduce step, or it is a Review
    /// frontend's reviewer.
    reviewer: bool,
    failure: Option<TaskReportFailureClassV1>,
    tokens: u128,
    elapsed_ms: Option<u64>,
    checks: Vec<TaskReportCheckV1>,
}

/// The failure figures and the per-node rows of `counted`, in start order. A row is one node
/// under one role and Worker: a node whose Attempts ran under plans that bound it to different
/// Workers gets one row per Worker, so each Worker carries only its own Attempts' tokens. The
/// figures' `total` is filled by the caller from the budget, as `af task show` counts it.
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
    let output: TaskOutputV1 = artifact(cas, output_id, TASK_OUTPUT_V1)?;
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

/// A node's role, the Worker its slot is bound to in the Attempt's own plan, and whether it is a
/// reviewer: a Review frontend's reviewer or scatter node, or a Worker node whose result a
/// review reduce step reads.
fn role_and_worker(
    cas: &Cas,
    plans: &mut Plans,
    plan_id: &str,
    node: &str,
) -> Result<(String, Option<TaskReportWorkerV1>, bool), String> {
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
    let reduced = graph.nodes.values().any(|n| {
        matches!(
            n.operator,
            CompiledOperator::Primitive {
                operator: TaskOperatorV1::ReviewReduce {},
                ..
            }
        ) && n.inputs.values().any(|address| address.node == node)
    });
    Ok(match operator {
        None => ("unknown".into(), None, false),
        Some(CompiledOperator::Primitive { operator, .. }) => match operator {
            TaskOperatorV1::Worker { slot }
            | TaskOperatorV1::Verify { slot }
            | TaskOperatorV1::FixVerify { slot } => {
                (slot_role(graph, slot), bound_worker(plan, slot), reduced)
            }
            other => (
                serde_json::to_value(other).map_err(|e| e.to_string())?["op"]
                    .as_str()
                    .unwrap_or("kernel")
                    .to_owned(),
                None,
                false,
            ),
        },
        Some(CompiledOperator::ReviewDomain { operation, .. }) => match operation {
            ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot } => {
                (slot_role(graph, slot), bound_worker(plan, slot), true)
            }
            ReviewOperation::Generation => ("review_generation".into(), None, false),
            ReviewOperation::Gate => ("review_gate".into(), None, false),
            ReviewOperation::Gather => ("review_gather".into(), None, false),
            ReviewOperation::Ledger => ("review_ledger".into(), None, false),
            ReviewOperation::Slicer => ("review_slicer".into(), None, false),
        },
        // All listed slots share one captured Provider capability.
        Some(CompiledOperator::ProviderAdmission { bindings }) => (
            "provider_admission".into(),
            bindings
                .iter()
                .next()
                .and_then(|slot| bound_worker(plan, slot)),
            false,
        ),
        Some(CompiledOperator::ReviewIntegrationChecks { .. }) => {
            ("integration_checks".into(), None, false)
        }
        Some(CompiledOperator::RootInputs) => ("root_inputs".into(), None, false),
        Some(CompiledOperator::Select) => ("select".into(), None, false),
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

/// The Markdown block: the markers, a heading, one line per pipeline (and one
/// `**unknown pipeline**: not retained` line for Tasks that name none), the round table with its
/// totals row, and each round's node breakdown in a collapsed `<details>` element. Every
/// recorded value is sanitized display text, so recorded data can neither break the table nor
/// carry markup.
pub(crate) fn markdown(report: &TaskReportV1) -> String {
    let mut out = String::new();
    let mut line = |text: &str| {
        out.push_str(text);
        out.push('\n');
    };
    line(TASK_REPORT_BEGIN);
    line("### af task report");
    line("");
    for pipeline in &report.pipelines {
        line(&pipeline_line(pipeline));
        line("");
    }
    // A Task whose plan was collected, or that never reached planning, names no pipeline: one
    // line says so, so the block still leads with a pipeline line.
    if report.tasks.iter().any(|task| task.pipeline.is_none()) {
        line(TASK_REPORT_UNKNOWN_PIPELINE);
        line("");
    }
    line(&raw_row(TASK_REPORT_COLUMNS.iter().map(|c| c.to_string())));
    line("| ---: | --- | --- | --- | ---: | ---: |");
    for task in &report.tasks {
        let outcome = if task.collected {
            format!("{} (collected)", task.outcome)
        } else {
            task.outcome.clone()
        };
        line(&raw_row([
            task.round.to_string(),
            cell(&task.task_id),
            cell(&outcome),
            findings_cell(task),
            thousands(task.chargeable_tokens.get()),
            duration(task.active_ms),
        ]));
    }
    let totals = &report.totals;
    line(&raw_row([
        String::new(),
        format!(
            "{TASK_REPORT_TOTAL} {}",
            match totals.attempts {
                Some(1) => format!("1 Attempt{}", failed_suffix(totals.failed_attempts)),
                Some(n) => format!("{n} Attempts{}", failed_suffix(totals.failed_attempts)),
                None => format!("{UNKNOWN} Attempts"),
            }
        ),
        String::new(),
        String::new(),
        thousands(totals.chargeable_tokens.get()),
        duration(totals.active_ms),
    ]));
    for task in &report.tasks {
        line("");
        line("<details>");
        line(&format!(
            "<summary>Round {} · {}</summary>",
            task.round,
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

/// `**name@version**: ` and the pipeline's steps, stages joined by ` → ` and the steps of one
/// stage by ` + `. A step reads as its role and, in parentheses, its Worker or a gate's checks,
/// led by its node names when it has several nodes or shares its stage:
/// `review (bugs, correctness: codex gpt-6-sol/high)`.
fn pipeline_line(pipeline: &TaskReportPipelineV1) -> String {
    let mut stages: Vec<Vec<&TaskReportStepV1>> = Vec::new();
    for step in &pipeline.steps {
        match stages.last_mut() {
            Some(stage) if stage[0].stage == step.stage => stage.push(step),
            _ => stages.push(vec![step]),
        }
    }
    let steps = stages
        .iter()
        .map(|stage| {
            stage
                .iter()
                .map(|step| {
                    let detail = if step.checks.is_empty() {
                        worker_label(step.worker.as_ref())
                    } else {
                        step.checks.join(", ")
                    };
                    let names = step
                        .nodes
                        .iter()
                        .map(|node| node.rsplit('.').next().unwrap_or(node))
                        .collect::<Vec<_>>()
                        .join(", ");
                    if step.nodes.len() > 1 || stage.len() > 1 {
                        format!("{} ({}: {})", cell(&step.role), cell(&names), cell(&detail))
                    } else {
                        format!("{} ({})", cell(&step.role), cell(&detail))
                    }
                })
                .collect::<Vec<_>>()
                .join(" + ")
        })
        .collect::<Vec<_>>();
    format!(
        "**{}**: {}",
        cell(&pipeline.label()),
        if steps.is_empty() {
            "no Worker or check step".to_owned()
        } else {
            steps.join(" → ")
        }
    )
}

/// The Findings cell: counts by severity (`6 major, 1 minor`), `none` when the review ran and
/// its complete rounds found nothing, `unknown` when it ran but a round has no complete finding
/// set, `gate failed` when a check failed and the review did not run, `not run` when it has not
/// run, `—` for a Task without a review and `unknown` for a collected one; then
/// `; N reviewer(s) failed` when a reviewer's Attempt failed.
fn findings_cell(task: &TaskReportEntryV1) -> String {
    let Some(findings) = &task.findings else {
        return if task.collected { UNKNOWN } else { "—" }.into();
    };
    let counts = [
        (findings.blocker, "blocker"),
        (findings.major, "major"),
        (findings.minor, "minor"),
    ]
    .into_iter()
    .map(|(n, severity)| n.map(|n| (n, severity)))
    .collect::<Option<Vec<_>>>();
    let mut text = if findings.gate_failed {
        "gate failed".to_owned()
    } else if !findings.review_ran {
        "not run".to_owned()
    } else {
        match counts {
            None => UNKNOWN.to_owned(),
            Some(counts) => {
                let counts = counts
                    .into_iter()
                    .filter(|(n, _)| *n > 0)
                    .map(|(n, severity)| format!("{n} {severity}"))
                    .collect::<Vec<_>>();
                if counts.is_empty() {
                    "none".to_owned()
                } else {
                    counts.join(", ")
                }
            }
        }
    };
    if findings.failed_reviewers > 0 {
        text.push_str(&format!(
            "; {} {} failed",
            findings.failed_reviewers,
            plural(findings.failed_reviewers, "reviewer", "reviewers")
        ));
    }
    text
}

/// The round's `<summary>` after `Round N · `: its Task, runs, wall time and failed Attempts.
fn details_summary(task: &TaskReportEntryV1) -> String {
    let mut summary = format!(
        "{}: {} {}, {} wall",
        task.task_id,
        task.runs,
        plural(task.runs, "run", "runs"),
        duration(task.wall_ms)
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

/// A table row of recorded values, each sanitized.
fn row(cells: impl IntoIterator<Item = String>) -> String {
    raw_row(cells.into_iter().map(|c| cell(&c)))
}

/// A table row of cells the caller already made safe: sanitized recorded values beside the
/// renderer's own text, such as the `—` of a Task without a review.
fn raw_row(cells: impl IntoIterator<Item = String>) -> String {
    format!("| {} |", cells.into_iter().collect::<Vec<_>>().join(" | "))
}

#[cfg(test)]
mod tests;
