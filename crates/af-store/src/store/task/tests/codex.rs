use super::*;
use af_contracts::PortCardinality;
use af_contracts::task::execution::*;
use af_contracts::task::pipeline::{
    PipelineContractV1, PipelinePortV1, PortAffinityV1, TaskOperatorV1,
};
use af_contracts::task::plan::{EffectiveWorkerBindingV3, WorkerExecutionV3};
use af_contracts::task::source::TaskSnapshot;
use af_graph::task::{Address, CompiledNode, CompiledOperator, CompiledTask};

const NODE: &str = "root.nodes.write";
const SEAL: &str = "root.nodes.seal";

fn put(cas: &Cas, kind: &str, value: &impl serde::Serialize) -> String {
    cas.put_artifact(
        kind,
        producer(),
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

fn port(artifact_type: &str) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        brief: None,
        covers: BTreeSet::new(),
    }
}

fn fixture(with_seal: bool) -> (Fixture, TaskLease) {
    let initial = Fixture::new(false);
    let mut f = if with_seal {
        initial.with_execution_graph_outputs(BTreeMap::from([(
            "candidate".into(),
            port("af/CandidateTree@1"),
        )]))
    } else {
        initial.with_execution_graph()
    };
    let mut graph: CompiledTask =
        payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@4").unwrap();
    if with_seal {
        let manifest_id = f.cas.put_json(&json!({"entries":[]})).unwrap();
        let parent = TaskSnapshot {
            schema: "af.task-snapshot/1".into(),
            origin_id: f.cas.put(b"fixture origin").unwrap(),
            manifest_id: manifest_id.clone(),
            content_digest: manifest_id,
            parent_snapshot_id: None,
        };
        let parent_id = f
            .cas
            .put_json(&serde_json::to_value(parent).unwrap())
            .unwrap();
        let (source_id, _) = f
            .cas
            .put_artifact(
                "af/SourceTree@1",
                producer(),
                vec![parent_id.clone()],
                Some(parent_id.clone()),
                json!({"snapshot_id": parent_id}),
            )
            .unwrap();
        let source = task::ArtifactInputV1 {
            artifact_ids: vec![source_id],
            artifact_type: "af/SourceTree@1".into(),
            snapshot_id: Some(parent_id),
            cardinality: PortCardinality::One,
        };
        f.revision.inputs.insert("source".into(), source.clone());
        f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
        f.plan.task_revision_id = f.revision_id.clone();
        f.plan.inputs.insert("source".into(), source.clone());
        graph.inputs.insert("source".into(), source);
        graph
            .nodes
            .get_mut("root.inputs")
            .unwrap()
            .contract
            .outputs
            .insert("source".into(), port("af/SourceTree@1"));
        let worker = graph.nodes.get_mut(NODE).unwrap();
        worker
            .contract
            .inputs
            .insert("source".into(), port("af/SourceTree@1"));
        worker.inputs.insert(
            "source".into(),
            Address {
                node: "root.inputs".into(),
                port: "source".into(),
            },
        );
        graph.nodes.insert(
            SEAL.into(),
            CompiledNode {
                operator: CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Seal {},
                    signature: "seal".into(),
                },
                contract: PipelineContractV1 {
                    inputs: BTreeMap::from([("candidate".into(), port("af/CandidateTree@1"))]),
                    outputs: BTreeMap::from([("snapshot".into(), port("af/SourceTree@1"))]),
                },
                inputs: BTreeMap::from([(
                    "candidate".into(),
                    Address {
                        node: NODE.into(),
                        port: "candidate".into(),
                    },
                )]),
                conditions: vec![],
            },
        );
        graph.order = graph.scheduler_plan().unwrap().order;
    }
    let (slot, worker) = graph.slots.iter_mut().next().unwrap();
    worker.role = "implement".into();
    let package_id = f.plan.pipeline_id.clone();
    f.plan.bindings.insert(
        slot.clone(),
        EffectiveWorkerBindingV3 {
            package_digest: package_id.clone(),
            package_artifact_id: package_id.clone(),
            invocation_policy_id: package_id.clone(),
            execution: WorkerExecutionV3::Model {
                provider: "codex-fixture".into(),
                provider_kind: "codex".into(),
                principal_id: Some(package_id),
                model: "codex-fixture".into(),
                effort: "low".into(),
                pi: None,
            },
        },
    );
    f.plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@4", &graph);
    f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V4, &f.plan);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 3_000)
        .unwrap();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    (f, lease)
}

