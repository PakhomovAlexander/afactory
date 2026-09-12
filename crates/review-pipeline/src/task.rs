//! One Task dispatcher over the existing graph scheduler and common Store. Domain handlers
//! supply typed operations; they do not schedule children or create their own Attempt budgets.

pub mod broker;
pub mod code;
pub(crate) mod control;
pub mod document;
pub mod host;
mod integration;
pub mod lease;
pub mod legacy_review;
mod owned;
pub mod planning;
pub mod provider;
mod provider_admissions;
pub use provider_admissions::TaskProviderAdmissionReport;
mod report;
pub mod review;
pub mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, atomic::AtomicBool};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use review_core::task::execution::*;
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use review_core::{ArtifactEnvelope, Producer};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_graph::{ArtifactMap, Dispatch, Node, NodeFailureClass, RunReport};
use review_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use review_store::store::task::{TaskAuthority, TaskLease, TaskProjection, task_run_id};
use review_store::{Cas, EventStore, SharedEventStore};

/// Data expansion only. The captured template supplies every child contract, operator and
/// allowance; the Store registers this complete source order before any child can reserve.
pub struct TaskOwnedChildrenInputs {
    pub source_artifact_id: String,
    pub source_item_ids: Vec<String>,
}

pub struct TaskWorkOutput {
    pub usage_observation: Option<review_core::task::usage::TaskUsageObservationV1>,
    /// Adapter-reported counters survive output/CAS failure until durable accounting.
    pub usage: Option<review_core::task::usage::TaskTokenUsageV3>,
    pub outputs: Result<BTreeMap<String, ArtifactInputV1>, String>,
    /// None means usage is unavailable, so the complete reservation remains charged.
    pub charged_tokens: Option<u128>,
    pub raw_artifact_ids: Vec<String>,
    pub usage_id: Option<String>,
    pub feedback_id: Option<String>,
}

pub trait TaskOperatorHost: Sync {
    fn prepare_owned_children(
        &self,
        _cas: &Cas,
        _parent: &TaskInvocationV1,
    ) -> Result<TaskOwnedChildrenInputs, String> {
        Err("Task operator has no installed child expansion".into())
    }

    /// Pure terminal fold of Store-proven facts. This cannot invoke Workers or report usage.
    fn complete_owned_children(
        &self,
        _cas: &Cas,
        _parent: &TaskInvocationV1,
        _children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        _facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        Err("Task operator has no installed child completion".into())
    }

    /// Pure lookup of the exact captured operations for this invocation. None grants no
    /// Broker authority; this hook cannot create an Attempt or enlarge its reservation.
    fn broker_operations(
        &self,
        _cas: &Cas,
        _input: &TaskInvocationV1,
    ) -> Result<Option<Vec<review_core::BrokerOperationPolicyV1>>, String> {
        Ok(None)
    }

    /// Publish domain input identity after the common invocation is durable, before context
    /// capture or Attempt reservation. Replays call this again; publication must be idempotent
    /// and must not invoke Workers, checks or Providers. Pure context capture can then reference
    /// an actual canonical domain invocation event instead of predicting its identity.
    fn commit_domain_invocation(
        &self,
        _cas: &Cas,
        _invocation_id: &str,
        _input: &TaskInvocationV1,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Called after the common output is durably published, before downstream dispatch, and
    /// again on replay. Domain publication must be idempotent. This host hook cannot run paid
    /// work: the Attempt is already settled. Its failure preserves the output for recovery.
    fn commit_domain_output(
        &self,
        _cas: &Cas,
        _input: &TaskInvocationV1,
        _output_id: &str,
        _output: &TaskOutputV1,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Pure rendering/capture only: no Provider operation or subprocess may start here.
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback_ids: &[String],
    ) -> Result<String, String>;

    /// Same pure capture boundary, with the actual persisted reservation available to render
    /// adapters whose invocation protocol includes Attempt identity and resource authority.
    fn prepare_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.prepare_context(cas, input, attempt.feedback_ids())
    }

