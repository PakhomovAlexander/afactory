//! Durable Codex dispatch fencing, independent of normal Attempt settlement.

use super::*;
use af_contracts::task::pipeline::TaskOperatorV1;
use af_contracts::task::plan::WorkerExecutionV3;
use af_contracts::task::source::TaskSnapshot;

#[derive(Debug, Clone)]
pub(super) struct RecordedLaunch {
    id: String,
    launch: CodexLaunchV1,
    resolution: Option<(String, CodexLaunchOutcomeV1)>,
    checkpoint_id: Option<String>,
}

impl RecordedLaunch {
    pub(super) fn new(id: &str, launch: CodexLaunchV1) -> Self {
        Self {
            id: id.into(),
            launch,
            resolution: None,
            checkpoint_id: None,
        }
    }

    pub(super) fn completed(&self) -> bool {
        matches!(
            &self.resolution,
            Some((_, CodexLaunchOutcomeV1::Completed { .. }))
        )
    }
}

pub(super) fn checkpoint(cas: &Cas, id: &str) -> Result<CodexCheckpointV1, StoreError> {
    let value: CodexCheckpointV1 = payload(cas, id, CODEX_CHECKPOINT_V1)?;
    value.validate().map_err(conflict)?;
    Ok(value)
}

pub(super) fn dispatch_evidence(
    cas: &Cas,
    id: &str,
) -> Result<CodexDispatchEvidenceV1, StoreError> {
    let value: CodexDispatchEvidenceV1 = payload(cas, id, CODEX_DISPATCH_EVIDENCE_V1)?;
    if !af_contracts::is_digest(&value.proof_id) {
        return Err(conflict("Invalid Codex dispatch proof identity"));
    }
    Ok(value)
}

fn evidence_producer(run_id: &str) -> af_contracts::Producer {
    af_contracts::Producer::KernelOperation {
        run_id: run_id.into(),
        node_id: None,
        operation_id: "codex-dispatch-evidence@1".into(),
    }
}

impl TaskExecutionProjection {
    pub(super) fn check_codex_fence(&self, node: &str) -> Result<(), StoreError> {
        for (id, attempt) in &self.attempts {
            if attempt.reservation.node == node
                && let Some(launch) = &attempt.codex_launch
                && launch.resolution.is_none()
            {
                return Err(conflict(format!(
                    "Unresolved Codex dispatch {} for Attempt {id}: missing definitive non-dispatch or quiescent terminal evidence",
                    launch.id
                )));
            }
        }
        Ok(())
    }

    pub(super) fn validate_codex_launch(
        &self,
        cas: &Cas,
        node: &str,
        attempt_id: &str,
        launch: &CodexLaunchV1,
    ) -> Result<(), StoreError> {
        let attempt = &self.attempts[attempt_id];
        if attempt.context_id.as_ref() != Some(&launch.context_id)
            || self.attempts.values().any(|other| {
                other.codex_launch.as_ref().is_some_and(|previous| {
                    previous.launch.nonce == launch.nonce
                        || previous.launch.thread_id == launch.thread_id
                })
            })
        {
            return Err(conflict(
                "Codex launch changed its context or reused native dispatch identity",
            ));
        }
        let Some(CompiledOperator::Primitive {
            operator: TaskOperatorV1::Worker { slot },
            ..
        }) = self.graph.nodes.get(node).map(|n| &n.operator)
        else {
            return Err(conflict("Codex launch requires a compiled Worker"));
        };
        let execution_plan: ExecutionPlan =
            payload(cas, &attempt.plan_id, task::EXECUTION_PLAN_V4)?;
        if self
            .graph
            .slots
            .get(slot)
            .is_none_or(|s| s.role != "implement")
            || !matches!(
                execution_plan.bindings.get(slot).map(|b| &b.execution),
                Some(WorkerExecutionV3::Model { provider_kind, principal_id: Some(_), .. })
                    if provider_kind == "codex"
            )
        {
            return Err(conflict(
                "Codex launch requires an implementer with a pinned Codex principal",
            ));
        }
        let inputs = &self.invocations[node].1.inputs;
        let findings = inputs.get("findings").is_some_and(|input| {
            input.artifact_type == task::findings::TASK_REVIEW_FINDINGS_V1
                && input.artifact_ids.len() == 1
        });
        match &launch.mode {
            CodexLaunchModeV1::Producer {}
                if !findings && !inputs.contains_key("codex_checkpoint") =>
            {
                Ok(())
            }
            CodexLaunchModeV1::WarmRepair { checkpoint_id }
                if findings
                    && inputs.get("codex_checkpoint").is_some_and(|input| {
                        input.artifact_type == CODEX_CHECKPOINT_V1
                            && input.artifact_ids.as_slice() == std::slice::from_ref(checkpoint_id)
                    }) =>
            {
                checkpoint(cas, checkpoint_id)?;
                Ok(())
            }
            CodexLaunchModeV1::FreshRepair {}
                if findings && !inputs.contains_key("codex_checkpoint") =>
            {
                Ok(())
            }
            _ => Err(conflict(
                "Codex launch mode differs from its admitted inputs",
            )),
        }
    }