fn launch(f: &Fixture, context_id: &str) -> CodexLaunchV1 {
    let digest = f.plan_id.clone();
    CodexLaunchV1 {
        context_id: context_id.into(),
        executable_digest: digest.clone(),
        protocol_digest: digest.clone(),
        runtime_digest: digest.clone(),
        sandbox_digest: digest.clone(),
        reply_schema_digest: digest.clone(),
        submission_digest: digest.clone(),
        nonce: digest,
        thread_id: "fixture-thread".into(),
        mode: CodexLaunchModeV1::Producer {},
    }
}

#[test]
fn codex_fence_survives_writer_loss_and_releases_only_on_correlated_evidence() {
    test_clock::with_time(1_000_000, |clock| {
        let (mut f, lease) = fixture(false);
        let context_id = f.cas.put_json(&json!({"context": "fixture"})).unwrap();
        let prepared = f
            .store
            .reserve_and_bind_task_attempt(&f.cas, &lease, NODE, &context_id, &f.authority)
            .unwrap();
        let launch = launch(&f, &context_id);
        let mut wrong_context = launch.clone();
        wrong_context.context_id = f.plan_id.clone();
        assert!(
            f.store
                .arm_codex_launch(&f.cas, &lease, &prepared, &f.authority, wrong_context)
                .is_err()
        );
        let before = f.state().next_sequence;
        let launch_id = f
            .store
            .arm_codex_launch(&f.cas, &lease, &prepared, &f.authority, launch.clone())
            .unwrap();
        let state = f.state();
        assert_eq!(state.next_sequence, before + 1);
        assert_eq!(state.execution.unwrap().budget.begun_attempts(), 1);
        assert!(
            f.store
                .arm_codex_launch(&f.cas, &lease, &prepared, &f.authority, launch.clone())
                .is_err()
        );
        assert!(
            f.store
                .record_codex_checkpoint(
                    &f.cas,
                    &lease,
                    prepared.id(),
                    &CodexCheckpointV1 {
                        launch_id: launch_id.clone(),
                        output_id: context_id.clone(),
                        candidate_id: context_id.clone(),
                        sealed_output_id: context_id.clone(),
                        snapshot_id: context_id.clone(),
                        reply_id: context_id.clone(),
                        prefix_id: context_id,
                        prefix_bytes: 1,
                    }
                )
                .unwrap_err()
                .to_string()
                .contains("completed producer")
        );

        clock.set(f.state().lease_until + 1);
        f.store = EventStore::open(&f.path).unwrap();
        let successor = f
            .store
            .take_task_lease(&f.cas, lease.task_id(), "writer-2", 3_000)
            .unwrap();
        f.store.recover_task_attempts(&f.cas, &successor).unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let execution = f.state().execution.unwrap();
        let accounting = execution.attempt_accounting();
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(execution.budget.committed_tokens(), 0);
        assert!(accounting[0].unknown_usage.is_some());
        assert_eq!(
            accounting[0].abandoned,
            Some(execution::TaskAbandonCause::WriterLost)
        );

        let refused = f
            .store
            .reserve_task_attempt(&f.cas, &successor, NODE, &f.authority)
            .unwrap_err();
        assert!(refused.to_string().contains(&launch_id), "{refused}");
        let mut receipt = CodexDispatchEvidenceV1 {
            launch_id,
            nonce: f.revision_id.clone(),
            thread_id: launch.thread_id,
            outcome: CodexLaunchOutcomeV1::Completed {
                turn_id: "fixture-turn".into(),
            },
            proof_id: f
                .cas
                .put(b"fixture correlated terminal and drained")
                .unwrap(),
        };
        assert!(
            f.store
                .record_codex_resolution(&f.cas, &successor, prepared.id(), &receipt)
                .is_err()
        );
        receipt.nonce = launch.nonce;
        assert!(
            f.store
                .record_codex_resolution(&f.cas, &lease, prepared.id(), &receipt)
                .is_err()
        );
        f.store
            .record_codex_resolution(&f.cas, &successor, prepared.id(), &receipt)
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let before = f.state().next_sequence;
        f.store
            .record_codex_resolution(&f.cas, &successor, prepared.id(), &receipt)
            .unwrap();
        assert_eq!(f.state().next_sequence, before);
        receipt.outcome = CodexLaunchOutcomeV1::NotDispatched {};
        assert!(
            f.store
                .record_codex_resolution(&f.cas, &successor, prepared.id(), &receipt)
                .is_err()
        );

        let retry = f
            .store
            .reserve_task_attempt(&f.cas, &successor, NODE, &f.authority)
            .unwrap();
        assert_ne!(retry.id(), prepared.id());
        assert_ne!(retry.reservation().id, prepared.reservation().id);
        assert!(f.state().execution.unwrap().reusable_output(NODE).is_none());
    });
}

