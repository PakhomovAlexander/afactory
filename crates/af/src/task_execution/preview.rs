//! Plain ASCII views of captured plans. Rendering never resolves live packages or Providers.
use super::*;
use review_core::task::input_bindings::TaskInputBindingV1;
use review_core::task::pipeline::TaskOperatorV1;
use review_core::task::plan::WorkerExecutionV1;
use review_graph::task::{CompiledNode, CompiledOperator};
use std::fmt::Write;

const WIDTH: usize = 96;

// Task goals, model labels and referenced Task IDs are untrusted display data: no terminal
// controls, newlines, bidi controls or non-ASCII characters may impersonate another row or
// approval action.
pub(super) fn text(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}
/// The same sanitized text, bounded: every consumer of untrusted display data — including the
/// refusals `input_bindings` writes for a Task file's `inputs` table — goes through it.
pub(super) fn short(value: &str, limit: usize) -> String {
    let value = text(value);
    if value.len() <= limit {
        value
    } else {
        format!("{}...", &value[..limit.saturating_sub(3)])
    }
}
fn line(out: &mut String, value: &str) {
    let value = text(value);
    for chunk in value.as_bytes().chunks(WIDTH) {
        writeln!(out, "{}", std::str::from_utf8(chunk).expect("ASCII")).unwrap();
    }
}
/// A suggested shell command: sanitized like every row but never wrapped, so a copied command
/// keeps every token, its `\` continuation and an ID of any admitted length intact.
fn command(out: &mut String, value: &str) {
    writeln!(out, "{}", text(value)).unwrap();
}
/// The action that approves and runs exactly `plan_id` in the Store this preview read (the
/// `selectors` it was given), continued only between arguments.
fn run_action(out: &mut String, task_id: &str, plan_id: &str, selectors: &str) {
    line(out, "Approve and run this plan:");
    command(out, &format!("  af task run {task_id} \\"));
    if selectors.is_empty() {
        command(out, &format!("    --confirm-plan {plan_id}"));
    } else {
        command(out, &format!("    --confirm-plan {plan_id} \\"));
        command(out, &format!("   {selectors}"));
    }
}
fn details_hint(out: &mut String, task_id: &str, selectors: &str) {
    command(
        out,
        &format!("Details: af task explain {task_id} --tree{selectors} (or --json)"),
    );
}
pub(super) fn package_label(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    name: &str,
) -> Result<String, String> {
    let (name, version) = package_name_version(cas, plan, name)?;
    Ok(format!("{name}@{version}"))
}
/// A captured package's name and version, as sanitized display text.
pub(super) fn package_name_version(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    name: &str,
) -> Result<(String, String), String> {
    let dependency = plan
        .dependencies
        .get(name)
        .ok_or("Preview Pipeline dependency missing")?;
    let package: serde_json::Value = artifact(
        cas,
        &dependency.artifact_id,
        review_config::task::catalog::TASK_PACKAGE_V1,
    )?;
    let version = package["version"]
        .as_str()
        .ok_or("Preview package version missing")?;
    Ok((text(name), text(version)))
}
fn worker(node: &CompiledNode) -> Option<&str> {
    match &node.operator {
        CompiledOperator::Primitive {
            operator:
                TaskOperatorV1::Worker { slot }
                | TaskOperatorV1::Verify { slot }
                | TaskOperatorV1::FixVerify { slot },
            ..
        } => Some(slot),
        CompiledOperator::ReviewDomain {
            operation:
                review_graph::task::ReviewOperation::Reviewer { slot }
                | review_graph::task::ReviewOperation::Scatter { slot },
            ..
        } => Some(slot),
        _ => None,
    }
}
fn execution_label(execution: &WorkerExecutionV1) -> String {
    match execution {
        WorkerExecutionV1::Command {} => "command Worker".into(),
        WorkerExecutionV1::Model {
            model,
            effort,
            provider,
            ..
        } => format!("{}/{} via {}", text(model), text(effort), text(provider)),
    }
}
fn binding(plan: &ExecutionPlanV1, slot: &str) -> String {
    plan.bindings
        .get(slot)
        .map(|b| execution_label(&b.execution))
        .unwrap_or_else(|| "binding unavailable (inspect JSON)".into())
}
/// The slots an optimization experiment node runs as its baseline and candidate arms, each
/// qualified in the node's Pipeline scope as the experiment runtime qualifies them.
fn experiment_slots(graph: &CompiledTask) -> Vec<String> {
    let mut slots = Vec::new();
    for (id, node) in &graph.nodes {
        let CompiledOperator::Primitive {
            operator:
                TaskOperatorV1::OptimizationExperiment {
                    baseline_slot,
                    candidate_slot,
                },
            ..
        } = &node.operator
        else {
            continue;
        };
        let scope = id.split_once(".nodes.").map(|(scope, _)| scope);
        for slot in [baseline_slot, candidate_slot] {
            let qualified = match scope {
                Some(scope) if !graph.slots.contains_key(slot) => format!("{scope}.slots.{slot}"),
                _ => slot.clone(),
            };
            if !slots.contains(&qualified) {
                slots.push(qualified);
            }
        }
    }
    slots
}
/// The Attempt wall of a Worker slot: the longest `wall_ms_per_attempt` of the nodes its Worker
/// runs as, as the compiled graph's allowances captured it, and for an optimization experiment's
/// baseline or candidate arm the Attempt wall its captured Worker package declares, the wall
/// every arm's Attempt runs under. `None` for a slot no node runs.
pub(super) fn slot_wall(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    slot: &str,
) -> Option<u64> {
    let arm = experiment_slots(graph)
        .iter()
        .any(|arm| arm == slot)
        .then(|| plan.bindings.get(slot))
        .flatten()
        .and_then(|binding| {
            review_config::task::catalog::TaskPlanCompiler::captured_worker_package(
                cas,
                &binding.package_artifact_id,
            )
            .ok()
        })
        .and_then(|package| package.worker.signature.attempt)
        .map(|attempt| attempt.wall_ms)
        .filter(|wall| *wall > 0);
    graph
        .nodes
        .iter()
        .filter(|(_, node)| worker(node) == Some(slot))
        .filter_map(|(id, _)| graph.allowances.get(id))
        .map(|allowance| allowance.wall_ms_per_attempt)
        .chain(arm)
        .max()
}
/// A slot's binding and its Attempt wall, as the plan preview prints them side by side.
fn binding_and_wall(cas: &Cas, plan: &ExecutionPlanV1, graph: &CompiledTask, slot: &str) -> String {
    match slot_wall(cas, plan, graph, slot) {
        Some(wall) => format!(
            "{}, Attempt wall {}",
            binding(plan, slot),
            super::state::duration_words(wall)
        ),
        None => binding(plan, slot),
    }
}
/// The slots the plan summary lists, and how many it folds: every slot a node runs, each with
/// its Attempt wall however many there are, then bound slots no node runs up to eight lines in
/// all. `slots` pairs each slot, in preview order, with whether a node runs it.
fn listed_slots(slots: Vec<(String, bool)>) -> (Vec<String>, usize) {
    let (runs, idle): (Vec<_>, Vec<_>) = slots.into_iter().partition(|(_, runs)| *runs);
    let shown = 8usize.saturating_sub(runs.len()).min(idle.len());
    let folded = idle.len() - shown;
    let listed = runs
        .into_iter()
        .chain(idle.into_iter().take(shown))
        .map(|(slot, _)| slot)
        .collect();
    (listed, folded)
}
/// Every Worker slot of the plan, in the order the preview lists them: the slots of nodes in
/// graph order, an optimization experiment's arms, then every other bound slot.
fn ordered_slots(plan: &ExecutionPlanV1, graph: &CompiledTask) -> Vec<String> {
    let mut slots: Vec<String> = Vec::new();
    let mut push = |slot: &str| {
        if !slots.iter().any(|known| known == slot) {
            slots.push(slot.to_owned());
        }
    };
    for id in &graph.order {
        if let Some(slot) = graph.nodes.get(id).and_then(worker) {
            push(slot);
        }
    }
    for slot in experiment_slots(graph) {
        push(&slot);
    }
    for slot in plan.bindings.keys() {
        push(slot);
    }
    slots
}
/// Each Worker slot of the plan with its binding and its Attempt wall, the `slots` member of
/// the plan's `--json` document (ADR-0253), in the order the preview lists them.
pub(super) fn slots_value(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
) -> serde_json::Value {
    let slots = ordered_slots(plan, graph);
    serde_json::Value::Array(
        slots
            .into_iter()
            .map(|slot| {
                let mut row = json!({
                    "slot": slot,
                    "worker": graph.slots.get(&slot).map(|s| s.worker.clone()),
                    "attempt_wall_ms": slot_wall(cas, plan, graph, &slot),
                });
                match plan.bindings.get(&slot).map(|b| &b.execution) {
                    Some(WorkerExecutionV1::Model {
                        provider,
                        model,
                        effort,
                        ..
                    }) => {
                        row["execution"] = json!("model");
                        row["provider"] = json!(provider);
                        row["model"] = json!(model);
                        row["effort"] = json!(effort);
                    }
                    Some(WorkerExecutionV1::Command {}) => row["execution"] = json!("command"),
                    None => row["execution"] = json!(null),
                }
                row
            })
            .collect(),
    )
}
/// A slot's Worker package and binding, as the browser's status line names a highlighted row.
fn slot_binding(plan: &ExecutionPlanV1, graph: &CompiledTask, slot: &str) -> String {
    let name = text(slot.strip_prefix("root.slots.").unwrap_or(slot));
    let worker = graph.slots.get(slot).map(|slot| slot.worker.as_str());
    let worker = text(worker.unwrap_or("?"));
    let bound = binding(plan, slot);
    format!("{name}: {worker} -> {bound}")
}
fn compact_visible(node: &CompiledNode) -> bool {
    !matches!(
        node.operator,
        CompiledOperator::RootInputs
            | CompiledOperator::Select
            | CompiledOperator::Primitive {
                operator: TaskOperatorV1::Seal {}
                    | TaskOperatorV1::Accept {}
                    | TaskOperatorV1::ReviewBind {}
                    | TaskOperatorV1::ReviewReduce {}
                    | TaskOperatorV1::ReviewAccept {}
                    | TaskOperatorV1::DocumentSeal {}
                    | TaskOperatorV1::DocumentAccept {}
                    | TaskOperatorV1::ReportSeal {}
                    | TaskOperatorV1::ReportAccept {},
                ..
            }
    )
}

