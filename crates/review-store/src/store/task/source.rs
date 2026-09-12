//! An explicit issue revision invalidates execution authority without replacing its ledger.
use super::*;
use review_core::task::{ArtifactInputV1, source::*};
use review_graph::task::CompiledTask;

fn source_capture(
    cas: &Cas,
    input: &ArtifactInputV1,
) -> Result<(ArtifactEnvelope, ArtifactEnvelope, TaskSourceCaptureV1), StoreError> {
    if input.artifact_type != "af/Requirements@1"
        || input.artifact_ids.len() != 1
        || input.snapshot_id.is_some()
    {
        return Err(conflict(
            "Source refresh needs one normalized Requirements artifact",
        ));
    }
    let requirements = envelope(cas, &input.artifact_ids[0], "af/Requirements@1")?;
    if !matches!(
        requirements.producer,
        review_core::Producer::KernelOperation { .. }
    ) {
        return Err(conflict(
            "Source requirements must be captured outside Worker execution",
        ));
    }
    let mut sources = Vec::new();
    for id in &requirements.input_artifacts {
        if cas
            .get_json(id)
            .ok()
            .is_some_and(|v| v["type"] == TASK_SOURCE_CAPTURE_V1)
        {
            let env = envelope(cas, id, TASK_SOURCE_CAPTURE_V1)?;
            if !matches!(env.producer, review_core::Producer::KernelOperation { .. }) {
                return Err(conflict("Issue capture is not a trusted source operation"));
            }
            let capture: TaskSourceCaptureV1 = serde_json::from_value(env.payload.clone())?;
            capture.validate().map_err(conflict)?;
            let mut refs = BTreeSet::from([capture.raw_source_id.clone()]);
            for field in capture.fields.values() {
                refs.insert(field.value_id.clone());
                refs.insert(field.text_id.clone());
            }
            if refs.into_iter().collect::<Vec<_>>() != env.input_artifacts {
                return Err(conflict(
                    "Issue capture omitted or changed a selected field reference",
                ));
            }
            sources.push((env, capture));
        }
    }
    if sources.len() != 1 {
        return Err(conflict("Requirements must retain one exact issue capture"));
    }
    let (env, capture) = sources.pop().unwrap();
    Ok((requirements, env, capture))
}

