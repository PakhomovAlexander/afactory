//! Inspection of selected sealed repairs is independent of automatic Integration eligibility.
use super::*;
use af_contracts::ArtifactEnvelope;
use af_contracts::task::native_review::{TASK_REVIEW_PROPOSAL_V1, TaskReviewProposalV1};
use af_source_git::task::{CANDIDATE_TREE_V1, CandidateTree, read_manifest};

/// A checked original declaration and its exact sealed tree. This is no promotion receipt.
pub struct SealedReviewProposal {
    pub id: String,
    pub base_snapshot_id: String,
    pub candidate_manifest_id: String,
    pub finding_ids: BTreeSet<String>,
}

impl ReviewTaskDomain {
    /// Only selected outputs admitted by this Task may supply a proposed repair. A complete
    /// changes-requested Round has the same original Report authority as a passing Round.
    pub fn sealed_proposals(
        &self,
        cas: &Cas,
        task: &TaskProjection,
    ) -> Result<Vec<SealedReviewProposal>, String> {
        let _operation = self.begin_operation()?;
        let execution = task
            .execution
            .as_ref()
            .ok_or("Task has no admitted execution")?;
        if execution.graph != self.graph {
            return Err("Proposal inspection changed its captured graph".into());
        }
        let run = task_run_id(&task.task_id).map_err(|e| e.to_string())?;
        let mut rounds = BTreeMap::new();
        for (_, output) in execution.outputs.values() {
            for port in output
                .outputs
                .values()
                .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
            {
                for id in &port.artifact_ids {
                    let frame = envelope(cas, id)?;
                    let round: TaskReviewRoundV1 =
                        serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
                    if round.finding_set_id.is_some() {
                        rounds.insert(id.clone(), self.restore_round(cas, id, 0)?);
                    }
                }
            }
        }
        let mut proposals = Vec::new();
        for (node, (_, output)) in &execution.outputs {
            let declarations: Vec<_> = output
                .outputs
                .values()
                .filter(|p| p.artifact_type == TASK_REVIEW_PROPOSAL_V1)
                .collect();
            if declarations.is_empty() {
                continue;
            }
            let CompiledOperator::Primitive {
                operator: TaskOperatorV1::Worker { .. },
                ..
            } = &self.graph.nodes[node].operator
            else {
                return Err("Proposal must be an admitted source-writing Worker output".into());
            };
            let (invocation_id, invocation) = execution
                .invocations
                .get(node)
                .ok_or("Proposal lost its admitted invocation")?;
            if output.invocation_id != *invocation_id
                || task.plan_id.as_deref() != Some(&invocation.plan_id)
            {
                return Err("Proposal changed its exact admitted Task plan or invocation".into());
            }
            let base = source_input(
                cas,
                invocation
                    .inputs
                    .get("source")
                    .ok_or("Proposal author has no source")?,
            )?;
            let candidates: Vec<_> = output
                .outputs
                .values()
                .filter(|p| p.artifact_type == CANDIDATE_TREE_V1)
                .collect();
            if candidates.len() != 1
                || candidates[0].artifact_ids.len() != 1
                || declarations.len() != 1
                || declarations[0].artifact_ids.len() != 1
            {
                return Err(
                    "Proposal requires one declaration beside one exact sealed candidate".into(),
                );
            }
            let candidate_id = &candidates[0].artifact_ids[0];
            let candidate_frame = envelope(cas, candidate_id)?;
            let candidate: CandidateTree = serde_json::from_value(candidate_frame.payload.clone())
                .map_err(|e| e.to_string())?;
            let id = &declarations[0].artifact_ids[0];
            let declaration_frame = envelope(cas, id)?;
            let declaration: TaskReviewProposalV1 =
                serde_json::from_value(declaration_frame.payload.clone())
                    .map_err(|e| e.to_string())?;
            declaration.validate()?;
            if candidate_frame.producer != declaration_frame.producer
                || !matches!(&declaration_frame.producer, Producer::Attempt { run_id, node_id, .. } if run_id == &run && node_id == node)
                || candidate_frame.subject_snapshot_id.as_ref() != Some(&base)
                || declaration_frame.subject_snapshot_id.as_ref() != Some(&base)
                || candidates[0].snapshot_id.as_ref() != Some(&base)
                || declarations[0].snapshot_id.as_ref() != Some(&base)
                || candidate.parent_snapshot_id != base
            {
                return Err("Proposal changed its selected Attempt, sealed candidate or exact original base".into());
            }
            let original: Vec<_> = rounds
                .iter()
                .flat_map(|(round_id, (ledger, round))| {
                    round
                        .selected_results
                        .iter()
                        .filter(|(_, selected)| **selected == declaration.result_id)
                        .map(move |(reviewer, _)| (round_id, ledger, round, reviewer))
                })
                .filter(|(round_id, _, round, _)| {
                    round.snapshot_id == base
                        && (declaration_frame
                            .input_artifacts
                            .contains(&declaration.result_id)
                            || declaration_frame.input_artifacts.contains(round_id))
                })
                .collect();
            if original.len() != 1 {
                return Err("Proposal requires one exact complete original Round and selected Reviewer Result".into());
            }
            let (_, ledger, round, reviewer) = original[0];
            let canonical_source =
                self.selected_review_source(cas, round, reviewer, &declaration.result_id)?;
            let result = envelope(cas, &declaration.result_id)?;
            if result.artifact_type != RESULT_TYPE
                || result.subject_snapshot_id.as_ref() != Some(&base)
            {
                return Err("Proposal changed its original Reviewer Result Snapshot".into());
            }
            let stage = crate::reviewer_stage_output(result.payload.clone())?;
            let mut finding_ids = BTreeSet::new();
            for index in &declaration.report_indices {
                let report = stage
                    .reports
                    .get(*index as usize)
                    .ok_or("Proposal names an absent original Report")?
                    .clone()
                    .into_report(*index as usize)
                    .map_err(|e| format!("Invalid Proposal Report: {e:?}"))?;
                let mut refs = vec![declaration.result_id.clone()];
                refs.extend(result.input_artifacts.clone());
                let mut seen = BTreeSet::new();
                refs.retain(|id| seen.insert(id.clone()));
                let payload = serde_json::to_value(report).map_err(|e| e.to_string())?;
                let mut report = ArtifactEnvelope {
                    artifact_type: af_contracts::contract::FINDING_REPORT_V1.into(),
                    artifact_id: String::new(),
                    content_id: af_store::canonical::content_id(&payload)
                        .map_err(|e| e.to_string())?,
                    producer: result.producer.clone(),
                    input_artifacts: refs,
                    subject_snapshot_id: Some(base.clone()),
                    payload,
                };
                report.artifact_id =
                    af_store::canonical::artifact_id(&report).map_err(|e| e.to_string())?;
                let finding = ledger
                    .finding_views()
                    .into_iter()
                    .find(|f| {
                        f.reports.iter().any(|r| {
                            r.report_id == report.artifact_id
                                && r.source == canonical_source
                                && r.round == round.round
                        })
                    })
                    .ok_or("Proposal lost its exact original Report/Finding provenance")?;
                finding_ids.insert(finding.key);
            }
            for evidence in &declaration.evidence_ids {
                cas.verify(evidence).map_err(|e| e.to_string())?;
            }
            let (_, before) = read_snapshot(cas, &base)?;
            let after = read_manifest(cas, &candidate.manifest_id)?;
            let changed: Vec<_> = before
                .entries
                .iter()
                .chain(&after.entries)
                .map(|e| e.path.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|p| before.get(p) != after.get(p))
                .collect();
            if declaration.paths != changed {
                return Err(
                    "Proposal declaration does not equal the complete sealed candidate diff".into(),
                );
            }
            proposals.push(SealedReviewProposal {
                id: id.clone(),
                base_snapshot_id: base,
                candidate_manifest_id: candidate.manifest_id,
                finding_ids,
            });
        }
        proposals.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(proposals)
    }
}

