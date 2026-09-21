//! Protected experimental-child preparation, approval and registration.

use super::*;
use review_core::task::optimization_experiment::{
    EXPERIMENT_PLAN_DECISION_V1, EXPERIMENT_PREPARED_V1, EXPERIMENT_SPECIFICATION_V1,
    EXPERIMENTAL_SLOT_V1, EXPERIMENTAL_SLOT_V2, ExperimentAllowanceV1, ExperimentIntervalKindV1,
    ExperimentMeasurementIntervalV1, ExperimentPlanDecisionV1, ExperimentPreparedV1,
    ExperimentSpecificationV1, ExperimentalSlotV1, ExperimentalSlotV2,
    validate_experiment_registration, validate_experiment_registration_v2,
};
use review_core::task::optimization_light::{
    OPTIMIZATION_EXECUTION_CONFIGURATION_V1, OptimizationExecutionConfigurationV1,
};
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1,
};
use review_graph::task::{
    EXPERIMENT_EXECUTION_PLAN_V1, ExperimentExecutionPlanV1, ExperimentPlannedChildV1,
};

#[derive(Debug, Clone)]
pub(in crate::store::task) struct RecordedExperiment {
    pub prepared: ExperimentPreparedV1,
    pub parent_node: String,
    pub decision: Option<(String, ExperimentPlanDecisionV1)>,
    pub child_plan: Option<ExperimentExecutionPlanV1>,
}

#[derive(Debug, Clone)]
pub struct RegisteredTaskExperiment {
    pub prepared_id: String,
    pub prepared: ExperimentPreparedV1,
    pub parent_node: String,
    pub child_plan: ExperimentExecutionPlanV1,
}

