//! Owned expansion is data registration beneath the existing scheduler and Attempt ledger.
use super::*;
use review_core::task::owned_children::{
    TASK_OWNED_CHILD_SET_V1, TaskOwnedChildSetV1, TaskOwnedChildV1,
};
use review_core::task::pipeline::PipelinePortV1;
use review_graph::{NodeKind, NodeOutcome, OwnedChildDispatch, PortContract, SnapshotAffinity};
use review_store::store::task::execution::owned::{RegisteredTaskChildren, ResolvedTaskNode};

fn port(name: &str, value: &PipelinePortV1) -> PortContract {
    // Exact Snapshot affinity is checked by common Store output admission. The scheduler
    // transports the captured typed ports and does not manufacture a new affinity policy.
    PortContract {
        name: name.into(),
        artifact_type: value.artifact_type.clone(),
        cardinality: value.cardinality,
        optional: value.optional,
        snapshot_affinity: SnapshotAffinity::Any,
    }
}

impl TaskRuntime<'_, '_> {
    pub(super) fn resolve_node(&self, node: &str) -> Result<ResolvedTaskNode, String> {
        if let Some(definition) = self.graph.nodes.get(node) {
            return Ok(ResolvedTaskNode {
                definition: definition.clone(),
                allowance: self.graph.allowances.get(node).cloned(),
                owned: None,
                expected_inputs: None,
            });
        }
        self.projection()?
            .execution
            .ok_or("Task has no execution")?
            .resolve_node(node)
            .map_err(|e| e.to_string())
    }

    fn registered_children(
        &self,
        parent_invocation: &str,
    ) -> Result<Option<RegisteredTaskChildren>, String> {
        self.store
            .lock()
            .expect("Task Store")
            .get_task_owned_children(self.cas, self.lease.task_id(), parent_invocation)
            .map_err(|e| e.to_string())
    }

