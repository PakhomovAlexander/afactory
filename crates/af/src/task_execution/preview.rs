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
fn package_label(cas: &Cas, plan: &ExecutionPlanV1, name: &str) -> Result<String, String> {
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
    Ok(format!("{}@{}", text(name), text(version)))
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
                    | TaskOperatorV1::DocumentAccept {},
                ..
            }
    )
}

fn node_label(id: &str, node: &CompiledNode, plan: &ExecutionPlanV1) -> String {
    let name = id.rsplit('.').next().unwrap_or(id);
    let detail = if let Some(slot) = worker(node) {
        binding(plan, slot)
    } else {
        match &node.operator {
            CompiledOperator::ProviderAdmission { .. } => "Provider admission (budgeted)".into(),
            CompiledOperator::Primitive { operator, .. } => match operator {
                TaskOperatorV1::Check { checks } => format!(
                    "checks: {}",
                    checks.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
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
fn tree_rows(
    out: &mut String,
    cas: &Cas,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
    parent: &str,
    prefix: &str,
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
            )?;
        } else {
            line(
                out,
                &format!("{prefix}{branch}{}", node_label(id, &graph.nodes[id], plan)),
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
            Some(_) => binding_origin(binding),
            None => format!("artifact {}", text(&binding.artifact_id)),
        };
        labels.push(format!("{} <- {origin}", text(name)));
    }
    labels.join(", ")
}

/// One `BOUND` group per bound port for `--tree`: the referenced Task, its output port, the
/// acceptance and domain conclusion it had, and the exact artifact ID. An exact-artifact
/// binding reads `artifact` in place of the Task and port and `-` where no Task supplied an
/// acceptance or a conclusion. The digest gets its own row so no wrap can split it.
fn binding_rows(bound: &BindingRows) -> Vec<String> {
    let mut rows = Vec::new();
    for (name, binding) in bound {
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
        tree_rows(&mut out, cas, plan, graph, "root", "")?;
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
        let mut slots = Vec::new();
        for id in &graph.order {
            if let Some(slot) = worker(&graph.nodes[id])
                && !slots.contains(&slot)
            {
                slots.push(slot);
            }
        }
        for slot in plan.bindings.keys() {
            if !slots.contains(&slot.as_str()) {
                slots.push(slot);
            }
        }
        for slot in slots.into_iter().take(8) {
            line(
                &mut out,
                &format!(
                    "  {}: {}",
                    slot.strip_prefix("root.slots.").unwrap_or(slot),
                    binding(plan, slot)
                ),
            );
        }
        if plan.bindings.len() > 8 {
            line(
                &mut out,
                &format!(
                    "  ... {} more bindings; use --tree or --json",
                    plan.bindings.len() - 8
                ),
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
    let left = plan.limits.deadline_unix_ms.saturating_sub(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    );
    line(
        &mut out,
        &format!(
            "TIME  {}s remaining | deadline {} Unix ms{}",
            left / 1000,
            plan.limits.deadline_unix_ms,
            if left == 0 { " [EXPIRED]" } else { "" }
        ),
    );
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
    if status == "finished" || status == "historical plan (not current)" {
        line(&mut out, "Recorded plan only; this preview starts no work.");
    } else {
        if plan.requires_developer_approval() && status == "needs signed plan approval" {
            line(
                &mut out,
                "First inspect and sign this exact generated plan with task decision-payload / approve.",
            );
        }
        line(&mut out, "Approve and run this plan:");
        line(
            &mut out,
            &format!("  af task run {} \\", text(&revision.task_id)),
        );
        line(&mut out, &format!("    --confirm-plan {plan_id}"));
        line(
            &mut out,
            "Use the same --repo and --state selectors as this preview.",
        );
    }
    line(
        &mut out,
        &format!(
            "Details: af task explain {} --tree (or --json)",
            text(&revision.task_id)
        ),
    );
    Ok(out)
}

pub(super) fn current(cas: &Cas, state: &TaskProjection, tree: bool) -> Result<String, String> {
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
    render(cas, &state.revision, id, &plan, &graph, status, tree)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
