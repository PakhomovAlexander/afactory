//! Review is a domain of the common Task runtime. The atomic gather/reducer barrier uses
//! the historical canonical reducer; it owns neither an executor nor a child allowance.

mod context;

use std::collections::{BTreeMap, BTreeSet};

use af_contracts::task::execution::{TaskInvocationV1, TaskOutputV1};
use af_contracts::task::pipeline::*;
use af_contracts::task::plan::ExecutionPlan;
use af_contracts::task::review::*;
use af_contracts::task::verification::TASK_CHECK_RECEIPT_V2;
use af_contracts::task::*;
use af_contracts::{DemandRequirement, PortCardinality, Producer, Severity, SubjectV1};
use af_graph::task::{CompiledOperator, CompiledTask, OperatorSignature};
use af_graph::{NodeOutcome, RunReport};
use af_source_git::task::{SOURCE_TREE_V1, read_snapshot};
use af_store::store::task::execution::PreparedTaskAttempt;
use af_store::store::task::{TaskProjection, task_run_id};
use af_store::{CanonicalStage, Cas, ConvergencePolicy, Ledger};
use serde::{Deserialize, Serialize};

use super::code::CodeTaskDomain;
use super::host::TaskDomain;
use super::source::{invocation_producer, source_input};
use super::{TaskOperatorHost, TaskWorkOutput, envelope};
mod continuation;
mod decisions;
mod implementation;
mod integration;
pub mod proposal;
mod repair;
mod scatter;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTaskPolicy {
    pub schema: String,
    pub check_policy_id: String,
    /// Every `required` reviewer must be bound by a review Pipeline; an `advisory` one may stay
    /// unbound (`OperatorSignature::unbound_allowed`). Values also govern Demands.
    pub reviewers: BTreeMap<String, DemandRequirement>,
    pub gate: Severity,
    pub clean_rounds: u32,
    pub max_rounds: u32,
    pub allow_targeted_repairs: bool,
}
/// The one Task Review policy: Subject@3, one assignment per reviewer and ReviewerResult@2.
pub const REVIEW_TASK_POLICY_SCHEMA: &str = "af.review-task-policy/2";
const RESULT_TYPE: &str = af_contracts::contract::REVIEWER_RESULT_V2;