fn validate_revision(
    cas: &Cas,
    state: &TaskProjection,
    next: &TaskRevisionV1,
) -> Result<(), StoreError> {
    if state.deliveries.last().is_some_and(|(_, delivery)| {
        delivery.status == task::delivery::TaskDeliveryStatusV1::Prepared
    }) {
        return Err(conflict(
            "Resolve the pending local delivery before refreshing its Task",
        ));
    }
    let old_input = state
        .revision
        .inputs
        .get("requirements")
        .ok_or_else(|| conflict("Task has no issue requirements"))?;
    let new_input = next
        .inputs
        .get("requirements")
        .ok_or_else(|| conflict("Refreshed Task omitted requirements"))?;
    let (old_req, old_source, old_capture) = source_capture(cas, old_input)?;
    let (new_req, new_source, new_capture) = source_capture(cas, new_input)?;
    if old_capture.adapter != new_capture.adapter
        || old_capture.external_id != new_capture.external_id
        || old_capture.external_key != new_capture.external_key
        || (new_capture.adapter == TaskSourceAdapterV1::JiraCloud
            && (old_capture.locator != new_capture.locator
                || old_capture.fields.keys().ne(new_capture.fields.keys())))
        || old_input.artifact_type != new_input.artifact_type
        || old_input.cardinality != new_input.cardinality
        || old_input == new_input
    {
        return Err(conflict(
            "Source refresh changed its issue, adapter or input contract, or supplied no new input",
        ));
    }
    let without_source = |input: &ArtifactEnvelope, id: &str| {
        input
            .input_artifacts
            .iter()
            .filter(|v| v.as_str() != id)
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    if without_source(&old_req, &old_source.artifact_id)
        != without_source(&new_req, &new_source.artifact_id)
    {
        return Err(conflict(
            "Source refresh changed the originating Task definition",
        ));
    }
    let old_normal: NormalizedRequirementsV1 = serde_json::from_value(old_req.payload)?;
    let new_normal: NormalizedRequirementsV1 = serde_json::from_value(new_req.payload)?;
    old_normal.validate().map_err(conflict)?;
    new_normal.validate().map_err(conflict)?;
    let text = |name: &str| -> Result<String, StoreError> {
        let bytes = cas
            .get_bounded(&new_capture.fields[name].text_id, 131072)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        String::from_utf8(bytes).map_err(|_| conflict("Captured issue text is not UTF-8"))
    };
    let issue = IssueInputV1 {
        schema: "af.issue-input/1".into(),
        id: new_capture.external_id.clone(),
        key: new_capture.external_key.clone(),
        revision: new_capture.source_revision.clone(),
        summary: text("summary")?,
        description: text("description")?,
        acceptance: new_capture
            .fields
            .keys()
            .filter(|n| !matches!(n.as_str(), "summary" | "description"))
            .map(|n| Ok((n.clone(), text(n)?)))
            .collect::<Result<_, StoreError>>()?,
    };
    issue.validate().map_err(conflict)?;
    if issue.requirements(old_normal.specification.clone()) != new_normal {
        return Err(conflict(
            "Source refresh changed structured requirements or failed field normalization",
        ));
    }
    let prefix = state
        .revision
        .goal
        .strip_suffix(&old_normal.text)
        .filter(|s| s.ends_with("\n\n"))
        .ok_or_else(|| conflict("Task goal lost its original source normalization"))?;
    if next.goal != format!("{prefix}{}", new_normal.text) {
        return Err(conflict(
            "Refreshed goal differs from the captured action and issue text",
        ));
    }
    let mut expected = state.revision.clone();
    expected.revision = expected
        .revision
        .checked_add(1)
        .ok_or_else(|| conflict("Task revision overflow"))?;
    expected.previous_revision_id = Some(state.revision_id.clone());
    expected.goal = next.goal.clone();
    expected
        .inputs
        .insert("requirements".into(), new_input.clone());
    expected.provenance.adapter_id = next.provenance.adapter_id.clone();
    expected.provenance.input_artifact_ids = expected
        .inputs
        .values()
        .flat_map(|p| p.artifact_ids.iter().cloned())
        .collect();
    if &expected != next {
        return Err(conflict(
            "Issue refresh must retain Task policy, limits, source Snapshot, verification and selection facts",
        ));
    }
    Ok(())
}

impl TaskProjection {
    pub(super) fn apply_source_refreshed(
        &mut self,
        cas: &Cas,
        revision_id: &str,
        plan_id: Option<&str>,
        waiting: Option<TaskWaitingReasonV1>,
        time: u64,
    ) -> Result<(), StoreError> {
        let next = revision(cas, revision_id)?;
        validate_revision(cas, self, &next)?;
        if plan_id.is_some() == waiting.is_some()
            || waiting == Some(TaskWaitingReasonV1::NeedsPlanReview)
        {
            return Err(conflict(
                "Refresh needs one exact plan or an explicit unresolved reason",
            ));
        }
        if let Some(execution) = &mut self.execution {
            if !execution.pending_attempts().is_empty() {
                return Err(conflict("Source refresh has pending Attempts"));
            }
            execution.budget.invalidate_plan(time).map_err(conflict)?;
            execution.invocations.clear();
            execution.outputs.clear();
        }
        self.revision_id = revision_id.into();
        self.revision = next;
        self.plan_id = plan_id.map(str::to_owned);
        self.admitted = false;
        self.resume_phase = None;
        if let Some(id) = plan_id {
            let plan = plan(cas, id, self)?;
            let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
            if graph.inputs != self.revision.inputs
                || graph.order != graph.scheduler_plan().map_err(conflict)?.order
            {
                return Err(conflict(
                    "Refreshed plan has a noncanonical graph or stale inputs",
                ));
            }
            if let Some(execution) = &mut self.execution {
                graph
                    .budget(execution.budget.remaining_limits())
                    .map_err(conflict)?;
                execution
                    .budget
                    .install_graph_with_token_scopes(
                        graph.allowances.clone(),
                        graph
                            .calls
                            .iter()
                            .map(|(n, c)| (n.clone(), c.max_attempts))
                            .collect(),
                        graph.token_scopes.clone(),
                        time,
                        plan.preparation.is_some(),
                    )
                    .map_err(conflict)?;
                execution.graph = graph;
            }
            if plan.preparation.is_some() {
                self.planning = None;
            }
            self.phase = if plan.requires_developer_approval() {
                TaskPhaseV1::Waiting {
                    reason: TaskWaitingReasonV1::NeedsPlanReview,
                }
            } else {
                TaskPhaseV1::Ready {}
            };
        } else {
            self.phase = TaskPhaseV1::Waiting {
                reason: waiting.unwrap(),
            };
        }
        Ok(())
    }
}
impl EventStore {
    #[allow(clippy::too_many_arguments)]
    pub fn refresh_task_source(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        revision_id: &str,
        plan_id: Option<&str>,
        waiting: Option<TaskWaitingReasonV1>,
        authority: &dyn TaskAuthority,
    ) -> Result<RunEvent, StoreError> {
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown Task"))?;
        let next = revision(cas, revision_id)?;
        validate_revision(cas, &state, &next)?;
        if plan_id.is_some() == waiting.is_some()
            || waiting == Some(TaskWaitingReasonV1::NeedsPlanReview)
        {
            return Err(conflict(
                "Refresh needs one exact plan or an explicit unresolved reason",
            ));
        }
        let time = now()?;
        let mut budget = state.execution.as_ref().map(|e| e.budget.clone());
        if let Some(budget) = &mut budget {
            // Pending work and a backwards clock are conflicts, never capacity refusals.
            budget.invalidate_plan(time).map_err(conflict)?;
        }
        let mut admitted_plan = plan_id;
        let mut unresolved = waiting;
        if let Some(id) = plan_id {
            let mut candidate = state.clone();
            candidate.revision = next;
            candidate.revision_id = revision_id.into();
            let plan = self.authorized_plan(cas, &candidate, id, authority)?;
            let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
            let capacity = budget.as_ref().map_or_else(
                || candidate.revision.limits.clone(),
                |b| b.remaining_limits(),
            );
            // Selection may have taken time. Recheck only resource feasibility after exact
            // source/plan authorization and record the changed request even when it no longer fits.
            let fits = (|| -> Result<(), String> {
                graph.budget(capacity.clone())?;
                if time >= capacity.deadline_unix_ms
                    || capacity.deadline_unix_ms - time < capacity.verification.wall_ms
                {
                    return Err("Refreshed plan no longer has its verification time".into());
                }
                if let Some(budget) = &mut budget {
                    budget.install_graph_with_token_scopes(
                        graph.allowances.clone(),
                        graph
                            .calls
                            .iter()
                            .map(|(n, c)| (n.clone(), c.max_attempts))
                            .collect(),
                        graph.token_scopes.clone(),
                        time,
                        plan.preparation.is_some(),
                    )?;
                }
                Ok(())
            })();
            if fits.is_err() {
                admitted_plan = None;
                unresolved = Some(TaskWaitingReasonV1::NeedsResources);
            }
        }
        self.task_change(
            cas,
            lease,
            TaskChangeV1::SourceRefreshed {
                revision_id: revision_id.into(),
                plan_id: admitted_plan.map(str::to_owned),
                waiting: unresolved,
            },
            time,
        )
    }
}
