//! Installed Review compatibility compilation. Persisted mappings describe exact captured
//! authority; deserializing a mapping never authorizes a Task or replaces recompilation.

pub mod artifact;
pub mod owned;
pub mod resources;

use std::collections::{BTreeMap, BTreeSet};

use review_attempt::task_budget::NodeAllowance;
use review_core::task::pipeline::{
    PipelineContractV1, PipelinePortV1, PortAffinityV1, ReceiptOutcomeV1, WorkerSlotV1,
};
use review_core::task::review_compat::TASK_REVIEW_RESULT_METADATA_V1;
pub use review_core::task::review_compat::{
    LEGACY_REVIEW_GATE_OUTCOME_V1 as REVIEW_GATE_OUTCOME_V1,
    LEGACY_REVIEW_ROUND_V1 as REVIEW_ROUND_V1,
};
use review_core::task::{ArtifactInputV1, TaskLimitsV1};
use review_core::{PortCardinality, SnapshotAffinity, contract};
use review_graph::task::{
    Address, CompiledCall, CompiledCondition, CompiledNode, CompiledOperator, CompiledTask,
    ReviewOperation,
};
use review_graph::{NodeKind, PortContract};
use serde::{Deserialize, Serialize};

use self::artifact::ReviewArtifactCodec;
use crate::Loaded;

const HEAD: &str = "af_review_head";
const ROUND: &str = "af_review_round";

/// Settings derived by the caller from captured commands/packages and resource policy.
/// No model response or serialized compiled graph may supply these settings as authority.
pub struct ReviewWorker {
    pub package: String,
    pub allowance: NodeAllowance,
}