    /// A paid operation receives its durably started Attempt capability. Implementations must
    /// report failed usage too. Pure installed operators receive None and cannot launch Workers.
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput;

    /// Optional host interruption must be consumed explicitly; None retains existing hosts.
    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&AtomicBool>,
    ) -> TaskWorkOutput {
        if cancellation.is_some() {
            return control::refused("Task operator does not support cancellation");
        }
        self.execute_with_broker(cas, input, attempt, broker)
    }

    /// The runtime supplies an opaque client only after binding the already-started Attempt.
    /// Existing hosts refuse a capability they do not consume before performing any work.
    fn execute_with_broker(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> TaskWorkOutput {
        if broker.is_some() {
            return TaskWorkOutput {
                usage_observation: None,
                usage: None,
                outputs: Err("Task operator does not consume Broker Handles".into()),
                charged_tokens: Some(0),
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            };
        }
        self.execute(cas, input, attempt)
    }
}

pub struct TaskRuntime<'store, 'host> {
    store: SharedEventStore<'store>,
    cas: &'store Cas,
    lease: TaskLease,
    plan_id: String,
    graph: CompiledTask,
    integration:
        Option<review_store::store::task::review_integration::RegisteredTaskReviewIntegration>,
    authority: &'host dyn TaskAuthority,
    host: &'host dyn TaskOperatorHost,
    cancellation: Option<&'host AtomicBool>,
    broker_providers: BTreeMap<String, &'host broker::TaskBrokerProvider>,
    broker_probes: BTreeMap<String, &'host broker::TaskBrokerProvider>,
    prepared: Mutex<BTreeMap<String, PreparedTaskAttempt>>,
    pending_outputs: Mutex<BTreeMap<String, (String, Option<String>)>>,
    failures: Mutex<BTreeMap<String, NodeFailureClass>>,
    publication_failures: Mutex<BTreeSet<String>>,
}

fn envelope(cas: &Cas, id: &str) -> Result<ArtifactEnvelope, String> {
    cas.get_artifact(id).map_err(|e| e.to_string())
}

fn artifact_map(values: &BTreeMap<String, ArtifactInputV1>) -> ArtifactMap {
    values
        .iter()
        .map(|(name, value)| (name.clone(), value.artifact_ids.clone()))
        .collect()
}

impl<'store, 'host> TaskRuntime<'store, 'host> {
    pub fn new(
        store: &'store mut EventStore,
        cas: &'store Cas,
        lease: TaskLease,
        authority: &'host dyn TaskAuthority,
        host: &'host dyn TaskOperatorHost,
    ) -> Result<Self, String> {
        Self::with_store(SharedEventStore::new(store), cas, lease, authority, host)
    }

