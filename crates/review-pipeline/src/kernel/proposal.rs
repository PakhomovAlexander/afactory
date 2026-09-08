//! Proposals: verifying a reviewer's declaration against its sealed sandbox diff, and publishing
//! `PatchProposal@1` once canonical Report IDs exist (ADR-0038).

use std::collections::BTreeMap;

use review_attempt::AttemptId;
use review_core::{
    EventType, MAX_CHANGE_SET_BYTES, Producer, ProposalAcceptedPayloadV1, ProposalCandidateV1,
    ProposalPreparedPayloadV1, ProposalRefusalReasonV1, ProposalRefusedPayloadV1,
};
use review_runner::ReviewerProposalDeclaration;
use review_source_git::manifest_diff;
use review_store::NewEvent;

use crate::kernel::Kernel;

pub(crate) enum PreparedProposal {
    None,
    Prepared {
        candidate_artifact: String,
        event: NewEvent,
    },
    Refused(NewEvent),
}

impl Kernel<'_> {
    #[allow(clippy::too_many_arguments)] // one exact Attempt boundary; grouping would obscure authority inputs
    pub(crate) fn prepare_proposal(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        result_artifact: &str,
        declaration: Result<Option<ReviewerProposalDeclaration>, String>,
        // `permitted_finding_ids` is every Finding the Round delivered to this reviewer — the
        // whole union, not the reviewer's own partition of it. A reviewer may attach a fix to a
        // peer's claim; only a claim naming a Finding outside the Round at all is invalid.
        permitted_finding_ids: &[String],
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
        if self.authority.finding_identity_policy != review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
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
                .any(|id| !permitted_finding_ids.contains(id))
            || report_indexes.is_empty() && finding_ids.is_empty()
        {
            return Ok(refused(ProposalRefusalReasonV1::InvalidClaim));
        }
        let mut evidence_ids = declaration.evidence_ids;
        evidence_ids.sort();
        if evidence_ids.windows(2).any(|pair| pair[0] == pair[1])
            || evidence_ids.iter().any(|id| self.cas.verify(id).is_err())
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
            .capture_snapshot(self.cas)
            .map_err(|error| error.to_string())?;
        let diff = manifest_diff(sealed.baseline.as_ref(), &final_manifest, self.cas)
            .map_err(|error| error.to_string())?;
        if declaration.patch.as_bytes() != diff.patch() {
            return Ok(refused(ProposalRefusalReasonV1::PatchMismatch));
        }
        let patch_artifact_id = self
            .cas
            .put(diff.patch())
            .map_err(|error| error.to_string())?;
        let derived_manifest_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&final_manifest).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let candidate = ProposalCandidateV1 {
            base_snapshot_id: self.authority.head_snapshot_id.clone(),
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
        let candidate_artifact = self
            .cas
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

    pub(crate) fn finalize_proposals(
        &self,
        reduction: &review_store::CanonicalReduction,
    ) -> Result<Vec<String>, String> {
        let existing = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                event.event_type == EventType::ProposalAcceptedV1
                    && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
            })
            .map(|event| {
                let payload: ProposalAcceptedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                Ok((payload.candidate_artifact_id.clone(), payload))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let selections = self
            .reviewer_selections
            .lock()
            .expect("reviewer selections")
            .clone();
        let mut proposal_ids = Vec::new();
        let mut events = Vec::new();
        for (node, selection) in selections {
            let Some(candidate_artifact) = selection.proposal_candidate else {
                continue;
            };
            if let Some(accepted) = existing.get(&candidate_artifact) {
                self.cas
                    .verify(&accepted.proposal_artifact_id)
                    .map_err(|error| error.to_string())?;
                proposal_ids.push(accepted.proposal_id.clone());
                continue;
            }
            let candidate: ProposalCandidateV1 = serde_json::from_value(
                self.cas
                    .get_json(&candidate_artifact)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            candidate.validate().map_err(str::to_string)?;
            if candidate.result_artifact_id != selection.result_artifact
                || candidate.base_snapshot_id != self.authority.head_snapshot_id
            {
                return Err(format!(
                    "prepared Proposal for `{node}` contradicts selected Round authority"
                ));
            }
            let source_reports = reduction
                .report_ids_by_source
                .get(&node)
                .ok_or_else(|| format!("prepared Proposal source `{node}` reached no Reports"))?;
            let mut finding_refs = candidate
                .finding_ids
                .iter()
                .map(|id| review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Finding,
                    id: id.clone(),
                })
                .collect::<Vec<_>>();
            for index in &candidate.report_indexes {
                let report_id = source_reports
                    .get(*index as usize)
                    .ok_or_else(|| {
                        format!("prepared Proposal for `{node}` names absent Report {index}")
                    })?
                    .clone();
                finding_refs.push(review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Report,
                    id: report_id,
                });
            }
            let proposal = review_core::PatchProposal {
                base_snapshot_id: candidate.base_snapshot_id.clone(),
                patch_artifact_id: candidate.patch_artifact_id.clone(),
                finding_refs,
                evidence_ids: candidate.evidence_ids.clone(),
                paths: candidate.paths.clone(),
                description: candidate.description.clone(),
                auto_apply_nominated: candidate.auto_apply_nominated,
            };
            proposal.check_shape().map_err(str::to_string)?;
            let mut inputs = vec![
                candidate_artifact.clone(),
                candidate.result_artifact_id.clone(),
                candidate.patch_artifact_id.clone(),
                candidate.derived_manifest_artifact_id.clone(),
            ];
            inputs.extend(candidate.evidence_ids.iter().cloned());
            inputs.sort();
            inputs.dedup();
            let (proposal_artifact_id, envelope) = self
                .cas
                .put_artifact(
                    review_core::contract::PATCH_PROPOSAL_V1,
                    Producer::Attempt {
                        run_id: self.run_id.clone(),
                        node_id: node.clone(),
                        attempt_id: selection.attempt_id.clone(),
                    },
                    inputs,
                    Some(candidate.base_snapshot_id.clone()),
                    serde_json::to_value(proposal).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            let payload = ProposalAcceptedPayloadV1 {
                proposal_id: envelope.artifact_id.clone(),
                proposal_artifact_id: proposal_artifact_id.clone(),
                candidate_artifact_id: candidate_artifact.clone(),
            };
            proposal_ids.push(envelope.artifact_id);
            events.push(
                NewEvent::new(
                    EventType::ProposalAcceptedV1,
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .node(node)
                .attempt(selection.attempt_id)
                .referencing(vec![proposal_artifact_id, candidate_artifact]),
            );
        }
        self.append_batch(&events)?;
        proposal_ids.sort();
        proposal_ids.dedup();
        Ok(proposal_ids)
    }
}
