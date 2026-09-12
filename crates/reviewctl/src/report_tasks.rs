//! Review inspection reads the common Task ledger once per Task. RunReport@6 values are
//! frozen cumulative snapshots, never Round costs or inputs to another spend accumulator.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::task::usage::{DecimalU64, DecimalU128, TaskTokenUsageV2};
use review_core::task::{ArtifactInputV1, EXECUTION_PLAN_V1};
use review_graph::task::{CompiledOperator, CompiledTask, ReviewOperation};
use review_store::store::task::execution::TaskAttemptAccounting;
use review_store::store::task::task_run_id;
use review_store::{Cas, EventStore, TaskAttemptWall};

#[derive(serde::Serialize)]
pub(super) struct TaskAccountingView {
    task_id: String,
    through_sequence: u64,
    chargeable_tokens: DecimalU128,
    reserved_tokens: DecimalU128,
    attempts_started: DecimalU64,
    provider_attempts_started: DecimalU64,
    business_attempts_started: DecimalU64,
    other_attempts_started: DecimalU64,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall_ms: Option<u64>,
    attempts: Vec<TaskAttemptView>,
}

#[derive(serde::Serialize)]
struct TaskAttemptView {
    attempt_id: String,
    invocation_id: String,
    plan_id: String,
    node: String,
    category: Category,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_node: Option<String>,
    binding_slots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u32>,
    started: bool,
    outcome: &'static str,
    reservation_id: String,
    reserved_tokens: DecimalU64,
    chargeable_tokens: DecimalU128,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<TaskAttemptResultV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall: Option<TaskWallView>,
}

#[derive(Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Category {
    Provider,
    Business,
    Other,
}

#[derive(serde::Serialize)]
struct TaskWallView {
    started_unix_ms: u64,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<TaskTokenUsageV2>,
}

pub(super) struct TaskAccountingReport {
    pub tasks: Vec<TaskAccountingView>,
    pub wall_rows: Vec<TaskAttemptWall>,
    pub rounds: BTreeSet<(u32, u32)>,
}

impl TaskAccountingReport {
    /// Merge raw Attempt intervals by their original Round/epoch. Neither cumulative report
    /// snapshots nor overlapping legacy/common intervals are added as separate durations.
    pub fn wall_ms(&self, legacy: &[review_store::AttemptWall]) -> Option<u64> {
        super::wall_spans_ms(
            legacy
                .iter()
                .map(|row| {
                    (
                        (row.round, row.epoch),
                        (row.started_unix_ms, row.elapsed_ms),
                    )
                })
                .chain(self.wall_rows.iter().map(|row| {
                    (
                        (row.round, row.epoch),
                        (row.started_unix_ms, row.elapsed_ms),
                    )
                })),
        )
    }
}

pub(super) fn summary(tasks: &[TaskAccountingView]) -> String {
    tasks
        .iter()
        .map(task_summary)
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_summary(task: &TaskAccountingView) -> String {
    format!(
        "Task {}: cumulative charge {} tokens; held {} tokens; {} started Attempts (Provider {}, business {}, other {}); through sequence {}",
        task.task_id,
        task.chargeable_tokens.get(),
        task.reserved_tokens.get(),
        task.attempts_started.get(),
        task.provider_attempts_started.get(),
        task.business_attempts_started.get(),
        task.other_attempts_started.get(),
        task.through_sequence
    )
}

struct RecordedPlan {
    graph: CompiledTask,
    round: Option<LegacyReviewRoundV1>,
}

fn artifact<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, kind: &str) -> Result<T, String> {
    let envelope = cas.get_artifact(id).map_err(|error| error.to_string())?;
    if envelope.artifact_type != kind {
        return Err(format!("Expected {kind} while inspecting Task accounting"));
    }
    serde_json::from_value(envelope.payload).map_err(|error| error.to_string())
}

