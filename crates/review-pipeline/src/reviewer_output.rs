//! Reviewer business output checks. These helpers prepare immutable artifacts and domain
//! facts; the execution owner must select the real Attempt before publishing those facts.

use review_core::{
    EventType, MAX_CHANGE_SET_BYTES, ProposalCandidateV1, ProposalPreparedPayloadV1,
    ProposalRefusalReasonV1, ProposalRefusedPayloadV1,
};
use review_runner::ReviewerProposalDeclaration;
use review_source_git::manifest_diff;
use review_store::{Cas, NewEvent};

use super::{PreparedProposal, RoundAuthority};

#[allow(clippy::too_many_arguments)] // one exact Attempt boundary; grouping would obscure authority inputs
pub(super) fn prepare_proposal(
    cas: &Cas,
    authority: &RoundAuthority,
    node_id: &str,
    attempt: &str,
    result_artifact: &str,
    declaration: Result<Option<ReviewerProposalDeclaration>, String>,
    assigned_finding_ids: &[String],
    report_count: usize,
    sealed: &review_sandbox::SealedSandbox,
) -> Result<PreparedProposal, String> {
    let refused = |reason| {
        PreparedProposal::Refused(
            NewEvent::new(
                EventType::ProposalRefusedV1,
                serde_json::to_value(ProposalRefusedPayloadV1 {
                    reason,
                    result_artifact_id: result_artifact.to_string(),
                })
                .expect("typed Proposal refusal serializes"),
            )
            .node(node_id)
            .attempt(attempt.to_string())
            .referencing(vec![result_artifact.to_string()]),
        )
    };
    let declaration = match declaration {
        Ok(Some(declaration)) => declaration,
        Ok(None) => return Ok(PreparedProposal::None),
        Err(_) => return Ok(refused(ProposalRefusalReasonV1::MalformedDeclaration)),
    };
    if authority.finding_identity_policy != review_core::CANONICAL_FINDING_IDENTITY_POLICY {
        return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
    }
    if declaration.patch.is_empty() || declaration.patch.len() > MAX_CHANGE_SET_BYTES {
        return Ok(refused(ProposalRefusalReasonV1::MalformedDeclaration));
    }
    let mut report_indexes = declaration.report_indexes;
    report_indexes.sort_unstable();
    if report_indexes.windows(2).any(|pair| pair[0] == pair[1])
        || report_indexes
            .iter()
            .any(|index| usize::try_from(*index).map_or(true, |index| index >= report_count))
    {
        return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
    }
    let mut finding_ids = declaration.finding_ids;
    finding_ids.sort();
    if finding_ids.windows(2).any(|pair| pair[0] == pair[1])
        || finding_ids
            .iter()
            .any(|id| !assigned_finding_ids.contains(id))
        || report_indexes.is_empty() && finding_ids.is_empty()
    {
        return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
    }
    let mut evidence_ids = declaration.evidence_ids;
    evidence_ids.sort();
    if evidence_ids.windows(2).any(|pair| pair[0] == pair[1])
        || evidence_ids.iter().any(|id| cas.verify(id).is_err())
    {
        return Ok(refused(ProposalRefusalReasonV1::InvalidEvidence));
    }
    let mut paths = declaration.paths;
    paths.sort();
    if paths.is_empty()
        || paths.windows(2).any(|pair| pair[0] == pair[1])
        || paths
            .iter()
            .any(|path| !review_core::is_valid_repo_path(path))
    {
        return Ok(refused(ProposalRefusalReasonV1::InvalidPath));
    }
    if sealed.mutations.is_empty() {
        return Ok(refused(ProposalRefusalReasonV1::EmptyMutation));
    }
    if paths != sealed.mutations.paths() {
        return Ok(refused(ProposalRefusalReasonV1::PathMismatch));
    }
    let final_manifest = sealed
        .capture_snapshot(cas)
        .map_err(|error| error.to_string())?;
    let diff = manifest_diff(sealed.baseline.as_ref(), &final_manifest, cas)
        .map_err(|error| error.to_string())?;
    if declaration.patch.as_bytes() != diff.patch() {
        return Ok(refused(ProposalRefusalReasonV1::PatchMismatch));
    }
    let patch_artifact_id = cas.put(diff.patch()).map_err(|error| error.to_string())?;
    let derived_manifest_artifact_id = cas
        .put_json(&serde_json::to_value(&final_manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let candidate = ProposalCandidateV1 {
        base_snapshot_id: authority.head_snapshot_id.clone(),
        patch_artifact_id: patch_artifact_id.clone(),
        derived_manifest_artifact_id: derived_manifest_artifact_id.clone(),
        result_artifact_id: result_artifact.to_string(),
        report_indexes,
        finding_ids,
        evidence_ids: evidence_ids.clone(),
        paths,
        description: declaration.description,
        auto_apply_nominated: declaration.auto_apply_nominated,
    };
    candidate.validate().map_err(str::to_string)?;
    let candidate_artifact = cas
        .put_json(&serde_json::to_value(candidate).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let mut artifacts = vec![
        candidate_artifact.clone(),
        result_artifact.to_string(),
        patch_artifact_id,
        derived_manifest_artifact_id,
    ];
    artifacts.extend(evidence_ids);
    Ok(PreparedProposal::Prepared {
        candidate_artifact: candidate_artifact.clone(),
        event: NewEvent::new(
            EventType::ProposalPreparedV1,
            serde_json::to_value(ProposalPreparedPayloadV1 {
                candidate_artifact_id: candidate_artifact,
                result_artifact_id: result_artifact.to_string(),
            })
            .map_err(|error| error.to_string())?,
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(artifacts),
    })
}
