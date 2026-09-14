use super::*;
use review_core::PortCardinality;
use review_core::task::review_compat::LegacyReviewRoundV1;

pub(super) fn missing_shards(
    slices: &review_core::SliceSetV1,
    source: &str,
) -> review_core::ShardSetV1 {
    review_core::ShardSetV1 {
        subject_id: slices.subject_id.clone(),
        slice_set_id: source.into(),
        all_shards_required: true,
        shards: slices
            .slices
            .iter()
            .map(|slice| review_core::ShardReceiptV1 {
                slice_id: slice.slice_id.clone(),
                runtime_node_id: slice.runtime_node_id.clone(),
                outcome: review_core::ShardOutcomeV1::Missing {
                    reason: "fixture omitted child before dispatch".into(),
                },
            })
            .collect(),
    }
}

fn captured_fixture() -> (Fixture, LegacyReviewRoundV1, review_core::SliceSetV1) {
    let (f, round) = review::round::round_fixture();
    let mut f = with_owned_graph(f);
    let slices = review_core::SliceSetV1 {
        subject_id: round.subject_id.clone(),
        coverage: review_core::SliceCoverageV1::Complete,
        max_fanout: 2,
        all_shards_required: true,
        closeout: review_core::CloseoutPolicyV1::Required,
        slices: (0..2)
            .map(|index| review_core::ReviewSliceV1 {
                slice_id: f.cas.put_json(&json!({"slice":index})).unwrap(),
                runtime_node_id: format!("scatter#slice:{}", index + 1),
                paths: vec![format!("file{index}.rs")],
                overlaps: vec![],
            })
            .collect(),
    };
    let source = f
        .cas
        .put_artifact(
            review_core::contract::SLICE_SET_V1,
            producer(),
            vec![],
            Some(round.head_snapshot_id.clone()),
            serde_json::to_value(&slices).unwrap(),
        )
        .unwrap()
        .0;
    let root = task::ArtifactInputV1 {
        artifact_ids: vec![source],
        artifact_type: review_core::contract::SLICE_SET_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: Some(round.head_snapshot_id.clone()),
    };
    f.revision
        .inputs
        .insert("requirements".into(), root.clone());
    f.revision_id = f
        .cas
        .put_artifact(
            task::TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.revision).unwrap(),
        )
        .unwrap()
        .0;
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs.insert("requirements".into(), root);
    graph
        .nodes
        .get_mut("root.inputs")
        .unwrap()
        .contract
        .outputs
        .get_mut("requirements")
        .unwrap()
        .artifact_type = review_core::contract::SLICE_SET_V1.into();
    let parent = graph.nodes.get_mut(PARENT).unwrap();
    parent
        .contract
        .inputs
        .get_mut("input")
        .unwrap()
        .artifact_type = review_core::contract::SLICE_SET_V1.into();
    let mut port = parent.contract.outputs.remove("output").unwrap();
    port.artifact_type = review_core::contract::SHARD_SET_V1.into();
    parent.contract.outputs.insert("o0".into(), port);
    let template = graph.owned_children.get_mut(PARENT).unwrap();
    template.operator = CompiledOperator::ReviewDomain {
        review_node: "scatter".into(),
        operation: ReviewOperation::Reviewer {
            slot: "author".into(),
        },
    };
    template
        .contract
        .inputs
        .get_mut("input")
        .unwrap()
        .artifact_type = review_core::contract::SLICE_SET_V1.into();
    template
        .contract
        .inputs
        .get_mut("item")
        .unwrap()
        .artifact_type = review_core::contract::REVIEW_SLICE_V1.into();
    f.plan.compiled_graph_id = f
        .cas
        .put_artifact(
            "af/CompiledTask@1",
            producer(),
            vec![],
            None,
            serde_json::to_value(graph).unwrap(),
        )
        .unwrap()
        .0;
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.inputs = f.revision.inputs.clone();
    f.plan_id = f
        .cas
        .put_artifact(
            task::EXECUTION_PLAN_V1,
            producer(),
            vec![f.revision_id.clone()],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;
    (f, round, slices)
}