impl ReviewTaskPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REVIEW_TASK_POLICY_SCHEMA
            || !af_contracts::is_digest(&self.check_policy_id)
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
        brief: None,
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
    if !af_contracts::is_digest(policy_id) {
        return Err("Invalid Review policy identity".into());
    }
    let signature = |inputs, outputs, evidence, retains, outcome_port| OperatorSignature {
        unbound_allowed: Default::default(),
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
    let mut bind_outputs = BTreeMap::from([(
        "subject".into(),
        port(TASK_REVIEW_SUBJECT_V3, same(), false),
    )]);
    for name in policy.reviewers.keys() {
        bind_outputs.insert(name.clone(), port(TASK_REVIEW_ASSIGNMENT_V1, same(), false));
    }
    let bind = signature(
        BTreeMap::from([
            ("source".into(), source.clone()),
            (
                "base".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, true),
            ),
            ("history".into(), history.clone()),
            (
                "continuation".into(),
                port(
                    af_contracts::task::repair::TASK_REVIEW_CONTINUATION_V1,
                    same(),
                    true,
                ),
            ),
        ]),
        bind_outputs,
        BTreeMap::new(),
        BTreeMap::new(),
        None,
    );
    let mut inputs = BTreeMap::from([
        ("source".into(), source),
        ("history".into(), history),
        (
            "subject".into(),
            port(TASK_REVIEW_SUBJECT_V3, same(), false),
        ),
        ("checks".into(), port(TASK_CHECK_RECEIPT_V2, same(), true)),
    ]);
    for name in policy.reviewers.keys() {
        inputs.insert(name.clone(), port(RESULT_TYPE, same(), true));
    }
    let retained = inputs
        .keys()
        .filter(|name| *name != "source")
        .cloned()
        .collect();
    let mut reduce = signature(
        inputs,
        BTreeMap::from([
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
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([("result".into(), retained)]),
        Some("result".into()),
    );
    // An advisory reviewer may stay unbound: a Pipeline that binds only the required ones (the
    // stock review-code) and one that adds the advisory reviewer (review-light) plan under the
    // same policy. The reduce skips a reviewer no node binds; a bound one with no result is still
    // missing evidence.
    reduce.unbound_allowed = policy
        .reviewers
        .iter()
        .filter(|(_, requirement)| **requirement == DemandRequirement::Advisory)
        .map(|(name, _)| name.clone())
        .collect();
    let mut integrated_bind = bind.clone();
    integrated_bind.contract.inputs.remove("continuation");
    integrated_bind.contract.inputs.insert(
        "integration".into(),
        port(
            af_contracts::task::native_review::TASK_REVIEW_COMMIT_V1,
            same(),
            false,
        ),
    );
    let mut installed = BTreeMap::from([
        ("operator/review-bind".into(), bind),
        ("operator/review-bind-integrated".into(), integrated_bind),
        ("operator/review-reduce".into(), reduce),
        (
            "operator/review-accept".into(),
            implementation::signature(policy_id, policy.allow_targeted_repairs),
        ),
    ]);
    installed.extend(repair::signatures(policy_id, policy.allow_targeted_repairs));
    installed.extend(integration::signatures(policy));
    installed.insert("operator/review-slice".into(), scatter::slice_signature());
    installed.insert("operator/review-continue".into(), continuation::signature());
    installed.insert(
        "operator/review-decisions".into(),
        decisions::signature(policy_id),
    );
    Ok(installed)
}

#[derive(Default)]
struct ReviewMemo {
    rounds: BTreeMap<String, (Ledger, TaskReviewRoundV1)>,
    repairs: BTreeMap<String, af_contracts::task::repair::TaskRepairContextV1>,
}

pub struct ReviewTaskDomain {
    /// Serializes only synchronous domain reductions, never Worker execution.
    operation: std::sync::Mutex<()>,
    /// Bounded scratch reused only within the current locked operation.
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
        graph.native_integration_successors()?;
        let installed = review_signatures(policy_id, &policy)?;
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator: TaskOperatorV1::ReviewReduce {},
                signature,
            } = &node.operator
                && signature == "operator/review-decisions"
            {
                if node.contract != installed[signature].contract
                    || node.inputs.iter().any(|(name, address)| {
                        address.node != "root.inputs" || &address.port != name
                    })
                    || !graph.slots.is_empty()
                    || graph.nodes.len() != 2
                    || graph.nodes.values().any(|n| {
                        matches!(
                            n.operator,
                            CompiledOperator::Primitive {
                                operator: TaskOperatorV1::ReviewCommit {}
                                    | TaskOperatorV1::ReviewIntegrate { .. },
                                ..
                            }
                        )
                    })
                {
                    return Err("Human Review decisions require exact root inputs and a read-only Task without Workers or Integration".into());
                }
                continue;
            }
            if let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
                && matches!(
                    operator,
                    TaskOperatorV1::ReviewBind {}
                        | TaskOperatorV1::ReviewBindIntegrated {}
                        | TaskOperatorV1::ReviewIntegrate { .. }
                        | TaskOperatorV1::ReviewCommit {}
                        | TaskOperatorV1::ReviewIntegrationSource {}
                        | TaskOperatorV1::ReviewSlice { .. }
                        | TaskOperatorV1::ReviewReduce {}
                        | TaskOperatorV1::ReviewAccept {}
                        | TaskOperatorV1::AttestFixes {}
                        | TaskOperatorV1::RepairAccept {}
                        | TaskOperatorV1::ReviewContinue {}
                )
                && installed.get(signature).is_none_or(|s| match operator {
                    TaskOperatorV1::ReviewIntegrate { reviewers, .. } => {
                        af_graph::task::review_integration_signature(s, reviewers)
                            .map_or(true, |s| s.contract != node.contract)
                    }
                    _ => s.contract != node.contract,
                })
            {
                return Err("Compiled Review operator changed its installed contract".into());
            }
            if matches!(
                node.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::ReviewReduce {}
                        | TaskOperatorV1::ReviewReduceShards { .. },
                    ..
                }
            ) {
                let (sharded, closeout) = match &node.operator {
                    CompiledOperator::Primitive {
                        operator:
                            TaskOperatorV1::ReviewReduceShards {
                                reviewers,
                                closeout,
                            },
                        ..
                    } => {
                        let mut expected = installed["operator/review-reduce"].contract.clone();
                        for name in reviewers {
                            expected
                                .inputs
                                .get_mut(name)
                                .ok_or("Sharded reducer names an unknown reviewer")?
                                .artifact_type = af_contracts::contract::SHARD_SET_V1.into();
                        }
                        if expected != node.contract
                            || policy.reviewers.get(closeout) != Some(&DemandRequirement::Required)
                        {
                            return Err("Sharded reducer changed its installed contract or required whole-Subject closeout".into());
                        }
                        (reviewers.clone(), Some(closeout))
                    }
                    _ => (BTreeSet::new(), None),
                };
                let maximum_stages = node
                    .inputs
                    .keys()
                    .filter(|name| policy.reviewers.contains_key(*name))
                    .try_fold(0usize, |count, name| {
                        let stages = if sharded.contains(name) {
                            let address = &node.inputs[name];
                            graph
                                .owned_children
                                .get(&address.node)
                                .ok_or("Sharded reviewer lacks its captured owned template")?
                                .max_children as usize
                        } else {
                            1
                        };
                        count
                            .checked_add(stages)
                            .ok_or("Review stage bound overflow")
                    })?;
                if maximum_stages > 64 {
                    return Err(
                        "Complete native Review exceeds the shared reducer's 64-stage bound".into(),
                    );
                }
                for (name, requirement) in &policy.reviewers {
                    let Some(address) = node.inputs.get(name) else {
                        if *requirement == DemandRequirement::Advisory {
                            // An advisory reviewer this Pipeline never bound (`unbound_allowed`).
                            continue;
                        }
                        return Err("Review reducer omitted a required reviewer binding".into());
                    };
                    let reviewer = graph
                        .nodes
                        .get(&address.node)
                        .ok_or("Review binding has no Worker node")?;
                    let correct_role = if sharded.contains(name) {
                        matches!(
                            reviewer.operator,
                            CompiledOperator::Primitive {
                                operator: TaskOperatorV1::ReviewScatter { .. },
                                ..
                            }
                        )
                    } else {
                        matches!(
                            reviewer.operator,
                            CompiledOperator::Primitive {
                                operator: TaskOperatorV1::Verify { .. },
                                ..
                            }
                        )
                    };
                    if !correct_role
                        || ["source", "subject", "history", "checks"]
                            .iter()
                            .any(|port| reviewer.inputs.get(*port) != node.inputs.get(*port))
                    {
                        return Err("Required reviewer must use a reserved verification role and the reducer's exact declared inputs".into());
                    }
                    let subject = node
                        .inputs
                        .get("subject")
                        .ok_or("Review has no Subject binding")?;
                    let assignment = reviewer
                        .inputs
                        .get("assignment")
                        .ok_or("Reviewer lacks its explicit assignment")?;
                    if assignment.node != subject.node
                        || assignment.port != *name
                        || reviewer
                            .contract
                            .inputs
                            .get("assignment")
                            .is_none_or(|p| p.artifact_type != TASK_REVIEW_ASSIGNMENT_V1)
                        || graph.nodes.get(&subject.node).is_none_or(|n| {
                            !matches!(
                                n.operator,
                                CompiledOperator::Primitive {
                                    operator: TaskOperatorV1::ReviewBind {}
                                        | TaskOperatorV1::ReviewBindIntegrated {},
                                    ..
                                }
                            )
                        })
                        || reviewer
                            .contract
                            .outputs
                            .get(&address.port)
                            .is_none_or(|p| {
                                p.artifact_type
                                    != if sharded.contains(name) {
                                        af_contracts::contract::SHARD_SET_V1
                                    } else {
                                        RESULT_TYPE
                                    }
                            })
                    {
                        return Err("Reviewer must retain its own captured assignment and ReviewerResult@2 contract".into());
                    }
                    if closeout == Some(name)
                        && (reviewer
                            .contract
                            .inputs
                            .values()
                            .any(|p| p.artifact_type == af_contracts::contract::REVIEW_SLICE_V1)
                            || sharded.iter().any(|shard| {
                                node.inputs.get(shard).is_none_or(|address| {
                                    !reviewer.inputs.values().any(|bound| bound == address)
                                })
                            }))
                    {
                        return Err("Whole-Subject closeout must consume every complete Shard Set before it runs".into());
                    }
                }
            }
        }
        for node in graph.nodes.values() {
            if matches!(
                node.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::RepairAccept {} | TaskOperatorV1::ReviewContinue {},
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
            operation: std::sync::Mutex::new(()),
            memo: std::sync::Mutex::new(ReviewMemo::default()),
            review_task: true,
            policy_id: policy_id.into(),
            policy,
            graph,
            code,
        })
    }
    /// Machine-local selection supplied explicitly by the coordinator before check dispatch.
    pub fn with_rust_toolchain_mapping(mut self, mapping: Option<std::path::PathBuf>) -> Self {
        self.code = self.code.with_rust_toolchain_mapping(mapping);
        self
    }

    /// Keep the warm build directories of the checks this domain runs below `root`, when the
    /// machine moved them from `$XDG_CACHE_HOME/af/task-build-cache`.
    pub fn with_task_build_cache_root(mut self, root: Option<std::path::PathBuf>) -> Self {
        if let Some(root) = root {
            self.code = self.code.with_task_build_cache_root(root);
        }
        self
    }

    /// Machine-local Remote Check configuration (ADR-0140) for the checks this domain runs.
    pub fn with_remote_checks(mut self, remote: super::remote_check::RemoteCheckHost) -> Self {
        self.code = self.code.with_remote_checks(remote);
        self
    }

    /// Chosen by the captured Task-kind profile, never by a Worker response or display name.
    pub fn with_review_task(mut self, review: bool) -> Self {
        self.review_task = review;
        self
    }

    /// The checks this domain runs are the code domain's, so a `[warm] caches` kind resolves
    /// through the same machine-local policy.
    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(af_sandbox::CacheKind) -> Result<af_sandbox::CacheSource, af_sandbox::CacheError>
            + Send
            + Sync
            + 'static,
    {
        self.code = self.code.with_cache_source_resolver(resolver);
        self
    }

    // Every external domain boundary starts fresh. Keeping the operation guard alive
    // prevents another concurrent callback from sharing or clearing this operation's memo.
    fn begin_operation(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        let guard = self
            .operation
            .lock()
            .map_err(|_| "Review operation poisoned")?;
        *self.memo.lock().map_err(|_| "Review memo poisoned")? = ReviewMemo::default();
        Ok(guard)
    }

    fn operator(&self, input: &TaskInvocationV1) -> Result<&TaskOperatorV1, String> {
        let operator = if let Some(node) = self.graph.nodes.get(&input.node) {
            &node.operator
        } else {
            &self.dynamic_template(input)?.1.operator
        };
        match operator {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Not a domain operator".into()),
        }
    }
    fn reviewer(&self, input: &TaskInvocationV1) -> bool {
        let contract = self
            .graph
            .nodes
            .get(&input.node)
            .map(|n| &n.contract)
            .or_else(|| self.dynamic_template(input).ok().map(|(_, t)| &t.contract));
        contract.is_some_and(|contract| {
            contract
                .outputs
                .values()
                .any(|p| p.artifact_type == RESULT_TYPE)
        })
    }

    fn dynamic_template(
        &self,
        input: &TaskInvocationV1,
    ) -> Result<
        (
            &af_graph::task::CompiledNode,
            &af_graph::task::OwnedChildTemplateV1,
        ),
        String,
    > {
        let (parent, index) = input
            .node
            .rsplit_once(".slice")
            .ok_or("Unknown native Review node")?;
        let template = self
            .graph
            .owned_children
            .get(parent)
            .ok_or("Unknown native Scatter owner")?;
        let index: u32 = index.parse().map_err(|_| "Invalid native shard ordinal")?;
        if index == 0
            || index > template.max_children
            || input.node != format!("{parent}.slice{index}")
        {
            return Err("Native shard is outside its exact captured fanout".into());
        }
        Ok((&self.graph.nodes[parent], template))
    }

    fn input_addresses(
        &self,
        input: &TaskInvocationV1,
    ) -> Result<&BTreeMap<String, af_graph::task::Address>, String> {
        if let Some(node) = self.graph.nodes.get(&input.node) {
            Ok(&node.inputs)
        } else {
            Ok(&self.dynamic_template(input)?.0.inputs)
        }
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

    fn bind_subject(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ResolvedReviewSubject, String> {
        let snapshot = source_input(
            cas,
            input.inputs.get("source").ok_or("Review has no source")?,
        )?;
        let (_, head) = read_snapshot(cas, &snapshot)?;
        let subject = if let Some(base) = input.inputs.get("base") {
            let base_id = source_input(cas, base)?;
            let (_, base) = read_snapshot(cas, &base_id)?;
            let diff =
                af_source_git::git::manifest_diff(&base, &head, cas).map_err(|e| e.to_string())?;
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
        let bound = ResolvedReviewSubject {
            subject_id: subject_id.clone(),
            subject,
            change_set,
            snapshot_id: snapshot.clone(),
            prior_history_id: history_id,
            continuation_id: input
                .inputs
                .get("continuation")
                .or_else(|| input.inputs.get("integration"))
                .map(|p| p.artifact_ids[0].clone()),
            round,
        };
        bound.validate()?;
        if let Some(id) = &bound.continuation_id {
            self.validate_continuation_binding(cas, id, &bound, input)?;
        }
        Ok(bound)
    }

    fn current_subject(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ResolvedReviewSubject, String> {
        let subject = self.read_subject(cas, input)?;
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
        let resolved =
            af_store::resolve_subject_scope(cas, &subject.subject_id).map_err(|e| e.to_string())?;
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
        if let Some(id) = &subject.continuation_id {
            self.validate_continuation_binding(cas, id, &subject, input)?;
        }
        Ok(subject)
    }

    fn prior_ledger(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        subject: &ResolvedReviewSubject,
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
                if let Some(id) = &subject.continuation_id
                    && envelope(cas, id)?.artifact_type
                        == af_contracts::task::native_review::TASK_REVIEW_COMMIT_V1
                {
                    let commit = self.restore_integration_commit(cas, id, subject, input)?;
                    ledger
                        .project_task_integration(id, &commit.pending_claims)
                        .map_err(|e| e.to_string())?;
                }
                ledger
                    .bind_task_subject(cas, &subject.subject_id, prior.round)
                    .map_err(|e| e.to_string())?;
                if let Some(id) = &subject.continuation_id {
                    if envelope(cas, id)?.artifact_type
                        == af_contracts::task::native_review::TASK_REVIEW_COMMIT_V1
                    {
                        ledger
                            .bind_task_subject(cas, &subject.subject_id, subject.round)
                            .map_err(|e| e.to_string())?;
                        return Ok(ledger);
                    }
                    let continuation = self.restore_continuation(cas, id, subject, input)?;
                    // A failed or unavailable current check cannot promote any positive claim.
                    if self.code.review_checks(cas, &continuation.invocation)?
                        == ReceiptOutcomeV1::Passed
                    {
                        ledger
                            .project_task_fixes(cas, &continuation.assessment)
                            .map_err(|e| e.to_string())?;
                    }
                }
                ledger
                    .bind_task_subject(cas, &subject.subject_id, subject.round)
                    .map_err(|e| e.to_string())?;
                Ok(ledger)
            }
            _ => Err("Review Round has no admitted preceding history".into()),
        }
    }

    /// Rebuild a complete native Round for read-only observers. CAS presence alone is not
    /// evidence: the captured plan, policy, assignments and entire reduction are rechecked.
    pub fn recorded_ledger(cas: &Cas, id: &str) -> Result<Ledger, String> {
        let view = cas.verifying_existing();
        let cas = &view;
        let record = envelope(cas, id)?;
        if record.artifact_type != TASK_REVIEW_ROUND_V1 {
            return Err("Expected a native Task Review Round".into());
        }
        let receipt: TaskReviewRoundV1 =
            serde_json::from_value(record.payload).map_err(|e| e.to_string())?;
        receipt.validate()?;
        let plan_record = envelope(cas, &receipt.invocation.plan_id)?;
        if plan_record.artifact_type != af_contracts::task::EXECUTION_PLAN_V4 {
            return Err("Native Round has no current plan".into());
        }
        let plan: ExecutionPlan =
            serde_json::from_value(plan_record.payload).map_err(|e| e.to_string())?;
        plan.validate()?;
        let graph_record = envelope(cas, &plan.compiled_graph_id)?;
        if graph_record.artifact_type != af_pipeline_config::task::catalog::COMPILED_TASK_V4 {
            return Err("Native Round has no current graph".into());
        }
        let graph: CompiledTask =
            serde_json::from_value(graph_record.payload).map_err(|e| e.to_string())?;
        Self::captured(cas, &receipt.policy_id, graph)?
            .restore_round(cas, id, 0)
            .map(|(ledger, _)| ledger)
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
        // Prior Rounds retain their own captured graph and requirements. A new authorized
        // Task has another graph even when it uses the same Pipeline and Review policy.
        // Recompute under that exact original graph, preserving the shared depth bound.
        let original_plan = envelope(cas, &receipt.invocation.plan_id)?;
        if original_plan.artifact_type != af_contracts::task::EXECUTION_PLAN_V4 {
            return Err("Review history lost its current captured plan".into());
        }
        let original_plan: ExecutionPlan =
            serde_json::from_value(original_plan.payload).map_err(|e| e.to_string())?;
        original_plan.validate()?;
        let original_graph = envelope(cas, &original_plan.compiled_graph_id)?;
        if original_graph.artifact_type != af_pipeline_config::task::catalog::COMPILED_TASK_V4 {
            return Err("Review history lost its current captured graph".into());
        }
        if original_graph.payload != serde_json::to_value(&self.graph).map_err(|e| e.to_string())? {
            let graph: CompiledTask =
                serde_json::from_value(original_graph.payload).map_err(|e| e.to_string())?;
            return Self::captured(cas, &self.policy_id, graph)?.restore_round(cas, id, depth);
        }
        if artifact.artifact_type != TASK_REVIEW_ROUND_V1
            || receipt.policy_id != self.policy_id
            || artifact.producer != invocation_producer(cas, &receipt.invocation, None)?
            || artifact.subject_snapshot_id.as_ref() != Some(&receipt.snapshot_id)
            || !matches!(
                self.operator(&receipt.invocation)?,
                TaskOperatorV1::ReviewReduce {} | TaskOperatorV1::ReviewReduceShards { .. }
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
        if self.is_decision(input) {
            return self
                .reduce_decisions(cas, input, depth)
                .map(|(receipt, ledger)| (receipt, Some(ledger)));
        }
        let subject = self.current_subject(cas, input)?;
        let ledger = self.prior_ledger(cas, input, &subject, depth)?;
        let mut results = Vec::new();
        let mut selected = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for (name, requirement) in &self.policy.reviewers {
            match reviewer_presence(
                *requirement,
                self.graph.nodes[&input.node].inputs.contains_key(name),
                input.inputs.contains_key(name),
            ) {
                ReviewerPresence::NotInThisReview => continue,
                ReviewerPresence::Missing => {
                    missing.insert(name.clone());
                    continue;
                }
                ReviewerPresence::Answered => {}
            }
            let address = &self.graph.nodes[&input.node].inputs[name];
            let sharded = matches!(self.operator(input)?, TaskOperatorV1::ReviewReduceShards {reviewers, ..} if reviewers.contains(name));
            if sharded {
                let (_, _, shards) = self.checked_shards(cas, input, name)?;
                if !shards.complete() {
                    missing.insert(name.clone());
                }
                for (index, shard) in shards.shards.iter().enumerate() {
                    if let af_contracts::ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } = &shard.outcome
                    {
                        let [id] = result_artifact_ids.as_slice() else {
                            return Err(
                                "Native shard must retain exactly one Reviewer Result".into()
                            );
                        };
                        let item = self.selected_stage(
                            cas,
                            input,
                            &subject,
                            &ledger,
                            name,
                            id,
                            &format!("{}.slice{}", address.node, index + 1),
                        )?;
                        let key = format!("{name}_slice{}", index + 1);
                        if self.policy.reviewers.contains_key(&key)
                            || selected.insert(key, id.clone()).is_some()
                        {
                            return Err(
                                "Native shard result identity collides with a configured reviewer"
                                    .into(),
                            );
                        }
                        results.push(item);
                    }
                }
            } else {
                let id = input.inputs[name].artifact_ids[0].clone();
                results.push(self.selected_stage(
                    cas,
                    input,
                    &subject,
                    &ledger,
                    name,
                    &id,
                    &address.node,
                )?);
                selected.insert(name.clone(), id);
            }
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
            })
            .collect();
        let reduction = af_store::prepare_canonical_task_review_with_shards(
            cas,
            &run_id,
            &ledger,
            &stages,
            &self.shard_membership(cas, input)?,
        )
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
        let findings = af_contracts::FindingSetV1 {
            subject_id: subject.subject_id.clone(),
            round: subject.round,
            prior_finding_set_id: prior_findings,
            reducer_version: af_contracts::FINDING_REDUCER_VERSION_V2.into(),
            identity_policy: af_contracts::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: reduction.reduction.selected_report_ids,
            relation_ids: reduction.reduction.relation_ids,
            resolution_ids: reduction.ledger.resolution_artifact_ids(),
            findings: crate::finding_set_entries(&reduction.ledger),
        };
        findings.validate()?;
        let demands = af_contracts::DemandSetV1 {
            subject_id: subject.subject_id.clone(),
            round: subject.round,
            prior_demand_set_id: prior_demands,
            reducer_version: af_contracts::DEMAND_REDUCER_VERSION.into(),
            selected_demand_artifact_ids: reduction.reduction.selected_demand_artifact_ids,
            satisfaction_artifact_ids: reduction.ledger.demand_reduction_artifact_ids().1,
            waiver_artifact_ids: reduction.ledger.demand_reduction_artifact_ids().2,
            demands: reduction.ledger.demand_views(),
        };
        demands.validate()?;
        receipt.finding_set_id = Some(
            self.put(
                cas,
                input,
                af_contracts::contract::FINDING_SET_V1,
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
                af_contracts::contract::DEMAND_SET_V1,
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
            && convergence.verdict == af_store::Verdict::Converged
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
        let mut refs: Vec<String> = receipt
            .finding_set_id
            .iter()
            .chain(receipt.demand_set_id.iter())
            .cloned()
            .collect();
        refs.extend(self.closure_refs(cas, input, ledger.is_some())?);
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
                    af_contracts::task::repair::TaskReviewClaimV1 {
                        view_id,
                        file: finding.file,
                        line: finding.line,
                        title: finding.title,
                        body: finding.body,
                        remedy: finding.fix,
                    },
                );
            }
        }
        let claim_context = af_contracts::task::repair::TaskReviewClaimsV1 {
            round_report_id: result.artifact_ids[0].clone(),
            snapshot_id: receipt.snapshot_id.clone(),
            claims,
        };
        claim_context.validate()?;
        let findings = self.put(
            cas,
            input,
            af_contracts::task::repair::TASK_REVIEW_CLAIMS_V1,
            Some(&receipt.snapshot_id),
            &claim_context,
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
        let _operation = self.begin_operation()?;
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
            execution: if execution.budget.breached()
                || report
                    .outcomes
                    .iter()
                    .any(|(_, o)| matches!(o, NodeOutcome::Failed { .. }))
            {
                TaskExecutionV1::Exhausted
            } else {
                TaskExecutionV1::Completed
            },
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "incomplete".into(),
            outputs,
            evidence,
            missing_obligations: BTreeSet::new(),
        };
        self.assess(cas, &state.revision, &mut result)?;
        af_store::store::task::review_integration::require_native_successors(cas, state, &result)
            .map_err(|e| e.to_string())?;
        if self.review_task
            && let Some(id) = af_store::store::task::review_integration::native_review_conclusion(
                cas, state, &result,
            )
            .map_err(|e| e.to_string())?
        {
            result.evidence.insert(id);
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
            let origins = self.graph.evidence_origins(address)?;
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
                if !origins.iter().any(|origin| &origin.node == node)
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
                let plan: ExecutionPlan =
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
                        TaskOperatorV1::ReviewReduce {} | TaskOperatorV1::ReviewReduceShards { .. }
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
        let complete = missing.is_empty()
            && !conclusions.is_empty()
            && result.execution == TaskExecutionV1::Completed;
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
    fn prepare_owned_children(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<super::TaskOwnedChildrenInputs, String> {
        let _operation = self.begin_operation()?;
        self.prepare_native_children(cas, input)
    }
    fn complete_owned_children(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
        facts: &[af_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let _operation = self.begin_operation()?;
        self.native_shards(cas, input, children, facts)
    }
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &af_graph::task::CompiledNode,
        attempt: &af_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.code.prepare_context(cas, input, definition, attempt)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &af_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }

        if !matches!(
            self.operator(input),
            Ok(TaskOperatorV1::ReviewBind {}
                | TaskOperatorV1::ReviewBindIntegrated {}
                | TaskOperatorV1::ReviewIntegrate { .. }
                | TaskOperatorV1::ReviewCommit {}
                | TaskOperatorV1::ReviewIntegrationSource {}
                | TaskOperatorV1::ReviewSlice { .. }
                | TaskOperatorV1::ReviewReduce {}
                | TaskOperatorV1::ReviewReduceShards { .. }
                | TaskOperatorV1::ReviewAccept {}
                | TaskOperatorV1::AttestFixes {}
                | TaskOperatorV1::RepairAccept {}
                | TaskOperatorV1::ReviewContinue {})
        ) {
            return self
                .code
                .execute(cas, input, definition, attempt, cancellation);
        }
        let _operation = match self.begin_operation() {
            Ok(guard) => guard,
            Err(error) => return super::control::refused(error),
        };
        let outputs = match self.operator(input) {
            Ok(TaskOperatorV1::ReviewBind {} | TaskOperatorV1::ReviewBindIntegrated {}) => {
                self.bind_outputs(cas, input)
            }
            Ok(TaskOperatorV1::ReviewIntegrate { .. }) => self.integrate_outputs(cas, input),
            Ok(TaskOperatorV1::ReviewCommit {}) => self.commit_integration_outputs(cas, input),
            Ok(TaskOperatorV1::ReviewIntegrationSource {}) => {
                self.integration_source_outputs(cas, input)
            }
            Ok(TaskOperatorV1::ReviewSlice { .. }) => self.slice_outputs(cas, input),
            Ok(TaskOperatorV1::ReviewReduce {} | TaskOperatorV1::ReviewReduceShards { .. }) => {
                self.reduce_outputs(cas, input)
            }
            Ok(TaskOperatorV1::ReviewAccept {}) => self.accept_implementation(cas, input),
            Ok(TaskOperatorV1::AttestFixes {}) => self.attest_fixes(cas, input),
            Ok(TaskOperatorV1::RepairAccept {}) => self.accept_repair(cas, input),
            Ok(TaskOperatorV1::ReviewContinue {}) => self.continue_review(cas, input),
            _ => {
                return self
                    .code
                    .execute(cas, input, definition, attempt, cancellation);
            }
        };
        TaskWorkOutput {
            notes: Default::default(),
            usage_observation: None,
            usage: None,
            outputs,
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
            unknown_usage_cause: None,
            refused_input: false,
        }
    }
}
impl TaskDomain for ReviewTaskDomain {
    fn validate_owned_children(
        &self,
        cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlan,
        parent: &TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        let _operation = self.begin_operation()?;
        self.check_native_children(cas, parent, children)
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        _task: &TaskRevisionV1,
        _plan: &ExecutionPlan,
        parent: &TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
        facts: &[af_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let _operation = self.begin_operation()?;
        if output.outputs != self.native_shards(cas, parent, children, facts)? {
            return Err("Native Shard Set differs from every durable child outcome".into());
        }
        Ok(())
    }
    fn admit_checks(
        &self,
        cas: &Cas,
        task: &af_contracts::task::TaskRevisionV1,
    ) -> Result<(), String> {
        self.code.admit_toolchains(cas, task)
    }
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
        attempt: &af_store::store::task::execution::ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        let _operation = self.begin_operation()?;
        if matches!(self.operator(input), Ok(TaskOperatorV1::FixVerify { .. })) {
            return self.validate_fix_context(cas, input);
        }
        if self.reviewer(input) {
            let subject = self.current_subject(cas, input)?;
            self.current_assignment(cas, input, &subject)?;
            let (_, mut manifest) = read_snapshot(cas, &subject.snapshot_id)?;
            super::source::add_review_inputs(
                cas,
                input,
                false,
                &subject.snapshot_id,
                &mut manifest,
            )?;
            if self.code.review_checks(cas, input)? != ReceiptOutcomeV1::Passed {
                return Err("Reviewer cannot dispatch before current checks pass".into());
            }
            return Ok(());
        }
        self.code.validate_context(cas, input, attempt, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlan,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        definition: &af_graph::task::CompiledNode,
    ) -> Result<(), String> {
        let _operation = self.begin_operation()?;
        if matches!(self.operator(input)?, TaskOperatorV1::ReviewScatter { .. }) {
            // Only the protected owned-completion operation can publish this coordinator;
            // validate_owned_completion recomputes all registered child facts below.
            return Ok(());
        }
        let expected = match self.operator(input)? {
            TaskOperatorV1::ReviewBind {} | TaskOperatorV1::ReviewBindIntegrated {} => {
                Some(self.bind_outputs(cas, input)?)
            }
            TaskOperatorV1::ReviewIntegrate { .. } => Some(self.integrate_outputs(cas, input)?),
            TaskOperatorV1::ReviewCommit {} => Some(self.commit_integration_outputs(cas, input)?),
            TaskOperatorV1::ReviewIntegrationSource {} => {
                Some(self.integration_source_outputs(cas, input)?)
            }
            TaskOperatorV1::ReviewSlice { .. } => Some(self.slice_outputs(cas, input)?),
            TaskOperatorV1::ReviewReduce {} | TaskOperatorV1::ReviewReduceShards { .. } => {
                Some(self.reduce_outputs(cas, input)?)
            }
            TaskOperatorV1::ReviewAccept {} => Some(self.accept_implementation(cas, input)?),
            TaskOperatorV1::AttestFixes {} => Some(self.attest_fixes(cas, input)?),
            TaskOperatorV1::RepairAccept {} => Some(self.accept_repair(cas, input)?),
            TaskOperatorV1::ReviewContinue {} => Some(self.continue_review(cas, input)?),
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
                    if artifact.artifact_type == af_source_git::task::CANDIDATE_TREE_V1 {
                        let candidate: af_source_git::task::CandidateTree =
                            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                        if candidate.parent_snapshot_id != subject.snapshot_id {
                            return Err("Proposal candidate changed its reviewed Snapshot".into());
                        }
                        af_source_git::task::read_manifest(cas, &candidate.manifest_id)?;
                        continue;
                    }
                    if artifact.artifact_type
                        == af_contracts::task::native_review::TASK_REVIEW_PROPOSAL_V1
                    {
                        let proposal: af_contracts::task::native_review::TaskReviewProposalV1 =
                            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                        proposal.validate()?;
                        continue;
                    }
                    if artifact.artifact_type
                        == af_contracts::task::verification::IMPLEMENTATION_TEST_EVIDENCE_V1
                    {
                        let tests: af_contracts::task::verification::ImplementationTestEvidenceV1 =
                            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                        tests.validate()?;
                        if artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
                            || tests.snapshot_id != subject.snapshot_id
                        {
                            return Err("Executable review evidence has a stale Snapshot".into());
                        }
                        continue;
                    }
                    if artifact.artifact_type != RESULT_TYPE
                        || artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
                    {
                        return Err("Reviewer output has a stale type or Snapshot".into());
                    }
                    let assignment = self.current_assignment(cas, input, &subject)?;
                    let stage = crate::reviewer_stage_output(artifact.payload)?;
                    self.validate_stage(&stage, &assignment)?;
                }
            }
            return Ok(());
        }
        self.code
            .validate_output(cas, task, plan, input, output, definition)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let _operation = self.begin_operation()?;
        let mut expected = result.clone();
        self.assess(cas, task, &mut expected)?;
        if &expected != result {
            return Err("Review conclusion contradicts its completeness evidence".into());
        }
        Ok(())
    }
}

