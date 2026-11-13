//! Requirements-aware verification needs the exact root input before paid work.
use super::*;
impl TaskPlanCompiler {
    pub(super) fn validate_requirements_inputs(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        graph: &CompiledTask,
    ) -> Result<(), String> {
        use af_contracts::task::pipeline::TaskOperatorV1;
        use af_contracts::task::verification::TASK_EVALUATION_V1;
        // Expand only the captured template view, never the runtime DAG. Owned reviewers
        // inherit the parent's resolved wires and receive one kernel-generated Slice.
        let mut children = Vec::new();
        for (owner, template) in &graph.owned_children {
            let parent = graph
                .nodes
                .get(owner)
                .ok_or("Owned reviewer has no parent")?;
            let mut child = parent.clone();
            child.operator = template.operator.clone();
            child.contract = template.contract.clone();
            child.inputs = template
                .inherited_inputs
                .iter()
                .filter_map(|(input, inherited)| {
                    parent
                        .inputs
                        .get(inherited)
                        .map(|wire| (input.clone(), wire.clone()))
                })
                .collect();
            children.push((owner, child, template));
        }
        for (name, node, template) in graph
            .nodes
            .iter()
            .map(|(n, v)| (n, v, None))
            .chain(children.iter().map(|(n, v, t)| (*n, v, Some(*t))))
        {
            let af_graph::task::CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::Worker { slot }
                    | TaskOperatorV1::Verify { slot }
                    | TaskOperatorV1::FixVerify { slot }
                    | TaskOperatorV1::ReviewScatter { slot, .. },
                ..
            } = &node.operator
            else {
                continue;
            };
            let worker = self
                .workers
                .get(&graph.slots[slot].worker)
                .ok_or("Evaluation Worker is absent")?;
            let files = self
                .package_files(&worker.name)
                .ok_or("Missing captured Worker files")?;
            let knowledge = super::super::knowledge::parse_knowledge(files)?.is_some();
            let development_warm = super::super::development::parse_development(files)?
                .is_some_and(|request| !request.caches.is_empty());
            if matches!(
                node.operator,
                af_graph::task::CompiledOperator::Primitive {
                    operator: TaskOperatorV1::ReviewScatter { .. },
                    ..
                }
            ) {
                if knowledge {
                    return Err(format!(
                        "Verifier {name}: knowledge selection is unsupported for a Scatter coordinator"
                    ));
                }
                if development_warm {
                    return Err(format!(
                        "Verifier {name}: development cache selection is unsupported for a Scatter coordinator"
                    ));
                }
                // Only the child invokes this Worker's schema.
                continue;
            }
            if !node.contract.outputs.values().any(|port| {
                port.artifact_type == TASK_EVALUATION_V1
                    || port.artifact_type == af_contracts::contract::REVIEWER_RESULT_V2
            }) {
                continue;
            }
            let schema = self.worker_schema(&worker.name, "input.schema.json")?;
            let mut supply: BTreeMap<_, _> = node
                .inputs
                .iter()
                .map(|(input, address)| {
                    let presence = if address.node == "root.inputs" {
                        Some(
                            graph
                                .inputs
                                .get(&address.port)
                                .is_some_and(|input| !input.artifact_ids.is_empty()),
                        )
                    } else {
                        graph
                            .nodes
                            .get(&address.node)
                            .and_then(|source| source.contract.outputs.get(&address.port))
                            .and_then(|port| {
                                (!port.optional
                                    && port.cardinality == af_contracts::PortCardinality::One)
                                    .then_some(true)
                            })
                    };
                    (input.clone(), presence)
                })
                .collect();
            if let Some(template) = template {
                supply.insert(template.item_input.clone(), Some(true));
            }
            // Static and owned reviewers may receive a kernel-generated Diff Subject.
            // Its presence depends on the source's captured base, not just a wire.
            let diff = af_contracts::task::diff_subject::DIFF_SUBJECT_PORT;
            if node.contract.inputs.contains_key(diff) {
                let presence = match supply.get("source").copied().flatten() {
                    Some(true) => diff_subject_presence(cas, task, graph, node)?,
                    presence => presence,
                };
                supply.insert(diff.into(), presence);
            }
            let replaced = if knowledge {
                af_contracts::task::knowledge::replaced_input_names(
                    node.contract
                        .inputs
                        .iter()
                        .map(|(name, port)| (name.as_str(), port.artifact_type.as_str())),
                )
            } else {
                BTreeSet::new()
            };
            let supply = af_contracts::task::worker_inputs::project(
                supply,
                development_warm.then_some(Some(true)),
                knowledge.then_some(Some(true)),
                &replaced,
            );
            super::input_presence::validate(&schema, &supply)
                .map_err(|reason| format!("Verifier {name}: {reason}"))?;
            let evaluator_outputs: Vec<_> = node
                .contract
                .outputs
                .iter()
                .filter(|(_, p)| {
                    p.artifact_type == TASK_EVALUATION_V1
                        || ((graph.inputs.contains_key("requirements")
                            || node.contract.inputs.iter().any(|(input, port)| {
                                port.artifact_type == "af/Requirements@1"
                                    && node.inputs.contains_key(input)
                            }))
                            && p.artifact_type == af_contracts::contract::REVIEWER_RESULT_V2)
                })
                .collect();
            if evaluator_outputs.is_empty() {
                continue;
            }
            let requires_input = evaluator_outputs
                .iter()
                .any(|(_, port)| port.artifact_type == TASK_EVALUATION_V1);
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

/// Only an exact root source or an exact declared base proves the kernel's supply.
/// An upstream source's future lineage stays unknown; a wire alone proves no base.
fn diff_subject_presence(
    cas: &Cas,
    task: &TaskRevisionV1,
    graph: &CompiledTask,
    node: &af_graph::task::CompiledNode,
) -> Result<Option<bool>, String> {
    if af_source_git::task::declared_base(cas, Some(task))?.is_some() {
        return Ok(Some(true));
    }
    let Some(source) = node
        .inputs
        .get("source")
        .filter(|wire| wire.node == "root.inputs")
        .and_then(|wire| graph.inputs.get(&wire.port))
    else {
        return Ok(None);
    };
    let Some(snapshot_id) = source.snapshot_id.as_deref() else {
        return Ok(None);
    };
    let (snapshot, _) = af_source_git::task::read_snapshot(cas, snapshot_id)?;
    Ok(Some(
        af_source_git::task::recorded_base(cas, &snapshot)?.is_some(),
    ))
}
