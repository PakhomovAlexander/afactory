//! Native Slicer/Scatter use the common owned-child registry and Attempt machinery. Every
//! registered Slice survives in the terminal Shard Set, even when no Worker could start.
use super::*;
use af_contracts::task::owned_children::TaskOwnedChildSetV1;
use af_contracts::{CloseoutPolicyV1, ShardOutcomeV1, ShardSetV1, SliceCoverageV1, SliceSetV1};
use af_store::store::task::execution::owned::TaskOwnedChildEvidence;

pub(super) fn slice_signature() -> OperatorSignature {
    OperatorSignature {
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([
                (
                    "source".into(),
                    port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}, false),
                ),
                (
                    "subject".into(),
                    port(TASK_REVIEW_SUBJECT_V3, same(), false),
                ),
                (
                    "history".into(),
                    port(REVIEW_HISTORY_V1, PortAffinityV1::Unbound {}, false),
                ),
            ]),
            outputs: BTreeMap::from([(
                "slices".into(),
                port(af_contracts::contract::SLICE_SET_V1, same(), false),
            )]),
        },
        effects: BTreeSet::new(),
        evidence: BTreeMap::new(),
        retains: BTreeMap::new(),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        attempt: None,
        outcome_port: None,
        unbound_allowed: BTreeSet::new(),
    }
}

impl ReviewTaskDomain {
    pub(super) fn selected_review_source(
        &self,
        cas: &Cas,
        review: &TaskReviewRoundV1,
        target: &str,
        result: &str,
    ) -> Result<String, String> {
        if self.policy.reviewers.contains_key(target) {
            return Ok(target.into());
        }
        if let TaskOperatorV1::ReviewReduceShards { reviewers, .. } =
            self.operator(&review.invocation)?
        {
            for source in reviewers {
                let (_, _, shards) = self.checked_shards(cas, &review.invocation, source)?;
                for (index, shard) in shards.shards.iter().enumerate() {
                    if target == format!("{source}_slice{}", index + 1)
                        && matches!(&shard.outcome, ShardOutcomeV1::Completed {result_artifact_ids} if result_artifact_ids == &[result])
                    {
                        return Ok(source.clone());
                    }
                }
            }
        }
        Err("Proposal target has no canonical static or owned-shard source".into())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn selected_stage(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        subject: &ResolvedReviewSubject,
        ledger: &Ledger,
        name: &str,
        id: &str,
        expected_node: &str,
    ) -> Result<
        (
            String,
            String,
            af_contracts::ReviewerStageOutput,
            String,
            Vec<String>,
        ),
        String,
    > {
        let artifact = envelope(cas, id)?;
        if artifact.artifact_type != RESULT_TYPE {
            return Err("Review shard changed its result contract".into());
        }
        let stage = crate::reviewer_stage_output(artifact.payload.clone())?;
        let assignment = self.assignment(ledger, subject, name)?;
        self.validate_stage(&stage, &assignment)?;
        self.validate_result_assignment(cas, input, name, &artifact, &assignment)?;
        let Producer::Attempt {
            run_id,
            node_id,
            attempt_id,
        } = &artifact.producer
        else {
            return Err("Review result has no selected Worker Attempt".into());
        };
        let expected_run = match invocation_producer(cas, input, None)? {
            Producer::KernelOperation { run_id, .. } => run_id,
            _ => unreachable!(),
        };
        if run_id != &expected_run
            || node_id != expected_node
            || artifact.subject_snapshot_id.as_ref() != Some(&subject.snapshot_id)
            || !artifact
                .input_artifacts
                .contains(&input.inputs["subject"].artifact_ids[0])
            || self.graph.inputs.get("requirements").is_some_and(|p| {
                p.artifact_ids
                    .iter()
                    .any(|id| !artifact.input_artifacts.contains(id))
            })
        {
            return Err("Review result changed its Task, Subject, Snapshot, requirements or declared Worker".into());
        }
        Ok((
            name.into(),
            id.into(),
            stage,
            attempt_id.clone(),
            artifact.input_artifacts,
        ))
    }
    pub(super) fn slice_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let TaskOperatorV1::ReviewSlice {
            scatter,
            max_paths_per_slice,
            max_fanout,
        } = self.operator(input)?
        else {
            return Err("Not a native Review Slicer".into());
        };
        let subject = self.current_subject(cas, input)?;
        let parent = input.node.rsplit_once('.').ok_or("Slicer has no scope")?.0;
        let scatter_node = format!("{parent}.{scatter}");
        let policy = crate::scatter::StaticSlicePolicy {
            scatter_node: scatter_node.clone(),
            max_paths_per_slice: *max_paths_per_slice as usize,
            max_fanout: *max_fanout,
            coverage: SliceCoverageV1::Complete,
            all_shards_required: true,
            closeout: CloseoutPolicyV1::Required,
        };
        let paths = match &subject.change_set {
            Some(changes) => changes.changed_paths.clone(),
            None => read_snapshot(cas, &subject.snapshot_id)?
                .1
                .entries
                .into_iter()
                .map(|e| e.path)
                .collect(),
        };
        if self
            .graph
            .owned_children
            .get(&scatter_node)
            .is_none_or(|t| t.max_children != *max_fanout)
        {
            return Err("Slicer does not name its exact bounded captured Scatter".into());
        }
        let slices = policy.plan(
            &subject.subject_id,
            &paths,
            &self.graph.nodes.keys().cloned().collect(),
        )?;
        let output = self.put(
            cas,
            input,
            af_contracts::contract::SLICE_SET_V1,
            Some(&subject.snapshot_id),
            &slices,
            vec![subject.subject_id],
        )?;
        Ok(BTreeMap::from([("slices".into(), output)]))
    }