    /// Domain handlers may retain a clone for durable evidence and broker checks. Locks must
    /// be released before calling a handler or starting an external operation.
    pub fn with_store(
        store: SharedEventStore<'store>,
        cas: &'store Cas,
        lease: TaskLease,
        authority: &'host dyn TaskAuthority,
        host: &'host dyn TaskOperatorHost,
    ) -> Result<Self, String> {
        let (plan, projection) = {
            let locked = store.lock().expect("Task Store");
            let plan = locked
                .check_current_task_plan_for_recording(cas, &lease, authority)
                .map_err(|e| e.to_string())?;
            let projection = locked
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
            (plan, projection)
        };
        if projection
            .execution
            .as_ref()
            .is_some_and(|e| e.active_review_integration().is_some())
        {
            return Err(
                "Closed Review Round requires its dedicated Integration phase runtime".into(),
            );
        }
        Self::from_captured(store, cas, lease, authority, host, plan, projection, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn from_captured(
        store: SharedEventStore<'store>,
        cas: &'store Cas,
        lease: TaskLease,
        authority: &'host dyn TaskAuthority,
        host: &'host dyn TaskOperatorHost,
        plan: review_core::task::plan::ExecutionPlanV1,
        projection: TaskProjection,
        integration: Option<
            review_store::store::task::review_integration::RegisteredTaskReviewIntegration,
        >,
    ) -> Result<Self, String> {
        let value = envelope(cas, &plan.compiled_graph_id)?;
        if value.artifact_type != "af/CompiledTask@1" {
            return Err("Task plan has no compiled Task graph".into());
        }
        let graph: CompiledTask =
            serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
        if graph.schema != "af.compiled-task/1" {
            return Err("Unsupported compiled Task version".into());
        }
        Ok(Self {
            store,
            cas,
            lease,
            plan_id: projection.plan_id.ok_or("Task has no plan")?,
            graph,
            integration,
            authority,
            host,
            cancellation: None,
            broker_providers: BTreeMap::new(),
            broker_probes: BTreeMap::new(),
            prepared: Mutex::new(BTreeMap::new()),
            pending_outputs: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeMap::new()),
            publication_failures: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn with_cancellation(mut self, cancellation: &'host AtomicBool) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    pub fn execute(&self) -> Result<RunReport, String> {
        if self.integration.is_some() {
            return Err("Activated Integration runtime can execute only its captured phase".into());
        }
        let report = lease::with_heartbeat_controlled(
            &self.store,
            self.cas,
            &self.lease,
            self.cancellation,
            || self.graph.run(self),
        )?;
        self.record_run_report(&report)?;
        if !self
            .publication_failures
            .lock()
            .expect("Task publication failures")
            .is_empty()
        {
            self.store
                .lock()
                .expect("Task Store")
                .wait_task(
                    self.cas,
                    &self.lease,
                    review_core::task::TaskWaitingReasonV1::NeedsHuman,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(report)
    }

    pub fn projection(&self) -> Result<TaskProjection, String> {
        self.store
            .lock()
            .expect("Task Store")
            .task_projection(self.cas, self.lease.task_id())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Unknown Task".into())
    }

    pub fn task(&self) -> Result<TaskRevisionV1, String> {
        Ok(self.projection()?.revision)
    }

    pub fn finish(&self, result_id: &str) -> Result<(), String> {
        self.store
            .lock()
            .expect("Task Store")
            .finish_task(self.cas, &self.lease, result_id, self.authority)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn commit_domain_output(&self, id: &str) -> Result<(), String> {
        let output: TaskOutputV1 =
            serde_json::from_value(envelope(self.cas, id)?.payload).map_err(|e| e.to_string())?;
        let input: TaskInvocationV1 =
            serde_json::from_value(envelope(self.cas, &output.invocation_id)?.payload)
                .map_err(|e| e.to_string())?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.host
                .commit_domain_output(self.cas, &input, id, &output)
        }))
        .unwrap_or_else(|_| Err("Task domain publication panicked".into()));
        self.record_domain_publication(&input.node, result)
    }

    fn record_domain_publication(
        &self,
        node: &str,
        result: Result<(), String>,
    ) -> Result<(), String> {
        let mut failures = self
            .publication_failures
            .lock()
            .expect("Task publication failures");
        if result.is_err() {
            failures.insert(node.into());
        } else {
            failures.remove(node);
        }
        result
    }

    fn typed_inputs(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let resolved = self.resolve_node(&node.id)?;
        let mut typed = BTreeMap::new();
        for port in &node.inputs {
            if !resolved.definition.contract.inputs.contains_key(&port.name) {
                continue; // Scheduler guards are control authority, not declared Worker data.
            }
            let values = inputs.get(&port.name).map(Vec::as_slice).unwrap_or(&[]);
            if values.is_empty() {
                if port.optional {
                    continue;
                } else {
                    return Err(format!("Required input {} is absent", port.name));
                }
            }
            let mut snapshot = None;
            for (index, id) in values.iter().enumerate() {
                let value = envelope(self.cas, id)?;
                if value.artifact_type != port.artifact_type {
                    return Err(format!("Input {} has a different type", port.name));
                }
                if index == 0 {
                    snapshot = value.subject_snapshot_id;
                } else if snapshot != value.subject_snapshot_id {
                    return Err("One Task port spans different Snapshots".into());
                }
            }
            let value = ArtifactInputV1 {
                artifact_ids: values.to_vec(),
                artifact_type: port.artifact_type.clone(),
                cardinality: port.cardinality,
                snapshot_id: snapshot,
            };
            value.validate()?;
            typed.insert(port.name.clone(), value);
        }
        Ok(typed)
    }

    fn prepare(&self, input: &TaskInvocationV1) -> Result<PreparedTaskAttempt, String> {
        let attempt = self
            .store
            .lock()
            .expect("Task Store")
            .reserve_task_attempt(self.cas, &self.lease, &input.node, self.authority)
            .map_err(|e| e.to_string())?;
        // Release the Store lock before pure host capture; it may read shared domain evidence.
        let context = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.host
                .prepare_context_for_attempt(self.cas, input, &attempt)
        }))
        .unwrap_or_else(|_| Err("Task context capture panicked".into()));
        let result = context.and_then(|context_id| {
            self.store
                .lock()
                .expect("Task Store")
                .bind_task_attempt_context(
                    self.cas,
                    &self.lease,
                    &attempt,
                    &context_id,
                    self.authority,
                )
                .map_err(|e| e.to_string())
        });
        if result.is_err() {
            // Failure is recorded in the RunReport. Release uses a bounded stable reason,
            // never arbitrary host error text; old-writer recovery covers a lost lease.
            self.store
                .lock()
                .expect("Task Store")
                .release_reserved_task_attempt(
                    self.cas,
                    &self.lease,
                    &attempt,
                    "Task context was not admitted",
                )
                .map_err(|e| {
                    format!(
                        "{}; reservation release failed: {e}",
                        result.as_ref().unwrap_err()
                    )
                })?;
        }
        result
    }

    fn record_output(
        &self,
        input_id: &str,
        input: &TaskInvocationV1,
        values: BTreeMap<String, ArtifactInputV1>,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> Result<String, String> {
        let producer = match attempt {
            Some(attempt) => Producer::Attempt {
                run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                node_id: input.node.clone(),
                attempt_id: attempt.id().into(),
            },
            None => Producer::KernelOperation {
                run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                node_id: Some(input.node.clone()),
                operation_id: "task-node-output@1".into(),
            },
        };
        let mut refs = BTreeSet::from([input_id.to_owned()]);
        refs.extend(values.values().flat_map(|p| p.artifact_ids.iter().cloned()));
        let out = TaskOutputV1 {
            invocation_id: input_id.into(),
            outputs: values,
        };
        out.validate()?;
        self.cas
            .put_artifact(
                TASK_OUTPUT_V1,
                producer,
                refs.into_iter().collect(),
                None,
                serde_json::to_value(out).map_err(|e| e.to_string())?,
            )
            .map(|(id, _)| id)
            .map_err(|e| e.to_string())
    }
}

impl TaskRuntime<'_, '_> {
    fn capture_invocation(&self, input: &TaskInvocationV1) -> Result<String, String> {
        input.validate()?;
        if input.plan_id != self.plan_id {
            return Err("Task invocation changed its captured plan".into());
        }
        let refs: BTreeSet<_> = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .chain([input.plan_id.clone()])
            .collect();
        let (id, _) = self
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                Producer::KernelOperation {
                    run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                    node_id: Some(input.node.clone()),
                    operation_id: "task-node-invocation@1".into(),
                },
                refs.into_iter().collect(),
                None,
                serde_json::to_value(input).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn execute_node(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        let state = self
            .projection()?
            .execution
            .ok_or("Task has no invocation")?;
        if let Some((_, output)) = state.outputs.get(&node.id) {
            return Ok(artifact_map(&output.outputs));
        }
        if let Some((id, attempt)) = state.reusable_output(&node.id) {
            let output: TaskOutputV1 = serde_json::from_value(envelope(self.cas, &id)?.payload)
                .map_err(|e| e.to_string())?;
            self.pending_outputs
                .lock()
                .expect("Task outputs")
                .insert(node.id.clone(), (id, Some(attempt)));
            return Ok(artifact_map(&output.outputs));
        }
        let (input_id, input) = state
            .invocations
            .get(&node.id)
            .ok_or("Task invocation is not recorded")?;
        // Replaying an already selected result above is factual recovery. Every new operation,
        // including a pure installed operation, still requires current dispatch authority.
        control::check(self.cancellation)?;
        self.check_node_authority(&node.id, true)?;
        let resolved = state.resolve_node(&node.id).map_err(|e| e.to_string())?;
        let compiled = &resolved.definition;
        if matches!(
            compiled.operator,
            CompiledOperator::RootInputs | CompiledOperator::Select
        ) {
            let values = if matches!(compiled.operator, CompiledOperator::RootInputs) {
                self.graph.inputs.clone()
            } else {
                let selected = self.graph.select_output(&node.id, inputs, |id| {
                    serde_json::from_value(
                        envelope(self.cas, id)?
                            .payload
                            .get("outcome")
                            .cloned()
                            .ok_or("Missing outcome")?,
                    )
                    .map_err(|e| e.to_string())
                })?;
                let ids = &selected["output"];
                let value = input
                    .inputs
                    .iter()
                    .find(|(name, value)| {
                        name.as_str() != "condition" && &value.artifact_ids == ids
                    })
                    .map(|(_, value)| value.clone())
                    .ok_or("Selected value is not a declared arm")?;
                BTreeMap::from([("output".into(), value)])
            };
            control::check(self.cancellation)?;
            let out = self.record_output(input_id, input, values.clone(), None)?;
            self.pending_outputs
                .lock()
                .expect("Task outputs")
                .insert(node.id.clone(), (out, None));
            return Ok(artifact_map(&values));
        }
        let allowance = resolved.allowance.as_ref();
        let mut last_error = String::new();
        for index in 0..allowance.map_or(1, |a| a.max_attempts) {
            control::check(self.cancellation)?;
            let attempt = if allowance.is_some() {
                Some(if index == 0 {
                    self.prepared
                        .lock()
                        .expect("prepared Tasks")
                        .remove(&node.id)
                        .ok_or("Task Attempt was not prepared")?
                } else {
                    self.prepare(input)?
                })
            } else {
                None
            };
            if let Some(attempt) = &attempt {
                let start = self.store.lock().expect("Task Store").start_task_attempt(
                    self.cas,
                    &self.lease,
                    attempt,
                    self.authority,
                );
                if let Err(error) = start {
                    let _ = self.store.lock().expect("Task Store").release_task_attempt(
                        self.cas,
                        &self.lease,
                        attempt,
                        &error.to_string(),
                    );
                    return Err(error.to_string());
                }
            }
            let started = SystemTime::now();
            let timer = Instant::now();
            let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.execute_host(input, attempt.as_ref())
            }))
            .unwrap_or_else(|_| TaskWorkOutput {
                usage_observation: None,
                usage: None,
                outputs: Err("Task operator panicked".into()),
                charged_tokens: None,
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            });
            if let Some(attempt) = &attempt {
                // Capture known charge before output CAS admission: a crash during output or
                // diagnostic publication must not hide an already reported Provider overrun.
                // The event ledger remains authoritative; recovery only raises its charge.
                let usage_result = match result.usage.take() {
                    Some(usage) => Ok(Some(usage)),
                    None => result
                        .usage_id
                        .as_deref()
                        .map(|id| review_runner::task::usage::read_task_usage_exact(self.cas, id))
                        .transpose(),
                };
                let usage = usage_result.as_ref().ok().and_then(Option::as_ref);
                let observation = result.usage_observation.take();
                if let Some(observation) = &observation {
                    observation.validate()?;
                    if observation.reported_usage.as_ref() != usage {
                        result.outputs = Err(
                            "Native usage observation differs from the returned counters".into(),
                        );
                    }
                    if !observation.charge_complete {
                        result.outputs = Err("Native billing usage is incomplete".into());
                    }
                }
                let charge = result
                    .charged_tokens
                    .into_iter()
                    .chain(usage.as_ref().map(|usage| usage.chargeable_tokens.get()))
                    .chain(
                        observation
                            .as_ref()
                            .and_then(|o| o.reported_usage.as_ref())
                            .map(|u| u.chargeable_tokens.get()),
                    )
                    .chain(
                        observation
                            .as_ref()
                            .filter(|o| !o.charge_complete)
                            .map(|_| u128::from(attempt.reservation().tokens)),
                    )
                    .max();
                result.charged_tokens = charge;
                let wall = review_store::TaskAttemptWall {
                    run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                    attempt_id: attempt.id().into(),
                    node_id: node.id.clone(),
                    round: 0,
                    epoch: u32::try_from(self.lease.epoch()).unwrap_or(u32::MAX),
                    started_unix_ms: started
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                    elapsed_ms: timer.elapsed().as_millis() as u64,
                    usage: charge.map(|chargeable_tokens| {
                        review_core::task::usage::TaskTokenUsageV3 {
                            input_tokens: usage.as_ref().and_then(|u| u.input_tokens),
                            output_tokens: usage.as_ref().and_then(|u| u.output_tokens),
                            cache_read_tokens: usage.as_ref().and_then(|u| u.cache_read_tokens),
                            cache_write_tokens: usage.as_ref().and_then(|u| u.cache_write_tokens),
                            reasoning_tokens: usage.as_ref().and_then(|u| u.reasoning_tokens),
                            chargeable_tokens: chargeable_tokens.into(),
                        }
                    }),
                };
                let store = self.store.lock().expect("Task Store");
                match &observation {
                    Some(observation) => {
                        store.record_task_attempt_wall_with_observation(&wall, observation)
                    }
                    None => store.record_task_attempt_wall(&wall),
                }
                .map_err(|error| format!("Cannot retain Task usage before publication: {error}"))?;
                // SQL-only reads remain available through CAS failure. A repeated measurement
                // cannot erase earlier incompleteness or lower the effective sidecar floor.
                let observation = store
                    .task_attempt_usage_observation(&wall.run_id, attempt.id())
                    .map_err(|e| e.to_string())?;
                let prior_charge = store
                    .task_attempt_wall(&wall.run_id)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|w| w.attempt_id == attempt.id())
                    .and_then(|w| w.usage)
                    .map(|u| u.chargeable_tokens.get());
                drop(store);
                let charge = charge.into_iter().chain(prior_charge).max();
                result.charged_tokens = charge;
                if let Some(observation) = &observation {
                    if !observation.charge_complete {
                        result.outputs = Err("Native billing usage is incomplete".into());
                    }
                    let id = review_store::store::task::execution::usage_observation::capture_task_usage_observation(
                        self.cas, Producer::Attempt { run_id: wall.run_id.clone(), node_id: input.node.clone(), attempt_id: attempt.id().into() },
                        attempt.context_id(), observation,
                    ).map_err(|error| error.to_string())?;
                    result.raw_artifact_ids.push(id);
                }
                let usage = usage_result?;
                if let Some(charge) = charge
                    && (result.usage_id.is_none()
                        || usage
                            .as_ref()
                            .is_none_or(|u| u.chargeable_tokens.get() != charge))
                {
                    let mut usage = usage.unwrap_or_default();
                    usage.chargeable_tokens = charge.into();
                    result.usage_id = Some(review_runner::task::usage::persist_task_usage_exact(
                        self.cas,
                        Producer::Attempt {
                            run_id: wall.run_id,
                            node_id: input.node.clone(),
                            attempt_id: attempt.id().into(),
                        },
                        attempt.context_id(),
                        &usage,
                    )?);
                }
                if let (Some(charged_tokens), Some(usage_id)) = (charge, &result.usage_id) {
                    self.store
                        .lock()
                        .expect("Task Store")
                        .observe_task_usage(
                            self.cas,
                            &self.lease,
                            TaskExecutionRecordV1::UsageObserved {
                                attempt_id: attempt.id().into(),
                                charged_tokens,
                                usage_id: usage_id.clone(),
                                raw_artifact_ids: result.raw_artifact_ids.clone(),
                            },
                        )
                        .map_err(|error| error.to_string())?;
                }
            }
            let charged = result
                .charged_tokens
                .into_iter()
                .chain(
                    result
                        .usage
                        .as_ref()
                        .map(|usage| usage.chargeable_tokens.get()),
                )
                .max()
                .unwrap_or_else(|| {
                    attempt
                        .as_ref()
                        .map_or(0, |a| u128::from(a.reservation().tokens))
                });
            if attempt.is_none() && (charged != 0 || result.usage_observation.is_some()) {
                return Err("Pure Task operator reported a paid operation".into());
            }
            if let Err(error) = control::check(self.cancellation) {
                result.outputs = Err(error);
            }
            let produced = result.outputs.and_then(|values| {
                self.record_output(input_id, input, values.clone(), attempt.as_ref())
                    .map(|id| (id, values))
            });
            if let Some(attempt) = &attempt {
                let conclusion = match &produced {
                    Ok((output_id, _)) => TaskAttemptResultV1::Succeeded {
                        output_id: output_id.clone(),
                    },
                    Err(error) => {
                        let diagnostic_id = self.cas.put_json(&serde_json::json!({"schema":"af.task-diagnostic/1", "error":error})).map_err(|e| e.to_string())?;
                        TaskAttemptResultV1::Failed {
                            diagnostic_id,
                            feedback_id: result.feedback_id,
                        }
                    }
                };
                self.store
                    .lock()
                    .expect("Task Store")
                    .settle_task_attempt(
                        self.cas,
                        &self.lease,
                        TaskExecutionRecordV1::Settled {
                            attempt_id: attempt.id().into(),
                            charged_tokens: charged,
                            result: conclusion,
                            raw_artifact_ids: result.raw_artifact_ids,
                            usage_id: result.usage_id,
                        },
                        self.authority,
                    )
                    .map_err(|e| e.to_string())?;
            }
            match produced {
                Ok((id, values)) => {
                    self.pending_outputs.lock().expect("Task outputs").insert(
                        node.id.clone(),
                        (id, attempt.as_ref().map(|a| a.id().to_owned())),
                    );
                    return Ok(artifact_map(&values));
                }
                Err(error) => last_error = error,
            }
        }
        if allowance.is_some() {
            self.failures
                .lock()
                .expect("Task failures")
                .insert(node.id.clone(), NodeFailureClass::RunBudgetExhausted);
        }
        Err(last_error)
    }
}