    pub(super) fn resolve_codex_launch(
        &mut self,
        cas: &Cas,
        record_id: &str,
        attempt_id: &str,
        launch_id: &str,
        evidence_id: &str,
        outcome: &CodexLaunchOutcomeV1,
    ) -> Result<(), StoreError> {
        let launch = self
            .attempts
            .get_mut(attempt_id)
            .and_then(|a| a.codex_launch.as_mut())
            .filter(|l| l.id == launch_id)
            .ok_or_else(|| conflict("Codex resolution requires its exact armed launch"))?;
        if let Some((selected, _)) = &launch.resolution {
            return if selected == record_id {
                Ok(())
            } else {
                Err(conflict("Codex launch already has a different resolution"))
            };
        }
        let evidence = dispatch_evidence(cas, evidence_id)?;
        let receipt = envelope(cas, evidence_id, CODEX_DISPATCH_EVIDENCE_V1)?;
        let marker = read_execution_record(cas, launch_id)?.envelope;
        if receipt.producer != evidence_producer(marker.producer.run_id())
            || receipt.input_artifacts != [launch_id.to_owned(), evidence.proof_id.clone()]
            || receipt.subject_snapshot_id.is_some()
            || evidence.launch_id != launch_id
            || evidence.thread_id != launch.launch.thread_id
            || evidence.nonce != launch.launch.nonce
            || &evidence.outcome != outcome
            || cas
                .verify(&evidence.proof_id)
                .map_err(|error| artifact_read_error(&evidence.proof_id, error))?
                == 0
        {
            return Err(conflict(
                "Codex resolution needs exact correlated dispatch evidence",
            ));
        }
        launch.resolution = Some((record_id.into(), outcome.clone()));
        Ok(())
    }