fn captured_round(
    cas: &Cas,
    inputs: &BTreeMap<String, ArtifactInputV1>,
) -> Result<Option<LegacyReviewRoundV1>, String> {
    let mut rounds = inputs
        .values()
        .filter(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1);
    let Some(input) = rounds.next() else {
        return Ok(None);
    };
    if rounds.next().is_some() || input.artifact_ids.len() != 1 {
        return Err("Task accounting requires one exact captured Review Round".into());
    }
    let round: LegacyReviewRoundV1 = artifact(cas, &input.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
    round.validate()?;
    Ok(Some(round))
}

/// Enumerate validated streams as well as reported Task IDs: Provider/context failures can
/// precede both the first canonical conclusion and the first selected business result.
pub(super) fn read(
    store: &EventStore,
    cas: &Cas,
    campaign_run: &str,
    events: &[review_core::RunEvent],
) -> Result<TaskAccountingReport, String> {
    let mut referenced = BTreeSet::new();
    for event in events {
        match event.event_type {
            review_core::EventType::RunReportV6 => {
                let report: review_core::RunReportPayloadV6 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                referenced.insert(report.task_accounting.task_id);
            }
            review_core::EventType::TaskReviewResultSelectedV1 => {
                let selected: review_core::task::review_compat::TaskReviewResultSelectedV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                referenced.insert(selected.task_id);
            }
            _ => {}
        }
    }
    let ids: BTreeSet<_> = store
        .task_ids(cas)
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect();
    if !referenced.is_subset(&ids) {
        return Err("Review accounting references a missing Task stream".into());
    }
    let mut report = TaskAccountingReport {
        tasks: Vec::new(),
        wall_rows: Vec::new(),
        rounds: BTreeSet::new(),
    };
    for id in ids {
        let task = store
            .task_projection(cas, &id)
            .map_err(|error| error.to_string())?
            .ok_or("Task disappeared during accounting inspection")?;
        let current_round = captured_round(cas, &task.revision.inputs)?;
        let attempts = task
            .execution
            .as_ref()
            .map_or_else(Vec::new, |execution| execution.attempt_accounting());
        let mut plans = BTreeMap::new();
        for attempt in &attempts {
            if !plans.contains_key(&attempt.plan_id) {
                let plan: ExecutionPlanV1 = artifact(cas, &attempt.plan_id, EXECUTION_PLAN_V1)?;
                let graph = artifact(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
                plans.insert(
                    attempt.plan_id.clone(),
                    RecordedPlan {
                        round: captured_round(cas, &plan.inputs)?,
                        graph,
                    },
                );
            }
        }
        let rounds: Vec<_> = current_round
            .iter()
            .chain(plans.values().filter_map(|plan| plan.round.as_ref()))
            .collect();
        if !rounds.iter().any(|round| round.campaign_id == campaign_run) {
            if referenced.contains(&id) {
                return Err("Review accounting Task has no matching captured Campaign".into());
            }
            continue;
        }
        if rounds.iter().any(|round| round.campaign_id != campaign_run) {
            return Err("Review accounting Task spans different captured Campaigns".into());
        }
        report
            .rounds
            .extend(rounds.iter().map(|round| (round.round, round.epoch)));
        let walls: BTreeMap<_, _> = store
            .task_attempt_wall(&task_run_id(&id).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|row| (row.attempt_id.clone(), row))
            .collect();
        let mut counts = [0_u64; 3];
        let mut task_walls = Vec::new();
        let mut views = Vec::new();
        for attempt in attempts {
            let plan = &plans[&attempt.plan_id];
            let resolved = task
                .execution
                .as_ref()
                .ok_or("Task execution is missing")?
                .resolve_attempt_node(&attempt, &plan.graph)
                .map_err(|e| e.to_string())?;
            let (category, mut review_node, binding_slots) =
                classify(&resolved.definition.operator);
            if let Some(address) = resolved.owned {
                review_node = address.review_node;
            }
            if attempt.started {
                counts[match category {
                    Category::Provider => 0,
                    Category::Business => 1,
                    Category::Other => 2,
                }] += 1;
            }
            let wall = walls.get(&attempt.attempt_id).map(|row| TaskWallView {
                started_unix_ms: row.started_unix_ms,
                elapsed_ms: row.elapsed_ms,
                usage: row.usage.clone(),
            });
            if let (Some(row), Some(round)) = (walls.get(&attempt.attempt_id), &plan.round) {
                let mut row = row.clone();
                row.round = round.round;
                row.epoch = round.epoch;
                task_walls.push(row);
            }
            views.push(TaskAttemptView {
                outcome: outcome(&attempt),
                attempt_id: attempt.attempt_id,
                invocation_id: attempt.invocation_id,
                plan_id: attempt.plan_id,
                node: attempt.reservation.node,
                category,
                review_node,
                binding_slots,
                round: plan.round.as_ref().map(|round| round.round),
                epoch: plan.round.as_ref().map(|round| round.epoch),
                started: attempt.started,
                reservation_id: attempt.reservation.id,
                reserved_tokens: attempt.reservation.tokens.into(),
                chargeable_tokens: attempt.charged_tokens.into(),
                result: attempt.result,
                wall,
            });
        }
        let budget = task.execution.as_ref().map(|execution| &execution.budget);
        let begun = budget.map_or(0, |budget| budget.begun_attempts());
        if counts.iter().sum::<u64>() != begun {
            return Err("Task accounting Attempt count differs from its common budget".into());
        }
        report.tasks.push(TaskAccountingView {
            task_id: id,
            through_sequence: task
                .next_sequence
                .checked_sub(1)
                .ok_or("Task has no recorded prefix")?,
            chargeable_tokens: budget.map_or(0, |budget| budget.committed_tokens()).into(),
            reserved_tokens: budget.map_or(0, |budget| budget.reserved_tokens()).into(),
            attempts_started: begun.into(),
            provider_attempts_started: counts[0].into(),
            business_attempts_started: counts[1].into(),
            other_attempts_started: counts[2].into(),
            wall_ms: super::wall_span_ms(&task_walls),
            attempts: views,
        });
        report.wall_rows.extend(task_walls);
    }
    Ok(report)
}

fn classify(operator: &CompiledOperator) -> (Category, Option<String>, Vec<String>) {
    match operator {
        CompiledOperator::ProviderAdmission { bindings }
        | CompiledOperator::ProviderAdmissionBrokered { bindings, .. } => {
            (Category::Provider, None, bindings.iter().cloned().collect())
        }
        CompiledOperator::ReviewDomain {
            review_node,
            operation,
        } => match operation {
            ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot } => (
                Category::Business,
                Some(review_node.clone()),
                vec![slot.clone()],
            ),
            _ => (Category::Other, Some(review_node.clone()), vec![]),
        },
        _ => (Category::Other, None, vec![]),
    }
}

fn outcome(attempt: &TaskAttemptAccounting) -> &'static str {
    match &attempt.result {
        Some(TaskAttemptResultV1::Failed { .. }) => "failed",
        Some(TaskAttemptResultV1::Abandoned { .. }) => "abandoned",
        Some(TaskAttemptResultV1::Succeeded { .. }) => match attempt.state {
            Some(
                review_attempt::AttemptState::Fenced | review_attempt::AttemptState::Quarantined,
            ) => "fenced",
            _ => "selected",
        },
        None if attempt.released => "released",
        None if attempt.started => "running",
        None => "reserved",
    }
}

