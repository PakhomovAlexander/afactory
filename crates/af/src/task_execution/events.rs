//! `af task events ID` (ADR-0250): a Task's log as JSON lines, one `af/task-event@1` document per
//! event the Store recorded, followed until the Task settles and ended by one terminal record.
//! Every line is read from the Task's own log and projection, never from a second record: an
//! event's `sequence` is the Store's, so the stream is gap-free and `--after N` resumes it.

use super::*;
use review_core::EventType;
use review_core::task::agent_loop::*;
use review_core::task::event::{TaskChangeV1, TaskTransitionV1};
use review_core::task::execution::{
    TASK_INVOCATION_V1, TASK_OUTPUT_V1, TaskAttemptResultV1, TaskExecutionRecordV1,
    TaskInvocationV1, TaskOutputV1,
};
use review_core::task::state::TaskStatePhaseV1;
use std::io::Write as _;
use std::time::Duration;

/// How often a follower reads the Store again while the Task runs, as `af task wait` does.
const POLL: Duration = Duration::from_millis(250);

/// `af task events ID [--after N] [--timeout S]`: print every event of the Task's log after
/// sequence N, following the Task until it settles — no writer holds it and no run of this host
/// is starting — then the terminal record, and exit with the Task's exit code; 124 without a
/// terminal record when `--timeout` passes first.
pub(crate) fn events(
    id: &str,
    after: Option<u64>,
    timeout: Option<u64>,
    selectors: &crate::cli::TaskStoreArgs,
) -> Result<i32, String> {
    let mut opened = open_task(id, &selectors.repo, selectors.state.as_deref())?;
    let run_id = review_store::store::task::task_run_id(id).map_err(|e| e.to_string())?;
    if let Some(after) = after
        && after >= opened.task.next_sequence
    {
        return Err(format!(
            "Task {id} recorded events 0 to {}, so --after {after} names none of them; follow \
             it from its first event with `{}`",
            opened.task.next_sequence.saturating_sub(1),
            with_selectors(&format!("af task events {id}"))
        ));
    }
    let started = std::time::Instant::now();
    let limit = timeout.map(Duration::from_secs);
    let mut next = after.map_or(0, |after| after.saturating_add(1));
    loop {
        // The state and the events come from one projection: the terminal record never claims
        // a state whose events it has not printed.
        let (_, view) = opened.view()?;
        let running = view.state.phase == TaskStatePhaseV1::Running;
        let starting = !running && runs::starting_run(&opened.state, &opened.task).is_some();
        // On every poll, not only the first: a run that was starting at an earlier poll and
        // ended before it took the Task leaves it planned, which no terminal record settles.
        if !running && !starting && view.state.phase == TaskStatePhaseV1::Planned {
            let next = view
                .state
                .next_action
                .as_deref()
                .unwrap_or("af task run ID");
            return Err(format!(
                "Task {id} is planned and no run of it is in progress, so it records no events \
                 to follow; run it with `{}`, or in the background with `{} --detach`",
                preview::text(next),
                preview::text(next)
            ));
        }
        let through = opened.task.next_sequence;
        if next < through {
            let attempts = attempt_nodes(&opened.task);
            let recorded = opened
                .store
                .replay_from(&run_id, next)
                .map_err(|e| e.to_string())?;
            for event in recorded.iter().take_while(|event| event.sequence < through) {
                let line = TaskEventV1::Event(entry(&opened.cas, id, event, &attempts)?);
                write_line(&line)?;
                next = event.sequence + 1;
            }
        }
        if !running && !starting {
            let state = view.state;
            let exit_code = TaskEventTerminalV1::exit_code_of(&state);
            write_line(&TaskEventV1::Terminal(TaskEventTerminalV1 {
                schema: TASK_EVENT_V1.into(),
                task_id: id.into(),
                last_sequence: through.saturating_sub(1),
                state,
                exit_code,
            }))?;
            return Ok(i32::from(exit_code));
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            let resume = match next.checked_sub(1) {
                Some(last) => format!("af task events {id} --after {last}"),
                None => format!("af task events {id}"),
            };
            eprintln!(
                "af task events: Task {id} still runs after {}; continue with `{}`",
                state::duration_words(u64::try_from(started.elapsed().as_millis()).unwrap_or(0)),
                with_selectors(&resume)
            );
            return Ok(124);
        }
        std::thread::sleep(POLL);
        opened.refresh()?;
    }
}

/// One validated line on stdout, written whole and flushed, so a follower reading a pipe sees
/// each event as soon as it is printed.
fn write_line(line: &TaskEventV1) -> Result<(), String> {
    line.validate()?;
    let text = serde_json::to_string(line).map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}")
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("writing the event stream to stdout: {e}"))
}

