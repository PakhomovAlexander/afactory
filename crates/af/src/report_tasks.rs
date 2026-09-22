//! Review inspection reads the common Task ledger once per Task. RunReport@6 values are
//! frozen cumulative snapshots, never Round costs.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::task::usage::{DecimalU64, DecimalU128, TaskTokenUsageV3};
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
    /// The warm layers this Attempt's review node used and what its bound context cost,
    /// joined from the Round's `WarmSetSelected@1`; absent for cold nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    warm: Option<super::AttemptWarmView>,
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
    usage: Option<TaskTokenUsageV3>,
}

pub(super) struct TaskAccountingReport {
    pub tasks: Vec<TaskAccountingView>,
    pub wall_rows: Vec<TaskAttemptWall>,
}

impl TaskAccountingReport {
    pub fn has_wide_usage(&self) -> bool {
        self.wall_rows
            .iter()
            .filter_map(|w| w.usage.as_ref())
            .any(|u| review_core::task::usage::TaskTokenUsageV2::try_from(u).is_err())
    }

    /// Merge raw Attempt intervals by their original Round/epoch. Neither cumulative report
    /// snapshots nor overlapping intervals are added as separate durations.
    pub fn wall_ms(&self) -> Option<u64> {
        super::wall_spans_ms(self.wall_rows.iter().map(|row| {
            (
                (row.round, row.epoch),
                (row.started_unix_ms, row.elapsed_ms),
            )
        }))
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
    let warm_selections = warm_selections(events)?;
    let workspace_preparations = workspace_preparations(events)?;
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
            let warm = review_node
                .as_deref()
                .zip(plan.round.as_ref())
                .and_then(|(node, round)| {
                    warm_selections.get(&(round.round, round.epoch, node.to_string()))
                })
                .map(|layers| {
                    let size = attempt
                        .context_id
                        .as_deref()
                        .and_then(|id| bound_context_size(cas, id));
                    let workspace = review_node
                        .as_deref()
                        .zip(plan.round.as_ref())
                        .and_then(|(node, round)| {
                            workspace_preparations.get(&(
                                round.round,
                                round.epoch,
                                node.to_string(),
                            ))
                        })
                        .cloned();
                    super::AttemptWarmView {
                        layers: layers.clone(),
                        rendered_bytes: size.map(|size| size.0),
                        estimated_tokens: size.map(|size| size.1),
                        workspace,
                    }
                });
            views.push(TaskAttemptView {
                warm,
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

/// `WarmSetSelected@1` layers by (Round, epoch, review node), read from the Campaign log so a
/// Task-backed review Attempt reports its warm layers exactly as a legacy one does.
type WarmSelections = BTreeMap<(u32, u32, String), Vec<String>>;

/// `WorkspaceRebased@1` by (Round, epoch, review node): what preparing the node's Warm
/// Workspace for that Round did and cost. Preparation precedes every Attempt of the Round, so
/// the report shows it once per Attempt view without adding it to any wall clock.
type WorkspacePreparations = BTreeMap<(u32, u32, String), super::WorkspacePreparationView>;

fn workspace_preparations(
    events: &[review_core::RunEvent],
) -> Result<WorkspacePreparations, String> {
    let mut rounds: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut preparations = BTreeMap::new();
    for event in events {
        match event.event_type {
            review_core::EventType::RoundStartedV1 => {
                let payload: review_core::RoundStartedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                rounds.insert(event.event_id.clone(), (payload.round, payload.epoch));
            }
            review_core::EventType::WorkspaceRebasedV1 => {
                let payload: review_core::WorkspaceRebasedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let Some((round, epoch)) = event
                    .causation_id
                    .as_deref()
                    .and_then(|round| rounds.get(round))
                    .copied()
                else {
                    continue;
                };
                let basis = serde_json::to_value(payload.basis)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let fallback = payload
                    .fallback
                    .and_then(|reason| serde_json::to_value(reason).ok())
                    .and_then(|value| value.as_str().map(str::to_owned));
                preparations.insert(
                    (round, epoch, payload.node.clone()),
                    super::WorkspacePreparationView {
                        basis,
                        fallback,
                        entries_touched: payload.entries_touched,
                        preparation_ms: payload.preparation_ms,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(preparations)
}

fn warm_selections(events: &[review_core::RunEvent]) -> Result<WarmSelections, String> {
    let mut rounds: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut selections = BTreeMap::new();
    for event in events {
        match event.event_type {
            review_core::EventType::RoundStartedV1 => {
                let payload: review_core::RoundStartedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                rounds.insert(event.event_id.clone(), (payload.round, payload.epoch));
            }
            review_core::EventType::WarmSetSelectedV1 => {
                let payload: review_core::WarmSetSelectedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let Some((round, epoch)) = event
                    .causation_id
                    .as_deref()
                    .and_then(|round| rounds.get(round))
                    .copied()
                else {
                    continue;
                };
                let node = event
                    .node_id
                    .clone()
                    .ok_or("WarmSetSelected@1 has no reviewer node")?;
                selections.insert(
                    (round, epoch, node),
                    payload
                        .layers
                        .iter()
                        .map(|layer| layer.as_str().to_string())
                        .collect(),
                );
            }
            _ => {}
        }
    }
    Ok(selections)
}

/// Rendered bytes and estimated tokens of an Attempt's bound Task context, whichever way the
/// context was stored: a captured Review Attempt's `af/TaskReviewContext@1` names its manifest
/// by `context_manifest_id`, a generic `af/TaskContext@1` carries it inline as `manifest`.
fn bound_context_size(cas: &Cas, context_id: &str) -> Option<(u64, u64)> {
    let context = match cas.get_optional_artifact(context_id).ok()? {
        Some(envelope) => envelope.payload,
        None => cas.get_json(context_id).ok()?,
    };
    let manifest = match context
        .get("context_manifest_id")
        .and_then(|id| id.as_str())
    {
        Some(manifest_id) => cas.get_json(manifest_id).ok()?,
        None => context.get("manifest")?.clone(),
    };
    Some((
        manifest.get("rendered_bytes")?.as_u64()?,
        manifest.get("estimated_tokens")?.as_u64()?,
    ))
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
                "\n- {}{}: Attempt {}; {}; {} tokens (original cap {}){}{}{}",
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
                    .unwrap_or_default(),
                super::attempt_warm_suffix(attempt.warm.as_ref())
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
