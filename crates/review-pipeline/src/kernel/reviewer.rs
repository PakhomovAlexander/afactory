//! The reviewer node: exact role-scoped input, dispatch through the bound adapter, admission of
//! the returned result, and bounded retries.

use std::collections::BTreeSet;
use std::time::{Instant, SystemTime};

use review_attempt::{Receipt, Selection};
use review_broker::{Broker, BrokerClient, Credential};
use review_core::event::AttemptAdmittedPayloadV1;
use review_core::{
    BrokerCredentialModeV1, BrokerLeaseV1, EventType, LegacyStageOutput, MAX_CHANGE_SET_BYTES,
    MAX_PRIOR_FINDINGS_BYTES, ReviewerExecutionBindingV1, ReviewerResultContract,
    ReviewerResultRejection,
};
use review_graph::{ArtifactMap, Node};
use review_runner::{ReviewerAttemptContext, ReviewerInputArtifact, ReviewerInputs, RunnerError};
use review_sandbox::Mode;
use review_store::NewEvent;

use crate::kernel::attempt::{AttemptFailureEvidence, failed_retry_context, fenced_retry_context};
use crate::kernel::proposal::PreparedProposal;
use crate::kernel::replay::SelectedReviewer;
use crate::kernel::{Kernel, KernelBrokerBoundary, PreparedReviewerAttempt};
use crate::mutations::mutation_summary;
use crate::{
    is_change_set_port, is_reviewer_finding_set_input, is_reviewer_prior_set_input,
    reviewer_result_contract,
};

