//! Review semantics for protected owned membership; no scheduling or resource account lives here.
use super::*;
use review_config::task::legacy_review::{
    ReviewInputLane, ReviewOutputPort, artifact::ReviewArtifactCodec,
};
use review_core::task::owned_children::TaskOwnedChildSetV1;
use review_core::{ShardOutcomeV1, ShardSetV1, SliceSetV1};
use review_store::store::task::execution::owned::{RegisteredTaskChildren, TaskOwnedChildEvidence};

#[derive(Clone)]
pub(super) struct OwnedReviewChild {
    pub node: Node,
    pub mapping: ReviewNodeMapping,
    pub operation: ReviewOperation,
    pub invocation: TaskInvocationV1,
    pub registered: RegisteredTaskChildren,
}

impl LegacyReviewTaskHost<'_, '_> {
    pub(super) fn output_contract(
        &self,
        input: &TaskInvocationV1,
        port: &str,
    ) -> Result<review_core::task::pipeline::PipelinePortV1, String> {
        let owned = self.owned.lock().expect("owned Review mappings");
        let contract = if let Some(child) = owned.get(&input.node) {
            &self.captured.compilation.graph.owned_children[child.registered.parent_node()].contract
        } else {
            &self
                .captured
                .compilation
                .graph
                .nodes
                .get(&input.node)
                .ok_or("Unknown Review output node")?
                .contract
        };
        contract
            .outputs
            .get(port)
            .cloned()
            .ok_or_else(|| "Unknown Review output port".into())
    }

    pub(super) fn hydrate_owned(&self) -> Result<(), String> {
        let mut registrations = Vec::new();
        {
            let store = self.domain.store.lock().expect("Task Store");
            let state = store
                .task_projection(self.domain.cas, &self.task.task_id)
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Review Task")?;
            if let Some(execution) = &state.execution {
                for owner in self.captured.compilation.graph.owned_children.keys() {
                    if let Some((id, _)) = execution.invocations.get(owner)
                        && let Some(children) = store
                            .get_task_owned_children(self.domain.cas, &self.task.task_id, id)
                            .map_err(|e| e.to_string())?
                    {
                        registrations.push(children);
                    }
                }
            }
        }
        for children in registrations {
            self.install_owned_mapping(self.domain.cas, children)?;
        }
        Ok(())
    }

