//! `af task list --attention` and the browser's attention view: the Tasks of one Store grouped by
//! what they wait for ([`TaskAttentionV1`]), read from the `state` of each `af task list --json`
//! entry and nothing else. Nothing is written or recorded: the groups are derived each time the
//! Store is read, as the state is.

use review_core::task::state::{TaskAttentionV1, TaskStateV1};
use serde_json::{Value, json};

/// The document `af task list --attention --json` prints.
pub(crate) const TASK_ATTENTION_V1: &str = "af/task-attention@1";

/// Every group, in [`TaskAttentionV1::ALL`] order, with the `af/task-list-entry@4` entries whose
/// state it holds, each group in the order the entries came.
pub(crate) fn groups(entries: Vec<Value>) -> Result<Vec<(TaskAttentionV1, Vec<Value>)>, String> {
    let mut groups: Vec<(TaskAttentionV1, Vec<Value>)> = TaskAttentionV1::ALL
        .into_iter()
        .map(|group| (group, Vec::new()))
        .collect();
    for entry in entries {
        let group = TaskAttentionV1::of(&state_of(&entry)?);
        if let Some((_, tasks)) = groups.iter_mut().find(|(each, _)| *each == group) {
            tasks.push(entry);
        }
    }
    Ok(groups)
}

/// The `state` of a list entry.
pub(crate) fn state_of(entry: &Value) -> Result<TaskStateV1, String> {
    serde_json::from_value(entry["state"].clone()).map_err(|error| {
        format!(
            "Task {} has no readable state: {error}",
            entry["task_id"].as_str().unwrap_or("-")
        )
    })
}

/// The `af/task-attention@1` document of `groups`.
pub(crate) fn document(groups: &[(TaskAttentionV1, Vec<Value>)]) -> Value {
    let groups: Vec<Value> = groups
        .iter()
        .map(|(group, tasks)| {
            json!({"group": group.as_str(), "needs_you": group.needs_you(), "tasks": tasks})
        })
        .collect();
    json!({"schema": TASK_ATTENTION_V1, "groups": groups})
}

