//! The one synthetic knowledge input is checked beside exact graph inputs, never exempted
//! merely because its port is optional. Private source records must be admitted outputs.
use super::*;
use af_contracts::task::knowledge::*;
use af_contracts::task::pipeline::{PortAffinityV1, TaskOperatorV1};

impl TaskExecutionProjection {
    pub(super) fn verify_knowledge(
        &self,
        cas: &Cas,
        task_id: &str,
        input: &TaskInvocationV1,
        node: &af_graph::task::CompiledNode,
        value: &task::ArtifactInputV1,
    ) -> Result<(), StoreError> {
        let slot = match &node.operator {
            CompiledOperator::Primitive {
                operator:
                    TaskOperatorV1::Worker { slot }
                    | TaskOperatorV1::Verify { slot }
                    | TaskOperatorV1::FixVerify { slot },
                ..
            } => slot,
            _ => return Err(conflict("Knowledge selection requires a captured Worker")),
        };
        if !node.contract.inputs.get(KNOWLEDGE_PORT).is_some_and(|p| {
            p.artifact_type == CONTEXT_TYPE
                && p.optional
                && p.cardinality == af_contracts::PortCardinality::One
                && p.affinity == (PortAffinityV1::Unbound {})
        }) || node.inputs.contains_key(KNOWLEDGE_PORT)
            || value.artifact_type != CONTEXT_TYPE
            || value.artifact_ids.len() != 1
            || value.cardinality != af_contracts::PortCardinality::One
            || value.snapshot_id.is_some()
        {
            return Err(conflict("Undeclared or wired knowledge selection"));
        }
        let envelope = cas
            .get_artifact(&value.artifact_ids[0])
            .map_err(|e| conflict(e.to_string()))?;
        let run = super::super::task_run_id(task_id)?;
        if envelope.artifact_type != CONTEXT_TYPE
            || envelope.subject_snapshot_id.is_some()
            || envelope.producer
                != (af_contracts::Producer::KernelOperation {
                    run_id: run.clone(),
                    node_id: Some(slot.clone()),
                    operation_id: CONTEXT_OPERATION.into(),
                })
        {
            return Err(conflict(
                "Knowledge context has no kernel selection authority",
            ));
        }
        let context: KnowledgeContextV1 =
            serde_json::from_value(envelope.payload).map_err(|e| conflict(e.to_string()))?;
        context.validate().map_err(conflict)?;
        let source = input
            .inputs
            .get("source")
            .ok_or_else(|| conflict("Knowledge has no declared source"))?;
        if context.manifest.run_id != run
            || &context.manifest.slot != slot
            || source.snapshot_id.as_deref() != Some(&context.manifest.source_snapshot_id)
            || !envelope.input_artifacts.contains(&context.index_id)
        {
            return Err(conflict(
                "Knowledge selection changed its Task, slot, source or index",
            ));
        }
        let index = cas
            .get_artifact(&context.index_id)
            .map_err(|e| conflict(e.to_string()))?;
        if index.artifact_type != INDEX_TYPE
            || index.payload != serde_json::to_value(&context.manifest)?
            || index.producer
                != (af_contracts::Producer::KernelOperation {
                    run_id: run.clone(),
                    node_id: Some(slot.clone()),
                    operation_id: INDEX_OPERATION.into(),
                })
        {
            return Err(conflict(
                "Knowledge manifest differs from its authoritative index",
            ));
        }
        let mut replaced = BTreeSet::from([QUERY_PORT.to_string()]);
        replaced.extend(
            input
                .inputs
                .iter()
                .filter(|(_, p)| p.artifact_type == task::WORKER_NOTES_V1)
                .map(|(n, _)| n.clone()),
        );
        if context.replaced_inputs != replaced {
            return Err(conflict(
                "Knowledge selection changed its private provenance inputs",
            ));
        }
        for entry in context.manifest.entries.values() {
            match entry.visibility {
                KnowledgeVisibilityV1::ProjectSource => {
                    if Some(&entry.source_snapshot_id) != source.snapshot_id.as_ref()
                        || !source.artifact_ids.contains(&entry.source_artifact_id)
                    {
                        return Err(conflict("Knowledge fact belongs to an undeclared source"));
                    }
                }
                KnowledgeVisibilityV1::SameSlot => {
                    if !input.inputs.values().any(|p| {
                        p.artifact_type == task::WORKER_NOTES_V1
                            && p.artifact_ids.contains(&entry.source_artifact_id)
                    }) {
                        return Err(conflict("Private knowledge has no declared Notes input"));
                    }
                    let notes = cas
                        .get_artifact(&entry.source_artifact_id)
                        .map_err(|e| conflict(e.to_string()))?;
                    let af_contracts::Producer::Attempt {
                        run_id,
                        node_id,
                        attempt_id,
                    } = &notes.producer
                    else {
                        return Err(conflict("Private knowledge has no Attempt producer"));
                    };
                    let prior = self.resolve_node(node_id)?;
                    if run_id != &run
                        || !matches!(&prior.definition.operator, CompiledOperator::Primitive {operator:TaskOperatorV1::Worker {slot:other} | TaskOperatorV1::Verify {slot:other} | TaskOperatorV1::FixVerify {slot:other},..} if other==slot)
                        || !self.outputs.get(node_id).is_some_and(|(_, o)| {
                            o.outputs.values().any(|p| {
                                p.artifact_type == task::WORKER_NOTES_V1
                                    && p.artifact_ids.contains(&entry.source_artifact_id)
                            })
                        })
                    {
                        return Err(conflict(
                            "Private knowledge is not an admitted same-slot output",
                        ));
                    }
                    let bound: af_contracts::WorkerNotesV1 = serde_json::from_value(notes.payload)
                        .map_err(|e| conflict(e.to_string()))?;
                    bound
                        .check_bound(node_id, attempt_id, &entry.source_snapshot_id)
                        .map_err(conflict)?;
                }
            }
        }
        if context.query_id.as_ref().is_some_and(|id| {
            !input.inputs.get(QUERY_PORT).is_some_and(|p| {
                p.artifact_type == QUERY_TYPE
                    && p.artifact_ids.as_slice() == std::slice::from_ref(id)
            })
        }) {
            return Err(conflict("Knowledge selected an undeclared query"));
        }
        for excerpt in &context.excerpts {
            if cas
                .get_bounded(&excerpt.entry.content_id, excerpt.entry.bytes)
                .map_err(|e| conflict(e.to_string()))?
                != excerpt.text.as_bytes()
            {
                return Err(conflict(
                    "Knowledge excerpt differs from its exact evidence bytes",
                ));
            }
        }
        Ok(())
    }
}