    pub(super) fn expand_children(&self, node: &Node) -> Result<Vec<OwnedChildDispatch>, String> {
        let state = self
            .projection()?
            .execution
            .ok_or("Task has no execution")?;
        // A sealed parent replays its original fold. Late accounting must not reopen children
        // or recompute the Shard Set from a different history prefix.
        if state.outputs.contains_key(&node.id) {
            return Ok(vec![]);
        }
        let (parent_id, parent) = state
            .invocations
            .get(&node.id)
            .ok_or("Owned parent invocation is not admitted")?;
        let template = self
            .graph
            .owned_children
            .get(&node.id)
            .ok_or("Task node has no captured child template")?;
        let children = match self.registered_children(parent_id)? {
            Some(children) => children,
            None => {
                let source = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.host.prepare_owned_children(self.cas, parent)
                }))
                .unwrap_or_else(|_| Err("Task child expansion panicked".into()))?;
                if parent
                    .inputs
                    .get(&template.source_input)
                    .map(|p| p.artifact_ids.as_slice())
                    != Some(std::slice::from_ref(&source.source_artifact_id))
                    || source.source_item_ids.len() > template.max_children as usize
                {
                    return Err(
                        "Owned expansion differs from its captured source or fan-out".into(),
                    );
                }
                let mut children = Vec::with_capacity(source.source_item_ids.len());
                for (index, item_id) in source.source_item_ids.into_iter().enumerate() {
                    let item = envelope(self.cas, &item_id)?;
                    let mut inputs = BTreeMap::new();
                    for (child_port, parent_port) in &template.inherited_inputs {
                        if let Some(value) = parent.inputs.get(parent_port) {
                            inputs.insert(child_port.clone(), value.clone());
                        }
                    }
                    inputs.insert(
                        template.item_input.clone(),
                        ArtifactInputV1 {
                            artifact_ids: vec![item_id.clone()],
                            artifact_type: item.artifact_type,
                            cardinality: review_core::PortCardinality::One,
                            snapshot_id: item.subject_snapshot_id,
                        },
                    );
                    let child = TaskInvocationV1 {
                        plan_id: self.plan_id.clone(),
                        node: format!("{}.slice{}", node.id, index + 1),
                        inputs,
                    };
                    let invocation_id = self.capture_invocation(&child)?;
                    children.push(TaskOwnedChildV1 {
                        node: child.node,
                        source_item_id: item_id,
                        invocation_id,
                    });
                }
                let set = TaskOwnedChildSetV1 {
                    plan_id: self.plan_id.clone(),
                    parent_invocation_id: parent_id.clone(),
                    source_artifact_id: source.source_artifact_id,
                    children,
                };
                set.validate()?;
                let (id, _) = self
                    .cas
                    .put_artifact(
                        TASK_OWNED_CHILD_SET_V1,
                        Producer::KernelOperation {
                            run_id: task_run_id(self.lease.task_id()).map_err(|e| e.to_string())?,
                            node_id: Some(node.id.clone()),
                            operation_id: "task-owned-children@1".into(),
                        },
                        set.artifact_refs().into_iter().map(str::to_owned).collect(),
                        None,
                        serde_json::to_value(&set).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                self.store
                    .lock()
                    .expect("Task Store")
                    .register_task_owned_children(self.cas, &self.lease, &id, self.authority)
                    .map_err(|e| e.to_string())?
            }
        };
        let state = self
            .projection()?
            .execution
            .ok_or("Task has no execution")?;
        children
            .child_set()
            .children
            .iter()
            .map(|child| {
                let resolved = state.resolve_node(&child.node).map_err(|e| e.to_string())?;
                let input = resolved
                    .expected_inputs
                    .ok_or("Registered child has no exact inputs")?;
                let mut inputs = artifact_map(&input);
                for name in resolved.definition.contract.inputs.keys() {
                    inputs.entry(name.clone()).or_default();
                }
                Ok(OwnedChildDispatch {
                    node: Node::new(&child.node, NodeKind::Task)
                        .accepting_contracts(
                            resolved
                                .definition
                                .contract
                                .inputs
                                .iter()
                                .map(|(n, p)| port(n, p))
                                .collect(),
                        )
                        .emitting_contracts(
                            resolved
                                .definition
                                .contract
                                .outputs
                                .iter()
                                .map(|(n, p)| port(n, p))
                                .collect(),
                        ),
                    inputs,
                })
            })
            .collect()
    }

    pub(super) fn complete_children(
        &self,
        node: &Node,
        outcomes: &[(String, NodeOutcome)],
    ) -> Result<ArtifactMap, String> {
        let state = self
            .projection()?
            .execution
            .ok_or("Task has no execution")?;
        if let Some((_, output)) = state.outputs.get(&node.id) {
            return Ok(artifact_map(&output.outputs));
        }
        let (parent_id, parent) = state
            .invocations
            .get(&node.id)
            .ok_or("Owned parent invocation is not admitted")?;
        let children = self
            .registered_children(parent_id)?
            .ok_or("Owned parent has no complete registry")?;
        if outcomes
            .iter()
            .map(|(node, _)| node.as_str())
            .collect::<Vec<_>>()
            != children
                .child_set()
                .children
                .iter()
                .map(|child| child.node.as_str())
                .collect::<Vec<_>>()
        {
            return Err("Scheduler completion changed the complete owned child order".into());
        }
        let facts = self
            .store
            .lock()
            .expect("Task Store")
            .task_owned_child_evidence(self.cas, &children)
            .map_err(|e| e.to_string())?;
        // Error strings from scheduler memory are not durable authority. The host classifies
        // every registered child using exact common Attempt/publication evidence instead.
        let values = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.host
                .complete_owned_children(self.cas, parent, children.child_set(), &facts)
        }))
        .unwrap_or_else(|_| Err("Task owned completion panicked".into()))?;
        let id = self.record_output(parent_id, parent, values.clone(), None)?;
        self.pending_outputs
            .lock()
            .expect("Task outputs")
            .insert(node.id.clone(), (id, None));
        Ok(artifact_map(&values))
    }

    /// Use factual publication only for registered children and their coordinating parent.
    /// Each Store operation rechecks current plan approval and the exact durable prefixes.
    pub(super) fn publish_owned_output(
        &self,
        node: &str,
        output_id: &str,
        attempt_id: Option<&str>,
    ) -> Result<bool, String> {
        let state = self
            .projection()?
            .execution
            .ok_or("Task has no execution")?;
        let resolved = state.resolve_node(node).map_err(|e| e.to_string())?;
        if let Some(address) = resolved.owned {
            let children = self
                .registered_children(&address.parent_invocation_id)?
                .ok_or("Owned child lost its registration")?;
            self.store
                .lock()
                .expect("Task Store")
                .publish_task_owned_child(
                    self.cas,
                    &self.lease,
                    &children,
                    output_id,
                    attempt_id.ok_or("Owned child output has no selected Attempt")?,
                    self.authority,
                )
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }
        if self.graph.owned_children.contains_key(node) {
            if attempt_id.is_some() {
                return Err("Owned parent cannot own a paid Attempt".into());
            }
            let (parent_id, _) = state
                .invocations
                .get(node)
                .ok_or("Owned parent invocation missing")?;
            let children = self
                .registered_children(parent_id)?
                .ok_or("Owned parent registry missing")?;
            self.store
                .lock()
                .expect("Task Store")
                .complete_task_owned_children(
                    self.cas,
                    &self.lease,
                    &children,
                    output_id,
                    self.authority,
                )
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }
        Ok(false)
    }
}