impl Kernel<'_> {
    pub(crate) fn run_reviewer(
        &self,
        node: &Node,
        node_inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        let node_id = node.id.as_str();
        let binding_node = self.reviewer_binding_node(node_id);
        let mut prepared = self
            .prepared_attempts
            .lock()
            .expect("prepared attempts")
            .remove(node_id);
        let adapter = match self.reviewers.get(&binding_node) {
            Some(adapter) => adapter,
            None => {
                let error = format!("no reviewer bound to node {node_id}");
                if let Some(prepared) = prepared.take() {
                    self.release_prepared_attempt(
                        node_id,
                        &prepared.attempt,
                        prepared.reservation.as_ref(),
                        &error,
                    )?;
                }
                return Err(error);
            }
        };
        let result_contract = match reviewer_result_contract(node) {
            Ok(contract) => contract,
            Err(error) => {
                if let Some(prepared) = prepared.take() {
                    self.release_prepared_attempt(
                        node_id,
                        &prepared.attempt,
                        prepared.reservation.as_ref(),
                        &error,
                    )?;
                }
                return Err(error);
            }
        };

        // Prior findings arrive through the wired `prior_findings` input port — a data artifact
        // the pipeline routed from the generation node — not from ambient kernel state. A
        // reviewer that declares no such input receives none; the plan is the delivery.
        let prior_findings_contract = node
            .inputs
            .iter()
            .find(|port| is_reviewer_prior_set_input(port, self.pipeline_version));
        let exact_finding_set = prior_findings_contract.is_some_and(is_reviewer_finding_set_input);
        if (result_contract == ReviewerResultContract::V2) != exact_finding_set {
            let error = format!(
                "reviewer `{node_id}` must pair ReviewerResult@2 with an exact FindingSet@1 input"
            );
            if let Some(prepared) = prepared.take() {
                self.release_prepared_attempt(
                    node_id,
                    &prepared.attempt,
                    prepared.reservation.as_ref(),
                    &error,
                )?;
            }
            return Err(error);
        }
        let prior_findings_port = prior_findings_contract.map(|port| port.name.as_str());
        let prior_findings_artifact = prior_findings_port
            .and_then(|port| node_inputs.get(port))
            .and_then(|artifacts| artifacts.first())
            .cloned();
        let mut inputs = ReviewerInputs {
            result_contract,
            finding_identity_policy: Some(self.authority.finding_identity_policy.clone()),
            ..ReviewerInputs::default()
        };
        let resolved_inputs = (|| -> Result<(), String> {
            for (port, artifacts) in node_inputs {
                let contract = node
                    .inputs
                    .iter()
                    .find(|contract| contract.name == *port)
                    .ok_or_else(|| {
                        format!("reviewer input port '{port}' has no declared contract")
                    })?;
                if is_reviewer_prior_set_input(contract, self.pipeline_version) {
                    continue;
                }
                let is_change_set = is_change_set_port(contract, self.pipeline_version);
                let mut resolved = Vec::with_capacity(artifacts.len());
                for artifact in artifacts {
                    if is_change_set
                        && self.authority.change_set_id.as_deref() == Some(artifact.as_str())
                    {
                        resolved.push(ReviewerInputArtifact::from_resolved_change_set(
                            self.authority
                                .change_set
                                .as_ref()
                                .ok_or("Round authority has no validated Change Set input")?
                                .clone(),
                        )?);
                        continue;
                    }
                    let limit = if is_change_set {
                        MAX_CHANGE_SET_BYTES
                    } else {
                        MAX_PRIOR_FINDINGS_BYTES
                    };
                    let encoded = self
                        .cas
                        .get_bounded(artifact, limit as u64)
                        .map_err(|error| error.to_string())?;
                    if is_change_set {
                        resolved.push(ReviewerInputArtifact::change_set_from_encoded(
                            artifact.clone(),
                            &encoded,
                        )?);
                    } else {
                        let value =
                            serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
                        resolved.push(ReviewerInputArtifact::from_json(
                            artifact.clone(),
                            contract.artifact_type.clone(),
                            value,
                            encoded.len(),
                        ));
                    }
                }
                inputs.artifacts.insert(port.clone(), resolved);
            }
            Ok(())
        })();
        if let Err(error) = resolved_inputs {
            if let Some(prepared) = prepared.take() {
                self.release_prepared_attempt(
                    node_id,
                    &prepared.attempt,
                    prepared.reservation.as_ref(),
                    &error,
                )?;
            }
            return Err(error);
        }
        if let Some(artifact) = &prior_findings_artifact {
            let encoded = match self
                .cas
                .get_bounded(artifact, MAX_PRIOR_FINDINGS_BYTES as u64)
            {
                Ok(encoded) => encoded,
                Err(error) => {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error.to_string(),
                        )?;
                    }
                    return Err(error.to_string());
                }
            };
            let value: serde_json::Value = match serde_json::from_slice(&encoded) {
                Ok(value) => value,
                Err(error) => {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error.to_string(),
                        )?;
                    }
                    return Err(error.to_string());
                }
            };
            let value = if exact_finding_set {
                let envelope: review_core::ArtifactEnvelope = match serde_json::from_value(value) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        let error = format!(
                            "exact prior FindingSet@1 `{artifact}` is not an envelope: {error}"
                        );
                        if let Some(prepared) = prepared.take() {
                            self.release_prepared_attempt(
                                node_id,
                                &prepared.attempt,
                                prepared.reservation.as_ref(),
                                &error,
                            )?;
                        }
                        return Err(error);
                    }
                };
                if let Err(error) = review_store::validate_envelope(&envelope) {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error,
                        )?;
                    }
                    return Err(error);
                }
                if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                    let error = format!("exact prior artifact `{artifact}` is not FindingSet@1");
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error,
                        )?;
                    }
                    return Err(error);
                }
                let mut set: review_core::FindingSetV1 =
                    match serde_json::from_value(envelope.payload) {
                        Ok(set) => set,
                        Err(error) => {
                            let error = format!("exact prior FindingSet@1 is invalid: {error}");
                            if let Some(prepared) = prepared.take() {
                                self.release_prepared_attempt(
                                    node_id,
                                    &prepared.attempt,
                                    prepared.reservation.as_ref(),
                                    &error,
                                )?;
                            }
                            return Err(error);
                        }
                    };
                if let Err(error) = set.validate() {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error,
                        )?;
                    }
                    return Err(error);
                }
                let round_assignment = match self.cas.get_json(&self.authority.prior_finding_set_id)
                {
                    Ok(assignment) => assignment,
                    Err(error) => {
                        let error =
                            format!("exact Round finding assignment is unreadable: {error}");
                        if let Some(prepared) = prepared.take() {
                            self.release_prepared_attempt(
                                node_id,
                                &prepared.attempt,
                                prepared.reservation.as_ref(),
                                &error,
                            )?;
                        }
                        return Err(error);
                    }
                };
                let assignment_node = self.reviewer_binding_node(node_id);
                if let Err(error) =
                    retain_round_assignment(&mut set, &round_assignment, &assignment_node)
                {
                    if let Some(prepared) = prepared.take() {
                        self.release_prepared_attempt(
                            node_id,
                            &prepared.attempt,
                            prepared.reservation.as_ref(),
                            &error,
                        )?;
                    }
                    return Err(error);
                }
                serde_json::to_value(set).expect("validated FindingSet@1 serializes")
            } else {
                value
            };
            // An empty assignment needs no prompt section and requires an empty disposition list.
            let findings_field = if exact_finding_set {
                "findings"
            } else {
                "prior_findings"
            };
            let has_findings = value
                .get(findings_field)
                .and_then(|findings| findings.as_array())
                .is_some_and(|findings| !findings.is_empty());
            if has_findings {
                inputs.prior_findings = Some(value);
            }
        }
        inputs.prior_findings_artifact_id = prior_findings_artifact.clone();

        let mut retry_failures: Vec<String> = Vec::new();
        let broker_fence_authority = self
            .reviewer_execution
            .get(&binding_node)
            .filter(|execution| {
                execution.credential_mode == review_core::BrokerCredentialModeV1::Brokered
            })
            .map(|execution| review_core::broker_authority_usage(&execution.operations))
            .transpose()?
            .unwrap_or(0);
        for _ in 0..=self.timeout_retries {
            // The scheduler prepares the first attempt in plan order before spawning this
            // worker. Retries are prepared here only after the predecessor is terminal.
            let PreparedReviewerAttempt {
                attempt,
                reservation,
                refusal_history_id,
            } = match prepared.take() {
                Some(prepared) => prepared,
                None => self.prepare_reviewer_attempt(
                    node_id,
                    prior_findings_artifact.as_ref(),
                    &retry_failures,
                )?,
            };

            inputs.refusal_history_artifact_id = refusal_history_id.clone();
            inputs.refused_attempts = match refusal_history_id.as_ref() {
                Some(refusal_history_id) => {
                    let decoded = self
                        .cas
                        .get_json(refusal_history_id)
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            serde_json::from_value(value).map_err(|error| error.to_string())
                        });
                    match decoded {
                        Ok(history) => history,
                        Err(error) => {
                            self.release_prepared_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &error,
                            )?;
                            return Err(error);
                        }
                    }
                }
                None => Vec::new(),
            };
            retry_failures.clone_from(&inputs.refused_attempts);
            inputs.attempt_context = Some(ReviewerAttemptContext {
                attempt_id: attempt.to_string(),
                round: self.authority.round,
                epoch: self.authority.epoch,
                subject_id: self.authority.subject_id.clone(),
                head_snapshot_id: self.authority.head_snapshot_id.clone(),
                campaign_manifest_id: self.authority.campaign_manifest_id.clone(),
                reviewer_package_artifact_id: self
                    .authority
                    .reviewer_packages
                    .get(&binding_node)
                    .map(|(artifact_id, _)| artifact_id.clone()),
                reviewer_package_digest: self
                    .authority
                    .reviewer_packages
                    .get(&binding_node)
                    .map(|(_, digest)| digest.clone()),
                policy_ids: self.authority.policy_ids.clone(),
                reserved_tokens: reservation.as_ref().map(|reservation| reservation.amount),
            });

            let boundary = KernelBrokerBoundary { kernel: self };
            let brokered = (|| -> Result<Option<Broker<'_>>, String> {
                let Some(execution) = self.reviewer_execution.get(&binding_node) else {
                    return Ok(None);
                };
                let lease_epoch = self
                    .attempts
                    .lock()
                    .expect("attempt ledger")
                    .attempt(&attempt)
                    .and_then(|attempt| attempt.epoch.checked_add(1))
                    .ok_or_else(|| "reviewer Attempt has no broker lease epoch".to_string())?;
                let mut broker = None;
                let broker_handle = if execution.credential_mode == BrokerCredentialModeV1::Brokered
                {
                    let provider = self.broker_providers.get(&binding_node).ok_or_else(|| {
                        format!("brokered reviewer `{node_id}` has no machine-local provider")
                    })?;
                    let issued = Broker::issue(
                        BrokerLeaseV1 {
                            campaign_id: self.run_id.clone(),
                            round_event_id: self.authority.round_event_id.clone(),
                            node_id: node_id.to_string(),
                            attempt_id: attempt.to_string(),
                            lease_epoch,
                        },
                        execution.operations.clone(),
                        Credential::new(provider.credential.clone())
                            .map_err(|error| error.to_string())?,
                        &boundary,
                        provider.connector.as_ref(),
                        &boundary,
                    )
                    .map_err(|error| error.to_string())?;
                    let handle = issued.handle().as_str().to_string();
                    broker = Some(issued);
                    Some(handle)
                } else {
                    None
                };
                let binding = ReviewerExecutionBindingV1 {
                    node: node_id.to_string(),
                    attempt_id: attempt.to_string(),
                    lease_epoch,
                    credential_mode: execution.credential_mode,
                    auto_apply: execution.auto_apply,
                    broker_handle,
                    operations: execution.operations.clone(),
                    admitted: true,
                };
                self.append(
                    NewEvent::new(
                        EventType::ReviewerExecutionBoundV1,
                        serde_json::to_value(binding).map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .attempt(attempt.to_string()),
                )?;
                Ok(broker)
            })();
            let broker = match brokered {
                Ok(broker) => broker,
                Err(error) => {
                    self.release_prepared_attempt(node_id, &attempt, reservation.as_ref(), &error)?;
                    return Err(error);
                }
            };

            // Each attempt gets its own fresh sandbox. Reviewers may edit freely — a TDD
            // reviewer must — and nothing they do can reach a sibling, the source, the
            // snapshot, or a retry of themselves.
            let sandbox = match self.sandbox(Mode::EphemeralWrite) {
                Ok(sandbox) => sandbox,
                Err(error) => {
                    self.release_prepared_attempt(node_id, &attempt, reservation.as_ref(), &error)?;
                    return Err(error);
                }
            };

            let wall_started = SystemTime::now();
            let wall_clock = Instant::now();
            let invoked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                adapter.invoke_with_broker(
                    self.cas,
                    sandbox.root(),
                    &inputs,
                    broker.as_ref().map(|broker| broker as &dyn BrokerClient),
                )
            }));
            let broker_charged = broker.as_ref().map_or(0, Broker::charged_usage);
            let invoked = match invoked {
                Ok(invoked) => invoked,
                Err(_) => {
                    let error = format!("reviewer adapter panicked for node {node_id}");
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
                    self.fail_started_attempt(
                        node_id,
                        &attempt,
                        reservation.as_ref(),
                        &error,
                        charged,
                        AttemptFailureEvidence::default(),
                    )?;
                    return Err(error);
                }
            };

            // Wall-clock and provider usage live beside the event stream, never in it: identity,
            // replay, the Ledger, and convergence ignore them; people read them through
            // `af review report` and `af review campaigns`.
            self.record_attempt_wall(
                node_id,
                &attempt,
                wall_started,
                wall_clock.elapsed(),
                invoked.as_ref().ok().map(|receipted| &receipted.usage),
            );

            match invoked {
                Ok(receipted) => {
                    let reported_charge = receipted
                        .returned
                        .cost_tokens
                        .max(receipted.usage.chargeable_tokens);
                    if broker.is_some()
                        && (receipted.returned.cost_tokens != broker_charged
                            || receipted.usage.chargeable_tokens != broker_charged)
                    {
                        let error = format!(
                            "brokered reviewer usage mismatch: Broker charged {broker_charged}, adapter reported cost_tokens={} and chargeable_tokens={}",
                            receipted.returned.cost_tokens, receipted.usage.chargeable_tokens
                        );
                        self.fail_started_attempt(
                            node_id,
                            &attempt,
                            reservation.as_ref(),
                            &error,
                            broker_charged.max(reported_charge),
                            AttemptFailureEvidence {
                                raw_artifact: Some(&receipted.returned.raw_artifact),
                                refusal_history: None,
                            },
                        )?;
                        return Err(error);
                    }
                    let returned = receipted.returned;
                    let proposal_declaration = returned.proposal;
                    let assigned_finding_ids = inputs
                        .prior_findings
                        .as_ref()
                        .and_then(|value| value.get("findings"))
                        .and_then(serde_json::Value::as_array)
                        .map(|findings| {
                            findings
                                .iter()
                                .filter_map(|finding| finding.get("finding_id"))
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let result_value = match reviewer_result_value(
                        &returned.output,
                        result_contract,
                        &assigned_finding_ids,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            retry_failures.push(failed_retry_context(
                                &attempt.to_string(),
                                "contract_error",
                                Some(error.code()),
                            ));
                            let detail = error.to_string();
                            self.fail_started_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &detail,
                                returned.cost_tokens.max(broker_charged),
                                AttemptFailureEvidence {
                                    raw_artifact: Some(&returned.raw_artifact),
                                    refusal_history: Some(&retry_failures),
                                },
                            )?;
                            continue;
                        }
                    };
                    let artifacts = (|| -> Result<(String, String, PreparedProposal), String> {
                        let sealed = sandbox.seal().map_err(|error| error.to_string())?;
                        let result_artifact = self
                            .cas
                            .put_json(&result_value)
                            .map_err(|error| error.to_string())?;
                        let proposal = self.prepare_proposal(
                            node_id,
                            &attempt,
                            &result_artifact,
                            proposal_declaration,
                            &assigned_finding_ids,
                            returned.output.findings.len(),
                            &sealed,
                        )?;
                        // The mutation set can be enormous — a reviewer that built to verify a
                        // claim leaves a whole target/ behind. The full list lives once in the
                        // CAS; provenance carries only a bounded summary.
                        let mutations_artifact = self
                            .cas
                            .put_json(&serde_json::json!({
                                "added": sealed.mutations.added,
                                "modified": sealed.mutations.modified,
                                "deleted": sealed.mutations.deleted,
                            }))
                            .map_err(|error| error.to_string())?;
                        let mutation_summary =
                            mutation_summary(&sealed.mutations, &mutations_artifact);
                        let provenance_artifact = self
                            .cas
                            .put_json(&serde_json::json!({
                                "node": node_id,
                                "attempt": attempt.to_string(),
                                "result_artifact": result_artifact,
                                "cost_tokens": returned.cost_tokens,
                                "usage": receipted.usage,
                                "context_manifest": receipted.context_manifest,
                                "raw": returned.raw_artifact,
                                "sandbox_mutations": mutation_summary,
                            }))
                            .map_err(|error| error.to_string())?;
                        Ok((result_artifact, provenance_artifact, proposal))
                    })();
                    let (result_artifact, provenance_artifact, proposal) = match artifacts {
                        Ok(artifacts) => artifacts,
                        Err(error) => {
                            self.fail_started_attempt(
                                node_id,
                                &attempt,
                                reservation.as_ref(),
                                &error,
                                returned.cost_tokens.max(broker_charged),
                                AttemptFailureEvidence {
                                    raw_artifact: Some(&returned.raw_artifact),
                                    refusal_history: None,
                                },
                            )?;
                            return Err(error);
                        }
                    };

                    // Selection is recorded only after the complete receipted output exists.
                    let selection = self
                        .attempts
                        .lock()
                        .expect("attempt ledger")
                        .admit(&Receipt {
                            attempt: attempt.clone(),
                            output: returned.raw_artifact.clone(),
                            cost: returned.cost_tokens,
                        });
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, returned.cost_tokens);
                    }
                    let admitted = NewEvent::new(
                        EventType::AttemptAdmittedV1,
                        serde_json::to_value(AttemptAdmittedPayloadV1 {
                            selection: match selection {
                                Selection::Selected => "selected",
                                Selection::Quarantined => "quarantined",
                            }
                            .to_string(),
                            cost_tokens: returned.cost_tokens,
                            result_artifact: Some(result_artifact.clone()),
                            provenance_artifact: Some(provenance_artifact.clone()),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .attempt(attempt.to_string())
                    .referencing(vec![
                        result_artifact.clone(),
                        provenance_artifact,
                        returned.raw_artifact.clone(),
                    ]);
                    if selection == Selection::Quarantined {
                        self.append(admitted)?;
                        return Err(format!(
                            "attempt {attempt} was fenced; its late result is quarantined"
                        ));
                    }
                    if self
                        .reviewer_selections
                        .lock()
                        .expect("reviewer selections")
                        .insert(
                            node_id.to_string(),
                            SelectedReviewer {
                                attempt_id: attempt.to_string(),
                                result_artifact: result_artifact.clone(),
                                proposal_candidate: match &proposal {
                                    PreparedProposal::Prepared {
                                        candidate_artifact, ..
                                    } => Some(candidate_artifact.clone()),
                                    PreparedProposal::None | PreparedProposal::Refused(_) => None,
                                },
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("reviewer {node_id} selected more than one attempt"));
                    }
                    self.buffer_reviewer_event(node_id, admitted);
                    match proposal {
                        PreparedProposal::None => {}
                        PreparedProposal::Prepared { event, .. }
                        | PreparedProposal::Refused(event) => {
                            self.buffer_reviewer_event(node_id, event)
                        }
                    }
                    return Ok(vec![result_artifact]);
                }
                Err(RunnerError::MalformedOutput { raw_artifact, why }) => {
                    let error = format!(
                        "reviewer output is not a {}: {why}",
                        result_contract.artifact_type()
                    );
                    retry_failures.push(failed_retry_context(
                        &attempt.to_string(),
                        "parse_error",
                        None,
                    ));
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
                    self.fail_started_attempt(
                        node_id,
                        &attempt,
                        reservation.as_ref(),
                        &error,
                        charged,
                        AttemptFailureEvidence {
                            raw_artifact: Some(&raw_artifact),
                            refusal_history: Some(&retry_failures),
                        },
                    )?;
                    continue;
                }
                Err(RunnerError::TimedOut {
                    after_ms,
                    raw_artifact,
                }) => {
                    // Fence, charge, retry. The killed process's true spend is unreportable,
                    // so the full reservation is charged — the conservative reading of "a
                    // fenced attempt charges", and the one that keeps a hang from being a
                    // free retry.
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged)
                        .max(broker_fence_authority);
                    self.attempts.lock().expect("attempt ledger").fence(node_id);
                    self.attempts
                        .lock()
                        .expect("attempt ledger")
                        .charge(&attempt, charged);
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, charged);
                    }
                    let reason = format!("timed out after {after_ms}ms");
                    retry_failures.push(fenced_retry_context(&attempt.to_string(), &reason));
                    let fenced = NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::json!({
                            "reason": reason,
                            "charged": reservation
                                .as_ref()
                                .map(|_| charged)
                                .or((broker_charged > 0).then_some(charged)),
                        }),
                    )
                    .node(node_id)
                    .attempt(attempt.to_string())
                    .referencing(raw_artifact.into_iter().collect());
                    let feedback = self.feedback_event(node_id, &attempt, &retry_failures)?;
                    self.append_batch(&[fenced, feedback])?;
                }
                Err(error @ (RunnerError::Refused(_) | RunnerError::Unavailable(_))) => {
                    if broker_charged > 0 {
                        let detail = error.to_string();
                        self.fail_started_attempt(
                            node_id,
                            &attempt,
                            reservation.as_ref(),
                            &detail,
                            broker_charged,
                            AttemptFailureEvidence::default(),
                        )?;
                        return Err(detail);
                    }
                    // No Broker operation or model execution spent anything, so release rather
                    // than turning a structural refusal into a charge.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .release(reservation);
                    }
                    self.append(
                        NewEvent::new(
                            EventType::AttemptReleasedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "released": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    )?;
                    return Err(error.to_string());
                }
                Err(error) => {
                    // Failed: the reviewer did execute, its spend is unreported, and forgiving
                    // it would make crashing cheaper than answering. Full reservation, same
                    // rule as a timeout. Malformed answers took the durable correction loop above.
                    let charged = reservation
                        .as_ref()
                        .map_or(broker_charged, |reservation| reservation.amount)
                        .max(broker_charged);
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, charged);
                    }
                    self.attempts
                        .lock()
                        .expect("attempt ledger")
                        .charge(&attempt, charged);
                    self.append(
                        NewEvent::new(
                            EventType::AttemptFailedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "charged": reservation
                                    .as_ref()
                                    .map(|_| charged)
                                    .or((broker_charged > 0).then_some(charged)),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    )?;
                    return Err(error.to_string());
                }
            }
        }
        Err(format!(
            "every reviewer attempt failed: {}",
            retry_failures.join("; ")
        ))
    }
}

