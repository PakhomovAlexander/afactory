//! Review is a domain of the common Task runtime. The atomic gather/reducer barrier uses
//! the historical canonical reducer; it owns neither an executor nor a child allowance.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::review::*;
use review_core::task::verification::TASK_CHECK_RECEIPT_V1;
use review_core::task::*;
use review_core::{DemandRequirement, PortCardinality, Producer, Severity, SubjectV1};
use review_graph::task::{CompiledOperator, CompiledTask, OperatorSignature};
use review_graph::{NodeOutcome, RunReport};
use review_source_git::task::{SOURCE_TREE_V1, read_snapshot};
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::store::task::{TaskProjection, task_run_id};
use review_store::{CanonicalStage, Cas, ConvergencePolicy, Ledger};
use serde::{Deserialize, Serialize};

use super::code::CodeTaskDomain;
use super::host::TaskDomain;
use super::source::{invocation_producer, source_input};
use super::{TaskOperatorHost, TaskWorkOutput, envelope};
mod implementation;
mod repair;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTaskPolicy {
    pub schema: String,
    pub check_policy_id: String,
    /// Every named reviewer is required for the atomic gather. Values govern Demands.
    pub reviewers: BTreeMap<String, DemandRequirement>,
    pub gate: Severity,
    pub clean_rounds: u32,
    pub max_rounds: u32,
    #[serde(default)]
    pub allow_targeted_repairs: bool,
}
impl ReviewTaskPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.review-task-policy/1"
            || !review_core::is_digest(&self.check_policy_id)
            || self.reviewers.is_empty()
            || self.reviewers.len() > 64
            || self.reviewers.keys().any(|name| {
                !is_name(name)
                    || matches!(
                        name.as_str(),
                        "source" | "base" | "subject" | "history" | "checks"
                    )
            })
            || self.clean_rounds == 0
            || self.clean_rounds > self.max_rounds
            || self.max_rounds > 16
        {
            return Err(
                "Review policy requires bounded required reviewers, checks and Rounds".into(),
            );
        }
        Ok(())
    }
}

fn port(ty: &str, affinity: PortAffinityV1, optional: bool) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        optional,
        affinity,
        root_default: None,
        covers: BTreeSet::new(),
    }
}
fn same() -> PortAffinityV1 {
    PortAffinityV1::SameAs {
        input: "source".into(),
    }
}

pub fn review_signatures(
    policy_id: &str,
    policy: &ReviewTaskPolicy,
) -> Result<BTreeMap<String, OperatorSignature>, String> {
    policy.validate()?;
    if !review_core::is_digest(policy_id) {
        return Err("Invalid Review policy identity".into());
    }
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
    let source = port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false);
    let history = port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}, false);
    let bind = signature(
        BTreeMap::from([
            ("source".into(), source.clone()),
            (
                "base".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, true),
            ),
            ("history".into(), history.clone()),
        ]),
        BTreeMap::from([(
            "subject".into(),
            port(TASK_REVIEW_SUBJECT_V1, same(), false),
        )]),
        BTreeMap::new(),
        BTreeMap::new(),
        None,
    );
    let mut inputs = BTreeMap::from([
        ("source".into(), source),
        ("history".into(), history),
        (
            "subject".into(),
            port(TASK_REVIEW_SUBJECT_V1, same(), false),
        ),
        ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same(), true)),
    ]);
    for name in policy.reviewers.keys() {
        inputs.insert(
            name.clone(),
            port(review_core::contract::REVIEWER_RESULT_V1, same(), true),
        );
    }
    let retained = inputs
        .keys()
        .filter(|name| *name != "source")
        .cloned()
        .collect();
    let reduce = signature(
        inputs,
        BTreeMap::from([
            ("result".into(), port(TASK_REVIEW_ROUND_V1, same(), false)),
            (
                "findings".into(),
                port(
                    review_core::task::repair::TASK_REVIEW_CLAIMS_V1,
                    same(),
                    false,
                ),
            ),
            (
                "history".into(),
                port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}, false),
            ),
        ]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([("result".into(), retained)]),
        Some("result".into()),
    );
    let mut installed = BTreeMap::from([
        ("operator/review-bind".into(), bind),
        ("operator/review-reduce".into(), reduce),
        (
            "operator/review-accept".into(),
            implementation::signature(policy_id, policy.allow_targeted_repairs),
        ),
    ]);
    installed.extend(repair::signatures(policy_id, policy.allow_targeted_repairs));
    Ok(installed)
}