/// The node of every Attempt the Task's current execution reserved, as a person names it.
fn attempt_nodes(task: &TaskProjection) -> BTreeMap<String, String> {
    task.execution
        .iter()
        .flat_map(|execution| execution.attempt_accounting())
        .map(|attempt| {
            (
                attempt.attempt_id,
                runs::node_name(&attempt.reservation.node),
            )
        })
        .collect()
}

/// The `kind` a Store record serializes its variant as, the name `detail` carries.
fn kind_of(value: &impl Serialize) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "a Store record without its kind".to_owned())
}

/// The event line of one event of the Task's log.
fn entry(
    cas: &Cas,
    task_id: &str,
    event: &review_core::RunEvent,
    attempts: &BTreeMap<String, String>,
) -> Result<TaskEventEntryV1, String> {
    fn payload<T: serde::de::DeserializeOwned>(event: &review_core::RunEvent) -> Result<T, String> {
        serde_json::from_value(event.payload.clone())
            .map_err(|e| format!("event {} of the Task's log: {e}", event.sequence))
    }
    let mut line = TaskEventEntryV1 {
        schema: TASK_EVENT_V1.into(),
        task_id: task_id.into(),
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        event_type: event.event_type.as_str().into(),
        at_unix_ms: 0,
        change: TaskEventChangeV1::Opened,
        detail: None,
        node: None,
        attempt_id: None,
        result: None,
        charged_tokens: None,
    };
    match event.event_type {
        EventType::TaskTransitionV5 => {
            let transition: TaskTransitionV1 = payload(event)?;
            line.at_unix_ms = transition.now_unix_ms;
            line.change = TaskEventChangeV1::of(&transition.change);
            match &transition.change {
                TaskChangeV1::Waiting { reason } => line.detail = Some(kind_of(reason)?),
                TaskChangeV1::ExecutionRecorded { record_id } => {
                    execution(cas, record_id, attempts, &mut line)?;
                }
                _ => {}
            }
        }
        EventType::TaskExecutionStartedV1 => {
            let marker: review_core::task::execution_started::TaskExecutionStartedV1 =
                payload(event)?;
            line.at_unix_ms = marker.now_unix_ms;
            line.change = TaskEventChangeV1::ExecutionStarted;
        }
        EventType::TaskGateRetriedV1 => {
            let retry: review_core::task::gate_retry::TaskGateRetriedV1 = payload(event)?;
            line.at_unix_ms = retry.now_unix_ms;
            line.change = TaskEventChangeV1::GateRetried;
        }
        EventType::TaskProviderEventV1 => {
            let provider: review_core::task::provider_event::TaskProviderEventV1 = payload(event)?;
            line.at_unix_ms = provider.now_unix_ms;
            line.change = TaskEventChangeV1::ProviderEvent;
            line.detail = Some(kind_of(&provider.event)?);
            line.node = Some(runs::node_name(&provider.node));
            line.attempt_id = Some(provider.attempt_id);
        }
        other => {
            return Err(format!(
                "event {} of Task {task_id}'s log is a {other} event, which no Task records",
                event.sequence
            ));
        }
    }
    Ok(line)
}

