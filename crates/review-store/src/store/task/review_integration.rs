//! One captured post-Round sequence, on the original Task ledger and two checked log prefixes.
use super::*;
use review_core::task::campaign_review::{CAMPAIGN_REVIEW_ROUND_V1, CampaignReviewRoundV1};
use review_core::task::report::{TaskNodeOutcomeV1, TaskRunReportV1};
use review_core::task::review_integration::*;
use review_graph::task::{CompiledReviewIntegrationV1, CompiledTask};

#[derive(Debug, Clone)]
pub struct RegisteredTaskReviewIntegration {
    id: String,
    phase: TaskReviewIntegrationPhaseV1,
    compiled: CompiledReviewIntegrationV1,
    report_id: Option<String>,
    committed_event_id: Option<String>,
}
impl RegisteredTaskReviewIntegration {
    pub fn phase_id(&self) -> &str {
        &self.id
    }
    pub fn phase(&self) -> &TaskReviewIntegrationPhaseV1 {
        &self.phase
    }
    pub fn node(&self) -> &str {
        &self.compiled.node
    }
    pub fn requires_checks(&self) -> bool {
        matches!(
            self.phase.selection,
            TaskReviewIntegrationSelectionV1::Prepared { .. }
        )
    }
    pub fn finished(&self) -> bool {
        !self.requires_checks() || self.report_id.is_some()
    }
    pub fn report_id(&self) -> Option<&str> {
        self.report_id.as_deref()
    }
    pub fn integration_committed_event_id(&self) -> Option<&str> {
        self.committed_event_id.as_deref()
    }
    pub fn inputs(&self) -> Result<BTreeMap<String, task::ArtifactInputV1>, StoreError> {
        let TaskReviewIntegrationSelectionV1::Prepared {
            derived_snapshot_id,
            ..
        } = &self.phase.selection
        else {
            return Err(conflict("Integration selection has no executable sequence"));
        };
        Ok(BTreeMap::from([(
            "phase".into(),
            task::ArtifactInputV1 {
                artifact_type: TASK_REVIEW_INTEGRATION_PHASE_V1.into(),
                cardinality: review_core::PortCardinality::One,
                artifact_ids: vec![self.id.clone()],
                snapshot_id: Some(derived_snapshot_id.clone()),
            },
        )]))
    }
    pub(super) fn resolved(&self) -> Result<execution::owned::ResolvedTaskNode, StoreError> {
        Ok(execution::owned::ResolvedTaskNode {
            definition: self.compiled.definition(),
            allowance: Some(self.compiled.allowance.clone()),
            owned: None,
            expected_inputs: Some(self.inputs()?),
        })
    }
}
#[derive(Debug, Clone)]
pub struct TaskReviewIntegrationEvidence {
    pub round: CampaignReviewRoundV1,
    pub closing_report: RunEvent,
    pub events: Vec<RunEvent>,
    pub finding_set_id: String,
    pub demand_set_id: String,
    pub semantic_closure_id: String,
}
fn producer(task_id: &str) -> Result<review_core::Producer, StoreError> {
    Ok(review_core::Producer::KernelOperation {
        run_id: task_run_id(task_id)?,
        node_id: None,
        operation_id: "review-integration@1".into(),
    })
}
fn phase_snapshot(value: &TaskReviewIntegrationPhaseV1) -> Option<String> {
    match &value.selection {
        TaskReviewIntegrationSelectionV1::Prepared {
            derived_snapshot_id,
            ..
        } => Some(derived_snapshot_id.clone()),
        _ => None,
    }
}
pub fn capture_task_review_integration(
    cas: &Cas,
    value: &TaskReviewIntegrationPhaseV1,
) -> Result<String, StoreError> {
    value.validate().map_err(conflict)?;
    cas.put_artifact(
        TASK_REVIEW_INTEGRATION_PHASE_V1,
        producer(&value.task_id)?,
        value.artifact_refs(),
        phase_snapshot(value),
        serde_json::to_value(value)?,
    )
    .map(|(id, _)| id)
    .map_err(|e| StoreError::Artifact(e.to_string()))
}
pub fn read_task_review_integration(
    cas: &Cas,
    id: &str,
) -> Result<TaskReviewIntegrationPhaseV1, StoreError> {
    let frame = envelope(cas, id, TASK_REVIEW_INTEGRATION_PHASE_V1)?;
    let value: TaskReviewIntegrationPhaseV1 = serde_json::from_value(frame.payload)?;
    value.validate().map_err(conflict)?;
    if frame.producer != producer(&value.task_id)?
        || frame.input_artifacts != value.artifact_refs()
        || frame.subject_snapshot_id != phase_snapshot(&value)
    {
        return Err(conflict(
            "Integration phase changed its captured producer, references or Snapshot",
        ));
    }
    for id in value.artifact_refs() {
        cas.verify(&id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    Ok(value)
}
fn captured(
    cas: &Cas,
    state: &TaskProjection,
    phase: &TaskReviewIntegrationPhaseV1,
) -> Result<CompiledReviewIntegrationV1, StoreError> {
    if phase.task_id != state.task_id
        || phase.task_revision_id != state.revision_id
        || Some(&phase.plan_id) != state.plan_id.as_ref()
    {
        return Err(conflict(
            "Integration phase names another Task revision or plan",
        ));
    }
    let plan = plan(cas, &phase.plan_id, state)?;
    let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
    let compiled = graph
        .review_integration
        .ok_or_else(|| conflict("Task plan has no captured post-Round sequence"))?;
    compiled.validate().map_err(conflict)?;
    let frame = envelope(
        cas,
        &compiled.sequence_policy_id,
        TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1,
    )?;
    let sequence: TaskReviewCheckSequencePolicyV1 = serde_json::from_value(frame.payload)?;
    sequence.validate().map_err(conflict)?;
    let round: CampaignReviewRoundV1 = payload(cas, &phase.round_id, CAMPAIGN_REVIEW_ROUND_V1)?;
    round.validate().map_err(conflict)?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&round.campaign_manifest_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    manifest.validate().map_err(conflict)?;
    if !state.revision.inputs.values().any(|i| {
        i.artifact_type == CAMPAIGN_REVIEW_ROUND_V1 && i.artifact_ids == [phase.round_id.clone()]
    }) || frame.input_artifacts != sequence.artifact_refs()
        || frame.subject_snapshot_id.is_some()
        || sequence.authority_policy_id != plan.authority.policy_id
        || sequence.pipeline_policy_id != manifest.pipeline.artifact_id
        || !plan.dependencies.values().any(|d| {
            d.artifact_id == compiled.sequence_policy_id && d.content_digest == frame.content_id
        })
        || compiled.allowance.wall_ms_per_attempt
            != sequence
                .check_timeout_ms
                .checked_mul(sequence.ordered_check_names.len() as u64)
                .ok_or_else(|| conflict("Integration wall bound overflow"))?
    {
        return Err(conflict(
            "Integration sequence differs from captured plan, Round, policy or original allowance",
        ));
    }
    Ok(compiled)
}
impl EventStore {
    fn integration_evidence(
        &self,
        cas: &Cas,
        phase: &TaskReviewIntegrationPhaseV1,
    ) -> Result<TaskReviewIntegrationEvidence, StoreError> {
        self.integration_evidence_with_replays(cas, phase, &mut ReviewReplays::default())
    }

    fn integration_evidence_with_replays(
        &self,
        cas: &Cas,
        phase: &TaskReviewIntegrationPhaseV1,
        replays: &mut ReviewReplays,
    ) -> Result<TaskReviewIntegrationEvidence, StoreError> {
        let round: CampaignReviewRoundV1 = payload(cas, &phase.round_id, CAMPAIGN_REVIEW_ROUND_V1)?;
        round.validate().map_err(conflict)?;
        let events = replays.read(self, &round.campaign_id)?;
        let closing = events
            .iter()
            .find(|e| e.event_id == phase.closing_report_event_id)
            .ok_or_else(|| conflict("Integration closing report is absent"))?
            .clone();
        if closing.event_type != EventType::RunReportV6
            || closing.causation_id.as_deref() != Some(&round.round_event_id)
        {
            return Err(conflict(
                "Integration requires the exact Task-backed Round conclusion",
            ));
        }
        let report: review_core::RunReportPayloadV6 =
            serde_json::from_value(closing.payload.clone())?;
        report.validate().map_err(conflict)?;
        if report.verdict != (review_core::RunVerdictV3::Pass {})
            || report.task_accounting.task_id != phase.task_id
            || report.task_accounting.task_revision_id != phase.task_revision_id
            || report.task_accounting.plan_id != phase.plan_id
        {
            return Err(conflict(
                "Integration requires its original passing Task conclusion",
            ));
        }
        let task_report = super::report::round_report(cas, &report.task_accounting.task_report_id)?;
        if task_report.plan_id != phase.plan_id
            || task_report.task_revision_id != phase.task_revision_id
        {
            return Err(conflict(
                "Integration closing report changed its common report",
            ));
        }
        let (finding_set_id, demand_set_id) =
            super::review_handoff::selected_prior_sets(cas, &phase.plan_id, &round, &task_report)?;
        let closures: Vec<_> = events
            .iter()
            .filter(|e| {
                e.causation_id.as_deref() == Some(&round.round_event_id)
                    && e.event_type == EventType::SemanticClosureCheckedV1
            })
            .collect();
        let [closure] = closures.as_slice() else {
            return Err(conflict(
                "Integration needs one exact semantic-closure record",
            ));
        };
        let semantic_closure_id = closure
            .payload
            .get("record_id")
            .and_then(|v| v.as_str())
            .filter(|id| review_core::is_digest(id))
            .ok_or_else(|| conflict("Integration semantic closure is invalid"))?
            .to_string();
        Ok(TaskReviewIntegrationEvidence {
            round,
            closing_report: closing,
            events: events.as_ref().clone(),
            finding_set_id,
            demand_set_id,
            semantic_closure_id,
        })
    }
    fn checked_integration(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        phase: &TaskReviewIntegrationPhaseV1,
        authority: &dyn TaskAuthority,
        dispatching: bool,
    ) -> Result<(TaskProjection, ExecutionPlanV1), StoreError> {
        let state = self
            .task_projection(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let time = now()?;
        state.check_lease(&TaskTransitionV1 {
            writer: lease.writer.clone(),
            epoch: lease.epoch,
            now_unix_ms: time,
            change: TaskChangeV1::Resumed {},
        })?;
        if !state.admitted || state.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict("Integration requires an admitted running Task"));
        }
        if dispatching
            && state
                .execution
                .as_ref()
                .is_some_and(|e| e.budget.breached())
        {
            return Err(conflict(
                "Integration cannot dispatch or promote after Task resource breach",
            ));
        }
        let plan = if dispatching {
            self.current_task_plan(cas, &state, authority, time)?
        } else {
            let p = self.authorized_plan(cas, &state, &phase.plan_id, authority)?;
            state.check_plan_decision(cas, time)?;
            if let Some(d) = state.decisions.get(&phase.plan_id) {
                authority
                    .authorization_current(&d.value)
                    .map_err(conflict)?;
            }
            p
        };
        captured(cas, &state, phase)?;
        super::review_round::ReviewRoundFence::capture_closed(
            cas,
            &state.revision,
            &phase.closing_report_event_id,
        )?
        .validate(&self.conn)?;
        Ok((state, plan))
    }
    pub fn check_task_review_integration_current(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        registered: &RegisteredTaskReviewIntegration,
        authority: &dyn TaskAuthority,
        dispatching: bool,
    ) -> Result<ExecutionPlanV1, StoreError> {
        let (state, plan) =
            self.checked_integration(cas, lease, &registered.phase, authority, dispatching)?;
        let active = state
            .execution
            .as_ref()
            .and_then(|e| e.active_review_integration())
            .ok_or_else(|| conflict("Integration is not activated"))?;
        if active.id != registered.id
            || active.phase != registered.phase
            || (dispatching && active.finished())
        {
            return Err(conflict("Integration handle is stale or sealed"));
        }
        Ok(plan)
    }
    /// Identify only factual original-resource loss. Writer, plan, approval, Round and CAS
    /// failures remain errors and must never be relabeled as an exhausted check sequence.
    pub fn task_review_integration_resource_refusal(
        &self,
        cas: &Cas,
        lease: &TaskLease,
        registered: &RegisteredTaskReviewIntegration,
        authority: &dyn TaskAuthority,
    ) -> Result<Option<String>, StoreError> {
        let (state, _) =
            self.checked_integration(cas, lease, &registered.phase, authority, false)?;
        let active = state
            .execution
            .as_ref()
            .and_then(|e| e.active_review_integration())
            .ok_or_else(|| conflict("Integration is not activated"))?;
        if active.id != registered.id || active.phase != registered.phase || active.finished() {
            return Err(conflict(
                "Integration resource observation names a stale or sealed phase",
            ));
        }
        let execution = state.execution.as_ref().expect("active phase");
        Ok(
            resource_refusal(execution, state.revision.limits.deadline_unix_ms, now()?)
                .map(str::to_owned),
        )
    }

    pub fn task_review_integration_evidence(
        &self,
        cas: &Cas,
        registered: &RegisteredTaskReviewIntegration,
    ) -> Result<TaskReviewIntegrationEvidence, StoreError> {
        if read_task_review_integration(cas, &registered.id)? != registered.phase {
            return Err(conflict("Integration handle changed its artifact"));
        }
        self.integration_evidence(cas, &registered.phase)
    }
    pub fn registered_task_review_integration(
        &self,
        cas: &Cas,
        task_id: &str,
    ) -> Result<Option<RegisteredTaskReviewIntegration>, StoreError> {
        Ok(self.task_projection(cas, task_id)?.and_then(|s| {
            s.execution
                .as_ref()
                .and_then(|e| e.active_review_integration())
                .cloned()
        }))
    }
    pub fn select_task_review_integration(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        phase_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RegisteredTaskReviewIntegration, StoreError> {
        let phase = read_task_review_integration(cas, phase_id)?;
        let (state, plan) = self.checked_integration(cas, lease, &phase, authority, true)?;
        if let Some(active) = state
            .execution
            .as_ref()
            .and_then(|e| e.active_review_integration())
        {
            if active.id == phase_id && active.phase == phase {
                return Ok(active.clone());
            }
            return Err(conflict(
                "This Round already selected its Integration phase",
            ));
        }
        if state
            .execution
            .as_ref()
            .is_some_and(|e| !e.pending_attempts().is_empty())
        {
            return Err(conflict(
                "Integration cannot activate with pending common Attempts",
            ));
        }
        let evidence = self.integration_evidence(cas, &phase)?;
        authority
            .validate_review_integration_selection(cas, &state.revision, &plan, &phase, &evidence)
            .map_err(conflict)?;
        let events = selection_events(cas, &phase, &evidence)?;
        self.append_integration_atomic(
            cas,
            &state,
            &plan,
            &evidence,
            TaskChangeV1::ReviewIntegrationSelected {
                phase_id: phase_id.into(),
            },
            &events,
        )?;
        self.registered_task_review_integration(cas, &lease.task_id)?
            .ok_or_else(|| conflict("Integration selection was not replayed"))
    }
    pub fn finish_task_review_integration(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        registered: &RegisteredTaskReviewIntegration,
        phase_report_id: &str,
        events: &[NewEvent],
        authority: &dyn TaskAuthority,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let (state, plan) =
            self.checked_integration(cas, lease, &registered.phase, authority, false)?;
        let active = state
            .execution
            .as_ref()
            .and_then(|e| e.active_review_integration())
            .ok_or_else(|| conflict("Integration is not activated"))?;
        if active.id != registered.id {
            return Err(conflict("Integration finish names another phase"));
        }
        if active.report_id.as_deref() == Some(phase_report_id) {
            return Ok(Vec::new());
        }
        if active.finished() {
            return Err(conflict("Integration phase is already sealed"));
        }
        let report = super::report::phase_report(cas, phase_report_id)?;
        if report.phase_id.as_deref() != Some(registered.id.as_str())
            || report.plan_id != registered.phase.plan_id
            || report.task_revision_id != registered.phase.task_revision_id
            || report.nodes[0].node != registered.node()
            || !state.run_reports.iter().any(|id| id == phase_report_id)
        {
            return Err(conflict(
                "Integration completion has no recorded exact phase report",
            ));
        }
        let evidence = self.integration_evidence(cas, &registered.phase)?;
        validate_completion(cas, &state, registered, &report, events, &evidence)?;
        authority
            .validate_review_integration_completion(
                cas,
                &state.revision,
                &plan,
                &registered.phase,
                &report,
                events,
                &evidence,
            )
            .map_err(conflict)?;
        if events
            .last()
            .is_some_and(|e| e.event_type == EventType::IntegrationCommittedV1)
        {
            self.checked_integration(cas, lease, &registered.phase, authority, true)?;
        }
        let committed = events
            .last()
            .filter(|e| e.event_type == EventType::IntegrationCommittedV1)
            .map(|_| {
                super::super::derive_event_id(
                    &evidence.round.campaign_id,
                    (evidence.events.len() + events.len() - 1) as i64,
                )
            });
        self.append_integration_atomic(
            cas,
            &state,
            &plan,
            &evidence,
            TaskChangeV1::ReviewIntegrationFinished {
                phase_id: registered.id.clone(),
                report_id: phase_report_id.into(),
                integration_committed_event_id: committed,
            },
            events,
        )
    }
}
fn selection_events(
    cas: &Cas,
    phase: &TaskReviewIntegrationPhaseV1,
    evidence: &TaskReviewIntegrationEvidence,
) -> Result<Vec<NewEvent>, StoreError> {
    match &phase.selection {
        TaskReviewIntegrationSelectionV1::Empty {} => Ok(Vec::new()),
        TaskReviewIntegrationSelectionV1::Conflict { conflict: payload } => Ok(vec![
            NewEvent::new(
                EventType::IntegrationConflictV1,
                serde_json::to_value(payload)?,
            )
            .correlating(evidence.round.subject_id.clone()),
        ]),
        TaskReviewIntegrationSelectionV1::Prepared {
            integration_plan_id,
            derived_snapshot_id,
        } => {
            let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
                cas.get_json(integration_plan_id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?,
            )?;
            plan.validate().map_err(conflict)?;
            Ok(vec![
                NewEvent::new(
                    EventType::IntegrationPreparedV1,
                    serde_json::to_value(review_core::IntegrationPreparedPayloadV1 {
                        batch_id: format!("integration-{}", &integration_plan_id[7..23]),
                        plan_artifact_id: integration_plan_id.clone(),
                        derived_snapshot_id: derived_snapshot_id.clone(),
                    })?,
                )
                .correlating(evidence.round.subject_id.clone())
                .referencing(vec![
                    integration_plan_id.clone(),
                    derived_snapshot_id.clone(),
                    plan.derived_manifest_artifact_id,
                ]),
            ])
        }
    }
}
fn selected_checks(
    cas: &Cas,
    registered: &RegisteredTaskReviewIntegration,
    report: &TaskRunReportV1,
) -> Result<Option<review_core::IntegrationChecksV1>, StoreError> {
    if report.phase_id.as_deref() != Some(registered.id.as_str())
        || report.plan_id != registered.phase.plan_id
        || report.task_revision_id != registered.phase.task_revision_id
        || report.nodes[0].node != registered.node()
        || !registered.requires_checks()
    {
        return Err(conflict(
            "Integration report changed its exact activated phase",
        ));
    }
    let TaskNodeOutcomeV1::Completed { output_id } = &report.nodes[0].outcome else {
        return Ok(None);
    };
    let output = execution::output(cas, output_id)?;
    let port = output
        .outputs
        .get("checks")
        .ok_or_else(|| conflict("Integration output lacks checks"))?;
    let [typed_id] = port.artifact_ids.as_slice() else {
        return Err(conflict("Integration output is not singular"));
    };
    let checks: review_core::IntegrationChecksV1 =
        payload(cas, typed_id, review_core::contract::INTEGRATION_CHECKS_V1)?;
    checks.validate().map_err(conflict)?;
    let TaskReviewIntegrationSelectionV1::Prepared {
        derived_snapshot_id,
        ..
    } = &registered.phase.selection
    else {
        unreachable!()
    };
    let sequence: TaskReviewCheckSequencePolicyV1 = payload(
        cas,
        &registered.compiled.sequence_policy_id,
        TASK_REVIEW_CHECK_SEQUENCE_POLICY_V1,
    )?;
    sequence.validate().map_err(conflict)?;
    if checks.derived_snapshot_id != *derived_snapshot_id
        || !checks
            .checks
            .iter()
            .map(|c| &c.name)
            .eq(sequence.ordered_check_names.iter())
    {
        return Err(conflict(
            "Integration checks changed captured Snapshot or complete order",
        ));
    }
    Ok(Some(checks))
}
fn resource_refusal(
    execution: &execution::TaskExecutionProjection,
    deadline: u64,
    time: u64,
) -> Option<&'static str> {
    if execution.budget.breached() {
        Some(
            "Integration promotion refused because the original Task budget was breached; completed checks remain recorded",
        )
    } else if time >= deadline {
        Some(
            "Integration promotion refused because the original Task deadline expired; completed checks remain recorded",
        )
    } else {
        None
    }
}
fn validate_resource_recovery(
    execution: &execution::TaskExecutionProjection,
    node: &str,
    report: &TaskRunReportV1,
    deadline: u64,
    time: u64,
) -> Result<(), StoreError> {
    if execution.outputs.contains_key(node)
        && matches!(
            &report.nodes[0].outcome,
            TaskNodeOutcomeV1::Failed {
                class: task::report::TaskFailureClassV1::Resources,
                ..
            }
        )
        && resource_refusal(execution, deadline, time).is_none()
    {
        return Err(conflict(
            "Completed Integration checks cannot be relabeled as resource loss without original deadline or budget breach",
        ));
    }
    Ok(())
}
fn validate_completion(
    cas: &Cas,
    state: &TaskProjection,
    registered: &RegisteredTaskReviewIntegration,
    report: &TaskRunReportV1,
    events: &[NewEvent],
    evidence: &TaskReviewIntegrationEvidence,
) -> Result<(), StoreError> {
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Integration has no common execution"))?;
    if !execution.pending_attempts().is_empty() {
        return Err(conflict(
            "Integration completion has pending common Attempts",
        ));
    }
    if let TaskNodeOutcomeV1::Completed { output_id } = &report.nodes[0].outcome {
        if execution.outputs.get(registered.node()).map(|(id, _)| id) != Some(output_id) {
            return Err(conflict(
                "Integration checks lack the selected common output",
            ));
        }
    }
    validate_resource_recovery(
        execution,
        registered.node(),
        report,
        state.revision.limits.deadline_unix_ms,
        now()?,
    )?;
    validate_canonical_completion(cas, registered, report, events, evidence)
}
fn validate_canonical_completion(
    cas: &Cas,
    registered: &RegisteredTaskReviewIntegration,
    report: &TaskRunReportV1,
    events: &[NewEvent],
    evidence: &TaskReviewIntegrationEvidence,
) -> Result<(), StoreError> {
    let Some(checks) = selected_checks(cas, registered, report)? else {
        if !events.is_empty() {
            return Err(conflict(
                "Failed Integration execution cannot publish checks or promote a head",
            ));
        }
        return Ok(());
    };
    let TaskReviewIntegrationSelectionV1::Prepared {
        integration_plan_id,
        derived_snapshot_id,
    } = &registered.phase.selection
    else {
        unreachable!()
    };
    let first = events
        .first()
        .ok_or_else(|| conflict("Selected checks require their canonical publication"))?;
    if first.event_type != EventType::IntegrationChecksCompletedV1 {
        return Err(conflict(
            "Integration must first publish the exact complete checks",
        ));
    }
    let published: review_core::IntegrationChecksCompletedPayloadV1 =
        serde_json::from_value(first.payload.clone())?;
    let raw: review_core::IntegrationChecksV1 = serde_json::from_value(
        cas.get_json(&published.checks_artifact_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    if raw != checks
        || published.passed != checks.passed()
        || checks.derived_snapshot_id != *derived_snapshot_id
        || published.batch_id != format!("integration-{}", &integration_plan_id[7..23])
    {
        return Err(conflict(
            "Canonical Integration checks differ from the selected common output",
        ));
    }
    let mut check_refs = vec![
        published.checks_artifact_id.clone(),
        derived_snapshot_id.clone(),
    ];
    check_refs.extend(checks.checks.iter().map(|c| c.result_artifact_id.clone()));
    check_canonical_metadata(first, derived_snapshot_id, &check_refs)?;
    if !checks.passed() {
        if events.len() != 1 {
            return Err(conflict("Failed checks cannot promote the derived head"));
        }
        return Ok(());
    }
    let Some(last) = events
        .last()
        .filter(|event| event.event_type == EventType::IntegrationCommittedV1)
    else {
        return Err(conflict(
            "Passing Integration checks require the exact atomic commit",
        ));
    };
    if events.len() < 2
        || events[1..events.len() - 1]
            .iter()
            .any(|e| e.event_type != EventType::ChangeAttestedV1)
    {
        return Err(conflict(
            "Integration commit contains an unexpected transition",
        ));
    }
    let commit: review_core::IntegrationCommittedPayloadV1 =
        serde_json::from_value(last.payload.clone())?;
    if commit.expected_finding_set_id != evidence.finding_set_id
        || commit.expected_demand_set_id != evidence.demand_set_id
        || commit.semantic_closure_id != evidence.semantic_closure_id
        || commit.prior_subject_id != evidence.round.subject_id
        || commit.prior_snapshot_id != evidence.round.head_snapshot_id
        || commit.derived_snapshot_id != *derived_snapshot_id
        || commit.batch_id != published.batch_id
    {
        return Err(conflict(
            "Integration commit changed selected lineage or its exact checked Snapshot",
        ));
    }
    let mut commit_refs = vec![
        integration_plan_id.clone(),
        published.checks_artifact_id.clone(),
        commit.derived_subject_id.clone(),
        derived_snapshot_id.clone(),
        evidence.finding_set_id.clone(),
        evidence.demand_set_id.clone(),
        evidence.semantic_closure_id.clone(),
    ];
    commit_refs.extend(commit.attestation_ids.iter().cloned());
    check_canonical_metadata(last, &evidence.round.subject_id, &commit_refs)?;
    validate_task_attestations(
        cas,
        integration_plan_id,
        &commit,
        &events[1..events.len() - 1],
        evidence,
    )?;
    Ok(())
}
fn check_canonical_metadata(
    event: &NewEvent,
    correlation: &str,
    refs: &[String],
) -> Result<(), StoreError> {
    if event.node_id.is_some()
        || event.attempt_id.is_some()
        || event.causation_id.is_some()
        || event.correlation_id.as_deref() != Some(correlation)
        || event.artifact_refs != refs
    {
        return Err(conflict(
            "Task Integration canonical event changed its authority or exact references",
        ));
    }
    Ok(())
}
fn validate_task_attestations(
    cas: &Cas,
    plan_id: &str,
    commit: &review_core::IntegrationCommittedPayloadV1,
    events: &[NewEvent],
    evidence: &TaskReviewIntegrationEvidence,
) -> Result<(), StoreError> {
    let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
        cas.get_json(plan_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&evidence.round.subject_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    let derived: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&commit.derived_subject_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    derived.validate().map_err(conflict)?;
    if derived.kind != subject.kind
        || derived.base_snapshot_id != subject.base_snapshot_id
        || derived.head_snapshot_id != commit.derived_snapshot_id
    {
        return Err(conflict(
            "Integration changed the original Subject kind or Campaign Base",
        ));
    }
    let end = evidence
        .events
        .iter()
        .position(|e| {
            e.event_type == EventType::IntegrationChecksCompletedV1
                && e.payload.get("batch_id").and_then(|v| v.as_str()) == Some(&commit.batch_id)
        })
        .unwrap_or(evidence.events.len());
    let prefix = &evidence.events[..end];
    let ledger = crate::LedgerProjection::from_events(&evidence.round.campaign_id, prefix, cas)?;
    let accepted = prefix
        .iter()
        .filter(|e| {
            e.event_type == EventType::ProposalAcceptedV1
                && e.causation_id.as_deref() == Some(&evidence.round.round_event_id)
        })
        .map(|e| {
            serde_json::from_value::<review_core::ProposalAcceptedPayloadV1>(e.payload.clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    #[derive(Default)]
    struct AttestationInputs {
        paths: BTreeSet<String>,
        evidence_ids: BTreeSet<String>,
        inputs: BTreeSet<String>,
    }
    let mut by_finding: BTreeMap<String, AttestationInputs> = BTreeMap::new();
    for candidate in &plan.candidates {
        let records: Vec<_> = accepted
            .iter()
            .filter(|p| p.proposal_id == candidate.proposal_id)
            .collect();
        let [proposal] = records.as_slice() else {
            return Err(conflict(
                "Integration attestation has no exact accepted Proposal",
            ));
        };
        for finding in &candidate.finding_ids {
            let entry = by_finding.entry(finding.clone()).or_default();
            entry.paths.extend(candidate.paths.iter().cloned());
            entry
                .evidence_ids
                .extend(candidate.evidence_ids.iter().cloned());
            entry.inputs.insert(proposal.proposal_artifact_id.clone());
        }
    }
    if events.len() != by_finding.len() || commit.attestation_ids.len() != events.len() {
        return Err(conflict(
            "Integration requires exactly one automatic attestation for every selected Finding",
        ));
    }
    for (
        (
            (
                finding,
                AttestationInputs {
                    paths,
                    evidence_ids,
                    mut inputs,
                },
            ),
            event,
        ),
        expected_id,
    ) in by_finding
        .into_iter()
        .zip(events)
        .zip(&commit.attestation_ids)
    {
        let recorded: review_core::RecordedArtifactPayloadV1 =
            serde_json::from_value(event.payload.clone())?;
        if recorded.artifact_id != *expected_id {
            return Err(conflict("Integration changed canonical attestation order"));
        }
        check_canonical_metadata(event, &finding, std::slice::from_ref(expected_id))?;
        let frame = envelope(
            cas,
            expected_id,
            review_core::contract::CHANGE_ATTESTATION_V1,
        )?;
        let actual: review_core::ChangeAttestationV1 = serde_json::from_value(frame.payload)?;
        let expected = review_core::ChangeAttestationV1 {
            finding_id: finding.clone(),
            expected_finding_view_id: ledger
                .ledger()
                .finding_view_id(&finding)
                .ok_or_else(|| conflict("Integration Finding disappeared before attestation"))?,
            subject_id: evidence.round.subject_id.clone(),
            change_set_id: subject.change_set_id.clone(),
            changed_regions: paths
                .into_iter()
                .map(|path| review_core::ChangedRegionV1 {
                    path,
                    start_line: None,
                    end_line: None,
                })
                .collect(),
            actor: "review.kernel/automatic-integration@1".into(),
            reason: format!("checked Integration batch {}", commit.batch_id),
            evidence_ids: evidence_ids.iter().cloned().collect(),
        };
        inputs.extend(evidence_ids);
        let producer = review_core::Producer::KernelOperation {
            run_id: evidence.round.campaign_id.clone(),
            node_id: None,
            operation_id: format!("automatic-integration:{}:{finding}", commit.batch_id),
        };
        if actual != expected
            || frame.producer != producer
            || frame.subject_snapshot_id.as_deref() != Some(&evidence.round.head_snapshot_id)
            || frame.input_artifacts != inputs.into_iter().collect::<Vec<_>>()
        {
            return Err(conflict(
                "Integration attestation differs from selected candidates and the exact current Finding view",
            ));
        }
    }
    Ok(())
}
impl TaskProjection {
    pub(super) fn apply_review_integration(
        &mut self,
        cas: &Cas,
        change: &TaskChangeV1,
        time: u64,
    ) -> Result<(), StoreError> {
        if !self.admitted || self.phase != (TaskPhaseV1::Running {}) {
            return Err(conflict("Integration requires admitted running execution"));
        }
        self.check_plan_decision(cas, time)?;
        let phase_id = match change {
            TaskChangeV1::ReviewIntegrationSelected { phase_id }
            | TaskChangeV1::ReviewIntegrationFinished { phase_id, .. } => phase_id,
            _ => return Err(conflict("Expected Integration phase transition")),
        };
        let phase = read_task_review_integration(cas, phase_id)?;
        let compiled = captured(cas, self, &phase)?;
        if matches!(change, TaskChangeV1::ReviewIntegrationSelected { .. }) {
            self.check_approval(cas, time)?;
            if self.execution.as_ref().is_some_and(|e| e.budget.breached()) {
                return Err(conflict(
                    "Integration activation follows Task resource breach",
                ));
            }
        }
        if self.execution.is_none() {
            self.execution = Some(execution::TaskExecutionProjection::new(cas, self)?);
        }
        let execution = self.execution.as_mut().expect("initialized");
        match change {
            TaskChangeV1::ReviewIntegrationSelected { .. } => {
                if execution.active_review_integration.is_some()
                    || !execution.pending_attempts().is_empty()
                {
                    return Err(conflict(
                        "Integration selection is duplicated or has pending work",
                    ));
                }
                execution.review_integrations.insert(
                    phase_id.clone(),
                    RegisteredTaskReviewIntegration {
                        id: phase_id.clone(),
                        phase,
                        compiled,
                        report_id: None,
                        committed_event_id: None,
                    },
                );
                execution.active_review_integration = Some(phase_id.clone());
            }
            TaskChangeV1::ReviewIntegrationFinished {
                report_id,
                integration_committed_event_id,
                ..
            } => {
                if execution.active_review_integration.as_ref() != Some(phase_id)
                    || !execution.pending_attempts().is_empty()
                    || !self.run_reports.contains(report_id)
                {
                    return Err(conflict(
                        "Integration completion has no active quiescent reported phase",
                    ));
                }
                let active = execution
                    .review_integrations
                    .get(phase_id)
                    .ok_or_else(|| conflict("Unknown Integration phase"))?;
                if active.finished() {
                    return Err(conflict("Integration phase is already sealed"));
                }
                let report = super::report::phase_report(cas, report_id)?;
                let checks = selected_checks(cas, active, &report)?;
                validate_resource_recovery(
                    execution,
                    active.node(),
                    &report,
                    self.revision.limits.deadline_unix_ms,
                    time,
                )?;
                if let TaskNodeOutcomeV1::Completed { output_id } = &report.nodes[0].outcome {
                    if execution.outputs.get(active.node()).map(|(id, _)| id) != Some(output_id) {
                        return Err(conflict(
                            "Integration replay lost the selected common checks",
                        ));
                    }
                }
                if integration_committed_event_id.is_some()
                    != checks.as_ref().is_some_and(|c| c.passed())
                {
                    return Err(conflict(
                        "Integration replay commit disagrees with selected checks",
                    ));
                }
                if integration_committed_event_id.is_some() {
                    if execution.budget.breached() {
                        return Err(conflict(
                            "Integration promotion follows Task resource breach",
                        ));
                    }
                    self.check_approval(cas, time)?;
                }
                let execution = self.execution.as_mut().expect("initialized");
                let active = execution
                    .review_integrations
                    .get_mut(phase_id)
                    .expect("checked");
                active.report_id = Some(report_id.clone());
                active.committed_event_id = integration_committed_event_id.clone();
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
impl execution::TaskExecutionProjection {
    pub fn active_review_integration(&self) -> Option<&RegisteredTaskReviewIntegration> {
        self.active_review_integration
            .as_ref()
            .and_then(|id| self.review_integrations.get(id))
    }
    pub fn review_integrations(&self) -> Vec<RegisteredTaskReviewIntegration> {
        self.review_integrations.values().cloned().collect()
    }
    pub(super) fn check_integration_node(&self, node: &str) -> Result<(), StoreError> {
        if let Some(active) = self.active_review_integration() {
            if active.node() != node || active.finished() {
                return Err(conflict(
                    "Closed Review Round permits only its active unsealed Integration node",
                ));
            }
        } else if self
            .graph
            .review_integration
            .as_ref()
            .is_some_and(|p| p.node == node)
        {
            return Err(conflict("Dormant Integration node has not been activated"));
        }
        Ok(())
    }
}
impl EventStore {
    fn append_integration_atomic(
        &mut self,
        cas: &Cas,
        state: &TaskProjection,
        plan: &ExecutionPlanV1,
        evidence: &TaskReviewIntegrationEvidence,
        change: TaskChangeV1,
        review_events: &[NewEvent],
    ) -> Result<Vec<RunEvent>, StoreError> {
        let dispatching = matches!(
            change,
            TaskChangeV1::ReviewIntegrationSelected { .. }
                | TaskChangeV1::ReviewIntegrationFinished {
                    integration_committed_event_id: Some(_),
                    ..
                }
        );
        let time = now()?;
        let transition = TaskTransitionV1 {
            writer: state.writer.clone(),
            epoch: state.epoch,
            now_unix_ms: time,
            change,
        };
        let (kind, value) = super::review_handoff::encode_transition(&transition)?;
        let event = NewEvent::new(kind, value.clone()).referencing(super::references(
            cas,
            &transition.change,
            Some(state),
        )?);
        let run_id = task_run_id(&state.task_id)?;
        let first = state.next_sequence;
        let mut candidate = state.clone();
        candidate.apply(
            cas,
            &RunEvent {
                run_id: run_id.clone(),
                event_id: super::super::derive_event_id(&run_id, first as i64),
                sequence: first,
                event_type: kind,
                occurred_at: event.occurred_at.clone(),
                node_id: None,
                attempt_id: None,
                causation_id: None,
                correlation_id: None,
                artifact_refs: event.artifact_refs.clone(),
                payload: value,
            },
            &transition,
        )?;
        let task_prepared = self.prepare_event_artifacts(cas, std::slice::from_ref(&event))?;
        let review_prepared = self.prepare_event_artifacts(cas, review_events)?;
        let approval_until = state
            .plan_id
            .as_ref()
            .and_then(|id| state.decisions.get(id))
            .map_or(u64::MAX, |d| d.valid_until);
        let valid_until = state.lease_until.min(approval_until).min(if dispatching {
            plan.limits.deadline_unix_ms
        } else {
            u64::MAX
        });
        let fence = super::review_round::ReviewRoundFence::capture_closed(
            cas,
            &state.revision,
            &evidence.closing_report.event_id,
        )?;
        let campaign = &evidence.round.campaign_id;
        let review_first = evidence.events.len() as i64;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task_current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence)+1,0) FROM events WHERE run_id=?1",
            [&run_id],
            |r| r.get(0),
        )?;
        let review_current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence)+1,0) FROM events WHERE run_id=?1",
            [campaign],
            |r| r.get(0),
        )?;
        if task_current != first as i64 || review_current != review_first || now()? >= valid_until {
            return Err(conflict(
                "Integration publication lost its Task/Review prefix or current authority",
            ));
        }
        fence.validate(&tx)?;
        let mut appended = Vec::new();
        // Checks become visible to the existing commit validator inside this transaction;
        // attestation + commit remain one validation batch and rollback with every other row.
        let cut = usize::from(
            review_events
                .first()
                .is_some_and(|e| e.event_type == EventType::IntegrationChecksCompletedV1),
        );
        for chunk in [&review_events[..cut], &review_events[cut..]] {
            if chunk.is_empty() {
                continue;
            }
            let at = review_first + appended.len() as i64;
            super::super::validate_campaign_transition(
                &tx,
                cas,
                campaign,
                chunk,
                at,
                &review_prepared,
            )?;
            appended.extend(super::super::insert_events(&tx, campaign, chunk, at)?);
        }
        // The candidate state and every referenced artifact were validated before BEGIN;
        // exact prefixes and authority are rechecked above under the SQLite writer lock.
        let _ = task_prepared;
        appended.extend(super::super::insert_events(
            &tx,
            &run_id,
            &[event],
            first as i64,
        )?);
        tx.commit()?;
        Ok(appended)
    }
}

pub(in crate::store) fn canonical_task_integration_views(
    cas: &Cas,
    report: &review_core::RunReportPayloadV6,
) -> Result<(String, String), StoreError> {
    let revision = revision(cas, &report.task_accounting.task_revision_id)?;
    let input = revision
        .inputs
        .values()
        .find(|i| i.artifact_type == CAMPAIGN_REVIEW_ROUND_V1)
        .ok_or_else(|| conflict("Task Integration report lacks captured Round"))?;
    let [id] = input.artifact_ids.as_slice() else {
        return Err(conflict("Task Integration has ambiguous Round authority"));
    };
    let round: CampaignReviewRoundV1 = payload(cas, id, CAMPAIGN_REVIEW_ROUND_V1)?;
    let task_report = super::report::round_report(cas, &report.task_accounting.task_report_id)?;
    super::review_handoff::selected_prior_sets(
        cas,
        &report.task_accounting.plan_id,
        &round,
        &task_report,
    )
}
pub(super) fn validate_handoff(
    store: &EventStore,
    cas: &Cas,
    handoff: &task::review_handoff::TaskReviewHandoffV1,
    event: &RunEvent,
    next: &CampaignReviewRoundV1,
    next_started: &review_core::RoundStartedPayloadV1,
    replays: &mut ReviewReplays,
) -> Result<(), StoreError> {
    let task::review_handoff::TaskReviewHandoffEvidenceV1::IntegratedRound {
        report_event_id,
        phase_id,
        integration_committed_event_id,
    } = &handoff.evidence
    else {
        return Err(conflict("Expected integrated handoff"));
    };
    let phase = read_task_review_integration(cas, phase_id)?;
    if phase.task_id != handoff.task_id
        || phase.task_revision_id != handoff.predecessor_revision_id
        || phase.plan_id != handoff.predecessor_plan_id
        || phase.round_id != handoff.predecessor_round_id
        || phase.closing_report_event_id != *report_event_id
        || event.event_id != *integration_committed_event_id
        || event.event_type != EventType::IntegrationCommittedV1
    {
        return Err(conflict(
            "Integrated handoff changed its exact closed phase",
        ));
    }
    let evidence = store.integration_evidence_with_replays(cas, &phase, replays)?;
    if evidence.closing_report.sequence >= event.sequence {
        return Err(conflict("Integration commit precedes its passing report"));
    }
    let committed: review_core::IntegrationCommittedPayloadV1 =
        serde_json::from_value(event.payload.clone())?;
    committed.validate().map_err(conflict)?;
    let TaskReviewIntegrationSelectionV1::Prepared {
        integration_plan_id,
        derived_snapshot_id,
    } = &phase.selection
    else {
        return Err(conflict("Integrated handoff has no prepared sequence"));
    };
    if committed.batch_id != format!("integration-{}", &integration_plan_id[7..23])
        || committed.derived_snapshot_id != *derived_snapshot_id
        || committed.prior_subject_id != evidence.round.subject_id
        || committed.prior_snapshot_id != evidence.round.head_snapshot_id
        || committed.derived_subject_id != next.subject_id
        || committed.derived_snapshot_id != next.head_snapshot_id
        || committed.expected_finding_set_id != evidence.finding_set_id
        || committed.expected_demand_set_id != evidence.demand_set_id
        || committed.semantic_closure_id != evidence.semantic_closure_id
        || next_started.prior_demand_set_id != evidence.demand_set_id
    {
        return Err(conflict(
            "Integrated handoff changed its checked head or selected lineage",
        ));
    }
    let task_events = replays.read(store, &task_run_id(&handoff.task_id)?)?;
    let finishes:Vec<_>=task_events.iter().map(super::read_task_transition).collect::<Result<Vec<_>,_>>()?.into_iter().filter(|t|matches!(&t.change,TaskChangeV1::ReviewIntegrationFinished{phase_id:p,integration_committed_event_id:Some(id),..} if p==phase_id && id==integration_committed_event_id)).collect();
    if finishes.len() != 1 {
        return Err(conflict(
            "Integrated handoff requires one protected common phase completion",
        ));
    }
    Ok(())
}
pub(super) fn validate_cached(
    store: &EventStore,
    cas: &Cas,
    state: &TaskProjection,
    replays: &mut ReviewReplays,
) -> Result<(), StoreError> {
    let Some(execution) = &state.execution else {
        return Ok(());
    };
    for phase in execution.review_integrations.values() {
        if read_task_review_integration(cas, &phase.id)? != phase.phase {
            return Err(conflict("Cached Integration phase changed identity"));
        }
        let evidence = store.integration_evidence_with_replays(cas, &phase.phase, replays)?;
        let original_plan: ExecutionPlanV1 =
            payload(cas, &phase.phase.plan_id, task::EXECUTION_PLAN_V1)?;
        let original_graph: CompiledTask =
            payload(cas, &original_plan.compiled_graph_id, "af/CompiledTask@1")?;
        if original_graph.review_integration.as_ref() != Some(&phase.compiled) {
            return Err(conflict(
                "Integration lost its original captured dormant allowance",
            ));
        }
        for expected in selection_events(cas, &phase.phase, &evidence)? {
            let matching: Vec<_> = evidence
                .events
                .iter()
                .filter(|e| {
                    e.sequence > evidence.closing_report.sequence
                        && e.event_type == expected.event_type
                        && e.payload == expected.payload
                })
                .collect();
            let [actual] = matching.as_slice() else {
                return Err(conflict(
                    "Integration selection lost its exact canonical publication",
                ));
            };
            if actual.node_id != expected.node_id
                || actual.attempt_id != expected.attempt_id
                || actual.causation_id != expected.causation_id
                || actual.correlation_id != expected.correlation_id
                || actual.artifact_refs != expected.artifact_refs
            {
                return Err(conflict(
                    "Integration selection changed canonical authority or references",
                ));
            }
        }
        if let Some(report_id) = &phase.report_id {
            let report = super::report::phase_report(cas, report_id)?;
            let checks = selected_checks(cas, phase, &report)?;
            if phase.committed_event_id.is_some() != checks.as_ref().is_some_and(|c| c.passed()) {
                return Err(conflict(
                    "Cached Integration commit disagrees with its selected checks",
                ));
            }
            let TaskReviewIntegrationSelectionV1::Prepared {
                integration_plan_id,
                ..
            } = &phase.phase.selection
            else {
                return Err(conflict("Completed Integration was not prepared"));
            };
            let batch = format!("integration-{}", &integration_plan_id[7..23]);
            let checks_events: Vec<_> = evidence
                .events
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.event_type == EventType::IntegrationChecksCompletedV1
                        && e.payload.get("batch_id").and_then(|v| v.as_str()) == Some(&batch)
                })
                .collect();
            let canonical: Vec<_> = if checks.is_some() {
                let [(start, _)] = checks_events.as_slice() else {
                    return Err(conflict(
                        "Integration completion requires one canonical check publication",
                    ));
                };
                let end = if let Some(commit) = &phase.committed_event_id {
                    evidence
                        .events
                        .iter()
                        .position(|e| e.event_id == *commit)
                        .filter(|end| end > start)
                        .ok_or_else(|| conflict("Integration completion lost its exact commit"))?
                } else {
                    *start
                };
                evidence.events[*start..=end].iter().collect()
            } else {
                if !checks_events.is_empty() {
                    return Err(conflict(
                        "Failed Integration execution gained canonical checks",
                    ));
                }
                Vec::new()
            };
            let events: Vec<_> = canonical
                .iter()
                .map(|e| NewEvent {
                    event_type: e.event_type,
                    occurred_at: e.occurred_at.clone(),
                    node_id: e.node_id.clone(),
                    attempt_id: e.attempt_id.clone(),
                    causation_id: e.causation_id.clone(),
                    correlation_id: e.correlation_id.clone(),
                    artifact_refs: e.artifact_refs.clone(),
                    payload: e.payload.clone(),
                })
                .collect();
            validate_canonical_completion(cas, phase, &report, &events, &evidence)?;
            if canonical
                .last()
                .filter(|e| e.event_type == EventType::IntegrationCommittedV1)
                .map(|e| e.event_id.as_str())
                != phase.committed_event_id.as_deref()
                || canonical
                    .iter()
                    .any(|e| e.sequence <= evidence.closing_report.sequence)
            {
                return Err(conflict(
                    "Integration completion changed its exact canonical event prefix",
                ));
            }
        }
    }
    Ok(())
}
