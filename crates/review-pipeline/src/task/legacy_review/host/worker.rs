//! One Review Worker transport below an already-started common Attempt. Usage remains outside
//! parsing, Proposal preparation and CAS writes so those failures cannot refund model spend.

use super::*;
use crate::reviewer_output::ReviewerResultCapture;
use review_core::task::feedback::{
    TASK_RETRY_FEEDBACK_V1, TaskFeedbackCodeV1, TaskRetryFeedbackV1,
};
use review_core::task::review_compat::*;
use review_core::task::usage::TaskTokenUsageV3;
use review_runner::{ContextManifest, ReviewerInputs};

impl LegacyReviewTaskHost<'_, '_> {
    fn slot(&self, node: &str) -> Result<&str, String> {
        let base = self.domain.reviewer_binding_node(node);
        let task_node = &self.captured.compilation.nodes[&base].task_node;
        let CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        } = &self.captured.compilation.graph.nodes[task_node].operator
        else {
            return Err("Not a Review Worker slot".into());
        };
        Ok(slot)
    }

    fn model(&self, node: &str) -> Result<&TaskModelBinding<'_>, String> {
        let slot = self.slot(node)?;
        let value = self
            .models
            .get(slot)
            .ok_or("Review Model Worker lacks its effective binding")?;
        if self.plan.bindings.get(slot) != Some(&value.binding) {
            return Err(
                "Review Model Worker changed its exact account or invocation policy".into(),
            );
        }
        Ok(value)
    }

    fn execution(&self, node: &str) -> Result<&WorkerExecutionV1, String> {
        Ok(&self.plan.bindings[self.slot(node)?].execution)
    }

    pub(super) fn validate_transports(&self) -> Result<(), String> {
        let mut expected = std::collections::BTreeSet::new();
        for node in self.captured.loaded.reviewers().keys() {
            let actual_mode = match self.execution(node)? {
                WorkerExecutionV1::Command {} => {
                    review_core::BrokerCredentialModeV1::CredentialFree
                }
                WorkerExecutionV1::Model { .. } => self.model(node)?.adapter.credential_mode(),
            };
            if self
                .captured
                .loaded
                .reviewer_execution()
                .get(node)
                .is_some_and(|policy| policy.credential_mode != actual_mode)
            {
                return Err("Review transport differs from the captured credential mode".into());
            }
            match self.execution(node)? {
                WorkerExecutionV1::Command {} => {}
                WorkerExecutionV1::Model {
                    provider_kind,
                    model,
                    effort,
                    ..
                } => {
                    expected.insert(self.slot(node)?.to_owned());
                    let adapter = self.model(node)?.adapter;
                    if adapter.provider_kind() != provider_kind
                        || adapter.model_settings() != Some((model.clone(), effort.clone()))
                    {
                        return Err(
                            "Review transport differs from its captured Provider/model/effort"
                                .into(),
                        );
                    }
                    self.instructions(node)?;
                }
            }
        }
        if self
            .models
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            != expected
        {
            return Err("Review transport map contains an undeclared Model Worker".into());
        }
        Ok(())
    }

    fn instructions(&self, node: &str) -> Result<String, String> {
        let base = self.domain.reviewer_binding_node(node);
        let package = self
            .captured
            .loaded
            .packages()
            .get(&base)
            .ok_or("Native Review Worker requires captured package instructions")?;
        let bytes = package
            .file("reviewer.md")
            .ok_or("Review package lacks reviewer.md")?;
        let mut instructions = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            self.domain
                .cas
                .get_json(&self.domain.authority.campaign_manifest_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if let Some(focus) = manifest.focus {
            review_runner::append_focus(&mut instructions, &focus);
        }
        Ok(instructions)
    }

    fn inputs(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<ReviewerInputs, String> {
        let (node, mapping, _) = self.operation(input)?.ok_or("Not a Review Worker")?;
        let mut inputs = crate::reviewer_inputs::prepare(
            cas,
            &self.domain.authority,
            &node,
            &self.raw_inputs(cas, input, &mapping)?,
        )?;
        // Warm layers are declared inputs bound before the exact context is captured; the
        // Warm Set itself was recorded when the invocation was published, before reservation.
        crate::warm::request_notes(&mut inputs, self.domain.notes_max_bytes(&node.id));
        if let Some(record) = self.domain.select_warm_set(&node.id, None)? {
            crate::warm::apply_warm_set(cas, &mut inputs, &record)?;
        }
        Ok(inputs)
    }

    pub(super) fn worker_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        let (node, _, _) = self.operation(input)?.ok_or("Not a Review Worker")?;
        let (invocation, event) = self.invocation(input)?;
        if attempt.task_id() != self.task.task_id
            || attempt.plan_id() != self.plan_id
            || attempt.node() != input.node
            || attempt.invocation_id() != invocation
        {
            return Err("Review context has another Task's reserved Attempt".into());
        }
        let mut inputs = self.inputs(cas, input)?;
        crate::reviewer_inputs::bind_attempt(
            &mut inputs,
            &self.domain.authority,
            &node.id,
            attempt.id(),
            Some(attempt.reservation().tokens),
        );
        for id in attempt.feedback_ids() {
            let artifact = cas.get_artifact(id).map_err(|e| e.to_string())?;
            if artifact.artifact_type != review_core::task::feedback::TASK_RETRY_FEEDBACK_V1 {
                return Err("Review retry requires typed durable feedback".into());
            }
            let feedback: review_core::task::feedback::TaskRetryFeedbackV1 =
                serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
            feedback.validate()?;
            let contract = &self.plan.bindings[self.slot(&node.id)?].invocation_policy_id;
            if &feedback.contract_id != contract
                || feedback.compiler.is_some()
                || artifact.producer
                    != (Producer::Attempt {
                        run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                        node_id: input.node.clone(),
                        attempt_id: feedback.attempt_id.clone(),
                    })
                || feedback.attempt_id == attempt.id()
            {
                return Err(
                    "Review retry feedback changed its prior Attempt or captured contract".into(),
                );
            }
            for reference in artifact.input_artifacts {
                cas.verify(&reference).map_err(|e| e.to_string())?;
            }
            inputs
                .refused_attempts
                .push(serde_json::to_string(&feedback).map_err(|e| e.to_string())?);
        }
        if !attempt.feedback_ids().is_empty() {
            inputs.refusal_history_artifact_id = Some(
                cas.put_json(
                    &serde_json::to_value(attempt.feedback_ids()).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?,
            );
        }
        let (bytes, manifest) = match self.execution(&node.id)? {
            WorkerExecutionV1::Command {} => review_runner::compose_command_input(&inputs)?,
            WorkerExecutionV1::Model { .. } => {
                let (text, manifest) =
                    review_runner::compose_model_prompt(&self.instructions(&node.id)?, &inputs)?;
                if manifest.estimated_tokens > attempt.reservation().tokens {
                    return Err(
                        "Rendered Review input exceeds the common Attempt reservation".into(),
                    );
                }
                (text.into_bytes(), manifest)
            }
        };
        let context = TaskReviewContextV1 {
            campaign_id: self.domain.run_id.clone(),
            round_event_id: self.domain.authority.round_event_id.clone(),
            invocation_event_id: event,
            review_node: node.id.clone(),
            subject_id: self.domain.authority.subject_id.clone(),
            campaign_manifest_id: self.domain.authority.campaign_manifest_id.clone(),
            task_invocation_id: invocation,
            attempt_id: attempt.id().into(),
            reviewer_inputs_id: cas
                .put_json(&serde_json::to_value(&inputs).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
            rendered_input_id: cas.put(&bytes).map_err(|e| e.to_string())?,
            context_manifest_id: cas
                .put_json(&serde_json::to_value(manifest).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        };
        context.validate()?;
        cas.put_artifact(
            TASK_REVIEW_CONTEXT_V1,
            Producer::Attempt {
                run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                node_id: input.node.clone(),
                attempt_id: attempt.id().into(),
            },
            context
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(self.domain.authority.head_snapshot_id.clone()),
            serde_json::to_value(context).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
        .map(|v| v.0)
    }

    pub(super) fn execute_reviewer(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        let mut result = TaskWorkOutput {
            usage_observation: None,
            usage: Some(TaskTokenUsageV3::charge_only(0)),
            outputs: Err("Review Worker was not started".into()),
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        };
        let mut feedback_code = TaskFeedbackCodeV1::ContextRejected;
        let mut runtime_evidence_id: Option<String> = None;
        result.outputs = (|| {
            let attempt = attempt.ok_or("Review Worker has no started common Attempt")?;
            let deadline = self.current(attempt)?;
            let (node, mapping, _) = self.operation(input)?.ok_or("Not a Review Worker")?;
            let context: TaskReviewContextV1 = serde_json::from_value(
                cas.get_artifact(attempt.context_id())
                    .map_err(|e| e.to_string())?
                    .payload,
            )
            .map_err(|e| e.to_string())?;
            context.validate()?;
            if context.attempt_id != attempt.id()
                || context.task_invocation_id != self.invocation(input)?.0
            {
                return Err("Review transport has another Attempt's context".into());
            }
            let inputs = self.inputs(cas, input)?;
            let bytes = cas
                .get(&context.rendered_input_id)
                .map_err(|e| e.to_string())?;
            // The exact context manifest must be readable before the Worker runs; the
            // selected Attempt's evidence reads it back from this identity.
            serde_json::from_value::<ContextManifest>(
                cas.get_json(&context.context_manifest_id)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let sandbox = self
                .domain
                .sandbox_for(&node.id, review_sandbox::Mode::EphemeralWrite)?;
            // The Round's recorded Warm Set names the Build Cache, if any; it is cloned into
            // this exact sandbox and its location reaches the adapter as sandbox-local
            // environment, never as rendered context.
            // The Task-hosted frontend installs no session capability, so no delta is sized.
            let warm_set = self.domain.select_warm_set(&node.id, None)?;
            let (environment, clone_evidence) =
                self.domain
                    .materialize_build_cache(&node.id, warm_set.as_ref(), &sandbox)?;
            if let Some(evidence) = clone_evidence {
                // The clone is this exact Attempt's preparation. It settles with the Attempt,
                // whatever the Worker then does, and never as a node-wide fact.
                runtime_evidence_id =
                    Some(self.retain_worker_runtime_evidence(cas, input, attempt, evidence)?);
            }
            let runtime = match self.execution(&node.id)? {
                WorkerExecutionV1::Command {} => {
                    Some(tempfile::tempdir().map_err(|e| e.to_string())?)
                }
                WorkerExecutionV1::Model { .. } => None,
            };
            self.current(attempt)?;
            // Materialization consumes this same Attempt's allowance, too.
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err("Review Attempt expired during sandbox setup".into());
            }
            feedback_code = match self.execution(&node.id)? {
                WorkerExecutionV1::Command {} => TaskFeedbackCodeV1::ProcessFailure,
                WorkerExecutionV1::Model { .. } => TaskFeedbackCodeV1::ProviderFailure,
            };
            let returned = match self.execution(&node.id)? {
                WorkerExecutionV1::Command {} => {
                    review_runner::task::invoke_command_bytes_controlled_with_environment(
                        cas,
                        sandbox.root(),
                        runtime.as_ref().expect("command runtime").path(),
                        &self.captured.loaded.reviewers()
                            [&self.domain.reviewer_binding_node(&node.id)],
                        bytes,
                        timeout,
                        cancellation,
                        &environment,
                    )
                }
                // Preserve the native Review capability profile: ADR-0042 keeps Claude
                // read-only, while the legacy Codex adapter permits sandbox Proposals.
                // A different installed backend has no implicit edit authority.
                WorkerExecutionV1::Model { provider_kind, .. } => self
                    .model(&node.id)?
                    .adapter
                    .invoke_controlled_with_environment(
                        cas,
                        sandbox.root(),
                        bytes,
                        timeout,
                        provider_kind == "codex",
                        cancellation,
                        &environment,
                    ),
            };
            // Build cache bytes leave before the seal: they never enter the sealed diff, a
            // Proposal, or provenance.
            if !environment.is_empty() {
                review_sandbox::remove_materialized_caches(&sandbox)?;
            }
            result.usage = returned.usage;
            result.usage_observation = returned.usage_observation;
            result.charged_tokens = result
                .usage
                .as_ref()
                .map(|usage| usage.chargeable_tokens.get());
            result.raw_artifact_ids = returned.raw_artifact_ids;
            let message = returned.message?;
            feedback_code = TaskFeedbackCodeV1::InvalidOutputContract;
            let text = std::str::from_utf8(&message).map_err(|e| e.to_string())?;
            let parsed = review_runner::parse_reviewer_result(text)?;
            let assigned: Vec<String> = inputs
                .prior_findings
                .as_ref()
                .and_then(|v| v.get("findings"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.get("finding_id").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect();
            let value = crate::reviewer_result_value(&parsed, &assigned)
                .map_err(|e| format!("Review result rejected: {e:?}"))?;
            feedback_code = TaskFeedbackCodeV1::OutputAdmissionRejected;
            let raw = if let Some(id) = result.raw_artifact_ids.first() {
                id.clone()
            } else {
                let id = cas.put(&message).map_err(|e| e.to_string())?;
                result.raw_artifact_ids.push(id.clone());
                id
            };
            let unknown = TaskTokenUsageV3::charge_only(u128::from(attempt.reservation().tokens));
            let usage = result.usage.as_ref().unwrap_or(&unknown);
            let captured = crate::reviewer_output::capture_task_result(
                cas,
                &self.domain.authority,
                sandbox,
                ReviewerResultCapture {
                    node_id: &node.id,
                    attempt_id: attempt.id(),
                    result: &value,
                    result_contract: inputs.result_contract,
                    proposal: review_runner::parse_proposal_declaration(text),
                    notes: review_runner::parse_notes_declaration(text),
                    notes_max_bytes: self.domain.notes_max_bytes(&node.id),
                    head_manifest: &self.domain.snapshot,
                    assigned_finding_ids: &assigned,
                    report_count: parsed.findings.len(),
                    cost_tokens: usage.chargeable_tokens.get(),
                    usage,
                    raw_artifact: &raw,
                },
                attempt.context_id(),
                result.usage.is_some(),
            )?;
            // The Notes outcome is a durable Round fact of this exact Attempt. It is appended
            // now, under the Attempt epoch, so a later Round's Warm Set selection reads it from
            // the log rather than from process memory.
            if let Some(notes) = captured.notes {
                self.domain.append(notes.event)?;
            }
            let raw_outputs = BTreeMap::from([(
                node.outputs[0].name.clone(),
                vec![captured.metadata.result_artifact_id.clone()],
            )]);
            let mut ports = self.lift(
                cas,
                input,
                &mapping,
                &raw_outputs,
                &self.producer(input, Some(attempt))?,
            )?;
            let metadata = &captured.metadata;
            let id = cas
                .put_artifact(
                    TASK_REVIEW_RESULT_METADATA_V1,
                    Producer::Attempt {
                        run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                        node_id: input.node.clone(),
                        attempt_id: attempt.id().into(),
                    },
                    metadata
                        .artifact_refs()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    Some(self.domain.authority.head_snapshot_id.clone()),
                    serde_json::to_value(metadata).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            ports.insert(
                "metadata".into(),
                artifact_input(
                    cas,
                    TASK_REVIEW_RESULT_METADATA_V1,
                    vec![id],
                    review_core::PortCardinality::One,
                )?,
            );
            Ok(ports)
        })();
        // Retained after the raw response, so the first raw artifact stays the Worker's reply.
        result.raw_artifact_ids.extend(runtime_evidence_id);
        if result.outputs.is_err() {
            result.feedback_id = (|| {
                let attempt = attempt.ok_or("Review feedback has no actual Attempt")?;
                let (node, _, _) = self.operation(input)?.ok_or("Not a Review Worker")?;
                let contract_id = &self.plan.bindings[self.slot(&node.id)?].invocation_policy_id;
                let feedback = TaskRetryFeedbackV1 {
                    attempt_id: attempt.id().into(),
                    contract_id: contract_id.clone(),
                    code: feedback_code,
                    compiler: None,
                };
                feedback.validate()?;
                cas.put_artifact(
                    TASK_RETRY_FEEDBACK_V1,
                    self.producer(input, Some(attempt))?,
                    vec![attempt.context_id().into(), contract_id.clone()],
                    None,
                    serde_json::to_value(feedback).map_err(|e| e.to_string())?,
                )
                .map(|v| v.0)
                .map_err(|e| e.to_string())
            })()
            .ok();
        }
        result
    }

    pub(super) fn publish_reviewer(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        output_id: &str,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let (node, _, _) = self.operation(input)?.ok_or("Not a Review Worker")?;
        let wrapper = cas.get_artifact(output_id).map_err(|e| e.to_string())?;
        let Producer::Attempt { attempt_id, .. } = wrapper.producer else {
            return Err("Review selection lacks its actual common Attempt".into());
        };
        let registered = self
            .owned
            .lock()
            .expect("owned Review mappings")
            .get(&input.node)
            .map(|child| child.registered.clone());
        let mut store = self.domain.store.lock().expect("Task Store");
        if let Some(registered) = registered {
            store.publish_task_owned_review_result(
                cas,
                &self.lease,
                &registered,
                output_id,
                &self.authority(),
            )
        } else if store
            .task_projection(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .is_some_and(|state| state.has_recording_recovery())
        {
            store.publish_task_recorded_review_result(
                cas,
                &self.lease,
                output_id,
                &self.authority(),
            )
        } else {
            store.publish_task_review_result(cas, &self.lease, output_id, &self.authority())
        }
        .map_err(|e| e.to_string())?;
        drop(store);
        let metadata: TaskReviewResultMetadataV1 = serde_json::from_value(
            cas.get_artifact(&output.outputs["metadata"].artifact_ids[0])
                .map_err(|e| e.to_string())?
                .payload,
        )
        .map_err(|e| e.to_string())?;
        let candidate = match &metadata.proposal {
            TaskReviewProposalV1::Prepared {
                candidate_artifact_id,
            } => Some(candidate_artifact_id.clone()),
            _ => None,
        };
        self.domain
            .reviewer_selections
            .lock()
            .expect("Review selections")
            .insert(
                node.id.clone(),
                crate::SelectedReviewer {
                    attempt_id: attempt_id.clone(),
                    result_artifact: metadata.result_artifact_id.clone(),
                    proposal_candidate: candidate.clone(),
                },
            );
        let event = match metadata.proposal {
            TaskReviewProposalV1::None {} => None,
            TaskReviewProposalV1::Prepared {
                candidate_artifact_id,
            } => {
                let value: review_core::ProposalCandidateV1 = serde_json::from_value(
                    cas.get_json(&candidate_artifact_id)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let mut refs = vec![
                    candidate_artifact_id.clone(),
                    value.result_artifact_id,
                    value.patch_artifact_id,
                    value.derived_manifest_artifact_id,
                ];
                refs.extend(value.evidence_ids);
                Some(
                    NewEvent::new(
                        EventType::ProposalPreparedV1,
                        serde_json::to_value(review_core::ProposalPreparedPayloadV1 {
                            candidate_artifact_id,
                            result_artifact_id: metadata.result_artifact_id,
                        })
                        .map_err(|e| e.to_string())?,
                    )
                    .node(&node.id)
                    .attempt(&attempt_id)
                    .referencing(refs),
                )
            }
            TaskReviewProposalV1::Refused { reason } => Some(
                NewEvent::new(
                    EventType::ProposalRefusedV1,
                    serde_json::to_value(review_core::ProposalRefusedPayloadV1 {
                        reason,
                        result_artifact_id: metadata.result_artifact_id.clone(),
                    })
                    .map_err(|e| e.to_string())?,
                )
                .node(&node.id)
                .attempt(&attempt_id)
                .referencing(vec![metadata.result_artifact_id]),
            ),
        };
        if let Some(event) = event {
            let exists = self
                .domain
                .store
                .lock()
                .expect("Task Store")
                .replay(&self.domain.run_id)
                .map_err(|e| e.to_string())?
                .iter()
                .any(|old| {
                    old.event_type == event.event_type
                        && old.node_id == event.node_id
                        && old.attempt_id == event.attempt_id
                        && old.causation_id.as_deref()
                            == Some(&self.domain.authority.round_event_id)
                        && old.payload == event.payload
                });
            if !exists {
                self.domain.append(event)?;
            }
        }
        Ok(())
    }
}
