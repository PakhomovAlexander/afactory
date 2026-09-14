//! Bounded owned nodes beside the immutable DAG, using the ordinary Task Attempt ledger.
use super::*;
use review_attempt::task_budget::{NodeAllowance, OwnedNodeAllowance};
use review_core::task::owned_children::*;
use review_graph::task::{CompiledNode, ReviewOperation};

mod review_shards;

#[derive(Debug, Clone)]
pub(in crate::store::task) struct RecordedChildren {
    id: String,
    set: TaskOwnedChildSetV1,
    parent_node: String,
    invocations: BTreeMap<String, TaskInvocationV1>,
    review_nodes: BTreeMap<String, String>,
    published: BTreeMap<String, (String, String)>,
    completed: Option<String>,
}

/// Exact recorded membership, not an extension of a writer lease or dispatch authority.
#[derive(Debug, Clone)]
pub struct RegisteredTaskChildren {
    task_id: String,
    id: String,
    set: TaskOwnedChildSetV1,
    parent_node: String,
}

impl RegisteredTaskChildren {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn child_set_id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn child_set(&self) -> &TaskOwnedChildSetV1 {
        &self.set
    }
    pub fn parent_node(&self) -> &str {
        &self.parent_node
    }
}

#[derive(Debug, Clone)]
pub struct OwnedChildAddress {
    pub child_set_id: String,
    pub parent_node: String,
    pub parent_invocation_id: String,
    pub source_item_id: String,
    pub review_node: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedTaskNode {
    pub definition: CompiledNode,
    pub allowance: Option<NodeAllowance>,
    pub owned: Option<OwnedChildAddress>,
    pub expected_inputs: Option<BTreeMap<String, task::ArtifactInputV1>>,
}

/// In-memory evidence supplied to pure domain validation. Completed artifact IDs are
/// available only after the Store proves publication, including canonical Review receipts.
#[derive(Debug, Clone)]
pub struct TaskOwnedChildEvidence {
    pub child: TaskOwnedChildV1,
    pub attempts: Vec<TaskAttemptAccounting>,
    pub selected_output_id: Option<String>,
    pub published_output_id: Option<String>,
    pub completed_artifact_ids: Option<Vec<String>>,
}

pub(in crate::store::task) fn templates(
    graph: &CompiledTask,
) -> BTreeMap<String, OwnedNodeAllowance> {
    graph
        .owned_children
        .iter()
        .map(|(parent, template)| {
            (
                parent.clone(),
                OwnedNodeAllowance {
                    allowance: template.allowance.clone(),
                    max_children: template.max_children,
                },
            )
        })
        .collect()
}

pub fn read_task_owned_children(cas: &Cas, id: &str) -> Result<TaskOwnedChildSetV1, StoreError> {
    let artifact = envelope(cas, id, TASK_OWNED_CHILD_SET_V1)?;
    let set: TaskOwnedChildSetV1 = serde_json::from_value(artifact.payload)?;
    set.validate().map_err(conflict)?;
    if artifact.input_artifacts != set.artifact_refs() || artifact.subject_snapshot_id.is_some() {
        return Err(conflict(
            "Owned child set differs from its exact artifact references",
        ));
    }
    for id in set.artifact_refs() {
        cas.verify(id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    Ok(set)
}

impl TaskExecutionProjection {
    fn active_children(&self, set: &RecordedChildren) -> bool {
        self.invocations
            .get(&set.parent_node)
            .is_some_and(|(id, input)| {
                id == &set.set.parent_invocation_id && input.plan_id == set.set.plan_id
            })
    }

    pub fn resolve_node(&self, node: &str) -> Result<ResolvedTaskNode, StoreError> {
        if let Some(phase) = self
            .active_review_integration()
            .filter(|p| p.node() == node)
        {
            return phase.resolved();
        }
        if let Some(definition) = self.graph.nodes.get(node) {
            return Ok(ResolvedTaskNode {
                definition: definition.clone(),
                allowance: self.graph.allowances.get(node).cloned(),
                owned: None,
                expected_inputs: None,
            });
        }
        for set in self.owned.values().filter(|set| self.active_children(set)) {
            let Some(input) = set.invocations.get(node) else {
                continue;
            };
            return resolve_child(&self.graph, set, node, input);
        }
        Err(conflict(
            "Invocation names an unplanned or unregistered node",
        ))
    }

    /// Historical accounting only. The caller supplies the graph loaded from this Attempt's
    /// original plan; archived registration never restores dispatch authority after handoff.
    pub fn resolve_attempt_node(
        &self,
        attempt: &TaskAttemptAccounting,
        graph: &CompiledTask,
    ) -> Result<ResolvedTaskNode, StoreError> {
        let recorded = self
            .attempts
            .get(&attempt.attempt_id)
            .ok_or_else(|| conflict("Unknown accounting Attempt"))?;
        if recorded.plan_id != attempt.plan_id
            || recorded.invocation_id != attempt.invocation_id
            || recorded.reservation != attempt.reservation
        {
            return Err(conflict("Accounting Attempt changed its original identity"));
        }
        let node = &attempt.reservation.node;
        if let Some(compiled) = &graph.review_integration {
            if compiled.node == *node {
                let phase = self
                    .review_integrations
                    .values()
                    .find(|p| p.phase().plan_id == attempt.plan_id && p.node() == node)
                    .ok_or_else(|| {
                        conflict("Historical Integration Attempt lacks its exact phase")
                    })?;
                return phase.resolved();
            }
        }
        if let Some(definition) = graph.nodes.get(node) {
            return Ok(ResolvedTaskNode {
                definition: definition.clone(),
                allowance: graph.allowances.get(node).cloned(),
                owned: None,
                expected_inputs: None,
            });
        }
        for set in self
            .owned
            .values()
            .filter(|set| set.set.plan_id == attempt.plan_id)
        {
            if set
                .set
                .children
                .iter()
                .any(|child| &child.node == node && child.invocation_id == attempt.invocation_id)
            {
                return resolve_child(graph, set, node, &set.invocations[node]);
            }
        }
        Err(conflict(
            "Attempt has no original captured node or child registration",
        ))
    }

    pub(super) fn check_owned_open(&self, node: &str) -> Result<(), StoreError> {
        self.check_integration_node(node)?;
        let resolved = self.resolve_node(node)?;
        if let Some(address) = resolved.owned
            && self.owned[&address.child_set_id].completed.is_some()
        {
            return Err(conflict("Owned Task parent is sealed"));
        }
        Ok(())
    }

    fn checked_set(
        &self,
        children: &RegisteredTaskChildren,
    ) -> Result<&RecordedChildren, StoreError> {
        let set = self
            .owned
            .get(&children.id)
            .ok_or_else(|| conflict("Unknown owned child registration"))?;
        if set.set != children.set
            || set.parent_node != children.parent_node
            || !self.active_children(set)
        {
            return Err(conflict(
                "Owned child capability belongs to another plan or invocation",
            ));
        }
        Ok(set)
    }

    fn derive_children(&self, cas: &Cas, id: &str) -> Result<RecordedChildren, StoreError> {
        let set = read_task_owned_children(cas, id)?;
        let parent = invocation(cas, &set.parent_invocation_id)?;
        if parent.plan_id != set.plan_id
            || self.invocations.get(&parent.node).map(|(id, _)| id)
                != Some(&set.parent_invocation_id)
            || self.graph.allowances.contains_key(&parent.node)
        {
            return Err(conflict(
                "Owned children require their admitted non-reservable parent invocation",
            ));
        }
        let template = self
            .graph
            .owned_children
            .get(&parent.node)
            .ok_or_else(|| conflict("Parent has no captured child template"))?;
        let source = parent
            .inputs
            .get(&template.source_input)
            .ok_or_else(|| conflict("Owned parent has no captured source input"))?;
        if source.artifact_ids != [set.source_artifact_id.clone()]
            || set.children.is_empty()
            || set.children.len() > template.max_children as usize
        {
            return Err(conflict(
                "Owned child source or fan-out differs from its template",
            ));
        }
        let review = matches!(
            template.operator,
            CompiledOperator::ReviewDomain {
                operation: ReviewOperation::Reviewer { .. },
                ..
            }
        );
        let slices = if review {
            let source = envelope(
                cas,
                &set.source_artifact_id,
                review_core::contract::SLICE_SET_V1,
            )?;
            let slices: review_core::SliceSetV1 = serde_json::from_value(source.payload)?;
            slices.validate().map_err(conflict)?;
            if slices.slices.len() != set.children.len()
                || slices.max_fanout != template.max_children
            {
                return Err(conflict(
                    "Owned Review set omits or changes captured Slices",
                ));
            }
            Some(slices)
        } else {
            None
        };
        let mut invocations = BTreeMap::new();
        let mut review_nodes = BTreeMap::new();
        for (index, child) in set.children.iter().enumerate() {
            if child.node != format!("{}.slice{}", parent.node, index + 1)
                || self.graph.nodes.contains_key(&child.node)
            {
                return Err(conflict(
                    "Owned child address differs from its canonical source order",
                ));
            }
            let item = cas
                .get_artifact(&child.source_item_id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?;
            if item.input_artifacts != [set.source_artifact_id.clone()]
                || item.subject_snapshot_id != source.snapshot_id
            {
                return Err(conflict("Owned item changed its source or Snapshot"));
            }
            if let Some(slices) = &slices {
                let slice: review_core::ReviewSliceV1 =
                    serde_json::from_value(item.payload.clone())?;
                if item.artifact_type != review_core::contract::REVIEW_SLICE_V1
                    || slice != slices.slices[index]
                {
                    return Err(conflict(
                        "Owned Review item differs from the complete Slice Set",
                    ));
                }
                review_nodes.insert(child.node.clone(), slice.runtime_node_id);
            }
            let mut inputs = BTreeMap::new();
            for (name, parent_port) in &template.inherited_inputs {
                if let Some(value) = parent.inputs.get(parent_port) {
                    inputs.insert(name.clone(), value.clone());
                } else if template
                    .contract
                    .inputs
                    .get(name)
                    .is_none_or(|port| !port.optional)
                {
                    return Err(conflict("Owned child lacks a required inherited input"));
                }
            }
            inputs.insert(
                template.item_input.clone(),
                task::ArtifactInputV1 {
                    artifact_ids: vec![child.source_item_id.clone()],
                    artifact_type: item.artifact_type,
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: item.subject_snapshot_id,
                },
            );
            if inputs.iter().any(|(name, value)| {
                template.contract.inputs.get(name).is_none_or(|port| {
                    port.artifact_type != value.artifact_type
                        || port.cardinality != value.cardinality
                })
            }) || template
                .contract
                .inputs
                .iter()
                .any(|(name, port)| !port.optional && !inputs.contains_key(name))
            {
                return Err(conflict(
                    "Owned inputs differ from the captured child contract",
                ));
            }
            let input = invocation(cas, &child.invocation_id)?;
            if input.plan_id != set.plan_id || input.node != child.node || input.inputs != inputs {
                return Err(conflict(
                    "Owned invocation changed its exact inherited or item inputs",
                ));
            }
            invocations.insert(child.node.clone(), input);
        }
        Ok(RecordedChildren {
            id: id.into(),
            set,
            parent_node: parent.node,
            invocations,
            review_nodes,
            published: BTreeMap::new(),
            completed: None,
        })
    }

    pub(super) fn apply_owned(
        &mut self,
        cas: &Cas,
        task_id: &str,
        record: &TaskExecutionRecordV1,
    ) -> Result<(), StoreError> {
        match record {
            TaskExecutionRecordV1::OwnedChildrenRegistered { child_set_id } => {
                let registered = self.derive_children(cas, child_set_id)?;
                if self
                    .owned
                    .values()
                    .any(|old| old.set.parent_invocation_id == registered.set.parent_invocation_id)
                    || self.outputs.contains_key(&registered.parent_node)
                {
                    return Err(conflict(
                        "Owned Task parent already has a registry or output",
                    ));
                }
                self.budget
                    .register_owned_children(
                        &registered.parent_node,
                        &registered
                            .set
                            .children
                            .iter()
                            .map(|child| child.node.clone())
                            .collect::<Vec<_>>(),
                    )
                    .map_err(conflict)?;
                self.owned.insert(child_set_id.clone(), registered);
            }
            TaskExecutionRecordV1::OwnedChildPublished {
                child_set_id,
                output_id,
                attempt_id,
            } => {
                let out = output(cas, output_id)?;
                let input = self.verify_output(cas, &out)?;
                verify_attempt_producer(cas, task_id, &input.node, attempt_id, output_id, &out)?;
                self.check_owned_open(&input.node)?;
                let resolved = self.resolve_node(&input.node)?;
                if resolved.owned.as_ref().map(|owner| &owner.child_set_id) != Some(child_set_id)
                    || self.outputs.contains_key(&input.node)
                    || self.reusable_output(&input.node)
                        != Some((output_id.clone(), attempt_id.clone()))
                {
                    return Err(conflict(
                        "Owned publication is not its exact selected child output",
                    ));
                }
                self.owned
                    .get_mut(child_set_id)
                    .expect("registered child")
                    .published
                    .insert(input.node.clone(), (output_id.clone(), attempt_id.clone()));
                self.outputs.insert(input.node, (output_id.clone(), out));
            }
            TaskExecutionRecordV1::OwnedChildrenCompleted {
                child_set_id,
                output_id,
            } => {
                let set = self
                    .owned
                    .get(child_set_id)
                    .ok_or_else(|| conflict("Unknown owned child set"))?;
                if !self.active_children(set)
                    || set.completed.is_some()
                    || self.outputs.contains_key(&set.parent_node)
                    || self.attempts.values().any(|attempt| {
                        set.set
                            .children
                            .iter()
                            .any(|child| child.invocation_id == attempt.invocation_id)
                            && !attempt.released
                            && attempt.settlement.is_none()
                    })
                {
                    return Err(conflict(
                        "Owned completion has stale, sealed or pending child work",
                    ));
                }
                let out = output(cas, output_id)?;
                if out.invocation_id != set.set.parent_invocation_id {
                    return Err(conflict("Owned completion changed its parent invocation"));
                }
                let input = self.verify_output(cas, &out)?;
                self.outputs.insert(input.node, (output_id.clone(), out));
                self.owned
                    .get_mut(child_set_id)
                    .expect("known set")
                    .completed = Some(output_id.clone());
            }
            _ => return Err(conflict("Expected owned Task execution record")),
        }
        Ok(())
    }
}

pub(in crate::store::task) fn is_owned_record(record: &TaskExecutionRecordV1) -> bool {
    matches!(
        record,
        TaskExecutionRecordV1::OwnedChildrenRegistered { .. }
            | TaskExecutionRecordV1::OwnedChildPublished { .. }
            | TaskExecutionRecordV1::OwnedChildrenCompleted { .. }
    )
}

impl TaskExecutionProjection {
    pub(super) fn verify_owned_invocation_id(
        &self,
        node: &str,
        id: &str,
    ) -> Result<(), StoreError> {
        if let Some(owner) = self.resolve_node(node)?.owned
            && !self.owned[&owner.child_set_id]
                .set
                .children
                .iter()
                .any(|child| child.node == node && child.invocation_id == id)
        {
            return Err(conflict(
                "Owned child changed its registered invocation envelope",
            ));
        }
        Ok(())
    }

    pub(super) fn check_owned_publication(
        &self,
        children: &RegisteredTaskChildren,
        output_id: &str,
    ) -> Result<(), StoreError> {
        let set = self.checked_set(children)?;
        if !set.published.values().any(|(id, _)| id == output_id) {
            return Err(conflict(
                "Canonical owned publication requires an unsealed recorded child output",
            ));
        }
        Ok(())
    }
}

pub(in crate::store::task) fn validate_cached(
    cas: &Cas,
    state: &TaskProjection,
) -> Result<(), StoreError> {
    if let Some(execution) = &state.execution {
        for set in execution.owned.values() {
            if read_task_owned_children(cas, &set.id)? != set.set {
                return Err(conflict("Cached owned registration changed identity"));
            }
            for child in &set.set.children {
                if invocation(cas, &child.invocation_id)? != set.invocations[&child.node] {
                    return Err(conflict("Cached owned invocation changed identity"));
                }
                cas.verify(&child.source_item_id)
                    .map_err(|e| StoreError::Artifact(e.to_string()))?;
            }
            for id in set
                .published
                .values()
                .map(|(id, _)| id)
                .chain(set.completed.iter())
            {
                let mut refs = BTreeSet::new();
                for value in output(cas, id)?.outputs.values() {
                    validate_input_refs(cas, value, &mut refs)?;
                }
                for id in refs {
                    cas.verify(&id)
                        .map_err(|e| StoreError::Artifact(e.to_string()))?;
                }
            }
        }
    }
    Ok(())
}

impl EventStore {
    fn owned_review_prefix(
        &self,
        cas: &Cas,
        state: &TaskProjection,
    ) -> Result<Option<(String, u64)>, StoreError> {
        use review_core::task::review_compat::*;
        state
            .revision
            .inputs
            .values()
            .find(|port| port.artifact_type == LEGACY_REVIEW_ROUND_V1)
            .map(|port| {
                let round: LegacyReviewRoundV1 =
                    payload(cas, &port.artifact_ids[0], LEGACY_REVIEW_ROUND_V1)?;
                Ok((round.campaign_id.clone(), self.len(&round.campaign_id)?))
            })
            .transpose()
    }

    pub fn register_task_owned_children(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        child_set_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<RegisteredTaskChildren, StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        if let Some(old) = execution.owned.get(child_set_id) {
            let handle = registered(&state.task_id, old);
            execution.checked_set(&handle)?;
            return Ok(handle);
        }
        let value = execution.derive_children(cas, child_set_id)?;
        let parent = invocation(cas, &value.set.parent_invocation_id)?;
        authority
            .validate_owned_children(cas, &state.revision, &plan, &parent, &value.set)
            .map_err(conflict)?;
        let fresh = self.checked_task_recording(cas, lease, authority)?.0;
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict("Task changed during owned admission"));
        }
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::OwnedChildrenRegistered {
                child_set_id: child_set_id.into(),
            },
            now()?,
        )?;
        Ok(registered(&state.task_id, &value))
    }

    pub fn get_task_owned_children(
        &self,
        cas: &Cas,
        task_id: &str,
        parent_invocation_id: &str,
    ) -> Result<Option<RegisteredTaskChildren>, StoreError> {
        Ok(self.task_projection(cas, task_id)?.and_then(|state| {
            state.execution.and_then(|execution| {
                execution
                    .owned
                    .values()
                    .find(|set| {
                        set.set.parent_invocation_id == parent_invocation_id
                            && execution.active_children(set)
                    })
                    .map(|set| registered(task_id, set))
            })
        }))
    }

    pub fn task_owned_child_evidence(
        &self,
        cas: &Cas,
        children: &RegisteredTaskChildren,
    ) -> Result<Vec<TaskOwnedChildEvidence>, StoreError> {
        let state = self
            .task_projection(cas, &children.task_id)?
            .ok_or_else(|| conflict("Unknown Task"))?;
        self.owned_child_evidence(cas, &state, children)
    }

    fn owned_child_evidence(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        children: &RegisteredTaskChildren,
    ) -> Result<Vec<TaskOwnedChildEvidence>, StoreError> {
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        let set = execution.checked_set(children)?;
        let attempts = execution.attempt_accounting();
        set.set
            .children
            .iter()
            .map(|child| {
                let selected_output_id = execution.reusable_output(&child.node).map(|(id, _)| id);
                let published_output_id = set.published.get(&child.node).map(|(id, _)| id.clone());
                let completed_artifact_ids = if let Some(id) = &published_output_id {
                    if set.review_nodes.contains_key(&child.node) {
                        self.owned_review_artifacts(cas, state, id)?
                    } else {
                        Some(
                            output(cas, id)?
                                .outputs
                                .into_values()
                                .flat_map(|value| value.artifact_ids)
                                .collect(),
                        )
                    }
                } else {
                    None
                };
                Ok(TaskOwnedChildEvidence {
                    child: child.clone(),
                    attempts: attempts
                        .iter()
                        .filter(|attempt| attempt.invocation_id == child.invocation_id)
                        .cloned()
                        .collect(),
                    selected_output_id,
                    published_output_id,
                    completed_artifact_ids,
                })
            })
            .collect()
    }

    fn owned_review_artifacts(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        id: &str,
    ) -> Result<Option<Vec<String>>, StoreError> {
        use rusqlite::{OptionalExtension, params};
        let (context, expected) = review::selected_review_event(cas, state, id)?;
        let row: Option<String> = self.conn.query_row("SELECT payload FROM events WHERE run_id=?1 AND causation_id=?2 AND node_id=?3 AND attempt_id=?4 AND type='TaskReviewResultSelected@1'", params![context.campaign_id,context.round_event_id,context.review_node,context.attempt_id], |row| row.get(0)).optional()?;
        let Some(row) = row else { return Ok(None) };
        if serde_json::from_str::<serde_json::Value>(&row)? != expected.payload {
            return Err(conflict(
                "Owned Review selection differs from common selected output",
            ));
        }
        let selected: review_core::task::review_compat::TaskReviewResultSelectedV1 =
            serde_json::from_value(expected.payload)?;
        let row: Option<String> = self.conn.query_row("SELECT payload FROM events WHERE run_id=?1 AND causation_id=?2 AND node_id=?3 AND attempt_id=?4 AND type='NodeOutputReceipt@1'", params![context.campaign_id,context.round_event_id,context.review_node,context.attempt_id], |row| row.get(0)).optional()?;
        let Some(row) = row else { return Ok(None) };
        let receipt: review_core::NodeOutputReceiptPayloadV1 = serde_json::from_str(&row)?;
        let result = cas
            .get_artifact(&selected.result_envelope_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
        let [port] = receipt.outputs.as_slice() else {
            return Err(conflict("Owned Review receipt has ambiguous outputs"));
        };
        if receipt.node != context.review_node
            || port.artifact_ids != [selected.result_artifact_id.clone()]
            || port.artifact_type != result.artifact_type
            || port.cardinality != review_core::PortCardinality::One
            || port.subject_snapshot_id != result.subject_snapshot_id
        {
            return Err(conflict(
                "Owned canonical receipt changed its selected result or Snapshot",
            ));
        }
        Ok(Some(vec![selected.result_artifact_id]))
    }

    pub fn publish_task_owned_child(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        children: &RegisteredTaskChildren,
        output_id: &str,
        attempt_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        if children.task_id != lease.task_id {
            return Err(conflict("Owned child capability belongs to another Task"));
        }
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        let set = execution.checked_set(children)?;
        let (_, input) =
            Self::validate_task_output(cas, &state, &plan, output_id, Some(attempt_id), authority)?;
        let fresh = self.checked_task_recording(cas, lease, authority)?.0;
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict("Task changed during owned publication"));
        }
        if let Some(old) = set.published.get(&input.node) {
            return if old == &(output_id.into(), attempt_id.into()) {
                Ok(())
            } else {
                Err(conflict("Conflicting owned publication replay"))
            };
        }
        self.task_execution_record(
            cas,
            lease,
            TaskExecutionRecordV1::OwnedChildPublished {
                child_set_id: children.id.clone(),
                output_id: output_id.into(),
                attempt_id: attempt_id.into(),
            },
            now()?,
        )?;
        Ok(())
    }

