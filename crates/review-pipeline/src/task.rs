//! One Task dispatcher over the existing graph scheduler and common Store. Domain handlers
//! supply typed operations; they do not schedule children or create their own Attempt budgets.

pub mod code;
pub mod document;
pub mod host;
pub mod lease;
pub mod planning;
pub mod provider;
mod report;
pub mod review;
pub mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use review_core::task::execution::*;
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use review_core::{ArtifactEnvelope, Producer};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_graph::{ArtifactMap, Dispatch, Node, NodeFailureClass, RunReport};
use review_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use review_store::store::task::{TaskAuthority, TaskLease, TaskProjection, task_run_id};
use review_store::{Cas, EventStore, SharedEventStore, validate_envelope};

pub struct TaskWorkOutput {
    pub outputs: Result<BTreeMap<String, ArtifactInputV1>, String>,
    /// None means usage is unavailable, so the complete reservation remains charged.
    pub charged_tokens: Option<u64>,
    pub raw_artifact_ids: Vec<String>,
    pub usage_id: Option<String>,
    pub feedback_id: Option<String>,
}

pub trait TaskOperatorHost: Sync {
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
}

pub struct TaskRuntime<'a> {
    store: SharedEventStore<'a>,
    cas: &'a Cas,
    lease: TaskLease,
    plan_id: String,
    graph: CompiledTask,
    authority: &'a dyn TaskAuthority,
    host: &'a dyn TaskOperatorHost,
    prepared: Mutex<BTreeMap<String, PreparedTaskAttempt>>,
    pending_outputs: Mutex<BTreeMap<String, (String, Option<String>)>>,
    failures: Mutex<BTreeMap<String, NodeFailureClass>>,
    publication_failures: Mutex<BTreeSet<String>>,
}

fn envelope(cas: &Cas, id: &str) -> Result<ArtifactEnvelope, String> {
    let value: ArtifactEnvelope =
        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    validate_envelope(&value)?;
    if value.artifact_id != id {
        return Err("Task artifact identity differs from its reference".into());
    }
    Ok(value)
}

fn artifact_map(values: &BTreeMap<String, ArtifactInputV1>) -> ArtifactMap {
    values
        .iter()
        .map(|(name, value)| (name.clone(), value.artifact_ids.clone()))
        .collect()
}

impl<'a> TaskRuntime<'a> {
    pub fn new(
        store: &'a mut EventStore,
        cas: &'a Cas,
        lease: TaskLease,
        authority: &'a dyn TaskAuthority,
        host: &'a dyn TaskOperatorHost,
    ) -> Result<Self, String> {
        Self::with_store(SharedEventStore::new(store), cas, lease, authority, host)
    }

    /// Domain handlers may retain a clone for durable evidence and broker checks. Locks must
    /// be released before calling a handler or starting an external operation.
    pub fn with_store(
        store: SharedEventStore<'a>,
        cas: &'a Cas,
        lease: TaskLease,
        authority: &'a dyn TaskAuthority,
        host: &'a dyn TaskOperatorHost,
    ) -> Result<Self, String> {
        let (plan, projection) = {
            let locked = store.lock().expect("Task Store");
            let plan = locked
                .check_task_dispatch(cas, &lease, authority)
                .map_err(|e| e.to_string())?;
            let projection = locked
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Task")?;
            (plan, projection)
        };
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
            authority,
            host,
            prepared: Mutex::new(BTreeMap::new()),
            pending_outputs: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeMap::new()),
            publication_failures: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn execute(&self) -> Result<RunReport, String> {
        let report =
            lease::with_heartbeat(&self.store, self.cas, &self.lease, || self.graph.run(self))?;
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
        let mut failures = self
            .publication_failures
            .lock()
            .expect("Task publication failures");
        if result.is_err() {
            failures.insert(input.node);
        } else {
            failures.remove(&input.node);
        }
        result
    }

    fn typed_inputs(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let mut typed = BTreeMap::new();
        for port in &node.inputs {
            if !self.graph.nodes[&node.id]
                .contract
                .inputs
                .contains_key(&port.name)
            {
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

impl Dispatch for TaskRuntime<'_> {
    fn task_node_selected(&self, node: &Node, inputs: &ArtifactMap) -> Result<bool, String> {
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
        let refs: BTreeSet<_> = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .chain([self.plan_id.clone()])
            .collect();
        let (id, _) = self
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                Producer::KernelOperation {
                    run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                    node_id: Some(node.id.clone()),
                    operation_id: "task-node-invocation@1".into(),
                },
                refs.into_iter().collect(),
                None,
                serde_json::to_value(&input).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        self.store
            .lock()
            .expect("Task Store")
            .record_task_invocation(self.cas, &self.lease, &id, self.authority)
            .map_err(|e| e.to_string())?;
        let replayed = self.projection()?.execution.as_ref().is_some_and(|e| {
            e.outputs.contains_key(&node.id) || e.reusable_output(&node.id).is_some()
        });
        if self.graph.allowances.contains_key(&node.id) && !replayed {
            let attempt = self.prepare(&input)?;
            self.prepared
                .lock()
                .expect("prepared Tasks")
                .insert(node.id.clone(), attempt);
        }
        Ok(())
    }

    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
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
        let compiled = &self.graph.nodes[&node.id];
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
            let out = self.record_output(input_id, input, values.clone(), None)?;
            self.pending_outputs
                .lock()
                .expect("Task outputs")
                .insert(node.id.clone(), (out, None));
            return Ok(artifact_map(&values));
        }
        let allowance = self.graph.allowances.get(&node.id);
        let mut last_error = String::new();
        for index in 0..allowance.map_or(1, |a| a.max_attempts) {
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
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.host.execute(self.cas, input, attempt.as_ref())
            }))
            .unwrap_or_else(|_| TaskWorkOutput {
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
                let usage = result
                    .usage_id
                    .as_deref()
                    .and_then(|id| self.cas.get_json(id).ok())
                    .and_then(|value| {
                        serde_json::from_value::<review_runner::TokenUsage>(value).ok()
                    });
                let charge = result
                    .charged_tokens
                    .or_else(|| usage.as_ref().map(|u| u.chargeable_tokens));
                let wall = review_store::AttemptWall {
                    run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                    attempt_id: attempt.id().into(),
                    node_id: node.id.clone(),
                    round: 0,
                    epoch: u32::try_from(self.lease.epoch()).unwrap_or(u32::MAX),
                    started_unix_ms: started
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                    elapsed_ms: timer.elapsed().as_millis() as u64,
                    usage: charge.map(|chargeable_tokens| review_store::AttemptUsage {
                        input_tokens: usage.as_ref().and_then(|u| u.input_tokens),
                        output_tokens: usage.as_ref().and_then(|u| u.output_tokens),
                        cache_read_tokens: usage.as_ref().and_then(|u| u.cache_read_tokens),
                        cache_write_tokens: usage.as_ref().and_then(|u| u.cache_write_tokens),
                        reasoning_tokens: usage.as_ref().and_then(|u| u.reasoning_tokens),
                        chargeable_tokens,
                    }),
                };
                let _ = self
                    .store
                    .lock()
                    .expect("Task Store")
                    .record_attempt_wall(&wall);
            }
            let charged = result
                .charged_tokens
                .unwrap_or_else(|| attempt.as_ref().map_or(0, |a| a.reservation().tokens));
            if attempt.is_none() && charged != 0 {
                return Err("Pure Task operator reported a paid operation".into());
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
