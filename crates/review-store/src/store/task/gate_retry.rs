//! `af task run ID --retry-gate`: reopen a finished Task whose last round ended at a failed
//! gate, so its check node runs again, in a new check Attempt of the same round, on the same
//! sealed candidate (`TaskGateRetried@1`, its own event of the Task's log beside
//! `TaskTransition@5`).
//!
//! The retry grants nothing a plan did not. It needs the Task's current lease, the plan's
//! current approval and deadline, and a budget that still holds a new Attempt of the check
//! node; it creates no plan, revision or Task and admits nothing again. It reopens only a Task
//! that ended unsatisfied with a check receipt that did not pass and whose every paid node
//! after that check never started an Attempt: no reviewer or evaluator ran, so the last failure
//! was the gate's. It clears the check's published output and every output and invocation
//! after it, and retires their selected Attempts, which stay in the log and in the Task's
//! accounting; the check's own invocation stays, because its candidate does not change.

use review_core::task::gate_retry::{RetriedGateV1, TASK_GATE_RETRIED_V1, TaskGateRetriedV1};
use review_core::task::pipeline::TaskOperatorV1;
use review_graph::task::CompiledOperator;

use super::*;

pub fn is_gate_retried(event: &RunEvent) -> bool {
    event.event_type == EventType::TaskGateRetriedV1
}

pub fn read_gate_retried(event: &RunEvent) -> Result<TaskGateRetriedV1, StoreError> {
    if event.event_type != EventType::TaskGateRetriedV1 {
        return Err(conflict("Expected a Task gate retry"));
    }
    let value: TaskGateRetriedV1 = serde_json::from_value(event.payload.clone())?;
    value.validate().map_err(conflict)?;
    Ok(value)
}

/// Why `--retry-gate` cannot reopen the Task, in one sentence, or its failed gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateRetry {
    /// The check nodes that run again, in graph order.
    Gates(Vec<String>),
    Refused(String),
}