    pub fn complete_task_owned_children(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        children: &RegisteredTaskChildren,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        if children.task_id != lease.task_id {
            return Err(conflict("Owned child capability belongs to another Task"));
        }
        let execution = state
            .execution
            .as_ref()
            .ok_or_else(|| conflict("Task has no execution"))?;
        let set = execution.checked_set(children)?;
        let (out, parent) =
            Self::validate_task_output(cas, &state, &plan, output_id, None, authority)?;
        let fresh = self.checked_task_recording(cas, lease, authority)?.0;
        if fresh.next_sequence != state.next_sequence {
            return Err(conflict("Task changed during owned completion validation"));
        }
        if let Some(old) = &set.completed {
            return if old == output_id {
                Ok(())
            } else {
                Err(conflict("Conflicting owned completion replay"))
            };
        }
        let prefix = self.owned_review_prefix(cas, &state)?;
        let facts = self.owned_child_evidence(cas, &state, children)?;
        authority
            .validate_owned_completion(
                cas,
                &state.revision,
                &plan,
                &parent,
                &children.set,
                &facts,
                &out,
            )
            .map_err(conflict)?;
        let fresh = self.checked_task_recording(cas, lease, authority)?.0;
        if fresh.next_sequence != state.next_sequence
            || self.owned_review_prefix(cas, &fresh)? != prefix
        {
            return Err(conflict(
                "Task or Review changed during owned completion validation",
            ));
        }
        self.task_execution_record_with_owned_prefix(
            cas,
            lease,
            TaskExecutionRecordV1::OwnedChildrenCompleted {
                child_set_id: children.id.clone(),
                output_id: output_id.into(),
            },
            now()?,
            (state.next_sequence, prefix),
        )?;
        Ok(())
    }
}

