//! Repair continues a closed discovery Round; it does not create a second Review Run.
use super::*;
use review_core::task::repair::*;
use review_core::task::verification::{
    ImplementationReviewScopeV1, REPAIR_ALLOWED_IMPLEMENTATION_V1, ReviewedImplementationV1,
};

pub(super) fn signatures(policy: &str, enabled: bool) -> BTreeMap<String, OperatorSignature> {
    let signature = |inputs, outputs, evidence, retains, outcome_port| OperatorSignature {
        contract: PipelineContractV1 { inputs, outputs },
        effects: BTreeSet::new(),
        evidence,
        retains,
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        attempt: None,
        outcome_port,
    };
    let unbound = |ty| port(ty, PortAffinityV1::Unbound {}, false);
    let attest = signature(
        BTreeMap::from([
            ("source".into(), unbound(SOURCE_TREE_V1)),
            ("previous".into(), unbound(SOURCE_TREE_V1)),
            ("review".into(), unbound(TASK_REVIEW_ROUND_V1)),
            ("history".into(), unbound(REVIEW_HISTORY_V1)),
        ]),
        BTreeMap::from([("repair".into(), port(TASK_REPAIR_CONTEXT_V1, same(), false))]),
        BTreeMap::new(),
        BTreeMap::new(),
        None,
    );
    let retained = BTreeSet::from(["repair".into(), "checks".into(), "verification".into()]);
    let accept = signature(
        BTreeMap::from([
            ("source".into(), unbound(SOURCE_TREE_V1)),
            ("repair".into(), port(TASK_REPAIR_CONTEXT_V1, same(), false)),
            ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same(), false)),
            (
                "verification".into(),
                port(TASK_FIX_VERIFICATION_V1, same(), true),
            ),
        ]),
        BTreeMap::from([
            ("snapshot".into(), port(SOURCE_TREE_V1, same(), false)),
            (
                "result".into(),
                port(REPAIR_ALLOWED_IMPLEMENTATION_V1, same(), false),
            ),
            (
                "assessment".into(),
                port(REPAIR_ASSESSMENT_V1, same(), false),
            ),
        ]),
        if enabled {
            BTreeMap::from([("result".into(), BTreeSet::from([policy.into()]))])
        } else {
            BTreeMap::new()
        },
        BTreeMap::from([
            ("result".into(), retained.clone()),
            ("snapshot".into(), retained),
        ]),
        Some("result".into()),
    );
    BTreeMap::from([
        ("operator/attest-fixes".into(), attest),
        ("operator/repair-accept".into(), accept),
    ])
}