/// The text of `af task list --attention` at `now`, `width` columns wide: each group that needs
/// a person and holds a Task, under its title and count, one line per Task (its ID, state and
/// age) and its next command on the line below; then the running Tasks by ID, and how many need
/// nothing.
pub(crate) fn lines(
    groups: &[(TaskAttentionV1, Vec<Value>)],
    now: u64,
    width: usize,
) -> Result<Vec<String>, String> {
    if groups.iter().all(|(_, tasks)| tasks.is_empty()) {
        return Ok(vec!["no Tasks".to_owned()]);
    }
    let needs_you = || {
        groups
            .iter()
            .filter(|(group, tasks)| group.needs_you() && !tasks.is_empty())
    };
    let mut rows = Vec::new();
    for (_, tasks) in needs_you() {
        for task in tasks {
            let id = task["task_id"].as_str().unwrap_or("-").to_owned();
            let age = task["updated_unix_ms"].as_u64().map_or_else(
                || "-".to_owned(),
                |at| crate::task::age(now.saturating_sub(at)),
            );
            rows.push((id, state_of(task)?.label(), age));
        }
    }
    let id_width = rows.iter().map(|row| row.0.chars().count()).max();
    let state_width = rows.iter().map(|row| row.1.chars().count()).max();
    let mut rows = rows.into_iter();
    let mut out = Vec::new();
    for (group, tasks) in needs_you() {
        out.push(format!("{} ({})", group.title(), tasks.len()));
        for task in tasks {
            let (id, state, age) = rows.next().expect("one row per Task");
            let line = format!(
                "  {id:<w$}  {state:<s$}  {age}",
                w = id_width.unwrap_or(0),
                s = state_width.unwrap_or(0)
            );
            out.push(line.trim_end().to_owned());
            let next = task["state"]["next_action"].as_str().unwrap_or("none");
            out.extend(
                crate::human::fit_command(&format!("    next: {next}"), width)
                    .lines()
                    .map(str::to_owned),
            );
        }
    }
    if out.is_empty() {
        out.push("No Task needs you now.".to_owned());
    }
    for (group, tasks) in groups.iter().filter(|(group, _)| !group.needs_you()) {
        if tasks.is_empty() {
            continue;
        }
        let line = match group {
            TaskAttentionV1::Running => {
                let ids: Vec<&str> = tasks
                    .iter()
                    .map(|task| task["task_id"].as_str().unwrap_or("-"))
                    .collect();
                format!("{} ({}): {}", group.title(), tasks.len(), ids.join(", "))
            }
            _ => format!(
                "{} ({}): `af task list` shows every Task",
                group.title(),
                tasks.len()
            ),
        };
        out.push(line);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::state::*;

    fn entry(task_id: &str, state: TaskStateV1, updated: u64) -> Value {
        json!({"task_id": task_id, "state": state, "updated_unix_ms": updated})
    }

    fn finished(outcome: TaskOutcomeV1, cause: Option<TaskCauseV1>) -> TaskStateV1 {
        TaskStateV1::ended(TaskStatePhaseV1::Finished, outcome, cause)
    }

    /// One Task in each group, in Task ID order as `af task list` reads them.
    fn store() -> Vec<Value> {
        vec![
            entry(
                "a-planned",
                TaskStateV1::new(TaskStatePhaseV1::Planned)
                    .with_next("af task run a-planned --confirm-plan sha256:1"),
                40_000,
            ),
            entry(
                "b-gate",
                finished(TaskOutcomeV1::GateFailed, Some(TaskCauseV1::CheckFailed))
                    .with_next("af task run b-gate --retry-gate"),
                0,
            ),
            entry(
                "c-findings",
                finished(
                    TaskOutcomeV1::ChangesRequested,
                    Some(TaskCauseV1::ReviewerFindings),
                )
                .with_next("af task findings c-findings"),
                0,
            ),
            entry(
                "d-verified",
                finished(TaskOutcomeV1::Verified, None).with_next(
                    "af task deliver d-verified --branch af/d-verified --worktree ../d-verified \
                     --confirm d-verified",
                ),
                0,
            ),
            entry(
                "e-stopped",
                TaskStateV1::ended(
                    TaskStatePhaseV1::Abandoned,
                    TaskOutcomeV1::Cancelled,
                    Some(TaskCauseV1::HostCancelled),
                )
                .with_next("af task run e-stopped"),
                0,
            ),
            entry("f-running", TaskStateV1::new(TaskStatePhaseV1::Running), 0),
            entry("g-delivered", finished(TaskOutcomeV1::Verified, None), 0),
            entry(
                "h-collected",
                finished(TaskOutcomeV1::GateFailed, Some(TaskCauseV1::Collected)),
                0,
            ),
            entry(
                "i-waiting",
                TaskStateV1::new(TaskStatePhaseV1::Waiting)
                    .with_next("af task explain i-waiting --tree"),
                0,
            ),
        ]
    }

    #[test]
    fn each_task_is_in_the_group_its_state_names_in_list_order() {
        let groups = groups(store()).unwrap();
        let ids = |group: TaskAttentionV1| -> Vec<&str> {
            let (_, tasks) = groups.iter().find(|(g, _)| *g == group).unwrap();
            tasks
                .iter()
                .map(|t| t["task_id"].as_str().unwrap())
                .collect()
        };
        assert_eq!(
            groups.iter().map(|(g, _)| *g).collect::<Vec<_>>(),
            TaskAttentionV1::ALL
        );
        assert_eq!(ids(TaskAttentionV1::Approval), ["a-planned", "i-waiting"]);
        assert_eq!(ids(TaskAttentionV1::GateFailed), ["b-gate"]);
        assert_eq!(ids(TaskAttentionV1::Findings), ["c-findings"]);
        assert_eq!(ids(TaskAttentionV1::Deliver), ["d-verified"]);
        assert_eq!(ids(TaskAttentionV1::Stopped), ["e-stopped"]);
        assert_eq!(ids(TaskAttentionV1::Running), ["f-running"]);
        assert_eq!(
            ids(TaskAttentionV1::Nothing),
            ["g-delivered", "h-collected"]
        );
        let document = document(&groups);
        assert_eq!(document["schema"], "af/task-attention@1");
        assert_eq!(document["groups"].as_array().unwrap().len(), 7);
        assert_eq!(document["groups"][5]["group"], "running");
        assert_eq!(document["groups"][5]["needs_you"], false);
        assert_eq!(document["groups"][0]["needs_you"], true);
        assert!(super::groups(vec![json!({"task_id": "x", "state": {}})]).is_err());
    }

    #[test]
    fn the_text_lists_what_needs_you_with_its_next_command_then_running_and_idle_tasks() {
        let lines = lines(&groups(store()).unwrap(), 100_000, 100).unwrap();
        assert_eq!(
            lines,
            [
                "Waiting for approval (2)",
                "  a-planned   planned                                       1m",
                "    next: af task run a-planned --confirm-plan sha256:1",
                "  i-waiting   waiting                                       1m",
                "    next: af task explain i-waiting --tree",
                "Failed gate (1)",
                "  b-gate      finished/gate_failed/check_failed             1m",
                "    next: af task run b-gate --retry-gate",
                "Findings to answer (1)",
                "  c-findings  finished/changes_requested/reviewer_findings  1m",
                "    next: af task findings c-findings",
                "Verified, ready to deliver (1)",
                "  d-verified  finished/verified                             1m",
                "    next: af task deliver d-verified --branch af/d-verified --worktree \
                 ../d-verified --confirm \\",
                "              d-verified",
                "Stopped (1)",
                "  e-stopped   abandoned/cancelled/host_cancelled            1m",
                "    next: af task run e-stopped",
                "Running (1): f-running",
                "Nothing to do (2): `af task list` shows every Task",
            ]
        );
        // A next command wider than the terminal continues between its arguments, each of its
        // lines within the width.
        let narrow = super::lines(&groups(store()).unwrap(), 100_000, 60).unwrap();
        assert!(
            narrow
                .iter()
                .filter(|line| line.starts_with("    "))
                .all(|line| line.chars().count() <= 60),
            "{narrow:?}"
        );
        assert!(
            narrow.iter().any(|line| line.ends_with(" \\")),
            "{narrow:?}"
        );
    }

    #[test]
    fn a_store_that_needs_nothing_says_so_and_an_empty_one_has_no_tasks() {
        let idle: Vec<Value> = store()
            .into_iter()
            .filter(|task| !TaskAttentionV1::of(&state_of(task).unwrap()).needs_you())
            .collect();
        assert_eq!(
            lines(&groups(idle).unwrap(), 0, 100).unwrap(),
            [
                "No Task needs you now.",
                "Running (1): f-running",
                "Nothing to do (2): `af task list` shows every Task",
            ]
        );
        assert_eq!(
            lines(&groups(vec![]).unwrap(), 0, 100).unwrap(),
            ["no Tasks"]
        );
    }
}
