use super::*;
use review_core::task::delivery::*;
use review_core::task::optimization_experiment::{
    ComparisonConclusionV1, EXPERIMENT_COMPARISON_V1, EXPERIMENT_PREPARED_V1,
    EXPERIMENT_SPECIFICATION_V1, ExperimentChildClosureV1, ExperimentComparisonV1,
    ExperimentPreparedV1, ExperimentSpecificationV1, ExperimentTrialV1, OPTIMIZATION_EVALUATION_V1,
    OPTIMIZATION_HARNESS_V1, OPTIMIZATION_PACKAGE_REPIN_V1, OPTIMIZATION_VERIFICATION_V1,
    OptimizationEvaluationV1, OptimizationHarnessV1, OptimizationPackageRepinV1,
    OptimizationVerificationV1, compare_experiment,
};
use review_core::task::optimization_light::{
    OPTIMIZATION_PROPOSAL_V1, OPTIMIZATION_RESULT_V1, OptimizationResultConclusionV1,
    OptimizationResultV1,
};
use review_graph::task::CompiledOperator;

/// Installed optimize-profile adapter. Generic satisfied Task state is insufficient: local
/// delivery additionally requires the exact positive comparison and independent verification
/// bound to the source and emitted candidate Snapshot.
fn validate_optimization_delivery_payload(
    cas: &Cas,
    task: &TaskRevisionV1,
    result: &TaskResultV1,
) -> Result<(), StoreError> {
    if task.kind != "optimize" {
        return Ok(());
    }
    let source = task
        .inputs
        .get("source")
        .and_then(|value| value.snapshot_id.as_deref())
        .ok_or_else(|| conflict("Optimization delivery has no exact source Snapshot"))?;
    let candidate = result
        .outputs
        .get("snapshot")
        .and_then(|value| value.snapshot_id.as_deref())
        .ok_or_else(|| conflict("Optimization delivery has no exact candidate Snapshot"))?;
    let requirements = task
        .inputs
        .get("requirements")
        .filter(|port| port.artifact_type == "af/Requirements@1" && port.artifact_ids.len() == 1)
        .and_then(|port| port.artifact_ids.first())
        .ok_or_else(|| conflict("Optimization delivery has no exact Task Requirements"))?;
    let current_run = task_run_id(&task.task_id)?;
    if result.outputs.contains_key("proposal") {
        let proposal = result
            .outputs
            .get("proposal")
            .filter(|port| {
                port.artifact_type == OPTIMIZATION_PROPOSAL_V1 && port.artifact_ids.len() == 1
            })
            .and_then(|port| port.artifact_ids.first())
            .ok_or_else(|| conflict("Light optimization delivery has no exact proposal"))?;
        let result_id = result
            .outputs
            .get("result")
            .filter(|port| {
                port.artifact_type == OPTIMIZATION_RESULT_V1 && port.artifact_ids.len() == 1
            })
            .and_then(|port| port.artifact_ids.first())
            .ok_or_else(|| conflict("Light optimization delivery has no economics decision"))?;
        let envelope = cas
            .get_artifact(result_id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        let economics: OptimizationResultV1 = serde_json::from_value(envelope.payload)?;
        economics.validate().map_err(conflict)?;
        if !matches!(
            envelope.producer,
            review_core::Producer::KernelOperation { ref run_id, ref operation_id, .. }
                if run_id == &current_run && operation_id == "optimization-exact-economics-v1"
        ) || economics.proposal_id != *proposal
            || economics.source_snapshot_id != source
            || economics.candidate_snapshot_id != candidate
            || economics.conclusion != OptimizationResultConclusionV1::Validated
            || !economics.adoption_offered
        {
            return Err(conflict(
                "Light optimization delivery was withheld by exact measured economics",
            ));
        }
    }
    let mut matched = None;
    for id in &result.evidence {
        let artifact = cas
            .get_artifact(id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        if artifact.artifact_type != OPTIMIZATION_VERIFICATION_V1 {
            continue;
        }
        if matched.is_some() {
            return Err(conflict(
                "Optimization delivery has ambiguous verification evidence",
            ));
        }
        let verification: OptimizationVerificationV1 = serde_json::from_value(artifact.payload)?;
        verification.validate().map_err(conflict)?;
        if !matches!(
            &artifact.producer,
            review_core::Producer::KernelOperation { run_id, operation_id, .. }
                if run_id == &current_run && operation_id == "optimization-final-verification-v1"
        ) {
            return Err(conflict(
                "Optimization verification was not produced by the installed current-Task finalizer",
            ));
        }
        let required = BTreeSet::from([
            verification.source_snapshot_id.clone(),
            verification.candidate_snapshot_id.clone(),
            verification.requirements_id.clone(),
            verification.harness_id.clone(),
            verification.comparison_id.clone(),
            verification.evaluation_id.clone(),
            verification.package_repin_id.clone(),
        ]);
        if verification.conclusion != ComparisonConclusionV1::Accepted
            || !verification.deliverable
            || !verification.protected_checks_passed
            || verification.source_snapshot_id != source
            || verification.candidate_snapshot_id != candidate
            || verification.requirements_id != *requirements
            || !required.is_subset(&artifact.input_artifacts.iter().cloned().collect())
        {
            return Err(conflict(
                "Optimization delivery differs from its positive candidate verification",
            ));
        }
        let comparison: ExperimentComparisonV1 =
            payload(cas, &verification.comparison_id, EXPERIMENT_COMPARISON_V1)?;
        comparison.validate().map_err(conflict)?;
        let comparison_envelope = cas
            .get_artifact(&verification.comparison_id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        if !matches!(
            &comparison_envelope.producer,
            review_core::Producer::KernelOperation { run_id, operation_id, .. }
                if run_id == &current_run && operation_id == "optimization-comparison-v1"
        ) {
            return Err(conflict(
                "Optimization comparison was not produced by the installed current-Task reducer",
            ));
        }
        let specification: ExperimentSpecificationV1 = payload(
            cas,
            &comparison.specification_id,
            EXPERIMENT_SPECIFICATION_V1,
        )?;
        let prepared: ExperimentPreparedV1 =
            payload(cas, &comparison.prepared_id, EXPERIMENT_PREPARED_V1)?;
        specification.validate().map_err(conflict)?;
        prepared.validate().map_err(conflict)?;
        if prepared.specification_id != comparison.specification_id
            || prepared.task_revision_id != result.task_revision_id
            || !BTreeSet::from([
                comparison.specification_id.clone(),
                comparison.prepared_id.clone(),
            ])
            .is_subset(
                &comparison_envelope
                    .input_artifacts
                    .iter()
                    .cloned()
                    .collect(),
            )
        {
            return Err(conflict(
                "Optimization comparison belongs to another preparation or Task revision",
            ));
        }
        let recomputed = compare_experiment(
            &comparison.specification_id,
            &comparison.prepared_id,
            &specification,
            comparison.trials.clone(),
        )
        .map_err(conflict)?;
        if recomputed != comparison || comparison.conclusion != ComparisonConclusionV1::Accepted {
            return Err(conflict("Optimization delivery comparison is not accepted"));
        }
        let evaluation_envelope = cas
            .get_artifact(&verification.evaluation_id)
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        if evaluation_envelope.artifact_type != OPTIMIZATION_EVALUATION_V1
            || !matches!(
                &evaluation_envelope.producer,
                review_core::Producer::Attempt { run_id, .. } if run_id == &current_run
            )
        {
            return Err(conflict(
                "Optimization evaluation is not an independent current-Task Attempt output",
            ));
        }
        let evaluation: OptimizationEvaluationV1 =
            serde_json::from_value(evaluation_envelope.payload)?;
        evaluation.validate().map_err(conflict)?;
        let evaluation_inputs = evaluation_envelope
            .input_artifacts
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        // Workers retain the typed SourceTree input, whose payload and Subject bind
        // the Snapshot; the Snapshot is not itself an executable input port.
        let mut candidate_retained = evaluation_inputs.contains(candidate);
        for id in &evaluation_inputs {
            // These are already matched as exact retained identities and need not be typed CAS
            // envelopes. Inspect the remaining inputs for a typed SourceTree carrier.
            if id == candidate || id == requirements {
                continue;
            }
            let input = cas
                .get_artifact(id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            if input.artifact_type == "af/SourceTree@1"
                && input
                    .payload
                    .get("snapshot_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(candidate)
                && input.subject_snapshot_id.as_deref() == Some(candidate)
                && input.input_artifacts.iter().any(|id| id == candidate)
            {
                candidate_retained = true;
            }
        }
        if evaluation.task_revision_id != result.task_revision_id
            || evaluation.requirements_id != *requirements
            || evaluation.candidate_snapshot_id != candidate
            || evaluation.specification_id != comparison.specification_id
            || evaluation.prepared_id != comparison.prepared_id
            || evaluation.comparison_id != verification.comparison_id
            || evaluation.conclusion != ComparisonConclusionV1::Accepted
            || !candidate_retained
            || !BTreeSet::from([requirements.clone(), verification.comparison_id.clone()])
                .is_subset(&evaluation_inputs)
        {
            return Err(conflict(
                "Optimization evaluation does not cover the current Requirements, candidate and comparison",
            ));
        }
        let harness: OptimizationHarnessV1 =
            payload(cas, &verification.harness_id, OPTIMIZATION_HARNESS_V1)?;
        harness.validate().map_err(conflict)?;
        let repin: OptimizationPackageRepinV1 = payload(
            cas,
            &verification.package_repin_id,
            OPTIMIZATION_PACKAGE_REPIN_V1,
        )?;
        repin.validate().map_err(conflict)?;
        if repin.source_snapshot_id != source || repin.candidate_snapshot_id != candidate {
            return Err(conflict(
                "Optimization delivery uses a stale or unrelated package repin",
            ));
        }
        matched = Some(id);
    }
    if matched.is_none() {
        return Err(conflict(
            "Optimization delivery requires installed candidate verification evidence",
        ));
    }
    Ok(())
}

/// Delivery authority is a conjunction of the typed receipts and the checked common-runtime
/// projection. Artifact envelopes alone are descriptive: their producer strings and trial
/// booleans cannot prove that the registered closure actually ran and settled in this Task.
pub fn validate_optimization_delivery(
    cas: &Cas,
    task: &TaskProjection,
    result: &TaskResultV1,
) -> Result<(), StoreError> {
    validate_optimization_delivery_payload(cas, &task.revision, result)?;
    if task.revision.kind != "optimize" {
        return Ok(());
    }
    let execution = task.execution.as_ref().ok_or_else(|| {
        conflict("Optimization delivery has no common-runtime execution evidence")
    })?;
    let verification_id = result
        .evidence
        .iter()
        .find(|id| {
            cas.get_artifact(id)
                .is_ok_and(|artifact| artifact.artifact_type == OPTIMIZATION_VERIFICATION_V1)
        })
        .ok_or_else(|| conflict("Optimization delivery has no candidate verification"))?;
    let verification: OptimizationVerificationV1 =
        payload(cas, verification_id, OPTIMIZATION_VERIFICATION_V1)?;
    let comparison: ExperimentComparisonV1 =
        payload(cas, &verification.comparison_id, EXPERIMENT_COMPARISON_V1)?;
    let experiment = execution
        .experiments
        .get(&comparison.prepared_id)
        .filter(|experiment| experiment.child_plan.is_some())
        .ok_or_else(|| conflict("Optimization comparison was not registered in this Task"))?;
    if experiment.prepared.specification_id != comparison.specification_id {
        return Err(conflict(
            "Optimization comparison differs from its registered preparation",
        ));
    }
    let accounting = execution.attempt_accounting();
    for trial in &comparison.trials {
        let closure = registered_trial(&experiment.prepared, trial)?;
        let planned = experiment
            .child_plan
            .as_ref()
            .and_then(|plan| plan.children.get(&closure.node))
            .ok_or_else(|| conflict("Optimization trial has no registered executable node"))?;
        let trusted_verifier = matches!(
            planned.definition.operator,
            CompiledOperator::Primitive {
                operator: task::pipeline::TaskOperatorV1::Verify { .. }
                    | task::pipeline::TaskOperatorV1::FixVerify { .. },
                ..
            }
        ) && planned.allowance.verification_attempts > 0;
        if !trusted_verifier {
            return Err(conflict(
                "Optimization trial conclusion was not produced by a protected verifier slot",
            ));
        }
        let attempts = accounting
            .iter()
            // Preparation and runtime invocation envelopes have different producers and can
            // therefore have different artifact IDs for identical invocation bytes.  The
            // registered node is the durable scheduling identity; registration already proved
            // that its invocation exactly equals the captured preparation payload.
            .filter(|attempt| attempt.reservation.node == closure.node)
            .cloned()
            .collect::<Vec<_>>();
        let charged_tokens = attempts.iter().try_fold(0u128, |total, attempt| {
            total
                .checked_add(attempt.charged_tokens)
                .ok_or_else(|| conflict("Optimization trial charge overflow"))
        })?;
        let billing_complete = !attempts.is_empty()
            && attempts
                .iter()
                .map(|attempt| attempt.billing_complete(cas, closure.effort != "command"))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .all(|complete| complete);
        let verified = attempts.iter().any(|attempt| {
            matches!(
                attempt.result,
                Some(task::execution::TaskAttemptResultV1::Succeeded { .. })
            )
        }) && execution.outputs.contains_key(&closure.node);
        let measured = super::execution::experiment::experiment_measured_costs(cas, &attempts)?;
        let exact_measurements = trial.elapsed_ms == measured.elapsed_ms
            && trial.preparation_ms == measured.preparation_ms
            && trial.cache_population_ms == measured.cache_population_ms
            && trial.cache_lookup_ms == measured.cache_lookup_ms
            && trial.cache_copy_ms == measured.cache_copy_ms
            && trial.missing_measurements == measured.missing_measurements
            && trial.intervals == measured.intervals;
        validate_trial_facts(
            trial,
            charged_tokens,
            billing_complete,
            verified,
            exact_measurements,
        )?;
    }
    let evaluation = cas
        .get_artifact(&verification.evaluation_id)
        .map_err(|error| StoreError::Artifact(error.to_string()))?;
    let (evaluation_node, evaluation_attempt) = match &evaluation.producer {
        review_core::Producer::Attempt {
            node_id,
            attempt_id,
            ..
        } => (node_id, attempt_id),
        _ => return Err(conflict("Optimization evaluation has no Attempt producer")),
    };
    require_independent_evaluator(&experiment.prepared, evaluation_node)?;
    if !matches!(
        execution
            .graph
            .nodes
            .get(evaluation_node)
            .map(|node| &node.operator),
        Some(CompiledOperator::Primitive {
            operator: task::pipeline::TaskOperatorV1::Verify { .. }
                | task::pipeline::TaskOperatorV1::FixVerify { .. },
            ..
        })
    ) || execution
        .graph
        .allowances
        .get(evaluation_node)
        .is_none_or(|allowance| allowance.verification_attempts == 0)
        || !accounting.iter().any(|attempt| {
            &attempt.attempt_id == evaluation_attempt
                && &attempt.reservation.node == evaluation_node
                && attempt.result.as_ref().is_some_and(|result| {
                    matches!(
                        result,
                        task::execution::TaskAttemptResultV1::Succeeded { .. }
                    )
                })
        })
        || !execution
            .outputs
            .get(evaluation_node)
            .is_some_and(|(_, output)| {
                output.outputs.values().any(|port| {
                    port.artifact_type == OPTIMIZATION_EVALUATION_V1
                        && port.artifact_ids.contains(&verification.evaluation_id)
                })
            })
    {
        return Err(conflict(
            "Optimization evaluation is not a published independent verifier-node output",
        ));
    }
    Ok(())
}

fn registered_trial<'a>(
    prepared: &'a ExperimentPreparedV1,
    trial: &ExperimentTrialV1,
) -> Result<&'a ExperimentChildClosureV1, StoreError> {
    let closure = prepared
        .children
        .iter()
        .find(|closure| closure.invocation_id == trial.invocation_id)
        .ok_or_else(|| conflict("Optimization trial was not in the registered closure"))?;
    if closure.case_id != trial.case_id
        || closure.arm != trial.arm
        || closure.repetition != trial.repetition
    {
        return Err(conflict(
            "Optimization trial changed its registered case, arm or repetition",
        ));
    }
    Ok(closure)
}

fn validate_trial_facts(
    trial: &ExperimentTrialV1,
    charged_tokens: u128,
    billing_complete: bool,
    verified: bool,
    exact_measurements: bool,
) -> Result<(), StoreError> {
    if charged_tokens != u128::from(trial.charged_tokens)
        || trial.billing_complete != billing_complete
        || trial.verified != verified
        || trial.protected_checks_passed != verified
        || !exact_measurements
    {
        return Err(conflict(
            "Optimization trial payload contradicts settled accounting or protected output",
        ));
    }
    Ok(())
}

fn require_independent_evaluator(
    prepared: &ExperimentPreparedV1,
    evaluation_node: &str,
) -> Result<(), StoreError> {
    if prepared
        .children
        .iter()
        .any(|closure| closure.node == evaluation_node)
    {
        return Err(conflict(
            "Optimization evaluation reuses a registered experiment arm",
        ));
    }
    Ok(())
}

impl TaskProjection {
    pub(super) fn apply_delivery(&mut self, cas: &Cas, id: &str) -> Result<(), StoreError> {
        let value: TaskDeliveryRecordV1 = payload(cas, id, TASK_DELIVERY_RECORD_V1)?;
        value.validate().map_err(conflict)?;
        let TaskPhaseV1::Finished { result_id } = &self.phase else {
            return Err(conflict("Delivery requires a finished Task"));
        };
        let result: TaskResultV1 = payload(cas, result_id, task::TASK_RESULT_V1)?;
        validate_optimization_delivery(cas, self, &result)?;
        let snapshot = |ports: &BTreeMap<String, task::ArtifactInputV1>, name: &str| {
            ports
                .get(name)
                .filter(|v| v.artifact_type == "af/SourceTree@1" && v.artifact_ids.len() == 1)
                .and_then(|v| v.snapshot_id.clone())
        };
        if result.acceptance != TaskAcceptanceV1::Satisfied
            || value.task_id != self.task_id
            || value.result_id != *result_id
            || snapshot(&self.revision.inputs, "source").as_ref() != Some(&value.source_snapshot_id)
            || snapshot(&result.outputs, "snapshot").as_ref() != Some(&value.derived_snapshot_id)
        {
            return Err(conflict(
                "Delivery differs from the exact verified Task result",
            ));
        }
        let receipt = cas
            .get_json(&value.receipt_id)
            .map_err(|e| conflict(e.to_string()))?;
        let target = cas
            .get_json(&value.target_id)
            .map_err(|e| conflict(e.to_string()))?;
        let expected_schema = if value.status == TaskDeliveryStatusV1::Prepared {
            "af/task-delivery-prepared@1"
        } else {
            "af/task-delivery@1"
        };
        let expected_outcome = match value.status {
            TaskDeliveryStatusV1::Prepared => None,
            TaskDeliveryStatusV1::Delivered => Some("delivered"),
            TaskDeliveryStatusV1::Failed => Some("failed"),
        };
        if receipt["schema"] != expected_schema
            || receipt["task_id"] != value.task_id
            || receipt["result_id"] != value.result_id
            || receipt["source_snapshot_id"] != value.source_snapshot_id
            || receipt["derived_snapshot_id"] != value.derived_snapshot_id
            || receipt.get("target") != Some(&target)
            || receipt
                .pointer("/outcome/kind")
                .and_then(serde_json::Value::as_str)
                != expected_outcome
            || (expected_outcome.is_some() && receipt.get("remote_actions") != Some(&json!([])))
        {
            return Err(conflict(
                "Delivery receipt contradicts its recorded identity or local scope",
            ));
        }
        let previous = self
            .deliveries
            .iter()
            .rev()
            .find(|(_, previous)| previous.result_id == value.result_id);
        match (previous, value.status) {
            (None, TaskDeliveryStatusV1::Prepared) => {}
            (Some((_, previous)), TaskDeliveryStatusV1::Prepared)
                if previous.status == TaskDeliveryStatusV1::Failed => {}
            (
                Some((_, previous)),
                TaskDeliveryStatusV1::Delivered | TaskDeliveryStatusV1::Failed,
            ) if previous.status == TaskDeliveryStatusV1::Prepared
                && previous.target_id == value.target_id =>
            {
                let prepared = cas
                    .get_json(&previous.receipt_id)
                    .map_err(|e| conflict(e.to_string()))?;
                if receipt["delivery_id"] != prepared["delivery_id"] {
                    return Err(conflict(
                        "Delivery terminal receipt changed prepared operation",
                    ));
                }
            }
            _ => {
                return Err(conflict(
                    "Delivery transition has no matching prepared operation",
                ));
            }
        }
        self.deliveries.push((id.into(), value));
        Ok(())
    }
}

impl EventStore {
    pub fn record_task_delivery(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        record_id: &str,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(
            cas,
            lease,
            TaskChangeV1::DeliveryRecorded {
                record_id: record_id.into(),
            },
            now()?,
        )
    }

    pub fn record_task_adoption_observation(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        observation_id: &str,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(
            cas,
            lease,
            TaskChangeV1::AdoptionObservationRecorded {
                observation_id: observation_id.into(),
            },
            now()?,
        )
    }
}

#[cfg(test)]
mod optimization_delivery_tests {
    use super::*;
    use review_core::PortCardinality;
    use review_core::Producer;
    use review_core::task::optimization_experiment::*;
    use serde_json::json;

    fn id(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn producer() -> Producer {
        Producer::KernelOperation {
            run_id: "optimization-delivery-test".into(),
            node_id: None,
            operation_id: "fixture".into(),
        }
    }

    #[test]
    fn optimize_delivery_consumes_positive_comparison_harness_and_repin() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let run_id = task_run_id("optimize-1").unwrap();
        let task_revision = id('7');
        let source = id('1');
        let candidate = id('2');
        let requirements = id('3');
        let harness = OptimizationHarnessV1 {
            schema: "af.optimization-harness/1".into(),
            oracle_id: id('4'),
            checks: ["regression".into()].into(),
            transitive_dependencies: ["fixtures/oracle.sh".into()].into(),
            candidate_writable_paths: ["scripts/gate.sh".into()].into(),
            cache_read_scopes: ["cache/build".into()].into(),
            cache_write_scopes: ["cache/build".into()].into(),
        };
        let harness_id = cas
            .put_artifact(
                OPTIMIZATION_HARNESS_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(harness).unwrap(),
            )
            .unwrap()
            .0;
        let measured = |invocation_id: String, arm, verified| ExperimentTrialV1 {
            invocation_id,
            case_id: id('6'),
            family_id: id('7'),
            arm,
            repetition: 1,
            compatibility_id: id('8'),
            verified,
            protected_checks_passed: true,
            billing_complete: true,
            charged_tokens: 10,
            elapsed_ms: 10,
            preparation_ms: 1,
            cache_population_ms: 1,
            cache_lookup_ms: 1,
            cache_copy_ms: 1,
            missing_measurements: BTreeSet::new(),
            intervals: vec![
                ExperimentMeasurementIntervalV1 {
                    kind: ExperimentIntervalKindV1::Execution,
                    start_ms: 0,
                    end_ms: 10,
                },
                ExperimentMeasurementIntervalV1 {
                    kind: ExperimentIntervalKindV1::Preparation,
                    start_ms: 0,
                    end_ms: 1,
                },
                ExperimentMeasurementIntervalV1 {
                    kind: ExperimentIntervalKindV1::CachePopulation,
                    start_ms: 1,
                    end_ms: 2,
                },
                ExperimentMeasurementIntervalV1 {
                    kind: ExperimentIntervalKindV1::CacheLookup,
                    start_ms: 2,
                    end_ms: 3,
                },
                ExperimentMeasurementIntervalV1 {
                    kind: ExperimentIntervalKindV1::CacheCopy,
                    start_ms: 3,
                    end_ms: 4,
                },
            ],
        };
        let specification = ExperimentSpecificationV1 {
            schema: "af.experiment-specification/1".into(),
            slot_id: id('9'),
            policy_id: id('a'),
            profile_id: id('b'),
            development_set_id: id('c'),
            holdout_set_id: id('d'),
            protected_oracle_id: id('4'),
            baseline_authority_id: id('e'),
            candidate_authority_id: id('f'),
            baseline_package: "trial/baseline".into(),
            candidate_package: "trial/candidate".into(),
            recipe: ComparisonRecipeV1::DeterministicCorrection,
            uncertainty_rule: ComparisonUncertaintyRuleV1::Deterministic,
            repetitions: 1,
            minimum_families: 1,
            token_increase_ceiling_bps: 0,
            exposed_family_ids: BTreeSet::new(),
            cases: vec![ExperimentCaseV1 {
                case_id: id('6'),
                family_id: id('7'),
                membership: "holdout".into(),
                source_snapshot_id: source.clone(),
                requirements_id: requirements.clone(),
                compatibility_id: id('8'),
            }],
        };
        let specification_id = cas
            .put_artifact(
                EXPERIMENT_SPECIFICATION_V1,
                producer(),
                vec![source.clone(), requirements.clone()],
                None,
                serde_json::to_value(&specification).unwrap(),
            )
            .unwrap()
            .0;
        let child = |node: &str, arm, package: &str, authority: String, invocation_id: String| {
            ExperimentChildClosureV1 {
                node: node.into(),
                arm,
                case_id: id('6'),
                repetition: 1,
                task_kind: "trial".into(),
                package: package.into(),
                worker_package_id: id('1'),
                effort: "command".into(),
                effects: BTreeSet::new(),
                source_snapshot_id: source.clone(),
                requirements_id: requirements.clone(),
                authority_id: authority,
                invocation_id,
                allowance: ExperimentAllowanceV1 {
                    tokens: 10,
                    attempts: 1,
                    wall_ms: 10,
                },
            }
        };
        let prepared = ExperimentPreparedV1 {
            schema: "af.experiment-prepared/1".into(),
            task_revision_id: task_revision.clone(),
            outer_plan_id: id('2'),
            slot_id: id('9'),
            specification_id: specification_id.clone(),
            compiled_child_plan_id: id('3'),
            policy_id: id('a'),
            spent_accounting_prefix_id: id('4'),
            writer_epoch: 1,
            children: vec![
                child(
                    "root.baseline",
                    ExperimentArmV1::Baseline,
                    "trial/baseline",
                    id('e'),
                    id('5'),
                ),
                child(
                    "root.candidate",
                    ExperimentArmV1::Candidate,
                    "trial/candidate",
                    id('f'),
                    id('6'),
                ),
            ],
        };
        let prepared_id = cas
            .put_artifact(
                EXPERIMENT_PREPARED_V1,
                producer(),
                vec![specification_id.clone()],
                None,
                serde_json::to_value(&prepared).unwrap(),
            )
            .unwrap()
            .0;
        let comparison = compare_experiment(
            &specification_id,
            &prepared_id,
            &specification,
            vec![
                measured(id('5'), ExperimentArmV1::Baseline, false),
                measured(id('6'), ExperimentArmV1::Candidate, true),
            ],
        )
        .unwrap();
        let candidate_trial = comparison
            .trials
            .iter()
            .find(|trial| trial.arm == ExperimentArmV1::Candidate)
            .unwrap();
        registered_trial(&prepared, candidate_trial).unwrap();
        let mut unregistered = candidate_trial.clone();
        unregistered.invocation_id = id('0');
        assert!(registered_trial(&prepared, &unregistered).is_err());
        let mut forged_verdict = candidate_trial.clone();
        forged_verdict.verified = false;
        assert!(
            validate_trial_facts(&forged_verdict, 10, true, true, true).is_err(),
            "a trial cannot forge the Store-derived protected verdict"
        );
        assert!(
            require_independent_evaluator(&prepared, &prepared.children[1].node).is_err(),
            "an experiment arm cannot also act as the evaluator"
        );
        require_independent_evaluator(&prepared, "root.evaluate").unwrap();
        let comparison_id = cas
            .put_artifact(
                EXPERIMENT_COMPARISON_V1,
                Producer::KernelOperation {
                    run_id: run_id.clone(),
                    node_id: Some("root.compare".into()),
                    operation_id: "optimization-comparison-v1".into(),
                },
                vec![specification_id.clone(), prepared_id.clone()],
                None,
                serde_json::to_value(&comparison).unwrap(),
            )
            .unwrap()
            .0;
        let repin = OptimizationPackageRepinV1 {
            schema: "af.optimization-package-repin/1".into(),
            source_snapshot_id: source.clone(),
            candidate_snapshot_id: candidate.clone(),
            before_lock_id: id('b'),
            after_lock_id: id('c'),
            engine_release_id: id('d'),
            entailed: [(
                "project/candidate".into(),
                PackagePinChangeV1 {
                    before: id('e'),
                    after: id('f'),
                },
            )]
            .into(),
            protected_pins: [("engine/release".into(), id('d'))].into(),
        };
        let repin_id = cas
            .put_artifact(
                OPTIMIZATION_PACKAGE_REPIN_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(repin).unwrap(),
            )
            .unwrap()
            .0;
        let evaluation = OptimizationEvaluationV1 {
            schema: "af.optimization-evaluation/1".into(),
            task_revision_id: task_revision.clone(),
            requirements_id: requirements.clone(),
            candidate_snapshot_id: candidate.clone(),
            specification_id: specification_id.clone(),
            prepared_id: prepared_id.clone(),
            comparison_id: comparison_id.clone(),
            conclusion: ComparisonConclusionV1::Accepted,
            reason: "independently_accepted".into(),
        };
        let evaluation_id = cas
            .put_artifact(
                OPTIMIZATION_EVALUATION_V1,
                Producer::Attempt {
                    run_id: run_id.clone(),
                    node_id: "root.evaluate".into(),
                    attempt_id: "00000000000000000000000000".into(),
                },
                vec![
                    requirements.clone(),
                    candidate.clone(),
                    comparison_id.clone(),
                ],
                Some(candidate.clone()),
                serde_json::to_value(&evaluation).unwrap(),
            )
            .unwrap()
            .0;
        let verification = OptimizationVerificationV1 {
            schema: "af.optimization-verification/1".into(),
            profile: OptimizationProfileV1::Candidate,
            source_snapshot_id: source.clone(),
            candidate_snapshot_id: candidate.clone(),
            requirements_id: requirements.clone(),
            harness_id: harness_id.clone(),
            comparison_id: comparison_id.clone(),
            evaluation_id: evaluation_id.clone(),
            package_repin_id: repin_id.clone(),
            conclusion: ComparisonConclusionV1::Accepted,
            deliverable: true,
            protected_checks_passed: true,
        };
        let verification_id = cas
            .put_artifact(
                OPTIMIZATION_VERIFICATION_V1,
                Producer::KernelOperation {
                    run_id,
                    node_id: None,
                    operation_id: "optimization-final-verification-v1".into(),
                },
                vec![
                    source.clone(),
                    candidate.clone(),
                    requirements.clone(),
                    harness_id.clone(),
                    comparison_id,
                    evaluation_id,
                    repin_id.clone(),
                ],
                None,
                serde_json::to_value(&verification).unwrap(),
            )
            .unwrap()
            .0;
        let task: TaskRevisionV1 = serde_json::from_value(json!({
            "task_id":"optimize-1","revision":1,"kind":"optimize","goal":"verify candidate",
            "inputs":{
                "source":{"artifact_ids":[source],"artifact_type":"af/SourceTree@1","cardinality":"one","snapshot_id":id('1')},
                "requirements":{"artifact_ids":[requirements],"artifact_type":"af/Requirements@1","cardinality":"one"}
            },
            "required_outputs":{"snapshot":{"artifact_type":"af/SourceTree@1","cardinality":"one"}},
            "acceptance":{"verified":{"evidence_type":OPTIMIZATION_VERIFICATION_V1,"verifier_policy":id('4')}},
            "provenance":{"adapter_id":id('5'),"input_artifact_ids":[]},
            "authority":{"policy_id":id('6'),"allowed_effects":[],"data_destinations":[]},
            "limits":{"tokens":100,"max_attempts":2,"deadline_unix_ms":1000,"verification":{"tokens":0,"attempts":0,"wall_ms":0}},
            "strategy":"candidate","facts":{}
        })).unwrap();
        let mut result = TaskResultV1 {
            task_revision_id: task_revision,
            execution: review_core::task::TaskExecutionV1::Completed,
            acceptance: TaskAcceptanceV1::Satisfied,
            domain_conclusion: "verified".into(),
            outputs: [(
                "snapshot".into(),
                task::ArtifactInputV1 {
                    artifact_ids: vec![candidate.clone()],
                    artifact_type: "af/SourceTree@1".into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: Some(candidate.clone()),
                },
            )]
            .into(),
            evidence: [verification_id].into(),
            missing_obligations: BTreeSet::new(),
        };
        validate_optimization_delivery_payload(&cas, &task, &result).unwrap();

        let mut wrong_requirements = task.clone();
        wrong_requirements
            .inputs
            .get_mut("requirements")
            .unwrap()
            .artifact_ids = vec![id('0')];
        assert!(
            validate_optimization_delivery_payload(&cas, &wrong_requirements, &result).is_err()
        );

        let mut forged_comparison = comparison;
        forged_comparison.candidate_tokens = 1;
        let forged_comparison_id = cas
            .put_artifact(
                EXPERIMENT_COMPARISON_V1,
                Producer::KernelOperation {
                    run_id: task_run_id("optimize-1").unwrap(),
                    node_id: Some("root.compare".into()),
                    operation_id: "optimization-comparison-v1".into(),
                },
                vec![specification_id, prepared_id],
                None,
                serde_json::to_value(forged_comparison).unwrap(),
            )
            .unwrap()
            .0;
        let mut forged_evaluation = evaluation;
        forged_evaluation.comparison_id = forged_comparison_id.clone();
        let forged_evaluation_id = cas
            .put_artifact(
                OPTIMIZATION_EVALUATION_V1,
                Producer::Attempt {
                    run_id: task_run_id("optimize-1").unwrap(),
                    node_id: "root.evaluate".into(),
                    attempt_id: "00000000000000000000000001".into(),
                },
                vec![
                    requirements.clone(),
                    candidate.clone(),
                    forged_comparison_id.clone(),
                ],
                Some(candidate.clone()),
                serde_json::to_value(forged_evaluation).unwrap(),
            )
            .unwrap()
            .0;
        let mut forged_verification = verification;
        forged_verification.comparison_id = forged_comparison_id.clone();
        forged_verification.evaluation_id = forged_evaluation_id.clone();
        let forged_verification_id = cas
            .put_artifact(
                OPTIMIZATION_VERIFICATION_V1,
                Producer::KernelOperation {
                    run_id: task_run_id("optimize-1").unwrap(),
                    node_id: None,
                    operation_id: "optimization-final-verification-v1".into(),
                },
                vec![
                    source,
                    candidate,
                    requirements,
                    harness_id,
                    forged_comparison_id,
                    forged_evaluation_id,
                    repin_id,
                ],
                None,
                serde_json::to_value(forged_verification).unwrap(),
            )
            .unwrap()
            .0;
        let mut forged_result = result.clone();
        forged_result.evidence = BTreeSet::from([forged_verification_id]);
        assert!(validate_optimization_delivery_payload(&cas, &task, &forged_result).is_err());

        result.acceptance = TaskAcceptanceV1::Unsatisfied;
        // The adapter is intentionally independent of generic Task state; callers enforce it too.
        result.evidence.clear();
        assert!(validate_optimization_delivery_payload(&cas, &task, &result).is_err());
    }
}