fn prepare(
    f: &mut Fixture,
    round: &LegacyReviewRoundV1,
    slices: &review_core::SliceSetV1,
) -> (TaskLease, RegisteredTaskChildren, String) {
    let (lease, parent) = started(f);
    let source = f.revision.inputs["requirements"].artifact_ids[0].clone();
    let mut items = vec![];
    for (index, slice) in slices.slices.iter().enumerate() {
        let item = f
            .cas
            .put_artifact(
                review_core::contract::REVIEW_SLICE_V1,
                producer(),
                vec![source.clone()],
                Some(round.head_snapshot_id.clone()),
                serde_json::to_value(slice).unwrap(),
            )
            .unwrap()
            .0;
        let node = format!("{PARENT}.slice{}", index + 1);
        let input = TaskInvocationV1 {
            plan_id: f.plan_id.clone(),
            node: node.clone(),
            inputs: BTreeMap::from([
                ("input".into(), f.revision.inputs["requirements"].clone()),
                (
                    "item".into(),
                    task::ArtifactInputV1 {
                        artifact_ids: vec![item.clone()],
                        artifact_type: review_core::contract::REVIEW_SLICE_V1.into(),
                        cardinality: PortCardinality::One,
                        snapshot_id: Some(round.head_snapshot_id.clone()),
                    },
                ),
            ]),
        };
        let invocation_id = f
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                producer(),
                vec![f.plan_id.clone(), item.clone()],
                None,
                serde_json::to_value(input).unwrap(),
            )
            .unwrap()
            .0;
        items.push(TaskOwnedChildV1 {
            node,
            source_item_id: item,
            invocation_id,
        });
    }
    let set = TaskOwnedChildSetV1 {
        plan_id: f.plan_id.clone(),
        parent_invocation_id: parent.clone(),
        source_artifact_id: source.clone(),
        children: items,
    };
    let handle = register(f, &lease, &write_set(f, &set));
    let shards = serde_json::to_value(missing_shards(slices, &source)).unwrap();
    let id = f
        .cas
        .put_artifact(
            review_core::contract::SHARD_SET_V1,
            Producer::KernelOperation {
                run_id: round.campaign_id.clone(),
                node_id: Some("scatter".into()),
                operation_id: crate::content_id(&shards).unwrap(),
            },
            vec![source],
            Some(round.head_snapshot_id.clone()),
            shards,
        )
        .unwrap()
        .0;
    let output = TaskOutputV1 {
        invocation_id: parent.clone(),
        outputs: BTreeMap::from([(
            "o0".into(),
            task::ArtifactInputV1 {
                artifact_ids: vec![id.clone()],
                artifact_type: review_core::contract::SHARD_SET_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: Some(round.head_snapshot_id.clone()),
            },
        )]),
    };
    let output_id = f
        .cas
        .put_artifact(
            TASK_OUTPUT_V1,
            producer(),
            vec![parent, id],
            None,
            serde_json::to_value(output).unwrap(),
        )
        .unwrap()
        .0;
    (lease, handle, output_id)
}

#[test]
fn owned_canonical_shards_require_exact_seal_and_reopen_idempotently() {
    let (mut f, round, slices) = captured_fixture();
    let (lease, handle, output) = prepare(&mut f, &round, &slices);
    let before = f.store.len(&round.campaign_id).unwrap();
    assert!(
        f.store
            .publish_task_owned_review_shards(&f.cas, &lease, &handle, &output, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("sealed parent output")
    );
    assert_eq!(f.store.len(&round.campaign_id).unwrap(), before);
    f.store
        .complete_task_owned_children(
            &f.cas,
            &lease,
            &handle,
            &output,
            &OwnedAuthority(&f.authority),
        )
        .unwrap();
    f.store
        .publish_task_owned_review_shards(&f.cas, &lease, &handle, &output, &f.authority)
        .unwrap();
    let events = f.store.replay(&round.campaign_id).unwrap();
    let event = events.last().unwrap();
    assert_eq!(event.event_type, EventType::ShardSetRecordedV1);
    assert_eq!(event.node_id.as_deref(), Some("scatter"));
    assert_eq!(event.causation_id.as_ref(), Some(&round.round_event_id));
    assert_eq!(event.correlation_id.as_ref(), Some(&round.subject_id));
    assert!(event.attempt_id.is_none());
    assert_eq!(
        event.artifact_refs[1],
        handle.child_set().source_artifact_id
    );
    f.store = EventStore::open(&f.path).unwrap();
    f.store
        .publish_task_owned_review_shards(&f.cas, &lease, &handle, &output, &f.authority)
        .unwrap();
    assert_eq!(f.store.replay(&round.campaign_id).unwrap(), events);
    review::round::supersede(&f, &round);
    assert!(
        f.store
            .publish_task_owned_review_shards(&f.cas, &lease, &handle, &output, &f.authority)
            .is_err()
    );
}

#[test]
fn owned_shard_transaction_rechecks_both_task_and_review_prefixes() {
    for change_task in [true, false] {
        let (mut f, round, slices) = captured_fixture();
        let (lease, handle, output) = prepare(&mut f, &round, &slices);
        f.store
            .complete_task_owned_children(
                &f.cas,
                &lease,
                &handle,
                &output,
                &OwnedAuthority(&f.authority),
            )
            .unwrap();
        f.store
            .publish_task_owned_review_shards(&f.cas, &lease, &handle, &output, &f.authority)
            .unwrap();
        let recorded = f.store.replay(&round.campaign_id).unwrap().pop().unwrap();
        let event = NewEvent::new(recorded.event_type, recorded.payload)
            .node(recorded.node_id.unwrap())
            .caused_by(recorded.causation_id.unwrap())
            .correlating(recorded.correlation_id.unwrap())
            .referencing(recorded.artifact_refs);
        let state = f.state();
        let permit = execution::review::WritePermit::for_checked_recording(
            &state,
            &f.plan,
            &round.campaign_id,
            f.store.len(&round.campaign_id).unwrap(),
            event.clone(),
        )
        .unwrap();
        // Capture exactly the permit used by publication, then change one durable prefix
        // through a second Store. No raw event or lifecycle insertion is used.
        if change_task {
            let mut other = EventStore::open(&f.path).unwrap();
            other.release_task_lease(&f.cas, &lease).unwrap();
            other
                .take_task_lease(&f.cas, lease.task_id(), "replacement-writer", 1_000_000)
                .unwrap();
        } else {
            review::round::supersede(&f, &round);
        }
        let before = f.store.replay(&round.campaign_id).unwrap();
        let error = f
            .store
            .append_batch_inner(&round.campaign_id, &f.cas, &[event], None, Some(&permit))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exact execution/lease comparison"),
            "{error}"
        );
        assert_eq!(f.store.replay(&round.campaign_id).unwrap(), before);
    }
}