fn registered(task_id: &str, set: &RecordedChildren) -> RegisteredTaskChildren {
    RegisteredTaskChildren {
        task_id: task_id.into(),
        id: set.id.clone(),
        set: set.set.clone(),
        parent_node: set.parent_node.clone(),
    }
}

fn resolve_child(
    graph: &CompiledTask,
    set: &RecordedChildren,
    node: &str,
    input: &TaskInvocationV1,
) -> Result<ResolvedTaskNode, StoreError> {
    let template = graph
        .owned_children
        .get(&set.parent_node)
        .ok_or_else(|| conflict("Owned node lost its captured template"))?;
    let child = set
        .set
        .children
        .iter()
        .find(|child| child.node == node)
        .ok_or_else(|| conflict("Unknown child"))?;
    Ok(ResolvedTaskNode {
        definition: CompiledNode {
            operator: template.operator.clone(),
            contract: template.contract.clone(),
            inputs: BTreeMap::new(),
            conditions: vec![],
        },
        allowance: Some(template.allowance.clone()),
        expected_inputs: Some(input.inputs.clone()),
        owned: Some(OwnedChildAddress {
            child_set_id: set.id.clone(),
            parent_node: set.parent_node.clone(),
            parent_invocation_id: set.set.parent_invocation_id.clone(),
            source_item_id: child.source_item_id.clone(),
            review_node: set.review_nodes.get(node).cloned(),
        }),
    })
}