impl TaskProjection {
    /// The check nodes `--retry-gate` would run again at `now_unix_ms`: each whose published
    /// check receipt did not pass, after which no paid node started an Attempt, and that does
    /// not itself follow another such check. A refusal when the Task is not finished, was
    /// satisfied or delivered, its last failure was not a gate alone (a paid node ran after or
    /// beside it), or the retry would not be admitted at `now_unix_ms`: the plan's approval or
    /// the Task's deadline expired, or its limits leave no Attempt for a gate. Inspection and
    /// the retry itself ask the same question, so a printed `--retry-gate` is one the kernel
    /// runs.
    pub fn failed_gates(&self, cas: &Cas, now_unix_ms: u64) -> Result<GateRetry, StoreError> {
        let refused = |why: &str| Ok(GateRetry::Refused(why.into()));
        let TaskPhaseV1::Finished { result_id } = &self.phase else {
            return refused("it has not finished; `af task run` continues it");
        };
        let result: task::TaskResultV1 = payload(cas, result_id, task::TASK_RESULT_V1)?;
        if result.acceptance == TaskAcceptanceV1::Satisfied {
            return refused("it was verified, so no gate failed");
        }
        if !self.deliveries.is_empty() {
            return refused("it was delivered");
        }
        let Some(execution) = &self.execution else {
            return refused("it never executed a gate");
        };
        let mut failed = Vec::new();
        for node in &execution.graph.order {
            let Some(definition) = execution.graph.nodes.get(node) else {
                continue;
            };
            if !matches!(
                &definition.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Check { .. },
                    ..
                }
            ) {
                continue;
            }
            let Some((_, output)) = execution.outputs.get(node) else {
                continue;
            };
            let Some(receipt) = output
                .outputs
                .get("result")
                .and_then(|port| port.artifact_ids.first())
            else {
                continue;
            };
            let receipt = envelope(cas, receipt, task::verification::TASK_CHECK_RECEIPT_V1)?;
            if receipt.payload.get("outcome") == Some(&json!("passed")) {
                continue;
            }
            failed.push(node.clone());
        }
        let gates: Vec<String> = failed
            .iter()
            .filter(|node| {
                !failed
                    .iter()
                    .any(|other| execution.graph.downstream_of(other).contains(*node))
            })
            .cloned()
            .collect();
        if gates.is_empty() {
            return refused(
                "its checks passed, so its last failure was not a gate; --retry-gate runs \
                 only a failed check again",
            );
        }
        for gate in &gates {
            let after = execution.graph.downstream_of(gate);
            if execution.attempts_of(&after) {
                return refused(
                    "a reviewer or evaluator ran after its failed gate, so its last failure \
                     was not the gate's alone; --retry-gate runs only a failed check again",
                );
            }
            // A paid node that neither feeds nor follows the gate (it consumed the same
            // candidate beside the check) keeps its Attempt across a retry, which reopens only
            // the gate's branch: when it failed, its failure survives the retry, so the last
            // failure was not the gate's alone.
            let beside: BTreeSet<String> = execution
                .graph
                .order
                .iter()
                .filter(|node| {
                    *node != gate
                        && !after.contains(*node)
                        && !execution.graph.downstream_of(node).contains(gate)
                })
                .cloned()
                .collect();
            if execution.failed_attempts_of(&beside) {
                return refused(
                    "a paid node ran beside its failed gate, so its last failure was not the \
                     gate's alone; --retry-gate runs only a failed check again",
                );
            }
            if after.iter().chain([gate]).any(|node| {
                execution.graph.owned_children.contains_key(node)
                    || execution.graph.experimental_slots.contains_key(node)
                    || execution.owned.contains_key(node)
            }) || execution.active_review_integration().is_some()
            {
                return refused(
                    "its gate is followed by owned children, an experiment or an Integration, \
                     which --retry-gate does not reopen",
                );
            }
        }
        if review_round::ReviewRoundFence::capture(cas, &self.revision)?.is_some() {
            return refused(
                "it runs a captured Campaign Review Round, which --retry-gate does not reopen",
            );
        }
        if let Some(why) = self.retry_admission(cas, execution, &gates, now_unix_ms)? {
            return Ok(GateRetry::Refused(why));
        }
        Ok(GateRetry::Gates(gates))
    }

    /// Why a retry of `gates` would not be admitted at `now`, the checks
    /// [`Self::apply_gate_retried`] makes before it reopens anything: an admitted plan whose
    /// approval and deadline still hold, and a budget with one more Attempt for each gate.
    fn retry_admission(
        &self,
        cas: &Cas,
        execution: &execution::TaskExecutionProjection,
        gates: &[String],
        now: u64,
    ) -> Result<Option<String>, StoreError> {
        if !self.admitted {
            return Ok(Some("its plan is not admitted".into()));
        }
        let time = now.max(self.last_time);
        if self.check_plan_decision(cas, time).is_err() {
            return Ok(Some(
                "its plan approval expired or was revoked; plan it again with \
                 `af task plan --file TASK_FILE --fit`"
                    .into(),
            ));
        }
        if time >= self.deadline_at(time) {
            return Ok(Some(
                "its deadline passed; plan it again with \
                 `af task plan --file TASK_FILE --fit`"
                    .into(),
            ));
        }
        let mut budget = execution.budget.clone();
        for gate in gates {
            budget.allow_retry(gate).map_err(conflict)?;
        }
        for gate in gates {
            if let Err(error) = budget.prepare(gate, time) {
                return Ok(Some(format!(
                    "the Task's limits leave no Attempt for {gate} ({error}); plan it again \
                     with `af task plan --file TASK_FILE --fit`"
                )));
            }
        }
        Ok(None)
    }

    /// Apply `TaskGateRetried@1`: the next event of the log, by the current lease, naming exactly
    /// the failed gates [`Self::failed_gates`] finds and their published outputs, while the
    /// plan's approval and the budget still hold each new check Attempt. The Task runs again.
    pub(super) fn apply_gate_retried(
        &mut self,
        cas: &Cas,
        event: &RunEvent,
        retry: &TaskGateRetriedV1,
    ) -> Result<(), StoreError> {
        if event.sequence != self.next_sequence {
            return Err(conflict("Task event sequence has a gap or duplicate"));
        }
        if event.artifact_refs != retry.artifact_refs() {
            return Err(conflict(
                "Task gate retry references disagree with its typed payload",
            ));
        }
        let time = self.lease_time(&retry.writer, retry.epoch, retry.now_unix_ms);
        if time < self.last_time {
            return Err(conflict(
                "Task is finished or its policy clock moved backwards",
            ));
        }
        self.check_lease_of(&retry.writer, retry.epoch, retry.now_unix_ms)?;
        match &self.phase {
            TaskPhaseV1::Finished { result_id } if *result_id == retry.result_id => {}
            _ => {
                return Err(conflict(
                    "A gate retry reopens exactly the finished result it names",
                ));
            }
        }
        if !self.admitted {
            return Err(conflict("A gate retry needs an admitted plan"));
        }
        match self.failed_gates(cas, time)? {
            GateRetry::Gates(gates)
                if gates.iter().eq(retry.gates.iter().map(|gate| &gate.node)) => {}
            GateRetry::Gates(_) => {
                return Err(conflict(
                    "A gate retry names other nodes than the Task's failed gates",
                ));
            }
            GateRetry::Refused(why) => {
                return Err(conflict(format!("A gate retry is refused: {why}")));
            }
        }
        self.check_approval(cas, time)?;
        let execution = self
            .execution
            .as_mut()
            .ok_or_else(|| conflict("A gate retry needs an execution"))?;
        for gate in &retry.gates {
            if execution.outputs.get(&gate.node).map(|(id, _)| id) != Some(&gate.output_id) {
                return Err(conflict(
                    "A gate retry names another output than the gate's published one",
                ));
            }
        }
        // The retried checks need one more Attempt each than they had; a budget that no longer
        // holds them refuses the retry here, never a reopened Task that cannot run.
        for gate in &retry.gates {
            execution.reopen_gate(&gate.node);
            execution.budget.allow_retry(&gate.node).map_err(conflict)?;
        }
        let mut budget = execution.budget.clone();
        for gate in &retry.gates {
            budget.prepare(&gate.node, time).map_err(|error| {
                conflict(format!(
                    "the Task's limits leave no Attempt for {}: {error}",
                    gate.node
                ))
            })?;
        }
        self.phase = TaskPhaseV1::Running {};
        self.artifact_refs.extend(retry.artifact_refs());
        self.gate_retries.push(retry.clone());
        self.last_time = time;
        self.next_sequence = event.sequence + 1;
        Ok(())
    }
}