#[derive(Default)]
struct ReviewMemo {
    rounds: BTreeMap<String, (Ledger, TaskReviewRoundV1)>,
    repairs: BTreeMap<String, review_core::task::repair::TaskRepairContextV1>,
}

pub struct ReviewTaskDomain {
    /// Process-local memo of validated immutable CAS identities under this exact domain.
    memo: std::sync::Mutex<ReviewMemo>,
    review_task: bool,
    policy_id: String,
    policy: ReviewTaskPolicy,
    graph: CompiledTask,
    code: CodeTaskDomain,
}

impl ReviewTaskDomain {
    pub fn captured(cas: &Cas, policy_id: &str, graph: CompiledTask) -> Result<Self, String> {
        let policy: ReviewTaskPolicy =
            serde_json::from_value(cas.get_json(policy_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let installed = review_signatures(policy_id, &policy)?;
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
                && matches!(
                    operator,
                    TaskOperatorV1::ReviewBind {}
                        | TaskOperatorV1::ReviewReduce {}
                        | TaskOperatorV1::ReviewAccept {}
                        | TaskOperatorV1::AttestFixes {}
                        | TaskOperatorV1::RepairAccept {}
                )
                && installed
                    .get(signature)
                    .is_none_or(|s| s.contract != node.contract)
            {
                return Err("Compiled Review operator changed its installed contract".into());
            }
            if matches!(
                node.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::ReviewReduce {},
                    ..
                }
            ) {
                for name in policy.reviewers.keys() {
                    let address = node
                        .inputs
                        .get(name)
                        .ok_or("Review reducer omitted a required reviewer binding")?;
                    let reviewer = graph
                        .nodes
                        .get(&address.node)
                        .ok_or("Review binding has no Worker node")?;
                    if !matches!(
                        reviewer.operator,
                        CompiledOperator::Primitive {
                            operator: TaskOperatorV1::Verify { .. },
                            ..
                        }
                    ) || ["source", "subject", "history", "checks"]
                        .iter()
                        .any(|port| reviewer.inputs.get(*port) != node.inputs.get(*port))
                    {
                        return Err("Required reviewer must use a reserved verification role and the reducer's exact declared inputs".into());
                    }
                }
            }
        }
        for node in graph.nodes.values() {
            if matches!(
                node.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::RepairAccept {},
                    ..
                }
            ) {
                let address = node
                    .inputs
                    .get("verification")
                    .ok_or("Repair acceptance must bind a reserved fix verifier")?;
                let verifier = graph
                    .nodes
                    .get(&address.node)
                    .ok_or("Unknown repair verifier")?;
                if !matches!(
                    verifier.operator,
                    CompiledOperator::Primitive {
                        operator: TaskOperatorV1::FixVerify { .. },
                        ..
                    }
                ) || ["source", "repair", "checks"]
                    .iter()
                    .any(|name| verifier.inputs.get(*name) != node.inputs.get(*name))
                {
                    return Err(
                        "Repair acceptance requires a reserved verifier with exact current inputs"
                            .into(),
                    );
                }
            }
        }
        let code = CodeTaskDomain::captured(cas, &policy.check_policy_id, graph.clone())?;
        Ok(Self {
            memo: std::sync::Mutex::new(ReviewMemo::default()),
            review_task: true,
            policy_id: policy_id.into(),
            policy,
            graph,
            code,
        })
    }
    /// Chosen by the captured Task-kind profile, never by a Worker response or display name.
    pub fn with_review_task(mut self, review: bool) -> Self {
        self.review_task = review;
        self
    }

    fn operator(&self, input: &TaskInvocationV1) -> Result<&TaskOperatorV1, String> {
        match &self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown Review operator")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Not a domain operator".into()),
        }
    }
    fn reviewer(&self, input: &TaskInvocationV1) -> bool {
        self.graph.nodes.get(&input.node).is_some_and(|node| {
            node.contract
                .outputs
                .values()
                .any(|p| p.artifact_type == review_core::contract::REVIEWER_RESULT_V1)
        })
    }
    fn value<T: serde::de::DeserializeOwned>(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        name: &str,
        ty: &str,
    ) -> Result<(String, T), String> {
        let port = input
            .inputs
            .get(name)
            .ok_or_else(|| format!("Missing Review input {name}"))?;
        port.validate()?;
        if port.artifact_type != ty || port.cardinality != PortCardinality::One {
            return Err(format!("Review input {name} changed its type"));
        }
        let id = &port.artifact_ids[0];
        let artifact = envelope(cas, id)?;
        if artifact.artifact_type != ty || artifact.subject_snapshot_id != port.snapshot_id {
            return Err("Review input envelope changed its type or Snapshot".into());
        }
        Ok((
            id.clone(),
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?,
        ))
    }
    fn put<T: Serialize>(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        ty: &str,
        snapshot: Option<&str>,
        value: &T,
        extra: Vec<String>,
    ) -> Result<ArtifactInputV1, String> {
        let refs = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .chain([input.plan_id.clone(), self.policy_id.clone()])
            .chain(extra)
            .collect();
        let id = cas
            .put_artifact(
                ty,
                invocation_producer(cas, input, None)?,
                refs,
                snapshot.map(str::to_owned),
                serde_json::to_value(value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(ArtifactInputV1 {
            artifact_ids: vec![id],
            artifact_type: ty.into(),
            cardinality: PortCardinality::One,
            snapshot_id: snapshot.map(str::to_owned),
        })
    }

    fn bind(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<ArtifactInputV1, String> {
        let snapshot = source_input(
            cas,
            input.inputs.get("source").ok_or("Review has no source")?,
        )?;
        let (_, head) = read_snapshot(cas, &snapshot)?;
        let subject = if let Some(base) = input.inputs.get("base") {
            let base_id = source_input(cas, base)?;
            let (_, base) = read_snapshot(cas, &base_id)?;
            let diff = review_source_git::git::manifest_diff(&base, &head, cas)
                .map_err(|e| e.to_string())?;
            let changes = diff.change_set(&base_id, &snapshot)?;
            if changes.changed_paths.is_empty() {
                return Err("Cannot review an empty Diff Subject".into());
            }
            let id = cas
                .put_json(&serde_json::to_value(changes).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            SubjectV1::diff(&snapshot, &base_id, id)
        } else {
            SubjectV1::whole_tree(&snapshot)
        };
        let subject_id = cas
            .put_json(&serde_json::to_value(&subject).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let (history_id, history): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        history.validate()?;
        let round = match history {
            ReviewHistoryV1::Empty {} => 1,
            ReviewHistoryV1::Recorded {
                round_report_id, ..
            } => {
                let (_, receipt) = self.restore_round(cas, &round_report_id, 0)?;
                receipt
                    .round
                    .checked_add(1)
                    .ok_or("Review Round overflow")?
            }
        };
        if round > self.policy.max_rounds {
            return Err("Review exceeds captured discovery Round bound".into());
        }
        let change_set = subject
            .change_set_id
            .as_ref()
            .map(|id| {
                cas.get_json(id)
                    .map_err(|e| e.to_string())
                    .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            })
            .transpose()?;
        let bound = TaskReviewSubjectV1 {
            subject_id: subject_id.clone(),
            subject,
            change_set,
            snapshot_id: snapshot.clone(),
            prior_history_id: history_id,
            round,
        };
        bound.validate()?;
        self.put(
            cas,
            input,
            TASK_REVIEW_SUBJECT_V1,
            Some(&snapshot),
            &bound,
            vec![subject_id],
        )
    }

    fn current_subject(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskReviewSubjectV1, String> {
        let (_, subject): (_, TaskReviewSubjectV1) =
            self.value(cas, input, "subject", TASK_REVIEW_SUBJECT_V1)?;
        subject.validate()?;
        let (history_id, history): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        history.validate()?;
        let source = source_input(
            cas,
            input
                .inputs
                .get("source")
                .ok_or("Review lacks current source")?,
        )?;
        let resolved = review_store::resolve_subject_scope(cas, &subject.subject_id)
            .map_err(|e| e.to_string())?;
        if subject.prior_history_id != history_id
            || subject.snapshot_id != source
            || resolved.subject.head_snapshot_id != source
            || subject.round > self.policy.max_rounds
        {
            return Err("Review Subject changed its exact history or current Snapshot".into());
        }
        if resolved.subject != subject.subject
            || subject
                .subject
                .change_set_id
                .as_ref()
                .map(|id| cas.get_json(id).map_err(|e| e.to_string()))
                .transpose()?
                != subject
                    .change_set
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|e| e.to_string())?
        {
            return Err("Review context changed its captured Subject or Change Set bytes".into());
        }
        Ok(subject)
    }

    fn prior_ledger(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        subject: &TaskReviewSubjectV1,
        depth: u32,
    ) -> Result<Ledger, String> {
        let (_, history): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        match history {
            ReviewHistoryV1::Empty {} if subject.round == 1 => {
                Ledger::for_task_subject(cas, &subject.subject_id, 1).map_err(|e| e.to_string())
            }
            ReviewHistoryV1::Recorded {
                subject_id,
                finding_set_id,
                demand_set_id,
                round_report_id,
                ..
            } => {
                let (mut ledger, prior) = self.restore_round(cas, &round_report_id, depth + 1)?;
                if subject.round != prior.round + 1
                    || prior.subject_id != subject_id
                    || prior.finding_set_id.as_ref() != Some(&finding_set_id)
                    || prior.demand_set_id.as_ref() != Some(&demand_set_id)
                {
                    return Err("Review history changed its original closed Round and views".into());
                }
                ledger
                    .bind_task_subject(cas, &subject.subject_id, subject.round)
                    .map_err(|e| e.to_string())?;
                Ok(ledger)
            }
            _ => Err("Review Round has no admitted preceding history".into()),
        }
    }

    fn restore_round(
        &self,
        cas: &Cas,
        id: &str,
        depth: u32,
    ) -> Result<(Ledger, TaskReviewRoundV1), String> {
        if depth >= 16 {
            return Err("Review history exceeds bounded Round depth".into());
        }
        if let Some(value) = self
            .memo
            .lock()
            .map_err(|_| "Review memo poisoned")?
            .rounds
            .get(id)
            .cloned()
        {
            return Ok(value);
        }
        let artifact = envelope(cas, id)?;
        let receipt: TaskReviewRoundV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        receipt.validate()?;
        if artifact.artifact_type != TASK_REVIEW_ROUND_V1
            || receipt.policy_id != self.policy_id
            || artifact.producer != invocation_producer(cas, &receipt.invocation, None)?
            || artifact.subject_snapshot_id.as_ref() != Some(&receipt.snapshot_id)
            || !matches!(
                self.operator(&receipt.invocation)?,
                TaskOperatorV1::ReviewReduce {}
            )
        {
            return Err("Review Round has no captured reducer authority".into());
        }
        let (expected, ledger) = self.reduce(cas, &receipt.invocation, depth)?;
        if expected != receipt {
            return Err("Review Round differs from its exact canonical reduction".into());
        }
        let value = (
            ledger.ok_or("Incomplete Review has no authoritative Ledger")?,
            receipt,
        );
        let mut memo = self.memo.lock().map_err(|_| "Review memo poisoned")?;
        if memo.rounds.len() < 64 {
            memo.rounds.insert(id.into(), value.clone());
        }
        Ok(value)
    }

    fn reduce(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        depth: u32,
    ) -> Result<(TaskReviewRoundV1, Option<Ledger>), String> {
        let subject = self.current_subject(cas, input)?;
        let ledger = self.prior_ledger(cas, input, &subject, depth)?;
        let mut results = Vec::new();
        let mut selected = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for name in self.policy.reviewers.keys() {
            if !input.inputs.contains_key(name) {
                missing.insert(name.clone());
                continue;
            }
            let (id, stage): (_, serde_json::Value) =
                self.value(cas, input, name, review_core::contract::REVIEWER_RESULT_V1)?;
            let (_, stage) = crate::reviewer_stage_output(stage)?;
            let artifact = envelope(cas, &id)?;
            review_core::legacy::validate_reviewer_result(&artifact.payload)?;
            let Producer::Attempt {
                run_id,
                node_id,
                attempt_id,
            } = &artifact.producer
            else {
                return Err("Review result has no selected Worker Attempt".into());
            };
            let expected_run = match invocation_producer(cas, input, None)? {
                Producer::KernelOperation { run_id, .. } => run_id,
                _ => unreachable!(),
            };
            let address = self.graph.nodes[&input.node]
                .inputs
                .get(name)
                .ok_or("Review result has no declared producer")?;
            if run_id != &expected_run
                || node_id != &address.node
                || artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
                || !artifact
                    .input_artifacts
                    .contains(&input.inputs["subject"].artifact_ids[0])
            {
                return Err(
                    "Review result changed its Task, Subject, Snapshot or declared Worker".into(),
                );
            }
            selected.insert(name.clone(), id.clone());
            results.push((
                name.clone(),
                id,
                stage,
                attempt_id.clone(),
                artifact.input_artifacts,
            ));
        }
        let checks = if input.inputs.contains_key("checks") {
            self.code.review_checks(cas, input)?
        } else {
            ReceiptOutcomeV1::Inconclusive
        };
        let mut receipt = TaskReviewRoundV1 {
            invocation: input.clone(),
            policy_id: self.policy_id.clone(),
            subject_id: subject.subject_id.clone(),
            snapshot_id: subject.snapshot_id.clone(),
            round: subject.round,
            outcome: ReceiptOutcomeV1::Inconclusive,
            conclusion: ReviewConclusionV1::Incomplete,
            selected_results: selected,
            missing_reviewers: missing,
            finding_set_id: None,
            demand_set_id: None,
        };
        if !receipt.missing_reviewers.is_empty() || checks == ReceiptOutcomeV1::Inconclusive {
            receipt.validate()?;
            return Ok((receipt, None));
        }
        let run_id = match invocation_producer(cas, input, None)? {
            Producer::KernelOperation { run_id, .. } => run_id,
            _ => unreachable!(),
        };
        let stages: Vec<_> = results
            .iter()
            .map(|(name, id, stage, attempt_id, refs)| CanonicalStage {
                source: name,
                demand_requirement: self.policy.reviewers[name],
                stage,
                attempt_id,
                result_artifact_id: id,
                input_artifacts: refs,
                subject_snapshot_id: &subject.snapshot_id,
                subject_id: &subject.subject_id,
                result_contract: review_core::ReviewerResultContract::V1,
            })
            .collect();
        let reduction = review_store::prepare_canonical_review(cas, &run_id, &ledger, &stages)
            .map_err(|e| e.to_string())?;
        let (_, prior): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        let (prior_findings, prior_demands) = match prior {
            ReviewHistoryV1::Empty {} => (
                subject.prior_history_id.clone(),
                subject.prior_history_id.clone(),
            ),
            ReviewHistoryV1::Recorded {
                finding_set_id,
                demand_set_id,
                ..
            } => (finding_set_id, demand_set_id),
        };
        let findings = review_core::FindingSetV1 {
            subject_id: subject.subject_id.clone(),
            round: subject.round,
            prior_finding_set_id: prior_findings,
            reducer_version: reduction.reduction.reducer_version.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: reduction.reduction.selected_report_ids,
            relation_ids: reduction.reduction.relation_ids,
            resolution_ids: vec![],
            findings: crate::finding_set_entries(&reduction.ledger),
        };
        findings.validate()?;
        let demands = review_core::DemandSetV1 {
            subject_id: subject.subject_id.clone(),
            round: subject.round,
            prior_demand_set_id: prior_demands,
            reducer_version: review_core::DEMAND_REDUCER_VERSION.into(),
            selected_demand_artifact_ids: reduction.reduction.selected_demand_artifact_ids,
            satisfaction_artifact_ids: vec![],
            waiver_artifact_ids: vec![],
            demands: reduction.ledger.demand_views(),
        };
        demands.validate()?;
        receipt.finding_set_id = Some(
            self.put(
                cas,
                input,
                review_core::contract::FINDING_SET_V1,
                Some(&subject.snapshot_id),
                &findings,
                reduction.reduction.input_artifact_ids,
            )?
            .artifact_ids[0]
                .clone(),
        );
        receipt.demand_set_id = Some(
            self.put(
                cas,
                input,
                review_core::contract::DEMAND_SET_V1,
                Some(&subject.snapshot_id),
                &demands,
                reduction.reduction.demand_input_artifact_ids,
            )?
            .artifact_ids[0]
                .clone(),
        );
        let convergence = reduction.ledger.convergence(ConvergencePolicy {
            clean_rounds: self.policy.clean_rounds,
            max_rounds: self.policy.max_rounds,
            gate: self.policy.gate,
        });
        receipt.conclusion = if checks == ReceiptOutcomeV1::Passed
            && convergence.verdict == review_store::Verdict::Converged
        {
            ReviewConclusionV1::Pass
        } else if self.policy.max_rounds > 1 && subject.round >= self.policy.max_rounds {
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
        Ok((receipt, Some(reduction.ledger)))
    }

    fn reduce_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let (receipt, ledger) = self.reduce(cas, input, 0)?;
        let refs = receipt
            .finding_set_id
            .iter()
            .chain(receipt.demand_set_id.iter())
            .cloned()
            .collect();
        let result = self.put(
            cas,
            input,
            TASK_REVIEW_ROUND_V1,
            Some(&receipt.snapshot_id),
            &receipt,
            refs,
        )?;
        let mut claims = BTreeMap::new();
        if let Some(ledger) = ledger {
            for finding in ledger
                .finding_views()
                .into_iter()
                .filter(|f| f.status.is_active())
            {
                let view_id = cas
                    .put_json(&serde_json::to_value(&finding).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                claims.insert(
                    finding.key,
                    review_core::task::repair::TaskReviewClaimV1 {
                        view_id,
                        title: finding.title,
                        body: finding.body,
                        remedy: finding.fix.unwrap_or_default(),
                    },
                );
            }
        }
        let findings = self.put(
            cas,
            input,
            review_core::task::repair::TASK_REVIEW_CLAIMS_V1,
            Some(&receipt.snapshot_id),
            &review_core::task::repair::TaskReviewClaimsV1 {
                round_report_id: result.artifact_ids[0].clone(),
                snapshot_id: receipt.snapshot_id.clone(),
                claims,
            },
            result.artifact_ids.clone(),
        )?;
        let (_, prior): (_, ReviewHistoryV1) =
            self.value(cas, input, "history", REVIEW_HISTORY_V1)?;
        let history = match (&receipt.finding_set_id, &receipt.demand_set_id) {
            (Some(finding_set_id), Some(demand_set_id)) => ReviewHistoryV1::Recorded {
                lineage_id: match prior {
                    ReviewHistoryV1::Recorded { lineage_id, .. } => lineage_id,
                    ReviewHistoryV1::Empty {} => input.inputs["history"].artifact_ids[0].clone(),
                },
                subject_id: receipt.subject_id,
                finding_set_id: finding_set_id.clone(),
                demand_set_id: demand_set_id.clone(),
                round_report_id: result.artifact_ids[0].clone(),
            },
            _ => prior,
        };
        let history = self.put(
            cas,
            input,
            REVIEW_HISTORY_V1,
            None,
            &history,
            result.artifact_ids.clone(),
        )?;
        Ok(BTreeMap::from([
            ("result".into(), result),
            ("history".into(), history),
            ("findings".into(), findings),
        ]))
    }

    pub fn result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state
            .execution
            .as_ref()
            .ok_or("Review Task has no execution")?;
        let outputs = self
            .graph
            .outputs
            .iter()
            .filter_map(|(name, address)| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, out)| out.outputs.get(&address.port))
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect();
        let evidence = self
            .graph
            .coverage
            .values()
            .filter_map(|address| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, out)| out.outputs.get(&address.port))
            })
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .collect();
        let mut result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: TaskExecutionV1::Completed,
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "incomplete".into(),
            outputs,
            evidence,
            missing_obligations: BTreeSet::new(),
        };
        self.assess(cas, &state.revision, &mut result)?;
        if report
            .outcomes
            .iter()
            .any(|(_, o)| matches!(o, NodeOutcome::Failed { .. }))
            && result.acceptance != TaskAcceptanceV1::Satisfied
        {
            result.execution = TaskExecutionV1::Exhausted;
        }
        result.validate()?;
        Ok(result)
    }

    fn assess(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        if !self.review_task {
            return self.assess_implementation(cas, task, result);
        }
        let mut conclusions = Vec::new();
        let mut missing = BTreeSet::new();
        for (name, obligation) in &task.acceptance {
            if obligation.evidence_type != TASK_REVIEW_ROUND_V1
                || obligation.verifier_policy != self.policy_id
            {
                return Err("Review Task has unsupported acceptance authority".into());
            }
            let address = self
                .graph
                .coverage
                .get(name)
                .ok_or("Review obligation has no public coverage")?;
            let mut covered = false;
            for id in &result.evidence {
                let artifact = envelope(cas, id)?;
                let Producer::KernelOperation {
                    run_id,
                    node_id: Some(node),
                    ..
                } = &artifact.producer
                else {
                    continue;
                };
                if node != &address.node
                    || run_id != &task_run_id(&task.task_id).map_err(|e| e.to_string())?
                {
                    continue;
                }
                if artifact.artifact_type != TASK_REVIEW_ROUND_V1 {
                    return Err("Review evidence changed its type".into());
                }
                let receipt: TaskReviewRoundV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                receipt.validate()?;
                let plan: ExecutionPlanV1 =
                    serde_json::from_value(envelope(cas, &receipt.invocation.plan_id)?.payload)
                        .map_err(|e| e.to_string())?;
                if plan.task_revision_id != result.task_revision_id
                    || receipt.policy_id != self.policy_id
                    || envelope(cas, &plan.compiled_graph_id)?.payload
                        != serde_json::to_value(&self.graph).map_err(|e| e.to_string())?
                    || receipt.invocation.node != *node
                    || artifact.subject_snapshot_id.as_ref() != Some(&receipt.snapshot_id)
                    || !matches!(
                        self.operator(&receipt.invocation)?,
                        TaskOperatorV1::ReviewReduce {}
                    )
                    || self.reduce(cas, &receipt.invocation, 0)?.0 != receipt
                {
                    return Err(
                        "Review result changed its exact Task, plan or canonical evidence".into(),
                    );
                }
                covered = receipt.conclusion != ReviewConclusionV1::Incomplete;
                conclusions.push(receipt.conclusion);
            }
            if !covered {
                missing.insert(name.clone());
            }
        }
        let complete = missing.is_empty() && !conclusions.is_empty();
        let conclusion = if !complete {
            ReviewConclusionV1::Incomplete
        } else if conclusions.contains(&ReviewConclusionV1::ConvergenceExhausted) {
            ReviewConclusionV1::ConvergenceExhausted
        } else if conclusions.contains(&ReviewConclusionV1::ChangesRequested) {
            ReviewConclusionV1::ChangesRequested
        } else {
            ReviewConclusionV1::Pass
        };
        result.acceptance = if complete {
            TaskAcceptanceV1::Satisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match conclusion {
            ReviewConclusionV1::Pass => "pass",
            ReviewConclusionV1::ChangesRequested => "changes_requested",
            ReviewConclusionV1::ConvergenceExhausted => "convergence_exhausted",
            ReviewConclusionV1::Incomplete => "incomplete",
        }
        .into();
        if complete {
            result.execution = TaskExecutionV1::Completed;
        } else if result.execution == TaskExecutionV1::Completed {
            result.execution = TaskExecutionV1::Exhausted;
        }
        result.missing_obligations = missing;
        conclusion.validate_result(result.execution, result.acceptance, complete)
    }
}