#[test]
fn codex_checkpoint_replays_only_for_its_exact_published_seal() {
    check_checkpoint(CheckpointFixture::Exact);
}

#[test]
fn codex_checkpoint_refuses_seal_missing_the_candidate_reference() {
    check_checkpoint(CheckpointFixture::UnlinkedSeal);
}

#[test]
fn codex_checkpoint_refuses_same_manifest_from_a_different_source() {
    check_checkpoint(CheckpointFixture::ForeignSource);
}

enum CheckpointFixture {
    Exact,
    UnlinkedSeal,
    ForeignSource,
}

fn check_checkpoint(fixture_kind: CheckpointFixture) {
    test_clock::with_time(1_000_000, |_| {
        let (mut f, lease) = fixture(true);
        let context_id = f.cas.put(b"fixture context").unwrap();
        let attempt = f
            .store
            .reserve_and_bind_task_attempt(&f.cas, &lease, NODE, &context_id, &f.authority)
            .unwrap();
        let request = launch(&f, &context_id);
        let launch_id = f
            .store
            .arm_codex_launch(&f.cas, &lease, &attempt, &f.authority, request.clone())
            .unwrap();
        let receipt = CodexDispatchEvidenceV1 {
            launch_id: launch_id.clone(),
            nonce: request.nonce,
            thread_id: request.thread_id,
            outcome: CodexLaunchOutcomeV1::Completed {
                turn_id: "fixture-turn".into(),
            },
            proof_id: f.cas.put(b"fixture terminal and drained").unwrap(),
        };
        f.store
            .record_codex_resolution(&f.cas, &lease, attempt.id(), &receipt)
            .unwrap();

        let invocation_id = f.state().execution.unwrap().invocations[NODE].0.clone();
        let mut output: TaskOutputV1 = payload(
            &f.cas,
            &f.execution_output(&invocation_id, attempt.id()),
            TASK_OUTPUT_V1,
        )
        .unwrap();
        let author = Producer::Attempt {
            run_id: task_run_id(lease.task_id()).unwrap(),
            node_id: NODE.into(),
            attempt_id: attempt.id().into(),
        };
        let source = &f.revision.inputs["source"];
        let mut parent_id = source.snapshot_id.clone().unwrap();
        let mut parent: TaskSnapshot =
            serde_json::from_value(f.cas.get_json(&parent_id).unwrap()).unwrap();
        if matches!(fixture_kind, CheckpointFixture::ForeignSource) {
            parent.origin_id = f.cas.put(b"another origin over the same Manifest").unwrap();
            parent_id = f
                .cas
                .put_json(&serde_json::to_value(&parent).unwrap())
                .unwrap();
        }
        let manifest_id = parent.manifest_id.clone();
        let (candidate_id, _) = f
            .cas
            .put_artifact(
                "af/CandidateTree@1",
                author.clone(),
                vec![
                    source.artifact_ids[0].clone(),
                    manifest_id.clone(),
                    parent_id.clone(),
                ],
                Some(parent_id.clone()),
                json!({"manifest_id":manifest_id,"parent_snapshot_id":parent_id}),
            )
            .unwrap();
        let candidate = task::ArtifactInputV1 {
            artifact_ids: vec![candidate_id.clone()],
            artifact_type: "af/CandidateTree@1".into(),
            snapshot_id: Some(parent_id.clone()),
            cardinality: PortCardinality::One,
        };
        output.outputs.insert("candidate".into(), candidate.clone());
        let (output_id, _) = f
            .cas
            .put_artifact(
                TASK_OUTPUT_V1,
                author,
                vec![invocation_id, candidate_id.clone()],
                None,
                serde_json::to_value(output).unwrap(),
            )
            .unwrap();
        let reply_id = f
            .cas
            .put_json(
                &json!({"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"fixture"}]}}),
            )
            .unwrap();
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 0,
                    result: TaskAttemptResultV1::Succeeded {
                        output_id: output_id.clone(),
                    },
                    raw_artifact_ids: vec![reply_id.clone()],
                    usage_id: None,
                    unknown_usage: None,
                },
                &f.authority,
            )
            .unwrap();
        f.store
            .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
            .unwrap();

        let seal_invocation = put(
            &f.cas,
            TASK_INVOCATION_V1,
            &TaskInvocationV1 {
                plan_id: f.plan_id.clone(),
                node: SEAL.into(),
                inputs: BTreeMap::from([("candidate".into(), candidate)]),
            },
        );
        f.store
            .record_task_invocation(&f.cas, &lease, &seal_invocation, &f.authority)
            .unwrap();
        let snapshot = TaskSnapshot {
            parent_snapshot_id: Some(parent_id),
            ..parent
        };
        let snapshot_id = f
            .cas
            .put_json(&serde_json::to_value(snapshot).unwrap())
            .unwrap();
        let mut source_refs = vec![snapshot_id.clone()];
        if !matches!(fixture_kind, CheckpointFixture::UnlinkedSeal) {
            source_refs.push(candidate_id.clone());
        }
        let (source_id, _) = f
            .cas
            .put_artifact(
                "af/SourceTree@1",
                Producer::KernelOperation {
                    run_id: task_run_id(lease.task_id()).unwrap(),
                    node_id: Some(SEAL.into()),
                    operation_id: "task-builtin@1".into(),
                },
                source_refs,
                Some(snapshot_id.clone()),
                json!({"snapshot_id":snapshot_id}),
            )
            .unwrap();
        let seal_id = put(
            &f.cas,
            TASK_OUTPUT_V1,
            &TaskOutputV1 {
                invocation_id: seal_invocation,
                outputs: BTreeMap::from([(
                    "snapshot".into(),
                    task::ArtifactInputV1 {
                        artifact_ids: vec![source_id],
                        artifact_type: "af/SourceTree@1".into(),
                        snapshot_id: Some(snapshot_id.clone()),
                        cardinality: PortCardinality::One,
                    },
                )]),
            },
        );
        f.store
            .publish_task_output(&f.cas, &lease, &seal_id, None, &f.authority)
            .unwrap();
        let prefix = b"fixture history through the completed turn";
        let checkpoint = CodexCheckpointV1 {
            launch_id,
            output_id,
            candidate_id,
            sealed_output_id: seal_id.clone(),
            snapshot_id: snapshot_id.clone(),
            reply_id,
            prefix_id: f.cas.put(prefix).unwrap(),
            prefix_bytes: prefix.len() as u64,
        };
        assert_eq!(
            f.store
                .selected_codex_checkpoint(&f.cas, lease.task_id(), NODE, &seal_id, &snapshot_id)
                .unwrap(),
            None
        );
        let refusal = match fixture_kind {
            CheckpointFixture::Exact => None,
            CheckpointFixture::UnlinkedSeal => Some("exact seal or history bytes"),
            CheckpointFixture::ForeignSource => Some("admitted source"),
        };
        if let Some(reason) = refusal {
            let error = f
                .store
                .record_codex_checkpoint(&f.cas, &lease, attempt.id(), &checkpoint)
                .unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
            assert_eq!(
                f.store
                    .selected_codex_checkpoint(
                        &f.cas,
                        lease.task_id(),
                        NODE,
                        &seal_id,
                        &snapshot_id
                    )
                    .unwrap(),
                None
            );
            return;
        }
        let incorrect = CodexCheckpointV1 {
            snapshot_id: checkpoint.reply_id.clone(),
            ..checkpoint.clone()
        };
        assert!(
            f.store
                .record_codex_checkpoint(&f.cas, &lease, attempt.id(), &incorrect)
                .unwrap_err()
                .to_string()
                .contains("sealed Snapshot")
        );
        let id = f
            .store
            .record_codex_checkpoint(&f.cas, &lease, attempt.id(), &checkpoint)
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        assert_eq!(
            f.store
                .selected_codex_checkpoint(&f.cas, lease.task_id(), NODE, &seal_id, &snapshot_id)
                .unwrap(),
            Some(id)
        );
        assert!(
            f.store
                .selected_codex_checkpoint(
                    &f.cas,
                    lease.task_id(),
                    NODE,
                    &seal_id,
                    &incorrect.snapshot_id
                )
                .is_err()
        );
        let mut alternative = checkpoint;
        alternative.prefix_id = f.cas.put(b"other history").unwrap();
        alternative.prefix_bytes = b"other history".len() as u64;
        assert!(
            f.store
                .record_codex_checkpoint(&f.cas, &lease, attempt.id(), &alternative)
                .is_err()
        );
    });
}
