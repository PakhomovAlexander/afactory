//! Deterministic human decisions use the same native Round and canonical Ledger reducer.
use super::*;
use af_contracts::task::review_decisions::*;
use af_contracts::{EventType, ResolutionChallengeKind};

pub(super) fn signature(policy: &str) -> OperatorSignature {
    let mut inputs: BTreeMap<String, PipelinePortV1> = [
        ("source", SOURCE_TREE_V1),
        ("history", REVIEW_HISTORY_V1),
        ("decisions", REVIEW_DECISIONS_V1),
        ("original_result", TASK_RESULT_V1),
        ("original_conclusion", TASK_REVIEW_CONCLUSION_V1),
    ]
    .into_iter()
    .map(|(name, ty)| (name.into(), port(ty, PortAffinityV1::Unbound {}, false)))
    .collect();
    inputs.insert(
        "base".into(),
        port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, true),
    );
    OperatorSignature {
        contract: PipelineContractV1 {
            inputs: inputs.clone(),
            outputs: BTreeMap::from([
                ("result".into(), port(TASK_REVIEW_ROUND_V1, same(), false)),
                (
                    "findings".into(),
                    port(
                        af_contracts::task::repair::TASK_REVIEW_CLAIMS_V1,
                        same(),
                        false,
                    ),
                ),
                (
                    "history".into(),
                    port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}, false),
                ),
            ]),
        },
        evidence: BTreeMap::from([("result".into(), BTreeSet::from([policy.into()]))]),
        retains: BTreeMap::from([("result".into(), inputs.keys().cloned().collect())]),
        effects: BTreeSet::new(),
        roles: BTreeSet::new(),
        unbound_allowed: BTreeSet::from(["base".into()]),
        worker_input_type: None,
        worker_output_type: None,
        attempt: None,
        outcome_port: Some("result".into()),
    }
}

impl ReviewTaskDomain {
    pub(super) fn is_decision(&self, input: &TaskInvocationV1) -> bool {
        matches!(self.graph.nodes.get(&input.node).map(|n| &n.operator), Some(CompiledOperator::Primitive { operator: TaskOperatorV1::ReviewReduce {}, signature }) if signature == "operator/review-decisions")
    }

