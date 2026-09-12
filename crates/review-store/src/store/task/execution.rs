//! Durable invocation/admission/settlement shared by every new Task kind. The scheduler and
//! domain adapters call this boundary; no Worker output can bypass plan or lease admission.

pub mod broker;
mod encoding;
pub mod owned;
pub use encoding::{DecodedTaskExecutionRecord, read_execution_record};

use review_attempt::task_budget::{TaskBudget, TaskReservation};
use review_attempt::{AttemptId, AttemptLedger, ExactReceipt, Selection};
use review_core::task::execution::*;
use review_graph::task::{CompiledOperator, CompiledTask, condition_input};

use super::*;

pub(in crate::store) mod review;

#[derive(Debug, Clone)]
pub struct TaskExecutionProjection {
    pub graph: CompiledTask,
    pub budget: TaskBudget,
    pub invocations: BTreeMap<String, (String, TaskInvocationV1)>,
    pub outputs: BTreeMap<String, (String, TaskOutputV1)>,
    ledger: AttemptLedger,
    attempts: BTreeMap<String, RecordedAttempt>,
    brokers: BTreeMap<String, broker::RecordedBroker>,
    pub(super) owned: BTreeMap<String, owned::RecordedChildren>,
}

#[derive(Debug, Clone)]
struct RecordedAttempt {
    invocation_id: String,
    plan_id: String,
    reservation: TaskReservation,
    prepared_epoch: u64,
    context_id: Option<String>,
    feedback_ids: Vec<String>,
    started: bool,
    released: bool,
    settlement: Option<TaskExecutionRecordV1>,
}

/// Read-only accounting from the common ledger. The effective charge includes observations
/// received after settlement; the terminal result and original reservation stay unchanged.
#[derive(Debug, Clone)]
pub struct TaskAttemptAccounting {
    pub attempt_id: String,
    pub invocation_id: String,
    pub plan_id: String,
    pub reservation: TaskReservation,
    pub started: bool,
    pub released: bool,
    pub charged_tokens: u128,
    pub state: Option<review_attempt::AttemptState>,
    pub result: Option<TaskAttemptResultV1>,
}

/// Unstarted reservation authority. Only the Store can construct this capability; a Worker
/// cannot choose its identity, feedback, writer epoch or resource allowance.
#[derive(Debug, Clone)]
pub struct ReservedTaskAttempt {
    task_id: String,
    invocation_id: String,
    plan_id: String,
    writer_epoch: u64,
    id: String,
    node: String,
    reservation: TaskReservation,
    feedback_ids: Vec<String>,
}

impl ReservedTaskAttempt {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }
    pub fn writer_epoch(&self) -> u64 {
        self.writer_epoch
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn node(&self) -> &str {
        &self.node
    }
    pub fn reservation(&self) -> &TaskReservation {
        &self.reservation
    }
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    pub fn feedback_ids(&self) -> &[String] {
        &self.feedback_ids
    }
}

/// The common Store returns this after publishing the reservation. It is not a wire type.
#[derive(Debug, Clone)]
pub struct PreparedTaskAttempt {
    task_id: String,
    writer_epoch: u64,
    id: String,
    node: String,
    reservation: TaskReservation,
    context_id: String,
}

impl PreparedTaskAttempt {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn writer_epoch(&self) -> u64 {
        self.writer_epoch
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn node(&self) -> &str {
        &self.node
    }
    pub fn reservation(&self) -> &TaskReservation {
        &self.reservation
    }
    pub fn context_id(&self) -> &str {
        &self.context_id
    }
}

fn invocation(cas: &Cas, id: &str) -> Result<TaskInvocationV1, StoreError> {
    let value: TaskInvocationV1 = payload(cas, id, TASK_INVOCATION_V1)?;
    value.validate().map_err(conflict)?;
    Ok(value)
}

fn output(cas: &Cas, id: &str) -> Result<TaskOutputV1, StoreError> {
    let value: TaskOutputV1 = payload(cas, id, TASK_OUTPUT_V1)?;
    value.validate().map_err(conflict)?;
    Ok(value)
}

fn verify_attempt_producer(
    cas: &Cas,
    task_id: &str,
    node: &str,
    attempt_id: &str,
    output_id: &str,
    output: &TaskOutputV1,
) -> Result<(), StoreError> {
    let expected = review_core::Producer::Attempt {
        run_id: task_run_id(task_id)?,
        node_id: node.into(),
        attempt_id: attempt_id.into(),
    };
    if envelope(cas, output_id, TASK_OUTPUT_V1)?.producer != expected {
        return Err(conflict("Task output wrapper belongs to another Attempt"));
    }
    for port in output.outputs.values() {
        for id in &port.artifact_ids {
            if envelope(cas, id, &port.artifact_type)?.producer != expected {
                return Err(conflict("Task output artifact belongs to another Attempt"));
            }
        }
    }
    Ok(())
}

pub(super) fn references(cas: &Cas, id: &str) -> Result<Vec<String>, StoreError> {
    let record = read_execution_record(cas, id)?.record;
    let mut refs: BTreeSet<_> = record
        .artifact_refs()
        .into_iter()
        .map(str::to_owned)
        .collect();
    refs.insert(id.into());
    let mut invocation_ids = Vec::new();
    match &record {
        TaskExecutionRecordV1::Invocation { invocation_id }
        | TaskExecutionRecordV1::Prepared { invocation_id, .. }
        | TaskExecutionRecordV1::Reserved { invocation_id, .. } => {
            invocation_ids.push(invocation_id.clone())
        }
        TaskExecutionRecordV1::OwnedChildrenRegistered { child_set_id } => {
            refs.extend(
                owned::read_task_owned_children(cas, child_set_id)?
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned),
            );
        }
        TaskExecutionRecordV1::Published { output_id, .. }
        | TaskExecutionRecordV1::OwnedChildPublished { output_id, .. }
        | TaskExecutionRecordV1::OwnedChildrenCompleted { output_id, .. }
        | TaskExecutionRecordV1::Settled {
            result: TaskAttemptResultV1::Succeeded { output_id },
            ..
        } => {
            let out = output(cas, output_id)?;
            invocation_ids.push(out.invocation_id.clone());
            for port in out.outputs.values() {
                validate_input_refs(cas, port, &mut refs)?;
            }
        }
        _ => (),
    }
    if let TaskExecutionRecordV1::OwnedChildPublished { child_set_id, .. }
    | TaskExecutionRecordV1::OwnedChildrenCompleted { child_set_id, .. } = &record
    {
        refs.extend(
            owned::read_task_owned_children(cas, child_set_id)?
                .artifact_refs()
                .into_iter()
                .map(str::to_owned),
        );
    }
    for id in invocation_ids {
        let input = invocation(cas, &id)?;
        refs.insert(id);
        refs.insert(input.plan_id);
        for port in input.inputs.values() {
            validate_input_refs(cas, port, &mut refs)?;
        }
    }
    Ok(refs.into_iter().collect())
}