impl Dispatch for TaskRuntime<'_, '_> {
    fn coordinates_owned_children(&self, node: &Node) -> bool {
        self.graph.owned_children.contains_key(&node.id)
    }

    fn expand_owned_children(
        &self,
        node: &Node,
        _inputs: &ArtifactMap,
    ) -> Result<Vec<review_graph::OwnedChildDispatch>, String> {
        self.expand_children(node)
    }

    fn complete_owned_children(
        &self,
        node: &Node,
        _inputs: &ArtifactMap,
        children: &[(String, review_graph::NodeOutcome)],
    ) -> Result<ArtifactMap, String> {
        self.complete_children(node, children)
    }

    fn requires_successful_predecessors(&self, node: &Node) -> bool {
        self.graph.requires_successful_predecessors(&node.id)
    }

    fn task_node_selected(&self, node: &Node, inputs: &ArtifactMap) -> Result<bool, String> {
        if self.resolve_node(&node.id)?.owned.is_some() {
            return Ok(true); // The captured Provider/condition barriers admit the owner.
        }
        self.graph.node_selected(&node.id, inputs, |id| {
            let value = envelope(self.cas, id)?;
            serde_json::from_value(
                value
                    .payload
                    .get("outcome")
                    .cloned()
                    .ok_or("Typed receipt has no outcome")?,
            )
            .map_err(|e| e.to_string())
        })
    }