impl ReviewTaskDomain {
    fn repair_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskRepairContextV1, String> {
        let key =
            review_store::content_id(&serde_json::to_value(input).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if let Some(value) = self
            .memo
            .lock()
            .map_err(|_| "Review memo poisoned")?
            .repairs
            .get(&key)
            .cloned()
        {
            return Ok(value);
        }
        let value = self.build_repair_context(cas, input)?;
        let mut memo = self.memo.lock().map_err(|_| "Review memo poisoned")?;
        if memo.repairs.len() < 64 {
            memo.repairs.insert(key, value.clone());
        }
        Ok(value)
    }

    fn build_repair_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskRepairContextV1, String> {
        let current_id = source_input(cas, input.inputs.get("source").ok_or("Repair lacks S2")?)?;
        let previous_id =
            source_input(cas, input.inputs.get("previous").ok_or("Repair lacks S1")?)?;
        let (current, current_manifest) = read_snapshot(cas, &current_id)?;
        let (previous, previous_manifest) = read_snapshot(cas, &previous_id)?;
        if current.origin_id != previous.origin_id
            || current.parent_snapshot_id.as_ref() != Some(&previous_id)
        {
            return Err("Repair must attest the sealed S1-to-S2 transition".into());
        }
        let (review_id, _): (_, TaskReviewRoundV1) =
            self.value(cas, input, "review", TASK_REVIEW_ROUND_V1)?;
        let (mut ledger, review) = self.restore_round(cas, &review_id, 0)?;
        if review.invocation.plan_id != input.plan_id
            || review.snapshot_id != previous_id
            || review.outcome != ReceiptOutcomeV1::Failed
        {
            return Err(
                "Repair requires this plan's original complete finding-bearing S1 Review".into(),
            );
        }
        let (history_id, _): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        if self.reduce_outputs(cas, &review.invocation)?.get("history")
            != input.inputs.get("history")
        {
            return Err("Repair cannot rewrite the original closed Review Round".into());
        }
        let prior_subject = self.current_subject(cas, &review.invocation)?;
        let base_id = prior_subject
            .subject
            .base_snapshot_id
            .as_ref()
            .ok_or("Targeted implementation repair requires original S0-to-S1 Diff")?;
        let (_, base) = read_snapshot(cas, base_id)?;
        let changes = review_source_git::git::manifest_diff(&base, &current_manifest, cas)
            .map_err(|e| e.to_string())?
            .change_set(base_id, &current_id)?;
        if changes.changed_paths.is_empty() {
            return Err("Repair has an empty S0-to-S2 Subject".into());
        }
        let changes_id = cas
            .put_json(&serde_json::to_value(&changes).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let subject = SubjectV1::diff(&current_id, base_id, changes_id);
        let subject_id = cas
            .put_json(&serde_json::to_value(&subject).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let originals: Vec<_> = ledger
            .finding_views()
            .into_iter()
            .filter(|f| f.status.is_active())
            .collect();
        if originals.is_empty() || originals.len() > 64 {
            return Err("Targeted repair requires one to 64 preserved original Findings".into());
        }
        ledger
            .bind_task_subject(cas, &subject_id, review.round)
            .map_err(|e| e.to_string())?;
        let patch =
            review_source_git::git::manifest_diff(&previous_manifest, &current_manifest, cas)
                .map_err(|e| e.to_string())?
                .change_set(&previous_id, &current_id)?;
        if patch.changed_paths.is_empty() {
            return Err("Repair did not change sealed S1".into());
        }
        let patch_id = cas
            .put_json(&serde_json::to_value(&patch).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut claims = BTreeMap::new();
        for original in originals {
            let original_view_id = cas
                .put_json(&serde_json::to_value(&original).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let view = ledger
                .finding_view(&original.key)
                .ok_or("Rebound Review lost an original Finding")?;
            let current_view_id = cas
                .put_json(&serde_json::to_value(&view).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let attestation = review_core::ChangeAttestationV1 {
                finding_id: original.key.clone(), expected_finding_view_id: current_view_id.clone(),
                subject_id: subject_id.clone(), change_set_id: Some(patch_id.clone()),
                changed_regions: patch.changed_paths.iter().map(|path| review_core::ChangedRegionV1 {path: path.clone(), start_line: None, end_line: None}).collect(),
                actor: "af/attest-fixes".into(), reason: "Captured sealed S1-to-S2 changed paths; independent verification determines whether this Finding is fixed".into(),
                evidence_ids: vec![review_id.clone(), original_view_id.clone(), current_view_id.clone(), previous_id.clone(), current_id.clone()],
            };
            attestation.validate()?;
            let attestation_id = self
                .put(
                    cas,
                    input,
                    review_core::contract::CHANGE_ATTESTATION_V1,
                    Some(&current_id),
                    &attestation,
                    attestation
                        .evidence_ids
                        .iter()
                        .cloned()
                        .chain([patch_id.clone(), subject_id.clone()])
                        .collect(),
                )?
                .artifact_ids[0]
                .clone();
            claims.insert(
                original.key,
                TaskRepairClaimV1 {
                    original_view_id,
                    current_view_id,
                    file: original.file,
                    line: original.line,
                    title: original.title,
                    body: original.body,
                    remedy: original.fix.unwrap_or_default(),
                    attestation_id,
                    attestation,
                },
            );
        }
        let plan: ExecutionPlanV1 = serde_json::from_value(envelope(cas, &input.plan_id)?.payload)
            .map_err(|e| e.to_string())?;
        let continuation = VerificationContinuationV1 {
            task_revision_id: plan.task_revision_id,
            plan_id: input.plan_id.clone(),
            prior_history_id: history_id,
            previous_subject_id: review.subject_id,
            current_subject_id: subject_id,
            current_snapshot_id: current_id.clone(),
            policy_id: self.policy_id.clone(),
            claims: claims
                .iter()
                .map(|(key, claim)| (key.clone(), claim.current_view_id.clone()))
                .collect(),
        };
        continuation.validate()?;
        let continuation_id = self
            .put(
                cas,
                input,
                VERIFICATION_CONTINUATION_V1,
                Some(&current_id),
                &continuation,
                claims
                    .values()
                    .flat_map(|c| {
                        [
                            c.original_view_id.clone(),
                            c.current_view_id.clone(),
                            c.attestation_id.clone(),
                        ]
                    })
                    .collect(),
            )?
            .artifact_ids[0]
            .clone();
        let value = TaskRepairContextV1 {
            invocation: input.clone(),
            continuation_id,
            continuation,
            subject,
            previous_snapshot_id: previous_id,
            claims,
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn attest_fixes(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let value = self.repair_context(cas, input)?;
        let port = self.put(
            cas,
            input,
            TASK_REPAIR_CONTEXT_V1,
            Some(&value.continuation.current_snapshot_id),
            &value,
            vec![value.continuation_id.clone()],
        )?;
        Ok(BTreeMap::from([("repair".into(), port)]))
    }

    fn current_repair(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskRepairContextV1, String> {
        let (id, context): (_, TaskRepairContextV1) =
            self.value(cas, input, "repair", TASK_REPAIR_CONTEXT_V1)?;
        context.validate()?;
        if context.invocation.plan_id != input.plan_id
            || source_input(
                cas,
                input
                    .inputs
                    .get("source")
                    .ok_or("Fix verification lacks current source")?,
            )? != context.continuation.current_snapshot_id
            || !matches!(
                self.operator(&context.invocation)?,
                TaskOperatorV1::AttestFixes {}
            )
            || self.attest_fixes(cas, &context.invocation)?["repair"].artifact_ids != [id]
        {
            return Err(
                "Fix verification changed its current continuation or attestation authority".into(),
            );
        }
        Ok(context)
    }

    pub(super) fn validate_fix_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        self.current_repair(cas, input)?;
        if self.code.review_checks(cas, input)? != ReceiptOutcomeV1::Passed {
            return Err("Fix verifier cannot dispatch before current S2 checks pass".into());
        }
        Ok(())
    }

    pub(super) fn validate_fix_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let context = self.current_repair(cas, input)?;
        for port in output.outputs.values() {
            for id in &port.artifact_ids {
                let artifact = envelope(cas, id)?;
                if artifact.artifact_type != TASK_FIX_VERIFICATION_V1
                    || artifact.subject_snapshot_id.as_ref()
                        != Some(&context.continuation.current_snapshot_id)
                {
                    return Err("Fix verifier returned another type or stale Snapshot".into());
                }
                let value: TaskFixVerificationV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                value.validate_context(&context)?;
            }
        }
        Ok(())
    }

    pub(super) fn repair_acceptance(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<(ReviewedImplementationV1, RepairAssessmentV1), String> {
        if !self.policy.allow_targeted_repairs {
            return Err("Trusted Review policy does not permit targeted repair acceptance".into());
        }
        let context = self.current_repair(cas, input)?;
        let checks = self.code.review_checks(cas, input)?;
        let verification = if input.inputs.contains_key("verification") {
            let (id, value): (_, TaskFixVerificationV1) =
                self.value(cas, input, "verification", TASK_FIX_VERIFICATION_V1)?;
            value.validate_context(&context)?;
            let artifact = envelope(cas, &id)?;
            let address = self.graph.nodes[&input.node]
                .inputs
                .get("verification")
                .ok_or("Repair acceptance omitted verifier binding")?;
            let node = self
                .graph
                .nodes
                .get(&address.node)
                .ok_or("Unknown repair verifier")?;
            if !matches!(&artifact.producer, Producer::Attempt {run_id,node_id,..} if node_id == &address.node && *run_id == task_run_id(&self.task_for_invocation(cas,input)?.task_id).map_err(|e|e.to_string())?)
                || !matches!(
                    node.operator,
                    CompiledOperator::Primitive {
                        operator: TaskOperatorV1::FixVerify { .. },
                        ..
                    }
                )
                || ["source", "repair", "checks"].iter().any(|name| {
                    node.inputs.get(*name) != self.graph.nodes[&input.node].inputs.get(*name)
                })
                || !input
                    .inputs
                    .values()
                    .filter(|p| p.artifact_type != TASK_FIX_VERIFICATION_V1)
                    .flat_map(|p| &p.artifact_ids)
                    .all(|id| artifact.input_artifacts.contains(id))
            {
                return Err("Fix verification lost its independent current-S2 Attempt or exact declared inputs".into());
            }
            Some((id, value))
        } else {
            None
        };
        let mut claims = BTreeMap::new();
        let mut outcome = checks;
        for (finding, claim) in &context.claims {
            let decision = verification
                .as_ref()
                .map(|(_, v)| v.claims[finding].clone())
                .unwrap_or_else(|| TaskFixDecisionV1 {
                    expected_view_id: claim.current_view_id.clone(),
                    attestation_id: claim.attestation_id.clone(),
                    outcome: VerificationOutcomeV1::Inconclusive,
                    reason: "Required current-Snapshot verifier output is missing".into(),
                });
            match decision.outcome {
                VerificationOutcomeV1::Negative => outcome = ReceiptOutcomeV1::Failed,
                VerificationOutcomeV1::Inconclusive if outcome != ReceiptOutcomeV1::Failed => {
                    outcome = ReceiptOutcomeV1::Inconclusive
                }
                _ => (),
            }
            let receipt = TaskFixReceiptV1 {
                invocation: input.clone(),
                finding_id: finding.clone(),
                continuation_id: context.continuation_id.clone(),
                subject_id: context.continuation.current_subject_id.clone(),
                decision: decision.clone(),
                verifier_output_id: verification.as_ref().map(|(id, _)| id.clone()),
            };
            receipt.validate()?;
            let receipt_id = self
                .put(
                    cas,
                    input,
                    TASK_FIX_RECEIPT_V1,
                    Some(&context.continuation.current_snapshot_id),
                    &receipt,
                    vec![claim.attestation_id.clone()],
                )?
                .artifact_ids[0]
                .clone();
            claims.insert(
                finding.clone(),
                ClaimVerificationV1 {
                    expected_view_id: claim.current_view_id.clone(),
                    attestation_id: claim.attestation_id.clone(),
                    receipt_id,
                    outcome: decision.outcome,
                },
            );
        }
        let (original_id, _): (_, TaskReviewRoundV1) =
            self.value(cas, &context.invocation, "review", TASK_REVIEW_ROUND_V1)?;
        let (original, _) = self.restore_round(cas, &original_id, 0)?;
        if original.required_open_demands() > 0
            || !original.scope_authority_failures().is_empty()
            || original
                .finding_views()
                .iter()
                .any(|f| f.authority_diagnostic)
        {
            outcome = ReceiptOutcomeV1::Failed;
        }
        let assessment = RepairAssessmentV1 {
            continuation_id: context.continuation_id.clone(),
            current_subject_id: context.continuation.current_subject_id.clone(),
            current_snapshot_id: context.continuation.current_snapshot_id.clone(),
            check_receipt_id: input.inputs["checks"].artifact_ids[0].clone(),
            scope: RepairScopeV1::TargetedFixes,
            claims,
        };
        assessment.validate_continuation(&context.continuation_id, &context.continuation)?;
        let receipt = ReviewedImplementationV1 {
            invocation: input.clone(),
            scope: ImplementationReviewScopeV1::TargetedFixes,
            snapshot_id: context.continuation.current_snapshot_id,
            policy_id: self.policy_id.clone(),
            outcome,
        };
        receipt.validate()?;
        Ok((receipt, assessment))
    }

    fn task_for_invocation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskRevisionV1, String> {
        let plan: ExecutionPlanV1 = serde_json::from_value(envelope(cas, &input.plan_id)?.payload)
            .map_err(|e| e.to_string())?;
        serde_json::from_value(envelope(cas, &plan.task_revision_id)?.payload)
            .map_err(|e| e.to_string())
    }

    pub(super) fn accept_repair(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let (receipt, assessment) = self.repair_acceptance(cas, input)?;
        let assessment_port = self.put(
            cas,
            input,
            REPAIR_ASSESSMENT_V1,
            Some(&receipt.snapshot_id),
            &assessment,
            assessment
                .claims
                .values()
                .map(|c| c.receipt_id.clone())
                .collect(),
        )?;
        let result = self.put(
            cas,
            input,
            REPAIR_ALLOWED_IMPLEMENTATION_V1,
            Some(&receipt.snapshot_id),
            &receipt,
            assessment_port.artifact_ids.clone(),
        )?;
        let snapshot = review_source_git::task::source_tree(
            cas,
            invocation_producer(cas, input, None)?,
            &receipt.snapshot_id,
            input
                .inputs
                .values()
                .flat_map(|p| p.artifact_ids.iter().cloned())
                .chain(result.artifact_ids.iter().cloned())
                .collect(),
        )?;
        Ok(BTreeMap::from([
            ("snapshot".into(), snapshot),
            ("result".into(), result),
            ("assessment".into(), assessment_port),
        ]))
    }
}
