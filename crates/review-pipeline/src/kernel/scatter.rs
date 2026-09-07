//! Dynamic fan-out: the Slicer, the typed Scatter that owns its shards, and the Gather that
//! closes out every shard's outcome for the whole Subject (ADR-0039). The slice policy and the
//! closure arithmetic live in [`crate::scatter`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use review_core::{
    EventType, Producer, RecordedSetPayloadV1, ShardOutcomeV1, ShardReceiptV1, ShardSetV1,
    SliceSetAcceptedPayloadV1, SliceSetV1,
};
use review_graph::{ArtifactMap, Dispatch, Node, NodeKind, PortContract};
use review_store::NewEvent;

use crate::artifact_ids;
use crate::kernel::Kernel;

impl Kernel<'_> {
    pub(crate) fn run_slicer(&self, node: &Node) -> Result<Vec<String>, String> {
        let policy = self
            .slicing
            .get(&node.id)
            .ok_or_else(|| format!("Slicer `{}` has no captured slicing policy", node.id))?;
        let subject_paths = match &self.authority.change_set {
            Some(change_set) => change_set.change_set().changed_paths.clone(),
            None => self
                .snapshot
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        let slice_set = policy.plan(
            &self.authority.subject_id,
            &subject_paths,
            &self.static_node_ids,
        )?;
        let operation_id = review_store::content_id(&serde_json::json!({
            "operation": "slice-set@1",
            "node": node.id,
            "subject": self.authority.subject_id,
            "policy": {
                "scatter": policy.scatter_node,
                "max_paths_per_slice": policy.max_paths_per_slice,
                "max_fanout": policy.max_fanout,
                "coverage": policy.coverage,
                "all_shards_required": policy.all_shards_required,
                "closeout": policy.closeout,
            }
        }))
        .map_err(|error| error.to_string())?;
        let mut inputs = vec![self.authority.subject_id.clone()];
        inputs.extend(self.authority.change_set_id.clone());
        let (record_id, envelope) = self
            .cas
            .put_artifact(
                review_core::contract::SLICE_SET_V1,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some(node.id.clone()),
                    operation_id,
                },
                inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(slice_set).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        // This event is deliberately appended inside the Slicer, before its graph receipt can
        // make the artifact visible to Scatter.
        self.append(
            NewEvent::new(
                EventType::SliceSetAcceptedV1,
                serde_json::to_value(SliceSetAcceptedPayloadV1 {
                    slice_set_id: envelope.artifact_id,
                    slice_set_artifact_id: record_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .node(&node.id)
            .referencing(vec![record_id.clone()]),
        )?;
        Ok(vec![record_id])
    }

    pub(crate) fn run_scatter(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        let slice_port = node
            .inputs
            .iter()
            .find(|port| port.artifact_type == review_core::contract::SLICE_SET_V1)
            .ok_or_else(|| format!("Scatter `{}` has no SliceSet@1 input", node.id))?;
        let [slice_set_record] = inputs
            .get(&slice_port.name)
            .map(Vec::as_slice)
            .unwrap_or_default()
        else {
            return Err(format!(
                "Scatter `{}` did not receive exactly one SliceSet@1",
                node.id
            ));
        };
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            self.cas
                .get_json(slice_set_record)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("SliceSet@1 envelope is malformed: {error}"))?;
        review_store::validate_envelope(&envelope)?;
        if envelope.artifact_type != review_core::contract::SLICE_SET_V1
            || envelope.subject_snapshot_id.as_deref()
                != Some(self.authority.head_snapshot_id.as_str())
        {
            return Err("Scatter received a SliceSet outside current Subject authority".into());
        }
        let slice_set: SliceSetV1 = serde_json::from_value(envelope.payload)
            .map_err(|error| format!("SliceSet@1 payload is malformed: {error}"))?;
        let subject_paths = match &self.authority.change_set {
            Some(change_set) => change_set.change_set().changed_paths.clone(),
            None => self
                .snapshot
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        slice_set.validate_coverage(&subject_paths)?;
        if !self
            .slicing
            .values()
            .any(|policy| policy.scatter_node == node.id)
        {
            return Err(format!(
                "Scatter `{}` has no accepted Slicer owner",
                node.id
            ));
        }

        let mut inherited_contracts = node
            .inputs
            .iter()
            .filter(|contract| contract.artifact_type != review_core::contract::SLICE_SET_V1)
            .cloned()
            .collect::<Vec<_>>();
        if inherited_contracts
            .iter()
            .any(|contract| contract.name == "slice")
        {
            return Err(format!(
                "Scatter `{}` reserves dynamic input port `slice`",
                node.id
            ));
        }
        inherited_contracts.push(PortContract::new(
            "slice",
            review_core::contract::REVIEW_SLICE_V1,
        ));
        let result_contract = if inherited_contracts
            .iter()
            .any(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
        {
            review_core::contract::REVIEWER_RESULT_V2
        } else {
            review_core::contract::REVIEWER_RESULT_V1
        };
        let inherited_inputs = inputs
            .iter()
            .filter(|(port, _)| *port != &slice_port.name)
            .map(|(port, artifacts)| (port.clone(), artifacts.clone()))
            .collect::<ArtifactMap>();

        let mut runnable = Vec::new();
        let mut outcomes: BTreeMap<String, ShardOutcomeV1> = BTreeMap::new();
        for slice in &slice_set.slices {
            self.dynamic_reviewer_bases
                .lock()
                .expect("dynamic reviewer bases")
                .insert(slice.runtime_node_id.clone(), node.id.clone());
            let (slice_record, _) = self
                .cas
                .put_artifact(
                    review_core::contract::REVIEW_SLICE_V1,
                    Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: Some(node.id.clone()),
                        operation_id: format!("slice:{}", slice.slice_id),
                    },
                    vec![slice_set_record.clone()],
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(slice).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            let dynamic_node = Node::new(&slice.runtime_node_id, NodeKind::Reviewer)
                .accepting_contracts(inherited_contracts.clone())
                .emitting_contracts(vec![PortContract::new("out", result_contract)]);
            let mut dynamic_inputs = inherited_inputs.clone();
            dynamic_inputs.insert("slice".into(), vec![slice_record]);
            // Invocation and reservation both up front, in canonical Slice order: a shard
            // the fan-out cap refuses is a durable Missing outcome before any shard runs.
            match self
                .record_invocation(&dynamic_node, &dynamic_inputs)
                .and_then(|()| self.prepare_dispatch(&dynamic_node, &dynamic_inputs))
            {
                Ok(()) => runnable.push((slice.clone(), dynamic_node, dynamic_inputs)),
                Err(error) => {
                    outcomes.insert(
                        slice.slice_id.clone(),
                        ShardOutcomeV1::Missing { reason: error },
                    );
                }
            }
        }

        // Dispatches above are durable in canonical Slice order. Shards are model calls that
        // block for minutes, so they run on their own bounded thread set — never on the shared
        // filesystem executor, whose width is the host's CPU count and whose workers the shards'
        // own sandbox clone, seal, and CAS phases need. The in-flight bound is the Slice
        // policy's `max_fanout`: the persisted SliceSet@1 carries it and validates
        // `slices.len() <= max_fanout`, so the fan-out the plan declares is the fan-out that
        // runs. Receipts are committed below in the same canonical order.
        let executed = dispatch_shards(
            runnable,
            slice_set.max_fanout,
            |(slice, dynamic_node, inputs)| {
                (
                    slice,
                    dynamic_node.clone(),
                    self.run(&dynamic_node, &inputs),
                )
            },
        );
        for (slice, dynamic_node, result) in executed {
            let outcome = match result {
                Ok(outputs) => match self.record_outputs(&dynamic_node, &outputs) {
                    Ok(()) => ShardOutcomeV1::Completed {
                        result_artifact_ids: artifact_ids(&outputs),
                    },
                    Err(error) => ShardOutcomeV1::Failed { reason: error },
                },
                Err(error) => ShardOutcomeV1::Failed { reason: error },
            };
            outcomes.insert(slice.slice_id, outcome);
        }
        let shard_set = ShardSetV1 {
            subject_id: slice_set.subject_id.clone(),
            slice_set_id: slice_set_record.clone(),
            all_shards_required: slice_set.all_shards_required,
            shards: slice_set
                .slices
                .iter()
                .map(|slice| ShardReceiptV1 {
                    slice_id: slice.slice_id.clone(),
                    runtime_node_id: slice.runtime_node_id.clone(),
                    outcome: outcomes.remove(&slice.slice_id).unwrap_or_else(|| {
                        ShardOutcomeV1::Missing {
                            reason: "dynamic shard produced no terminal outcome".into(),
                        }
                    }),
                })
                .collect(),
        };
        shard_set.validate_against(&slice_set)?;
        let mut artifact_inputs = vec![slice_set_record.clone()];
        artifact_inputs.extend(
            shard_set
                .shards
                .iter()
                .flat_map(|shard| match &shard.outcome {
                    ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } => result_artifact_ids.clone(),
                    ShardOutcomeV1::Failed { .. } | ShardOutcomeV1::Missing { .. } => vec![],
                }),
        );
        let operation_id = review_store::content_id(
            &serde_json::to_value(&shard_set).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let (record_id, _) = self
            .cas
            .put_artifact(
                review_core::contract::SHARD_SET_V1,
                Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some(node.id.clone()),
                    operation_id,
                },
                artifact_inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(shard_set).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        self.append(
            NewEvent::new(
                EventType::ShardSetRecordedV1,
                serde_json::to_value(RecordedSetPayloadV1 {
                    artifact_id: record_id.clone(),
                    record_id: record_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .node(&node.id)
            .referencing(vec![record_id.clone(), slice_set_record.clone()]),
        )?;
        Ok(vec![record_id])
    }

    /// A real gather: one artifact holding exactly the report artifacts the edges delivered.
    /// A reviewer whose result port feeds no edge is absent here, and therefore absent from
    /// everything downstream — the plan is the data flow, not a suggestion about it.
    ///
    /// This is also the run's canonical barrier: every reviewer has finished, so the buffered
    /// reviewer events are flushed here in node order, giving the log a shape that is a
    /// function of the pipeline rather than of thread timing.
    pub(crate) fn run_gather(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        self.flush_reviewer_events()?;
        if self.authority.finding_identity_policy == review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            let sources = self.input_bindings.get(&node.id).ok_or_else(|| {
                format!("canonical gather `{}` has no pinned input graph", node.id)
            })?;
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let node_outputs = self.node_outputs.lock().expect("node outputs");
            let mut manifest: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (port, artifacts) in inputs {
                let upstream = sources.get(port).map(Vec::as_slice).unwrap_or(&[]);
                let mut remaining = artifacts.clone();
                for (source, source_port, kind) in upstream {
                    let produced = node_outputs
                        .get(source)
                        .and_then(|outputs| outputs.get(source_port))
                        .ok_or_else(|| {
                            format!(
                                "canonical gather `{}.{port}` has no durable output for pinned source `{source}.{source_port}`",
                                node.id
                            )
                        })?;
                    if *kind == NodeKind::Reviewer {
                        let selected = selections.get(source).ok_or_else(|| {
                            format!(
                                "canonical gather `{}.{port}` received from `{source}` without a selected reviewer Attempt",
                                node.id
                            )
                        })?;
                        if produced.as_slice() != [selected.result_artifact.as_str()] {
                            return Err(format!(
                                "canonical gather `{}.{port}` input from `{source}` disagrees with its selected reviewer Attempt",
                                node.id
                            ));
                        }
                    }
                    for artifact in produced {
                        let Some(index) =
                            remaining.iter().position(|delivered| delivered == artifact)
                        else {
                            return Err(format!(
                                "canonical gather `{}.{port}` input from `{source}.{source_port}` disagrees with its durable output",
                                node.id
                            ));
                        };
                        manifest
                            .entry(source.clone())
                            .or_default()
                            .push(remaining.remove(index));
                    }
                }
                if !remaining.is_empty() {
                    return Err(format!(
                        "canonical gather `{}.{port}` has {} artifacts without pinned upstream provenance",
                        node.id,
                        remaining.len()
                    ));
                }
            }
            let artifact = self
                .cas
                .put_json(&serde_json::to_value(manifest).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            return Ok(vec![artifact]);
        }
        let artifact = self
            .cas
            .put_json(&serde_json::json!(inputs))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }
}

/// Run every shard on at most `bound` scoped threads and return the results in input order.
/// Scoped, so a shard borrows the kernel like a static node does; bounded by the Slice policy,
/// not by the machine, so a 32-shard policy on an 8-core host still runs 32 model calls at once
/// while the filesystem executor stays free for the phases inside each shard.
fn dispatch_shards<T, R>(items: Vec<T>, bound: u32, operation: impl Fn(T) -> R + Sync) -> Vec<R>
where
    T: Send,
    R: Send,
{
    let count = items.len();
    let workers = usize::try_from(bound)
        .unwrap_or(usize::MAX)
        .clamp(1, count.max(1));
    let queue = Mutex::new(items.into_iter().enumerate().collect::<VecDeque<_>>());
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..count).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("shard queue").pop_front();
                    let Some((index, item)) = next else {
                        break;
                    };
                    let result = operation(item);
                    results.lock().expect("shard results")[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("shard results")
        .into_iter()
        .map(|result| result.expect("every shard produced a result"))
        .collect()
}
