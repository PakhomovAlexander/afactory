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
        // Which of the delivered Findings this node *owes* a disposition for. Membership and
        // coverage are deliberately different sets: every reviewer is delivered the whole Round
        // union and may name any of it — that is the only route a peer's wrong claim reaches
        // `contested` — while only its own rows are required back.
        let mut required_finding_ids: BTreeSet<String> = BTreeSet::new();
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
                if let Err(error) = retain_round_assignment(&mut set, &round_assignment) {
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
                // A dynamic shard owes what its Scatter owes: coverage is keyed by the static
                // node the Round's rows name as their `source`.
                let assignment_node = self.reviewer_binding_node(node_id);
                // Cloned, not held: the coverage decision must not keep a lock across the
                // failure path below.
                let orphaned = self
                    .orphaned_prior_sources
                    .lock()
                    .expect("orphaned prior sources")
                    .clone();
                match round_coverage(
                    &round_assignment,
                    &self.prior_finding_receivers,
                    &orphaned,
                    &assignment_node,
                ) {
                    Ok(coverage) => required_finding_ids = coverage,
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

        // Every Finding the Round delivered to this node: what a disposition or a Proposal claim
        // may name. This is the whole Round union, as it was before the delivered set was ever
        // narrowed, so a reviewer can still `dispute` a peer's claim and attach a fix to it.
        let permitted_finding_ids: Vec<String> = inputs
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
        required_finding_ids.retain(|id| permitted_finding_ids.iter().any(|kept| kept == id));
        // State the coverage partition in the Worker's own context. Without it a reviewer cannot
        // tell its rows from a peer's: a row's `source` names the reporting node, and nothing
        // delivered tells a Worker its own node id or which sources are orphan this Round. The
        // shared prior-Finding artifact is untouched — this is keys only, beside it.
        if exact_finding_set && inputs.prior_findings.is_some() {
            inputs.required_finding_ids =
                Some(required_finding_ids.iter().cloned().collect::<Vec<_>>());
        }

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
                    let result_value = match reviewer_result_value(
                        &returned.output,
                        result_contract,
                        &permitted_finding_ids,
                        &required_finding_ids,
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
                            &permitted_finding_ids,
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

/// Admit one returned reviewer result. For `ReviewerResult@2` the two prior-Finding sets are
/// deliberately different: `permitted_finding_ids` is every Finding the Round delivered (the
/// whole union — a disposition may name a peer's claim, which is how a Dispute reaches
/// `contested` at all), while `required_finding_ids` is only this node's own partition of that
/// union, which it must return in full. Membership is the union; coverage is the partition.
fn reviewer_result_value(
    stage: &LegacyStageOutput,
    contract: ReviewerResultContract,
    permitted_finding_ids: &[String],
    required_finding_ids: &BTreeSet<String>,
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
            let permitted: BTreeSet<_> = permitted_finding_ids.iter().map(String::as_str).collect();
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
                // Outside the Round entirely — an invented or stale key, not a peer's claim.
                if !permitted.contains(finding_id) {
                    return Err(ReviewerResultRejection::UnassignedDisposition);
                }
            }
            if required_finding_ids
                .iter()
                .any(|required| !actual.contains(required.as_str()))
            {
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

/// Reduce the exact FindingSet@1 to the Round's pinned prior-Finding union: the rows the Round
/// document names, and nothing else. Every reviewer receives the same union — the delivered
/// input bytes are the Round's, not a per-reviewer slice — because a reviewer that cannot see a
/// peer's claim cannot dispute it, and `contested` is the only route by which peer review
/// challenges a wrong claim.
///
/// The reduction is real: the reducer's Set carries every Finding view, including the rejected,
/// wontfix, and authority-diagnostic rows the Round document deliberately leaves out. So the
/// Set-level reduction provenance — `selected_report_ids`, `relation_ids`, `resolution_ids` —
/// describes the reduction, not this projection of it, and would otherwise name Findings the
/// delivered document does not carry and a sandbox cannot dereference. When anything is
/// dropped, those lists go with it; when the union is the whole reduction, they are untouched.
fn retain_round_assignment(
    set: &mut review_core::FindingSetV1,
    round_assignment: &serde_json::Value,
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
    let reduced = assigned.len() < set.findings.len();
    set.findings
        .retain(|finding| assigned.contains(finding.finding_id.as_str()));
    if reduced {
        set.selected_report_ids.clear();
        set.relation_ids.clear();
        set.resolution_ids.clear();
    }
    Ok(())
}

/// The rows `node` must return a disposition for this Round, derived — never read back from the
/// persisted document, which carries only the union (see
/// `reviewctl::authority::prior_finding_set_document`). The rule, once:
///
/// - a row belongs to the receiving node its `source` names; a Scatter shard `node#slice:…`
///   counts as its Scatter `node`, whose shards all inherit the Scatter's rows;
/// - a row whose source is no receiving node — a reviewer with no `FindingSet@1` input, a
///   legacy imported source, or a Scatter this generation already closed without one
///   `Completed` shard — is orphan, and joins *every* receiving node's coverage, so an open
///   prior Finding can never lose its disposition obligation by falling between reviewers;
/// - when the caller composed no pipeline definition, `receiving` is empty, every row is orphan
///   and the whole union is required — the conservative pre-partition obligation.
///
/// What this saves, and what it does not: the *output* obligation shrinks from R×N dispositions
/// to N across R reviewers; the delivered input bytes are unchanged, because the union is
/// delivered whole. Delivering only the partition would save those bytes too, but it would make
/// a cross-reviewer Dispute and a cross-reviewer Proposal claim structurally impossible — that
/// is a documented mechanism (see `CONTEXT.md`, **Dispute**) and needs its own ADR, so it is
/// deliberately not done here.
fn round_coverage(
    round_assignment: &serde_json::Value,
    receiving: &[String],
    orphaned: &BTreeSet<String>,
    node: &str,
) -> Result<BTreeSet<String>, String> {
    let rows = round_assignment
        .get("prior_findings")
        .and_then(serde_json::Value::as_array)
        .ok_or("Round prior Finding assignment does not contain a prior_findings array")?;
    let owns = |source: &str| {
        let base = source
            .split_once("#slice:")
            .map_or(source, |(base, _)| base);
        receiving.iter().any(|receiver| receiver == base)
            && !orphaned.contains(base)
            && base == node
    };
    let orphan = |source: &str| {
        let base = source
            .split_once("#slice:")
            .map_or(source, |(base, _)| base);
        !receiving.iter().any(|receiver| receiver == base) || orphaned.contains(base)
    };
    let mut coverage = BTreeSet::new();
    for row in rows {
        let key = row
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or("Round prior Finding assignment contains a row without a key")?;
        let source = row
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if owns(source) || orphan(source) {
            coverage.insert(key.to_string());
        }
    }
    Ok(coverage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(finding_id: String, source: &str) -> review_core::FindingSetEntryV1 {
        review_core::FindingSetEntryV1 {
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
            report_ids: vec![format!("sha256:{}", "d".repeat(64))],
        }
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    #[test]
    fn exact_finding_set_is_filtered_by_the_pinned_round_assignment() {
        let keep = digest('a');
        let declined = digest('b');
        let diagnostic = digest('c');
        let mut set = review_core::FindingSetV1 {
            subject_id: digest('d'),
            round: 1,
            prior_finding_set_id: digest('e'),
            reducer_version: review_core::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: vec![digest('f')],
            relation_ids: vec![digest('0')],
            resolution_ids: vec![digest('1')],
            findings: vec![
                entry(keep.clone(), "correctness"),
                entry(declined, "correctness"),
                entry(diagnostic, "correctness"),
            ],
        };

        retain_round_assignment(
            &mut set,
            &serde_json::json!({
                "subject_id": digest('f'),
                "round": 2,
                "prior_findings": [{"key": keep, "source": "correctness"}]
            }),
        )
        .unwrap();

        assert_eq!(set.findings.len(), 1);
        assert_eq!(set.findings[0].finding_id, keep);
        // The reduction's own provenance describes the reduction, not this projection: it named
        // two Findings the delivered document no longer carries, and a sandbox can dereference
        // none of it (ADR-0028).
        assert!(set.selected_report_ids.is_empty());
        assert!(set.relation_ids.is_empty());
        assert!(set.resolution_ids.is_empty());
    }

    /// Delivery is the Round-wide union for every reviewer, exactly as it was before the
    /// partition existed. Nothing per-reviewer is read out of the persisted document; a stray
    /// `assignments` key written by an unreleased dev build is ignored rather than obeyed.
    #[test]
    fn a_partitioned_round_assignment_delivers_only_the_reviewers_own_rows() {
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
        let union = serde_json::json!({
            "subject_id": digest('f'),
            "round": 2,
            "prior_findings": [
                {"key": architecture_row, "source": "architecture"},
                {"key": performance_row, "source": "performance"},
                {"key": orphan_row, "source": "imported"},
            ],
        });
        let mut with_stray_key = union.clone();
        with_stray_key.as_object_mut().unwrap().insert(
            "assignments".into(),
            serde_json::json!({"architecture": [architecture_row.clone()]}),
        );

        for document in [&union, &with_stray_key] {
            for node in ["architecture", "performance"] {
                let mut set = full_set();
                retain_round_assignment(&mut set, document).unwrap();
                assert_eq!(
                    set.findings
                        .iter()
                        .map(|finding| finding.finding_id.clone())
                        .collect::<Vec<_>>(),
                    vec![
                        architecture_row.clone(),
                        performance_row.clone(),
                        orphan_row.clone()
                    ],
                    "{node} sees the whole union, so it can dispute a peer's claim"
                );
            }
        }
    }

    /// Coverage is the partition: a row goes to the receiving node its `source` names (a shard
    /// to its Scatter), an orphan row to every receiving node, and with no pipeline definition
    /// at all the whole union is required — the conservative pre-partition obligation.
    #[test]
    fn prior_assignments_partition_by_source_and_send_orphans_everywhere() {
        let rows = serde_json::json!({
            "subject_id": digest('f'),
            "round": 2,
            "prior_findings": [
                {"key": "a", "source": "architecture"},
                {"key": "b", "source": "performance"},
                {"key": "c", "source": "scatter#slice:1:0123456789abcdef"},
                {"key": "d", "source": "legacy-import"},
            ],
        });
        let receiving: Vec<String> = ["architecture", "performance", "scatter"]
            .into_iter()
            .map(String::from)
            .collect();
        let none = BTreeSet::new();
        let coverage = |node| round_coverage(&rows, &receiving, &none, node).unwrap();
        assert_eq!(coverage("architecture"), keys(["a", "d"]));
        assert_eq!(coverage("performance"), keys(["b", "d"]));
        assert_eq!(coverage("scatter"), keys(["c", "d"]));

        // A Scatter that closed without one Completed shard owes nothing; its rows join the
        // coverage of the receiving nodes still to be delivered theirs.
        let dead: BTreeSet<String> = ["scatter"].into_iter().map(String::from).collect();
        assert_eq!(
            round_coverage(&rows, &receiving, &dead, "architecture").unwrap(),
            keys(["a", "c", "d"])
        );
        assert_eq!(
            round_coverage(&rows, &receiving, &dead, "performance").unwrap(),
            keys(["b", "c", "d"])
        );

        // No pipeline definition: every row is orphan, so the whole union stays required.
        assert_eq!(
            round_coverage(&rows, &[], &none, "architecture").unwrap(),
            keys(["a", "b", "c", "d"])
        );
    }

    fn keys<const N: usize>(keys: [&str; N]) -> BTreeSet<String> {
        keys.into_iter().map(String::from).collect()
    }

    /// The split the Round contract now draws: a reviewer must return its own rows and may name
    /// any Finding the Round delivered — including a peer's, which is the only route by which
    /// `dispute` moves a wrong claim to `contested` (`CONTEXT.md`, **Dispute**). Only a key
    /// outside the Round entirely is refused.
    #[test]
    fn a_disposition_may_name_a_peers_finding_while_coverage_stays_the_partition() {
        let mine = digest('a');
        let peers = digest('b');
        let stranger = digest('c');
        let result = |dispositions: serde_json::Value| {
            let (contract, output) = reviewer_stage_output(serde_json::json!({
                "verdict": "approve",
                "summary": null,
                "reports": [],
                "benchmark_demands": [],
                "dispositions": dispositions,
            }))
            .unwrap();
            let permitted = vec![mine.clone(), peers.clone()];
            let required: BTreeSet<String> = [mine.clone()].into_iter().collect();
            reviewer_result_value(&output, contract, &permitted, &required)
        };
        let disposition = |finding_id: &str, position: &str| {
            serde_json::json!({
                "finding_id": finding_id,
                "position": position,
                "reason": "checked against the current Subject",
            })
        };

        // Own row only: the obligation is N, not R×N.
        result(serde_json::json!([disposition(&mine, "not_reproduced")])).unwrap();

        // Own row plus a Dispute of a peer's claim: accepted.
        let value = result(serde_json::json!([
            disposition(&mine, "corroborate"),
            disposition(&peers, "dispute"),
        ]))
        .unwrap();
        assert_eq!(value["dispositions"].as_array().unwrap().len(), 2);

        // A peer's claim alone leaves the reviewer's own row uncovered.
        assert_eq!(
            result(serde_json::json!([disposition(&peers, "dispute")])).unwrap_err(),
            ReviewerResultRejection::MissingDispositionCoverage
        );

        // A key the Round never delivered is still refused.
        assert_eq!(
            result(serde_json::json!([
                disposition(&mine, "corroborate"),
                disposition(&stranger, "dispute"),
            ]))
            .unwrap_err(),
            ReviewerResultRejection::UnassignedDisposition
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
        // A node whose partition happens to be the whole Round: permitted and required coincide.
        let assigned = vec!["finding:a".to_string(), "finding:b".to_string()];
        let required: BTreeSet<String> = assigned.iter().cloned().collect();

        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
                &required,
            )
            .unwrap_err(),
            ReviewerResultRejection::MissingDispositionCoverage
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:a"]),
                ReviewerResultContract::V2,
                &assigned,
                &required,
            )
            .unwrap_err(),
            ReviewerResultRejection::DuplicateDisposition
        );
        assert_eq!(
            reviewer_result_value(
                &stage(&["finding:a", "finding:outside"]),
                ReviewerResultContract::V2,
                &assigned,
                &required,
            )
            .unwrap_err(),
            ReviewerResultRejection::UnassignedDisposition
        );

        let value = reviewer_result_value(
            &stage(&["finding:b", "finding:a"]),
            ReviewerResultContract::V2,
            &assigned,
            &required,
        )
        .unwrap();
        assert!(value.get("disputes").is_none());
        assert_eq!(value["dispositions"].as_array().unwrap().len(), 2);
    }
}