fn node_label(
    cas: &Cas,
    id: &str,
    node: &CompiledNode,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
) -> String {
    let name = id.rsplit('.').next().unwrap_or(id);
    // A Worker's binding beside its Attempt wall, as the summary lists it (ADR-0253).
    let detail = if let Some(slot) = worker(node) {
        binding_and_wall(cas, plan, graph, slot)
    } else {
        match &node.operator {
            CompiledOperator::ProviderAdmission { .. } => "Provider admission (budgeted)".into(),
            CompiledOperator::Primitive { operator, .. } => match operator {
                TaskOperatorV1::Check {
                    checks,
                    remote_checks,
                } => {
                    let list = |names: &BTreeSet<String>| {
                        names.iter().cloned().collect::<Vec<_>>().join(", ")
                    };
                    // Where each check runs is the pipeline's choice, so the preview says it.
                    match (checks.is_empty(), remote_checks.is_empty()) {
                        (_, true) => format!("checks: {}", list(checks)),
                        (true, false) => format!("remote checks: {}", list(remote_checks)),
                        (false, false) => format!(
                            "checks: {}; remote checks: {}",
                            list(checks),
                            list(remote_checks)
                        ),
                    }
                }
                TaskOperatorV1::Measure { measures } => format!(
                    "measure: {}",
                    measures
                        .iter()
                        .map(|m| text(m))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                TaskOperatorV1::Compare { objective } => {
                    format!("compare: {}", text(objective))
                }
                // Each arm's Worker with the Attempt wall its every Attempt runs under.
                TaskOperatorV1::OptimizationExperiment { .. } => {
                    let scope = id.split_once(".nodes.").map(|(scope, _)| scope);
                    let arms = experiment_slots(graph)
                        .into_iter()
                        .filter(|slot| scope.is_none_or(|scope| slot.starts_with(scope)))
                        .map(|slot| {
                            format!(
                                "{}: {}",
                                slot.rsplit('.').next().unwrap_or(&slot),
                                binding_and_wall(cas, plan, graph, &slot)
                            )
                        })
                        .collect::<Vec<_>>();
                    format!("optimization experiment ({})", arms.join("; "))
                }
                _ => serde_json::to_value(operator).expect("operator serializes")["op"]
                    .as_str()
                    .unwrap_or("operation")
                    .to_owned(),
            },
            CompiledOperator::Select => "select by outcome".into(),
            CompiledOperator::ReviewDomain { operation, .. } => serde_json::to_value(operation)
                .expect("operation serializes")["kind"]
                .as_str()
                .unwrap_or("review operation")
                .to_owned(),
            CompiledOperator::ReviewIntegrationChecks { .. } => "integration checks".into(),
            CompiledOperator::RootInputs => "captured inputs".into(),
        }
    };
    let conditions = node
        .conditions
        .iter()
        .map(|c| {
            format!(
                "{}={}",
                c.source
                    .node
                    .strip_prefix("root.")
                    .unwrap_or(&c.source.node),
                serde_json::to_value(c.outcome)
                    .expect("outcome serializes")
                    .as_str()
                    .unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}  {}{}",
        text(name),
        detail,
        if conditions.is_empty() {
            String::new()
        } else {
            format!(" [if {}]", text(&conditions))
        }
    )
}

// Collapse a child's expanded nodes into its real call address, preserving first appearance
// in the compiler's topological order. No inferred Pipeline name or invented stage is used.
fn children(graph: &CompiledTask, parent: &str) -> Vec<String> {
    let prefix = format!("{parent}.");
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for id in &graph.order {
        if !id.starts_with(&prefix)
            || matches!(graph.nodes[id].operator, CompiledOperator::RootInputs)
        {
            continue;
        }
        let child = graph
            .calls
            .keys()
            .filter(|call| call.starts_with(&prefix) && id.starts_with(&format!("{call}.")))
            .min_by_key(|call| call.len())
            .unwrap_or(id);
        if seen.insert(child.clone()) {
            result.push(child.clone());
        }
    }
    result
}
/// Line index of every row that names a Worker slot, and that slot's binding.
pub(super) type SlotMarks = Vec<(usize, String)>;

#[allow(clippy::too_many_arguments)]
fn tree_rows(
    out: &mut String,
    cas: &Cas,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    parent: &str,
    prefix: &str,
    marks: &mut SlotMarks,
) -> Result<(), String> {
    let rows = children(graph, parent);
    for (i, id) in rows.iter().enumerate() {
        let last = i + 1 == rows.len();
        let branch = if last { "'-- " } else { "+-- " };
        if let Some(call) = graph.calls.get(id) {
            line(
                out,
                &format!(
                    "{prefix}{branch}{}: call {}",
                    id.rsplit('.').next().unwrap_or(id),
                    package_label(cas, plan, &call.pipeline)?
                ),
            );
            tree_rows(
                out,
                cas,
                plan,
                graph,
                id,
                &format!("{prefix}{}", if last { "    " } else { "|   " }),
                marks,
            )?;
        } else {
            if let Some(slot) = worker(&graph.nodes[id]) {
                let at = out.matches('\n').count();
                marks.push((at, slot_binding(plan, graph, slot)));
            }
            line(
                out,
                &format!(
                    "{prefix}{branch}{}",
                    node_label(cas, id, &graph.nodes[id], plan, graph)
                ),
            );
        }
    }
    Ok(())
}

type BindingRows = std::collections::BTreeMap<String, TaskInputBindingV1>;

/// `task <id>/<port>` or `artifact` — the untrusted half of every binding row, short enough
/// that an exact artifact ID printed beside it still fits one line.
fn binding_origin(binding: &TaskInputBindingV1) -> String {
    let Some(task) = &binding.task else {
        return "artifact".into();
    };
    let id = short(&task.task_id, 40);
    let port = short(&task.port, 24);
    format!("task {id}/{port}")
}

pub(super) fn acceptance_name(value: TaskAcceptanceV1) -> &'static str {
    match value {
        TaskAcceptanceV1::Satisfied => "satisfied",
        TaskAcceptanceV1::Unsatisfied => "unsatisfied",
        TaskAcceptanceV1::Inconclusive => "inconclusive",
    }
}

/// The `IN` row: every planned input port, annotated where a binding replaced this adapter's
/// construction of it.
fn input_labels<'a>(ports: impl Iterator<Item = &'a String>, bound: &BindingRows) -> String {
    let mut labels = Vec::new();
    for name in ports {
        let Some(binding) = bound.get(name) else {
            labels.push(text(name));
            continue;
        };
        let origin = match &binding.task {
            Some(_) => bound_outputs(binding)
                .map(binding_origin)
                .collect::<Vec<_>>()
                .join(" + "),
            None => format!("artifact {}", text(&binding.artifact_id)),
        };
        labels.push(format!("{} <- {origin}", text(name)));
    }
    labels.join(", ")
}