fn reviewer_result_value(
    stage: &LegacyStageOutput,
    contract: ReviewerResultContract,
    assigned_finding_ids: &[String],
) -> Result<serde_json::Value, ReviewerResultRejection> {
    let mut object =
        match serde_json::to_value(stage).map_err(|_| ReviewerResultRejection::ReportPayload)? {
            serde_json::Value::Object(object) => object,
            _ => return Err(ReviewerResultRejection::NotObject),
        };
    let reports = object
        .remove("findings")
        .ok_or(ReviewerResultRejection::UnexpectedFields)?;
    object.insert("reports".into(), reports);
    let entries = object
        .get_mut("disputes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(ReviewerResultRejection::MalformedDispute)?;
    for entry in entries.iter_mut() {
        let entry = entry.as_object_mut().ok_or(match contract {
            ReviewerResultContract::V1 => ReviewerResultRejection::MalformedDispute,
            ReviewerResultContract::V2 => ReviewerResultRejection::MalformedDisposition,
        })?;
        let finding_id = entry.remove("fp").ok_or(match contract {
            ReviewerResultContract::V1 => ReviewerResultRejection::InvalidDispute,
            ReviewerResultContract::V2 => ReviewerResultRejection::InvalidDisposition,
        })?;
        let key = match contract {
            ReviewerResultContract::V1 => "claim_id",
            ReviewerResultContract::V2 => "finding_id",
        };
        entry.insert(key.into(), finding_id);
        let valid = match contract {
            ReviewerResultContract::V1 => matches!(
                entry.get("position").and_then(serde_json::Value::as_str),
                Some("confirm" | "refute")
            ),
            ReviewerResultContract::V2 => matches!(
                entry.get("position").and_then(serde_json::Value::as_str),
                Some("corroborate" | "not_reproduced" | "dispute")
            ),
        };
        if !valid {
            return Err(match contract {
                ReviewerResultContract::V1 => ReviewerResultRejection::InvalidDispute,
                ReviewerResultContract::V2 => ReviewerResultRejection::InvalidDisposition,
            });
        }
    }
    if contract == ReviewerResultContract::V2 {
        let dispositions = object
            .remove("disputes")
            .ok_or(ReviewerResultRejection::MalformedDisposition)?;
        object.insert("dispositions".into(), dispositions);
    }
    let value = serde_json::Value::Object(object);
    match contract {
        ReviewerResultContract::V1 => {
            review_core::validate_reviewer_result_classified(&value)?;
        }
        ReviewerResultContract::V2 => {
            review_core::validate_reviewer_result_v2_classified(&value)?;
            let expected: BTreeSet<_> = assigned_finding_ids.iter().map(String::as_str).collect();
            let dispositions = value["dispositions"]
                .as_array()
                .expect("ReviewerResult@2 validator checked dispositions");
            let mut actual = BTreeSet::new();
            for disposition in dispositions {
                let finding_id = disposition["finding_id"]
                    .as_str()
                    .expect("ReviewerResult@2 validator checked finding_id");
                if !actual.insert(finding_id) {
                    return Err(ReviewerResultRejection::DuplicateDisposition);
                }
                if !expected.contains(finding_id) {
                    return Err(ReviewerResultRejection::UnassignedDisposition);
                }
            }
            if actual != expected {
                return Err(ReviewerResultRejection::MissingDispositionCoverage);
            }
        }
    }
    Ok(value)
}

pub(crate) fn reviewer_stage_output(
    value: serde_json::Value,
) -> Result<(ReviewerResultContract, LegacyStageOutput), String> {
    let contract = match (
        value.get("disputes").is_some(),
        value.get("dispositions").is_some(),
    ) {
        (true, false) => ReviewerResultContract::V1,
        (false, true) => ReviewerResultContract::V2,
        _ => return Err("reviewer result has ambiguous versioned disposition fields".into()),
    };
    match contract {
        ReviewerResultContract::V1 => review_core::validate_reviewer_result(&value)?,
        ReviewerResultContract::V2 => review_core::validate_reviewer_result_v2(&value)?,
    }
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        _ => return Err("ReviewerResult is not an object".into()),
    };
    let reports = object
        .remove("reports")
        .ok_or("ReviewerResult has no reports array")?;
    object.insert("findings".into(), reports);
    if contract == ReviewerResultContract::V2 {
        let mut dispositions = object
            .remove("dispositions")
            .ok_or("ReviewerResult@2 has no dispositions array")?;
        for disposition in dispositions
            .as_array_mut()
            .ok_or("ReviewerResult@2 dispositions is not an array")?
        {
            let disposition = disposition
                .as_object_mut()
                .ok_or("ReviewerResult@2 disposition is not an object")?;
            let finding_id = disposition
                .remove("finding_id")
                .ok_or("ReviewerResult@2 disposition has no finding_id")?;
            disposition.insert("fp".into(), finding_id);
        }
        object.insert("disputes".into(), dispositions);
    }
    serde_json::from_value(serde_json::Value::Object(object))
        .map(|stage| (contract, stage))
        .map_err(|error| error.to_string())
}

