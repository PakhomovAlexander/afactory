//! Native Proposal Integration is data preparation, ordinary Task checks, then one admitted
//! output publication. The Store's existing prefix/lease transaction is the promotion boundary.
use super::*;
use af_contracts::task::native_review::*;
use af_source_git::task::{CANDIDATE_TREE_V1, CandidateTree, capture_snapshot, read_manifest};

pub(super) fn signatures(policy: &ReviewTaskPolicy) -> BTreeMap<String, OperatorSignature> {
    let signature =
        |inputs: BTreeMap<String, PipelinePortV1>, outputs, outcome| OperatorSignature {
            retains: BTreeMap::from_iter([("result".into(), inputs.keys().cloned().collect())]),
            contract: PipelineContractV1 { inputs, outputs },
            effects: BTreeSet::new(),
            evidence: BTreeMap::new(),
            roles: BTreeSet::new(),
            worker_input_type: None,
            worker_output_type: None,
            attempt: None,
            outcome_port: Some(outcome),
            unbound_allowed: BTreeSet::new(),
        };
    let mut inputs = BTreeMap::from([
        (
            "source".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
        ),
        ("review".into(), port(TASK_REVIEW_ROUND_V1, same(), false)),
    ]);
    for name in policy.reviewers.keys() {
        inputs.insert(
            format!("candidate_{name}"),
            port(CANDIDATE_TREE_V1, same(), true),
        );
        inputs.insert(
            format!("proposal_{name}"),
            port(TASK_REVIEW_PROPOSAL_V1, same(), true),
        );
    }
    let prepare = signature(
        inputs,
        BTreeMap::from([(
            "result".into(),
            port(
                TASK_REVIEW_INTEGRATION_V1,
                PortAffinityV1::Unbound {},
                false,
            ),
        )]),
        "result".into(),
    );
    let commit = signature(
        BTreeMap::from([
            (
                "source".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
            ),
            (
                "integration".into(),
                port(
                    TASK_REVIEW_INTEGRATION_V1,
                    PortAffinityV1::Unbound {},
                    false,
                ),
            ),
            ("checks".into(), port(TASK_CHECK_RECEIPT_V2, same(), false)),
        ]),
        BTreeMap::from([
            ("result".into(), port(TASK_REVIEW_COMMIT_V1, same(), false)),
            (
                "history".into(),
                port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}, false),
            ),
        ]),
        "result".into(),
    );
    let mut source = prepare.clone();
    source.contract.inputs = BTreeMap::from([
        (
            "source".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
        ),
        (
            "integration".into(),
            port(
                TASK_REVIEW_INTEGRATION_V1,
                PortAffinityV1::Unbound {},
                false,
            ),
        ),
    ]);
    source.contract.outputs = BTreeMap::from([(
        "source".into(),
        port(
            SOURCE_TREE_V1,
            PortAffinityV1::DerivedFrom {
                input: "source".into(),
            },
            false,
        ),
    )]);
    source.outcome_port = None;
    source.retains = BTreeMap::from([("source".into(), BTreeSet::from(["integration".into()]))]);
    BTreeMap::from([
        ("operator/review-integrate".into(), prepare),
        ("operator/review-commit".into(), commit),
        ("operator/review-integration-source".into(), source),
    ])
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}