pub(super) fn render(tasks: &[TaskAccountingView], markdown: bool) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    if tasks.is_empty() {
        return text;
    }
    writeln!(
        text,
        "{}Task accounting:",
        if markdown { "## " } else { "" }
    )
    .unwrap();
    for task in tasks {
        writeln!(text, "\n{}", task_summary(task)).unwrap();
        for attempt in &task.attempts {
            let name = attempt.review_node.as_deref().unwrap_or(&attempt.node);
            writeln!(
                text,
                "\n- {}{}: Attempt {}; {}; {} tokens (original cap {}){}{}",
                attempt
                    .round
                    .zip(attempt.epoch)
                    .map(|(round, epoch)| format!("Round {round} epoch {epoch}, "))
                    .unwrap_or_default(),
                name,
                attempt.attempt_id,
                attempt.outcome,
                attempt.chargeable_tokens.get(),
                attempt.reserved_tokens.get(),
                if attempt.binding_slots.is_empty() {
                    String::new()
                } else {
                    format!("; bindings {}", attempt.binding_slots.join(", "))
                },
                attempt
                    .wall
                    .as_ref()
                    .map(|wall| format!("; {}", super::human_duration(wall.elapsed_ms)))
                    .unwrap_or_default()
            )
            .unwrap();
            if let Some(usage) = attempt.wall.as_ref().and_then(|wall| wall.usage.as_ref()) {
                let kinds = [
                    ("in", usage.input_tokens),
                    ("out", usage.output_tokens),
                    ("cache-read", usage.cache_read_tokens),
                    ("cache-write", usage.cache_write_tokens),
                    ("reasoning", usage.reasoning_tokens),
                ]
                .into_iter()
                .filter_map(|(kind, tokens)| {
                    tokens.map(|tokens| format!("{kind} {}", tokens.get()))
                })
                .collect::<Vec<_>>();
                writeln!(
                    text,
                    "  Provider usage: chargeable {}{}",
                    usage.chargeable_tokens.get(),
                    if kinds.is_empty() {
                        String::new()
                    } else {
                        format!("; {}", kinds.join(", "))
                    }
                )
                .unwrap();
            }
        }
    }
    text
}

#[cfg(test)]
mod tests;
