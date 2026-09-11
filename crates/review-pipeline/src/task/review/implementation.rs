//! Implementation acceptance composes the same canonical Review used by standalone Tasks.
use super::*;
use review_core::task::verification::{
    ImplementationReviewScopeV1, REPAIR_ALLOWED_IMPLEMENTATION_V1, REVIEWED_IMPLEMENTATION_V1,
    ReviewedImplementationV1,
};

pub(super) fn signature(policy: &str, allow_targeted: bool) -> OperatorSignature {
    let retained = BTreeSet::from(["review".into(), "checks".into()]);
    OperatorSignature {
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([
                (
                    "source".into(),
                    port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
                ),
                ("review".into(), port(TASK_REVIEW_ROUND_V1, same(), false)),
                ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same(), false)),
            ]),
            outputs: BTreeMap::from([
                ("snapshot".into(), port(SOURCE_TREE_V1, same(), false)),
                (
                    "result".into(),
                    port(REVIEWED_IMPLEMENTATION_V1, same(), false),
                ),
                (
                    "repair_result".into(),
                    port(REPAIR_ALLOWED_IMPLEMENTATION_V1, same(), false),
                ),
            ]),
        },
        effects: BTreeSet::new(),
        evidence: BTreeMap::from([
            ("result".into(), BTreeSet::from([policy.into()])),
            (
                "repair_result".into(),
                if allow_targeted {
                    BTreeSet::from([policy.into()])
                } else {
                    BTreeSet::new()
                },
            ),
        ]),
        retains: BTreeMap::from([
            ("result".into(), retained.clone()),
            ("repair_result".into(), retained.clone()),
            ("snapshot".into(), retained),
        ]),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: Some("result".into()),
        attempt: None,
    }
}