    pub(super) fn select_codex_checkpoint(
        &mut self,
        cas: &Cas,
        task_id: &str,
        attempt_id: &str,
        checkpoint_id: &str,
    ) -> Result<(), StoreError> {
        let value = checkpoint(cas, checkpoint_id)?;
        let attempt = self
            .attempts
            .get(attempt_id)
            .ok_or_else(|| conflict("Codex checkpoint requires its producing Attempt"))?;
        let launch = attempt
            .codex_launch
            .as_ref()
            .filter(|launch| {
                launch.id == value.launch_id
                    && launch.completed()
                    && launch.launch.mode == (CodexLaunchModeV1::Producer {})
            })
            .ok_or_else(|| conflict("Codex checkpoint requires its completed producer launch"))?;
        if launch.checkpoint_id.as_deref() == Some(checkpoint_id) {
            return Ok(());
        }
        let node = &attempt.reservation.node;
        let expected = af_contracts::Producer::KernelOperation {
            run_id: task_run_id(task_id)?,
            node_id: Some(node.clone()),
            operation_id: "codex-checkpoint@1".into(),
        };
        let envelope = envelope(cas, checkpoint_id, CODEX_CHECKPOINT_V1)?;
        if launch.checkpoint_id.is_some()
            || envelope.producer != expected
            || envelope.subject_snapshot_id.as_ref() != Some(&value.snapshot_id)
            || envelope
                .input_artifacts
                .iter()
                .map(String::as_str)
                .ne(value.artifact_refs())
        {
            return Err(conflict(
                "Codex checkpoint differs from its kernel producer or references",
            ));
        }
        let Some(TaskExecutionRecordV1::Settled {
            result: TaskAttemptResultV1::Succeeded { output_id },
            raw_artifact_ids,
            ..
        }) = &attempt.settlement
        else {
            return Err(conflict(
                "Codex checkpoint requires a successful settled result",
            ));
        };
        if output_id != &value.output_id
            || !raw_artifact_ids.contains(&value.reply_id)
            || self.reusable_output(node).as_ref() != Some(&(output_id.clone(), attempt_id.into()))
        {
            return Err(conflict(
                "Codex checkpoint differs from its selected Attempt output or reply",
            ));
        }

        let (_, producer) = self
            .outputs
            .get(node)
            .filter(|(id, _)| id == output_id)
            .ok_or_else(|| conflict("Codex checkpoint requires a published producer output"))?;
        let candidate = producer
            .outputs
            .get("candidate")
            .filter(|input| {
                input.artifact_type == "af/CandidateTree@1"
                    && input.artifact_ids.as_slice() == std::slice::from_ref(&value.candidate_id)
            })
            .ok_or_else(|| conflict("Codex checkpoint requires the exact produced candidate"))?;
        let candidate_tree = super::envelope(cas, &value.candidate_id, "af/CandidateTree@1")?;
        let parent_id = candidate
            .snapshot_id
            .as_deref()
            .ok_or_else(|| conflict("Codex checkpoint candidate has no parent Snapshot"))?;
        let producer_source = self.invocations[node]
            .1
            .inputs
            .get("source")
            .filter(|input| {
                input.artifact_type == "af/SourceTree@1"
                    && input.artifact_ids.len() == 1
                    && input.snapshot_id.as_deref() == Some(parent_id)
            })
            .ok_or_else(|| {
                conflict("Codex checkpoint candidate differs from the admitted source")
            })?;
        if !candidate_tree
            .input_artifacts
            .contains(&producer_source.artifact_ids[0])
            || candidate_tree.subject_snapshot_id.as_deref() != Some(parent_id)
            || candidate_tree
                .payload
                .get("parent_snapshot_id")
                .and_then(serde_json::Value::as_str)
                != Some(parent_id)
        {
            return Err(conflict(
                "Codex checkpoint candidate does not identify the admitted source and parent Snapshot",
            ));
        }
        let (seal_node, (_, seal)) = self
            .outputs
            .iter()
            .find(|(_, (id, _))| id == &value.sealed_output_id)
            .ok_or_else(|| conflict("Codex checkpoint requires a published seal output"))?;
        let seal_input = &self.invocations[seal_node].1;
        let seal_definition = self.resolve_node(seal_node)?.definition;
        if !matches!(
            seal_definition.operator,
            CompiledOperator::Primitive {
                operator: TaskOperatorV1::Seal {},
                ..
            }
        ) || seal_input.inputs.get("candidate") != Some(candidate)
        {
            return Err(conflict(
                "Codex checkpoint seal did not consume the produced candidate",
            ));
        }
        let snapshot_port = seal
            .outputs
            .get("snapshot")
            .filter(|port| {
                port.artifact_type == "af/SourceTree@1"
                    && port.artifact_ids.len() == 1
                    && port.snapshot_id.as_ref() == Some(&value.snapshot_id)
            })
            .ok_or_else(|| {
                conflict("Codex checkpoint differs from the sealed Snapshot identity")
            })?;
        let source = super::envelope(cas, &snapshot_port.artifact_ids[0], "af/SourceTree@1")?;
        let sealer = af_contracts::Producer::KernelOperation {
            run_id: task_run_id(task_id)?,
            node_id: Some(seal_node.clone()),
            operation_id: "task-builtin@1".into(),
        };
        let snapshot: TaskSnapshot = cas
            .get_json(&value.snapshot_id)
            .map_err(|error| artifact_read_error(&value.snapshot_id, error))
            .and_then(|json| Ok(serde_json::from_value(json)?))?;
        snapshot.validate(&value.snapshot_id).map_err(conflict)?;
        if source.producer != sealer
            || source.subject_snapshot_id.as_ref() != Some(&value.snapshot_id)
            || !source.input_artifacts.contains(&value.candidate_id)
            || !source.input_artifacts.contains(&value.snapshot_id)
            || source
                .payload
                .get("snapshot_id")
                .and_then(serde_json::Value::as_str)
                != Some(&value.snapshot_id)
            || snapshot.parent_snapshot_id != candidate.snapshot_id
            || candidate_tree
                .payload
                .get("manifest_id")
                .and_then(serde_json::Value::as_str)
                != Some(&snapshot.manifest_id)
            || cas
                .verify(&value.prefix_id)
                .map_err(|error| artifact_read_error(&value.prefix_id, error))?
                != value.prefix_bytes
        {
            return Err(conflict(
                "Codex checkpoint differs from the exact seal or history bytes",
            ));
        }
        self.attempts
            .get_mut(attempt_id)
            .expect("known Attempt")
            .codex_launch
            .as_mut()
            .expect("known launch")
            .checkpoint_id = Some(checkpoint_id.into());
        Ok(())
    }
}