impl TaskExecutionProjection {
    fn check_reservation(
        &self,
        lease: &TaskLease,
        attempt: &ReservedTaskAttempt,
    ) -> Result<&RecordedAttempt, StoreError> {
        let recorded = self
            .attempts
            .get(&attempt.id)
            .ok_or_else(|| conflict("Unknown Task reservation"))?;
        if attempt.task_id != lease.task_id
            || attempt.writer_epoch != lease.epoch
            || recorded.prepared_epoch != lease.epoch
            || recorded.invocation_id != attempt.invocation_id
            || recorded.plan_id != attempt.plan_id
            || recorded.reservation != attempt.reservation
            || recorded.feedback_ids != attempt.feedback_ids
            || recorded.started
            || recorded.released
            || recorded.settlement.is_some()
            || self.invocations.get(&attempt.node).map(|(id, _)| id) != Some(&attempt.invocation_id)
        {
            return Err(conflict(
                "Task reservation is stale or belongs to another authority",
            ));
        }
        Ok(recorded)
    }

    pub(super) fn enter_execution(
        &mut self,
        graph: CompiledTask,
        time: u64,
    ) -> Result<(), StoreError> {
        if !self.pending_attempts().is_empty() {
            return Err(conflict("Planning handoff has pending Attempts"));
        }
        // Feasibility uses remaining capacity, while the existing ledger retains all spent
        // reservations, late usage and the original deadline. Never create a second budget.
        graph
            .budget(self.budget.remaining_limits())
            .map_err(conflict)?;
        self.budget
            .install_graph_with_owned_templates(
                graph.allowances.clone(),
                graph
                    .calls
                    .iter()
                    .map(|(name, call)| (name.clone(), call.max_attempts))
                    .collect(),
                graph.token_scopes.clone(),
                owned::templates(&graph),
                time,
                false,
            )
            .map_err(conflict)?;
        self.graph = graph;
        self.invocations.clear();
        self.outputs.clear();
        Ok(())
    }
    /// Settlement selects a result durably before the scheduler publishes its ports. A new
    /// writer may finish that publication without starting another paid Attempt.
    pub fn reusable_output(&self, node: &str) -> Option<(String, String)> {
        self.attempts.iter().find_map(|(id, attempt)| {
            if attempt.reservation.node != node
                || self.invocations.get(node).map(|(_, i)| &i.plan_id) != Some(&attempt.plan_id)
                || self.ledger.attempt(&AttemptId(id.clone()))?.state
                    != review_attempt::AttemptState::Selected
            {
                return None;
            }
            match &attempt.settlement {
                Some(TaskExecutionRecordV1::Settled {
                    result: TaskAttemptResultV1::Succeeded { output_id },
                    ..
                }) => Some((output_id.clone(), id.clone())),
                _ => None,
            }
        })
    }

