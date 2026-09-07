//! Integration admission: plan and commit authority for composing sealed Proposals into a
//! derived Snapshot (ADR-0040).

use review_core::definition::NodeKindSpec;
use rusqlite::params;

use crate::cas::Cas;
use crate::store::StoreError;
use crate::store::authority::AuthorityPlan;
use crate::store::proposals::{manifest_entries, manifest_value, validate_candidate_manifest};

fn integration_binding_node<'a>(plan: &'a AuthorityPlan, node: &'a str) -> Option<&'a str> {
    if plan.nodes.contains_key(node) {
        return Some(node);
    }
    let (owner, _) = node.split_once("#slice:")?;
    plan.nodes
        .get(owner)
        .is_some_and(|node| node.kind == NodeKindSpec::Scatter)
        .then_some(owner)
}

#[derive(Clone, Copy)]
pub(crate) struct IntegrationTarget<'a> {
    pub(crate) batch_id: &'a str,
    pub(crate) derived_snapshot_id: &'a str,
}

fn authority_paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn integration_finding_ids(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    proposal: &review_core::PatchProposal,
) -> Result<Vec<String>, StoreError> {
    let mut findings = std::collections::BTreeSet::new();
    for claim in &proposal.finding_refs {
        match claim.kind {
            review_core::ClaimRefKind::Finding => {
                let known: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events WHERE run_id = ?1
                     AND type = 'FindingReported@1' AND correlation_id = ?2",
                    params![run_id, claim.id],
                    |row| row.get(0),
                )?;
                if known == 0 {
                    return Err(StoreError::Conflict(
                        "Integration Proposal names an unknown Finding".into(),
                    ));
                }
                findings.insert(claim.id.clone());
            }
            review_core::ClaimRefKind::Report => {
                let rows = tx
                    .prepare(
                        "SELECT correlation_id FROM events WHERE run_id = ?1
                         AND causation_id = ?2 AND type = 'FindingReported@1'
                         AND json_extract(payload, '$.report_id') = ?3",
                    )?
                    .query_map(params![run_id, round_event_id, claim.id], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let [finding] = rows.as_slice() else {
                    return Err(StoreError::Conflict(
                        "Integration Proposal Report does not resolve uniquely in the current Round"
                            .into(),
                    ));
                };
                findings.insert(finding.clone());
            }
        }
    }
    Ok(findings.into_iter().collect())
}

