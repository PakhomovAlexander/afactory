//! Explicit generation-three transformation. Frozen static compilation is unchanged.

use super::*;
use review_graph::task::OwnedChildTemplateV1;

pub const REVIEW_SLICE_INPUT: &str = "af_review_slice";

pub fn install_owned_review_children(
    loaded: &Loaded,
    compilation: &mut LegacyReviewCompilation,
) -> Result<(), String> {
    if !compilation.graph.owned_children.is_empty() {
        return Err("Owned Review templates can be installed only once".into());
    }
    let mut templates = BTreeMap::new();
    for (review_node, mapping) in &compilation.nodes {
        let parent = &compilation.graph.nodes[&mapping.task_node];
        let CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Scatter { slot },
            ..
        } = &parent.operator
        else {
            continue;
        };
        let policies: Vec<_> = loaded
            .slicing()
            .values()
            .filter(|policy| &policy.scatter == review_node)
            .collect();
        let [policy] = policies.as_slice() else {
            return Err("Owned Scatter requires its one captured Slicer policy".into());
        };
        let source_ports: Vec<_> = parent
            .contract
            .inputs
            .iter()
            .filter(|(_, port)| port.artifact_type == contract::SLICE_SET_V1)
            .collect();
        let [(source_input, source_port)] = source_ports.as_slice() else {
            return Err("Owned Scatter requires its one captured SliceSet input".into());
        };
        if source_port.optional || source_port.cardinality != PortCardinality::One {
            return Err("Owned Scatter requires a complete, singular SliceSet".into());
        }
        let mut inputs = parent.contract.inputs.clone();
        inputs.remove(*source_input);
        let inherited_inputs = inputs
            .keys()
            .map(|name| (name.clone(), name.clone()))
            .collect();
        let mut item_port = port(contract::REVIEW_SLICE_V1, PortCardinality::One, false);
        item_port.affinity = PortAffinityV1::SameAs { input: HEAD.into() };
        if inputs
            .insert(REVIEW_SLICE_INPUT.into(), item_port)
            .is_some()
        {
            return Err("Captured Scatter collides with the reserved Slice input".into());
        }
        let mut result = port(
            &compilation.graph.slots[slot].output_type,
            PortCardinality::One,
            false,
        );
        result.affinity = PortAffinityV1::SameAs { input: HEAD.into() };
        let allowance = compilation
            .graph
            .allowances
            .get(&mapping.task_node)
            .ok_or("Captured Scatter has no original child allowance")?
            .clone();
        if allowance.verification_attempts != 0 {
            return Err("Owned Scatter cannot reinterpret a parent verification reserve".into());
        }
        templates.insert(
            mapping.task_node.clone(),
            OwnedChildTemplateV1 {
                operator: CompiledOperator::ReviewDomain {
                    review_node: review_node.clone(),
                    operation: ReviewOperation::Reviewer { slot: slot.clone() },
                },
                contract: PipelineContractV1 {
                    inputs,
                    outputs: BTreeMap::from([
                        ("o0".into(), result),
                        (
                            "metadata".into(),
                            port(TASK_REVIEW_RESULT_METADATA_V1, PortCardinality::One, false),
                        ),
                    ]),
                },
                allowance,
                max_children: policy.max_fanout,
                source_input: (*source_input).clone(),
                item_input: REVIEW_SLICE_INPUT.into(),
                inherited_inputs,
            },
        );
    }
    // Validate a complete replacement before changing the captured preparation object.
    let mut graph = compilation.graph.clone();
    for owner in templates.keys() {
        graph.allowances.remove(owner);
    }
    graph.owned_children = templates;
    graph.owned_template_allowances()?;
    compilation.graph = graph;
    Ok(())
}