impl ReviewTaskDomain {
    /// Validate an actually published commit with the same recomputation used for successor
    /// restoration. Export never interprets an unrelated Round as a promoted head.
    pub fn export_commit(
        &self,
        cas: &Cas,
        task: &TaskProjection,
        output_id: &str,
    ) -> Result<Option<(String, String)>, String> {
        let Some(execution) = &task.execution else {
            return Ok(None);
        };
        if execution.graph != self.graph {
            return Err("Commit inspection changed its captured graph".into());
        }
        let Some((node, (_, output))) = execution
            .outputs
            .iter()
            .find(|(_, (id, _))| id == output_id)
        else {
            return Ok(None);
        };
        if !matches!(
            self.graph.nodes[node].operator,
            CompiledOperator::Primitive {
                operator: TaskOperatorV1::ReviewCommit {},
                ..
            }
        ) {
            return Ok(None);
        }
        let input = &execution
            .invocations
            .get(node)
            .ok_or("Commit lost its admitted invocation")?
            .1;
        let expected = self.commit_integration_outputs(cas, input)?;
        if expected != output.outputs {
            return Err("Commit changed its exact sealed publication".into());
        }
        let frame = envelope(cas, &expected["result"].artifact_ids[0])?;
        let commit: af_contracts::task::native_review::TaskReviewCommitV1 =
            serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
        if commit.outcome != ReceiptOutcomeV1::Passed {
            return Ok(None);
        }
        let frame = envelope(cas, &commit.integration_id)?;
        let integration: af_contracts::task::native_review::TaskReviewIntegrationV1 =
            serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
        Ok(Some((integration.base_snapshot_id, commit.snapshot_id)))
    }
}