pub(crate) fn validate_integration_plan_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    authority: &AuthorityPlan,
    integration_plan: &review_core::IntegrationPlanV1,
    target: IntegrationTarget<'_>,
) -> Result<(), StoreError> {
    let policy = authority.integration.as_ref().ok_or_else(|| {
        StoreError::Conflict("captured pipeline has no automatic-Integration policy".into())
    })?;
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&active.subject_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    subject.validate().map_err(StoreError::Conflict)?;
    integration_plan.validate().map_err(StoreError::Conflict)?;
    if integration_plan.subject_id != active.subject_id
        || integration_plan.base_snapshot_id != subject.head_snapshot_id
        || integration_plan.policy_id != authority.pipeline_policy_id
        || integration_plan.protected_paths != policy.protected_paths
    {
        return Err(StoreError::Conflict(
            "Integration plan contradicts the captured Subject or policy".into(),
        ));
    }
    let plan_value = serde_json::to_value(integration_plan)?;
    let plan_id = crate::canonical::content_id(&plan_value)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    if target.batch_id != format!("integration-{}", &plan_id[7..23]) {
        return Err(StoreError::Conflict(
            "Integration batch identity is not derived from its exact plan".into(),
        ));
    }

    let round_event_id: String = tx.query_row(
        "SELECT event_id FROM events WHERE run_id = ?1 AND type = 'RoundStarted@1'
         ORDER BY sequence DESC LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let priorities = policy
        .reviewer_priority
        .iter()
        .enumerate()
        .map(|(index, node)| (node.as_str(), index as u32))
        .collect::<std::collections::BTreeMap<_, _>>();
    let default_priority = u32::try_from(priorities.len()).unwrap_or(u32::MAX);
    let mut prior_paths = Vec::<String>::new();
    let mut seen_patches = std::collections::BTreeSet::new();

    let base_snapshot: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let base_manifest_id = base_snapshot
        .artifact_manifest
        .as_deref()
        .ok_or_else(|| StoreError::Conflict("Integration Base Snapshot has no Manifest".into()))?;
    let base_manifest = cas
        .get_json(base_manifest_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let path_encoding = base_manifest.get("path_encoding").cloned();
    let mut composed_entries = manifest_entries(&base_manifest)?;

    for candidate in &integration_plan.candidates {
        let rows = tx
            .prepare(
                "SELECT node_id, attempt_id, payload FROM events
                 WHERE run_id = ?1 AND causation_id = ?2 AND type = 'ProposalAccepted@1'
                   AND json_extract(payload, '$.proposal_id') = ?3 ORDER BY sequence",
            )?
            .query_map(
                params![run_id, round_event_id, candidate.proposal_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let [(node, attempt, raw)] = rows.as_slice() else {
            return Err(StoreError::Conflict(
                "Integration candidate has no unique current-Round accepted Proposal".into(),
            ));
        };
        let accepted: review_core::ProposalAcceptedPayloadV1 = serde_json::from_str(raw)?;
        if accepted.candidate_artifact_id != candidate.candidate_artifact_id
            || node != &candidate.node_id
        {
            return Err(StoreError::Conflict(
                "Integration candidate contradicts its accepted Proposal event".into(),
            ));
        }
        let binding_node = integration_binding_node(authority, node).ok_or_else(|| {
            StoreError::Conflict(
                "Integration candidate node is absent from captured authority".into(),
            )
        })?;
        let execution = authority.reviewer_execution_for(node).ok_or_else(|| {
            StoreError::Conflict("Integration candidate has no reviewer Execution Binding".into())
        })?;
        if !execution.auto_apply
            || candidate.priority
                != priorities
                    .get(binding_node)
                    .copied()
                    .unwrap_or(default_priority)
        {
            return Err(StoreError::Conflict(
                "Integration candidate lacks captured auto-apply or priority authority".into(),
            ));
        }
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&accepted.proposal_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
        match &envelope.producer {
            review_core::Producer::Attempt {
                run_id: producer_run,
                node_id: producer_node,
                attempt_id: producer_attempt,
            } if producer_run == run_id && producer_node == node && producer_attempt == attempt => {
            }
            _ => {
                return Err(StoreError::Conflict(
                    "Integration Proposal producer is not its selected Attempt".into(),
                ));
            }
        }
        if envelope.artifact_type != review_core::contract::PATCH_PROPOSAL_V1
            || envelope.artifact_id != accepted.proposal_id
            || envelope.subject_snapshot_id.as_deref() != Some(subject.head_snapshot_id.as_str())
        {
            return Err(StoreError::Conflict(
                "Integration candidate has an invalid Proposal envelope".into(),
            ));
        }
        let proposal: review_core::PatchProposal = serde_json::from_value(envelope.payload)?;
        proposal
            .check_shape()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let prepared_candidate: review_core::ProposalCandidateV1 = serde_json::from_value(
            cas.get_json(&candidate.candidate_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        prepared_candidate
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        validate_candidate_manifest(cas, &subject, &prepared_candidate)?;
        let finding_ids = integration_finding_ids(tx, run_id, &round_event_id, &proposal)?;
        if !proposal.auto_apply_nominated
            || proposal.base_snapshot_id != subject.head_snapshot_id
            || proposal.patch_artifact_id != candidate.patch_artifact_id
            || proposal.patch_artifact_id != prepared_candidate.patch_artifact_id
            || proposal.paths != candidate.paths
            || proposal.paths != prepared_candidate.paths
            || proposal.evidence_ids != candidate.evidence_ids
            || proposal.evidence_ids != prepared_candidate.evidence_ids
            || prepared_candidate.derived_manifest_artifact_id
                != candidate.derived_manifest_artifact_id
            || finding_ids != candidate.finding_ids
            || !seen_patches.insert(candidate.patch_artifact_id.clone())
        {
            return Err(StoreError::Conflict(
                "Integration candidate contradicts its sealed Proposal authority".into(),
            ));
        }
        if candidate.paths.iter().any(|path| {
            policy
                .protected_paths
                .iter()
                .any(|protected| authority_paths_overlap(path, protected))
                || prior_paths
                    .iter()
                    .any(|prior| authority_paths_overlap(path, prior))
        }) {
            return Err(StoreError::Conflict(
                "Integration plan changes a protected or overlapping path".into(),
            ));
        }
        prior_paths.extend(candidate.paths.iter().cloned());
        let candidate_manifest = cas
            .get_json(&candidate.derived_manifest_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        if candidate_manifest.get("path_encoding") != path_encoding.as_ref() {
            return Err(StoreError::Conflict(
                "Integration candidate changed the Manifest path encoding".into(),
            ));
        }
        let candidate_entries = manifest_entries(&candidate_manifest)?;
        for path in &candidate.paths {
            match candidate_entries.get(path).cloned() {
                Some(entry) => {
                    composed_entries.insert(path.clone(), entry);
                }
                None => {
                    composed_entries.remove(path);
                }
            }
        }
    }

    let expected_manifest = manifest_value(path_encoding, composed_entries);
    let expected_manifest_id = crate::canonical::content_id(&expected_manifest)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let recorded_manifest = cas
        .get_json(&integration_plan.derived_manifest_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    if expected_manifest_id != integration_plan.derived_manifest_artifact_id
        || recorded_manifest != expected_manifest
    {
        return Err(StoreError::Conflict(
            "Integration derived Manifest is not the deterministic Proposal composition".into(),
        ));
    }
    let derived: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(target.derived_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let capture_matches = matches!(
        &derived.capture,
        review_core::Capture::Derived {
            tree_id,
            parent_snapshot_id,
            integration_batch_id,
        } if tree_id == &derived.content_digest
            && parent_snapshot_id == &subject.head_snapshot_id
            && integration_batch_id == target.batch_id
    );
    if !capture_matches
        || derived.repository_id != base_snapshot.repository_id
        || derived.vcs != base_snapshot.vcs
        || derived.parent_snapshot_id.as_deref() != Some(subject.head_snapshot_id.as_str())
        || derived.artifact_manifest.as_deref()
            != Some(integration_plan.derived_manifest_artifact_id.as_str())
        || derived.source_revision != base_snapshot.source_revision
        || derived.submodules != base_snapshot.submodules
    {
        return Err(StoreError::Conflict(
            "Integration derived Snapshot is not the exact sealed child of its Base".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_integration_commit_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    committed: &review_core::IntegrationCommittedPayloadV1,
    batch_attestations: &std::collections::BTreeSet<String>,
    authority: &AuthorityPlan,
) -> Result<(), StoreError> {
    let subject: review_core::SubjectV1 = serde_json::from_value(
        cas.get_json(&active.subject_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if subject.head_snapshot_id != committed.prior_snapshot_id {
        return Err(StoreError::Conflict(
            "Integration commit expected head is stale".into(),
        ));
    }
    let opened: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1' LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&opened)?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&opened.campaign_manifest_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if committed.policy_id != manifest.pipeline.artifact_id
        || !manifest
            .execution_policy_ids
            .iter()
            .any(|policy| policy == &committed.policy_id)
    {
        return Err(StoreError::Conflict(
            "Integration commit policy is absent from captured Campaign authority".into(),
        ));
    }

    let prepared_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1
         AND type = 'IntegrationPrepared@1'
         AND json_extract(payload, '$.batch_id') = ?2",
        params![run_id, committed.batch_id],
        |row| row.get(0),
    )?;
    let prepared: review_core::IntegrationPreparedPayloadV1 = serde_json::from_str(&prepared_raw)?;
    let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
        cas.get_json(&prepared.plan_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    plan.validate().map_err(StoreError::Conflict)?;
    let planned_proposals: Vec<&str> = plan
        .candidates
        .iter()
        .map(|candidate| candidate.proposal_id.as_str())
        .collect();
    let committed_proposals: Vec<&str> =
        committed.proposal_ids.iter().map(String::as_str).collect();
    if plan.subject_id != active.subject_id
        || plan.base_snapshot_id != committed.prior_snapshot_id
        || plan.policy_id != committed.policy_id
        || prepared.derived_snapshot_id != committed.derived_snapshot_id
        || planned_proposals != committed_proposals
    {
        return Err(StoreError::Conflict(
            "Integration commit contradicts its exact prepared plan".into(),
        ));
    }
    validate_integration_plan_authority(
        tx,
        cas,
        run_id,
        active,
        authority,
        &plan,
        IntegrationTarget {
            batch_id: &committed.batch_id,
            derived_snapshot_id: &committed.derived_snapshot_id,
        },
    )?;

    let checks_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1
         AND type = 'IntegrationChecksCompleted@1'
         AND json_extract(payload, '$.batch_id') = ?2
         AND json_extract(payload, '$.passed') = 1",
        params![run_id, committed.batch_id],
        |row| row.get(0),
    )?;
    let checked: review_core::IntegrationChecksCompletedPayloadV1 =
        serde_json::from_str(&checks_raw)?;
    let checks: review_core::IntegrationChecksV1 = serde_json::from_value(
        cas.get_json(&checked.checks_artifact_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    if !checks.passed() || checks.derived_snapshot_id != committed.derived_snapshot_id {
        return Err(StoreError::Conflict(
            "Integration commit is not bound to passing checks on its exact Snapshot".into(),
        ));
    }

    let mut finding_set = None;
    let mut demand_set = None;
    let mut statement = tx.prepare(
        "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = (
             SELECT event_id FROM events WHERE run_id = ?1 AND type = 'RoundStarted@1'
             ORDER BY sequence DESC LIMIT 1
         ) AND type = 'NodeOutputReceipt@1' ORDER BY sequence",
    )?;
    for raw in statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
    {
        let receipt: review_core::NodeOutputReceiptPayloadV1 = serde_json::from_str(&raw)?;
        for port in receipt.outputs {
            let [artifact] = port.artifact_ids.as_slice() else {
                continue;
            };
            if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                finding_set = Some(artifact.clone());
            } else if port.artifact_type == review_core::contract::DEMAND_SET_V1 {
                demand_set = Some(artifact.clone());
            }
        }
    }
    if finding_set.as_deref() != Some(committed.expected_finding_set_id.as_str())
        || demand_set.as_deref() != Some(committed.expected_demand_set_id.as_str())
    {
        return Err(StoreError::Conflict(
            "Integration commit expected Finding or Demand view is stale".into(),
        ));
    }

    let closure_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1
         AND causation_id = (SELECT event_id FROM events WHERE run_id = ?1
             AND type = 'RoundStarted@1' ORDER BY sequence DESC LIMIT 1)
         AND type = 'SemanticClosureChecked@1'
         AND json_extract(payload, '$.record_id') = ?2",
        params![run_id, committed.semantic_closure_id],
        |row| row.get(0),
    )?;
    if closure_count != 1 {
        return Err(StoreError::Conflict(
            "Integration commit lacks the current Round semantic-closure proof".into(),
        ));
    }
    let mut accepted = std::collections::BTreeSet::new();
    let mut statement = tx.prepare(
        "SELECT payload FROM events WHERE run_id = ?1
         AND causation_id = (SELECT event_id FROM events WHERE run_id = ?1
             AND type = 'RoundStarted@1' ORDER BY sequence DESC LIMIT 1)
         AND type = 'ProposalAccepted@1'",
    )?;
    for raw in statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
    {
        let payload: review_core::ProposalAcceptedPayloadV1 = serde_json::from_str(&raw)?;
        accepted.insert(payload.proposal_id);
    }
    if committed
        .proposal_ids
        .iter()
        .any(|proposal| !accepted.contains(proposal))
        || committed
            .attestation_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            != batch_attestations.iter().collect()
    {
        return Err(StoreError::Conflict(
            "Integration commit names unselected Proposals or non-atomic attestations".into(),
        ));
    }
    Ok(())
}