impl EventStore {
    /// Reopen the finished Task of `lease` at every failed gate ([`TaskProjection::failed_gates`])
    /// with one `TaskGateRetried@1`, so `af task run` runs those checks again on the same
    /// candidate. Refused, with the reason, when the Task's last failure was not a gate or its
    /// plan, approval, lease or budget no longer holds the new Attempt.
    pub fn retry_task_gate(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        authority: &dyn TaskAuthority,
    ) -> Result<Vec<String>, StoreError> {
        let mut state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let gates = match state.failed_gates(cas, now()?)? {
            GateRetry::Gates(gates) => gates,
            GateRetry::Refused(why) => {
                return Err(conflict(format!(
                    "Task {} cannot retry its gate: {why}",
                    lease.task_id
                )));
            }
        };
        let plan_id = state
            .plan_id
            .clone()
            .ok_or_else(|| conflict("Task has no plan"))?;
        self.authorized_plan(cas, &state, &plan_id, authority)?;
        if let Some(decision) = state.decisions.get(&plan_id) {
            authority
                .authorization_current(&decision.value)
                .map_err(conflict)?;
        }
        let TaskPhaseV1::Finished { result_id } = state.phase.clone() else {
            return Err(conflict("A gate retry reopens a finished Task"));
        };
        let run_id = task_run_id(&lease.task_id)?;
        let mut retry = TaskGateRetriedV1 {
            schema: TASK_GATE_RETRIED_V1.into(),
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now()?,
            result_id,
            gates: gates
                .iter()
                .map(|node| {
                    state
                        .execution
                        .as_ref()
                        .and_then(|execution| execution.outputs.get(node))
                        .map(|(id, _)| RetriedGateV1 {
                            node: node.clone(),
                            output_id: id.clone(),
                        })
                        .ok_or_else(|| conflict("A failed gate lost its published output"))
                })
                .collect::<Result<_, _>>()?,
        };
        // Stamped before it is persisted, so the log stays monotonic and replays unchanged.
        retry.now_unix_ms = state.lease_time(&retry.writer, retry.epoch, retry.now_unix_ms);
        retry.validate().map_err(conflict)?;
        let value = serde_json::to_value(&retry)?;
        let first = state.next_sequence;
        let event = NewEvent::new(EventType::TaskGateRetriedV1, value.clone())
            .referencing(retry.artifact_refs());
        state.apply_gate_retried(
            cas,
            &RunEvent {
                run_id: run_id.clone(),
                event_id: crate::store::derive_event_id(&run_id, first as i64),
                sequence: first,
                event_type: EventType::TaskGateRetriedV1,
                occurred_at: event.occurred_at.clone(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: None,
                artifact_refs: event.artifact_refs.clone(),
                payload: value.clone(),
            },
            &retry,
        )?;
        let valid_until = state
            .decisions
            .get(&plan_id)
            .map_or(state.lease_until, |decision| {
                state.lease_until.min(decision.valid_until)
            });
        let permit = WritePermit {
            run_id: run_id.clone(),
            first,
            payloads: vec![value],
            event_type: EventType::TaskGateRetriedV1,
            valid_until: Some(valid_until),
            review_round: None,
            review_prefix: None,
            bound_tasks: Vec::new(),
        };
        self.append_batch_observed(&run_id, cas, &[event], Some(&permit), None, Some(&state))?;
        Ok(gates)
    }
}
