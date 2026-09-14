//! Requirements-aware verification needs the exact root input before paid work.
use super::*;
impl TaskPlanCompiler {
    pub(super) fn validate_requirements_inputs(&self, graph: &CompiledTask) -> Result<(), String> {
        use review_core::task::pipeline::TaskOperatorV1;
        use review_core::task::verification::TASK_EVALUATION_V1;
        for (name, node) in &graph.nodes {
            let review_graph::task::CompiledOperator::Primitive {
                operator: TaskOperatorV1::Verify { slot },
                ..
            } = &node.operator
            else {
                continue;
            };
            let evaluator_outputs: Vec<_> = node
                .contract
                .outputs
                .iter()
                .filter(|(_, p)| {
                    p.artifact_type == TASK_EVALUATION_V1
                        || (graph.inputs.contains_key("requirements")
                            && matches!(
                                p.artifact_type.as_str(),
                                review_core::contract::REVIEWER_RESULT_V1
                                    | review_core::contract::REVIEWER_RESULT_V2
                            ))
                })
                .collect();
            if evaluator_outputs.is_empty() {
                continue;
            }
            let requires_input = evaluator_outputs
                .iter()
                .any(|(_, port)| port.artifact_type == TASK_EVALUATION_V1);
            let worker = self
                .workers
                .get(&graph.slots[slot].worker)
                .ok_or("Evaluation Worker is absent")?;
            let requirements: Vec<_> = node
                .contract
                .inputs
                .iter()
                .filter(|(input, port)| {
                    port.artifact_type == "af/Requirements@1"
                        && (!requires_input || !port.optional)
                        && node.inputs.get(*input).is_some_and(|address| {
                            address.node == "root.inputs" && address.port == "requirements"
                        })
                })
                .map(|(name, _)| name)
                .collect();
            if requirements.len() != 1
                || evaluator_outputs.iter().any(|(output, _)| {
                    !worker
                        .signature
                        .retains
                        .get(*output)
                        .is_some_and(|inputs| inputs.contains(requirements[0]))
                })
            {
                return Err(format!(
                    "Verifier {name} must consume and retain the exact Task Requirements"
                ));
            }
        }
        Ok(())
    }
}