impl EventStore {
    /// Read the selected checkpoint for the producer and exact sealed output resolved
    /// by continuation admission. A staged artifact has no authority.
    pub fn selected_codex_checkpoint(
        &self,
        cas: &Cas,
        producer_task_id: &str,
        producer_node: &str,
        sealed_output_id: &str,
        snapshot_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let state = self
            .task_projection(cas, producer_task_id)?
            .ok_or_else(|| conflict("Unknown checkpoint Task"))?;
        let Some(execution) = state.execution else {
            return Ok(None);
        };
        execution.check_codex_fence(producer_node)?;
        let Some((_, attempt_id)) = execution.reusable_output(producer_node) else {
            return Ok(None);
        };
        let Some(id) = execution
            .attempts
            .get(&attempt_id)
            .and_then(|attempt| attempt.codex_launch.as_ref())
            .and_then(|launch| launch.checkpoint_id.as_ref())
        else {
            return Ok(None);
        };
        let selected = checkpoint(cas, id)?;
        if selected.sealed_output_id != sealed_output_id || selected.snapshot_id != snapshot_id {
            return Err(conflict(
                "Codex checkpoint differs from the selected continuation Snapshot",
            ));
        }
        Ok(Some(id.clone()))
    }

    /// The current writer calls this after inert transport preflight and before making its
    /// one send. An error leaves the caller without authority to submit.
    pub fn arm_codex_launch(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt: &PreparedTaskAttempt,
        authority: &dyn TaskAuthority,
        launch: CodexLaunchV1,
    ) -> Result<String, StoreError> {
        let (state, _) = self.checked_task_dispatch(cas, lease, authority)?;
        state.check_prepared_capability(lease, attempt)?;
        let event = self.task_execution_record_from_state(
            cas,
            lease,
            TaskExecutionRecordV1::CodexLaunchArmed {
                attempt_id: attempt.id.clone(),
                launch,
            },
            now()?,
            state,
            None,
        )?;
        match journal::read_task_transition(&event)?.change {
            TaskChangeV1::ExecutionRecorded { record_id } => Ok(record_id),
            _ => unreachable!("committed a Codex launch as an execution record"),
        }
    }

    /// Records evidence from the trusted transport. It must have proved that the exact
    /// submission was never sent and the sender cannot send it, or that the correlated
    /// native turn has terminated and all tool processes are quiescent.
    pub fn record_codex_resolution(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt_id: &str,
        evidence: &CodexDispatchEvidenceV1,
    ) -> Result<(), StoreError> {
        let time = now()?;
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown reconciliation Task"))?;
        state.check_lease_of(&lease.writer, lease.epoch, state.last_time.max(time))?;
        let (evidence_id, _) = cas
            .put_artifact(
                CODEX_DISPATCH_EVIDENCE_V1,
                evidence_producer(&task_run_id(lease.task_id())?),
                vec![evidence.launch_id.clone(), evidence.proof_id.clone()],
                None,
                serde_json::to_value(evidence)?,
            )
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        let record = TaskExecutionRecordV1::CodexLaunchResolved {
            attempt_id: attempt_id.into(),
            launch_id: evidence.launch_id.clone(),
            evidence_id,
            outcome: evidence.outcome.clone(),
        };
        if let Some((id, _)) = state
            .execution
            .as_ref()
            .and_then(|e| e.attempts.get(attempt_id))
            .and_then(|a| a.codex_launch.as_ref())
            .and_then(|l| l.resolution.as_ref())
            && read_execution_record(cas, id)?.record == record
        {
            return Ok(());
        }
        self.task_execution_record_from_state(cas, lease, record, time, state, None)?;
        Ok(())
    }

    /// Select a settled producer checkpoint after its candidate was sealed. The trusted
    /// adapter supplies only the bounded canonical prefix and validated reply references.
    pub fn record_codex_checkpoint(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        attempt_id: &str,
        value: &CodexCheckpointV1,
    ) -> Result<String, StoreError> {
        value.validate().map_err(conflict)?;
        let state = self
            .task_projection(cas, lease.task_id())?
            .ok_or_else(|| conflict("Unknown checkpoint Task"))?;
        let attempt = state
            .execution
            .as_ref()
            .and_then(|e| e.attempts.get(attempt_id))
            .ok_or_else(|| conflict("Unknown checkpoint Attempt"))?;
        let (checkpoint_id, _) = cas
            .put_artifact(
                CODEX_CHECKPOINT_V1,
                af_contracts::Producer::KernelOperation {
                    run_id: task_run_id(lease.task_id())?,
                    node_id: Some(attempt.reservation.node.clone()),
                    operation_id: "codex-checkpoint@1".into(),
                },
                value.artifact_refs().map(str::to_owned).to_vec(),
                Some(value.snapshot_id.clone()),
                serde_json::to_value(value)?,
            )
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        self.task_execution_record_from_state(
            cas,
            lease,
            TaskExecutionRecordV1::CodexCheckpointSelected {
                attempt_id: attempt_id.into(),
                checkpoint_id: checkpoint_id.clone(),
            },
            now()?,
            state,
            None,
        )?;
        Ok(checkpoint_id)
    }
}
