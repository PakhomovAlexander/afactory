//! The Ledger node: the canonical reduction of this Round's reviewer outputs into the exact
//! Finding Set and Demand Set every downstream consumer names by artifact ID.

use std::collections::{BTreeMap, BTreeSet};

use review_core::{
    EventType, LegacyStageOutput, Producer, RecordedSetPayloadV1, ReviewerResultContract,
    ShardOutcomeV1, ShardSetV1, SliceSetV1,
};
use review_graph::{ArtifactMap, Node, NodeKind};
use review_store::{Ingest, NewEvent};

use crate::kernel::Kernel;
use crate::kernel::reviewer::reviewer_stage_output;
use crate::{is_demand_set_port, is_generation_finding_set_output};

impl Kernel<'_> {
    pub(crate) fn run_ledger(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<ArtifactMap, String> {
        // The ledger reduces what its edges delivered — never a global map of whatever happened
        // to run. Each input is one reviewer's result, or a gather manifest of result ids.
        let canonical = self.authority.finding_identity_policy
            == review_core::CANONICAL_FINDING_IDENTITY_POLICY;
        let mut results: Vec<(String, String, ReviewerResultContract, LegacyStageOutput)> =
            Vec::new();
        let mut dynamic_sets: Vec<(String, String, SliceSetV1, ShardSetV1)> = Vec::new();
        let mut direct_sources_used = BTreeSet::new();
        let mut load = |node: &str, id: &str, value: serde_json::Value| -> Result<(), String> {
            let (contract, output) =
                reviewer_stage_output(value).map_err(|error| format!("artifact {id}: {error}"))?;
            results.push((node.to_string(), id.to_string(), contract, output));
            Ok(())
        };
        for (input_port, artifacts) in inputs {
            for input in artifacts {
                let value = self.cas.get_json(input).map_err(|e| e.to_string())?;
                if value.get("type").and_then(serde_json::Value::as_str)
                    == Some(review_core::contract::SHARD_SET_V1)
                {
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value)
                        .map_err(|error| format!("ShardSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&envelope)?;
                    if envelope.subject_snapshot_id.as_deref()
                        != Some(self.authority.head_snapshot_id.as_str())
                    {
                        return Err(
                            "Ledger received a ShardSet outside current Subject authority".into(),
                        );
                    }
                    let scatter_node = match &envelope.producer {
                        Producer::KernelOperation {
                            node_id: Some(node),
                            ..
                        } => node.clone(),
                        _ => return Err("ShardSet@1 was not produced by its static Scatter".into()),
                    };
                    let shard_set: ShardSetV1 = serde_json::from_value(envelope.payload)
                        .map_err(|error| format!("ShardSet@1 payload is malformed: {error}"))?;
                    let slice_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(&shard_set.slice_set_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| format!("SliceSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&slice_envelope)?;
                    if slice_envelope.artifact_type != review_core::contract::SLICE_SET_V1 {
                        return Err("ShardSet@1 references a non-SliceSet artifact".into());
                    }
                    let slice_set: SliceSetV1 = serde_json::from_value(slice_envelope.payload)
                        .map_err(|error| format!("SliceSet@1 payload is malformed: {error}"))?;
                    shard_set.validate_against(&slice_set)?;
                    if slice_set.all_shards_required && !shard_set.complete() {
                        let failures = shard_set
                            .shards
                            .iter()
                            .filter_map(|shard| match &shard.outcome {
                                ShardOutcomeV1::Failed { reason }
                                | ShardOutcomeV1::Missing { reason } => {
                                    Some(format!("{}: {reason}", shard.runtime_node_id))
                                }
                                ShardOutcomeV1::Completed { .. } => None,
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        return Err(format!(
                            "Scatter `{scatter_node}` has a failed or missing required shard: {failures}"
                        ));
                    }
                    for shard in &shard_set.shards {
                        if let ShardOutcomeV1::Completed {
                            result_artifact_ids,
                        } = &shard.outcome
                        {
                            for result_id in result_artifact_ids {
                                let result = self
                                    .cas
                                    .get_json(result_id)
                                    .map_err(|error| error.to_string())?;
                                load(&shard.runtime_node_id, result_id, result)?;
                            }
                        }
                    }
                    dynamic_sets.push((scatter_node, input.clone(), slice_set, shard_set));
                    continue;
                }
                if value.get("verdict").is_some() && value.get("reports").is_some() {
                    let source = if canonical {
                        let upstream = self
                            .input_bindings
                            .get(&node.id)
                            .and_then(|ports| ports.get(input_port))
                            .ok_or_else(|| {
                                format!(
                                    "canonical ledger `{}.{input_port}` has no pinned input graph",
                                    node.id
                                )
                            })?;
                        let selections = self
                            .reviewer_selections
                            .lock()
                            .expect("reviewer selections");
                        let source = upstream.iter().find(|(source, _, kind)| {
                            *kind == NodeKind::Reviewer
                                && !direct_sources_used.contains(source)
                                && selections
                                    .get(source)
                                    .is_some_and(|selection| selection.result_artifact == *input)
                        });
                        source.map(|(source, _, _)| source.clone()).ok_or_else(|| {
                            format!(
                                "canonical ledger `{}.{input_port}` cannot bind delivered result {input} to its pinned upstream reviewers",
                                node.id
                            )
                        })?
                    } else {
                        input_port.clone()
                    };
                    direct_sources_used.insert(source.clone());
                    load(&source, input, value)?;
                    continue;
                }
                match value {
                    serde_json::Value::Object(manifest) => {
                        for (node, ids) in manifest {
                            let ids = ids.as_array().ok_or_else(|| {
                                format!("gather manifest {input} has a non-array port")
                            })?;
                            for id in ids {
                                let id = id.as_str().ok_or_else(|| {
                                    format!("gather manifest {input} holds a non-id")
                                })?;
                                let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                                load(&node, id, value)?;
                            }
                        }
                    }
                    // Compatibility for gather manifests emitted before source-labelled maps.
                    serde_json::Value::Array(ids) => {
                        for id in &ids {
                            let id = id
                                .as_str()
                                .ok_or_else(|| format!("gather manifest {input} holds a non-id"))?;
                            if canonical {
                                let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                                load(input_port, id, value)?;
                                continue;
                            }
                            let selected: Vec<String> = self
                                .reviewer_selections
                                .lock()
                                .expect("reviewer selections")
                                .iter()
                                .filter(|(_, selection)| selection.result_artifact == id)
                                .map(|(node, _)| node.clone())
                                .collect();
                            if selected.len() != 1 {
                                return Err(format!(
                                    "legacy gather manifest {input} cannot uniquely identify artifact {id}"
                                ));
                            }
                            let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                            load(&selected[0], id, value)?;
                        }
                    }
                    _ => {
                        return Err(format!(
                            "artifact {input} is neither a supported ReviewerResult nor a gather manifest"
                        ));
                    }
                }
            }
        }
        if canonical {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let mut result_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, (_, result_id, _, _)) in results.iter().enumerate() {
                result_indices
                    .entry(result_id.clone())
                    .or_default()
                    .push(index);
            }
            for (result_id, indices) in result_indices {
                let mut assigned = BTreeSet::new();
                let mut unmatched = Vec::new();
                for index in indices {
                    let node = &results[index].0;
                    if selections
                        .get(node)
                        .is_some_and(|selection| selection.result_artifact == result_id)
                    {
                        if !assigned.insert(node.clone()) {
                            return Err(format!(
                                "selected reviewer result for `{node}` was delivered more than once"
                            ));
                        }
                    } else {
                        unmatched.push(index);
                    }
                }
                let mut selected: Vec<_> = selections
                    .iter()
                    .filter(|(node, selection)| {
                        selection.result_artifact == result_id && !assigned.contains(*node)
                    })
                    .map(|(node, _)| node.clone())
                    .collect();
                if selected.len() < unmatched.len() {
                    return Err(format!(
                        "selected reviewer result {result_id} has {} unlabelled delivered copies and {} unused matching Attempts",
                        unmatched.len(),
                        selected.len(),
                    ));
                }
                selected.sort();
                unmatched.sort_by(|left, right| {
                    (&results[*left].0, *left).cmp(&(&results[*right].0, *right))
                });
                for (index, node) in unmatched.into_iter().zip(selected) {
                    results[index].0 = node;
                }
            }
        }
        // Canonical gather order: reviewer node id — not completion order, input-port label, or
        // artifact digest order. Legacy campaigns retain their frozen port-labelled projection.
        results.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

        let canonical_metadata = if canonical {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let reviewer_inputs = self
                .reviewer_input_artifacts
                .lock()
                .expect("reviewer inputs");
            Some(
                results
                    .iter()
                    .map(|(node, result_id, _, _)| {
                        let selection = selections.get(node).ok_or_else(|| {
                            format!("selected reviewer result for `{node}` has no Attempt")
                        })?;
                        if &selection.result_artifact != result_id {
                            return Err(format!(
                                "selected reviewer result for `{node}` disagrees with its Attempt"
                            ));
                        }
                        Ok((
                            selection.attempt_id.clone(),
                            reviewer_inputs.get(node).cloned().ok_or_else(|| {
                                format!("selected reviewer `{node}` has no exact invocation inputs")
                            })?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            )
        } else {
            None
        };

        let projection = self.take_ledger_projection();
        let (
            round,
            finding_count,
            finding_entries,
            grouping_relation_ids,
            grouping_input_artifact_ids,
            resolution_ids,
            resolution_input_artifact_ids,
            demand_entries,
            demand_artifact_ids,
            demand_input_artifact_ids,
            reduction,
            mut projection,
        ) = {
            let mut store = self.store.lock().expect("event store");
            let mut ingest =
                Ingest::from_projection(*store, self.cas, self.run_id.clone(), projection)
                    .map_err(|e| e.to_string())?
                    .under_round(&self.authority.round_event_id);
            let reduction_round =
                canonical_reduction_round(ingest.ledger().round, self.authority.round)?;
            let reduction = match &canonical_metadata {
                Some(metadata) => {
                    let stages: Vec<_> = results
                        .iter()
                        .zip(metadata)
                        .map(
                            |(
                                (node, result_id, result_contract, stage),
                                (attempt_id, input_artifacts),
                            )| {
                                let binding_node = self.reviewer_binding_node(node);
                                review_store::CanonicalStage {
                                    source: node,
                                    demand_requirement: self
                                        .demand_requirements
                                        .get(&binding_node)
                                        .copied()
                                        .unwrap_or(review_core::DemandRequirement::Required),
                                    stage,
                                    attempt_id,
                                    result_artifact_id: result_id,
                                    input_artifacts,
                                    subject_snapshot_id: &self.authority.head_snapshot_id,
                                    subject_id: &self.authority.subject_id,
                                    result_contract: *result_contract,
                                }
                            },
                        )
                        .collect();
                    Some(
                        ingest
                            .add_canonical_stage_outputs(&stages)
                            .map_err(|error| error.to_string())?,
                    )
                }
                None => {
                    if results
                        .iter()
                        .any(|(_, _, contract, _)| *contract == ReviewerResultContract::V2)
                    {
                        return Err(
                            "ReviewerResult@2 requires canonical Finding identity authority".into(),
                        );
                    }
                    let stages: Vec<_> = results
                        .iter()
                        .map(|(node, _, _, stage)| (node.as_str(), stage))
                        .collect();
                    ingest
                        .add_live_stage_outputs(&stages)
                        .map_err(|error| error.to_string())?;
                    None
                }
            };
            (
                reduction_round,
                ingest.ledger().finding_views().len(),
                canonical.then(|| finding_set_entries(ingest.ledger())),
                ingest.ledger().grouping_relation_ids(),
                ingest.ledger().grouping_input_artifact_ids(),
                ingest.ledger().resolution_artifact_ids(),
                ingest.ledger().resolution_input_artifact_ids(),
                canonical.then(|| ingest.ledger().demand_views()),
                ingest.ledger().demand_reduction_artifact_ids(),
                ingest.ledger().demand_reduction_input_ids(),
                reduction,
                ingest.into_projection(),
            )
        };
        let proposal_ids = if let Some(reduction) = &reduction {
            let proposals = self.finalize_proposals(reduction)?;
            projection
                .fast_forward(*self.store.lock().expect("event store"), self.cas)
                .map_err(|error| error.to_string())?;
            proposals
        } else {
            Vec::new()
        };
        let semantic_reduction_outputs = reduction.as_ref().map(|reduction| {
            (
                reduction.selected_report_ids.clone(),
                reduction.relation_ids.clone(),
                reduction.selected_demand_artifact_ids.clone(),
            )
        });
        let semantic_grouping_ids = grouping_relation_ids.clone();
        let semantic_resolution_ids = resolution_ids.clone();
        let semantic_demand_lifecycle_ids = demand_artifact_ids.clone();
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        let findings_artifact = if let (Some(entries), Some(reduction)) =
            (finding_entries, reduction)
        {
            let reducer_version = reduction.reducer_version;
            let payload = review_core::FindingSetV1 {
                subject_id: self.authority.subject_id.clone(),
                round,
                prior_finding_set_id: self.authority.prior_reduction_finding_set_id.clone(),
                reducer_version: reducer_version.to_string(),
                identity_policy: self.authority.finding_identity_policy.clone(),
                selected_report_ids: reduction.selected_report_ids,
                relation_ids: reduction
                    .relation_ids
                    .into_iter()
                    .chain(grouping_relation_ids)
                    .collect(),
                resolution_ids,
                findings: entries,
            };
            payload.validate()?;
            let mut reduction_inputs = vec![self.authority.prior_reduction_finding_set_id.clone()];
            reduction_inputs.extend(reduction.input_artifact_ids);
            reduction_inputs.extend(grouping_input_artifact_ids);
            reduction_inputs.extend(resolution_input_artifact_ids);
            let operation_digest = review_store::content_id(&serde_json::json!({
                "reducer_version": reducer_version,
                "identity_policy": self.authority.finding_identity_policy,
                "inputs": reduction_inputs,
            }))
            .map_err(|error| error.to_string())?;
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::FINDING_SET_V1,
                    review_core::Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some("ledger".into()),
                        operation_id: format!(
                            "{}:{}:{}",
                            reducer_version,
                            self.authority.finding_identity_policy,
                            operation_digest
                        ),
                    },
                    reduction_inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            record_id
        } else {
            // The `findings` port must carry a real artifact, not a label: the scheduler delivers
            // exactly this string to whatever consumes the port, and a downstream event referencing
            // a non-CAS string would be rejected as a dangling artifact far from its cause.
            self.cas
                .put_json(&serde_json::json!({
                    "round": round,
                    "sources": results.iter().map(|(node, _, _, _)| node).collect::<Vec<_>>(),
                    "findings": finding_count,
                }))
                .map_err(|e| e.to_string())?
        };

        let finding_port = node
            .outputs
            .iter()
            .find(|port| is_generation_finding_set_output(port))
            .or_else(|| (node.outputs.len() == 1).then(|| &node.outputs[0]))
            .ok_or_else(|| "ledger node has no Finding Set output".to_string())?;
        let mut outputs = ArtifactMap::from([(finding_port.name.clone(), vec![findings_artifact])]);

        if let Some(demands) = demand_entries {
            if round == 1 && self.authority.prior_demand_set_id != self.authority.demand_genesis_id
            {
                return Err("Round 1 Demand Set does not descend from Campaign genesis".into());
            }
            let (selected_demand_artifact_ids, satisfaction_artifact_ids, waiver_artifact_ids) =
                demand_artifact_ids;
            let payload = review_core::DemandSetV1 {
                subject_id: self.authority.subject_id.clone(),
                round,
                prior_demand_set_id: self.authority.prior_demand_set_id.clone(),
                reducer_version: review_core::DEMAND_REDUCER_VERSION.into(),
                selected_demand_artifact_ids,
                satisfaction_artifact_ids,
                waiver_artifact_ids,
                demands,
            };
            payload.validate()?;
            let mut reduction_inputs = vec![self.authority.prior_demand_set_id.clone()];
            reduction_inputs.extend(demand_input_artifact_ids);
            let mut unique = BTreeSet::new();
            reduction_inputs.retain(|id| unique.insert(id.clone()));
            let operation_digest = review_store::content_id(&serde_json::json!({
                "reducer_version": review_core::DEMAND_REDUCER_VERSION,
                "inputs": reduction_inputs,
            }))
            .map_err(|error| error.to_string())?;
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::DEMAND_SET_V1,
                    review_core::Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some("ledger".into()),
                        operation_id: format!(
                            "{}:{}",
                            review_core::DEMAND_REDUCER_VERSION,
                            operation_digest
                        ),
                    },
                    reduction_inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            match node.outputs.iter().find(|port| is_demand_set_port(port)) {
                Some(port) => {
                    outputs.insert(port.name.clone(), vec![record_id]);
                }
                None if !outputs.is_empty()
                    && self
                        .ledger_cache
                        .lock()
                        .expect("ledger cache")
                        .as_ref()
                        .is_some_and(|projection| {
                            !projection.ledger().demand_views().is_empty()
                        }) =>
                {
                    return Err(
                        "ledger selected Demands but declares no review.kernel/DemandSet@1 output"
                            .into(),
                    );
                }
                None => {}
            }
        }
        if !dynamic_sets.is_empty() {
            let delivered_results = results
                .iter()
                .map(|(source, artifact, _, _)| (source.clone(), artifact.clone()))
                .collect::<BTreeSet<_>>();
            let round_events = self
                .store
                .lock()
                .expect("event store")
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|event| {
                    event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
                })
                .collect::<Vec<_>>();
            for (scatter_node, shard_record, slice_set, shard_set) in dynamic_sets {
                let mut selected = Vec::new();
                for shard in &shard_set.shards {
                    if let ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } = &shard.outcome
                    {
                        for artifact in result_artifact_ids {
                            if !delivered_results
                                .contains(&(shard.runtime_node_id.clone(), artifact.clone()))
                            {
                                return Err(format!(
                                    "dynamic result `{artifact}` from `{}` did not reach Ledger",
                                    shard.runtime_node_id
                                ));
                            }
                            selected.push(artifact.clone());
                        }
                    }
                }
                let selected_outputs = BTreeMap::from([("reviewer_results".into(), selected)]);
                let mut sinks = selected_outputs
                    .values()
                    .flatten()
                    .map(|artifact| (artifact.clone(), "ledger:reviewer-result".into()))
                    .collect::<BTreeMap<String, String>>();
                let closeout_result_id = match &slice_set.closeout {
                    review_core::CloseoutPolicyV1::Required => {
                        let closeout = self.closeouts.get(&scatter_node).ok_or_else(|| {
                            format!("Scatter `{scatter_node}` has no captured closeout reviewer")
                        })?;
                        let ids = results
                            .iter()
                            .filter(|(source, _, _, _)| source == closeout)
                            .map(|(_, artifact, _, _)| artifact.clone())
                            .collect::<Vec<_>>();
                        let [artifact] = ids.as_slice() else {
                            return Err(format!(
                                "closeout reviewer `{closeout}` did not deliver exactly one result"
                            ));
                        };
                        sinks.insert(artifact.clone(), "ledger:whole-subject-closeout".into());
                        Some(artifact.clone())
                    }
                    review_core::CloseoutPolicyV1::Waived { policy_id, .. } => {
                        if !self.authority.policy_ids.contains(policy_id) {
                            return Err(format!(
                                "closeout waiver `{policy_id}` is absent from Authority Snapshot"
                            ));
                        }
                        None
                    }
                };
                let mut closure = crate::scatter::prove_semantic_closure(
                    &slice_set,
                    &shard_set,
                    &selected_outputs,
                    &sinks,
                    closeout_result_id.clone(),
                )?;
                let mut semantic_sinks = closure
                    .dispositions
                    .iter()
                    .map(|disposition| (disposition.artifact_id.clone(), disposition.sink.clone()))
                    .collect::<BTreeMap<_, _>>();
                if let Some(closeout) = closeout_result_id {
                    semantic_sinks.insert(closeout, "ledger:whole-subject-closeout".into());
                }
                if let Some((reports, relations, demands)) = &semantic_reduction_outputs {
                    for artifact in reports {
                        semantic_sinks.insert(artifact.clone(), "ledger:finding-set".into());
                    }
                    for artifact in relations {
                        semantic_sinks.insert(artifact.clone(), "ledger:relation".into());
                    }
                    for artifact in demands {
                        semantic_sinks.insert(artifact.clone(), "ledger:demand-set".into());
                    }
                }
                for artifact in &semantic_grouping_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:grouping".into());
                }
                for artifact in &semantic_resolution_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:resolution".into());
                }
                for artifact in semantic_demand_lifecycle_ids
                    .0
                    .iter()
                    .chain(&semantic_demand_lifecycle_ids.1)
                    .chain(&semantic_demand_lifecycle_ids.2)
                {
                    semantic_sinks.insert(artifact.clone(), "ledger:demand-lifecycle".into());
                }
                for proposal_id in &proposal_ids {
                    semantic_sinks.insert(proposal_id.clone(), "proposal-store".into());
                    let proposal_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(proposal_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                    let proposal: review_core::PatchProposal =
                        serde_json::from_value(proposal_envelope.payload)
                            .map_err(|error| error.to_string())?;
                    for evidence in proposal.evidence_ids {
                        semantic_sinks.insert(evidence, "proposal-store:evidence".into());
                    }
                }
                for event in &round_events {
                    let sink = match event.event_type {
                        EventType::CheckCompletedV1 => Some("event-log:check"),
                        EventType::GateDecisionV1 => Some("event-log:policy"),
                        EventType::EvidenceAddedV1 => Some("ledger:evidence"),
                        _ => None,
                    };
                    if let Some(sink) = sink {
                        let artifact = self
                            .cas
                            .put_json(&event.payload)
                            .map_err(|error| error.to_string())?;
                        semantic_sinks.insert(artifact, sink.into());
                    }
                }
                closure.required_artifact_ids = semantic_sinks.keys().cloned().collect();
                closure.dispositions = semantic_sinks
                    .into_iter()
                    .map(|(artifact_id, sink)| review_core::SemanticDispositionV1 {
                        artifact_id,
                        sink,
                    })
                    .collect();
                closure.validate()?;
                let mut closure_inputs = vec![shard_record.clone(), shard_set.slice_set_id.clone()];
                closure_inputs.extend(closure.required_artifact_ids.iter().cloned());
                closure_inputs.sort();
                closure_inputs.dedup();
                for artifact in &closure_inputs {
                    self.cas.verify(artifact).map_err(|error| {
                        format!("semantic closure input `{artifact}` is not durable: {error}")
                    })?;
                }
                let operation_id = review_store::content_id(
                    &serde_json::to_value(&closure).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let (record_id, _) = self
                    .cas
                    .put_artifact(
                        review_core::contract::SEMANTIC_CLOSURE_V1,
                        Producer::KernelOperation {
                            run_id: self.run_id.clone(),
                            node_id: Some(node.id.clone()),
                            operation_id,
                        },
                        closure_inputs,
                        Some(self.authority.head_snapshot_id.clone()),
                        serde_json::to_value(closure).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::SemanticClosureCheckedV1,
                        serde_json::to_value(RecordedSetPayloadV1 {
                            artifact_id: record_id.clone(),
                            record_id: record_id.clone(),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .node(&node.id)
                    .referencing(vec![record_id]),
                )?;
            }
        }
        Ok(outputs)
    }
}

fn canonical_reduction_round(ledger_round: u32, authority_round: u32) -> Result<u32, String> {
    if ledger_round != authority_round {
        return Err(format!(
            "canonical ledger is at Round {ledger_round} but active Round authority is {authority_round}"
        ));
    }
    Ok(authority_round)
}

fn finding_set_entries(ledger: &review_store::Ledger) -> Vec<review_core::FindingSetEntryV1> {
    ledger
        .finding_views()
        .iter()
        .map(|finding| {
            let (file, line, location_unrecorded) =
                if finding.identity_file == review_core::legacy::CHANGE_WIDE_SENTINEL {
                    (None, None, false)
                } else if review_core::is_valid_repo_path(&finding.identity_file) {
                    (
                        Some(finding.identity_file.clone()),
                        finding.identity_line,
                        false,
                    )
                } else {
                    (None, None, true)
                };
            review_core::FindingSetEntryV1 {
                finding_id: finding.key.clone(),
                status: finding.status.as_str().to_string(),
                severity: finding.severity,
                effective_severity: finding.convergence_severity,
                scope: finding.convergence_scope_label().to_string(),
                file,
                line,
                location_unrecorded,
                title: finding.title.clone(),
                body: finding.body.clone(),
                fix: finding.fix.clone(),
                confidence: finding.confidence,
                source: finding.source.clone(),
                last_seen_round: finding.last_seen_round,
                report_ids: finding
                    .reports
                    .iter()
                    .map(|report| {
                        report
                            .artifact_id
                            .clone()
                            .unwrap_or_else(|| report.report_id.clone())
                    })
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_store::{Cas, Ledger};

    #[test]
    fn canonical_reduction_refuses_a_stale_ledger_round() {
        assert_eq!(canonical_reduction_round(2, 2).unwrap(), 2);
        assert_eq!(
            canonical_reduction_round(1, 2).unwrap_err(),
            "canonical ledger is at Round 1 but active Round authority is 2"
        );
    }

    #[test]
    fn unreadable_report_locations_are_omitted_from_finding_sets() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let report_id = cas.put(b"not a report").unwrap();
        let mut ledger = Ledger::default();
        ledger
            .apply_event(
                &review_core::RunEvent {
                    event_id: "finding".into(),
                    run_id: "run".into(),
                    sequence: 0,
                    event_type: EventType::FindingReportedV1,
                    occurred_at: "2026-08-26T00:00:00Z".into(),
                    node_id: None,
                    attempt_id: None,
                    causation_id: None,
                    correlation_id: Some("claim".into()),
                    artifact_refs: vec![report_id.clone()],
                    payload: serde_json::json!({
                        "key": "claim",
                        "round": 1,
                        "source": "correctness",
                        "report_id": report_id,
                    }),
                },
                &cas,
            )
            .unwrap();

        let entries = finding_set_entries(&ledger);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file, None);
        assert!(entries[0].location_unrecorded);
    }
}