/// Reduce the exact FindingSet@1 to what `node` must examine this Round: the rows the Round's
/// pinned assignment document partitions to it under `assignments` — or, for a Round started
/// before partitions existed, the whole Round-wide union, which stays the frozen delivery for
/// those Rounds. A dynamic shard passes its Scatter's id, the node the partition is keyed by.
fn retain_round_assignment(
    set: &mut review_core::FindingSetV1,
    round_assignment: &serde_json::Value,
    node: &str,
) -> Result<(), String> {
    let rows = round_assignment
        .get("prior_findings")
        .and_then(serde_json::Value::as_array)
        .ok_or("Round prior Finding assignment does not contain a prior_findings array")?;
    let mut assigned = BTreeSet::new();
    for row in rows {
        let key = row
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or("Round prior Finding assignment contains a row without a key")?;
        if !assigned.insert(key) {
            return Err("Round prior Finding assignment contains a duplicate key".into());
        }
    }
    let available: BTreeSet<_> = set
        .findings
        .iter()
        .map(|finding| finding.finding_id.as_str())
        .collect();
    if !assigned.is_subset(&available) {
        return Err(
            "Round prior Finding assignment is not a subset of its exact FindingSet@1".into(),
        );
    }
    let retained: BTreeSet<&str> = match round_assignment.get("assignments") {
        None => assigned,
        Some(assignments) => {
            let partition = assignments
                .as_object()
                .ok_or("Round prior Finding assignments are not an object")?;
            let mine = partition
                .get(node)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    format!("Round prior Finding assignment has no partition for reviewer `{node}`")
                })?;
            let mut retained = BTreeSet::new();
            for key in mine {
                let key = key.as_str().ok_or_else(|| {
                    format!("Round prior Finding assignment for `{node}` contains a non-string key")
                })?;
                if !assigned.contains(key) {
                    return Err(format!(
                        "Round prior Finding assignment for `{node}` names a Finding outside the Round assignment"
                    ));
                }
                retained.insert(key);
            }
            retained
        }
    };
    set.findings
        .retain(|finding| retained.contains(finding.finding_id.as_str()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_finding_set_is_filtered_by_the_pinned_round_assignment() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let entry = |finding_id: String| review_core::FindingSetEntryV1 {
            finding_id,
            status: "open".into(),
            severity: review_core::Severity::Major,
            effective_severity: Some(review_core::Severity::Major),
            scope: "in".into(),
            file: Some("src/lib.rs".into()),
            line: Some(1),
            location_unrecorded: false,
            title: "claim".into(),
            body: "body".into(),
            fix: Some("fix".into()),
            confidence: Some(0.9),
            source: "correctness".into(),
            last_seen_round: 1,
            report_ids: vec![digest('d')],
        };
        let keep = digest('a');
        let declined = digest('b');
        let diagnostic = digest('c');
        let mut set = review_core::FindingSetV1 {
            subject_id: digest('d'),
            round: 1,
            prior_finding_set_id: digest('e'),
            reducer_version: review_core::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: vec![entry(keep.clone()), entry(declined), entry(diagnostic)],
        };

        retain_round_assignment(
            &mut set,
            &serde_json::json!({
                "subject_id": digest('f'),
                "round": 2,
                "prior_findings": [{"key": keep}]
            }),
            "correctness",
        )
        .unwrap();

        assert_eq!(set.findings.len(), 1);
        assert_eq!(set.findings[0].finding_id, keep);
    }

    /// With a partition beside the union, a reviewer is delivered only its own rows; a Round
    /// without one (started before partitions existed) still delivers the whole union; a node
    /// the partition does not name is refused rather than silently given everything.
    #[test]
    fn a_partitioned_round_assignment_delivers_only_the_reviewers_own_rows() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let entry = |finding_id: String, source: &str| review_core::FindingSetEntryV1 {
            finding_id,
            status: "open".into(),
            severity: review_core::Severity::Major,
            effective_severity: Some(review_core::Severity::Major),
            scope: "in".into(),
            file: Some("src/lib.rs".into()),
            line: Some(1),
            location_unrecorded: false,
            title: "claim".into(),
            body: "body".into(),
            fix: Some("fix".into()),
            confidence: Some(0.9),
            source: source.into(),
            last_seen_round: 1,
            report_ids: vec![digest('d')],
        };
        let architecture_row = digest('a');
        let performance_row = digest('b');
        let orphan_row = digest('c');
        let full_set = || review_core::FindingSetV1 {
            subject_id: digest('d'),
            round: 2,
            prior_finding_set_id: digest('e'),
            reducer_version: review_core::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: vec![
                entry(architecture_row.clone(), "architecture"),
                entry(performance_row.clone(), "performance"),
                entry(orphan_row.clone(), "imported"),
            ],
        };
        let partitioned = serde_json::json!({
            "subject_id": digest('f'),
            "round": 2,
            "prior_findings": [
                {"key": architecture_row, "source": "architecture"},
                {"key": performance_row, "source": "performance"},
                {"key": orphan_row, "source": "imported"},
            ],
            "assignments": {
                "architecture": [architecture_row, orphan_row],
                "performance": [performance_row, orphan_row],
            },
        });

        let mut set = full_set();
        retain_round_assignment(&mut set, &partitioned, "architecture").unwrap();
        assert_eq!(
            set.findings
                .iter()
                .map(|finding| finding.finding_id.clone())
                .collect::<Vec<_>>(),
            vec![architecture_row.clone(), orphan_row.clone()]
        );

        let mut set = full_set();
        retain_round_assignment(&mut set, &partitioned, "performance").unwrap();
        assert_eq!(
            set.findings
                .iter()
                .map(|finding| finding.finding_id.clone())
                .collect::<Vec<_>>(),
            vec![performance_row.clone(), orphan_row.clone()]
        );

        let error = retain_round_assignment(&mut full_set(), &partitioned, "tests").unwrap_err();
        assert!(
            error.contains("no partition for reviewer `tests`"),
            "{error}"
        );

        let mut legacy = partitioned.clone();
        legacy.as_object_mut().unwrap().remove("assignments");
        let mut set = full_set();
        retain_round_assignment(&mut set, &legacy, "architecture").unwrap();
        assert_eq!(
            set.findings.len(),
            3,
            "a Round without a partition keeps the union"
        );
    }

    #[test]
    fn flat_reviewer_reports_reach_the_legacy_reducer() {
        let (contract, output) = reviewer_stage_output(serde_json::json!({
            "verdict": "request-changes",
            "summary": null,
            "reports": [{
                "severity": "major",
                "file": "src/a.rs",
                "line": 1,
                "title": "flat claim",
                "body": "body",
                "fix": "fix",
                "confidence": 0.9
            }],
            "benchmark_demands": [],
            "disputes": [{
                "claim_id": "prior",
                "position": "refute",
                "reason": "not reproduced"
            }]
        }))
        .unwrap();
        assert_eq!(contract, ReviewerResultContract::V1);
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].file, "src/a.rs");
        assert_eq!(output.disputes[0].fp, "prior");
    }

    #[test]
    fn reviewer_result_v2_requires_exact_disposition_coverage() {
        let stage = |ids: &[&str]| LegacyStageOutput {
            verdict: review_core::legacy::LegacyVerdict::Approve,
            summary: None,
            findings: Vec::new(),
            benchmark_demands: Vec::new(),
            disputes: ids
                .iter()
                .map(|id| review_core::legacy::LegacyDispute {
                    fp: (*id).into(),
                    position: "not_reproduced".into(),
                    reason: "the current Subject no longer reaches the failing branch".into(),
                })
                .collect(),
        };
        let assigned = vec!["finding:a".to_string(), "finding:b".to_string()];

        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::MissingDispositionCoverage
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::DuplicateDisposition
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:outside"]),
                ReviewerResultContract::V2,
                &assigned,
            )
            .unwrap_err(),
            ReviewerResultRejection::UnassignedDisposition
        );

        let value = reviewer_result_value(
            &stage(&["finding:b", "finding:a"]),
            ReviewerResultContract::V2,
            &assigned,
        )
        .unwrap();
        assert!(value.get("disputes").is_none());
        assert_eq!(value["dispositions"].as_array().unwrap().len(), 2);
    }
}