    pub fn retry_feedback(&self, node: &str) -> Vec<String> {
        self.attempts
            .values()
            .filter(|a| {
                a.reservation.node == node
                    && self.invocations.get(node).map(|(_, i)| &i.plan_id) == Some(&a.plan_id)
            })
            .filter_map(|a| match &a.settlement {
                Some(TaskExecutionRecordV1::Settled {
                    result:
                        TaskAttemptResultV1::Failed {
                            feedback_id: Some(id),
                            ..
                        },
                    ..
                }) => Some(id.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn validate_retry(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        node: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (invocation_id, input) = self
            .invocations
            .get(node)
            .ok_or_else(|| conflict("Unknown Task invocation"))?;
        let mut previous = BTreeMap::new();
        for (id, attempt) in self
            .attempts
            .iter()
            .filter(|(_, a)| &a.invocation_id == invocation_id && !a.released)
        {
            match &attempt.settlement {
                Some(TaskExecutionRecordV1::Settled {
                    result: TaskAttemptResultV1::Succeeded { .. },
                    ..
                }) => {
                    return Err(conflict(
                        "Task invocation already selected an output; recover its publication",
                    ));
                }
                Some(TaskExecutionRecordV1::Settled { result, .. }) => {
                    previous.insert(id.clone(), result.clone());
                }
                _ => return Err(conflict("Task invocation still has a pending Attempt")),
            }
        }
        if !previous.is_empty() {
            authority
                .validate_retry(cas, task, plan, input, &previous)
                .map_err(conflict)?;
        }
        Ok(())
    }

    fn new(cas: &Cas, state: &TaskProjection) -> Result<Self, StoreError> {
        let plan = plan(
            cas,
            state
                .plan_id
                .as_ref()
                .ok_or_else(|| conflict("Task has no plan"))?,
            state,
        )?;
        let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
        if graph.schema != "af.compiled-task/1" || graph.inputs != state.revision.inputs {
            return Err(conflict("Task execution requires its exact compiled graph"));
        }
        let planned = graph.scheduler_plan().map_err(conflict)?;
        if graph.order != planned.order {
            return Err(conflict("Compiled Task order is not canonical"));
        }
        let budget = graph
            .budget(state.revision.limits.clone())
            .map_err(conflict)?;
        let budget = if plan.preparation.is_some() {
            budget.with_deferred_verification().map_err(conflict)?
        } else {
            budget
        };
        Ok(Self {
            graph,
            budget,
            invocations: BTreeMap::new(),
            outputs: BTreeMap::new(),
            ledger: AttemptLedger::scoped(
                format!("af/task-attempts/1:{}", task_run_id(&state.task_id)?),
                BTreeMap::new(),
            ),
            attempts: BTreeMap::new(),
            brokers: BTreeMap::new(),
            owned: BTreeMap::new(),
        })
    }

    pub fn pending_attempts(&self) -> Vec<String> {
        self.attempts
            .iter()
            .filter(|(_, attempt)| !attempt.released && attempt.settlement.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Includes unstarted reservations so inspection can explain released work without
    /// counting it as an Attempt that began. Historical plans remain attached to each row.
    pub fn attempt_accounting(&self) -> Vec<TaskAttemptAccounting> {
        self.attempts
            .iter()
            .map(|(id, attempt)| {
                let ledger = self.ledger.attempt(&AttemptId(id.clone()));
                TaskAttemptAccounting {
                    attempt_id: id.clone(),
                    invocation_id: attempt.invocation_id.clone(),
                    plan_id: attempt.plan_id.clone(),
                    reservation: attempt.reservation.clone(),
                    started: attempt.started,
                    released: attempt.released,
                    charged_tokens: ledger.map_or(0, |row| row.charged),
                    state: ledger.map(|row| row.state),
                    result: match &attempt.settlement {
                        Some(TaskExecutionRecordV1::Settled { result, .. }) => Some(result.clone()),
                        _ => None,
                    },
                }
            })
            .collect()
    }

    /// Immutable observations of settled Attempts, including failures. These references do
    /// not grant output selection; a domain must verify its own typed facts and provenance.
    pub fn settled_artifacts(&self) -> BTreeMap<String, (String, Vec<String>)> {
        self.attempts
            .iter()
            .filter_map(|(id, attempt)| match &attempt.settlement {
                Some(TaskExecutionRecordV1::Settled {
                    raw_artifact_ids, ..
                }) => Some((
                    id.clone(),
                    (attempt.reservation.node.clone(), raw_artifact_ids.clone()),
                )),
                _ => None,
            })
            .collect()
    }

    fn verify_invocation(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<(), StoreError> {
        self.check_owned_open(&input.node)?;
        let resolved = self.resolve_node(&input.node)?;
        if let Some(expected) = resolved.expected_inputs {
            return if input.inputs == expected {
                Ok(())
            } else {
                Err(conflict("Owned invocation changed its registered inputs"))
            };
        }
        let node = &resolved.definition;
        let sources = node.inputs.clone();
        let mut expected = BTreeMap::new();
        for (name, source) in sources {
            let value = self
                .outputs
                .get(&source.node)
                .and_then(|(_, output)| output.outputs.get(&source.port));
            if let Some(value) = value {
                expected.insert(name, value.clone());
            } else if node
                .contract
                .inputs
                .get(&name)
                .is_some_and(|port| !port.optional)
            {
                return Err(conflict("Required Task input has no admitted producer"));
            }
        }
        if input.inputs != expected {
            return Err(conflict(
                "Invocation does not match the exact admitted graph inputs",
            ));
        }
        let mut ids: BTreeMap<_, _> = input
            .inputs
            .iter()
            .map(|(name, port)| (name.clone(), port.artifact_ids.clone()))
            .collect();
        for (index, condition) in node.conditions.iter().enumerate() {
            if let Some(value) = self
                .outputs
                .get(&condition.source.node)
                .and_then(|(_, output)| output.outputs.get(&condition.source.port))
            {
                ids.insert(condition_input(index), value.artifact_ids.clone());
            }
        }
        let selected = self
            .graph
            .node_selected(&input.node, &ids, |id| {
                let value: ArtifactEnvelope =
                    serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                validate_envelope(&value)?;
                serde_json::from_value(
                    value
                        .payload
                        .get("outcome")
                        .cloned()
                        .ok_or("Typed branch receipt has no outcome")?,
                )
                .map_err(|e| e.to_string())
            })
            .map_err(conflict)?;
        if !selected {
            return Err(conflict(
                "Inactive Task branch cannot publish an invocation",
            ));
        }
        Ok(())
    }

    fn verify_output(
        &self,
        cas: &Cas,
        receipt: &TaskOutputV1,
    ) -> Result<TaskInvocationV1, StoreError> {
        let input = invocation(cas, &receipt.invocation_id)?;
        if self.invocations.get(&input.node).map(|(id, _)| id) != Some(&receipt.invocation_id) {
            return Err(conflict("Output has no admitted invocation"));
        }
        let resolved = self.resolve_node(&input.node)?;
        let node = &resolved.definition;
        if matches!(node.operator, CompiledOperator::RootInputs)
            && receipt.outputs != self.graph.inputs
        {
            return Err(conflict(
                "Root input receipt changed the Task's normalized inputs",
            ));
        }
        if matches!(node.operator, CompiledOperator::Select) {
            let values = input
                .inputs
                .iter()
                .map(|(port, value)| (port.clone(), value.artifact_ids.clone()))
                .collect();
            let selected = self
                .graph
                .select_output(&input.node, &values, |id| {
                    let value = envelope(cas, id, &input.inputs["condition"].artifact_type)
                        .map_err(|e| e.to_string())?;
                    serde_json::from_value(
                        value
                            .payload
                            .get("outcome")
                            .cloned()
                            .ok_or("Typed branch receipt has no outcome")?,
                    )
                    .map_err(|e| e.to_string())
                })
                .map_err(conflict)?;
            let actual: BTreeMap<_, _> = receipt
                .outputs
                .iter()
                .map(|(port, value)| (port.clone(), value.artifact_ids.clone()))
                .collect();
            if actual != selected {
                return Err(conflict(
                    "Select output differs from its admitted condition and arm",
                ));
            }
        }
        for (name, contract) in &node.contract.outputs {
            match receipt.outputs.get(name) {
                None if contract.optional => (),
                None => return Err(conflict(format!("Task output lacks {name}"))),
                Some(value) => {
                    if value.artifact_type != contract.artifact_type
                        || value.cardinality != contract.cardinality
                    {
                        return Err(conflict("Task output changed its port contract"));
                    }
                    match &contract.affinity {
                        review_core::task::pipeline::PortAffinityV1::SameAs { input: source } => {
                            if input.inputs.get(source).map(|i| &i.snapshot_id)
                                != Some(&value.snapshot_id)
                            {
                                return Err(conflict(
                                    "Task output changed its input Snapshot affinity",
                                ));
                            }
                        }
                        review_core::task::pipeline::PortAffinityV1::DerivedFrom {
                            input: source,
                        } => {
                            let prior = input
                                .inputs
                                .get(source)
                                .and_then(|i| i.snapshot_id.as_ref());
                            if value.snapshot_id.is_none()
                                || prior.is_none()
                                || value.snapshot_id.as_ref() == prior
                            {
                                return Err(conflict(
                                    "Derived Task output did not produce a distinct Snapshot",
                                ));
                            }
                        }
                        _ => (),
                    }
                }
            }
        }
        if receipt
            .outputs
            .keys()
            .any(|port| !node.contract.outputs.contains_key(port))
        {
            return Err(conflict("Task output has an undeclared port"));
        }
        Ok(input)
    }
}

impl TaskProjection {
    fn check_prepared_capability(
        &self,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
    ) -> Result<(), StoreError> {
        let recorded = self
            .execution
            .as_ref()
            .and_then(|e| e.attempts.get(&attempt.id))
            .ok_or_else(|| conflict("Unknown prepared Task Attempt"))?;
        if attempt.task_id != lease.task_id
            || attempt.writer_epoch != lease.epoch
            || recorded.prepared_epoch != lease.epoch
            || recorded.context_id.as_deref() != Some(attempt.context_id.as_str())
            || recorded.reservation != attempt.reservation
            || recorded.reservation.node != attempt.node
            || self.plan_id.as_ref() != Some(&recorded.plan_id)
        {
            return Err(conflict(
                "Prepared Task capability differs from admitted context or authority",
            ));
        }
        Ok(())
    }

    pub(super) fn apply_execution(
        &mut self,
        cas: &Cas,
        record_id: &str,
        time: u64,
    ) -> Result<(), StoreError> {
        let record = read_execution_record(cas, record_id)?.record;
        let dispatching = matches!(
            record,
            TaskExecutionRecordV1::Invocation { .. }
                | TaskExecutionRecordV1::Prepared { .. }
                | TaskExecutionRecordV1::Reserved { .. }
                | TaskExecutionRecordV1::ContextBound { .. }
                | TaskExecutionRecordV1::Started { .. }
                | TaskExecutionRecordV1::Published { .. }
        );
        if dispatching {
            if !self.admitted || self.phase != (TaskPhaseV1::Running {}) {
                return Err(conflict("Task execution is not admitted and running"));
            }
            self.check_approval(cas, time)?;
        }
        if owned::is_owned_record(&record) {
            if !self.admitted || self.phase != (TaskPhaseV1::Running {}) {
                return Err(conflict(
                    "Owned Task recording requires admitted running execution",
                ));
            }
            self.check_plan_decision(cas, time)?;
        }
        if self.execution.is_none() {
            self.execution = Some(TaskExecutionProjection::new(cas, self)?);
        }
        let execution = self.execution.as_mut().expect("execution initialized");
        match &record {
            TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
            | TaskExecutionRecordV1::OwnedChildPublished { .. }
            | TaskExecutionRecordV1::OwnedChildrenCompleted { .. } => {
                execution.apply_owned(cas, &self.task_id, &record)?;
            }
            TaskExecutionRecordV1::UsageObserved {
                attempt_id,
                charged_tokens,
                ..
            } => {
                let attempt = execution
                    .attempts
                    .get(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?;
                if !attempt.started || attempt.released {
                    return Err(conflict("Usage observation requires started Task work"));
                }
                execution
                    .budget
                    .observe_charge_exact(&attempt.reservation.id, *charged_tokens)
                    .map_err(conflict)?;
                let id = AttemptId(attempt_id.clone());
                let charged = execution
                    .ledger
                    .attempt(&id)
                    .map_or(0, |a| a.charged)
                    .max(*charged_tokens);
                execution
                    .ledger
                    .charge_exact(&id, charged)
                    .map_err(conflict)?;
            }
            TaskExecutionRecordV1::Invocation { invocation_id } => {
                let input = invocation(cas, invocation_id)?;
                if self.plan_id.as_ref() != Some(&input.plan_id) {
                    return Err(conflict("Task invocation belongs to another plan"));
                }
                if execution.invocations.contains_key(&input.node) {
                    return Err(conflict("Task invocation is duplicated"));
                }
                execution.verify_invocation(cas, &input)?;
                execution.verify_owned_invocation_id(&input.node, invocation_id)?;
                execution
                    .invocations
                    .insert(input.node.clone(), (invocation_id.clone(), input));
            }
            TaskExecutionRecordV1::Prepared {
                invocation_id,
                attempt_id,
                reservation_id,
                reserved_tokens,
                deadline_unix_ms,
                feedback_ids,
                ..
            }
            | TaskExecutionRecordV1::Reserved {
                invocation_id,
                attempt_id,
                reservation_id,
                reserved_tokens,
                deadline_unix_ms,
                feedback_ids,
            } => {
                let input = invocation(cas, invocation_id)?;
                execution.check_owned_open(&input.node)?;
                if execution.invocations.get(&input.node).map(|(id, _)| id) != Some(invocation_id)
                    || execution.outputs.contains_key(&input.node)
                    || execution.reusable_output(&input.node).is_some()
                {
                    return Err(conflict("Task Attempt has no pending invocation"));
                }
                if execution.attempts.values().any(|a| {
                    &a.invocation_id == invocation_id && !a.released && a.settlement.is_none()
                }) {
                    return Err(conflict("Task invocation already has a live Attempt"));
                }
                let expected_feedback: Vec<_> = execution
                    .attempts
                    .values()
                    .filter(|a| &a.invocation_id == invocation_id)
                    .filter_map(|a| match &a.settlement {
                        Some(TaskExecutionRecordV1::Settled {
                            result:
                                TaskAttemptResultV1::Failed {
                                    feedback_id: Some(id),
                                    ..
                                },
                            ..
                        }) => Some(id.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if feedback_ids != &expected_feedback {
                    return Err(conflict(
                        "Retry input does not match admitted Task feedback",
                    ));
                }
                let reservation = execution
                    .budget
                    .prepare(&input.node, time)
                    .map_err(conflict)?;
                if reservation.id != *reservation_id
                    || reservation.tokens != *reserved_tokens
                    || reservation.deadline_unix_ms != *deadline_unix_ms
                {
                    return Err(conflict("Task reservation differs from shared accounting"));
                }
                let attempt = execution.ledger.dispatch(&input.node);
                if attempt.0 != *attempt_id {
                    return Err(conflict(
                        "Task Attempt identity differs from its durable namespace",
                    ));
                }
                execution.attempts.insert(
                    attempt_id.clone(),
                    RecordedAttempt {
                        invocation_id: invocation_id.clone(),
                        plan_id: input.plan_id.clone(),
                        reservation,
                        prepared_epoch: self.epoch,
                        context_id: match &record {
                            TaskExecutionRecordV1::Prepared { context_id, .. } => {
                                Some(context_id.clone())
                            }
                            _ => None,
                        },
                        feedback_ids: feedback_ids.clone(),
                        started: false,
                        released: false,
                        settlement: None,
                    },
                );
            }
            TaskExecutionRecordV1::ContextBound {
                attempt_id,
                context_id,
            } => {
                let node = &execution
                    .attempts
                    .get(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?
                    .reservation
                    .node;
                execution.check_owned_open(node)?;
                let attempt = execution
                    .attempts
                    .get_mut(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?;
                if attempt.started
                    || attempt.released
                    || attempt.settlement.is_some()
                    || attempt.prepared_epoch != self.epoch
                    || attempt.context_id.is_some()
                    || self.plan_id.as_ref() != Some(&attempt.plan_id)
                {
                    return Err(conflict("Task context cannot bind this reservation"));
                }
                attempt.context_id = Some(context_id.clone());
            }
            TaskExecutionRecordV1::Started { attempt_id } => {
                let node = &execution
                    .attempts
                    .get(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?
                    .reservation
                    .node;
                execution.check_owned_open(node)?;
                let attempt = execution
                    .attempts
                    .get_mut(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?;
                if attempt.context_id.is_none()
                    || attempt.started
                    || attempt.released
                    || attempt.settlement.is_some()
                    || attempt.prepared_epoch != self.epoch
                {
                    return Err(conflict(
                        "Task Attempt cannot start under this writer epoch",
                    ));
                }
                execution
                    .budget
                    .begin(&attempt.reservation.id, time)
                    .map_err(conflict)?;
                attempt.started = true;
            }
            TaskExecutionRecordV1::Released { attempt_id, .. } => {
                let attempt = execution
                    .attempts
                    .get_mut(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?;
                execution
                    .budget
                    .release(&attempt.reservation.id)
                    .map_err(conflict)?;
                execution.ledger.fence(&attempt.reservation.node);
                attempt.released = true;
            }
            TaskExecutionRecordV1::Settled {
                attempt_id,
                charged_tokens,
                result,
                ..
            } => {
                let attempt = execution
                    .attempts
                    .get(attempt_id)
                    .ok_or_else(|| conflict("Unknown Task Attempt"))?
                    .clone();
                if !attempt.started || attempt.released || attempt.settlement.is_some() {
                    return Err(conflict("Task settlement has no unsettled started Attempt"));
                }
                if let TaskAttemptResultV1::Succeeded { output_id } = result {
                    let out = output(cas, output_id)?;
                    verify_attempt_producer(
                        cas,
                        &self.task_id,
                        &attempt.reservation.node,
                        attempt_id,
                        output_id,
                        &out,
                    )?;
                    if out.invocation_id != attempt.invocation_id {
                        return Err(conflict(
                            "Task Attempt returned another invocation's output",
                        ));
                    }
                    execution.verify_output(cas, &out)?;
                }
                if matches!(result, TaskAttemptResultV1::Abandoned { .. })
                    && *charged_tokens < u128::from(attempt.reservation.tokens)
                {
                    return Err(conflict(
                        "Abandoned Task work retains its full reserved charge",
                    ));
                }
                // Preserve the original terminal receipt for exact replay. The common
                // accounting projection also retains every earlier cumulative usage floor.
                let charge = execution
                    .ledger
                    .attempt(&AttemptId(attempt_id.clone()))
                    .map_or(0, |a| a.charged)
                    .max(*charged_tokens);
                execution
                    .budget
                    .settle_exact(&attempt.reservation.id, charge)
                    .map_err(conflict)?;
                execution
                    .ledger
                    .charge_exact(&AttemptId(attempt_id.clone()), charge)
                    .map_err(conflict)?;
                if !matches!(result, TaskAttemptResultV1::Succeeded { .. })
                    || attempt.prepared_epoch != self.epoch
                {
                    execution.ledger.fence(&attempt.reservation.node);
                } else if let TaskAttemptResultV1::Succeeded { output_id } = result {
                    if execution
                        .ledger
                        .admit_exact(&ExactReceipt {
                            attempt: AttemptId(attempt_id.clone()),
                            output: output_id.clone(),
                            cost: charge,
                        })
                        .map_err(conflict)?
                        != Selection::Selected
                    {
                        return Err(conflict("Task settlement was quarantined"));
                    }
                }
                execution
                    .attempts
                    .get_mut(attempt_id)
                    .expect("known Attempt")
                    .settlement = Some(record.clone());
            }
            TaskExecutionRecordV1::Published {
                output_id,
                attempt_id,
            } => {
                let out = output(cas, output_id)?;
                let input = execution.verify_output(cas, &out)?;
                if execution.resolve_node(&input.node)?.owned.is_some()
                    || execution.graph.owned_children.contains_key(&input.node)
                {
                    return Err(conflict(
                        "Owned child publication requires its registered ownership record",
                    ));
                }
                if execution.outputs.contains_key(&input.node) {
                    return Err(conflict("Task output has already been published"));
                }
                if let Some(id) = attempt_id {
                    let attempt = execution
                        .attempts
                        .get(id)
                        .ok_or_else(|| conflict("Unknown output Attempt"))?;
                    let Some(TaskExecutionRecordV1::Settled {
                        result:
                            TaskAttemptResultV1::Succeeded {
                                output_id: selected,
                            },
                        ..
                    }) = &attempt.settlement
                    else {
                        return Err(conflict("Task output has no successful settlement"));
                    };
                    if selected != output_id
                        || execution
                            .ledger
                            .attempt(&AttemptId(id.clone()))
                            .is_none_or(|a| a.state != review_attempt::AttemptState::Selected)
                    {
                        return Err(conflict("Task output is stale or fenced"));
                    }
                } else if execution.resolve_node(&input.node)?.allowance.is_some() {
                    return Err(conflict(
                        "A paid Task node needs a settled selected Attempt",
                    ));
                }
                execution
                    .outputs
                    .insert(input.node, (output_id.clone(), out));
            }
        }
        Ok(())
    }
}

impl EventStore {
    /// Recover a previous writer's pending work before new dispatch. Started work is charged
    /// conservatively; only a durable prepare with no start record may release its credit.
    pub fn recover_task_attempts(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
    ) -> Result<(), StoreError> {
        self.recover_task_attempts_at(cas, lease, now()?)
    }

    pub(super) fn recover_task_attempts_at(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        time: u64,
    ) -> Result<(), StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        let Some(execution) = state.execution else {
            return Ok(());
        };
        if execution
            .attempts
            .values()
            .any(|a| !a.released && a.settlement.is_none() && a.prepared_epoch == lease.epoch)
        {
            return Err(conflict("Current writer still owns a pending Task Attempt"));
        }
        let walls = self.task_attempt_wall(&task_run_id(&lease.task_id)?)?;
        for (id, attempt) in execution
            .attempts
            .iter()
            .filter(|(_, a)| !a.released && a.settlement.is_none())
        {
            if attempt.prepared_epoch == lease.epoch {
                return Err(conflict("Current writer still owns this Task Attempt"));
            }
            let record = if attempt.started {
                let observed = walls
                    .iter()
                    .filter(|w| &w.attempt_id == id)
                    .filter_map(|w| w.usage.as_ref().map(|u| u.chargeable_tokens.get()))
                    .max()
                    .unwrap_or(0)
                    .max(
                        execution
                            .ledger
                            .attempt(&AttemptId(id.clone()))
                            .map_or(0, |a| a.charged),
                    );
                let diagnostic_id = cas.put_json(&json!({"schema":"af.task-diagnostic/1", "reason":"previous Task writer disappeared", "attempt_id":id})).map_err(|e| StoreError::Artifact(e.to_string()))?;
                let usage_id = walls
                    .iter()
                    .find(|wall| &wall.attempt_id == id)
                    .and_then(|wall| wall.usage.as_ref())
                    .map(|usage| {
                        cas.put_artifact(
                            review_core::task::usage::TASK_TOKEN_USAGE_V2,
                            review_core::Producer::Attempt {
                                run_id: task_run_id(&lease.task_id)?,
                                node_id: attempt.reservation.node.clone(),
                                attempt_id: id.clone(),
                            },
                            attempt.context_id.iter().cloned().collect(),
                            None,
                            serde_json::to_value(usage)?,
                        )
                        .map(|(id, _)| id)
                        .map_err(|error| StoreError::Artifact(error.to_string()))
                    })
                    .transpose()?;
                TaskExecutionRecordV1::Settled {
                    attempt_id: id.clone(),
                    charged_tokens: observed.max(u128::from(attempt.reservation.tokens)),
                    result: TaskAttemptResultV1::Abandoned { diagnostic_id },
                    raw_artifact_ids: vec![],
                    usage_id,
                }
            } else {
                TaskExecutionRecordV1::Released {
                    attempt_id: id.clone(),
                    reason: "Recovered before durable dispatch".into(),
                }
            };
            self.task_execution_record(cas, lease, record, time)?;
        }
        Ok(())
    }

    /// Record a trusted cumulative usage floor, even after approval or Attempt authority ends.
    /// The current Store writer must retain receipt identities; the observation grants no
    /// execution authority and settlement cannot refund already recorded usage.
    pub fn observe_task_usage(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        observation: TaskExecutionRecordV1,
    ) -> Result<(), StoreError> {
        if !matches!(observation, TaskExecutionRecordV1::UsageObserved { .. }) {
            return Err(conflict("Expected a Task usage observation"));
        }
        self.task_execution_record(cas, lease, observation, now()?)?;
        Ok(())
    }

    fn task_execution_record(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        record: TaskExecutionRecordV1,
        time: u64,
    ) -> Result<RunEvent, StoreError> {
        self.task_execution_record_inner(cas, lease, record, time, None)
    }

    fn task_execution_record_with_owned_prefix(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        record: TaskExecutionRecordV1,
        time: u64,
        prefix: (u64, Option<(String, u64)>),
    ) -> Result<RunEvent, StoreError> {
        self.task_execution_record_inner(cas, lease, record, time, Some(prefix))
    }

    fn task_execution_record_inner(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        record: TaskExecutionRecordV1,
        time: u64,
        prefix: Option<(u64, Option<(String, u64)>)>,
    ) -> Result<RunEvent, StoreError> {
        let (kind, payload) = encoding::encode_record(&record)?;
        let (id, _) = cas
            .put_artifact(
                kind,
                review_core::Producer::KernelOperation {
                    run_id: task_run_id(&lease.task_id)?,
                    node_id: None,
                    operation_id: "task-execution@1".into(),
                },
                record
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                payload,
            )
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        self.append_task_transition_with_owned_prefix(
            cas,
            &lease.task_id,
            TaskTransitionV1 {
                writer: lease.writer.clone(),
                epoch: lease.epoch,
                now_unix_ms: time,
                change: TaskChangeV1::ExecutionRecorded { record_id: id },
            },
            prefix,
        )
    }

    pub fn record_task_invocation(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        invocation_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, _) = self.checked_task_dispatch(cas, lease, authority)?;
        let input = invocation(cas, invocation_id)?;
        if let Some((old, _)) = state
            .execution
            .as_ref()
            .and_then(|e| e.invocations.get(&input.node))
        {
            return if old == invocation_id {
                Ok(())
            } else {
                Err(conflict("Invocation replay changed its inputs"))
            };
        }
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Invocation {
                invocation_id: invocation_id.into(),
            },
            now()?,
        )?;
        Ok(())
    }

    /// Reserve before rendering, so exact context can include the real Attempt authority.
    /// This does not start work or consume an Attempt; the reservation can still be released.
    pub fn reserve_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        node: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<ReservedTaskAttempt, StoreError> {
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
        let mut execution = state
            .execution
            .ok_or_else(|| conflict("Task has no recorded invocation"))?;
        execution.check_owned_open(node)?;
        execution.validate_retry(cas, &state.revision, &plan, node, authority)?;
        let (invocation_id, input) = execution
            .invocations
            .get(node)
            .ok_or_else(|| conflict("Unknown Task invocation"))?;
        let time = now()?;
        let reservation = execution.budget.prepare(node, time).map_err(conflict)?;
        let attempt = execution.ledger.dispatch(node);
        let reserved = ReservedTaskAttempt {
            task_id: lease.task_id.clone(),
            invocation_id: invocation_id.clone(),
            plan_id: input.plan_id.clone(),
            writer_epoch: lease.epoch,
            id: attempt.0,
            node: node.into(),
            reservation,
            feedback_ids: execution.retry_feedback(node),
        };
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Reserved {
                invocation_id: reserved.invocation_id.clone(),
                attempt_id: reserved.id.clone(),
                reservation_id: reserved.reservation.id.clone(),
                reserved_tokens: reserved.reservation.tokens,
                deadline_unix_ms: reserved.reservation.deadline_unix_ms,
                feedback_ids: reserved.feedback_ids.clone(),
            },
            time,
        )?;
        Ok(reserved)
    }

    /// Bind only the context admitted for this exact live reservation. A second binding is
    /// rejected, including under the same writer; recovery releases unstarted reservations.
    pub fn bind_task_attempt_context(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &ReservedTaskAttempt,
        context_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<PreparedTaskAttempt, StoreError> {
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        execution.check_owned_open(&attempt.node)?;
        let recorded = execution.check_reservation(lease, attempt)?;
        if recorded.context_id.is_some() {
            return Err(conflict("Task reservation already has a bound context"));
        }
        let input = invocation(cas, &attempt.invocation_id)?;
        authority
            .validate_context_for_attempt(cas, &state.revision, &plan, &input, attempt, context_id)
            .map_err(conflict)?;
        // Domain callbacks cannot leave stale plan, revocation or artifact authority admitted.
        self.check_task_dispatch(cas, lease, authority)?;
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::ContextBound {
                attempt_id: attempt.id.clone(),
                context_id: context_id.into(),
            },
            now()?,
        )?;
        Ok(PreparedTaskAttempt {
            task_id: lease.task_id.clone(),
            writer_epoch: lease.epoch,
            id: attempt.id.clone(),
            node: attempt.node.clone(),
            reservation: attempt.reservation.clone(),
            context_id: context_id.into(),
        })
    }

    pub fn release_reserved_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &ReservedTaskAttempt,
        reason: &str,
    ) -> Result<(), StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?
            .check_reservation(lease, attempt)?;
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Released {
                attempt_id: attempt.id.clone(),
                reason: reason.into(),
            },
            now()?,
        )?;
        Ok(())
    }

    pub fn prepare_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        node: &str,
        context_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<PreparedTaskAttempt, StoreError> {
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
        let mut execution = state
            .execution
            .ok_or_else(|| conflict("Task has no recorded invocation"))?;
        execution.check_owned_open(node)?;
        execution.validate_retry(cas, &state.revision, &plan, node, authority)?;
        let (invocation_id, invocation) = execution
            .invocations
            .get(node)
            .ok_or_else(|| conflict("Unknown Task invocation"))?;
        let time = now()?;
        let reservation = execution.budget.prepare(node, time).map_err(conflict)?;
        let attempt = execution.ledger.dispatch(node);
        let feedback_ids: Vec<String> = execution
            .attempts
            .values()
            .filter(|a| &a.invocation_id == invocation_id)
            .filter_map(|a| match &a.settlement {
                Some(TaskExecutionRecordV1::Settled {
                    result:
                        TaskAttemptResultV1::Failed {
                            feedback_id: Some(id),
                            ..
                        },
                    ..
                }) => Some(id.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        authority
            .validate_context(
                cas,
                &state.revision,
                &plan,
                invocation,
                &feedback_ids,
                context_id,
            )
            .map_err(conflict)?;
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Prepared {
                invocation_id: invocation_id.clone(),
                attempt_id: attempt.0.clone(),
                reservation_id: reservation.id.clone(),
                reserved_tokens: reservation.tokens,
                deadline_unix_ms: reservation.deadline_unix_ms,
                context_id: context_id.into(),
                feedback_ids,
            },
            time,
        )?;
        Ok(PreparedTaskAttempt {
            task_id: lease.task_id.clone(),
            writer_epoch: lease.epoch,
            id: attempt.0,
            node: node.into(),
            reservation,
            context_id: context_id.into(),
        })
    }

    pub fn start_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, _) = self.checked_task_dispatch(cas, lease, authority)?;
        state.check_prepared_capability(lease, attempt)?;
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Started {
                attempt_id: attempt.id.clone(),
            },
            now()?,
        )?;
        Ok(())
    }

    /// Recheck a domain-owned effect during the one common started Attempt. Broker adapters
    /// can use this boundary without maintaining another Attempt ledger or accepting an ID alone.
    pub fn check_task_attempt_current(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, _) = self.checked_task_dispatch(cas, lease, authority)?;
        state.check_prepared_capability(lease, attempt)?;
        state
            .execution
            .as_ref()
            .expect("checked execution")
            .check_owned_open(&attempt.node)?;
        if state
            .execution
            .as_ref()
            .is_some_and(|e| e.budget.breached())
        {
            return Err(conflict(
                "Task Attempt cannot authorize another effect after a budget overrun",
            ));
        }
        let recorded = &state
            .execution
            .as_ref()
            .expect("checked prepared execution")
            .attempts[attempt.id()];
        if !recorded.started
            || recorded.released
            || recorded.settlement.is_some()
            || now()? >= recorded.reservation.deadline_unix_ms
        {
            return Err(conflict("Task Attempt is not current started work"));
        }
        Ok(())
    }

    pub fn release_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        reason: &str,
    ) -> Result<(), StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state.check_prepared_capability(lease, attempt)?;
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Released {
                attempt_id: attempt.id.clone(),
                reason: reason.into(),
            },
            now()?,
        )?;
        Ok(())
    }

    pub fn settle_task_attempt(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        mut settlement: TaskExecutionRecordV1,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let TaskExecutionRecordV1::Settled { attempt_id, .. } = &settlement else {
            return Err(conflict("Expected a Task settlement"));
        };
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: now()?,
            change: TaskChangeV1::Resumed {},
        })?;
        if let Some(old) = state
            .execution
            .as_ref()
            .and_then(|e| e.attempts.get(attempt_id))
            .and_then(|a| a.settlement.as_ref())
        {
            return if old == &settlement {
                Ok(())
            } else {
                Err(conflict("Conflicting Task settlement"))
            };
        }
        let failure = match &settlement {
            TaskExecutionRecordV1::Settled {
                result: TaskAttemptResultV1::Succeeded { output_id },
                ..
            } => (|| {
                let id = state
                    .plan_id
                    .as_ref()
                    .ok_or_else(|| conflict("Task has no plan"))?;
                let plan = self.authorized_plan(cas, &state, id, authority)?;
                Self::validate_task_output(
                    cas,
                    &state,
                    &plan,
                    output_id,
                    Some(attempt_id),
                    authority,
                )
            })()
            .err(),
            _ => None,
        };
        if let Some(error) = &failure {
            let diagnostic_id = cas
                .put_json(&json!({"schema":"af.task-diagnostic/1", "error":error.to_string()}))
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            if let TaskExecutionRecordV1::Settled { result, .. } = &mut settlement {
                *result = TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: None,
                };
            }
        }
        self.task_execution_record(cas, lease, settlement, now()?)?;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn validate_task_output(
        cas: &Cas,
        state: &TaskProjection,
        plan: &ExecutionPlanV1,
        output_id: &str,
        attempt_id: Option<&str>,
        authority: &dyn TaskAuthority,
    ) -> Result<(TaskOutputV1, TaskInvocationV1), StoreError> {
        let out = output(cas, output_id)?;
        let input = invocation(cas, &out.invocation_id)?;
        if let Some(attempt_id) = attempt_id {
            verify_attempt_producer(
                cas,
                &state.task_id,
                &input.node,
                attempt_id,
                output_id,
                &out,
            )?;
        }
        state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no admitted execution"))?
            .verify_output(cas, &out)?;
        let mut refs = BTreeSet::new();
        for value in out.outputs.values() {
            validate_input_refs(cas, value, &mut refs)?;
        }
        for id in refs {
            cas.verify(&id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
        }
        authority
            .validate_output(cas, &state.revision, plan, &input, &out)
            .map_err(conflict)?;
        Ok((out, input))
    }

    pub fn publish_task_output(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        output_id: &str,
        attempt_id: Option<&str>,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = self.checked_task_dispatch(cas, lease, authority)?;
        let (_, input) =
            Self::validate_task_output(cas, &state, &plan, output_id, attempt_id, authority)?;
        if let Some(execution) = &state.execution
            && (execution.resolve_node(&input.node)?.owned.is_some()
                || execution.graph.owned_children.contains_key(&input.node))
        {
            return Err(conflict(
                "Owned output requires its exact ownership publication API",
            ));
        }
        // New publication revalidates after the domain callback at append_task_transition.
        // Idempotent replay has no append, so retain that fresh read explicitly on this path.
        let state = if state
            .execution
            .as_ref()
            .is_some_and(|e| e.outputs.contains_key(&input.node))
        {
            self.task_projection(cas, &lease.task_id)?
                .ok_or_else(|| conflict("Unknown Task"))?
        } else {
            state
        };
        if let Some(execution) = &state.execution {
            if let Some((old, _)) = execution.outputs.get(&input.node) {
                let same_attempt = match attempt_id {
                    Some(id) => execution
                        .ledger
                        .attempt(&AttemptId(id.into()))
                        .is_some_and(|a| {
                            a.node == input.node
                                && a.state == review_attempt::AttemptState::Selected
                        }),
                    None => execution.resolve_node(&input.node)?.allowance.is_none(),
                };
                return if old == output_id && same_attempt {
                    Ok(())
                } else {
                    Err(conflict("Conflicting Task output replay"))
                };
            }
        }
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::Published {
                output_id: output_id.into(),
                attempt_id: attempt_id.map(str::to_owned),
            },
            now()?,
        )?;
        Ok(())
    }
}