    pub(super) fn reduce_decisions(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        depth: u32,
    ) -> Result<(TaskReviewRoundV1, Ledger), String> {
        if depth >= 16 {
            return Err("Review decision history exceeds its bounded depth".into());
        }
        if input.inputs != self.graph.inputs {
            return Err("Review decisions changed their exact approved root inputs".into());
        }
        let (_, batch): (_, ReviewDecisionsV1) =
            self.value(cas, input, "decisions", REVIEW_DECISIONS_V1)?;
        batch.validate()?;
        let capture = envelope(cas, &input.inputs["decisions"].artifact_ids[0])?;
        if !matches!(capture.producer, Producer::KernelOperation { ref run_id, node_id: None, ref operation_id } if run_id == "task-file-adapter-v1" && operation_id == "capture@1")
        {
            return Err("Human Review decisions require operator-captured input, never a Worker-authored batch".into());
        }
        if batch.policy_id != self.policy_id {
            return Err("Review decisions changed the original policy".into());
        }
        let (mut ledger, prior) = self.restore_round(cas, &batch.review_id, depth + 1)?;
        let (_, original): (_, TaskResultV1) =
            self.value(cas, input, "original_result", TASK_RESULT_V1)?;
        original.validate()?;
        let (conclusion_id, conclusion): (_, TaskReviewConclusionV1) =
            self.value(cas, input, "original_conclusion", TASK_REVIEW_CONCLUSION_V1)?;
        conclusion.validate()?;
        let report = envelope(cas, &conclusion.task_report_id)?;
        if report.artifact_type != af_contracts::task::run_report::TASK_RUN_REPORT_V3 {
            return Err("Human decisions lost the original common Task report".into());
        }
        let report: af_contracts::task::run_report::TaskRunReportV1 =
            serde_json::from_value(report.payload).map_err(|e| e.to_string())?;
        report.validate()?;
        let original_plan: ExecutionPlan =
            serde_json::from_value(envelope(cas, &prior.invocation.plan_id)?.payload)
                .map_err(|e| e.to_string())?;
        if original.execution != TaskExecutionV1::Completed
            || original.acceptance != TaskAcceptanceV1::Satisfied
            || original.outputs.get("review").is_none_or(|p| {
                p.artifact_type != TASK_REVIEW_ROUND_V1
                    || p.artifact_ids != [batch.review_id.clone()]
            })
            || !original.evidence.contains(&conclusion_id)
            || conclusion.task_revision_id != original.task_revision_id
            || original_plan.task_revision_id != original.task_revision_id
            || original.outputs.get("history") != input.inputs.get("history")
            || conclusion.plan_id != prior.invocation.plan_id
            || conclusion.conclusion != prior.conclusion
            || !conclusion.round_ids.contains(&batch.review_id)
            || report.task_revision_id != original.task_revision_id
            || report.plan_id != conclusion.plan_id
            || report.nodes.iter().any(|n| {
                matches!(
                    n.outcome,
                    af_contracts::task::run_report::TaskNodeOutcomeV1::Failed { .. }
                        | af_contracts::task::run_report::TaskNodeOutcomeV1::Suppressed {
                            reason:
                                af_contracts::task::run_report::TaskSuppressionV1::UpstreamMissing
                        }
                )
            })
        {
            return Err("Human decisions require the original complete Task, selected Review, checked history and exact accounting conclusion; failed required work cannot be bypassed".into());
        }
        let (_, history): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        if !matches!(history, ReviewHistoryV1::Recorded { subject_id, finding_set_id, demand_set_id, round_report_id, .. }
            if subject_id == prior.subject_id && Some(finding_set_id.as_str()) == prior.finding_set_id.as_deref() && Some(demand_set_id.as_str()) == prior.demand_set_id.as_deref() && round_report_id == batch.review_id)
            || batch.subject_id != prior.subject_id
            || batch.snapshot_id != prior.snapshot_id
            || source_input(cas, &input.inputs["source"])? != prior.snapshot_id
            || batch.base_snapshot_id
                != af_store::resolve_subject_scope(cas, &prior.subject_id)
                    .map_err(|e| e.to_string())?
                    .subject
                    .base_snapshot_id
            || input
                .inputs
                .get("base")
                .map(|port| source_input(cas, port))
                .transpose()?
                != batch.base_snapshot_id
        {
            return Err(
                "Review decisions changed original source, Base, complete history or Subject"
                    .into(),
            );
        }
        let mut attached = BTreeMap::new();
        for (index, action) in batch.actions.iter().enumerate() {
            match action {
                ReviewDecisionV1::Attach { demand_id, content } => {
                    if ledger.demand(demand_id).is_none() {
                        return Err("Evidence names an unknown original Demand".into());
                    }
                    let content_id = cas.put(content.as_bytes()).map_err(|e| e.to_string())?;
                    let evidence = af_contracts::EvidenceV1 {
                        demand_id: demand_id.clone(),
                        subject_id: batch.subject_id.clone(),
                        content_artifact_id: content_id.clone(),
                        actor: batch.actor.clone(),
                    };
                    let id = self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceAddedV1,
                        af_contracts::contract::EVIDENCE_V1,
                        &evidence,
                        vec![content_id],
                    )?;
                    attached.insert(index as u32, id);
                }
                ReviewDecisionV1::Satisfy {
                    demand_id,
                    evidence,
                    reason,
                } => {
                    let evidence_id = match evidence {
                        DecisionEvidenceV1::Recorded { artifact_id } => artifact_id,
                        DecisionEvidenceV1::Attached { action } => attached
                            .get(action)
                            .ok_or("Satisfaction has no preceding attachment")?,
                    };
                    let record = ledger
                        .demand(demand_id)
                        .ok_or("Satisfaction names an unknown Demand")?;
                    let evidence = record.evidence.iter().find(|e| &e.artifact_id == evidence_id).ok_or("Satisfaction requires attached Evidence of this exact Demand; use explicit reuse_evidence for another Demand")?;
                    if evidence.evidence.subject_id != batch.subject_id {
                        return Err("Stale Evidence requires explicit reuse authority".into());
                    }
                    cas.verify(&evidence.evidence.content_artifact_id)
                        .map_err(|e| e.to_string())?;
                    let value = af_contracts::EvidenceSatisfactionV1 {
                        demand_id: demand_id.clone(),
                        evidence_id: evidence_id.clone(),
                        subject_id: batch.subject_id.clone(),
                        policy_revision: batch.policy_id.clone(),
                        reason: reason.clone(),
                    };
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceSatisfiedV1,
                        af_contracts::contract::EVIDENCE_SATISFACTION_V1,
                        &value,
                        vec![evidence_id.clone()],
                    )?;
                }
                ReviewDecisionV1::Reuse {
                    demand_id,
                    satisfaction_id,
                    reason,
                } => {
                    let record = ledger
                        .demand(demand_id)
                        .ok_or("Reuse names an unknown Demand")?;
                    let satisfaction = record
                        .satisfactions
                        .iter()
                        .find(|s| &s.artifact_id == satisfaction_id)
                        .ok_or("Reuse names no satisfaction of this Demand")?;
                    let evidence = record
                        .evidence
                        .iter()
                        .find(|e| e.artifact_id == satisfaction.satisfaction.evidence_id)
                        .ok_or("Reuse lost its original Evidence")?;
                    cas.verify(&evidence.evidence.content_artifact_id)
                        .map_err(|e| e.to_string())?;
                    let value = af_contracts::EvidenceReuseAdmissionV1 {
                        demand_id: demand_id.clone(),
                        satisfaction_id: satisfaction_id.clone(),
                        subject_id: satisfaction.satisfaction.subject_id.clone(),
                        actor: batch.actor.clone(),
                        policy_revision: batch.policy_id.clone(),
                        reason: reason.clone(),
                    };
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceReuseAdmittedV1,
                        af_contracts::contract::EVIDENCE_REUSE_ADMISSION_V1,
                        &value,
                        vec![],
                    )?;
                }
                ReviewDecisionV1::ReuseEvidence {
                    demand_id,
                    evidence_id,
                    reason,
                } => {
                    let source = ledger
                        .demand_views()
                        .iter()
                        .find_map(|d| {
                            let record = ledger.demand(&d.demand_id)?;
                            record
                                .evidence
                                .iter()
                                .find(|e| &e.artifact_id == evidence_id)
                                .map(|e| (record.demand.clone(), e.clone()))
                        })
                        .ok_or("Reuse names no attached original Evidence")?;
                    let target = ledger
                        .demand(demand_id)
                        .ok_or("Reuse names an unknown target Demand")?;
                    if source.0.claim != target.demand.claim
                        || source.0.why != target.demand.why
                        || source.0.suggested_method != target.demand.suggested_method
                        || source.0.requirement != target.demand.requirement
                    {
                        return Err("Cross-Demand reuse requires identical claim, method, reason and requirement".into());
                    }
                    cas.verify(&source.1.evidence.content_artifact_id)
                        .map_err(|e| e.to_string())?;
                    let evidence = af_contracts::EvidenceV1 {
                        demand_id: demand_id.clone(),
                        subject_id: batch.subject_id.clone(),
                        content_artifact_id: source.1.evidence.content_artifact_id,
                        actor: batch.actor.clone(),
                    };
                    let id = self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceAddedV1,
                        af_contracts::contract::EVIDENCE_V1,
                        &evidence,
                        vec![source.1.record_id],
                    )?;
                    let satisfaction = af_contracts::EvidenceSatisfactionV1 {
                        demand_id: demand_id.clone(),
                        evidence_id: id,
                        subject_id: batch.subject_id.clone(),
                        policy_revision: batch.policy_id.clone(),
                        reason: reason.clone(),
                    };
                    let id = self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceSatisfiedV1,
                        af_contracts::contract::EVIDENCE_SATISFACTION_V1,
                        &satisfaction,
                        vec![evidence_id.clone()],
                    )?;
                    let admission = af_contracts::EvidenceReuseAdmissionV1 {
                        demand_id: demand_id.clone(),
                        satisfaction_id: id,
                        subject_id: batch.subject_id.clone(),
                        actor: batch.actor.clone(),
                        policy_revision: batch.policy_id.clone(),
                        reason: reason.clone(),
                    };
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::EvidenceReuseAdmittedV1,
                        af_contracts::contract::EVIDENCE_REUSE_ADMISSION_V1,
                        &admission,
                        vec![evidence_id.clone()],
                    )?;
                }
                ReviewDecisionV1::Waive { waiver } => {
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::DemandWaivedV1,
                        af_contracts::contract::DEMAND_WAIVER_V1,
                        waiver,
                        vec![],
                    )?;
                }
                ReviewDecisionV1::Resolve { resolution } => {
                    for id in &resolution.evidence_ids {
                        cas.verify(id).map_err(|e| e.to_string())?;
                    }
                    let view = ledger
                        .finding_view(&resolution.finding_id)
                        .ok_or("Resolution names an unknown Finding")?;
                    let view_id = cas
                        .put_json(&serde_json::to_value(&view).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                    if view_id != resolution.expected_finding_view_id {
                        return Err("Resolution changed its exact current Finding view".into());
                    }
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::FindingResolutionRecordedV1,
                        af_contracts::contract::FINDING_RESOLUTION_V1,
                        resolution,
                        resolution
                            .evidence_ids
                            .iter()
                            .cloned()
                            .chain([view_id])
                            .collect(),
                    )?;
                }
                ReviewDecisionV1::Challenge { challenge } => {
                    self.validate_human_challenge(cas, &ledger, challenge)?;
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::FindingResolutionChallengedV1,
                        af_contracts::contract::RESOLUTION_CHALLENGE_V1,
                        challenge,
                        challenge.evidence_ids.clone(),
                    )?;
                }
                ReviewDecisionV1::PolicyTime { time } => {
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        EventType::PolicyTimeAdvancedV1,
                        af_contracts::contract::POLICY_TIME_V1,
                        time,
                        vec![],
                    )?;
                    let expired: Vec<_> = ledger
                        .expiring_resolutions(time.tick)
                        .into_iter()
                        .cloned()
                        .collect();
                    for resolution in expired {
                        let challenge = af_contracts::ResolutionChallengeV1 {
                            finding_id: resolution.resolution.finding_id,
                            resolution_id: resolution.artifact_id,
                            subject_id: batch.subject_id.clone(),
                            kind: ResolutionChallengeKind::Expired,
                            actor: batch.actor.clone(),
                            reason: time.reason.clone(),
                            evidence_ids: vec![],
                        };
                        self.decision_record(
                            cas,
                            input,
                            &mut ledger,
                            EventType::FindingResolutionChallengedV1,
                            af_contracts::contract::RESOLUTION_CHALLENGE_V1,
                            &challenge,
                            vec![resolution.record_id],
                        )?;
                    }
                }
                ReviewDecisionV1::Group {
                    grouping,
                    from_view_id,
                    into_view_id,
                } => {
                    if grouping.round != prior.round
                        || ledger.finding_view_id(&grouping.from).as_ref() != Some(from_view_id)
                        || ledger.finding_view_id(&grouping.into).as_ref() != Some(into_view_id)
                    {
                        return Err("Grouping changed an exact Finding view or Round".into());
                    }
                    self.decision_record(
                        cas,
                        input,
                        &mut ledger,
                        if grouping.action == af_contracts::FindingGroupingAction::Group {
                            EventType::FindingsGroupedV1
                        } else {
                            EventType::FindingsUngroupedV1
                        },
                        af_contracts::contract::FINDING_GROUPING_V1,
                        grouping,
                        vec![],
                    )?;
                }
            }
        }
        let old_findings = prior
            .finding_set_id
            .clone()
            .ok_or("Human decisions require a complete original Finding Set")?;
        let old_demands = prior
            .demand_set_id
            .clone()
            .ok_or("Human decisions require a complete original Demand Set")?;
        let mut findings: af_contracts::FindingSetV1 =
            serde_json::from_value(envelope(cas, &old_findings)?.payload)
                .map_err(|e| e.to_string())?;
        findings.prior_finding_set_id = old_findings;
        findings.resolution_ids = ledger.resolution_artifact_ids();
        findings.relation_ids.extend(ledger.grouping_relation_ids());
        findings.relation_ids.sort();
        findings.relation_ids.dedup();
        findings.findings = crate::finding_set_entries(&ledger);
        findings.validate()?;
        let mut demands: af_contracts::DemandSetV1 =
            serde_json::from_value(envelope(cas, &old_demands)?.payload)
                .map_err(|e| e.to_string())?;
        demands.prior_demand_set_id = old_demands;
        let (_, satisfactions, waivers) = ledger.demand_reduction_artifact_ids();
        demands.satisfaction_artifact_ids = satisfactions;
        demands.waiver_artifact_ids = waivers;
        demands.demands = ledger.demand_views();
        demands.validate()?;
        let mut receipt = prior.clone();
        receipt.invocation = input.clone();
        receipt.finding_set_id = Some(
            self.put(
                cas,
                input,
                af_contracts::contract::FINDING_SET_V1,
                Some(&batch.snapshot_id),
                &findings,
                ledger
                    .resolution_input_artifact_ids()
                    .into_iter()
                    .chain(ledger.grouping_input_artifact_ids())
                    .collect(),
            )?
            .artifact_ids[0]
                .clone(),
        );
        receipt.demand_set_id = Some(
            self.put(
                cas,
                input,
                af_contracts::contract::DEMAND_SET_V1,
                Some(&batch.snapshot_id),
                &demands,
                ledger.demand_reduction_input_ids(),
            )?
            .artifact_ids[0]
                .clone(),
        );
        let converged = ledger
            .convergence(ConvergencePolicy {
                gate: self.policy.gate,
                clean_rounds: self.policy.clean_rounds,
                max_rounds: self.policy.max_rounds,
            })
            .verdict
            == af_store::Verdict::Converged;
        receipt.conclusion = if converged && self.original_checks_passed(cas, &prior, depth + 1)? {
            ReviewConclusionV1::Pass
        } else if prior.round >= self.policy.max_rounds && self.policy.max_rounds > 1 {
            ReviewConclusionV1::ConvergenceExhausted
        } else {
            ReviewConclusionV1::ChangesRequested
        };
        receipt.outcome = if receipt.conclusion == ReviewConclusionV1::Pass {
            ReceiptOutcomeV1::Passed
        } else {
            ReceiptOutcomeV1::Failed
        };
        receipt.validate()?;
        Ok((receipt, ledger))
    }

    // The caller has already recomputed the complete prior Ledger. Walk only that checked
    // lineage's immutable links here: restoring every prefix again would multiply reduction
    // work with each human Task. This walk grants no independent history authority.
    fn original_checks_passed(
        &self,
        cas: &Cas,
        prior: &TaskReviewRoundV1,
        depth: u32,
    ) -> Result<bool, String> {
        let mut prior = prior.clone();
        for _ in depth..16 {
            if prior
                .invocation
                .inputs
                .get("decisions")
                .is_some_and(|port| port.artifact_type == REVIEW_DECISIONS_V1)
            {
                let (_, batch): (_, ReviewDecisionsV1) =
                    self.value(cas, &prior.invocation, "decisions", REVIEW_DECISIONS_V1)?;
                let artifact = envelope(cas, &batch.review_id)?;
                if artifact.artifact_type != TASK_REVIEW_ROUND_V1 {
                    return Err("Decision history lost its current Round type".into());
                }
                prior = serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                prior.validate()?;
                continue;
            }
            let plan: ExecutionPlan =
                serde_json::from_value(envelope(cas, &prior.invocation.plan_id)?.payload)
                    .map_err(|e| e.to_string())?;
            let graph: CompiledTask =
                serde_json::from_value(envelope(cas, &plan.compiled_graph_id)?.payload)
                    .map_err(|e| e.to_string())?;
            let original = Self::captured(cas, &prior.policy_id, graph)?;
            return Ok(
                original.code.review_checks(cas, &prior.invocation)? == ReceiptOutcomeV1::Passed
            );
        }
        Err("Review decision checks exceed bounded history".into())
    }

    fn validate_human_challenge(
        &self,
        cas: &Cas,
        ledger: &Ledger,
        challenge: &af_contracts::ResolutionChallengeV1,
    ) -> Result<(), String> {
        let resolution = ledger
            .resolution(&challenge.finding_id)
            .ok_or("Challenge has no current Resolution")?;
        if resolution.artifact_id != challenge.resolution_id {
            return Err("Challenge changed its original Resolution".into());
        }
        for id in &challenge.evidence_ids {
            cas.verify(id).map_err(|e| e.to_string())?;
        }
        let justified = match challenge.kind {
            ResolutionChallengeKind::NewEvidence => {
                let original: BTreeSet<_> = resolution
                    .resolution
                    .evidence_ids
                    .iter()
                    .map(|id| evidence_content(cas, id))
                    .collect::<Result<_, _>>()?;
                challenge
                    .evidence_ids
                    .iter()
                    .map(|id| evidence_content(cas, id))
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .any(|id| !original.contains(id))
            }
            ResolutionChallengeKind::OutsideScope => {
                resolution.resolution.subject_id != challenge.subject_id
            }
            ResolutionChallengeKind::Expired => resolution
                .resolution
                .expires_at_policy_time
                .is_some_and(|t| t <= ledger.policy_time()),
            ResolutionChallengeKind::HigherSeverity => {
                let original: af_store::Finding = serde_json::from_value(
                    cas.get_json(&resolution.resolution.expected_finding_view_id)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                ledger.finding_view(&challenge.finding_id).is_some_and(|f| {
                    f.severity.rank()
                        > resolution
                            .resolution
                            .max_accepted_severity
                            .unwrap_or(original.severity)
                            .rank()
                })
            }
        };
        if !justified {
            return Err(
                "Challenge has no materially new evidence, severity, scope or expiry".into(),
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn decision_record<T: Serialize>(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        ledger: &mut Ledger,
        event_type: EventType,
        ty: &str,
        value: &T,
        extra: Vec<String>,
    ) -> Result<String, String> {
        let snapshot = ledger
            .active_head_snapshot_id()
            .ok_or("Decisions lost their Subject")?
            .to_owned();
        let id = self
            .put(cas, input, ty, Some(&snapshot), value, extra)?
            .artifact_ids[0]
            .clone();
        let record = envelope(cas, &id)?;
        let payload = if matches!(
            event_type,
            EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1
        ) {
            serde_json::json!({"from":record.payload["from"], "into":record.payload["into"], "grouping_artifact_id":id})
        } else {
            serde_json::json!({"artifact_id":id})
        };
        ledger
            .apply_event(
                &af_contracts::RunEvent {
                    event_id: id.clone(),
                    run_id: "native-review-decision".into(),
                    sequence: 0,
                    event_type,
                    occurred_at: String::new(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: None,
                    artifact_refs: vec![id.clone()],
                    payload,
                },
                cas,
            )
            .map_err(|e| e.to_string())?;
        // The canonical typed identity, rather than its CAS envelope reference.
        Ok(record.artifact_id)
    }
}

fn evidence_content(cas: &Cas, id: &str) -> Result<String, String> {
    cas.verify(id).map_err(|e| e.to_string())?;
    let Ok(record) = cas.get_artifact(id) else {
        return Ok(id.to_owned());
    };
    if record.artifact_type == af_contracts::contract::EVIDENCE_V1 {
        let evidence: af_contracts::EvidenceV1 =
            serde_json::from_value(record.payload).map_err(|e| e.to_string())?;
        evidence.validate()?;
        cas.verify(&evidence.content_artifact_id)
            .map_err(|e| e.to_string())?;
        Ok(evidence.content_artifact_id)
    } else {
        Ok(record.content_id)
    }
}
