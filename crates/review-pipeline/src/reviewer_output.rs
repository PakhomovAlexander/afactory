//! Reviewer business output checks. These helpers prepare immutable artifacts and domain
//! facts; the execution owner must select the real Attempt before publishing those facts.

use review_core::task::review_compat::*;
use review_core::task::usage::TaskTokenUsageV3;
use review_core::{MAX_CHANGE_SET_BYTES, ProposalCandidateV1, ProposalRefusalReasonV1};
use review_runner::{ReviewerNotesDeclaration, ReviewerProposalDeclaration};
use review_source_git::{Manifest, manifest_diff};
use review_store::Cas;

use super::RoundAuthority;
use crate::warm::{NotesCapture, PreparedNotes};

/// One already validated adapter reply. Capturing it seals the actual sandbox and creates
/// immutable result/provenance artifacts; it cannot select an Attempt or publish domain facts.
pub(super) struct ReviewerResultCapture<'a> {
    pub node_id: &'a str,
    pub attempt_id: &'a str,
    pub result: &'a serde_json::Value,
    pub result_contract: review_core::ReviewerResultContract,
    pub proposal: Result<Option<ReviewerProposalDeclaration>, String>,
    /// The Notes transported beside the same answer, the ADR-0038 pattern again.
    pub notes: Result<Option<ReviewerNotesDeclaration>, String>,
    /// The node's Notes bound. `None` for a cold node: nothing is recorded and the Attempt's
    /// evidence stays byte-identical to a pre-warm Attempt.
    pub notes_max_bytes: Option<u64>,
    /// The head tree the Attempt inspected, so inspected paths bind to exact tree entries.
    pub head_manifest: &'a Manifest,
    pub assigned_finding_ids: &'a [String],
    pub report_count: usize,
    pub cost_tokens: u128,
    pub usage: &'a TaskTokenUsageV3,
    pub raw_artifact: &'a str,
}

pub(super) struct CapturedReviewerResult {
    pub metadata: TaskReviewResultMetadataV1,
    /// Present only for warm nodes; the owner publishes its event beside the admission.
    pub notes: Option<PreparedNotes>,
}

/// Capture one Task-hosted Review Attempt's reply against its exact Task Review context.
/// `usage_known` is false when the provider reported no usage and the charge is the
/// reservation.
pub(super) fn capture_task_result(
    cas: &Cas,
    authority: &RoundAuthority,
    sandbox: review_sandbox::Sandbox,
    reply: ReviewerResultCapture<'_>,
    context_id: &str,
    usage_known: bool,
) -> Result<CapturedReviewerResult, String> {
    let sealed = sandbox.seal().map_err(|error| error.to_string())?;
    let result_artifact = cas
        .put_json(reply.result)
        .map_err(|error| error.to_string())?;
    let proposal = prepare_proposal(
        cas,
        authority,
        &result_artifact,
        reply.proposal,
        reply.assigned_finding_ids,
        reply.report_count,
        &sealed,
    )?;
    let notes = match reply.notes_max_bytes {
        Some(max_bytes) => {
            let capture = NotesCapture {
                declaration: reply.notes,
                max_bytes,
                head_manifest: reply.head_manifest,
            };
            let node_id = reply.node_id;
            let attempt_id = reply.attempt_id;
            let prepared = crate::warm::prepare_notes(
                cas,
                authority,
                node_id,
                attempt_id,
                &result_artifact,
                capture,
            )?;
            Some(prepared)
        }
        None => None,
    };
    // A build can leave thousands of mutations. Capture the complete set once, then retain
    // only its digest and bounded summary in provenance.
    let mutations_artifact = cas
        .put_json(&serde_json::json!({
            "added":sealed.mutations.added, "modified":sealed.mutations.modified,
            "deleted":sealed.mutations.deleted,
        }))
        .map_err(|error| error.to_string())?;
    let usage = reply.usage;
    let provenance_artifact = {
        let envelope = cas.get_artifact(context_id).map_err(|e| e.to_string())?;
        if envelope.artifact_type != TASK_REVIEW_CONTEXT_V1 {
            return Err("Task Review provenance has another context type".into());
        }
        let context: TaskReviewContextV1 =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        context.validate()?;
        if context.attempt_id != reply.attempt_id || context.review_node != reply.node_id {
            return Err("Task Review provenance changed its actual Attempt".into());
        }
        let usage_id = usage_known
            .then(|| {
                review_runner::task::usage::persist_task_usage_exact(
                    cas,
                    envelope.producer.clone(),
                    context_id,
                    usage,
                )
            })
            .transpose()?;
        let provenance = TaskReviewAttemptProvenanceV2 {
            context_id: context_id.into(),
            task_invocation_id: context.task_invocation_id,
            attempt_id: reply.attempt_id.into(),
            review_node: reply.node_id.into(),
            result_artifact_id: result_artifact.clone(),
            mutations_artifact_id: mutations_artifact,
            raw_artifact_id: reply.raw_artifact.into(),
            charged_tokens: reply.cost_tokens.into(),
            usage_id,
        };
        provenance.validate()?;
        let payload = serde_json::to_value(&provenance).map_err(|e| e.to_string())?;
        cas.put_artifact(
            TASK_REVIEW_ATTEMPT_PROVENANCE_V2,
            envelope.producer,
            provenance
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(authority.head_snapshot_id.clone()),
            payload,
        )
        .map_err(|e| e.to_string())?
        .0
    };
    let metadata = TaskReviewResultMetadataV1 {
        result_contract: reply.result_contract,
        result_artifact_id: result_artifact,
        provenance_artifact_id: provenance_artifact,
        proposal,
    };
    metadata.validate()?;
    Ok(CapturedReviewerResult { metadata, notes })
}

/// Verify one Proposal declaration against the complete sealed sandbox diff. A prepared
/// candidate is written to the CAS; the execution owner publishes the matching event.
fn prepare_proposal(
    cas: &Cas,
    authority: &RoundAuthority,
    result_artifact: &str,
    declaration: Result<Option<ReviewerProposalDeclaration>, String>,
    assigned_finding_ids: &[String],
    report_count: usize,
    sealed: &review_sandbox::SealedSandbox,
) -> Result<TaskReviewProposalV1, String> {
    let refused = |reason| TaskReviewProposalV1::Refused { reason };
    let declaration = match declaration {
        Ok(Some(declaration)) => declaration,
        Ok(None) => return Ok(TaskReviewProposalV1::None {}),
        Err(_) => return Ok(refused(ProposalRefusalReasonV1::MalformedDeclaration)),
    };
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
        patch_artifact_id,
        derived_manifest_artifact_id,
        result_artifact_id: result_artifact.to_string(),
        report_indexes,
        finding_ids,
        evidence_ids,
        paths,
        description: declaration.description,
        auto_apply_nominated: declaration.auto_apply_nominated,
    };
    candidate.validate().map_err(str::to_string)?;
    let candidate_artifact_id = cas
        .put_json(&serde_json::to_value(candidate).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(TaskReviewProposalV1::Prepared {
        candidate_artifact_id,
    })
}