pub struct ReviewCompileContext {
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub head_input: String,
    pub round_input: String,
    pub workers: BTreeMap<String, ReviewWorker>,
    /// Public output names mapped to exact original Review node/port pairs.
    pub outputs: BTreeMap<String, Address>,
    pub limits: TaskLimitsV1,
    pub max_parallel: u32,
    /// Captured bound for the whole Gate operation, including every configured check.
    pub gate_wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInputLane {
    pub review_port: String,
    pub source_node: String,
    pub source_port: String,
    pub codec: ReviewArtifactCodec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutputPort {
    pub review_port: String,
    pub codec: ReviewArtifactCodec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewNodeMapping {
    pub task_node: String,
    pub review_inputs: BTreeSet<String>,
    pub inputs: BTreeMap<String, ReviewInputLane>,
    pub outputs: BTreeMap<String, ReviewOutputPort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyReviewCompilation {
    pub contract: PipelineContractV1,
    pub graph: CompiledTask,
    /// Original canonical Review node IDs, independent of Task-qualified name restrictions.
    pub nodes: BTreeMap<String, ReviewNodeMapping>,
}

impl ReviewNodeMapping {
    /// Reconstruct the exact legacy fan-in after the Task scheduler selected every lane.
    /// Callers pass only the compiled data lanes, excluding head/Round and branch guards.
    /// Sorting occurs after restoring raw IDs, matching the original Review scheduler.
    pub fn restore_inputs(
        &self,
        cas: &review_store::Cas,
        lanes: &review_graph::ArtifactMap,
    ) -> Result<review_graph::ArtifactMap, String> {
        if lanes.keys().ne(self.inputs.keys()) {
            return Err("Review inputs differ from the exact compiled lanes".into());
        }
        let mut inputs: review_graph::ArtifactMap = self
            .review_inputs
            .iter()
            .map(|port| (port.clone(), Vec::new()))
            .collect();
        for (name, ids) in lanes {
            let lane = &self.inputs[name];
            let target = inputs
                .get_mut(&lane.review_port)
                .ok_or("Undeclared Review input port")?;
            for id in ids {
                target.push(lane.codec.restore(cas, id)?);
            }
        }
        for ids in inputs.values_mut() {
            ids.sort();
        }
        Ok(inputs)
    }
}

fn port(artifact_type: &str, cardinality: PortCardinality, optional: bool) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality,
        optional,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        covers: BTreeSet::new(),
    }
}

fn output_codec(kind: NodeKind, output: &PortContract) -> Result<ReviewArtifactCodec, String> {
    use ReviewArtifactCodec::{Envelope, Flat};
    let ty = output.artifact_type.as_str();
    let opaque = ty == contract::OPAQUE_V1;
    let flat = |ty: &str| Flat {
        artifact_type: ty.into(),
    };
    let enveloped = || Envelope {
        artifact_type: ty.into(),
    };
    match kind {
        NodeKind::Generation => match ty {
            contract::FINDING_SET_V1 => Ok(enveloped()),
            contract::CHANGE_SET_V1 => Ok(flat(ty)),
            _ => Err("Review Generation requires a supported explicit output contract".into()),
        },
        NodeKind::Gate if opaque || ty == contract::GATE_DECISION_V1 => {
            Ok(flat(contract::GATE_DECISION_V1))
        }
        NodeKind::Reviewer if ty == contract::REVIEWER_RESULT_V2 => Ok(flat(ty)),
        NodeKind::Gather if opaque || ty == contract::REPORT_SET_V1 => {
            Ok(flat(contract::REPORT_SET_V1))
        }
        NodeKind::Ledger if opaque => Ok(Envelope {
            artifact_type: contract::FINDING_SET_V1.into(),
        }),
        NodeKind::Ledger if matches!(ty, contract::FINDING_SET_V1 | contract::DEMAND_SET_V1) => {
            Ok(enveloped())
        }
        NodeKind::Slicer if ty == contract::SLICE_SET_V1 => Ok(enveloped()),
        NodeKind::Scatter if ty == contract::SHARD_SET_V1 => Ok(enveloped()),
        _ => Err(format!("Unsupported Review output {} ({ty})", output.name)),
    }
}

/// Compile a validated legacy definition into the same scheduler and Attempt budget used by
/// Task Pipelines. This is a pure compiler: callers must re-create it from captured manifest,
/// lock/package bytes, exact Round inputs and trusted settings when validating an execution.
pub fn compile_legacy_review(
    loaded: &Loaded,
    context: ReviewCompileContext,
) -> Result<LegacyReviewCompilation, String> {
    context.limits.validate()?;
    if context.max_parallel == 0 || context.gate_wall_ms == 0 {
        return Err("Review requires nonzero concurrency and Gate wall bounds".into());
    }
    if context.inputs.is_empty() || context.outputs.is_empty() {
        return Err("Review requires an explicit public input/output contract".into());
    }
    for (name, value) in &context.inputs {
        if !review_core::task::is_name(name) {
            return Err("Invalid Review public input name".into());
        }
        value.validate()?;
    }
    let head = context
        .inputs
        .get(&context.head_input)
        .ok_or("Missing captured Review head input")?;
    let round = context
        .inputs
        .get(&context.round_input)
        .ok_or("Missing captured Review Round input")?;
    if head.artifact_type != contract::SOURCE_SNAPSHOT_V1
        || head.cardinality != PortCardinality::One
        || head.snapshot_id.is_none()
        || round.artifact_type != REVIEW_ROUND_V1
        || round.cardinality != PortCardinality::One
        || round.snapshot_id != head.snapshot_id
    {
        return Err("Review requires exact SourceSnapshot and Round input contracts".into());
    }
    let planned = loaded.planned();
    let worker_nodes: BTreeSet<_> = planned
        .nodes
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::Reviewer | NodeKind::Scatter))
        .map(|(name, _)| name.clone())
        .collect();
    if worker_nodes != context.workers.keys().cloned().collect() {
        return Err("Review Worker settings differ from the captured graph".into());
    }
    let root_address = |port: &str| Address {
        node: "root.inputs".into(),
        port: port.into(),
    };
    let mut graph = CompiledTask {
        schema: "af.compiled-task/1".into(),
        nodes: BTreeMap::new(),
        order: Vec::new(),
        inputs: context.inputs.clone(),
        outputs: BTreeMap::new(),
        coverage: BTreeMap::new(),
        calls: BTreeMap::new(),
        slots: BTreeMap::new(),
        replaced_workers: BTreeMap::new(),
        max_parallel: context.max_parallel,
        allowances: BTreeMap::new(),
        owned_children: BTreeMap::new(),
        experimental_slots: BTreeMap::new(),
        review_integration: None,
        token_scopes: BTreeMap::new(),
    };
    graph.nodes.insert(
        "root.inputs".into(),
        CompiledNode {
            operator: CompiledOperator::RootInputs,
            contract: PipelineContractV1 {
                inputs: BTreeMap::new(),
                outputs: context
                    .inputs
                    .iter()
                    .map(|(name, input)| {
                        (
                            name.clone(),
                            port(
                                &input.artifact_type,
                                input.cardinality,
                                input.artifact_ids.is_empty(),
                            ),
                        )
                    })
                    .collect(),
            },
            inputs: BTreeMap::new(),
            conditions: vec![],
        },
    );
    let mut mappings = BTreeMap::new();
    for (index, (name, node)) in planned.nodes.iter().enumerate() {
        let task_node = format!("root.review.n{index}");
        let mut mapping = ReviewNodeMapping {
            task_node: task_node.clone(),
            review_inputs: node.inputs.iter().map(|port| port.name.clone()).collect(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        };
        let mut outputs = BTreeMap::new();
        for (index, original) in node.outputs.iter().enumerate() {
            let output_name = format!("o{index}");
            let codec = output_codec(node.kind, original)?;
            let mut contract = port(
                codec.artifact_type(),
                original.cardinality,
                original.optional,
            );
            if original.snapshot_affinity == SnapshotAffinity::SameSubject {
                contract.affinity = PortAffinityV1::SameAs { input: HEAD.into() };
            }
            outputs.insert(output_name.clone(), contract);
            mapping.outputs.insert(
                output_name,
                ReviewOutputPort {
                    review_port: original.name.clone(),
                    codec,
                },
            );
        }
        let operation = match node.kind {
            NodeKind::Generation => ReviewOperation::Generation,
            NodeKind::Gate => {
                outputs.insert(
                    "outcome".into(),
                    port(REVIEW_GATE_OUTCOME_V1, PortCardinality::One, false),
                );
                graph.allowances.insert(
                    task_node.clone(),
                    NodeAllowance {
                        tokens_per_attempt: 0,
                        wall_ms_per_attempt: context.gate_wall_ms,
                        max_attempts: 1,
                        verification_attempts: 0,
                    },
                );
                ReviewOperation::Gate
            }
            NodeKind::Reviewer | NodeKind::Scatter => {
                let worker = &context.workers[name];
                let slot = format!("root.reviewer{index}");
                if node.kind == NodeKind::Reviewer && node.outputs.len() != 1 {
                    return Err("Review Worker requires exactly one original result output".into());
                }
                // Scatter sub-invocations use the parent's captured slot; compilation does
                // not grant a second scheduler or an independent allowance to the host.
                let output_type = if node.kind == NodeKind::Reviewer {
                    outputs["o0"].artifact_type.clone()
                } else {
                    contract::REVIEWER_RESULT_V2.into()
                };
                let spec = WorkerSlotV1 {
                    worker: worker.package.clone(),
                    role: "reviewer".into(),
                    input_type: "review.kernel/ReviewerInputs@1".into(),
                    output_type,
                    min_attempts: worker.allowance.verification_attempts,
                    max_attempts: worker.allowance.max_attempts,
                    allow_local_replacement: false,
                    independent_from: BTreeSet::new(),
                };
                spec.validate()?;
                graph.slots.insert(slot.clone(), spec);
                graph
                    .allowances
                    .insert(task_node.clone(), worker.allowance.clone());
                if node.kind == NodeKind::Reviewer {
                    outputs.insert(
                        "metadata".into(),
                        port(TASK_REVIEW_RESULT_METADATA_V1, PortCardinality::One, false),
                    );
                    ReviewOperation::Reviewer { slot }
                } else {
                    ReviewOperation::Scatter { slot }
                }
            }
            NodeKind::Gather => ReviewOperation::Gather,
            NodeKind::Ledger => {
                for (name, ty) in [
                    ("finding_set", contract::FINDING_SET_V1),
                    ("demand_set", contract::DEMAND_SET_V1),
                ] {
                    let mut companion = port(ty, PortCardinality::One, false);
                    companion.affinity = PortAffinityV1::SameAs { input: HEAD.into() };
                    outputs.insert(name.into(), companion);
                }
                ReviewOperation::Ledger
            }
            NodeKind::Slicer => ReviewOperation::Slicer,
            NodeKind::Task => {
                return Err("Legacy Review cannot declare Task execution nodes".into());
            }
        };
        graph.nodes.insert(
            task_node,
            CompiledNode {
                operator: CompiledOperator::ReviewDomain {
                    review_node: name.clone(),
                    operation,
                },
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([
                        (
                            HEAD.into(),
                            port(&head.artifact_type, head.cardinality, false),
                        ),
                        (
                            ROUND.into(),
                            port(&round.artifact_type, round.cardinality, false),
                        ),
                    ]),
                    outputs,
                },
                inputs: BTreeMap::from([
                    (HEAD.into(), root_address(&context.head_input)),
                    (ROUND.into(), root_address(&context.round_input)),
                ]),
                conditions: vec![],
            },
        );
        mappings.insert(name.clone(), mapping);
    }
    for name in planned.nodes.keys() {
        let mut edges = planned.dependencies_of(name);
        edges.sort_by(|a, b| {
            (&a.to.name, &a.from.node, &a.from.name).cmp(&(&b.to.name, &b.from.node, &b.from.name))
        });
        for (index, edge) in edges.iter().enumerate() {
            let source = &mappings[&edge.from.node];
            let (source_port, output) = source
                .outputs
                .iter()
                .find(|(_, output)| output.review_port == edge.from.name)
                .ok_or("Missing compiled Review source port")?;
            let address = Address {
                node: source.task_node.clone(),
                port: source_port.clone(),
            };
            let mut input_contract =
                graph.nodes[&source.task_node].contract.outputs[source_port].clone();
            let target_port = planned.nodes[name]
                .inputs
                .iter()
                .find(|port| port.name == edge.to.name)
                .ok_or("Missing Review input contract")?;
            input_contract.optional |= target_port.optional;
            input_contract.affinity =
                if target_port.snapshot_affinity == SnapshotAffinity::SameSubject {
                    PortAffinityV1::SameAs { input: HEAD.into() }
                } else {
                    PortAffinityV1::Unbound {}
                };
            let lane = ReviewInputLane {
                review_port: edge.to.name.clone(),
                source_node: edge.from.node.clone(),
                source_port: edge.from.name.clone(),
                codec: output.codec.clone(),
            };
            let lane_name = format!("i{index}");
            let mapping = mappings.get_mut(name).unwrap();
            mapping.inputs.insert(lane_name.clone(), lane);
            let node = graph.nodes.get_mut(&mapping.task_node).unwrap();
            node.contract
                .inputs
                .insert(lane_name.clone(), input_contract);
            node.inputs.insert(lane_name, address);
        }
        let conditions = planned
            .gates_for(name)
            .iter()
            .map(|gate| CompiledCondition {
                source: Address {
                    node: mappings[gate].task_node.clone(),
                    port: "outcome".into(),
                },
                outcome: ReceiptOutcomeV1::Passed,
            })
            .collect();
        graph
            .nodes
            .get_mut(&mappings[name].task_node)
            .unwrap()
            .conditions = conditions;
    }
    for (public, source) in &context.outputs {
        if !review_core::task::is_name(public) {
            return Err("Invalid Review public output name".into());
        }
        let node = mappings
            .get(&source.node)
            .ok_or("Unknown Review public output node")?;
        let (port, _) = node
            .outputs
            .iter()
            .find(|(_, output)| output.review_port == source.port)
            .ok_or("Unknown Review public output port")?;
        graph.outputs.insert(
            public.clone(),
            Address {
                node: node.task_node.clone(),
                port: port.clone(),
            },
        );
    }
    graph.calls.insert(
        "root".into(),
        CompiledCall {
            pipeline: "af/legacy-review".into(),
            inputs: context
                .inputs
                .keys()
                .map(|name| (name.clone(), root_address(name)))
                .collect(),
            outputs: graph.outputs.clone(),
            coverage: BTreeMap::new(),
            max_attempts: context.limits.max_attempts,
            max_parallel: context.max_parallel,
        },
    );
    for node in graph.nodes.values() {
        node.contract.validate()?;
    }
    graph.order = graph.scheduler_plan()?.order;
    graph.budget(context.limits)?;
    let contract = PipelineContractV1 {
        inputs: graph.nodes["root.inputs"].contract.outputs.clone(),
        outputs: graph
            .outputs
            .iter()
            .map(|(name, source)| {
                let mut port = graph.nodes[&source.node].contract.outputs[&source.port].clone();
                if matches!(port.affinity, PortAffinityV1::SameAs { .. }) {
                    port.affinity = PortAffinityV1::SameAs {
                        input: context.head_input.clone(),
                    };
                }
                (name.clone(), port)
            })
            .collect(),
    };
    contract.validate()?;
    Ok(LegacyReviewCompilation {
        contract,
        graph,
        nodes: mappings,
    })
}

#[cfg(test)]
mod tests;