/// What an `execution_recorded` change records: the record's kind, the node and Attempt it
/// concerns, and a settlement's result and charge.
fn execution(
    cas: &Cas,
    record_id: &str,
    attempts: &BTreeMap<String, String>,
    line: &mut TaskEventEntryV1,
) -> Result<(), String> {
    let record = review_store::store::task::execution::read_execution_record(cas, record_id)
        .map_err(|e| e.to_string())?
        .record;
    line.detail = Some(kind_of(&record)?);
    let invocation_node = |id: &str| -> Result<String, String> {
        let invocation: TaskInvocationV1 = artifact(cas, id, TASK_INVOCATION_V1)?;
        Ok(runs::node_name(&invocation.node))
    };
    let attempt = |id: &String| (attempts.get(id).cloned(), Some(id.clone()));
    let (node, attempt_id) = match &record {
        TaskExecutionRecordV1::Invocation { invocation_id } => {
            (Some(invocation_node(invocation_id)?), None)
        }
        TaskExecutionRecordV1::Reserved {
            invocation_id,
            attempt_id,
            ..
        } => (
            Some(invocation_node(invocation_id)?),
            Some(attempt_id.clone()),
        ),
        TaskExecutionRecordV1::Published {
            output_id,
            attempt_id,
        } => {
            let output: TaskOutputV1 = artifact(cas, output_id, TASK_OUTPUT_V1)?;
            (
                Some(invocation_node(&output.invocation_id)?),
                attempt_id.clone(),
            )
        }
        TaskExecutionRecordV1::ContextBound { attempt_id, .. }
        | TaskExecutionRecordV1::Started { attempt_id }
        | TaskExecutionRecordV1::Released { attempt_id, .. }
        | TaskExecutionRecordV1::Settled { attempt_id, .. }
        | TaskExecutionRecordV1::UsageObserved { attempt_id, .. }
        | TaskExecutionRecordV1::OwnedChildPublished { attempt_id, .. } => attempt(attempt_id),
        TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
        | TaskExecutionRecordV1::OwnedChildrenCompleted { .. }
        | TaskExecutionRecordV1::ExperimentPrepared { .. }
        | TaskExecutionRecordV1::ExperimentPlanDecided { .. }
        | TaskExecutionRecordV1::ExperimentChildrenRegistered { .. } => (None, None),
    };
    line.node = node;
    line.attempt_id = attempt_id;
    match record {
        TaskExecutionRecordV1::Settled {
            charged_tokens,
            result,
            ..
        } => {
            line.result = Some(match result {
                TaskAttemptResultV1::Succeeded { .. } => TaskEventResultV1::Succeeded,
                TaskAttemptResultV1::Failed { .. } => TaskEventResultV1::Failed,
                TaskAttemptResultV1::Abandoned { .. } => TaskEventResultV1::Abandoned,
            });
            line.charged_tokens = Some(charged_tokens.into());
        }
        TaskExecutionRecordV1::UsageObserved { charged_tokens, .. } => {
            line.charged_tokens = Some(charged_tokens.into());
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::RunEvent;
    use review_core::task::TaskWaitingReasonV1;

    fn event(sequence: u64, event_type: EventType, payload: serde_json::Value) -> RunEvent {
        RunEvent {
            event_id: "0123456789abcdef0123456789".into(),
            run_id: "task:pagination".into(),
            sequence,
            event_type,
            occurred_at: "1970-01-01T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload,
        }
    }

    fn transition(change: TaskChangeV1) -> serde_json::Value {
        serde_json::to_value(TaskTransitionV1 {
            writer: "cli-1".into(),
            epoch: 1,
            now_unix_ms: 1_700_000_000_000,
            change,
        })
        .unwrap()
    }

    /// A transition's line carries the Store's sequence, ID and type, its change's kind and the
    /// reason a wait names; a lease names no detail, node or charge.
    #[test]
    fn a_transition_is_one_line_with_the_stores_sequence_and_its_change() {
        let root = tempfile::tempdir().unwrap();
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let attempts = BTreeMap::new();
        let waiting = event(
            7,
            EventType::TaskTransitionV5,
            transition(TaskChangeV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsPlanReview,
            }),
        );
        let line = entry(&cas, "pagination", &waiting, &attempts).unwrap();
        TaskEventV1::Event(line.clone()).validate().unwrap();
        assert_eq!(line.sequence, 7);
        assert_eq!(line.event_id, "0123456789abcdef0123456789");
        assert_eq!(line.event_type, "TaskTransition@5");
        assert_eq!(line.change, TaskEventChangeV1::Waiting);
        assert_eq!(line.detail.as_deref(), Some("needs_plan_review"));
        assert_eq!(line.at_unix_ms, 1_700_000_000_000);
        let lease = event(
            8,
            EventType::TaskTransitionV5,
            transition(TaskChangeV1::LeaseRenewed {
                lease_until_unix_ms: 1_700_000_060_000,
            }),
        );
        let line = entry(&cas, "pagination", &lease, &attempts).unwrap();
        TaskEventV1::Event(line.clone()).validate().unwrap();
        assert_eq!(line.change, TaskEventChangeV1::LeaseRenewed);
        assert_eq!(
            (line.detail, line.node, line.result, line.charged_tokens),
            (None, None, None, None)
        );
        let foreign = event(9, EventType::FindingReportedV1, serde_json::json!({}));
        let refused = entry(&cas, "pagination", &foreign, &attempts).unwrap_err();
        assert!(refused.contains("no Task records"), "{refused}");
    }

    /// The terminal record's exit code is the one `af task wait` ends with.
    #[test]
    fn the_terminal_exit_code_is_the_waits() {
        use review_core::task::state::{TaskOutcomeV1, TaskStateV1};
        let waiting = TaskStateV1::new(TaskStatePhaseV1::Waiting);
        assert_eq!(TaskEventTerminalV1::exit_code_of(&waiting), 4);
        let failed = TaskStateV1::ended(
            TaskStatePhaseV1::Finished,
            TaskOutcomeV1::GateFailed,
            Some(review_core::task::state::TaskCauseV1::CheckFailed),
        );
        assert_eq!(
            i32::from(TaskEventTerminalV1::exit_code_of(&failed)),
            runs::settled_code(&failed)
        );
    }
}