/// The canonical receipt may follow its Task selection in a later transaction. Recheck the
/// ownership seal under that same canonical writer lock, without calling a host or opening a
/// second Store. Static Review selections and historical legacy Attempts retain their path.
pub(in crate::store) fn check_canonical_child_receipt(
    connection: &rusqlite::Connection,
    cas: &Cas,
    campaign: &str,
    round: &str,
    node: &str,
    attempt: &str,
) -> Result<(), StoreError> {
    use review_core::task::review_compat::*;
    use rusqlite::{OptionalExtension, params};
    let selection:Option<String> = connection.query_row(
        "SELECT payload FROM events WHERE run_id=?1 AND causation_id=?2 AND node_id=?3 AND attempt_id=?4 AND type='TaskReviewResultSelected@1'",
        params![campaign,round,node,attempt], |row|row.get(0),
    ).optional()?;
    let Some(selection) = selection else {
        return Ok(());
    };
    let selected: TaskReviewResultSelectedV1 = serde_json::from_str(&selection)?;
    selected.validate().map_err(conflict)?;
    let plan: ExecutionPlanV1 = payload(cas, &selected.plan_id, task::EXECUTION_PLAN_V1)?;
    let graph: CompiledTask = payload(cas, &plan.compiled_graph_id, "af/CompiledTask@1")?;
    if graph.nodes.contains_key(&selected.task_node) {
        return Ok(());
    }
    let context: TaskReviewContextV1 = payload(cas, &selected.context_id, TASK_REVIEW_CONTEXT_V1)?;
    context.validate().map_err(conflict)?;
    if context.campaign_id != campaign
        || context.round_event_id != round
        || context.review_node != node
        || context.attempt_id != attempt
        || context.task_invocation_id != selected.invocation_id
    {
        return Err(conflict(
            "Owned canonical receipt changed its exact Task and Review association",
        ));
    }
    // The protected execution transition records its output and child-set IDs as exact
    // references. Filter those indexed run rows before decoding; unrelated Attempt history
    // and renewal payloads are not part of this ownership-seal check. General Task replay
    // continues to validate that complete history separately.
    let run_id = task_run_id(&selected.task_id)?;
    let mut published_set = None;
    for row in receipt_records_referencing(connection, &run_id, &selected.output_id)? {
        if let TaskExecutionRecordV1::OwnedChildPublished {
            child_set_id,
            output_id,
            attempt_id,
        } = read_execution_record(cas, &row)?.record
            && output_id == selected.output_id
            && attempt_id == attempt
        {
            if published_set.as_ref().is_some_and(|id| id != &child_set_id) {
                return Err(conflict(
                    "Canonical child receipt has ambiguous ownership publication",
                ));
            }
            published_set = Some(child_set_id);
        }
    }
    let child_set_id = published_set.ok_or_else(|| {
        conflict("Canonical child receipt has no exact protected ownership publication")
    })?;
    let set = read_task_owned_children(cas, &child_set_id)?;
    if set.plan_id != selected.plan_id
        || !set.children.iter().any(|child| {
            child.node == selected.task_node && child.invocation_id == selected.invocation_id
        })
    {
        return Err(conflict(
            "Canonical child receipt changed its registered ownership",
        ));
    }
    let mut registered = false;
    let mut published = false;
    for record_id in receipt_records_referencing(connection, &run_id, &child_set_id)? {
        match read_execution_record(cas, &record_id)?.record {
            TaskExecutionRecordV1::OwnedChildrenRegistered { child_set_id: id }
                if id == child_set_id =>
            {
                registered = true
            }
            TaskExecutionRecordV1::OwnedChildPublished {
                child_set_id: id,
                output_id,
                attempt_id,
            } if registered
                && id == child_set_id
                && output_id == selected.output_id
                && attempt_id == attempt =>
            {
                published = true;
            }
            TaskExecutionRecordV1::OwnedChildrenCompleted {
                child_set_id: id, ..
            } if registered && id == child_set_id => {
                return Err(conflict(
                    "Owned parent sealed before canonical child receipt publication",
                ));
            }
            _ => {}
        }
    }
    if !registered || !published {
        return Err(conflict(
            "Canonical child receipt has no exact protected ownership publication",
        ));
    }
    Ok(())
}

pub(in crate::store::task) fn receipt_records_referencing(
    connection: &rusqlite::Connection,
    run_id: &str,
    artifact_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut query = connection.prepare(
        "SELECT payload,artifact_refs FROM events
         WHERE run_id=?1 AND type='TaskTransition@1' AND instr(artifact_refs,?2)>0
         AND EXISTS (SELECT 1 FROM json_each(events.artifact_refs)
                     WHERE json_each.type='text' AND json_each.value=?2)
         ORDER BY sequence",
    )?;
    let rows = query.query_map(rusqlite::params![run_id, artifact_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (payload, refs) = row?;
        let refs: Vec<String> = serde_json::from_str(&refs)?;
        if !refs.iter().any(|id| id == artifact_id) {
            return Err(conflict("Owned receipt reference query changed identity"));
        }
        let transition: TaskTransitionV1 = serde_json::from_str(&payload)?;
        transition.validate().map_err(conflict)?;
        if let TaskChangeV1::ExecutionRecorded { record_id } = transition.change {
            if !refs.contains(&record_id) {
                return Err(conflict(
                    "Owned receipt transition omitted its execution record",
                ));
            }
            records.push(record_id);
        }
    }
    Ok(records)
}
