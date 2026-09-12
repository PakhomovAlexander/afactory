//! Publish the already sealed Review fold under both original log prefixes.
use super::*;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};

impl EventStore {
    /// A canonical ShardSet projects an exact sealed parent output. This cannot dispatch work
    /// or refold the set after late usage; current lease, plan decision and Round still apply.
    pub fn publish_task_owned_review_shards(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        children: &RegisteredTaskChildren,
        output_id: &str,
        authority: &dyn TaskAuthority,
    ) -> Result<(), StoreError> {
        let (state, plan) = self.checked_task_recording(cas, lease, authority)?;
        let (campaign, event) = sealed_review_shards(cas, &state, children, output_id)?;
        if let Some(recorded) = self.replay(&campaign)?.into_iter().find(|recorded| {
            recorded.event_type == event.event_type
                && recorded.node_id == event.node_id
                && recorded.causation_id == event.causation_id
        }) {
            return if recorded.payload == event.payload
                && recorded.artifact_refs == event.artifact_refs
                && recorded.correlation_id == event.correlation_id
                && recorded.attempt_id == event.attempt_id
            {
                Ok(())
            } else {
                Err(conflict(
                    "Owned ShardSet changed its canonical recorded output",
                ))
            };
        }
        let permit = review::WritePermit::for_checked_recording(
            &state,
            &plan,
            &campaign,
            self.len(&campaign)?,
            event.clone(),
        )?;
        self.append_batch_inner(&campaign, cas, &[event], None, Some(&permit))?;
        Ok(())
    }
}

fn sealed_review_shards(
    cas: &Cas,
    state: &TaskProjection,
    children: &RegisteredTaskChildren,
    output_id: &str,
) -> Result<(String, NewEvent), StoreError> {
    if children.task_id != state.task_id {
        return Err(conflict(
            "Owned ShardSet capability belongs to another Task",
        ));
    }
    let execution = state
        .execution
        .as_ref()
        .ok_or_else(|| conflict("Task has no execution"))?;
    let set = execution.checked_set(children)?;
    let output = output(cas, output_id)?;
    if set.completed.as_deref() != Some(output_id)
        || execution.outputs.get(&set.parent_node) != Some(&(output_id.into(), output.clone()))
        || output.invocation_id != set.set.parent_invocation_id
    {
        return Err(conflict(
            "Owned ShardSet lacks its exact sealed parent output",
        ));
    }
    let definition = execution
        .graph
        .nodes
        .get(&set.parent_node)
        .ok_or_else(|| conflict("Owned ShardSet lost its captured parent"))?;
    let CompiledOperator::ReviewDomain {
        review_node,
        operation: ReviewOperation::Scatter { .. },
    } = &definition.operator
    else {
        return Err(conflict(
            "Owned ShardSet parent is not captured Review Scatter",
        ));
    };
    let input = state
        .revision
        .inputs
        .values()
        .find(|input| input.artifact_type == LEGACY_REVIEW_ROUND_V1)
        .ok_or_else(|| conflict("Owned ShardSet lacks a captured Review Round"))?;
    let [round_id] = input.artifact_ids.as_slice() else {
        return Err(conflict(
            "Owned ShardSet has ambiguous Review Round authority",
        ));
    };
    let round: LegacyReviewRoundV1 = payload(cas, round_id, LEGACY_REVIEW_ROUND_V1)?;
    round.validate().map_err(conflict)?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&round.campaign_manifest_id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?,
    )?;
    let port = output
        .outputs
        .get("o0")
        .ok_or_else(|| conflict("Owned Review output lacks ShardSet"))?;
    let [id] = port.artifact_ids.as_slice() else {
        return Err(conflict("Owned Review output has ambiguous ShardSet"));
    };
    if output.outputs.len() != 1
        || port.artifact_type != review_core::contract::SHARD_SET_V1
        || port.cardinality != review_core::PortCardinality::One
        || port.snapshot_id.as_ref() != Some(&round.head_snapshot_id)
    {
        return Err(conflict(
            "Owned ShardSet changed its canonical output contract",
        ));
    }
    let artifact = envelope(cas, id, review_core::contract::SHARD_SET_V1)?;
    let shards: review_core::ShardSetV1 = serde_json::from_value(artifact.payload.clone())?;
    let slices: review_core::SliceSetV1 = payload(
        cas,
        &set.set.source_artifact_id,
        review_core::contract::SLICE_SET_V1,
    )?;
    shards.validate_against(&slices).map_err(conflict)?;
    let mut inputs = vec![shards.slice_set_id.clone()];
    inputs.extend(shards.shards.iter().flat_map(|shard| match &shard.outcome {
        review_core::ShardOutcomeV1::Completed {
            result_artifact_ids,
        } => result_artifact_ids.clone(),
        _ => vec![],
    }));
    if shards.slice_set_id != set.set.source_artifact_id
        || shards.subject_id != round.subject_id
        || artifact.subject_snapshot_id.as_ref() != Some(&round.head_snapshot_id)
        || artifact.input_artifacts != inputs
        || artifact.producer
            != (review_core::Producer::KernelOperation {
                run_id: round.campaign_id.clone(),
                node_id: Some(review_node.clone()),
                operation_id: crate::content_id(&artifact.payload)
                    .map_err(|e| conflict(e.to_string()))?,
            })
    {
        return Err(conflict(
            "Owned ShardSet changed its canonical source or producer",
        ));
    }
    for id in &inputs {
        cas.verify(id)
            .map_err(|e| StoreError::Artifact(e.to_string()))?;
    }
    let mut refs = vec![id.clone(), set.set.source_artifact_id.clone()];
    for id in [
        manifest.authority_snapshot_id,
        round.campaign_manifest_id,
        round.subject_id.clone(),
        round.head_snapshot_id,
    ] {
        if !refs.contains(&id) {
            refs.push(id);
        }
    }
    Ok((
        round.campaign_id,
        NewEvent::new(
            EventType::ShardSetRecordedV1,
            serde_json::to_value(review_core::RecordedSetPayloadV1 {
                artifact_id: id.clone(),
                record_id: id.clone(),
            })?,
        )
        .node(review_node)
        .caused_by(round.round_event_id)
        .correlating(round.subject_id)
        .referencing(refs),
    ))
}