/// Every recorded output one port was bound from, in Task-file order: the binding itself, then
/// each further output a `many` port gathered (ADR-0134).
pub(super) fn bound_outputs(
    binding: &TaskInputBindingV1,
) -> impl Iterator<Item = &TaskInputBindingV1> {
    std::iter::once(binding).chain(&binding.also)
}

/// One `BOUND` group per bound output for `--tree`: the referenced Task, its output port, the
/// acceptance and domain conclusion it had, and the exact artifact ID. An exact-artifact
/// binding reads `artifact` in place of the Task and port and `-` where no Task supplied an
/// acceptance or a conclusion. The digest gets its own row so no wrap can split it. A port
/// bound from several outputs has one group per output, each in the same shape.
fn binding_rows(bound: &BindingRows) -> Vec<String> {
    let mut rows = Vec::new();
    let outputs = bound
        .iter()
        .flat_map(|(name, binding)| bound_outputs(binding).map(move |output| (name, output)));
    for (name, binding) in outputs {
        let mut acceptance = "-".to_owned();
        let mut conclusion = "-".to_owned();
        if let Some(task) = &binding.task {
            acceptance = acceptance_name(task.acceptance).to_owned();
            conclusion = short(&task.domain_conclusion, 24);
        }
        rows.push(format!(
            "BOUND {} <- {} ({acceptance}/{conclusion})",
            text(name),
            binding_origin(binding)
        ));
        rows.push(format!("      {}", text(&binding.artifact_id)));
        if let Some(resolved) = &binding.resolved_artifact_id {
            rows.push(format!("      re-rooted {}", text(resolved)));
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    cas: &Cas,
    revision: &TaskRevisionV1,
    plan_id: &str,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    status: &str,
    tree: bool,
) -> Result<String, String> {
    let clock = TaskClock::Deadline(plan.limits.deadline_unix_ms);
    render_marked(cas, revision, plan_id, plan, graph, status, tree, clock).map(|(text, _)| text)
}

/// What the preview's `TIME` row states: the wall a Task that has not run yet will have once its
/// clock starts at its first run, or the deadline of one that ran.
#[derive(Clone, Copy)]
enum TaskClock {
    NotStarted { wall_ms: u64 },
    Deadline(u64),
}

/// The preview, and the lines of its tree that name a Worker slot.
#[allow(clippy::too_many_arguments)]
fn render_marked(
    cas: &Cas,
    revision: &TaskRevisionV1,
    plan_id: &str,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    status: &str,
    tree: bool,
    clock: TaskClock,
) -> Result<(String, SlotMarks), String> {
    let mut marks = SlotMarks::new();
    if graph.order.iter().any(|id| !graph.nodes.contains_key(id)) {
        return Err("Captured graph order references a missing node".into());
    }
    let mut out = String::new();
    let root = graph
        .calls
        .get("root")
        .ok_or("Preview has no captured root Pipeline")?;
    let origin = if plan.preparation.is_some() {
        "planner preparation"
    } else if plan.requires_developer_approval() {
        "generated; signed approval enforced"
    } else {
        "configured"
    };
    line(
        &mut out,
        &format!(
            "TASK  {}: {}",
            short(&revision.task_id, 36),
            short(&revision.goal, 52)
        ),
    );
    line(
        &mut out,
        &format!(
            "PIPE  {}  [{}]",
            package_label(cas, plan, &root.pipeline)?,
            origin
        ),
    );
    line(&mut out, &format!("PLAN  {plan_id}"));
    line(&mut out, &format!("STATE {status}"));
    out.push('\n');
    if tree {
        tree_rows(&mut out, cas, plan, graph, "root", "", &mut marks)?;
    } else {
        let all_rows = children(graph, "root");
        let rows: Vec<_> = all_rows
            .iter()
            .filter(|id| {
                graph.calls.contains_key(*id) || graph.nodes.get(*id).is_some_and(compact_visible)
            })
            .cloned()
            .collect();
        let labels: Vec<_> =
            rows.iter()
                .take(12)
                .map(|id| {
                    let conditional = graph
                        .nodes
                        .get(id)
                        .is_some_and(|n| !n.conditions.is_empty())
                        || graph.nodes.iter().any(|(n, v)| {
                            n.starts_with(&format!("{id}.")) && !v.conditions.is_empty()
                        });
                    format!(
                        "{}{}{}",
                        if graph.calls.contains_key(id) {
                            "call "
                        } else {
                            ""
                        },
                        id.rsplit('.').next().unwrap_or(id),
                        if conditional { "?" } else { "" }
                    )
                })
                .collect();
        line(
            &mut out,
            "FLOW  dependency order; ? = conditional work (see --tree)",
        );
        let mut row = String::new();
        for label in labels {
            if !row.is_empty() && row.len() + label.len() + 4 > WIDTH {
                line(&mut out, &row);
                row = "  -> ".into();
            } else if !row.is_empty() {
                row.push_str(" -> ");
            }
            row.push_str(&label);
        }
        line(&mut out, &row);
        if rows.len() > 12 {
            line(
                &mut out,
                &format!("... {} more stages; use --tree", rows.len() - 12),
            );
        }
        if all_rows.len() > rows.len() {
            line(
                &mut out,
                &format!(
                    "  {} root internal steps folded; --tree expands all calls",
                    all_rows.len() - rows.len()
                ),
            );
        }
        // Each slot's Attempt wall beside its model: how long one Attempt may take (ADR-0253).
        // Every slot a node runs is listed with its wall; only bound slots no node runs fold.
        let slots = ordered_slots(plan, graph)
            .into_iter()
            .map(|slot| {
                let runs = slot_wall(cas, plan, graph, &slot).is_some();
                (slot, runs)
            })
            .collect();
        let (listed, folded) = listed_slots(slots);
        for slot in &listed {
            line(
                &mut out,
                &format!(
                    "  {}: {}",
                    slot.strip_prefix("root.slots.").unwrap_or(slot),
                    binding_and_wall(cas, plan, graph, slot)
                ),
            );
        }
        if folded > 0 {
            line(
                &mut out,
                &format!("  ... {folded} more bindings; use --tree or --json"),
            );
        }
    }
    out.push('\n');
    // A bound port is annotated where its name is already printed, so an operator reading the
    // plan sees that `source` came from another Task rather than from this checkout.
    let recorded = super::input_bindings::recorded(cas, revision)?;
    let bound = recorded.map_or_else(BindingRows::new, |r| r.bindings);
    let labels = input_labels(plan.inputs.keys(), &bound);
    line(&mut out, &format!("IN    {labels}"));
    // Each Project Brief port: its Brief, its size and staleness, or why it carries none.
    // Never wrapped, so the Brief's ID stays whole.
    for brief in crate::project_brief::preview_lines(cas, revision)? {
        command(&mut out, &format!("      {brief}"));
    }
    if tree {
        for row in binding_rows(&bound) {
            line(&mut out, &row);
        }
    }
    line(
        &mut out,
        &format!(
            "OUT   {}",
            graph.outputs.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    );
    line(
        &mut out,
        &format!(
            "CAP   {} tokens total | {} reserved for verification | {} attempts",
            plan.limits.tokens, plan.limits.verification.tokens, plan.limits.max_attempts
        ),
    );
    match clock {
        TaskClock::NotStarted { wall_ms } => line(
            &mut out,
            &format!(
                "TIME  {} wall | deadline starts when you run",
                super::state::duration_words(wall_ms)
            ),
        ),
        TaskClock::Deadline(deadline) => {
            let left = deadline.saturating_sub(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            );
            line(
                &mut out,
                &format!(
                    "TIME  {}s remaining | deadline {}{}",
                    left / 1000,
                    super::state::local_time(deadline),
                    if left == 0 { " [EXPIRED]" } else { "" }
                ),
            );
        }
    }
    line(
        &mut out,
        &format!(
            "EFFECTS {}",
            if plan.authority.allowed_effects.is_empty() {
                "none".into()
            } else {
                plan.authority
                    .allowed_effects
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    );
    line(
        &mut out,
        &format!(
            "SEND  {}",
            if plan.authority.data_destinations.is_empty() {
                "none".into()
            } else {
                plan.authority
                    .data_destinations
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    );
    line(
        &mut out,
        "Delivery is a separate, explicitly confirmed action.",
    );
    // The Store this preview read, as its command was told to find it.
    let selectors = super::store_selectors();
    if status == "finished" || status == "historical plan (not current)" {
        line(&mut out, "Recorded plan only; this preview starts no work.");
    } else {
        if plan.requires_developer_approval() && status == "needs signed plan approval" {
            line(
                &mut out,
                "First inspect and sign this exact generated plan with task decision-payload / approve.",
            );
        }
        run_action(&mut out, &revision.task_id, plan_id, &selectors);
    }
    details_hint(&mut out, &revision.task_id, &selectors);
    Ok((out, marks))
}

pub(super) fn current(cas: &Cas, state: &TaskProjection, tree: bool) -> Result<String, String> {
    current_marked(cas, state, tree).map(|(text, _)| text)
}

/// The current plan's preview, and the lines of its tree that name a Worker slot.
pub(super) fn current_marked(
    cas: &Cas,
    state: &TaskProjection,
    tree: bool,
) -> Result<(String, SlotMarks), String> {
    let id = state
        .plan_id
        .as_deref()
        .ok_or("Task has no captured plan")?;
    let plan: ExecutionPlanV1 = artifact(cas, id, EXECUTION_PLAN_V1)?;
    let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
    let status = if matches!(state.phase, TaskPhaseV1::Finished { .. }) {
        "finished"
    } else if state.phase
        == (TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsPlanReview,
        })
    {
        "needs signed plan approval"
    } else if state.admitted {
        "admitted; resume captured execution"
    } else {
        "confirmation required; no new Worker dispatched"
    };
    // A plan that never ran has its whole wall ahead of it: the clock starts at its first run.
    let clock = match state.clock_started_unix_ms() {
        None => TaskClock::NotStarted {
            wall_ms: state.wall_ms(),
        },
        Some(_) => TaskClock::Deadline(state.deadline_unix_ms()),
    };
    render_marked(cas, &state.revision, id, &plan, &graph, status, tree, clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Every slot a node runs is listed with its Attempt wall, a ninth one included; only bound
    /// slots no node runs fold behind `use --tree or --json` (ADR-0253).
    #[test]
    fn every_executable_slot_is_listed_and_only_idle_bindings_fold() {
        let slot = |n: usize, runs: bool| (format!("root.slots.w{n}"), runs);
        let nine: Vec<_> = (1..=9).map(|n| slot(n, true)).collect();
        let (listed, folded) = listed_slots(nine);
        assert_eq!(listed.len(), 9);
        assert_eq!(listed.last().map(String::as_str), Some("root.slots.w9"));
        assert_eq!(folded, 0);
        let mut mixed: Vec<_> = (1..=3).map(|n| slot(n, true)).collect();
        mixed.extend((4..=10).map(|n| slot(n, false)));
        let (listed, folded) = listed_slots(mixed);
        assert_eq!(listed.len(), 8);
        assert!(
            listed[..3]
                .iter()
                .all(|s| ["w1", "w2", "w3"].iter().any(|w| s.ends_with(w)))
        );
        assert_eq!(folded, 2);
        let mut runs_last: Vec<_> = (1..=9).map(|n| slot(n, false)).collect();
        runs_last.push(slot(10, true));
        let (listed, folded) = listed_slots(runs_last);
        assert_eq!(
            listed[0], "root.slots.w10",
            "a running slot is never folded"
        );
        assert_eq!((listed.len(), folded), (8, 2));
    }
    #[test]
    fn model_display_uses_the_effective_model_effort_and_account() {
        let execution = WorkerExecutionV1::Model {
            provider: "claude-personal".into(),
            provider_kind: "claude".into(),
            principal_id: "private-account-identity".into(),
            model: "claude-opus-5".into(),
            effort: "xhigh".into(),
        };
        assert_eq!(
            execution_label(&execution),
            "claude-opus-5/xhigh via claude-personal"
        );
        assert!(!execution_label(&execution).contains("private-account-identity"));
        assert_eq!(
            execution_label(&WorkerExecutionV1::Command {}),
            "command Worker"
        );
    }

    #[test]
    fn display_data_cannot_inject_terminal_actions() {
        let hostile = "ticket\nAPPROVED\u{1b}[2J\u{202e}run";
        let shown = text(hostile);
        assert!(shown.is_ascii());
        assert!(!shown.contains('\n') && !shown.contains('\u{1b}'));
        assert_eq!(short("abcdefgh", 6), "abc...");
        let mut out = String::new();
        line(&mut out, &"x".repeat(250));
        assert!(out.lines().all(|s| s.len() <= WIDTH));
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    /// The shell tokens of the command that starts on the row holding `start`, following `\`
    /// continuations the way a shell joins copied lines.
    fn copied(out: &str, start: &str) -> Vec<String> {
        let mut rows = out.lines().skip_while(|row| !row.contains(start));
        let mut tokens = Vec::new();
        for row in rows.by_ref() {
            let row = &row[row.find("af task").unwrap_or(0)..];
            let (row, continued) = match row.strip_suffix(" \\") {
                Some(row) => (row, true),
                None => (row, false),
            };
            tokens.extend(row.split_whitespace().map(str::to_owned));
            if !continued {
                break;
            }
        }
        tokens
    }

    #[test]
    fn suggested_commands_keep_every_token_of_short_and_maximum_length_ids() {
        let plan = digest('p');
        let longest = "a".repeat(128);
        assert!(review_core::task::is_name(&longest));
        for id in ["pagination-cli", "x", longest.as_str()] {
            let mut out = String::new();
            run_action(&mut out, id, &plan, "");
            details_hint(&mut out, id, "");
            assert_eq!(
                copied(&out, "af task run"),
                ["af", "task", "run", id, "--confirm-plan", &plan]
            );
            assert_eq!(
                copied(&out, "af task explain"),
                ["af", "task", "explain", id, "--tree", "(or", "--json)"]
            );
            // Every row ending in `\` is a continuation of the command, never a split token.
            let continued: Vec<_> = out.lines().filter(|row| row.ends_with('\\')).collect();
            assert_eq!(continued, [format!("  af task run {id} \\")]);
            let prose = out.lines().filter(|row| !row.contains("af task"));
            assert!(prose.into_iter().all(|row| row.len() <= WIDTH));

            // Given selectors, both commands carry them: run as printed, they read this Store.
            let selectors = " --repo ../repo --state ../state";
            let mut out = String::new();
            run_action(&mut out, id, &plan, selectors);
            details_hint(&mut out, id, selectors);
            assert_eq!(
                copied(&out, "af task run"),
                [
                    "af",
                    "task",
                    "run",
                    id,
                    "--confirm-plan",
                    &plan,
                    "--repo",
                    "../repo",
                    "--state",
                    "../state"
                ]
            );
            assert_eq!(
                copied(&out, "af task explain")[..9],
                [
                    "af", "task", "explain", id, "--tree", "--repo", "../repo", "--state",
                    "../state"
                ]
            );
            assert!(!out.contains("same --repo"), "{out}");
        }
    }

    #[test]
    fn unwrapped_commands_are_still_sanitized() {
        let mut out = String::new();
        run_action(
            &mut out,
            "ticket\nAPPROVED\u{1b}[2J\u{202e}run",
            &digest('p'),
            "",
        );
        details_hint(&mut out, "x\r\u{7}y", "");
        assert!(out.is_ascii() && !out.contains('\u{1b}') && !out.contains('\r'));
        assert!(out.contains("  af task run ticket?APPROVED?[2J?run \\\n"));
        assert!(out.ends_with("Details: af task explain x??y --tree (or --json)\n"));
        assert_eq!(out.lines().count(), 4);
    }

    /// One exact-artifact binding and one Task binding whose Task ID is hostile display data
    /// and whose derived source had to be re-rooted.
    fn bound() -> BindingRows {
        use review_core::task::input_bindings::ReferencedTaskV1;
        let exact = TaskInputBindingV1 {
            artifact_id: digest('a'),
            resolved_artifact_id: None,
            snapshot_id: None,
            rerooted_snapshot_id: None,
            task: None,
            also: Vec::new(),
        };
        let referenced = TaskInputBindingV1 {
            artifact_id: digest('b'),
            resolved_artifact_id: Some(digest('c')),
            snapshot_id: Some(digest('d')),
            rerooted_snapshot_id: Some(digest('e')),
            task: Some(ReferencedTaskV1 {
                task_id: "l3b\nOK\u{1b}[2J\u{202e}x".into(),
                task_revision_id: digest('1'),
                result_id: digest('2'),
                port: "snapshot".into(),
                acceptance: TaskAcceptanceV1::Unsatisfied,
                domain_conclusion: "changes_requested".into(),
            }),
            also: Vec::new(),
        };
        BindingRows::from([
            ("history".to_owned(), exact),
            ("source".to_owned(), referenced),
        ])
    }

    #[test]
    fn bound_ports_are_annotated_and_sanitized_in_text_and_tree() {
        let bound = bound();
        let ports = ["history", "requirements", "source"].map(str::to_owned);
        let labels = input_labels(ports.iter(), &bound);
        let hostile = "l3b?OK?[2J?x";
        let a = digest('a');
        let tail = format!("source <- task {hostile}/snapshot");
        let want = format!("history <- artifact {a}, requirements, {tail}");
        assert_eq!(labels, want);

        let rows = binding_rows(&bound);
        let row = format!("BOUND source <- task {hostile}/snapshot");
        let expected = vec![
            "BOUND history <- artifact (-/-)".to_owned(),
            format!("      {}", digest('a')),
            format!("{row} (unsatisfied/changes_requested)"),
            format!("      {}", digest('b')),
            format!("      re-rooted {}", digest('c')),
        ];
        assert_eq!(rows, expected);

        // Every row still goes through the sanitizer and the wrap bound.
        let mut out = String::new();
        line(&mut out, &labels);
        for row in &rows {
            line(&mut out, row);
        }
        assert!(out.is_ascii() && !out.contains('\u{1b}'));
        assert!(out.lines().all(|row| row.len() <= WIDTH));
        // The exact artifact ID is never split across the wrap boundary.
        assert!(out.lines().any(|row| row.trim() == digest('b')));
    }

    /// A `many` port bound from several outputs (ADR-0134) is annotated with every output on
    /// the `IN` row and gets one `BOUND` group per output, each in the single-output shape.
    #[test]
    fn a_port_bound_from_several_outputs_shows_each_output_in_the_same_shape() {
        use review_core::task::input_bindings::ReferencedTaskV1;
        let output = |byte: char, port: &str| TaskInputBindingV1 {
            artifact_id: digest(byte),
            resolved_artifact_id: None,
            snapshot_id: Some(digest('f')),
            rerooted_snapshot_id: None,
            task: Some(ReferencedTaskV1 {
                task_id: "experiment".into(),
                task_revision_id: digest('1'),
                result_id: digest('2'),
                port: port.into(),
                acceptance: TaskAcceptanceV1::Satisfied,
                domain_conclusion: "verified".into(),
            }),
            also: Vec::new(),
        };
        let mut measurements = output('a', "baseline");
        measurements.also.push(output('b', "candidate"));
        let bound = BindingRows::from([
            ("comparison".to_owned(), output('c', "comparison")),
            ("measurements".to_owned(), measurements),
        ]);
        let ports = ["comparison", "measurements", "requirements"].map(str::to_owned);
        assert_eq!(
            input_labels(ports.iter(), &bound),
            "comparison <- task experiment/comparison, measurements <- task \
             experiment/baseline + task experiment/candidate, requirements"
        );
        assert_eq!(
            binding_rows(&bound),
            vec![
                "BOUND comparison <- task experiment/comparison (satisfied/verified)".to_owned(),
                format!("      {}", digest('c')),
                "BOUND measurements <- task experiment/baseline (satisfied/verified)".to_owned(),
                format!("      {}", digest('a')),
                "BOUND measurements <- task experiment/candidate (satisfied/verified)".to_owned(),
                format!("      {}", digest('b')),
            ]
        );
    }
}
