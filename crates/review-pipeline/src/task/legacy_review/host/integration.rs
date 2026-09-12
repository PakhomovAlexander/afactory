//! Post-Round Review business operations. Store alone activates, charges and atomically
//! publishes this phase; the host supplies the captured selection and Check implementation.
use super::*;
use crate::review_domain::integration::{
    IntegrationSelection, IntegrationViews, PreparedIntegration,
};
use review_config::task::legacy_review::artifact::ReviewArtifactCodec;
use review_core::task::report::{TASK_RUN_REPORT_V2, TaskNodeOutcomeV1, TaskRunReportV2};
use review_core::task::review_integration::*;
use review_store::store::task::review_integration::{
    RegisteredTaskReviewIntegration, TaskReviewIntegrationEvidence,
    capture_task_review_integration, read_task_review_integration,
};

impl LegacyReviewTaskHost<'_, '_> {
    pub(super) fn is_integration(&self, input: &TaskInvocationV1) -> bool {
        input.plan_id == self.plan_id
            && self
                .captured
                .compilation
                .graph
                .review_integration
                .as_ref()
                .is_some_and(|p| p.node == input.node)
    }

    fn integration_input(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewIntegrationPhaseV1, String> {
        if !self.is_integration(input) {
            return Err("Not the captured Integration sequence".into());
        }
        let port = input
            .inputs
            .get("phase")
            .ok_or("Integration lacks its phase")?;
        let [id] = port.artifact_ids.as_slice() else {
            return Err("Integration phase is not singular".into());
        };
        let phase = read_task_review_integration(cas, id).map_err(|e| e.to_string())?;
        let TaskReviewIntegrationSelectionV1::Prepared {
            derived_snapshot_id,
            ..
        } = &phase.selection
        else {
            return Err("Integration has no prepared checks".into());
        };
        if input.inputs.len() != 1
            || port.artifact_type != TASK_REVIEW_INTEGRATION_PHASE_V1
            || port.cardinality != review_core::PortCardinality::One
            || port.snapshot_id.as_ref() != Some(derived_snapshot_id)
            || phase.task_id != self.task.task_id
            || phase.task_revision_id != self.plan.task_revision_id
            || phase.plan_id != self.plan_id
            || phase.round_id != self.task.inputs["round"].artifact_ids[0]
        {
            return Err("Integration invocation changed its exact phase authority".into());
        }
        Ok(phase)
    }

    pub(super) fn commit_integration_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        self.integration_input(cas, input)?;
        let store = self.domain.store.lock().expect("Task Store");
        let phase = store
            .registered_task_review_integration(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Integration is not activated")?;
        store
            .check_task_review_integration_current(
                cas,
                &self.lease,
                &phase,
                &self.authority(),
                false,
            )
            .map_err(|e| e.to_string())?;
        let state = store
            .task_projection(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        if state
            .execution
            .as_ref()
            .and_then(|e| e.invocations.get(&input.node))
            != Some(&(id.into(), input.clone()))
            || phase.inputs().map_err(|e| e.to_string())? != input.inputs
        {
            return Err("Integration invocation has no exact durable admission".into());
        }
        self.invocations
            .lock()
            .expect("Review invocations")
            .insert(input.node.clone(), (id.into(), phase.phase_id().into()));
        Ok(())
    }

    /// The passing Round is published first. Selecting a phase performs no subprocess or
    /// reservation; Empty and Conflict are terminal observations of the original head.
    pub fn select_recorded_integration(
        &self,
        cas: &Cas,
    ) -> Result<Option<RegisteredTaskReviewIntegration>, String> {
        if self.captured.compilation.graph.review_integration.is_none() {
            return Ok(None);
        }
        let existing = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .registered_task_review_integration(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?;
        if let Some(existing) = existing {
            self.domain
                .store
                .lock()
                .expect("Task Store")
                .check_task_review_integration_current(
                    cas,
                    &self.lease,
                    &existing,
                    &self.authority(),
                    false,
                )
                .map_err(|e| e.to_string())?;
            return Ok(Some(existing));
        }
        // Existing activation remains factual recovery work after late resource loss. Only
        // creating a new phase requires a passing, resource-current Round conclusion.
        let conclusion = self.publish_recorded_round_conclusion(cas)?;
        if conclusion.verdict != crate::RunVerdict::Pass || conclusion.resources_failed {
            return Ok(None);
        }
        let policy = self
            .captured
            .loaded
            .integration()
            .ok_or("Captured Integration policy disappeared")?;
        let events = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .replay(&self.domain.run_id)
            .map_err(|e| e.to_string())?;
        let projection =
            review_store::LedgerProjection::from_events(&self.domain.run_id, &events, cas)
                .map_err(|e| e.to_string())?;
        let selection = match self.domain.select_integration(
            policy,
            self.captured.loaded.reviewer_execution(),
            &events,
            projection.ledger(),
        )? {
            IntegrationSelection::Empty => TaskReviewIntegrationSelectionV1::Empty {},
            IntegrationSelection::Conflict(event) => TaskReviewIntegrationSelectionV1::Conflict {
                conflict: serde_json::from_value(event.payload).map_err(|e| e.to_string())?,
            },
            IntegrationSelection::Candidates(selected) => {
                let prepared = self.domain.prepare_integration(policy, &selected)?;
                TaskReviewIntegrationSelectionV1::Prepared {
                    integration_plan_id: prepared.plan_artifact_id,
                    derived_snapshot_id: prepared.derived_snapshot_id,
                }
            }
        };
        let phase = TaskReviewIntegrationPhaseV1 {
            task_id: self.task.task_id.clone(),
            task_revision_id: self.plan.task_revision_id.clone(),
            plan_id: self.plan_id.clone(),
            round_id: self.task.inputs["round"].artifact_ids[0].clone(),
            closing_report_event_id: conclusion.canonical_report_event_id,
            selection,
        };
        let id = capture_task_review_integration(cas, &phase).map_err(|e| e.to_string())?;
        self.domain
            .store
            .lock()
            .expect("Task Store")
            .select_task_review_integration(cas, &self.lease, &id, &self.authority())
            .map(Some)
            .map_err(|e| e.to_string())
    }

    fn prepared_integration(
        &self,
        cas: &Cas,
        phase: &TaskReviewIntegrationPhaseV1,
        evidence: &TaskReviewIntegrationEvidence,
    ) -> Result<PreparedIntegration, String> {
        let policy = self
            .captured
            .loaded
            .integration()
            .ok_or("No captured Integration policy")?;
        let ledger =
            review_store::LedgerProjection::from_events(&self.domain.run_id, &evidence.events, cas)
                .map_err(|e| e.to_string())?;
        let IntegrationSelection::Candidates(selected) = self.domain.select_integration(
            policy,
            self.captured.loaded.reviewer_execution(),
            &evidence.events,
            ledger.ledger(),
        )?
        else {
            return Err("Prepared Integration no longer matches its selected Proposals".into());
        };
        let TaskReviewIntegrationSelectionV1::Prepared {
            integration_plan_id,
            derived_snapshot_id,
        } = &phase.selection
        else {
            return Err("Integration selection has no checks".into());
        };
        self.domain.read_prepared_integration(
            policy,
            &selected,
            integration_plan_id,
            derived_snapshot_id,
        )
    }

    pub(super) fn execute_integration(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> TaskWorkOutput {
        let mut raw_artifact_ids = Vec::new();
        let outputs = (|| {
            if broker.is_some() {
                return Err("Integration checks do not consume Broker Handles".into());
            }
            let attempt =
                attempt.ok_or("Integration checks require their started common Attempt")?;
            if attempt.node() != input.node || attempt.context_id() != self.invocation(input)?.0 {
                return Err(
                    "Integration changed its started Attempt or exact invocation context".into(),
                );
            }
            let deadline = self.current(attempt)?;
            let phase = self.integration_input(cas, input)?;
            let (registered, evidence) = {
                let store = self.domain.store.lock().expect("Task Store");
                let registered = store
                    .registered_task_review_integration(cas, &self.task.task_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("Integration is not activated")?;
                let evidence = store
                    .task_review_integration_evidence(cas, &registered)
                    .map_err(|e| e.to_string())?;
                (registered, evidence)
            };
            if registered.phase() != &phase {
                return Err("Integration changed its registered phase".into());
            }
            let prepared = self.prepared_integration(cas, &phase, &evidence)?;
            let checks = match self.domain.run_integration_checks_recorded(
                self.captured
                    .loaded
                    .integration()
                    .ok_or("No Integration policy")?,
                &prepared.derived_manifest,
                &prepared.derived_snapshot_id,
                Some(deadline),
            ) {
                Ok(checks) => checks,
                Err(failure) => {
                    raw_artifact_ids.extend(failure.result_artifact_ids);
                    return Err(failure.message);
                }
            };
            raw_artifact_ids.extend(checks.checks.iter().map(|c| c.result_artifact_id.clone()));
            // Legacy Integration retains NotRun in its complete checks artifact. The common
            // Task must distinguish an unavailable/unverifiable Check from a failing verdict.
            for check in &checks.checks {
                let result: review_check::CheckResult = serde_json::from_value(
                    cas.get_json(&check.result_artifact_id)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if result.status == review_check::CheckStatus::NotRun {
                    return Err(format!(
                        "Integration Check `{}` did not run to a verifiable verdict: {}",
                        check.name,
                        result.reason.as_deref().unwrap_or("no result")
                    ));
                }
            }
            let raw = cas
                .put_json(&serde_json::to_value(&checks).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            raw_artifact_ids.push(raw.clone());
            let id = ReviewArtifactCodec::Flat {
                artifact_type: review_core::contract::INTEGRATION_CHECKS_V1.into(),
            }
            .capture(
                cas,
                &raw,
                Producer::Attempt {
                    run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                    node_id: input.node.clone(),
                    attempt_id: attempt.id().into(),
                },
                Some(prepared.derived_snapshot_id),
            )?;
            let outputs = BTreeMap::from([(
                "checks".into(),
                artifact_input(
                    cas,
                    review_core::contract::INTEGRATION_CHECKS_V1,
                    vec![id],
                    review_core::PortCardinality::One,
                )?,
            )]);
            self.admitted_outputs
                .lock()
                .expect("Review outputs")
                .entry(self.invocation(input)?.0)
                .or_default()
                .push(outputs.clone());
            Ok(outputs)
        })();
        TaskWorkOutput {
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
            outputs,
            charged_tokens: Some(0),
            raw_artifact_ids,
            usage_id: None,
            feedback_id: None,
        }
    }

    /// Atomically publish the complete checks and any promotion only from a selected common
    /// output. Failed/incomplete execution seals factual phase evidence without promotion.
    pub fn finish_recorded_integration(
        &self,
        cas: &Cas,
        phase: &RegisteredTaskReviewIntegration,
        report_id: &str,
    ) -> Result<RegisteredTaskReviewIntegration, String> {
        let (active, evidence) = {
            let store = self.domain.store.lock().expect("Task Store");
            let active = store
                .registered_task_review_integration(cas, &self.task.task_id)
                .map_err(|e| e.to_string())?
                .ok_or("Integration missing")?;
            if active.phase_id() != phase.phase_id() {
                return Err("Integration finish changed its phase".into());
            }
            store
                .check_task_review_integration_current(
                    cas,
                    &self.lease,
                    &active,
                    &self.authority(),
                    false,
                )
                .map_err(|e| e.to_string())?;
            let evidence = store
                .task_review_integration_evidence(cas, &active)
                .map_err(|e| e.to_string())?;
            (active, evidence)
        };
        if active.finished() {
            if active.report_id() != Some(report_id) {
                return Err("Integration already sealed another report".into());
            }
            return Ok(active);
        }
        let value = cas.get_artifact(report_id).map_err(|e| e.to_string())?;
        if value.artifact_type != TASK_RUN_REPORT_V2 {
            return Err("Integration requires its phase report".into());
        }
        let report: TaskRunReportV2 =
            serde_json::from_value(value.payload).map_err(|e| e.to_string())?;
        report.validate()?;
        let mut events = Vec::new();
        if let TaskNodeOutcomeV1::Completed { output_id } = &report.nodes[0].outcome {
            let output: TaskOutputV1 = serde_json::from_value(
                cas.get_artifact(output_id)
                    .map_err(|e| e.to_string())?
                    .payload,
            )
            .map_err(|e| e.to_string())?;
            let id = output
                .outputs
                .get("checks")
                .and_then(|p| p.artifact_ids.first())
                .ok_or("Integration checks missing")?;
            let raw = ReviewArtifactCodec::Flat {
                artifact_type: review_core::contract::INTEGRATION_CHECKS_V1.into(),
            }
            .restore(cas, id)?;
            let checks: review_core::IntegrationChecksV1 =
                serde_json::from_value(cas.get_json(&raw).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let prepared = self.prepared_integration(cas, active.phase(), &evidence)?;
            let (checks_id, event) = self.domain.integration_checks_event(&prepared, &checks)?;
            events.push(event);
            if checks.passed() {
                if let Some(refusal) = self.integration_resource_refusal(cas, &active)? {
                    return self.finish_recorded_integration(cas, &active, &refusal);
                }
                let ledger = review_store::LedgerProjection::from_events(
                    &self.domain.run_id,
                    &evidence.events,
                    cas,
                )
                .map_err(|e| e.to_string())?;
                let views = IntegrationViews {
                    finding_set_id: evidence.finding_set_id,
                    demand_set_id: evidence.demand_set_id,
                    semantic_closure_id: evidence.semantic_closure_id,
                };
                events.extend(
                    self.domain
                        .prepare_integration_commit(&prepared, &checks_id, &views, ledger.ledger())?
                        .events,
                );
            }
        }
        let result = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .finish_task_review_integration(
                cas,
                &self.lease,
                &active,
                report_id,
                &events,
                &self.authority(),
            );
        if let Err(error) = result {
            if events
                .last()
                .is_some_and(|e| e.event_type == EventType::IntegrationCommittedV1)
                && let Some(refusal) = self.integration_resource_refusal(cas, &active)?
            {
                return self.finish_recorded_integration(cas, &active, &refusal);
            }
            return Err(error.to_string());
        }
        self.domain
            .store
            .lock()
            .expect("Task Store")
            .registered_task_review_integration(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Integration finish was not recorded".into())
    }

    fn integration_resource_refusal(
        &self,
        _cas: &Cas,
        phase: &RegisteredTaskReviewIntegration,
    ) -> Result<Option<String>, String> {
        let authority = self.authority();
        let runtime = crate::task::TaskRuntime::with_review_integration(
            self.domain.store.clone(),
            self.domain.cas,
            self.lease.clone(),
            &authority,
            self,
            phase,
        )?;
        runtime.record_integration_resource_refusal(phase)
    }

    pub(super) fn validate_integration_selection(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &TaskReviewIntegrationPhaseV1,
        evidence: &TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        if task != &self.task
            || plan != &self.plan
            || phase.plan_id != self.plan_id
            || phase.round_id != self.task.inputs["round"].artifact_ids[0]
            || phase.closing_report_event_id != evidence.closing_report.event_id
            || evidence.round != self.compiler.round().binding()
            || self.captured.compilation.graph.review_integration.is_none()
        {
            return Err("Integration differs from captured Task or Round".into());
        }
        let policy = self
            .captured
            .loaded
            .integration()
            .ok_or("No captured Integration policy")?;
        let ledger =
            review_store::LedgerProjection::from_events(&self.domain.run_id, &evidence.events, cas)
                .map_err(|e| e.to_string())?;
        match (
            self.domain.select_integration(
                policy,
                self.captured.loaded.reviewer_execution(),
                &evidence.events,
                ledger.ledger(),
            )?,
            &phase.selection,
        ) {
            (IntegrationSelection::Empty, TaskReviewIntegrationSelectionV1::Empty {}) => Ok(()),
            (
                IntegrationSelection::Conflict(event),
                TaskReviewIntegrationSelectionV1::Conflict { conflict },
            ) if event.payload == serde_json::to_value(conflict).map_err(|e| e.to_string())? => {
                Ok(())
            }
            (
                IntegrationSelection::Candidates(selected),
                TaskReviewIntegrationSelectionV1::Prepared {
                    integration_plan_id,
                    derived_snapshot_id,
                },
            ) => self
                .domain
                .read_prepared_integration(
                    policy,
                    &selected,
                    integration_plan_id,
                    derived_snapshot_id,
                )
                .map(|_| ()),
            _ => Err("Integration phase changed the deterministic Proposal selection".into()),
        }
    }
    pub(super) fn apply_integration_result(
        &self,
        cas: &Cas,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        use review_core::task::{TaskAcceptanceV1, TaskExecutionV1};
        if self.captured.compilation.graph.review_integration.is_none() {
            return Ok(());
        }
        let phase = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .registered_task_review_integration(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?;
        let Some(phase) = phase else {
            return if result.acceptance == TaskAcceptanceV1::Satisfied {
                Err("Passing Review must durably select its captured Integration phase before Task finish".into())
            } else {
                Ok(())
            };
        };
        if phase.phase().plan_id != self.plan_id || !phase.finished() {
            return Err(
                "Review Integration remains unresolved; finish its factual phase first".into(),
            );
        }
        if phase.integration_committed_event_id().is_some() {
            return Err("Committed Integration requires a full Review of the derived head before Task finish".into());
        }
        result.evidence.insert(phase.phase_id().into());
        let state = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .task_projection(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        if let Some((id, _)) = state
            .execution
            .as_ref()
            .and_then(|e| e.outputs.get(phase.node()))
        {
            result.evidence.insert(id.clone());
        }
        if let Some(id) = phase.report_id() {
            result.evidence.insert(id.into());
            let report: TaskRunReportV2 =
                serde_json::from_value(cas.get_artifact(id).map_err(|e| e.to_string())?.payload)
                    .map_err(|e| e.to_string())?;
            match &report.nodes[0].outcome {
                TaskNodeOutcomeV1::Completed { output_id } => {
                    result.evidence.insert(output_id.clone());
                    // Completed typed failing checks decline promotion. Acceptance still
                    // names only the original passing head and its original output artifacts.
                    if result.acceptance == TaskAcceptanceV1::Satisfied {
                        result.domain_conclusion = "review_passed_integration_not_promoted".into();
                    }
                }
                TaskNodeOutcomeV1::Failed { class, .. } => {
                    result.execution = if result.execution == TaskExecutionV1::Exhausted
                        || *class == review_core::task::report::TaskFailureClassV1::Resources
                    {
                        TaskExecutionV1::Exhausted
                    } else {
                        TaskExecutionV1::Incomplete
                    };
                    result.acceptance = TaskAcceptanceV1::Inconclusive;
                    result.domain_conclusion = "review_integration_incomplete".into();
                }
                TaskNodeOutcomeV1::Suppressed { .. } => {
                    return Err("Integration phase cannot be suppressed".into());
                }
            }
        }
        result.validate()
    }
}