    fn native_slices(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<(String, SliceSetV1), String> {
        let (id, slices): (_, SliceSetV1) =
            self.value(cas, input, "slices", af_contracts::contract::SLICE_SET_V1)?;
        let address = self.graph.nodes[&input.node]
            .inputs
            .get("slices")
            .ok_or("Scatter has no captured Slicer binding")?;
        let artifact = envelope(cas, &id)?;
        let Producer::KernelOperation {
            run_id,
            node_id: Some(node),
            ..
        } = &artifact.producer
        else {
            return Err("Slice Set has no kernel producer".into());
        };
        let expected_run = match invocation_producer(cas, input, None)? {
            Producer::KernelOperation { run_id, .. } => run_id,
            _ => unreachable!(),
        };
        let slicer = &self.graph.nodes[&address.node];
        let slicer_input = TaskInvocationV1 {
            plan_id: input.plan_id.clone(),
            node: address.node.clone(),
            inputs: slicer
                .inputs
                .keys()
                .map(|name| {
                    input
                        .inputs
                        .get(name)
                        .cloned()
                        .map(|p| (name.clone(), p))
                        .ok_or("Slicer and Scatter changed exact shared inputs")
                })
                .collect::<Result<_, _>>()?,
        };
        let subject = self.current_subject(cas, input)?;
        let expected = self.slice_outputs(cas, &slicer_input)?;
        if *run_id != expected_run
            || *node != address.node
            || address.port != "slices"
            || expected["slices"].artifact_ids != [id.clone()]
            || slices.subject_id != subject.subject_id
            || slices.max_fanout != self.graph.owned_children[&input.node].max_children
        {
            return Err(
                "Scatter changed the exact complete Slice Set or its producing Slicer".into(),
            );
        }
        Ok((id, slices))
    }

    pub(super) fn prepare_native_children(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<super::super::TaskOwnedChildrenInputs, String> {
        let (source, slices) = self.native_slices(cas, input)?;
        let snapshot = source_input(cas, &input.inputs["source"])?;
        let producer = invocation_producer(cas, input, None)?;
        let source_item_ids = slices
            .slices
            .iter()
            .map(|slice| {
                cas.put_artifact(
                    af_contracts::contract::REVIEW_SLICE_V1,
                    producer.clone(),
                    vec![source.clone()],
                    Some(snapshot.clone()),
                    serde_json::to_value(slice).map_err(|e| e.to_string())?,
                )
                .map(|(id, _)| id)
                .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(super::super::TaskOwnedChildrenInputs {
            source_artifact_id: source,
            source_item_ids,
        })
    }

    pub(super) fn check_native_children(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        children: &TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        children.validate()?;
        let expected = self.prepare_native_children(cas, input)?;
        let invocation = envelope(cas, &children.parent_invocation_id)?;
        if children.plan_id != input.plan_id
            || children.source_artifact_id != expected.source_artifact_id
            || invocation.artifact_type != af_contracts::task::execution::TASK_INVOCATION_V1
            || invocation.payload != serde_json::to_value(input).map_err(|e| e.to_string())?
            || children.children.len() != expected.source_item_ids.len()
            || children
                .children
                .iter()
                .zip(&expected.source_item_ids)
                .enumerate()
                .any(|(i, (child, id))| {
                    child.source_item_id != *id
                        || child.node != format!("{}.slice{}", input.node, i + 1)
                })
        {
            return Err(
                "Owned Review membership changed plan, complete Slice order or exact parent".into(),
            );
        }
        Ok(())
    }

    pub(super) fn native_shards(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        children: &TaskOwnedChildSetV1,
        facts: &[TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        self.check_native_children(cas, input, children)?;
        let (source, slices) = self.native_slices(cas, input)?;
        if facts.len() != children.children.len()
            || facts
                .iter()
                .zip(&children.children)
                .any(|(f, c)| f.child != *c)
        {
            return Err("Owned Review fold omitted or reordered a registered child".into());
        }
        let mut outcomes = BTreeMap::new();
        for (slice, fact) in slices.slices.iter().zip(facts) {
            let outcome = if let Some(ids) = &fact.completed_artifact_ids {
                if ids.len() != 1 || fact.published_output_id.is_none() {
                    return Err("Completed native shard lost its selected published result".into());
                }
                ShardOutcomeV1::Completed {
                    result_artifact_ids: ids.clone(),
                }
            } else {
                if fact
                    .attempts
                    .iter()
                    .any(|a| a.started && a.result.is_none())
                {
                    return Err("Native shard still has unsettled started work".into());
                }
                if fact.selected_output_id.is_some()
                    || fact.published_output_id.is_some()
                    || fact.attempts.iter().any(|a| a.started)
                {
                    ShardOutcomeV1::Failed {reason: "registered Review child has no admitted published result after its bounded Attempts".into()}
                } else {
                    ShardOutcomeV1::Missing {
                        reason: "registered Review child did not start an Attempt".into(),
                    }
                }
            };
            outcomes.insert(slice.slice_id.clone(), outcome);
        }
        let shards = crate::scatter::fold_shards(&slices, &source, &outcomes)?;
        let result = self.put(
            cas,
            input,
            af_contracts::contract::SHARD_SET_V1,
            input.inputs["source"].snapshot_id.as_deref(),
            &shards,
            crate::scatter::shard_artifact_inputs(&shards),
        )?;
        Ok(BTreeMap::from([("result".into(), result)]))
    }

    pub(super) fn checked_shards(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        name: &str,
    ) -> Result<(String, SliceSetV1, ShardSetV1), String> {
        let (id, shards): (_, ShardSetV1) =
            self.value(cas, input, name, af_contracts::contract::SHARD_SET_V1)?;
        let frame = envelope(cas, &id)?;
        let address = &self.graph.nodes[&input.node].inputs[name];
        let parent = &self.graph.nodes[&address.node];
        let expected = match invocation_producer(cas, input, None)? {
            Producer::KernelOperation { run_id, .. } => Producer::KernelOperation {
                run_id,
                node_id: Some(address.node.clone()),
                operation_id: "task-builtin@1".into(),
            },
            _ => unreachable!(),
        };
        if frame.producer != expected
            || frame.subject_snapshot_id != input.inputs["source"].snapshot_id
            || !matches!(
                parent.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::ReviewScatter { .. },
                    ..
                }
            )
        {
            return Err("Shard Set changed its captured native Scatter producer".into());
        }
        let source = envelope(cas, &shards.slice_set_id)?;
        if source.artifact_type != af_contracts::contract::SLICE_SET_V1 {
            return Err("Shard Set lost its complete Slice Set".into());
        }
        let slices: SliceSetV1 =
            serde_json::from_value(source.payload).map_err(|e| e.to_string())?;
        let subject = self.current_subject(cas, input)?;
        shards.validate_against(&slices)?;
        if shards.subject_id != subject.subject_id
            || !slices.all_shards_required
            || slices.coverage != SliceCoverageV1::Complete
            || slices.closeout != CloseoutPolicyV1::Required
        {
            return Err("Native Review requires complete coverage, all shard outcomes and whole-Subject closeout".into());
        }
        Ok((id, slices, shards))
    }

    pub(super) fn shard_membership(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
        let TaskOperatorV1::ReviewReduceShards { reviewers, .. } = self.operator(input)? else {
            return Ok(BTreeMap::new());
        };
        reviewers
            .iter()
            .filter(|name| input.inputs.contains_key(*name))
            .map(|name| {
                let (_, _, shards) = self.checked_shards(cas, input, name)?;
                let ids = shards
                    .shards
                    .into_iter()
                    .flat_map(|s| match s.outcome {
                        ShardOutcomeV1::Completed {
                            result_artifact_ids,
                        } => result_artifact_ids,
                        _ => vec![],
                    })
                    .collect();
                Ok((name.clone(), ids))
            })
            .collect()
    }

    pub(super) fn closure_refs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        complete: bool,
    ) -> Result<Vec<String>, String> {
        let TaskOperatorV1::ReviewReduceShards {
            reviewers,
            closeout,
        } = self.operator(input)?
        else {
            return Ok(vec![]);
        };
        if !complete {
            return Ok(vec![]);
        }
        let closeout = input
            .inputs
            .get(closeout)
            .ok_or("Whole-Subject closeout result is missing")?
            .artifact_ids[0]
            .clone();
        reviewers
            .iter()
            .map(|name| {
                let (_, slices, shards) = self.checked_shards(cas, input, name)?;
                let ids: Vec<_> = shards
                    .shards
                    .iter()
                    .flat_map(|s| match &s.outcome {
                        ShardOutcomeV1::Completed {
                            result_artifact_ids,
                        } => result_artifact_ids.clone(),
                        _ => vec![],
                    })
                    .collect();
                let selected = BTreeMap::from([("reviewer_results".into(), ids.clone())]);
                let sinks = ids
                    .into_iter()
                    .map(|id| (id, format!("{}:canonical-ledger", input.node)))
                    .collect();
                let closure = crate::scatter::prove_semantic_closure(
                    &slices,
                    &shards,
                    &selected,
                    &sinks,
                    Some(closeout.clone()),
                )?;
                self.put(
                    cas,
                    input,
                    af_contracts::contract::SEMANTIC_CLOSURE_V1,
                    input.inputs["source"].snapshot_id.as_deref(),
                    &closure,
                    crate::scatter::shard_artifact_inputs(&shards),
                )
                .map(|p| p.artifact_ids[0].clone())
            })
            .collect()
    }
}
