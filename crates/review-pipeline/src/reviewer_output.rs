//! Reviewer business output checks. These helpers prepare immutable artifacts and domain
//! facts; the execution owner must select the real Attempt before publishing those facts.

use review_core::task::review_compat::{TaskReviewProposalV1, TaskReviewResultMetadataV1};
use review_core::{
    EventType, MAX_CHANGE_SET_BYTES, ProposalCandidateV1, ProposalPreparedPayloadV1,
    ProposalRefusalReasonV1, ProposalRefusedPayloadV1,
};
use review_runner::ReviewerProposalDeclaration;
use review_source_git::manifest_diff;
use review_store::{Cas, NewEvent};

use super::{PreparedProposal, RoundAuthority};

/// One already validated adapter reply. Capturing it seals the actual sandbox and creates
/// immutable result/provenance artifacts; it cannot select an Attempt or publish domain facts.
pub(super) struct ReviewerResultCapture<'a> {
    pub node_id: &'a str,
    pub attempt_id: &'a str,
    pub result: &'a serde_json::Value,
    pub result_contract: review_core::ReviewerResultContract,
    pub proposal: Result<Option<ReviewerProposalDeclaration>, String>,
    pub assigned_finding_ids: &'a [String],
    pub report_count: usize,
    pub cost_tokens: u64,
    pub usage: &'a review_runner::TokenUsage,
    pub context_manifest: &'a review_runner::ContextManifest,
    pub raw_artifact: &'a str,
}

pub(super) struct CapturedReviewerResult {
    pub metadata: TaskReviewResultMetadataV1,
    pub proposal: PreparedProposal,
}

pub(super) fn capture_result(
    cas: &Cas,
    authority: &RoundAuthority,
    sandbox: review_sandbox::Sandbox,
    reply: ReviewerResultCapture<'_>,
) -> Result<CapturedReviewerResult, String> {
    let sealed = sandbox.seal().map_err(|error| error.to_string())?;
    let result_artifact = cas
        .put_json(reply.result)
        .map_err(|error| error.to_string())?;
    let proposal = prepare_proposal(
        cas,
        authority,
        reply.node_id,
        reply.attempt_id,
        &result_artifact,
        reply.proposal,
        reply.assigned_finding_ids,
        reply.report_count,
        &sealed,
    )?;
    // A build can leave thousands of mutations. Capture the complete set once, then retain
    // only its digest and bounded summary in provenance.
    let mutations_artifact = cas
        .put_json(&serde_json::json!({
            "added":sealed.mutations.added, "modified":sealed.mutations.modified,
            "deleted":sealed.mutations.deleted,
        }))
        .map_err(|error| error.to_string())?;
    let provenance_artifact = cas.put_json(&serde_json::json!({
        "node":reply.node_id, "attempt":reply.attempt_id, "result_artifact":result_artifact,
        "cost_tokens":reply.cost_tokens, "usage":reply.usage, "context_manifest":reply.context_manifest,
        "raw":reply.raw_artifact,
        "sandbox_mutations":super::mutation_summary(&sealed.mutations, &mutations_artifact),
    })).map_err(|error| error.to_string())?;
    let disposition = match &proposal {
        PreparedProposal::None => TaskReviewProposalV1::None {},
        PreparedProposal::Prepared {
            candidate_artifact, ..
        } => TaskReviewProposalV1::Prepared {
            candidate_artifact_id: candidate_artifact.clone(),
        },
        PreparedProposal::Refused(event) => TaskReviewProposalV1::Refused {
            reason: serde_json::from_value::<ProposalRefusedPayloadV1>(event.payload.clone())
                .map_err(|e| e.to_string())?
                .reason,
        },
    };
    let metadata = TaskReviewResultMetadataV1 {
        result_contract: reply.result_contract,
        result_artifact_id: result_artifact,
        provenance_artifact_id: provenance_artifact,
        proposal: disposition,
    };
    metadata.validate()?;
    Ok(CapturedReviewerResult { metadata, proposal })
}

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