impl ReviewTaskDomain {
    pub(super) fn integration_preparation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewIntegrationV1, String> {
        let TaskOperatorV1::ReviewIntegrate {
            reviewers,
            protected_paths,
        } = self.operator(input)?
        else {
            return Err("Not a native Integration preparation".into());
        };
        let (review_id, review): (_, TaskReviewRoundV1) =
            self.value(cas, input, "review", TASK_REVIEW_ROUND_V1)?;
        let (ledger, expected) = self.restore_round(cas, &review_id, 0)?;
        let base = source_input(cas, &input.inputs["source"])?;
        if expected != review
            || review.invocation.plan_id != input.plan_id
            || base != review.snapshot_id
            || review.round >= self.policy.max_rounds
        {
            return Err("Integration requires a complete current Round and allowance for its full successor".into());
        }
        let (snapshot, base_manifest) = read_snapshot(cas, &base)?;
        let mut receipt = TaskReviewIntegrationV1 {
            invocation: input.clone(),
            policy_id: self.policy_id.clone(),
            review_id,
            base_snapshot_id: base.clone(),
            outcome: ReceiptOutcomeV1::Inconclusive,
            selection: TaskReviewIntegrationSelection::Empty {},
        };
        let mut proposals = BTreeMap::new();
        let mut changed = BTreeSet::<String>::new();
        let mut edits = BTreeSet::new();
        let mut entries: BTreeMap<_, _> = base_manifest
            .entries
            .iter()
            .cloned()
            .map(|e| (e.path.clone(), e))
            .collect();
        for name in reviewers {
            let candidate_port = format!("candidate_{name}");
            let proposal_port = format!("proposal_{name}");
            if !input.inputs.contains_key(&candidate_port)
                && !input.inputs.contains_key(&proposal_port)
            {
                continue;
            }
            let (candidate_id, candidate): (_, CandidateTree) =
                self.value(cas, input, &candidate_port, CANDIDATE_TREE_V1)?;
            let (declaration_id, declaration): (_, TaskReviewProposalV1) =
                self.value(cas, input, &proposal_port, TASK_REVIEW_PROPOSAL_V1)?;
            declaration.validate()?;
            let result_id = review
                .selected_results
                .get(name)
                .ok_or("Proposal reviewer did not participate in the closed Round")?;
            if declaration.result_id != *result_id {
                return Err("Proposal changed its exact selected original Reviewer Result".into());
            }
            let canonical_source = self.selected_review_source(cas, &review, name, result_id)?;
            let result = envelope(cas, result_id)?;
            let candidate_frame = envelope(cas, &candidate_id)?;
            let declaration_frame = envelope(cas, &declaration_id)?;
            let bindings = &self.graph.nodes[&input.node].inputs;
            let address = bindings
                .get(&proposal_port)
                .ok_or("Proposal has no captured author binding")?;
            let worker = &self.graph.nodes[&address.node];
            let CompiledOperator::Primitive {
                operator: TaskOperatorV1::Worker { slot },
                ..
            } = &worker.operator
            else {
                return Err("Native Proposal must belong to an explicit source-writing author; Review verifiers remain independent".into());
            };
            let plan: ExecutionPlan =
                serde_json::from_value(envelope(cas, &input.plan_id)?.payload)
                    .map_err(|e| e.to_string())?;
            // Current model adapters hold reusable credentials. They cannot acquire auto-apply
            // authority merely by writing a declaration (ADR-0042).
            if !matches!(
                plan.bindings.get(slot).map(|b| &b.execution),
                Some(af_contracts::task::plan::WorkerExecutionV3::Command {})
            ) {
                return Err(
                    "Automatic Proposal Integration requires a credential-free captured Worker"
                        .into(),
                );
            }
            if bindings
                .get(&candidate_port)
                .is_none_or(|a| a.node != address.node || a.port != "candidate")
                || bindings
                    .get(&proposal_port)
                    .is_none_or(|a| a.node != address.node || a.port != "proposal")
                || candidate_frame.producer != declaration_frame.producer
                || !matches!((&declaration_frame.producer, &result.producer),
                    (Producer::Attempt { run_id: author_run, node_id, .. }, Producer::Attempt { run_id: reviewer_run, .. })
                        if node_id == &address.node && author_run == reviewer_run)
                || candidate_frame.subject_snapshot_id.as_ref() != Some(&base)
                || declaration_frame.subject_snapshot_id.as_ref() != Some(&base)
                || candidate.parent_snapshot_id != base
                || !(declaration_frame.input_artifacts.contains(result_id)
                    || declaration_frame
                        .input_artifacts
                        .contains(&receipt.review_id))
            {
                return Err("Proposal changed its exact selected Attempt, declaration, source or candidate binding".into());
            }
            let manifest = read_manifest(cas, &candidate.manifest_id)?;
            // Entries retain mode, kind, content and length. Compare the complete path set;
            // rendering an unused textual patch would add a git process to every replay.
            let changed_paths: Vec<_> = base_manifest
                .entries
                .iter()
                .chain(&manifest.entries)
                .map(|entry| entry.path.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|path| base_manifest.get(path) != manifest.get(path))
                .collect();
            if declaration.paths != changed_paths {
                return Err(
                    "Proposal declaration does not equal the complete sealed candidate diff".into(),
                );
            }
            for id in &declaration.evidence_ids {
                cas.verify(id).map_err(|e| e.to_string())?;
            }
            if !declaration.auto_apply_nominated {
                continue;
            }
            let stage = crate::reviewer_stage_output(result.payload.clone())?;
            let mut claims = BTreeMap::new();
            for index in &declaration.report_indices {
                let report = stage
                    .reports
                    .get(*index as usize)
                    .ok_or("Proposal names an absent same-result Report index")?
                    .clone()
                    .into_report(*index as usize)
                    .map_err(|e| format!("Invalid Proposal Report: {e:?}"))?;
                let mut refs = vec![result_id.clone()];
                refs.extend(result.input_artifacts.clone());
                let mut seen = BTreeSet::new();
                refs.retain(|id| seen.insert(id.clone()));
                let report_id = cas
                    .put_artifact(
                        af_contracts::contract::FINDING_REPORT_V1,
                        result.producer.clone(),
                        refs,
                        Some(base.clone()),
                        serde_json::to_value(report).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                    .0;
                let finding = ledger
                    .finding_views()
                    .into_iter()
                    .find(|f| {
                        f.status.is_active()
                            && f.reports.iter().any(|r| {
                                r.report_id == report_id
                                    && r.source == canonical_source
                                    && r.round == review.round
                            })
                    })
                    .ok_or("Proposal Report has no active canonical Finding in this exact Round")?;
                let view = cas
                    .put_json(&serde_json::to_value(&finding).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                if ledger.finding_view_id(&finding.key).as_ref() != Some(&view) {
                    return Err("Proposal lost its exact original Finding view".into());
                }
                claims.insert(finding.key.clone(), view);
            }
            let delta: Vec<_> = changed_paths
                .iter()
                .map(|path| (path, manifest.get(path)))
                .collect();
            let edit_id = af_store::canonical::content_id(
                &serde_json::to_value(delta).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let duplicate = !edits.insert(edit_id);
            let forbidden: Vec<_> = declaration
                .paths
                .iter()
                .filter(|p| {
                    protected_paths
                        .iter()
                        .any(|protected| overlaps(p, protected))
                        || (!duplicate && changed.iter().any(|other| overlaps(p, other)))
                })
                .cloned()
                .collect();
            if !forbidden.is_empty() {
                receipt.outcome = ReceiptOutcomeV1::Failed;
                receipt.selection = TaskReviewIntegrationSelection::Conflict { paths: forbidden,
                    reason: "Proposal changes a protected path or overlaps another selected sealed Proposal".into() };
                receipt.validate()?;
                return Ok(receipt);
            }
            if !duplicate {
                for path in &declaration.paths {
                    changed.insert(path.clone());
                    if let Some(entry) = manifest.get(path) {
                        entries.insert(path.clone(), entry.clone());
                    } else {
                        entries.remove(path);
                    }
                }
            }
            // Duplicate edits share one application, while each original declaration keeps
            // its own selected result and canonical claims in the checked receipt.
            proposals.insert(
                name.clone(),
                TaskReviewSelectedProposalV1 {
                    result_id: result_id.clone(),
                    declaration_id,
                    candidate_id,
                    claims,
                },
            );
        }
        if !proposals.is_empty() {
            let manifest = af_source_git::Manifest::new(entries.into_values().collect())
                .map_err(|e| e.to_string())?;
            let manifest_id = cas
                .put_json(&serde_json::to_value(&manifest).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let snapshot_id = capture_snapshot(cas, &manifest, &snapshot.origin_id, Some(&base))?;
            receipt.outcome = ReceiptOutcomeV1::Passed;
            receipt.selection = TaskReviewIntegrationSelection::Prepared {
                snapshot_id,
                manifest_id,
                proposals,
            };
        }
        receipt.validate()?;
        Ok(receipt)
    }

    pub(super) fn integrate_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let receipt = self.integration_preparation(cas, input)?;
        let mut extra = vec![receipt.review_id.clone()];
        if let TaskReviewIntegrationSelection::Prepared {
            snapshot_id,
            manifest_id,
            proposals,
        } = &receipt.selection
        {
            extra.extend([snapshot_id.clone(), manifest_id.clone()]);
            for proposal in proposals.values() {
                extra.extend([
                    proposal.result_id.clone(),
                    proposal.declaration_id.clone(),
                    proposal.candidate_id.clone(),
                ]);
                extra.extend(proposal.claims.values().cloned());
            }
        }
        let prepared = self.put(
            cas,
            input,
            TASK_REVIEW_INTEGRATION_V1,
            None,
            &receipt,
            extra,
        )?;
        Ok(BTreeMap::from([("result".into(), prepared)]))
    }

    pub(super) fn integration_source_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let (id, receipt): (_, TaskReviewIntegrationV1) =
            self.value(cas, input, "integration", TASK_REVIEW_INTEGRATION_V1)?;
        receipt.validate()?;
        if receipt.invocation.plan_id != input.plan_id
            || source_input(cas, &input.inputs["source"])? != receipt.base_snapshot_id
            || self.integrate_outputs(cas, &receipt.invocation)?["result"]
                != input.inputs["integration"]
        {
            return Err("Unpromoted source changed its exact native preparation or parent".into());
        }
        let TaskReviewIntegrationSelection::Prepared { snapshot_id, .. } = receipt.selection else {
            return Err("Only prepared Integration exposes an unpromoted source".into());
        };
        Ok(BTreeMap::from([(
            "source".into(),
            af_source_git::task::source_tree(
                cas,
                invocation_producer(cas, input, None)?,
                &snapshot_id,
                vec![id],
            )?,
        )]))
    }

    fn integration_commit(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewCommitV1, String> {
        let (integration_id, prepared): (_, TaskReviewIntegrationV1) =
            self.value(cas, input, "integration", TASK_REVIEW_INTEGRATION_V1)?;
        prepared.validate()?;
        if self
            .integrate_outputs(cas, &prepared.invocation)?
            .get("result")
            != input.inputs.get("integration")
            || prepared.invocation.plan_id != input.plan_id
        {
            return Err("Integration commit changed its exact prepared selection".into());
        }
        let TaskReviewIntegrationSelection::Prepared {
            snapshot_id,
            proposals,
            ..
        } = &prepared.selection
        else {
            return Err("Empty or conflicting Integration cannot promote".into());
        };
        if source_input(cas, &input.inputs["source"])? != *snapshot_id {
            return Err("Integration checks belong to another derived Snapshot".into());
        }
        let (_, review) = self.restore_round(cas, &prepared.review_id, 0)?;
        let history =
            self.reduce_outputs(cas, &review.invocation)?["history"].artifact_ids[0].clone();
        let outcome = self.code.review_checks(cas, input)?;
        let receipt = TaskReviewCommitV1 {
            invocation: input.clone(),
            integration_id,
            snapshot_id: snapshot_id.clone(),
            prior_history_id: history,
            pending_claims: if outcome == ReceiptOutcomeV1::Passed {
                proposals.values().flat_map(|p| p.claims.clone()).collect()
            } else {
                BTreeMap::new()
            },
            outcome,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub(super) fn commit_integration_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let receipt = self.integration_commit(cas, input)?;
        let result = self.put(
            cas,
            input,
            TASK_REVIEW_COMMIT_V1,
            Some(&receipt.snapshot_id),
            &receipt,
            vec![
                receipt.integration_id.clone(),
                receipt.prior_history_id.clone(),
            ],
        )?;
        let frame = envelope(cas, &receipt.prior_history_id)?;
        let outputs = BTreeMap::from([
            ("result".into(), result.clone()),
            (
                "history".into(),
                ArtifactInputV1 {
                    artifact_type: REVIEW_HISTORY_V1.into(),
                    cardinality: PortCardinality::One,
                    artifact_ids: vec![receipt.prior_history_id.clone()],
                    snapshot_id: frame.subject_snapshot_id,
                },
            ),
        ]);
        Ok(outputs)
    }

    pub(super) fn restore_integration_commit(
        &self,
        cas: &Cas,
        id: &str,
        subject: &ResolvedReviewSubject,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewCommitV1, String> {
        let artifact = envelope(cas, id)?;
        let receipt: TaskReviewCommitV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        receipt.validate()?;
        if artifact.artifact_type != TASK_REVIEW_COMMIT_V1
            || receipt.invocation.plan_id != input.plan_id
            || receipt.snapshot_id != subject.snapshot_id
            || receipt.prior_history_id != subject.prior_history_id
            || receipt.outcome != ReceiptOutcomeV1::Passed
            || self.commit_integration_outputs(cas, &receipt.invocation)?["result"].artifact_ids
                != [id]
        {
            return Err(
                "Integrated successor changed its checked source, original history or exact commit"
                    .into(),
            );
        }
        Ok(receipt)
    }
}