    pub(super) fn hydrate_owned_invocation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        if self
            .captured
            .compilation
            .graph
            .nodes
            .contains_key(&input.node)
            || self
                .owned
                .lock()
                .expect("owned Review mappings")
                .contains_key(&input.node)
        {
            return Ok(());
        }
        let registered = {
            let store = self.domain.store.lock().expect("Task Store");
            let state = store
                .task_projection(cas, &self.task.task_id)
                .map_err(|e| e.to_string())?
                .ok_or("Unknown Review Task")?;
            let execution = state
                .execution
                .as_ref()
                .ok_or("Review Task has no execution")?;
            let address = execution
                .resolve_node(&input.node)
                .map_err(|e| e.to_string())?
                .owned
                .ok_or("Unregistered Review child")?;
            store
                .get_task_owned_children(cas, &self.task.task_id, &address.parent_invocation_id)
                .map_err(|e| e.to_string())?
                .ok_or("Missing Review child registration")?
        };
        self.install_owned_mapping(cas, registered)
    }

    fn install_owned_mapping(
        &self,
        cas: &Cas,
        registered: RegisteredTaskChildren,
    ) -> Result<(), String> {
        let set = registered.child_set();
        let parent: TaskInvocationV1 = serde_json::from_value(
            cas.get_artifact(&set.parent_invocation_id)
                .map_err(|e| e.to_string())?
                .payload,
        )
        .map_err(|e| e.to_string())?;
        self.check_owned_set(cas, &parent, set)?;
        if registered.task_id() != self.task.task_id || registered.parent_node() != parent.node {
            return Err("Owned Review registration changed Task or parent".into());
        }
        let (base, original, _) = self
            .operation(&parent)?
            .ok_or("Owned Review parent is not Scatter")?;
        let template = &self.captured.compilation.graph.owned_children[&parent.node];
        let mut inputs = base
            .inputs
            .iter()
            .filter(|port| port.artifact_type != review_core::contract::SLICE_SET_V1)
            .cloned()
            .collect::<Vec<_>>();
        if inputs.iter().any(|port| port.name == "slice") {
            return Err("Scatter reserves canonical child port slice".into());
        }
        inputs.push(review_graph::PortContract::new(
            "slice",
            review_core::contract::REVIEW_SLICE_V1,
        ));
        let source_lane = &original.inputs[&template.source_input];
        let mut mappings = Vec::new();
        for child in &set.children {
            let slice: review_core::ReviewSliceV1 = serde_json::from_value(
                cas.get_artifact(&child.source_item_id)
                    .map_err(|e| e.to_string())?
                    .payload,
            )
            .map_err(|e| e.to_string())?;
            let mut mapping = original.clone();
            mapping.task_node = child.node.clone();
            mapping.inputs.remove(&template.source_input);
            mapping.review_inputs.remove(&source_lane.review_port);
            mapping.review_inputs.insert("slice".into());
            mapping.inputs.insert(
                template.item_input.clone(),
                ReviewInputLane {
                    review_port: "slice".into(),
                    source_node: base.id.clone(),
                    source_port: "slice".into(),
                    codec: ReviewArtifactCodec::Envelope {
                        artifact_type: review_core::contract::REVIEW_SLICE_V1.into(),
                    },
                },
            );
            mapping.outputs = BTreeMap::from([(
                "o0".into(),
                ReviewOutputPort {
                    review_port: "out".into(),
                    codec: ReviewArtifactCodec::Flat {
                        artifact_type: template.contract.outputs["o0"].artifact_type.clone(),
                    },
                },
            )]);
            let node = Node::new(&slice.runtime_node_id, NodeKind::Reviewer)
                .accepting_contracts(inputs.clone())
                .emitting_contracts(vec![review_graph::PortContract::new(
                    "out",
                    &template.contract.outputs["o0"].artifact_type,
                )]);
            let CompiledOperator::ReviewDomain { operation, .. } = &template.operator else {
                return Err("Owned Review template changed operator".into());
            };
            let invocation = serde_json::from_value(
                cas.get_artifact(&child.invocation_id)
                    .map_err(|e| e.to_string())?
                    .payload,
            )
            .map_err(|e| e.to_string())?;
            mappings.push((
                child.node.clone(),
                OwnedReviewChild {
                    node,
                    mapping,
                    operation: operation.clone(),
                    invocation,
                    registered: registered.clone(),
                },
            ));
        }
        let mut owned = self.owned.lock().expect("owned Review mappings");
        for (id, child) in &mappings {
            if let Some(old) = owned.get(id)
                && (old.registered.child_set_id() != registered.child_set_id()
                    || old.invocation != child.invocation)
            {
                return Err("Owned Review mapping changed its durable membership".into());
            }
        }
        for (id, child) in mappings {
            self.domain
                .dynamic_reviewer_bases
                .lock()
                .expect("dynamic Reviewer bases")
                .insert(child.node.id.clone(), base.id.clone());
            owned.insert(id, child);
        }
        Ok(())
    }

    fn owned_source(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
    ) -> Result<(Node, String, SliceSetV1), String> {
        let template = self
            .captured
            .compilation
            .graph
            .owned_children
            .get(&parent.node)
            .ok_or("Review parent has no captured owned template")?;
        let (node, _, operation) = self.operation(parent)?.ok_or("Not a Review owner")?;
        if !matches!(operation, ReviewOperation::Scatter { .. }) {
            return Err("Owned Review parent must be Scatter".into());
        }
        let input = parent
            .inputs
            .get(&template.source_input)
            .ok_or("Owned Review parent lost its SliceSet")?;
        let [source] = input.artifact_ids.as_slice() else {
            return Err("Owned Review requires one complete SliceSet".into());
        };
        let envelope = cas.get_artifact(source).map_err(|e| e.to_string())?;
        if input.artifact_type != review_core::contract::SLICE_SET_V1
            || input.cardinality != review_core::PortCardinality::One
            || envelope.artifact_type != input.artifact_type
            || envelope.subject_snapshot_id.as_deref()
                != Some(&self.domain.authority.head_snapshot_id)
            || input.snapshot_id != envelope.subject_snapshot_id
        {
            return Err("Owned SliceSet changed type or current Snapshot".into());
        }
        let slices: SliceSetV1 =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        let policy = self
            .domain
            .slicing
            .values()
            .find(|policy| policy.scatter_node == node.id)
            .ok_or("Owned Scatter has no captured Slicer policy")?;
        let paths = self
            .domain
            .authority
            .change_set
            .as_ref()
            .map(|change| change.change_set().changed_paths.clone())
            .unwrap_or_else(|| {
                self.domain
                    .snapshot
                    .entries
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect()
            });
        let expected = policy.plan(
            &self.domain.authority.subject_id,
            &paths,
            &self.domain.static_node_ids,
        )?;
        if slices != expected || template.max_children != slices.max_fanout {
            return Err("Owned SliceSet differs from exact captured partition or policy".into());
        }
        Ok((node, source.clone(), slices))
    }

    pub(super) fn prepare_owned_review(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
    ) -> Result<crate::task::TaskOwnedChildrenInputs, String> {
        self.invocation(parent)?;
        let (node, source, slices) = self.owned_source(cas, parent)?;
        let source_item_ids = slices
            .slices
            .iter()
            .map(|slice| {
                cas.put_artifact(
                    review_core::contract::REVIEW_SLICE_V1,
                    crate::scatter::slice_producer(&self.domain.run_id, &node.id, slice),
                    vec![source.clone()],
                    Some(self.domain.authority.head_snapshot_id.clone()),
                    serde_json::to_value(slice).map_err(|e| e.to_string())?,
                )
                .map(|value| value.0)
                .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(crate::task::TaskOwnedChildrenInputs {
            source_artifact_id: source,
            source_item_ids,
        })
    }

    pub(super) fn check_owned_set(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        set.validate()?;
        let (node, source, slices) = self.owned_source(cas, parent)?;
        let invocation = cas
            .get_artifact(&set.parent_invocation_id)
            .map_err(|e| e.to_string())?;
        if set.plan_id != self.plan_id
            || set.source_artifact_id != source
            || set.children.len() != slices.slices.len()
            || invocation.artifact_type != review_core::task::execution::TASK_INVOCATION_V1
            || invocation.payload != serde_json::to_value(parent).map_err(|e| e.to_string())?
        {
            return Err(
                "Owned Review set changed parent, plan, source or complete membership".into(),
            );
        }
        for (index, (child, slice)) in set.children.iter().zip(&slices.slices).enumerate() {
            let item = cas
                .get_artifact(&child.source_item_id)
                .map_err(|e| e.to_string())?;
            if child.node != format!("{}.slice{}", parent.node, index + 1)
                || item.artifact_type != review_core::contract::REVIEW_SLICE_V1
                || item.producer
                    != crate::scatter::slice_producer(&self.domain.run_id, &node.id, slice)
                || item.input_artifacts != [source.clone()]
                || item.subject_snapshot_id.as_deref()
                    != Some(&self.domain.authority.head_snapshot_id)
                || item.payload != serde_json::to_value(slice).map_err(|e| e.to_string())?
            {
                return Err(
                    "Owned Review child changed canonical source order, producer or exact Slice"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn owned_shards(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
        facts: &[TaskOwnedChildEvidence],
    ) -> Result<(Node, ShardSetV1), String> {
        self.check_owned_set(cas, parent, set)?;
        let (node, source, slices) = self.owned_source(cas, parent)?;
        if facts.len() != set.children.len()
            || facts
                .iter()
                .zip(&set.children)
                .any(|(fact, child)| fact.child != *child)
        {
            return Err("Owned Review completion omitted or reordered child facts".into());
        }
        let mut outcomes = BTreeMap::new();
        for (slice, fact) in slices.slices.iter().zip(facts) {
            let outcome = if let Some(ids) = &fact.completed_artifact_ids {
                if ids.is_empty() || fact.published_output_id.is_none() {
                    return Err("Completed Review shard lacks published canonical outputs".into());
                }
                ShardOutcomeV1::Completed {
                    result_artifact_ids: ids.clone(),
                }
            } else {
                if fact
                    .attempts
                    .iter()
                    .any(|attempt| attempt.started && attempt.result.is_none())
                {
                    return Err("Owned Review shard has unresolved started work".into());
                }
                if fact.selected_output_id.is_some() || fact.published_output_id.is_some() {
                    ShardOutcomeV1::Failed {
                        reason: "selected Review child did not complete canonical publication"
                            .into(),
                    }
                } else if fact.attempts.iter().any(|attempt| attempt.started) {
                    ShardOutcomeV1::Failed {reason:"registered Review child exhausted its admitted Attempts without a selected output".into()}
                } else {
                    ShardOutcomeV1::Missing {
                        reason: "registered Review child did not start an Attempt".into(),
                    }
                }
            };
            outcomes.insert(slice.slice_id.clone(), outcome);
        }
        Ok((
            node,
            crate::scatter::fold_shards(&slices, &source, &outcomes)?,
        ))
    }

    pub(super) fn complete_owned_review(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
        facts: &[TaskOwnedChildEvidence],
    ) -> Result<Ports, String> {
        let (node, shards) = self.owned_shards(cas, parent, set, facts)?;
        let payload = serde_json::to_value(&shards).map_err(|e| e.to_string())?;
        let operation_id = review_store::content_id(&payload).map_err(|e| e.to_string())?;
        let (id, _) = cas
            .put_artifact(
                review_core::contract::SHARD_SET_V1,
                Producer::KernelOperation {
                    run_id: self.domain.run_id.clone(),
                    node_id: Some(node.id),
                    operation_id,
                },
                crate::scatter::shard_artifact_inputs(&shards),
                Some(self.domain.authority.head_snapshot_id.clone()),
                payload,
            )
            .map_err(|e| e.to_string())?;
        let output = BTreeMap::from([(
            "o0".into(),
            artifact_input(
                cas,
                review_core::contract::SHARD_SET_V1,
                vec![id],
                review_core::PortCardinality::One,
            )?,
        )]);
        self.admitted_outputs
            .lock()
            .expect("Review outputs")
            .entry(set.parent_invocation_id.clone())
            .or_default()
            .push(output.clone());
        Ok(output)
    }

    pub(super) fn check_owned_completion(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        set: &TaskOwnedChildSetV1,
        facts: &[TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let (node, shards) = self.owned_shards(cas, parent, set, facts)?;
        if output.invocation_id != set.parent_invocation_id || output.outputs.len() != 1 {
            return Err("Owned Review completion changed its parent output".into());
        }
        let port = output
            .outputs
            .get("o0")
            .ok_or("Owned Review completion lacks ShardSet")?;
        let [id] = port.artifact_ids.as_slice() else {
            return Err("Owned Review completion requires one ShardSet".into());
        };
        let artifact = cas.get_artifact(id).map_err(|e| e.to_string())?;
        let payload = serde_json::to_value(&shards).map_err(|e| e.to_string())?;
        let producer = Producer::KernelOperation {
            run_id: self.domain.run_id.clone(),
            node_id: Some(node.id),
            operation_id: review_store::content_id(&payload).map_err(|e| e.to_string())?,
        };
        if artifact.artifact_type != review_core::contract::SHARD_SET_V1
            || artifact.payload != payload
            || artifact.producer != producer
            || artifact.input_artifacts != crate::scatter::shard_artifact_inputs(&shards)
            || artifact.subject_snapshot_id.as_deref()
                != Some(&self.domain.authority.head_snapshot_id)
            || artifact_input(
                cas,
                review_core::contract::SHARD_SET_V1,
                vec![id.clone()],
                review_core::PortCardinality::One,
            )? != *port
        {
            return Err(
                "Owned Review completion differs from the exact lossless canonical fold".into(),
            );
        }
        Ok(())
    }

    pub(super) fn publish_owned_shards(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        output_id: &str,
    ) -> Result<(), String> {
        let (invocation_id, _) = self.invocation(input)?;
        let mut store = self.domain.store.lock().expect("Task Store");
        let registered = store
            .get_task_owned_children(cas, &self.task.task_id, &invocation_id)
            .map_err(|e| e.to_string())?
            .ok_or("Owned ShardSet lacks its durable child registry")?;
        store
            .publish_task_owned_review_shards(
                cas,
                &self.lease,
                &registered,
                output_id,
                &self.authority(),
            )
            .map_err(|e| e.to_string())
    }
}