impl ReviewTaskDomain {
    fn implementation_receipt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ReviewedImplementationV1, String> {
        let source = input
            .inputs
            .get("source")
            .ok_or("Implementation acceptance lacks source")?;
        let snapshot_id = source_input(cas, source)?;
        let (id, review): (_, TaskReviewRoundV1) =
            self.value(cas, input, "review", TASK_REVIEW_ROUND_V1)?;
        review.validate()?;
        let artifact = envelope(cas, &id)?;
        if review.invocation.plan_id != input.plan_id
            || review.snapshot_id != snapshot_id
            || review.policy_id != self.policy_id
            || artifact.producer != invocation_producer(cas, &review.invocation, None)?
            || !matches!(
                self.operator(&review.invocation)?,
                TaskOperatorV1::ReviewReduce {}
            )
            || self.reduce(cas, &review.invocation, 0)?.0 != review
            || review.invocation.inputs.get("checks") != input.inputs.get("checks")
            || source_input(
                cas,
                review
                    .invocation
                    .inputs
                    .get("source")
                    .ok_or("Review lacks its current source")?,
            )? != snapshot_id
        {
            return Err(
                "Implementation requires the exact embedded Review and its current checks".into(),
            );
        }
        // Recheck named policy obligations, not merely a caller-supplied passing outcome.
        let checks = self.code.review_checks(cas, input)?;
        let outcome = match checks {
            ReceiptOutcomeV1::Passed => review.outcome,
            other => other,
        };
        let value = ReviewedImplementationV1 {
            invocation: input.clone(),
            scope: ImplementationReviewScopeV1::CompleteReview,
            snapshot_id,
            policy_id: self.policy_id.clone(),
            outcome,
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn accept_implementation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let value = self.implementation_receipt(cas, input)?;
        let result = self.put(
            cas,
            input,
            REVIEWED_IMPLEMENTATION_V1,
            Some(&value.snapshot_id),
            &value,
            vec![],
        )?;
        let repair_result = self.put(
            cas,
            input,
            REPAIR_ALLOWED_IMPLEMENTATION_V1,
            Some(&value.snapshot_id),
            &value,
            vec![],
        )?;
        let refs = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .chain(result.artifact_ids.iter().cloned())
            .chain(repair_result.artifact_ids.iter().cloned())
            .collect();
        let source = review_source_git::task::source_tree(
            cas,
            invocation_producer(cas, input, None)?,
            &value.snapshot_id,
            refs,
        )?;
        Ok(BTreeMap::from([
            ("snapshot".into(), source),
            ("result".into(), result),
            ("repair_result".into(), repair_result),
        ]))
    }

    pub(super) fn assess_implementation(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        if task.acceptance.values().all(|a| {
            !matches!(
                a.evidence_type.as_str(),
                REVIEWED_IMPLEMENTATION_V1 | REPAIR_ALLOWED_IMPLEMENTATION_V1
            )
        }) {
            return self.code.assess(cas, task, result);
        }
        let mut missing = BTreeSet::new();
        let mut failed = false;
        for (name, obligation) in &task.acceptance {
            if !matches!(
                obligation.evidence_type.as_str(),
                REVIEWED_IMPLEMENTATION_V1 | REPAIR_ALLOWED_IMPLEMENTATION_V1
            ) || obligation.verifier_policy != self.policy_id
            {
                return Err("Reviewed implementation changed its acceptance authority".into());
            }
            let address = self
                .graph
                .coverage
                .get(name)
                .ok_or("Implementation has no public review coverage")?;
            let origins = self.graph.evidence_origins(address)?;
            let mut found = false;
            let mut passed = false;
            for id in &result.evidence {
                let artifact = envelope(cas, id)?;
                if !matches!(&artifact.producer, Producer::KernelOperation {run_id, node_id:Some(node), ..} if origins.iter().any(|a| &a.node == node) && *run_id == task_run_id(&task.task_id).map_err(|e|e.to_string())?)
                {
                    continue;
                }
                if found || artifact.artifact_type != obligation.evidence_type {
                    return Err(
                        "Implementation has ambiguous or incompatible acceptance evidence".into(),
                    );
                }
                found = true;
                let receipt: ReviewedImplementationV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                receipt.validate()?;
                let plan_artifact = envelope(cas, &receipt.invocation.plan_id)?;
                let plan: ExecutionPlanV1 =
                    serde_json::from_value(plan_artifact.payload).map_err(|e| e.to_string())?;
                if plan_artifact.artifact_type != EXECUTION_PLAN_V1
                    || plan.task_revision_id != result.task_revision_id
                    || plan.authority != task.authority
                    || !origins.iter().any(|a| a.node == receipt.invocation.node)
                    || envelope(cas, &plan.compiled_graph_id)?.payload
                        != serde_json::to_value(&self.graph).map_err(|e| e.to_string())?
                    || !matches!(
                        self.operator(&receipt.invocation)?,
                        TaskOperatorV1::ReviewAccept {} | TaskOperatorV1::RepairAccept {}
                    )
                    || (if matches!(
                        self.operator(&receipt.invocation)?,
                        TaskOperatorV1::RepairAccept {}
                    ) {
                        if obligation.evidence_type != REPAIR_ALLOWED_IMPLEMENTATION_V1 {
                            return Err("Targeted repairs cannot satisfy complete S2 review".into());
                        }
                        self.repair_acceptance(cas, &receipt.invocation)?.0
                    } else {
                        self.implementation_receipt(cas, &receipt.invocation)?
                    }) != receipt
                {
                    return Err(
                        "Implementation receipt changed its exact Task, plan or Review evidence"
                            .into(),
                    );
                }
                let expected = if matches!(
                    self.operator(&receipt.invocation)?,
                    TaskOperatorV1::RepairAccept {}
                ) {
                    self.accept_repair(cas, &receipt.invocation)?
                } else {
                    self.accept_implementation(cas, &receipt.invocation)?
                };
                if !origins
                    .iter()
                    .filter(|a| a.node == receipt.invocation.node)
                    .any(|a| {
                        expected
                            .get(&a.port)
                            .is_some_and(|p| p.artifact_ids == [id.clone()])
                    })
                    || result.outputs.get("snapshot") != expected.get("snapshot")
                {
                    return Err("Implementation receipt is stale for its public Snapshot".into());
                }
                passed = receipt.outcome == ReceiptOutcomeV1::Passed;
                failed |= receipt.outcome == ReceiptOutcomeV1::Failed;
            }
            if !passed {
                missing.insert(name.clone());
            }
        }
        result.acceptance = if missing.is_empty() {
            TaskAcceptanceV1::Satisfied
        } else if failed {
            TaskAcceptanceV1::Unsatisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match result.acceptance {
            TaskAcceptanceV1::Satisfied => "verified",
            TaskAcceptanceV1::Unsatisfied => "changes_requested",
            TaskAcceptanceV1::Inconclusive => "incomplete",
        }
        .into();
        result.missing_obligations = missing;
        Ok(())
    }
}
