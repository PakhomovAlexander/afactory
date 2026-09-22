//! Exact declared Review inputs, resolved without execution or accounting authority.

use review_core::{MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES, ReviewerResultContract};
use review_graph::{ArtifactMap, Node};
use review_runner::{ReviewerAttemptContext, ReviewerInputArtifact, ReviewerInputs};
use review_store::Cas;

use super::{
    RoundAuthority, is_change_set_port, is_reviewer_finding_set_input, is_reviewer_prior_set_input,
    retain_round_assignment, reviewer_result_contract,
};

/// Bind the execution owner's actual identity after resolving the exact declared inputs.
/// No ID allocation, budget mutation, Store mutation or Worker invocation occurs here.
pub(super) fn bind_attempt(
    inputs: &mut ReviewerInputs,
    authority: &RoundAuthority,
    binding_node: &str,
    attempt_id: &str,
    reserved_tokens: Option<u64>,
) {
    inputs.attempt_context = Some(ReviewerAttemptContext {
        attempt_id: attempt_id.into(),
        round: authority.round,
        epoch: authority.epoch,
        subject_id: authority.subject_id.clone(),
        head_snapshot_id: authority.head_snapshot_id.clone(),
        campaign_manifest_id: authority.campaign_manifest_id.clone(),
        reviewer_package_artifact_id: authority
            .reviewer_packages
            .get(binding_node)
            .map(|(id, _)| id.clone()),
        reviewer_package_digest: authority
            .reviewer_packages
            .get(binding_node)
            .map(|(_, digest)| digest.clone()),
        policy_ids: authority.policy_ids.clone(),
        reserved_tokens,
    });
}

pub(super) fn prepare(
    cas: &Cas,
    authority: &RoundAuthority,
    pipeline_version: u32,
    node: &Node,
    node_inputs: &ArtifactMap,
) -> Result<ReviewerInputs, String> {
    let node_id = node.id.as_str();
    let result_contract = reviewer_result_contract(node)?;

    // Prior findings arrive through the wired `prior_findings` input port — a data artifact
    // the pipeline routed from the generation node — not from ambient kernel state. A
    // reviewer that declares no such input receives none; the plan is the delivery.
    let prior_findings_contract = node
        .inputs
        .iter()
        .find(|port| is_reviewer_prior_set_input(port, pipeline_version));
    let exact_finding_set = prior_findings_contract.is_some_and(is_reviewer_finding_set_input);
    if (result_contract == ReviewerResultContract::V2) != exact_finding_set {
        let error = format!(
            "reviewer `{node_id}` must pair ReviewerResult@2 with an exact FindingSet@1 input"
        );
        return Err(error);
    }
    let prior_findings_port = prior_findings_contract.map(|port| port.name.as_str());
    let prior_findings_artifact = prior_findings_port
        .and_then(|port| node_inputs.get(port))
        .and_then(|artifacts| artifacts.first())
        .cloned();
    let mut inputs = ReviewerInputs {
        result_contract,
        ..ReviewerInputs::default()
    };
    let resolved_inputs = (|| -> Result<(), String> {
        for (port, artifacts) in node_inputs {
            let contract = node
                .inputs
                .iter()
                .find(|contract| contract.name == *port)
                .ok_or_else(|| format!("reviewer input port '{port}' has no declared contract"))?;
            if is_reviewer_prior_set_input(contract, pipeline_version) {
                continue;
            }
            let is_change_set = is_change_set_port(contract, pipeline_version);
            let mut resolved = Vec::with_capacity(artifacts.len());
            for artifact in artifacts {
                if is_change_set && authority.change_set_id.as_deref() == Some(artifact.as_str()) {
                    resolved.push(ReviewerInputArtifact::from_resolved_change_set(
                        authority
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
                let encoded = cas
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
    resolved_inputs?;
    if let Some(artifact) = &prior_findings_artifact {
        let encoded = match cas.get_bounded(artifact, MAX_PRIOR_FINDINGS_BYTES as u64) {
            Ok(encoded) => encoded,
            Err(error) => {
                return Err(error.to_string());
            }
        };
        let value: serde_json::Value = match serde_json::from_slice(&encoded) {
            Ok(value) => value,
            Err(error) => {
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
                    return Err(error);
                }
            };
            review_store::validate_envelope(&envelope)?;
            if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                let error = format!("exact prior artifact `{artifact}` is not FindingSet@1");
                return Err(error);
            }
            let mut set: review_core::FindingSetV1 = match serde_json::from_value(envelope.payload)
            {
                Ok(set) => set,
                Err(error) => {
                    let error = format!("exact prior FindingSet@1 is invalid: {error}");
                    return Err(error);
                }
            };
            set.validate()?;
            let round_assignment = match cas.get_json(&authority.prior_finding_set_id) {
                Ok(assignment) => assignment,
                Err(error) => {
                    let error = format!("exact Round finding assignment is unreadable: {error}");
                    return Err(error);
                }
            };
            retain_round_assignment(&mut set, &round_assignment)?;
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
    Ok(inputs)
}