/// Where a policy reviewer stands in one reduce invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewerPresence {
    /// Its result arrived.
    Answered,
    /// The Round lacks it: its result never arrived, or it is required and the Pipeline never
    /// bound it.
    Missing,
    /// An advisory reviewer the Pipeline never bound (`unbound_allowed`): not part of this
    /// review, so never missing.
    NotInThisReview,
}

/// Where reviewer `requirement` stands when the reducer `declared` its input and its result
/// `arrived`: only an advisory reviewer may be absent without being missing, and only when the
/// Pipeline never bound it; a bound one whose result never arrived is missing.
fn reviewer_presence(
    requirement: DemandRequirement,
    declared: bool,
    arrived: bool,
) -> ReviewerPresence {
    if arrived {
        ReviewerPresence::Answered
    } else if !declared && requirement == DemandRequirement::Advisory {
        ReviewerPresence::NotInThisReview
    } else {
        ReviewerPresence::Missing
    }
}

#[cfg(test)]
mod advisory_reviewer_tests {
    use super::*;

    /// The reduce's runtime reduction: an unbound advisory `uix` is left out of `missing`, a
    /// bound advisory `uix` whose result never arrived stays in it, and so does every required
    /// reviewer without a result, bound or not.
    #[test]
    fn the_reduce_leaves_only_an_unbound_advisory_reviewer_out_of_missing() {
        use DemandRequirement::{Advisory, Required};
        fn missing(
            reviewers: &[(&'static str, DemandRequirement, bool, bool)],
        ) -> Vec<&'static str> {
            reviewers
                .iter()
                .filter(|(_, requirement, declared, arrived)| {
                    reviewer_presence(*requirement, *declared, *arrived)
                        == ReviewerPresence::Missing
                })
                .map(|(name, ..)| *name)
                .collect()
        }
        assert_eq!(
            missing(&[
                ("bugs", Required, true, true),
                ("correctness", Required, true, true),
                ("uix", Advisory, false, false),
            ]),
            Vec::<&str>::new()
        );
        assert_eq!(
            missing(&[
                ("bugs", Required, true, true),
                ("correctness", Required, true, true),
                ("uix", Advisory, true, false),
            ]),
            ["uix"]
        );
        assert_eq!(
            missing(&[
                ("bugs", Required, false, false),
                ("correctness", Required, true, false),
                ("uix", Advisory, true, true),
            ]),
            ["bugs", "correctness"]
        );
        assert_eq!(
            reviewer_presence(Advisory, false, false),
            ReviewerPresence::NotInThisReview
        );
    }

    fn policy_with(reviewers: &[(&str, DemandRequirement)]) -> ReviewTaskPolicy {
        ReviewTaskPolicy {
            schema: REVIEW_TASK_POLICY_SCHEMA.into(),
            check_policy_id: format!("sha256:{}", "a".repeat(64)),
            reviewers: reviewers
                .iter()
                .map(|(name, requirement)| ((*name).to_string(), *requirement))
                .collect(),
            gate: Severity::Major,
            clean_rounds: 1,
            max_rounds: 3,
            allow_targeted_repairs: false,
        }
    }

    #[test]
    fn the_reduce_lets_only_advisory_reviewers_stay_unbound() {
        let policy = policy_with(&[
            ("bugs", DemandRequirement::Required),
            ("correctness", DemandRequirement::Required),
            ("uix", DemandRequirement::Advisory),
        ]);
        let installed = review_signatures(&format!("sha256:{}", "b".repeat(64)), &policy).unwrap();
        let reduce = &installed["operator/review-reduce"];
        assert_eq!(reduce.unbound_allowed, BTreeSet::from(["uix".to_string()]));
        for name in ["bugs", "correctness", "uix"] {
            assert!(
                reduce.contract.inputs[name].optional,
                "{name} stays optional at runtime"
            );
        }
        let bind = &installed["operator/review-bind"];
        assert!(
            bind.unbound_allowed.is_empty(),
            "the bind still assigns every reviewer"
        );
        let required_only = policy_with(&[("bugs", DemandRequirement::Required)]);
        let installed =
            review_signatures(&format!("sha256:{}", "b".repeat(64)), &required_only).unwrap();
        assert!(
            installed["operator/review-reduce"]
                .unbound_allowed
                .is_empty()
        );
    }

    /// The light default pipeline (`default-light/review`, ADR-0262): its review graph binds only
    /// `correctness`. The repository's catalog marks `bugs` advisory, so the planner may leave it
    /// unbound (`unbound_allowed`) and the captured domain admits the graph; under a policy that
    /// marks `bugs` required the planner must bind it and the captured domain refuses the same
    /// graph.
    #[test]
    fn an_advisory_bugs_reviewer_admits_a_correctness_only_review_graph() {
        use af_graph::task::{Address, COMPILED_TASK_SCHEMA_V6, CompiledNode};
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let code: super::super::code::CodeTaskPolicy = serde_json::from_value(serde_json::json!({
            "schema": "af.code-task-policy/1",
            "checks": {"kernel": {"name": "kernel", "required": true,
                "command": {"program": "true", "args": []}}},
            "check_wall_ms": 60_000,
            "require_container": false
        }))
        .unwrap();
        let code_id = cas.put_json(&serde_json::to_value(&code).unwrap()).unwrap();
        let code_installed = super::super::code::code_signatures(&code_id, &code).unwrap();
        let policy = |bugs| {
            let mut policy = policy_with(&[
                ("bugs", bugs),
                ("correctness", DemandRequirement::Required),
                ("uix", DemandRequirement::Advisory),
            ]);
            policy.check_policy_id = code_id.clone();
            let id = cas
                .put_json(&serde_json::to_value(&policy).unwrap())
                .unwrap();
            (id, policy)
        };
        let (light_id, light) = policy(DemandRequirement::Advisory);
        let (strict_id, strict) = policy(DemandRequirement::Required);
        let installed = review_signatures(&light_id, &light).unwrap();
        assert_eq!(
            installed["operator/review-reduce"].unbound_allowed,
            BTreeSet::from(["bugs".to_string(), "uix".to_string()])
        );
        let strict_installed = review_signatures(&strict_id, &strict).unwrap();
        assert_eq!(
            strict_installed["operator/review-reduce"].unbound_allowed,
            BTreeSet::from(["uix".to_string()])
        );

        let at = |node: &str, port: &str| Address {
            node: node.into(),
            port: port.into(),
        };
        let primitive = |operator, signature: &str, contract, inputs| CompiledNode {
            operator: CompiledOperator::Primitive {
                operator,
                signature: signature.into(),
            },
            contract,
            inputs,
            conditions: Vec::new(),
        };
        let shared = BTreeMap::from([
            ("source".to_string(), at("root", "source")),
            ("history".to_string(), at("root", "history")),
            ("subject".to_string(), at("bind", "subject")),
            ("checks".to_string(), at("checks", "result")),
        ]);
        let mut reviewer_inputs = shared.clone();
        reviewer_inputs.insert("assignment".into(), at("bind", "correctness"));
        let mut reduce_inputs = shared;
        reduce_inputs.insert("correctness".into(), at("correctness", "result"));
        let reviewer_contract = PipelineContractV1 {
            inputs: BTreeMap::from([(
                "assignment".to_string(),
                port(TASK_REVIEW_ASSIGNMENT_V1, same(), false),
            )]),
            outputs: BTreeMap::from([("result".to_string(), port(RESULT_TYPE, same(), false))]),
        };
        let graph = CompiledTask {
            schema: COMPILED_TASK_SCHEMA_V6.into(),
            nodes: BTreeMap::from([
                (
                    "bind".to_string(),
                    primitive(
                        TaskOperatorV1::ReviewBind {},
                        "operator/review-bind",
                        installed["operator/review-bind"].contract.clone(),
                        BTreeMap::from([
                            ("source".to_string(), at("root", "source")),
                            ("history".to_string(), at("root", "history")),
                        ]),
                    ),
                ),
                (
                    "checks".to_string(),
                    primitive(
                        TaskOperatorV1::Check {
                            checks: BTreeSet::from(["kernel".to_string()]),
                            remote_checks: BTreeSet::new(),
                        },
                        "operator/check",
                        code_installed["operator/check"].contract.clone(),
                        BTreeMap::from([("source".to_string(), at("root", "source"))]),
                    ),
                ),
                (
                    "correctness".to_string(),
                    primitive(
                        TaskOperatorV1::Verify {
                            slot: "correctness".into(),
                        },
                        "worker/default-light/correctness",
                        reviewer_contract,
                        reviewer_inputs,
                    ),
                ),
                (
                    "reduce".to_string(),
                    primitive(
                        TaskOperatorV1::ReviewReduce {},
                        "operator/review-reduce",
                        installed["operator/review-reduce"].contract.clone(),
                        reduce_inputs,
                    ),
                ),
            ]),
            order: ["bind", "checks", "correctness", "reduce"]
                .map(String::from)
                .to_vec(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([("review".to_string(), at("reduce", "result"))]),
            coverage: BTreeMap::from([("reviewed".to_string(), at("reduce", "result"))]),
            calls: BTreeMap::new(),
            slots: BTreeMap::new(),
            replaced_workers: BTreeMap::new(),
            max_parallel: 3,
            allowances: BTreeMap::new(),
            owned_children: BTreeMap::new(),
            experimental_slots: BTreeMap::new(),
            token_scopes: BTreeMap::new(),
        };
        assert_eq!(
            installed["operator/review-reduce"].contract,
            strict_installed["operator/review-reduce"].contract,
            "both policies install the same reduce contract, so only the requirement differs"
        );

        assert!(ReviewTaskDomain::captured(&cas, &light_id, graph.clone()).is_ok());
        assert_eq!(
            ReviewTaskDomain::captured(&cas, &strict_id, graph)
                .err()
                .as_deref(),
            Some("Review reducer omitted a required reviewer binding")
        );
    }
}