impl TaskOperatorHost for ReviewTaskDomain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.code.prepare_context(cas, input, feedback)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let outputs = match self.operator(input) {
            Ok(TaskOperatorV1::ReviewBind {}) => self
                .bind(cas, input)
                .map(|v| BTreeMap::from([("subject".into(), v)])),
            Ok(TaskOperatorV1::ReviewReduce {}) => self.reduce_outputs(cas, input),
            Ok(TaskOperatorV1::ReviewAccept {}) => self.accept_implementation(cas, input),
            Ok(TaskOperatorV1::AttestFixes {}) => self.attest_fixes(cas, input),
            Ok(TaskOperatorV1::RepairAccept {}) => self.accept_repair(cas, input),
            _ => return self.code.execute(cas, input, attempt),
        };
        TaskWorkOutput {
            outputs,
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskDomain for ReviewTaskDomain {
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        self.result(cas, state, report)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        id: &str,
    ) -> Result<(), String> {
        if matches!(self.operator(input), Ok(TaskOperatorV1::FixVerify { .. })) {
            return self.validate_fix_context(cas, input);
        }
        if self.reviewer(input) {
            self.current_subject(cas, input)?;
            if self.code.review_checks(cas, input)? != ReceiptOutcomeV1::Passed {
                return Err("Reviewer cannot dispatch before current checks pass".into());
            }
            return Ok(());
        }
        self.code.validate_context(cas, input, feedback, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let expected = match self.operator(input)? {
            TaskOperatorV1::ReviewBind {} => {
                Some(BTreeMap::from([("subject".into(), self.bind(cas, input)?)]))
            }
            TaskOperatorV1::ReviewReduce {} => Some(self.reduce_outputs(cas, input)?),
            TaskOperatorV1::ReviewAccept {} => Some(self.accept_implementation(cas, input)?),
            TaskOperatorV1::AttestFixes {} => Some(self.attest_fixes(cas, input)?),
            TaskOperatorV1::RepairAccept {} => Some(self.accept_repair(cas, input)?),
            _ => None,
        };
        if let Some(expected) = expected {
            if expected != output.outputs {
                return Err("Review output differs from its exact domain reduction".into());
            }
            return Ok(());
        }
        if matches!(self.operator(input), Ok(TaskOperatorV1::FixVerify { .. })) {
            return self.validate_fix_output(cas, input, output);
        }
        if self.reviewer(input) {
            let subject = self.current_subject(cas, input)?;
            for port in output.outputs.values() {
                for id in &port.artifact_ids {
                    let artifact = envelope(cas, id)?;
                    if artifact.artifact_type != review_core::contract::REVIEWER_RESULT_V1
                        || artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
                    {
                        return Err("Reviewer output has a stale type or Snapshot".into());
                    }
                    review_core::legacy::validate_reviewer_result(&artifact.payload)?;
                }
            }
            return Ok(());
        }
        self.code.validate_output(cas, task, plan, input, output)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(cas, task, &mut expected)?;
        if &expected != result {
            return Err("Review conclusion contradicts its completeness evidence".into());
        }
        Ok(())
    }
}