    fn failure_class(&self, node_id: &str) -> Option<NodeFailureClass> {
        self.failures
            .lock()
            .expect("Task failures")
            .get(node_id)
            .copied()
    }

    fn record_invocation(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        let input = TaskInvocationV1 {
            plan_id: self.plan_id.clone(),
            node: node.id.clone(),
            inputs: self.typed_inputs(node, inputs)?,
        };
        let id = self.capture_invocation(&input)?;
        let state = self.projection()?.execution;
        let existing = state
            .as_ref()
            .and_then(|state| state.invocations.get(&node.id));
        if let Some((recorded, prior)) = existing {
            if recorded != &id || prior != &input {
                return Err("Invocation replay changed its exact inputs".into());
            }
            self.check_node_authority(&node.id, false)?;
        } else {
            control::check(self.cancellation)?;
            self.store
                .lock()
                .expect("Task Store")
                .record_task_invocation(self.cas, &self.lease, &id, self.authority)
                .map_err(|e| e.to_string())?;
        }
        let published = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.host.commit_domain_invocation(self.cas, &id, &input)
        }))
        .unwrap_or_else(|_| Err("Task domain invocation publication panicked".into()));
        self.record_domain_publication(&node.id, published)?;
        // The callback runs without the Store lock. A concurrent authority change must stop
        // this invocation even when it is an installed operation with no paid Attempt.
        let replayed = self.projection()?.execution.as_ref().is_some_and(|e| {
            e.outputs.contains_key(&node.id) || e.reusable_output(&node.id).is_some()
        });
        if !replayed {
            control::check(self.cancellation)?;
        }
        self.check_node_authority(
            &node.id,
            !replayed && !self.graph.owned_children.contains_key(&node.id),
        )?;
        if self.resolve_node(&node.id)?.allowance.is_some() && !replayed {
            let attempt = self.prepare(&input)?;
            self.prepared
                .lock()
                .expect("prepared Tasks")
                .insert(node.id.clone(), attempt);
        }
        Ok(())
    }

    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        self.execute_node(node, inputs)
    }

    fn record_outputs(&self, node: &Node, outputs: &ArtifactMap) -> Result<(), String> {
        if let Some((id, recorded)) = self
            .projection()?
            .execution
            .as_ref()
            .and_then(|e| e.outputs.get(&node.id))
        {
            return if artifact_map(&recorded.outputs) == *outputs {
                self.commit_domain_output(id)
            } else {
                Err("Replayed Task outputs changed".into())
            };
        }
        let (id, attempt) = self
            .pending_outputs
            .lock()
            .expect("Task outputs")
            .remove(&node.id)
            .ok_or("Task output has no durable settlement")?;
        if self.publish_owned_output(&node.id, &id, attempt.as_deref())? {
            return self.commit_domain_output(&id);
        }
        self.store
            .lock()
            .expect("Task Store")
            .publish_task_output(
                self.cas,
                &self.lease,
                &id,
                attempt.as_deref(),
                self.authority,
            )
            .map_err(|e| e.to_string())?;
        self.commit_domain_output(&id)
    }
}