#[derive(Debug, Clone)]
pub struct ExperimentChildEvidence {
    pub closure: review_core::task::optimization_experiment::ExperimentChildClosureV1,
    pub attempts: Vec<TaskAttemptAccounting>,
    pub selected_output_id: Option<String>,
    pub published_output_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentMeasuredCosts {
    pub elapsed_ms: u64,
    pub preparation_ms: u64,
    pub cache_population_ms: u64,
    pub cache_lookup_ms: u64,
    pub cache_copy_ms: u64,
    pub missing_measurements: BTreeSet<String>,
    pub intervals: Vec<ExperimentMeasurementIntervalV1>,
}

fn interval_union(
    intervals: &[ExperimentMeasurementIntervalV1],
    kind: ExperimentIntervalKindV1,
) -> Result<u64, StoreError> {
    let mut spans = intervals
        .iter()
        .filter(|span| span.kind == kind)
        .map(|span| (span.start_ms, span.end_ms))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    let mut total = 0u64;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in spans {
        if start >= end {
            return Err(conflict(
                "Runtime measurement interval is empty or reversed",
            ));
        }
        match current {
            Some((old_start, old_end)) if start <= old_end => {
                current = Some((old_start, old_end.max(end)));
            }
            Some((old_start, old_end)) => {
                total = total
                    .checked_add(old_end - old_start)
                    .ok_or_else(|| conflict("Runtime measurement interval overflow"))?;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((start, end)) = current {
        total = total
            .checked_add(end - start)
            .ok_or_else(|| conflict("Runtime measurement interval overflow"))?;
    }
    Ok(total)
}

/// Reduce AF-owned timing/cache receipts for every retry of one registered child. Missing
/// instrumentation is explicit and therefore cannot be interpreted as zero overhead.
pub fn experiment_measured_costs(
    cas: &Cas,
    attempts: &[TaskAttemptAccounting],
) -> Result<ExperimentMeasuredCosts, StoreError> {
    let started = attempts
        .iter()
        .filter_map(|attempt| attempt.started_unix_ms)
        .min()
        .ok_or_else(|| conflict("Experiment Attempt has no measured start"))?;
    let settled = attempts
        .iter()
        .filter_map(|attempt| attempt.settled_unix_ms)
        .max()
        .ok_or_else(|| conflict("Experiment Attempt has no measured settlement"))?;
    let elapsed_ms = settled.saturating_sub(started).max(1);
    let mut intervals = vec![ExperimentMeasurementIntervalV1 {
        kind: ExperimentIntervalKindV1::Execution,
        start_ms: 0,
        end_ms: elapsed_ms,
    }];
    let mut saw_preparation = false;
    let mut saw_cache = false;
    let mut cache_population_cursor = 0u64;
    let mut cache_lookup_cursor = 0u64;
    let mut toolchain_unknown = false;
    for attempt in attempts {
        for id in &attempt.raw_artifact_ids {
            let Some(envelope) = cas
                .get_optional_artifact(id)
                .map_err(|error| StoreError::Artifact(error.to_string()))?
            else {
                continue;
            };
            if envelope.artifact_type != TASK_RUNTIME_EVIDENCE_V1 {
                continue;
            }
            let evidence: TaskRuntimeEvidenceV1 = serde_json::from_value(envelope.payload)?;
            evidence.validate().map_err(conflict)?;
            if evidence.attempt_id != attempt.attempt_id
                || evidence.node != attempt.reservation.node
                || !matches!(envelope.producer, review_core::Producer::Attempt { ref node_id, ref attempt_id, .. }
                    if node_id == &attempt.reservation.node && attempt_id == &attempt.attempt_id)
            {
                return Err(conflict(
                    "Experiment runtime evidence differs from its settled Attempt",
                ));
            }
            for span in evidence.spans {
                if span.kind == TaskRuntimeSpanKindV1::DependencyPreparation {
                    saw_preparation = true;
                    let start = span.started_unix_ms.saturating_sub(started);
                    let end = start
                        .checked_add(span.elapsed_ms)
                        .ok_or_else(|| conflict("Preparation timing overflow"))?;
                    if end > start {
                        intervals.push(ExperimentMeasurementIntervalV1 {
                            kind: ExperimentIntervalKindV1::Preparation,
                            start_ms: start,
                            end_ms: end,
                        });
                    }
                }
            }
            for cache in evidence.caches {
                // Every cache observation is dependency preparation that populates the cache.
                saw_cache = true;
                saw_preparation = true;
                toolchain_unknown |= cache.toolchain_id.is_none();
                if cache.lookup_ms > 0 {
                    let end = cache_lookup_cursor
                        .checked_add(cache.lookup_ms)
                        .ok_or_else(|| conflict("Cache lookup timing overflow"))?;
                    intervals.push(ExperimentMeasurementIntervalV1 {
                        kind: ExperimentIntervalKindV1::CacheLookup,
                        start_ms: cache_lookup_cursor,
                        end_ms: end,
                    });
                    cache_lookup_cursor = end;
                }
                if cache.materialization_ms > 0 {
                    let end = cache_population_cursor
                        .checked_add(cache.materialization_ms)
                        .ok_or_else(|| conflict("Cache materialization timing overflow"))?;
                    intervals.push(ExperimentMeasurementIntervalV1 {
                        kind: ExperimentIntervalKindV1::CachePopulation,
                        start_ms: cache_population_cursor,
                        end_ms: end,
                    });
                    cache_population_cursor = end;
                }
            }
        }
    }
    let mut missing_measurements = BTreeSet::new();
    if !saw_preparation {
        missing_measurements.insert("preparation".into());
    }
    if !saw_cache {
        missing_measurements.extend(
            ["cache_population", "cache_lookup", "cache_copy"]
                .into_iter()
                .map(str::to_owned),
        );
    }
    if saw_cache && toolchain_unknown {
        missing_measurements.insert("cache_toolchain_identity".into());
    }
    Ok(ExperimentMeasuredCosts {
        elapsed_ms,
        preparation_ms: interval_union(&intervals, ExperimentIntervalKindV1::Preparation)?,
        cache_population_ms: interval_union(&intervals, ExperimentIntervalKindV1::CachePopulation)?,
        cache_lookup_ms: interval_union(&intervals, ExperimentIntervalKindV1::CacheLookup)?,
        cache_copy_ms: interval_union(&intervals, ExperimentIntervalKindV1::CacheCopy)?,
        missing_measurements,
        intervals,
    })
}

impl RecordedExperiment {
    pub(super) fn registered_child(&self, node: &str) -> Option<&ExperimentPlannedChildV1> {
        self.child_plan.as_ref()?.children.get(node)
    }
}

fn typed<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, kind: &str) -> Result<T, StoreError> {
    payload(cas, id, kind)
}

fn child_plan(cas: &Cas, id: &str) -> Result<ExperimentExecutionPlanV1, StoreError> {
    typed(cas, id, EXPERIMENT_EXECUTION_PLAN_V1)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentalWorkerPackage {
    schema: String,
    name: String,
    version: String,
    digest: String,
    files: BTreeMap<String, Vec<u8>>,
}

fn worker_package_digest(files: &BTreeMap<String, Vec<u8>>) -> Result<String, StoreError> {
    let listed: BTreeMap<&str, String> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), crate::canonical::blob_content_id(bytes)))
        .collect();
    crate::canonical::content_id(&serde_json::json!({"kind":"reviewer-package@1","files":listed}))
        .map_err(|error| conflict(format!("Worker package digest: {error}")))
}

fn validate_derived_worker_package(
    cas: &Cas,
    child: &ExperimentPlannedChildV1,
    closure: &review_core::task::optimization_experiment::ExperimentChildClosureV1,
    original_id: &str,
) -> Result<(), StoreError> {
    let derived_envelope = cas
        .get_artifact(&closure.worker_package_id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    let original_envelope = cas
        .get_artifact(original_id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    if derived_envelope.artifact_type != "af/TaskPackage@1"
        || original_envelope.artifact_type != "af/TaskPackage@1"
    {
        return Err(conflict(
            "Experimental Worker derivation is not a TaskPackage",
        ));
    }
    let derived: ExperimentalWorkerPackage = serde_json::from_value(derived_envelope.payload)
        .map_err(|error| conflict(format!("Derived Worker package: {error}")))?;
    let original: ExperimentalWorkerPackage = serde_json::from_value(original_envelope.payload)
        .map_err(|error| conflict(format!("Original Worker package: {error}")))?;
    let configuration_ids = derived_envelope
        .input_artifacts
        .iter()
        .filter_map(|id| {
            cas.get_artifact(id)
                .ok()
                .filter(|value| value.artifact_type == OPTIMIZATION_EXECUTION_CONFIGURATION_V1)
                .map(|_| id.clone())
        })
        .collect::<Vec<_>>();
    let [configuration_id] = configuration_ids.as_slice() else {
        return Err(conflict(
            "Derived Worker package needs one exact execution derivation",
        ));
    };
    let execution: OptimizationExecutionConfigurationV1 = payload(
        cas,
        configuration_id,
        OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
    )?;
    execution.validate().map_err(conflict)?;
    let task_configuration_id = child
        .invocation
        .inputs
        .get("configuration")
        .and_then(|port| port.artifact_ids.first())
        .ok_or_else(|| conflict("Derived Worker invocation lacks protected configuration"))?;
    let task_configuration = cas
        .get_artifact(task_configuration_id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    let field = |name: &str| {
        task_configuration
            .payload
            .get(name)
            .and_then(serde_json::Value::as_str)
    };
    let mut original_without_instructions = original.files.clone();
    let mut derived_without_instructions = derived.files.clone();
    let original_instructions = original_without_instructions.remove("instructions.md");
    let derived_instructions = derived_without_instructions.remove("instructions.md");
    if derived.schema != "af.task-package/1"
        || original.schema != "af.task-package/1"
        || derived.name != original.name
        || derived.name != closure.package
        || derived.version != original.version
        || worker_package_digest(&original.files)? != original.digest
        || worker_package_digest(&derived.files)? != derived.digest
        || original_without_instructions != derived_without_instructions
        || original_instructions.as_deref() == derived_instructions.as_deref()
        || derived_instructions.as_deref() != Some(execution.instructions.as_bytes())
        || crate::canonical::blob_content_id(execution.instructions.as_bytes())
            != execution.instructions_id
        || execution.original_package_id != original_id
        || execution.original_package_digest != original.digest
        || execution.package_digest != derived.digest
        || execution.package != closure.package
        || !derived_envelope
            .input_artifacts
            .contains(&original_id.to_owned())
        || !derived_envelope
            .input_artifacts
            .contains(&execution.instructions_id)
        || field("candidate_execution_configuration_id") != Some(configuration_id.as_str())
        || field("source_snapshot_id") != Some(execution.source_snapshot_id.as_str())
        || field("candidate_snapshot_id") != Some(execution.candidate_snapshot_id.as_str())
        || field("repin_id") != Some(execution.repin_id.as_str())
    {
        return Err(conflict(
            "Experimental Worker is not the approved instruction-only package derivation",
        ));
    }
    Ok(())
}

fn validate_child_plan(
    cas: &Cas,
    outer_plan_id: &str,
    prepared: &ExperimentPreparedV1,
    value: &ExperimentExecutionPlanV1,
) -> Result<(), StoreError> {
    let outer: ExecutionPlanV1 = payload(cas, outer_plan_id, task::EXECUTION_PLAN_V1)?;
    let graph: CompiledTask = payload(cas, &outer.compiled_graph_id, "af/CompiledTask@1")?;
    if value.schema != "af.experiment-execution-plan/1"
        || value.children.len() != prepared.children.len()
    {
        return Err(conflict(
            "Experimental execution plan changed its prepared closure",
        ));
    }
    for closure in &prepared.children {
        let child = value
            .children
            .get(&closure.node)
            .ok_or_else(|| conflict("Experimental plan omits a prepared child"))?;
        child.invocation.validate().map_err(conflict)?;
        let (slot_name, trusted_verifier) = match &child.definition.operator {
            CompiledOperator::Primitive {
                operator: task::pipeline::TaskOperatorV1::Worker { slot },
                ..
            } => (slot, false),
            CompiledOperator::Primitive {
                operator:
                    task::pipeline::TaskOperatorV1::Verify { slot }
                    | task::pipeline::TaskOperatorV1::FixVerify { slot },
                ..
            } => (slot, true),
            _ => {
                return Err(conflict(
                    "Experimental child is not an executable Worker operation",
                ));
            }
        };
        let binding = outer
            .bindings
            .get(slot_name)
            .ok_or_else(|| conflict("Experimental child has no exact outer Worker binding"))?;
        let effort = match &binding.execution {
            task::plan::WorkerExecutionV1::Command {} => "command",
            task::plan::WorkerExecutionV1::Model { effort, .. } => effort.as_str(),
        };
        if child.invocation.plan_id != outer_plan_id || child.invocation.node != closure.node {
            return Err(conflict(
                "Experimental child changed its outer plan or node",
            ));
        }
        let package_matches = binding.package_artifact_id == closure.worker_package_id;
        let derived_signature = match &child.definition.operator {
            CompiledOperator::Primitive { signature, .. } => {
                signature == &format!("worker-derived/{}", closure.worker_package_id)
            }
            _ => false,
        };
        let derived_matches = closure.arm
            == review_core::task::optimization_experiment::ExperimentArmV1::Candidate
            && derived_signature
            && validate_derived_worker_package(cas, child, closure, &binding.package_artifact_id)
                .is_ok();
        if graph.slots.get(slot_name).map(|slot| slot.worker.as_str())
            != Some(closure.package.as_str())
            || (!package_matches && !derived_matches)
            || (package_matches && derived_signature)
            || effort != closure.effort
        {
            return Err(conflict(
                "Experimental child changed its package, binding or effort",
            ));
        }
        if child.allowance.tokens_per_attempt != closure.allowance.tokens
            || child.allowance.max_attempts != closure.allowance.attempts
            || child.allowance.wall_ms_per_attempt != closure.allowance.wall_ms
            || child.allowance.verification_attempts > child.allowance.max_attempts
            || (child.allowance.verification_attempts > 0 && !trusted_verifier)
        {
            return Err(conflict(
                "Experimental child changed its registered sub-allocation",
            ));
        }
        if invocation(cas, &closure.invocation_id)? != child.invocation {
            return Err(conflict(
                "Experimental child changed its captured invocation",
            ));
        }
    }
    Ok(())
}

impl TaskExecutionProjection {
    pub(super) fn apply_experiment(
        &mut self,
        cas: &Cas,
        task: &TaskRevisionV1,
        revision_id: &str,
        outer_plan_id: &str,
        record: &TaskExecutionRecordV1,
        now: u64,
    ) -> Result<(), StoreError> {
        match record {
            TaskExecutionRecordV1::ExperimentPrepared { prepared_id } => {
                if !self.experiments.is_empty() {
                    return Err(conflict("An experimental closure is already prepared"));
                }
                let prepared: ExperimentPreparedV1 =
                    typed(cas, prepared_id, EXPERIMENT_PREPARED_V1)?;
                prepared.validate().map_err(conflict)?;
                let plan = child_plan(cas, &prepared.compiled_child_plan_id)?;
                validate_child_plan(cas, outer_plan_id, &prepared, &plan)?;
                let slot = self
                    .graph
                    .experimental_slots
                    .get(&plan.parent_node)
                    .ok_or_else(|| conflict("Prepared experiment has no captured outer slot"))?;
                if prepared.task_revision_id != revision_id
                    || prepared.outer_plan_id != outer_plan_id
                    || prepared.slot_id != slot.slot_id
                {
                    return Err(conflict(
                        "Prepared experiment belongs to another Task or slot",
                    ));
                }
                self.experiments.insert(
                    prepared_id.clone(),
                    RecordedExperiment {
                        prepared,
                        parent_node: plan.parent_node,
                        decision: None,
                        child_plan: None,
                    },
                );
            }
            TaskExecutionRecordV1::ExperimentPlanDecided {
                prepared_id,
                decision_id,
            } => {
                let recorded = self
                    .experiments
                    .get_mut(prepared_id)
                    .ok_or_else(|| conflict("Experiment decision has no prepared closure"))?;
                if recorded.decision.is_some() {
                    return Err(conflict("Experiment already has a decision"));
                }
                let decision: ExperimentPlanDecisionV1 =
                    typed(cas, decision_id, EXPERIMENT_PLAN_DECISION_V1)?;
                decision.validate().map_err(conflict)?;
                if decision.prepared_id.as_str() != prepared_id
                    || decision.task_revision_id != recorded.prepared.task_revision_id
                    || decision.outer_plan_id != recorded.prepared.outer_plan_id
                    || decision.slot_id != recorded.prepared.slot_id
                    || decision.specification_id != recorded.prepared.specification_id
                    || decision.compiled_child_plan_id != recorded.prepared.compiled_child_plan_id
                    || decision.policy_id != recorded.prepared.policy_id
                    || now >= decision.expires_unix_ms
                {
                    return Err(conflict(
                        "Experiment decision is expired, stale or belongs to another closure",
                    ));
                }
                recorded.decision = Some((decision_id.clone(), decision));
            }
            TaskExecutionRecordV1::ExperimentChildrenRegistered {
                prepared_id,
                decision_id,
                child_plan_id,
            } => {
                let recorded = self
                    .experiments
                    .get_mut(prepared_id)
                    .ok_or_else(|| conflict("Experiment registration has no preparation"))?;
                if recorded.child_plan.is_some()
                    || recorded.decision.as_ref().map(|(id, _)| id) != Some(decision_id)
                    || &recorded.prepared.compiled_child_plan_id != child_plan_id
                {
                    return Err(conflict(
                        "Experiment registration changed its approved closure",
                    ));
                }
                let decision = &recorded.decision.as_ref().expect("checked").1;
                let slot_envelope = cas
                    .get_artifact(&recorded.prepared.slot_id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?;
                let specification: ExperimentSpecificationV1 = typed(
                    cas,
                    &recorded.prepared.specification_id,
                    EXPERIMENT_SPECIFICATION_V1,
                )?;
                let remaining = self.budget.remaining_limits();
                let remaining = ExperimentAllowanceV1 {
                    tokens: remaining.tokens,
                    attempts: remaining.max_attempts,
                    wall_ms: task.limits.deadline_unix_ms.saturating_sub(now),
                };
                let max_children = match slot_envelope.artifact_type.as_str() {
                    EXPERIMENTAL_SLOT_V1 => {
                        let slot: ExperimentalSlotV1 =
                            serde_json::from_value(slot_envelope.payload)?;
                        validate_experiment_registration(
                            &recorded.prepared.slot_id,
                            &slot,
                            &recorded.prepared.specification_id,
                            &specification,
                            prepared_id,
                            &recorded.prepared,
                            decision,
                            now,
                            &remaining,
                        )
                        .map_err(conflict)?;
                        slot.max_children
                    }
                    EXPERIMENTAL_SLOT_V2 => {
                        let slot: ExperimentalSlotV2 =
                            serde_json::from_value(slot_envelope.payload)?;
                        validate_experiment_registration_v2(
                            &recorded.prepared.slot_id,
                            &slot,
                            &recorded.prepared.specification_id,
                            &specification,
                            prepared_id,
                            &recorded.prepared,
                            decision,
                            now,
                            &remaining,
                        )
                        .map_err(conflict)?;
                        slot.max_children
                    }
                    _ => return Err(conflict("Unsupported experimental slot generation")),
                };
                let plan = child_plan(cas, child_plan_id)?;
                validate_child_plan(cas, outer_plan_id, &recorded.prepared, &plan)?;
                self.budget
                    .register_experimental_children(
                        &recorded.parent_node,
                        &plan
                            .children
                            .iter()
                            .map(|(name, child)| (name.clone(), child.allowance.clone()))
                            .collect(),
                        max_children,
                    )
                    .map_err(conflict)?;
                recorded.child_plan = Some(plan);
            }
            _ => return Err(conflict("Expected experimental execution record")),
        }
        Ok(())
    }
}

pub(super) fn is_experiment_record(record: &TaskExecutionRecordV1) -> bool {
    matches!(
        record,
        TaskExecutionRecordV1::ExperimentPrepared { .. }
            | TaskExecutionRecordV1::ExperimentPlanDecided { .. }
            | TaskExecutionRecordV1::ExperimentChildrenRegistered { .. }
    )
}

impl EventStore {
    pub fn pending_task_experiment(
        &self,
        cas: &Cas,
        task_id: &str,
    ) -> Result<Option<(String, ExperimentPreparedV1)>, StoreError> {
        let Some(state) = self.task_projection(cas, task_id)? else {
            return Ok(None);
        };
        Ok(state.execution.and_then(|execution| {
            execution.experiments.into_iter().find_map(|(id, value)| {
                (value.child_plan.is_none() && value.decision.is_none())
                    .then_some((id, value.prepared))
            })
        }))
    }

    pub fn task_experiment(
        &self,
        cas: &Cas,
        task_id: &str,
        parent_node: &str,
    ) -> Result<Option<RegisteredTaskExperiment>, StoreError> {
        let Some(state) = self.task_projection(cas, task_id)? else {
            return Ok(None);
        };
        Ok(state.execution.and_then(|execution| {
            execution
                .experiments
                .into_iter()
                .find_map(|(prepared_id, value)| {
                    if value.parent_node != parent_node {
                        return None;
                    }
                    let child_plan = value.child_plan?;
                    Some(RegisteredTaskExperiment {
                        prepared_id,
                        prepared: value.prepared,
                        parent_node: value.parent_node,
                        child_plan,
                    })
                })
        }))
    }

    pub fn task_experiment_evidence(
        &self,
        cas: &Cas,
        task_id: &str,
        prepared_id: &str,
    ) -> Result<Vec<ExperimentChildEvidence>, StoreError> {
        let state = self
            .task_projection(cas, task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        let experiment = execution
            .experiments
            .get(prepared_id)
            .ok_or_else(|| conflict("Unknown experiment"))?;
        if experiment.child_plan.is_none() {
            return Err(conflict("Experiment is not registered"));
        }
        let accounting = execution.attempt_accounting();
        experiment
            .prepared
            .children
            .iter()
            .map(|closure| {
                let selected = execution.reusable_output(&closure.node).map(|(id, _)| id);
                Ok(ExperimentChildEvidence {
                    closure: closure.clone(),
                    attempts: accounting
                        .iter()
                        // Runtime invocation envelopes carry the common Task producer and may
                        // therefore have a different artifact ID from the preparation copy.
                        // The registered child node is the exact durable scheduling identity;
                        // validate_child_plan already proved its invocation bytes.
                        .filter(|a| a.reservation.node == closure.node)
                        .cloned()
                        .collect(),
                    published_output_id: execution
                        .outputs
                        .get(&closure.node)
                        .map(|(id, _)| id.clone()),
                    selected_output_id: selected,
                })
            })
            .collect()
    }

    pub fn prepare_task_experiment(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        prepared_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        let prepared: ExperimentPreparedV1 = typed(cas, prepared_id, EXPERIMENT_PREPARED_V1)?;
        prepared.validate().map_err(conflict)?;
        if prepared.writer_epoch != lease.epoch
            || prepared.task_revision_id != state.revision_id
            || state.plan_id.as_deref() != Some(prepared.outer_plan_id.as_str())
        {
            return Err(conflict(
                "Experimental preparation changed writer or outer authority",
            ));
        }
        authority
            .validate_experiment_preparation(cas, &state.revision, &plan, &prepared)
            .map_err(conflict)?;
        let fresh = self.checked_task_recording(cas, lease, authority)?.0;
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict("Task changed during experiment preparation"));
        }
        self.task_execution_record_from_state(
            cas,
            lease,
            TaskExecutionRecordV1::ExperimentPrepared {
                prepared_id: prepared_id.into(),
            },
            now()?,
            fresh,
            None,
        )?;
        Ok(())
    }

    pub fn decide_task_experiment(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        prepared_id: &str,
        decision_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        if state.phase
            != (TaskPhaseV1::Waiting {
                reason: TaskWaitingReasonV1::NeedsPlanReview,
            })
        {
            return Err(conflict("Task is not waiting for experimental plan review"));
        }
        let decision: ExperimentPlanDecisionV1 =
            typed(cas, decision_id, EXPERIMENT_PLAN_DECISION_V1)?;
        authority
            .experiment_authorization_current(&decision)
            .map_err(conflict)?;
        if let Some((recorded_id, _)) = state
            .execution
            .as_ref()
            .and_then(|execution| execution.experiments.get(prepared_id))
            .and_then(|experiment| experiment.decision.as_ref())
        {
            return if recorded_id == decision_id {
                Ok(())
            } else {
                Err(conflict("Experiment already has another signed decision"))
            };
        }
        self.task_execution_record_from_state(
            cas,
            lease,
            TaskExecutionRecordV1::ExperimentPlanDecided {
                prepared_id: prepared_id.into(),
                decision_id: decision_id.into(),
            },
            time,
            state,
            None,
        )?;
        Ok(())
    }

    pub fn register_task_experiment(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        prepared_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        let experiment = state
            .execution
            .as_ref()
            .and_then(|e| e.experiments.get(prepared_id))
            .ok_or_else(|| conflict("Unknown prepared experiment"))?;
        let (decision_id, decision) = experiment
            .decision
            .as_ref()
            .ok_or_else(|| conflict("Experiment has no signed decision"))?;
        authority
            .experiment_authorization_current(decision)
            .map_err(conflict)?;
        self.current_task_plan(cas, &state, authority, time)?;
        self.task_execution_record_from_state(
            cas,
            lease,
            TaskExecutionRecordV1::ExperimentChildrenRegistered {
                prepared_id: prepared_id.into(),
                decision_id: decision_id.clone(),
                child_plan_id: experiment.prepared.compiled_child_plan_id.clone(),
            },
            time,
            state,
            None,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod derived_package_tests {
    use super::*;
    use review_attempt::task_budget::NodeAllowance;
    use review_core::PortCardinality;
    use review_core::task::ArtifactInputV1;
    use review_core::task::optimization_experiment::ExperimentChildClosureV1;
    use review_core::task::pipeline::{PipelineContractV1, TaskOperatorV1, WorkerSlotV1};
    use review_core::task::plan::{EffectiveWorkerBindingV1, WorkerExecutionV1};
    use review_graph::task::{CompiledNode, CompiledOperator, CompiledTask};

    fn producer() -> review_core::Producer {
        review_core::Producer::KernelOperation {
            run_id: "derived-package-store-test".into(),
            node_id: None,
            operation_id: "fixture@1".into(),
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        cas: Cas,
        outer_plan_id: String,
        prepared: ExperimentPreparedV1,
        plan: ExperimentExecutionPlanV1,
        original_id: String,
        execution_id: String,
        instructions_id: String,
        task_configuration_id: String,
    }

    impl Fixture {
        fn package(
            cas: &Cas,
            files: BTreeMap<String, Vec<u8>>,
            refs: Vec<String>,
        ) -> (String, String) {
            let digest = worker_package_digest(&files).unwrap();
            let id = cas
                .put_artifact(
                    "af/TaskPackage@1",
                    producer(),
                    refs,
                    None,
                    serde_json::json!({
                        "schema":"af.task-package/1",
                        "name":"project/worker",
                        "version":"1.0.0",
                        "digest":digest,
                        "files":files,
                    }),
                )
                .unwrap()
                .0;
            (id, digest)
        }

        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let cas = Cas::open(directory.path().join("cas")).unwrap();
            let digest = |label: &str| cas.put_json(&serde_json::json!({"id":label})).unwrap();
            let source = digest("source");
            let candidate = digest("candidate");
            let repin = digest("repin");
            let instructions = "Use the approved candidate instructions.";
            let instructions_id = crate::canonical::blob_content_id(instructions.as_bytes());
            cas.put(instructions.as_bytes()).unwrap();
            let original_files = BTreeMap::from([
                (
                    "instructions.md".into(),
                    b"Use baseline instructions.".to_vec(),
                ),
                (
                    "worker.toml".into(),
                    b"runner = 'model'\neffects = []\n".to_vec(),
                ),
                ("input.schema.json".into(), b"{}".to_vec()),
                ("outputs/result.schema.json".into(), b"{}".to_vec()),
            ]);
            let (original_id, original_digest) =
                Self::package(&cas, original_files.clone(), vec![]);
            let mut derived_files = original_files;
            derived_files.insert("instructions.md".into(), instructions.as_bytes().to_vec());
            let derived_digest = worker_package_digest(&derived_files).unwrap();
            let execution = OptimizationExecutionConfigurationV1 {
                schema: "af.optimization-execution-configuration/1".into(),
                recipe_id: "context_retrieval_dedup".into(),
                original_package_id: original_id.clone(),
                original_package_digest: original_digest.clone(),
                source_snapshot_id: source.clone(),
                candidate_snapshot_id: candidate.clone(),
                repin_id: repin.clone(),
                package: "project/worker".into(),
                package_digest: derived_digest.clone(),
                instructions_id: instructions_id.clone(),
                instructions: instructions.into(),
            };
            let execution_id = cas
                .put_artifact(
                    OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
                    producer(),
                    vec![
                        original_id.clone(),
                        source.clone(),
                        candidate.clone(),
                        repin.clone(),
                        instructions_id.clone(),
                    ],
                    None,
                    serde_json::to_value(&execution).unwrap(),
                )
                .unwrap()
                .0;
            let (derived_id, _) = Self::package(
                &cas,
                derived_files,
                vec![
                    original_id.clone(),
                    execution_id.clone(),
                    instructions_id.clone(),
                ],
            );
            let task_configuration_id = cas
                .put_artifact(
                    "af/OptimizationConfiguration@1",
                    producer(),
                    vec![execution_id.clone()],
                    None,
                    serde_json::json!({
                        "candidate_execution_configuration_id":execution_id,
                        "source_snapshot_id":source,
                        "candidate_snapshot_id":candidate,
                        "repin_id":repin,
                    }),
                )
                .unwrap()
                .0;
            let contract = PipelineContractV1 {
                inputs: BTreeMap::new(),
                outputs: BTreeMap::new(),
            };
            let slot = "candidate";
            let definition = CompiledNode {
                operator: CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Worker { slot: slot.into() },
                    signature: format!("worker-derived/{derived_id}"),
                },
                contract: contract.clone(),
                inputs: BTreeMap::new(),
                conditions: vec![],
            };
            let graph = CompiledTask {
                schema: "af.compiled-task/1".into(),
                nodes: BTreeMap::new(),
                order: vec![],
                inputs: BTreeMap::new(),
                outputs: BTreeMap::new(),
                coverage: BTreeMap::new(),
                calls: BTreeMap::new(),
                slots: BTreeMap::from([(
                    slot.into(),
                    WorkerSlotV1 {
                        worker: "project/worker".into(),
                        role: "implementer".into(),
                        input_type: "af/Requirements@1".into(),
                        output_type: "af/Result@1".into(),
                        min_attempts: 1,
                        max_attempts: 1,
                        allow_local_replacement: false,
                        independent_from: BTreeSet::new(),
                    },
                )]),
                replaced_workers: BTreeMap::new(),
                max_parallel: 1,
                allowances: BTreeMap::new(),
                owned_children: BTreeMap::new(),
                experimental_slots: BTreeMap::new(),
                review_integration: None,
                token_scopes: BTreeMap::new(),
            };
            let compiled_graph_id = cas
                .put_artifact(
                    "af/CompiledTask@1",
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(graph).unwrap(),
                )
                .unwrap()
                .0;
            let mut outer: review_core::task::plan::ExecutionPlanV1 = serde_json::from_slice(
                &std::fs::read(
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../fixtures/task-contracts/v1/execution-plan.json"),
                )
                .unwrap(),
            )
            .unwrap();
            outer.compiled_graph_id = compiled_graph_id;
            outer.bindings = BTreeMap::from([(
                slot.into(),
                EffectiveWorkerBindingV1 {
                    package_digest: original_digest.clone(),
                    package_artifact_id: original_id.clone(),
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: digest("invocation-policy"),
                },
            )]);
            let outer_plan_id = cas
                .put_artifact(
                    task::EXECUTION_PLAN_V1,
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(outer).unwrap(),
                )
                .unwrap()
                .0;
            let node = "root.experiment.case0_rep1_candidate";
            let invocation = TaskInvocationV1 {
                plan_id: outer_plan_id.clone(),
                node: node.into(),
                inputs: BTreeMap::from([(
                    "configuration".into(),
                    ArtifactInputV1 {
                        artifact_ids: vec![task_configuration_id.clone()],
                        artifact_type: "af/OptimizationConfiguration@1".into(),
                        cardinality: PortCardinality::One,
                        snapshot_id: None,
                    },
                )]),
            };
            let invocation_id = cas
                .put_artifact(
                    task::execution::TASK_INVOCATION_V1,
                    producer(),
                    vec![outer_plan_id.clone(), task_configuration_id.clone()],
                    None,
                    serde_json::to_value(&invocation).unwrap(),
                )
                .unwrap()
                .0;
            let allowance = NodeAllowance {
                tokens_per_attempt: 100,
                wall_ms_per_attempt: 1000,
                max_attempts: 1,
                verification_attempts: 0,
            };
            let closure = ExperimentChildClosureV1 {
                node: node.into(),
                arm: review_core::task::optimization_experiment::ExperimentArmV1::Candidate,
                case_id: digest("case"),
                repetition: 1,
                task_kind: "implement".into(),
                package: "project/worker".into(),
                worker_package_id: derived_id,
                effort: "command".into(),
                effects: BTreeSet::new(),
                source_snapshot_id: digest("closure-source"),
                requirements_id: digest("requirements"),
                authority_id: digest("authority"),
                invocation_id,
                allowance: ExperimentAllowanceV1 {
                    tokens: 100,
                    attempts: 1,
                    wall_ms: 1000,
                },
            };
            let plan = ExperimentExecutionPlanV1 {
                schema: "af.experiment-execution-plan/1".into(),
                parent_node: "root.experiment".into(),
                children: BTreeMap::from([(
                    node.into(),
                    ExperimentPlannedChildV1 {
                        definition,
                        invocation,
                        allowance,
                    },
                )]),
            };
            let prepared = ExperimentPreparedV1 {
                schema: "af.experiment-prepared/1".into(),
                task_revision_id: digest("revision"),
                outer_plan_id: outer_plan_id.clone(),
                slot_id: digest("slot"),
                specification_id: digest("specification"),
                compiled_child_plan_id: digest("child-plan"),
                policy_id: digest("policy"),
                spent_accounting_prefix_id: digest("accounting"),
                writer_epoch: 1,
                children: vec![closure],
            };
            Self {
                _directory: directory,
                cas,
                outer_plan_id,
                prepared,
                plan,
                original_id,
                execution_id,
                instructions_id,
                task_configuration_id,
            }
        }

        fn validate(&self) -> Result<(), StoreError> {
            validate_child_plan(&self.cas, &self.outer_plan_id, &self.prepared, &self.plan)
        }
    }

    #[test]
    fn child_plan_accepts_only_the_exact_instruction_derivation() {
        let fixture = Fixture::new();
        fixture.validate().unwrap();

        let mut normal = fixture.plan.clone();
        let child = normal.children.values_mut().next().unwrap();
        let CompiledOperator::Primitive { signature, .. } = &mut child.definition.operator else {
            unreachable!()
        };
        *signature = format!("worker/{}", fixture.original_id);
        let mut prepared = fixture.prepared.clone();
        prepared.children[0].worker_package_id = fixture.original_id.clone();
        validate_child_plan(&fixture.cas, &fixture.outer_plan_id, &prepared, &normal).unwrap();
        let CompiledOperator::Primitive { signature, .. } = &mut normal
            .children
            .values_mut()
            .next()
            .unwrap()
            .definition
            .operator
        else {
            unreachable!()
        };
        *signature = format!("worker-derived/{}", fixture.original_id);
        assert!(
            validate_child_plan(&fixture.cas, &fixture.outer_plan_id, &prepared, &normal).is_err()
        );
    }

    #[test]
    fn child_plan_rejects_stale_linkage_and_every_non_instruction_change() {
        for field in [
            "candidate_execution_configuration_id",
            "source_snapshot_id",
            "candidate_snapshot_id",
            "repin_id",
        ] {
            let mut fixture = Fixture::new();
            let mut payload = fixture
                .cas
                .get_artifact(&fixture.task_configuration_id)
                .unwrap()
                .payload;
            payload[field] = serde_json::json!(
                fixture
                    .cas
                    .put_json(&serde_json::json!({"stale":field}))
                    .unwrap()
            );
            let changed = fixture
                .cas
                .put_artifact(
                    "af/OptimizationConfiguration@1",
                    producer(),
                    vec![fixture.execution_id.clone()],
                    None,
                    payload,
                )
                .unwrap()
                .0;
            fixture
                .plan
                .children
                .values_mut()
                .next()
                .unwrap()
                .invocation
                .inputs
                .get_mut("configuration")
                .unwrap()
                .artifact_ids = vec![changed];
            assert!(fixture.validate().is_err(), "accepted stale {field}");
        }

        for path in [
            "worker.toml",
            "input.schema.json",
            "outputs/result.schema.json",
            "model.json",
            "effects.json",
        ] {
            let mut fixture = Fixture::new();
            let original: ExperimentalWorkerPackage = serde_json::from_value(
                fixture
                    .cas
                    .get_artifact(&fixture.original_id)
                    .unwrap()
                    .payload,
            )
            .unwrap();
            let execution: OptimizationExecutionConfigurationV1 = payload(
                &fixture.cas,
                &fixture.execution_id,
                OPTIMIZATION_EXECUTION_CONFIGURATION_V1,
            )
            .unwrap();
            let mut files = original.files;
            files.insert(
                "instructions.md".into(),
                execution.instructions.into_bytes(),
            );
            files.insert(path.into(), b"changed authority".to_vec());
            let (changed_id, changed_digest) = Fixture::package(
                &fixture.cas,
                files,
                vec![
                    fixture.original_id.clone(),
                    fixture.execution_id.clone(),
                    fixture.instructions_id.clone(),
                ],
            );
            let child = fixture.plan.children.values_mut().next().unwrap();
            let CompiledOperator::Primitive { signature, .. } = &mut child.definition.operator
            else {
                unreachable!()
            };
            *signature = format!("worker-derived/{changed_id}");
            fixture.prepared.children[0].worker_package_id = changed_id;
            assert!(
                fixture.validate().is_err(),
                "accepted changed {path}: {changed_digest}"
            );
        }
    }
}
